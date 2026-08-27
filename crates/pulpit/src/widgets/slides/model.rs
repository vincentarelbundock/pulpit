//! Slide previews: what to show, and how it fits its pane.

use serde::{Deserialize, Serialize};

/// How a slide image fills the pane it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlideFit {
    /// Whole slide visible, letterboxed. The safe default.
    #[default]
    Fit,
    /// Fill the cell, cropping the overflow. The aspect ratio is still the
    /// document's; only the edges are lost.
    Fill,
    /// Kept so layouts written before it was withdrawn still load. A slide is
    /// a fixed-aspect document — distorting it misrepresents what the author
    /// drew — so this now behaves as `Fit` and is not offered in the editor.
    #[serde(alias = "stretch")]
    Stretch,
}

impl SlideFit {
    #[allow(dead_code)] // see widgets::tokens
    pub const ALL: [SlideFit; 2] = [SlideFit::Fit, SlideFit::Fill];

    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn label(self) -> &'static str {
        match self {
            SlideFit::Fit | SlideFit::Stretch => "Fit",
            SlideFit::Fill => "Fill",
        }
    }
}

/// Which slide a preview pane shows, relative to the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relative {
    Previous,
    Current,
    Next,
}

impl Relative {
    /// The index this pane shows, and whether it exists in the deck.
    ///
    /// Pure, so the deck-boundary behaviour is testable without drawing
    /// anything: the first slide has no previous, the last has no next.
    pub fn index(self, current: usize, count: usize) -> (usize, bool) {
        let index = match self {
            Relative::Previous => current.saturating_sub(1),
            Relative::Current => current,
            Relative::Next => current + 1,
        };
        let exists = match self {
            Relative::Previous => current > 0,
            Relative::Current => count > 0,
            Relative::Next => index < count,
        };
        (index, exists)
    }

    pub fn caption(self) -> &'static str {
        match self {
            Relative::Previous => "Previous",
            Relative::Current => "Current",
            Relative::Next => "Next",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deck_boundaries_are_honest_about_what_exists() {
        // First slide: no previous.
        assert_eq!(Relative::Previous.index(0, 6), (0, false));
        assert_eq!(Relative::Current.index(0, 6), (0, true));
        assert_eq!(Relative::Next.index(0, 6), (1, true));

        // Last slide: no next.
        assert_eq!(Relative::Previous.index(5, 6), (4, true));
        assert_eq!(Relative::Next.index(5, 6), (6, false));

        // No document at all.
        assert_eq!(Relative::Current.index(0, 0), (0, false));
    }

    #[test]
    fn a_withdrawn_fit_still_loads_and_behaves_as_fit() {
        assert_eq!(SlideFit::Stretch.label(), "Fit");
        assert!(!SlideFit::ALL.contains(&SlideFit::Stretch));
    }
}

/// How a slide pane is configured.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SlideOptions {
    pub fit: SlideFit,
}
