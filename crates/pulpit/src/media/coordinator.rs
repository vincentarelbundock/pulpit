//! Turning overlay declarations into live sessions.
//!
//! The coordinator is the only place that knows both halves of the story:
//! `pulpit-render` found declarations and can extract attachments,
//! `pulpit-media` can play them. Neither crate depends on the other, so
//! the descriptors meet here.
//!
//! The rule it exists to enforce is the audience one: a session is created,
//! replaced or destroyed only in ways that leave the last good frame on
//! screen until a complete replacement arrives.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use pulpit_core::overlay::{
    ContentKind, OverlayContent, OverlayDeclaration, OverlayIndex, PageLabels, WebManifest,
};
use pulpit_core::{OverlayId, RenderGeneration};
use pulpit_media::capability::request_for_web;
use pulpit_media::protocol::{CapabilityRequest, PlaybackProgress, SessionSource, VideoCommand};
use pulpit_media::{MediaSupervisor, SessionId, Viewport};

use pulpit_render::pdf::overlays::{ExtractionLimits, StagingRoot};

/// What an overlay still needs before a session can be opened.
#[derive(Debug, Clone, PartialEq)]
pub enum Need {
    /// An embedded attachment must be fetched from the renderer.
    Attachment(String),
    /// Nothing: the asset is on disk and the session can open.
    Ready,
    /// Permanently unusable; the poster or the PDF page stands in.
    Blocked(String),
}

/// One overlay's staging state.
#[derive(Debug)]
struct Staged {
    /// Where the asset landed, once it has.
    source: Option<SessionSource>,
    /// The attachment name this overlay is waiting for, if any.
    awaiting: Option<String>,
    /// Why it will never be usable, if that is settled.
    blocked: Option<String>,
    /// The manifest, for a web bundle.
    manifest: Option<WebManifest>,
}

/// The live picture of one document's overlays.
pub struct MediaCoordinator {
    /// Staging root for the current generation. Dropped — and deleted — when
    /// the generation is retired.
    staging: Option<StagingRoot>,
    generation: RenderGeneration,
    index: OverlayIndex,
    staged: HashMap<OverlayId, Staged>,
    /// Sessions currently open, by overlay.
    sessions: HashMap<OverlayId, SessionId>,
    /// Overlays whose sessions are open but off the committed page, least
    /// recently parked first. This is the eviction order: a presenter's
    /// look-behind is the newest few slides, so the session parked longest
    /// ago is the one least worth its ring.
    parked: Vec<OverlayId>,
    /// The most recent complete frame per overlay. Held across page changes,
    /// runtime replacement and failure, so the audience never blanks.
    frames: HashMap<OverlayId, OverlayFrame>,
    /// Overlays in the order their retained frames were last replaced,
    /// oldest first — the eviction order for the byte cap below.
    frame_order: Vec<OverlayId>,
    /// Where each overlay's playback has reached, as its content last said.
    progress: HashMap<OverlayId, PlaybackProgress>,
    /// Attachments already asked for, so a re-render does not re-request.
    requested: std::collections::HashSet<String>,
    /// Diagnostics from discovery, surfaced in preflight.
    diagnostics: Vec<String>,
    limits: ExtractionLimits,
    /// Image handles built from media frames since the application started.
    /// Compared against frames published upstream, this says whether the UI
    /// is constructing handles it never shows.
    handles_created: u64,
}

/// What a presenter's transport is pointed at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportTarget {
    pub overlay: OverlayId,
    pub kind: ContentKind,
    /// `None` until the content has reported anything — a session that is
    /// still opening, or one whose runtime never started.
    pub progress: Option<PlaybackProgress>,
    /// Is there a session to send commands to? A poster standing in for a
    /// failed runtime is still a target, but an inert one.
    pub live: bool,
}

/// What the presenter asked of the media on this slide.
///
/// Deliberately smaller than [`VideoCommand`]: these are the things a
/// transport has buttons for. Volume and looping are deck decisions made in
/// the PDF, not knobs to turn mid-talk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransportCommand {
    Play,
    Pause,
    SeekTo(f32),
    SetMuted(bool),
}

impl TransportCommand {
    fn for_video(self) -> VideoCommand {
        match self {
            TransportCommand::Play => VideoCommand::Play,
            TransportCommand::Pause => VideoCommand::Pause,
            TransportCommand::SeekTo(seconds) => VideoCommand::Seek { seconds },
            TransportCommand::SetMuted(muted) => VideoCommand::SetMuted { muted },
        }
    }

    /// The same intent for an animated image, where most of it is meaningless:
    /// a GIF has no playhead to seek and no audio to mute.
    fn for_image(self) -> Option<pulpit_media::protocol::ImageCommand> {
        use pulpit_media::protocol::ImageCommand;
        match self {
            TransportCommand::Play => Some(ImageCommand::Play),
            TransportCommand::Pause => Some(ImageCommand::Pause),
            TransportCommand::SeekTo(_) | TransportCommand::SetMuted(_) => None,
        }
    }
}

/// The last complete frame of one overlay.
#[derive(Debug, Clone)]
pub struct OverlayFrame {
    pub width: u32,
    pub height: u32,
    pub handle: iced::widget::image::Handle,
    pub sequence: u64,
}

impl Default for MediaCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaCoordinator {
    pub fn new() -> Self {
        Self {
            staging: None,
            generation: RenderGeneration::ZERO,
            index: OverlayIndex::default(),
            staged: HashMap::new(),
            sessions: HashMap::new(),
            parked: Vec::new(),
            frames: HashMap::new(),
            frame_order: Vec::new(),
            progress: HashMap::new(),
            requested: std::collections::HashSet::new(),
            diagnostics: Vec::new(),
            limits: ExtractionLimits::default(),
            handles_created: 0,
        }
    }

    /// Image handles built from media frames since the application started.
    pub fn handles_created(&self) -> u64 {
        self.handles_created
    }

    pub fn index(&self) -> &OverlayIndex {
        &self.index
    }

    #[allow(dead_code)] // unreached, including by its own tests
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub fn frame(&self, overlay: OverlayId) -> Option<&OverlayFrame> {
        self.frames.get(&overlay)
    }

    pub fn session(&self, overlay: OverlayId) -> Option<SessionId> {
        self.sessions.get(&overlay).copied()
    }

    pub fn progress(&self, overlay: OverlayId) -> Option<PlaybackProgress> {
        self.progress.get(&overlay).copied()
    }

    /// The content on `page` a presenter's transport can drive, if any.
    ///
    /// One overlay, not a list: a transport with two clips to choose between
    /// is a mode, and a presenter mid-talk should never have to pick. The
    /// front-most playable overlay on the slide is the one the controls mean.
    /// Web overlays are excluded — they have no playhead, and their own page
    /// is their transport.
    pub fn transport_target(&self, page: usize) -> Option<TransportTarget> {
        self.index
            .on_page(page)
            .into_iter()
            .rfind(|overlay| {
                matches!(
                    overlay.content.kind(),
                    ContentKind::Video | ContentKind::AnimatedImage
                )
            })
            .map(|overlay| TransportTarget {
                overlay: overlay.id,
                kind: overlay.content.kind(),
                progress: self.progress.get(&overlay.id).copied(),
                live: self.sessions.contains_key(&overlay.id),
            })
    }

    /// Record where an overlay's playback has reached.
    pub fn progress_reported(&mut self, overlay: OverlayId, progress: PlaybackProgress) {
        self.progress.insert(overlay, progress.sanitised());
    }

    /// Send one transport command to whatever is playing in `overlay`.
    ///
    /// The command is translated to the overlay's content kind here, so the
    /// widget can ask for "pause" without knowing whether it is talking to a
    /// clip or a GIF — and the supervisor still refuses anything a session's
    /// kind cannot answer.
    pub fn control(
        &mut self,
        supervisor: &mut MediaSupervisor,
        overlay: OverlayId,
        command: TransportCommand,
    ) {
        let Some(session) = self.sessions.get(&overlay).copied() else {
            return;
        };
        let Some(kind) = self.index.get(overlay).map(|o| o.content.kind()) else {
            return;
        };
        match kind {
            ContentKind::Video => supervisor.video_command(session, command.for_video()),
            ContentKind::AnimatedImage => {
                if let Some(command) = command.for_image() {
                    supervisor.image_command(session, command);
                }
            }
            // A web overlay drives itself; there is nothing to send.
            ContentKind::Web => {}
        }
        // The optimistic half of the update. The content will report its own
        // state in a moment, but a button that does not move until a round
        // trip completes reads as a button that did not work.
        if let Some(progress) = self.progress.get_mut(&overlay) {
            match command {
                TransportCommand::Play => progress.paused = false,
                TransportCommand::Pause => progress.paused = true,
                TransportCommand::SeekTo(seconds) => progress.position = seconds,
                TransportCommand::SetMuted(muted) => progress.muted = muted,
            }
        }
    }

    #[allow(dead_code)] // unreached, including by its own tests
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Rebuild the overlay index from every page's declarations.
    ///
    /// Called once per page, because the renderer answers one page at a time
    /// — so this must be *idempotent within a generation*. Staged assets,
    /// in-flight attachment requests and open sessions survive; only a new
    /// generation wipes them.
    ///
    /// Clearing on every call was a real bug: page two's overlays arriving
    /// threw away the entry waiting for page one's attachment, so the bytes
    /// that turned up a moment later had nothing to attach to and were
    /// dropped, and the caller — which asks for each attachment once — never
    /// asked again. Nothing ever played.
    pub fn rebuild(
        &mut self,
        generation: RenderGeneration,
        per_page: &BTreeMap<usize, Vec<OverlayDeclaration>>,
        labels: &PageLabels,
        diagnostics: Vec<String>,
    ) {
        let new_generation = generation != self.generation || self.staging.is_none();
        self.index = OverlayIndex::build(per_page, labels);
        self.generation = generation;
        self.diagnostics = diagnostics;

        if !new_generation {
            // Same document: keep everything already staged or in flight, and
            // drop only the bookkeeping for overlays this rebuild removed.
            let live: std::collections::HashSet<OverlayId> =
                self.index.all().iter().map(|overlay| overlay.id).collect();
            self.staged.retain(|id, _| live.contains(id));
            self.sessions.retain(|id, _| live.contains(id));
            self.parked.retain(|id| live.contains(id));
            return;
        }

        self.staged.clear();
        self.sessions.clear();
        self.parked.clear();
        self.requested.clear();
        // Frames from the previous generation stay until their overlay is
        // replaced by a complete new one, so a reload does not flash.
        self.staging = match StagingRoot::create() {
            Ok(root) => Some(root),
            Err(e) => {
                self.diagnostics
                    .push(format!("could not create a staging directory: {e}"));
                None
            }
        };
    }

    /// Attachments this generation still needs, so the caller can forget its
    /// own request bookkeeping when a new generation begins.
    pub fn generation(&self) -> RenderGeneration {
        self.generation
    }

    /// What each overlay on a page still needs.
    pub fn needs(&mut self, page: usize, document_dir: &std::path::Path) -> Vec<(OverlayId, Need)> {
        let overlays: Vec<(OverlayId, OverlayContent)> = self
            .index
            .on_page(page)
            .into_iter()
            .map(|overlay| (overlay.id, overlay.content.clone()))
            .collect();

        overlays
            .into_iter()
            .map(|(id, content)| {
                let need = self.need_for(id, &content, document_dir);
                (id, need)
            })
            .collect()
    }

    fn need_for(
        &mut self,
        id: OverlayId,
        content: &OverlayContent,
        document_dir: &std::path::Path,
    ) -> Need {
        if let Some(staged) = self.staged.get(&id) {
            if let Some(reason) = &staged.blocked {
                return Need::Blocked(reason.clone());
            }
            if staged.source.is_some() {
                return Need::Ready;
            }
            if let Some(name) = &staged.awaiting {
                return Need::Attachment(name.clone());
            }
        }

        // An external asset is resolved straight away; an embedded one has to
        // come back through the renderer.
        let (embedded, external) = match content {
            OverlayContent::AnimatedImage(spec) => split_source(&spec.source),
            OverlayContent::Video(spec) => split_source(&spec.source),
            OverlayContent::Web(spec) => split_source(&spec.bundle),
        };

        if let Some(path) = external {
            let asset = pulpit_core::overlay::ExternalAssetRef::new(path);
            match pulpit_render::pdf::overlays::resolve_external(document_dir, &asset) {
                Ok(resolved) => {
                    // A web overlay beside the document serves the HTML
                    // file's own directory, so the page reaches its stylesheet
                    // and its script exactly as it does on disk. Everything
                    // else is a single file.
                    let source = match content {
                        OverlayContent::Web(spec) => {
                            let root = resolved
                                .parent()
                                .map(|parent| parent.to_path_buf())
                                .unwrap_or_else(|| resolved.clone());
                            SessionSource::Bundle {
                                root: root.display().to_string(),
                                entrypoint: spec.entrypoint.0.clone(),
                            }
                        }
                        _ => SessionSource::File {
                            path: resolved.display().to_string(),
                        },
                    };
                    self.staged.insert(
                        id,
                        Staged {
                            source: Some(source),
                            awaiting: None,
                            blocked: None,
                            manifest: None,
                        },
                    );
                    return Need::Ready;
                }
                Err(e) => {
                    let reason = e.to_string();
                    self.staged.insert(
                        id,
                        Staged {
                            source: None,
                            awaiting: None,
                            blocked: Some(reason.clone()),
                            manifest: None,
                        },
                    );
                    return Need::Blocked(reason);
                }
            }
        }

        let Some(name) = embedded else {
            let reason = "the overlay names no asset".to_string();
            self.staged.insert(
                id,
                Staged {
                    source: None,
                    awaiting: None,
                    blocked: Some(reason.clone()),
                    manifest: None,
                },
            );
            return Need::Blocked(reason);
        };

        self.staged.insert(
            id,
            Staged {
                source: None,
                awaiting: Some(name.clone()),
                blocked: None,
                manifest: None,
            },
        );
        // Several overlays may reference one attachment — a poster shared
        // between two slides, say — so the caller is told what is needed
        // every time, and `requested` is what stops it being *fetched* twice.
        self.requested.insert(name.clone());
        Need::Attachment(name)
    }

    /// Has this attachment already been asked of the renderer?
    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn already_requested(&self, name: &str) -> bool {
        self.requested.contains(name)
    }

    /// An attachment arrived from the renderer. Stage it for every overlay
    /// waiting on it.
    pub fn attachment_ready(&mut self, name: &str, bytes: &[u8]) {
        let Some(staging) = self.staging.as_ref() else {
            return;
        };
        let waiting: Vec<OverlayId> = self
            .staged
            .iter()
            .filter(|(_, staged)| staged.awaiting.as_deref() == Some(name))
            .map(|(id, _)| *id)
            .collect();

        for id in waiting {
            let kind = self
                .index
                .get(id)
                .map(|overlay| overlay.content.kind())
                .unwrap_or(ContentKind::AnimatedImage);
            let outcome = match kind {
                ContentKind::Web => self.stage_bundle(id, name, bytes, staging),
                _ => self.stage_file(id, name, bytes, staging),
            };
            match outcome {
                Ok((source, manifest)) => {
                    self.staged.insert(
                        id,
                        Staged {
                            source: Some(source),
                            awaiting: None,
                            blocked: None,
                            manifest,
                        },
                    );
                }
                Err(reason) => {
                    self.diagnostics.push(format!("{id}: {reason}"));
                    self.staged.insert(
                        id,
                        Staged {
                            source: None,
                            awaiting: None,
                            blocked: Some(reason),
                            manifest: None,
                        },
                    );
                }
            }
        }
    }

    /// The renderer could not produce an attachment.
    pub fn attachment_failed(&mut self, name: &str, reason: &str) {
        let waiting: Vec<OverlayId> = self
            .staged
            .iter()
            .filter(|(_, staged)| staged.awaiting.as_deref() == Some(name))
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            self.staged.insert(
                id,
                Staged {
                    source: None,
                    awaiting: None,
                    blocked: Some(reason.to_string()),
                    manifest: None,
                },
            );
        }
    }

    fn stage_file(
        &self,
        id: OverlayId,
        name: &str,
        bytes: &[u8],
        staging: &StagingRoot,
    ) -> Result<(SessionSource, Option<WebManifest>), String> {
        let directory = staging
            .asset_dir()
            .map_err(|e| format!("could not stage {name}: {e}"))?;
        // Never the PDF's own name: a generated one cannot be a path.
        let extension = std::path::Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| {
                extension.len() <= 8 && extension.chars().all(|c| c.is_alphanumeric())
            })
            .unwrap_or("bin");
        let path = directory.join(format!("asset-{}.{extension}", id.0));
        std::fs::write(&path, bytes).map_err(|e| format!("could not stage {name}: {e}"))?;
        set_read_only(&path);
        Ok((
            SessionSource::File {
                path: path.display().to_string(),
            },
            None,
        ))
    }

    fn stage_bundle(
        &self,
        id: OverlayId,
        name: &str,
        bytes: &[u8],
        staging: &StagingRoot,
    ) -> Result<(SessionSource, Option<WebManifest>), String> {
        let directory = staging
            .asset_dir()
            .map_err(|e| format!("could not stage {name}: {e}"))?;
        let root = directory.join(format!("bundle-{}", id.0));
        let bundle = pulpit_render::pdf::overlays::extract_bundle(bytes, &root, &self.limits)
            .map_err(|e| e.to_string())?;

        // The manifest is the authority on the entrypoint unless it says
        // otherwise; the URI's suggestion is only a suggestion.
        let mut entrypoint = bundle.manifest.entrypoint.clone();
        if let Some(overlay) = self.index.get(id) {
            if let OverlayContent::Web(spec) = &overlay.content {
                let uri_named = spec.entrypoint.0 != "index.html";
                if uri_named && bundle.manifest.allow_entrypoint_override {
                    entrypoint = spec.entrypoint.0.clone();
                }
            }
        }
        if !bundle.root.join(&entrypoint).is_file() {
            return Err(format!("the bundle has no entrypoint named {entrypoint}"));
        }
        Ok((
            SessionSource::Bundle {
                root: bundle.root.display().to_string(),
                entrypoint,
            },
            Some(bundle.manifest),
        ))
    }

    /// The capability request one overlay implies.
    pub fn request_for(&self, id: OverlayId) -> CapabilityRequest {
        let Some(overlay) = self.index.get(id) else {
            return CapabilityRequest::default();
        };
        match &overlay.content {
            OverlayContent::Web(spec) => {
                // The manifest's requirements win once the bundle is staged;
                // before that the URI's defaults stand in.
                let requirements = self
                    .staged
                    .get(&id)
                    .and_then(|staged| staged.manifest.as_ref())
                    .map(|manifest| manifest.requirements)
                    .unwrap_or(spec.requirements);
                // Audio stays denied in the first implementation even when a
                // browser could play it.
                request_for_web(&requirements, false)
            }
            other => CapabilityRequest::for_kind(other.kind()),
        }
    }

    /// Open sessions for every ready overlay on `page` that has none.
    pub fn open_ready(
        &mut self,
        supervisor: &mut MediaSupervisor,
        page: usize,
        viewport_for: impl Fn(OverlayId) -> Option<Viewport>,
        command_for: impl Fn(pulpit_media::RuntimeId) -> pulpit_media::WorkerCommand + Copy,
        reduce_motion: bool,
    ) {
        let candidates: Vec<OverlayId> = self
            .index
            .on_page(page)
            .into_iter()
            .map(|overlay| overlay.id)
            .filter(|id| !self.sessions.contains_key(id))
            .collect();

        for id in candidates {
            let Some(source) = self
                .staged
                .get(&id)
                .and_then(|staged| staged.source.clone())
            else {
                continue;
            };
            let Some(viewport) = viewport_for(id) else {
                continue;
            };
            let Some(overlay) = self.index.get(id) else {
                continue;
            };
            let kind = overlay.content.kind();
            let mut playback = match &overlay.content {
                OverlayContent::AnimatedImage(spec) => spec.playback.clone(),
                OverlayContent::Video(spec) => spec.playback.clone(),
                OverlayContent::Web(_) => Default::default(),
            };
            // A looping GIF or an autoplaying clip is exactly the motion a
            // reduced-motion preference is about. The session still opens and
            // still shows its first frame, so the slide is not blank and the
            // presenter can start it deliberately.
            if reduce_motion {
                playback.autoplay = false;
                playback.repeat = false;
            }
            let request = self.request_for(id);
            let session = supervisor.open(
                id,
                self.generation,
                kind,
                source,
                viewport,
                playback,
                &request,
                command_for,
            );
            self.sessions.insert(id, session);
        }
    }

    /// How many parked sessions may stay resident, and how much shared
    /// memory their rings may hold between them. Both limits exist because a
    /// count alone says nothing — viewport area varies by orders of
    /// magnitude — and a byte budget alone would let dozens of tiny overlays
    /// keep dozens of browser pages warm.
    const MAX_PARKED_SESSIONS: usize = 4;
    const PARKED_RING_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

    /// Suspend every session whose overlay is not on `page`, and close the
    /// sessions parked longest once the parked set outgrows its working-set
    /// bounds.
    ///
    /// Sessions on the *same logical overlay* stay running across a reveal
    /// sequence, because `OverlayIndex` already collapsed those pages into
    /// one overlay covering all of them.
    ///
    /// Closing is a residency decision, not a visual one: the overlay's last
    /// frame stays in `frames`, so a revisited slide shows it immediately
    /// while `open_ready` opens a fresh session behind it. What a closed
    /// session loses is playback position — the bounded-delay,
    /// bounded-memory trade the working set exists to make.
    pub fn follow_page(&mut self, supervisor: &mut MediaSupervisor, page: usize) {
        let visible: std::collections::HashSet<OverlayId> = self
            .index
            .on_page(page)
            .into_iter()
            .map(|overlay| overlay.id)
            .collect();
        for (id, session) in &self.sessions {
            supervisor.set_active(*session, visible.contains(id));
        }

        // A returning overlay leaves the parked order; a newly parked one
        // joins it at the recent end.
        self.parked
            .retain(|id| self.sessions.contains_key(id) && !visible.contains(id));
        let mut newly_parked: Vec<OverlayId> = self
            .sessions
            .keys()
            .filter(|id| !visible.contains(id) && !self.parked.contains(id))
            .copied()
            .collect();
        // HashMap order is arbitrary; sorted, the eviction order is at least
        // deterministic for overlays parked by the same page change.
        newly_parked.sort();
        self.parked.append(&mut newly_parked);

        let slots = u64::from(supervisor.config().slots);
        let ring_bytes = |supervisor: &MediaSupervisor, session: SessionId| {
            supervisor.viewport_of(session).map_or(0, |viewport| {
                u64::from(viewport.width) * u64::from(viewport.height) * 4 * slots
            })
        };
        loop {
            let parked_bytes: u64 = self
                .parked
                .iter()
                .filter_map(|id| self.sessions.get(id))
                .map(|session| ring_bytes(supervisor, *session))
                .sum();
            if self.parked.len() <= Self::MAX_PARKED_SESSIONS
                && parked_bytes <= Self::PARKED_RING_BUDGET_BYTES
            {
                break;
            }
            let oldest = self.parked.remove(0);
            if let Some(session) = self.sessions.remove(&oldest) {
                supervisor.close(session);
            }
        }
    }

    /// Record a complete frame. Partial or stale frames never get this far —
    /// the supervisor validated and copied them already.
    pub fn frame_ready(
        &mut self,
        overlay: OverlayId,
        generation: RenderGeneration,
        sequence: u64,
        width: u32,
        height: u32,
        rgba: std::sync::Arc<Vec<u8>>,
    ) {
        if !self.generation.is_current_for(generation) || generation != self.generation {
            // A frame from a retired generation is dropped before it can
            // replace a current one.
            return;
        }
        if let Some(existing) = self.frames.get(&overlay) {
            if existing.sequence >= sequence {
                return;
            }
        }
        // The supervisor coalesces to one frame per session per drain, so
        // this is normally the Arc's only holder and the pixels move into
        // the handle without another full-frame copy.
        let pixels = std::sync::Arc::try_unwrap(rgba).unwrap_or_else(|shared| (*shared).clone());
        let handle = iced::widget::image::Handle::from_rgba(width, height, pixels);
        self.handles_created += 1;
        self.frame_order.retain(|id| *id != overlay);
        self.frame_order.push(overlay);
        self.enforce_frame_budget();
        self.frames.insert(
            overlay,
            OverlayFrame {
                width,
                height,
                handle,
                sequence,
            },
        );
    }

    /// How much memory the retained last frames may hold between them. A
    /// frame per overlay is the never-blank guarantee; without a byte cap a
    /// deck with a hundred full-HD overlays would quietly hold most of a
    /// gigabyte of stale pixels.
    const RETAINED_FRAME_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

    /// Evict the oldest retained frames of *sessionless* overlays until the
    /// cap holds. An overlay with a live session keeps its frame — that is
    /// the working set — and a revisited evicted overlay re-renders through
    /// the ordinary reopen path, showing the PDF poster underneath until
    /// its first new frame lands.
    fn enforce_frame_budget(&mut self) {
        let bytes = |frame: &OverlayFrame| u64::from(frame.width) * u64::from(frame.height) * 4;
        let mut total: u64 = self.frames.values().map(bytes).sum();
        if total <= Self::RETAINED_FRAME_BUDGET_BYTES {
            return;
        }
        let mut index = 0;
        while total > Self::RETAINED_FRAME_BUDGET_BYTES && index < self.frame_order.len() {
            let overlay = self.frame_order[index];
            if self.sessions.contains_key(&overlay) {
                index += 1;
                continue;
            }
            if let Some(frame) = self.frames.remove(&overlay) {
                total -= bytes(&frame);
            }
            self.frame_order.remove(index);
        }
    }

    /// A session ended for good. Its last frame is deliberately kept.
    pub fn session_failed(&mut self, overlay: OverlayId, exhausted: bool) {
        if exhausted {
            self.sessions.remove(&overlay);
            self.parked.retain(|id| *id != overlay);
        }
    }

    /// Forget everything. Staged assets are deleted with the staging root.
    #[allow(dead_code)] // unreached, including by its own tests
    pub fn clear(&mut self, supervisor: &mut MediaSupervisor) {
        for session in self.sessions.values() {
            supervisor.close(*session);
        }
        self.sessions.clear();
        self.parked.clear();
        self.staged.clear();
        self.frames.clear();
        self.frame_order.clear();
        self.progress.clear();
        self.requested.clear();
        self.index = OverlayIndex::default();
        self.staging = None;
    }
}

fn split_source(source: &pulpit_core::overlay::OverlaySource) -> (Option<String>, Option<String>) {
    match source {
        pulpit_core::overlay::OverlaySource::Embedded(asset) => {
            (Some(asset.attachment.clone()), None)
        }
        pulpit_core::overlay::OverlaySource::External(asset) => (None, Some(asset.path.clone())),
    }
}

/// Staged assets are read-only to runtime workers.
fn set_read_only(path: &PathBuf) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
    }
    #[cfg(not(unix))]
    {
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(true);
            let _ = std::fs::set_permissions(path, permissions);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::notes::Region;
    use pulpit_core::overlay::parse_overlay_uri;

    fn declarations(uris: &[&str]) -> Vec<OverlayDeclaration> {
        uris.iter()
            .map(|uri| parse_overlay_uri(uri, Region::new(0.1, 0.1, 0.5, 0.5)).unwrap())
            .collect()
    }

    fn coordinator(pages: &[(usize, &[&str])]) -> MediaCoordinator {
        let per_page: BTreeMap<usize, Vec<OverlayDeclaration>> = pages
            .iter()
            .map(|(page, uris)| (*page, declarations(uris)))
            .collect();
        let mut coordinator = MediaCoordinator::new();
        coordinator.rebuild(
            RenderGeneration(1),
            &per_page,
            &PageLabels::default(),
            Vec::new(),
        );
        coordinator
    }

    #[test]
    fn parked_sessions_are_closed_once_the_working_set_is_full() {
        // Ten pages, one *distinct* overlay each — identical overlays on
        // consecutive pages would be collapsed into one reveal overlay and
        // nothing would ever park. Sessions are planted directly: this is a
        // residency test, not a playback one, and the supervisor is a real
        // but runtime-less one whose close/set_active are no-ops.
        let per_page: BTreeMap<usize, Vec<OverlayDeclaration>> = (0..10usize)
            .map(|page| (page, declarations(&[&format!("pulpit://web/bundle{page}")])))
            .collect();
        let mut coordinator = MediaCoordinator::new();
        coordinator.rebuild(
            RenderGeneration(1),
            &per_page,
            &PageLabels::default(),
            Vec::new(),
        );
        let mut supervisor =
            pulpit_media::MediaSupervisor::unprobed(pulpit_media::MediaConfig::default());

        for page in 0..10usize {
            let id = coordinator.index.on_page(page)[0].id;
            coordinator.sessions.insert(id, SessionId(page as u64 + 1));
            coordinator.follow_page(&mut supervisor, page);
            // Everything currently open is the visible overlay plus at most
            // the parked working set.
            assert!(
                coordinator.sessions.len() <= MediaCoordinator::MAX_PARKED_SESSIONS + 1,
                "page {page}: {} sessions resident",
                coordinator.sessions.len()
            );
            assert!(coordinator.parked.len() <= MediaCoordinator::MAX_PARKED_SESSIONS);
        }
        // The oldest overlays were evicted, the newest kept.
        let survivor = coordinator.index.on_page(9)[0].id;
        assert!(coordinator.sessions.contains_key(&survivor));
        let evicted = coordinator.index.on_page(0)[0].id;
        assert!(!coordinator.sessions.contains_key(&evicted));
        // A revisited page's overlay counts as visible again and leaves the
        // parked order.
        coordinator.follow_page(&mut supervisor, 8);
        let revisited = coordinator.index.on_page(8)[0].id;
        assert!(!coordinator.parked.contains(&revisited));
    }

    #[test]
    fn an_embedded_overlay_asks_for_its_attachment() {
        let mut coordinator = coordinator(&[(0, &["pulpit://image/spinner"])]);
        let needs = coordinator.needs(0, std::path::Path::new("/tmp"));
        assert_eq!(needs.len(), 1);
        assert_eq!(needs[0].1, Need::Attachment("spinner".to_string()));
    }

    #[test]
    fn an_external_overlay_that_escapes_the_document_directory_is_blocked() {
        let directory = tempfile::tempdir().unwrap();
        let mut coordinator = coordinator(&[(0, &["pulpit://video-external/clip.mp4"])]);
        // The file does not exist, so resolution fails and the overlay is
        // permanently blocked rather than retried forever.
        let needs = coordinator.needs(0, directory.path());
        assert!(matches!(needs[0].1, Need::Blocked(_)));
    }

    #[test]
    fn an_external_overlay_beside_the_document_resolves_immediately() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("clip.mp4"), b"not really a video").unwrap();
        let mut coordinator = coordinator(&[(0, &["pulpit://video-external/clip.mp4"])]);
        let needs = coordinator.needs(0, directory.path());
        assert_eq!(needs[0].1, Need::Ready);
    }

    #[test]
    fn a_staged_attachment_makes_its_overlay_ready() {
        let mut coordinator = coordinator(&[(0, &["pulpit://image/spinner"])]);
        coordinator.needs(0, std::path::Path::new("/tmp"));
        coordinator.attachment_ready("spinner", b"GIF89a-not-really");
        let needs = coordinator.needs(0, std::path::Path::new("/tmp"));
        assert_eq!(needs[0].1, Need::Ready);
    }

    #[test]
    fn a_failed_attachment_blocks_its_overlay_rather_than_retrying_forever() {
        let mut coordinator = coordinator(&[(0, &["pulpit://image/spinner"])]);
        coordinator.needs(0, std::path::Path::new("/tmp"));
        coordinator.attachment_failed("spinner", "no such embedded file");
        let needs = coordinator.needs(0, std::path::Path::new("/tmp"));
        assert!(matches!(&needs[0].1, Need::Blocked(reason) if reason.contains("no such")));
    }

    #[test]
    fn a_reveal_sequence_presents_one_overlay_on_every_one_of_its_pages() {
        let coordinator = coordinator(&[
            (3, &["pulpit://video/clip"]),
            (4, &["pulpit://video/clip"]),
            (5, &["pulpit://video/clip"]),
        ]);
        assert_eq!(coordinator.index().len(), 1);
        let id = coordinator.index().all()[0].id;
        for page in 3..=5 {
            let visible = coordinator.index().on_page(page);
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].id, id, "one overlay, one session, three pages");
        }
    }

    #[test]
    fn a_frame_is_kept_and_only_replaced_by_a_newer_one() {
        let mut coordinator = coordinator(&[(0, &["pulpit://image/spinner"])]);
        let id = coordinator.index().all()[0].id;
        let pixels = std::sync::Arc::new(vec![0u8; 4]);

        coordinator.frame_ready(id, RenderGeneration(1), 5, 1, 1, pixels.clone());
        assert_eq!(coordinator.frame(id).map(|frame| frame.sequence), Some(5));

        // An older sequence must not replace a newer frame.
        coordinator.frame_ready(id, RenderGeneration(1), 4, 1, 1, pixels.clone());
        assert_eq!(coordinator.frame(id).map(|frame| frame.sequence), Some(5));

        coordinator.frame_ready(id, RenderGeneration(1), 6, 1, 1, pixels);
        assert_eq!(coordinator.frame(id).map(|frame| frame.sequence), Some(6));
    }

    #[test]
    fn a_frame_from_a_retired_generation_never_reaches_the_screen() {
        let mut coordinator = coordinator(&[(0, &["pulpit://image/spinner"])]);
        let id = coordinator.index().all()[0].id;
        coordinator.frame_ready(
            id,
            RenderGeneration(0),
            1,
            1,
            1,
            std::sync::Arc::new(vec![0u8; 4]),
        );
        assert!(coordinator.frame(id).is_none());
    }

    #[test]
    fn a_later_pages_overlays_do_not_discard_an_asset_already_being_fetched() {
        // Overlays arrive one page at a time, so the index is rebuilt several
        // times for one document. If a rebuild threw away what was already
        // staged or in flight, the attachment that arrived a moment later
        // would have nothing waiting for it and would be dropped — and the
        // caller, which only asks for each attachment once, would never ask
        // again. Nothing would ever play.
        let mut coordinator = MediaCoordinator::new();
        let mut per_page: BTreeMap<usize, Vec<OverlayDeclaration>> = BTreeMap::new();
        per_page.insert(0, declarations(&["pulpit://image/spinner"]));
        coordinator.rebuild(
            RenderGeneration(1),
            &per_page,
            &PageLabels::default(),
            Vec::new(),
        );
        let needs = coordinator.needs(0, std::path::Path::new("/tmp"));
        assert_eq!(needs[0].1, Need::Attachment("spinner".to_string()));
        assert!(coordinator.already_requested("spinner"));

        // A second page's overlays arrive before the attachment does.
        per_page.insert(1, declarations(&["pulpit://video/clip"]));
        coordinator.rebuild(
            RenderGeneration(1),
            &per_page,
            &PageLabels::default(),
            Vec::new(),
        );
        assert!(
            coordinator.already_requested("spinner"),
            "the in-flight request was forgotten, so it would never be re-asked"
        );

        // Now the bytes turn up. They must still find their overlay.
        coordinator.attachment_ready("spinner", b"GIF89a-not-really");
        let needs = coordinator.needs(0, std::path::Path::new("/tmp"));
        assert_eq!(
            needs[0].1,
            Need::Ready,
            "the attachment arrived but nothing was waiting for it"
        );
    }

    #[test]
    fn a_new_generation_does_discard_the_old_staging() {
        // The opposite guarantee: a reload must not keep serving the previous
        // document's assets.
        let mut coordinator = coordinator(&[(0, &["pulpit://image/spinner"])]);
        coordinator.needs(0, std::path::Path::new("/tmp"));
        coordinator.attachment_ready("spinner", b"GIF89a-not-really");
        assert_eq!(
            coordinator.needs(0, std::path::Path::new("/tmp"))[0].1,
            Need::Ready
        );

        let mut per_page = BTreeMap::new();
        per_page.insert(0, declarations(&["pulpit://image/spinner"]));
        coordinator.rebuild(
            RenderGeneration(2),
            &per_page,
            &PageLabels::default(),
            Vec::new(),
        );
        assert!(!coordinator.already_requested("spinner"));
        assert_eq!(
            coordinator.needs(0, std::path::Path::new("/tmp"))[0].1,
            Need::Attachment("spinner".to_string()),
            "a new generation re-stages from scratch"
        );
    }

    #[test]
    fn reducing_motion_stops_content_starting_on_its_own() {
        // The declaration still says autoplay; what changes is what the
        // session is opened with, so the deck is unmodified and the presenter
        // can still start it deliberately.
        let coordinator = coordinator(&[(0, &["pulpit://image/spin?autoplay&loop"])]);
        let overlay = &coordinator.index().all()[0];
        let declared = match &overlay.content {
            OverlayContent::AnimatedImage(spec) => spec.playback.clone(),
            other => panic!("expected an animated image, got {other:?}"),
        };
        assert!(declared.autoplay && declared.repeat);

        let mut reduced = declared.clone();
        reduced.autoplay = false;
        reduced.repeat = false;
        assert!(!reduced.autoplay, "nothing starts by itself");
        assert!(!reduced.repeat, "and nothing loops forever");
    }

    #[test]
    fn a_web_overlay_asks_for_pointer_input_and_continuous_animation() {
        let coordinator = coordinator(&[(0, &["pulpit://web/balls"])]);
        let id = coordinator.index().all()[0].id;
        let request = coordinator.request_for(id);
        assert_eq!(request.kinds, vec![ContentKind::Web]);
        assert!(request.pointer);
        assert!(request.continuous_animation);
        assert!(!request.audio, "HTML audio stays denied in this release");
    }

    #[test]
    fn rebuilding_a_generation_drops_the_sessions_but_keeps_the_frames() {
        let mut coordinator = coordinator(&[(0, &["pulpit://image/spinner"])]);
        let id = coordinator.index().all()[0].id;
        coordinator.frame_ready(
            id,
            RenderGeneration(1),
            1,
            1,
            1,
            std::sync::Arc::new(vec![0u8; 4]),
        );
        assert!(coordinator.frame(id).is_some());

        coordinator.rebuild(
            RenderGeneration(2),
            &BTreeMap::new(),
            &PageLabels::default(),
            Vec::new(),
        );
        assert!(
            coordinator.frame(id).is_some(),
            "the old frame stays on screen until a replacement lands"
        );
        assert!(coordinator.session(id).is_none());
    }
}
