//! The renderer supervisor.
//!
//! Owns a small pool of worker processes, a prioritised queue, the
//! shared-memory regions and the render generations. A worker crash fails its
//! active request, is reported, and triggers a bounded restart; it never
//! touches presentation state.

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pulpit_core::RenderGeneration;

use crate::cache::Frame;
use crate::protocol::{
    read_message, write_message, OpenedDocument, Quality, RenderJob, Request, RequestId, Response,
    PROTOCOL_VERSION,
};
use crate::shm::{RegionNamer, SharedRegion};

/// How a worker process is launched, and the marker that stops a worker
/// launching more. One definition, in `pulpit-core`: the two supervisors and
/// the document session each kept their own, and each said in a comment that
/// the copies had to agree while nothing checked that they did.
pub use pulpit_core::ipc::{as_worker, WorkerCommand, WORKER_MARKER};

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Two workers is the documented starting point; more is a memory
    /// trade-off to be measured, not assumed.
    pub workers: usize,
    pub command: WorkerCommand,
    /// After this long, an unresponsive worker is terminated and replaced.
    /// This is the *final* cancellation mechanism, after the pause callback.
    pub deadline: Duration,
    /// Restarts allowed inside `restart_window` before the supervisor gives
    /// up and reports a persistent failure.
    pub max_restarts: u32,
    pub restart_window: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            command: WorkerCommand::CurrentExe {
                arg: "--render-worker".into(),
            },
            deadline: Duration::from_secs(10),
            max_restarts: 5,
            restart_window: Duration::from_secs(60),
        }
    }
}

/// Something the supervisor wants the application to know.
#[derive(Debug, Clone)]
pub enum RenderEvent {
    Opened(OpenedDocument),
    OpenFailed {
        document: u64,
        reason: String,
    },
    /// A completed frame, already copied out of shared memory.
    Frame {
        job: RenderJob,
        frame: Frame,
        /// How long the worker held this job, from the moment it was
        /// handed over. Everything before that was queueing here.
        worked: Duration,
        /// How long the rasteriser itself took. `worked` minus this is the
        /// wait in the worker's own inbox, which nothing else can see.
        rendered: Duration,
    },
    Failed {
        job: RenderJob,
        reason: String,
    },
    Cancelled {
        id: RequestId,
    },
    /// A worker died. The presentation is unaffected.
    WorkerCrashed {
        worker: usize,
        restarts: u32,
        reason: String,
    },
    WorkerRestarted {
        worker: usize,
    },
    /// Restarts exhausted: rendering is degraded and the user must be told.
    WorkerGaveUp {
        worker: usize,
    },
    /// A worker exceeded its deadline and was terminated.
    WorkerTimedOut {
        worker: usize,
        job: RenderJob,
    },
    /// The link annotations on one page.
    Links {
        document: u64,
        page: usize,
        links: Vec<pulpit_core::PageLink>,
    },
    /// The overlays one page declares, plus a diagnostic for every media URI
    /// the producer wrote that could not be honoured.
    Overlays {
        document: u64,
        page: usize,
        declarations: Vec<pulpit_core::overlay::OverlayDeclaration>,
        diagnostics: Vec<String>,
    },
    /// The page labels and outline of one document.
    Navigation {
        document: u64,
        navigation: pulpit_core::navigation::DocumentNavigation,
    },
    /// Search hits for one run of pages, under the generation that asked.
    ///
    /// `searchable` is false when the backend cannot read a text layer at
    /// all, which is a different fact from finding nothing.
    Found {
        document: u64,
        generation: pulpit_core::search::SearchGeneration,
        chunk: pulpit_core::search::HitChunk,
        searchable: bool,
    },
    /// What one document declares that pulpit will flatten or ignore.
    Capabilities {
        document: u64,
        capabilities: crate::pdf::capabilities::DocumentCapabilities,
    },
    /// The bytes of one embedded file, ready to be staged.
    Attachment {
        document: u64,
        name: String,
        bytes: Vec<u8>,
    },
    /// An attachment that could not be delivered. The static page is
    /// unaffected.
    AttachmentFailed {
        document: u64,
        name: String,
        reason: String,
    },
}

/// Everything the supervisor knows about how rendering is going, sampled at
/// one instant.
///
/// This exists to answer the two questions a presenter actually asks — "why
/// is this slide blurry" and "why was it late" — with something better than a
/// shrug. The counters are the evidence; [`RenderDiagnostics::explanations`]
/// is the answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderDiagnostics {
    /// Backend name as the worker reported it, e.g. `pdfium`.
    pub backend: Option<String>,
    /// Backend build or library path, for comparing two machines.
    pub backend_version: Option<String>,
    pub workers_alive: usize,
    pub workers_configured: usize,
    pub queued: usize,
    pub in_flight: usize,
    /// Queued or in-flight jobs that would sharpen what is on screen.
    pub pending_refined: usize,
    pub submitted: u64,
    pub dispatched: u64,
    /// Frames delivered, per quality tier.
    pub coarse_frames: u64,
    pub refined_frames: u64,
    /// The last frame the supervisor delivered: size and tier.
    pub last_frame: Option<(u32, u32, Quality)>,
    /// The largest frame delivered, which is what the display asked for.
    pub peak_resolution: Option<(u32, u32)>,
    pub cancelled: u64,
    pub failed: u64,
    /// The most recent failure reasons, newest last, bounded.
    pub failures: Vec<String>,
    pub timed_out: u64,
    pub worker_restarts: u32,
    pub workers_given_up: usize,
    /// Cache accounting, when the application has reported it.
    pub cache: Option<crate::cache::CacheStats>,
    pub cache_budget_bytes: Option<u64>,
    /// Time a worker spent actually producing frames, measured from the
    /// moment each job was handed to it.
    ///
    /// The application knows when it submitted a job, so this is the half
    /// that splits the wait: submission to dispatch is queueing, dispatch to
    /// frame is work. A slow rasteriser and a deep queue look identical
    /// without it, and they call for opposite fixes.
    pub worked_total: Duration,
    pub worked_worst: Duration,
    pub worked_count: u64,
}

/// Failure reasons kept for the report. Older ones say nothing the newest
/// does not, and this struct is cloned on every sample.
const MAX_REMEMBERED_FAILURES: usize = 8;

impl RenderDiagnostics {
    /// Cache hit rate over the whole session, or `None` before any lookup.
    pub fn cache_hit_rate(&self) -> Option<f32> {
        let stats = self.cache?;
        let looked_up = stats.hits + stats.misses;
        (looked_up > 0).then(|| stats.hits as f32 / looked_up as f32)
    }

    /// Plain-language consequences of the numbers above, in the order a
    /// presenter would want them. Empty when nothing is wrong.
    pub fn explanations(&self) -> Vec<String> {
        let mut out = Vec::new();

        match self.last_frame {
            Some((width, height, Quality::Coarse)) if self.pending_refined > 0 => {
                out.push(format!(
                "The slide on screen is blurry because the last frame delivered was the coarse \
                 first pass ({width}×{height}). {} refined render(s) are still queued or in \
                 flight; the sharp frame replaces it as soon as one lands.",
                self.pending_refined
            ))
            }
            Some((width, height, Quality::Coarse)) => out.push(format!(
                "The slide on screen is the coarse first pass ({width}×{height}) and no refined \
                 render is outstanding: the refined one was cancelled by a newer navigation or \
                 failed, so the coarse frame stays until the page is requested again.",
            )),
            _ => {}
        }

        if self.queued > self.workers_alive.max(1) * 2 {
            out.push(format!(
                "Slides are landing late because the queue is deep: {} jobs waiting for {} live \
                 worker(s). Adjacent-page prefetching is competing with the page being shown.",
                self.queued, self.workers_alive
            ));
        }

        if self.workers_alive < self.workers_configured {
            out.push(format!(
                "Only {} of {} workers are alive, so everything is rendered with less \
                 parallelism and new pages take correspondingly longer.",
                self.workers_alive, self.workers_configured
            ));
        }

        if self.worker_restarts > 0 {
            out.push(format!(
                "A worker restarted {} time(s). Each restart fails whatever it was rendering and \
                 re-opens every document, which shows up as one late slide around the restart.",
                self.worker_restarts
            ));
        }

        if self.timed_out > 0 {
            out.push(format!(
                "{} render(s) exceeded the deadline and their worker was replaced; those pages \
                 are unusually expensive to draw and will be slow every time.",
                self.timed_out
            ));
        }

        if let Some(reason) = self.failures.last() {
            out.push(format!(
                "{} render(s) failed. Most recent reason: {reason}",
                self.failed
            ));
        }

        if let Some(stats) = self.cache {
            if stats.evictions > 0 && stats.misses > stats.hits {
                out.push(format!(
                    "The frame cache is missing more often than it hits ({} hits, {} misses, {} \
                     evictions): the budget is too small for this resolution, so pages already \
                     rendered are being rendered again.",
                    stats.hits, stats.misses, stats.evictions
                ));
            }
            if stats.rejected > 0 {
                out.push(format!(
                    "{} frame(s) were larger than the whole cache budget and were never cached; \
                     every visit to those pages re-renders them.",
                    stats.rejected
                ));
            }
        }

        out
    }

    /// The whole snapshot as text, ready for a diagnostics pane or a bug
    /// report.
    pub fn to_report(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "backend: {} ({})",
            self.backend.as_deref().unwrap_or("not yet reported"),
            self.backend_version.as_deref().unwrap_or("unknown version")
        ));
        lines.push(format!(
            "workers: {} alive of {} configured, {} restart(s), {} given up",
            self.workers_alive,
            self.workers_configured,
            self.worker_restarts,
            self.workers_given_up
        ));
        lines.push(format!(
            "work: {} submitted, {} dispatched, {} queued, {} in flight ({} refined pending)",
            self.submitted, self.dispatched, self.queued, self.in_flight, self.pending_refined
        ));
        lines.push(format!(
            "frames: {} coarse, {} refined, {} cancelled, {} failed, {} timed out",
            self.coarse_frames, self.refined_frames, self.cancelled, self.failed, self.timed_out
        ));
        lines.push(match self.last_frame {
            Some((width, height, quality)) => format!(
                "resolution: last {width}×{height} {}, peak {}",
                match quality {
                    Quality::Coarse => "coarse",
                    Quality::Refined => "refined",
                },
                match self.peak_resolution {
                    Some((width, height)) => format!("{width}×{height}"),
                    None => "none".to_string(),
                }
            ),
            None => "resolution: no frame delivered yet".to_string(),
        });
        lines.push(match self.cache {
            Some(stats) => format!(
                "cache: {} frames, {} bytes of {} budget, {} hits, {} misses, {} evictions, {} \
                 rejected",
                stats.frames,
                stats.total_bytes(),
                self.cache_budget_bytes
                    .map(|budget| budget.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                stats.hits,
                stats.misses,
                stats.evictions,
                stats.rejected
            ),
            None => "cache: not reported".to_string(),
        });
        let explanations = self.explanations();
        if explanations.is_empty() {
            lines.push("rendering is healthy: nothing is degraded or outstanding".to_string());
        } else {
            lines.push("what this means:".to_string());
            lines.extend(explanations.into_iter().map(|line| format!("  - {line}")));
        }
        lines.join("\n")
    }
}

/// The mutable half of [`RenderDiagnostics`]: everything that is counted
/// rather than read off the queue at sample time.
#[derive(Debug, Default)]
struct Counters {
    backend: Option<String>,
    backend_version: Option<String>,
    submitted: u64,
    dispatched: u64,
    coarse_frames: u64,
    refined_frames: u64,
    last_frame: Option<(u32, u32, Quality)>,
    peak_resolution: Option<(u32, u32)>,
    cancelled: u64,
    failed: u64,
    failures: Vec<String>,
    timed_out: u64,
    worker_restarts: u32,
    workers_given_up: usize,
    cache: Option<crate::cache::CacheStats>,
    cache_budget_bytes: Option<u64>,
    worked_total: Duration,
    worked_worst: Duration,
    worked_count: u64,
    rendered_total: Duration,
    rendered_worst: Duration,
}

impl Counters {
    /// Note how long a worker held a job it was actually working on.
    fn record_worked(&mut self, elapsed: Duration) {
        self.worked_total += elapsed;
        self.worked_worst = self.worked_worst.max(elapsed);
        self.worked_count += 1;
    }

    /// Note how long the rasteriser itself took.
    fn record_rendered(&mut self, elapsed: Duration) {
        self.rendered_total += elapsed;
        self.rendered_worst = self.rendered_worst.max(elapsed);
    }
}

impl Counters {
    fn record_failure(&mut self, reason: &str) {
        self.failed += 1;
        if self.failures.len() == MAX_REMEMBERED_FAILURES {
            self.failures.remove(0);
        }
        self.failures.push(reason.to_string());
    }

    fn record_frame(&mut self, width: u32, height: u32, quality: Quality) {
        match quality {
            Quality::Coarse => self.coarse_frames += 1,
            Quality::Refined => self.refined_frames += 1,
        }
        self.last_frame = Some((width, height, quality));
        let pixels = |(width, height): (u32, u32)| width as u64 * height as u64;
        if self
            .peak_resolution
            .is_none_or(|peak| pixels(peak) < pixels((width, height)))
        {
            self.peak_resolution = Some((width, height));
        }
    }
}

#[derive(Debug)]
struct InFlight {
    job: RenderJob,
    /// When this job was handed to its worker.
    ///
    /// The application already knows when it submitted the job, so this is
    /// the number that splits the wait in two: everything before it is time
    /// spent in a queue, everything after is the worker actually working.
    /// Without the split, a deep queue and a slow rasteriser are the same
    /// measurement — and they call for opposite fixes.
    dispatched: Instant,
}

/// How many inline-frame jobs a worker may hold at once.
///
/// Large frames own the shared region until the supervisor's next pump copies
/// them out, so at most one may be in flight per worker. Inline frames travel
/// in the pipe and need no such exclusivity, so a worker is given a little
/// work in hand: enough that it starts the next render the instant it
/// finishes one, and no more.
///
/// This was sixteen, and sixteen was right for the application it was
/// measured against: a worker that ran dry then idled until the *next
/// application tick*, which was ~97% of warming time. The doorbell removed
/// that wait — a finished frame now wakes the supervisor, which dispatches
/// again in the same breath — and the depth outlived the reason for it.
///
/// Depth is not free, because a job in a worker's inbox has left the only
/// queue the supervisor can manage. It cannot be reordered when a page turn
/// makes something else urgent, and cancelling it costs a round trip rather
/// than a `retain`. Measured on a 730-page deck, that inbox was where a page
/// turn's time went: 6 ms rasterising, 4 ms in the supervisor's queue, and
/// 507 ms sitting inside a worker.
///
/// Four is one job rendering and three in hand: enough to cover the jitter
/// between a frame arriving and the next dispatch, and few enough that a
/// worker cannot accumulate work the supervisor has lost the ability to
/// steer. It is also the point at which `MAX_INLINE_IN_FLIGHT_BYTES` still
/// binds first for frames near the inline threshold, which is the property
/// `inline_bytes_in_flight_are_capped` exists to hold.
const SMALL_PIPELINE_DEPTH: usize = 4;

struct Worker {
    index: usize,
    epoch: u64,
    child: Child,
    stdin: std::io::BufWriter<ChildStdin>,
    region: SharedRegion,
    /// Dispatched and unanswered, oldest first. At most one entry is a
    /// region (non-inline) job.
    in_flight: Vec<InFlight>,
    /// When this worker last did something observable: was handed work while
    /// idle, or sent any message back. The deadline measures silence against
    /// this, not per-job age — a job queued behind another must not accrue
    /// waiting time as if it were being worked on.
    last_progress: Instant,
    restarts: u32,
    restart_times: VecDeque<Instant>,
    alive: bool,
    /// Documents this worker has open, so a restarted worker can catch up.
    documents: Vec<(u64, String)>,
}

/// Message from a worker's reader thread.
struct WorkerMessage {
    worker: usize,
    /// Spawn epoch. A reader thread belonging to a replaced worker can still
    /// be draining its pipe; its messages must not touch the replacement.
    epoch: u64,
    payload: WorkerPayload,
}

enum WorkerPayload {
    Response(Response),
    Died(String),
}

use pulpit_core::ipc::Sink as WakeupSink;
/// A worker has said something, so there is a reason to call
/// [`RendererSupervisor::pump`].
///
/// Deliberately carries nothing: it is a doorbell, not a delivery. The
/// messages themselves stay on the supervisor's own channel, which only the
/// event-loop thread may drain, so nothing here can race a dispatch or
/// duplicate an event. A caller that misses one loses nothing — the next
/// `pump` drains everything waiting — which is what lets the sink drop
/// signals rather than block a reader thread.
pub use pulpit_core::ipc::{Doorbell as RenderWakeup, Wakeup};

pub struct RendererSupervisor {
    config: SupervisorConfig,
    workers: Vec<Worker>,
    queue: VecDeque<RenderJob>,
    events: Receiver<WorkerMessage>,
    /// One message pulled only to determine whether a bounded drain has more
    /// work. It remains ordered ahead of the channel on the next pass.
    deferred: VecDeque<WorkerMessage>,
    sender: Sender<WorkerMessage>,
    /// Handed to every reader thread, including those of workers spawned or
    /// restarted later, so the doorbell survives a crash.
    wakeup: WakeupSink,
    /// The listener's end, taken once by whoever drives the event loop.
    wakeup_inbox: Option<Arc<RenderWakeup>>,
    namer: RegionNamer,
    next_request: u64,
    generation_floor: RenderGeneration,
    documents: Vec<(u64, String)>,
    counters: Counters,
}

impl std::fmt::Debug for RendererSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RendererSupervisor")
            .field("workers", &self.workers.len())
            .field("queued", &self.queue.len())
            .field("generation_floor", &self.generation_floor)
            .finish()
    }
}

const DEFAULT_REGION_BYTES: u64 = 1920 * 1080 * 4;

impl RendererSupervisor {
    pub fn start(config: SupervisorConfig) -> std::io::Result<Self> {
        let (sender, events) = channel();
        let (signal, inbox) = pulpit_core::ipc::doorbell();
        let mut supervisor = Self {
            workers: Vec::new(),
            queue: VecDeque::new(),
            events,
            deferred: VecDeque::new(),
            sender,
            wakeup: signal,
            wakeup_inbox: Some(Arc::new(inbox)),
            namer: RegionNamer::new(),
            next_request: 0,
            generation_floor: RenderGeneration::ZERO,
            documents: Vec::new(),
            counters: Counters::default(),
            config,
        };
        // One worker up front; the rest of the configured pool spawns on the
        // first queue contention. A worker process carries a whole PDFium,
        // and an idle application — or a small deck one worker keeps up
        // with — should not pay for two of them.
        let worker = supervisor.spawn_worker(0, 0)?;
        supervisor.workers.push(worker);
        Ok(supervisor)
    }

    /// Bring up another worker from the configured pool, replaying the open
    /// documents it missed. Returns false when the pool is already full or
    /// the spawn failed.
    fn spawn_additional_worker(&mut self) -> bool {
        if self.workers.len() >= self.config.workers {
            return false;
        }
        let index = self.workers.len();
        let mut worker = match self.spawn_worker(index, 0) {
            Ok(worker) => worker,
            Err(e) => {
                tracing::warn!(worker = index, error = %e, "cannot spawn an additional worker");
                return false;
            }
        };
        for (document, path) in &self.documents {
            let request = Request::Open {
                document: *document,
                path: path.clone(),
            };
            if write_message(&mut worker.stdin, &request).is_ok() {
                worker.documents.push((*document, path.clone()));
            }
        }
        self.workers.push(worker);
        true
    }

    fn spawn_worker(&self, index: usize, epoch: u64) -> std::io::Result<Worker> {
        let mut child = self.config.command.build("renderer worker")?.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let region = SharedRegion::create(&self.namer.next(), DEFAULT_REGION_BYTES)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let sender = self.sender.clone();
        let wakeup = self.wakeup.clone();
        std::thread::Builder::new()
            .name(format!("render-reader-{index}"))
            .spawn(move || read_responses(index, epoch, stdout, sender, wakeup))?;

        let mut worker = Worker {
            index,
            epoch,
            child,
            // Buffered so each request is one write syscall instead of
            // three; `write_message` still flushes every message.
            stdin: std::io::BufWriter::new(stdin),
            region,
            in_flight: Vec::new(),
            last_progress: Instant::now(),
            restarts: 0,
            restart_times: VecDeque::new(),
            alive: true,
            documents: Vec::new(),
        };
        let _ = write_message(
            &mut worker.stdin,
            &Request::Hello {
                version: PROTOCOL_VERSION,
            },
        );
        Ok(worker)
    }

    pub fn worker_count(&self) -> usize {
        self.workers.iter().filter(|w| w.alive).count()
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn in_flight(&self) -> usize {
        self.workers.iter().map(|w| w.in_flight.len()).sum()
    }

    /// Jobs that would sharpen what is on screen: refined work still queued or
    /// in flight. A coarse frame with none of these outstanding is a frame
    /// that will stay blurry.
    pub fn pending_refined(&self) -> usize {
        self.queue
            .iter()
            .filter(|job| job.quality == Quality::Refined)
            .count()
            + self
                .workers
                .iter()
                .flat_map(|worker| &worker.in_flight)
                .filter(|f| f.job.quality == Quality::Refined)
                .count()
    }

    /// Tell the supervisor what the frame cache is doing. The cache is owned
    /// by the application, so its numbers can only reach the report by being
    /// handed over; without this the report simply says so.
    pub fn note_cache_stats(&mut self, stats: crate::cache::CacheStats, budget_bytes: u64) {
        self.counters.cache = Some(stats);
        self.counters.cache_budget_bytes = Some(budget_bytes);
    }

    /// A snapshot of how rendering is going, with the queue read at this
    /// instant and the counters accumulated since start.
    pub fn diagnostics(&self) -> RenderDiagnostics {
        RenderDiagnostics {
            backend: self.counters.backend.clone(),
            backend_version: self.counters.backend_version.clone(),
            workers_alive: self.worker_count(),
            workers_configured: self.config.workers,
            queued: self.queued(),
            in_flight: self.in_flight(),
            pending_refined: self.pending_refined(),
            submitted: self.counters.submitted,
            dispatched: self.counters.dispatched,
            coarse_frames: self.counters.coarse_frames,
            refined_frames: self.counters.refined_frames,
            last_frame: self.counters.last_frame,
            peak_resolution: self.counters.peak_resolution,
            cancelled: self.counters.cancelled,
            failed: self.counters.failed,
            failures: self.counters.failures.clone(),
            timed_out: self.counters.timed_out,
            worker_restarts: self.counters.worker_restarts,
            workers_given_up: self.counters.workers_given_up,
            cache: self.counters.cache,
            cache_budget_bytes: self.counters.cache_budget_bytes,
            worked_total: self.counters.worked_total,
            worked_worst: self.counters.worked_worst,
            worked_count: self.counters.worked_count,
        }
    }

    pub fn next_request_id(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }

    /// Open a document on every worker. Each worker holds its own PDFium
    /// handle; the shared identifier is the supervisor's.
    pub fn open(&mut self, document: u64, path: &str) {
        self.documents.push((document, path.to_string()));
        let request = Request::Open {
            document,
            path: path.to_string(),
        };
        for worker in &mut self.workers {
            if worker.alive && write_message(&mut worker.stdin, &request).is_ok() {
                worker.documents.push((document, path.to_string()));
            }
        }
    }

    /// Ask one worker for the link annotations on a page. The answer arrives
    /// as [`RenderEvent::Links`]; a page without links answers with an empty
    /// list. Cheap enough that no queueing or cancellation is needed.
    pub fn request_links(&mut self, document: u64, page: usize) {
        // Through `ask`, like every other question — and because the worker
        // choice is deterministic between two back-to-back calls, the
        // Overlays request that always follows lands on the same worker,
        // where the links it derives from are still cached.
        self.ask(Request::Links { document, page });
    }

    /// Ask one worker which overlays a page declares. The answer arrives as
    /// [`RenderEvent::Overlays`].
    pub fn request_overlays(&mut self, document: u64, page: usize) {
        self.ask(Request::Overlays { document, page });
    }

    /// Ask one worker for a document's page labels and outline. The answer
    /// arrives as [`RenderEvent::Navigation`].
    pub fn request_navigation(&mut self, document: u64) {
        self.ask(Request::Navigation { document });
    }

    /// Ask one worker to find a string in a run of pages. The answer arrives
    /// as [`RenderEvent::Found`].
    pub fn request_find_text(
        &mut self,
        document: u64,
        generation: pulpit_core::search::SearchGeneration,
        query: pulpit_core::search::Query,
        pages: std::ops::Range<usize>,
    ) {
        self.ask(Request::FindText {
            document,
            generation,
            query,
            from_page: pages.start,
            to_page: pages.end,
        });
    }

    /// Ask one worker what the document declares that pulpit will not
    /// honour. The answer arrives as [`RenderEvent::Capabilities`].
    pub fn request_capabilities(&mut self, document: u64) {
        self.ask(Request::Capabilities { document });
    }

    /// Ask one worker for the bytes of an embedded file. The answer arrives
    /// as [`RenderEvent::Attachment`] or [`RenderEvent::AttachmentFailed`].
    pub fn request_attachment(&mut self, document: u64, name: &str) {
        self.ask(Request::Attachment {
            document,
            name: name.to_string(),
        });
    }

    /// Send a question to a live worker, preferring an idle one. Some of
    /// these questions are not cheap — a capabilities scan walks hundreds of
    /// pages' annotations — and always picking the first worker piled every
    /// one of them onto the same process that renders the visible frames.
    fn ask(&mut self, request: Request) {
        let target = self
            .workers
            .iter_mut()
            .filter(|worker| worker.alive)
            .min_by_key(|worker| worker.in_flight.len());
        if let Some(worker) = target {
            let _ = write_message(&mut worker.stdin, &request);
        }
    }

    pub fn close(&mut self, document: u64) {
        self.documents.retain(|(id, _)| *id != document);
        for worker in &mut self.workers {
            worker.documents.retain(|(id, _)| *id != document);
            if worker.alive {
                let _ = write_message(&mut worker.stdin, &Request::Close { document });
            }
        }
    }

    /// Enqueue a job. Jobs older than the current generation floor are
    /// dropped immediately: obsolete work is never dispatched.
    pub fn submit(&mut self, job: RenderJob) {
        if job.generation < self.generation_floor {
            return;
        }
        // A newer request for the same page, quality and size supersedes an
        // older queued one rather than queueing behind it. Generation and
        // region are part of "the same": a `/FitR` re-crop or a reload asks
        // for a genuinely different picture at the same dimensions, and
        // silently dropping the older job forced a redundant re-request
        // later.
        self.queue.retain(|queued| {
            !(queued.page == job.page
                && queued.quality == job.quality
                && queued.width == job.width
                && queued.height == job.height
                && queued.document == job.document
                && queued.generation == job.generation
                && queued.region == job.region)
        });
        self.queue.push_back(job);
        self.counters.submitted += 1;
        self.dispatch();
    }

    /// Everything before `generation` is obsolete: queued work is dropped and
    /// in-flight work is asked to yield through the pause callback.
    pub fn cancel_older_than(&mut self, generation: RenderGeneration) {
        self.generation_floor = generation;
        self.queue.retain(|job| job.generation >= generation);
        for worker in &mut self.workers {
            if worker.alive {
                let _ = write_message(&mut worker.stdin, &Request::CancelGeneration { generation });
            }
        }
    }

    pub fn cancel(&mut self, id: RequestId) {
        self.queue.retain(|job| job.id != id);
        for worker in &mut self.workers {
            if worker.alive {
                let _ = write_message(&mut worker.stdin, &Request::Cancel { id });
            }
        }
    }

    /// The doorbell, for whoever drives the event loop. `None` after the
    /// first call: one listener, so a signal cannot be delivered to a thread
    /// that is not the one that will call [`pump`](Self::pump).
    pub fn take_wakeup(&mut self) -> Option<Arc<RenderWakeup>> {
        self.wakeup_inbox.take()
    }

    /// Ring the doorbell without a worker having said anything.
    ///
    /// The listener is woken so it can notice a state change of the caller's
    /// own — a shutdown, most usefully, which no worker will announce.
    pub fn wake(&self) {
        self.wakeup.ring();
    }

    /// Drain worker events, enforce deadlines and dispatch queued work.
    /// Called from the application's update loop.
    pub fn pump(&mut self) -> Vec<RenderEvent> {
        let mut all = Vec::new();
        loop {
            let batch = self.pump_bounded(usize::MAX);
            all.extend(batch.events);
            if !batch.more {
                return all;
            }
        }
    }

    /// Drain at most `limit` worker messages, preserving FIFO order and
    /// reporting whether the event loop should schedule a continuation.
    pub fn pump_bounded(&mut self, limit: usize) -> PumpBatch {
        let mut events = Vec::new();
        let mut handled = 0;
        while handled < limit {
            let message = self
                .deferred
                .pop_front()
                .map(Ok)
                .unwrap_or_else(|| self.events.try_recv());
            match message {
                Ok(message) => self.handle(message, &mut events),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
            handled += 1;
        }
        if handled == limit {
            if let Ok(message) = self.events.try_recv() {
                self.deferred.push_back(message);
            }
        }
        self.enforce_deadlines(&mut events);
        self.reap(&mut events);
        self.dispatch();
        PumpBatch {
            events,
            more: !self.deferred.is_empty(),
        }
    }

    /// Block for up to `timeout` waiting for at least one event. Convenient
    /// for tests and for a background pump thread.
    pub fn pump_blocking(&mut self, timeout: Duration) -> Vec<RenderEvent> {
        let mut events = Vec::new();
        if let Ok(message) = self.events.recv_timeout(timeout) {
            self.handle(message, &mut events);
        }
        events.extend(self.pump());
        events
    }

    fn handle(&mut self, message: WorkerMessage, events: &mut Vec<RenderEvent>) {
        let index = message.worker;
        if self
            .workers
            .get(index)
            .is_none_or(|worker| worker.epoch != message.epoch)
        {
            // A message from a worker instance that has already been
            // replaced. Dropping it is what keeps a crash from cascading
            // into the healthy replacement.
            return;
        }
        // Any message is proof of life; the deadline measures silence.
        if let Some(worker) = self.workers.get_mut(index) {
            worker.last_progress = Instant::now();
        }
        match message.payload {
            WorkerPayload::Response(Response::Hello {
                version,
                backend,
                backend_version,
            }) => {
                self.counters.backend = Some(backend.clone());
                self.counters.backend_version = Some(backend_version);
                if version != PROTOCOL_VERSION {
                    tracing::error!(
                        worker = index,
                        version,
                        "protocol mismatch; replacing worker"
                    );
                    self.kill(index, "protocol version mismatch", events);
                } else {
                    tracing::info!(worker = index, backend, "worker ready");
                }
            }
            WorkerPayload::Response(Response::Opened(opened)) => {
                // Only report the first worker's answer; the others are
                // identical by construction.
                if index == self.first_alive() {
                    events.push(RenderEvent::Opened(opened));
                }
            }
            WorkerPayload::Response(Response::OpenFailed { document, reason }) => {
                if index == self.first_alive() {
                    events.push(RenderEvent::OpenFailed { document, reason });
                }
            }
            WorkerPayload::Response(Response::Rendered {
                id,
                width,
                height,
                bytes,
                quality,
                pixels,
                render_micros,
                ..
            }) => {
                let floor = self.generation_floor;
                let Some(worker) = self.workers.get_mut(index) else {
                    return;
                };
                let Some(position) = worker.in_flight.iter().position(|f| f.job.id == id) else {
                    return;
                };
                let in_flight = worker.in_flight.remove(position);
                if in_flight.job.generation < floor {
                    // Results from an obsolete generation are discarded on
                    // this side too, even though the worker produced them.
                    self.counters.cancelled += 1;
                    events.push(RenderEvent::Cancelled { id });
                    return;
                }
                // Validate the declared frame before reading a single byte,
                // whether it arrived inline or through the region we own.
                let declared = width as u64 * height as u64 * 4;
                let pixels = match pixels {
                    Some(inline) => {
                        if bytes != declared || inline.len() as u64 != declared {
                            let reason = format!(
                                "worker declared {bytes} bytes and sent {} for a \
                                 {width}×{height} inline frame",
                                inline.len()
                            );
                            self.counters.record_failure(&reason);
                            events.push(RenderEvent::Failed {
                                job: in_flight.job,
                                reason,
                            });
                            return;
                        }
                        inline
                    }
                    None => {
                        if bytes != declared || bytes > worker.region.len() as u64 {
                            let reason = format!(
                                "worker declared {bytes} bytes for a {width}×{height} frame in \
                                 a {} byte region",
                                worker.region.len()
                            );
                            self.counters.record_failure(&reason);
                            events.push(RenderEvent::Failed {
                                job: in_flight.job,
                                reason,
                            });
                            return;
                        }
                        worker.region.as_slice()[..bytes as usize].to_vec()
                    }
                };
                let worked = in_flight.dispatched.elapsed();
                let rendered = Duration::from_micros(render_micros);
                self.counters.record_worked(worked);
                self.counters.record_rendered(rendered);
                self.counters.record_frame(width, height, quality);
                events.push(RenderEvent::Frame {
                    job: in_flight.job,
                    frame: Frame::new(width, height, pixels),
                    worked,
                    rendered,
                });
            }
            WorkerPayload::Response(Response::RenderFailed { id, reason, .. }) => {
                let Some(worker) = self.workers.get_mut(index) else {
                    return;
                };
                if let Some(position) = worker.in_flight.iter().position(|f| f.job.id == id) {
                    let in_flight = worker.in_flight.remove(position);
                    self.counters.record_failure(&reason);
                    events.push(RenderEvent::Failed {
                        job: in_flight.job,
                        reason,
                    });
                }
            }
            WorkerPayload::Response(Response::Cancelled { id }) => {
                if let Some(worker) = self.workers.get_mut(index) {
                    worker.in_flight.retain(|f| f.job.id != id);
                }
                self.counters.cancelled += 1;
                events.push(RenderEvent::Cancelled { id });
            }
            WorkerPayload::Response(Response::Links {
                document,
                page,
                links,
            }) => {
                events.push(RenderEvent::Links {
                    document,
                    page,
                    links,
                });
            }
            WorkerPayload::Response(Response::Overlays {
                document,
                page,
                declarations,
                diagnostics,
            }) => {
                events.push(RenderEvent::Overlays {
                    document,
                    page,
                    declarations,
                    diagnostics,
                });
            }
            WorkerPayload::Response(Response::Navigation {
                document,
                navigation,
            }) => {
                events.push(RenderEvent::Navigation {
                    document,
                    navigation,
                });
            }
            WorkerPayload::Response(Response::Found {
                document,
                generation,
                chunk,
                searchable,
            }) => {
                events.push(RenderEvent::Found {
                    document,
                    generation,
                    chunk,
                    searchable,
                });
            }
            WorkerPayload::Response(Response::Capabilities {
                document,
                capabilities,
            }) => {
                events.push(RenderEvent::Capabilities {
                    document,
                    capabilities,
                });
            }
            WorkerPayload::Response(Response::Attachment {
                document,
                name,
                bytes,
            }) => {
                events.push(RenderEvent::Attachment {
                    document,
                    name,
                    bytes,
                });
            }
            WorkerPayload::Response(Response::AttachmentFailed {
                document,
                name,
                reason,
            }) => {
                events.push(RenderEvent::AttachmentFailed {
                    document,
                    name,
                    reason,
                });
            }
            WorkerPayload::Died(reason) => self.kill(index, &reason, events),
        }
    }

    fn first_alive(&self) -> usize {
        self.workers
            .iter()
            .find(|w| w.alive)
            .map(|w| w.index)
            .unwrap_or(0)
    }

    fn enforce_deadlines(&mut self, events: &mut Vec<RenderEvent>) {
        let deadline = self.config.deadline;
        // A worker is overdue when it has been silent for the deadline while
        // holding work — not when any one job is old. A job dispatched behind
        // another spends its early life legitimately waiting, and a worker
        // steadily answering is healthy however deep its backlog. The oldest
        // job stands for the backlog in the report; `kill` fails the rest.
        let overdue: Vec<(usize, RenderJob)> = self
            .workers
            .iter()
            .filter_map(|worker| {
                let in_flight = worker.in_flight.first()?;
                (worker.alive && worker.last_progress.elapsed() > deadline)
                    .then(|| (worker.index, in_flight.job.clone()))
            })
            .collect();
        for (index, job) in overdue {
            tracing::warn!(
                worker = index,
                ?job,
                "worker exceeded its deadline; terminating"
            );
            self.counters.timed_out += 1;
            events.push(RenderEvent::WorkerTimedOut { worker: index, job });
            self.kill(index, "deadline exceeded", events);
        }
    }

    /// Notice workers that exited without their reader thread reporting it.
    fn reap(&mut self, events: &mut Vec<RenderEvent>) {
        let dead: Vec<usize> = self
            .workers
            .iter_mut()
            .filter(|w| w.alive)
            .filter_map(|worker| match worker.child.try_wait() {
                Ok(Some(status)) => Some((worker.index, status)),
                _ => None,
            })
            .map(|(index, _)| index)
            .collect();
        for index in dead {
            self.kill(index, "worker exited", events);
        }
    }

    fn kill(&mut self, index: usize, reason: &str, events: &mut Vec<RenderEvent>) {
        let Some(worker) = self.workers.get_mut(index) else {
            return;
        };
        if !worker.alive {
            return;
        }
        worker.alive = false;
        let _ = worker.child.kill();
        let _ = worker.child.wait();

        // Every dispatched request fails; presentation state is untouched.
        // Missing even one would leave its requester waiting forever — the
        // application frees an outstanding-work slot only when a job is
        // answered.
        for in_flight in worker.in_flight.drain(..) {
            let reason = format!("worker {index} died: {reason}");
            self.counters.record_failure(&reason);
            events.push(RenderEvent::Failed {
                job: in_flight.job,
                reason,
            });
        }

        let now = Instant::now();
        let window = self.config.restart_window;
        worker.restart_times.push_back(now);
        while worker
            .restart_times
            .front()
            .is_some_and(|t| now.duration_since(*t) > window)
        {
            worker.restart_times.pop_front();
        }
        worker.restarts += 1;
        let recent = worker.restart_times.len() as u32;
        events.push(RenderEvent::WorkerCrashed {
            worker: index,
            restarts: worker.restarts,
            reason: reason.to_string(),
        });

        if recent > self.config.max_restarts {
            tracing::error!(worker = index, "restart budget exhausted");
            self.counters.workers_given_up += 1;
            events.push(RenderEvent::WorkerGaveUp { worker: index });
            return;
        }

        let documents = self.documents.clone();
        let epoch = self.workers[index].epoch + 1;
        match self.spawn_worker(index, epoch) {
            Ok(mut replacement) => {
                replacement.restarts = self.workers[index].restarts;
                replacement.restart_times = self.workers[index].restart_times.clone();
                for (document, path) in &documents {
                    let request = Request::Open {
                        document: *document,
                        path: path.clone(),
                    };
                    if write_message(&mut replacement.stdin, &request).is_ok() {
                        replacement.documents.push((*document, path.clone()));
                    }
                }
                self.workers[index] = replacement;
                self.counters.worker_restarts += 1;
                events.push(RenderEvent::WorkerRestarted { worker: index });
            }
            Err(e) => {
                tracing::error!(worker = index, error = %e, "cannot restart worker");
                self.counters.workers_given_up += 1;
                events.push(RenderEvent::WorkerGaveUp { worker: index });
            }
        }
    }

    /// Whether this worker can be handed this job right now. An inline job
    /// needs only a slot; a region job additionally needs the region, which
    /// the previous region job owns until its frame is copied out on the
    /// pump.
    fn eligible(worker: &Worker, job: &RenderJob) -> bool {
        if !(worker.alive && worker.in_flight.len() < SMALL_PIPELINE_DEPTH) {
            return false;
        }
        if job.is_inline() {
            // Bounded in bytes as well as jobs: the depth cap alone would
            // let sixteen threshold-sized frames sit buffered between pumps.
            let held: u64 = worker
                .in_flight
                .iter()
                .filter(|f| f.job.is_inline())
                .map(|f| f.job.byte_len())
                .sum();
            held + job.byte_len() <= crate::protocol::MAX_INLINE_IN_FLIGHT_BYTES
        } else {
            worker.in_flight.iter().all(|f| f.job.is_inline())
        }
    }

    fn dispatch(&mut self) {
        loop {
            // Highest priority, coarse before refined, then FIFO — among the
            // jobs some worker can take. A region job blocked on every
            // region does not block the inline work behind it: the workers
            // drain their own inboxes highest-priority-first, so letting a
            // thumbnail board early never lets it render early.
            let mut order: Vec<usize> = (0..self.queue.len()).collect();
            order.sort_by_key(|&position| {
                let job = &self.queue[position];
                (
                    job.priority,
                    matches!(job.quality, Quality::Refined),
                    position,
                )
            });
            let choice = order.into_iter().find_map(|position| {
                let job = &self.queue[position];
                self.workers
                    .iter()
                    .filter(|worker| Self::eligible(worker, job))
                    .min_by_key(|worker| worker.in_flight.len())
                    .map(|worker| (position, worker.index))
            });
            let Some((position, index)) = choice else {
                // Work is waiting and nobody can take it: this is the
                // contention the rest of the configured pool exists for.
                if !self.queue.is_empty() && self.spawn_additional_worker() {
                    continue;
                }
                return;
            };
            let mut job = self
                .queue
                .remove(position)
                .expect("position from this queue");

            let worker = &mut self.workers[index];
            if job.is_inline() {
                job.region_name = String::new();
            } else {
                if worker.region.ensure_capacity(job.byte_len()).is_err() {
                    tracing::warn!(?job, "cannot size the shared region for this job");
                    continue;
                }
                job.region_name = worker.region.name().to_string();
            }
            if write_message(&mut worker.stdin, &Request::Render(job.clone())).is_err() {
                // The pipe is gone; the reader thread will report the death.
                self.queue.push_front(job);
                return;
            }
            let _ = worker.stdin.flush();
            if worker.in_flight.is_empty() {
                // The deadline clock measures silence-while-holding-work;
                // an idle worker's silence was innocent and ends here.
                worker.last_progress = Instant::now();
            }
            worker.in_flight.push(InFlight {
                job,
                dispatched: Instant::now(),
            });
            self.counters.dispatched += 1;
        }
    }

    /// Ask every worker to exit, then wait briefly.
    pub fn shutdown(&mut self) {
        for worker in &mut self.workers {
            if worker.alive {
                let _ = write_message(&mut worker.stdin, &Request::Shutdown);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        for worker in &mut self.workers {
            while worker.alive && Instant::now() < deadline {
                match worker.child.try_wait() {
                    Ok(Some(_)) => worker.alive = false,
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            if worker.alive {
                let _ = worker.child.kill();
                let _ = worker.child.wait();
                worker.alive = false;
            }
        }
    }
}

/// One bounded event-loop drain.
#[derive(Default)]
pub struct PumpBatch {
    pub events: Vec<RenderEvent>,
    pub more: bool,
}

impl Drop for RendererSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The reader thread only parses and forwards. Copying pixels out of the
/// shared region is the supervisor's job: it owns the region and knows which
/// job is in flight, so no copy can race the next dispatch.
fn read_responses(
    index: usize,
    epoch: u64,
    stdout: std::process::ChildStdout,
    sender: Sender<WorkerMessage>,
    wakeup: WakeupSink,
) {
    let mut reader = std::io::BufReader::new(stdout);
    loop {
        match read_message::<Response>(&mut reader) {
            Ok(response) => {
                if sender
                    .send(WorkerMessage {
                        worker: index,
                        epoch,
                        payload: WorkerPayload::Response(response),
                    })
                    .is_err()
                {
                    return;
                }
                // After the message is on the channel, never before: a
                // listener woken first would drain an empty channel and go
                // back to sleep with the frame still in flight.
                wakeup.ring();
            }
            Err(e) => {
                let _ = sender.send(WorkerMessage {
                    worker: index,
                    epoch,
                    payload: WorkerPayload::Died(e.to_string()),
                });
                // A death is the most urgent thing a worker ever says: the
                // restart is what gets the queue moving again.
                wakeup.ring();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheStats;

    fn healthy() -> RenderDiagnostics {
        RenderDiagnostics {
            backend: Some("pdfium".into()),
            backend_version: Some("/opt/lib/libpdfium.so".into()),
            workers_alive: 2,
            workers_configured: 2,
            last_frame: Some((1920, 1080, Quality::Refined)),
            peak_resolution: Some((1920, 1080)),
            refined_frames: 3,
            ..Default::default()
        }
    }

    #[test]
    fn a_healthy_renderer_explains_nothing_and_says_so() {
        let diagnostics = healthy();
        assert!(diagnostics.explanations().is_empty());
        let report = diagnostics.to_report();
        assert!(report.contains("backend: pdfium (/opt/lib/libpdfium.so)"));
        assert!(report.contains("last 1920×1080 refined"));
        assert!(report.contains("rendering is healthy"));
        assert!(report.contains("cache: not reported"));
    }

    #[test]
    fn a_blurry_slide_is_explained_by_the_refined_frame_that_has_not_landed() {
        let diagnostics = RenderDiagnostics {
            last_frame: Some((640, 360, Quality::Coarse)),
            pending_refined: 2,
            ..healthy()
        };
        let explanation = &diagnostics.explanations()[0];
        assert!(explanation.contains("blurry"), "{explanation}");
        assert!(explanation.contains("640×360"), "{explanation}");
        assert!(explanation.contains("2 refined render(s)"), "{explanation}");
    }

    #[test]
    fn a_coarse_frame_with_nothing_outstanding_says_the_refinement_is_not_coming() {
        let diagnostics = RenderDiagnostics {
            last_frame: Some((640, 360, Quality::Coarse)),
            pending_refined: 0,
            ..healthy()
        };
        let explanation = &diagnostics.explanations()[0];
        assert!(
            explanation.contains("cancelled by a newer navigation"),
            "{explanation}"
        );
    }

    #[test]
    fn a_deep_queue_and_a_restart_both_explain_a_delayed_slide() {
        let diagnostics = RenderDiagnostics {
            queued: 12,
            worker_restarts: 1,
            ..healthy()
        };
        let explanations = diagnostics.explanations().join("\n");
        assert!(explanations.contains("queue is deep"), "{explanations}");
        assert!(
            explanations.contains("12 jobs waiting for 2"),
            "{explanations}"
        );
        assert!(
            explanations.contains("restarted 1 time(s)"),
            "{explanations}"
        );
    }

    #[test]
    fn lost_workers_timeouts_and_failures_are_all_reported_with_reasons() {
        let diagnostics = RenderDiagnostics {
            workers_alive: 1,
            timed_out: 2,
            failed: 1,
            failures: vec!["shared memory: region too small".into()],
            ..healthy()
        };
        let explanations = diagnostics.explanations().join("\n");
        assert!(
            explanations.contains("Only 1 of 2 workers"),
            "{explanations}"
        );
        assert!(
            explanations.contains("exceeded the deadline"),
            "{explanations}"
        );
        assert!(
            explanations.contains("shared memory: region too small"),
            "{explanations}"
        );
    }

    #[test]
    fn a_thrashing_cache_is_reported_as_a_budget_problem() {
        let diagnostics = RenderDiagnostics {
            cache: Some(CacheStats {
                frames: 4,
                cpu_bytes: 100,

                hits: 2,
                misses: 40,
                evictions: 30,
                rejected: 1,
                pinned_overcommit_bytes: 0,
            }),
            cache_budget_bytes: Some(1024),
            ..healthy()
        };
        assert_eq!(diagnostics.cache_hit_rate(), Some(2.0 / 42.0));
        let report = diagnostics.to_report();
        assert!(report.contains("cache: 4 frames, 100 bytes of 1024 budget"));
        assert!(report.contains("budget is too small"), "{report}");
        assert!(
            report.contains("larger than the whole cache budget"),
            "{report}"
        );
    }

    #[test]
    fn counters_track_frames_failures_and_the_peak_resolution() {
        let mut counters = Counters::default();
        counters.record_frame(640, 360, Quality::Coarse);
        counters.record_frame(1920, 1080, Quality::Refined);
        counters.record_frame(800, 600, Quality::Refined);
        assert_eq!(counters.coarse_frames, 1);
        assert_eq!(counters.refined_frames, 2);
        assert_eq!(counters.last_frame, Some((800, 600, Quality::Refined)));
        assert_eq!(
            counters.peak_resolution,
            Some((1920, 1080)),
            "the peak is the largest frame, not the latest"
        );

        for index in 0..MAX_REMEMBERED_FAILURES + 3 {
            counters.record_failure(&format!("reason {index}"));
        }
        assert_eq!(counters.failed as usize, MAX_REMEMBERED_FAILURES + 3);
        assert_eq!(counters.failures.len(), MAX_REMEMBERED_FAILURES);
        assert_eq!(
            counters.failures.last().unwrap(),
            &format!("reason {}", MAX_REMEMBERED_FAILURES + 2),
            "the newest reason is the one kept"
        );
    }
}
