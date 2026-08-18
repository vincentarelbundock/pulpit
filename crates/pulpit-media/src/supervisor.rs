//! Session supervision (`docs-src/internals.typ`).
//!
//! Owns worker processes, the shared-memory rings and the session table. The
//! rule the rest of the application depends on is here: once a session has
//! published a frame, that frame is retained until a *complete* replacement
//! arrives. A crash, a hang, a malformed frame or an exhausted fallback chain
//! never blanks the audience.

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{
    channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pulpit_core::overlay::ContentKind;
use pulpit_core::{OverlayId, RenderGeneration};

use crate::capability::RuntimeProbe;
use crate::protocol::{
    read_message, write_message, CapabilityRequest, ImageCommand, InputEvent, MediaError,
    MediaErrorKind, MediaEvent, MediaRequest, MediaWarning, PixelFormat, PlaybackProgress,
    RuntimeId, SessionId, SessionSpec, SurfaceId, VideoCommand, Viewport, WebCommand,
    WorkerCounters, MEDIA_PROTOCOL_VERSION,
};
use crate::selection::{Attempt, AttemptOutcome, RuntimePolicy, Selection};
use crate::surface::{RingNamer, SurfaceRing, DEFAULT_SLOTS};

/// How long a worker gets to shut its browsers down cleanly before it is
/// killed. Comfortably more than the three seconds each browser is given, and
/// still short enough not to read as a hung quit.
const WORKER_EXIT_GRACE: Duration = Duration::from_secs(5);

/// A media worker queued an event. The payload remains on the supervisor's
/// ordered channel; this one-slot signal only wakes the application.
pub struct MediaWakeup {
    inbox: Mutex<Receiver<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wakeup {
    Ring,
    Idle,
    Closed,
}

impl MediaWakeup {
    pub fn wait(&self, timeout: Duration) -> Wakeup {
        let Ok(inbox) = self.inbox.try_lock() else {
            return Wakeup::Closed;
        };
        match inbox.recv_timeout(timeout) {
            Ok(()) => Wakeup::Ring,
            Err(RecvTimeoutError::Timeout) => Wakeup::Idle,
            Err(RecvTimeoutError::Disconnected) => Wakeup::Closed,
        }
    }
}

#[derive(Clone)]
struct WakeupSink(SyncSender<()>);

impl WakeupSink {
    fn ring(&self) {
        let _ = self.0.try_send(());
    }
}

/// How a worker process is launched.
#[derive(Debug, Clone)]
pub enum WorkerCommand {
    /// Re-execute the running executable with a flag that selects a worker
    /// role. Used for every runtime that links nothing optional, so there is
    /// no second binary to build, install or package — and therefore none to
    /// be missing.
    CurrentExe {
        arg: String,
    },
    Explicit {
        program: PathBuf,
        args: Vec<String>,
    },
}

/// Set on every worker process a supervisor spawns, and refused as input.
///
/// See the renderer supervisor's marker of the same name: a worker that
/// spawns workers grows exponentially and takes the machine down before any
/// deadline notices. The name is shared deliberately — a renderer worker must
/// not spawn media workers either, and one marker covers both directions.
pub const WORKER_MARKER: &str = "PULPIT_WORKER";

impl WorkerCommand {
    fn build(&self) -> std::io::Result<Command> {
        if std::env::var_os(WORKER_MARKER).is_some() {
            return Err(std::io::Error::other(
                "refusing to spawn a media worker from inside a worker process",
            ));
        }
        let mut command = match self {
            WorkerCommand::CurrentExe { arg } => {
                let mut command = Command::new(std::env::current_exe()?);
                command.arg(arg);
                command
            }
            WorkerCommand::Explicit { program, args } => {
                let mut command = Command::new(program);
                command.args(args);
                command
            }
        };
        command
            .env(WORKER_MARKER, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        Ok(command)
    }
}

#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// How long a worker has to answer its handshake before it is replaced.
    pub handshake_deadline: Duration,
    /// How long a session has to produce its first complete frame.
    pub first_frame_deadline: Duration,
    /// Restarts allowed for one session before the supervisor gives up and
    /// falls through to the next runtime.
    pub max_restarts: u32,
    pub slots: u32,
    /// The most frames per second any session's worker should decode and
    /// publish. The browser may paint faster; the excess is acknowledged and
    /// discarded before its decode is paid. Zero means uncapped.
    pub max_capture_fps: u32,
    pub image_runtime: RuntimePolicy,
    pub video_runtime: RuntimePolicy,
    pub web_runtime: RuntimePolicy,
    /// An explicitly configured browser executable, which leads the
    /// Chromium-family order when set.
    pub browser_path: Option<PathBuf>,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            handshake_deadline: Duration::from_secs(5),
            first_frame_deadline: Duration::from_secs(10),
            max_restarts: 1,
            slots: DEFAULT_SLOTS,
            max_capture_fps: 30,
            image_runtime: RuntimePolicy::Auto,
            video_runtime: RuntimePolicy::Auto,
            web_runtime: RuntimePolicy::Auto,
            browser_path: None,
        }
    }
}

impl MediaConfig {
    pub fn policy_for(&self, kind: ContentKind) -> RuntimePolicy {
        match kind {
            ContentKind::AnimatedImage => self.image_runtime,
            ContentKind::Video => self.video_runtime,
            ContentKind::Web => self.web_runtime,
        }
    }
}

/// What the application is told about a session. Frames arrive already
/// copied out of shared memory, so the ring slot is released immediately and
/// a slow UI can never stall a worker.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Ready {
        session: SessionId,
        overlay: OverlayId,
        runtime: RuntimeId,
    },
    Frame {
        session: SessionId,
        overlay: OverlayId,
        generation: RenderGeneration,
        sequence: u64,
        width: u32,
        height: u32,
        /// Tightly packed RGBA8, ready for an image handle or a texture.
        rgba: std::sync::Arc<Vec<u8>>,
    },
    /// Where a session's playback has reached, as the content reports it.
    Progress {
        session: SessionId,
        overlay: OverlayId,
        progress: PlaybackProgress,
    },
    Warning {
        session: SessionId,
        overlay: OverlayId,
        warning: MediaWarning,
    },
    Failed {
        session: SessionId,
        overlay: OverlayId,
        error: MediaError,
        /// True when the overlay has fallen through to its poster or the PDF
        /// page and no further runtime will be tried.
        exhausted: bool,
    },
    Closed {
        session: SessionId,
    },
}

/// One bounded supervisor drain.
pub struct PollBatch {
    pub events: Vec<SessionEvent>,
    pub more: bool,
}

/// One live session: its ring, the runtime hosting it and its place in the
/// fallback chain.
///
/// The worker is *not* here. One worker process hosts every session on its
/// runtime, so it is owned by the supervisor and found through `runtime`; a
/// browser costs a process and a hundred megabytes, and four overlays on a
/// slide should not cost four of them.
struct Session {
    id: SessionId,
    overlay: OverlayId,
    generation: RenderGeneration,
    kind: ContentKind,
    spec: SessionSpec,
    runtime: RuntimeId,
    fallbacks: Vec<RuntimeId>,
    ring: SurfaceRing,
    opened_at: Instant,
    first_frame_seen: bool,
    last_sequence: u64,
    restarts: u32,
    attempts: Vec<Attempt>,
}

/// A worker process and the thread draining its stdout.
struct Worker {
    runtime: RuntimeId,
    child: Child,
    stdin: std::io::BufWriter<ChildStdin>,
    events: Receiver<Result<MediaEvent, String>>,
}

impl Worker {
    fn spawn(
        runtime: RuntimeId,
        command: &WorkerCommand,
        wakeup: WakeupSink,
    ) -> Result<Worker, MediaError> {
        let mut child = command
            .build()
            .and_then(|mut command| command.spawn())
            .map_err(|e| {
                MediaError::new(
                    MediaErrorKind::LaunchFailed,
                    format!("could not start the {runtime} worker: {e}"),
                )
                .with_runtime(runtime)
            })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            MediaError::new(MediaErrorKind::LaunchFailed, "the worker has no stdin")
                .with_runtime(runtime)
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            MediaError::new(MediaErrorKind::LaunchFailed, "the worker has no stdout")
                .with_runtime(runtime)
        })?;

        let (sender, events): (Sender<Result<MediaEvent, String>>, _) = channel();
        std::thread::Builder::new()
            .name(format!("{runtime}-events"))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_message::<MediaEvent>(&mut reader) {
                        Ok(event) => {
                            if sender.send(Ok(event)).is_err() {
                                break;
                            }
                            wakeup.ring();
                        }
                        Err(crate::protocol::ProtocolError::Closed) => break,
                        Err(e) => {
                            let _ = sender.send(Err(e.to_string()));
                            wakeup.ring();
                            break;
                        }
                    }
                }
            })
            .map_err(|e| {
                MediaError::new(
                    MediaErrorKind::LaunchFailed,
                    format!("could not read from the {runtime} worker: {e}"),
                )
            })?;

        let mut worker = Worker {
            runtime,
            child,
            // Buffered so each request is one write syscall, not three
            // (length prefix, payload, flush). `write_message` still flushes,
            // so nothing sits in the buffer between requests.
            stdin: std::io::BufWriter::new(stdin),
            events,
        };
        worker.send(&MediaRequest::Hello {
            version: MEDIA_PROTOCOL_VERSION,
        })?;
        Ok(worker)
    }

    fn send(&mut self, request: &MediaRequest) -> Result<(), MediaError> {
        write_message(&mut self.stdin, request).map_err(|e| {
            MediaError::new(
                MediaErrorKind::Crashed,
                format!("the {} worker stopped listening: {e}", self.runtime),
            )
            .with_runtime(self.runtime)
        })
    }

    fn drain(&mut self, limit: usize) -> (Vec<MediaEvent>, Option<String>, bool) {
        let mut events = Vec::new();
        let mut fault = None;
        while events.len() < limit {
            match self.events.try_recv() {
                Ok(Ok(event)) => events.push(event),
                Ok(Err(problem)) => {
                    fault = Some(problem);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The reader thread ended: the worker's stdout closed.
                    fault = Some("the worker exited".to_string());
                    break;
                }
            }
        }
        let more = events.len() == limit;
        (events, fault, more)
    }

    /// Ask the worker to stop, and give it time to actually do it.
    ///
    /// The grace period is the whole point. A worker's own shutdown is not
    /// instant: it closes each browser over CDP and waits for the process to
    /// go before deleting the private profile it created. Killing it the
    /// instant after `Shutdown` is written — which is what a bare `try_wait`
    /// does, because no process exits that fast — skips every destructor it
    /// has. The browser is then never told to close and is never killed
    /// either: it is reparented and survives for the life of the machine,
    /// with its profile directory still on disk. Repeated over a few
    /// sessions that is gigabytes of `/tmp` and a screenful of orphans.
    ///
    /// So: poll for the worker to exit on its own, and use `kill` only for
    /// one that will not. The deadline is bounded because this runs on the
    /// way out of the application, where an unbounded wait would look like a
    /// hang.
    fn shutdown(&mut self) {
        let _ = write_message(&mut self.stdin, &MediaRequest::Shutdown);
        let _ = self.stdin.flush();
        let deadline = Instant::now() + WORKER_EXIT_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                // Either it is ignoring us or we cannot tell; either way it
                // does not get to outlive the application.
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The application-facing supervisor.
pub struct MediaSupervisor {
    config: MediaConfig,
    namer: RingNamer,
    sessions: HashMap<SessionId, Session>,
    /// One worker process per runtime, shared by its sessions and kept warm
    /// between them: a slide change should not restart a browser.
    workers: HashMap<RuntimeId, Worker>,
    next_session: u64,
    next_surface: u64,
    /// Probe results, gathered once per runtime per application start.
    probes: HashMap<RuntimeId, RuntimeProbe>,
    /// The last selection made for each overlay, for preflight reporting.
    selections: HashMap<OverlayId, Selection>,
    pending: Vec<SessionEvent>,
    wakeup: WakeupSink,
    wakeup_inbox: Option<Arc<MediaWakeup>>,
    /// The latest pipeline totals each worker reported about itself.
    worker_counters: HashMap<RuntimeId, WorkerCounters>,
    /// Frames copied out of shared memory and handed to the application.
    frames_forwarded: u64,
    /// Frames superseded within a single drain and released uncopied.
    frames_coalesced: u64,
}

impl MediaSupervisor {
    /// Build a supervisor and discover what this machine can actually play.
    ///
    /// Probing happens here rather than being left to the caller because a
    /// supervisor with no probes silently selects *nothing*: every overlay
    /// reports "no installed runtime can show this content", including the
    /// built-in decoder that needs nothing installed. That is exactly the bug
    /// this shipped with once. Tests that want to control the answers use
    /// [`MediaSupervisor::unprobed`] and say so.
    pub fn new(config: MediaConfig) -> Self {
        let browser = config.browser_path.clone();
        let mut supervisor = Self::unprobed(config);
        for probe in crate::runtime::probe_all(browser.as_deref()) {
            tracing::debug!(
                runtime = %probe.id,
                available = probe.is_available(),
                detail = probe.availability.detail(),
                "probed media runtime"
            );
            supervisor.record_probe(probe);
        }
        supervisor
    }

    /// A supervisor that knows nothing until it is told. Tests only.
    pub fn unprobed(config: MediaConfig) -> Self {
        let (signal, inbox) = sync_channel(1);
        Self {
            config,
            namer: RingNamer::new(),
            sessions: HashMap::new(),
            workers: HashMap::new(),
            next_session: 1,
            next_surface: 1,
            probes: HashMap::new(),
            selections: HashMap::new(),
            pending: Vec::new(),
            wakeup: WakeupSink(signal),
            wakeup_inbox: Some(Arc::new(MediaWakeup {
                inbox: Mutex::new(inbox),
            })),
            worker_counters: HashMap::new(),
            frames_forwarded: 0,
            frames_coalesced: 0,
        }
    }

    pub fn config(&self) -> &MediaConfig {
        &self.config
    }

    /// Take the application event loop's sole doorbell listener.
    pub fn take_wakeup(&mut self) -> Option<Arc<MediaWakeup>> {
        self.wakeup_inbox.take()
    }

    fn push_pending(&mut self, event: SessionEvent) {
        self.pending.push(event);
        self.wakeup.ring();
    }

    /// Record a probe result. Probing itself is the registry's job; the
    /// supervisor only consumes the answers.
    /// Have any runtime probes been recorded yet? Selection with no probes
    /// silently falls through to the static poster, so a caller deferring
    /// probing off the startup path must not open sessions before this is
    /// true.
    pub fn probed(&self) -> bool {
        !self.probes.is_empty()
    }

    pub fn record_probe(&mut self, probe: RuntimeProbe) {
        self.probes.insert(probe.id, probe);
    }

    pub fn probe(&self, runtime: RuntimeId) -> Option<&RuntimeProbe> {
        self.probes.get(&runtime)
    }

    pub fn probes(&self) -> impl Iterator<Item = &RuntimeProbe> {
        self.probes.values()
    }

    /// Rank runtimes for one overlay without opening anything, which is what
    /// preflight needs.
    pub fn plan(
        &mut self,
        overlay: OverlayId,
        kind: ContentKind,
        request: &CapabilityRequest,
    ) -> Selection {
        let probes = &self.probes;
        let selection =
            crate::selection::select(kind, self.config.policy_for(kind), request, |runtime| {
                probes.get(&runtime).cloned().unwrap_or_else(|| {
                    RuntimeProbe::unavailable(runtime, crate::capability::Availability::NotBuilt)
                })
            });
        self.selections.insert(overlay, selection.clone());
        selection
    }

    pub fn selection(&self, overlay: OverlayId) -> Option<&Selection> {
        self.selections.get(&overlay)
    }

    /// Open a session for an overlay, launching the first capable runtime.
    ///
    /// Returns the session identifier even when the launch fails, so the
    /// caller has one handle to report against; a failure arrives through
    /// [`MediaSupervisor::poll`] as a `Failed` event.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        overlay: OverlayId,
        generation: RenderGeneration,
        kind: ContentKind,
        source: crate::protocol::SessionSource,
        viewport: Viewport,
        playback: pulpit_core::overlay::PlaybackParams,
        request: &CapabilityRequest,
        command_for: impl Fn(RuntimeId) -> WorkerCommand,
    ) -> SessionId {
        let session_id = SessionId(self.next_session);
        self.next_session += 1;
        let surface = SurfaceId(self.next_surface);
        self.next_surface += 1;

        let selection = self.plan(overlay, kind, request);
        let Some(runtime) = selection.selected else {
            self.push_pending(SessionEvent::Failed {
                session: session_id,
                overlay,
                error: MediaError::new(
                    MediaErrorKind::Unavailable,
                    "no installed runtime can show this content",
                ),
                exhausted: true,
            });
            return session_id;
        };

        let slot_bytes = (viewport.width as u64) * (viewport.height as u64) * 4;
        let spec = SessionSpec {
            session: session_id,
            surface,
            generation,
            overlay,
            kind,
            source,
            viewport,
            playback,
            ring_name: self.namer.next_name(),
            slots: self.config.slots,
            slot_bytes,
            max_fps: self.config.max_capture_fps,
        };

        match self.launch(&spec, runtime, &command_for) {
            Ok(ring) => {
                self.sessions.insert(
                    session_id,
                    Session {
                        id: session_id,
                        overlay,
                        generation,
                        kind,
                        spec,
                        runtime,
                        fallbacks: selection.fallbacks.clone(),
                        ring,
                        opened_at: Instant::now(),
                        first_frame_seen: false,
                        last_sequence: 0,
                        restarts: 0,
                        attempts: selection.attempts.clone(),
                    },
                );
            }
            Err(error) => {
                self.fail_or_fall_back(
                    session_id,
                    overlay,
                    generation,
                    kind,
                    spec,
                    selection.fallbacks.clone(),
                    selection.attempts.clone(),
                    error,
                    &command_for,
                );
            }
        }
        session_id
    }

    /// Open a session on its runtime's worker, starting that worker if it is
    /// not already running.
    ///
    /// A worker that will not accept the request is discarded rather than kept
    /// in the table: it has already failed once, and the next session would
    /// otherwise inherit it.
    fn launch(
        &mut self,
        spec: &SessionSpec,
        runtime: RuntimeId,
        command_for: &impl Fn(RuntimeId) -> WorkerCommand,
    ) -> Result<SurfaceRing, MediaError> {
        spec.validate().map_err(|e| {
            MediaError::new(MediaErrorKind::ProtocolViolation, e.to_string()).with_runtime(runtime)
        })?;
        let ring =
            SurfaceRing::create(&spec.ring_name, spec.slots, spec.slot_bytes).map_err(|e| {
                MediaError::new(
                    MediaErrorKind::ResourceLimit,
                    format!("could not allocate a surface: {e}"),
                )
                .with_runtime(runtime)
            })?;
        if let std::collections::hash_map::Entry::Vacant(slot) = self.workers.entry(runtime) {
            slot.insert(Worker::spawn(
                runtime,
                &command_for(runtime),
                self.wakeup.clone(),
            )?);
        }
        let worker = self.workers.get_mut(&runtime).expect("just inserted");
        if let Err(error) = worker.send(&MediaRequest::Open(Box::new(spec.clone()))) {
            self.workers.remove(&runtime);
            return Err(error);
        }
        Ok(ring)
    }

    /// Try the next candidate, or report exhaustion.
    ///
    /// A policy denial stops the chain immediately: looking for a runtime
    /// that enforces less is precisely what the denial forbids.
    #[allow(clippy::too_many_arguments)]
    fn fail_or_fall_back(
        &mut self,
        session_id: SessionId,
        overlay: OverlayId,
        generation: RenderGeneration,
        kind: ContentKind,
        spec: SessionSpec,
        mut fallbacks: Vec<RuntimeId>,
        mut attempts: Vec<Attempt>,
        error: MediaError,
        command_for: &impl Fn(RuntimeId) -> WorkerCommand,
    ) {
        if let Some(failed) = error.runtime {
            for attempt in attempts.iter_mut() {
                if attempt.runtime == failed {
                    attempt.outcome = AttemptOutcome::Failed {
                        kind: error.kind,
                        detail: error.message.clone(),
                    };
                }
            }
        }

        if !error.kind.allows_fallback() || fallbacks.is_empty() {
            self.selections.insert(
                overlay,
                Selection {
                    selected: None,
                    fallbacks: Vec::new(),
                    attempts,
                },
            );
            self.push_pending(SessionEvent::Failed {
                session: session_id,
                overlay,
                error,
                exhausted: true,
            });
            return;
        }

        let next = fallbacks.remove(0);
        // Warn about the failed candidate, then try the next one. The last
        // good frame, if any, is still on screen throughout.
        self.push_pending(SessionEvent::Failed {
            session: session_id,
            overlay,
            error: error.clone(),
            exhausted: false,
        });

        let mut spec = spec;
        spec.ring_name = self.namer.next_name();
        match self.launch(&spec, next, command_for) {
            Ok(ring) => {
                for attempt in attempts.iter_mut() {
                    if attempt.runtime == next {
                        attempt.outcome = AttemptOutcome::Selected;
                    }
                }
                self.selections.insert(
                    overlay,
                    Selection {
                        selected: Some(next),
                        fallbacks: fallbacks.clone(),
                        attempts: attempts.clone(),
                    },
                );
                self.sessions.insert(
                    session_id,
                    Session {
                        id: session_id,
                        overlay,
                        generation,
                        kind,
                        spec,
                        runtime: next,
                        fallbacks,
                        ring,
                        opened_at: Instant::now(),
                        first_frame_seen: false,
                        last_sequence: 0,
                        restarts: 0,
                        attempts,
                    },
                );
            }
            Err(error) => self.fail_or_fall_back(
                session_id,
                overlay,
                generation,
                kind,
                spec,
                fallbacks,
                attempts,
                error,
                command_for,
            ),
        }
    }

    /// Close one session. Its worker stays running for the sessions still on
    /// it, and for the next one to open — a warm browser is the point of
    /// sharing it.
    pub fn close(&mut self, session: SessionId) {
        if let Some(session) = self.sessions.remove(&session) {
            if let Some(worker) = self.workers.get_mut(&session.runtime) {
                let _ = worker.send(&MediaRequest::Close {
                    session: session.id,
                });
            }
            self.push_pending(SessionEvent::Closed {
                session: session.id,
            });
        }
    }

    /// Close every session belonging to a retired generation.
    pub fn retire_generation(&mut self, current: RenderGeneration) {
        let stale: Vec<SessionId> = self
            .sessions
            .values()
            .filter(|session| !session.generation.is_current_for(current))
            .map(|session| session.id)
            .collect();
        for session in stale {
            self.close(session);
        }
    }

    pub fn set_active(&mut self, session: SessionId, active: bool) {
        self.request(session, MediaRequest::SetActive { session, active });
    }

    pub fn set_focus(&mut self, session: SessionId, focused: bool) {
        self.request(session, MediaRequest::SetFocus { session, focused });
    }

    pub fn set_viewport(&mut self, session: SessionId, viewport: Viewport) {
        if viewport.validate().is_err() {
            return;
        }
        if let Some(state) = self.sessions.get_mut(&session) {
            state.spec.viewport = viewport;
        }
        self.request(session, MediaRequest::SetViewport { session, viewport });
    }

    pub fn input(&mut self, session: SessionId, event: InputEvent) {
        if event.validate().is_err() {
            return;
        }
        self.request(session, MediaRequest::Input { session, event });
    }

    pub fn image_command(&mut self, session: SessionId, command: ImageCommand) {
        self.typed_command(
            session,
            ContentKind::AnimatedImage,
            MediaRequest::Image { session, command },
        );
    }

    pub fn video_command(&mut self, session: SessionId, command: VideoCommand) {
        self.typed_command(
            session,
            ContentKind::Video,
            MediaRequest::Video { session, command },
        );
    }

    pub fn web_command(&mut self, session: SessionId, command: WebCommand) {
        self.typed_command(
            session,
            ContentKind::Web,
            MediaRequest::Web { session, command },
        );
    }

    /// Content-specific commands are checked against the session's kind here
    /// so a worker never has to answer a command it cannot understand.
    fn typed_command(&mut self, session: SessionId, kind: ContentKind, request: MediaRequest) {
        match self.sessions.get(&session) {
            Some(state) if state.kind == kind => self.request(session, request),
            Some(state) => tracing::warn!(
                %session,
                expected = ?kind,
                actual = ?state.kind,
                "refused a command the session's content kind cannot answer"
            ),
            None => {}
        }
    }

    fn request(&mut self, session: SessionId, request: MediaRequest) {
        let Some(state) = self.sessions.get(&session) else {
            return;
        };
        let (runtime, overlay) = (state.runtime, state.overlay);
        let Some(worker) = self.workers.get_mut(&runtime) else {
            return;
        };
        if let Err(error) = worker.send(&request) {
            self.push_pending(SessionEvent::Failed {
                session,
                overlay,
                error,
                exhausted: false,
            });
        }
    }

    /// Drain everything the workers have produced since the last call.
    ///
    /// Each worker is drained once and its events routed to the sessions they
    /// name, because one worker now speaks for several of them.
    pub fn poll(&mut self, command_for: impl Fn(RuntimeId) -> WorkerCommand) -> Vec<SessionEvent> {
        self.poll_limit(command_for, usize::MAX).events
    }

    /// Poll with a per-worker message budget, yielding before a burst can
    /// monopolise the application event loop.
    pub fn poll_bounded(
        &mut self,
        command_for: impl Fn(RuntimeId) -> WorkerCommand,
        limit: usize,
    ) -> PollBatch {
        self.poll_limit(command_for, limit)
    }

    fn poll_limit(
        &mut self,
        command_for: impl Fn(RuntimeId) -> WorkerCommand,
        limit: usize,
    ) -> PollBatch {
        let mut out = std::mem::take(&mut self.pending);
        let mut casualties: Vec<(SessionId, MediaError)> = Vec::new();
        let mut more = false;

        let runtimes: Vec<RuntimeId> = self.workers.keys().copied().collect();
        for runtime in runtimes {
            let Some(worker) = self.workers.get_mut(&runtime) else {
                continue;
            };
            let (events, fault, worker_more) = worker.drain(limit);
            more |= worker_more;

            // Within one drain, only a session's newest complete frame is
            // worth copying: every frame is a full replacement, so an older
            // one in the same batch would cost a copy and an image handle
            // only to be overwritten before it is ever shown. Superseded
            // frames still release their ring slot, or the worker would run
            // out of slots and start dropping.
            let newest = newest_frames(&events);

            for event in events {
                if let MediaEvent::Counters(counters) = &event {
                    self.worker_counters.insert(runtime, *counters);
                    continue;
                }
                if let MediaEvent::FrameReady(frame) = &event {
                    if newest.get(&frame.session) != Some(&frame.sequence) {
                        self.frames_coalesced += 1;
                        if let Some(worker) = self.workers.get_mut(&runtime) {
                            let _ = worker.send(&MediaRequest::ReleaseFrame {
                                session: frame.session,
                                slot: frame.slot,
                                sequence: frame.sequence,
                            });
                        }
                        continue;
                    }
                }
                let Some(id) = event_session(&event) else {
                    // A failure that names no session is the worker itself
                    // failing, so it belongs to all of them rather than to
                    // none: dropped here, nothing would ever report it.
                    if let MediaEvent::Failed { error, .. } = event {
                        for session in self.sessions.values() {
                            if session.runtime == runtime {
                                casualties.push((session.id, error.clone()));
                            }
                        }
                    }
                    continue;
                };
                let Some(session) = self.sessions.get_mut(&id) else {
                    continue;
                };
                // The release goes back to the worker after the session's
                // borrow ends; both live in this same supervisor.
                let release = Self::handle_event(session, event, &mut out, &mut casualties);
                if let Some(release) = release {
                    // A release comes back exactly when a frame was copied
                    // out and forwarded.
                    self.frames_forwarded += 1;
                    if let Some(worker) = self.workers.get_mut(&runtime) {
                        let _ = worker.send(&release);
                    }
                }
            }

            if let Some(problem) = fault {
                // The process carrying several sessions has gone: every one of
                // them is a casualty, and the worker is dropped so the next
                // open starts a fresh one.
                self.workers.remove(&runtime);
                for session in self.sessions.values() {
                    if session.runtime == runtime {
                        casualties.push((
                            session.id,
                            MediaError::new(MediaErrorKind::Crashed, problem.clone())
                                .with_runtime(runtime),
                        ));
                    }
                }
            }
        }

        for session in self.sessions.values() {
            if !session.first_frame_seen
                && session.opened_at.elapsed() > self.config.first_frame_deadline
                && !casualties.iter().any(|(id, _)| *id == session.id)
            {
                casualties.push((
                    session.id,
                    MediaError::new(
                        MediaErrorKind::TimedOut,
                        "the runtime produced no frame before its deadline",
                    )
                    .with_runtime(session.runtime),
                ));
            }
        }

        for (id, error) in casualties {
            self.recover(id, error, &command_for, &mut out);
        }
        PollBatch { events: out, more }
    }

    /// Apply one worker event to the session it names, returning a request the
    /// caller must send back — the frame release, which cannot be sent from
    /// here because the worker is borrowed elsewhere.
    fn handle_event(
        session: &mut Session,
        event: MediaEvent,
        out: &mut Vec<SessionEvent>,
        casualties: &mut Vec<(SessionId, MediaError)>,
    ) -> Option<MediaRequest> {
        match event {
            MediaEvent::Hello(_) | MediaEvent::ProbeResult(_) => {}
            MediaEvent::Ready { session: id } if id == session.id => {
                out.push(SessionEvent::Ready {
                    session: session.id,
                    overlay: session.overlay,
                    runtime: session.runtime,
                })
            }
            MediaEvent::FrameReady(frame) => {
                if frame.session != session.id {
                    return None;
                }
                // Every field that will size a copy is checked before the
                // copy. A corrupt frame is discarded, leaving the current
                // one on screen untouched.
                if let Err(e) = frame.validate(session.spec.slot_bytes, session.spec.slots) {
                    tracing::warn!(session = %session.id, error = %e, "discarded a malformed frame");
                    return None;
                }
                if frame.sequence <= session.last_sequence && session.first_frame_seen {
                    return None;
                }
                let Ok(bytes) = session.ring.read_slot(frame.slot, frame.bytes) else {
                    return None;
                };
                let mut rgba = bytes.to_vec();
                if !frame.format.is_rgba_order() {
                    for pixel in rgba.as_chunks_mut::<4>().0 {
                        pixel.swap(0, 2);
                    }
                }
                session.last_sequence = frame.sequence;
                session.first_frame_seen = true;
                out.push(SessionEvent::Frame {
                    session: session.id,
                    overlay: session.overlay,
                    generation: session.generation,
                    sequence: frame.sequence,
                    width: frame.width,
                    height: frame.height,
                    rgba: std::sync::Arc::new(rgba),
                });
                // The slot is released as soon as the pixels are copied, so a
                // slow UI never stalls the worker.
                return Some(MediaRequest::ReleaseFrame {
                    session: session.id,
                    slot: frame.slot,
                    sequence: frame.sequence,
                });
            }
            MediaEvent::Progress { progress, .. } => out.push(SessionEvent::Progress {
                session: session.id,
                overlay: session.overlay,
                progress,
            }),
            MediaEvent::Warning { warning, .. } => out.push(SessionEvent::Warning {
                session: session.id,
                overlay: session.overlay,
                warning,
            }),
            MediaEvent::Failed { error, .. } => casualties.push((session.id, error)),
            MediaEvent::Closed { session: id } if id == session.id => {
                out.push(SessionEvent::Closed {
                    session: session.id,
                })
            }
            _ => {}
        }
        None
    }

    /// A session failed. Restart the same worker once for a transient fault,
    /// then fall through the remaining candidates, then go static.
    fn recover(
        &mut self,
        id: SessionId,
        error: MediaError,
        command_for: &impl Fn(RuntimeId) -> WorkerCommand,
        out: &mut Vec<SessionEvent>,
    ) {
        let Some(session) = self.sessions.remove(&id) else {
            return;
        };
        let Session {
            overlay,
            generation,
            kind,
            mut spec,
            runtime,
            fallbacks,
            restarts,
            attempts,
            first_frame_seen,
            ..
        } = session;

        let error = MediaError {
            runtime: error.runtime.or(Some(runtime)),
            overlay: Some(overlay),
            generation: Some(generation),
            ..error
        };

        if error.kind.is_transient() && restarts < self.config.max_restarts {
            spec.ring_name = self.namer.next_name();
            if let Ok(ring) = self.launch(&spec, runtime, command_for) {
                if first_frame_seen {
                    // Restarting a web runtime loses arbitrary JavaScript
                    // state; the presenter is told rather than left to guess.
                    out.push(SessionEvent::Warning {
                        session: id,
                        overlay,
                        warning: MediaWarning::ContentRestarted,
                    });
                }
                self.sessions.insert(
                    id,
                    Session {
                        id,
                        overlay,
                        generation,
                        kind,
                        spec,
                        runtime,
                        fallbacks,
                        ring,
                        opened_at: Instant::now(),
                        first_frame_seen: false,
                        last_sequence: 0,
                        restarts: restarts + 1,
                        attempts,
                    },
                );
                return;
            }
        }

        let mut fallback_events = Vec::new();
        std::mem::swap(&mut self.pending, &mut fallback_events);
        self.fail_or_fall_back(
            id,
            overlay,
            generation,
            kind,
            spec,
            fallbacks,
            attempts,
            error,
            command_for,
        );
        std::mem::swap(&mut self.pending, &mut fallback_events);
        out.extend(fallback_events);
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn runtime_of(&self, session: SessionId) -> Option<RuntimeId> {
        self.sessions.get(&session).map(|state| state.runtime)
    }

    pub fn overlay_of(&self, session: SessionId) -> Option<OverlayId> {
        self.sessions.get(&session).map(|state| state.overlay)
    }

    /// The viewport a session is currently rendering into.
    ///
    /// The application needs it to turn a fraction of an overlay into the
    /// CSS pixels a page's own event handlers expect.
    pub fn viewport_of(&self, session: SessionId) -> Option<Viewport> {
        self.sessions.get(&session).map(|state| state.spec.viewport)
    }

    /// Close every session and reap every worker.
    pub fn shutdown(&mut self) {
        self.sessions.clear();
        for (_, mut worker) in self.workers.drain() {
            worker.shutdown();
        }
    }

    /// How many worker processes are running. One per runtime in use, however
    /// many overlays are on the slide.
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// The latest pipeline totals reported by the workers, summed. Frames
    /// discarded before decode appear here and nowhere else: they cost the
    /// application nothing, which is the point of counting them.
    pub fn worker_counters(&self) -> WorkerCounters {
        let mut total = WorkerCounters::default();
        for counters in self.worker_counters.values() {
            total.cdp_frames_received += counters.cdp_frames_received;
            total.frames_discarded_before_decode += counters.frames_discarded_before_decode;
            total.frames_decoded += counters.frames_decoded;
            total.frames_scaled += counters.frames_scaled;
            total.frames_scale_elided += counters.frames_scale_elided;
            total.frames_published += counters.frames_published;
            total.ring_dropped += counters.ring_dropped;
        }
        total
    }

    /// Frames copied out of shared memory and handed to the application.
    pub fn frames_forwarded(&self) -> u64 {
        self.frames_forwarded
    }

    /// Frames superseded within one drain and released without a copy.
    pub fn frames_coalesced(&self) -> u64 {
        self.frames_coalesced
    }

    /// Shared-memory reservation across all live sessions' rings, in bytes.
    /// Address space, not necessarily resident pages.
    pub fn ring_bytes(&self) -> u64 {
        self.sessions
            .values()
            .map(|session| u64::from(session.spec.slots) * session.spec.slot_bytes)
            .sum()
    }
}

/// The newest frame sequence per session within one drained batch. Frames
/// below their session's entry are superseded and can be released uncopied;
/// non-frame events are not involved and keep their order.
fn newest_frames(events: &[MediaEvent]) -> HashMap<SessionId, u64> {
    let mut newest = HashMap::new();
    for event in events {
        if let MediaEvent::FrameReady(frame) = event {
            let best = newest.entry(frame.session).or_insert(frame.sequence);
            *best = (*best).max(frame.sequence);
        }
    }
    newest
}

/// Which session a worker event is about, if any.
///
/// A worker speaks for several sessions now, so every event has to be routed
/// rather than assumed to belong to the one that was drained.
fn event_session(event: &MediaEvent) -> Option<SessionId> {
    match event {
        MediaEvent::Hello(_) | MediaEvent::ProbeResult(_) | MediaEvent::Counters(_) => None,
        MediaEvent::Ready { session }
        | MediaEvent::Closed { session }
        | MediaEvent::StateChanged { session, .. }
        | MediaEvent::Progress { session, .. }
        | MediaEvent::CursorChanged { session, .. }
        | MediaEvent::WebMessage { session, .. } => Some(*session),
        MediaEvent::FrameReady(frame) => Some(frame.session),
        MediaEvent::Warning { session, .. } | MediaEvent::Failed { session, .. } => *session,
    }
}

impl Drop for MediaSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The pixel format the application receives after the supervisor's
/// normalisation. Workers may publish either byte order; the supervisor
/// swaps so the UI only ever sees RGBA.
pub const DELIVERED_FORMAT: PixelFormat = PixelFormat::Rgba8Premultiplied;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Availability, ContentCapabilities, InputCapabilities};

    fn available(id: RuntimeId, kinds: &[ContentKind]) -> RuntimeProbe {
        RuntimeProbe {
            content: ContentCapabilities {
                kinds: kinds.to_vec(),
                continuous_frames: true,
                ..Default::default()
            },
            input: InputCapabilities {
                pointer: true,
                ..Default::default()
            },
            ..RuntimeProbe::unavailable(id, Availability::Available)
        }
    }

    #[test]
    fn pending_events_ring_the_one_slot_doorbell() {
        let mut supervisor = MediaSupervisor::unprobed(MediaConfig::default());
        let wakeup = supervisor.take_wakeup().unwrap();
        supervisor.push_pending(SessionEvent::Closed {
            session: SessionId(7),
        });
        supervisor.push_pending(SessionEvent::Closed {
            session: SessionId(8),
        });

        assert_eq!(wakeup.wait(Duration::ZERO), Wakeup::Ring);
        assert_eq!(wakeup.wait(Duration::ZERO), Wakeup::Idle);
    }

    #[test]
    fn only_the_newest_frame_per_session_survives_one_drain() {
        use crate::protocol::{SurfaceFrame, SurfaceSlot};
        let frame = |session: u64, sequence: u64| {
            MediaEvent::FrameReady(SurfaceFrame {
                session: SessionId(session),
                surface: crate::protocol::SurfaceId(1),
                sequence,
                presentation_time: None,
                width: 64,
                height: 64,
                stride: 64 * 4,
                format: PixelFormat::Rgba8Straight,
                damage: Vec::new(),
                slot: SurfaceSlot(0),
                bytes: 64 * 64 * 4,
            })
        };
        let events = vec![
            frame(1, 4),
            frame(2, 9),
            frame(1, 5),
            MediaEvent::Ready {
                session: SessionId(1),
            },
            frame(1, 6),
        ];
        let newest = newest_frames(&events);
        assert_eq!(newest.get(&SessionId(1)), Some(&6));
        assert_eq!(newest.get(&SessionId(2)), Some(&9));
        // Frames below the newest are the coalesced ones; the newest itself
        // and every non-frame event pass through untouched.
        let survivors: Vec<u64> = events
            .iter()
            .filter_map(|event| match event {
                MediaEvent::FrameReady(frame)
                    if newest.get(&frame.session) == Some(&frame.sequence) =>
                {
                    Some(frame.sequence)
                }
                _ => None,
            })
            .collect();
        assert_eq!(survivors, vec![9, 6]);
    }

    #[test]
    fn a_supervisor_with_no_probes_plans_a_static_fallback() {
        let mut supervisor = MediaSupervisor::unprobed(MediaConfig::default());
        let selection = supervisor.plan(
            OverlayId(1),
            ContentKind::Web,
            &CapabilityRequest::for_kind(ContentKind::Web),
        );
        assert!(selection.is_static_fallback());
        assert!(selection
            .attempts
            .iter()
            .all(|attempt| matches!(attempt.outcome, AttemptOutcome::Skipped(_))));
    }

    #[test]
    fn planning_records_the_selection_for_preflight() {
        let mut supervisor = MediaSupervisor::unprobed(MediaConfig::default());
        supervisor.record_probe(available(
            RuntimeId::ExternalChromium,
            &[ContentKind::AnimatedImage],
        ));
        let selection = supervisor.plan(
            OverlayId(4),
            ContentKind::AnimatedImage,
            &CapabilityRequest::for_kind(ContentKind::AnimatedImage),
        );
        assert_eq!(selection.selected, Some(RuntimeId::ExternalChromium));
        assert_eq!(
            supervisor.selection(OverlayId(4)).and_then(|s| s.selected),
            Some(RuntimeId::ExternalChromium)
        );
    }

    #[test]
    fn opening_with_nothing_installed_reports_exhaustion_without_launching() {
        let mut supervisor = MediaSupervisor::unprobed(MediaConfig::default());
        let session = supervisor.open(
            OverlayId(1),
            RenderGeneration(1),
            ContentKind::Video,
            crate::protocol::SessionSource::File {
                path: "/staged/clip.mp4".into(),
            },
            Viewport::new(640, 360, 1.0),
            Default::default(),
            &CapabilityRequest::for_kind(ContentKind::Video),
            |_| WorkerCommand::Explicit {
                program: PathBuf::from("/nonexistent"),
                args: Vec::new(),
            },
        );
        let events = supervisor.poll(|_| WorkerCommand::Explicit {
            program: PathBuf::from("/nonexistent"),
            args: Vec::new(),
        });
        assert!(matches!(
            events.as_slice(),
            [SessionEvent::Failed {
                exhausted: true,
                session: reported,
                ..
            }] if *reported == session
        ));
        assert_eq!(supervisor.session_count(), 0);
    }

    #[test]
    fn a_worker_that_cannot_be_launched_falls_through_to_the_next_candidate() {
        let mut supervisor = MediaSupervisor::unprobed(MediaConfig::default());
        supervisor.record_probe(available(RuntimeId::ExternalChromium, &[ContentKind::Web]));
        supervisor.record_probe(available(RuntimeId::WebKitGtk, &[ContentKind::Web]));

        supervisor.open(
            OverlayId(2),
            RenderGeneration(1),
            ContentKind::Web,
            crate::protocol::SessionSource::File {
                path: "/staged/page.html".into(),
            },
            Viewport::new(64, 64, 1.0),
            Default::default(),
            &CapabilityRequest::for_kind(ContentKind::Web),
            // Neither worker exists, so both launches fail and the chain is
            // exhausted — but every candidate must have been *tried*.
            |_| WorkerCommand::Explicit {
                program: PathBuf::from("/nonexistent-worker"),
                args: Vec::new(),
            },
        );
        let events = supervisor.poll(|_| WorkerCommand::Explicit {
            program: PathBuf::from("/nonexistent-worker"),
            args: Vec::new(),
        });
        let failures = events
            .iter()
            .filter(|event| matches!(event, SessionEvent::Failed { .. }))
            .count();
        assert_eq!(failures, 2, "both candidates were attempted");
        assert!(matches!(
            events.last(),
            Some(SessionEvent::Failed {
                exhausted: true,
                ..
            })
        ));
        let selection = supervisor.selection(OverlayId(2)).unwrap();
        assert!(selection.is_static_fallback());
        // Every candidate was accounted for: the two that were probed as
        // capable were launched and failed, and any other was skipped with a
        // reason rather than silently dropped.
        assert!(selection.attempts.iter().all(|attempt| matches!(
            attempt.outcome,
            AttemptOutcome::Failed { .. } | AttemptOutcome::Skipped(_)
        )));
        assert_eq!(
            selection
                .attempts
                .iter()
                .filter(|attempt| matches!(attempt.outcome, AttemptOutcome::Failed { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn a_retired_generation_closes_its_sessions() {
        let mut supervisor = MediaSupervisor::unprobed(MediaConfig::default());
        // No sessions exist, so this must be a no-op rather than a panic.
        supervisor.retire_generation(RenderGeneration(5));
        assert_eq!(supervisor.session_count(), 0);
    }

    #[test]
    fn commands_for_an_unknown_session_are_ignored() {
        let mut supervisor = MediaSupervisor::unprobed(MediaConfig::default());
        supervisor.set_active(SessionId(99), true);
        supervisor.video_command(SessionId(99), VideoCommand::Play);
        supervisor.input(SessionId(99), InputEvent::PointerLeft);
        assert!(supervisor
            .poll(|_| WorkerCommand::Explicit {
                program: PathBuf::from("/nonexistent"),
                args: Vec::new(),
            })
            .is_empty());
    }

    #[test]
    fn an_unusable_input_event_never_reaches_a_worker() {
        let mut supervisor = MediaSupervisor::unprobed(MediaConfig::default());
        supervisor.input(
            SessionId(1),
            InputEvent::PointerMoved {
                x: f32::INFINITY,
                y: 0.0,
            },
        );
        assert!(supervisor.pending.is_empty());
    }

    #[test]
    fn the_policy_for_each_content_kind_is_read_from_configuration() {
        let config = MediaConfig {
            image_runtime: RuntimePolicy::Require(RuntimeId::ExternalChromium),
            web_runtime: RuntimePolicy::Prefer(RuntimeId::WebKitGtk),
            ..Default::default()
        };
        assert_eq!(
            config.policy_for(ContentKind::AnimatedImage),
            RuntimePolicy::Require(RuntimeId::ExternalChromium)
        );
        assert_eq!(
            config.policy_for(ContentKind::Web),
            RuntimePolicy::Prefer(RuntimeId::WebKitGtk)
        );
        assert_eq!(config.policy_for(ContentKind::Video), RuntimePolicy::Auto);
    }
}
