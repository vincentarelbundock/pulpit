//! Typed edits to a placed widget.
//!
//! The designer used to reach into a universal property bag and set fields on
//! it. It now sends one of these and the model decides: whether the patch
//! applies to this widget at all, whether the value is allowed, and what the
//! bounded value becomes. A patch meant for a timer cannot silently mutate a
//! title's unused timer options, because a title refuses it.
//!
//! Nothing sends a patch today: the designer has no properties panel, and
//! every presentation property is a token (see `widgets::tokens`). The layer
//! is kept whole, and exercised by its tests, because the rules it encodes —
//! which patch applies to which family, what is bounded, what is refused —
//! are the hard part, and re-deriving them the day configuration is offered
//! again would be a worse trade than carrying them.
#![allow(dead_code)]

use crate::widgets::annotations::model::AnnotationPatch;
use crate::widgets::common::{Align, Variant};
use crate::widgets::navigation::model::ButtonsPatch;
use crate::widgets::notes::model::NotesPatch;
use crate::widgets::slides::model::SlidePatch;
use crate::widgets::timing::model::{ClockPatch, TimerPatch};
use crate::widgets::{Family, Widget, WidgetConfig, WidgetKind};

/// One edit to one widget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WidgetPatch {
    // Style and fit are tokens today (see `widgets::tokens`); the patches
    // stay so the vocabulary is ready when they are offered again.
    #[allow(dead_code)]
    Common(CommonPatch),
    #[allow(dead_code)]
    Slide(SlidePatch),
    Notes(NotesPatch),
    Timer(TimerPatch),
    Clock(ClockPatch),
    Buttons(ButtonsPatch),
    Annotations(AnnotationPatch),
}

/// The properties every widget has.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum CommonPatch {
    Variant(Variant),
    Scale(f32),
    Alignment(Align),
}

/// Why an edit was refused. Both cases are worth different words in the UI:
/// one is a bug in the caller, the other is a rule the presenter should know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    #[error("{patch} does not apply to {kind}")]
    NotApplicable {
        kind: &'static str,
        patch: &'static str,
    },
}

impl WidgetPatch {
    /// A short name for the patch's family, for error messages.
    fn family_name(&self) -> &'static str {
        match self {
            WidgetPatch::Common(_) => "that property",
            WidgetPatch::Slide(_) => "a slide setting",
            WidgetPatch::Notes(_) => "a notes setting",
            WidgetPatch::Timer(_) => "a timer setting",
            WidgetPatch::Clock(_) => "a clock setting",
            WidgetPatch::Buttons(_) => "a button setting",
            WidgetPatch::Annotations(_) => "an annotation setting",
        }
    }

    /// Does this patch apply to that widget?
    ///
    /// Compound widgets follow their constituents: slide patches apply to
    /// the three-across strip. There is no compound-specific patch.
    pub fn applies_to(&self, kind: WidgetKind) -> bool {
        let occupies = |part: WidgetKind| kind.occupies().contains(&part);
        match self {
            WidgetPatch::Common(_) => true,
            WidgetPatch::Slide(_) => matches!(kind.family(), Family::Slides),
            WidgetPatch::Notes(_) => occupies(WidgetKind::SpeakerNotes),
            WidgetPatch::Timer(_) => occupies(WidgetKind::Timer),
            WidgetPatch::Clock(_) => occupies(WidgetKind::Clock),
            WidgetPatch::Buttons(_) => occupies(WidgetKind::SlideButtons),
            WidgetPatch::Annotations(_) => occupies(WidgetKind::Annotations),
        }
    }
}

impl Widget {
    /// Apply an edit, or leave the widget exactly as it was and say why.
    pub fn apply(&mut self, patch: WidgetPatch) -> Result<(), PatchError> {
        if !patch.applies_to(self.kind) {
            return Err(PatchError::NotApplicable {
                kind: self.kind.label(),
                patch: patch.family_name(),
            });
        }

        // Work on a copy so a refusal cannot leave a half-applied edit.
        let mut style = self.style;
        let mut config = *self.config();
        match patch {
            WidgetPatch::Common(CommonPatch::Variant(value)) => style.variant = value,
            WidgetPatch::Common(CommonPatch::Scale(value)) => style.scale = value,
            WidgetPatch::Common(CommonPatch::Alignment(value)) => style.alignment = value,
            WidgetPatch::Slide(patch) => {
                if let WidgetConfig::Slide(options) = &mut config {
                    patch.apply(&mut options.fit);
                }
            }
            WidgetPatch::Notes(patch) => {
                if let WidgetConfig::Notes(options) = &mut config {
                    patch.apply(options);
                }
            }
            WidgetPatch::Timer(patch) => {
                if let WidgetConfig::Timer(options) = &mut config {
                    patch.apply(options);
                }
            }
            WidgetPatch::Clock(patch) => {
                if let WidgetConfig::Clock(options) = &mut config {
                    patch.apply(options);
                }
            }
            WidgetPatch::Buttons(patch) => {
                if let WidgetConfig::Buttons(options) = &mut config {
                    patch.apply(options);
                }
            }
            WidgetPatch::Annotations(patch) => {
                if let WidgetConfig::Annotations(options) = &mut config {
                    patch.apply(options);
                }
            }
        }

        self.style = style;
        *self.config_mut() = config;
        // Bounds are the model's business, not the slider's.
        self.sanitise();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::notes::model::NotesSource;

    #[test]
    fn a_patch_for_another_family_is_refused_and_changes_nothing() {
        let mut title = Widget::new(WidgetKind::PresentationTitle);
        let before = title.clone();

        let refused = title.apply(WidgetPatch::Notes(NotesPatch::FontSize(30.0)));
        assert!(matches!(refused, Err(PatchError::NotApplicable { .. })));
        assert_eq!(title, before, "a refused patch leaves the widget alone");
    }

    #[test]
    fn compound_widgets_accept_their_constituents_patches() {
        let mut strip = Widget::new(WidgetKind::PreviousCurrentNext);
        assert!(strip
            .apply(WidgetPatch::Slide(SlidePatch::Fit(
                crate::widgets::slides::model::SlideFit::Fill
            )))
            .is_ok());
    }

    #[test]
    fn an_annotation_patch_applies_only_to_the_palette_and_is_bounded() {
        use pulpit_core::annotation::{AnnotationTool, INK_WIDTH_RANGE};

        let mut palette = Widget::new(WidgetKind::Annotations);
        palette
            .apply(WidgetPatch::Annotations(AnnotationPatch::Tool(
                AnnotationTool::Ink,
            )))
            .unwrap();
        assert_eq!(palette.annotations().tool, AnnotationTool::Ink);

        palette
            .apply(WidgetPatch::Annotations(AnnotationPatch::InkWidth(50.0)))
            .unwrap();
        assert_eq!(palette.annotations().ink_width, INK_WIDTH_RANGE.1);

        let mut timer = Widget::new(WidgetKind::Timer);
        let refused = timer.apply(WidgetPatch::Annotations(AnnotationPatch::AudienceVisible(
            true,
        )));
        assert!(matches!(refused, Err(PatchError::NotApplicable { .. })));
    }

    #[test]
    fn values_are_bounded_by_the_model() {
        let mut notes = Widget::new(WidgetKind::SpeakerNotes);
        notes
            .apply(WidgetPatch::Notes(NotesPatch::FontSize(1.0)))
            .unwrap();
        assert_eq!(
            notes.notes().font_size,
            crate::widgets::notes::model::FONT_SIZE_RANGE.0
        );
    }

    #[test]
    fn editing_one_thing_leaves_everything_else_alone() {
        let mut notes = Widget::new(WidgetKind::SpeakerNotes);
        notes
            .apply(WidgetPatch::Notes(NotesPatch::Source(
                NotesSource::NextSlide,
            )))
            .unwrap();
        let before = notes.clone();

        notes
            .apply(WidgetPatch::Notes(NotesPatch::FontSize(28.0)))
            .unwrap();

        assert_eq!(notes.notes().font_size, 28.0);
        assert_eq!(
            notes.notes().source,
            before.notes().source,
            "the rest of the family configuration is untouched"
        );
        assert_eq!(notes.style, before.style, "style is a token, not an edit");
    }
}
