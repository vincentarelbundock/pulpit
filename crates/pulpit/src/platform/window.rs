//! Window lifecycle and geometry policy.

use serde::{Deserialize, Serialize};

/// A window rectangle in **logical** pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Bounds {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Bounds {
        Bounds {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn intersects(&self, other: &Bounds) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// Lifecycle and geometry rules that differ by platform.
pub trait WindowPolicy: Send + Sync {
    /// Reverse-DNS application identity, used for desktop integration.
    #[allow(dead_code)] // reached by its tests, not by the application
    fn application_id(&self) -> &'static str;

    /// Smallest window in which the presenter view still works.
    fn minimum_size(&self) -> (f32, f32);

    /// Does closing the last window quit the application?
    ///
    /// macOS keeps the process alive; Windows and Linux do not.
    #[allow(dead_code)] // reached by its tests, not by the application
    fn quit_on_last_window_closed(&self) -> bool;

    /// Bring restored bounds back into a usable work area.
    fn clamp_to_work_area(&self, bounds: Bounds, work_areas: &[Bounds]) -> Bounds {
        clamp_to_work_area(bounds, work_areas, self.minimum_size())
    }
}

/// The portable rules.
#[derive(Debug, Default, Clone, Copy)]
pub struct DesktopWindowPolicy;

impl WindowPolicy for DesktopWindowPolicy {
    fn application_id(&self) -> &'static str {
        "com.example.pulpit"
    }

    fn minimum_size(&self) -> (f32, f32) {
        // The compact bands and drawers keep every action reachable at this
        // width. Height still reserves enough room for a full control target
        // above or below the working surface.
        (480.0, 600.0)
    }

    fn quit_on_last_window_closed(&self) -> bool {
        !cfg!(target_os = "macos")
    }
}

/// Pull a window back onto a display.
///
/// A window restored from settings after the monitor it lived on went away
/// must not reopen off-screen. The rule: if it overlaps any work area, leave
/// it alone; otherwise move it wholly onto the first one, shrinking only if
/// it cannot fit.
pub fn clamp_to_work_area(bounds: Bounds, work_areas: &[Bounds], minimum: (f32, f32)) -> Bounds {
    let mut bounds = Bounds {
        width: bounds.width.max(minimum.0),
        height: bounds.height.max(minimum.1),
        ..bounds
    };
    if work_areas.is_empty() {
        return bounds;
    }
    if work_areas.iter().any(|area| area.intersects(&bounds)) {
        return bounds;
    }

    let area = work_areas[0];
    bounds.width = bounds.width.min(area.width);
    bounds.height = bounds.height.min(area.height);
    bounds.x = area.x + ((area.width - bounds.width) / 2.0).max(0.0);
    bounds.y = area.y + ((area.height - bounds.height) / 2.0).max(0.0);
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laptop() -> Bounds {
        Bounds::new(0.0, 0.0, 1920.0, 1080.0)
    }

    #[test]
    fn a_window_on_a_live_display_is_left_alone() {
        let policy = DesktopWindowPolicy;
        let bounds = Bounds::new(100.0, 80.0, 1280.0, 800.0);
        assert_eq!(policy.clamp_to_work_area(bounds, &[laptop()]), bounds);
    }

    #[test]
    fn the_desktop_policy_allows_a_narrow_working_window() {
        assert_eq!(DesktopWindowPolicy.minimum_size(), (480.0, 600.0));
    }

    #[test]
    fn a_window_restored_onto_a_vanished_display_comes_back() {
        let policy = DesktopWindowPolicy;
        // Saved while a projector was to the right; the projector is gone.
        let stranded = Bounds::new(3000.0, 200.0, 1280.0, 800.0);
        let clamped = policy.clamp_to_work_area(stranded, &[laptop()]);

        assert!(
            laptop().intersects(&clamped),
            "the window must land on a real display, got {clamped:?}"
        );
        assert_eq!(clamped.width, 1280.0, "its size is preserved when it fits");
    }

    #[test]
    fn a_window_larger_than_the_display_is_shrunk_to_fit() {
        let small = Bounds::new(0.0, 0.0, 1024.0, 600.0);
        let huge = Bounds::new(-5000.0, -5000.0, 3840.0, 2160.0);
        let clamped = clamp_to_work_area(huge, &[small], (720.0, 480.0));
        assert_eq!(clamped.width, 1024.0);
        assert_eq!(clamped.height, 600.0);
        assert!(small.intersects(&clamped));
    }

    #[test]
    fn a_window_is_never_restored_below_the_minimum_size() {
        let clamped = clamp_to_work_area(
            Bounds::new(0.0, 0.0, 100.0, 50.0),
            &[laptop()],
            (720.0, 480.0),
        );
        assert_eq!((clamped.width, clamped.height), (720.0, 480.0));
    }

    #[test]
    fn with_no_displays_reported_nothing_is_moved() {
        let bounds = Bounds::new(10.0, 10.0, 800.0, 600.0);
        assert_eq!(clamp_to_work_area(bounds, &[], (720.0, 480.0)), bounds);
    }

    #[test]
    fn lifecycle_follows_the_platform() {
        let policy = DesktopWindowPolicy;
        assert_eq!(
            policy.quit_on_last_window_closed(),
            !cfg!(target_os = "macos")
        );
        assert!(policy.application_id().contains('.'));
    }
}
