//! Input routing between the presenter and interactive overlays
//! (`docs-src/internals.typ`).
//!
//! Two rules decide everything here. Interactive overlays get pointer events
//! before the PDF's own links, because an HTML widget drawn over a link is
//! meant to be clicked. And global presenter shortcuts stay global: a deck
//! that takes focus must never be able to swallow the key that advances the
//! slide, so Escape always releases focus.

use pulpit_core::OverlayId;
use pulpit_media::protocol::{InputEvent, PointerButton};

/// Where one input event went.
#[derive(Debug, Clone, PartialEq)]
pub enum Routed {
    /// Consumed by an overlay.
    ToOverlay {
        overlay: OverlayId,
        event: InputEvent,
    },
    /// Not over any interactive overlay: pulpit's own behaviour applies.
    ToApplication,
    /// A global shortcut, taken even while an overlay has focus.
    GlobalShortcut,
    /// Escape while focused: leave the overlay rather than tell it anything.
    ReleaseFocus,
}

/// Keys the presenter must never lose to focused web content.
///
/// Navigation and the blanking and timer controls are the presentation
/// itself; a deck cannot be allowed to capture them.
pub const GLOBAL_KEYS: &[&str] = &[
    "ArrowLeft",
    "ArrowRight",
    "PageUp",
    "PageDown",
    "Home",
    "End",
    "F1",
    "F5",
    "F11",
];

/// Which overlay currently has keyboard focus, and what a keypress means.
#[derive(Debug, Clone, Default)]
pub struct InputRouter {
    focused: Option<OverlayId>,
    /// The overlay the pointer was last inside, so it can be told when the
    /// pointer leaves.
    hovered: Option<OverlayId>,
    click_count: u32,
    last_press: Option<(OverlayId, PointerButton)>,
}

impl InputRouter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn focused(&self) -> Option<OverlayId> {
        self.focused
    }

    pub fn hovered(&self) -> Option<OverlayId> {
        self.hovered
    }

    pub fn focus(&mut self, overlay: Option<OverlayId>) {
        self.focused = overlay;
    }

    /// The pointer moved. `over` is the topmost interactive overlay under it,
    /// with the position inside that overlay.
    ///
    /// Leaving one overlay produces a `PointerLeft` for it, so hover states
    /// in the page clear instead of sticking.
    pub fn pointer_moved(&mut self, over: Option<(OverlayId, (f32, f32))>) -> Vec<Routed> {
        let mut routed = Vec::new();
        let now_over = over.map(|(overlay, _)| overlay);
        if let Some(previous) = self.hovered {
            if Some(previous) != now_over {
                routed.push(Routed::ToOverlay {
                    overlay: previous,
                    event: InputEvent::PointerLeft,
                });
            }
        }
        self.hovered = now_over;
        match over {
            Some((overlay, (x, y))) => routed.push(Routed::ToOverlay {
                overlay,
                event: InputEvent::PointerMoved { x, y },
            }),
            None => routed.push(Routed::ToApplication),
        }
        routed
    }

    /// A button went down. An overlay under the pointer takes it — and takes
    /// focus, so subsequent keystrokes reach the page.
    pub fn pointer_pressed(
        &mut self,
        over: Option<(OverlayId, (f32, f32))>,
        button: PointerButton,
    ) -> Routed {
        let Some((overlay, (x, y))) = over else {
            // A press outside every overlay releases focus and then behaves
            // exactly as pulpit always has: it may follow a PDF link.
            self.focused = None;
            self.last_press = None;
            return Routed::ToApplication;
        };
        self.click_count = match self.last_press {
            Some((previous, previous_button))
                if previous == overlay && previous_button == button =>
            {
                self.click_count.saturating_add(1).min(3)
            }
            _ => 1,
        };
        self.last_press = Some((overlay, button));
        self.focused = Some(overlay);
        Routed::ToOverlay {
            overlay,
            event: InputEvent::PointerPressed {
                x,
                y,
                button,
                click_count: self.click_count,
            },
        }
    }

    pub fn pointer_released(
        &mut self,
        over: Option<(OverlayId, (f32, f32))>,
        button: PointerButton,
    ) -> Routed {
        match over {
            Some((overlay, (x, y))) => Routed::ToOverlay {
                overlay,
                event: InputEvent::PointerReleased {
                    x,
                    y,
                    button,
                    click_count: self.click_count.max(1),
                },
            },
            None => Routed::ToApplication,
        }
    }

    pub fn wheel(
        &mut self,
        over: Option<(OverlayId, (f32, f32))>,
        delta_x: f32,
        delta_y: f32,
    ) -> Routed {
        match over {
            Some((overlay, (x, y))) => Routed::ToOverlay {
                overlay,
                event: InputEvent::Wheel {
                    x,
                    y,
                    delta_x,
                    delta_y,
                },
            },
            None => Routed::ToApplication,
        }
    }

    /// A key was pressed.
    ///
    /// The order is deliberate: global shortcuts first, then Escape, then the
    /// focused overlay. A deck cannot capture navigation however much
    /// JavaScript it runs.
    pub fn key_pressed(&mut self, key: &str, text: Option<String>) -> Routed {
        if GLOBAL_KEYS.contains(&key) {
            return Routed::GlobalShortcut;
        }
        let Some(overlay) = self.focused else {
            return Routed::ToApplication;
        };
        if key == "Escape" {
            self.focused = None;
            return Routed::ReleaseFocus;
        }
        Routed::ToOverlay {
            overlay,
            event: InputEvent::KeyPressed {
                key: key.to_string(),
                text,
            },
        }
    }

    pub fn key_released(&mut self, key: &str) -> Routed {
        if GLOBAL_KEYS.contains(&key) {
            return Routed::GlobalShortcut;
        }
        match self.focused {
            Some(overlay) => Routed::ToOverlay {
                overlay,
                event: InputEvent::KeyReleased {
                    key: key.to_string(),
                },
            },
            None => Routed::ToApplication,
        }
    }

    /// The overlay went away — a new page, a new document, a failure.
    pub fn forget(&mut self, overlay: OverlayId) {
        if self.focused == Some(overlay) {
            self.focused = None;
        }
        if self.hovered == Some(overlay) {
            self.hovered = None;
        }
        if self.last_press.map(|(id, _)| id) == Some(overlay) {
            self.last_press = None;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BALLS: OverlayId = OverlayId(1);
    const OTHER: OverlayId = OverlayId(2);

    #[test]
    fn a_press_inside_an_overlay_reaches_it_and_takes_focus() {
        let mut router = InputRouter::new();
        let routed = router.pointer_pressed(Some((BALLS, (0.5, 0.5))), PointerButton::Left);
        assert!(matches!(
            routed,
            Routed::ToOverlay {
                overlay: BALLS,
                event: InputEvent::PointerPressed { click_count: 1, .. }
            }
        ));
        assert_eq!(router.focused(), Some(BALLS));
    }

    #[test]
    fn a_press_outside_every_overlay_releases_focus_and_goes_to_pulpit() {
        let mut router = InputRouter::new();
        router.pointer_pressed(Some((BALLS, (0.5, 0.5))), PointerButton::Left);
        assert_eq!(router.focused(), Some(BALLS));

        // A click on the slide beside the overlay still follows PDF links.
        let routed = router.pointer_pressed(None, PointerButton::Left);
        assert_eq!(routed, Routed::ToApplication);
        assert_eq!(router.focused(), None);
    }

    #[test]
    fn repeated_presses_on_one_overlay_count_up_and_reset_elsewhere() {
        let mut router = InputRouter::new();
        for expected in 1..=3 {
            let routed = router.pointer_pressed(Some((BALLS, (0.5, 0.5))), PointerButton::Left);
            match routed {
                Routed::ToOverlay {
                    event: InputEvent::PointerPressed { click_count, .. },
                    ..
                } => assert_eq!(click_count, expected),
                other => panic!("expected a press, got {other:?}"),
            }
        }
        // A different overlay starts counting again.
        let routed = router.pointer_pressed(Some((OTHER, (0.5, 0.5))), PointerButton::Left);
        match routed {
            Routed::ToOverlay {
                event: InputEvent::PointerPressed { click_count, .. },
                ..
            } => assert_eq!(click_count, 1),
            other => panic!("expected a press, got {other:?}"),
        }
    }

    #[test]
    fn leaving_an_overlay_tells_it_so_its_hover_state_clears() {
        let mut router = InputRouter::new();
        router.pointer_moved(Some((BALLS, (0.5, 0.5))));

        let routed = router.pointer_moved(None);
        assert_eq!(
            routed[0],
            Routed::ToOverlay {
                overlay: BALLS,
                event: InputEvent::PointerLeft
            }
        );
        assert_eq!(routed[1], Routed::ToApplication);
        assert_eq!(router.hovered(), None);
    }

    #[test]
    fn moving_between_two_overlays_leaves_the_first_and_enters_the_second() {
        let mut router = InputRouter::new();
        router.pointer_moved(Some((BALLS, (0.5, 0.5))));
        let routed = router.pointer_moved(Some((OTHER, (0.1, 0.1))));
        assert_eq!(
            routed[0],
            Routed::ToOverlay {
                overlay: BALLS,
                event: InputEvent::PointerLeft
            }
        );
        assert!(matches!(
            routed[1],
            Routed::ToOverlay {
                overlay: OTHER,
                event: InputEvent::PointerMoved { .. }
            }
        ));
    }

    #[test]
    fn moving_within_one_overlay_does_not_keep_announcing_a_departure() {
        let mut router = InputRouter::new();
        router.pointer_moved(Some((BALLS, (0.1, 0.1))));
        let routed = router.pointer_moved(Some((BALLS, (0.2, 0.2))));
        assert_eq!(routed.len(), 1);
        assert!(matches!(
            routed[0],
            Routed::ToOverlay {
                event: InputEvent::PointerMoved { .. },
                ..
            }
        ));
    }

    #[test]
    fn navigation_keys_stay_global_even_while_a_deck_has_focus() {
        let mut router = InputRouter::new();
        router.focus(Some(BALLS));
        for key in ["ArrowRight", "PageDown", "Home", "F5"] {
            assert_eq!(
                router.key_pressed(key, None),
                Routed::GlobalShortcut,
                "{key} must never be captured by page JavaScript"
            );
        }
        assert_eq!(
            router.focused(),
            Some(BALLS),
            "a global shortcut does not disturb focus"
        );
    }

    #[test]
    fn escape_always_provides_a_way_out_of_web_focus() {
        let mut router = InputRouter::new();
        router.focus(Some(BALLS));
        assert_eq!(router.key_pressed("Escape", None), Routed::ReleaseFocus);
        assert_eq!(router.focused(), None);
    }

    #[test]
    fn ordinary_keys_reach_the_focused_overlay_and_nothing_otherwise() {
        let mut router = InputRouter::new();
        assert_eq!(
            router.key_pressed("a", Some("a".into())),
            Routed::ToApplication,
            "without focus, keys are pulpit's"
        );

        router.focus(Some(BALLS));
        assert!(matches!(
            router.key_pressed("a", Some("a".into())),
            Routed::ToOverlay {
                overlay: BALLS,
                event: InputEvent::KeyPressed { .. }
            }
        ));
    }

    #[test]
    fn a_wheel_over_an_overlay_scrolls_it_rather_than_the_deck() {
        let mut router = InputRouter::new();
        assert!(matches!(
            router.wheel(Some((BALLS, (0.5, 0.5))), 0.0, -120.0),
            Routed::ToOverlay {
                event: InputEvent::Wheel { delta_y, .. },
                ..
            } if delta_y == -120.0
        ));
        assert_eq!(router.wheel(None, 0.0, -120.0), Routed::ToApplication);
    }

    #[test]
    fn forgetting_an_overlay_clears_every_reference_to_it() {
        let mut router = InputRouter::new();
        router.pointer_moved(Some((BALLS, (0.5, 0.5))));
        router.pointer_pressed(Some((BALLS, (0.5, 0.5))), PointerButton::Left);
        assert_eq!(router.focused(), Some(BALLS));

        router.forget(BALLS);
        assert_eq!(router.focused(), None);
        assert_eq!(router.hovered(), None);
        // The click counter restarts rather than carrying over to whatever
        // takes that overlay's place.
        match router.pointer_pressed(Some((BALLS, (0.5, 0.5))), PointerButton::Left) {
            Routed::ToOverlay {
                event: InputEvent::PointerPressed { click_count, .. },
                ..
            } => assert_eq!(click_count, 1),
            other => panic!("expected a press, got {other:?}"),
        }
    }

    #[test]
    fn every_event_a_router_emits_is_one_a_worker_will_accept() {
        let mut router = InputRouter::new();
        let mut events = router.pointer_moved(Some((BALLS, (0.5, 0.5))));
        events.push(router.pointer_pressed(Some((BALLS, (0.5, 0.5))), PointerButton::Left));
        events.push(router.pointer_released(Some((BALLS, (0.5, 0.5))), PointerButton::Left));
        events.push(router.wheel(Some((BALLS, (0.5, 0.5))), 1.0, -2.0));
        router.focus(Some(BALLS));
        events.push(router.key_pressed("a", Some("a".into())));

        for routed in events {
            if let Routed::ToOverlay { event, .. } = routed {
                assert!(event.validate().is_ok(), "{event:?} would be refused");
            }
        }
    }
}
