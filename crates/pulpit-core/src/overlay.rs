//! The pure overlay domain: identifiers, geometry, content descriptors and
//! the `pulpit://` authoring convention (`docs-src/internals.typ`).
//!
//! This module owns no process, socket, decoder or filesystem handle. It
//! parses what a producer wrote into the PDF and decides what that *means*;
//! acting on the meaning belongs to `pulpit-media` and `pulpit`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::notes::Region;

/// Identifies one *logical* overlay for the lifetime of a document
/// generation. An overlay repeated across the physical pages of one
/// incremental-reveal slide keeps a single identifier (see
/// [`OverlayIndex`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OverlayId(pub u64);

impl std::fmt::Display for OverlayId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "overlay#{}", self.0)
    }
}

/// An entry in the PDF embedded-files name tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetRef {
    /// The name-tree key, exactly as the producer wrote it.
    pub attachment: String,
}

impl AssetRef {
    pub fn new(attachment: impl Into<String>) -> Self {
        Self {
            attachment: attachment.into(),
        }
    }
}

/// A file beside the document, resolved against the document directory.
///
/// The path is stored in its decoded form but is *not* yet known to be safe:
/// [`ExternalAssetRef::is_containable`] rejects the obvious escapes, and the
/// renderer canonicalises before reading.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalAssetRef {
    pub path: String,
}

impl ExternalAssetRef {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    /// A cheap, pre-canonicalisation rejection of paths that can only be
    /// meant to escape the document directory.
    pub fn is_containable(&self) -> bool {
        let path = self.path.as_str();
        if path.is_empty() || path.len() > 1024 {
            return false;
        }
        if path.contains('\0') {
            return false;
        }
        // Absolute, UNC and drive-letter forms are never document-relative.
        if path.starts_with('/') || path.starts_with('\\') {
            return false;
        }
        if path.as_bytes().get(1) == Some(&b':') {
            return false;
        }
        !path
            .split(['/', '\\'])
            .any(|segment| segment == ".." || segment.trim().is_empty() && !segment.is_empty())
    }
}

/// Where a non-web overlay's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OverlaySource {
    Embedded(AssetRef),
    External(ExternalAssetRef),
}

impl OverlaySource {
    /// A stable identity string, used for logical-slide grouping.
    pub fn identity(&self) -> &str {
        match self {
            OverlaySource::Embedded(asset) => &asset.attachment,
            OverlaySource::External(asset) => &asset.path,
        }
    }
}

/// Playback intent declared in the URI query string (`docs-src/internals.typ`).
///
/// These are *declarations*, not permissions: the single-audio rule and the
/// activation policy still decide what actually happens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PlaybackParams {
    pub autoplay: bool,
    pub repeat: bool,
    pub mute: bool,
    /// Start offset in seconds. Video only; always finite and non-negative.
    pub start: f32,
    /// A still image shown until the first frame is complete.
    pub poster: Option<AssetRef>,
}

/// Anything the parser could not honour. Never fatal: an overlay with
/// warnings still plays, using defaults for the fields it could not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlayWarning {
    /// A query parameter the version 1 grammar does not define.
    UnknownParameter(String),
    /// A defined parameter whose value did not parse; the default was used.
    MalformedParameter { name: String, value: String },
}

impl std::fmt::Display for OverlayWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverlayWarning::UnknownParameter(name) => {
                write!(f, "ignored unknown media parameter `{name}`")
            }
            OverlayWarning::MalformedParameter { name, value } => {
                write!(
                    f,
                    "media parameter `{name}` has an unusable value `{value}`"
                )
            }
        }
    }
}

/// How much of a viewport a web bundle asks for, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LogicalSize {
    pub width: u32,
    pub height: u32,
}

/// What a bundle is allowed to reach. Everything is denied by default and a
/// manifest can only *request*; approval happens in preflight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WebPermissions {
    /// Origins the bundle asks to reach. Requesting is not being granted.
    pub network: Vec<String>,
    pub audio: bool,
}

impl WebPermissions {
    pub fn requests_anything(&self) -> bool {
        !self.network.is_empty() || self.audio
    }
}

/// Whether JavaScript state survives leaving the slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PersistencePolicy {
    /// State is discarded when the overlay is left. The safe default.
    #[default]
    Reset,
    /// State survives for as long as the document generation lives.
    PageSession,
}

/// What a bundle needs from a runtime. A runtime missing any of these is
/// skipped during selection even when it claims the content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebRequirements {
    pub pointer: bool,
    pub keyboard: bool,
    pub webgl: bool,
    pub continuous_animation: bool,
}

impl Default for WebRequirements {
    fn default() -> Self {
        Self {
            pointer: true,
            keyboard: false,
            webgl: false,
            continuous_animation: true,
        }
    }
}

/// A path inside a staged bundle. Always relative, never escaping.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RelativeAssetPath(pub String);

impl RelativeAssetPath {
    /// Bundle-internal paths are held to the same rule as the ZIP entries
    /// themselves: relative, no traversal, no NUL, no absolute prefix.
    pub fn is_safe(&self) -> bool {
        let path = self.0.as_str();
        if path.is_empty() || path.len() > 512 || path.contains('\0') {
            return false;
        }
        if path.starts_with('/') || path.starts_with('\\') {
            return false;
        }
        if path.as_bytes().get(1) == Some(&b':') {
            return false;
        }
        path.split(['/', '\\'])
            .all(|segment| !segment.is_empty() && segment != ".." && segment != ".")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimatedImageSpec {
    pub source: OverlaySource,
    pub playback: PlaybackParams,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoSpec {
    pub source: OverlaySource,
    pub playback: PlaybackParams,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSpec {
    /// Where the bundle's files come from: a ZIP attachment inside the PDF,
    /// or a directory beside the document. The same two possibilities every
    /// other content kind has, for the same reason.
    pub bundle: OverlaySource,
    pub entrypoint: RelativeAssetPath,
    pub viewport: Option<LogicalSize>,
    pub permissions: WebPermissions,
    pub persistence: PersistencePolicy,
    pub requirements: WebRequirements,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OverlayContent {
    AnimatedImage(AnimatedImageSpec),
    Video(VideoSpec),
    Web(WebSpec),
}

impl OverlayContent {
    pub fn kind(&self) -> ContentKind {
        match self {
            OverlayContent::AnimatedImage(_) => ContentKind::AnimatedImage,
            OverlayContent::Video(_) => ContentKind::Video,
            OverlayContent::Web(_) => ContentKind::Web,
        }
    }

    /// The identity used for logical-slide grouping: same content, same
    /// source, same playback intent.
    fn grouping_identity(&self) -> String {
        match self {
            OverlayContent::AnimatedImage(spec) => {
                format!("image:{}", spec.source.identity())
            }
            OverlayContent::Video(spec) => format!("video:{}", spec.source.identity()),
            OverlayContent::Web(spec) => {
                format!("web:{}/{}", spec.bundle.identity(), spec.entrypoint.0)
            }
        }
    }

    pub fn poster(&self) -> Option<&AssetRef> {
        match self {
            OverlayContent::AnimatedImage(spec) => spec.playback.poster.as_ref(),
            OverlayContent::Video(spec) => spec.playback.poster.as_ref(),
            OverlayContent::Web(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentKind {
    AnimatedImage,
    Video,
    Web,
}

impl ContentKind {
    pub fn label(self) -> &'static str {
        match self {
            ContentKind::AnimatedImage => "animated image",
            ContentKind::Video => "video",
            ContentKind::Web => "interactive HTML",
        }
    }
}

/// When an overlay's session is created and started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ActivationPolicy {
    /// Start once the audience page is committed.
    #[default]
    OnCommit,
    /// Create the session but wait for the presenter to start it.
    Manual,
}

/// One overlay, resolved onto one *logical* slide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageOverlay {
    pub id: OverlayId,
    /// The first physical PDF page this overlay appears on (zero-based).
    pub page: usize,
    /// Every physical page it appears on, ascending and contiguous.
    pub pages: Vec<usize>,
    pub region: Region,
    pub z_index: i32,
    pub activation: ActivationPolicy,
    pub poster: Option<AssetRef>,
    pub content: OverlayContent,
    pub warnings: Vec<OverlayWarning>,
}

impl PageOverlay {
    pub fn covers_page(&self, page: usize) -> bool {
        self.pages.contains(&page)
    }

    /// Does a normalised page point fall inside this overlay?
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.region.x
            && x <= self.region.x + self.region.width
            && y >= self.region.y
            && y <= self.region.y + self.region.height
    }

    /// Overlays are interactive only when the content can consume input.
    ///
    /// All three kinds now can. A video carries a transport — play, pause and
    /// a seek slider — and an animated image toggles between running and
    /// frozen on a click; both are drawn in the page the browser renders, so
    /// the press has to reach it. The cost is deliberate and worth stating:
    /// a click *on* a media overlay no longer advances the slide. Clicking
    /// anywhere else on the page still does.
    pub fn is_interactive(&self) -> bool {
        matches!(
            self.content,
            OverlayContent::Web(_) | OverlayContent::Video(_) | OverlayContent::AnimatedImage(_)
        )
    }
}

/// What a single link annotation declared, before logical-slide grouping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayDeclaration {
    pub region: Region,
    pub z_index: i32,
    pub activation: ActivationPolicy,
    pub content: OverlayContent,
    pub warnings: Vec<OverlayWarning>,
}

// ---------------------------------------------------------------------------
// URI convention
// ---------------------------------------------------------------------------

/// The scheme every pulpit authoring convention shares.
pub const PULPIT_SCHEME: &str = "pulpit://";

/// Extensions the pdfpc `run:` convention is willing to treat as video.
const RUN_VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "webm", "mkv", "ogv", "ogg", "avi", "mpg", "mpeg", "wmv",
];

/// Extensions the pdfpc `run:` convention is willing to treat as an animated
/// image.
const RUN_IMAGE_EXTENSIONS: &[&str] = &["gif", "apng", "webp"];

/// Extensions `run:` treats as an interactive HTML overlay.
///
/// pdfpc has no notion of this, but the convention extends to it naturally
/// and it saves a deck from needing a pulpit-specific URI for the one
/// content kind PDF never standardised.
const RUN_WEB_EXTENSIONS: &[&str] = &["html", "htm"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UriError {
    #[error("not a pulpit media URI")]
    NotMedia,
    #[error("unknown pulpit URI form `{0}`")]
    UnknownForm(String),
    #[error("the URI names no asset")]
    MissingAsset,
    #[error("the asset reference `{0}` escapes the document directory")]
    UnsafePath(String),
    #[error("the entrypoint `{0}` is not a safe bundle-relative path")]
    UnsafeEntrypoint(String),
}

/// Parse one link-annotation URI into an overlay declaration.
///
/// `region` is the link rectangle, which *is* the overlay's region: the
/// producer draws its poster inside the link, so geometry needs no separate
/// declaration.
pub fn parse_overlay_uri(uri: &str, region: Region) -> Result<OverlayDeclaration, UriError> {
    let trimmed = uri.trim();
    if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, PULPIT_SCHEME) {
        return parse_pulpit_uri(rest, region);
    }
    if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, "run:") {
        return parse_run_uri(rest, region);
    }
    Err(UriError::NotMedia)
}

/// Is this URI worth handing to [`parse_overlay_uri`] at all?
pub fn is_media_uri(uri: &str) -> bool {
    let trimmed = uri.trim();
    strip_prefix_ignore_ascii_case(trimmed, PULPIT_SCHEME).is_some()
        || strip_prefix_ignore_ascii_case(trimmed, "run:").is_some()
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    (value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

fn parse_pulpit_uri(rest: &str, region: Region) -> Result<OverlayDeclaration, UriError> {
    let (path, query) = split_query(rest);
    let mut segments = path.splitn(2, '/');
    let form = segments.next().unwrap_or_default().to_ascii_lowercase();
    let remainder = segments.next().unwrap_or_default();

    match form.as_str() {
        "web" => {
            // `web/<attachment-id>/<entrypoint>`; the entrypoint is optional
            // and, when present, only overrides a manifest that allows it.
            let (attachment, entrypoint) = match remainder.split_once('/') {
                Some((attachment, entrypoint)) => (attachment, entrypoint),
                None => (remainder, ""),
            };
            let attachment = decode(attachment);
            if attachment.is_empty() {
                return Err(UriError::MissingAsset);
            }
            let entrypoint = decode(entrypoint);
            let entrypoint = if entrypoint.is_empty() {
                RelativeAssetPath("index.html".to_string())
            } else {
                let candidate = RelativeAssetPath(entrypoint.clone());
                if !candidate.is_safe() {
                    return Err(UriError::UnsafeEntrypoint(entrypoint));
                }
                candidate
            };
            // A web overlay's real parameters live in its manifest; the query
            // string carries nothing the grammar defines, so anything there
            // is reported rather than guessed at.
            let mut warnings = Vec::new();
            for (name, _) in query_pairs(query) {
                warnings.push(OverlayWarning::UnknownParameter(name));
            }
            Ok(web_declaration(
                region,
                OverlaySource::Embedded(AssetRef::new(attachment)),
                entrypoint,
                warnings,
            ))
        }
        "video" | "image" => {
            let source = embedded_source(remainder)?;
            let (playback, warnings) = parse_playback(query, form == "video");
            Ok(media_declaration(
                region,
                form == "video",
                source,
                playback,
                warnings,
            ))
        }
        "video-external" | "image-external" => {
            let source = external_source(remainder)?;
            let is_video = form == "video-external";
            let (playback, warnings) = parse_playback(query, is_video);
            Ok(media_declaration(
                region, is_video, source, playback, warnings,
            ))
        }
        other => Err(UriError::UnknownForm(other.to_string())),
    }
}

/// The pdfpc convention: `run:clip.mp4?autostart&loop`.
fn parse_run_uri(rest: &str, region: Region) -> Result<OverlayDeclaration, UriError> {
    let (path, query) = split_query(rest);
    let path = decode(path);
    if path.is_empty() {
        return Err(UriError::MissingAsset);
    }
    let extension = path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let is_video = RUN_VIDEO_EXTENSIONS.contains(&extension.as_str());
    let is_image = RUN_IMAGE_EXTENSIONS.contains(&extension.as_str());
    let is_web = RUN_WEB_EXTENSIONS.contains(&extension.as_str());
    if !is_video && !is_image && !is_web {
        // A `run:` link to a program is never executed; it simply is not an
        // overlay.
        return Err(UriError::NotMedia);
    }
    let asset = ExternalAssetRef::new(path.clone());
    if !asset.is_containable() {
        return Err(UriError::UnsafePath(path));
    }
    if is_web {
        // The bundle reference is the HTML file itself; its directory becomes
        // the served root, so the page reaches its stylesheet and its script
        // exactly as it does on disk.
        let file = path
            .rsplit_once('/')
            .map_or(path.as_str(), |(_, file)| file);
        let entrypoint = RelativeAssetPath(file.to_string());
        if !entrypoint.is_safe() {
            return Err(UriError::UnsafeEntrypoint(path));
        }
        return Ok(web_declaration(
            region,
            OverlaySource::External(asset),
            entrypoint,
            Vec::new(),
        ));
    }
    let (playback, warnings) = parse_playback(query, is_video);
    Ok(media_declaration(
        region,
        is_video,
        OverlaySource::External(asset),
        playback,
        warnings,
    ))
}

/// A web overlay, however its bundle was reached.
///
/// Embedded and external bundles differ only in the [`OverlaySource`]; every
/// other field of the declaration is the same, and a second copy of the
/// defaults is a second place for them to drift.
fn web_declaration(
    region: Region,
    bundle: OverlaySource,
    entrypoint: RelativeAssetPath,
    warnings: Vec<OverlayWarning>,
) -> OverlayDeclaration {
    OverlayDeclaration {
        region,
        z_index: 0,
        activation: ActivationPolicy::OnCommit,
        content: OverlayContent::Web(WebSpec {
            bundle,
            entrypoint,
            viewport: None,
            permissions: WebPermissions::default(),
            persistence: PersistencePolicy::default(),
            requirements: WebRequirements::default(),
        }),
        warnings,
    }
}

fn media_declaration(
    region: Region,
    is_video: bool,
    source: OverlaySource,
    playback: PlaybackParams,
    warnings: Vec<OverlayWarning>,
) -> OverlayDeclaration {
    let content = if is_video {
        OverlayContent::Video(VideoSpec { source, playback })
    } else {
        OverlayContent::AnimatedImage(AnimatedImageSpec { source, playback })
    };
    OverlayDeclaration {
        region,
        z_index: 0,
        activation: ActivationPolicy::OnCommit,
        content,
        warnings,
    }
}

fn embedded_source(remainder: &str) -> Result<OverlaySource, UriError> {
    let attachment = decode(remainder.trim_end_matches('/'));
    if attachment.is_empty() {
        return Err(UriError::MissingAsset);
    }
    Ok(OverlaySource::Embedded(AssetRef::new(attachment)))
}

fn external_source(remainder: &str) -> Result<OverlaySource, UriError> {
    let path = decode(remainder);
    if path.is_empty() {
        return Err(UriError::MissingAsset);
    }
    let asset = ExternalAssetRef::new(path.clone());
    if !asset.is_containable() {
        return Err(UriError::UnsafePath(path));
    }
    Ok(OverlaySource::External(asset))
}

fn split_query(value: &str) -> (&str, &str) {
    match value.split_once('?') {
        Some((path, query)) => (path, query),
        None => (value, ""),
    }
}

fn query_pairs(query: &str) -> impl Iterator<Item = (String, Option<String>)> + '_ {
    query
        .split(['&', ';'])
        .filter(|pair| !pair.trim().is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) => (decode(name).to_ascii_lowercase(), Some(decode(value))),
            None => (decode(pair).to_ascii_lowercase(), None),
        })
}

/// Parse the version 1 playback grammar. Unknown names and unusable values
/// are collected as warnings; the overlay still plays with defaults.
fn parse_playback(query: &str, is_video: bool) -> (PlaybackParams, Vec<OverlayWarning>) {
    let mut playback = PlaybackParams::default();
    let mut warnings = Vec::new();

    for (name, value) in query_pairs(query) {
        let flag = |value: &Option<String>| -> Option<bool> {
            match value.as_deref() {
                None | Some("") | Some("1") | Some("true") | Some("yes") => Some(true),
                Some("0") | Some("false") | Some("no") => Some(false),
                Some(_) => None,
            }
        };
        match name.as_str() {
            // `autostart` is pdfpc's spelling of the same intent.
            "autoplay" | "autostart" => match flag(&value) {
                Some(on) => playback.autoplay = on,
                None => warnings.push(malformed("autoplay", value)),
            },
            "loop" => match flag(&value) {
                Some(on) => playback.repeat = on,
                None => warnings.push(malformed("loop", value)),
            },
            "mute" if is_video => match flag(&value) {
                Some(on) => playback.mute = on,
                None => warnings.push(malformed("mute", value)),
            },
            "start" if is_video => match value.as_deref().map(str::parse::<f32>) {
                Some(Ok(seconds)) if seconds.is_finite() && seconds >= 0.0 => {
                    playback.start = seconds
                }
                _ => warnings.push(malformed("start", value)),
            },
            "poster" => match value.as_deref() {
                Some(id) if !id.is_empty() => playback.poster = Some(AssetRef::new(id)),
                _ => warnings.push(malformed("poster", value)),
            },
            other => warnings.push(OverlayWarning::UnknownParameter(other.to_string())),
        }
    }
    (playback, warnings)
}

fn malformed(name: &str, value: Option<String>) -> OverlayWarning {
    OverlayWarning::MalformedParameter {
        name: name.to_string(),
        value: value.unwrap_or_default(),
    }
}

/// Percent-decoding, tolerant of the `+`-for-space form inside queries.
///
/// Invalid escapes are left as written rather than dropped: a producer that
/// wrote a literal `%` should see it in the diagnostic, not lose it.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Logical-slide grouping (docs-src/internals.typ)
// ---------------------------------------------------------------------------

/// How close two regions must be to count as "the same rectangle" on
/// successive reveal steps. Producers re-emit identical geometry, so this
/// only absorbs floating-point conversion noise.
const REGION_TOLERANCE: f32 = 1e-3;

fn same_region(left: &Region, right: &Region) -> bool {
    (left.x - right.x).abs() <= REGION_TOLERANCE
        && (left.y - right.y).abs() <= REGION_TOLERANCE
        && (left.width - right.width).abs() <= REGION_TOLERANCE
        && (left.height - right.height).abs() <= REGION_TOLERANCE
}

/// Optional producer-supplied grouping of physical pages into logical slides.
///
/// `label_of(page)` returns the PDF page label; consecutive pages sharing a
/// label belong to the same logical slide. Beamer and mosaic both emit these
/// for incremental reveals.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PageLabels {
    pub labels: BTreeMap<usize, String>,
}

impl PageLabels {
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    pub fn label(&self, page: usize) -> Option<&str> {
        self.labels.get(&page).map(String::as_str)
    }

    /// May two consecutive physical pages hold the same logical overlay?
    fn continuous(&self, earlier: usize, later: usize) -> bool {
        match (self.label(earlier), self.label(later)) {
            (Some(a), Some(b)) => a == b,
            // Without labels for both pages, the equality rule decides alone.
            _ => true,
        }
    }
}

/// An overlay's identity, derived from the overlay itself.
///
/// Deliberately not a counter. The renderer answers one page at a time, so
/// the index is rebuilt from a map that *grows*; a positional id therefore
/// changed whenever a page sorting earlier happened to arrive later. The
/// application keys its live sessions by this id, so that silently opened a
/// second session and restarted a playing video mid-reveal.
///
/// FNV-1a rather than `DefaultHasher`: the value is written into session
/// snapshots, and a hash whose output is documented as unstable between
/// releases has no business there.
fn stable_overlay_id(identity: &str, z_index: i32, occurrence: u32) -> OverlayId {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(PRIME);
        }
    };
    eat(identity.as_bytes());
    eat(&z_index.to_le_bytes());
    eat(&occurrence.to_le_bytes());
    // Zero is reserved so an id is never confused with an absent one.
    OverlayId(hash | 1)
}

/// Every overlay in a document, with reveal-step repetitions already
/// collapsed into single logical overlays.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OverlayIndex {
    overlays: Vec<PageOverlay>,
}

impl OverlayIndex {
    /// Collapse per-page declarations into logical overlays.
    ///
    /// Declarations on consecutive physical pages with the same content
    /// identity and (within tolerance) the same rectangle are one overlay
    /// with one session, so advancing through a `\pause` sequence does not
    /// restart a video or lose JavaScript state.
    pub fn build(per_page: &BTreeMap<usize, Vec<OverlayDeclaration>>, labels: &PageLabels) -> Self {
        let mut overlays: Vec<PageOverlay> = Vec::new();
        // Index of the still-extendable overlay for each grouping identity.
        let mut open: BTreeMap<(String, i32), usize> = BTreeMap::new();
        // How many runs of each identity have been started, so the third
        // appearance of one clip is distinguishable from the first.
        let mut runs: BTreeMap<(String, i32), u32> = BTreeMap::new();

        for (&page, declarations) in per_page {
            let mut seen_this_page: Vec<(String, i32)> = Vec::new();
            for declaration in declarations {
                let key = (declaration.content.grouping_identity(), declaration.z_index);
                // Two identical declarations on one page are two overlays,
                // not one repeated: only *consecutive pages* collapse.
                let extendable = if seen_this_page.contains(&key) {
                    None
                } else {
                    open.get(&key).copied().filter(|&index| {
                        let existing: &PageOverlay = &overlays[index];
                        let last = *existing.pages.last().unwrap_or(&existing.page);
                        last + 1 == page
                            && same_region(&existing.region, &declaration.region)
                            && labels.continuous(last, page)
                    })
                };
                seen_this_page.push(key.clone());

                match extendable {
                    Some(index) => overlays[index].pages.push(page),
                    None => {
                        let occurrence = runs.entry(key.clone()).or_insert(0);
                        let id = stable_overlay_id(&key.0, key.1, *occurrence);
                        *occurrence += 1;
                        overlays.push(PageOverlay {
                            id,
                            page,
                            pages: vec![page],
                            region: declaration.region,
                            z_index: declaration.z_index,
                            activation: declaration.activation,
                            poster: declaration.content.poster().cloned(),
                            content: declaration.content.clone(),
                            warnings: declaration.warnings.clone(),
                        });
                        open.insert(key, overlays.len() - 1);
                    }
                }
            }
        }

        Self { overlays }
    }

    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    pub fn len(&self) -> usize {
        self.overlays.len()
    }

    pub fn all(&self) -> &[PageOverlay] {
        &self.overlays
    }

    pub fn get(&self, id: OverlayId) -> Option<&PageOverlay> {
        self.overlays.iter().find(|overlay| overlay.id == id)
    }

    /// Overlays visible on one physical page, painted back to front.
    pub fn on_page(&self, page: usize) -> Vec<&PageOverlay> {
        let mut visible: Vec<&PageOverlay> = self
            .overlays
            .iter()
            .filter(|overlay| overlay.covers_page(page))
            .collect();
        visible.sort_by_key(|overlay| overlay.z_index);
        visible
    }

    /// The topmost interactive overlay under a normalised page point.
    pub fn hit(&self, page: usize, x: f32, y: f32) -> Option<&PageOverlay> {
        self.on_page(page)
            .into_iter()
            .rev()
            .find(|overlay| overlay.is_interactive() && overlay.contains(x, y))
    }
}

// ---------------------------------------------------------------------------
// Web bundle manifest
// ---------------------------------------------------------------------------

/// The highest manifest version this build understands.
pub const WEB_MANIFEST_VERSION: u32 = 1;

/// The manifest filename at the root of every web bundle.
pub const WEB_MANIFEST_NAME: &str = "pulpit-web.json";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("the bundle has no {WEB_MANIFEST_NAME}")]
    Missing,
    #[error("{WEB_MANIFEST_NAME} is not readable JSON: {0}")]
    Malformed(String),
    #[error("manifest version {0} is newer than this build understands")]
    UnknownVersion(u32),
    #[error("the manifest entrypoint `{0}` is not a safe bundle-relative path")]
    UnsafeEntrypoint(String),
}

/// The on-disk form. Unknown fields are ignored for forward compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebManifest {
    pub version: u32,
    #[serde(default = "default_entrypoint")]
    pub entrypoint: String,
    /// Whether a URI may name a different entrypoint than this manifest.
    #[serde(default)]
    pub allow_entrypoint_override: bool,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub viewport: Option<LogicalSize>,
    #[serde(default)]
    pub persistence: PersistencePolicy,
    #[serde(default)]
    pub permissions: WebPermissions,
    #[serde(default)]
    pub requirements: WebRequirements,
}

fn default_entrypoint() -> String {
    "index.html".to_string()
}

impl Default for WebManifest {
    fn default() -> Self {
        Self {
            version: WEB_MANIFEST_VERSION,
            entrypoint: default_entrypoint(),
            allow_entrypoint_override: false,
            poster: None,
            viewport: None,
            persistence: PersistencePolicy::default(),
            permissions: WebPermissions::default(),
            requirements: WebRequirements::default(),
        }
    }
}

impl WebManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.version == 0 || self.version > WEB_MANIFEST_VERSION {
            return Err(ManifestError::UnknownVersion(self.version));
        }
        let entrypoint = RelativeAssetPath(self.entrypoint.clone());
        if !entrypoint.is_safe() {
            return Err(ManifestError::UnsafeEntrypoint(self.entrypoint.clone()));
        }
        Ok(())
    }

    /// Fold the manifest into the URI-derived spec.
    ///
    /// The URI entrypoint wins only when the manifest opts in; everything
    /// else — viewport, permissions, persistence, requirements — is the
    /// manifest's to state.
    pub fn apply(&self, spec: &mut WebSpec, uri_named_entrypoint: bool) {
        if !uri_named_entrypoint || !self.allow_entrypoint_override {
            spec.entrypoint = RelativeAssetPath(self.entrypoint.clone());
        }
        spec.viewport = self.viewport;
        spec.permissions = self.permissions.clone();
        spec.persistence = self.persistence;
        spec.requirements = self.requirements;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> Region {
        Region::new(0.1, 0.2, 0.4, 0.3)
    }

    fn declare(uri: &str) -> OverlayDeclaration {
        parse_overlay_uri(uri, region()).expect("uri should parse")
    }

    #[test]
    fn the_link_rectangle_is_the_overlay_region() {
        let declaration = declare("pulpit://video/clip");
        assert_eq!(declaration.region, region());
    }

    #[test]
    fn every_uri_form_resolves_to_its_content_kind() {
        assert_eq!(
            declare("pulpit://video/clip").content.kind(),
            ContentKind::Video
        );
        assert_eq!(
            declare("pulpit://image/spinner").content.kind(),
            ContentKind::AnimatedImage
        );
        assert_eq!(
            declare("pulpit://video-external/media/clip.mp4")
                .content
                .kind(),
            ContentKind::Video
        );
        assert_eq!(
            declare("pulpit://image-external/media/spin.gif")
                .content
                .kind(),
            ContentKind::AnimatedImage
        );
        assert_eq!(
            declare("pulpit://web/balls/index.html").content.kind(),
            ContentKind::Web
        );
    }

    #[test]
    fn the_scheme_is_case_insensitive_but_the_asset_id_is_not() {
        let declaration = declare("Pulpit://VIDEO/Clip");
        match declaration.content {
            OverlayContent::Video(spec) => {
                assert_eq!(spec.source.identity(), "Clip");
            }
            other => panic!("expected video, got {other:?}"),
        }
    }

    #[test]
    fn playback_parameters_follow_the_version_one_grammar() {
        let declaration = declare("pulpit://video/clip?autoplay&loop=1&mute=true&start=12.5");
        match declaration.content {
            OverlayContent::Video(spec) => {
                assert!(spec.playback.autoplay);
                assert!(spec.playback.repeat);
                assert!(spec.playback.mute);
                assert_eq!(spec.playback.start, 12.5);
            }
            other => panic!("expected video, got {other:?}"),
        }
        assert!(declaration.warnings.is_empty());
    }

    #[test]
    fn playback_defaults_are_off_so_nothing_starts_unasked() {
        let declaration = declare("pulpit://video/clip");
        match declaration.content {
            OverlayContent::Video(spec) => {
                assert!(!spec.playback.autoplay);
                assert!(!spec.playback.repeat);
                assert!(!spec.playback.mute);
                assert_eq!(spec.playback.start, 0.0);
            }
            other => panic!("expected video, got {other:?}"),
        }
    }

    #[test]
    fn unknown_and_malformed_parameters_warn_but_still_play() {
        let declaration = declare("pulpit://video/clip?autoplay=perhaps&sparkle=9&start=-4");
        assert!(declaration
            .warnings
            .contains(&OverlayWarning::UnknownParameter("sparkle".into())));
        assert!(declaration.warnings.iter().any(|warning| matches!(
            warning,
            OverlayWarning::MalformedParameter { name, .. } if name == "autoplay"
        )));
        assert!(declaration.warnings.iter().any(|warning| matches!(
            warning,
            OverlayWarning::MalformedParameter { name, .. } if name == "start"
        )));
        match declaration.content {
            OverlayContent::Video(spec) => {
                assert!(!spec.playback.autoplay, "the default survives a bad value");
                assert_eq!(spec.playback.start, 0.0);
            }
            other => panic!("expected video, got {other:?}"),
        }
    }

    #[test]
    fn video_only_parameters_are_not_offered_to_images() {
        let declaration = declare("pulpit://image/spin?mute&start=3");
        assert_eq!(declaration.warnings.len(), 2);
        assert!(declaration
            .warnings
            .iter()
            .all(|w| matches!(w, OverlayWarning::UnknownParameter(_))));
    }

    #[test]
    fn a_poster_names_an_attachment() {
        let declaration = declare("pulpit://video/clip?poster=cover");
        assert_eq!(declaration.content.poster(), Some(&AssetRef::new("cover")));
    }

    #[test]
    fn external_paths_that_escape_the_document_directory_are_refused() {
        for uri in [
            "pulpit://video-external/../../etc/passwd",
            "pulpit://video-external//etc/passwd",
            "pulpit://video-external/%2e%2e/secret.mp4",
            "pulpit://image-external/C:/windows/system32/x.gif",
        ] {
            assert!(
                matches!(
                    parse_overlay_uri(uri, region()),
                    Err(UriError::UnsafePath(_))
                ),
                "{uri} should be refused"
            );
        }
    }

    #[test]
    fn percent_encoding_is_decoded_before_the_containment_check() {
        let declaration = declare("pulpit://video-external/media/my%20clip.mp4");
        match declaration.content {
            OverlayContent::Video(spec) => {
                assert_eq!(spec.source.identity(), "media/my clip.mp4")
            }
            other => panic!("expected video, got {other:?}"),
        }
    }

    #[test]
    fn a_web_uri_without_an_entrypoint_defaults_to_index_html() {
        match declare("pulpit://web/balls").content {
            OverlayContent::Web(spec) => {
                assert_eq!(spec.bundle, OverlaySource::Embedded(AssetRef::new("balls")));
                assert_eq!(spec.entrypoint.0, "index.html");
            }
            other => panic!("expected web, got {other:?}"),
        }
    }

    #[test]
    fn a_traversing_entrypoint_is_refused() {
        assert!(matches!(
            parse_overlay_uri("pulpit://web/balls/../../etc/passwd", region()),
            Err(UriError::UnsafeEntrypoint(_))
        ));
    }

    #[test]
    fn unknown_forms_and_empty_assets_are_errors_not_guesses() {
        assert!(matches!(
            parse_overlay_uri("pulpit://hologram/x", region()),
            Err(UriError::UnknownForm(_))
        ));
        assert!(matches!(
            parse_overlay_uri("pulpit://video/", region()),
            Err(UriError::MissingAsset)
        ));
    }

    #[test]
    fn ordinary_links_are_not_media() {
        for uri in ["https://example.com", "mailto:a@b.c", "file:///tmp/x"] {
            assert!(!is_media_uri(uri));
            assert!(matches!(
                parse_overlay_uri(uri, region()),
                Err(UriError::NotMedia)
            ));
        }
    }

    #[test]
    fn the_pdfpc_run_convention_is_understood() {
        let declaration = declare("run:media/clip.mp4?autostart&loop=1");
        match declaration.content {
            OverlayContent::Video(spec) => {
                assert_eq!(spec.source.identity(), "media/clip.mp4");
                assert!(spec.playback.autoplay, "autostart is pdfpc's autoplay");
                assert!(spec.playback.repeat);
            }
            other => panic!("expected video, got {other:?}"),
        }
    }

    #[test]
    fn the_run_convention_covers_gifs_and_html_too() {
        // A deck should not need a pulpit-specific URI for anything: the
        // convention other PDF presenters already use extends to every
        // content kind, including the one PDF never standardised.
        match declare("run:media/spin.gif?autostart&loop").content {
            OverlayContent::AnimatedImage(spec) => {
                assert_eq!(spec.source.identity(), "media/spin.gif");
                assert!(spec.playback.autoplay && spec.playback.repeat);
            }
            other => panic!("expected an animated image, got {other:?}"),
        }
        match declare("run:media/balls.html").content {
            OverlayContent::Web(spec) => {
                assert_eq!(spec.bundle.identity(), "media/balls.html");
                assert_eq!(
                    spec.entrypoint.0, "balls.html",
                    "the file itself is the entrypoint; its directory is the root"
                );
            }
            other => panic!("expected web, got {other:?}"),
        }
    }

    #[test]
    fn an_html_file_at_the_document_root_still_resolves() {
        match declare("run:balls.html").content {
            OverlayContent::Web(spec) => {
                assert_eq!(spec.entrypoint.0, "balls.html");
                assert_eq!(spec.bundle.identity(), "balls.html");
            }
            other => panic!("expected web, got {other:?}"),
        }
    }

    #[test]
    fn a_run_link_to_a_program_is_not_an_overlay_and_is_never_executed() {
        for uri in ["run:rm", "run:setup.exe", "run:../../bin/sh"] {
            assert!(
                matches!(
                    parse_overlay_uri(uri, region()),
                    Err(UriError::NotMedia) | Err(UriError::UnsafePath(_))
                ),
                "{uri} must not become an overlay"
            );
        }
    }

    // -- grouping -----------------------------------------------------------

    fn pages(entries: &[(usize, &[&str])]) -> BTreeMap<usize, Vec<OverlayDeclaration>> {
        entries
            .iter()
            .map(|(page, uris)| {
                (
                    *page,
                    uris.iter().map(|uri| declare(uri)).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    #[test]
    fn a_reveal_sequence_keeps_one_overlay_and_one_session() {
        // Three physical pages of one `\pause` slide, each repeating the
        // same declaration: the video must not restart as bullets appear.
        let per_page = pages(&[
            (4, &["pulpit://video/clip?autoplay"]),
            (5, &["pulpit://video/clip?autoplay"]),
            (6, &["pulpit://video/clip?autoplay"]),
        ]);
        let index = OverlayIndex::build(&per_page, &PageLabels::default());
        assert_eq!(index.len(), 1);
        let overlay = &index.all()[0];
        assert_eq!(overlay.pages, vec![4, 5, 6]);
        assert_eq!(overlay.page, 4);
        for page in 4..=6 {
            assert_eq!(index.on_page(page).len(), 1);
            assert_eq!(index.on_page(page)[0].id, overlay.id);
        }
    }

    #[test]
    fn an_overlay_keeps_its_identity_as_other_pages_arrive() {
        // The renderer answers one page at a time, so the index is rebuilt
        // from a *growing* map. An overlay's identity must depend on the
        // overlay, not on how many other pages happen to be known — the
        // application keys its live sessions by that id, so a shift silently
        // restarts a playing video mid-reveal.
        let late = pages(&[(8, &["pulpit://video/clip?autoplay"])]);
        let alone = OverlayIndex::build(&late, &PageLabels::default()).all()[0].id;

        let with_earlier = pages(&[
            (1, &["pulpit://web/deck"]),
            (2, &["pulpit://image/spin"]),
            (8, &["pulpit://video/clip?autoplay"]),
        ]);
        let index = OverlayIndex::build(&with_earlier, &PageLabels::default());
        let now = index.on_page(8)[0].id;
        assert_eq!(
            alone, now,
            "an earlier page arriving renamed the overlay on page 8"
        );
    }

    #[test]
    fn a_reveal_run_keeps_one_identity_as_each_step_arrives() {
        // Advancing through the reveal: the same overlay must answer to the
        // same id after every step's declarations land.
        let mut per_page = BTreeMap::new();
        let mut previous = None;
        for page in 4..=6 {
            per_page.insert(page, vec![declare("pulpit://video/clip?autoplay")]);
            let index = OverlayIndex::build(&per_page, &PageLabels::default());
            let id = index.on_page(page)[0].id;
            if let Some(previous) = previous {
                assert_eq!(previous, id, "the overlay was renamed at page {page}");
            }
            previous = Some(id);
        }
    }

    #[test]
    fn a_gap_in_the_page_run_starts_a_new_overlay() {
        let per_page = pages(&[
            (1, &["pulpit://video/clip"]),
            (2, &["pulpit://video/clip"]),
            (7, &["pulpit://video/clip"]),
        ]);
        let index = OverlayIndex::build(&per_page, &PageLabels::default());
        assert_eq!(index.len(), 2);
        assert_eq!(index.all()[0].pages, vec![1, 2]);
        assert_eq!(index.all()[1].pages, vec![7]);
    }

    #[test]
    fn a_moved_rectangle_is_a_different_overlay() {
        let mut per_page = BTreeMap::new();
        per_page.insert(1, vec![declare("pulpit://video/clip")]);
        per_page.insert(
            2,
            vec![
                parse_overlay_uri("pulpit://video/clip", Region::new(0.5, 0.5, 0.2, 0.2)).unwrap(),
            ],
        );
        let index = OverlayIndex::build(&per_page, &PageLabels::default());
        assert_eq!(
            index.len(),
            2,
            "content that moves is not one continuous overlay"
        );
    }

    #[test]
    fn floating_point_noise_does_not_split_an_overlay() {
        let mut per_page = BTreeMap::new();
        per_page.insert(1, vec![declare("pulpit://video/clip")]);
        let nudged = Region::new(
            region().x + 1e-5,
            region().y - 1e-5,
            region().width,
            region().height,
        );
        per_page.insert(
            2,
            vec![parse_overlay_uri("pulpit://video/clip", nudged).unwrap()],
        );
        let index = OverlayIndex::build(&per_page, &PageLabels::default());
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn page_labels_stop_a_run_at_the_logical_slide_boundary() {
        // Same declaration on pages 1-3, but the producer says page 3 is a
        // different slide: the run must break there.
        let per_page = pages(&[
            (1, &["pulpit://video/clip"]),
            (2, &["pulpit://video/clip"]),
            (3, &["pulpit://video/clip"]),
        ]);
        let labels = PageLabels {
            labels: [
                (1, "3".to_string()),
                (2, "3".to_string()),
                (3, "4".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let index = OverlayIndex::build(&per_page, &labels);
        assert_eq!(index.len(), 2);
        assert_eq!(index.all()[0].pages, vec![1, 2]);
        assert_eq!(index.all()[1].pages, vec![3]);
    }

    #[test]
    fn two_identical_declarations_on_one_page_stay_two_overlays() {
        let per_page = pages(&[(1, &["pulpit://video/clip", "pulpit://video/clip"])]);
        let index = OverlayIndex::build(&per_page, &PageLabels::default());
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn different_content_on_the_same_page_run_stays_separate() {
        let per_page = pages(&[
            (1, &["pulpit://video/clip", "pulpit://image/spin"]),
            (2, &["pulpit://video/clip", "pulpit://image/spin"]),
        ]);
        let index = OverlayIndex::build(&per_page, &PageLabels::default());
        assert_eq!(index.len(), 2);
        assert!(index.all().iter().all(|o| o.pages == vec![1, 2]));
    }

    #[test]
    fn overlays_paint_and_hit_test_by_z_index() {
        let mut under = declare("pulpit://web/under");
        under.z_index = 1;
        let mut over = declare("pulpit://web/over");
        over.z_index = 5;
        let mut per_page = BTreeMap::new();
        per_page.insert(0, vec![over, under]);
        let index = OverlayIndex::build(&per_page, &PageLabels::default());

        let painted = index.on_page(0);
        assert_eq!(painted[0].z_index, 1, "lowest is painted first");
        assert_eq!(painted[1].z_index, 5);

        let hit = index.hit(0, 0.2, 0.3).expect("point is inside both");
        assert_eq!(hit.z_index, 5, "the topmost overlay takes the press");
    }

    #[test]
    fn every_media_kind_takes_the_press_it_now_has_something_to_do_with() {
        // A video has a transport and an animated image toggles on a click,
        // so both must receive the press. Advancing the slide by clicking the
        // overlay itself is what this trades away.
        for uri in [
            "pulpit://video/clip",
            "pulpit://image/spin",
            "pulpit://web/balls",
        ] {
            let per_page = pages(&[(0, &[uri])]);
            let index = OverlayIndex::build(&per_page, &PageLabels::default());
            assert!(index.hit(0, 0.2, 0.3).is_some(), "{uri} ignored the press");
        }
    }

    #[test]
    fn hit_testing_respects_the_rectangle() {
        let per_page = pages(&[(0, &["pulpit://web/balls"])]);
        let index = OverlayIndex::build(&per_page, &PageLabels::default());
        assert!(index.hit(0, 0.2, 0.3).is_some());
        assert!(index.hit(0, 0.9, 0.9).is_none());
    }

    // -- manifest -----------------------------------------------------------

    #[test]
    fn a_default_manifest_is_valid_and_denies_everything() {
        let manifest = WebManifest::default();
        assert!(manifest.validate().is_ok());
        assert!(!manifest.permissions.requests_anything());
        assert_eq!(manifest.persistence, PersistencePolicy::Reset);
    }

    #[test]
    fn a_newer_manifest_version_is_rejected_not_guessed_at() {
        let manifest = WebManifest {
            version: WEB_MANIFEST_VERSION + 1,
            ..Default::default()
        };
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::UnknownVersion(_))
        ));
    }

    #[test]
    fn a_traversing_manifest_entrypoint_is_rejected() {
        let manifest = WebManifest {
            entrypoint: "../../../etc/passwd".into(),
            ..Default::default()
        };
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::UnsafeEntrypoint(_))
        ));
    }

    #[test]
    fn the_uri_entrypoint_wins_only_when_the_manifest_allows_it() {
        let mut spec = match declare("pulpit://web/balls/other.html").content {
            OverlayContent::Web(spec) => spec,
            other => panic!("expected web, got {other:?}"),
        };
        let manifest = WebManifest {
            entrypoint: "index.html".into(),
            allow_entrypoint_override: false,
            ..Default::default()
        };
        manifest.apply(&mut spec, true);
        assert_eq!(
            spec.entrypoint.0, "index.html",
            "the manifest holds the line"
        );

        let mut spec = match declare("pulpit://web/balls/other.html").content {
            OverlayContent::Web(spec) => spec,
            other => panic!("expected web, got {other:?}"),
        };
        let manifest = WebManifest {
            allow_entrypoint_override: true,
            ..Default::default()
        };
        manifest.apply(&mut spec, true);
        assert_eq!(spec.entrypoint.0, "other.html");
    }

    #[test]
    fn a_manifest_carries_viewport_and_requirements_into_the_spec() {
        let mut spec = match declare("pulpit://web/balls").content {
            OverlayContent::Web(spec) => spec,
            other => panic!("expected web, got {other:?}"),
        };
        let manifest = WebManifest {
            viewport: Some(LogicalSize {
                width: 1280,
                height: 720,
            }),
            persistence: PersistencePolicy::PageSession,
            requirements: WebRequirements {
                pointer: true,
                keyboard: true,
                webgl: false,
                continuous_animation: true,
            },
            ..Default::default()
        };
        manifest.apply(&mut spec, false);
        assert_eq!(spec.viewport.map(|v| v.width), Some(1280));
        assert_eq!(spec.persistence, PersistencePolicy::PageSession);
        assert!(spec.requirements.keyboard);
    }
}
