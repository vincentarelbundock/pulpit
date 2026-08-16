//! The renderer worker: one PDFium instance, one process.
//!
//! The worker reads requests on stdin and writes responses on stdout. Pixels
//! go into the shared-memory region named in the job. A reader thread keeps
//! parsing while a render is in flight, so a cancel arrives *during* the
//! render and the pause callback can act on it.

use std::collections::VecDeque;
use std::io::{BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::pdf::{BackendDocumentId, PdfBackend, RenderRequest};
use pulpit_core::RenderGeneration;

use crate::protocol::{
    read_message, write_message, Priority, ProtocolError, Quality, RenderJob, Request, RequestId,
    Response, MAX_ATTACHMENT_BYTES, PROTOCOL_VERSION,
};
use crate::shm::AttachedRegion;

#[derive(Default)]
struct Inbox {
    jobs: VecDeque<RenderJob>,
    control: VecDeque<Request>,
    /// Ids cancelled while still queued.
    cancelled: Vec<RequestId>,
    /// Everything older than this is obsolete.
    floor: RenderGeneration,
    /// Jobs dropped from the queue without rendering. The supervisor is
    /// waiting on a reply for every job it dispatched, so each one must be
    /// acknowledged: a silently dropped job is a worker that looks hung.
    unacknowledged: Vec<RequestId>,
    closed: bool,
    shutdown: bool,
}

struct Shared {
    inbox: Mutex<Inbox>,
    signal: Condvar,
    /// Cancellation flag of the job currently being rendered.
    in_flight: Mutex<Option<(RequestId, Arc<AtomicBool>)>>,
}

impl Shared {
    fn cancel_in_flight_if(&self, matches: impl Fn(RequestId) -> bool) {
        if let Some((id, flag)) = self.in_flight.lock().unwrap().as_ref() {
            if matches(*id) {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// Run the worker loop until the peer closes the connection or asks it to
/// shut down. Returns `Ok(())` on a clean shutdown.
pub fn run(
    input: impl Read + Send + 'static,
    output: impl Write,
    mut backend: Box<dyn PdfBackend>,
) -> Result<(), ProtocolError> {
    let shared = Arc::new(Shared {
        inbox: Mutex::new(Inbox::default()),
        signal: Condvar::new(),
        in_flight: Mutex::new(None),
    });

    let reader_shared = Arc::clone(&shared);
    let reader = std::thread::Builder::new()
        .name("render-worker-reader".into())
        .spawn(move || read_requests(input, reader_shared))
        .expect("spawn reader");

    let mut output = BufWriter::new(output);
    let mut documents: Vec<(u64, BackendDocumentId)> = Vec::new();
    // The last page whose link annotations were extracted, kept because the
    // Overlays request for the same page arrives right behind Links and
    // derives from the identical extraction.
    let mut links_cache: Option<(u64, usize, Vec<pulpit_core::PageLink>)> = None;
    // The shared region this worker publishes into, mapped once and reused.
    // Re-opening per frame cost an open + stat + mmap/munmap per render, and
    // every page of the fresh mapping was faulted in again by the copy.
    let mut region: Option<(String, AttachedRegion)> = None;

    loop {
        // Control messages first: they are cheap and may cancel queued work.
        let next = {
            let mut inbox = shared.inbox.lock().unwrap();
            loop {
                // Shutdown stops the loop once nothing is pending: queued
                // work already accepted is finished, and the supervisor
                // kills anything that overstays its welcome.
                if inbox.shutdown && inbox.control.is_empty() && inbox.jobs.is_empty() {
                    break None;
                }
                if let Some(id) = inbox.unacknowledged.pop() {
                    break Some(Work::Acknowledge(id));
                }
                // A frame either window is waiting for pre-empts control
                // *queries*: a capabilities scan or an outline walk can take
                // hundreds of milliseconds, and running it ahead of the
                // committed page's frame is a priority inversion the
                // audience sees. Lifecycle messages are exempt — a render
                // must never jump ahead of the Open it depends on.
                let deferrable_control = matches!(
                    inbox.control.front(),
                    Some(
                        Request::Links { .. }
                            | Request::Overlays { .. }
                            | Request::Navigation { .. }
                            | Request::FindText { .. }
                            | Request::Capabilities { .. }
                            | Request::Attachment { .. }
                    )
                );
                if deferrable_control {
                    if let Some(index) = pick_job(&inbox).filter(|index| {
                        matches!(
                            inbox.jobs[*index].priority,
                            Priority::Audience | Priority::Presenter
                        )
                    }) {
                        let job = inbox.jobs.remove(index).expect("index from the same lock");
                        break Some(Work::Render(job));
                    }
                }
                if let Some(control) = inbox.control.pop_front() {
                    break Some(Work::Control(control));
                }
                // Highest priority, then oldest.
                if let Some(index) = pick_job(&inbox) {
                    let job = inbox.jobs.remove(index).expect("index from the same lock");
                    break Some(Work::Render(job));
                }
                if inbox.closed {
                    break None;
                }
                inbox = shared.signal.wait(inbox).unwrap();
            }
        };

        let Some(work) = next else { break };
        match work {
            Work::Control(Request::Hello { version }) => {
                if version != PROTOCOL_VERSION {
                    write_message(
                        &mut output,
                        &Response::Hello {
                            version: PROTOCOL_VERSION,
                            backend: backend.name().into(),
                            backend_version: backend.version(),
                        },
                    )?;
                    return Err(ProtocolError::VersionMismatch {
                        ours: PROTOCOL_VERSION,
                        theirs: version,
                    });
                }
                write_message(
                    &mut output,
                    &Response::Hello {
                        version: PROTOCOL_VERSION,
                        backend: backend.name().into(),
                        backend_version: backend.version(),
                    },
                )?;
            }
            Work::Control(Request::Open { document, path }) => {
                match backend.open(std::path::Path::new(&path)) {
                    Ok(handle) => {
                        let response = match backend.metadata(handle) {
                            Ok(metadata) => {
                                let notes_pdfpc = read_pdfpc_attachment(
                                    backend.as_ref(),
                                    handle,
                                    std::path::Path::new(&path),
                                );
                                documents.push((document, handle));
                                Response::Opened(crate::protocol::OpenedDocument {
                                    document,
                                    page_count: metadata.page_count,
                                    first_page_size: metadata.first_page_size,
                                    page_sizes: metadata.page_sizes,
                                    page_sizes_sampled: metadata.page_sizes_sampled,
                                    metadata_text: metadata.metadata_text,
                                    notes_pdfpc,
                                })
                            }
                            Err(e) => {
                                backend.close(handle);
                                Response::OpenFailed {
                                    document,
                                    reason: e.to_string(),
                                }
                            }
                        };
                        write_message(&mut output, &response)?;
                    }
                    Err(e) => write_message(
                        &mut output,
                        &Response::OpenFailed {
                            document,
                            reason: e.to_string(),
                        },
                    )?,
                }
            }
            Work::Control(Request::Links { document, page }) => {
                let links = documents
                    .iter()
                    .find(|(id, _)| *id == document)
                    .map(|(_, handle)| backend.links(*handle, page).unwrap_or_default())
                    .unwrap_or_default();
                // The Overlays request for this page follows immediately and
                // derives from these same links.
                links_cache = Some((document, page, links.clone()));
                write_message(
                    &mut output,
                    &Response::Links {
                        document,
                        page,
                        links,
                    },
                )?;
            }
            Work::Control(Request::Overlays { document, page }) => {
                // Links and Overlays for a page are requested back to back
                // and both come from the same annotation extraction; the
                // Links handler above just cached its result so this does
                // not walk the page's annotations a second time, and the
                // cached list is borrowed rather than cloned back out.
                let fresh;
                let links: &[pulpit_core::PageLink] = match &links_cache {
                    Some((cached_document, cached_page, links))
                        if *cached_document == document && *cached_page == page =>
                    {
                        links
                    }
                    _ => {
                        fresh = documents
                            .iter()
                            .find(|(id, _)| *id == document)
                            .map(|(_, handle)| backend.links(*handle, page).unwrap_or_default())
                            .unwrap_or_default();
                        &fresh
                    }
                };
                let (declarations, diagnostics) =
                    crate::pdf::overlays::declarations_from_links(links);
                write_message(
                    &mut output,
                    &Response::Overlays {
                        document,
                        page,
                        declarations,
                        diagnostics,
                    },
                )?;
            }
            Work::Control(Request::Navigation { document }) => {
                // A backend that cannot read labels or bookmarks answers with
                // an empty model: section display then shows nothing, which is
                // exactly what a deck without bookmarks looks like.
                let navigation = documents
                    .iter()
                    .find(|(id, _)| *id == document)
                    .map(|(_, handle)| {
                        pulpit_core::navigation::DocumentNavigation::new(
                            backend.page_labels(*handle).unwrap_or_default(),
                            backend.outline(*handle).unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default();
                write_message(
                    &mut output,
                    &Response::Navigation {
                        document,
                        navigation,
                    },
                )?;
            }
            Work::Control(Request::FindText {
                document,
                generation,
                from_page,
                to_page,
                query,
            }) => {
                let found = documents
                    .iter()
                    .find(|(id, _)| *id == document)
                    .map(|(_, handle)| backend.find_text(*handle, &query, from_page..to_page));
                // Three outcomes, kept apart: hits, "this backend cannot read
                // text", and a document that is not open here. Collapsing the
                // middle one into an empty answer would tell a presenter
                // their deck has no matches when nothing was ever searched.
                let (chunk, searchable) = match found {
                    Some(Ok(hits)) => (
                        pulpit_core::search::HitChunk {
                            from_page,
                            to_page,
                            truncated: hits.len() >= crate::document::limits::MAX_HITS_PER_SEARCH,
                            hits,
                        },
                        true,
                    ),
                    Some(Err(_)) | None => (
                        pulpit_core::search::HitChunk {
                            from_page,
                            to_page,
                            ..Default::default()
                        },
                        false,
                    ),
                };
                write_message(
                    &mut output,
                    &Response::Found {
                        document,
                        generation,
                        chunk,
                        searchable,
                    },
                )?;
            }
            Work::Control(Request::Capabilities { document }) => {
                let capabilities = documents
                    .iter()
                    .find(|(id, _)| *id == document)
                    .and_then(|(_, handle)| backend.evidence(*handle).ok())
                    .map(|evidence| crate::pdf::capabilities::analyse(&evidence))
                    .unwrap_or_default();
                write_message(
                    &mut output,
                    &Response::Capabilities {
                        document,
                        capabilities,
                    },
                )?;
            }
            Work::Control(Request::Attachment { document, name }) => {
                let response = match documents.iter().find(|(id, _)| *id == document) {
                    Some((_, handle)) => match backend.attachment(*handle, &name) {
                        Ok(bytes) if bytes.len() as u64 > MAX_ATTACHMENT_BYTES => {
                            Response::AttachmentFailed {
                                document,
                                name,
                                reason: format!(
                                    "attachment of {} bytes exceeds the {MAX_ATTACHMENT_BYTES} \
                                     byte limit",
                                    bytes.len()
                                ),
                            }
                        }
                        Ok(bytes) => Response::Attachment {
                            document,
                            name,
                            bytes,
                        },
                        Err(e) => Response::AttachmentFailed {
                            document,
                            name,
                            reason: e.to_string(),
                        },
                    },
                    None => Response::AttachmentFailed {
                        document,
                        name,
                        reason: format!("document {document} is not open"),
                    },
                };
                write_message(&mut output, &response)?;
            }
            Work::Control(Request::Close { document }) => {
                if let Some(position) = documents.iter().position(|(id, _)| *id == document) {
                    let (_, handle) = documents.remove(position);
                    backend.close(handle);
                }
            }
            Work::Control(_) => {}
            Work::Acknowledge(id) => {
                write_message(&mut output, &Response::Cancelled { id })?;
            }
            Work::Render(job) => {
                let response = render_one(&*backend, &documents, &shared, &job, &mut region);
                write_message(&mut output, &response)?;
            }
        }
    }

    let _ = reader.join();
    Ok(())
}

enum Work {
    Control(Request),
    Render(RenderJob),
    /// Tell the supervisor a queued job will never be rendered.
    Acknowledge(RequestId),
}

/// Remove every job matching `doomed` and return their ids so the main
/// thread can acknowledge them. A job the supervisor dispatched must always
/// produce exactly one reply.
fn drain_jobs(inbox: &mut Inbox, doomed: impl Fn(&RenderJob) -> bool) -> Vec<RequestId> {
    let mut dropped = Vec::new();
    inbox.jobs.retain(|job| {
        if doomed(job) {
            dropped.push(job.id);
            false
        } else {
            true
        }
    });
    dropped
}

fn pick_job(inbox: &Inbox) -> Option<usize> {
    inbox
        .jobs
        .iter()
        .enumerate()
        .min_by_key(|(index, job)| (job.priority, job.quality_rank(), *index))
        .map(|(index, _)| index)
}

impl RenderJob {
    /// Coarse work goes first within a priority class: something correct on
    /// screen beats something perfect later.
    fn quality_rank(&self) -> u8 {
        match self.quality {
            Quality::Coarse => 0,
            Quality::Refined => 1,
        }
    }
}

fn render_one(
    backend: &dyn PdfBackend,
    documents: &[(u64, BackendDocumentId)],
    shared: &Arc<Shared>,
    job: &RenderJob,
    region: &mut Option<(String, AttachedRegion)>,
) -> Response {
    if let Err(e) = job.validate() {
        return Response::RenderFailed {
            id: job.id,
            generation: job.generation,
            reason: e.to_string(),
        };
    }
    {
        let inbox = shared.inbox.lock().unwrap();
        if job.generation < inbox.floor || inbox.cancelled.contains(&job.id) {
            return Response::Cancelled { id: job.id };
        }
    }
    let Some((_, handle)) = documents.iter().find(|(id, _)| *id == job.document) else {
        return Response::RenderFailed {
            id: job.id,
            generation: job.generation,
            reason: format!("document {} is not open", job.document),
        };
    };

    // A small frame travels inline in the response and never touches the
    // shared region, so the supervisor can leave this worker several of them
    // and each render starts the moment the previous one is sent.
    let bytes = job.byte_len();
    if job.is_inline() {
        let cancel = Arc::new(AtomicBool::new(false));
        *shared.in_flight.lock().unwrap() = Some((job.id, Arc::clone(&cancel)));
        let request = RenderRequest {
            document: *handle,
            page: job.page,
            region: job.region,
            width: job.width,
            height: job.height,
            with_annotations: job.with_annotations,
        };
        let mut pixels = vec![0u8; bytes as usize];
        let started = std::time::Instant::now();
        let result = backend.render_into(&request, &mut pixels, cancel.as_ref());
        let render_micros = started.elapsed().as_micros() as u64;
        *shared.in_flight.lock().unwrap() = None;
        return match result {
            Ok(()) => Response::Rendered {
                id: job.id,
                generation: job.generation,
                width: job.width,
                height: job.height,
                quality: job.quality,
                bytes,
                pixels: Some(pixels),
                render_micros,
            },
            Err(crate::pdf::PdfError::Cancelled) => Response::Cancelled { id: job.id },
            Err(e) => Response::RenderFailed {
                id: job.id,
                generation: job.generation,
                reason: e.to_string(),
            },
        };
    }

    // The frame size is fully determined by the job, so the shared-memory
    // mapping is ensured *before* rendering and the backend draws straight
    // into it: the frame never exists as a separate allocation. The mapping
    // is kept between jobs — the supervisor reuses one region per worker,
    // growing it in place — so remapping happens only on first use or after
    // a growth the current mapping predates.
    let stale = match region {
        Some((name, mapped)) => {
            name != &job.region_name || (mapped.as_mut_slice().len() as u64) < bytes
        }
        None => true,
    };
    if stale {
        match AttachedRegion::open(&job.region_name, bytes) {
            Ok(fresh) => *region = Some((job.region_name.clone(), fresh)),
            Err(e) => {
                return Response::RenderFailed {
                    id: job.id,
                    generation: job.generation,
                    reason: format!("shared memory: {e}"),
                }
            }
        }
    }
    let (_, mapped) = region.as_mut().expect("just ensured");

    let cancel = Arc::new(AtomicBool::new(false));
    *shared.in_flight.lock().unwrap() = Some((job.id, Arc::clone(&cancel)));

    let request = RenderRequest {
        document: *handle,
        page: job.page,
        region: job.region,
        width: job.width,
        height: job.height,
        // A presentation job leaves this off — the presenter's marks are a
        // transient overlay — and a reader page job turns it on, because
        // there the document's own annotations are the point.
        with_annotations: job.with_annotations,
    };
    let started = std::time::Instant::now();
    let result = backend.render_into(&request, mapped.as_mut_slice(), cancel.as_ref());
    let render_micros = started.elapsed().as_micros() as u64;
    *shared.in_flight.lock().unwrap() = None;

    match result {
        Ok(()) => Response::Rendered {
            id: job.id,
            generation: job.generation,
            width: job.width,
            height: job.height,
            quality: job.quality,
            bytes,
            pixels: None,
            render_micros,
        },
        Err(crate::pdf::PdfError::Cancelled) => Response::Cancelled { id: job.id },
        Err(e) => Response::RenderFailed {
            id: job.id,
            generation: job.generation,
            reason: e.to_string(),
        },
    }
}

fn read_requests(input: impl Read, shared: Arc<Shared>) {
    let mut input = BufReader::new(input);
    loop {
        match read_message::<Request>(&mut input) {
            Ok(Request::Cancel { id }) => {
                shared.cancel_in_flight_if(|in_flight| in_flight == id);
                let mut inbox = shared.inbox.lock().unwrap();
                let dropped = drain_jobs(&mut inbox, |job| job.id == id);
                inbox.unacknowledged.extend(dropped);
                // The list only guards the race where a job arrives after
                // its own cancel, so old entries are dead weight — but a
                // long talk cancels thousands of jobs, and every render
                // scanned the whole list. Half is dropped at the cap: the
                // race window is a handful of messages, never five hundred.
                const MAX_REMEMBERED_CANCELS: usize = 1024;
                if inbox.cancelled.len() >= MAX_REMEMBERED_CANCELS {
                    inbox.cancelled.drain(..MAX_REMEMBERED_CANCELS / 2);
                }
                inbox.cancelled.push(id);
                shared.signal.notify_all();
            }
            Ok(Request::CancelGeneration { generation }) => {
                let mut inbox = shared.inbox.lock().unwrap();
                inbox.floor = generation;
                // Everything below the floor is rejected by the floor check
                // itself; cancels recorded before the generation moved on
                // guard jobs that can no longer arrive.
                inbox.cancelled.clear();
                let dropped = drain_jobs(&mut inbox, |job| job.generation < generation);
                inbox.unacknowledged.extend(dropped);
                drop(inbox);
                // The in-flight job may itself be obsolete.
                if let Some((_, flag)) = shared.in_flight.lock().unwrap().as_ref() {
                    flag.store(true, Ordering::Relaxed);
                }
                shared.signal.notify_all();
            }
            Ok(Request::Render(job)) => {
                let mut inbox = shared.inbox.lock().unwrap();
                if job.generation >= inbox.floor {
                    inbox.jobs.push_back(job);
                }
                shared.signal.notify_all();
            }
            Ok(Request::Shutdown) => {
                let mut inbox = shared.inbox.lock().unwrap();
                inbox.shutdown = true;
                drop(inbox);
                shared.cancel_in_flight_if(|_| true);
                shared.signal.notify_all();
                return;
            }
            Ok(control) => {
                let mut inbox = shared.inbox.lock().unwrap();
                inbox.control.push_back(control);
                shared.signal.notify_all();
            }
            Err(_) => {
                let mut inbox = shared.inbox.lock().unwrap();
                inbox.closed = true;
                drop(inbox);
                shared.cancel_in_flight_if(|_| true);
                shared.signal.notify_all();
                return;
            }
        }
    }
}

/// Choose the document's speaker-notes attachment, if it has one.
///
/// pdfpc's own convention is a member of the `/EmbeddedFiles` name tree whose
/// name ends in `.pdfpc`, and a deck may carry several attachments — a Typst
/// deck embeds its source alongside its notes, for one. The one named after
/// the PDF wins, because that is what both the LaTeX package and the sidecar
/// convention produce; otherwise the first `.pdfpc` member in name order does,
/// so the choice does not depend on how PDFium happens to enumerate.
pub(crate) fn select_pdfpc_attachment(
    names: &[String],
    document: &std::path::Path,
) -> Option<String> {
    let preferred = document
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| format!("{stem}.pdfpc"));
    let is_pdfpc = |name: &String| {
        std::path::Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdfpc"))
    };
    if let Some(preferred) = preferred.filter(|wanted| names.contains(wanted)) {
        return Some(preferred);
    }
    let mut candidates: Vec<&String> = names.iter().filter(|name| is_pdfpc(name)).collect();
    candidates.sort();
    candidates.first().map(|name| String::clone(name))
}

/// Read the embedded pdfpc payload, or nothing.
///
/// Every failure here is silent and non-fatal by design: a document without
/// notes, an attachment that will not read, and one that is not text are all
/// the same outcome to a presenter — this deck has no embedded notes — and
/// none of them is a reason to refuse to open the talk.
fn read_pdfpc_attachment(
    backend: &dyn PdfBackend,
    handle: BackendDocumentId,
    document: &std::path::Path,
) -> Option<String> {
    let names = backend.attachment_names(handle).ok()?;
    let name = select_pdfpc_attachment(&names, document)?;
    let bytes = backend.attachment(handle, &name).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::fixture::FixtureBackend;
    use crate::protocol::Priority;
    use crate::shm::{RegionNamer, SharedRegion};
    use pulpit_core::notes::Region;

    fn job(id: u64, generation: u64, priority: Priority, region_name: &str) -> RenderJob {
        RenderJob {
            id: RequestId(id),
            generation: RenderGeneration(generation),
            document: 1,
            page: 0,
            region: Region::FULL,
            width: 64,
            height: 36,
            priority,
            quality: Quality::Refined,
            with_annotations: false,
            region_name: region_name.to_string(),
        }
    }

    /// Drive the worker over in-memory pipes.
    fn run_worker(requests: Vec<Request>) -> Vec<Response> {
        let mut encoded = Vec::new();
        for request in &requests {
            write_message(&mut encoded, request).unwrap();
        }
        let output = SharedBuffer::default();
        let sink = output.clone();
        run(
            std::io::Cursor::new(encoded),
            sink,
            Box::new(FixtureBackend::new()),
        )
        .unwrap();

        let bytes = output.take();
        let mut cursor = std::io::Cursor::new(bytes);
        let mut responses = Vec::new();
        while let Ok(response) = read_message::<Response>(&mut cursor) {
            responses.push(response);
        }
        responses
    }

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn take(&self) -> Vec<u8> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn handshake_open_and_render() {
        let namer = RegionNamer::new();
        let name = namer.next();
        let _region = SharedRegion::create(&name, 64 * 36 * 4).unwrap();

        let responses = run_worker(vec![
            Request::Hello {
                version: PROTOCOL_VERSION,
            },
            Request::Open {
                document: 1,
                path: "fixture:pages=10".into(),
            },
            Request::Render(job(1, 1, Priority::Audience, &name)),
            Request::Shutdown,
        ]);

        assert!(matches!(
            responses[0],
            Response::Hello {
                version: PROTOCOL_VERSION,
                ..
            }
        ));
        assert!(matches!(&responses[1], Response::Opened(opened) if opened.page_count == 10));
        assert!(matches!(
            responses[2],
            Response::Rendered {
                id: RequestId(1),
                width: 64,
                height: 36,
                ..
            }
        ));
    }

    #[test]
    fn link_annotations_travel_over_the_protocol() {
        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:pages=3&links".into(),
            },
            Request::Links {
                document: 1,
                page: 0,
            },
            Request::Links {
                document: 1,
                page: 99,
            },
            Request::Shutdown,
        ]);
        assert!(
            matches!(
                &responses[1],
                Response::Links {
                    document: 1,
                    page: 0,
                    links
                } if links.len() == 2
            ),
            "{responses:?}"
        );
        assert!(
            matches!(
                &responses[2],
                Response::Links { page: 99, links, .. } if links.is_empty()
            ),
            "an out-of-range page answers with no links, not an error: {responses:?}"
        );
    }

    #[test]
    fn overlay_declarations_and_their_diagnostics_travel_over_the_protocol() {
        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:pages=3&media".into(),
            },
            Request::Overlays {
                document: 1,
                page: 0,
            },
            Request::Shutdown,
        ]);
        assert!(
            matches!(
                &responses[1],
                Response::Overlays {
                    page: 0,
                    declarations,
                    diagnostics,
                    ..
                } if declarations.len() == 1 && diagnostics.len() == 1
            ),
            "{responses:?}"
        );
    }

    #[test]
    fn the_navigation_model_travels_over_the_protocol() {
        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:pages=8&outline".into(),
            },
            Request::Navigation { document: 1 },
            Request::Navigation { document: 99 },
            Request::Shutdown,
        ]);
        assert!(
            matches!(
                &responses[1],
                Response::Navigation { document: 1, navigation }
                    if navigation.section_for_page(2) == Some("Measurements")
            ),
            "{responses:?}"
        );
        assert!(
            matches!(
                &responses[2],
                Response::Navigation { navigation, .. } if navigation.is_empty()
            ),
            "a document that is not open answers with an empty model: {responses:?}"
        );
    }

    #[test]
    fn capability_findings_travel_over_the_protocol() {
        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:pages=8&features".into(),
            },
            Request::Capabilities { document: 1 },
            Request::Capabilities { document: 99 },
            Request::Shutdown,
        ]);
        assert!(
            matches!(
                &responses[1],
                Response::Capabilities { document: 1, capabilities }
                    if capabilities.has(crate::pdf::capabilities::FindingKind::UnplayableMedia)
            ),
            "{responses:?}"
        );
        assert!(
            matches!(
                &responses[2],
                Response::Capabilities { capabilities, .. } if capabilities.is_empty()
            ),
            "{responses:?}"
        );
    }

    #[test]
    fn an_attachment_a_backend_cannot_supply_fails_without_killing_the_worker() {
        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:pages=3".into(),
            },
            Request::Attachment {
                document: 1,
                name: "balls.zip".into(),
            },
            Request::Attachment {
                document: 99,
                name: "balls.zip".into(),
            },
            Request::Shutdown,
        ]);
        assert!(matches!(
            &responses[1],
            Response::AttachmentFailed { document: 1, .. }
        ));
        assert!(
            matches!(&responses[2], Response::AttachmentFailed { reason, .. } if reason.contains("not open")),
            "{responses:?}"
        );
    }

    #[test]
    fn an_unopenable_document_reports_instead_of_dying() {
        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:unreadable".into(),
            },
            Request::Shutdown,
        ]);
        assert!(matches!(
            responses[0],
            Response::OpenFailed { document: 1, .. }
        ));
    }

    #[test]
    fn obsolete_generations_are_refused_by_the_worker_too() {
        let namer = RegionNamer::new();
        let name = namer.next();
        let _region = SharedRegion::create(&name, 64 * 36 * 4).unwrap();

        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:pages=10".into(),
            },
            Request::CancelGeneration {
                generation: RenderGeneration(5),
            },
            Request::Render(job(9, 2, Priority::Audience, &name)),
            Request::Shutdown,
        ]);
        assert!(
            !responses
                .iter()
                .any(|r| matches!(r, Response::Rendered { .. })),
            "a stale job must never produce pixels: {responses:?}"
        );
    }

    #[test]
    fn a_malformed_job_is_rejected_without_touching_shared_memory() {
        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:pages=10".into(),
            },
            Request::Render(RenderJob {
                width: 0,
                ..job(3, 1, Priority::Audience, "x")
            }),
            Request::Shutdown,
        ]);
        assert!(matches!(
            responses[1],
            Response::RenderFailed {
                id: RequestId(3),
                ..
            }
        ));
    }

    #[test]
    fn queued_work_is_ordered_by_priority_then_quality() {
        let namer = RegionNamer::new();
        let name = namer.next();
        let _region = SharedRegion::create(&name, 64 * 36 * 4).unwrap();

        let mut ancillary = job(1, 1, Priority::Ancillary, &name);
        ancillary.page = 1;
        let mut audience_refined = job(2, 1, Priority::Audience, &name);
        audience_refined.page = 2;
        let mut audience_coarse = job(3, 1, Priority::Audience, &name);
        audience_coarse.page = 3;
        audience_coarse.quality = Quality::Coarse;

        let responses = run_worker(vec![
            Request::Open {
                document: 1,
                path: "fixture:pages=10".into(),
            },
            Request::Render(ancillary),
            Request::Render(audience_refined),
            Request::Render(audience_coarse),
            Request::Shutdown,
        ]);
        let order: Vec<u64> = responses
            .iter()
            .filter_map(|r| match r {
                Response::Rendered { id, .. } => Some(id.0),
                _ => None,
            })
            .collect();
        // What this end-to-end test can honestly assert is that the worker
        // accepts a queue and answers without losing or duplicating requests.
        // *Which* job it starts first depends on how many of them are in the
        // queue when the renderer looks, which is thread scheduling — on a
        // loaded machine even shutdown can win the race. The priority rule
        // itself is asserted deterministically in the test below, against the
        // picker, where it belongs.
        assert!(!order.is_empty(), "some work was done: {responses:?}");
        let mut seen = order.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), order.len(), "no job answered twice: {order:?}");
        assert!(
            order.iter().all(|id| [1, 2, 3].contains(id)),
            "only the queued jobs were answered: {order:?}"
        );
    }

    /// Queue ordering itself, without the thread race.
    ///
    /// The end-to-end test above cannot assert an exact order: whether all
    /// three jobs are in the queue before the renderer picks the first one
    /// depends on thread scheduling. This one asks the picker directly.
    #[test]
    fn the_queue_prefers_the_audience_page_and_coarse_work_first() {
        let mut inbox = Inbox::default();
        let mut ancillary = job(1, 1, Priority::Ancillary, "region");
        ancillary.page = 1;
        let mut audience_refined = job(2, 1, Priority::Audience, "region");
        audience_refined.page = 2;
        let mut audience_coarse = job(3, 1, Priority::Audience, "region");
        audience_coarse.page = 3;
        audience_coarse.quality = Quality::Coarse;
        let mut presenter = job(4, 1, Priority::Presenter, "region");
        presenter.page = 4;

        inbox.jobs.push_back(ancillary);
        inbox.jobs.push_back(audience_refined);
        inbox.jobs.push_back(audience_coarse);
        inbox.jobs.push_back(presenter);

        let mut order = Vec::new();
        while let Some(index) = pick_job(&inbox) {
            order.push(inbox.jobs.remove(index).unwrap().id.0);
        }
        assert_eq!(
            order,
            vec![3, 2, 4, 1],
            "coarse audience, refined audience, presenter, then the rest"
        );
    }
}

#[cfg(test)]
mod pdfpc_attachment_tests {
    use super::select_pdfpc_attachment;
    use std::path::Path;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn the_attachment_named_after_the_document_wins() {
        let found = select_pdfpc_attachment(
            &names(&["talk.typ", "other.pdfpc", "talk.pdfpc"]),
            Path::new("/talks/talk.pdf"),
        );
        assert_eq!(found.as_deref(), Some("talk.pdfpc"));
    }

    #[test]
    fn any_pdfpc_member_will_do_when_none_matches_the_name() {
        let found = select_pdfpc_attachment(
            &names(&["deck.typ", "notes.pdfpc"]),
            Path::new("/talks/talk.pdf"),
        );
        assert_eq!(found.as_deref(), Some("notes.pdfpc"));
    }

    #[test]
    fn the_choice_does_not_depend_on_enumeration_order() {
        let one = select_pdfpc_attachment(&names(&["b.pdfpc", "a.pdfpc"]), Path::new("talk.pdf"));
        let other = select_pdfpc_attachment(&names(&["a.pdfpc", "b.pdfpc"]), Path::new("talk.pdf"));
        assert_eq!(
            one, other,
            "two orders of the same set choose the same file"
        );
        assert_eq!(one.as_deref(), Some("a.pdfpc"));
    }

    #[test]
    fn the_extension_is_matched_whatever_its_case() {
        let found = select_pdfpc_attachment(&names(&["NOTES.PDFPC"]), Path::new("talk.pdf"));
        assert_eq!(found.as_deref(), Some("NOTES.PDFPC"));
    }

    #[test]
    fn a_document_carrying_no_notes_selects_nothing() {
        assert_eq!(
            select_pdfpc_attachment(&names(&["talk.typ", "data.csv"]), Path::new("talk.pdf")),
            None
        );
        assert_eq!(select_pdfpc_attachment(&[], Path::new("talk.pdf")), None);
    }

    #[test]
    fn a_name_that_merely_contains_the_word_is_not_an_attachment() {
        assert_eq!(
            select_pdfpc_attachment(&names(&["pdfpc-notes.txt", "pdfpc"]), Path::new("talk.pdf")),
            None,
            "the convention is an extension, not a substring"
        );
    }
}
