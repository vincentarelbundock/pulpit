//! Speaker notes: which slide's notes, and how they are set.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotesSource {
    #[default]
    CurrentSlide,
    NextSlide,
}

impl NotesSource {
    #[allow(dead_code)] // unreached, including by its own tests
    pub const ALL: [NotesSource; 2] = [NotesSource::CurrentSlide, NotesSource::NextSlide];

    #[allow(dead_code)] // unreached, including by its own tests
    pub fn label(self) -> &'static str {
        match self {
            NotesSource::CurrentSlide => "Current slide",
            NotesSource::NextSlide => "Next slide",
        }
    }

    /// The slide whose notes this pane shows.
    pub fn slide(self, current: usize) -> usize {
        match self {
            NotesSource::CurrentSlide => current,
            NotesSource::NextSlide => current + 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotesOptions {
    pub font_size: f32,
    pub line_spacing: f32,
    pub source: NotesSource,
}

impl Default for NotesOptions {
    fn default() -> Self {
        Self {
            font_size: 18.0,
            line_spacing: 1.5,
            source: NotesSource::CurrentSlide,
        }
    }
}

/// Readable bounds. Enforced here, not only by the controls that set them.
pub const FONT_SIZE_RANGE: (f32, f32) = (10.0, 48.0);
pub const LINE_SPACING_RANGE: (f32, f32) = (1.0, 2.5);

impl NotesOptions {
    pub fn sanitise(&mut self) {
        self.font_size = self.font_size.clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1);
        self.line_spacing = self
            .line_spacing
            .clamp(LINE_SPACING_RANGE.0, LINE_SPACING_RANGE.1);
    }
}

/// Below this the pane shows a line or two and you scroll during the talk.
pub const USABLE_MINIMUM: (f32, f32) = (300.0, 140.0);

/// What is wrong with this notes pane at this size, if anything.
pub fn validate(_options: &NotesOptions, inner: (f32, f32)) -> Vec<crate::widgets::Complaint> {
    if inner.0 < USABLE_MINIMUM.0 || inner.1 < USABLE_MINIMUM.1 {
        return vec![crate::widgets::Complaint {
            message: "Speaker notes are too small",
            consequence: "Only a line or two will be visible; you will be scrolling during the \
                          talk instead of glancing.",
        }];
    }
    Vec::new()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_decides_which_slides_notes_are_shown() {
        assert_eq!(NotesSource::CurrentSlide.slide(3), 3);
        assert_eq!(NotesSource::NextSlide.slide(3), 4);
    }

    #[test]
    fn unreadable_sizes_are_repaired() {
        let mut options = NotesOptions {
            font_size: 2.0,
            line_spacing: 9.0,
            source: NotesSource::NextSlide,
        };
        options.sanitise();
        assert_eq!(options.font_size, FONT_SIZE_RANGE.0);
        assert_eq!(options.line_spacing, LINE_SPACING_RANGE.1);
        assert_eq!(
            options.source,
            NotesSource::NextSlide,
            "unrelated fields are untouched"
        );
    }
}
