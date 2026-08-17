//! The widget vocabulary, one family per directory.
//!
//! A widget family owns its configuration, its pure display decisions, its
//! rendering. Everything shared lives in
//! [`common`]; everything static lives once in [`catalog`].
//!
//! The split inside each family is deliberate and load-bearing: `model.rs`
//! must compile without Iced — it is what the layout tree, persistence and
//! validation depend on — while `view.rs` is free to draw.

use serde::{Deserialize, Serialize};

pub mod annotations;
pub mod catalog;
pub mod chrome;
pub mod common;
pub mod context;
pub mod document;
pub mod event;
pub mod media;
pub mod navigation;
pub mod notes;
pub mod patch;
pub mod plan;
pub mod registry;
pub mod sample;
pub mod search;
pub mod slides;
pub mod status;
pub mod timing;
pub mod tokens;
pub mod view_context;

pub use annotations::model::{AnnotationControls, AnnotationOptions};
pub use common::{Align, Variant};
pub use context::{Context, Mode};
pub use event::WidgetEvent;
pub use navigation::model::ButtonsOptions;
pub use notes::model::NotesOptions;
#[allow(unused_imports)] // see widgets::patch
pub use patch::{PatchError, WidgetPatch};
// `WidgetId`, `WidgetRegistration` and `registration` are Phase 3's public
// surface (persistence by stable id); reachable today via `widgets::registry`
// and via `WidgetKind::id`/`WidgetKind::from_id`, not yet re-exported at the
// crate boundary because nothing outside this module needs them yet.
pub use slides::model::SlideOptions;
pub use timing::model::{Alarm, AlarmControls, ClockOptions, TimerControls, TimerOptions};

/// Where a widget appears in the library sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetGroup {
    Slides,
    PresenterInformation,
    PresentationControls,
    OptionalInformation,
    /// The reader.
    Document,
}

impl WidgetGroup {
    pub fn label(self) -> &'static str {
        match self {
            WidgetGroup::Slides => "Slides",
            WidgetGroup::PresenterInformation => "Presenter information",
            WidgetGroup::PresentationControls => "Presentation controls",
            WidgetGroup::OptionalInformation => "Optional information",
            WidgetGroup::Document => "Document",
        }
    }

    pub const ALL: [WidgetGroup; 5] = [
        WidgetGroup::Slides,
        WidgetGroup::PresenterInformation,
        WidgetGroup::PresentationControls,
        WidgetGroup::OptionalInformation,
        WidgetGroup::Document,
    ];
}

/// What a widget kind actually does, independent of where it lives in the
/// catalog or which family renders it.
///
/// This is the vocabulary the layout validator and the layout-purpose
/// inference read instead of matching on [`WidgetKind`] or [`Family`]
/// directly: a new widget that shows the current slide only has to say so
/// here, rather than every predicate elsewhere growing another arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidgetCapability {
    /// Shows the audience's current slide, on its own or as part of a
    /// compound widget.
    ShowsCurrentSlide,
    /// Shows the document itself, or is only useful alongside it. What makes
    /// a layout a Reader rather than a presenter screen.
    ShowsDocument,
    /// Can move the presentation forward, as configured.
    NavigatesForward,
    /// Can move the presentation backward, as configured.
    NavigatesBackward,
    /// Starts, moves or stops the audience window.
    ControlsAudience,
    /// Pauses or resumes the timer.
    ControlsTimer,
    /// Finds a string in the deck or the document.
    SearchesDocument,
    /// Plays, pauses or scrubs media.
    ControlsMedia,
}

/// Everything that can be placed in a cell.
///
/// A closed enum on purpose: an exhaustive match at each dispatch point makes
/// a missing implementation a compile error rather than a blank pane in the
/// middle of a talk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetKind {
    // Slides
    CurrentSlide,
    PreviousSlide,
    NextSlide,
    /// Compound: previous, current and next as one coordinated strip.
    PreviousCurrentNext,
    // Presenter information
    SpeakerNotes,
    Timer,
    Clock,
    // Presentation controls
    /// Back and forward, to be placed where they suit.
    SlideButtons,
    SlideSlider,
    SlideCounter,
    PauseResume,
    EndPresentation,
    // Optional information
    PresentationTitle,
    CurrentSection,
    AudienceScreenStatus,
    ConnectionStatus,
    /// The presenter's own ink, highlighting and eraser controls.
    Annotations,
    /// Play, pause and scrub whatever media is on the current slide.
    MediaTransport,
    /// The hamburger: the way in to everything that is not on the layout.
    MainMenu,
    /// Start the audience window, choose which display it opens on, and stop
    /// it again.
    AudienceControls,

    // Document
    /// The page surface: continuous scroll, free zoom, annotation and form
    /// interaction. The one widget a document layout cannot omit.
    DocumentPage,
    /// Page counter, page entry, zoom control, fit-width and fit-page.
    DocumentNav,
    /// Bookmarks and page thumbnails.
    DocumentOutline,
    /// Tool selection and style for ink, highlighter, text, notes and stamps.
    AnnotationTools,

    /// Find a string in the deck or the document, from any view.
    ///
    /// Not a document widget: a presenter searching notes mid-talk is the
    /// case this exists for, and slides and reader ask the same question of
    /// the same model.
    Search,
}

impl WidgetKind {
    pub const ALL: [WidgetKind; 25] = [
        WidgetKind::CurrentSlide,
        WidgetKind::PreviousSlide,
        WidgetKind::NextSlide,
        WidgetKind::PreviousCurrentNext,
        WidgetKind::SpeakerNotes,
        WidgetKind::Timer,
        WidgetKind::Clock,
        WidgetKind::SlideButtons,
        WidgetKind::SlideSlider,
        WidgetKind::SlideCounter,
        WidgetKind::PauseResume,
        WidgetKind::EndPresentation,
        WidgetKind::PresentationTitle,
        WidgetKind::CurrentSection,
        WidgetKind::AudienceScreenStatus,
        WidgetKind::ConnectionStatus,
        WidgetKind::Annotations,
        WidgetKind::MediaTransport,
        WidgetKind::MainMenu,
        WidgetKind::AudienceControls,
        WidgetKind::DocumentPage,
        WidgetKind::DocumentNav,
        WidgetKind::DocumentOutline,
        WidgetKind::AnnotationTools,
        WidgetKind::Search,
    ];

    /// Which family implements this kind. The dispatcher
    /// both ask this rather than repeating the grouping.
    pub fn family(self) -> Family {
        match self {
            WidgetKind::CurrentSlide
            | WidgetKind::PreviousSlide
            | WidgetKind::NextSlide
            | WidgetKind::PreviousCurrentNext => Family::Slides,
            WidgetKind::SpeakerNotes => Family::Notes,
            WidgetKind::Timer | WidgetKind::Clock => Family::Timing,
            WidgetKind::SlideButtons
            | WidgetKind::SlideSlider
            | WidgetKind::SlideCounter
            | WidgetKind::PauseResume
            | WidgetKind::EndPresentation => Family::Navigation,
            WidgetKind::Annotations => Family::Annotations,
            WidgetKind::MainMenu | WidgetKind::AudienceControls => Family::Chrome,
            WidgetKind::MediaTransport => Family::Media,
            WidgetKind::PresentationTitle
            | WidgetKind::CurrentSection
            | WidgetKind::AudienceScreenStatus
            | WidgetKind::ConnectionStatus => Family::Status,
            WidgetKind::DocumentPage
            | WidgetKind::DocumentNav
            | WidgetKind::DocumentOutline
            | WidgetKind::AnnotationTools => Family::Document,
            WidgetKind::Search => Family::Search,
        }
    }

    // The metadata below is the registry's, not a second copy of it. The
    // registry itself resolves to the catalog's `WidgetDefinition`, so this
    // is one hop further from the same single source of truth.

    pub fn label(self) -> &'static str {
        registry::registration(self).label()
    }

    pub fn short_label(self) -> &'static str {
        registry::registration(self).short_label()
    }

    pub fn tooltip(self) -> &'static str {
        registry::registration(self).tooltip()
    }

    pub fn group(self) -> WidgetGroup {
        registry::registration(self).group()
    }

    pub fn parts(self) -> &'static [WidgetKind] {
        registry::registration(self).parts()
    }

    pub fn multi_instance(self) -> bool {
        registry::registration(self).multi_instance()
    }

    pub fn placement(self) -> catalog::PlacementPolicy {
        registry::registration(self).placement()
    }

    pub fn minimum_size(self) -> (f32, f32) {
        registry::registration(self).minimum_size()
    }

    /// This kind's own declared capabilities, not counting any compound
    /// parts. Almost always what [`WidgetKind::capabilities`] should be
    /// called instead — this exists for the table itself and its tests.
    pub fn own_capabilities(self) -> &'static [WidgetCapability] {
        registry::registration(self).capabilities()
    }

    /// The capabilities this kind offers, including whatever its compound
    /// parts offer — the same "counts as" mechanism [`WidgetKind::occupies`]
    /// uses, so a strip that shows the current slide answers a
    /// current-slide question the same way the plain widget does.
    pub fn capabilities(self) -> Vec<WidgetCapability> {
        let mut capabilities = Vec::new();
        for kind in self.occupies() {
            for capability in kind.own_capabilities() {
                if !capabilities.contains(capability) {
                    capabilities.push(*capability);
                }
            }
        }
        capabilities
    }

    /// Does this kind (or something it occupies) have this capability?
    pub fn has_capability(self, capability: WidgetCapability) -> bool {
        self.capabilities().contains(&capability)
    }

    /// Used by the catalog tests and by anything asking whether a kind
    /// stands for several.
    #[allow(dead_code)]
    pub fn is_compound(self) -> bool {
        !self.parts().is_empty()
    }

    /// The widget itself plus everything it counts as.
    pub fn occupies(self) -> Vec<WidgetKind> {
        let mut kinds = vec![self];
        kinds.extend_from_slice(self.parts());
        kinds
    }
}

/// The families a widget can belong to. One directory each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Slides,
    /// Finding a string, in whatever view is on screen.
    Search,
    /// The reader: the page surface and the controls around it.
    Document,
    Annotations,
    Media,
    Notes,
    Timing,
    Navigation,
    Status,
    /// The application's own chrome — the menu and the audience window's
    /// lifecycle — placed on the layout like anything else, so a presenter
    /// decides where those controls live rather than inheriting a strip.
    Chrome,
}

/// The style properties every widget has.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommonStyle {
    pub variant: Variant,
    /// Font or content scale, 0.5–2.0.
    pub scale: f32,
    pub alignment: Align,
}

impl Default for CommonStyle {
    fn default() -> Self {
        Self {
            variant: tokens::VARIANT,
            scale: tokens::SCALE,
            alignment: tokens::ALIGNMENT,
        }
    }
}

/// The configuration of one widget, and only the configuration it can have.
///
/// A title cannot carry notes options here, so a stray patch has nothing to
/// write to and an imported file cannot claim a timer is a clock.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetConfig {
    /// Widgets with nothing to configure beyond the common style.
    None,
    Slide(SlideOptions),
    Notes(NotesOptions),
    Timer(TimerOptions),
    Clock(ClockOptions),
    Buttons(ButtonsOptions),
    Annotations(AnnotationOptions),
}

impl WidgetConfig {
    /// The configuration a kind is born with, and the only shape it accepts.
    pub fn default_for(kind: WidgetKind) -> WidgetConfig {
        match kind {
            WidgetKind::CurrentSlide
            | WidgetKind::PreviousSlide
            | WidgetKind::NextSlide
            | WidgetKind::PreviousCurrentNext => WidgetConfig::Slide(SlideOptions::default()),
            WidgetKind::SpeakerNotes => WidgetConfig::Notes(NotesOptions::default()),
            WidgetKind::Timer => WidgetConfig::Timer(TimerOptions::default()),
            WidgetKind::Clock => WidgetConfig::Clock(ClockOptions::default()),
            WidgetKind::SlideButtons => WidgetConfig::Buttons(ButtonsOptions::default()),
            WidgetKind::Annotations => WidgetConfig::Annotations(AnnotationOptions::default()),
            WidgetKind::SlideSlider
            | WidgetKind::SlideCounter
            | WidgetKind::PauseResume
            | WidgetKind::EndPresentation
            | WidgetKind::PresentationTitle
            | WidgetKind::CurrentSection
            | WidgetKind::AudienceScreenStatus
            | WidgetKind::ConnectionStatus
            | WidgetKind::MediaTransport
            | WidgetKind::MainMenu
            | WidgetKind::AudienceControls
            | WidgetKind::DocumentPage
            | WidgetKind::DocumentNav
            | WidgetKind::DocumentOutline
            | WidgetKind::Search => WidgetConfig::None,
            // The document toolbar carries the same colour and size choices
            // the presenter palette does, so a mark made in one mode looks
            // the same in the other.
            WidgetKind::AnnotationTools => WidgetConfig::Annotations(AnnotationOptions::default()),
        }
    }

    /// Does this configuration belong to that kind?
    pub fn matches(&self, kind: WidgetKind) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(&WidgetConfig::default_for(kind))
    }
}

/// One widget placed in a cell.
///
/// `kind` and `config` are private because they must agree: a public field
/// would let a caller — or a file — leave a timer holding clock options.
/// Construction and deserialization are the only ways in, and both check.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Widget {
    kind: WidgetKind,
    #[serde(default)]
    pub style: CommonStyle,
    config: WidgetConfig,
}

impl<'de> Deserialize<'de> for Widget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Widget, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            kind: WidgetKind,
            #[serde(default)]
            style: CommonStyle,
            config: WidgetConfig,
        }

        let raw = Raw::deserialize(deserializer)?;
        // A mismatch is not something to sanitise away: the file says a thing
        // that cannot be true, and quietly picking one half would change what
        // the presenter sees without telling them.
        if !raw.config.matches(raw.kind) {
            return Err(serde::de::Error::custom(format!(
                "{} cannot have that configuration",
                raw.kind.label()
            )));
        }
        let mut widget = Widget {
            kind: raw.kind,
            style: raw.style,
            config: raw.config,
        };
        widget.sanitise();
        Ok(widget)
    }
}

impl Widget {
    pub fn new(kind: WidgetKind) -> Self {
        Self {
            kind,
            style: CommonStyle::default(),
            config: WidgetConfig::default_for(kind),
        }
    }

    pub fn kind(&self) -> WidgetKind {
        self.kind
    }

    pub fn config(&self) -> &WidgetConfig {
        &self.config
    }

    /// The configuration, mutably, for the model code that edits it. Kept
    /// crate-private so the kind/config agreement cannot be broken from a
    /// view.
    pub(crate) fn config_mut(&mut self) -> &mut WidgetConfig {
        &mut self.config
    }

    pub fn label(&self) -> &'static str {
        self.kind.label()
    }

    // The family options, or their defaults for a widget of another family.
    // A view only ever asks its own family, and a patch is checked before it
    // is applied, so the default is unreachable in practice — it exists so
    // callers need no `match`.

    pub fn slide(&self) -> SlideOptions {
        match self.config {
            WidgetConfig::Slide(options) => options,
            _ => SlideOptions::default(),
        }
    }

    pub fn notes(&self) -> NotesOptions {
        match self.config {
            WidgetConfig::Notes(options) => options,
            _ => NotesOptions::default(),
        }
    }

    pub fn timer(&self) -> TimerOptions {
        match self.config {
            WidgetConfig::Timer(options) => options,
            _ => TimerOptions::default(),
        }
    }

    pub fn clock(&self) -> ClockOptions {
        match self.config {
            WidgetConfig::Clock(options) => options,
            _ => ClockOptions::default(),
        }
    }

    pub fn buttons(&self) -> ButtonsOptions {
        match self.config {
            WidgetConfig::Buttons(options) => options,
            _ => ButtonsOptions::default(),
        }
    }

    pub fn annotations(&self) -> AnnotationOptions {
        match self.config {
            WidgetConfig::Annotations(options) => options,
            _ => AnnotationOptions::default(),
        }
    }

    /// Effective minimum size, taking the content scale into account.
    pub fn minimum_size(&self) -> (f32, f32) {
        let (width, height) = self.kind.minimum_size();
        let scale = self
            .style
            .scale
            .clamp(common::SCALE_RANGE.0, common::SCALE_RANGE.1);
        (width * scale, height * scale)
    }

    /// Does this widget, as configured, let the presenter move forward?
    pub fn provides_forward_navigation(&self) -> bool {
        if !self.kind.has_capability(WidgetCapability::NavigatesForward) {
            return false;
        }
        match self.kind {
            WidgetKind::SlideButtons => self.buttons().forward,
            _ => true,
        }
    }

    /// …and backward?
    pub fn provides_backward_navigation(&self) -> bool {
        if !self
            .kind
            .has_capability(WidgetCapability::NavigatesBackward)
        {
            return false;
        }
        match self.kind {
            WidgetKind::SlideButtons => self.buttons().back,
            _ => true,
        }
    }

    pub fn shows_current_slide(&self) -> bool {
        self.kind
            .has_capability(WidgetCapability::ShowsCurrentSlide)
    }

    /// Repair values that are out of range or not allowed for this kind.
    /// Construction, patching and import all go through this.
    pub fn sanitise(&mut self) {
        // Presentation style is not the presenter's to set today, so
        // whatever a file or an old layout carries is brought back to the
        // tokens. The fields stay on the widget, ready for the day they are
        // offered again.
        self.style = CommonStyle::default();
        match &mut self.config {
            WidgetConfig::Notes(options) => options.sanitise(),
            WidgetConfig::Timer(options) => options.sanitise(),
            WidgetConfig::Annotations(options) => options.sanitise(),
            // The fit is a token too: a presenter's reference copy of a
            // slide is shown whole.
            WidgetConfig::Slide(options) => options.fit = tokens::SLIDE_FIT,
            WidgetConfig::None | WidgetConfig::Clock(_) | WidgetConfig::Buttons(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compounds_count_as_their_constituents() {
        assert!(WidgetKind::PreviousCurrentNext.is_compound());
        assert!(WidgetKind::PreviousCurrentNext
            .occupies()
            .contains(&WidgetKind::CurrentSlide));
        assert!(!WidgetKind::Clock.is_compound());
        assert_eq!(WidgetKind::Clock.occupies(), vec![WidgetKind::Clock]);
    }

    #[test]
    fn every_widget_wears_the_tokens() {
        // Presentation style is not a per-widget choice today, and a file
        // that says otherwise is brought back into line on the way in.
        let mut notes = Widget::new(WidgetKind::SpeakerNotes);
        assert_eq!(notes.style, CommonStyle::default());
        notes.style.variant = Variant::Compact;
        notes.style.scale = 1.8;
        notes.sanitise();
        assert_eq!(notes.style.variant, tokens::VARIANT);
        assert_eq!(notes.style.scale, tokens::SCALE);
        assert_eq!(notes.style.alignment, tokens::ALIGNMENT);
    }

    #[test]
    fn sanitising_repairs_the_style_and_the_family_configuration() {
        let mut notes = Widget::new(WidgetKind::SpeakerNotes);
        if let WidgetConfig::Notes(options) = notes.config_mut() {
            options.font_size = 2.0;
        }
        notes.sanitise();
        assert_eq!(notes.notes().font_size, notes::model::FONT_SIZE_RANGE.0);

        let mut timer = Widget::new(WidgetKind::Timer);
        if let WidgetConfig::Timer(options) = timer.config_mut() {
            options.warning_minutes = 10_000;
        }
        timer.sanitise();
        assert_eq!(
            timer.timer().warning_minutes,
            timing::model::MAX_WARNING_MINUTES
        );
    }

    #[test]
    fn a_widget_only_carries_its_own_familys_configuration() {
        // A title has nothing to configure, so there is nowhere for a stray
        // notes or timer setting to be written.
        assert_eq!(
            *Widget::new(WidgetKind::PresentationTitle).config(),
            WidgetConfig::None
        );
        for kind in WidgetKind::ALL {
            let widget = Widget::new(kind);
            assert!(
                widget.config().matches(kind),
                "{kind:?} was born with the wrong configuration"
            );
        }
    }

    #[test]
    fn scale_moves_the_minimum_size() {
        let mut widget = Widget::new(WidgetKind::Timer);
        let (width, _) = widget.minimum_size();
        widget.style.scale = 2.0;
        assert!(widget.minimum_size().0 > width);
    }

    #[test]
    fn every_kind_has_a_family_and_a_definition() {
        for kind in WidgetKind::ALL {
            assert!(!kind.label().is_empty());
            assert!(WidgetGroup::ALL.contains(&kind.group()));
            // Exercises the family map for every kind, so a new widget that
            // forgets one is a test failure rather than a panic later.
            let _ = kind.family();
        }
    }
}

/// Something a widget has to say about its own configuration or size.
///
/// Widget-relative on purpose: a family knows what is wrong, the layout
/// validator knows which cell it happened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Complaint {
    pub message: &'static str,
    pub consequence: &'static str,
}

impl Widget {
    /// What this widget has to say about being this size, as configured.
    ///
    /// The layout validator calls this instead of matching on kinds: a new
    /// family brings its own rules with it.
    pub fn validate(&self, inner: (f32, f32)) -> Vec<Complaint> {
        match self.kind.family() {
            Family::Notes => notes::model::validate(&self.notes(), inner),
            Family::Timing => timing::model::validate(self.style.scale, inner),
            Family::Navigation if self.kind == WidgetKind::SlideButtons => {
                navigation::model::validate_buttons(&self.buttons())
            }
            _ => Vec::new(),
        }
    }
}
