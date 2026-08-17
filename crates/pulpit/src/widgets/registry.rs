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

    pub fn multi_instance(&self) -> bool {
        self.definition.multi_instance
    }

    pub fn minimum_size(&self) -> (f32, f32) {
        self.definition.minimum_size
    }
}

/// Declares the registry, the `WidgetKind -> WidgetId` mapping, the reverse
/// lookup and the render dispatch from one list, so a new widget touches
/// this macro invocation and nothing else in this module.
macro_rules! widget_registry {
    ( $( $kind:ident => $id:literal => $view:path ),+ $(,)? ) => {
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

widget_registry! {
    CurrentSlide         => "pulpit.slide.current"             => super::slides::view::view,
    PreviousSlide        => "pulpit.slide.previous"            => super::slides::view::view,
    NextSlide             => "pulpit.slide.next"                => super::slides::view::view,
    PreviousCurrentNext  => "pulpit.slide.previous-current-next" => super::slides::view::view,
    SpeakerNotes          => "pulpit.notes.speaker"             => super::notes::view::view,
    Timer                 => "pulpit.timer.elapsed"             => super::timing::view::view,
    Clock                 => "pulpit.timer.clock"               => super::timing::view::view,
    SlideButtons          => "pulpit.navigation.buttons"        => super::navigation::view::view,
    SlideSlider           => "pulpit.navigation.slider"         => super::navigation::view::view,
    SlideCounter          => "pulpit.navigation.counter"        => super::navigation::view::view,
    PauseResume           => "pulpit.navigation.pause-resume"   => super::navigation::view::view,
    EndPresentation       => "pulpit.navigation.end"            => super::navigation::view::view,
    PresentationTitle     => "pulpit.status.title"              => super::status::view::view,
    CurrentSection        => "pulpit.status.section"            => super::status::view::view,
    AudienceScreenStatus  => "pulpit.status.audience-screen"    => super::status::view::view,
    ConnectionStatus      => "pulpit.status.connection"         => super::status::view::view,
    Annotations           => "pulpit.annotations.palette"       => super::annotations::view::view,
    MediaTransport        => "pulpit.media.transport"           => super::media::view::view,
    MainMenu              => "pulpit.chrome.main-menu"          => super::chrome::view::view,
    AudienceControls      => "pulpit.chrome.audience-controls"  => super::chrome::view::view,
    DocumentPage          => "pulpit.document.page"             => super::document::view::view,
    DocumentNav           => "pulpit.document.nav"              => super::document::view::view,
    DocumentOutline       => "pulpit.document.outline"          => super::document::view::view,
    AnnotationTools       => "pulpit.document.annotation-tools" => super::document::view::view,
    Search                => "pulpit.search.query"              => super::search::view::view,
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
}
