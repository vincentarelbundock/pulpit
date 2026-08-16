//! The Iced application: one update loop, one presenter and an optional
//! audience window, one presentation state.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::{window, Size, Subscription, Task};

use crate::doc::manager::{Action as DocAction, RealFileProbe};
use crate::doc::{DocumentManager, DocumentWatcher, ReloadPolicy};
use crate::layout::{AspectRatio, Layout, LayoutId, LayoutStore};
use crate::reader::AppliedKind;
use crate::settings::diagnostics::describe_warning;
use crate::settings::{Action, DiagnosticsBundle, KeyBinding, Settings, SettingsStore};
use pulpit_core::annotation::AnnotationTool;
use pulpit_core::{Blank, Command as Nav, DocumentInfo, NotesMapping, PresentationState};
use pulpit_display::{
    apply_outcome, Action as DisplayAction, Reconciliation, Role, RoleTarget, WindowMode,
    WindowState,
};
use pulpit_render::cache::{FrameCache, FrameKey, FrameKind};
use pulpit_render::protocol::{Priority, Quality, RenderJob, RequestId};
use pulpit_render::supervisor::{RenderEvent, RendererSupervisor, SupervisorConfig, WorkerCommand};

use crate::platform::{Inhibitor, Platform};

use crate::display::{self, DisplayCoordinator};
use crate::theme::ThemeState;
use crate::toast::{Intent, Toasts};

/// How often the update loop wakes up: fast enough for a smooth clock and
/// prompt renderer events, cheap enough to leave the GPU alone.
const TICK: Duration = Duration::from_millis(50);
/// The tick while nothing is live: fast enough for the clock, file watching
/// and resume detection, slow enough that an idle talk barely wakes the CPU.
const SETTLED_TICK: Duration = Duration::from_millis(250);

/// How long a newly cached frame keeps the fast tick, so the windows can get
/// it onto the GPU before the next page turn asks them to draw it.
///
/// `residency` uploads at most one picture per pass on purpose — several tens
/// of mebibytes at once would trade a flash for a stall — so the pre-uploads
/// that make the *next* turn instant are paced by how often the application
/// draws. Settling the moment the last render landed left exactly those
/// uploads to trickle at the settled tick, and a turn arriving before they
/// finished paid for one synchronously, on the event loop, at the worst
/// possible moment.
///
/// Anchored to a frame arriving rather than to residency itself: which
/// pictures a window has uploaded is that window's widget state, published
/// nowhere, and a stale copy of it in the application would be a fast tick
/// that never ends the day a window stops drawing. This costs a handful of
/// extra wakeups after the last render of a turn and cannot outlive them.
const UPLOAD_SETTLE: Duration = Duration::from_millis(300);

/// How long the overview must go without a scroll event before it counts as
/// stopped. Long enough not to fire between two frames of a trackpad glide,
/// short enough that the selection catches up while the hand is still on the
/// pad rather than a beat later.
const OVERVIEW_SETTLE: Duration = Duration::from_millis(140);

/// How long a keyboard-requested scroll outranks incoming scroll events.
/// Momentum from a flick can run for a second or more; this only has to
/// cover the round trip until the requested offset echoes back, and lapsing
/// early merely restores the old behaviour.
const OVERVIEW_SCROLL_CLAIM: Duration = Duration::from_millis(400);

/// Pixels one notch of a wheel scrolls, for runtimes that deal in pixels.
///
/// Toolkits report a notched wheel in lines and browsers in pixels; sixteen
/// is the conventional line height that conversion assumes.
const LINE_SCROLL_PIXELS: f32 = 16.0;
/// Topology is polled at this interval as the baseline; native listeners
/// shorten the latency where they exist.
const POLL_TOPOLOGY: Duration = Duration::from_millis(1000);
/// Window mapping, fullscreen, and compositor placement are asynchronous.
/// Refocusing after both the first map and its placement retry keeps the
/// presenter in control instead of racing the window manager.
const PRESENTER_REFOCUS_DELAYS: [Duration; 3] = [
    Duration::from_millis(150),
    Duration::from_millis(500),
    Duration::from_millis(1200),
];

#[derive(Debug, Clone)]
pub enum Message {
    Tick(Instant),
    /// A worker has said something. The frames themselves are not carried
    /// here — this is the doorbell from `pulpit_render::supervisor`, and the
    /// handler drains the supervisor exactly as the tick does.
    ///
    /// Its whole purpose is latency: a finished frame used to become visible
    /// only when the next tick got round to looking, so every page turn paid
    /// up to a tick per rendering step for nothing but the poll.
    RenderReady,
    Key {
        key: Option<String>,
        text: Option<String>,
        scancode: Option<u32>,
        shift: bool,
        control: bool,
        alt: bool,
    },
    /// Text read asynchronously after Paste while a label is active.
    PasteAnnotationText {
        target: u64,
        value: Option<String>,
    },
    Do(Action),
    Nav(Nav),
    OpenDialog,
    Opened(Option<PathBuf>),
    /// Where the reader's annotated document should be written. `None` when
    /// the chooser was dismissed, which is not a failure.
    SaveDocumentTo(Option<PathBuf>),
    /// The media runtime probes, run on a helper thread at startup.
    MediaProbed(Vec<pulpit_media::RuntimeProbe>),
    WindowOpened {
        role: Role,
        id: window::Id,
    },
    WindowClosed(window::Id),
    NativeId {
        role: Role,
        native: Option<pulpit_display::NativeWindow>,
    },
    Resized {
        id: window::Id,
        size: Size,
    },
    /// The presenter window's physical-pixel ratio, which decides how wide a
    /// panel's frame must be. Re-read on every resize: moving a window to
    /// another display changes it.
    PresenterScale(f32),
    SetMapping(NotesMapping),
    /// Bind the most recent unrecognised key press to an action.
    BindUnboundKey(Action),
    /// Stop offering to bind it.
    ForgetUnboundKey,
    /// The left mouse button came up, anywhere. Ends a slider drag.
    PointerReleased,
    /// The overview was scrolled; the grid builds only what is on screen.
    OverviewScrolled(f32),
    /// Show or hide the deck as thumbnails.
    ToggleOverview,
    /// Jump to a slide from the overview, which then closes: picking a slide
    /// by eye is one gesture, not two.
    GoToFromOverview(usize),
    // ---- layouts and the designer
    /// A message from the layout editor.
    Designer(crate::designer::Msg),
    ShowPresenter,
    ShowLibrary,
    NewLayout,
    /// Make this layout the active presenter layout.
    UseLayout(LayoutId),
    EditLayout(LayoutId),
    /// Open a layout in the editor with preview mode already on.
    PreviewLayout(LayoutId),
    DuplicateLayout(LayoutId),
    DeleteLayout(LayoutId),
    ConfirmLayoutDialog,
    CancelLayoutDialog,
    // ---- shell
    /// Run a command selected from the hamburger menu after dismissing it.
    MenuAction(Box<Message>),
    ToggleMenu,
    CloseMenu,
    /// Start immediately with the saved display and fullscreen choices.
    StartAudience,
    /// Select a connected display and start the audience there immediately.
    StartAudienceOnDisplay {
        monitor: usize,
    },
    /// Return to automatic display selection and start immediately.
    StartAudienceAutomatic,
    /// Give the presenter time to switch to the projector's workspace before
    /// the hidden audience toplevel is mapped.
    /// Start without fullscreen so desktop controls remain easy to reach.
    StartAudienceWindowed,
    StopAudience,
    ToggleAudienceStartMenu,
    ShowSettings,
    SetAppearance(crate::platform::Appearance),
    SetBlankColor(crate::settings::BlankColor),
    SetMotion(crate::platform::MotionSetting),
    /// A key came up. Only interactive overlays care; pulpit's own
    /// bindings all act on the press.
    KeyReleased(String),
    Wheel {
        x: f32,
        y: f32,
    },
    EditColorScheme(crate::settings::ColorScheme),
    SetColor(crate::theme::ColorRole, String),
    /// Open or close the colour wheel for one role.
    OpenColorPicker(Option<crate::theme::ColorRole>),
    AskResetColors,
    CancelResetColors,
    ResetColors,
    /// Put the interrupted session back. Sent only by the restore dialog's
    /// confirming button: nothing else in the application may reach it.
    RestoreSession,
    /// Start fresh and forget the interrupted session.
    DiscardSession,
    DismissToast(u64),
    DismissAllToasts,
    /// Put the diagnostics report on the clipboard.
    CopyDiagnostics,
    /// Ask the desktop to reveal the open document.
    RevealDocument,
    /// The pointer moved over the live current slide, in normalised slide
    /// coordinates (values outside `0..=1` are the letterbox).
    SlideCursor {
        x: f32,
        y: f32,
    },
    /// The live current slide was pressed; follow a link if one is under
    /// the last cursor position.
    SlidePressed,
    /// Something the presenter asked of the annotation palette.
    Annotate(crate::widgets::event::AnnotationCommand),
    Alarm(crate::widgets::event::AlarmCommand),
    /// Something the presenter asked of the timer: which way it runs, and
    /// how long the talk is.
    Timer(crate::widgets::event::TimerCommand),
    Transport(crate::widgets::event::TransportRequest),
    /// Something the reader asked of the open document.
    Read(crate::widgets::event::ReadCommand),
    /// Something asked of the search pane, in whatever view it is placed.
    Find(crate::widgets::event::FindCommand),
    /// Put back the edits a previous run did not save, or do not.
    RestoreReaderEdits,
    DiscardReaderEdits,
    /// A widget that produces nothing (preview mode).
    Ignore,
}

/// A question the library page is waiting on.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutDialog {
    ConfirmDelete { id: LayoutId, name: String },
}

/// Where an arrow key lands in the overview grid.
///
/// `None` means the key is not one the grid owns and should go on to the
/// keymap; `Some(None)` means it is, but there is nowhere to go — the edge of
/// the grid absorbs the press rather than letting it move the audience.
fn grid_target(
    key: &str,
    current: usize,
    count: usize,
    columns: usize,
    page_rows: usize,
) -> Option<Option<usize>> {
    let columns = columns.max(1);
    let last = count.saturating_sub(1);
    // A page is a screenful of rows, so the selection moves by exactly what
    // the eye just read; a viewport too short to hold a whole row still
    // moves one.
    let page = columns * page_rows.max(1);
    // The grid answers to the vim keys as well as the arrows, in the vim
    // sense rather than the navigation one: here `j` and `k` move between
    // rows and `h` and `l` along one, because this is a grid being looked
    // over and not a deck being advanced through.
    Some(match key {
        "Left" | "h" => current.checked_sub(1),
        "Right" | "l" => (current < last).then(|| current + 1),
        "Up" | "k" => current.checked_sub(columns),
        // The last row is usually short. Dropping to its final page is what
        // the eye expects of a grid; refusing to move is not.
        "Down" | "j" if current + columns <= last => Some(current + columns),
        "Down" | "j" => (current / columns < last / columns).then_some(last),
        // A page step that would fall off the end lands on the first or the
        // last page rather than nowhere — the same reasoning as a short last
        // row, over a whole screenful.
        "PageUp" => (current > 0).then(|| current.saturating_sub(page)),
        "PageDown" => (current < last).then(|| (current + page).min(last)),
        _ => return None,
    })
}

/// Where the selection belongs once the grid has stopped scrolling.
///
/// `None` means it is already on screen and should stay exactly where it is:
/// scrolling a little should not shuffle the selection about. Otherwise it
/// moves to the nearest edge of what is on screen, keeping its column, so the
/// selection arrives from the direction the scroll came from.
///
/// A row counts as on screen when at least half of it is, which is what the
/// eye means by seeing a thumbnail rather than a sliver of one.
fn settled_selection(
    selected: usize,
    count: usize,
    scroll: f32,
    grid: OverviewGrid,
) -> Option<usize> {
    if count == 0 || grid.row_height <= 0.0 || grid.viewport_height <= 0.0 {
        return None;
    }
    let columns = grid.columns.max(1);
    let last_row = (count - 1) / columns;
    let half = grid.row_height / 2.0;
    let first = ((scroll + half) / grid.row_height).floor().max(0.0) as usize;
    let last =
        (((scroll + grid.viewport_height - half) / grid.row_height).floor()).max(0.0) as usize;
    let (first, last) = (first.min(last_row), last.min(last_row));
    let row = selected / columns;
    if (first..=last).contains(&row) {
        return None;
    }
    let target_row = row.clamp(first, last);
    let slide = (target_row * columns + selected % columns).min(count - 1);
    (slide != selected).then_some(slide)
}

/// The shape of the overview grid as it was last drawn.
///
/// A default of one column is the honest answer before the grid has ever
/// been laid out: up and down then behave exactly like back and forward,
/// which is the arrangement a single column actually has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverviewGrid {
    pub columns: usize,
    /// One row plus the gap beneath it, in pixels.
    pub row_height: f32,
    /// How much of the grid is on screen, in pixels.
    pub viewport_height: f32,
}

impl Default for OverviewGrid {
    fn default() -> Self {
        Self {
            columns: 1,
            row_height: 1.0,
            viewport_height: 0.0,
        }
    }
}

/// How wide a page is rendered for the overview grid, the slider's preview
/// card, and the panels' stand-in while a real frame renders. One width, one
/// pass: a page is rendered once and its picture never changes for the life
/// of the document, so nothing downstream ever swaps a thumbnail texture.
/// Sharp enough for the preview card; a deck warms in a few seconds.
pub const THUMBNAIL_WIDTH: u32 = 480;

/// The narrowest a thumbnail is ever rendered.
///
/// Below this a page is a grey smudge rather than a picture of anything, so a
/// deck long enough to need it is one whose furthest pages the budget cannot
/// hold at any useful size. It gets the floor, and [`ThumbnailCache::trim`]
/// keeps the pages nearest the presenter.
pub const THUMBNAIL_MIN_WIDTH: u32 = 120;

/// What the whole deck's thumbnails may occupy. At 480×270 a page is about
/// 520 kB, so this holds roughly two hundred and fifty of them — more than
/// nearly any deck anyone brings to a talk; a longer one is warmed at
/// whatever narrower width does fit, down to [`THUMBNAIL_MIN_WIDTH`].
/// Separate from the frame cache so the two can never evict each other.
const THUMBNAIL_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

/// How many thumbnails may be outstanding (queued or rendering) at once.
///
/// This is the warming throughput throttle. Renderer events are drained once
/// per tick, so a warming pass completes at most this many pages per tick
/// however fast the workers are: 32 per 50 ms tick is ~640 pages a second —
/// a screenful of the overview refills in a single tick, and a 700-page deck
/// warms in about a second — while staying well short of saturating the
/// machine during a talk. Small enough, still, that a document swap does not
/// leave a long tail of stale requests to cancel.
const THUMBNAILS_OUTSTANDING: usize = 32;

/// Everything the thumbnail plan is a function of: the render generation, the
/// slide count, the presenter's page, the page warming is working outwards
/// from, how many pictures are held, and how many are still wanted. If none of
/// it moved, the plan cannot have moved either.
type ThumbnailPlanInputs = (
    pulpit_core::RenderGeneration,
    usize,
    usize,
    usize,
    usize,
    usize,
);

/// A mark the reader placed and is typing into (§8.5).
///
/// Held in the application rather than in the gesture state because it is not
/// a gesture: the click chose a spot, and what happens next is a text editor
/// with a caret, a clipboard and an input method behind it. It is defined
/// beside the reader's other facets because the page surface draws it.
pub use crate::widgets::context::ComposingMark;

/// One mutation sent to the document worker, waiting to be confirmed.
struct PendingEdit {
    kind: AppliedKind,
    /// Whether the answer to this names a mark the presenter just drew, so
    /// the overlay stroke can be told which annotation it is showing.
    names_a_presenter_mark: bool,
    /// The transaction, for the journal. `None` for an undo or a redo, whose
    /// journal entry is written from the answer instead.
    transaction: Option<pulpit_render::document::DocumentTransaction>,
    /// Whether the previews this edit left on screen cover it completely, so
    /// the rendered picture may wait. Decided when the edit is sent for a
    /// commit, and from the answer for an undo or a redo.
    urgency: crate::reader::RasterUrgency,
}

/// A rectangle of a page, drawn by the document worker from the document it
/// actually holds, standing in for the part of the frame an edit invalidated
/// until a full frame containing that edit arrives (§9.4).
///
/// Bounded to one per page: the interesting case is an edit or two while the
/// snapshot is on its way, and a page accumulating patches is a page that
/// should simply be re-rendered.
struct ReaderPatch {
    region: pulpit_core::notes::Region,
    width: u32,
    height: u32,
    /// The full-page frame size this was drawn to fit. A frame of any other
    /// size — the reader zoomed, the window moved — cannot take it, and waits
    /// for its own render instead of being handed a mismatched rectangle.
    frame_width: u32,
    frame_height: u32,
    pixels: Vec<u8>,
    revision: pulpit_render::document::DocumentRevision,
}

/// Where the reader's pages are rendered from.
///
/// With no edits committed, they come straight from the presentation's own
/// document in the render pool, drawn `with_annotations` so the file's marks
/// are in the pixels. After an edit only the document worker's in-memory PDF
/// contains the commit, so the worker writes a *snapshot* to a scratch path
/// and the pool renders from that, under a generation of its own. Generation
/// order is revision order, which is what makes the cache's "newest complete
/// frame wins, never a downgrade" lookup the same guarantee A7 asks for.
#[derive(Default)]
struct ReaderRenderState {
    /// The snapshot currently rendered from, once any edit has landed.
    snapshot: Option<ReaderSnapshot>,
    /// The serial and destination of the snapshot being written, if one is.
    /// One at a time: the worker is serial, and a second request would only
    /// queue behind the round trips the reader is already waiting on.
    snapshot_in_flight: Option<(u64, PathBuf)>,
    /// When the last confirmed edit landed, for the debounce that lets a
    /// burst of strokes become one snapshot rather than one each.
    edited_at: Option<Instant>,
    /// How soon the pending edits need the picture to catch up: the strongest
    /// urgency among them, since one mark the previews cannot draw makes the
    /// whole wait pointless.
    urgency: crate::reader::RasterUrgency,
    /// Take the next snapshot regardless of the clock: the page is about to
    /// stop being shown, or is about to be written out.
    forced: bool,
    /// Counts snapshots, and names their files, documents and generations.
    serial: u64,
}

/// One snapshot the render pool is drawing reader pages from.
struct ReaderSnapshot {
    /// Its id in the render pool's document table.
    document: u64,
    generation: pulpit_core::RenderGeneration,
    /// The document revision the snapshot was taken at, so a frame rendered
    /// from it says which retained mark previews it makes redundant (§9.2).
    revision: pulpit_render::document::DocumentRevision,
    /// The scratch file, deleted when a newer snapshot replaces it.
    path: PathBuf,
}

/// Reader snapshots live in a namespace of their own, far above anything the
/// document manager or a presentation reload will ever allocate, so the two
/// id spaces and the two generation sequences cannot collide — and a reader
/// generation is never fed to `cancel_older_than`, whose floor would take
/// presentation jobs with it.
const READER_RENDER_BASE: u64 = 1 << 32;

/// How long after an edit the picture cannot show, the snapshot is taken.
/// Long enough to coalesce a flurry, short enough that a mark no preview can
/// draw sharpens on the page while the pen is still in the air.
const SNAPSHOT_DEBOUNCE: Duration = Duration::from_millis(250);

/// How long after an edit the previews *do* cover, the snapshot is taken.
///
/// Much longer, because nothing is waiting on it: the page already shows the
/// mark. What it buys is the taking down of previews and a frame that needs no
/// compositing, which is worth doing when the hand stops and worth nothing
/// while it is moving. Making a mark and taking it back inside this window
/// costs no render at all.
const SNAPSHOT_QUIET: Duration = Duration::from_secs(2);

/// How many marks may be drawn as previews before the picture is made to catch
/// up regardless of the clock.
///
/// Deferring costs one composite per retained mark per draw. That is nothing
/// for the handful a burst of drawing produces and is not nothing for a
/// hand that never pauses, so the quiet spell is not the only way out.
const MAX_RETAINED_MARKS: usize = 24;

/// Where reader snapshots are written: a per-process scratch directory, so a
/// crash leaves nothing worse than temporary files and two running pulpits
/// cannot overwrite each other's.
fn reader_snapshot_directory() -> PathBuf {
    std::env::temp_dir().join(format!("pulpit-reader-{}", std::process::id()))
}

pub struct App {
    pub state: PresentationState,
    /// Which page the presenter window is showing.
    pub page: crate::designer::Page,
    pub layouts: LayoutStore,
    /// The layout the presenter screen is drawn from.
    pub active_layout: Layout,
    pub designer: Option<crate::designer::Designer>,
    pub layout_dialog: Option<LayoutDialog>,
    pub settings: Settings,
    pub store: SettingsStore,
    pub cache: FrameCache,
    /// The frame the audience window last had: held on to across a page
    /// change so the output never blanks between renders.
    last_audience: Option<FrameKey>,
    /// The last allocated canonical frame shown in the Current Slide panel.
    /// A cold jump holds this frame until the target allocation is ready.
    last_presenter: Option<FrameKey>,
    /// One texture handle per cached frame, dropped with the frame it names.
    ///
    /// A handle is a name, not a residency: which *window* can draw it at once
    /// is decided by that window's own view, in `residency`.
    handles: std::collections::HashMap<FrameKey, iced::widget::image::Handle>,
    /// Page pictures with a retained highlight multiplied into them, keyed by
    /// a hash of the frame and the washes. Interior mutability because the
    /// composite is made on demand inside the view's frame lookup, which is
    /// `&self`; bounded by a hard cap, and empty again the moment the frames
    /// containing the real highlights arrive.
    wash_cache: std::cell::RefCell<std::collections::HashMap<u64, iced::widget::image::Handle>>,
    /// The partial repaints standing in for edits the page's frame predates,
    /// one per page. Dropped when a full frame containing the same revision
    /// arrives, which is the same rule the retained previews follow.
    reader_patches: std::collections::HashMap<pulpit_core::page::PageIndex, ReaderPatch>,
    /// The full-page frame size each in-flight patch was asked for, so the
    /// answer can be matched to the frame it was meant to fit.
    reader_patch_pending: std::collections::HashMap<pulpit_core::page::PageIndex, (u32, u32)>,
    pub supervisor: Option<RendererSupervisor>,
    /// The renderer's doorbell, listened to by [`App::subscription`].
    render_wakeup: Option<std::sync::Arc<pulpit_render::supervisor::RenderWakeup>>,
    /// When the windows have had long enough to upload the frames that have
    /// arrived. Keeps the fast tick until then; see [`UPLOAD_SETTLE`].
    uploads_settle_by: Option<Instant>,
    /// Where a page turn's time actually goes. Always on: it costs a couple
    /// of clock reads per event, and the alternative is arguing from the
    /// source about which of five plausible delays is the real one.
    pub latency: crate::latency::Latency,
    /// Where the windows report the uploads they blocked on. Shared with the
    /// `residency` widget in each window's view.
    upload_meter: crate::latency::UploadMeter,
    /// When each outstanding render was submitted, so a frame can report how
    /// long it took. Separate from `pending` because it is diagnostic only.
    submitted_at: std::collections::HashMap<RequestId, Instant>,
    pub documents: DocumentManager,
    pub coordinator: DisplayCoordinator,
    pub diagnostics: DiagnosticsBundle,
    pub inhibitor: Inhibitor,
    /// The desktop we are running on, behind its contracts.
    pub platform: Platform,
    /// The palette in use and how it was chosen.
    pub theme: ThemeState,
    /// Whether pulpit keeps its own motion down, resolved from the
    /// desktop preference and the application setting.
    pub motion: crate::platform::Motion,
    pub editing_colors: crate::settings::ColorScheme,
    /// Incomplete HEX input belongs to the editor, never to persisted
    /// settings. A valid value moves from here into the sparse overrides.
    pub color_drafts:
        std::collections::BTreeMap<(crate::settings::ColorScheme, crate::theme::ColorRole), String>,
    pub confirm_reset_colors: bool,
    /// Which role's colour wheel is open, if any. One at a time: two wheels
    /// over one another would be a puzzle about which one is being dragged.
    pub color_picker_open: Option<crate::theme::ColorRole>,
    /// Where the crash-recovery snapshot lives.
    pub session: crate::session::SessionStore,
    /// The offer made by an interrupted previous run, until it is answered.
    ///
    /// While this is `Some` the application is a fresh start in every way the
    /// audience can see: nothing from the snapshot has been applied, and no
    /// new snapshot is written over it.
    pub pending_restore: Option<crate::session::RestorePlan>,
    /// A confirmed restore waiting for its document to finish opening, so the
    /// slide is set on the deck it was taken against.
    restoring_into_document: Option<crate::session::RestorePlan>,
    /// Keeps snapshot writing off the tick path.
    session_throttle: crate::session::SaveThrottle,
    /// The snapshot last written, so a session sitting still writes nothing.
    last_session: Option<crate::session::SessionSnapshot>,
    /// Corner notices. Never shown on the audience window.
    pub toasts: Toasts,
    /// Whether the main menu is open.
    pub menu_open: bool,
    /// Whether the arrow beside Start has unrolled its alternate actions.
    pub audience_start_menu_open: bool,
    /// Intent, separate from the asynchronous Iced window lifecycle.
    pub audience_started: bool,
    pub presenter_window: Option<window::Id>,
    pub audience_window: Option<window::Id>,
    pub audience_size: Size,
    pub preview_size: Size,
    /// The presenter window in logical points, which is what the layout's
    /// fractions are fractions *of*. `preview_size` is 45% of it and cannot
    /// be un-multiplied back into it without assuming that number is right,
    /// which is the assumption this branch exists to remove.
    presenter_size: Size,
    /// Physical pixels per logical pixel on the presenter's display. Asked
    /// for, never assumed: it decides how wide a panel's frame has to be to
    /// look sharp, and guessing high is four times the pixels for nothing.
    presenter_scale: f32,
    pub last_poll: Instant,
    pub now: Instant,
    watcher: Option<DocumentWatcher>,
    needs_reconcile: bool,
    pending: Vec<(RequestId, FrameKey)>,
    /// Link annotations by (document, pdf page), as reported by the renderer.
    links: std::collections::HashMap<(u64, usize), Vec<pulpit_core::PageLink>>,
    /// Pages whose links have already been asked for, so navigation does not
    /// re-request them every tick.
    links_requested: std::collections::HashSet<(u64, usize)>,
    /// Documents whose outline and feature report have been asked for, so a
    /// per-document question is not repeated on every reconcile.
    document_survey_requested: std::collections::HashSet<u64>,
    /// Page labels and the outline of the open document, as reported by the
    /// renderer. Section display and the outline navigator read this.
    pub navigation: std::collections::HashMap<u64, pulpit_core::DocumentNavigation>,
    /// What the open document declares that pulpit will flatten or ignore.
    pub capabilities: std::collections::HashMap<u64, pulpit_render::DocumentCapabilities>,
    /// Overlay declarations, staging and the frames they produce.
    pub media: crate::media::MediaCoordinator,
    /// Media worker supervision. Absent when no runtime is usable at all,
    /// which is not a failure: every overlay then shows its static fallback.
    pub media_supervisor: Option<pulpit_media::MediaSupervisor>,
    /// Attachments already asked of the renderer, so a re-render does not
    /// fetch the same embedded file twice.
    attachments_requested: std::collections::HashSet<String>,
    /// Whether the presenter has been told a media runtime could not start.
    /// Once per run: every overlay on the slide reports the same thing.
    media_runtime_warned: bool,
    /// Which overlay has focus, and what a keypress means.
    pub input_router: crate::media::InputRouter,
    /// Overlay declarations gathered per page, awaiting the page labels that
    /// group them into logical slides.
    overlay_declarations:
        std::collections::BTreeMap<usize, Vec<pulpit_core::overlay::OverlayDeclaration>>,
    /// Whether overlay declarations changed since the index was last rebuilt.
    /// A deck's opening burst delivers one Overlays event per page; rebuilt
    /// per event, the cumulative work is quadratic in page count, so the
    /// rebuild happens once per drained batch instead.
    overlays_dirty: bool,
    /// Overlay diagnostics accumulated since that last rebuild.
    pending_overlay_diagnostics: Vec<String>,
    /// Last pointer position over the live current slide, in normalised
    /// slide coordinates. What a press hit-tests against the page's links.
    slide_cursor: Option<(f32, f32)>,
    /// Which link the pointer is over, as an index into the current page's
    /// links. Presenter-only: an affordance never reaches the audience.
    hovered_link: Option<usize>,
    /// Which link the keyboard has focused, if any. Kept separate from the
    /// hover: a presenter tabbing through links must not lose their place
    /// because the mouse happens to be resting somewhere.
    focused_link: Option<usize>,
    document_serial: u64,
    /// A document the workers still hold for a file the presenter has moved
    /// on from. It is released once its replacement is promoted, so a failed
    /// open leaves the old deck rendering.
    retired_document: Option<pulpit_core::DocumentId>,
    /// Placement requests the compositor has not honoured yet. Some window
    /// managers ignore placement issued before a window is mapped, so a
    /// refused or unapplied request is retried a bounded number of times.
    placement_retries: Vec<PlacementRetry>,
    /// Post-map focus repairs waiting for the window manager to finish
    /// showing or moving the audience window.
    presenter_refocus_deadlines: Vec<Instant>,
    /// Wall clock and monotonic clock at the previous tick, used to notice a
    /// suspend/resume: the monotonic clock stops while the machine sleeps.
    last_wall: std::time::SystemTime,
    /// The most recent key press that resolved to no action, offered in the
    /// UI so an unidentified remote key can be bound on the spot.
    pub unbound_key: Option<(Option<String>, u32)>,
    /// The deck as thumbnails, over the presenter screen.
    pub overview: bool,
    /// A scroll the keyboard asked for and has not seen arrive yet: the
    /// offset it asked for, and the moment the claim lapses. macOS keeps
    /// delivering momentum scroll events after the fingers lift, and those
    /// would otherwise overwrite the position an arrow or page key just
    /// chose, so the glide is ignored until the requested offset echoes back
    /// or the claim runs out.
    overview_scroll_claim: Option<(f32, Instant)>,
    /// The moment the last scroll event arrived, while the grid is still
    /// coasting. Once it has been quiet for [`OVERVIEW_SETTLE`] the selection
    /// catches up with what is on screen.
    overview_settling: Option<Instant>,
    /// Where the overview is scrolled to, so the grid can build only the
    /// rows that are actually on screen.
    pub overview_scroll: f32,
    /// The shape the overview grid last laid itself out in: columns, the
    /// height of one row including its gap, and the height of the visible
    /// area. Only the view knows these — they depend on the window — and the
    /// keyboard needs them to move a selection up and down the grid and to
    /// keep it on screen, so the view records them as it builds.
    pub overview_grid: std::cell::Cell<OverviewGrid>,
    /// Every page as a small picture, on its own budget.
    pub thumbnails: crate::thumbnails::ThumbnailCache,
    /// Pages still wanting one, nearest the presenter first, each with the
    /// width it should be rendered at: coarse for coverage, fine for the
    /// band around the presenter once coverage is done.
    thumbnail_queue: std::collections::VecDeque<(usize, u32)>,
    /// Which in-flight requests are warming work. Tracked explicitly rather
    /// than inferred from the frame size, because a small presenter panel can
    /// legitimately ask for a frame of exactly the thumbnail width, and
    /// misrouting that would leave the panel empty for ever.
    thumbnail_requests: std::collections::HashSet<RequestId>,
    /// The generation and page count the warming plan was made for.
    thumbnail_plan: Option<(pulpit_core::RenderGeneration, usize)>,
    /// The one width this document's thumbnails are rendered at, chosen when
    /// the plan was made so the whole deck fits the budget.
    thumbnail_plan_width: u32,
    /// Everything the thumbnail plan depends on, so the per-tick replan is
    /// skipped whenever none of it moved.
    thumbnail_plan_inputs: Option<ThumbnailPlanInputs>,
    /// A slider drag in progress: the presenter is choosing a slide by
    /// dragging, and wants to see what they are about to land on.
    pub scrubbing: bool,
    /// The presenter's marks on the current slide, and what the pointer is
    /// armed to do.
    ///
    /// What is on screen: the unfinished gesture, which is this and nothing
    /// else's (A2), and a view of the annotations the document holds for the
    /// page this slide is showing. There is no cache of other slides' marks
    /// any more — the document is where they are (A1), and a page turn reads
    /// them back out of it.
    pub annotations: pulpit_core::annotation::Annotations,
    /// A shared snapshot of `annotations` the views draw from. Refreshed by
    /// [`App::sync_annotation_layers`] only when the model changed, so a
    /// view pass shares a reference instead of copying every stroke.
    annotations_view: std::sync::Arc<pulpit_core::annotation::Annotations>,
    /// Last complete Typst SVGs, compiled out of process and shared by both
    /// windows so the audience never sees a partial replacement.
    typst_annotations: crate::typst_annotation::Coordinator,
    /// Tessellated annotation geometry per drawing site, replayed between
    /// changes.
    marks_caches: crate::widgets::annotations::view::MarksCaches,
    /// What the snapshot and caches were last built from.
    marks_signature: Option<MarksSignature>,
    /// The outline section for (document, page), memoised because the view
    /// asks on every pass. Interior mutability: the view only reads `App`.
    section_cache: SectionCache,
    /// The document fingerprint last taken for the crash-recovery snapshot,
    /// keyed by generation and path so the metadata syscall happens on
    /// document changes rather than on a two-second clock.
    session_fingerprint: Option<(
        pulpit_core::RenderGeneration,
        std::path::PathBuf,
        Option<crate::session::DocumentFingerprint>,
    )>,
    /// The settings page's diagnostics text, rebuilt at most once a second
    /// while that page is open.
    diagnostics_report_cache: std::cell::RefCell<Option<(std::time::Instant, String)>>,
    /// The desktop's appearance preference, read over the portal at startup
    /// and on resume. Cached: the read is a blocking D-Bus round trip, and
    /// the colour editor was paying two of them per keystroke.
    appearance_probe: crate::platform::appearance::SystemAppearance,
    /// The desktop's reduced-motion preference, cached the same way.
    motion_probe: crate::platform::appearance::MotionPreference,
    /// Whether the settings have changed since they were last written.
    /// Writing is a TOML serialise plus a fsync-class rename, so the flush
    /// is throttled to the tick rather than done per keystroke.
    settings_dirty: bool,
    settings_throttle: crate::session::SaveThrottle,
    /// The newest pointer move bound for an overlay, held until the tick.
    /// Moves arrive at device rate and only the latest position matters;
    /// forwarding each one flooded the worker pipe and the CDP connection.
    pending_pointer_move: Option<(pulpit_media::SessionId, pulpit_media::InputEvent)>,
    /// The scrub card's anchor pane, memoised by panel size while a drag is
    /// in progress so the layout is not re-solved on every pass of it.
    pub scrub_anchor_cache: ScrubAnchorCache,
    /// The reader: the viewport, the armed document tool and the answers the
    /// worker last gave about the open document. Beside the presentation
    /// state rather than replacing it, because mode is which layout is
    /// mounted, not which document is loaded (§2.3 of `SPEC-document.md`).
    pub reader: crate::reader::ReaderSession,
    /// The one search, whichever view is on screen.
    ///
    /// On the application rather than on the reader: a presenter looking for
    /// the slide whose notes mention a name is searching the same document
    /// through a different layout, and two models would be two answers to one
    /// question.
    pub search: pulpit_core::search::SearchState,
    /// The thread talking to this document's worker, when document mode is
    /// available for it. `None` when nothing is open, or when the worker
    /// could not be started — presentation mode does not depend on it.
    reader_link: Option<crate::reader_link::ReaderLink>,
    /// What each in-flight mutation was, waiting for its answer.
    ///
    /// The worker answers in order and says nothing about why it was asked,
    /// so the intent is remembered here rather than guessed from the answer —
    /// and the transaction is kept with it, because it is journalled when the
    /// answer confirms it and not before. A mutation with no answer is not a
    /// mutation that happened (§11.5).
    reader_pending: std::collections::VecDeque<PendingEdit>,
    /// Is a text-selection query at the document worker with no answer yet?
    ///
    /// The worker is serial, and a drag samples the pointer far faster than
    /// text can be resolved. Without this guard every sample queued a query,
    /// and the release's finalising one — the one that commits — sat behind
    /// the whole backlog: the highlighter "eventually worked". With it there
    /// is one query in flight and one waiting, and the waiting one is always
    /// the newest.
    selection_query_in_flight: bool,
    selection_query_waiting: Option<(
        pulpit_core::page::PageIndex,
        pulpit_render::document::TextSelection,
    )>,
    /// Every edit, on disk as it is made (§11.1). `None` when there is no
    /// document open, or when the journal could not be written — in which
    /// case the user has been told that a crash would lose their edits.
    reader_journal: Option<crate::reader_journal::Journal>,
    /// What the last run left unsaved for the document being opened, held
    /// until the document is up and the offer can be made.
    pending_reader_recovery: Option<crate::reader_journal::RecoveredJournal>,
    /// The offer itself, drawn as a dialogue with no way out but an answer.
    /// Inert: nothing is applied until one is given (§11.4).
    pub reader_recovery: Option<crate::reader_journal::RecoveredJournal>,
    /// How the reader's pages reach the render worker pool: which supervisor
    /// document and generation they are rendered from, and the snapshot
    /// machinery that moves those forward after an edit (A7).
    reader_render: ReaderRenderState,
    /// The highlighter's open text selection on the live slide.
    ///
    /// The *same* gesture type document mode uses, and deliberately so: the
    /// highlighter selects text in both modes, so there is one implementation
    /// of "sweep text, ask the engine where it is, commit a `/Highlight` over
    /// it" and this is presentation's handle on it. What differs between the
    /// modes is only the space the pointer arrives in — slide fractions here,
    /// page points there — and `SlidePlacement` is the whole of that
    /// difference (§14.3).
    ///
    /// Separate from `self.reader.interaction` rather than shared with it
    /// because the two modes arm tools independently: a highlighter armed at
    /// the lectern must not disarm the reader's eraser.
    presenter_interaction: pulpit_core::annotate::AnnotationInteraction,
    /// The note or text mark being written, if one is.
    pub composing_mark: Option<ComposingMark>,
    /// Live tool choices for the annotation palette. These are session
    /// controls, not mutations to a built-in layout.
    pub annotation_controls: crate::widgets::AnnotationControls,
    /// The wall-clock cues and the state of the popup that edits them. Also
    /// session controls: they belong to this talk, not to the layout.
    pub alarm_controls: crate::widgets::AlarmControls,
    /// Which way the timer runs, how long the talk is, and whether the menu
    /// that sets them is open. Session controls beside the alarms.
    pub timer_controls: crate::widgets::TimerControls,
    /// The wall-clock second the last tick saw, so a cue is caught by the
    /// window it fell in rather than by an equality test.
    pub last_seconds_of_day: u32,
    /// Whether the presenter has already been told that marks made over this
    /// document are not being kept. Said once, not once per stroke.
    warned_marks_are_not_kept: bool,
}

/// A cached frame together with the texture handle that draws it.
#[derive(Debug, Clone)]
pub struct Picture {
    pub handle: iced::widget::image::Handle,
}

/// A placement that has not taken effect yet.
#[derive(Debug, Clone)]
struct PlacementRetry {
    role: Role,
    identity: pulpit_display::MonitorIdentity,
    mode: WindowMode,
    attempt: u32,
    due: Instant,
}

/// How many times a refused placement is retried after the window is mapped.
const MAX_PLACEMENT_RETRIES: u32 = 4;
const PLACEMENT_RETRY_DELAY: Duration = Duration::from_millis(250);
/// A monotonic gap larger than this means the machine was asleep.
const RESUME_GAP: Duration = Duration::from_secs(5);

fn annotation_options_in(layout: &Layout) -> crate::widgets::AnnotationOptions {
    layout
        .widgets()
        .into_iter()
        .find(|widget| widget.kind() == crate::widgets::WidgetKind::Annotations)
        .map(|widget| widget.annotations())
        .unwrap_or_default()
}

impl App {
    pub fn new(
        initial: Option<PathBuf>,
        start_page: crate::StartPage,
        settings: crate::settings::Settings,
    ) -> (Self, Task<Message>) {
        // `main` already loaded the settings to configure logging; they are
        // passed in rather than read and parsed a second time.
        let store = SettingsStore::default();

        // The desktop, behind its contracts. Everything the views ask about
        // capabilities comes from this snapshot, never from an OS name.
        let platform = Platform::detect();
        let system_appearance = platform.services.system_appearance();
        let motion_probe = platform.services.reduced_motion();
        let preference = settings.appearance.appearance;
        let theme = ThemeState::new(
            system_appearance.resolve(preference),
            system_appearance.fell_back(preference),
            &settings.appearance.colors,
        );

        let mut diagnostics = DiagnosticsBundle::new(platform_description());
        for line in platform.capabilities.report() {
            diagnostics.note(line);
        }
        if theme.fell_back {
            diagnostics
                .note("system appearance could not be read; using the dark palette".to_string());
        }
        let coordinator = DisplayCoordinator::new(settings.display.roles.clone());
        diagnostics.display_backend = coordinator.backend.name().to_string();
        diagnostics.capabilities = format!("{:?}", coordinator.capabilities);

        let mut supervisor = RendererSupervisor::start(SupervisorConfig {
            workers: settings.rendering.workers.clamp(1, 8),
            command: WorkerCommand::CurrentExe {
                arg: "--render-worker".into(),
            },
            deadline: Duration::from_secs(settings.rendering.worker_deadline_secs.max(1)),
            ..SupervisorConfig::default()
        })
        .map_err(|e| {
            tracing::error!(error = %e, "cannot start the renderer");
            e
        })
        .ok();
        // Taken once, here, and held for the life of the application: the
        // subscription is rebuilt after every message and must hand back the
        // same listener each time or iced would see a new subscription and
        // restart it.
        let render_wakeup = supervisor
            .as_mut()
            .and_then(|supervisor| supervisor.take_wakeup());

        let now = Instant::now();

        // A snapshot that survived the last run means that run did not go
        // through `quit`, which deletes it: this is the crash signal. The
        // snapshot is only turned into an offer here — nothing from it is
        // applied until the presenter confirms, so startup proceeds exactly
        // as it would with no snapshot at all.
        let session = crate::session::SessionStore::default();
        let pending_restore = session
            .load()
            .filter(|snapshot| snapshot.is_worth_offering())
            .map(|snapshot| {
                let current = snapshot
                    .document
                    .as_ref()
                    .and_then(|document| crate::session::fingerprint(&document.path));
                snapshot.plan(current.as_ref())
            });
        if pending_restore.is_some() {
            tracing::info!("a previous session did not exit cleanly; offering to restore it");
        }

        // The layout library lives beside the settings file.
        let layouts = LayoutStore::load(platform.services.directories().layouts());
        let active_layout = settings
            .layout
            .active
            .as_ref()
            .map(|id| LayoutId(id.clone()))
            .and_then(|id| layouts.get(&id).cloned())
            .unwrap_or_else(|| {
                layouts
                    .built_in()
                    .first()
                    .cloned()
                    .expect("there is always a built-in layout")
            });
        let annotation_controls =
            crate::widgets::AnnotationControls::new(annotation_options_in(&active_layout));
        let mut alarm_controls = crate::widgets::AlarmControls::new(settings.timer.alarms.clone());
        alarm_controls.snooze_minutes = settings.timer.snooze_minutes;
        alarm_controls.sanitise();
        let mut timer_controls =
            crate::widgets::TimerControls::new(settings.timer.target(), settings.timer.count_down);
        timer_controls.snooze_minutes = alarm_controls.snooze_minutes;

        let mut app = Self {
            state: PresentationState::default(),
            page: crate::designer::Page::Presenter,
            layouts,
            active_layout,
            designer: None,
            layout_dialog: None,
            cache: FrameCache::new(settings.rendering.cache_budget_mib * 1024 * 1024),
            handles: std::collections::HashMap::new(),
            wash_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
            reader_patches: std::collections::HashMap::new(),
            reader_patch_pending: std::collections::HashMap::new(),
            last_audience: None,
            last_presenter: None,
            documents: DocumentManager::new(
                initial.clone().unwrap_or_default(),
                ReloadPolicy {
                    debounce: Duration::from_millis(settings.rendering.watch_debounce_ms),
                    ..ReloadPolicy::default()
                },
            ),
            settings,
            store,
            supervisor,
            render_wakeup,
            uploads_settle_by: None,
            latency: crate::latency::Latency::default(),
            upload_meter: crate::latency::UploadMeter::default(),
            submitted_at: std::collections::HashMap::new(),
            coordinator,
            diagnostics,
            inhibitor: Inhibitor::new(),
            theme,
            // Settled properly by `apply_appearance` once the platform can be
            // asked; full motion until then, so nothing is reduced on a guess.
            motion: crate::platform::Motion::Full,
            editing_colors: match theme.resolved {
                crate::platform::appearance::Resolved::Light => crate::settings::ColorScheme::Light,
                crate::platform::appearance::Resolved::Dark
                | crate::platform::appearance::Resolved::HighContrast => {
                    crate::settings::ColorScheme::Dark
                }
            },
            color_drafts: std::collections::BTreeMap::new(),
            confirm_reset_colors: false,
            color_picker_open: None,
            session,
            pending_restore,
            restoring_into_document: None,
            session_throttle: crate::session::SaveThrottle::default(),
            last_session: None,
            platform,
            toasts: Toasts::new(),
            menu_open: false,
            audience_start_menu_open: false,
            audience_started: false,
            presenter_window: None,
            audience_window: None,
            audience_size: Size::new(1280.0, 720.0),
            preview_size: Size::new(640.0, 360.0),
            // Replaced by the first resize, which arrives before any panel
            // render is asked for.
            presenter_size: Size::new(1422.0, 800.0),
            presenter_scale: 1.0,
            last_poll: now,
            now,
            watcher: None,
            needs_reconcile: true,
            links: std::collections::HashMap::new(),
            links_requested: std::collections::HashSet::new(),
            document_survey_requested: std::collections::HashSet::new(),
            navigation: std::collections::HashMap::new(),
            capabilities: std::collections::HashMap::new(),
            media: crate::media::MediaCoordinator::new(),
            // Unprobed: probing runs the candidate browsers' `--version`,
            // hundreds of milliseconds of subprocess spawning that was paid
            // synchronously here, before the first window could appear. The
            // probes arrive via `Message::MediaProbed` from a helper thread,
            // and no session opens until they do.
            media_supervisor: Some(pulpit_media::MediaSupervisor::unprobed(
                crate::media::config_from_settings(None, None, None, None),
            )),
            attachments_requested: std::collections::HashSet::new(),
            media_runtime_warned: false,
            input_router: crate::media::InputRouter::new(),
            overlay_declarations: std::collections::BTreeMap::new(),
            overlays_dirty: false,
            pending_overlay_diagnostics: Vec::new(),
            slide_cursor: None,
            hovered_link: None,
            focused_link: None,
            pending: Vec::new(),
            document_serial: 0,
            retired_document: None,
            placement_retries: Vec::new(),
            presenter_refocus_deadlines: Vec::new(),
            last_wall: std::time::SystemTime::now(),
            unbound_key: None,
            overview: false,
            overview_scroll_claim: None,
            overview_settling: None,
            overview_scroll: 0.0,
            overview_grid: std::cell::Cell::new(OverviewGrid::default()),
            thumbnails: crate::thumbnails::ThumbnailCache::new(THUMBNAIL_BUDGET_BYTES),
            thumbnail_queue: std::collections::VecDeque::new(),
            thumbnail_requests: std::collections::HashSet::new(),
            thumbnail_plan: None,
            thumbnail_plan_width: THUMBNAIL_WIDTH,
            thumbnail_plan_inputs: None,
            scrubbing: false,
            annotations: pulpit_core::annotation::Annotations::default(),
            annotations_view: std::sync::Arc::new(pulpit_core::annotation::Annotations::default()),
            typst_annotations: Default::default(),
            marks_caches: Default::default(),
            marks_signature: None,
            section_cache: std::cell::RefCell::new(None),
            session_fingerprint: None,
            diagnostics_report_cache: std::cell::RefCell::new(None),
            appearance_probe: system_appearance,
            motion_probe,
            settings_dirty: false,
            settings_throttle: crate::session::SaveThrottle::default(),
            pending_pointer_move: None,
            scrub_anchor_cache: std::cell::RefCell::new(None),
            reader: crate::reader::ReaderSession::new(),
            search: pulpit_core::search::SearchState::new(),
            reader_render: ReaderRenderState::default(),
            reader_link: None,
            reader_pending: std::collections::VecDeque::new(),
            selection_query_in_flight: false,
            selection_query_waiting: None,
            warned_marks_are_not_kept: false,
            reader_journal: None,
            pending_reader_recovery: None,
            reader_recovery: None,
            presenter_interaction: pulpit_core::annotate::AnnotationInteraction::new(),
            composing_mark: None,
            annotation_controls,
            alarm_controls,
            timer_controls,
            last_seconds_of_day: crate::view::seconds_of_day(),
        };

        // Open directly on a layout page when asked.
        match start_page {
            crate::StartPage::Presenter => {}
            crate::StartPage::Library => app.page = crate::designer::Page::Library,
            crate::StartPage::Editor(id) => {
                let id = LayoutId(id);
                match app.layouts.get(&id).cloned() {
                    Some(layout) => {
                        let mut designer = crate::designer::Designer::open(layout);
                        designer.revalidate();
                        app.designer = Some(designer);
                        app.page = crate::designer::Page::Editor;
                    }
                    None => {
                        app.notify(format!("No layout “{id}”; showing the library instead."));
                        app.page = crate::designer::Page::Library;
                    }
                }
            }
        }

        // The presenter window is the only window created at startup. The
        // audience toplevel does not exist until Start, so generic desktops
        // can place it on the workspace active at that intentional moment.
        // The default size is only a wish: on a small panel it is clamped onto
        // a real work area, and it never opens below the size in which the
        // presenter view still works.
        let (minimum_width, minimum_height) = app.platform.window.minimum_size();
        let work_areas: Vec<crate::platform::Bounds> = app
            .coordinator
            .snapshot
            .monitors
            .iter()
            .map(|monitor| {
                crate::platform::Bounds::new(
                    monitor.geometry.x as f32,
                    monitor.geometry.y as f32,
                    monitor.geometry.width as f32,
                    monitor.geometry.height as f32,
                )
            })
            .collect();
        let bounds = app.platform.window.clamp_to_work_area(
            crate::platform::Bounds::new(0.0, 0.0, 1280.0, 800.0),
            &work_areas,
        );
        let (presenter, open_presenter) = window::open(display::identify_window(
            window::Settings {
                size: Size::new(bounds.width, bounds.height),
                min_size: Some(Size::new(minimum_width, minimum_height)),
                ..window::Settings::default()
            },
            Role::Presenter,
        ));
        app.presenter_window = Some(presenter);

        let mut tasks = vec![open_presenter.map(move |id| Message::WindowOpened {
            role: Role::Presenter,
            id,
        })];
        if let Some(path) = initial {
            tasks.push(Task::done(Message::Opened(Some(path))));
        }
        // Read the UTC offset now, on purpose: it is cached in a OnceLock
        // but primed by spawning `date +%z`, and the first clock widget to
        // draw should not be the thing paying for a subprocess.
        let _ = crate::view::seconds_of_day();

        // Probe the media runtimes off the startup path: each candidate
        // browser is asked `--version` in a subprocess, and paying that
        // before the first window appears was pure startup latency.
        let browser = app
            .media_supervisor
            .as_ref()
            .and_then(|media| media.config().browser_path.clone());
        tasks.push(Task::future(async move {
            let (sender, receiver) = iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let _ = sender.send(pulpit_media::runtime::probe_all(browser.as_deref()));
            });
            Message::MediaProbed(receiver.await.unwrap_or_default())
        }));
        (app, Task::batch(tasks))
    }

    pub fn title(&self, window: window::Id) -> String {
        let name = self
            .state
            .document()
            .and_then(|d| d.path.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "no document".into());
        if Some(window) == self.audience_window {
            format!("{name} — pulpit (audience)")
        } else {
            format!("{name} — pulpit")
        }
    }

    pub fn theme(&self, _window: window::Id) -> iced::Theme {
        crate::theme::iced_theme(self.theme.palette)
    }

    /// Is anything happening that deserves the fast tick? When nothing is —
    /// no renders in flight, no media playing, no thumbnails warming, no
    /// toast counting down — the application settles to a slow tick, and an
    /// idle presentation costs wakeups a hand can count instead of twenty a
    /// second. Everything the slow tick still drives (resume detection, the
    /// clock, file watching, the throttled saves) tolerates its cadence.
    fn is_live(&self) -> bool {
        !self.pending.is_empty()
            // Frames that have arrived but may not be on the GPU yet. The
            // renders being finished is precisely when this matters: the
            // uploads that make the next turn instant happen after it.
            || self.uploads_settle_by.is_some_and(|at| self.now < at)
            || !self.thumbnail_queue.is_empty()
            || self.scrubbing
            || !self.placement_retries.is_empty()
            // A pending focus repair is timed in tens of milliseconds; the
            // settled tick would overshoot every deadline in the list.
            || !self.presenter_refocus_deadlines.is_empty()
            || !self.toasts.is_empty()
            // A cue going off is animating: at the settled tick the pulse
            // would arrive in about five steps and read as a stutter rather
            // than a fade.
            || self.alarm_controls.ringing.is_some()
            || self.needs_reconcile
            // An edit on its way to the page: the worker has been asked and
            // has not answered, or it has and the snapshot the render pool
            // draws from is still owed. Every step of that is a round trip
            // polled from the tick, so at the settled tick a placed mark waits
            // a quarter of a second per step for no reason other than the
            // clock — which is what "it takes a long time to appear" is.
            || !self.reader_pending.is_empty()
            // A text selection being swept out. The answer that says where
            // the text is comes back on a round trip drained from the tick,
            // so at the settled tick the quads follow the hand at four frames
            // a second — which is what "selecting text lags" is.
            || self.selection_query_in_flight
            || self.selection_query_waiting.is_some()
            // An edit the page cannot show yet. An edit the previews *do*
            // show is not on this list: it is already on screen, and its
            // snapshot is two seconds away, which the settled tick notices
            // perfectly well.
            || (self.reader_render.edited_at.is_some()
                && (self.reader_render.forced
                    || self.reader_render.urgency == crate::reader::RasterUrgency::Prompt))
            || self.reader_render.snapshot_in_flight.is_some()
            // A scroll settles in about a tenth of a second; the settled tick
            // would miss the moment the grid stopped moving entirely.
            || self.overview_settling.is_some()
            || self
                .media_supervisor
                .as_ref()
                .is_some_and(|media| media.session_count() > 0)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // Subscription identity is stable across view rebuilds, so timers and
        // watchers are never duplicated. The tick interval is the one
        // deliberate exception: it follows `is_live`, and iced restarts the
        // timer when it changes.
        let interval = if self.is_live() { TICK } else { SETTLED_TICK };
        let ticks = iced::time::every(interval).map(Message::Tick);
        let keys = iced::event::listen_with(|event, status, _window| match event {
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                text,
                ..
            }) => Some(Message::Key {
                key: describe_key(&key),
                text: text.map(|value| value.to_string()),
                scancode: physical_scancode(&physical_key),
                shift: modifiers.shift(),
                control: modifiers.command() || modifiers.control(),
                alt: modifiers.alt(),
            }),
            // Letting go of the button ends a scrub, wherever the pointer
            // happens to be. The slider's own release only arrives when the
            // release lands on the slider, so a drag that wanders off it —
            // which is most of them — would otherwise leave the thumbnail on
            // the screen with nothing to take it away.
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                Some(Message::PointerReleased)
            }
            // A finger or a pen lifting is the same event as a mouse button
            // coming up. Without this a stroke drawn on a touchscreen never
            // ends — the pen stays down for the rest of the talk — and the
            // scrub thumbnail is never taken away. `FingerLost` is the
            // compositor withdrawing the touch (a gesture taken over, a
            // palm rejected), which must end the stroke just as firmly.
            iced::Event::Touch(
                iced::touch::Event::FingerLifted { .. } | iced::touch::Event::FingerLost { .. },
            ) => Some(Message::PointerReleased),
            // A key held down inside a web overlay needs its release, or the
            // page believes it is still held for the rest of the talk.
            iced::Event::Keyboard(iced::keyboard::Event::KeyReleased { key, .. }) => {
                describe_key(&key).map(Message::KeyReleased)
            }
            iced::Event::Mouse(iced::mouse::Event::WheelScrolled { delta }) => {
                if status == iced::event::Status::Captured {
                    return None;
                }
                let (x, y) = match delta {
                    iced::mouse::ScrollDelta::Lines { x, y } => {
                        // Lines are what a notched wheel reports; a browser
                        // deals in pixels, and this is the conventional
                        // multiplier for one notch.
                        (x * LINE_SCROLL_PIXELS, y * LINE_SCROLL_PIXELS)
                    }
                    iced::mouse::ScrollDelta::Pixels { x, y } => (x, y),
                };
                Some(Message::Wheel { x, y })
            }
            _ => None,
        });
        let closes = window::close_events().map(Message::WindowClosed);
        let resizes = window::resize_events().map(|(id, size)| Message::Resized { id, size });
        let mut subscriptions = vec![ticks, keys, closes, resizes];
        if let Some(wakeup) = self.render_wakeup.clone() {
            subscriptions.push(render_wakeups(wakeup));
        }
        Subscription::batch(subscriptions)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.dispatch(message);
        // After every message, not per handler: annotations mutate from a
        // dozen places, and the view must never draw a stale snapshot.
        self.sync_annotation_layers();
        task
    }

    /// Refresh the annotation snapshot the views draw from, when — and only
    /// when — something that affects the drawn layer changed. The revision
    /// makes the check one integer compare; the deep copy happens per edit,
    /// not per view pass.
    fn sync_annotation_layers(&mut self) {
        let signature = MarksSignature {
            revision: self.annotations.revision(),
            style: self.annotation_options().style(),
            aspect: self.slide_aspect(),
            crop: self
                .state
                .audience_source()
                .map(|source| source.region)
                .unwrap_or(pulpit_core::notes::Region::FULL),
            accent: self.theme.palette.accent,
        };
        if self.marks_signature == Some(signature) {
            return;
        }
        self.marks_signature = Some(signature);
        self.annotations_view = std::sync::Arc::new(self.annotations.clone());
        self.typst_annotations
            .sync(&self.annotations, std::time::Instant::now());
        self.marks_caches.invalidate();
    }

    /// The annotation snapshot the views draw from.
    pub fn annotations_snapshot(&self) -> &std::sync::Arc<pulpit_core::annotation::Annotations> {
        &self.annotations_view
    }

    pub fn rendered_text_snapshot(
        &self,
    ) -> &std::sync::Arc<std::collections::HashMap<u64, crate::typst_annotation::RenderedText>>
    {
        self.typst_annotations.snapshot()
    }

    /// The audience window's annotation-geometry cache.
    pub fn audience_marks_cache(&self) -> &std::rc::Rc<iced::widget::canvas::Cache> {
        &self.marks_caches.audience
    }

    /// The settings page's diagnostics report, rebuilt at most once a
    /// second. Identical content between rebuilds is what lets iced keep the
    /// shaped paragraph instead of re-laying out several kilobytes of text
    /// on every view pass.
    pub fn diagnostics_report(&self) -> String {
        {
            let cached = self.diagnostics_report_cache.borrow();
            if let Some((at, text)) = cached.as_ref() {
                if at.elapsed() < std::time::Duration::from_secs(1) {
                    return text.clone();
                }
            }
        }
        let report = self.build_diagnostics_report();
        *self.diagnostics_report_cache.borrow_mut() =
            Some((std::time::Instant::now(), report.clone()));
        report
    }

    /// Which PDF backend is really in use, asked of the supervisor that knows.
    ///
    /// The bundle carries a field of its own for this, but only a test ever
    /// set it, so every report a user could produce said "pdf backend: " and
    /// left the first question any rendering bug raises unanswered.
    fn pdf_backend(&self) -> String {
        let Some(render) = self.supervisor.as_ref().map(|s| s.diagnostics()) else {
            return "no renderer".into();
        };
        match (render.backend, render.backend_version) {
            (Some(backend), Some(version)) => format!("{backend} ({version})"),
            (Some(backend), None) => backend,
            // A worker announces its backend on the first thing it is asked
            // to do, so this is the honest answer before any document opens.
            (None, _) => "not yet reported".into(),
        }
    }

    /// Where a page turn's time went, in the terms a presenter would use.
    ///
    /// Settled is the honest headline — the last surface to answer — and the
    /// first picture is next to it because they answer different complaints:
    /// "it did not respond" is the first, "it looked soft for a moment" is
    /// the gap between them. The stages below are only there to locate the
    /// time when the headline is bad.
    fn latency_report(&self) -> String {
        let mut report = String::from("\n## Page turns\n");
        let Some((typical, worst)) = self.latency.settled_summary() else {
            report.push_str("- nothing measured yet\n");
            return report;
        };
        let turns = self.latency.turns();
        let first: Duration = turns
            .iter()
            .map(|turn| turn.first_picture())
            .sum::<Duration>()
            / turns.len() as u32;
        report.push_str(&format!(
            "- {} turns: settled {} typical, {} worst; first picture {} typical\n",
            turns.len(),
            millis(typical),
            millis(worst),
            millis(first),
        ));
        if let Some(turn) = turns.back() {
            report.push_str(&format!(
                "- last turn (slide {}): projector {}{}, panel {}{}\n",
                turn.slide + 1,
                millis(turn.audience_exact),
                stand_in_note(turn.audience_stand_in),
                millis(turn.presenter_exact),
                stand_in_note(turn.presenter_stand_in),
            ));
        }
        if self.latency.abandoned() > 0 {
            report.push_str(&format!(
                "- {} turns abandoned, overtaken by the next one\n",
                self.latency.abandoned(),
            ));
        }
        // Submitted to frame in hand, so the queue wait is in it: that wait
        // is part of what a turn spends, and hiding it would flatter the
        // number that matters.
        for (name, total, worked, rasterised) in [
            (
                "renders a window waited for",
                self.latency.render(),
                self.latency.render_worked(),
                self.latency.render_rendered(),
            ),
            (
                "deck warming, which nobody waits for",
                self.latency.warming(),
                self.latency.warming_worked(),
                self.latency.warming_rendered(),
            ),
        ] {
            if total.calls == 0 {
                continue;
            }
            report.push_str(&format!(
                "- {name}: {} finished, {} typical, {} worst\n",
                total.calls,
                millis(total.mean()),
                millis(total.worst),
            ));
            // The worker's share, and by subtraction ours. A deep queue and a
            // slow rasteriser are one number without this split, and they
            // call for opposite fixes.
            // Three places a job's time can go, and only one of them is work:
            // this process's queue, the worker's own inbox, and the
            // rasteriser. They call for entirely different fixes, and until
            // all three were separated the answer moved every time a bucket
            // was split.
            report.push_str(&format!(
                "    rasterising {} typical, {} worst; worker inbox {} typical; our queue {} typical\n",
                millis(rasterised.mean()),
                millis(rasterised.worst),
                millis(worked.mean().saturating_sub(rasterised.mean())),
                millis(total.mean().saturating_sub(worked.mean())),
            ));
        }
        // The distinction that decides everything: a render for the page a
        // window is showing is one a presenter waits on, and a render for
        // the page after it is not. Both are "live"; only one is urgent.
        for (name, stage) in [
            ("for the page on screen", self.latency.on_screen()),
            ("for a page one step away", self.latency.prefetch()),
        ] {
            if stage.calls == 0 {
                continue;
            }
            report.push_str(&format!(
                "    {name}: {} finished, {} typical, {} worst\n",
                stage.calls,
                millis(stage.mean()),
                millis(stage.worst),
            ));
        }
        // Everything below happens on the event loop, where the interface is
        // not drawing. That is the whole reason to count it separately from
        // a render, which happens in another process entirely.
        let stages = self.latency.stages();
        for (name, stage) in [
            ("planning renders", stages.plan_renders),
            ("following media", stages.service_media),
            ("taking delivery of frames", stages.drain_renderer),
        ] {
            if stage.calls == 0 {
                continue;
            }
            report.push_str(&format!(
                "- on the event loop, {name}: {} calls, {} typical, {} worst\n",
                stage.calls,
                millis(stage.mean()),
                millis(stage.worst),
            ));
        }
        // Uploads are on the event loop too, but outside `update`: they
        // happen while a window lays itself out, which is why they are
        // reported from a meter the widget writes to rather than from a
        // stage. Counting only the stages, as this once did, said the event
        // loop was innocent while the one thing that blocks it for tens of
        // milliseconds went unmeasured.
        // The pool, because a queue only means something next to the number
        // of things draining it. Workers are spawned on contention rather
        // than up front, so "configured" is a ceiling and not a count.
        if let Some(render) = self.supervisor.as_ref().map(|s| s.diagnostics()) {
            report.push_str(&format!(
                "- renderer: {} of {} workers up, {} queued here, {} in worker hands\n",
                render.workers_alive, render.workers_configured, render.queued, render.in_flight,
            ));
        }
        let upload = self.upload_meter.get();
        if upload.calls > 0 {
            report.push_str(&format!(
                "- on the event loop, uploading pictures to the GPU: {} uploads, {} typical, {} worst\n",
                upload.calls,
                millis(upload.mean()),
                millis(upload.worst),
            ));
        }
        let copies = self.latency.copies();
        if copies.frames > 0 {
            report.push_str(&format!(
                "- {} large frames copied out of shared memory on this thread, {:.0} MiB in total\n",
                copies.frames,
                copies.bytes as f64 / 1_048_576.0,
            ));
        }
        report
    }

    /// The whole report: what the session is, then every summary, then the
    /// event log at the back.
    ///
    /// Order is the whole usability of this thing. It is read through a box a
    /// few lines tall, and the event log gains a line per page turn, so with
    /// the log in the middle — where it used to be — a presenter who had
    /// turned fifty pages had fifty lines between them and the numbers.
    fn build_diagnostics_report(&self) -> String {
        let mut report = self.diagnostics.to_report_with_backend(&self.pdf_backend());
        report.push_str("\n## Session inhibition\n");
        report.push_str(&format!("- {}\n", self.inhibitor.state().describe()));
        for attempt in self.inhibitor.state().attempts() {
            report.push_str(&format!("- tried {attempt}\n"));
        }
        report.push_str("\n## Frame cache\n");
        let stats = self.cache.stats();
        // Source bytes only: the cache cannot see the image handles or GPU
        // textures built from its pixels, so it does not pretend to.
        report.push_str(&format!(
            "- {} frames, {:.1} MiB source bytes, budget {:.0} MiB, {} evictions\n",
            stats.frames,
            stats.cpu_bytes as f64 / 1_048_576.0,
            self.cache.budget_bytes() as f64 / 1_048_576.0,
            stats.evictions,
        ));
        // Hits and misses are deliberately absent. They are counted only by
        // `FrameCache::get`, which the application never calls — every
        // lookup goes through `best_exact`, `best_fitting` or `contains` —
        // so the pair could only ever print "0 hits, 0 misses", and the
        // specification's own rule is that a permanently-zero figure is
        // worse than its absence.
        report.push_str(&format!(
            "- {} rejected as larger than the budget\n",
            stats.rejected,
        ));
        if stats.pinned_overcommit_bytes > 0 {
            report.push_str(&format!(
                "- {:.1} MiB over budget because every remaining frame is pinned on screen\n",
                stats.pinned_overcommit_bytes as f64 / 1_048_576.0,
            ));
        }
        report.push_str(&self.latency_report());
        if let Some(media) = self.media_supervisor.as_ref() {
            let counters = media.worker_counters();
            report.push_str("\n## Media pipeline\n");
            report.push_str(&format!(
                "- {} sessions, {:.1} MiB of surface rings\n",
                media.session_count(),
                media.ring_bytes() as f64 / 1_048_576.0,
            ));
            report.push_str(&format!(
                "- worker: {} frames from the browser, {} discarded before decode, {} decoded ({} scaled, {} written as-is), {} published, {} dropped at the ring\n",
                counters.cdp_frames_received,
                counters.frames_discarded_before_decode,
                counters.frames_decoded,
                counters.frames_scaled,
                counters.frames_scale_elided,
                counters.frames_published,
                counters.ring_dropped,
            ));
            report.push_str(&format!(
                "- application: {} frames forwarded, {} coalesced before copying, {} image handles built\n",
                media.frames_forwarded(),
                media.frames_coalesced(),
                self.media.handles_created(),
            ));
        }
        report.push_str(&format!(
            "\n## Layout\n- active: {} ({})\n",
            self.active_layout.name,
            self.active_layout.design_ratio.label()
        ));
        // Last, deliberately: it is the longest section and the least often
        // the answer.
        report.push_str(&self.diagnostics.events_report());
        report
    }

    fn dispatch(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick(now) => self.on_tick(now),
            Message::RenderReady => {
                self.pump_renderer();
                Task::none()
            }
            Message::Key {
                key,
                text,
                scancode,
                shift,
                control,
                alt,
            } => {
                if self.annotations.is_typing() {
                    match key.as_deref() {
                        Some(key) if control && key.eq_ignore_ascii_case("v") => {
                            let Some(target) = self
                                .annotations
                                .typing_index()
                                .and_then(|index| self.annotations.texts.get(index))
                                .map(|mark| mark.id)
                            else {
                                return Task::none();
                            };
                            return iced::clipboard::read()
                                .map(move |value| Message::PasteAnnotationText { target, value });
                        }
                        Some("Enter") if control => {
                            self.annotations.type_text("\n");
                        }
                        Some("Enter") => {
                            self.annotations.finish_text();
                        }
                        Some("Escape") => {
                            self.annotations.finish_text();
                        }
                        Some("Backspace") => {
                            self.annotations.backspace_text();
                        }
                        _ if !control => {
                            if let Some(value) = text.as_deref() {
                                self.annotations.type_text(value);
                            }
                        }
                        _ => {}
                    }
                    return Task::none();
                }
                // A mark being written owns the keyboard while the caret is in
                // it: the box on the page takes the typing itself, and every
                // key that reached the bindings from here would be a document
                // shortcut fired by someone writing a word — "n" for the next
                // page, Home for the first one. Escape is the way out, and
                // cancelling is not a mutation (§8.5).
                if self.composing_mark.is_some() {
                    if key.as_deref() == Some("Escape") {
                        self.composing_mark = None;
                    }
                    return Task::none();
                }
                if key.as_deref() == Some("Escape") && self.confirm_reset_colors {
                    self.confirm_reset_colors = false;
                    return Task::none();
                }
                // A cue going off is acknowledged by Escape as well as by the
                // clock: hands are not always on the mouse. Dismissing comes
                // before closing the popup, since the marker is the thing
                // demanding attention.
                if key.as_deref() == Some("Escape") && self.alarm_controls.ringing.is_some() {
                    self.alarm_controls.dismiss();
                    return Task::none();
                }
                // And the timer's overrun, which pulses for the same reason and
                // is answered the same way.
                if key.as_deref() == Some("Escape") && self.timer_controls.overtime_since.is_some()
                {
                    self.timer_controls.dismiss_overtime();
                    return Task::none();
                }
                if key.as_deref() == Some("Escape") && self.alarm_controls.open {
                    self.alarm_controls.open = false;
                    return Task::none();
                }
                if key.as_deref() == Some("Escape") && self.timer_controls.open {
                    self.timer_controls.open = false;
                    return Task::none();
                }
                // The editor owns the keyboard while it is open: presenter
                // shortcuts must not blank the audience while someone is
                // typing a layout name.
                // Escape backs out of whatever is open, before any binding is
                // consulted: it is the one key everyone tries first.
                // An open annotation panel is the innermost thing on screen,
                // so it is what Escape closes first — and closing it must not
                // also cancel the preview behind it.
                if key.as_deref() == Some("Escape")
                    && (self.annotation_controls.overflow
                        || self.annotation_controls.open.is_some())
                {
                    self.annotation_controls.overflow = false;
                    self.annotation_controls.open = None;
                    return Task::none();
                }
                if key.as_deref() == Some("Escape")
                    && (self.menu_open
                        || self.audience_start_menu_open
                        || self.unbound_key.is_some()
                        || self.overview)
                {
                    self.menu_open = false;
                    self.audience_start_menu_open = false;
                    self.unbound_key = None;
                    // Backing out of the overview returns to the slide that
                    // was showing, without committing a jump: the grid moves
                    // the preview, so abandoning it is what undoes the look
                    // around.
                    let was_overview = self.overview;
                    self.overview = false;
                    if was_overview {
                        return self.update(Message::Nav(Nav::CancelPreview));
                    }
                    return Task::none();
                }
                if back_to_presenter_key(self.page, key.as_deref(), self.layout_dialog.is_some()) {
                    return self.update(Message::ShowPresenter);
                }
                if self.page != crate::designer::Page::Presenter {
                    return self.editor_key(key, shift, control);
                }
                // The overview is a grid, so while it is open the arrow keys
                // move about a grid: left and right along a row, up and down
                // between rows. Anything else — including the key that opened
                // it — still means what it always means.
                if self.overview {
                    if let Some(task) = self.overview_key(key.as_deref()) {
                        return task;
                    }
                }
                // A focused overlay may take the key — but never a global
                // shortcut, and never Escape, which is always the way out.
                if self.input_router.focused().is_some() {
                    if let Some(name) = key.as_deref() {
                        let routed = self.input_router.key_pressed(name, None);
                        match routed {
                            crate::media::Routed::ToOverlay { .. } => {
                                self.deliver(routed);
                                return Task::none();
                            }
                            // Escape has already given the focus back inside
                            // the router; nothing else needs to happen.
                            crate::media::Routed::ReleaseFocus => return Task::none(),
                            // A global shortcut falls through to the keymap
                            // below, focus or no focus.
                            _ => {}
                        }
                    }
                }
                // Reading a document, Page Down means the next screenful of
                // this document — not the next slide. The presenter's
                // bindings are unchanged and mean what they always did the
                // moment the reader is closed again.
                if let Some(task) = self.document_key(key.as_deref(), control, shift) {
                    return task;
                }
                match self.settings.keymap.resolve_with_mods(
                    key.as_deref(),
                    crate::settings::Mods::new(control, shift, alt),
                    scancode,
                ) {
                    Some(action) => self.update(Message::Do(action)),
                    None => {
                        // An unbound press is the raw-scancode fallback path:
                        // record it and offer it for binding in the presenter
                        // window, so a remote whose keys the toolkit cannot
                        // name is still usable without editing a config file.
                        if let Some(code) = scancode.filter(|_| offers_binding(key.as_deref())) {
                            let name = key.clone().unwrap_or_else(|| "unidentified".into());
                            self.diagnostics
                                .note(format!("unbound key: {name} (scancode {code})"));
                            self.unbound_key = Some((key, code));
                        }
                        Task::none()
                    }
                }
            }
            Message::PasteAnnotationText { target, value } => {
                let still_editing_target = self
                    .annotations
                    .typing_index()
                    .and_then(|index| self.annotations.texts.get(index))
                    .is_some_and(|mark| mark.id == target);
                if let Some(value) = value.filter(|_| still_editing_target) {
                    self.annotations.type_text(&value);
                }
                Task::none()
            }
            Message::Do(action) => self.on_action(action),
            Message::Nav(command) => {
                // A scrub is a drag on the slider, and `PreviewGoTo` is the
                // only thing that carries one. Every other navigation — a
                // key, a button, the overview — means the drag is over or
                // never happened, so the thumbnail is not left behind by
                // some path nobody thought of.
                self.scrubbing = matches!(command, Nav::PreviewGoTo(_));
                let changed = self.state.apply(command, self.now);
                if changed.committed {
                    // The clock starts here: the state has moved, and every
                    // millisecond after this is one the presenter is waiting.
                    // The wall clock, not `self.now`, which is as old as the
                    // last tick — the very error this exists to find.
                    self.latency
                        .begin_turn(self.state.committed(), Instant::now());
                    // The marks on the slide being left are in the document,
                    // so there is nothing to stash. What the slide arriving
                    // carries is asked for here and adopted when the engine
                    // answers; until then the overlay is empty, which is the
                    // right way round — a stale mark drawn over new content is
                    // the failure worth avoiding, and a mark that appears a
                    // frame late is not. A link highlight indexes into this
                    // page's link list, so it goes rather than pointing at the
                    // wrong rectangle on the next one.
                    self.annotations.clear_on_slide_change();
                    self.request_marks_for_this_slide();
                    self.focused_link = None;
                    self.hovered_link = None;
                    // Media follows the committed page here, on the page
                    // turn itself: overlay events only fire the first time a
                    // page's declarations are fetched, so navigation between
                    // known pages would otherwise leave the previous slide's
                    // media running and the new slide's suspended.
                    let start = Instant::now();
                    self.service_media();
                    self.latency
                        .record_stage(|stages| &mut stages.service_media, start.elapsed());
                }
                if changed.any() {
                    // The log, not the bundle. Ordinary navigation is the one
                    // event a talk produces hundreds of, and noting each one
                    // pushed every genuine event — a placement refusal, a
                    // reload, a worker crash — out of a four-hundred-entry
                    // ring before the interesting part of a long talk.
                    tracing::debug!(
                        committed = self.state.committed() + 1,
                        preview = self.state.preview() + 1,
                        "navigated"
                    );
                }
                let start = Instant::now();
                self.request_renders();
                self.latency
                    .record_stage(|stages| &mut stages.plan_renders, start.elapsed());
                Task::none()
            }
            Message::OpenDialog => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .pick_file()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                Message::Opened,
            ),
            Message::Opened(Some(path)) => self.open_document(path),
            Message::Opened(None) => Task::none(),
            Message::WindowOpened { role, id } => {
                match role {
                    Role::Presenter => self.presenter_window = Some(id),
                    Role::Audience => self.audience_window = Some(id),
                }
                let window = self.coordinator.window_state_mut(role);
                window.visible = role == Role::Presenter;
                window.mode = if role == Role::Presenter {
                    WindowMode::Windowed
                } else {
                    WindowMode::Hidden
                };
                if role == Role::Audience {
                    self.schedule_presenter_refocus();
                }
                let native =
                    display::native_window_id(id, move |native| Message::NativeId { role, native });
                match role {
                    // The first panel renders are asked for before any resize
                    // arrives, so the pixel ratio has to be known by then.
                    Role::Presenter => Task::batch([
                        window::scale_factor(id).map(Message::PresenterScale),
                        native,
                    ]),
                    Role::Audience => native,
                }
            }
            Message::NativeId { role, native } => {
                self.coordinator.set_native(role, native);
                self.reconcile()
            }
            Message::WindowClosed(id) => {
                if Some(id) == self.presenter_window {
                    // Closing the presenter closes the show.
                    return self.quit();
                }
                if Some(id) == self.audience_window {
                    self.audience_window = None;
                    self.audience_started = false;
                    self.presenter_refocus_deadlines.clear();
                    self.coordinator.set_native(Role::Audience, None);
                    *self.coordinator.window_state_mut(Role::Audience) = WindowState::default();
                    self.coordinator
                        .reconciler
                        .note_windows(&self.coordinator.windows);
                    self.inhibitor.release(self.platform.services.as_ref());
                }
                Task::none()
            }
            Message::Resized { id, size } => {
                if Some(id) == self.audience_window {
                    self.audience_size = size;
                } else {
                    self.presenter_size = size;
                    self.preview_size = Size::new(size.width * 0.45, size.height * 0.45);
                    // A resize is also how a window arrives on a display with
                    // a different pixel ratio, so the ratio is re-read here
                    // rather than only at startup.
                    let scale = self.presenter_scale_task();
                    // The editor canvas is the middle column of the page;
                    // divider drags are expressed as fractions of it.
                    if self.designer.is_some() {
                        let canvas = Size::new(
                            (size.width - 560.0).max(200.0),
                            (size.height - 120.0).max(200.0),
                        );
                        let designer = self.update(Message::Designer(
                            crate::designer::Msg::CanvasResized(canvas),
                        ));
                        return Task::batch([scale, designer]);
                    }
                    self.request_renders();
                    return scale;
                }
                self.request_renders();
                Task::none()
            }
            Message::PresenterScale(scale) => {
                if (self.presenter_scale - scale).abs() > f32::EPSILON {
                    tracing::debug!(scale, "presenter pixel ratio");
                    self.presenter_scale = scale;
                    // Every panel frame is now the wrong size by definition:
                    // ask for the right ones rather than upscale for ever.
                    self.request_renders();
                }
                Task::none()
            }
            Message::SetMapping(mapping) => {
                self.state
                    .apply(Nav::SetNotesMapping(mapping.clone()), self.now);
                if let Some(document) = self.state.document() {
                    self.settings
                        .remember_mapping(document.path.clone(), mapping);
                    self.persist();
                }
                self.invalidate_renders();
                Task::none()
            }
            Message::BindUnboundKey(action) => {
                if let Some((name, code)) = self.unbound_key.take() {
                    // Prefer the logical name when the toolkit produced one;
                    // fall back to the raw scancode otherwise.
                    let binding = match name {
                        Some(name) if name != "unidentified" => KeyBinding::named(&name),
                        _ => KeyBinding::scancode(code),
                    };
                    self.notify(format!(
                        "bound {} to {}",
                        binding.describe(),
                        action.label()
                    ));
                    self.settings.keymap.bind(binding, action);
                    self.persist();
                }
                Task::none()
            }
            Message::ForgetUnboundKey => {
                self.unbound_key = None;
                Task::none()
            }
            Message::SlideCursor { x, y } => {
                self.slide_cursor = Some((x, y));
                self.hovered_link = self.link_under_cursor();
                self.track_annotation((x, y));
                // Interactive overlays follow the pointer, including being
                // told when it leaves — otherwise a hover state inside the
                // page sticks after the pointer has gone.
                let over = self.overlay_under_cursor();
                for routed in self.input_router.pointer_moved(over) {
                    self.deliver(routed);
                }
                Task::none()
            }
            Message::SlidePressed => {
                // An armed tool takes the press; nothing else about the
                // slide panel changes, so a disarmed palette leaves links
                // and media overlays exactly as they were.
                if self.begin_annotation() {
                    return Task::none();
                }
                // An interactive overlay drawn over a link is meant to be
                // clicked, so it takes the press before the link does.
                if self.press_overlay() {
                    return Task::none();
                }
                self.follow_link()
            }
            // Saving is the one palette command that leaves the process, so
            // it answers with a task rather than a state change.
            Message::Annotate(crate::widgets::event::AnnotationCommand::Save) => {
                // The palette’s save is the document’s Save As. There is no
                // separate "annotated copy" any more: the marks *are* the
                // document’s own annotations, so saving the document saves
                // them (criterion 7).
                self.ask_where_to_save_document()
            }
            Message::Annotate(command) => {
                self.on_annotation_command(command);
                Task::none()
            }
            Message::Read(command) => self.on_read_command(command),
            Message::Find(command) => self.on_find_command(command),
            Message::RestoreReaderEdits => self.restore_reader_edits(),
            Message::DiscardReaderEdits => self.discard_reader_edits(),
            Message::SaveDocumentTo(Some(path)) => self.save_document_to(path),
            Message::SaveDocumentTo(None) => Task::none(),
            Message::Alarm(command) => self.on_alarm_command(command),
            Message::Timer(command) => self.on_timer_command(command),
            Message::Transport(request) => {
                self.on_transport_request(request);
                Task::none()
            }
            Message::PointerReleased => {
                // The thumbnail belongs to the drag and nothing else: the
                // moment the button is up it goes, whether the slider took
                // the release or not.
                self.scrubbing = false;
                // A stroke ends where the button came up, wherever that was:
                // releasing outside the panel must not leave the pen down.
                // The pen coming up is also what turns the gesture into an
                // annotation in the open document (§14.3 step 4) — the stroke
                // that was drawn and the marks an eraser sweep took, in one
                // transaction, because they are one thing the presenter did.
                // A highlighter sweep is not a stroke and does not end like
                // one: the release asks the engine once more, and *that*
                // answer is what commits (§7.2).
                if self.presenter_interaction.pending_selection().is_some() {
                    self.ask_presenter_selection(true);
                } else {
                    let finished = self.annotations.end_stroke();
                    self.commit_presenter_gesture(finished);
                }
                // The same is true of a press inside a browser: a mouseup it
                // never hears leaves the page dragging for ever.
                let over = self.overlay_under_cursor();
                let routed = self
                    .input_router
                    .pointer_released(over, pulpit_media::PointerButton::Left);
                self.deliver(routed);
                Task::none()
            }
            Message::OverviewScrolled(offset) => {
                let now = Instant::now();
                // A scroll the keyboard asked for wins over one the hand did
                // not: while the claim stands, only the offset it asked for
                // is believed, and the claim ends the moment that offset
                // arrives.
                if let Some((target, deadline)) = self.overview_scroll_claim {
                    if now < deadline {
                        if (offset - target).abs() <= 0.5 {
                            self.overview_scroll_claim = None;
                            self.overview_scroll = offset;
                        }
                        return Task::none();
                    }
                    self.overview_scroll_claim = None;
                }
                self.overview_scroll = offset;
                self.overview_settling = Some(now);
                // Scrolling moves what warming should be working outwards
                // from. Re-planning here rather than on the next tick is what
                // makes a fast scroll into an unwarmed part of a long deck
                // fill under the eye instead of behind it; the plan is
                // memoised on its inputs, so a scroll that stays within one
                // row of the grid costs a comparison.
                self.plan_thumbnails();
                self.pump_thumbnails();
                Task::none()
            }
            Message::ToggleOverview => {
                self.overview = !self.overview;
                // It can be reached from the menu, which must not stay open
                // over the grid it just opened.
                self.menu_open = false;
                // Opening it is what asks for the thumbnails: rendering the
                // whole deck on the off-chance would be work nobody asked
                // for, on every document.
                self.request_renders();
                // Opening or closing the grid changes both the priority the
                // remaining thumbnails go out at and where warming works
                // outwards from, so neither waits for the next tick.
                self.plan_thumbnails();
                self.pump_thumbnails();
                if self.overview {
                    // Open on the slide the presenter is on, however far down
                    // a long deck that is — and in a document layout that is
                    // the page the reader is showing, not the session index.
                    let slide = if crate::layout::builtin::LayoutMode::of(&self.active_layout)
                        == crate::layout::builtin::LayoutMode::Document
                    {
                        let slide = self.slide_showing(self.reader.controls().page.get());
                        // The grid's cursor and accent read the preview slide,
                        // so it is seeded here rather than left where the last
                        // presentation left it.
                        let _ = self.state.apply(Nav::PreviewGoTo(slide), self.now);
                        slide
                    } else {
                        self.state.preview()
                    };
                    return self.reveal_in_overview(slide);
                }
                Task::none()
            }
            Message::GoToFromOverview(slide) => {
                self.overview = false;
                // In a document layout the grid is a way of moving the reader,
                // not of showing a slide to a room: the session index would
                // change with nothing on screen following it.
                if crate::layout::builtin::LayoutMode::of(&self.active_layout)
                    == crate::layout::builtin::LayoutMode::Document
                {
                    let page = self.page_showing(slide);
                    return self
                        .on_read_command(crate::widgets::event::ReadCommand::GoToPage(page));
                }
                self.update(Message::Nav(Nav::GoTo(slide)))
            }
            Message::Ignore => Task::none(),
            Message::ShowPresenter => {
                self.page = crate::designer::Page::Presenter;
                Task::none()
            }
            Message::ShowLibrary => {
                self.page = crate::designer::Page::Library;
                self.designer = None;
                Task::none()
            }
            Message::NewLayout => {
                self.designer = Some(crate::designer::Designer::create("Untitled layout"));
                self.page = crate::designer::Page::Editor;
                Task::none()
            }
            Message::UseLayout(id) => {
                if let Some(layout) = self.layouts.get(&id).cloned() {
                    self.adopt_layout(layout);
                    // Switching layouts never touches the audience output.
                    self.page = crate::designer::Page::Presenter;
                }
                Task::none()
            }
            Message::EditLayout(id) | Message::PreviewLayout(id) => self.open_editor(id),
            Message::DuplicateLayout(id) => {
                match self.layouts.duplicate(&id) {
                    Ok(new_id) => {
                        if let Some(layout) = self.layouts.get(&new_id).cloned() {
                            let mut designer = crate::designer::Designer::open(layout);
                            designer.revalidate();
                            self.designer = Some(designer);
                            self.page = crate::designer::Page::Editor;
                        }
                    }
                    Err(e) => self.notify(format!("Could not duplicate: {e}")),
                }
                Task::none()
            }
            Message::DeleteLayout(id) => {
                let name = self
                    .layouts
                    .get(&id)
                    .map(|layout| layout.name.clone())
                    .unwrap_or_default();
                self.layout_dialog = Some(LayoutDialog::ConfirmDelete { id, name });
                Task::none()
            }
            Message::CancelLayoutDialog => {
                self.layout_dialog = None;
                Task::none()
            }
            Message::ConfirmLayoutDialog => {
                match self.layout_dialog.take() {
                    Some(LayoutDialog::ConfirmDelete { id, .. }) => {
                        match self.layouts.delete(&id) {
                            Ok(()) => {
                                if self.active_layout.id == id {
                                    // The active layout was deleted: fall back
                                    // to a built-in rather than to nothing.
                                    if let Some(layout) = self.layouts.built_in().first().cloned() {
                                        self.adopt_layout(layout);
                                    }
                                }
                            }
                            Err(e) => self.notify(format!("Could not delete: {e}")),
                        }
                    }
                    None => {}
                }
                Task::none()
            }
            Message::MenuAction(message) => {
                self.menu_open = false;
                self.audience_start_menu_open = false;
                self.dispatch(*message)
            }
            Message::ToggleMenu => {
                self.menu_open = !self.menu_open;
                self.audience_start_menu_open = false;
                Task::none()
            }
            Message::CloseMenu => {
                self.menu_open = false;
                self.audience_start_menu_open = false;
                Task::none()
            }
            Message::StartAudience => self.start_audience(false),
            Message::StartAudienceOnDisplay { monitor } => {
                if let Some(monitor) = self.coordinator.snapshot.get(monitor) {
                    self.coordinator.roles.set_target(
                        Role::Audience,
                        RoleTarget::Monitor(Box::new(monitor.record())),
                    );
                    self.settings.display.roles = self.coordinator.roles.clone();
                    self.persist();
                    self.start_audience(false)
                } else {
                    self.notify("That display is no longer connected.".into());
                    self.reconcile()
                }
            }
            Message::StartAudienceAutomatic => {
                self.coordinator
                    .roles
                    .set_target(Role::Audience, RoleTarget::Auto);
                self.settings.display.roles = self.coordinator.roles.clone();
                self.persist();
                self.start_audience(false)
            }
            Message::StartAudienceWindowed => self.start_audience(true),
            Message::StopAudience => self.stop_audience(),
            Message::ToggleAudienceStartMenu => {
                if !self.audience_started {
                    self.audience_start_menu_open = !self.audience_start_menu_open;
                    self.menu_open = false;
                }
                Task::none()
            }
            Message::MediaProbed(probes) => {
                if let Some(media) = self.media_supervisor.as_mut() {
                    for probe in probes {
                        media.record_probe(probe);
                    }
                }
                // A deck opened before the probes landed has overlays
                // waiting: service them now that a runtime can be selected.
                self.service_media();
                Task::none()
            }
            Message::ShowSettings => {
                self.menu_open = false;
                self.page = crate::designer::Page::Settings;
                Task::none()
            }
            Message::SetAppearance(appearance) => {
                self.settings.appearance.appearance = appearance;
                self.apply_appearance();
                self.persist();
                Task::none()
            }
            Message::SetBlankColor(color) => {
                self.settings.display.blank_color = color;
                // A blank already on screen follows the new choice at once,
                // so the setting can be checked against the room without
                // toggling twice.
                if self.state.blank().is_blanked() {
                    self.state.apply(Nav::SetBlank(color.blank()), self.now);
                }
                self.persist();
                Task::none()
            }
            Message::KeyReleased(key) => {
                if self.input_router.focused().is_some() {
                    let routed = self.input_router.key_released(&key);
                    self.deliver(routed);
                }
                Task::none()
            }
            Message::Wheel { x, y } => {
                // A wheel over an interactive overlay scrolls the page, not
                // the deck; anywhere else pulpit keeps its own behaviour.
                let over = self.overlay_under_cursor();
                let routed = self.input_router.wheel(over, x, y);
                self.deliver(routed);
                Task::none()
            }
            Message::SetMotion(setting) => {
                self.settings.appearance.motion = setting;
                self.apply_appearance();
                self.persist();
                Task::none()
            }
            Message::EditColorScheme(scheme) => {
                self.editing_colors = scheme;
                Task::none()
            }
            Message::OpenColorPicker(role) => {
                self.color_picker_open = role;
                Task::none()
            }
            Message::SetColor(role, value) => {
                let key = (self.editing_colors, role);
                // A colour arriving from the wheel is a colour chosen; the
                // wheel has done its job and gets out of the way.
                if self.color_picker_open == Some(role) {
                    self.color_picker_open = None;
                }
                if crate::settings::parse_hex_color(&value).is_some() {
                    self.color_drafts.remove(&key);
                    self.settings
                        .appearance
                        .colors
                        .set(self.editing_colors, role, value);
                    self.apply_appearance();
                    self.persist();
                } else {
                    self.color_drafts.insert(key, value);
                }
                Task::none()
            }
            Message::AskResetColors => {
                if self.settings.appearance.colors.has_overrides() || !self.color_drafts.is_empty()
                {
                    self.confirm_reset_colors = true;
                }
                Task::none()
            }
            Message::CancelResetColors => {
                self.confirm_reset_colors = false;
                Task::none()
            }
            Message::ResetColors => {
                self.settings.appearance.colors.reset();
                self.color_drafts.clear();
                self.confirm_reset_colors = false;
                self.apply_appearance();
                self.persist();
                Task::none()
            }
            Message::RestoreSession => self.restore_session(),
            Message::DiscardSession => {
                // Declining is final: the snapshot goes, so the offer cannot
                // reappear on the next start.
                self.pending_restore = None;
                self.session.clear();
                Task::none()
            }
            Message::DismissToast(id) => {
                self.toasts.dismiss(id);
                Task::none()
            }
            Message::DismissAllToasts => {
                self.toasts.dismiss_all();
                Task::none()
            }
            Message::CopyDiagnostics => {
                // The very report the page is showing. It used to copy the
                // display bundle alone, so everything the application adds to
                // it — the frame cache, the page turns, the media pipeline,
                // the active layout — was on screen and absent from the
                // clipboard, and a pasted "full" report stopped without
                // saying that it had.
                let report = self.diagnostics_report();
                self.notify_done("Diagnostics copied.".to_string());
                iced::clipboard::write(report)
            }
            Message::RevealDocument => {
                self.menu_open = false;
                let Some(path) = self.state.document().map(|document| document.path.clone()) else {
                    self.notify("No document is open.".to_string());
                    return Task::none();
                };
                let outcome = self.platform.services.reveal(&path);
                if let Some(problem) = outcome.describe() {
                    self.notify(problem);
                }
                Task::none()
            }
            Message::Designer(designer_message) => {
                let Some(designer) = self.designer.as_mut() else {
                    return Task::none();
                };
                match designer.handle(designer_message, &mut self.layouts) {
                    crate::designer::Effect::None => {}
                    crate::designer::Effect::Back => {
                        self.designer = None;
                        self.page = crate::designer::Page::Library;
                    }
                    crate::designer::Effect::Saved(id) => {
                        // A saved layout that is currently in use updates the
                        // presenter screen immediately.
                        if self.active_layout.id == id {
                            if let Some(layout) = self.layouts.get(&id).cloned() {
                                self.active_layout = layout;
                                self.annotation_controls = crate::widgets::AnnotationControls::new(
                                    annotation_options_in(&self.active_layout),
                                );
                            }
                        }
                    }
                    crate::designer::Effect::Duplicate => {
                        let id = designer.layout().id.clone();
                        return self.update(Message::DuplicateLayout(id));
                    }
                }
                Task::none()
            }
        }
    }

    /// Open a layout in the editor, optionally starting in preview mode.
    fn open_editor(&mut self, id: LayoutId) -> Task<Message> {
        let Some(layout) = self.layouts.get(&id).cloned() else {
            return Task::none();
        };
        let mut designer = crate::designer::Designer::open(layout);
        designer.canvas_ratio = self.suggested_ratio(designer.canvas_ratio);
        designer.revalidate();
        self.designer = Some(designer);
        self.page = crate::designer::Page::Editor;
        Task::none()
    }

    /// Make a layout the active presenter layout and remember it.
    ///
    /// If a presentation is live the presenter screen switches immediately;
    /// the audience display is not touched, because the layout describes the
    /// presenter screen alone.
    fn adopt_layout(&mut self, layout: Layout) {
        self.diagnostics
            .note(format!("presenter layout: {}", layout.name));
        // Each mode remembers its own (§2.3): choosing a presenter variant
        // must never change what a PDF opens into, and the reverse.
        match crate::layout::builtin::LayoutMode::of(&layout) {
            crate::layout::builtin::LayoutMode::Presentation => {
                self.settings.layout.active = Some(layout.id.0.clone());
            }
            crate::layout::builtin::LayoutMode::Document => {
                self.settings.layout.active_document = Some(layout.id.0.clone());
            }
        }
        self.active_layout = layout;
        self.annotation_controls =
            crate::widgets::AnnotationControls::new(annotation_options_in(&self.active_layout));
        self.persist();
        self.check_layout_ratio();
    }

    /// The ratio of the presenter's own display, when one is known.
    fn screen_ratio(&self) -> Option<AspectRatio> {
        let monitor = self
            .coordinator
            .snapshot
            .monitors
            .iter()
            .find(|monitor| {
                self.coordinator
                    .windows
                    .presenter
                    .monitor
                    .as_ref()
                    .is_some_and(|identity| &monitor.identity == identity)
            })
            .or_else(|| self.coordinator.snapshot.monitors.first())?;
        Some(AspectRatio::Detected {
            width: monitor.geometry.width,
            height: monitor.geometry.height,
        })
    }

    /// Prefer the real screen's ratio in the editor when we know it.
    fn suggested_ratio(&self, fallback: AspectRatio) -> AspectRatio {
        self.screen_ratio().unwrap_or(fallback)
    }

    /// A layout designed at a very different ratio deserves one dismissible
    /// notice — not silent reflow, and not a nag.
    fn check_layout_ratio(&mut self) {
        if self.settings.layout.ratio_notice_dismissed {
            return;
        }
        let Some(screen) = self.screen_ratio() else {
            return;
        };
        if self
            .active_layout
            .design_ratio
            .differs_substantially_from(screen)
        {
            // One notice, in the corner, and never again once it has been
            // shown: a layout at an unusual ratio is worth mentioning once
            // and is not worth nagging about.
            self.settings.layout.ratio_notice_dismissed = true;
            self.persist();
            let message = format!(
                "“{}” was designed at {} and this screen is {}.",
                self.active_layout.name,
                self.active_layout.design_ratio.label(),
                screen.label()
            );
            self.diagnostics.note(message.clone());
            self.toasts.push(
                Intent::Warning,
                message,
                Some(
                    "It still fills the window. Open it in the layout editor at this ratio if \
                     it looks wrong."
                        .into(),
                ),
                self.now,
            );
        }
    }

    /// The rendering context the presenter view and the editor preview
    /// share, assembled from application state into the narrow facets a
    /// widget is allowed to see.
    pub fn render_context<'a>(
        &'a self,
        mode: crate::widgets::Mode,
        frames: &'a dyn crate::widgets::context::FrameSource,
        sample_notes: &'a str,
    ) -> crate::widgets::Context<'a> {
        let live = mode == crate::widgets::Mode::Live;
        crate::widgets::Context {
            mode,
            slides: crate::widgets::context::SlideData {
                current: if live {
                    self.state.committed()
                } else {
                    crate::widgets::sample::SLIDE
                },
                preview: if live {
                    self.state.preview()
                } else {
                    crate::widgets::sample::SLIDE
                },
                count: if live {
                    self.state.slide_count()
                } else {
                    crate::widgets::sample::SLIDE_COUNT
                },
                frames,
                preview_width: self.preview_size.width.max(160.0) as u32,
                aspect: self.current_slide_aspect(live),
                text_notes: self.state.document().filter(|_| live).and_then(|document| {
                    document.text_notes.as_ref().map(|notes| {
                        crate::widgets::context::TextNotesData {
                            notes,
                            mapping: self.state.mapping(),
                            pdf_pages: document.pdf_pages,
                        }
                    })
                }),
                has_links: live && self.current_slide_has_links(),
                link_highlights: if live {
                    self.link_highlights()
                } else {
                    Vec::new()
                },
                overlays: if live {
                    self.current_slide_overlays()
                } else {
                    Vec::new()
                },
                crop: self
                    .state
                    .audience_source()
                    .map(|source| source.region)
                    .unwrap_or(pulpit_core::notes::Region::FULL),
                annotations: if live {
                    &self.annotations_view
                } else {
                    &crate::widgets::sample::ANNOTATIONS
                },
                rendered_text: if live {
                    self.typst_annotations.snapshot()
                } else {
                    static EMPTY: std::sync::LazyLock<
                        std::sync::Arc<
                            std::collections::HashMap<u64, crate::typst_annotation::RenderedText>,
                        >,
                    > = std::sync::LazyLock::new(|| {
                        std::sync::Arc::new(std::collections::HashMap::new())
                    });
                    &EMPTY
                },
                marks_cache: if live {
                    std::rc::Rc::clone(&self.marks_caches.live)
                } else {
                    std::rc::Rc::clone(&self.marks_caches.sample)
                },
                annotation_controls: if live {
                    crate::widgets::AnnotationControls {
                        can_save: self.reader.is_open() && self.reader.is_dirty(),
                        can_undo: self.reader.can_undo(),
                        can_redo: self.reader.can_redo(),
                        ..self.annotation_controls
                    }
                } else {
                    crate::widgets::AnnotationControls::default()
                },
                annotation_style: self.annotation_options().style(),
            },
            timing: crate::widgets::context::TimingData {
                elapsed: self.state.timer().elapsed(self.now),
                target: self.state.timer().target,
                running: self.state.timer().is_running(),
                seconds_of_day: crate::view::seconds_of_day(),
            },
            // The editor shows the clock with no cues set, so a layout is
            // arranged against the widget's resting state rather than
            // against whatever this talk happens to have pending.
            alarms: if live {
                &self.alarm_controls
            } else {
                &crate::widgets::sample::ALARMS
            },
            timer_controls: if live {
                &self.timer_controls
            } else {
                &crate::widgets::sample::TIMER
            },
            document: crate::widgets::context::DocumentData {
                title: if live {
                    self.document_title()
                } else {
                    crate::widgets::sample::TITLE.to_string()
                },
                section: if live {
                    self.current_section()
                } else {
                    Some("Reconnection".to_string())
                },
                sample_notes,
            },
            // The reader's facet. A presentation has no document open in the
            // reader's sense, so its widgets say so rather than drawing a
            // sample page that would be mistaken for the user's own (§2).
            reader: {
                let mut reader = self.reader.facet(
                    live,
                    &|page, width| self.reader_frame(page, width),
                    &self.search,
                );
                // The half-written mark belongs to the application, and the
                // page surface is where it is drawn (§8.5).
                if live {
                    reader.composing = self.composing_mark.clone();
                }
                reader
            },
            // One model, whichever layout is mounted: the pane in a presenter
            // layout and the pane in a document layout are the same search.
            search: crate::widgets::context::SearchData {
                state: if live {
                    &self.search
                } else {
                    // The editor judges the pane against a search that found
                    // something; an empty box says nothing about how much
                    // room the results want.
                    &crate::widgets::sample::SEARCH
                },
            },
            audience: crate::widgets::context::AudienceData {
                blank: self.state.blank(),
                connected: self.coordinator.snapshot.len() > 1,
                fullscreen: self.coordinator.windows.audience.mode
                    == pulpit_display::WindowMode::Fullscreen,
                started: self.audience_started,
                menu_open: self.menu_open,
            },
            media: crate::widgets::context::MediaData {
                // The transport always means the media the *audience* is
                // seeing, never the slide the presenter has previewed ahead
                // to: pressing play must not start a clip nobody is watching.
                transport: if live {
                    crate::widgets::media::model::Transport::for_target(
                        self.media.transport_target(self.state.committed()),
                    )
                } else {
                    crate::widgets::sample::transport()
                },
            },
        }
    }

    /// Drive whatever media the audience is currently seeing.
    fn on_transport_request(&mut self, request: crate::widgets::event::TransportRequest) {
        use crate::media::TransportCommand;
        use crate::widgets::event::TransportRequest;
        let Some(supervisor) = self.media_supervisor.as_mut() else {
            return;
        };
        let Some(target) = self.media.transport_target(self.state.committed()) else {
            return;
        };
        let command = match request {
            TransportRequest::Play => TransportCommand::Play,
            TransportRequest::Pause => TransportCommand::Pause,
            TransportRequest::SeekTo(seconds) => TransportCommand::SeekTo(seconds),
            TransportRequest::SetMuted(muted) => TransportCommand::SetMuted(muted),
        };
        self.media.control(supervisor, target.overlay, command);
    }

    /// The shape of the deck's pages, for laying out a grid of them.
    ///
    /// The same measure the current-slide panel uses, because a thumbnail is
    /// rendered from the same source: cells that match the pictures leave no
    /// letterbox inside them.
    pub fn slide_aspect(&self) -> f32 {
        self.current_slide_aspect(true)
    }

    /// Aspect ratio of what the current-slide panel actually draws: the page
    /// aspect, corrected for the notes-mapping crop.
    fn current_slide_aspect(&self, live: bool) -> f32 {
        let fallback = 16.0 / 9.0;
        if !live {
            return fallback;
        }
        let page = self
            .state
            .first_page_size()
            .map(|size| size.aspect_ratio())
            .unwrap_or(fallback);
        match self.state.audience_source() {
            Some(source) if source.region.height > 0.0 => {
                page * source.region.width / source.region.height
            }
            _ => page,
        }
    }

    /// Does the committed page carry link annotations? Drives the pointer
    /// cursor over the current-slide panel.
    fn current_slide_has_links(&self) -> bool {
        let Some(document) = self.state.document().map(|d| d.id.0) else {
            return false;
        };
        let Some(source) = self.state.audience_source() else {
            return false;
        };
        self.links
            .get(&(document, source.pdf_page))
            .is_some_and(|links| !links.is_empty())
    }

    /// The overlays the *audience* window composites.
    ///
    /// The same frames the presenter sees, from the same sessions: one
    /// authoritative session feeds both windows. What differs is that nothing
    /// here is ever marked interactive, because a focus ring is chrome and
    /// chrome never reaches the audience.
    pub fn audience_overlays(&self) -> Vec<crate::widgets::context::SlideOverlay> {
        // A blanked screen shows nothing at all: an overlay drawn over the
        // blank would undo it.
        if self.state.blank().is_blanked() {
            return Vec::new();
        }
        self.current_slide_overlays()
            .into_iter()
            .map(|overlay| crate::widgets::context::SlideOverlay {
                interactive: false,
                ..overlay
            })
            .collect()
    }

    /// The overlays to composite over the audience page, back to front.
    ///
    /// An overlay with no complete frame yet is simply absent, so the PDF
    /// page — or the poster the author drew inside the link rectangle —
    /// shows through instead of a hole.
    fn current_slide_overlays(&self) -> Vec<crate::widgets::context::SlideOverlay> {
        let Some(source) = self.state.audience_source() else {
            return Vec::new();
        };
        self.media
            .index()
            .on_page(source.pdf_page)
            .into_iter()
            .filter_map(|overlay| {
                let frame = self.media.frame(overlay.id)?;
                Some(crate::widgets::context::SlideOverlay {
                    region: overlay.region,
                    handle: frame.handle.clone(),
                    interactive: overlay.is_interactive(),
                })
            })
            .collect()
    }

    /// Which monitor a role is actually on, once automatic choice and
    /// identity resolution have had their say.
    pub fn resolved_index(&self, role: Role) -> Option<usize> {
        match role {
            Role::Presenter => self.coordinator.resolved.presenter,
            Role::Audience => self.coordinator.resolved.audience,
        }
    }

    /// The key a menu entry should quote for an action, read from the live
    /// keymap rather than written beside the entry. Menu labels used to spell
    /// their own keys, so they went stale the moment a binding moved — and
    /// they never showed a rebinding at all.
    pub fn action_shortcut(&self, action: crate::settings::keys::Action) -> Option<String> {
        use crate::platform::input::Modifier;
        use crate::platform::Shortcut;
        use crate::settings::keys::KeyBinding;

        let binding = self.settings.keymap.display_binding(action)?;
        let KeyBinding::Named { key, mods } = binding else {
            return None;
        };
        let mut modifiers = Vec::new();
        // `ctrl` in a keymap means "the modifier this desktop uses for
        // application commands", which is Command on macOS.
        if mods.ctrl {
            modifiers.push(Modifier::Primary);
        }
        if mods.alt {
            modifiers.push(Modifier::Alt);
        }
        if mods.shift {
            modifiers.push(Modifier::Shift);
        }
        Some(self.platform.input.format(&Shortcut {
            modifiers,
            key: display_key(key),
        }))
    }

    pub fn document_title(&self) -> String {
        self.state
            .document()
            .map(|document| {
                document
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| document.path.display().to_string())
            })
            .unwrap_or_else(|| "No document".to_string())
    }

    /// Re-resolve the palette from the setting and the system preference.
    /// Re-read the desktop's appearance and motion preferences. Two blocking
    /// portal round-trips, so this runs when the desktop may actually have
    /// changed — startup and resume — never on the setting handlers, which
    /// only change pulpit's own preference.
    fn refresh_appearance_probe(&mut self) {
        self.appearance_probe = self.platform.services.system_appearance();
        self.motion_probe = self.platform.services.reduced_motion();
    }

    fn apply_appearance(&mut self) {
        let system = self.appearance_probe;
        let preference = self.settings.appearance.appearance;
        // Motion is settled on the same pass: both come from the desktop's
        // accessibility preferences, and a user who changes one often means
        // the other too.
        self.motion =
            crate::platform::Motion::resolve(self.motion_probe, self.settings.appearance.motion);
        self.diagnostics.note(format!("motion: {:?}", self.motion));
        self.theme = ThemeState::new(
            system.resolve(preference),
            system.fell_back(preference),
            &self.settings.appearance.colors,
        );
        self.diagnostics.note(format!(
            "appearance: {} ({})",
            self.theme.resolved.label(),
            if self.theme.fell_back {
                "system preference unreadable, fell back"
            } else {
                "as requested"
            }
        ));
    }

    /// Keyboard handling for the layout pages.
    ///
    /// Standard shortcuts, plus the arrow-key divider resizing the
    /// specification asks for: 1% per press, 5% with shift.
    fn editor_key(&mut self, key: Option<String>, shift: bool, control: bool) -> Task<Message> {
        let Some(key) = key else { return Task::none() };
        use crate::designer::Msg;

        // The library page has a dialog of its own to dismiss.
        if self.page == crate::designer::Page::Library {
            return match key.as_str() {
                "Escape" => {
                    self.layout_dialog = None;
                    Task::none()
                }
                "Enter" | "Return" if self.layout_dialog.is_some() => {
                    self.update(Message::ConfirmLayoutDialog)
                }
                _ => Task::none(),
            };
        }

        let message = match (key.as_str(), control, shift) {
            ("z", true, false) | ("Z", true, false) => Some(Msg::Undo),
            ("z", true, true) | ("Z", true, true) | ("y", true, _) => Some(Msg::Redo),
            ("s", true, _) => Some(Msg::Save),
            ("Escape", _, _) => Some(Msg::CloseDialog),
            // Divider nudging. Left/right move a vertical divider, up/down a
            // horizontal one; the reconciler ignores a nudge on the wrong
            // axis, so either pair is safe to press.
            ("Left", false, _) | ("Up", false, _) => Some(Msg::NudgeDivider(-1, shift)),
            ("Right", false, _) | ("Down", false, _) => Some(Msg::NudgeDivider(1, shift)),
            _ => None,
        };
        match message {
            Some(message) => self.update(Message::Designer(message)),
            None => Task::none(),
        }
    }

    fn on_action(&mut self, action: Action) -> Task<Message> {
        match action {
            Action::ToggleReader => self.toggle_reader(),
            // The caret goes to the box wherever the layout put it. Nothing
            // is opened or mounted: a layout without a search pane has
            // nowhere for the caret to go, and the key does nothing rather
            // than rearranging the presenter's screen mid-talk.
            // The rail collapses in place, wherever the layout put it, and a
            // layout without an outline pane simply has nothing to collapse.
            Action::ToggleOutline => {
                let collapsed = self.reader.controls().outline_collapsed;
                self.on_read_command(crate::widgets::event::ReadCommand::SetOutlineCollapsed(
                    !collapsed,
                ))
            }
            Action::FocusSearch => {
                iced::widget::operation::focus(crate::widgets::search::view::input_id())
            }
            Action::FindNext => self.on_find_command(crate::widgets::event::FindCommand::Next),
            Action::FindPrevious => {
                self.on_find_command(crate::widgets::event::FindCommand::Previous)
            }
            Action::Next => self.update(Message::Nav(Nav::Next)),
            Action::Previous => self.update(Message::Nav(Nav::Previous)),
            Action::First => self.update(Message::Nav(Nav::First)),
            Action::Last => self.update(Message::Nav(Nav::Last)),
            Action::PreviewNext => self.update(Message::Nav(Nav::PreviewNext)),
            Action::PreviewPrevious => self.update(Message::Nav(Nav::PreviewPrevious)),
            // Committing the preview is what Return ordinarily does; when a
            // link is focused it activates that instead, which is the only
            // reading a visible highlight leaves available.
            Action::CommitPreview if self.focused_link.is_some() => self
                .follow_focused_link()
                .unwrap_or_else(|| self.update(Message::Nav(Nav::CommitPreview))),
            // Escape gives the focus back before it cancels anything else, so
            // there is always a way out of the link cycle.
            Action::CancelPreview if self.focused_link.is_some() => {
                self.focused_link = None;
                Task::none()
            }
            Action::CommitPreview => self.update(Message::Nav(Nav::CommitPreview)),
            Action::CancelPreview => self.update(Message::Nav(Nav::CancelPreview)),
            // The venue decides which colour "blank" means; the alternate key
            // is always the other one, so both stay within reach without a
            // visit to the settings page.
            Action::Blank => match self.settings.display.blank_color {
                crate::settings::BlankColor::Black => self.update(Message::Nav(Nav::ToggleBlack)),
                crate::settings::BlankColor::White => self.update(Message::Nav(Nav::ToggleWhite)),
            },
            Action::BlankAlternate => match self.settings.display.blank_color {
                crate::settings::BlankColor::Black => self.update(Message::Nav(Nav::ToggleWhite)),
                crate::settings::BlankColor::White => self.update(Message::Nav(Nav::ToggleBlack)),
            },
            Action::ToggleTimer => self.update(Message::Nav(Nav::ToggleTimer)),
            Action::ResetTimer => self.update(Message::Nav(Nav::ResetTimer)),
            Action::SwapDisplays => {
                self.coordinator.roles = self.coordinator.roles.swapped();
                self.settings.display.roles = self.coordinator.roles.clone();
                self.persist();
                self.diagnostics.note("displays swapped");
                self.reconcile()
            }
            Action::ToggleAudienceFullscreen => {
                let wanted = !self.coordinator.roles.audience_fullscreen;
                self.coordinator.roles.audience_fullscreen = wanted;

                // On one screen the audience window would cover the presenter
                // view, so reconciliation normally keeps it windowed. Asking
                // for fullscreen is a decision the presenter is allowed to
                // make — they may be about to mirror, or just checking — so
                // it is honoured, and what it does is said out loud.
                let shared = self.resolved_index(Role::Presenter).is_some()
                    && self.resolved_index(Role::Presenter) == self.resolved_index(Role::Audience);
                self.coordinator.roles.allow_shared_display = wanted && shared;
                if wanted && shared && self.audience_started {
                    // The way back out has to be the key that is actually
                    // bound: this notice is the only thing on screen when the
                    // audience window is covering the presenter view.
                    let key = self
                        .action_shortcut(Action::ToggleAudienceFullscreen)
                        .map(|key| format!("Press {key} to bring it back."))
                        .unwrap_or_else(|| {
                            "Bind a key to \"Audience fullscreen\" to bring it back.".to_string()
                        });
                    self.notify(format!(
                        "The audience window is now fullscreen on this screen and covers the \
                         presenter view. {key}"
                    ));
                }

                self.settings.display.roles = self.coordinator.roles.clone();
                self.persist();
                self.reconcile()
            }
            Action::OpenDocument => self.update(Message::OpenDialog),
            Action::ReloadDocument => {
                let now = self.now;
                let actions = self.documents.open_initial(now);
                self.run_document_actions(actions)
            }
            Action::ShowOverview => self.update(Message::ToggleOverview),
            Action::ShowLayouts => self.update(Message::ShowLibrary),
            Action::AnnotateInk => self.arm_from_key(AnnotationTool::Ink),
            Action::AnnotateHighlighter => self.arm_from_key(AnnotationTool::Highlighter),
            Action::AnnotateEraser => self.arm_from_key(AnnotationTool::Eraser),
            // The key arms whichever of the two the pointer control is set
            // to, so the mode chosen in its options is the mode the key gives.
            Action::AnnotatePointer => self.arm_from_key(self.annotation_options().pointer_tool()),
            Action::UndoAnnotation => self.update(Message::Annotate(
                crate::widgets::event::AnnotationCommand::Undo,
            )),
            Action::RedoAnnotation => self.update(Message::Annotate(
                crate::widgets::event::AnnotationCommand::Redo,
            )),
            Action::ClearAnnotations => self.update(Message::Annotate(
                crate::widgets::event::AnnotationCommand::Clear,
            )),
            Action::ToggleAnnotationAudience => self.update(Message::Annotate(
                crate::widgets::event::AnnotationCommand::ToggleAudience,
            )),
            Action::FocusNextLink => self.step_link_focus(true),
            Action::FocusPreviousLink => self.step_link_focus(false),
            Action::Quit => self.quit(),
        }
    }

    fn quit(&mut self) -> Task<Message> {
        self.inhibitor.release(self.platform.services.as_ref());
        if let Some(supervisor) = self.supervisor.as_mut() {
            supervisor.shutdown();
        }
        // Synchronous on purpose: a helper thread would race process exit
        // and the last settings change would be the one that vanished.
        self.settings_dirty = false;
        if let Err(e) = self.store.save(&self.settings) {
            tracing::warn!(error = %e, "cannot save settings");
        }
        // A clean exit must never offer a restore, so the snapshot goes with
        // the process — and so does the document journal, for the same reason
        // and by the same rule (§11.1).
        //
        // Unsaved annotations are lost here, and that is the honest outcome:
        // they were never written to a file, and quitting is the user saying
        // so. What must not happen is the *next* run offering to put them
        // back into a document that has moved on.
        self.session.clear();
        self.reader_journal = None;
        crate::reader_journal::Journal::discard(&Self::journal_path());
        iced::exit()
    }

    /// Put the interrupted session back, having been told to.
    ///
    /// Everything audience-visible happens here and nowhere else. The
    /// snapshot is cleared first: a restore that itself crashes should be
    /// diagnosed from the new session, not replayed for ever.
    fn restore_session(&mut self) -> Task<Message> {
        let Some(plan) = self.pending_restore.take() else {
            return Task::none();
        };
        self.session.clear();

        if let Some(id) = plan.layout() {
            let id = LayoutId(id.to_string());
            if let Some(layout) = self.layouts.get(&id).cloned() {
                self.active_layout = layout;
                self.annotation_controls = crate::widgets::AnnotationControls::new(
                    annotation_options_in(&self.active_layout),
                );
                match crate::layout::builtin::LayoutMode::of(&self.active_layout) {
                    crate::layout::builtin::LayoutMode::Presentation => {
                        self.settings.layout.active = Some(id.0)
                    }
                    crate::layout::builtin::LayoutMode::Document => {
                        self.settings.layout.active_document = Some(id.0)
                    }
                }
                self.persist();
            }
        }

        self.coordinator.roles = plan.roles().clone();
        self.settings.display.roles = self.coordinator.roles.clone();
        self.persist();

        let document = plan.document().map(|path| path.to_path_buf());
        let already_open = self
            .state
            .document()
            .map(|open| Some(&open.path) == document.as_ref())
            .unwrap_or(false);

        let mut roles = self.coordinator.roles.clone();
        let task = match document {
            Some(path) if !already_open => {
                // The slide cannot be set until the deck is there; the rest
                // of the plan is applied against the loaded document in
                // `on_tick`.
                self.restoring_into_document = Some(plan);
                Task::batch([Task::done(Message::Opened(Some(path))), self.reconcile()])
            }
            _ => {
                plan.apply_to(&mut self.state, &mut roles, self.now);
                self.adopt_restored_target();
                self.coordinator.roles = roles;
                self.invalidate_renders();
                self.reconcile()
            }
        };
        self.notify_done("Restored the interrupted session.".to_string());
        task
    }

    /// Take the restored timer's target as the talk's length.
    ///
    /// A restore puts back the timer wholesale, target included. Without this
    /// the menu would go on offering the length from before the interruption
    /// while the timer counted against another.
    fn adopt_restored_target(&mut self) {
        let seconds = self
            .state
            .timer()
            .target
            .map(|target| target.as_secs().max(1) as u32);
        self.timer_controls.set_target(seconds);
    }

    /// Finish a confirmed restore once its document has been promoted.
    fn resume_restore_into_document(&mut self) {
        let Some(plan) = self.restoring_into_document.as_ref() else {
            return;
        };
        let ready = match (self.state.document(), plan.document()) {
            (Some(open), Some(wanted)) => open.path == wanted,
            _ => false,
        };
        if !ready {
            return;
        }
        let plan = self
            .restoring_into_document
            .take()
            .expect("checked just above");
        let mut roles = self.coordinator.roles.clone();
        plan.apply_to(&mut self.state, &mut roles, self.now);
        self.adopt_restored_target();
        self.coordinator.roles = roles;
        self.invalidate_renders();
        self.needs_reconcile = true;
    }

    /// Write the crash-recovery snapshot, at most once per interval and only
    /// when something actually changed.
    ///
    /// Nothing is written while a restore offer is unanswered: overwriting
    /// the snapshot with the fresh, empty session would silently destroy the
    /// very thing being offered.
    fn save_session(&mut self, now: Instant) {
        if self.pending_restore.is_some() || !self.session_throttle.due(now) {
            return;
        }
        // Fingerprinting is a metadata syscall — milliseconds on a network
        // mount — so it happens when the document (generation) changes, not
        // on every periodic save. External edits reach us through the file
        // watcher as a new generation anyway.
        let generation = self.state.generation();
        let document_path = self.state.document().map(|document| document.path.clone());
        let fingerprint = match (&self.session_fingerprint, &document_path) {
            (Some((cached, path, fingerprint)), Some(current))
                if *cached == generation && path == current =>
            {
                fingerprint.clone()
            }
            (_, None) => None,
            (_, Some(current)) => {
                let fingerprint = crate::session::fingerprint(current);
                self.session_fingerprint = Some((generation, current.clone(), fingerprint.clone()));
                fingerprint
            }
        };
        let snapshot = crate::session::SessionSnapshot::capture(
            &self.state,
            Some(self.active_layout.id.0.clone()),
            &self.coordinator.roles,
            fingerprint,
            now,
        );
        if !snapshot.is_worth_offering() {
            return;
        }
        if self
            .last_session
            .as_ref()
            .is_some_and(|last| last.matches_content(&snapshot))
        {
            return;
        }
        // The durable write happens off the UI thread; the snapshot is
        // remembered optimistically. A failed write logs from the helper
        // and the next interval retries, which is exactly what the retry
        // would have been anyway. Writes are seconds apart, so two cannot
        // race.
        let session = self.session.clone();
        let to_write = snapshot.clone();
        std::thread::spawn(move || {
            if let Err(e) = session.save(&to_write) {
                tracing::warn!(error = %e, "cannot save the session snapshot");
            }
        });
        self.last_session = Some(snapshot);
    }

    /// Drain everything the renderer workers have said, and the bookkeeping
    /// that must follow a batch of them.
    ///
    /// Shared by the tick and by the doorbell so a frame reaches the windows
    /// by whichever arrives first, with identical effects either way.
    fn pump_renderer(&mut self) {
        // Timed around the drain rather than around `pump` alone: copying a
        // large frame out of the shared region happens inside `pump`, and
        // turning it into a texture handle happens in the loop below. Both
        // are on the event loop, and a turn pays for both.
        let start = Instant::now();
        let events = self
            .supervisor
            .as_mut()
            .map(|s| s.pump())
            .unwrap_or_default();
        if events.is_empty() {
            return;
        }
        for event in events {
            self.on_render_event(event);
        }
        self.latency
            .record_stage(|stages| &mut stages.drain_renderer, start.elapsed());
        // One overlay-index rebuild for the whole batch of Overlays events
        // drained above, however many pages announced themselves.
        self.flush_overlay_rebuild();
    }

    fn on_tick(&mut self, now: Instant) -> Task<Message> {
        let previous = self.now;
        self.now = now;
        self.typst_annotations.service(&self.annotations, now);
        let mut tasks = Vec::new();

        // 0. Did the machine just wake up? The monotonic clock stops while
        //    Linux is suspended, so a gap far larger than the tick interval
        //    means resume. Nothing about the presentation changes — page,
        //    timer, blanking and roles are all preserved — but the topology
        //    and the GPU surfaces may have moved underneath us.
        let monotonic_gap = now.saturating_duration_since(previous);
        let wall_gap = std::time::SystemTime::now()
            .duration_since(self.last_wall)
            .unwrap_or_default();
        self.last_wall = std::time::SystemTime::now();
        if monotonic_gap >= RESUME_GAP || wall_gap.saturating_sub(monotonic_gap) >= RESUME_GAP {
            self.on_resume(monotonic_gap.max(wall_gap));
        }

        // 1. Expire routine toasts. Failures stay until dismissed.
        self.toasts.tick(now);

        // 1b. Collect what the document worker has said and ask for whatever
        //     the reader now needs drawn. On the tick rather than in a view
        //     pass: a page render must not happen inside a draw.
        self.pump_reader();
        self.pump_search();

        // 1c. Has the overview stopped moving? A grid that scrolls out from
        //     under its own selection is two objects rather than one, and
        //     Return would then jump back to a slide nobody can see.
        if let Some(last) = self.overview_settling {
            if now.saturating_duration_since(last) >= OVERVIEW_SETTLE {
                self.overview_settling = None;
                if let Some(task) = self.settle_overview_selection() {
                    tasks.push(task);
                }
            }
        }

        // 1a. Strike any wall-clock cue the last tick did not cover. The
        //     window, not the instant: ticks do not land on whole seconds.
        //     `crossed` drops everything after a suspend, so a machine that
        //     woke from a closed lid does not announce a lunchtime of cues.
        let seconds_of_day = crate::view::seconds_of_day();
        self.alarm_controls
            .strike(self.last_seconds_of_day, seconds_of_day, now);
        self.last_seconds_of_day = seconds_of_day;

        // 1b. Running past the target is the timer's version of a cue going
        //     off, and is announced the same way: the same pulse, timed from
        //     the moment it happened rather than from whenever the view next
        //     asked.
        let timer = self.state.timer();
        self.timer_controls
            .note_overtime(timer.elapsed(now), self.timer_controls.target(), now);

        // Opening a second toplevel and then fullscreening or moving it can
        // steal focus after the command that created it has already
        // completed. Repair focus on the app clock, after those window-manager
        // operations have had time to settle.
        let refocus_due = self
            .presenter_refocus_deadlines
            .iter()
            .any(|deadline| *deadline <= now);
        self.presenter_refocus_deadlines
            .retain(|deadline| *deadline > now);
        if refocus_due && self.audience_started {
            // Two requests, because neither is universally honoured: the
            // toolkit's own (which Wayland compositors mostly ignore, a client
            // cannot activate itself) and the backend's, which speaks the
            // compositor's or window manager's actual focus protocol.
            if let Some(presenter) = self.presenter_window {
                tasks.push(window::gain_focus::<Message>(presenter));
            }
            if let Some(native) = self.coordinator.native(Role::Presenter) {
                let outcome = self.coordinator.backend.focus(native);
                if !matches!(
                    outcome,
                    pulpit_display::PlacementOutcome::Applied
                        | pulpit_display::PlacementOutcome::Unsupported
                ) {
                    self.diagnostics.note(format!(
                        "could not return focus to the presenter: {outcome:?}"
                    ));
                }
            }
        }

        // 2. Renderer events. Also driven by `Message::RenderReady` the
        //    moment a worker speaks; the tick keeps calling it because the
        //    deadline and restart checks inside `pump` are the supervisor's
        //    clock, and a silent worker is exactly the case no doorbell
        //    reports.
        self.pump_renderer();

        // 2b. Media events. Frames arrive already validated and copied, so
        //     the only thing that reaches presentation state is a complete
        //     replacement for a frame already on screen. The newest pointer
        //     position goes out first: one coalesced move per tick.
        self.flush_pointer_move();
        self.poll_media();

        // The audience window's current frame, so a page change has something
        // to hold while the next one renders.
        self.remember_audience_frame();

        // Warm the deck's thumbnails in the background, a trickle per tick,
        // so opening the overview shows a finished grid.
        self.plan_thumbnails();
        self.pump_thumbnails();

        // 2. File watching.
        if self.settings.rendering.watch_document {
            if self.watcher.as_ref().is_some_and(|watcher| watcher.drain()) {
                let actions = self.documents.on_file_event(now);
                tasks.push(self.run_document_actions(actions));
            }
            let actions = self.documents.tick(now, &RealFileProbe);
            tasks.push(self.run_document_actions(actions));
        }

        // 3. A pending reconciliation (a new frame, a role change).
        if std::mem::take(&mut self.needs_reconcile) {
            tasks.push(self.reconcile());
        }

        // 4. Placement requests the window manager has not honoured yet.
        tasks.push(self.retry_placements(now));

        // 4b. A confirmed restore whose document has now finished opening,
        //     and the throttled crash-recovery snapshot.
        self.resume_restore_into_document();
        self.save_session(now);
        if self.settings_dirty && self.settings_throttle.due(now) {
            self.flush_settings();
        }

        // 5. Topology. Polling is the baseline; a native listener only makes
        //    it prompter, never authoritative.
        if now.duration_since(self.last_poll) >= POLL_TOPOLOGY {
            self.last_poll = now;
            if self.coordinator.refresh() {
                self.diagnostics
                    .record_snapshot(self.coordinator.snapshot.clone());
                tasks.push(self.reconcile());
            }
        }

        Task::batch(tasks)
    }

    /// Recover from suspend/resume.
    ///
    /// The presentation state is deliberately untouched: acceptance criterion
    /// 7 is that sleep/resume loses neither page, timer, blanking state nor
    /// the audience frame. What *is* redone is everything that depends on the
    /// hardware: a fresh topology snapshot, a reconciliation, and a re-request
    /// of the frames the two windows need in case surfaces were lost.
    fn on_resume(&mut self, gap: Duration) {
        tracing::info!(gap_secs = gap.as_secs(), "resumed from suspend");
        self.diagnostics.note(format!(
            "resumed after {}s: re-enumerating displays and re-requesting frames",
            gap.as_secs()
        ));
        // A stale snapshot from before the sleep must not be trusted — and
        // neither may the cached desktop appearance: the theme could have
        // changed while the machine slept.
        self.refresh_appearance_probe();
        self.apply_appearance();
        self.coordinator.refresh();
        self.diagnostics
            .record_snapshot(self.coordinator.snapshot.clone());
        self.needs_reconcile = true;
        // Placement is re-asserted from scratch: a projector that came back
        // on a different connector needs the whole sequence again, not a
        // half-applied retry from before the sleep.
        self.placement_retries.clear();
        self.request_renders();
    }

    /// Re-issue placements that were refused or could not be applied.
    ///
    /// Some window managers ignore placement requested before a window is
    /// mapped; this is the post-map retry path for exactly that case. It is
    /// bounded: after `MAX_PLACEMENT_RETRIES` the user is told what to do
    /// instead, because silently retrying forever is how a presenter ends up
    /// staring at a projector that never changes.
    fn retry_placements(&mut self, now: Instant) -> Task<Message> {
        let due: Vec<PlacementRetry> = self
            .placement_retries
            .iter()
            .filter(|retry| retry.due <= now)
            .cloned()
            .collect();
        if due.is_empty() {
            return Task::none();
        }
        self.placement_retries.retain(|retry| retry.due > now);

        let mut tasks = Vec::new();
        for retry in due {
            let Some(native) = self.coordinator.native(retry.role) else {
                continue;
            };
            let outcome = self
                .coordinator
                .backend
                .place(native, &retry.identity, retry.mode);
            self.diagnostics.note(format!(
                "placement retry {} for the {} window: {outcome:?}",
                retry.attempt,
                retry.role.as_str()
            ));
            match outcome {
                pulpit_display::PlacementOutcome::Applied => {
                    if let Some(id) = self.window_id(retry.role) {
                        tasks.push(window::set_mode::<Message>(
                            id,
                            display::iced_mode(retry.mode),
                        ));
                    }
                    if retry.role == Role::Audience {
                        self.schedule_presenter_refocus();
                    }
                }
                pulpit_display::PlacementOutcome::Disappeared => {
                    // Normal topology race: converge through reconciliation.
                    self.needs_reconcile = true;
                }
                _ if retry.attempt < MAX_PLACEMENT_RETRIES => {
                    self.placement_retries.push(PlacementRetry {
                        attempt: retry.attempt + 1,
                        due: now + PLACEMENT_RETRY_DELAY * retry.attempt,
                        ..retry
                    });
                }
                other => {
                    if let Some(message) = display::describe_placement(&other) {
                        self.notify(format!("display: {message}"));
                    }
                }
            }
        }
        Task::batch(tasks)
    }

    fn on_render_event(&mut self, event: RenderEvent) {
        match event {
            // A reader snapshot opening or failing to open is the reader's
            // business alone: routed into the deck-reload machinery it would
            // masquerade as a candidate presentation.
            RenderEvent::Opened(opened) if opened.document >= READER_RENDER_BASE => {
                self.diagnostics.note(format!(
                    "reader snapshot {} open ({} pages)",
                    opened.document, opened.page_count
                ));
            }
            RenderEvent::OpenFailed { document, reason } if document >= READER_RENDER_BASE => {
                tracing::warn!(document, reason, "reader snapshot failed to open");
                // Fall back to the previous picture source rather than
                // resubmitting jobs against a document no worker holds: the
                // pages keep their pre-edit pixels, the way they would if the
                // snapshot had never been taken, and the next edit tries
                // again with a fresh one.
                if self
                    .reader_render
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.document == document)
                {
                    if let Some(snapshot) = self.reader_render.snapshot.take() {
                        let _ = std::fs::remove_file(&snapshot.path);
                    }
                    self.notify(format!("The page cannot show recent edits yet: {reason}"));
                }
            }
            RenderEvent::Opened(opened) => {
                let mut info = DocumentInfo::new(
                    pulpit_core::DocumentId(opened.document),
                    self.documents.path(),
                    opened.page_count,
                )
                .with_first_page_size(opened.first_page_size)
                .with_page_sizes(opened.page_sizes.clone(), opened.page_sizes_sampled)
                .with_text_notes(
                    opened
                        .notes_pdfpc
                        .as_deref()
                        .and_then(pulpit_core::pdfpc::TextNotes::parse),
                );
                if let Some(notes) = info.text_notes.as_ref() {
                    self.diagnostics
                        .note(format!("embedded pdfpc notes: {} pages", notes.len()));
                } else if opened.notes_pdfpc.is_some() {
                    // The deck went to the trouble of carrying a payload and
                    // it could not be read. Silence here looks identical to a
                    // deck with no notes, which is the one thing it is not.
                    self.diagnostics
                        .note("embedded pdfpc notes could not be read".to_string());
                    self.notify(
                        "this deck carries speaker notes pulpit could not read".to_string(),
                    );
                }
                info.path = self.documents.path().to_path_buf();

                // A mapping the user chose for this document outranks every
                // other source, and neither source below may disturb it.
                let explicit = self
                    .settings
                    .notes
                    .per_document
                    .iter()
                    .any(|entry| entry.path == info.path);
                let from_metadata = if explicit || !self.settings.notes.honour_metadata_contract {
                    None
                } else {
                    NotesMapping::from_metadata(&opened.metadata_text)
                };
                // Only page geometry, and only beamer's doubled page. The
                // sizes may be a sample of the document; the detector requires
                // one shape across whatever it is given.
                let from_geometry =
                    if explicit || from_metadata.is_some() || !self.settings.notes.detect_split {
                        None
                    } else {
                        pulpit_core::notes::detect_split(&opened.page_sizes)
                    };
                if let Some(mapping) = from_metadata {
                    self.diagnostics
                        .note(format!("metadata contract selected mapping: {mapping}"));
                    self.state.apply(Nav::SetNotesMapping(mapping), self.now);
                } else if let Some(mapping) = from_geometry {
                    self.diagnostics
                        .note(format!("doubled pages selected mapping: {mapping}"));
                    // Said out loud, not only in diagnostics. beamer's default
                    // is assumed for which half is which, and a presenter must
                    // not discover that assumption was wrong by noticing the
                    // notes are on the projector.
                    self.notify(
                        "beamer notes detected: slide left, notes right — swap it in Settings"
                            .to_string(),
                    );
                    self.state.apply(Nav::SetNotesMapping(mapping), self.now);
                }
                let actions = self.documents.on_candidate_opened(info);
                let _ = self.run_document_actions(actions);
            }
            RenderEvent::OpenFailed { document, reason } => {
                let actions = self.documents.on_candidate_failed(
                    pulpit_core::DocumentId(document),
                    reason,
                    self.now,
                );
                let _ = self.run_document_actions(actions);
            }
            // The link annotations on a page, kept until the document is
            // replaced. A press on the current slide is hit-tested against
            // these.
            RenderEvent::Links {
                document,
                page,
                links,
            } => {
                tracing::debug!(page, links = links.len(), "links received");
                self.links.insert((document, page), links);
            }
            RenderEvent::Navigation {
                document,
                navigation,
            } => {
                tracing::debug!(
                    document,
                    sections = navigation.sections().len(),
                    "navigation model received"
                );
                self.navigation.insert(document, navigation);
                // The memoised section may predate this outline.
                *self.section_cache.borrow_mut() = None;
            }
            RenderEvent::Found {
                document: _,
                generation,
                chunk,
                searchable,
            } => {
                if searchable {
                    self.search.accept(generation, chunk);
                } else {
                    self.search.fail(
                        generation,
                        pulpit_core::search::SearchProblem::Unsupported(
                            "this deck has no text layer to search".into(),
                        ),
                    );
                }
            }
            RenderEvent::Capabilities {
                document,
                capabilities,
            } => {
                tracing::debug!(
                    document,
                    findings = capabilities.len(),
                    "unsupported-feature findings received"
                );
                self.capabilities.insert(document, capabilities);
            }
            RenderEvent::Overlays {
                page,
                declarations,
                diagnostics,
                ..
            } => {
                tracing::debug!(page, declarations = declarations.len(), "overlays received");
                for problem in &diagnostics {
                    self.diagnostics
                        .note(format!("page {}: {problem}", page + 1));
                }
                if declarations.is_empty() {
                    self.overlay_declarations.remove(&page);
                } else {
                    self.overlay_declarations.insert(page, declarations);
                }
                // Rebuilding regroups every page's declarations, which is
                // what collapses a reveal sequence into one overlay — and it
                // regroups *every* page, so doing it once per drained batch
                // keeps a deck's opening burst linear instead of quadratic.
                // The page the audience is looking at cannot wait for the
                // rest of the batch: its media should start now.
                self.overlays_dirty = true;
                self.pending_overlay_diagnostics.extend(diagnostics);
                let committed = self.state.audience_source().map(|source| source.pdf_page);
                if committed == Some(page) {
                    self.flush_overlay_rebuild();
                }
            }
            RenderEvent::Attachment { name, bytes, .. } => {
                tracing::debug!(name, bytes = bytes.len(), "attachment received");
                self.media.attachment_ready(&name, &bytes);
                self.service_media();
            }
            RenderEvent::AttachmentFailed { name, reason, .. } => {
                tracing::debug!(name, reason, "attachment unavailable");
                self.media.attachment_failed(&name, &reason);
            }
            RenderEvent::Frame {
                job,
                frame,
                worked,
                rendered,
            } => {
                // Whether this was warming work has to be read before
                // `take_pending`, which forgets it.
                let was_thumbnail = self.thumbnail_requests.contains(&job.id);
                // The whole wait and the worker's share of it. Recorded
                // before the early returns below: a frame that arrives too
                // late to be wanted was still rendered and still copied.
                if let Some(submitted) = self.submitted_at.remove(&job.id) {
                    // The two top tiers are the page a window is showing;
                    // everything below is for a page one step away.
                    let on_screen =
                        matches!(job.priority, Priority::Audience | Priority::Presenter);
                    self.latency.note_render(
                        submitted.elapsed(),
                        worked,
                        rendered,
                        was_thumbnail,
                        on_screen,
                    );
                }
                if frame.cpu_bytes() >= pulpit_render::protocol::INLINE_FRAME_BYTES {
                    self.latency.note_copy(frame.cpu_bytes());
                }
                let Some(key) = self.take_pending(job.id) else {
                    return;
                };
                if key.generation < self.state.generation() {
                    return;
                }
                if was_thumbnail {
                    // A thumbnail lives in its own cache, on its own budget;
                    // it never enters the frame cache the windows draw from.
                    self.thumbnails.insert(
                        key.slide,
                        iced::widget::image::Handle::from_rgba(
                            frame.width,
                            frame.height,
                            shared_pixels(&frame.pixels),
                        ),
                        frame.pixels.len() as u64,
                        frame.width,
                        // Room is kept around whatever the grid is showing,
                        // for the same reason warming works outwards from it.
                        self.warming_centre(),
                    );
                    // A slot just came free. Waiting for the next tick to
                    // notice would leave the pool idle for up to the tick
                    // interval on every batch, which on a long deck is most
                    // of the warming time; refilling here keeps the workers
                    // busy at their own pace instead of the timer's.
                    self.plan_thumbnails();
                    self.pump_thumbnails();
                    return;
                }
                tracing::debug!(slide = key.slide, quality = ?key.quality, width = key.width, "frame cached");
                // A reader page rendered from a snapshot now shows every mark
                // committed at or before that snapshot's revision, so their
                // retained previews come down (§9.2).
                if key.kind == FrameKind::Page {
                    if let Some(snapshot) = &self.reader_render.snapshot {
                        if key.generation >= snapshot.generation {
                            let page = pulpit_core::page::PageIndex(key.slide);
                            self.reader.frame_landed(page, snapshot.revision);
                            // A full frame is the baseline every partial
                            // repaint was standing in for (§9.4): once one
                            // contains the patch's revision, the patch is a
                            // second copy of pixels the frame already has.
                            if self
                                .reader_patches
                                .get(&page)
                                .is_some_and(|patch| patch.revision <= snapshot.revision)
                            {
                                self.reader_patches.remove(&page);
                                self.wash_cache.borrow_mut().clear();
                            }
                        }
                    }
                }
                if self.cache.insert(key, frame.clone()) {
                    // The handle shares the cached frame's own allocation, so
                    // naming a frame costs nothing. Uploading it is a separate
                    // question, asked once per window by the view that draws
                    // it — never here, where the answer would be "upload it
                    // everywhere, and keep it there for ever".
                    let handle = iced::widget::image::Handle::from_rgba(
                        frame.width,
                        frame.height,
                        shared_pixels(&frame.pixels),
                    );
                    self.frame_ready(key, handle);
                }
                self.pin_visible();

                // The candidate document's first frame is what promotes it.
                if let Some(active) = self.documents.active() {
                    if active.id.0 != job.document {
                        let info = DocumentInfo::new(
                            pulpit_core::DocumentId(job.document),
                            self.documents.path(),
                            active.pdf_pages,
                        );
                        let _ = info;
                    }
                }
                self.mark_audience_frame();
                self.remember_audience_frame();
            }
            RenderEvent::Failed { job, reason } => {
                tracing::warn!(?job, reason, "render failed");
                self.diagnostics
                    .note(format!("render of page {} failed: {reason}", job.page + 1));
                let was_thumbnail = self.thumbnail_requests.contains(&job.id);
                self.take_pending(job.id);
                // A failure frees a warming slot exactly as a frame does, and
                // a slot nobody reclaims is warming that quietly stops.
                if was_thumbnail {
                    self.pump_thumbnails();
                }
            }
            RenderEvent::Cancelled { id } => {
                let was_thumbnail = self.thumbnail_requests.contains(&id);
                self.take_pending(id);
                if was_thumbnail {
                    self.pump_thumbnails();
                }
            }
            RenderEvent::WorkerCrashed {
                worker,
                restarts,
                reason,
            } => {
                self.notify(format!(
                    "renderer worker {worker} stopped ({reason}); restarting (#{restarts})"
                ));
            }
            RenderEvent::WorkerRestarted { worker } => {
                self.diagnostics.note(format!("worker {worker} restarted"));
                self.request_renders();
            }
            RenderEvent::WorkerGaveUp { worker } => {
                self.notify(format!(
                    "renderer worker {worker} keeps failing; rendering is degraded"
                ));
            }
            RenderEvent::WorkerTimedOut { worker, job } => {
                self.diagnostics.note(format!(
                    "worker {worker} timed out on page {}",
                    job.page + 1
                ));
            }
        }
    }

    fn run_document_actions(&mut self, actions: Vec<DocAction>) -> Task<Message> {
        for action in actions {
            match action {
                DocAction::OpenCandidate {
                    path,
                    document,
                    attempt,
                } => {
                    tracing::info!(path = %path.display(), attempt, "opening candidate");
                    self.diagnostics.note(format!(
                        "opening candidate {} (attempt {attempt})",
                        path.display()
                    ));
                    if let Some(supervisor) = self.supervisor.as_mut() {
                        supervisor.open(document.0, &path.to_string_lossy());
                    }
                }
                DocAction::RenderFirstFrame { info, .. } => {
                    // Promotion is immediate here because the state machine
                    // already validated the candidate; the audience frame is
                    // requested below and the previous frame stays visible
                    // until it lands.
                    let promoted = self.documents.on_first_frame(info);
                    let _ = self.run_document_actions(promoted);
                }
                DocAction::Promote { info } => {
                    tracing::info!(path = %info.path.display(), pages = info.pdf_pages, "promoted document");
                    self.diagnostics.note(format!(
                        "promoted {} ({} pages)",
                        info.path.display(),
                        info.pdf_pages
                    ));
                    let mapping = self.settings.mapping_for(&info.path);
                    if self.state.document().is_none() {
                        self.state = PresentationState::new(info.clone(), mapping);
                        // A fresh state starts with no target; the length of
                        // the talk was settled before the deck was opened.
                        self.state.timer_mut().target = self.timer_controls.target();
                    } else {
                        self.state
                            .apply(Nav::ReplaceDocument(info.clone()), self.now);
                    }
                    if let Some(retired) = self.retired_document.take() {
                        if retired != info.id {
                            self.links.retain(|(document, _), _| *document != retired.0);
                            if let Some(supervisor) = self.supervisor.as_mut() {
                                supervisor.close(retired.0);
                            }
                            // A reload keeps its old frames as stand-ins
                            // while the new ones render, because they show
                            // the same slide. A different deck's slide 3 is
                            // not a stand-in for this one's, so those frames
                            // go rather than being shown by the fallback in
                            // `audience_frame`.
                            self.cache.clear();
                            for evicted in self.cache.take_evicted() {
                                self.handles.remove(&evicted);
                            }
                            self.last_audience = None;
                            self.last_presenter = None;
                        }
                    }
                    self.settings.remember_recent(info.path.clone());
                    self.persist();
                    self.invalidate_renders();
                }
                DocAction::DiscardCandidate { document } => {
                    if let Some(supervisor) = self.supervisor.as_mut() {
                        supervisor.close(document.0);
                    }
                }
                DocAction::ReportFailure { reason, attempts } => {
                    self.notify_error(
                        format!("The document could not be reopened: {reason}"),
                        Some(format!(
                            "Tried {attempts} times. The last good version is still showing; \
                             fix the build and it will reload itself."
                        )),
                    );
                }
                DocAction::ClearFailure => {
                    self.toasts.dismiss_all();
                    self.notify_done("The document reloaded successfully.".to_string());
                }
            }
        }
        Task::none()
    }

    fn open_document(&mut self, path: PathBuf) -> Task<Message> {
        let mut documents = DocumentManager::new(
            path.clone(),
            ReloadPolicy {
                debounce: Duration::from_millis(self.settings.rendering.watch_debounce_ms),
                ..ReloadPolicy::default()
            },
        );
        // The workers still hold the document we are replacing, so the new
        // one has to be numbered after it, and the old one is let go once the
        // new one has actually taken over.
        documents.continue_ids_after(&self.documents);
        self.retired_document = self.documents.active().map(|info| info.id);
        self.documents = documents;
        self.watcher = match DocumentWatcher::new(&path) {
            Ok(watcher) => Some(watcher),
            Err(e) => {
                self.notify(format!("automatic reload is off: {e}"));
                None
            }
        };
        self.document_serial += 1;
        // Slide 7 of one deck has nothing to do with slide 7 of another, so
        // opening a document starts with a clean sheet. The new document's own
        // marks arrive from its engine once it has described itself.
        self.annotations.clear();
        self.warned_marks_are_not_kept = false;
        // Diagnostic kill switch while chasing a page-turn regression: skip
        // the document session entirely to measure its cost.
        if std::env::var_os("PULPIT_NO_READER").is_none() {
            self.open_for_reading(&path);
        }
        let actions = self.documents.open_initial(self.now);
        self.run_document_actions(actions)
    }

    /// Open the same file for the reader: a document worker of its own,
    /// holding one open PDF it can annotate (§5.1, §6).
    ///
    /// Separate from the render workers on purpose. Those hold the document
    /// read-only for the projector and are interchangeable; this one is the
    /// single execution context that owns the mutations, and a frame it draws
    /// contains the commit that was just made (A7).
    ///
    /// A failure here is not a failure to open the deck: presentation mode
    /// works whether or not document mode does, so the reader stays closed and
    /// says why rather than taking the presentation down with it.
    fn open_for_reading(&mut self, path: &std::path::Path) {
        self.reader.closed();
        self.reset_reader_rendering();
        self.reader_link = None;
        self.reader_journal = None;

        // Anything the last run left unsaved for *this* file, before a new
        // journal replaces it. The offer is inert: nothing is applied without
        // an explicit answer (§11.4).
        let journal_path = Self::journal_path();
        let fingerprint = crate::session::fingerprint(path);
        self.pending_reader_recovery = crate::reader_journal::Journal::recover(&journal_path)
            .filter(|recovered| {
                fingerprint
                    .as_ref()
                    .is_some_and(|current| recovered.applies_to(path, current))
            });

        match crate::reader_link::ReaderLink::open(path) {
            Ok(mut link) => {
                // Ask for the shape of it straight away: the reader can lay
                // out nothing until it knows how many pages there are and how
                // big they are.
                link.ask(crate::reader_link::Ask::Describe { pages: 0 });
                self.reader_link = Some(link);
            }
            Err(error) => {
                tracing::warn!(%error, "document mode is unavailable for this file");
                return;
            }
        }

        // A journal for this run. Started only once the document opened: a
        // file that cannot be read has nothing to journal about.
        match fingerprint {
            Some(fingerprint) => {
                match crate::reader_journal::Journal::start(&journal_path, path, fingerprint) {
                    Ok(journal) => self.reader_journal = Some(journal),
                    Err(error) => {
                        // Editing still works; what is lost is the promise
                        // that an edit survives a crash, and saying so is
                        // better than discovering it after one.
                        self.notify(format!("Unsaved edits will not survive a crash: {error}"));
                    }
                }
            }
            None => tracing::warn!("no fingerprint for the document; edits are not journalled"),
        }
    }

    /// Where the document journal lives: beside the session snapshot, because
    /// it answers the same question about the same run (§11.1).
    fn journal_path() -> PathBuf {
        crate::settings::store::config_directory().join("document-journal.jsonl")
    }

    /// Record one revision-incrementing operation, durably, at commit.
    ///
    /// A failure here is reported once and does not stop the edit: the edit is
    /// in the document, which is what the user asked for. What is lost is only
    /// its survival of a crash.
    fn journal(&mut self, entry: crate::reader_journal::JournalEntry) {
        let Some(journal) = self.reader_journal.as_mut() else {
            return;
        };
        let full = journal.is_full();
        if let Err(error) = journal.append(&entry) {
            self.reader_journal = None;
            self.notify(format!(
                "Unsaved edits are no longer being recorded: {error}"
            ));
            return;
        }
        if !full && self.reader_journal.as_ref().is_some_and(|j| j.is_full()) {
            self.notify(
                "This session has more edits than the crash journal holds; \
                 the newest are not recorded. Save to be safe."
                    .to_string(),
            );
        }
    }

    /// Collect whatever the document worker has said, and ask for whatever the
    /// reader now needs drawn.
    ///
    /// Called from the tick rather than from a view pass: a render and, later,
    /// a keystroke round trip must not happen inside a draw.
    fn pump_reader(&mut self) {
        // Collected first, then handled: the answers are drained from the link
        // and the link is put down, so handling one can notify, close the
        // reader, or ask for the next thing without the borrow in the way.
        let Some(told) = self
            .reader_link
            .as_mut()
            .map(crate::reader_link::ReaderLink::collect)
        else {
            return;
        };
        for told in told {
            match told {
                crate::reader_link::Told::Described {
                    info,
                    geometry,
                    outline,
                } => {
                    // The document is up, so what the last run left unsaved
                    // can be *offered*. It is not applied: recovery needs an
                    // explicit answer, and the offer itself is inert (§11.4).
                    self.reader_recovery = self.pending_reader_recovery.take();
                    // A different document: page numbers mean something else
                    // now, and a repaint of page three of the last one is not
                    // a repaint of anything.
                    self.reader_patches.clear();
                    self.reader_patch_pending.clear();
                    self.wash_cache.borrow_mut().clear();
                    self.reader
                        .opened(geometry, info.level, info.warnings.clone());
                    self.reader.set_outline(
                        outline
                            .flattened()
                            .into_iter()
                            .filter_map(|entry| {
                                Some(crate::widgets::context::OutlineRow {
                                    title: entry.title.clone(),
                                    // A bookmark that points at a URI orders
                                    // nothing in this document, so it is not a
                                    // row the rail can take you to.
                                    page: pulpit_core::page::PageIndex(entry.page()?),
                                    depth: entry.depth,
                                })
                            })
                            .collect(),
                    );
                    // Every warning is said once, before the first edit, which
                    // is what A9 requires of the signature one in particular.
                    for warning in &info.warnings {
                        self.notify(warning.message().to_string());
                    }
                }
                crate::reader_link::Told::Found { generation, chunk } => {
                    // Stale chunks land nowhere: the model compares the
                    // generation and drops what belongs to a query the
                    // reader has already typed past.
                    self.search.accept(generation, chunk);
                }
                crate::reader_link::Told::CannotSearch {
                    generation,
                    message,
                } => {
                    self.search.fail(
                        generation,
                        pulpit_core::search::SearchProblem::Unsupported(message),
                    );
                }
                crate::reader_link::Told::Snapshotted(saved) => {
                    self.reader_snapshot_landed(saved);
                }
                crate::reader_link::Told::SnapshotFailed { message } => {
                    // The edit is safe in the document worker; what failed is
                    // only the copy the render pool draws from, so the page
                    // keeps its pre-edit picture. Said once, and not retried
                    // in a loop: the next edit arms the debounce again.
                    self.reader_render.snapshot_in_flight = None;
                    self.notify(format!("The page cannot show recent edits yet: {message}"));
                }
                crate::reader_link::Told::Applied(applied) => {
                    let pending = self.reader_pending.pop_front();
                    let kind = pending
                        .as_ref()
                        .map(|pending| pending.kind)
                        .unwrap_or(AppliedKind::Edit);
                    // What the previews could not absorb — decided when the
                    // edit was sent, and from the answer for an undo or a
                    // redo, which only says what it did once it has done it.
                    let urgency = self.reader.applied(&applied, kind).max(
                        pending
                            .as_ref()
                            .map(|pending| pending.urgency)
                            .unwrap_or_default(),
                    );
                    // The render pool cannot see this commit until the next
                    // snapshot, so arm the wait that takes one. How long a
                    // wait is what `urgency` decides.
                    self.reader_render.edited_at = Some(Instant::now());
                    self.reader_render.urgency = self.reader_render.urgency.max(urgency);
                    if self.reader.retained_count() > MAX_RETAINED_MARKS {
                        self.reader_render.forced = true;
                    }
                    // Nothing on screen draws this edit, so ask the worker for
                    // the rectangle it changed rather than making the page
                    // wait on the snapshot behind it (§9.4).
                    if urgency == crate::reader::RasterUrgency::Prompt {
                        self.ask_reader_patch(&applied);
                    }

                    // A mark the presenter drew now has a name. Giving it to
                    // the overlay stroke is what makes the two one thing: the
                    // eraser can delete it, a page turn can recognise it, and
                    // document mode is editing the same annotation.
                    if pending
                        .as_ref()
                        .is_some_and(|pending| pending.names_a_presenter_mark)
                    {
                        for effect in &applied.effects {
                            if let pulpit_render::document::AppliedEffect::Annotation(summary) =
                                effect
                            {
                                self.annotations.name_stroke(summary.id.clone());
                            }
                        }
                    }

                    // Journalled now that the worker has confirmed it, and
                    // not when it was sent: a mutation with no answer is not
                    // a mutation that happened (§11.5). Undos and redos are
                    // recorded like anything else, in revision order, so
                    // replay reproduces the history rather than only its
                    // surviving edits (§11.1).
                    let entry = match pending.and_then(|pending| pending.transaction) {
                        Some(transaction) => crate::reader_journal::JournalEntry::Applied {
                            revision: applied.document_revision,
                            transaction,
                        },
                        None => crate::reader_journal::JournalEntry::Reversed {
                            revision: applied.document_revision,
                            operation: Box::new(applied.undo.clone()),
                        },
                    };
                    self.journal(entry);
                }
                crate::reader_link::Told::Annotations { page, summaries } => {
                    self.reader.set_annotations(page, &summaries);
                    // …and if this is the page the projector is showing, the
                    // slide's marks are these (A1). Presentation and document
                    // mode read the same answer.
                    self.adopt_document_marks(page);
                }
                crate::reader_link::Told::Selection { result, finalising } => {
                    // The worker is free again: the newest waiting sample, if
                    // any, goes out before this answer is even drawn.
                    self.selection_query_answered();
                    // Both modes sweep text through the same worker, and only
                    // one of them can have a sweep open, because there is one
                    // pointer. The presenter is asked first and answers for
                    // its own gesture only.
                    if self.presenter_selection_resolved(
                        result.quads.clone(),
                        result.text.clone(),
                        finalising,
                    ) {
                        // Answered by the presenter's gesture.
                    } else if let Some(transaction) =
                        self.reader
                            .selection_resolved(result.quads, result.text, finalising)
                    {
                        self.commit_to_document(transaction);
                    } else if finalising {
                        // A selection that resolved to nothing commits nothing
                        // and says why, rather than leaving the reader to
                        // wonder whether the highlighter is broken (§8.2).
                        self.notify(
                            "There is no selectable text there, so there is nothing to \
                             highlight."
                                .to_string(),
                        );
                    }
                }
                crate::reader_link::Told::Patched(frame) => {
                    self.reader_patch_landed(*frame);
                }
                crate::reader_link::Told::Saved(saved) => {
                    self.notify(format!("Saved {}", saved.path.display()));
                    // Nothing is unsaved any more: the edits are in the file
                    // the user just wrote. A journal kept past a save would
                    // offer to replay edits a document already has.
                    if let Some(journal) = self.reader_journal.as_mut() {
                        journal.finish();
                    }
                }
                crate::reader_link::Told::Failed { message, fatal } => {
                    // A refusal is reported and the reader carries on; a lost
                    // worker closes document mode, because nothing more will
                    // be answered and a mutation in flight must not be
                    // assumed committed (§11.5).
                    self.notify(message);
                    // Whatever was asked for has not been answered and will
                    // not be. An annotation list left marked in flight would
                    // never be asked for again, and the eraser would sweep a
                    // page it knows nothing about.
                    self.reader.annotations_abandoned();
                    // …and if it answered a commit, the mark the UI is still
                    // drawing for it is a mark the document will never hold.
                    self.reader.commit_refused();
                    // A selection query that failed is answered too, as far
                    // as the in-flight guard is concerned.
                    self.selection_query_in_flight = false;
                    self.selection_query_waiting = None;
                    // If the refusal answered a snapshot request, nothing
                    // will confirm it either.
                    self.reader_render.snapshot_in_flight = None;
                    if fatal {
                        self.reader.closed();
                        self.reset_reader_rendering();
                        self.reader_link = None;
                        return;
                    }
                }
            }
        }

        // How big the page surface actually is, which is what a fit is fitted
        // to. Taken from the layout rather than reported by the view: a view
        // pass draws, and asking it to send a message about its own size is
        // how a layout loop starts.
        if let Some(cell) = self.page_surface_size() {
            self.reader.set_cell(cell.0, cell.1);
        }

        // Whatever is on screen and out of date. Bounded by the window: a
        // thousand-page document asks for the two or three pages in front of
        // the reader and nothing else.
        // What is on the pages in the window, which is what the eraser and
        // the selection tool hit-test against. Asked for once per page and
        // dropped again the moment that page is edited.
        let pages = self.reader.annotations_wanted();
        if let Some(link) = self.reader_link.as_mut() {
            for page in pages {
                link.ask(crate::reader_link::Ask::ListAnnotations { page });
            }
        }

        // A burst of edits settled: take one snapshot of the document as it
        // now stands, so the render pool can draw pages that contain them.
        self.pump_reader_snapshot();

        // …and ask the pool for whatever the window now needs drawn.
        self.request_reader_renders();
    }

    /// Ask the document worker for a snapshot when one is due.
    fn pump_reader_snapshot(&mut self) {
        if self.reader_render.snapshot_in_flight.is_some() {
            return;
        }
        let Some(edited_at) = self.reader_render.edited_at else {
            return;
        };
        // How long the edits are left to settle depends on whether anything is
        // waiting for them. A mark the previews already draw is on the page
        // now; re-rendering it sooner shows the user nothing they cannot
        // already see, and costs a whole-document snapshot, a reopen and a
        // cold render of every visible page.
        let settle = match self.reader_render.urgency {
            crate::reader::RasterUrgency::Prompt => SNAPSHOT_DEBOUNCE,
            crate::reader::RasterUrgency::Deferred => SNAPSHOT_QUIET,
        };
        if !self.reader_render.forced && edited_at.elapsed() < settle {
            return;
        }
        let Some(link) = self.reader_link.as_mut() else {
            return;
        };
        let serial = self.reader_render.serial + 1;
        let destination = reader_snapshot_directory().join(format!("snapshot-{serial}.pdf"));
        if let Err(error) = std::fs::create_dir_all(reader_snapshot_directory()) {
            tracing::warn!(%error, "no snapshot directory; edits stay off the page until saved");
            self.reader_render.edited_at = None;
            return;
        }
        if link.ask(crate::reader_link::Ask::Snapshot {
            destination: destination.clone(),
        }) {
            self.reader_render.serial = serial;
            self.reader_render.snapshot_in_flight = Some((serial, destination));
            self.reader_render.edited_at = None;
            self.reader_render.urgency = crate::reader::RasterUrgency::Deferred;
            self.reader_render.forced = false;
        }
    }

    /// A snapshot landed: point the render pool at it and let the generation
    /// walk prefer its frames over everything older (A7).
    fn reader_snapshot_landed(&mut self, saved: pulpit_render::document::SavedDocument) {
        let Some((serial, destination)) = self.reader_render.snapshot_in_flight.take() else {
            tracing::warn!("a snapshot nobody asked for was ignored");
            return;
        };
        let previous = self.reader_render.snapshot.take();
        let snapshot = ReaderSnapshot {
            document: READER_RENDER_BASE + serial,
            generation: pulpit_core::RenderGeneration(READER_RENDER_BASE + serial),
            revision: saved.revision,
            path: destination,
        };
        tracing::debug!(
            revision = saved.revision.0,
            document = snapshot.document,
            "reader: rendering from a new snapshot"
        );
        if let Some(supervisor) = self.supervisor.as_mut() {
            supervisor.open(snapshot.document, &snapshot.path.to_string_lossy());
            if let Some(previous) = &previous {
                supervisor.close(previous.document);
            }
        }
        if let Some(previous) = previous {
            let _ = std::fs::remove_file(&previous.path);
        }
        self.reader_render.snapshot = Some(snapshot);
        // The stale-job sweep in `request_reader_renders` cancels everything
        // still in flight at the old generation on its next pass, which is
        // right now.
        self.request_reader_renders();
    }

    /// Which supervisor document and generation reader pages render from.
    ///
    /// The latest snapshot once there is one; before any edit, the
    /// presentation's own document — already open in every pool worker — at
    /// the presentation's generation.
    fn reader_render_source(&self) -> Option<(u64, pulpit_core::RenderGeneration)> {
        if let Some(snapshot) = &self.reader_render.snapshot {
            return Some((snapshot.document, snapshot.generation));
        }
        self.state
            .document()
            .map(|document| (document.id.0, self.state.generation()))
    }

    /// Forget the reader's snapshots and cancel its renders: the document
    /// closed, or a new one is taking its place.
    fn reset_reader_rendering(&mut self) {
        let state = std::mem::take(&mut self.reader_render);
        let doomed: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, key)| key.kind == FrameKind::Page)
            .map(|(id, _)| *id)
            .collect();
        self.pending.retain(|(_, key)| key.kind != FrameKind::Page);
        if let Some(supervisor) = self.supervisor.as_mut() {
            for id in doomed {
                supervisor.cancel(id);
            }
            if let Some(snapshot) = &state.snapshot {
                supervisor.close(snapshot.document);
            }
        }
        if let Some(snapshot) = state.snapshot {
            let _ = std::fs::remove_file(&snapshot.path);
        }
    }

    /// Submit the reader's render plan to the pool, and cancel what it no
    /// longer wants.
    ///
    /// The same shape as `request_renders`: plan, drop what the cache already
    /// satisfies and what is in flight, cancel the obsolete, submit the rest.
    /// Only `FrameKind::Page` keys are touched here, and only they are spared
    /// there, so the two sweeps cannot cancel each other's work.
    fn request_reader_renders(&mut self) {
        if !self.reader.is_open() || self.page_surface_size().is_none() {
            return;
        }
        let Some((document, generation)) = self.reader_render_source() else {
            return;
        };
        let scale = self.presenter_scale_factor();
        let plan = self.reader.render_plan(scale);

        let mut jobs = Vec::new();
        let mut still_wanted: Vec<FrameKey> = Vec::new();
        for entry in plan {
            let key = FrameKey {
                generation,
                slide: entry.page.get(),
                kind: FrameKind::Page,
                quality: entry.quality,
                width: entry.width,
                height: entry.height,
            };
            still_wanted.push(key);
            if self.cache.satisfies(
                generation,
                key.slide,
                FrameKind::Page,
                key.quality,
                key.width,
            ) {
                continue;
            }
            if self.pending.iter().any(|(_, pending)| *pending == key) {
                continue;
            }
            // The pages on screen outrank the margin; nothing here outranks
            // an audience frame, and in the reader layout there is none.
            let priority = if entry.visible {
                Priority::Presenter
            } else {
                Priority::Adjacent
            };
            jobs.push((key, priority));
        }

        // A page render still in flight for a page the reader has scrolled
        // away from — or from before the last snapshot — is a worker burning
        // time on a picture nobody will look at, ahead of one somebody is
        // waiting for. This cancellation is most of the difference between
        // paging through a document and waiting for it.
        let obsolete: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, key)| {
                key.kind == FrameKind::Page
                    && (key.generation < generation || !still_wanted.contains(key))
            })
            .map(|(id, _)| *id)
            .collect();
        let Some(supervisor) = self.supervisor.as_mut() else {
            return;
        };
        if !obsolete.is_empty() {
            let doomed: std::collections::HashSet<RequestId> = obsolete.iter().copied().collect();
            self.pending
                .retain(|(pending, _)| !doomed.contains(pending));
            for id in obsolete {
                supervisor.cancel(id);
                self.submitted_at.remove(&id);
            }
        }

        for (key, priority) in jobs {
            let id = supervisor.next_request_id();
            supervisor.submit(RenderJob {
                id,
                generation,
                document,
                page: key.slide,
                region: pulpit_core::notes::Region::FULL,
                width: key.width,
                height: key.height,
                priority,
                quality: key.quality,
                with_annotations: true,
                region_name: String::new(),
            });
            self.pending.push((id, key));
            self.submitted_at.insert(id, Instant::now());
        }
    }

    /// The best resident picture for one reader page drawn `width` layout
    /// points wide, preferring the newest snapshot generation: a frame from
    /// before the last edit is shown until one containing it exists, and a
    /// coarse frame can never replace a refined one at the same generation.
    fn reader_frame(
        &self,
        page: pulpit_core::page::PageIndex,
        width: f32,
    ) -> Option<iced::widget::image::Handle> {
        let key = self.ready_reader_frame_key(page, width)?;
        let washes = self.reader.retained_washes(page);
        // A patch drawn for a frame of another size belongs to a page that has
        // since been resized; it is dropped rather than stretched.
        let patch = self
            .reader_patches
            .get(&page)
            .filter(|patch| patch.frame_width == key.width && patch.frame_height == key.height);
        if !washes.is_empty() || patch.is_some() {
            if let Some(composed) = self.composited_frame(key, page, &washes, patch) {
                return Some(composed);
            }
        }
        self.handles.get(&key).cloned()
    }

    /// Ask the document worker to draw the rectangle an edit changed (§9.4).
    ///
    /// Only for edits no preview can stand in for, and only for the frame the
    /// reader is actually looking at: a patch is worth a round trip because it
    /// replaces a *snapshot* round trip plus a reopen plus a cold render of
    /// every visible page, and it is worth nothing at all for a page nobody
    /// can see.
    fn ask_reader_patch(&mut self, applied: &pulpit_render::document::Applied) {
        let Some(dirty) = applied.dirty_region else {
            return;
        };
        // One page, because a rectangle belongs to one page. A transaction
        // that touched several waits for the snapshot, as it did before.
        let [page] = applied.dirty_pages[..] else {
            return;
        };
        let Some((surface_width, _)) = self.page_surface_size() else {
            return;
        };
        let Some(key) = self.ready_reader_frame_key(page, surface_width) else {
            return;
        };
        let Some(geometry) = self.reader.page_geometry(page) else {
            return;
        };
        if geometry.width <= 0.0 || geometry.height <= 0.0 {
            return;
        }
        // A margin, in page points, so the edge of a mark's antialiasing is
        // inside the patch rather than split down the middle of a pixel by it.
        const MARGIN: f32 = 2.0;
        let left = ((dirty.left - MARGIN) / geometry.width).clamp(0.0, 1.0);
        let top = ((dirty.top - MARGIN) / geometry.height).clamp(0.0, 1.0);
        let right = ((dirty.right + MARGIN) / geometry.width).clamp(0.0, 1.0);
        let bottom = ((dirty.bottom + MARGIN) / geometry.height).clamp(0.0, 1.0);
        let region = pulpit_core::notes::Region::new(left, top, right - left, bottom - top);
        if !region.is_valid() {
            return;
        }
        let width = (region.width * key.width as f32).round() as u32;
        let height = (region.height * key.height as f32).round() as u32;
        if width == 0 || height == 0 {
            return;
        }
        self.reader_patch_pending
            .insert(page, (key.width, key.height));
        if let Some(link) = self.reader_link.as_mut() {
            link.ask(crate::reader_link::Ask::RenderPatch {
                page,
                region,
                width,
                height,
                expected_revision: applied.document_revision,
            });
        }
    }

    /// A partial repaint arrived. It is held over the page's frame until a
    /// full frame containing the same revision replaces it.
    fn reader_patch_landed(&mut self, frame: pulpit_render::document::protocol::DocumentFrame) {
        let Some((frame_width, frame_height)) = self.reader_patch_pending.remove(&frame.page)
        else {
            return;
        };
        if !frame.is_consistent() {
            return;
        }
        // Only usable if the previews on the page can be reconciled with it.
        if !self
            .reader
            .patch_landed(frame.page, frame.region, frame.revision)
        {
            return;
        }
        self.wash_cache.borrow_mut().clear();
        self.reader_patches.insert(
            frame.page,
            ReaderPatch {
                region: frame.region,
                width: frame.width,
                height: frame.height,
                frame_width,
                frame_height,
                pixels: frame.pixels,
                revision: frame.revision,
            },
        );
    }

    /// The page picture with the parts an edit changed pasted in and its
    /// retained highlights multiplied in.
    ///
    /// Two stand-ins for the same thing — a frame that predates an edit — and
    /// they compose in this order because they are not alternatives. The patch
    /// is the renderer's own pixels for a rectangle, so it goes down first and
    /// replaces what was there; the washes are pulpit's arithmetic for marks
    /// the renderer has not drawn yet, so they go on top. A wash inside a
    /// patched rectangle is not both: [`crate::reader::ReaderSession::patch_landed`]
    /// takes it down as the patch arrives, or refuses the patch.
    ///
    /// A committed `/Highlight` is blended by multiplying, so text under it
    /// stays fully dark; a translucent rectangle drawn over the frame would
    /// lighten it, and the wash would visibly settle when the real frame
    /// arrived (§9.2). Multiplying the frame's own pixels — `p·(1 − a·(1−c))`
    /// per channel, which is multiply at `/CA` — is the same arithmetic the
    /// renderer does, so the retained wash and the rendered one match.
    fn composited_frame(
        &self,
        key: FrameKey,
        page: pulpit_core::page::PageIndex,
        washes: &[&crate::widgets::document::preview::GesturePreview],
        patch: Option<&ReaderPatch>,
    ) -> Option<iced::widget::image::Handle> {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        if let Some(patch) = patch {
            patch.revision.hash(&mut hasher);
            patch.width.hash(&mut hasher);
            patch.height.hash(&mut hasher);
            for value in [
                patch.region.x,
                patch.region.y,
                patch.region.width,
                patch.region.height,
            ] {
                value.to_bits().hash(&mut hasher);
            }
        }
        for wash in washes {
            for quad in &wash.quads {
                let bounds = quad.bounds();
                for value in [bounds.left, bounds.top, bounds.right, bounds.bottom] {
                    value.to_bits().hash(&mut hasher);
                }
            }
            for value in [wash.color.0, wash.color.1, wash.color.2, wash.opacity] {
                value.to_bits().hash(&mut hasher);
            }
        }
        let stamp = hasher.finish();
        if let Some(handle) = self.wash_cache.borrow().get(&stamp) {
            return Some(handle.clone());
        }

        let (_, frame) =
            self.cache
                .best_exact(key.generation, key.slide, key.kind, key.width, key.height)?;
        let geometry = self.reader.page_geometry(page)?;
        if geometry.width <= 0.0 || geometry.height <= 0.0 {
            return None;
        }
        let scale_x = frame.width as f32 / geometry.width;
        let scale_y = frame.height as f32 / geometry.height;

        let mut pixels = (*frame.pixels).clone();

        // The renderer's own pixels for the rectangle that changed, row by
        // row into the frame. Clipped rather than trusted: the patch was sized
        // from this frame's dimensions, but a rounding disagreement at the
        // right or bottom edge must not run off the end of the buffer.
        if let Some(patch) = patch {
            let x0 = (patch.region.x * frame.width as f32).round() as usize;
            let y0 = (patch.region.y * frame.height as f32).round() as usize;
            let columns = (patch.width as usize).min((frame.width as usize).saturating_sub(x0));
            let rows = (patch.height as usize).min((frame.height as usize).saturating_sub(y0));
            for row in 0..rows {
                let from = row * patch.width as usize * 4;
                let to = ((y0 + row) * frame.width as usize + x0) * 4;
                pixels[to..to + columns * 4]
                    .copy_from_slice(&patch.pixels[from..from + columns * 4]);
            }
        }

        for wash in washes {
            let alpha = wash.opacity.clamp(0.0, 1.0);
            // 8.8 fixed point per channel; 256 is "leave the pixel alone".
            let factor = [wash.color.0, wash.color.1, wash.color.2]
                .map(|c| ((1.0 - alpha * (1.0 - c.clamp(0.0, 1.0))) * 256.0).round() as u32);
            for quad in &wash.quads {
                let bounds = quad.bounds();
                let x0 = ((bounds.left * scale_x).floor().max(0.0)) as usize;
                let y0 = ((bounds.top * scale_y).floor().max(0.0)) as usize;
                let x1 = ((bounds.right * scale_x).ceil() as usize).min(frame.width as usize);
                let y1 = ((bounds.bottom * scale_y).ceil() as usize).min(frame.height as usize);
                for y in y0..y1 {
                    let row = y * frame.width as usize;
                    for x in x0..x1 {
                        let at = (row + x) * 4;
                        for channel in 0..3 {
                            pixels[at + channel] =
                                ((u32::from(pixels[at + channel]) * factor[channel]) >> 8).min(255)
                                    as u8;
                        }
                    }
                }
            }
        }
        let handle = iced::widget::image::Handle::from_rgba(frame.width, frame.height, pixels);
        let mut cache = self.wash_cache.borrow_mut();
        // A hard cap rather than an LRU: at most a handful of pages can have
        // a highlight in flight, and a stale composite is rebuilt in one pass.
        if cache.len() >= 8 {
            cache.clear();
        }
        cache.insert(stamp, handle.clone());
        Some(handle)
    }

    fn ready_reader_frame_key(
        &self,
        page: pulpit_core::page::PageIndex,
        width: f32,
    ) -> Option<FrameKey> {
        let (_, generation) = self.reader_render_source()?;
        // The same arithmetic the plan asked in, rounded the same way. The
        // refined frame is rendered at `round(width × scale)`; a ceiling this
        // truncated instead sat a pixel *below* it whenever that product had
        // a fraction of a half or more, and `best_fitting` then preferred the
        // coarse frame that did fit — the page stayed soft for as long as it
        // was looked at, on some zoom levels and pages and not others.
        let max_width = crate::reader::rendered_width(width, self.presenter_scale_factor());
        self.cache
            .generations_at_or_below(generation)
            .into_iter()
            .find_map(|generation| {
                self.cache
                    .best_fitting(generation, page.get(), FrameKind::Page, max_width)
                    .map(|(key, _)| key)
                    .filter(|key| self.handles.contains_key(key))
            })
    }

    /// The inside of the page-surface cell in the mounted layout, if there is
    /// one.
    ///
    /// The layout tree already knows: it is the same computation the renderer
    /// does to draw the cell, run here so the reader can fit a page to a cell
    /// whose size the view never has to report back.
    fn page_surface_size(&self) -> Option<(f32, f32)> {
        let frame = crate::layout::Frame::new(
            0.0,
            0.0,
            self.presenter_size.width,
            self.presenter_size.height,
        );
        let (placements, _) = self.active_layout.compute(frame, true);
        let cell = self.active_layout.cells().into_iter().find(|cell| {
            cell.widget.as_ref().map(|widget| widget.kind())
                == Some(crate::widgets::WidgetKind::DocumentPage)
        })?;
        let placed = placements
            .iter()
            .find(|placement| placement.id == cell.id)?
            .frame;
        Some((
            (placed.width - cell.padding * 2.0).max(0.0),
            (placed.height - cell.padding * 2.0).max(0.0),
        ))
    }

    /// The keys that mean something different while a document is being read.
    ///
    /// `None` when the key is not one of them, or when the reader is not what
    /// is on screen: everything else goes on to the keymap, so a presenter's
    /// remote is never quietly rebound by opening a document.
    fn document_key(
        &mut self,
        key: Option<&str>,
        control: bool,
        shift: bool,
    ) -> Option<Task<Message>> {
        use crate::layout::builtin::LayoutMode;
        use crate::widgets::event::ReadCommand;
        use pulpit_core::annotation::AnnotationTool;

        if !self.reader.is_open() || LayoutMode::of(&self.active_layout) != LayoutMode::Document {
            return None;
        }
        let key = key?;

        // Scrolling first: it is what the keys were bound to before anything
        // else was, and a reader pressing Page Down means the page.
        let windows = match key {
            "PageDown" => Some(1),
            "PageUp" => Some(-1),
            _ => None,
        };
        if let Some(windows) = windows {
            self.reader.apply(&ReadCommand::ScrollByWindows(windows));
            return Some(self.scroll_surface_to_reader());
        }

        // History, on the keys every editor on the machine uses for it. These
        // were toolbar buttons only, which meant the one action a reader takes
        // most often was the one they had to reach for the mouse to take.
        if control && key.eq_ignore_ascii_case("z") {
            let command = if shift {
                ReadCommand::Redo
            } else {
                ReadCommand::Undo
            };
            return Some(self.on_read_command(command));
        }
        if control && key.eq_ignore_ascii_case("y") {
            return Some(self.on_read_command(ReadCommand::Redo));
        }

        // The rest are single keys, so a modifier means the press belongs to
        // some other binding and not to the toolbar.
        if control {
            return None;
        }

        // What to do with the mark that is held. Delete and Backspace both,
        // because which one removes a selection is a thing keyboards disagree
        // about; Enter opens what it says, as it does for a file name.
        if self.reader.selected().is_some() {
            match key {
                "Delete" | "Backspace" => {
                    return Some(self.on_read_command(ReadCommand::DeleteSelected))
                }
                "Enter" => return Some(self.on_read_command(ReadCommand::EditSelected)),
                // Escape puts a mark down without committing anything. It is
                // consumed here only when something is actually held, so it
                // still backs out of everything else when nothing is.
                "Escape" => return Some(self.on_read_command(ReadCommand::ClearSelection)),
                _ => {}
            }
        }

        // Arming a tool by number, in the order the toolbar draws them, plus
        // the escape hatch back to the hand. These are the reader's own keys
        // and reach the presenter's palette in neither direction (§5.3): the
        // presenter's annotation bindings arm the slide overlay, which has
        // nothing to do with the document on screen here.
        let armed = match key {
            "0" => Some(None),
            "1" => Some(Some(AnnotationTool::Select)),
            "2" => Some(Some(AnnotationTool::Ink)),
            "3" => Some(Some(AnnotationTool::Highlighter)),
            "4" => Some(Some(AnnotationTool::Text)),
            "5" => Some(Some(AnnotationTool::Note)),
            "6" => Some(Some(AnnotationTool::Eraser)),
            _ => None,
        };
        if let Some(tool) = armed {
            return Some(self.on_read_command(ReadCommand::Arm(tool)));
        }

        None
    }

    /// Put the page surface where the session says the reader is.
    ///
    /// The scrollable owns a scroll position of its own and only hears about
    /// the ones it caused. A page jump, a zoom, a key or the hand dragging
    /// the page all move the session's offset behind its back, and this is
    /// how the two are brought back together.
    fn scroll_surface_to_reader(&self) -> Task<Message> {
        iced::widget::operation::scroll_to(
            crate::widgets::document::view::page_surface_id(),
            iced::widget::operation::AbsoluteOffset {
                x: self.reader.controls().offset_x,
                y: self.reader.controls().offset,
            },
        )
    }

    /// Something the reader's widgets asked for.
    ///
    /// The split is the one §5.3 draws: the viewport belongs to the session
    /// and is answered here and now, and anything that changes the document
    /// belongs to the worker and is posted to it. Nothing is applied locally
    /// on the way — a mark that has not been committed must not be drawn as
    /// though it had been (A1, §9.2).
    fn on_read_command(&mut self, command: crate::widgets::event::ReadCommand) -> Task<Message> {
        use crate::widgets::event::ReadCommand;

        match &command {
            ReadCommand::Undo | ReadCommand::Redo => {
                let redoing = matches!(command, ReadCommand::Redo);
                let operation = if redoing {
                    self.reader.redo_operation()
                } else {
                    self.reader.undo_operation()
                };
                let Some(operation) = operation else {
                    return Task::none();
                };
                // An undo is itself a mutation and takes the same optimistic
                // revision check as any other (§6.2).
                let expected = self.reader.revision();
                self.reader_pending.push_back(PendingEdit {
                    kind: if redoing {
                        AppliedKind::Redo
                    } else {
                        AppliedKind::Undo
                    },
                    names_a_presenter_mark: false,
                    transaction: None,
                    // Not knowable until the answer says what came back.
                    urgency: crate::reader::RasterUrgency::Deferred,
                });
                if let Some(link) = self.reader_link.as_mut() {
                    link.ask(crate::reader_link::Ask::Undo {
                        expected_revision: expected,
                        operation,
                    });
                } else {
                    self.reader_pending.pop_back();
                }
                Task::none()
            }
            ReadCommand::SaveAs => self.ask_where_to_save_document(),
            // Writing a mark on the page is the same four steps the dialog
            // used to be, minus the dialog (§8.5).
            ReadCommand::ComposeMark(text) => {
                if let Some(composing) = self.composing_mark.as_mut() {
                    composing.text = text.clone();
                }
                Task::none()
            }
            ReadCommand::ComposeAsTypst(typst) => {
                if let Some(composing) = self.composing_mark.as_mut() {
                    composing.typst = *typst;
                }
                Task::none()
            }
            ReadCommand::CommitMark => self.commit_composed_mark(),
            ReadCommand::CancelMark => {
                self.composing_mark = None;
                Task::none()
            }
            ReadCommand::PageCursor { page, x, y } => {
                // The hand moves faster than the tick. Draining here means a
                // selection answer is taken up on the very next pointer
                // sample rather than waiting on the clock, so the quads land
                // at pointer rate instead of tick rate.
                self.pump_reader();
                self.reader.pointer_moved(*page, *x, *y);
                // The hand moved the document, and the scrollable has no way
                // of knowing that unless it is told.
                if self.reader.is_panning() {
                    return self.scroll_surface_to_reader();
                }
                // A drag with the highlighter is a *text* selection, and only
                // the engine knows where the text is. The query is read-only
                // and never moves the revision (§6.3); the UI draws whatever
                // came back last, which is why re-querying as the drag moves
                // is a redraw rather than a mutation.
                if let Some((page, selection)) = self.reader.pending_selection() {
                    self.ask_select_text(page, selection, false);
                }
                Task::none()
            }
            ReadCommand::PagePressed => {
                // A press an armed tool does not take belongs to the
                // document's own links and fields, and is not this path's.
                if self.reader.pointer_pressed() {
                    return Task::none();
                }
                // …unless the armed tool *places* a mark rather than drawing
                // one. Those have no gesture: the click chooses the spot and
                // the text arrives from an editor (§8.5).
                if let Some((page, at, tool)) = self.reader.placement() {
                    self.composing_mark = Some(ComposingMark {
                        page,
                        at,
                        tool,
                        text: String::new(),
                        typst: false,
                        editing: None,
                    });
                    // The caret goes to the box on the page, and nowhere else.
                    // Without this the keyboard is still wherever it was — the
                    // page-number box, or the presenter's own bindings — and
                    // typing the mark's text navigates the document instead of
                    // writing it.
                    return iced::widget::operation::focus(
                        crate::widgets::document::view::compose_input_id(),
                    );
                }
                Task::none()
            }
            ReadCommand::DeleteSelected => {
                // One mark, one transaction, one undo entry — the same shape
                // as every other edit, so taking a mark back is one press of
                // undo however it was removed (§9.1).
                if let Some(transaction) = self.reader.delete_selected() {
                    self.commit_to_document(transaction);
                }
                Task::none()
            }
            ReadCommand::EditSelected => {
                let Some(found) = self.reader.selected_editable() else {
                    return Task::none();
                };
                self.composing_mark = Some(ComposingMark {
                    page: found.page,
                    at: found.at,
                    tool: found.tool,
                    text: found.text,
                    typst: found.typst,
                    editing: Some(found.id),
                });
                iced::widget::operation::focus(crate::widgets::document::view::compose_input_id())
            }
            ReadCommand::PageDoubleClicked => {
                // Whatever the armed tool is: opening what a mark says is not
                // a tool, it is what double-clicking text means. With the text
                // tool armed the press before this one opened an empty box at
                // the click; that box is replaced by the mark's own words, and
                // an empty one places nothing anyway (§8.5).
                let Some(found) = self.reader.text_under_cursor() else {
                    return Task::none();
                };
                self.composing_mark = Some(ComposingMark {
                    page: found.page,
                    at: found.at,
                    tool: found.tool,
                    text: found.text,
                    typst: found.typst,
                    editing: Some(found.id),
                });
                iced::widget::operation::focus(crate::widgets::document::view::compose_input_id())
            }
            ReadCommand::PageReleased => {
                // The release commits on the newest answer, so take up
                // anything already waiting before asking for the last one.
                self.pump_reader();
                // One gesture, one transaction, one revision, one undo entry
                // (§9.1) — however many marks an eraser sweep took.
                match self.reader.pointer_released() {
                    crate::reader::Released::Commit(transaction) => {
                        self.commit_to_document(transaction);
                    }
                    crate::reader::Released::AwaitingSelection { page, selection } => {
                        // The quads the UI is drawing may be one query behind,
                        // and `/QuadPoints` has to describe the text that was
                        // actually selected (§7.2). So the release asks once
                        // more and the answer is what commits.
                        self.ask_select_text(page, selection, true);
                    }
                    crate::reader::Released::Nothing => {}
                }
                Task::none()
            }
            ReadCommand::PageCancelled => {
                self.reader.pointer_cancelled();
                Task::none()
            }
            _ => {
                let _needs_render = self.reader.apply(&command);
                // A page jump or a zoom moves the session's offset, and the
                // scrollable has no way of knowing that: it is told. A scroll
                // is the other direction — the widget is already where it
                // says it is, and sending it back would fight the wheel.
                if matches!(
                    command,
                    ReadCommand::GoToPage(_)
                        | ReadCommand::CommitPage
                        | ReadCommand::DragScrollHandle(_)
                        | ReadCommand::ScrollByWindows(_)
                        | ReadCommand::SetZoom(_)
                        | ReadCommand::ZoomIn
                        | ReadCommand::ZoomOut
                ) {
                    return self.scroll_surface_to_reader();
                }
                Task::none()
            }
        }
    }

    /// Place the mark that was being written (§8.5).
    ///
    /// Committing new text creates one annotation, which is one revision and
    /// one undo entry like any other. Empty text places nothing: a note
    /// nobody wrote is not a note, and it is not an error either.
    fn commit_composed_mark(&mut self) -> Task<Message> {
        let Some(composing) = self.composing_mark.take() else {
            return Task::none();
        };
        if composing.text.trim().is_empty() {
            return Task::none();
        }

        if composing.typst {
            // Typst markup has no lossless standard encoding, so §7.4 has
            // pulpit generate the appearance and keep the source: other
            // viewers show the picture, pulpit reopens the markup.
            //
            // Compiled here rather than through the closed-world worker
            // because this is one mark on a click rather than a stream of
            // edits being debounced. The world is the same closed one either
            // way: no files, no packages, no network, no clock (§12).
            let colour = self.annotation_options().text_color.rgb();
            let colour = (
                (colour.0 * 255.0) as u8,
                (colour.1 * 255.0) as u8,
                (colour.2 * 255.0) as u8,
            );
            match crate::typst_annotation::rasterise(&composing.text, 240.0, 12.0, colour, 2.0) {
                Ok(rendered) => {
                    // A rewrite keeps the mark's identity and its corner; only
                    // a new mark chooses where it goes (A3, §8.4).
                    let transaction = match &composing.editing {
                        Some(id) => self.reader.replace_typst(
                            id,
                            composing.page,
                            composing.at,
                            composing.text,
                            rendered,
                        ),
                        None => self.reader.place_typst(
                            composing.page,
                            composing.at,
                            composing.text,
                            rendered,
                        ),
                    };
                    if let Some(transaction) = transaction {
                        self.commit_to_document(transaction);
                    }
                }
                // A compile failure is the markup's, not the application's,
                // and the message is Typst's own — which is the useful one.
                Err(error) => self.notify(format!("That does not compile: {error}")),
            }
            return Task::none();
        }

        let transaction = match &composing.editing {
            Some(id) => self
                .reader
                .replace_text(id, composing.page, composing.text.clone()),
            None => {
                self.reader
                    .place_text(composing.page, composing.at, composing.tool, composing.text)
            }
        };
        if let Some(transaction) = transaction {
            self.commit_to_document(transaction);
        }
        Task::none()
    }

    /// Put back what a previous run left unsaved (§11.4).
    ///
    /// Replayed in revision order, undos and redos included, so the document
    /// ends where the last run left it rather than carrying every edit that
    /// was ever made — an edit the user took back stays taken back.
    ///
    /// Each entry is sent like any other mutation and is validated by the
    /// engine under the current limits. An entry that no longer applies —
    /// naming an annotation that is not there — is refused by the worker and
    /// reported, rather than applied to a guessed target.
    fn restore_reader_edits(&mut self) -> Task<Message> {
        let Some(recovered) = self.reader_recovery.take() else {
            return Task::none();
        };
        let entries = recovered.in_order();
        let count = entries.len();
        for entry in entries {
            match entry {
                crate::reader_journal::JournalEntry::Applied { transaction, .. } => {
                    self.commit_to_document(transaction);
                }
                crate::reader_journal::JournalEntry::Reversed { operation, .. } => {
                    // Replayed as an undo, which is what it was. The revision
                    // it expects is whatever the replay has reached, not the
                    // one it had in the run that recorded it.
                    let expected = self.reader.revision();
                    self.reader_pending.push_back(PendingEdit {
                        kind: AppliedKind::Undo,
                        names_a_presenter_mark: false,
                        transaction: None,
                        urgency: crate::reader::RasterUrgency::Deferred,
                    });
                    match self.reader_link.as_mut() {
                        Some(link) => {
                            link.ask(crate::reader_link::Ask::Undo {
                                expected_revision: expected,
                                operation: *operation,
                            });
                        }
                        None => {
                            self.reader_pending.pop_back();
                        }
                    }
                }
            }
        }
        self.notify(format!(
            "Putting back {count} unsaved {}.",
            if count == 1 { "edit" } else { "edits" }
        ));
        Task::none()
    }

    /// Start fresh: the journal goes, and the document is the file on disk.
    fn discard_reader_edits(&mut self) -> Task<Message> {
        self.reader_recovery = None;
        // The journal for *this* run has already replaced the old file, so
        // there is nothing left to remove; dropping the offer is the whole of
        // the answer.
        Task::none()
    }

    /// Move between reading the document and presenting it (§2.3).
    ///
    /// Mode is which layout is mounted, not which document is loaded: nothing
    /// is closed, no revision changes, and unsaved annotations stay in the
    /// document because that is where they are (A1). Each mode comes back to
    /// the layout it was last in, which is why the two are remembered apart.
    fn toggle_reader(&mut self) -> Task<Message> {
        use crate::layout::builtin::LayoutMode;

        let wanted = match LayoutMode::of(&self.active_layout) {
            LayoutMode::Presentation => LayoutMode::Document,
            LayoutMode::Document => LayoutMode::Presentation,
        };
        let remembered = match wanted {
            LayoutMode::Presentation => self.settings.layout.active.clone(),
            LayoutMode::Document => self.settings.layout.active_document.clone(),
        };
        // The layout this mode was last in, or its default when it has not
        // been in one yet. A remembered id that no longer names a layout —
        // a custom one that was deleted — falls back the same way.
        let id = remembered
            .map(LayoutId)
            .filter(|id| self.layouts.get(id).is_some())
            .unwrap_or_else(|| crate::layout::builtin::default_for(wanted));

        let Some(layout) = self.layouts.get(&id).cloned() else {
            self.notify(format!("The {} layout is missing.", wanted.label()));
            return Task::none();
        };
        if wanted == LayoutMode::Document && !self.reader.is_open() {
            // Saying so beats mounting a Reader over a document that is not
            // there and letting the empty page speak for itself.
            self.notify("There is no document open to read.".to_string());
            return Task::none();
        }
        self.adopt_layout(layout);
        Task::none()
    }

    /// Are these the same file on disk?
    ///
    /// Compared by canonical path where both resolve and literally otherwise,
    /// because a destination that does not exist yet cannot be canonicalised
    /// — which is the normal case for a Save As.
    fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        }
    }

    /// Ask where the annotated document should go.
    ///
    /// Save As, always — there is no "Save" (A6). The suggested name is beside
    /// the source with `-annotated` on it, which the chooser will refuse to
    /// let the user reduce back to the source's own name only by accident; the
    /// engine refuses it outright either way.
    fn ask_where_to_save_document(&mut self) -> Task<Message> {
        let Some(document) = self.documents.active() else {
            return Task::none();
        };
        let source = document.path.clone();
        let directory = source
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".to_string());
        Task::perform(
            async move {
                rfd::AsyncFileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .set_directory(directory)
                    .set_file_name(format!("{stem}-annotated.pdf"))
                    .save_file()
                    .await
                    .map(|handle| handle.path().to_path_buf())
            },
            Message::SaveDocumentTo,
        )
    }

    /// Write the annotated document where the user said.
    fn save_document_to(&mut self, destination: PathBuf) -> Task<Message> {
        // A6, checked here as well as in the engine: the two checks are not
        // redundant, because this one can say something useful about it and
        // the engine's is the one that holds when this is bypassed.
        if let Some(document) = self.documents.active() {
            if Self::same_path(&document.path, &destination) {
                self.notify(
                    "That is the document you opened. pulpit writes a copy, so choose \
                     another name."
                        .to_string(),
                );
                return Task::none();
            }
        }
        match self.reader_link.as_mut() {
            Some(link) => {
                link.ask(crate::reader_link::Ask::SaveAs {
                    destination,
                    options: pulpit_render::document::SaveOptions::verified(),
                });
            }
            None => self.notify("There is no document open to save.".to_string()),
        }
        Task::none()
    }

    /// Where the slide the presenter is annotating sits on which page.
    ///
    /// This is the bridge between the two coordinate systems (§8.7, A4): the
    /// notes mapping says which physical page the audience is looking at and
    /// which part of it, and the document engine says how big that page is
    /// after its crop box and rotation. Both are needed, and `None` when
    /// either is missing — a deck whose mapping does not reach this slide, or
    /// a document mode that failed to open. A mark that cannot be placed is
    /// not committed, rather than committed to page zero.
    fn slide_placement(&self) -> Option<pulpit_core::annotate::presenter::SlidePlacement> {
        let document = self.state.document()?;
        let source = self
            .state
            .mapping()
            .audience_source(self.state.committed(), document.pdf_pages)?;
        let page = pulpit_core::page::PageIndex(source.pdf_page);
        let geometry = self.reader.page_geometry(page)?;
        let placement =
            pulpit_core::annotate::presenter::SlidePlacement::new(page, source.region, geometry);
        placement.is_usable().then_some(placement)
    }

    /// The highlighter went down on the live slide: anchor a text selection.
    ///
    /// Returns whether the press was taken. It is refused when the slide
    /// cannot be placed on a page — an unmapped slide, or a document mode that
    /// never opened — because there is no text to select without one. That is
    /// a harder refusal than ink's, which at least leaves a mark on the screen
    /// for the length of the slide; a highlight over text nobody can find is
    /// not a lesser version of a highlight, it is nothing. Saying so once is
    /// better than a tool that silently does nothing all talk.
    fn begin_presenter_selection(&mut self, point: (f32, f32)) -> bool {
        let Some(placement) = self.slide_placement() else {
            self.warn_marks_are_not_kept();
            return false;
        };
        // The palette's colour is the mark's colour, and the highlighter's
        // opacity is what makes it a highlighter (§7.2).
        let options = self.annotation_options();
        self.presenter_interaction
            .set_highlight_style(pulpit_core::annotate::MarkStyle {
                color: options.highlight_color,
                ..pulpit_core::annotate::MarkStyle::highlighter()
            });
        self.presenter_interaction
            .arm(Some(pulpit_core::annotation::AnnotationTool::Highlighter));
        self.annotations.set_selection(None);
        self.presenter_interaction
            .begin(placement.page, placement.to_page(point))
    }

    /// The highlighter swept further: ask the engine what text that covers.
    ///
    /// Read-only, and re-asked as the drag moves. The answer is what the
    /// overlay draws, so the sweep shows the words rather than the pointer's
    /// path — one query per move event, never a mutation (§6.3).
    fn extend_presenter_selection(&mut self, point: (f32, f32)) {
        let Some(placement) = self.slide_placement() else {
            return;
        };
        if !self.presenter_interaction.extend(placement.to_page(point)) {
            return;
        }
        self.ask_presenter_selection(false);
    }

    /// Something the search pane asked for.
    ///
    /// A query change restarts the search: the in-process sources — speaker
    /// notes and the bookmark tree — are answered here and now, so the list
    /// has something in it before the first chunk of page text comes back,
    /// and the page scan is left to [`App::pump_search`].
    fn on_find_command(&mut self, command: crate::widgets::event::FindCommand) -> Task<Message> {
        use crate::widgets::event::FindCommand;
        match command {
            FindCommand::Type(typed) => {
                let query = pulpit_core::search::Query::new(
                    &typed,
                    self.search.query().case_sensitive,
                    self.search.query().whole_word,
                );
                self.restart_search(query);
                Task::none()
            }
            FindCommand::ToggleCaseSensitive | FindCommand::ToggleWholeWord => {
                let current = self.search.query();
                let case_sensitive =
                    current.case_sensitive ^ (command == FindCommand::ToggleCaseSensitive);
                let whole_word = current.whole_word ^ (command == FindCommand::ToggleWholeWord);
                let query =
                    pulpit_core::search::Query::new(current.text(), case_sensitive, whole_word);
                self.restart_search(query);
                Task::none()
            }
            FindCommand::Clear => {
                self.search.clear();
                Task::none()
            }
            FindCommand::Next => {
                let hit = self.search.advance().cloned();
                self.go_to_hit(hit)
            }
            FindCommand::Previous => {
                let hit = self.search.retreat().cloned();
                self.go_to_hit(hit)
            }
            FindCommand::Focus(index) => {
                let hit = self.search.focus(index).cloned();
                self.go_to_hit(hit)
            }
        }
    }

    /// Point the search at the open document under a new query.
    fn restart_search(&mut self, query: pulpit_core::search::Query) {
        // The page count comes from whichever half is open. In document mode
        // the reader knows it; in presentation mode the deck does.
        let pages = if self.reader.is_open() {
            self.reader.page_count()
        } else {
            self.state
                .document()
                .map(|document| document.pdf_pages)
                .unwrap_or(0)
        };
        self.search.open(pages);
        self.search.set_query(query);
        if self.search.query().is_empty() {
            return;
        }
        // Notes and bookmarks are already in this process, so they are
        // searched now rather than asked for: the box has results before the
        // first round trip, which in a long deck is the difference between
        // "instant" and "a second of nothing".
        let mut found = Vec::new();
        if let Some(document) = self.state.document() {
            if let Some(notes) = document.text_notes.as_ref() {
                found.extend(pulpit_core::search::search_notes(
                    self.search.query(),
                    notes,
                    document.pdf_pages,
                ));
            }
            if let Some(navigation) = self.navigation.get(&document.id.0) {
                found.extend(pulpit_core::search::search_outline(
                    self.search.query(),
                    &navigation.outline,
                ));
            }
        }
        self.search.absorb(found);
    }

    /// Show a hit: put its page on screen in whichever view is mounted.
    ///
    /// Navigation goes through the ordinary verbs — the reader's `GoToPage`,
    /// the presentation's preview move — rather than a second way to move,
    /// because in presentation mode a page change has an audience window on
    /// the other end of it.
    fn go_to_hit(&mut self, hit: Option<pulpit_core::search::Hit>) -> Task<Message> {
        let Some(hit) = hit else {
            return Task::none();
        };
        use crate::layout::builtin::LayoutMode;
        if LayoutMode::of(&self.active_layout) == LayoutMode::Document {
            return self.on_read_command(crate::widgets::event::ReadCommand::GoToPage(hit.page));
        }
        // In presentation mode the presenter moves and the audience does not:
        // finding a slide is looking for it, not showing it to the room.
        let slide = self.slide_showing(hit.page.get());
        self.update(Message::Nav(pulpit_core::Command::PreviewGoTo(slide)))
    }

    /// Which PDF page a given slide shows.
    ///
    /// The inverse of `slide_showing`, and asked of the mapping for the same
    /// reason: under a paired deck the grid's index and the reader's page are
    /// not the same number.
    fn page_showing(&self, slide: usize) -> pulpit_core::PageIndex {
        let pdf_pages = self
            .state
            .document()
            .map(|document| document.pdf_pages)
            .unwrap_or(0);
        let page = self
            .state
            .mapping()
            .audience_source(slide, pdf_pages)
            .map(|source| source.pdf_page)
            .unwrap_or(slide);
        pulpit_core::PageIndex(page)
    }

    /// Which slide shows a given PDF page.
    ///
    /// A search hit is a fact about a page; the presenter moves in slides, and
    /// under a paired notes mapping those are not the same number. Resolved by
    /// asking the mapping in force rather than by arithmetic on it, so a
    /// swapped or split deck lands on the slide the reader meant.
    fn slide_showing(&self, page: usize) -> usize {
        let pdf_pages = self
            .state
            .document()
            .map(|document| document.pdf_pages)
            .unwrap_or(0);
        let mapping = self.state.mapping();
        (0..self.state.slide_count())
            .find(|slide| {
                mapping
                    .audience_source(*slide, pdf_pages)
                    .is_some_and(|source| source.pdf_page == page)
            })
            .unwrap_or(page)
    }

    /// Ask for the next chunk of page text, if a search is running and the
    /// document worker is there to answer.
    ///
    /// Called from the tick, like every other round trip: a scan must not
    /// start inside a draw.
    fn pump_search(&mut self) {
        let Some((generation, pages)) = self.search.next_request() else {
            return;
        };
        let query = self.search.query().clone();
        let sent = self
            .reader_link
            .as_mut()
            .map(|link| {
                link.ask(crate::reader_link::Ask::FindText {
                    generation,
                    query,
                    from_page: pages.start,
                    to_page: pages.end,
                })
            })
            .unwrap_or(false);
        if sent {
            return;
        }
        // No document worker — presentation mode, where the render pool holds
        // the deck. It searches through the same matcher over the same text
        // layer, so what the presenter finds is what the reader would.
        let asked = self.state.document().map(|document| document.id.0);
        if let (Some(document), Some(supervisor)) = (asked, self.supervisor.as_mut()) {
            supervisor.request_find_text(document, generation, self.search.query().clone(), pages);
            return;
        }
        // Nothing open that can answer. Notes and bookmarks have already been
        // searched in this process; saying "no page text here" once is better
        // than asking nobody again on every tick.
        self.search.fail(
            generation,
            pulpit_core::search::SearchProblem::Unsupported(
                "the page text of this document is not available".into(),
            ),
        );
    }

    /// Put one text-selection query to the document worker, coalescing the
    /// drag's samples: one query in flight, one waiting, newest wins.
    ///
    /// A finalising query — the one whose answer commits — is never held
    /// back, and supersedes whatever sample was waiting: the release is the
    /// newest position by definition.
    fn ask_select_text(
        &mut self,
        page: pulpit_core::page::PageIndex,
        selection: pulpit_render::document::TextSelection,
        finalising: bool,
    ) {
        if finalising {
            self.selection_query_waiting = None;
        } else if self.selection_query_in_flight {
            self.selection_query_waiting = Some((page, selection));
            return;
        }
        if let Some(link) = self.reader_link.as_mut() {
            if link.ask(crate::reader_link::Ask::SelectText {
                page,
                selection,
                finalising,
            }) {
                self.selection_query_in_flight = true;
            }
        }
    }

    /// A selection query was answered (or will never be): the next waiting
    /// sample, if any, goes out now.
    fn selection_query_answered(&mut self) {
        self.selection_query_in_flight = false;
        if let Some((page, selection)) = self.selection_query_waiting.take() {
            self.ask_select_text(page, selection, false);
        }
    }

    /// Put the open presenter selection to the engine.
    ///
    /// `finalising` marks the query whose answer commits: the quads on screen
    /// may be one round trip behind the hand, and `/QuadPoints` has to
    /// describe the text that was actually selected (§7.2). Same reasoning,
    /// and same request, as the reader's release.
    fn ask_presenter_selection(&mut self, finalising: bool) {
        let Some((page, anchor, head)) = self.presenter_interaction.pending_selection() else {
            return;
        };
        let selection = pulpit_render::document::TextSelection::Range { anchor, head };
        self.ask_select_text(page, selection, finalising);
    }

    /// The engine answered a presenter selection query.
    ///
    /// Returns whether the answer was this path's — the reader and the
    /// presenter ask the same question of the same worker, and only one of
    /// them can have a selection open, because there is one pointer.
    fn presenter_selection_resolved(
        &mut self,
        quads: Vec<pulpit_core::page::PageQuad>,
        text: String,
        finalising: bool,
    ) -> bool {
        if self.presenter_interaction.pending_selection().is_none() {
            return false;
        }
        let Some(placement) = self.slide_placement() else {
            return false;
        };
        self.presenter_interaction
            .set_selection_result(quads.clone(), text);
        if !finalising {
            // Still sweeping: show the words, commit nothing.
            let runs = quads
                .iter()
                .map(|quad| placement.quad_to_slide(quad))
                .collect::<Vec<_>>();
            let style = self.presenter_interaction.highlight_style();
            let selection = pulpit_core::annotation::SlideSelection {
                runs,
                color: style.color,
                opacity: style.opacity,
            };
            self.annotations
                .set_selection((!selection.runs.is_empty()).then_some(selection));
            return true;
        }
        // The sweep is over and this is the answer that commits. The live
        // selection goes first: from here the mark is the document's, and a
        // copy of it on the overlay would be a second representation (A1).
        self.annotations.set_selection(None);
        let geometry = self.reader.page_geometry(placement.page).cloned();
        let outcome = match geometry {
            Some(geometry) => self.presenter_interaction.finish(&geometry),
            None => {
                self.presenter_interaction.cancel();
                pulpit_core::annotate::GestureOutcome::Nothing
            }
        };
        match outcome {
            pulpit_core::annotate::GestureOutcome::Commit(commands) => {
                let transaction =
                    pulpit_render::document::DocumentTransaction::from_annotations(commands);
                if !self.commit_to_document(transaction) {
                    self.warn_marks_are_not_kept();
                }
            }
            pulpit_core::annotate::GestureOutcome::Nothing => {
                self.notify(
                    "There is no selectable text there, so there is nothing to highlight."
                        .to_string(),
                );
            }
        }
        true
    }

    /// A completed presenter gesture becomes an annotation in the open
    /// document (§14.3 step 4).
    ///
    /// Called on every pointer release, because that is when a gesture is
    /// over. Everything it might have produced goes in *one* transaction:
    /// a stroke that was drawn, and the marks an eraser sweep took. One
    /// gesture is one revision and one undo entry (§9.1, criterion 8),
    /// whether the presenter drew one line or swept through eleven.
    fn commit_presenter_gesture(&mut self, finished: Option<pulpit_core::InkStroke>) {
        use pulpit_core::annotate::AnnotationCommand;

        let erased = self.annotations.take_erased();
        if finished.is_none() && erased.is_empty() {
            return;
        }
        let mut commands: Vec<AnnotationCommand> = erased
            .into_iter()
            .map(|id| AnnotationCommand::Delete { id })
            .collect();

        let mut expects_a_name = false;
        if let Some(stroke) = finished {
            match self.slide_placement() {
                Some(placement) => {
                    if let Some(draft) =
                        pulpit_core::annotate::presenter::stroke_to_draft(&stroke, &placement)
                    {
                        commands.push(AnnotationCommand::Create(draft));
                        expects_a_name = true;
                    }
                }
                None => {
                    // The mark stays on the screen for this slide, because
                    // taking it away under the presenter's hand would be
                    // worse. It will not come back, and saying so once is
                    // better than letting them find out after the talk.
                    self.warn_marks_are_not_kept();
                }
            }
        }
        if commands.is_empty() {
            return;
        }
        let transaction = pulpit_render::document::DocumentTransaction::from_annotations(commands);
        if self.commit_to_document(transaction) {
            if let Some(pending) = self.reader_pending.back_mut() {
                pending.names_a_presenter_mark = expects_a_name;
            }
        } else if expects_a_name {
            self.warn_marks_are_not_kept();
        }
    }

    /// Say once, per document, that marks made here are not being kept.
    ///
    /// Once: a presenter who draws thirty strokes over a deck pulpit cannot
    /// annotate does not need thirty toasts, and the thirtieth would be over
    /// the slide.
    fn warn_marks_are_not_kept(&mut self) {
        if self.warned_marks_are_not_kept {
            return;
        }
        self.warned_marks_are_not_kept = true;
        self.notify(
            "These marks are on the screen only: this document cannot be annotated, so \
             there is nothing to save them into."
                .to_string(),
        );
    }

    /// Take every mark off this slide, and out of the document.
    ///
    /// One transaction, so "clear" is one undo (§9.1) — a presenter who wipes
    /// a slide by accident presses undo once, not once per stroke.
    ///
    /// Only what is *on this slide*: for a split-page deck that is the marks
    /// on the slide half of the page, and the notes half is left alone.
    fn clear_marks_on_this_slide(&mut self) {
        use pulpit_core::annotate::AnnotationCommand;

        self.annotations.settle();
        let ids: Vec<_> = self
            .annotations
            .strokes
            .iter()
            .filter_map(|stroke| stroke.id.clone())
            .chain(
                self.annotations
                    .texts
                    .iter()
                    .filter_map(|mark| mark.annotation.clone()),
            )
            .collect();
        self.annotations.clear();
        if ids.is_empty() {
            return;
        }
        self.commit_to_document(
            pulpit_render::document::DocumentTransaction::from_annotations(
                ids.into_iter()
                    .map(|id| AnnotationCommand::Delete { id })
                    .collect::<Vec<_>>(),
            ),
        );
    }

    /// Ask the engine what is on the page this slide is showing.
    ///
    /// Sent on every page turn. The answer arrives as `Told::Annotations` and
    /// is adopted then; if document mode is not open there is no answer and no
    /// marks, which is the same thing said two ways.
    fn request_marks_for_this_slide(&mut self) {
        let Some(placement) = self.slide_placement() else {
            return;
        };
        if let Some(link) = self.reader_link.as_mut() {
            link.ask(crate::reader_link::Ask::ListAnnotations {
                page: placement.page,
            });
        }
    }

    /// Take what the document says is on this slide and draw that.
    ///
    /// The other half of A1: the marks a presenter sees are a *view* of the
    /// annotations in the open document, so a mark made in document mode, or
    /// one that was in the PDF before pulpit opened it, appears on the slide
    /// like any other.
    fn adopt_document_marks(&mut self, page: pulpit_core::page::PageIndex) {
        use pulpit_core::annotate::presenter;

        let Some(placement) = self.slide_placement() else {
            return;
        };
        if placement.page != page {
            // An answer about a page the presenter has since left. Dropped
            // rather than drawn: it would put the previous slide's marks over
            // this one, which is the exact failure the old per-slide cache
            // existed to avoid and this must not reintroduce.
            return;
        }
        let strokes = self
            .reader
            .annotations_on(page)
            .filter(|summary| {
                // Ink is what a slide draws. A highlight over text, a note, a
                // stamp: those are document marks, and the presenter's overlay
                // has no way to draw them that would not be a second, worse
                // rendering of what the page already shows.
                summary.kind == pulpit_core::annotate::AnnotationKind::Ink
                    && !summary.geometry_elided
            })
            .filter_map(|summary| {
                presenter::ink_to_stroke(
                    summary.id.clone(),
                    page,
                    &summary.path,
                    summary.style.color,
                    summary.style.width,
                    presenter::kind_of(&summary.style),
                    &placement,
                )
            })
            .collect();
        self.annotations.adopt(strokes);
    }

    /// Post one atomic user action to the document worker.
    ///
    /// One transaction is one revision and one undo entry, whatever it
    /// contains (§9.1) — an eraser sweep that took eleven marks included.
    fn commit_to_document(
        &mut self,
        transaction: pulpit_render::document::DocumentTransaction,
    ) -> bool {
        if transaction.is_empty() {
            return false;
        }
        let expected = self.reader.revision();
        // Keep drawing what this commit creates until a frame containing it
        // arrives (§9.2): the stroke must not vanish at release and reappear
        // a snapshot round trip later. The answer also says whether that frame
        // is owed *soon* or merely eventually.
        let urgency = self.reader.retain_commit(&transaction);
        self.reader_pending.push_back(PendingEdit {
            kind: AppliedKind::Edit,
            names_a_presenter_mark: false,
            transaction: Some(transaction.clone()),
            urgency,
        });
        let sent = match self.reader_link.as_mut() {
            Some(link) => link.ask(crate::reader_link::Ask::Apply {
                expected_revision: expected,
                transaction,
            }),
            None => false,
        };
        if !sent {
            self.reader_pending.pop_back();
            self.reader.commit_refused();
        }
        sent
    }

    /// The scale a rendered page is drawn at, so a frame is made at the
    /// resolution it is shown at rather than upscaled.
    fn presenter_scale_factor(&self) -> f32 {
        // The presenter window's own scale, which the application already
        // tracks for slide frames. A page drawn at logical size on a 2×
        // display is soft, and a reader looks at one for an hour.
        if self.presenter_scale.is_finite() && self.presenter_scale > 0.0 {
            self.presenter_scale
        } else {
            1.0
        }
    }

    /// Reconcile displays and perform the resulting actions.
    fn reconcile(&mut self) -> Task<Message> {
        if self.coordinator.snapshot.is_empty() {
            self.coordinator.refresh();
        }
        let snapshot = self.coordinator.snapshot.clone();
        let roles = self.coordinator.roles.clone();
        let capabilities = self.coordinator.capabilities;
        let windows = self.coordinator.windows.clone();

        let mut outcome =
            match self
                .coordinator
                .reconciler
                .reconcile(&snapshot, &roles, capabilities, &windows)
            {
                Reconciliation::Applied(outcome) => outcome,
                Reconciliation::Unchanged => return Task::none(),
                Reconciliation::Stale { sequence, newest } => {
                    self.diagnostics.note(format!(
                        "ignored stale topology #{sequence} (newest #{newest})"
                    ));
                    return Task::none();
                }
            };

        // The pure reconciler always models two roles. Before Start (and
        // after Stop), keep resolving those roles for the menu but suppress
        // every action or warning that assumes an audience window was asked
        // for. This is the application-level lifecycle boundary.
        if !self.audience_started {
            outcome
                .actions
                .retain(|action| action.role() != Role::Audience);
            outcome.warnings.retain(|warning| {
                !matches!(
                    warning,
                    pulpit_display::Warning::NoSecondaryDisplay
                        | pulpit_display::Warning::SharedDisplay
                        | pulpit_display::Warning::AmbiguousSelection {
                            role: Role::Audience,
                            ..
                        }
                        | pulpit_display::Warning::SelectedDisplayMissing {
                            role: Role::Audience
                        }
                        | pulpit_display::Warning::CannotLeaveFullscreen {
                            role: Role::Audience
                        }
                        | pulpit_display::Warning::AwaitingFirstFrame
                )
            });
        }

        self.coordinator.resolved = outcome.resolved;
        self.diagnostics
            .record_roles(&roles.presenter, &roles.audience);
        self.diagnostics.record_outcome(&outcome);
        // Display warnings go to the corner and to the diagnostics bundle,
        // which is the durable record. Standing conditions — no second
        // display, a saved display missing — stay up while they are true;
        // events fade.
        let mut conditions = Vec::new();
        for warning in &outcome.warnings {
            let text = describe_warning(warning);
            tracing::warn!(target: "pulpit::display", "{text}");
            self.diagnostics.note(format!("display: {text}"));
            if !shows_a_notice(warning) {
                continue;
            }
            if warning.is_condition() {
                conditions.push((warning.key(), text, advice(warning)));
            } else {
                self.toasts.warning(text, self.now);
            }
        }
        self.toasts.set_conditions(&conditions);

        // Does this outcome change what the audience window is doing? Asked
        // before the actions are carried out, because the answer decides
        // whether the focus has to be pulled back afterwards.
        let audience_mode_changed = outcome.actions.iter().any(|action| match action {
            DisplayAction::Place { role, mode, .. } => {
                *role == Role::Audience && *mode != self.coordinator.windows.audience.mode
            }
            DisplayAction::Show { role } | DisplayAction::Unfullscreen { role } => {
                *role == Role::Audience
            }
        });

        let mut tasks = Vec::new();
        for action in &outcome.actions {
            match action {
                DisplayAction::Place {
                    role,
                    identity,
                    mode,
                    ..
                } => {
                    let outcome = match self.coordinator.native(*role) {
                        Some(native) => self.coordinator.backend.place(native, identity, *mode),
                        // The window is not mapped yet, so there is no native
                        // id to place. Queue it: this is the pre-map case.
                        None => pulpit_display::PlacementOutcome::Refused,
                    };
                    let placed = matches!(outcome, pulpit_display::PlacementOutcome::Applied);
                    if !placed && self.coordinator.capabilities.can_place() {
                        // Retry after the window is mapped rather than giving
                        // up on the first refusal.
                        self.placement_retries.retain(|retry| retry.role != *role);
                        self.placement_retries.push(PlacementRetry {
                            role: *role,
                            identity: identity.clone(),
                            mode: *mode,
                            attempt: 1,
                            due: self.now + PLACEMENT_RETRY_DELAY,
                        });
                    } else if !placed {
                        // Backends that cannot place (Wayland, tiling) still
                        // get their window mode set below, so no error toast
                        // here.
                        if let Some(message) = display::describe_placement(&outcome) {
                            self.diagnostics.note(format!("display: {message}"));
                        }
                    }
                    // Whether or not targeted placement worked, the window's
                    // own mode is still ours to set.
                    if let Some(id) = self.window_id(*role) {
                        tasks.push(window::set_mode::<Message>(id, display::iced_mode(*mode)));
                    }
                    if !placed && *mode == WindowMode::Fullscreen {
                        self.diagnostics
                            .note("targeted placement unavailable; used toolkit fullscreen");
                    }
                }
                DisplayAction::Show { role } => {
                    if let Some(id) = self.window_id(*role) {
                        // Showing is a mode change in Iced. Preserve the mode
                        // planned in this same reconciliation instead of
                        // briefly undoing fullscreen while mapping.
                        let mode = outcome
                            .actions
                            .iter()
                            .find_map(|action| match action {
                                DisplayAction::Place {
                                    role: placed_role,
                                    mode,
                                    ..
                                } if placed_role == role => Some(*mode),
                                _ => None,
                            })
                            .unwrap_or(WindowMode::Windowed);
                        tasks.push(window::set_mode::<Message>(id, display::iced_mode(mode)));
                    }
                    // A window that has not been mapped yet may not be
                    // placeable. Re-assert its placement just after mapping,
                    // so the audience window reaches the selected display
                    // without ever flashing an empty frame.
                    if self.coordinator.capabilities.can_place()
                        && !self.coordinator.capabilities.place_before_map
                    {
                        let planned = outcome.actions.iter().find_map(|action| match action {
                            DisplayAction::Place {
                                role: placed_role,
                                identity,
                                mode,
                                ..
                            } if placed_role == role => Some((identity.clone(), *mode)),
                            _ => None,
                        });
                        let current = self.coordinator.windows.get(*role);
                        let placement = planned.or_else(|| {
                            current
                                .monitor
                                .clone()
                                .map(|identity| (identity, current.mode))
                        });
                        if let Some((identity, mode)) = placement {
                            self.placement_retries.retain(|retry| retry.role != *role);
                            self.placement_retries.push(PlacementRetry {
                                role: *role,
                                identity,
                                mode,
                                attempt: 1,
                                due: self.now + PLACEMENT_RETRY_DELAY,
                            });
                        }
                    }
                }
                DisplayAction::Unfullscreen { role } => {
                    if let Some(id) = self.window_id(*role) {
                        tasks.push(window::set_mode::<Message>(id, window::Mode::Windowed));
                    }
                }
            }
        }

        // Changing the audience window's mode (fullscreen in particular)
        // makes most window managers focus it. Schedule focus repair after
        // mapping settles; doing it in this same task batch loses the race.
        if audience_mode_changed {
            self.schedule_presenter_refocus();
        }

        let mut windows = self.coordinator.windows.clone();
        apply_outcome(&mut windows, &outcome);
        self.coordinator.windows = windows;
        self.coordinator
            .reconciler
            .note_windows(&self.coordinator.windows);

        // Inhibition follows the audience output, not the application.
        let fullscreen = self.coordinator.windows.audience.mode == WindowMode::Fullscreen;
        if self.settings.display.inhibit_screensaver {
            let state = self
                .inhibitor
                .set_desired(fullscreen, self.platform.services.as_ref())
                .clone();
            self.diagnostics.note(state.describe());
        }

        Task::batch(tasks)
    }

    fn window_id(&self, role: Role) -> Option<window::Id> {
        match role {
            Role::Presenter => self.presenter_window,
            Role::Audience => self.audience_window,
        }
    }

    fn schedule_presenter_refocus(&mut self) {
        self.presenter_refocus_deadlines.clear();
        self.presenter_refocus_deadlines.extend(
            PRESENTER_REFOCUS_DELAYS
                .into_iter()
                .map(|delay| self.now + delay),
        );
    }

    /// Map the prepared audience toplevel and let ordinary reconciliation put
    /// it on the selected display.
    fn start_audience(&mut self, windowed: bool) -> Task<Message> {
        self.audience_start_menu_open = false;
        if self.audience_started {
            return Task::none();
        }
        if self.state.document().is_none() {
            self.notify("Open a document before starting the audience window.".into());
            return Task::none();
        }

        if windowed {
            self.coordinator.roles.audience_fullscreen = false;
        }
        self.audience_started = true;
        self.mark_audience_frame();
        self.request_renders();
        self.diagnostics.note(if windowed {
            "audience started windowed"
        } else {
            "audience started"
        });
        if self.audience_window.is_none() {
            self.open_audience_window()
        } else {
            self.reconcile()
        }
    }

    /// Destroy the audience toplevel. A later Start creates it afresh on the
    /// desktop context active at that moment.
    fn stop_audience(&mut self) -> Task<Message> {
        let was_active = self.audience_started;
        self.audience_started = false;
        self.audience_start_menu_open = false;
        self.presenter_refocus_deadlines.clear();
        // "Start windowed" is a one-run placement aid, not a preference
        // change. Restore the saved default for the next ordinary Start.
        self.coordinator.roles.audience_fullscreen =
            self.settings.display.roles.audience_fullscreen;
        self.coordinator.roles.allow_shared_display =
            self.settings.display.roles.allow_shared_display;
        self.placement_retries
            .retain(|retry| retry.role != Role::Audience);
        *self.coordinator.window_state_mut(Role::Audience) = WindowState::default();
        self.coordinator.set_native(Role::Audience, None);
        self.coordinator
            .reconciler
            .note_windows(&self.coordinator.windows);
        self.inhibitor.release(self.platform.services.as_ref());
        if was_active {
            self.notify_done("Audience stopped.".into());
        }
        self.audience_window
            .take()
            .map(window::close::<Message>)
            .unwrap_or_else(Task::none)
    }

    /// Create the audience hidden. Reconciliation places it and only reveals
    /// it once a complete slide frame is ready.
    fn open_audience_window(&mut self) -> Task<Message> {
        let (id, opened) = window::open(display::identify_window(
            window::Settings {
                size: self.audience_size,
                decorations: false,
                visible: false,
                ..window::Settings::default()
            },
            Role::Audience,
        ));
        self.audience_window = Some(id);
        opened.map(move |id| Message::WindowOpened {
            role: Role::Audience,
            id,
        })
    }

    /// Tell the presenter something, without taking layout space.
    ///
    /// Everything shown as a toast is also written to the diagnostics
    /// bundle, so a transient notice is never the only record of it.
    fn notify(&mut self, message: String) {
        tracing::info!(message);
        self.diagnostics.note(message.clone());
        self.toasts.warning(message, self.now);
    }

    /// A failure the presenter must see and act on. It stays until dismissed.
    fn notify_error(&mut self, message: String, action: Option<String>) {
        tracing::warn!(message);
        self.diagnostics.note(format!("problem: {message}"));
        self.toasts.error(message, action, self.now);
    }

    /// A quiet confirmation that fades by itself.
    fn notify_done(&mut self, message: String) {
        tracing::info!(message);
        self.diagnostics.note(message.clone());
        self.toasts.info(message, self.now);
    }

    /// Mark the settings changed. The write happens from the tick, at most
    /// every couple of seconds, and unconditionally on quit — a keystroke in
    /// the colour editor must not cost a TOML serialise and an fsync.
    fn persist(&mut self) {
        self.settings_dirty = true;
    }

    /// Write the settings out if they changed. The write itself — TOML,
    /// temp file, fsync, rename — happens on a helper thread: durability
    /// discipline is worth keeping, paying for it on the UI thread is not.
    /// Writes are throttled to seconds apart, so two can never race.
    fn flush_settings(&mut self) {
        if !std::mem::take(&mut self.settings_dirty) {
            return;
        }
        let store = self.store.clone();
        let settings = self.settings.clone();
        std::thread::spawn(move || {
            if let Err(e) = store.save(&settings) {
                tracing::warn!(error = %e, "cannot save settings");
            }
        });
    }

    fn invalidate_renders(&mut self) {
        self.state.apply(Nav::InvalidateRenders, self.now);
        let generation = self.state.generation();
        if let Some(supervisor) = self.supervisor.as_mut() {
            supervisor.cancel_older_than(generation);
        }
        // Old frames are kept until the replacements land, then dropped.
        self.request_renders();
    }

    /// How the palette in the active layout says marks should be drawn, or
    /// the model's defaults when no palette is placed. The presenter can
    /// still annotate from the keyboard in that case: a widget decides how
    /// the marks look, never whether they are possible.
    pub fn annotation_options(&self) -> crate::widgets::AnnotationOptions {
        self.annotation_controls.options
    }

    /// The pointer moved over the live current slide: follow it with
    /// whatever the armed tool draws.
    fn track_annotation(&mut self, point: (f32, f32)) {
        use pulpit_core::annotation::AnnotationTool;
        match self.annotations.tool {
            Some(AnnotationTool::Pointer) => self.annotations.set_pointer(Some(point)),
            Some(AnnotationTool::Spotlight) => self.annotations.set_spotlight(Some(point)),
            Some(AnnotationTool::Ink) => {
                self.annotations.extend_stroke(point);
            }
            Some(AnnotationTool::Highlighter) => self.extend_presenter_selection(point),
            Some(AnnotationTool::Eraser) => {
                self.annotations
                    .extend_erase(point, self.annotation_options().eraser_radius);
            }
            Some(AnnotationTool::Text) => {}
            // Document-mode tools. A live slide is not a document surface, so
            // the pointer stays with links and media overlays here.
            Some(AnnotationTool::Note | AnnotationTool::Stamp | AnnotationTool::Select) => {}
            None => {}
        }
    }

    /// Take the press for the armed tool, and say whether it was taken.
    ///
    /// A press is only ever the annotations' when a tool is armed, so links
    /// and interactive media overlays keep the pointer the rest of the time.
    fn begin_annotation(&mut self) -> bool {
        use pulpit_core::annotation::AnnotationTool;
        let Some(tool) = self.annotations.tool else {
            return false;
        };
        let Some(point) = self.slide_cursor else {
            return false;
        };
        match tool {
            AnnotationTool::Pointer => self.annotations.set_pointer(Some(point)),
            AnnotationTool::Spotlight => self.annotations.set_spotlight(Some(point)),
            AnnotationTool::Ink => {
                let options = self.annotation_options();
                self.annotations
                    .begin_stroke(point, options.ink_width, options.ink_color);
            }
            // The highlighter does not draw where the hand goes: it selects
            // the page's own text and lays a `/Highlight` over the words. The
            // press only anchors the sweep; the engine says where the text is
            // (§8.2), exactly as it does in document mode.
            AnnotationTool::Highlighter => return self.begin_presenter_selection(point),
            AnnotationTool::Eraser => {
                self.annotations
                    .begin_erase(point, self.annotation_options().eraser_radius);
            }
            AnnotationTool::Text => {
                let options = self.annotation_options();
                self.annotations
                    .begin_text(point, options.text_size, options.text_color);
            }
            // Document-mode tools. The presenter palette cannot arm one
            // (`AnnotationTool::ALL`), so the press is not the annotations'.
            AnnotationTool::Note | AnnotationTool::Stamp | AnnotationTool::Select => return false,
        }
        true
    }

    /// A tool key arms its tool, or puts it down when it is already armed:
    /// one key each, and pressing it twice always returns the pointer to the
    /// document's own links.
    fn arm_from_key(&mut self, tool: AnnotationTool) -> Task<Message> {
        let wanted = (self.annotations.tool != Some(tool)).then_some(tool);
        self.update(Message::Annotate(
            crate::widgets::event::AnnotationCommand::Arm(wanted),
        ))
    }

    /// The alarm popup, and the acknowledgement of a cue that has gone off.
    ///
    /// The time is typed, four digits of it: at a lectern the presenter knows
    /// what time they hand off, and typing "1420" is one gesture where dialling
    /// towards it was a dozen.
    fn on_alarm_command(&mut self, command: crate::widgets::event::AlarmCommand) -> Task<Message> {
        use crate::widgets::event::{AlarmCommand, TimeField};
        let now = crate::view::seconds_of_day();
        match command {
            AlarmCommand::Open(open) => {
                if open && !self.alarm_controls.open {
                    // Opening on the next quarter hour: the cue a presenter
                    // wants is far more often "soon" than "at midnight", and a
                    // field that already holds a sensible time is one that can
                    // be added without typing at all.
                    self.alarm_controls
                        .set_entry_to((now + 900) / 900 * 900 % 86_400);
                }
                self.alarm_controls.open = open;
            }
            AlarmCommand::Type(TimeField::Left, typed) => {
                // Two digits are a whole hour, so the third one the presenter
                // types belongs to the minutes: the typing crosses the colon
                // by itself rather than waiting to be told with Tab.
                if self.alarm_controls.entry.type_left(&typed) {
                    return iced::widget::operation::focus(crate::view::ALARM_MINUTES.clone());
                }
            }
            AlarmCommand::Type(TimeField::Right, typed) => {
                self.alarm_controls.entry.type_right(&typed);
            }
            AlarmCommand::SetAfternoon(afternoon) => self.alarm_controls.afternoon = afternoon,
            AlarmCommand::DraftFromNow(seconds) => {
                self.alarm_controls.set_entry_to(now + seconds);
            }
            AlarmCommand::Add => {
                // A field holding something that is not a time adds nothing:
                // the popup greys the control that gets here, and this is the
                // same answer for the keyboard path to it.
                if let Some(at) = self.alarm_controls.entered() {
                    self.alarm_controls
                        .add(crate::widgets::Alarm::new(at, None));
                    self.settings.timer.alarms = self.alarm_controls.alarms.clone();
                    self.settings_dirty = true;
                }
            }
            AlarmCommand::Remove(at) => {
                self.alarm_controls.remove(at);
                self.settings.timer.alarms = self.alarm_controls.alarms.clone();
                self.settings_dirty = true;
            }
            AlarmCommand::NudgeSnooze(delta) => {
                self.alarm_controls.nudge_snooze(delta);
                self.timer_controls.snooze_minutes = self.alarm_controls.snooze_minutes;
                self.settings.timer.snooze_minutes = self.alarm_controls.snooze_minutes;
                self.settings_dirty = true;
            }
            AlarmCommand::Snooze => self.alarm_controls.snooze(now),
            AlarmCommand::Dismiss => self.alarm_controls.dismiss(),
        }
        Task::none()
    }

    /// Set which way the timer runs, and how long the talk is.
    ///
    /// The target is pushed straight through to the running timer: a
    /// presenter who dials thirty minutes mid-talk means the timer they are
    /// looking at, not the next one they start. The elapsed time is untouched
    /// by all of this — changing the length of the talk is not restarting it.
    fn on_timer_command(&mut self, command: crate::widgets::event::TimerCommand) -> Task<Message> {
        use crate::widgets::event::{TimeField, TimerCommand};
        match command {
            TimerCommand::Open(open) => {
                if open && !self.timer_controls.open {
                    // The field opens holding the length the timer is already
                    // counting to, so it can be adjusted rather than retyped.
                    self.timer_controls.sync_entry();
                }
                self.timer_controls.open = open;
                return Task::none();
            }
            // Typed lengths are taken on commit rather than per keystroke: a
            // target that changed under a half-typed "1" would have the timer
            // counting to one minute for as long as it took to type the rest.
            TimerCommand::Type(TimeField::Left, typed) => {
                if self.timer_controls.entry.type_left(&typed) {
                    return iced::widget::operation::focus(crate::view::TIMER_SECONDS.clone());
                }
                return Task::none();
            }
            TimerCommand::Type(TimeField::Right, typed) => {
                self.timer_controls.entry.type_right(&typed);
                return Task::none();
            }
            TimerCommand::CommitLength => {
                // A field holding something that is not a length sets nothing:
                // the menu greys the control that gets here, and this is the
                // same answer for the keyboard path to it.
                match self.timer_controls.entered() {
                    Some(seconds) => self.timer_controls.set_target(Some(seconds)),
                    None => return Task::none(),
                }
            }
            TimerCommand::SetCountDown(down) => self.timer_controls.set_count_down(down),
            TimerCommand::NudgeTarget(delta) => self.timer_controls.nudge_target(delta),
            TimerCommand::SetTarget(seconds) => self.timer_controls.set_target(Some(seconds)),
            TimerCommand::ClearTarget => self.timer_controls.set_target(None),
            TimerCommand::Snooze => self.timer_controls.snooze(),
            TimerCommand::Dismiss => {
                // Dismissing an overrun stops the clock as well as the pulse.
                // The presenter is saying the talk is over; a timer that went
                // on counting up in red would be answering a question nobody
                // is still asking. The elapsed time stays put — it is what
                // the talk took — and pressing play starts it again.
                self.timer_controls.dismiss_overtime();
                self.state.timer_mut().pause(self.now);
                return Task::none();
            }
            // One snooze length for both halves of the pair, wherever it was
            // set: two popups offering the same setting and disagreeing about
            // it would be worse than not offering it at all.
            TimerCommand::NudgeSnooze(delta) => {
                self.timer_controls.nudge_snooze(delta);
                self.alarm_controls.snooze_minutes = self.timer_controls.snooze_minutes;
                self.settings.timer.snooze_minutes = self.timer_controls.snooze_minutes;
                self.settings_dirty = true;
                return Task::none();
            }
        }
        // Any change to what the end of the talk means is a new end, and an
        // overrun that was acknowledged is no longer the one in front of us.
        if !matches!(command, TimerCommand::Snooze) {
            self.timer_controls.overtime_dismissed = false;
        }
        self.state.timer_mut().target = self.timer_controls.target();
        self.settings.timer.target_seconds = self
            .timer_controls
            .target_seconds
            .map(|seconds| seconds as u64);
        self.settings.timer.count_down = self.timer_controls.count_down;
        self.settings_dirty = true;
        Task::none()
    }

    fn on_annotation_command(&mut self, command: crate::widgets::event::AnnotationCommand) {
        use crate::widgets::event::AnnotationCommand;
        // Picking something in the overflow menu is done with it: the menu is
        // opened to reach one control, and leaving it up afterwards covers
        // the slide it was opened over. Only the commands that *are* the menu
        // — opening a panel, and the settings inside one — leave it up.
        if !matches!(
            command,
            AnnotationCommand::OpenOptions(_)
                | AnnotationCommand::OpenOverflow(_)
                | AnnotationCommand::SetSize(..)
                | AnnotationCommand::SetColor(..)
                | AnnotationCommand::SetPointerSpotlight(_)
                | AnnotationCommand::OpenColorWheel(_)
        ) {
            // The panel inside the menu goes with the menu; a panel opened
            // from the row itself stays, because it was not what the press
            // was reaching past.
            if self.annotation_controls.overflow {
                self.annotation_controls.open = None;
            }
            self.annotation_controls.overflow = false;
        }
        match command {
            AnnotationCommand::Arm(tool) => {
                self.annotation_controls.open = None;
                self.annotation_controls.overflow = false;
                self.annotations.arm(tool);
                match tool {
                    Some(tool) => self
                        .diagnostics
                        .note(format!("annotating: {}", tool.label())),
                    None => self
                        .diagnostics
                        .note("annotation tool put down".to_string()),
                }
            }
            AnnotationCommand::OpenOptions(tool) => {
                self.annotation_controls.open = tool;
                self.annotation_controls.wheel = None;
            }
            // The wheel replaces the options panel rather than sitting on top
            // of it: it is anchored to the same button, and the panel would
            // be underneath it saying the old colour.
            AnnotationCommand::OpenColorWheel(tool) => {
                self.annotation_controls.wheel = tool;
                if tool.is_some() {
                    self.annotation_controls.open = None;
                }
            }
            AnnotationCommand::OpenOverflow(open) => {
                self.annotation_controls.overflow = open;
                if !open {
                    self.annotation_controls.open = None;
                }
            }
            AnnotationCommand::SetSize(tool, value) => {
                match tool {
                    AnnotationTool::Ink => self.annotation_controls.options.ink_width = value,
                    AnnotationTool::Highlighter => {
                        self.annotation_controls.options.highlight_width = value
                    }
                    AnnotationTool::Eraser => {
                        self.annotation_controls.options.eraser_radius = value
                    }
                    AnnotationTool::Spotlight => {
                        self.annotation_controls.options.spotlight_radius = value
                    }
                    AnnotationTool::Pointer => {
                        self.annotation_controls.options.pointer_radius = value
                    }
                    AnnotationTool::Text => self.annotation_controls.options.text_size = value,
                    // The presenter palette draws no control for these
                    // (see `AnnotationTool::ALL`), so nothing can name one.
                    AnnotationTool::Note | AnnotationTool::Stamp | AnnotationTool::Select => {}
                }
                self.annotation_controls.options.sanitise();
            }
            AnnotationCommand::SetColor(tool, color) => {
                match tool {
                    AnnotationTool::Ink => self.annotation_controls.options.ink_color = color,
                    AnnotationTool::Highlighter => {
                        self.annotation_controls.options.highlight_color = color
                    }
                    AnnotationTool::Pointer => {
                        self.annotation_controls.options.pointer_color = color
                    }
                    AnnotationTool::Text => self.annotation_controls.options.text_color = color,
                    AnnotationTool::Spotlight
                    | AnnotationTool::Eraser
                    | AnnotationTool::Note
                    | AnnotationTool::Stamp
                    | AnnotationTool::Select => {}
                }
                // A colour chosen from the wheel is the wheel finished.
                if self.annotation_controls.wheel == Some(tool) {
                    self.annotation_controls.wheel = None;
                }
            }
            // Changing the pointer's mode while it is in hand changes what is
            // in hand: a presenter who asks for the spotlight mid-sentence
            // means now, not next time they arm it.
            AnnotationCommand::SetPointerSpotlight(spotlight) => {
                self.annotation_controls.options.pointer_spotlight = spotlight;
                let pointing = matches!(
                    self.annotations.tool,
                    Some(AnnotationTool::Pointer | AnnotationTool::Spotlight)
                );
                if pointing {
                    self.annotations
                        .arm(Some(self.annotation_controls.options.pointer_tool()));
                }
            }
            // Undo and redo are the *document's*, in both modes. There is one
            // history because there is one representation (A1, criterion 8):
            // a stroke drawn at the lectern and a highlight made in document
            // mode go back in the order they were made, and taking one back
            // restores the annotation rather than drawing a new one that
            // looks like it (§9.4). The open gesture is settled first, so
            // undo means the same thing whether or not the pen is down.
            AnnotationCommand::Undo | AnnotationCommand::Redo => {
                let redoing = matches!(command, AnnotationCommand::Redo);
                if self.annotations.has_open_gesture() {
                    self.annotations.settle();
                    let finished = self.annotations.strokes.last().cloned();
                    self.commit_presenter_gesture(finished);
                }
                // The reader's own handler is the one place that sends an
                // undo, so presentation goes through it rather than growing a
                // second copy that could drift from it. It answers with an
                // empty task: the work is a message to the worker.
                let _ = self.on_read_command(if redoing {
                    crate::widgets::event::ReadCommand::Redo
                } else {
                    crate::widgets::event::ReadCommand::Undo
                });
            }
            // Clearing takes the marks off this slide, which now means
            // deleting the annotations they are showing.
            AnnotationCommand::Clear => self.clear_marks_on_this_slide(),
            // Saving leaves the process, so it is answered with a task at the
            // message boundary rather than as a state change here.
            AnnotationCommand::Save => {}
            AnnotationCommand::ToggleAudience => {
                let visible = !self.annotations.audience_visible;
                self.annotations.set_audience_visible(visible);
                // Putting ink on the projector is worth saying out loud: it
                // is the one annotation decision the room can see and the
                // presenter cannot, from where they are standing.
                self.notify(if visible {
                    "Annotations are now on the audience screen".to_string()
                } else {
                    "Annotations are presenter-only again".to_string()
                });
            }
        }
    }

    /// The outline section the audience page falls in, if the document has
    /// an outline at all.
    ///
    /// Read from the *audience* page rather than the preview: the widget
    /// answers "where are we in the talk", which is a fact about what the
    /// room is looking at, not about what the presenter is peeking at.
    /// The outline section the audience page falls in.
    ///
    /// Memoised on (document, page): the answer is read on every presenter
    /// view pass, and walking the whole outline plus allocating a `String`
    /// twenty times a second for a value that changes on page turns was
    /// pure waste.
    fn current_section(&self) -> Option<String> {
        let document = self.state.document()?;
        let source = self.state.audience_source()?;
        let key = (document.id.0, source.pdf_page);
        if let Some((cached_key, section)) = self.section_cache.borrow().as_ref() {
            if *cached_key == key {
                return section.clone();
            }
        }
        let section = self
            .navigation
            .get(&document.id.0)?
            .section_for_page(source.pdf_page)
            .map(str::to_string);
        *self.section_cache.borrow_mut() = Some((key, section.clone()));
        section
    }

    /// Move the preview a coarse step through the deck.
    ///
    /// The audience does not move: this is scrubbing, and it commits only
    /// when the presenter says so, exactly like dragging the slider.
    /// Move about the overview grid with the arrow keys.
    ///
    /// `None` means the key was not one the grid owns, and should go on to
    /// the keymap as usual. Vertical movement is a whole row — the grid's own
    /// columns, not a guess — and a step that would fall off the end of a
    /// short last row lands on the last page rather than nowhere. Page up and
    /// page down move by a screenful of those rows, on the same reasoning.
    fn overview_key(&mut self, key: Option<&str>) -> Option<Task<Message>> {
        let count = self.state.slide_count();
        if count == 0 {
            return None;
        }
        let grid = self.overview_grid.get();
        let columns = grid.columns.max(1);
        // The selection is the preview, so moving about the grid is looking,
        // not presenting: the audience stays on the slide it is on until the
        // presenter says Return.
        let current = self.state.preview().min(count - 1);
        // Return picks the slide the grid has landed on, which is the whole
        // point of the menu, and closes it — the same thing a click on that
        // thumbnail does.
        if matches!(key?, "Enter" | "Return") {
            return Some(self.update(Message::GoToFromOverview(current)));
        }
        // How many whole rows are on screen at once, which is what a page
        // key moves by. Zero before the grid has ever been laid out; the
        // step then falls back to a single row.
        let page_rows = if grid.row_height > 0.0 {
            (grid.viewport_height / grid.row_height).floor().max(1.0) as usize
        } else {
            1
        };
        // An arrow at the edge of the grid is still an arrow the grid owns:
        // it stays put rather than falling through to the binding that would
        // move the audience behind the open menu.
        let Some(target) = grid_target(key?, current, count, columns, page_rows)? else {
            return Some(Task::none());
        };
        Some(Task::batch([
            self.update(Message::Nav(Nav::PreviewGoTo(target))),
            self.reveal_in_overview(target),
        ]))
    }

    /// Bring the selection back onto the screen the scroll has arrived at.
    ///
    /// Only the preview moves, so the audience stays where it is: this is
    /// looking around the deck, not presenting it. No scroll is issued —
    /// the selection comes to the screen, never the screen to the selection.
    fn settle_overview_selection(&mut self) -> Option<Task<Message>> {
        if !self.overview {
            return None;
        }
        let target = settled_selection(
            self.state.preview(),
            self.state.slide_count(),
            self.overview_scroll,
            self.overview_grid.get(),
        )?;
        Some(self.update(Message::Nav(Nav::PreviewGoTo(target))))
    }

    /// Scroll the overview just far enough that `slide` is on screen.
    ///
    /// Only when it is not: a selection already in view should stay where it
    /// is on screen rather than being dragged to an edge under the presenter.
    fn reveal_in_overview(&mut self, slide: usize) -> Task<Message> {
        // The presenter has just said where the selection goes, so a glide
        // that has not settled yet has nothing left to say about it.
        self.overview_settling = None;
        let grid = self.overview_grid.get();
        if grid.row_height <= 0.0 || grid.viewport_height <= 0.0 {
            return Task::none();
        }
        let row = (slide / grid.columns.max(1)) as f32;
        let top = row * grid.row_height;
        let bottom = top + grid.row_height;
        let offset = if top < self.overview_scroll {
            top
        } else if bottom > self.overview_scroll + grid.viewport_height {
            bottom - grid.viewport_height
        } else {
            return Task::none();
        };
        let offset = offset.max(0.0);
        self.overview_scroll = offset;
        // This is the presenter's own choice, so it outranks any glide still
        // in flight, and there is nothing left to settle: the selection is
        // already where it asked to be.
        self.overview_scroll_claim = Some((offset, Instant::now() + OVERVIEW_SCROLL_CLAIM));
        self.overview_settling = None;
        iced::widget::operation::scroll_to(
            crate::view::overview_scrollable(),
            iced::widget::operation::AbsoluteOffset { x: 0.0, y: offset },
        )
    }

    /// The interactive overlay under the pointer, and where inside it.
    ///
    /// Worked out in *page* space rather than panel space: `slide_cursor` has
    /// already had the letterbox and the crop undone, so the overlay's own
    /// rectangle is all that remains to divide by. That also means the answer
    /// is the same whatever size the panel happens to be.
    fn overlay_under_cursor(&self) -> Option<(pulpit_core::OverlayId, (f32, f32))> {
        // An armed annotation tool owns the slide; a press must not reach a
        // browser that would then be drawn over.
        if self.annotations.is_armed() {
            return None;
        }
        let (x, y) = self.cursor_on_page()?;
        let source = self.state.audience_source()?;
        let overlay = self.media.index().hit(source.pdf_page, x, y)?;
        let region = overlay.region;
        if region.width <= 0.0 || region.height <= 0.0 {
            return None;
        }
        Some((
            overlay.id,
            (
                (x - region.x) / region.width,
                (y - region.y) / region.height,
            ),
        ))
    }

    /// Send one routed event to the session that should receive it.
    ///
    /// Web content is told CSS pixels, which is what its own event handlers
    /// deal in; the fraction is scaled by the viewport the session was opened
    /// with so a click lands on the thing under the pointer.
    fn deliver(&mut self, routed: crate::media::Routed) {
        use crate::media::Routed;
        let Routed::ToOverlay { overlay, event } = routed else {
            return;
        };
        let Some(session) = self.media.session(overlay) else {
            return;
        };
        let Some(media) = self.media_supervisor.as_mut() else {
            return;
        };
        let (css_width, css_height) = media
            .viewport_of(session)
            .map(|viewport| viewport.css_size())
            .unwrap_or((1280, 720));
        let scaled = match event {
            pulpit_media::InputEvent::PointerMoved { x, y } => {
                let (x, y) = crate::media::overlay::to_css_pixels((x, y), css_width, css_height);
                pulpit_media::InputEvent::PointerMoved { x, y }
            }
            pulpit_media::InputEvent::PointerPressed {
                x,
                y,
                button,
                click_count,
            } => {
                let (x, y) = crate::media::overlay::to_css_pixels((x, y), css_width, css_height);
                pulpit_media::InputEvent::PointerPressed {
                    x,
                    y,
                    button,
                    click_count,
                }
            }
            pulpit_media::InputEvent::PointerReleased {
                x,
                y,
                button,
                click_count,
            } => {
                let (x, y) = crate::media::overlay::to_css_pixels((x, y), css_width, css_height);
                pulpit_media::InputEvent::PointerReleased {
                    x,
                    y,
                    button,
                    click_count,
                }
            }
            other => other,
        };
        // Pointer moves arrive at device rate — up to a kilohertz — and only
        // the newest position means anything to the page. One is held back
        // and flushed on the tick, so the pipe carries at most twenty a
        // second; anything *other* than a move flushes the held move first,
        // so a press never arrives before the motion that led to it.
        if let pulpit_media::InputEvent::PointerMoved { .. } = scaled {
            self.pending_pointer_move = Some((session, scaled));
            return;
        }
        if let Some((held_session, held)) = self.pending_pointer_move.take() {
            media.input(held_session, held);
        }
        media.input(session, scaled);
    }

    /// Send the held pointer move, if there is one.
    fn flush_pointer_move(&mut self) {
        let Some((session, event)) = self.pending_pointer_move.take() else {
            return;
        };
        if let Some(media) = self.media_supervisor.as_mut() {
            media.input(session, event);
        }
    }

    /// Route a pointer press at the overlay under the cursor.
    ///
    /// Returns true when an overlay took it, so the caller knows not to
    /// follow a PDF link underneath.
    fn press_overlay(&mut self) -> bool {
        let over = self.overlay_under_cursor();
        let taken = over.is_some();
        let routed = self
            .input_router
            .pointer_pressed(over, pulpit_media::PointerButton::Left);
        self.deliver(routed);
        taken
    }

    /// The link outlines the presenter should see.
    ///
    /// Focus is drawn even when the pointer is elsewhere, and when both land
    /// on the same link only the stronger focus mark is drawn — two outlines
    /// on one rectangle would just look like a rendering fault.
    fn link_highlights(&self) -> Vec<crate::widgets::context::LinkHighlight> {
        use crate::widgets::context::LinkHighlight;
        // An armed annotation tool owns the slide: outlining a link the press
        // will not follow would be a promise pulpit is not keeping.
        if self.annotations.is_armed() {
            return Vec::new();
        }
        let links = self.current_links();
        crate::widgets::context::links_to_highlight(self.hovered_link, self.focused_link)
            .into_iter()
            .filter_map(|(index, reason)| {
                links.get(index).map(|link| LinkHighlight {
                    rect: link.rect,
                    reason,
                })
            })
            .collect()
    }

    /// The links on the page the audience is looking at.
    fn current_links(&self) -> &[pulpit_core::PageLink] {
        let Some(document) = self.state.document().map(|d| d.id.0) else {
            return &[];
        };
        let Some(source) = self.state.audience_source() else {
            return &[];
        };
        self.links
            .get(&(document, source.pdf_page))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Map the last cursor position into page coordinates, honouring the crop.
    ///
    /// `None` when the pointer is over the letterbox rather than the page,
    /// which is the same test a press uses — one answer, so the highlight and
    /// the click can never disagree.
    fn cursor_on_page(&self) -> Option<(f32, f32)> {
        let (u, v) = self.slide_cursor?;
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return None;
        }
        let source = self.state.audience_source()?;
        Some((
            source.region.x + u * source.region.width,
            source.region.y + v * source.region.height,
        ))
    }

    fn link_under_cursor(&self) -> Option<usize> {
        let (x, y) = self.cursor_on_page()?;
        pulpit_core::document::link_at(self.current_links(), x, y)
    }

    /// Move keyboard focus to the next or previous link on this page.
    ///
    /// An armed annotation tool owns the slide, so focusing a link then would
    /// offer an affordance a press could not honour.
    fn step_link_focus(&mut self, forward: bool) -> Task<Message> {
        if self.annotations.is_armed() {
            return Task::none();
        }
        let count = self.current_links().len();
        let next = pulpit_core::document::step_link_focus(self.focused_link, count, forward);
        if next.is_none() && count == 0 {
            self.notify("This slide has no links.".to_string());
        }
        self.focused_link = next;
        Task::none()
    }

    /// Follow the focused link, if the keyboard has one.
    ///
    /// Returns `None` when nothing is focused, so the caller can fall through
    /// to whatever that key ordinarily does.
    fn follow_focused_link(&mut self) -> Option<Task<Message>> {
        let index = self.focused_link?;
        let link = self.current_links().get(index)?.clone();
        self.focused_link = None;
        Some(self.follow(link))
    }

    /// Follow the PDF link under the last cursor position, if any: an
    /// internal destination commits that slide, an external URI goes to the
    /// desktop's default handler.
    fn follow_link(&mut self) -> Task<Message> {
        // The press lands on whatever the highlight was showing: hover and
        // hit-testing go through one function, so they cannot disagree.
        let Some(index) = self.link_under_cursor() else {
            // A click that lands on no link releases a `/FitR` zoom, so the
            // presenter always has a way back to the whole page.
            if self.state.zoom().is_some() {
                return self.update(Message::Nav(Nav::SetZoom(None)));
            }
            return Task::none();
        };
        let Some(link) = self.current_links().get(index).cloned() else {
            return Task::none();
        };
        // Pressing a link is a definite choice; a stale keyboard focus
        // pointing somewhere else would only be confusing afterwards.
        self.focused_link = None;
        self.follow(link)
    }

    /// Act on one link, however it was chosen.
    fn follow(&mut self, link: pulpit_core::PageLink) -> Task<Message> {
        match link.target {
            pulpit_core::LinkTarget::Page { page, zoom } => match self.slide_for_page(page) {
                Some(slide) => {
                    // Navigate first — that clears any zoom in force — then
                    // apply the destination's own `/FitR` view, if any.
                    let goto = self.update(Message::Nav(Nav::GoTo(slide)));
                    if zoom.is_some() {
                        Task::batch([goto, self.update(Message::Nav(Nav::SetZoom(zoom)))])
                    } else {
                        goto
                    }
                }
                None => Task::none(),
            },
            pulpit_core::LinkTarget::Uri(uri) => {
                // Only schemes a presenter plausibly means to open live on
                // stage. Anything else (file:, javascript:, custom handlers)
                // is surfaced rather than executed.
                let allowed = ["http://", "https://", "mailto:"]
                    .iter()
                    .any(|scheme| uri.to_ascii_lowercase().starts_with(scheme));
                if !allowed {
                    self.notify(format!("Not opening link with unusual target: {uri}"));
                    return Task::none();
                }
                self.diagnostics.note(format!("opening link: {uri}"));
                let outcome = self.platform.services.open(&uri);
                if let Some(problem) = outcome.describe() {
                    self.notify(problem);
                }
                Task::none()
            }
        }
    }

    /// The slide whose audience content is this physical PDF page, if the
    /// current notes mapping shows it at all.
    fn slide_for_page(&self, page: usize) -> Option<usize> {
        let pages = self.state.pdf_pages();
        (0..self.state.slide_count()).find(|slide| {
            self.state
                .mapping()
                .audience_source(*slide, pages)
                .is_some_and(|source| source.pdf_page == page)
        })
    }

    fn take_pending(&mut self, id: RequestId) -> Option<FrameKey> {
        // Warming ids are tracked alongside `pending` and have to be
        // forgotten with it: a leaked one counts against the in-flight limit
        // for ever, and warming stops without a word.
        self.thumbnail_requests.remove(&id);
        let position = self
            .pending
            .iter()
            .position(|(pending, _)| *pending == id)?;
        // Whatever becomes of this request, nobody will time it again.
        self.submitted_at.remove(&id);
        Some(self.pending.remove(position).1)
    }

    /// A frame the windows may now draw: record its texture, forget whatever
    /// the budget took to make room for it, and let the display slots and the
    /// pins move.
    ///
    /// Both slots move *before* anything is pinned: `pin_visible` protects
    /// what the slots point at, so pinning first left the frame this event put
    /// on the projector unprotected until some later pass — and the largest
    /// unpinned entry in the cache is the first thing the next insert evicts.
    fn frame_ready(&mut self, key: FrameKey, handle: iced::widget::image::Handle) {
        self.handles.insert(key, handle);
        // A picture some window will want on the GPU. Re-armed by each
        // arrival, so a burst holds the fast tick once rather than per frame.
        //
        // The wall clock rather than `self.now`, which only advances on the
        // tick: a frame delivered by the doorbell between two ticks would
        // otherwise date its own deadline to the last one and settle early.
        // `is_live` compares this against `self.now`, which can only be
        // older — so the error is always toward staying live a tick too long,
        // never toward settling a tick too soon.
        self.uploads_settle_by = Some(Instant::now() + UPLOAD_SETTLE);
        for evicted in self.cache.take_evicted() {
            self.handles.remove(&evicted);
        }
        // The panel's stand-in is a view-time choice with no slot of its own,
        // so it is noticed here, by asking the very function the view asks —
        // and before the slots move below, while the slot still holds the
        // page being left, which is what makes a stand-in a stand-in.
        if let Some(key) = self.presenter_stand_in() {
            self.note_answer(crate::latency::Surface::Presenter, key);
        }
        self.remember_presenter_frame();
        self.mark_audience_frame();
        self.remember_audience_frame();
        self.pin_visible();
    }

    fn pin_visible(&mut self) {
        // Ready presenter neighbours are the next turn's ammunition. The two
        // display slots are strong logical references and must survive cache
        // pressure until a complete replacement is allocated.
        let count = self.state.slide_count();
        let committed = self.state.committed();
        let mut candidates = vec![self.last_audience, self.last_presenter];
        // Each page at the width the panel that draws it uses, and the page
        // after this one also at the current-slide width, because that is the
        // picture the next turn swaps to.
        for slide in [
            committed.checked_sub(1),
            Some(committed),
            Some(committed + 1),
        ] {
            if let Some(slide) = slide.filter(|slide| *slide < count) {
                let width = self.panel_width(slide);
                candidates.push(self.ready_frame_key(slide, FrameKind::Slide, width));
                let current = self.slide_widths().current;
                if width != current {
                    candidates.push(self.ready_frame_key(slide, FrameKind::Slide, current));
                }
            }
        }
        // The projector's own frames for the pages one step away, which is
        // what makes a page turn a swap rather than a render. Unpinned, these
        // are by a wide margin the largest unprotected entries in the cache —
        // tens of megabytes each against a panel frame's few — so the budget
        // took them first and the very frames the prefetch exists to have
        // ready were gone by the turn that wanted them. The committed page's
        // own audience frame is included: `last_audience` covers what is on
        // the projector, which during a transition is still the page before.
        let audience = self.audience_width();
        for slide in [
            Some(committed),
            committed.checked_sub(1),
            Some(committed + 1),
        ] {
            if let Some(slide) = slide.filter(|slide| *slide < count) {
                candidates.push(self.ready_frame_key(slide, FrameKind::Slide, audience));
            }
        }
        // The coarse stand-in, which either window may have on screen right
        // now. It is a megabyte against an audience frame's tens, and losing
        // it is losing the picture that is up.
        candidates.push(self.ready_frame_key(committed, FrameKind::Slide, self.coarse_width()));
        // The reader's own sheets, when the reader layout is up: each one is
        // the picture on screen, and evicting it blanks a page mid-read.
        for placed in self.reader.visible_pages() {
            candidates.push(self.ready_reader_frame_key(placed.page, placed.width));
        }
        let mut pinned = Vec::new();
        for key in candidates.into_iter().flatten() {
            if self.handles.contains_key(&key) && !pinned.contains(&key) {
                pinned.push(key);
            }
        }
        self.cache.pin(pinned);
    }

    fn mark_audience_frame(&mut self) {
        if !self.audience_started {
            return;
        }
        let has_frame = self
            .audience_frame_key()
            .is_some_and(|key| key.slide == self.state.committed());
        let window = self.coordinator.window_state_mut(Role::Audience);
        if has_frame && !window.has_frame {
            window.has_frame = true;
            // The audience window was held back until exactly this moment:
            // it is placed and shown with a valid frame, never before.
            self.needs_reconcile = true;
        }
    }

    /// The frame the audience window should display right now: the best
    /// available for the committed page in the current generation, falling
    /// back to the previous generation so a reload never blanks the output.
    pub fn audience_frame(&self) -> Option<Picture> {
        if self.state.blank() != Blank::Off {
            return None;
        }
        self.audience_frame_key().and_then(|key| self.picture(&key))
    }

    /// The exact output-sized audience frame for the committed page, the
    /// coarse stand-in while a cold page is still rendering, or the frame
    /// already on the projector.
    fn audience_frame_key(&self) -> Option<FrameKey> {
        let slide = self.state.committed();
        let width = self.audience_width();
        let previous = self
            .last_audience
            .filter(|key| self.handles.contains_key(key));

        // A projector never consumes the presenter's canonical frame. It
        // changes exactly once, when its own output-sized frame is ready.
        if let Some(key) = self.ready_frame_key(slide, FrameKind::Slide, width) {
            return Some(key);
        }
        // Nothing output-sized yet. A stale page is the right answer while the
        // *same* page sharpens, and the wrong one once the presenter has
        // jumped somewhere cold: a correct page coarsely beats a sharp picture
        // of somewhere else. That is the whole job of the coarse stand-in, and
        // the only moment either window is allowed a two-step ladder.
        //
        // Never for the first frame of a session, where there is no wrong
        // slide to correct: the projector is revealed with the real picture
        // rather than with a soft one that sharpens in front of the room.
        if self.wants_coarse_stand_in() {
            if let Some(key) = self.ready_frame_key(slide, FrameKind::Slide, self.coarse_width()) {
                return Some(key);
            }
        }
        previous
    }

    /// Whether a coarse stand-in would be shown if one existed: a window is
    /// holding some *other* page, which is the only thing the stand-in
    /// improves on.
    ///
    /// Either window is enough to ask for it, and one render answers both.
    /// The presenter panel alone is the ordinary case for a windowed session
    /// with no projector attached, where nothing would otherwise be asked for
    /// and the panel would sit on the previous page for a whole canonical
    /// render.
    fn wants_coarse_stand_in(&self) -> bool {
        [self.last_audience, self.last_presenter]
            .into_iter()
            .any(|slot| wants_stand_in(self.held(slot), self.state.committed()))
    }

    /// A display slot's frame, if it is one a window can still draw.
    fn held(&self, slot: Option<FrameKey>) -> Option<FrameKey> {
        slot.filter(|key| self.handles.contains_key(key))
    }

    /// The projector's output width in pixels.
    fn audience_width(&self) -> u32 {
        self.audience_size.width.max(320.0) as u32
    }

    /// The pages the presenter's slide panels actually draw, most urgent
    /// first.
    ///
    /// The panels are Previous, Current and Next, and every one of them is
    /// relative to the *committed* page — `slides.current` in the widget
    /// model — so three pages is the whole of what the window can show. The
    /// preview position is only a slide panel's business while the presenter
    /// is browsing ahead of the room, and its notes pane asks separately.
    ///
    /// This used to be [`priority_slides`](pulpit_core::PresentationState::priority_slides),
    /// which reaches two pages either side. That list is the renderer's
    /// warming order and a fine one, but as a source of *panel* renders it
    /// bought two frames per navigation that no layout draws — at nine
    /// megabytes each, on a deck where the cache was already full and
    /// evicting. They were rendered, cached, and thrown away before anything
    /// could use them.
    /// The width each slide panel of the active layout draws at.
    ///
    /// Computed from the layout rather than guessed, because a guess cannot
    /// be right for two panels of different sizes and a custom layout can
    /// make them any size at all. See `layout::panels`.
    fn slide_widths(&self) -> crate::layout::panels::SlideWidths {
        crate::layout::panels::slide_widths(
            &self.active_layout,
            (self.presenter_size.width, self.presenter_size.height),
            self.presenter_scale,
            self.slide_aspect(),
        )
    }

    /// Which panel draws this page, and therefore how wide its picture is.
    ///
    /// The current-slide panel shows the committed page and nothing else;
    /// every other slide panel shows a neighbour. One page can therefore be
    /// wanted at two widths at once — small in the next-slide panel now, and
    /// large in the current-slide panel after the turn — which is why both
    /// are asked for ahead of the turn rather than after it.
    fn panel_role(&self, slide: usize) -> crate::layout::panels::Role {
        if slide == self.state.committed() {
            crate::layout::panels::Role::Current
        } else {
            crate::layout::panels::Role::Neighbour
        }
    }

    fn panel_width(&self, slide: usize) -> u32 {
        self.slide_widths().for_role(self.panel_role(slide))
    }

    /// Ask the runtime what the presenter window's pixel ratio is.
    fn presenter_scale_task(&self) -> Task<Message> {
        match self.presenter_window {
            Some(id) => window::scale_factor(id).map(Message::PresenterScale),
            None => Task::none(),
        }
    }

    /// The width of the stand-in rendered before the output-sized frame.
    ///
    /// Never wider than the output itself: on a small audience window the
    /// coarse frame *is* the frame, and asking for two sizes of the same
    /// picture would be a render and a texture for nothing.
    fn coarse_width(&self) -> u32 {
        self.settings
            .rendering
            .coarse_width
            .min(self.audience_width())
    }

    /// Every picture the audience window must be able to draw without
    /// waiting: what is on the projector now — blanked or not, because
    /// unblanking must not wait for an upload — and the two pages one step
    /// away, whose audience-size frames the prefetch has already asked for.
    ///
    /// On screen first: `residency` uploads one picture per pass, so the order
    /// decides what is ready this pass and what is ready for the next turn.
    pub fn audience_resident_handles(&self) -> Vec<iced::widget::image::Handle> {
        let width = self.audience_size.width.max(320.0) as u32;
        let committed = self.state.committed();
        let count = self.state.slide_count();
        let mut keys = vec![self.audience_frame_key()];
        for slide in [Some(committed + 1), committed.checked_sub(1)] {
            if let Some(slide) = slide.filter(|slide| *slide < count) {
                keys.push(self.ready_frame_key(slide, FrameKind::Slide, width));
            }
        }
        self.resident_handles(keys)
    }

    /// Every picture the presenter window draws: the slide panels' three
    /// pages and the notes for the page being previewed.
    ///
    /// Deliberately *only* those. A window's atlas grows to hold whatever it
    /// is told to keep, and growing it copies everything already in it, so
    /// holding the whole frame cache resident here — a quarter of a gigabyte
    /// of pictures for four panels — cost a texture copy of the entire atlas
    /// every time the budget refilled it.
    pub fn presenter_resident_handles(&self) -> Vec<iced::widget::image::Handle> {
        let committed = self.state.committed();
        let count = self.state.slide_count();
        let widths = self.slide_widths();
        // The stand-in ahead of the slot it is standing in for: it is on
        // screen this pass, and the slot's own frame is by definition of the
        // page the operator has already left.
        let mut keys = vec![self.presenter_stand_in(), self.last_presenter];
        for slide in [Some(committed + 1), committed.checked_sub(1)] {
            if let Some(slide) = slide.filter(|slide| *slide < count) {
                // What the neighbour panel draws now, and what the
                // current-slide panel will draw the moment this page is
                // turned to. Uploading the second one now is what keeps that
                // turn a swap rather than a wait.
                keys.push(self.ready_frame_key(slide, FrameKind::Slide, widths.neighbour));
                if widths.current != widths.neighbour {
                    keys.push(self.ready_frame_key(slide, FrameKind::Slide, widths.current));
                }
            }
        }
        if self.state.mapping().has_notes() {
            let notes = (self.preview_size.width.max(240.0)) as u32;
            keys.push(self.ready_frame_key(self.state.preview(), FrameKind::Notes, notes));
        }
        // The reader's sheets, when the reader layout is up. A full-size page
        // is tens of megabytes — far over the threshold at which iced uploads
        // asynchronously and *skips the image* while the upload is in flight
        // — so the sharpened frame that just replaced a coarse one would
        // paint as background for a few passes: a flicker on every settle.
        // Resident, the upload happens at layout, ahead of the prepare pass
        // that would have skipped it.
        for placed in self.reader.visible_pages() {
            keys.push(self.ready_reader_frame_key(placed.page, placed.width));
        }
        self.resident_handles(keys)
    }

    /// Where a window's view reports the uploads it blocked on.
    pub fn upload_meter(&self) -> crate::latency::UploadMeter {
        self.upload_meter.clone()
    }

    /// The textures for a window's wanted frames, in order, without repeats.
    fn resident_handles(&self, keys: Vec<Option<FrameKey>>) -> Vec<iced::widget::image::Handle> {
        let mut wanted: Vec<FrameKey> = Vec::new();
        for key in keys.into_iter().flatten() {
            if !wanted.contains(&key) {
                wanted.push(key);
            }
        }
        wanted
            .iter()
            .filter_map(|key| self.handles.get(key).cloned())
            .collect()
    }

    /// Remember what the audience window is actually showing, so the next
    /// navigation has something to hold on to. Called from the update loop,
    /// where state may change; the view itself only reads.
    fn remember_audience_frame(&mut self) {
        if self.state.blank() != Blank::Off {
            return;
        }
        if let Some(key) = self.audience_frame_key() {
            if self.last_audience != Some(key) {
                tracing::debug!(
                    slide = key.slide,
                    quality = ?key.quality,
                    width = key.width,
                    "audience frame change"
                );
                self.note_answer(crate::latency::Surface::Audience, key);
            }
            self.last_audience = Some(key);
        }
    }

    /// Record that a surface's picture just changed, for the turn timing.
    ///
    /// A frame at the projector's or the panel's own width is that surface's
    /// real answer; anything narrower is the coarse stand-in. Reading the
    /// width rather than tracking a separate flag keeps this honest if the
    /// stand-in rules change: whatever is on screen is classified by what it
    /// actually is.
    fn note_answer(&mut self, surface: crate::latency::Surface, key: FrameKey) {
        let exact_width = match surface {
            crate::latency::Surface::Audience => self.audience_width(),
            crate::latency::Surface::Presenter => self.slide_widths().current,
        };
        let answer = if key.width >= exact_width {
            crate::latency::Answer::Exact
        } else {
            crate::latency::Answer::StandIn
        };
        self.latency
            .answered(surface, answer, key.slide, Instant::now());
    }

    /// What a presenter panel draws for one page.
    ///
    /// `max_width` is the panel's own width, and only notes are chosen by it:
    /// slide panels use the width their own panel draws at, because a picture
    /// that changes size changes texture, and a texture change is a blink.
    /// Current Slide reads its display slot, plus the same coarse stand-in the
    /// projector uses while a cold page renders; the other panels draw their
    /// page as soon as it is ready.
    pub fn frame_for_width(
        &self,
        slide: usize,
        kind: FrameKind,
        max_width: u32,
    ) -> Option<Picture> {
        let key = if kind == FrameKind::Slide && slide == self.state.committed() {
            self.presenter_stand_in().or_else(|| {
                self.last_presenter
                    .filter(|key| self.handles.contains_key(key))
            })
        } else {
            let width = if kind == FrameKind::Slide {
                self.panel_width(slide)
            } else {
                max_width.max(64)
            };
            self.ready_frame_key(slide, kind, width)
        };
        match key {
            Some(key) => self.picture(&key),
            // Nothing rendered for this page yet — the warmed deck thumbnail
            // rather than an empty panel. It is a stand-in, never a rung on a
            // ladder: it is replaced once, by the first real frame, and a
            // panel that has one never comes back to it.
            None if kind == FrameKind::Slide => self.thumbnail(slide),
            None => None,
        }
    }

    /// The coarse frame Current Slide shows while the canonical one renders,
    /// and only then.
    ///
    /// The presenter panel was the one surface with no stand-in at all: the
    /// projector got a correct-but-soft picture within a coarse render of the
    /// keypress while the panel the operator is actually watching held the
    /// *previous* page until the full canonical render landed — the last
    /// surface in the application to answer the key they pressed.
    ///
    /// The rules are the projector's, for the same reasons and with the same
    /// bound of one extra step:
    ///
    /// - Only while the slot holds a *different* page. A stand-in over the
    ///   right page is a downgrade, and this must never become a rung on a
    ///   ladder — climbing one per turn is what the panels used to do, and
    ///   what reads as flicker.
    /// - Never before the slot has anything, where the thumbnail already
    ///   stands in and a soft frame would be a second stand-in for the same
    ///   emptiness.
    /// - The frame is the projector's own coarse stand-in, not a render of
    ///   its own: one picture, two windows, no extra work asked of a worker
    ///   that is busy with the page being turned to.
    fn presenter_stand_in(&self) -> Option<FrameKey> {
        let committed = self.state.committed();
        if !wants_stand_in(self.held(self.last_presenter), committed) {
            return None;
        }
        self.ready_frame_key(committed, FrameKind::Slide, self.coarse_width())
    }

    /// The deck thumbnail for a page, if it belongs to the document on screen.
    fn thumbnail(&self, slide: usize) -> Option<Picture> {
        if self.thumbnails.generation() != self.state.generation() {
            return None;
        }
        self.thumbnails.get(slide).map(|handle| Picture { handle })
    }

    /// Find one immutable, ready frame for this page and render epoch.
    fn ready_frame_key(&self, slide: usize, kind: FrameKind, max_width: u32) -> Option<FrameKey> {
        let height = self.frame_shape(slide, kind, max_width).1;
        self.cache
            .generations_at_or_below(self.state.generation())
            .into_iter()
            .find_map(|generation| {
                let frame = if kind == FrameKind::Slide {
                    self.cache
                        .best_exact(generation, slide, kind, max_width, height)
                } else {
                    self.cache.best_fitting(generation, slide, kind, max_width)
                };
                frame
                    .map(|(key, _)| key)
                    .filter(|key| self.handles.contains_key(key))
            })
    }

    /// The crop and the pixel height a `width`-wide frame of this page has.
    ///
    /// One function for the plan and for every lookup, because these two
    /// numbers *are* the frame's identity: a `/FitR` zoom re-crops the
    /// committed page, and a request that computes the cropped height while a
    /// lookup assumes the whole page asks for a picture that is never found —
    /// and finds one that was never asked for.
    fn frame_shape(
        &self,
        slide: usize,
        kind: FrameKind,
        width: u32,
    ) -> (Option<pulpit_core::notes::Region>, u32) {
        let mut aspect = self
            .state
            .first_page_size()
            .map(|size| size.aspect_ratio())
            .unwrap_or(16.0 / 9.0);
        let mut crop = None;
        if kind == FrameKind::Slide && slide == self.state.committed() {
            let source = self
                .state
                .mapping()
                .audience_source(slide, self.state.pdf_pages());
            if let Some(region) = source.and_then(|source| {
                self.state
                    .zoom()
                    .and_then(|zoom| source.region.intersect(&zoom))
            }) {
                let cropped = aspect * region.width / region.height;
                if cropped > 0.0 {
                    aspect = cropped;
                    crop = Some(region);
                }
            }
        }
        (crop, (width as f32 / aspect).max(1.0) as u32)
    }

    /// Atomically advance Current Slide when its canonical texture is ready.
    fn remember_presenter_frame(&mut self) {
        let slide = self.state.committed();
        let width = self.slide_widths().current;
        let candidate = self.ready_frame_key(slide, FrameKind::Slide, width);
        let next = ready_transition(self.last_presenter, slide, candidate);
        if next != self.last_presenter {
            if let Some(key) = next {
                tracing::debug!(
                    slide = key.slide,
                    width = key.width,
                    "presenter frame change"
                );
                self.note_answer(crate::latency::Surface::Presenter, key);
            }
            self.last_presenter = next;
        }
    }

    /// The texture for a cached frame.
    ///
    /// The handle must be the *same* one every view pass: `Handle::from_rgba`
    /// mints a fresh texture id each call, so building one per frame would
    /// upload several megabytes to the GPU twenty times a second and make the
    /// image flicker as the renderer's atlas churned.
    fn picture(&self, key: &FrameKey) -> Option<Picture> {
        Some(Picture {
            handle: self.handles.get(key)?.clone(),
        })
    }

    // ------------------------------------------------------------ thumbnails

    /// The page warming should work outwards from.
    ///
    /// Normally that is the presenter's own position: the pages they are
    /// about to reach are the pages they are about to want. But while the
    /// overview is open the grid *is* the presenter's screen, and they can
    /// scroll it a long way from where they are standing — open the grid on
    /// page twelve, scroll to page two hundred, and warming from `preview()`
    /// would spend the renderer on pages thirteen and fourteen while the
    /// pages under the eye stay blank. So when the grid is open and has been
    /// laid out, the centre is the middle of what is on screen.
    fn warming_centre(&self) -> usize {
        let count = self.state.slide_count();
        if !self.overview || count == 0 {
            return self.state.preview();
        }
        visible_centre(self.overview_scroll, self.overview_grid.get(), count)
            .unwrap_or_else(|| self.state.preview())
    }

    /// Decide which pages still want a thumbnail, nearest first.
    ///
    /// Rebuilt when the document changes, and re-ordered as the presenter
    /// moves, so what arrives next is what they are most likely to look at.
    /// A whole deck's worth of `usize` is nothing; it is the *rendering* that
    /// is expensive, and that is what the ordering is protecting.
    fn plan_thumbnails(&mut self) {
        let generation = self.state.generation();
        let count = self.state.slide_count();
        // This runs on every 50 ms tick, but the plan only changes when one
        // of its inputs does: the document, the presenter's position, or a
        // thumbnail landing. On the vast majority of ticks nothing moved and
        // rebuilding, re-filtering and re-sorting the queue — hundreds of
        // lookups and a sort on a long deck — produced the identical result.
        let centre = self.warming_centre();
        let inputs = (
            generation,
            count,
            self.state.preview(),
            centre,
            self.thumbnails.len(),
            self.thumbnail_queue.len(),
        );
        if self.thumbnail_plan_inputs == Some(inputs) {
            return;
        }
        self.thumbnail_plan_inputs = Some(inputs);
        // Both halves matter. The generation changes when the document is
        // replaced or re-read, and the count when a document finishes opening
        // — which for the first document of the session happens *without* a
        // generation change, so planning on the generation alone would leave
        // the very first deck unwarmed.
        if self.thumbnail_plan != Some((generation, count)) {
            if self.thumbnails.generation() != generation {
                self.thumbnails.reset(generation);
            }
            self.thumbnail_plan = Some((generation, count));
            // One pass at one width, decided up front and chosen so the whole
            // deck fits the budget: the upgrade pass this replaces re-rendered
            // pages the grid was already showing, and every upgrade was a
            // texture swap — a visible blink — in whatever panel was standing
            // in on that thumbnail at that moment.
            //
            // The width has to be *computed* rather than picked from a pair of
            // constants. A six-hundred-page book of portrait pages overflows
            // the budget at any fixed coarse width too, and what overflow
            // means here is not a coarser grid: it is eviction, and an evicted
            // page is one nothing ever asks for again, so its cell in the grid
            // stays empty for the life of the session.
            let aspect = self
                .state
                .first_page_size()
                .map(|size| size.aspect_ratio())
                .unwrap_or(16.0 / 9.0);
            let width = fitting_thumbnail_width(count, aspect, THUMBNAIL_BUDGET_BYTES);
            self.thumbnail_plan_width = width;
            self.thumbnail_queue = (0..count)
                .filter(|s| !self.thumbnails.contains(*s))
                .map(|s| (s, width))
                .collect();
        }
        // A page can go missing after the one pass has been through it: a
        // render that failed or was cancelled frees its slot without leaving
        // a picture, and a deck too long for the budget even at the floor
        // width has its furthest pages evicted. Either way nothing above
        // would ever ask again, and the grid keeps an empty cell for the rest
        // of the session.
        //
        // So the pages around the presenter are swept once the pass has
        // drained. Bounded to a window the budget can certainly hold, which
        // is what stops a deck that overflows from chasing its own tail:
        // re-requesting the far end would only evict the near end that the
        // presenter is looking at, and then re-request that.
        if self.thumbnail_queue.is_empty() && self.thumbnail_requests.is_empty() {
            let width = self.thumbnail_plan_width;
            let reach = self.thumbnails.capacity_at(width, count).max(1) / 2;
            let first = centre.saturating_sub(reach);
            let last = centre.saturating_add(reach).min(count.saturating_sub(1));
            self.thumbnail_queue = (first..=last)
                .filter(|s| !self.thumbnails.has_at_least(*s, width))
                .map(|s| (s, width))
                .collect();
        }
        if self.thumbnail_queue.is_empty() {
            return;
        }
        self.thumbnail_queue =
            warming_order(&self.thumbnail_queue, count, centre, &self.thumbnails);
    }

    /// Submit the next thumbnail or two, if the renderer has room.
    ///
    /// Warming happens from the moment a document opens rather than when the
    /// overview is asked for, because the whole point is that pressing the
    /// key shows a finished grid. It is deliberately a trickle: the queue is
    /// only fed when nothing more important is waiting, so a deck warming in
    /// the background cannot delay a page turn.
    fn pump_thumbnails(&mut self) {
        let Some(document) = self.state.document().map(|d| d.id.0) else {
            return;
        };
        if self.thumbnail_queue.is_empty() {
            return;
        }
        let generation = self.state.generation();
        let aspect = self
            .state
            .first_page_size()
            .map(|size| size.aspect_ratio())
            .unwrap_or(16.0 / 9.0);
        // Keeping several outstanding is safe, and is what lets a long deck
        // warm at the renderer's pace rather than one page per tick. It does
        // not cost the presenter anything, because the renderer dispatches by
        // priority: an audience frame submitted a moment later is picked
        // before any of these, and the only wait it can suffer is for a
        // thumbnail already *in* a worker — one small page, tens of
        // milliseconds, behind a window that is still showing its last frame.
        if self.thumbnail_requests.len() >= THUMBNAILS_OUTSTANDING {
            return;
        }
        let mut room = THUMBNAILS_OUTSTANDING - self.thumbnail_requests.len();
        // Warming is ancillary work — until the grid is open, at which point
        // these thumbnails are not warming for later, they are the thing the
        // presenter is looking at right now. They stay below the audience and
        // the presenter's own page, which must never wait behind a grid.
        let priority = if self.overview {
            Priority::Adjacent
        } else {
            Priority::Ancillary
        };

        while room > 0 {
            let Some((slide, width)) = self.thumbnail_queue.pop_front() else {
                return;
            };
            if self.thumbnails.has_at_least(slide, width) {
                continue;
            }
            let height = (width as f32 / aspect).max(1.0) as u32;
            let key = FrameKey {
                generation,
                slide,
                kind: FrameKind::Slide,
                quality: Quality::Refined,
                width,
                height,
            };
            if self.pending.iter().any(|(_, pending)| *pending == key) {
                continue;
            }
            let Some(source) = self
                .state
                .mapping()
                .audience_source(slide, self.state.pdf_pages())
            else {
                continue;
            };
            let Some(supervisor) = self.supervisor.as_mut() else {
                return;
            };
            let id = supervisor.next_request_id();
            supervisor.submit(RenderJob {
                id,
                generation,
                document,
                page: source.pdf_page,
                region: source.region,
                width,
                height,
                priority,
                quality: Quality::Refined,
                with_annotations: false,
                region_name: String::new(),
            });
            self.pending.push((id, key));
            self.submitted_at.insert(id, Instant::now());
            self.thumbnail_requests.insert(id);
            room -= 1;
        }
    }

    /// Ask for everything the two windows need, in priority order, coarse
    /// before refined. Requests already in flight are not repeated.
    fn request_renders(&mut self) {
        let Some(document) = self.state.document().map(|d| d.id.0) else {
            return;
        };
        let generation = self.state.generation();
        let committed = self.state.committed();
        let preview = self.state.preview();

        // Widths only: every height comes from `frame_shape` below, so a zoom
        // crop's height is the same number in the plan and in every lookup.
        let audience_width = self.audience_width();
        let widths = self.slide_widths();
        let coarse_width = self.coarse_width();
        let preview_width = (self.preview_size.width.max(240.0)) as u32;

        // One exact audience frame plus one immutable presenter representation
        // for every page in the bounded navigation neighbourhood — and, ahead
        // of both, the coarse stand-in, but only on the jumps where it would
        // actually be shown. Asked for on every turn it would be a render and
        // a texture bought on the overwhelming majority of turns, where the
        // page is already prefetched and the stand-in is never displayed.
        let mut wanted = live_slide_plan(
            committed,
            self.state.slide_count(),
            audience_width,
            widths,
            (coarse_width < audience_width && self.wants_coarse_stand_in()).then_some(coarse_width),
        );
        // The pages on either side of the committed one, at audience size, so
        // stepping swaps one finished frame for another instead of waiting
        // for a render. Both directions: backwards navigation used to find
        // only a preview-sized frame and put an upscaled thumbnail on the
        // projector.
        //
        // These come *after* the preview-size frames, and at the lowest slide
        // priority, deliberately. The supervisor drains highest priority
        // first, and each of these renders costs roughly ten preview frames;
        // queued ahead of the panels' own frames (as `Priority::Next` once
        // did) they delayed every panel update by hundreds of milliseconds —
        // a visible late "pop" on each navigation. Prefetch is a background
        // luxury; the panels the presenter is looking at are not.
        let mut neighbours = Vec::new();
        if committed + 1 < self.state.slide_count() {
            neighbours.push(committed + 1);
        }
        if committed > 0 {
            neighbours.push(committed - 1);
        }
        for slide in neighbours {
            wanted.push((
                slide,
                FrameKind::Slide,
                Priority::Adjacent,
                Quality::Refined,
                audience_width,
            ));
        }
        // 5. Notes.
        if self.state.mapping().has_notes() {
            wanted.push((
                preview,
                FrameKind::Notes,
                Priority::Ancillary,
                Quality::Refined,
                preview_width,
            ));
        }

        let mut jobs = Vec::new();
        // Every key the windows want *right now*, whether or not a job is
        // submitted for it this turn. The cancellation sweep below must spare
        // exactly this set. It used to spare only the newly submitted jobs —
        // but a render already in flight is skipped by the pending check and
        // so was never in that list, which meant each navigation *cancelled
        // the still-wanted renders of the previous turn* and re-requested
        // them one event later. Under rapid navigation nothing ever finished:
        // the panels starved and the projector waited a full render latency
        // after every settle.
        let mut still_wanted: Vec<FrameKey> = Vec::new();
        for (slide, kind, priority, quality, width) in wanted {
            let source = match kind {
                FrameKind::Slide => self
                    .state
                    .mapping()
                    .audience_source(slide, self.state.pdf_pages()),
                FrameKind::Notes => self
                    .state
                    .mapping()
                    .notes_source(slide, self.state.pdf_pages()),
                // Reader pages have a plan of their own; the slide plan never
                // produces one.
                FrameKind::Page => None,
            };
            let Some(mut source) = source else { continue };
            // A `/FitR` zoom re-crops the committed slide everywhere it is
            // shown at slide size. The frame keeps the crop's own aspect, or
            // the picture would arrive stretched — and the crop is part of its
            // identity, which is why the same function answers for lookups.
            let (crop, height) = self.frame_shape(slide, kind, width);
            if let Some(region) = crop {
                source.region = region;
            }
            let key = FrameKey {
                generation,
                slide,
                kind,
                quality,
                width,
                height,
            };
            still_wanted.push(key);
            if request_is_satisfied(&self.cache, key) {
                continue;
            }
            if self.pending.iter().any(|(_, pending)| *pending == key) {
                continue;
            }
            jobs.push((key, source, priority, quality, width, height, document));
        }

        // Anything still in flight that the two windows no longer need is
        // obsolete: cancel it so a worker stops burning time on a page the
        // presenter has already navigated away from. The pause callback makes
        // this prompt even mid-render.
        let obsolete: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(id, key)| {
                // Reader page renders have a sweep of their own in
                // `request_reader_renders`, keyed to the reader's generation
                // and window; judged against the slide plan they would all
                // look obsolete and be cancelled every navigation.
                if key.kind == FrameKind::Page {
                    return false;
                }
                // A thumbnail is never in this list — it is warming work, not
                // something either window is waiting for — so it must be
                // excluded explicitly or every navigation would cancel it and
                // the deck would never warm at all.
                if self.thumbnail_requests.contains(id) {
                    return key.generation < generation;
                }
                key.generation < generation
                    || !(still_wanted.contains(key)
                        || key.slide == committed
                        || key.slide == preview
                        || key.slide == preview + 1)
            })
            .map(|(id, _)| *id)
            .collect();

        let Some(supervisor) = self.supervisor.as_mut() else {
            return;
        };
        // One retain over `pending` for the whole batch, not one per id.
        if !obsolete.is_empty() {
            let doomed: std::collections::HashSet<RequestId> = obsolete.iter().copied().collect();
            self.pending
                .retain(|(pending, _)| !doomed.contains(pending));
        }
        for id in obsolete {
            supervisor.cancel(id);
            self.thumbnail_requests.remove(&id);
            self.submitted_at.remove(&id);
        }
        for (key, source, priority, quality, width, height, document) in jobs {
            let id = supervisor.next_request_id();
            supervisor.submit(RenderJob {
                id,
                generation,
                document,
                page: source.pdf_page,
                region: source.region,
                width,
                height,
                priority,
                quality,
                with_annotations: false,
                region_name: String::new(),
            });
            self.pending.push((id, key));
            self.submitted_at.insert(id, Instant::now());
        }

        // The outline and the unsupported-feature report are per document, so
        // they are asked for once, the first time work is scheduled for it.
        if self.document_survey_requested.insert(document) {
            supervisor.request_navigation(document);
            supervisor.request_capabilities(document);
        }

        // Link annotations for the pages a click can land on, fetched once
        // per page. Cheap metadata, so the neighbourhood is fetched alongside
        // the frames rather than lazily on the first click.
        let pages = self.state.pdf_pages();
        for slide in self.state.priority_slides() {
            let Some(source) = self.state.mapping().audience_source(slide, pages) else {
                continue;
            };
            if self.links_requested.insert((document, source.pdf_page)) {
                supervisor.request_links(document, source.pdf_page);
                // Overlay declarations come from the same annotations, so
                // they are asked for on the same schedule.
                supervisor.request_overlays(document, source.pdf_page);
            }
        }

        // What is needed just changed, so what must survive changed with it.
        // Re-anchor and re-pin *now*, not on the next render event: a
        // navigation whose target is fully cached produces no events at all,
        // and until one arrived the pins protected the previous position
        // while the frames actually on screen were fair game for eviction.
        self.remember_audience_frame();
        self.remember_presenter_frame();
        self.pin_visible();
    }

    /// Regroup every page's declarations into logical overlays.
    ///
    /// Page labels, when the producer emitted them, decide where a reveal
    /// sequence ends; without them the consecutive-page equality rule does.
    /// Rebuild the overlay index and restart media servicing, if any
    /// Overlays event has arrived since the last rebuild.
    fn flush_overlay_rebuild(&mut self) {
        if !std::mem::take(&mut self.overlays_dirty) {
            return;
        }
        let diagnostics = std::mem::take(&mut self.pending_overlay_diagnostics);
        self.rebuild_overlays(diagnostics);
        self.service_media();
    }

    fn rebuild_overlays(&mut self, diagnostics: Vec<String>) {
        let generation = self.state.generation();
        self.input_router.reset();
        // The coordinator forgets what it staged when the generation moves on,
        // so this must forget what it *asked for* at the same moment —
        // otherwise an attachment the coordinator no longer holds would never
        // be requested again.
        if generation != self.media.generation() {
            self.attachments_requested.clear();
        }
        self.media.rebuild(
            generation,
            &self.overlay_declarations,
            &pulpit_core::overlay::PageLabels::default(),
            diagnostics,
        );
    }

    /// Stage whatever the overlays on the audience page still need, then
    /// open sessions for the ones that are ready.
    ///
    /// Nothing here can blank the audience: an overlay with no session shows
    /// its poster or the PDF page underneath, which is already on screen.
    fn service_media(&mut self) {
        let Some(document) = self.state.document().cloned() else {
            return;
        };
        let Some(source) = self.state.audience_source() else {
            return;
        };
        let document_dir = document
            .path
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // Ask the renderer for anything still missing. One request per
        // attachment, however many overlays reference it.
        let needs = self.media.needs(source.pdf_page, &document_dir);
        if let Some(supervisor) = self.supervisor.as_mut() {
            for (_, need) in &needs {
                if let crate::media::Need::Attachment(name) = need {
                    if self.attachments_requested.insert(name.clone()) {
                        supervisor.request_attachment(document.id.0, name);
                    }
                }
            }
        }

        // The overlay's size on screen decides the surface it is given, so a
        // browser renders at the size it will actually be shown at. The
        // viewports are computed up front for exactly the overlays on this
        // page — cloning the whole index to satisfy the borrow below cost an
        // index copy per call, on every page's overlay response.
        let panel = self.audience_size;
        let aspect = self.audience_aspect();
        let crop = source.region;
        let scale = self.audience_scale();
        let generation = self.state.generation();
        let reduce_motion = self.motion.is_reduced();
        let viewports: std::collections::HashMap<pulpit_core::OverlayId, pulpit_media::Viewport> =
            self.media
                .index()
                .on_page(source.pdf_page)
                .into_iter()
                .filter_map(|overlay| {
                    let rectangle = crate::media::place(
                        panel,
                        aspect,
                        iced::ContentFit::Contain,
                        crop,
                        overlay.region,
                    )?;
                    let (width, height) = crate::media::viewport_for(rectangle, scale);
                    Some((
                        overlay.id,
                        pulpit_media::Viewport::new(width, height, scale),
                    ))
                })
                .collect();

        let Some(media) = self.media_supervisor.as_mut() else {
            return;
        };
        // Before the runtime probes land, selection would silently choose
        // the static poster for every overlay — and that choice sticks for
        // the session. `Message::MediaProbed` calls back in here.
        if !media.probed() {
            return;
        }
        media.retire_generation(generation);
        self.media.open_ready(
            media,
            source.pdf_page,
            |id| viewports.get(&id).copied(),
            crate::media::worker_command,
            reduce_motion,
        );
        self.media.follow_page(media, source.pdf_page);
    }

    /// The aspect ratio of the page the audience is showing.
    fn audience_aspect(&self) -> f32 {
        let base = self
            .state
            .document()
            .and_then(|document| document.first_page_size)
            .map(|size| size.aspect_ratio())
            .unwrap_or(16.0 / 9.0);
        match self.state.audience_source() {
            Some(source) if source.region.height > 0.0 => {
                base * (source.region.width / source.region.height)
            }
            _ => base,
        }
    }

    fn audience_scale(&self) -> f32 {
        // One physical pixel per logical pixel until the window reports
        // otherwise; an overlay that guesses high wastes a browser's time.
        1.0
    }

    /// Drain the media supervisor, holding every complete frame.
    fn poll_media(&mut self) {
        let Some(media) = self.media_supervisor.as_mut() else {
            return;
        };
        let events = media.poll(crate::media::worker_command);
        for event in events {
            match event {
                pulpit_media::SessionEvent::Ready {
                    overlay, runtime, ..
                } => {
                    self.diagnostics
                        .note(format!("{overlay} is playing through {runtime}"));
                }
                pulpit_media::SessionEvent::Frame {
                    overlay,
                    generation,
                    sequence,
                    width,
                    height,
                    rgba,
                    ..
                } => {
                    self.media
                        .frame_ready(overlay, generation, sequence, width, height, rgba);
                }
                pulpit_media::SessionEvent::Progress {
                    overlay, progress, ..
                } => {
                    self.media.progress_reported(overlay, progress);
                }
                pulpit_media::SessionEvent::Warning {
                    overlay, warning, ..
                } => {
                    // Presenter-only: warnings never reach the audience
                    // window, and never replace the frame on screen.
                    self.diagnostics.note(format!("{overlay}: {warning}"));
                    if matches!(warning, pulpit_media::MediaWarning::ContentRestarted) {
                        self.notify("The interactive content restarted.".to_string());
                    }
                }
                pulpit_media::SessionEvent::Failed {
                    overlay,
                    error,
                    exhausted,
                    ..
                } => {
                    self.diagnostics
                        .note(format!("{overlay}: {} ({:?})", error.message, error.kind));
                    self.media.session_failed(overlay, exhausted);
                    if exhausted {
                        self.input_router.forget(overlay);
                        // A runtime that could not even start is a setup
                        // problem, not a property of the deck, and the
                        // presenter cannot diagnose a poster that silently
                        // never becomes a video. Said once per run: every
                        // overlay on the slide would otherwise report it.
                        let missing = matches!(
                            error.kind,
                            pulpit_media::MediaErrorKind::LaunchFailed
                                | pulpit_media::MediaErrorKind::Unavailable
                        );
                        if missing && !self.media_runtime_warned {
                            self.media_runtime_warned = true;
                            self.notify(format!(
                                "Media on this slide is showing its still image: {}",
                                error.message
                            ));
                        }
                    }
                }
                pulpit_media::SessionEvent::Closed { .. } => {}
            }
        }
    }
}

/// The memoised outline section: (document, page) it was computed for, and
/// the answer.
type SectionCache = std::cell::RefCell<Option<((u64, usize), Option<String>)>>;

/// The memoised scrub anchor: the panel size (as bits) it was solved for,
/// and the slider pane found at that size.
pub type ScrubAnchorCache = std::cell::RefCell<Option<((u32, u32), Option<crate::layout::Frame>)>>;

/// Everything the drawn annotation layer depends on, in one comparable
/// value: the snapshot and geometry caches are refreshed exactly when this
/// changes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MarksSignature {
    revision: u64,
    style: pulpit_core::annotation::AnnotationStyle,
    aspect: f32,
    crop: pulpit_core::notes::Region,
    accent: iced::Color,
}

/// The cache's pixel allocation, wrapped so an iced image handle can share
/// it. `Handle::from_rgba` holds `bytes::Bytes`; built from an owner, the
/// handle and the frame cache reference the *same* allocation, where a
/// `Vec` clone both copied the frame and doubled its residency for as long
/// as the handle lived.
fn shared_pixels(pixels: &std::sync::Arc<Vec<u8>>) -> bytes::Bytes {
    struct SharedPixels(std::sync::Arc<Vec<u8>>);
    impl AsRef<[u8]> for SharedPixels {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }
    bytes::Bytes::from_owner(SharedPixels(std::sync::Arc::clone(pixels)))
}

/// Should an unbound press of this key offer to be bound?
///
/// The prompt exists for one thing: a presentation remote whose buttons the
/// toolkit cannot name, which would otherwise be unusable without editing a
/// configuration file by hand. It is not for ordinary typing. Brushing a
/// letter or a digit — which is what most stray presses are — must do
/// nothing, or the offer becomes a nag in the middle of a talk.
fn offers_binding(key: Option<&str>) -> bool {
    if is_modifier(key) {
        return false;
    }
    match key {
        // Nothing was named: exactly the remote-control case.
        None => true,
        Some("unidentified") => true,
        // A single character is someone touching the keyboard.
        Some(name) => name.chars().count() > 1,
    }
}

/// Is this key a modifier?
///
/// Modifiers are pressed constantly and mean nothing on their own, so
/// offering to bind one — "“Alt” is not bound. Use it for: Next slide…" — is
/// noise in front of a live presentation. A remote's unnamed buttons, which
/// is what the binding prompt exists for, are never modifiers.
fn is_modifier(key: Option<&str>) -> bool {
    let Some(key) = key else { return false };
    matches!(
        key,
        "Alt"
            | "Control"
            | "Shift"
            | "Super"
            | "Meta"
            | "Hyper"
            | "AltGraph"
            | "CapsLock"
            | "NumLock"
            | "ScrollLock"
            | "Fn"
            | "FnLock"
            | "Symbol"
            | "SymbolLock"
    )
}

/// Escape means Back on the pages that are places rather than modes.
///
/// Settings and the layout library both take the whole window and both offer
/// a Back button as the way out, so Escape means that button — the presenter
/// is one press from the room whichever of them is open. The editor is not on
/// this list: it holds work in progress, where Escape means cancel what you
/// are doing, not leave.
///
/// `dismissable` is whatever is open *in front* of the page. That takes the
/// first press and the page takes the second, which is the ordinary reading
/// of Escape everywhere else in the application.
fn back_to_presenter_key(
    page: crate::designer::Page,
    key: Option<&str>,
    dismissable: bool,
) -> bool {
    use crate::designer::Page;
    key == Some("Escape") && !dismissable && matches!(page, Page::Settings | Page::Library)
}

/// Is this warning worth putting in front of the presenter at all?
///
/// Working on one screen is a normal way to run: rehearsing, writing the
/// talk, or presenting from a laptop before the projector is plugged in. The
/// layout already shows the audience status, so a notice about it would be
/// nagging about a state the presenter chose. Both single-screen warnings
/// stay in the log and the diagnostics bundle, where they belong.
fn shows_a_notice(warning: &pulpit_display::Warning) -> bool {
    use pulpit_display::Warning as W;
    // AwaitingFirstFrame is internal bookkeeping — the audience window is
    // held back until it has something correct to show, which is the
    // behaviour working as designed, not news for the presenter.
    !matches!(
        warning,
        W::NoSecondaryDisplay | W::SharedDisplay | W::AwaitingFirstFrame
    )
}

/// What the presenter can do about a display warning, when there is
/// something. A notice that only states a problem wastes the glance it costs.
fn advice(warning: &pulpit_display::Warning) -> Option<String> {
    use pulpit_display::Warning as W;
    let advice = match warning {
        W::NoDisplays => "pulpit will place the windows as soon as one is reported.",
        W::NoSecondaryDisplay => {
            "Connect a projector or second screen; the audience view moves to it by itself."
        }
        W::SharedDisplay => {
            "Choose another audience display from the Start menu, or connect a second screen."
        }
        W::AmbiguousAutomaticRoles { .. } | W::AmbiguousSelection { .. } => {
            "Pick the audience display directly from the Start menu."
        }
        W::SelectedDisplayMissing {
            role: Role::Audience,
        } => "Reconnect it, or pick another display from the Start menu.",
        W::SelectedDisplayMissing {
            role: Role::Presenter,
        } => {
            "Reconnect the presenter display; pulpit will recover the window to an available screen."
        }
        W::CannotLeaveFullscreen { .. } => {
            "Leave fullscreen from the window manager if you need the window back."
        }
        W::OverlappingOutputs { .. } | W::WindowRecovered { .. } | W::AwaitingFirstFrame => {
            return None
        }
    };
    Some(advice.to_string())
}

fn platform_description() -> String {
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into());
    format!("{}/{session} ({desktop})", std::env::consts::OS)
}

/// The other direction: a keymap key name as a menu should print it. Letters
/// are stored as the toolkit reports them, in lower case, and the function
/// keys inconsistently so; a key cap is upper case either way.
fn display_key(key: &str) -> String {
    match key {
        "slash" => "/".into(),
        other if other.len() == 1 => other.to_ascii_uppercase(),
        other if other.len() <= 3 && other.starts_with(['f', 'F']) => other.to_ascii_uppercase(),
        other => other.to_string(),
    }
}

/// A stable, human-readable name for a logical key, matching the strings the
/// keymap uses.
fn describe_key(key: &iced::keyboard::Key) -> Option<String> {
    use iced::keyboard::key::Named;
    use iced::keyboard::Key;
    Some(match key {
        Key::Named(named) => match named {
            Named::ArrowRight => "Right".into(),
            Named::ArrowLeft => "Left".into(),
            Named::ArrowUp => "Up".into(),
            Named::ArrowDown => "Down".into(),
            Named::PageUp => "PageUp".into(),
            Named::PageDown => "PageDown".into(),
            Named::Home => "Home".into(),
            Named::End => "End".into(),
            Named::Space => "Space".into(),
            Named::Enter => "Enter".into(),
            Named::Escape => "Escape".into(),
            Named::Tab => "Tab".into(),
            Named::Backspace => "Backspace".into(),
            Named::F1 => "F1".into(),
            Named::F5 => "F5".into(),
            Named::MediaPlayPause => "MediaPlayPause".into(),
            Named::MediaTrackNext => "MediaTrackNext".into(),
            Named::MediaTrackPrevious => "MediaTrackPrevious".into(),
            Named::BrowserForward => "BrowserForward".into(),
            Named::BrowserBack => "BrowserBack".into(),
            Named::AudioVolumeUp => "AudioVolumeUp".into(),
            Named::AudioVolumeDown => "AudioVolumeDown".into(),
            other => format!("{other:?}"),
        },
        Key::Character(text) => text.to_string(),
        Key::Unidentified => return None,
    })
}

/// The raw scancode, for remote keys the toolkit cannot name.
fn physical_scancode(physical: &iced::keyboard::key::Physical) -> Option<u32> {
    use iced::keyboard::key::{NativeCode, Physical};
    match physical {
        Physical::Code(code) => Some(*code as u32),
        Physical::Unidentified(NativeCode::Xkb(code)) => Some(*code),
        Physical::Unidentified(NativeCode::Windows(code)) => Some(*code as u32),
        Physical::Unidentified(NativeCode::MacOS(code)) => Some(*code as u32),
        Physical::Unidentified(_) => None,
    }
}

#[cfg(test)]
mod key_prompt_tests {
    use super::{back_to_presenter_key, is_modifier, offers_binding};
    use crate::designer::Page;

    #[test]
    fn escape_selects_back_from_a_whole_window_page() {
        // Both pages that take the window read Escape as their Back button.
        assert!(back_to_presenter_key(Page::Settings, Some("Escape"), false));
        assert!(back_to_presenter_key(Page::Library, Some("Escape"), false));
        assert!(!back_to_presenter_key(Page::Settings, Some("Enter"), false));
        // The editor holds work in progress: Escape cancels, it does not
        // leave.
        assert!(!back_to_presenter_key(Page::Editor, Some("Escape"), false));
        assert!(!back_to_presenter_key(
            Page::Presenter,
            Some("Escape"),
            false
        ));
    }

    /// A dialog over the page takes the first Escape and the page takes the
    /// second, so leaving is never a surprise on top of a dismissal.
    #[test]
    fn something_open_in_front_of_the_page_takes_the_press_first() {
        assert!(!back_to_presenter_key(Page::Library, Some("Escape"), true));
        assert!(back_to_presenter_key(Page::Library, Some("Escape"), false));
    }

    #[test]
    fn typing_never_offers_a_binding() {
        for key in ["a", "Z", "7", "é", "/"] {
            assert!(!offers_binding(Some(key)), "{key} is someone typing");
        }
    }

    #[test]
    fn a_remotes_unnamed_button_does() {
        assert!(offers_binding(None));
        assert!(offers_binding(Some("unidentified")));
    }

    #[test]
    fn a_named_key_a_remote_might_send_does() {
        for key in ["F13", "XF86Forward", "PageDown", "BrowserBack"] {
            assert!(offers_binding(Some(key)), "{key} is worth binding");
        }
    }

    #[test]
    fn modifiers_never_do() {
        for key in ["Alt", "Control", "Shift", "Super", "CapsLock"] {
            assert!(is_modifier(Some(key)));
            assert!(!offers_binding(Some(key)));
        }
    }
}

/// The order pages are warmed in: nearest the presenter first.
///
/// Pages already held, and pages a shorter document no longer has, drop out.
/// Ordering is the whole of the strategy — a deck warms front to back from
/// wherever the presenter is standing, so the pictures they are most likely
/// to want are the ones that exist first, and a five-hundred-page deck is
/// useful long before it is finished.
/// The page in the middle of what the overview grid is showing.
///
/// `None` before the grid has been laid out — there is no honest answer then,
/// and the caller falls back to the presenter's own position.
fn visible_centre(scroll: f32, grid: OverviewGrid, count: usize) -> Option<usize> {
    if count == 0 || grid.row_height <= 0.0 || grid.viewport_height <= 0.0 {
        return None;
    }
    let columns = grid.columns.max(1);
    let middle = scroll + grid.viewport_height / 2.0;
    let row = (middle / grid.row_height).floor().max(0.0) as usize;
    Some((row * columns + columns / 2).min(count - 1))
}

/// How long the listener thread waits before looking again of its own accord.
///
/// Nothing depends on it: a ring wakes the thread immediately and a closed
/// doorbell returns at once. It exists only so the thread is never parked
/// unboundedly on a channel whose senders leaked.
const WAKEUP_POLL: Duration = Duration::from_secs(1);

/// The renderer's doorbell as a subscription identity.
///
/// The hash is a constant because there is exactly one doorbell for the life
/// of the application. Hashing the pointer instead would be the same value in
/// practice and a restarted subscription — a second listener thread on a
/// one-listener handle — the day it were not.
#[derive(Clone)]
struct RenderListener(std::sync::Arc<pulpit_render::supervisor::RenderWakeup>);

impl std::hash::Hash for RenderListener {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "pulpit::render-wakeup".hash(state);
    }
}

/// Turn the renderer's doorbell into [`Message::RenderReady`].
///
/// A thread, not a future, because the doorbell is a blocking channel: the
/// wait must not sit on the runtime that also draws. The channel it feeds is
/// one deep and the send is a `try_send`, so a burst of finished frames
/// collapses into a single pass of the event loop instead of a queue of
/// messages each asking for the same drain.
fn render_wakeups(
    wakeup: std::sync::Arc<pulpit_render::supervisor::RenderWakeup>,
) -> Subscription<Message> {
    use pulpit_render::supervisor::Wakeup;

    Subscription::run_with(RenderListener(wakeup), |listener| {
        let wakeup = listener.0.clone();
        let (mut sender, receiver) = iced::futures::channel::mpsc::channel(1);
        let spawned = std::thread::Builder::new()
            .name("render-wakeup".into())
            .spawn(move || loop {
                match wakeup.wait(WAKEUP_POLL) {
                    Wakeup::Ring => {
                        if sender
                            .try_send(Message::RenderReady)
                            .is_err_and(|error| error.is_disconnected())
                        {
                            return;
                        }
                    }
                    Wakeup::Idle => {}
                    // The supervisor is gone, which happens on the way out.
                    Wakeup::Closed => return,
                }
            });
        if let Err(error) = spawned {
            // The tick still drains the supervisor, so this costs latency
            // rather than frames.
            tracing::warn!(%error, "no renderer wakeup listener; falling back to the tick");
        }
        receiver
    })
}

type RenderWant = (usize, FrameKind, Priority, Quality, u32);

/// The complete live slide plan before audience-neighbour and notes prefetch.
///
/// One exact audience render and one canonical presenter representation per
/// logical page: no page is ever asked for at two live sizes, so no panel and
/// no projector can be handed a quality ladder to climb.
///
/// `coarse` is the one exception, and `Some` only on the jumps where the
/// stand-in would be shown — the committed page, ahead of its own refined
/// frame, so a worker that is busy with panels still answers the projector
/// first.
fn live_slide_plan(
    committed: usize,
    count: usize,
    audience: u32,
    widths: crate::layout::panels::SlideWidths,
    coarse: Option<u32>,
) -> Vec<RenderWant> {
    let mut wanted: Vec<RenderWant> = coarse
        .map(|width| {
            (
                committed,
                FrameKind::Slide,
                Priority::Audience,
                Quality::Coarse,
                width,
            )
        })
        .into_iter()
        .collect();
    wanted.push((
        committed,
        FrameKind::Slide,
        Priority::Audience,
        Quality::Refined,
        audience,
    ));

    // The page on screen, at the width the current-slide panel draws it.
    let push = |slide: usize, width: u32, priority: Priority, wanted: &mut Vec<RenderWant>| {
        if slide >= count {
            return;
        }
        // One page at one width is one picture, whatever priority asked for
        // it. The first ask wins, and the first ask is the more urgent one,
        // so a layout whose panels are all the same size costs exactly what
        // it did before rather than paying twice for one image.
        if wanted
            .iter()
            .any(|(other, _, _, _, other_width)| *other == slide && *other_width == width)
        {
            return;
        }
        wanted.push((slide, FrameKind::Slide, priority, Quality::Refined, width));
    };
    push(committed, widths.current, Priority::Presenter, &mut wanted);

    // Each neighbour twice, and deliberately: at the width the neighbour
    // panel draws it now, and at the current-slide width it will need the
    // instant it is turned to. Asking for the second one only after the turn
    // would make every turn wait for a render — which is what the settled
    // figure of about a millisecond is currently buying by having it ready.
    //
    // When a layout makes both panels the same size the two collapse into one
    // request, so a strip of equal panels costs exactly what it did before.
    for (slide, priority) in [
        (Some(committed + 1), Priority::Presenter),
        (committed.checked_sub(1), Priority::Next),
    ] {
        let Some(slide) = slide else { continue };
        push(slide, widths.neighbour, priority, &mut wanted);
        push(slide, widths.current, Priority::Adjacent, &mut wanted);
    }
    wanted
}

/// A duration as whole milliseconds, which is the resolution a page turn is
/// argued about in. Sub-millisecond stages print as `0 ms`, which is the
/// truthful answer to "is this what is slowing us down".
fn millis(duration: Duration) -> String {
    format!("{} ms", duration.as_millis())
}

/// How a stand-in reads in the report, or nothing when there was none.
fn stand_in_note(stand_in: Option<Duration>) -> String {
    match stand_in {
        Some(at) => format!(" (soft at {})", millis(at)),
        None => String::new(),
    }
}

/// Whether a window holding `holding` should take a coarse stand-in for
/// `wanted_slide` if one exists.
///
/// The rule both windows obey, in one place because they must agree: the
/// stand-in is asked for by `request_renders` and consumed by the views, and a
/// window that would show one the plan did not ask for waits for a frame that
/// is never coming.
///
/// A stand-in corrects a *wrong page*, and nothing else. It is never shown
/// over the right page — that would be a downgrade, and a ladder of textures
/// for one turn is the flicker this design exists to prevent — and never over
/// an empty slot, where there is no wrong page to correct and a soft picture
/// would sharpen in front of the room for no reason.
fn wants_stand_in(holding: Option<FrameKey>, wanted_slide: usize) -> bool {
    holding.is_some_and(|key| key.slide != wanted_slide)
}

/// Move a display slot only when the complete candidate is for its wanted
/// slide. A missing or stale candidate leaves the last valid frame untouched.
fn ready_transition(
    previous: Option<FrameKey>,
    wanted_slide: usize,
    candidate: Option<FrameKey>,
) -> Option<FrameKey> {
    candidate
        .filter(|key| key.slide == wanted_slide)
        .or(previous)
}

/// Slide outputs are immutable exact-size products. Notes may reuse a nearby
/// fitting render because they are not part of the atomic slide transition.
fn request_is_satisfied(cache: &FrameCache, key: FrameKey) -> bool {
    if key.kind == FrameKind::Slide {
        cache.contains(&key)
    } else {
        cache.satisfies(key.generation, key.slide, key.kind, key.quality, key.width)
    }
}

#[cfg(test)]
mod canonical_frame_tests {
    use super::{ready_transition, wants_stand_in};
    use pulpit_core::RenderGeneration;
    use pulpit_render::cache::{FrameKey, FrameKind};
    use pulpit_render::protocol::Quality;

    fn key(slide: usize, width: u32) -> FrameKey {
        FrameKey {
            generation: RenderGeneration(1),
            slide,
            kind: FrameKind::Slide,
            quality: Quality::Refined,
            width,
            height: width / 2,
        }
    }

    #[test]
    fn a_stand_in_corrects_a_wrong_page_and_nothing_else() {
        // The presenter panel is holding the page just left: this is the one
        // case a soft picture is an improvement, and the case the panel used
        // to answer by showing the previous slide until the full canonical
        // render landed.
        assert!(wants_stand_in(Some(key(3, 1280)), 4));
        // Already on the right page. A stand-in here is the second rung of a
        // ladder, which is the flicker, not the fix.
        assert!(!wants_stand_in(Some(key(4, 1280)), 4));
        // Nothing held: the thumbnail stands in, and the first real frame the
        // window ever shows is a sharp one.
        assert!(!wants_stand_in(None, 4));
    }

    const WIDTHS: crate::layout::panels::SlideWidths = crate::layout::panels::SlideWidths {
        current: 2688,
        neighbour: 1024,
    };

    #[test]
    fn the_live_plan_has_no_progressive_presenter_ladder() {
        let plan = super::live_slide_plan(4, 100, 3840, WIDTHS, None);
        assert!(plan.iter().all(|request| request.3 == Quality::Refined));
        assert_eq!(
            plan.iter()
                .filter(|request| request.2 == pulpit_render::protocol::Priority::Audience)
                .count(),
            1
        );
        // Every panel render is at one of the two widths the layout actually
        // draws — never a third, which would be a texture nobody displays.
        assert!(plan
            .iter()
            .filter(|request| request.2 != pulpit_render::protocol::Priority::Audience)
            .all(|request| request.4 == WIDTHS.current || request.4 == WIDTHS.neighbour));
    }

    /// A page is wanted small while it is the next slide and large the
    /// instant it is turned to. Both are asked for before the turn, because
    /// asking after it is what makes a turn wait for a render.
    #[test]
    fn a_neighbour_is_asked_for_at_both_widths_before_the_turn() {
        let plan = super::live_slide_plan(4, 100, 3840, WIDTHS, None);
        let for_next: Vec<u32> = plan
            .iter()
            .filter(|request| request.0 == 5)
            .map(|request| request.4)
            .collect();
        assert!(for_next.contains(&WIDTHS.neighbour), "the next-slide panel");
        assert!(for_next.contains(&WIDTHS.current), "and the turn to come");
    }

    /// A layout whose panels are all the same size costs exactly what it did
    /// before: one width, one render, no duplicate.
    #[test]
    fn equal_panels_collapse_to_one_request_per_page() {
        let equal = crate::layout::panels::SlideWidths {
            current: 1536,
            neighbour: 1536,
        };
        let plan = super::live_slide_plan(4, 100, 3840, equal, None);
        let for_next = plan.iter().filter(|request| request.0 == 5).count();
        assert_eq!(for_next, 1);
    }

    #[test]
    fn one_coarse_stand_in_is_asked_for_first_and_serves_both_windows() {
        let plan = super::live_slide_plan(4, 100, 3840, WIDTHS, Some(640));
        let coarse = plan.first().expect("a plan");
        assert_eq!(coarse.3, Quality::Coarse);
        assert_eq!(coarse.4, 640);
        assert_eq!(coarse.0, 4, "the committed page, never a neighbour");
        // Exactly one, however many windows will draw it: the projector and
        // the Current Slide panel show the same picture while the page they
        // have been sent to renders, and asking twice would buy a second
        // texture and a second render for one image.
        assert_eq!(
            plan.iter()
                .filter(|request| request.3 == Quality::Coarse)
                .count(),
            1
        );
    }

    #[test]
    fn a_nearby_width_never_suppresses_an_exact_slide_request() {
        let mut cache = pulpit_render::cache::FrameCache::new(32 * 1024 * 1024);
        let cached = key(0, 1408);
        cache.insert(
            cached,
            pulpit_render::cache::Frame {
                width: 1408,
                height: 704,
                pixels: std::sync::Arc::new(vec![0; 1408 * 704 * 4]),
            },
        );
        assert!(!super::request_is_satisfied(&cache, key(0, 1280)));
        assert!(super::request_is_satisfied(&cache, cached));
    }

    #[test]
    fn navigation_holds_the_previous_frame_until_the_target_is_ready() {
        let previous = key(3, 1280);
        assert_eq!(ready_transition(Some(previous), 4, None), Some(previous));
        assert_eq!(
            ready_transition(Some(previous), 4, Some(key(4, 1280))),
            Some(key(4, 1280))
        );
    }

    #[test]
    fn a_late_frame_for_another_slide_never_changes_the_display() {
        let previous = key(3, 1280);
        assert_eq!(
            ready_transition(Some(previous), 4, Some(key(5, 1280))),
            Some(previous)
        );
    }
}

/// The widest a page can be rendered and still leave room for every other
/// page in the deck.
///
/// The budget holds `count` pictures of `width × width/aspect × 4` bytes, so
/// the width that exactly spends it is the square root of
/// `budget × aspect / (4 × count)`. Rounded down to a multiple of eight,
/// because a texture width that is a round number of pixels is kinder to
/// every stage below this one, and clamped: never sharper than
/// [`THUMBNAIL_WIDTH`], which is all the grid can show, and never narrower
/// than [`THUMBNAIL_MIN_WIDTH`], below which there is nothing to look at.
///
/// A deck long enough to hit the floor is one the budget genuinely cannot
/// hold, and it is the only case where a thumbnail is evicted at all.
fn fitting_thumbnail_width(count: usize, aspect: f32, budget: u64) -> u32 {
    if count == 0 {
        return THUMBNAIL_WIDTH;
    }
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let exact = (budget as f64 * aspect as f64 / (4.0 * count as f64)).sqrt();
    if !exact.is_finite() {
        return THUMBNAIL_WIDTH;
    }
    let rounded = ((exact as u32) / 8) * 8;
    rounded.clamp(THUMBNAIL_MIN_WIDTH, THUMBNAIL_WIDTH)
}

fn warming_order(
    queue: &std::collections::VecDeque<(usize, u32)>,
    count: usize,
    here: usize,
    have: &crate::thumbnails::ThumbnailCache,
) -> std::collections::VecDeque<(usize, u32)> {
    let mut order: Vec<(usize, u32)> = queue
        .iter()
        .copied()
        .filter(|(slide, width)| *slide < count && !have.has_at_least(*slide, *width))
        .collect();
    order.sort_by_key(|(slide, _)| slide.abs_diff(here));
    order.into()
}

#[cfg(test)]
mod warming_tests {
    use super::{visible_centre, warming_order, OverviewGrid};
    use crate::thumbnails::ThumbnailCache;
    use std::collections::VecDeque;

    fn grid(columns: usize, rows_on_screen: f32) -> OverviewGrid {
        OverviewGrid {
            columns,
            row_height: 100.0,
            viewport_height: rows_on_screen * 100.0,
        }
    }

    #[test]
    fn an_unlaid_grid_has_no_centre() {
        assert_eq!(visible_centre(0.0, OverviewGrid::default(), 100), None);
        assert_eq!(visible_centre(0.0, grid(4, 3.0), 0), None);
    }

    #[test]
    fn the_centre_is_the_middle_of_what_is_on_screen() {
        // Four columns, three rows on screen, scrolled to row 50: rows 50-52
        // are showing, so the middle row is 51 and the middle of it is 51*4+2.
        assert_eq!(visible_centre(5000.0, grid(4, 3.0), 400), Some(51 * 4 + 2));
        // Unscrolled, the centre is in the first screenful, not at page zero.
        assert_eq!(visible_centre(0.0, grid(4, 3.0), 400), Some(4 + 2));
    }

    #[test]
    fn the_centre_stays_inside_a_short_deck() {
        // A grid scrolled past a deck that does not fill it must still name a
        // page that exists.
        assert_eq!(visible_centre(9000.0, grid(4, 3.0), 10), Some(9));
    }

    #[test]
    fn warming_follows_the_grid_rather_than_the_presenter() {
        // The presenter is on page 12 and has scrolled the grid to page 200:
        // what fills first is what they are looking at.
        let centre = visible_centre(5000.0, grid(4, 3.0), 400).unwrap();
        let order = warming_order(&coarse(0..400), 400, centre, &cache());
        for (slide, _) in order.iter().take(4) {
            assert!(
                slide.abs_diff(centre) <= 2,
                "{slide} is not near the rows on screen"
            );
        }
    }

    fn cache() -> ThumbnailCache {
        ThumbnailCache::new(1024 * 1024)
    }

    fn coarse(range: std::ops::Range<usize>) -> VecDeque<(usize, u32)> {
        range.map(|slide| (slide, super::THUMBNAIL_WIDTH)).collect()
    }

    #[test]
    fn the_nearest_pages_are_warmed_first() {
        let order = warming_order(&coarse(0..100), 100, 50, &cache());

        assert_eq!(
            order.front().map(|(slide, _)| *slide),
            Some(50),
            "the page in hand"
        );
        for (slide, _) in order.iter().take(5) {
            assert!(
                slide.abs_diff(50) <= 2,
                "{slide} is further than the first five should reach"
            );
        }
        assert_eq!(order.len(), 100, "and every page is still wanted");
    }

    #[test]
    fn pages_already_held_are_not_asked_for_again() {
        let mut have = cache();
        have.insert(
            3,
            iced::widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
            10,
            super::THUMBNAIL_WIDTH,
            3,
        );

        let order = warming_order(&coarse(0..10), 10, 0, &have);

        assert!(!order.iter().any(|(slide, _)| *slide == 3));
        assert_eq!(order.len(), 9);
    }

    #[test]
    fn a_coarse_picture_does_not_satisfy_a_wider_request() {
        // Warming is one pass at one width now, but a reload can lower a
        // giant deck to a narrower width and a later reload restore it:
        // a narrower picture must still count as missing.
        let mut have = cache();
        have.insert(
            3,
            iced::widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
            10,
            super::THUMBNAIL_MIN_WIDTH,
            3,
        );
        let queue: VecDeque<(usize, u32)> = [(3, super::THUMBNAIL_WIDTH)].into();

        let order = warming_order(&queue, 10, 0, &have);

        assert_eq!(
            order.len(),
            1,
            "the page is still wanted, at the wider width"
        );
    }

    /// The whole deck must fit the budget at the width warming chooses.
    /// Anything that does not fit is evicted, and nothing ever asks for an
    /// evicted page again — which is a grid with permanent holes in it.
    #[test]
    fn every_page_of_a_long_book_fits_the_budget_at_the_chosen_width() {
        use super::{fitting_thumbnail_width, THUMBNAIL_BUDGET_BYTES, THUMBNAIL_MIN_WIDTH};

        // A real one: 655 portrait pages, 439.42 × 683.15 points. At the two
        // fixed widths this replaced — 480 and 240 — this deck needed 938 MB
        // and 234 MB against a 128 MB budget, so roughly half of it was
        // evicted as fast as it was rendered and the grid never filled in.
        for (count, aspect) in [
            (655usize, 439.42f32 / 683.15),
            (120, 16.0 / 9.0),
            (1, 16.0 / 9.0),
            (2_000, 0.7),
        ] {
            let width = fitting_thumbnail_width(count, aspect, THUMBNAIL_BUDGET_BYTES);
            if width == THUMBNAIL_MIN_WIDTH {
                // The floor: a deck this long is one the budget cannot hold
                // at any width worth looking at, and eviction is the answer.
                continue;
            }
            let height = (width as f64 / aspect as f64).max(1.0) as u64;
            let total = width as u64 * height * 4 * count as u64;
            assert!(
                total <= THUMBNAIL_BUDGET_BYTES,
                "{count} pages at {width}px need {} MiB of a {} MiB budget",
                total / (1024 * 1024),
                THUMBNAIL_BUDGET_BYTES / (1024 * 1024),
            );
        }
    }

    /// A short deck is not punished for the long ones: it still gets the
    /// sharp width the grid is designed around.
    #[test]
    fn an_ordinary_deck_still_warms_at_the_sharp_width() {
        use super::{fitting_thumbnail_width, THUMBNAIL_BUDGET_BYTES, THUMBNAIL_WIDTH};

        assert_eq!(
            fitting_thumbnail_width(120, 16.0 / 9.0, THUMBNAIL_BUDGET_BYTES),
            THUMBNAIL_WIDTH
        );
        // A degenerate document cannot produce a nonsense width.
        assert_eq!(
            fitting_thumbnail_width(0, 16.0 / 9.0, THUMBNAIL_BUDGET_BYTES),
            THUMBNAIL_WIDTH
        );
        assert_eq!(
            fitting_thumbnail_width(10, f32::NAN, THUMBNAIL_BUDGET_BYTES),
            THUMBNAIL_WIDTH
        );
    }

    #[test]
    fn a_shorter_document_drops_the_pages_it_no_longer_has() {
        // A reload of a deck that lost its last twenty pages must not leave
        // requests for pages that cannot be rendered.
        let order = warming_order(&coarse(0..30), 10, 0, &cache());

        assert_eq!(order.len(), 10);
        assert!(order.iter().all(|(slide, _)| *slide < 10));
    }
}

#[cfg(test)]
mod grid_navigation_tests {
    use super::{grid_target, settled_selection, OverviewGrid};

    /// Two rows of five on screen, each row a hundred pixels tall.
    fn grid() -> OverviewGrid {
        OverviewGrid {
            columns: COLUMNS,
            row_height: 100.0,
            viewport_height: 200.0,
        }
    }

    /// How many whole rows a screenful of the grid holds in these tests.
    const PAGE_ROWS: usize = 2;

    // A five-column grid over eleven pages: two full rows and a row of one.
    const COLUMNS: usize = 5;
    const COUNT: usize = 11;

    #[test]
    fn the_arrows_move_in_all_four_directions() {
        assert_eq!(
            grid_target("Right", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(7))
        );
        assert_eq!(
            grid_target("Left", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(5))
        );
        assert_eq!(
            grid_target("Down", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(11 - 1))
        );
        assert_eq!(
            grid_target("Up", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(1))
        );
    }

    #[test]
    fn down_from_a_full_row_moves_a_whole_row() {
        assert_eq!(
            grid_target("Down", 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(6))
        );
    }

    #[test]
    fn down_into_a_short_last_row_lands_on_its_last_page() {
        // Column 3 of the middle row has no page beneath it; the eye still
        // expects to arrive somewhere on the row below.
        assert_eq!(
            grid_target("Down", 8, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(10))
        );
    }

    #[test]
    fn the_edges_absorb_the_press() {
        assert_eq!(grid_target("Up", 2, COUNT, COLUMNS, PAGE_ROWS), Some(None));
        assert_eq!(
            grid_target("Left", 0, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
        assert_eq!(
            grid_target("Right", COUNT - 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
        assert_eq!(
            grid_target("Down", COUNT - 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
    }

    #[test]
    fn the_grid_answers_to_the_vim_keys_as_well_as_the_arrows() {
        // In the vim sense: `j` and `k` between rows, `h` and `l` along one.
        // The overview is a grid being looked over, not a deck being advanced
        // through, so `j` here means what it means in vim rather than what it
        // means on the slide.
        for (vim, arrow) in [("h", "Left"), ("l", "Right"), ("k", "Up"), ("j", "Down")] {
            assert_eq!(
                grid_target(vim, 7, COUNT, COLUMNS, PAGE_ROWS),
                grid_target(arrow, 7, COUNT, COLUMNS, PAGE_ROWS),
                "{vim} should move like {arrow}"
            );
        }
    }

    #[test]
    fn a_key_the_grid_does_not_own_falls_through() {
        assert_eq!(grid_target("b", 3, COUNT, COLUMNS, PAGE_ROWS), None);
        assert_eq!(grid_target("Home", 3, COUNT, COLUMNS, PAGE_ROWS), None);
    }

    #[test]
    fn a_page_key_moves_a_screenful_of_rows() {
        // Two rows of five on screen, so a page is ten pages away.
        assert_eq!(
            grid_target("PageDown", 0, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(10))
        );
        assert_eq!(
            grid_target("PageUp", 10, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(0))
        );
    }

    #[test]
    fn a_page_key_past_the_end_lands_on_the_end() {
        assert_eq!(
            grid_target("PageDown", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(COUNT - 1))
        );
        assert_eq!(
            grid_target("PageUp", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(0))
        );
        assert_eq!(
            grid_target("PageDown", COUNT - 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
        assert_eq!(
            grid_target("PageUp", 0, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
    }

    #[test]
    fn a_selection_still_on_screen_is_left_alone() {
        // Rows 0 and 1 are on screen and the selection is in row 1.
        assert_eq!(settled_selection(6, COUNT, 0.0, grid()), None);
    }

    #[test]
    fn a_selection_scrolled_off_the_top_follows_the_screen_down() {
        // Scrolled to row 1: the selection in row 0 comes down a row and
        // keeps its column.
        assert_eq!(settled_selection(2, COUNT, 100.0, grid()), Some(7));
    }

    #[test]
    fn a_selection_scrolled_off_the_bottom_follows_the_screen_up() {
        // Back at the top, with the selection down in the short last row.
        assert_eq!(settled_selection(10, COUNT, 0.0, grid()), Some(5));
    }

    #[test]
    fn the_short_last_row_never_selects_past_the_deck() {
        // Row 2 holds a single page; a column-3 selection arriving there
        // lands on the last page rather than past it.
        assert_eq!(settled_selection(3, COUNT, 200.0, grid()), Some(COUNT - 1));
    }

    #[test]
    fn a_grid_that_has_never_been_laid_out_moves_nothing() {
        assert_eq!(
            settled_selection(3, COUNT, 0.0, OverviewGrid::default()),
            None
        );
    }

    #[test]
    fn one_column_behaves_like_a_list() {
        assert_eq!(grid_target("Down", 3, COUNT, 1, PAGE_ROWS), Some(Some(4)));
        assert_eq!(grid_target("Up", 3, COUNT, 1, PAGE_ROWS), Some(Some(2)));
    }
}
