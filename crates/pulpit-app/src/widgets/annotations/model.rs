//! The annotation palette: which tool it arms, and what that tool draws.
//!
//! The marks themselves live in [`pulpit_core::annotation`] — they are
//! presentation state, not configuration. What is configured here is only
//! what the palette starts out doing, so a presenter who always spotlights
//! does not have to arm the spotlight at the top of every talk.

use serde::{Deserialize, Serialize};

use pulpit_core::annotation::{
    AnnotationStyle, AnnotationTool, InkColor, ERASER_RADIUS_RANGE, HIGHLIGHT_WIDTH_RANGE,
};

/// How an annotation palette is configured.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnotationOptions {
    /// The tool the palette offers first. Arming is still a deliberate act:
    /// a default tool is not an armed tool, because a pointer that draws the
    /// moment a presentation opens would take every press away from the
    /// document's own links.
    pub tool: AnnotationTool,
    pub ink_color: InkColor,
    pub highlight_color: InkColor,
    pub pointer_color: InkColor,
    pub text_color: InkColor,
    /// Whether the pointer control lights a circle rather than showing a dot.
    ///
    /// One control with a mode rather than two buttons: pointing at a line
    /// and lighting the paragraph around it are the same gesture, and the
    /// palette has a row to fit.
    pub pointer_spotlight: bool,
    /// Stroke width as a fraction of the page width.
    pub ink_width: f32,
    pub highlight_width: f32,
    /// Radius of the stroke eraser as a fraction of the page width.
    pub eraser_radius: f32,
    /// Text height as a fraction of the page width.
    pub text_size: f32,
    /// Radius of the pointer dot as a fraction of the page width.
    pub pointer_radius: f32,
    /// Spotlight radius as a fraction of the page width.
    pub spotlight_radius: f32,
    /// Whether the audience screen shows annotations to begin with.
    ///
    /// On by default: a mark is drawn to be seen, so the projector shows it
    /// unless the presenter asks to keep it to their own copy.
    pub audience_visible: bool,
}

impl Default for AnnotationOptions {
    fn default() -> Self {
        let style = AnnotationStyle::default();
        Self {
            tool: AnnotationTool::Ink,
            ink_color: InkColor::default(),
            highlight_color: InkColor::Yellow,
            pointer_color: style.pointer_color,
            text_color: InkColor::Black,
            pointer_spotlight: false,
            ink_width: style.ink_width,
            highlight_width: 0.025,
            eraser_radius: 0.02,
            text_size: 0.025,
            pointer_radius: style.pointer_radius,
            spotlight_radius: style.spotlight_radius,
            audience_visible: true,
        }
    }
}

impl AnnotationOptions {
    /// Bring the measures back inside the ranges the model allows.
    pub fn sanitise(&mut self) {
        let mut style = self.style();
        style.sanitise();
        self.ink_width = style.ink_width;
        self.spotlight_radius = style.spotlight_radius;
        self.pointer_radius = style.pointer_radius;
        self.highlight_width = bound(self.highlight_width, 0.025, HIGHLIGHT_WIDTH_RANGE);
        self.eraser_radius = bound(self.eraser_radius, 0.02, ERASER_RADIUS_RANGE);
        self.text_size = bound(self.text_size, 0.025, (0.008, 0.12));
    }

    /// The drawing measures these options imply. Only the dimming behind the
    /// spotlight is not offered — it is the same on every deck — so it comes
    /// straight from the model's default.
    pub fn style(&self) -> AnnotationStyle {
        AnnotationStyle {
            ink_width: self.ink_width,
            spotlight_radius: self.spotlight_radius,
            pointer_radius: self.pointer_radius,
            pointer_color: self.pointer_color,
            ..AnnotationStyle::default()
        }
    }

    /// The tool the pointer control arms, which is the mode it is in.
    pub fn pointer_tool(&self) -> AnnotationTool {
        if self.pointer_spotlight {
            AnnotationTool::Spotlight
        } else {
            AnnotationTool::Pointer
        }
    }
}

/// An edit to an annotation palette.
///
/// Nothing sends one today, for the reason given in `widgets::patch`: the
/// designer has no properties panel yet. The vocabulary is here because the
/// rules it encodes are the hard part.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum AnnotationPatch {
    Tool(AnnotationTool),
    InkColor(InkColor),
    InkWidth(f32),
    HighlightColor(InkColor),
    HighlightWidth(f32),
    EraserRadius(f32),
    SpotlightRadius(f32),
    PointerColor(InkColor),
    PointerRadius(f32),
    PointerSpotlight(bool),
    TextColor(InkColor),
    TextSize(f32),
    AudienceVisible(bool),
}

impl AnnotationPatch {
    pub fn apply(self, options: &mut AnnotationOptions) {
        match self {
            AnnotationPatch::Tool(value) => options.tool = value,
            AnnotationPatch::InkColor(value) => options.ink_color = value,
            AnnotationPatch::InkWidth(value) => options.ink_width = value,
            AnnotationPatch::HighlightColor(value) => options.highlight_color = value,
            AnnotationPatch::HighlightWidth(value) => options.highlight_width = value,
            AnnotationPatch::EraserRadius(value) => options.eraser_radius = value,
            AnnotationPatch::SpotlightRadius(value) => options.spotlight_radius = value,
            AnnotationPatch::PointerColor(value) => options.pointer_color = value,
            AnnotationPatch::PointerRadius(value) => options.pointer_radius = value,
            AnnotationPatch::PointerSpotlight(value) => options.pointer_spotlight = value,
            AnnotationPatch::TextColor(value) => options.text_color = value,
            AnnotationPatch::TextSize(value) => options.text_size = value,
            AnnotationPatch::AudienceVisible(value) => options.audience_visible = value,
        }
    }
}

fn bound(value: f32, fallback: f32, range: (f32, f32)) -> f32 {
    if value.is_finite() {
        value.clamp(range.0, range.1)
    } else {
        fallback
    }
}

/// Transient state owned by the live presenter palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnnotationControls {
    pub options: AnnotationOptions,
    pub open: Option<AnnotationTool>,
    /// Whether the overflow menu — everything the palette was too narrow to
    /// draw — is showing.
    pub overflow: bool,
    /// Which tool's colour wheel is open, if any.
    ///
    /// The wheel is anchored to the tool's own button rather than drawn
    /// inside its options panel: a wheel is a thing to drag around in, and
    /// dragging inside a panel that is itself floating over the slide is one
    /// layer of floating too many.
    pub wheel: Option<AnnotationTool>,
    /// Whether there is anything to write out: marks somewhere in the deck,
    /// and a document on disk to write a copy of.
    pub can_save: bool,
}

impl AnnotationControls {
    pub fn new(options: AnnotationOptions) -> Self {
        Self {
            options,
            open: None,
            overflow: false,
            wheel: None,
            can_save: false,
        }
    }
}

impl Default for AnnotationControls {
    fn default() -> Self {
        Self::new(AnnotationOptions::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::annotation::{INK_WIDTH_RANGE, SPOTLIGHT_RADIUS_RANGE};

    #[test]
    fn a_palette_starts_with_ink_shown_to_the_audience() {
        let options = AnnotationOptions::default();
        assert!(
            options.audience_visible,
            "a mark is drawn to be seen; hiding it is the deliberate choice"
        );
        assert_eq!(options.tool, AnnotationTool::Ink);
        assert_eq!(options.ink_color, InkColor::Black);
        assert_eq!(options.text_color, InkColor::Black);
    }

    #[test]
    fn absurd_measures_are_bounded_by_the_model_not_the_palette() {
        let mut options = AnnotationOptions {
            ink_width: 40.0,
            spotlight_radius: 0.0,
            ..AnnotationOptions::default()
        };
        options.sanitise();
        assert_eq!(options.ink_width, INK_WIDTH_RANGE.1);
        assert_eq!(options.spotlight_radius, SPOTLIGHT_RADIUS_RANGE.0);
    }

    #[test]
    fn a_patch_changes_one_thing_and_leaves_the_rest_alone() {
        let mut options = AnnotationOptions::default();
        AnnotationPatch::InkColor(InkColor::Cyan).apply(&mut options);
        assert_eq!(options.ink_color, InkColor::Cyan);
        assert_eq!(options.tool, AnnotationOptions::default().tool);

        AnnotationPatch::AudienceVisible(true).apply(&mut options);
        AnnotationPatch::Tool(AnnotationTool::Ink).apply(&mut options);
        assert!(options.audience_visible);
        assert_eq!(options.tool, AnnotationTool::Ink);
        assert_eq!(options.ink_color, InkColor::Cyan);
    }

    #[test]
    fn every_measure_starts_inside_the_range_its_slider_offers() {
        // A size control drawn against the wrong range is a control that
        // fights the model: the value sits off the end of the track, and
        // whatever the presenter drags it to is clamped back on the way in.
        let options = AnnotationOptions::default();
        for (value, range, what) in [
            (options.ink_width, INK_WIDTH_RANGE, "ink"),
            (
                options.highlight_width,
                pulpit_core::annotation::HIGHLIGHT_WIDTH_RANGE,
                "highlighter",
            ),
            (
                options.eraser_radius,
                pulpit_core::annotation::ERASER_RADIUS_RANGE,
                "eraser",
            ),
            (
                options.pointer_radius,
                pulpit_core::annotation::POINTER_RADIUS_RANGE,
                "pointer",
            ),
            (
                options.spotlight_radius,
                SPOTLIGHT_RADIUS_RANGE,
                "spotlight",
            ),
        ] {
            assert!(
                (range.0..=range.1).contains(&value),
                "the {what} measure {value} is outside {range:?}"
            );
        }
    }

    #[test]
    fn a_size_the_presenter_drags_to_is_kept_rather_than_snapped_back() {
        let mut options = AnnotationOptions::default();
        AnnotationPatch::PointerRadius(0.04).apply(&mut options);
        AnnotationPatch::SpotlightRadius(0.2).apply(&mut options);
        options.sanitise();
        assert_eq!(options.pointer_radius, 0.04);
        assert_eq!(options.spotlight_radius, 0.2);
        assert_eq!(
            options.style().pointer_radius,
            0.04,
            "and reaches the marks"
        );
    }

    #[test]
    fn the_pointer_control_arms_whichever_thing_it_is_set_to_be() {
        let mut options = AnnotationOptions::default();
        assert_eq!(options.pointer_tool(), AnnotationTool::Pointer);
        AnnotationPatch::PointerSpotlight(true).apply(&mut options);
        assert_eq!(options.pointer_tool(), AnnotationTool::Spotlight);

        AnnotationPatch::PointerColor(InkColor::Green).apply(&mut options);
        assert_eq!(options.style().pointer_color, InkColor::Green);
    }

    #[test]
    fn the_configured_measures_reach_the_drawing_style() {
        let options = AnnotationOptions {
            ink_width: 0.01,
            spotlight_radius: 0.2,
            ..AnnotationOptions::default()
        };
        let style = options.style();
        assert_eq!(style.ink_width, 0.01);
        assert_eq!(style.spotlight_radius, 0.2);
        assert_eq!(
            style.pointer_radius,
            AnnotationStyle::default().pointer_radius,
            "what is not offered comes from the model"
        );
    }
}
