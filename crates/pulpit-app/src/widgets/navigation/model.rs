#![allow(dead_code)] // configuration vocabulary, kept for when it is offered again
//! Moving through the deck: buttons, slider, counter.

use serde::{Deserialize, Serialize};

/// How far one coarse scrub movement travels, for a deck of `count` slides.
///
/// A slider that only ever moves one slide is unusable on a long deck: it
/// takes forty presses to cross forty slides, and a finger dragging a 32-pixel
/// track cannot place itself to the slide anyway. A tenth of the deck is far
/// enough to be worth the key, and the bounds keep it sane at both extremes —
/// never zero on a short deck, never a wild jump on a very long one.
pub fn coarse_step(count: usize) -> usize {
    (count / 10).clamp(1, 25)
}

/// The back-and-forward buttons on their own.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ButtonsOptions {
    pub back: bool,
    pub forward: bool,
    /// Words beside the arrows, where there is room for them.
    pub labels: bool,
}

impl Default for ButtonsOptions {
    fn default() -> Self {
        Self {
            back: true,
            forward: true,
            labels: true,
        }
    }
}

impl ButtonsOptions {
    pub fn is_empty(&self) -> bool {
        !self.back && !self.forward
    }
}

/// An edit to the buttons.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ButtonsPatch {
    Back(bool),
    Forward(bool),
    Labels(bool),
}

impl ButtonsPatch {
    pub fn apply(self, options: &mut ButtonsOptions) {
        match self {
            ButtonsPatch::Back(value) => options.back = value,
            ButtonsPatch::Forward(value) => options.forward = value,
            ButtonsPatch::Labels(value) => options.labels = value,
        }
    }
}

/// What is wrong with a pair of buttons that are both switched off.
pub fn validate_buttons(options: &ButtonsOptions) -> Vec<crate::widgets::Complaint> {
    if options.is_empty() {
        return vec![crate::widgets::Complaint {
            message: "Neither button is shown",
            consequence: "The widget takes space and moves nothing; the deck can only be driven \
                          from the keyboard.",
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod buttons_tests {
    use crate::widgets::navigation::model::ButtonsPatch;
    use crate::widgets::patch::WidgetPatch;
    use crate::widgets::{Widget, WidgetKind};

    #[test]
    fn the_pair_can_be_placed_on_its_own() {
        let buttons = Widget::new(WidgetKind::SlideButtons);
        assert!(buttons.provides_forward_navigation());
        assert!(buttons.provides_backward_navigation());
        assert!(
            buttons.validate((200.0, 60.0)).is_empty(),
            "a working pair has nothing to complain about"
        );
    }

    #[test]
    fn a_pair_with_both_buttons_off_says_so() {
        let mut buttons = Widget::new(WidgetKind::SlideButtons);
        buttons
            .apply(WidgetPatch::Buttons(ButtonsPatch::Back(false)))
            .unwrap();
        buttons
            .apply(WidgetPatch::Buttons(ButtonsPatch::Forward(false)))
            .unwrap();

        assert!(!buttons.provides_forward_navigation());
        assert_eq!(buttons.validate((200.0, 60.0)).len(), 1);
    }

    #[test]
    fn only_one_pair_of_buttons_may_be_placed() {
        assert!(!WidgetKind::SlideButtons.multi_instance());
    }
}

#[cfg(test)]
mod scrub_tests {
    use super::*;

    #[test]
    fn a_coarse_step_is_a_tenth_of_the_deck() {
        assert_eq!(coarse_step(100), 10);
        assert_eq!(coarse_step(40), 4);
    }

    #[test]
    fn a_short_deck_still_moves_by_at_least_one_slide() {
        // Otherwise the key would appear dead on exactly the decks where the
        // slider is easiest to use.
        for count in 0..=9 {
            assert_eq!(coarse_step(count), 1, "{count} slides");
        }
    }

    #[test]
    fn a_very_long_deck_does_not_produce_a_wild_jump() {
        assert_eq!(coarse_step(5_000), 25);
        assert_eq!(coarse_step(usize::MAX), 25);
    }
}
