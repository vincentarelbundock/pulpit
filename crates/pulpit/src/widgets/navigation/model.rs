//! Moving through the deck: buttons, slider, counter.

use serde::{Deserialize, Serialize};

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
    use crate::widgets::{Widget, WidgetConfig, WidgetKind};

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
        if let WidgetConfig::Buttons(options) = buttons.config_mut() {
            options.back = false;
            options.forward = false;
        }

        assert!(!buttons.provides_forward_navigation());
        assert_eq!(buttons.validate((200.0, 60.0)).len(), 1);
    }

    #[test]
    fn only_one_pair_of_buttons_may_be_placed() {
        assert!(!WidgetKind::SlideButtons.multi_instance());
    }
}
