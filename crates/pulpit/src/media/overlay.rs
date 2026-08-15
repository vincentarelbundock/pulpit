//! Overlay geometry (`docs-src/internals.typ`).
//!
//! One pure function decides where an overlay is drawn and the *same*
//! function decides what a pointer position means. Two implementations would
//! eventually disagree, and the symptom would be a presenter clicking one
//! place and the browser receiving another.

use iced::{ContentFit, Rectangle, Size};
use pulpit_core::notes::Region;

/// Where the page itself is drawn inside a panel, after aspect fitting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PageBox {
    /// The drawn area of a page with `aspect` inside `panel`.
    pub fn fit(panel: Size, aspect: f32, fit: ContentFit) -> Option<PageBox> {
        if panel.width <= 0.0 || panel.height <= 0.0 || aspect <= 0.0 {
            return None;
        }
        let panel_aspect = panel.width / panel.height;
        let wide = panel_aspect > aspect;
        let (width, height) = match fit {
            ContentFit::Cover => {
                if wide {
                    (panel.width, panel.width / aspect)
                } else {
                    (panel.height * aspect, panel.height)
                }
            }
            _ => {
                if wide {
                    (panel.height * aspect, panel.height)
                } else {
                    (panel.width, panel.width / aspect)
                }
            }
        };
        Some(PageBox {
            x: (panel.width - width) / 2.0,
            y: (panel.height - height) / 2.0,
            width,
            height,
        })
    }
}

/// Place an overlay rectangle inside a panel.
///
/// `crop` is the part of the page currently on screen — a `/FitR` zoom shows
/// only some of it — so an overlay's position follows the zoom rather than
/// staying pinned to the panel.
pub fn place(
    panel: Size,
    aspect: f32,
    fit: ContentFit,
    crop: Region,
    overlay: Region,
) -> Option<Rectangle> {
    let page = PageBox::fit(panel, aspect, fit)?;
    if crop.width <= 0.0 || crop.height <= 0.0 {
        return None;
    }
    // Page coordinates → crop-relative → panel pixels.
    let u = (overlay.x - crop.x) / crop.width;
    let v = (overlay.y - crop.y) / crop.height;
    let width = overlay.width / crop.width;
    let height = overlay.height / crop.height;
    Some(Rectangle {
        x: page.x + u * page.width,
        y: page.y + v * page.height,
        width: width * page.width,
        height: height * page.height,
    })
}

/// Where a pointer sits inside an overlay, as a fraction of its rectangle.
///
/// Values outside `0.0..=1.0` mean the pointer is not over the overlay.
/// Letterboxed pixels never land inside one, because the same [`PageBox`]
/// produced both answers.
pub fn pointer_within(
    panel: Size,
    aspect: f32,
    fit: ContentFit,
    crop: Region,
    overlay: Region,
    point: iced::Point,
) -> Option<(f32, f32)> {
    let rectangle = place(panel, aspect, fit, crop, overlay)?;
    if rectangle.width <= 0.0 || rectangle.height <= 0.0 {
        return None;
    }
    Some((
        (point.x - rectangle.x) / rectangle.width,
        (point.y - rectangle.y) / rectangle.height,
    ))
}

/// Is a fractional position inside the overlay?
pub fn contains(position: (f32, f32)) -> bool {
    (0.0..=1.0).contains(&position.0) && (0.0..=1.0).contains(&position.1)
}

/// Convert a fractional overlay position into CSS pixels for a web runtime.
pub fn to_css_pixels(position: (f32, f32), css_width: u32, css_height: u32) -> (f32, f32) {
    (
        position.0 * css_width as f32,
        position.1 * css_height as f32,
    )
}

/// The physical-pixel viewport an overlay needs, given its size on screen.
///
/// Bounded so a maximised window on a high-density display cannot ask a
/// browser for a surface larger than the transport allows. The bound is
/// applied to both axes at once: clamping them independently would hand the
/// browser a viewport of a different shape than the rectangle it is drawn
/// into, and the picture would arrive stretched. A 2× display is what makes
/// that reachable in practice, so the shrink preserves the aspect ratio and
/// only the resolution is lost.
pub fn viewport_for(rectangle: Rectangle, scale: f32) -> (u32, u32) {
    const MAX_EDGE: f32 = 4096.0;
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let mut width = (rectangle.width * scale).max(1.0);
    let mut height = (rectangle.height * scale).max(1.0);
    let longest = width.max(height);
    if longest > MAX_EDGE {
        let shrink = MAX_EDGE / longest;
        width *= shrink;
        height *= shrink;
    }
    let width = width.round().clamp(1.0, MAX_EDGE);
    let height = height.round().clamp(1.0, MAX_EDGE);
    (width as u32, height as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: f32 = 16.0 / 9.0;

    #[test]
    fn a_letterboxed_page_is_centred_in_its_panel() {
        // A 16:9 page in a 200×200 panel: bars top and bottom.
        let page = PageBox::fit(Size::new(200.0, 200.0), WIDE, ContentFit::Contain).unwrap();
        assert!((page.width - 200.0).abs() < 1e-3);
        assert!((page.height - 112.5).abs() < 1e-3);
        assert!((page.x - 0.0).abs() < 1e-3);
        assert!((page.y - 43.75).abs() < 1e-3);
    }

    #[test]
    fn an_overlay_lands_where_its_page_rectangle_says() {
        // The middle quarter of the page, in a panel the page exactly fills.
        let rectangle = place(
            Size::new(1600.0, 900.0),
            WIDE,
            ContentFit::Contain,
            Region::FULL,
            Region::new(0.25, 0.25, 0.5, 0.5),
        )
        .unwrap();
        assert!((rectangle.x - 400.0).abs() < 1e-2);
        assert!((rectangle.y - 225.0).abs() < 1e-2);
        assert!((rectangle.width - 800.0).abs() < 1e-2);
        assert!((rectangle.height - 450.0).abs() < 1e-2);
    }

    #[test]
    fn an_overlay_follows_the_letterbox_rather_than_the_panel() {
        let rectangle = place(
            Size::new(200.0, 200.0),
            WIDE,
            ContentFit::Contain,
            Region::FULL,
            Region::FULL,
        )
        .unwrap();
        assert!(
            (rectangle.y - 43.75).abs() < 1e-3,
            "a full-page overlay starts where the page starts, not at y = 0"
        );
        assert!((rectangle.height - 112.5).abs() < 1e-3);
    }

    #[test]
    fn a_zoom_magnifies_the_overlay_with_the_page() {
        // Showing only the left half of the page doubles everything on it.
        let crop = Region::new(0.0, 0.0, 0.5, 1.0);
        let rectangle = place(
            Size::new(800.0, 900.0),
            0.5 * WIDE,
            ContentFit::Contain,
            crop,
            Region::new(0.0, 0.0, 0.25, 0.5),
        )
        .unwrap();
        assert!(
            (rectangle.width - 400.0).abs() < 1e-2,
            "a quarter-page overlay fills half the cropped view: {rectangle:?}"
        );
    }

    #[test]
    fn an_overlay_outside_the_crop_is_placed_off_screen_rather_than_clamped() {
        let crop = Region::new(0.5, 0.0, 0.5, 1.0);
        let rectangle = place(
            Size::new(800.0, 900.0),
            WIDE,
            ContentFit::Contain,
            crop,
            Region::new(0.0, 0.0, 0.2, 0.2),
        )
        .unwrap();
        assert!(
            rectangle.x < 0.0,
            "content scrolled out of the zoom is not dragged back into view"
        );
    }

    #[test]
    fn the_pointer_mapping_is_the_inverse_of_the_placement() {
        let panel = Size::new(1280.0, 720.0);
        let overlay = Region::new(0.1, 0.2, 0.4, 0.3);
        let rectangle = place(panel, WIDE, ContentFit::Contain, Region::FULL, overlay).unwrap();

        // The centre of the drawn rectangle is the centre of the overlay.
        let centre = iced::Point::new(
            rectangle.x + rectangle.width / 2.0,
            rectangle.y + rectangle.height / 2.0,
        );
        let position = pointer_within(
            panel,
            WIDE,
            ContentFit::Contain,
            Region::FULL,
            overlay,
            centre,
        )
        .unwrap();
        assert!((position.0 - 0.5).abs() < 1e-4);
        assert!((position.1 - 0.5).abs() < 1e-4);
        assert!(contains(position));
    }

    #[test]
    fn a_point_outside_the_overlay_is_reported_as_outside() {
        let panel = Size::new(1280.0, 720.0);
        let overlay = Region::new(0.1, 0.2, 0.2, 0.2);
        let position = pointer_within(
            panel,
            WIDE,
            ContentFit::Contain,
            Region::FULL,
            overlay,
            iced::Point::new(0.0, 0.0),
        )
        .unwrap();
        assert!(
            !contains(position),
            "the panel corner is not in the overlay"
        );
    }

    #[test]
    fn a_press_on_the_letterbox_bar_never_lands_in_a_full_page_overlay() {
        // A 16:9 page in a square panel; y = 5 is on the top bar.
        let panel = Size::new(200.0, 200.0);
        let position = pointer_within(
            panel,
            WIDE,
            ContentFit::Contain,
            Region::FULL,
            Region::FULL,
            iced::Point::new(100.0, 5.0),
        )
        .unwrap();
        assert!(
            !contains(position),
            "letterboxed pixels are not part of the content"
        );
    }

    #[test]
    fn degenerate_geometry_yields_no_rectangle_rather_than_a_panic() {
        assert!(PageBox::fit(Size::new(0.0, 100.0), WIDE, ContentFit::Contain).is_none());
        assert!(PageBox::fit(Size::new(100.0, 100.0), 0.0, ContentFit::Contain).is_none());
        assert!(place(
            Size::new(100.0, 100.0),
            WIDE,
            ContentFit::Contain,
            Region::new(0.0, 0.0, 0.0, 0.0),
            Region::FULL
        )
        .is_none());
    }

    #[test]
    fn panel_space_and_page_space_hit_testing_agree() {
        // The application routes input in *page* space, because the slide
        // panel has already undone the letterbox by the time a cursor
        // position reaches it. `pointer_within` does the same job in panel
        // space. Two ways of answering one question is exactly what the
        // specification warns about, so this pins them together: a
        // disagreement here is the bug where a presenter clicks one place
        // and the browser is told another.
        let panel = Size::new(1280.0, 720.0);
        let overlay = Region::new(0.2, 0.3, 0.4, 0.25);
        let crop = Region::FULL;

        for (u, v) in [(0.25, 0.35), (0.4, 0.4), (0.55, 0.5), (0.21, 0.31)] {
            // Panel space: place the overlay, then ask where a panel point
            // falls inside it.
            let page_box = PageBox::fit(panel, WIDE, ContentFit::Contain).unwrap();
            let point = iced::Point::new(
                page_box.x + u * page_box.width,
                page_box.y + v * page_box.height,
            );
            let panel_answer =
                pointer_within(panel, WIDE, ContentFit::Contain, crop, overlay, point).unwrap();

            // Page space: divide the page coordinate by the overlay's own
            // rectangle, which is what `App::overlay_under_cursor` does.
            let page_answer = (
                (u - overlay.x) / overlay.width,
                (v - overlay.y) / overlay.height,
            );

            assert!(
                (panel_answer.0 - page_answer.0).abs() < 1e-3
                    && (panel_answer.1 - page_answer.1).abs() < 1e-3,
                "at ({u}, {v}): panel space said {panel_answer:?}, page space said {page_answer:?}"
            );
            assert_eq!(
                contains(panel_answer),
                contains(page_answer),
                "at ({u}, {v}) the two disagree about whether the point is inside"
            );
        }
    }

    #[test]
    fn the_two_hit_tests_agree_under_a_zoom_as_well() {
        // A `/FitR` crop is where a second implementation would most easily
        // drift, because only one of the two divides by the crop.
        let panel = Size::new(1280.0, 720.0);
        let crop = Region::new(0.25, 0.25, 0.5, 0.5);
        let overlay = Region::new(0.3, 0.3, 0.2, 0.2);

        for (u, v) in [(0.35, 0.35), (0.45, 0.4), (0.3, 0.3)] {
            let page_box = PageBox::fit(panel, WIDE, ContentFit::Contain).unwrap();
            // Where that page point sits on screen, once cropped.
            let point = iced::Point::new(
                page_box.x + ((u - crop.x) / crop.width) * page_box.width,
                page_box.y + ((v - crop.y) / crop.height) * page_box.height,
            );
            let panel_answer =
                pointer_within(panel, WIDE, ContentFit::Contain, crop, overlay, point).unwrap();
            let page_answer = (
                (u - overlay.x) / overlay.width,
                (v - overlay.y) / overlay.height,
            );
            assert!(
                (panel_answer.0 - page_answer.0).abs() < 1e-3
                    && (panel_answer.1 - page_answer.1).abs() < 1e-3,
                "under a zoom at ({u}, {v}): {panel_answer:?} vs {page_answer:?}"
            );
        }
    }

    #[test]
    fn css_pixels_scale_a_fractional_position_by_the_viewport() {
        assert_eq!(to_css_pixels((0.5, 0.25), 1280, 720), (640.0, 180.0));
        assert_eq!(to_css_pixels((0.0, 0.0), 1280, 720), (0.0, 0.0));
    }

    #[test]
    fn a_viewport_follows_the_device_scale_and_stays_bounded() {
        let rectangle = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 360.0,
        };
        assert_eq!(viewport_for(rectangle, 1.0), (640, 360));
        assert_eq!(viewport_for(rectangle, 2.0), (1280, 720));

        let huge = Rectangle {
            width: 100_000.0,
            height: 10.0,
            ..rectangle
        };
        assert_eq!(viewport_for(huge, 2.0).0, 4096, "the surface stays bounded");

        let degenerate = Rectangle {
            width: 0.0,
            height: 0.0,
            ..rectangle
        };
        assert_eq!(viewport_for(degenerate, 1.0), (1, 1));

        // A full-bleed 16:9 overlay on a 2× display asks for more than the
        // bound on its long edge. Shrinking only that edge would hand the
        // browser a 1.2:1 viewport for a 16:9 rectangle.
        let retina = Rectangle {
            width: 3024.0,
            height: 1701.0,
            ..rectangle
        };
        let (width, height) = viewport_for(retina, 2.0);
        assert_eq!(width, 4096, "the long edge lands on the bound");
        let aspect = width as f32 / height as f32;
        assert!(
            (aspect - WIDE).abs() < 1e-2,
            "the bound preserves the shape: {width}×{height}"
        );

        assert_eq!(viewport_for(rectangle, f32::NAN), (640, 360));
    }
}
