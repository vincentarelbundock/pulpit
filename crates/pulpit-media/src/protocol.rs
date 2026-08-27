//! The versioned media worker protocol (`docs-src/internals.typ`).
//!
//! Deliberately separate from `pulpit-render::protocol`: a render is a
//! request that produces one answer, while a media session is continuous and
//! interactive. The *principles* are shared — versioned messages, bounded
//! length prefixes, validation before allocation, generation identifiers,
//! pixels through shared memory, explicit shutdown — but the message shapes
//! are not.

use std::io::{Read, Write};
use std::time::Duration;

use pulpit_core::overlay::{ContentKind, PlaybackParams};
use pulpit_core::{OverlayId, RenderGeneration};
use serde::{Deserialize, Serialize};

/// Bumped whenever the wire format changes. A worker answering with another
/// version is shut down rather than trusted.
pub const MEDIA_PROTOCOL_VERSION: u32 = 2;

/// Hard ceiling on one encoded message. Frames are metadata only — pixels
/// travel through shared memory — so this bounds a hostile or corrupt stream
/// rather than any legitimate payload.
pub const MAX_MESSAGE_BYTES: u32 = 1 << 20;

/// Hard ceiling on one surface slot: a 8k × 8k BGRA frame.
pub const MAX_SLOT_BYTES: u64 = 8_192 * 8_192 * 4;

/// Ceiling on one JSON message from page JavaScript through the bridge.
pub const MAX_WEB_MESSAGE_BYTES: usize = 16 * 1024;

/// Why a message could not be moved across the pipe.
///
/// One definition for both workers, in `pulpit-core`: the envelope is the same
/// problem whatever is inside it, and the half where a mistake is a security
/// bug rather than a wrong picture.
pub use pulpit_core::ipc::ProtocolError;

/// Identifies one media session for its whole lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session#{}", self.0)
    }
}

/// Identifies one continuously updated surface within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SurfaceId(pub u64);

/// One slot in a session's shared-memory ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SurfaceSlot(pub u32);

/// Which runtime a probe or session refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RuntimeId {
    /// An externally installed Chromium-family browser driven over CDP.
    ExternalChromium,
    /// An externally installed libmpv, decoding video and animated images —
    /// with audio — into shared memory. No JPEG round trip and no browser:
    /// for plain media this is an order of magnitude cheaper than the
    /// screencast path, which remains the fallback.
    LibMpv,
    /// WebKitGTK snapshots (Linux).
    WebKitGtk,
    /// WebView2 snapshots (Windows).
    WebView2,
    /// WKWebView snapshots (macOS).
    WkWebView,
}

impl RuntimeId {
    pub const ALL: [RuntimeId; 5] = [
        RuntimeId::ExternalChromium,
        RuntimeId::LibMpv,
        RuntimeId::WebKitGtk,
        RuntimeId::WebView2,
        RuntimeId::WkWebView,
    ];

    /// The stable name used in settings, diagnostics and the CLI.
    pub fn slug(self) -> &'static str {
        match self {
            RuntimeId::ExternalChromium => "external-chromium",
            RuntimeId::LibMpv => "libmpv",
            RuntimeId::WebKitGtk => "webkitgtk",
            RuntimeId::WebView2 => "webview2",
            RuntimeId::WkWebView => "wkwebview",
        }
    }

    pub fn from_slug(slug: &str) -> Option<RuntimeId> {
        RuntimeId::ALL
            .into_iter()
            .find(|runtime| runtime.slug() == slug)
    }

    /// The worker executable that hosts this runtime.
    pub fn worker_binary(self) -> &'static str {
        match self {
            RuntimeId::ExternalChromium => "pulpit-chromium-worker",
            RuntimeId::LibMpv => "pulpit-mpv-worker",
            RuntimeId::WebKitGtk => "pulpit-webkitgtk-worker",
            RuntimeId::WebView2 => "pulpit-webview2-worker",
            RuntimeId::WkWebView => "pulpit-wkwebview-worker",
        }
    }
}

impl std::fmt::Display for RuntimeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

/// Pixel layouts a worker may publish. Alpha handling is an explicit protocol
/// field precisely so no side has to assume it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PixelFormat {
    /// 8 bits per channel, alpha premultiplied.
    Rgba8Premultiplied,
    /// 8 bits per channel, alpha premultiplied.
    Bgra8Premultiplied,
    /// 8 bits per channel, straight (unassociated) alpha.
    Rgba8Straight,
}

impl PixelFormat {
    pub fn bytes_per_pixel(self) -> u32 {
        4
    }

    /// True when a consumer expecting RGBA can upload these bytes unchanged.
    pub fn is_rgba_order(self) -> bool {
        matches!(
            self,
            PixelFormat::Rgba8Premultiplied | PixelFormat::Rgba8Straight
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Metadata for one complete frame sitting in a shared-memory slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceFrame {
    pub session: SessionId,
    pub surface: SurfaceId,
    /// Monotonic per surface. Out-of-order or repeated sequences are dropped.
    pub sequence: u64,
    pub presentation_time: Option<Duration>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
    pub damage: Vec<PixelRect>,
    pub slot: SurfaceSlot,
    /// Bytes the worker actually wrote.
    pub bytes: u64,
}

impl SurfaceFrame {
    /// Validate every field that will size a mapping or a copy, *before* the
    /// consumer maps or copies anything. `slot_bytes` is the size the
    /// supervisor allocated, which the worker cannot change.
    pub fn validate(&self, slot_bytes: u64, slots: u32) -> Result<(), ProtocolError> {
        if self.width == 0 || self.height == 0 {
            return Err(ProtocolError::Malformed("zero-sized frame".into()));
        }
        if self.slot.0 >= slots {
            return Err(ProtocolError::Malformed(format!(
                "slot {} is outside the {slots}-slot ring",
                self.slot.0
            )));
        }
        let minimum_stride = self
            .width
            .checked_mul(self.format.bytes_per_pixel())
            .ok_or_else(|| ProtocolError::Malformed("row overflows".into()))?;
        if self.stride < minimum_stride {
            return Err(ProtocolError::Malformed(format!(
                "stride {} cannot hold a {}px row",
                self.stride, self.width
            )));
        }
        let needed = (self.stride as u64)
            .checked_mul(self.height as u64)
            .ok_or_else(|| ProtocolError::Malformed("frame overflows".into()))?;
        if needed > slot_bytes || self.bytes > slot_bytes || needed > MAX_SLOT_BYTES {
            return Err(ProtocolError::Malformed(format!(
                "frame of {needed} bytes does not fit a {slot_bytes} byte slot"
            )));
        }
        if self.bytes < needed {
            return Err(ProtocolError::Malformed(format!(
                "worker declared {} bytes but a frame needs {needed}",
                self.bytes
            )));
        }
        for rect in &self.damage {
            let right = rect.x.checked_add(rect.width);
            let bottom = rect.y.checked_add(rect.height);
            match (right, bottom) {
                (Some(right), Some(bottom)) if right <= self.width && bottom <= self.height => {}
                _ => {
                    return Err(ProtocolError::Malformed(
                        "a damage rectangle leaves the frame".into(),
                    ))
                }
            }
        }
        Ok(())
    }
}

/// The viewport a session renders into, in physical pixels plus the scale
/// that produced them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

impl Viewport {
    pub fn new(width: u32, height: u32, scale: f32) -> Self {
        Self {
            width,
            height,
            scale,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.width == 0 || self.height == 0 {
            return Err(ProtocolError::Malformed("zero-sized viewport".into()));
        }
        if !self.scale.is_finite() || self.scale <= 0.0 || self.scale > 8.0 {
            return Err(ProtocolError::Malformed(format!(
                "implausible device scale {}",
                self.scale
            )));
        }
        let bytes = (self.width as u64) * (self.height as u64) * 4;
        if bytes > MAX_SLOT_BYTES {
            return Err(ProtocolError::Malformed(format!(
                "viewport {}×{} exceeds the surface limit",
                self.width, self.height
            )));
        }
        Ok(())
    }

    /// CSS pixels, which is what a web runtime is told about.
    pub fn css_size(&self) -> (u32, u32) {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        (
            ((self.width as f32) / scale).round().max(1.0) as u32,
            ((self.height as f32) / scale).round().max(1.0) as u32,
        )
    }
}

/// Where a session's bytes live once staging has run. Workers receive real
/// paths, never PDF or ZIP names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionSource {
    /// A single staged media file.
    File { path: String },
    /// A staged web bundle: its root, and the entrypoint inside it.
    Bundle { root: String, entrypoint: String },
}

/// Everything a worker needs to open one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSpec {
    pub session: SessionId,
    pub surface: SurfaceId,
    /// The document generation this session belongs to. Events carrying a
    /// retired generation are dropped before they reach UI state.
    pub generation: RenderGeneration,
    pub overlay: OverlayId,
    pub kind: ContentKind,
    pub source: SessionSource,
    pub viewport: Viewport,
    pub playback: PlaybackParams,
    /// Name of the shared-memory ring the worker writes frames into.
    pub ring_name: String,
    pub slots: u32,
    pub slot_bytes: u64,
    /// The most frames per second the worker should decode and publish.
    /// The browser may paint faster; frames beyond this rate are acknowledged
    /// and discarded before their decode is ever paid. Zero means uncapped.
    pub max_fps: u32,
}

impl SessionSpec {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.viewport.validate()?;
        if self.slots == 0 || self.slots > 8 {
            return Err(ProtocolError::Malformed(format!(
                "{} surface slots is outside the supported range",
                self.slots
            )));
        }
        if self.slot_bytes == 0 || self.slot_bytes > MAX_SLOT_BYTES {
            return Err(ProtocolError::Malformed(format!(
                "slot size {} out of range",
                self.slot_bytes
            )));
        }
        if self.ring_name.is_empty() || self.ring_name.len() > 256 {
            return Err(ProtocolError::Malformed("bad shared-memory name".into()));
        }
        if self.ring_name.contains(['/', '\\', '\0']) {
            return Err(ProtocolError::Malformed(
                "shared-memory name must not contain path separators".into(),
            ));
        }
        match &self.source {
            SessionSource::File { path } => {
                if path.is_empty() {
                    return Err(ProtocolError::Malformed("empty source path".into()));
                }
            }
            SessionSource::Bundle { root, entrypoint } => {
                if root.is_empty() || entrypoint.is_empty() {
                    return Err(ProtocolError::Malformed("empty bundle path".into()));
                }
            }
        }
        // A worker only ever receives commands its content kind can answer;
        // the pairing is checked here so an image worker never has to reason
        // about a web session at all.
        match (self.kind, &self.source) {
            (ContentKind::Web, SessionSource::Bundle { .. }) => Ok(()),
            (ContentKind::Web, SessionSource::File { .. }) => Err(ProtocolError::Malformed(
                "a web session needs a bundle, not a file".into(),
            )),
            (_, SessionSource::Bundle { .. }) => Err(ProtocolError::Malformed(
                "only a web session takes a bundle".into(),
            )),
            _ => Ok(()),
        }
    }
}

/// Pointer buttons a presenter can press. Deliberately not a bitmask: the
/// runtime adapters translate one button at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

/// Input, already transformed out of window space by the application.
///
/// Coordinates for web content are CSS viewport pixels; image and video
/// controls receive normalised content coordinates. Letterboxed pixels never
/// produce an event at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    PointerMoved {
        x: f32,
        y: f32,
    },
    PointerLeft,
    PointerPressed {
        x: f32,
        y: f32,
        button: PointerButton,
        click_count: u32,
    },
    PointerReleased {
        x: f32,
        y: f32,
        button: PointerButton,
        click_count: u32,
    },
    Wheel {
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
    },
    KeyPressed {
        key: String,
        text: Option<String>,
    },
    KeyReleased {
        key: String,
    },
}

impl InputEvent {
    /// Reject implausible geometry and unbounded strings before a worker
    /// forwards them to a browser.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        const MAX_KEY_BYTES: usize = 64;
        let finite = |values: &[f32]| values.iter().all(|value| value.is_finite());
        let ok = match self {
            InputEvent::PointerMoved { x, y } => finite(&[*x, *y]),
            InputEvent::PointerLeft => true,
            InputEvent::PointerPressed { x, y, .. } | InputEvent::PointerReleased { x, y, .. } => {
                finite(&[*x, *y])
            }
            InputEvent::Wheel {
                x,
                y,
                delta_x,
                delta_y,
            } => finite(&[*x, *y, *delta_x, *delta_y]),
            InputEvent::KeyPressed { key, text } => {
                key.len() <= MAX_KEY_BYTES
                    && text.as_ref().is_none_or(|text| text.len() <= MAX_KEY_BYTES)
            }
            InputEvent::KeyReleased { key } => key.len() <= MAX_KEY_BYTES,
        };
        ok.then_some(())
            .ok_or_else(|| ProtocolError::Malformed("unusable input event".into()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ImageCommand {
    Play,
    Pause,
    /// Return to the first frame of the loop.
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum VideoCommand {
    Play,
    Pause,
    Seek { seconds: f32 },
    SetVolume { level: f32 },
    SetMuted { muted: bool },
    SetLooping { looping: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WebCommand {
    /// Discard JavaScript state and load the entrypoint again.
    Reload,
    /// A bounded, host-authored message *into* the page. There is
    /// deliberately no generic `eval`.
    Post { value: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MediaRequest {
    Hello {
        version: u32,
    },
    Probe(CapabilityRequest),
    Open(Box<SessionSpec>),
    Close {
        session: SessionId,
    },
    SetActive {
        session: SessionId,
        active: bool,
    },
    SetViewport {
        session: SessionId,
        viewport: Viewport,
    },
    SetFocus {
        session: SessionId,
        focused: bool,
    },
    Input {
        session: SessionId,
        event: InputEvent,
    },
    Image {
        session: SessionId,
        command: ImageCommand,
    },
    Video {
        session: SessionId,
        command: VideoCommand,
    },
    Web {
        session: SessionId,
        command: WebCommand,
    },
    /// The application has finished with a slot; the worker may write into it.
    ReleaseFrame {
        session: SessionId,
        slot: SurfaceSlot,
        sequence: u64,
    },
    Shutdown,
}

/// What the application needs of a runtime for one particular overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CapabilityRequest {
    pub kinds: Vec<ContentKind>,
    pub pointer: bool,
    pub keyboard: bool,
    pub webgl: bool,
    pub continuous_animation: bool,
    pub audio: bool,
}

impl CapabilityRequest {
    pub fn for_kind(kind: ContentKind) -> Self {
        Self {
            kinds: vec![kind],
            continuous_animation: true,
            pointer: matches!(kind, ContentKind::Web),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerDescription {
    pub version: u32,
    pub runtime: RuntimeId,
    /// Cargo features this worker was built with, for diagnostics.
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Loading,
    Playing,
    Paused,
    Ended,
}

/// Where a playing session has got to.
///
/// [`SessionState`] says whether something is running; this says where it is.
/// A presenter-side transport cannot draw a scrub bar without it, and the
/// alternative — asking the runtime on every frame — would put a round trip
/// in the frame path to answer a question the content already knows.
///
/// `duration` is optional because a stream, and a video whose metadata has
/// not loaded yet, genuinely do not have one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlaybackProgress {
    pub position: f32,
    pub duration: Option<f32>,
    pub paused: bool,
    pub muted: bool,
    pub volume: f32,
}

impl PlaybackProgress {
    /// Repair anything a runtime reported that cannot be true.
    ///
    /// The numbers come from a media element in a browser, which reports
    /// `NaN` for the duration of a stream and can report a position past a
    /// duration mid-seek. A transport that trusted either draws a slider
    /// with a backwards range.
    pub fn sanitised(mut self) -> PlaybackProgress {
        self.position = if self.position.is_finite() {
            self.position.max(0.0)
        } else {
            0.0
        };
        self.duration = self
            .duration
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0);
        if let Some(duration) = self.duration {
            self.position = self.position.min(duration);
        }
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self
    }

    /// How far through, 0.0 to 1.0. `None` without a known duration.
    pub fn fraction(&self) -> Option<f32> {
        self.duration
            .map(|duration| (self.position / duration).clamp(0.0, 1.0))
    }
}

/// The presenter-visible cursor a web runtime asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorIcon {
    Default,
    Pointer,
    Text,
    Grab,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MediaWarning {
    /// Frames were dropped because the consumer was behind. Diagnostic only.
    DroppedFrames { count: u64 },
    /// The runtime restarted; arbitrary JavaScript state is gone.
    ContentRestarted,
    /// A documented degradation, e.g. snapshot-only rendering.
    Degraded { detail: String },
    /// The declared MIME type and the sniffed bytes disagree.
    TypeMismatch { declared: String, sniffed: String },
}

impl std::fmt::Display for MediaWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaWarning::DroppedFrames { count } => write!(f, "dropped {count} frames"),
            MediaWarning::ContentRestarted => {
                write!(f, "the interactive content restarted and lost its state")
            }
            MediaWarning::Degraded { detail } => write!(f, "running degraded: {detail}"),
            MediaWarning::TypeMismatch { declared, sniffed } => {
                write!(f, "declared {declared} but the bytes look like {sniffed}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaErrorKind {
    Unavailable,
    Incompatible,
    Unsupported,
    PolicyDenied,
    MalformedAsset,
    LaunchFailed,
    LoadFailed,
    DecodeFailed,
    ProtocolViolation,
    TimedOut,
    Crashed,
    ResourceLimit,
}

impl MediaErrorKind {
    /// May the supervisor try the *next* runtime after this?
    ///
    /// A policy denial must never be bypassed by looking for a more
    /// permissive runtime (`docs-src/internals.typ`).
    pub fn allows_fallback(self) -> bool {
        !matches!(self, MediaErrorKind::PolicyDenied)
    }

    /// Is retrying the *same* worker worthwhile?
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            MediaErrorKind::Crashed | MediaErrorKind::TimedOut | MediaErrorKind::ResourceLimit
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaError {
    pub kind: MediaErrorKind,
    pub runtime: Option<RuntimeId>,
    pub overlay: Option<OverlayId>,
    pub generation: Option<RenderGeneration>,
    /// Safe to show a presenter mid-presentation: no paths, no page source.
    pub message: String,
}

impl MediaError {
    pub fn new(kind: MediaErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            runtime: None,
            overlay: None,
            generation: None,
            message: message.into(),
        }
    }

    pub fn with_runtime(mut self, runtime: RuntimeId) -> Self {
        self.runtime = Some(runtime);
        self
    }
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MediaEvent {
    Hello(WorkerDescription),
    ProbeResult(Box<crate::capability::RuntimeProbe>),
    Ready {
        session: SessionId,
    },
    FrameReady(SurfaceFrame),
    StateChanged {
        session: SessionId,
        state: SessionState,
    },
    /// Where playback has reached. Sent as the content reports it, not on a
    /// clock of the worker's own.
    Progress {
        session: SessionId,
        progress: PlaybackProgress,
    },
    CursorChanged {
        session: SessionId,
        cursor: CursorIcon,
    },
    WebMessage {
        session: SessionId,
        value: String,
    },
    Warning {
        session: Option<SessionId>,
        warning: MediaWarning,
    },
    Failed {
        session: Option<SessionId>,
        error: MediaError,
    },
    Closed {
        session: SessionId,
    },
    /// Cumulative pipeline totals for the whole worker, sent periodically so
    /// the application can account for work it never sees — frames dropped
    /// before decode cost nothing downstream and would otherwise be invisible.
    Counters(WorkerCounters),
}

/// Cumulative frame-pipeline totals for one worker process. Every field only
/// grows; rates are the consumer's business.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCounters {
    /// Screencast frames the browser delivered over CDP.
    pub cdp_frames_received: u64,
    /// Frames acknowledged but discarded before decode — stale within a
    /// batch, beyond the publish deadline, or for an inactive session.
    pub frames_discarded_before_decode: u64,
    /// Frames whose JPEG was actually decoded.
    pub frames_decoded: u64,
    /// Decoded frames that needed a real resample to fit the viewport.
    pub frames_scaled: u64,
    /// Decoded frames written to the ring without a scaling pass.
    pub frames_scale_elided: u64,
    /// Frames published to the surface ring.
    pub frames_published: u64,
    /// Frames dropped because every ring slot was still held.
    pub ring_dropped: u64,
}

/// Write one length-prefixed message, at this protocol's ceiling.
pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
) -> Result<(), ProtocolError> {
    pulpit_core::ipc::write_message(writer, message, MAX_MESSAGE_BYTES)
}

/// Read one length-prefixed message, refusing implausible lengths before
/// allocating anything.
pub fn read_message<T: serde::de::DeserializeOwned>(
    reader: &mut impl Read,
) -> Result<T, ProtocolError> {
    pulpit_core::ipc::read_message(reader, MAX_MESSAGE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> SurfaceFrame {
        SurfaceFrame {
            session: SessionId(1),
            surface: SurfaceId(1),
            sequence: 4,
            presentation_time: None,
            width: 640,
            height: 360,
            stride: 640 * 4,
            format: PixelFormat::Bgra8Premultiplied,
            damage: Vec::new(),
            slot: SurfaceSlot(0),
            bytes: 640 * 360 * 4,
        }
    }

    fn spec() -> SessionSpec {
        SessionSpec {
            session: SessionId(1),
            surface: SurfaceId(1),
            generation: RenderGeneration(2),
            overlay: OverlayId(7),
            kind: ContentKind::AnimatedImage,
            source: SessionSource::File {
                path: "/staged/asset-1.gif".into(),
            },
            viewport: Viewport::new(640, 360, 1.0),
            playback: PlaybackParams::default(),
            ring_name: "pulpit-media-1".into(),
            slots: 3,
            slot_bytes: 640 * 360 * 4,
            max_fps: 30,
        }
    }

    #[test]
    fn messages_round_trip() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &MediaRequest::Open(Box::new(spec()))).unwrap();
        write_message(
            &mut buffer,
            &MediaRequest::ReleaseFrame {
                session: SessionId(1),
                slot: SurfaceSlot(0),
                sequence: 4,
            },
        )
        .unwrap();

        let mut cursor = std::io::Cursor::new(buffer);
        let first: MediaRequest = read_message(&mut cursor).unwrap();
        assert_eq!(first, MediaRequest::Open(Box::new(spec())));
        let second: MediaRequest = read_message(&mut cursor).unwrap();
        assert!(matches!(second, MediaRequest::ReleaseFrame { .. }));
    }

    #[test]
    fn an_oversized_length_prefix_is_refused_before_allocating() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&(MAX_MESSAGE_BYTES + 1).to_le_bytes());
        let mut cursor = std::io::Cursor::new(buffer);
        assert!(matches!(
            read_message::<MediaRequest>(&mut cursor),
            Err(ProtocolError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_truncated_stream_reports_closure_not_corruption() {
        let mut cursor = std::io::Cursor::new(vec![1u8, 0, 0]);
        assert!(matches!(
            read_message::<MediaRequest>(&mut cursor),
            Err(ProtocolError::Closed)
        ));
    }

    #[test]
    fn a_well_formed_frame_validates() {
        assert!(frame().validate(640 * 360 * 4, 3).is_ok());
    }

    #[test]
    fn a_frame_that_would_read_past_its_slot_is_refused() {
        let slot_bytes = 640 * 360 * 4;
        for bad in [
            SurfaceFrame {
                height: 361,
                bytes: 640 * 361 * 4,
                ..frame()
            },
            SurfaceFrame {
                stride: 640 * 4 - 1,
                ..frame()
            },
            SurfaceFrame {
                width: 0,
                ..frame()
            },
            SurfaceFrame {
                bytes: slot_bytes + 1,
                ..frame()
            },
            // Claiming fewer bytes than the geometry needs would leave the
            // tail of the frame undefined.
            SurfaceFrame {
                bytes: 1024,
                ..frame()
            },
        ] {
            assert!(
                bad.validate(slot_bytes, 3).is_err(),
                "{bad:?} should not validate"
            );
        }
    }

    #[test]
    fn a_slot_outside_the_ring_is_refused() {
        let bad = SurfaceFrame {
            slot: SurfaceSlot(3),
            ..frame()
        };
        assert!(bad.validate(640 * 360 * 4, 3).is_err());
    }

    #[test]
    fn damage_must_stay_inside_the_frame() {
        let bad = SurfaceFrame {
            damage: vec![PixelRect {
                x: 600,
                y: 0,
                width: 100,
                height: 10,
            }],
            ..frame()
        };
        assert!(bad.validate(640 * 360 * 4, 3).is_err());

        let good = SurfaceFrame {
            damage: vec![PixelRect {
                x: 600,
                y: 0,
                width: 40,
                height: 10,
            }],
            ..frame()
        };
        assert!(good.validate(640 * 360 * 4, 3).is_ok());
    }

    #[test]
    fn a_session_spec_is_validated_before_anything_is_mapped() {
        assert!(spec().validate().is_ok());
        for bad in [
            SessionSpec { slots: 0, ..spec() },
            SessionSpec {
                slot_bytes: 0,
                ..spec()
            },
            SessionSpec {
                ring_name: "has/separator".into(),
                ..spec()
            },
            SessionSpec {
                ring_name: String::new(),
                ..spec()
            },
            SessionSpec {
                viewport: Viewport::new(0, 360, 1.0),
                ..spec()
            },
            SessionSpec {
                viewport: Viewport::new(640, 360, 0.0),
                ..spec()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?} should not validate");
        }
    }

    #[test]
    fn an_image_session_may_not_be_handed_a_web_bundle() {
        let bad = SessionSpec {
            source: SessionSource::Bundle {
                root: "/staged/balls".into(),
                entrypoint: "index.html".into(),
            },
            ..spec()
        };
        assert!(bad.validate().is_err());

        let good = SessionSpec {
            kind: ContentKind::Web,
            source: SessionSource::Bundle {
                root: "/staged/balls".into(),
                entrypoint: "index.html".into(),
            },
            ..spec()
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn a_web_session_may_not_be_handed_a_bare_file() {
        let bad = SessionSpec {
            kind: ContentKind::Web,
            ..spec()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn input_events_with_unusable_geometry_are_refused() {
        assert!(InputEvent::PointerMoved { x: 1.0, y: 2.0 }
            .validate()
            .is_ok());
        assert!(InputEvent::PointerMoved {
            x: f32::NAN,
            y: 2.0
        }
        .validate()
        .is_err());
        assert!(InputEvent::KeyPressed {
            key: "a".repeat(200),
            text: None
        }
        .validate()
        .is_err());
    }

    #[test]
    fn a_policy_denial_never_falls_through_to_a_laxer_runtime() {
        assert!(!MediaErrorKind::PolicyDenied.allows_fallback());
        for kind in [
            MediaErrorKind::Unavailable,
            MediaErrorKind::LaunchFailed,
            MediaErrorKind::DecodeFailed,
            MediaErrorKind::Crashed,
        ] {
            assert!(kind.allows_fallback(), "{kind:?} should allow fallback");
        }
    }

    #[test]
    fn only_genuinely_transient_failures_restart_the_same_worker() {
        assert!(MediaErrorKind::Crashed.is_transient());
        assert!(MediaErrorKind::TimedOut.is_transient());
        assert!(!MediaErrorKind::Unsupported.is_transient());
        assert!(!MediaErrorKind::MalformedAsset.is_transient());
    }

    #[test]
    fn runtime_slugs_round_trip_so_settings_stay_stable() {
        for runtime in RuntimeId::ALL {
            assert_eq!(RuntimeId::from_slug(runtime.slug()), Some(runtime));
        }
        assert_eq!(RuntimeId::from_slug("nonesuch"), None);
    }

    #[test]
    fn css_size_undoes_the_device_scale() {
        assert_eq!(Viewport::new(2560, 1440, 2.0).css_size(), (1280, 720));
        assert_eq!(Viewport::new(1280, 720, 1.0).css_size(), (1280, 720));
    }
}
