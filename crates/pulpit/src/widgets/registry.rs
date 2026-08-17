//! The single authority for widget identity and rendering.
//!
//! Phase 1 gave every family the same `view` entry point. This module gives
//! every [`WidgetKind`] a stable, dotted [`WidgetId`] and ties kind,
//! metadata and rendering together in one table, so adding a widget is one
//! registration touched in one place rather than a `WidgetKind` match
//! scattered across the crate.
//!
//! ## The generic-`Message` problem
//!
//! Every family's `view` is generic over `Message` (the application's
//! event type), and a `static` cannot hold a generic function pointer — Rust
//! has no way to store `for<Message> fn(...) -> Element<Message>` as a
//! value. The registry therefore splits in two:
//!
//! - [`REGISTRY`]: a `static` array of non-generic facts — id, kind,
//!   catalog metadata — one entry per kind, checked for completeness by
//!   the tests below.
//! - [`dispatch`]: a single generic function, fed by the registry's kinds
//!   via a macro so the family match lives in exactly one place, which
//!   `layout_renderer::widget` calls instead of matching on `Family`
//!   itself.
//!
//! This is option (a) from the widget-host refactor plan: no boxing, no
//! `dyn Element` trees, and the per-kind mapping to its family's `view` is
//! generated from the same list that drives [`REGISTRY`].
//!
//! ## Adding a widget
//!
//! `pulpit.status.blank` (`widgets/status/blank.rs`, `WidgetKind::BlankSpace`)
//! is a living example — a static, decorative spacer with no configuration
//! and no capabilities. Copy its shape. Wiring a new kind touches:
//!
//! 1. **`widgets/mod.rs`** — add the `WidgetKind` variant, one line in
//!    `WidgetKind::ALL`, one arm in `WidgetKind::family()`, and one arm in
//!    `WidgetConfig::default_for` (join the `WidgetConfig::None` group
//!    unless the widget needs its own configuration shape).
//! 2. **`widgets/catalog.rs`** — one `WidgetDefinition` in `CATALOG`, kept in
//!    its group's run (the sidebar-order test enforces this). Its
//!    `thumbnail` field says what a layout thumbnail sketches for it — see
//!    [`catalog::ThumbnailContent`] — so `layout::thumbnail` needs no
//!    change.
//! 3. **`widgets/<family>/mod.rs`** and a new module (or an existing one) —
//!    the widget's own view code, and whatever pure model it needs.
//! 4. **`widgets/<family>/view.rs`** — one arm in that family's `view`
//!    dispatching to the new module. This is the one `match WidgetKind`
//!    site the source-scan test below allows per family, by design: within
//!    a family, choosing among its own kinds is that family's business.
//! 5. **This module's `widget_registry!` invocation** — one line: kind, its
//!    stable dotted id, its family's `view`, and a `plan` hook
//!    (`plan::none` unless it needs frames rendered).
//!
//! Five touches, all inside `widgets/`, none of them a new central `match`.
//! `WidgetKind::ALL`'s length and the tests below fail loudly if a step is
//! skipped.

use super::plan::WidgetPlan;
use super::view_context::WidgetViewContext;
use super::{catalog, Widget, WidgetGroup, WidgetKind};
use iced::Element;

/// A stable, dotted identity for a widget kind.
///
/// Distinct from [`WidgetKind`]'s Rust-side name: this is the identity a
/// persisted layout or a future scripting surface refers to, so renaming a
/// Rust variant does not have to mean rewriting every saved file. Phase 3
/// is where persistence actually switches to it; today it exists to be
/// unique, stable and looked up in both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetId(pub &'static str);

impl WidgetId {
    // Used by the tests below and by Phase 3's persistence work; kept public
    // now so the id round-trips before anything depends on it.
    #[allow(dead_code)]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for WidgetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// One widget's static facts: its identity, its kind and the catalog entry
/// that already described it.
#[derive(Debug, Clone, Copy)]
pub struct WidgetRegistration {
    // Read by the id-integrity tests below; Phase 3's persistence work reads
    // it in earnest.
    #[allow(dead_code)]
    pub id: WidgetId,
    pub kind: WidgetKind,
    pub definition: &'static catalog::WidgetDefinition,
    /// What frames this kind needs rendered, given its placed widget and its
    /// cell's share of the window's width. See [`super::plan`].
    pub plan: fn(&Widget, f32) -> WidgetPlan,
}

impl WidgetRegistration {
    pub fn label(&self) -> &'static str {
        self.definition.label
    }

    pub fn short_label(&self) -> &'static str {
        self.definition.short_label
    }

    pub fn tooltip(&self) -> &'static str {
        self.definition.tooltip
    }

    pub fn group(&self) -> WidgetGroup {
        self.definition.group
    }

    pub fn parts(&self) -> &'static [WidgetKind] {
        self.definition.parts
    }

    pub fn placement(&self) -> super::catalog::PlacementPolicy {
        self.definition.placement
    }

    /// Thin wrapper kept for existing callers; `placement()` is the real
    /// policy and knows more than yes/no.
    pub fn multi_instance(&self) -> bool {
        self.definition.placement.max_instances.is_none()
    }

    pub fn capabilities(&self) -> &'static [super::WidgetCapability] {
        self.definition.capabilities
    }

    pub fn minimum_size(&self) -> (f32, f32) {
        self.definition.minimum_size
    }

    /// This kind's frame needs, for a placed `widget` occupying `cell_width`
    /// (its share of the window's width, 0..=1).
    pub fn plan(&self, widget: &Widget, cell_width: f32) -> WidgetPlan {
        (self.plan)(widget, cell_width)
    }
}

/// Declares the registry, the `WidgetKind -> WidgetId` mapping, the reverse
/// lookup and the render dispatch from one list, so a new widget touches
/// this macro invocation and nothing else in this module.
macro_rules! widget_registry {
    ( $( $kind:ident => $id:literal => $view:path => $plan:path ),+ $(,)? ) => {
        /// One entry per [`WidgetKind`], in `WidgetKind::ALL` order.
        ///
        /// `catalog::definition` does a runtime scan (it is not `const fn`),
        /// so this is a lazily-built static rather than a plain `const`
        /// array — built once, on first use, and thereafter as cheap as a
        /// `const` would have been.
        pub static REGISTRY: std::sync::LazyLock<[WidgetRegistration; WidgetKind::ALL.len()]> =
            std::sync::LazyLock::new(|| {
                [
                    $(
                        WidgetRegistration {
                            id: WidgetId($id),
                            kind: WidgetKind::$kind,
                            definition: catalog::definition(WidgetKind::$kind),
                            plan: $plan,
                        },
                    )+
                ]
            });

        impl WidgetKind {
            /// This kind's stable, dotted identity.
            // Exercised by the round-trip test below; Phase 3's persistence
            // format is the real caller.
            #[allow(dead_code)]
            pub fn id(self) -> WidgetId {
                match self {
                    $( WidgetKind::$kind => WidgetId($id), )+
                }
            }

            /// The kind that owns an id, if any is registered under it.
            #[allow(dead_code)]
            pub fn from_id(id: &str) -> Option<WidgetKind> {
                match id {
                    $( $id => Some(WidgetKind::$kind), )+
                    _ => None,
                }
            }
        }

        /// Render one widget through its family's uniform entry point.
        ///
        /// The only place in the crate that matches on `WidgetKind` (rather
        /// than `Family`) to choose a renderer; [`crate::layout_renderer`]
        /// calls this instead of matching on `Family` itself.
        pub fn dispatch<'ctx, 'a, Message: Clone + 'static>(
            ctx: &WidgetViewContext<'ctx, 'a, Message>,
            widget: &Widget,
        ) -> Element<'a, Message> {
            match widget.kind() {
                $( WidgetKind::$kind => $view(ctx, widget), )+
            }
        }
    };
}

/// A slide panel's plan hook, bound to its own kind so one function works for
/// `CurrentSlide`, `PreviousSlide` and `NextSlide` alike.
fn plan_current_slide(widget: &Widget, cell_width: f32) -> WidgetPlan {
    super::plan::slide_panel(widget.kind(), cell_width)
}

fn plan_previous_current_next(_widget: &Widget, cell_width: f32) -> WidgetPlan {
    super::plan::previous_current_next(cell_width)
}

widget_registry! {
    CurrentSlide         => "pulpit.slide.current"             => super::slides::view::view => plan_current_slide,
    PreviousSlide        => "pulpit.slide.previous"            => super::slides::view::view => plan_current_slide,
    NextSlide             => "pulpit.slide.next"                => super::slides::view::view => plan_current_slide,
    PreviousCurrentNext  => "pulpit.slide.previous-current-next" => super::slides::view::view => plan_previous_current_next,
    SpeakerNotes          => "pulpit.notes.speaker"             => super::notes::view::view => super::plan::none,
    Timer                 => "pulpit.timer.elapsed"             => super::timing::view::view => super::plan::none,
    Clock                 => "pulpit.timer.clock"               => super::timing::view::view => super::plan::none,
    SlideButtons          => "pulpit.navigation.buttons"        => super::navigation::view::view => super::plan::none,
    SlideSlider           => "pulpit.navigation.slider"         => super::navigation::view::view => super::plan::none,
    SlideCounter          => "pulpit.navigation.counter"        => super::navigation::view::view => super::plan::none,
    PauseResume           => "pulpit.navigation.pause-resume"   => super::navigation::view::view => super::plan::none,
    EndPresentation       => "pulpit.navigation.end"            => super::navigation::view::view => super::plan::none,
    PresentationTitle     => "pulpit.status.title"              => super::status::view::view => super::plan::none,
    CurrentSection        => "pulpit.status.section"            => super::status::view::view => super::plan::none,
    AudienceScreenStatus  => "pulpit.status.audience-screen"    => super::status::view::view => super::plan::none,
    ConnectionStatus      => "pulpit.status.connection"         => super::status::view::view => super::plan::none,
    Annotations           => "pulpit.annotations.palette"       => super::annotations::view::view => super::plan::none,
    MediaTransport        => "pulpit.media.transport"           => super::media::view::view => super::plan::none,
    MainMenu              => "pulpit.chrome.main-menu"          => super::chrome::view::view => super::plan::none,
    AudienceControls      => "pulpit.chrome.audience-controls"  => super::chrome::view::view => super::plan::none,
    DocumentPage          => "pulpit.document.page"             => super::document::view::view => super::plan::document_page,
    DocumentNav           => "pulpit.document.nav"              => super::document::view::view => super::plan::none,
    DocumentOutline       => "pulpit.document.outline"          => super::document::view::view => super::plan::none,
    AnnotationTools       => "pulpit.document.annotation-tools" => super::document::view::view => super::plan::none,
    Search                => "pulpit.search.query"              => super::search::view::view => super::plan::none,
    BlankSpace            => "pulpit.status.blank"               => super::status::view::view => super::plan::none,
}

/// The registration for a kind. Total by construction, proved by the tests
/// below.
pub fn registration(kind: WidgetKind) -> &'static WidgetRegistration {
    REGISTRY
        .iter()
        .find(|entry| entry.kind == kind)
        .expect("every WidgetKind is registered; the registry tests prove it")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_registration_per_kind() {
        for kind in WidgetKind::ALL {
            let matches = REGISTRY.iter().filter(|entry| entry.kind == kind).count();
            assert_eq!(matches, 1, "{kind:?} is registered {matches} times");
        }
        assert_eq!(REGISTRY.len(), WidgetKind::ALL.len());
    }

    #[test]
    fn registry_matches_the_catalog() {
        for entry in REGISTRY.iter() {
            assert_eq!(entry.definition.kind, entry.kind);
            assert_eq!(entry.definition, catalog::definition(entry.kind));
        }
    }

    #[test]
    fn every_id_is_unique_dotted_and_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for entry in REGISTRY.iter() {
            let id = entry.id.as_str();
            assert!(
                id.contains('.'),
                "{id:?} for {:?} is not dotted",
                entry.kind
            );
            assert_eq!(
                id,
                id.to_lowercase(),
                "{id:?} for {:?} is not lowercase",
                entry.kind
            );
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-'),
                "{id:?} for {:?} has an unexpected character",
                entry.kind
            );
            assert!(
                seen.insert(id),
                "{id:?} is registered more than once ({:?})",
                entry.kind
            );
        }
    }

    #[test]
    fn every_id_round_trips_through_the_reverse_lookup() {
        for entry in REGISTRY.iter() {
            assert_eq!(WidgetKind::from_id(entry.id.as_str()), Some(entry.kind));
            assert_eq!(entry.kind.id(), entry.id);
        }
        assert_eq!(WidgetKind::from_id("not.a.real.id"), None);
    }

    #[test]
    fn dispatch_draws_every_kind() {
        // Exercises the generated match; a kind missing from the macro list
        // would be a compile error (non-exhaustive match), so this mainly
        // proves the function actually returns rather than panicking.
        use crate::widgets::context::{
            AudienceData, Context, DocumentData, FrameSource, MediaData, Mode, SearchData,
            SlideData, TimingData,
        };
        use iced::widget::image::Handle;
        use pulpit_render::cache::FrameKind;

        struct NoFrames;
        impl FrameSource for NoFrames {
            fn frame(&self, _slide: usize, _kind: FrameKind, _max_width: u32) -> Option<Handle> {
                None
            }
        }

        let context = Context {
            mode: Mode::Live,
            search: SearchData {
                state: &super::super::sample::SEARCH,
            },
            slides: SlideData {
                current: super::super::sample::SLIDE,
                preview: super::super::sample::SLIDE,
                count: super::super::sample::SLIDE_COUNT,
                frames: &NoFrames,
                preview_width: 640,
                aspect: 16.0 / 9.0,
                text_notes: None,
                has_links: false,
                link_highlights: Vec::new(),
                overlays: Vec::new(),
                crop: pulpit_core::notes::Region::FULL,
                annotations: &super::super::sample::ANNOTATIONS,
                rendered_text: {
                    static EMPTY: std::sync::LazyLock<
                        std::sync::Arc<
                            std::collections::HashMap<u64, crate::typst_annotation::RenderedText>,
                        >,
                    > = std::sync::LazyLock::new(|| {
                        std::sync::Arc::new(std::collections::HashMap::new())
                    });
                    &EMPTY
                },
                marks_cache: std::rc::Rc::new(iced::widget::canvas::Cache::new()),
                annotation_controls: super::super::AnnotationControls::default(),
                annotation_style: pulpit_core::annotation::AnnotationStyle::default(),
            },
            alarms: &super::super::sample::ALARMS,
            timer_controls: &super::super::sample::TIMER,
            timing: TimingData {
                elapsed: std::time::Duration::from_secs(12 * 60),
                target: Some(std::time::Duration::from_secs(40 * 60)),
                running: true,
                seconds_of_day: 13 * 3600,
            },
            document: DocumentData {
                title: super::super::sample::TITLE.to_string(),
                section: Some("Reconnection".to_string()),
                sample_notes: super::super::sample::NOTES,
            },
            reader: super::super::sample::closed_reader(),
            audience: AudienceData {
                blank: pulpit_core::Blank::Off,
                connected: true,
                fullscreen: true,
                started: true,
                menu_open: false,
            },
            media: MediaData {
                transport: super::super::sample::transport(),
            },
        };
        let ctx: WidgetViewContext<'_, '_, ()> =
            WidgetViewContext::new(&context, None, |_| (), iced::Color::BLACK, 1.0);
        for kind in WidgetKind::ALL {
            let widget = Widget::new(kind);
            let _: Element<'_, ()> = dispatch(&ctx, &widget);
        }
    }

    /// Every registration, end to end: a unique id, a real catalog entry
    /// under the same kind, and capabilities that make sense for what the
    /// kind actually is (a compound's parts, not the compound itself, are
    /// where its capabilities come from). `dispatch_draws_every_kind` above
    /// is this same completeness claim for the `view`/`plan` half — every
    /// kind reachable through the macro-generated match — so this test
    /// covers the catalog half instead of repeating it.
    #[test]
    fn every_registration_is_a_complete_widget() {
        let mut seen_ids = std::collections::HashSet::new();
        for kind in WidgetKind::ALL {
            let entry = registration(kind);
            assert_eq!(entry.kind, kind);
            assert!(seen_ids.insert(entry.id.as_str()), "{kind:?}: duplicate id");
            assert_eq!(
                entry.definition,
                catalog::definition(kind),
                "{kind:?}: registry and catalog disagree"
            );
            // A compound's own capabilities are folded into its parts'
            // (`WidgetKind::capabilities`), so a compound with no
            // capabilities of its own is not a gap — check the union
            // instead of the bare table entry.
            if kind.parts().is_empty() && entry.capabilities().is_empty() {
                assert!(
                    kind.capabilities().is_empty(),
                    "{kind:?}: has capabilities from nowhere"
                );
            }
        }
    }

    /// The architectural gate: outside a widget's own family (or this
    /// module and `plan.rs`, which *are* the registry), a `match` on
    /// `WidgetKind` or `Family` is exactly the scattering this refactor
    /// closed off. A new central dispatch point is a regression, not a
    /// style choice, so this fails the build rather than waiting for review.
    ///
    /// Deliberately simple: a source-text scan for the handful of literal
    /// patterns every real dispatch site in this crate happens to use, over
    /// each file's text up to its first `#[cfg(test)]` (test-only reference
    /// implementations, e.g. `layout::builtin`'s and `layout::panels`'
    /// pre-refactor comparison walks, are not the scattering this guards
    /// against). Good enough to catch a new one being added; not a Rust
    /// parser, and not trying to be.
    #[test]
    fn widgetkind_and_family_matches_stay_where_they_belong() {
        // Files where a `match` on `WidgetKind`/`Family` is the point of the
        // file, not a leak: this module and `plan.rs` generate/hold the
        // per-kind dispatch tables; `widgets/mod.rs` is the vocabulary
        // itself (`family()`, `WidgetConfig::default_for`, and two small
        // configuration reads); each family's own `view.rs` chooses among
        // its own kinds, which is that family's business, not the host's.
        const ALLOWED: &[&str] = &[
            "widgets/mod.rs",
            "widgets/registry.rs",
            "widgets/plan.rs",
            "widgets/slides/view.rs",
            "widgets/notes/view.rs",
            "widgets/timing/view.rs",
            "widgets/navigation/view.rs",
            "widgets/status/view.rs",
            "widgets/annotations/view.rs",
            "widgets/media/view.rs",
            "widgets/chrome/view.rs",
            "widgets/document/view.rs",
            "widgets/search/view.rs",
        ];
        const PATTERNS: &[&str] = &[
            "match widget.kind()",
            "match kind {",
            "match self.kind {",
            "match self.kind.family()",
        ];

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        visit(&root, &root, ALLOWED, PATTERNS, &mut offenders);
        assert!(
            offenders.is_empty(),
            "match over WidgetKind/Family found outside the allowlist: {offenders:?}\n\
             Either move the dispatch into a family module, drive it from \
             registry/catalog data, or add the file to ALLOWED with a reason."
        );

        fn visit(
            dir: &std::path::Path,
            root: &std::path::Path,
            allowed: &[&str],
            patterns: &[&str],
            offenders: &mut Vec<String>,
        ) {
            for entry in std::fs::read_dir(dir).expect("readable src tree") {
                let path = entry.expect("readable dir entry").path();
                if path.is_dir() {
                    visit(&path, root, allowed, patterns, offenders);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                if allowed.contains(&relative.as_str()) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("readable source file");
                let production = text.split("#[cfg(test)]").next().unwrap_or(&text);
                for pattern in patterns {
                    for (start, _) in production.match_indices(pattern) {
                        // A `match kind {`/`match widget.kind() {` header
                        // names no type; confirm this one is actually a
                        // `WidgetKind`/`Family` match, not some other
                        // `kind`-named enum (`ContentKind`, `AppliedKind`,
                        // `FrameKind`, ...), by checking its arms name one.
                        let window_end = (start + 400).min(production.len());
                        let window = &production[start..window_end];
                        if window.contains("WidgetKind::") || window.contains("Family::") {
                            offenders.push(format!("{relative}: {pattern:?}"));
                            break;
                        }
                    }
                }
            }
        }
    }
}
