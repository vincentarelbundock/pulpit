//! The unfinished stroke, drawn by the UI while the pointer is down.
//!
//! Invariant A2, and the reason it is permitted: a stroke that only appeared
//! when its frame came back would follow the hand a round trip late, and the
//! round trip includes rasterising a page. So the UI draws the gesture itself,
//! from the points it already has.
//!
//! What makes that safe rather than a second copy of the mark:
//!
//! * the preview is *only* the open gesture. On release the gesture is
//!   consumed and this draws nothing, so there is no state here that could
//!   outlive a commit and become a duplicate (A1);
//! * it is removed when a frame at or beyond the commit's revision arrives,
//!   which is the reader session's decision and not this file's (§9.2);
//! * it is never painted over a frame that already contains the same mark,
//!   because by then the gesture is gone.

use iced::widget::canvas as canvas_widget;
use iced::widget::canvas::{self, Path, Stroke};
use iced::{Color, Element, Length, Point, Renderer, Size, Theme};

use pulpit_core::page::PagePoint;

/// The open gesture, ready to draw over one sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct GesturePreview {
    /// The stroke's points, in canonical page space (A4).
    pub points: Vec<PagePoint>,
    /// The text runs a selection has resolved to so far, also canonical.
    pub quads: Vec<pulpit_core::page::PageQuad>,
    /// sRGB, as the tool will lay it down.
    pub color: (f32, f32, f32),
    pub opacity: f32,
    /// Painted width in page points.
    pub width: f32,
}

impl GesturePreview {
    pub fn is_empty(&self) -> bool {
        self.points.len() < 1 && self.quads.is_empty()
    }
}

/// Draw `preview` at the size a sheet was drawn at.
///
/// `canonical` is the page's size in points and `drawn` its size on screen;
/// the ratio between them is the only scaling this does, which is what keeps
/// the preview and the committed mark the same shape.
pub fn layer<'a, Message: 'a>(
    preview: GesturePreview,
    canonical: (f32, f32),
    drawn: (f32, f32),
) -> Element<'a, Message> {
    canvas_widget::Canvas::new(Painter {
        preview,
        canonical,
        drawn,
    })
    .width(Length::Fixed(drawn.0))
    .height(Length::Fixed(drawn.1))
    .into()
}

struct Painter {
    preview: GesturePreview,
    canonical: (f32, f32),
    drawn: (f32, f32),
}

impl<Message> canvas::Program<Message> for Painter {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        // Deliberately uncached: the gesture changes on every pointer sample,
        // which is the one case where a cache is pure overhead. The committed
        // mark is drawn by the renderer, not here.
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        let scale_x = if self.canonical.0 > 0.0 {
            self.drawn.0 / self.canonical.0
        } else {
            1.0
        };
        let scale_y = if self.canonical.1 > 0.0 {
            self.drawn.1 / self.canonical.1
        } else {
            1.0
        };
        let place = |point: &PagePoint| Point::new(point.x * scale_x, point.y * scale_y);

        let (red, green, blue) = self.preview.color;
        let color = Color::from_rgba(red, green, blue, self.preview.opacity.clamp(0.0, 1.0));

        // A selection's runs, as translucent blocks: what the highlighter
        // would mark if the pointer came up now.
        for quad in &self.preview.quads {
            let bounds = quad.bounds();
            let top_left = place(&PagePoint::new(bounds.left, bounds.top));
            let size = Size::new(
                bounds.width() * scale_x,
                (bounds.height() * scale_y).max(1.0),
            );
            frame.fill_rectangle(top_left, size, color);
        }

        match self.preview.points.len() {
            0 => {}
            // A press that has not moved is a dot, and a dot is a mark
            // somebody meant to make (§7.1) — so it is previewed as one
            // rather than as nothing.
            1 => {
                let at = place(&self.preview.points[0]);
                let radius = (self.preview.width * scale_x / 2.0).max(0.5);
                frame.fill(&Path::circle(at, radius), color);
            }
            _ => {
                let path = Path::new(|builder| {
                    builder.move_to(place(&self.preview.points[0]));
                    for point in &self.preview.points[1..] {
                        builder.line_to(place(point));
                    }
                });
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(color)
                        .with_width((self.preview.width * scale_x).max(0.5))
                        // Round joins and caps, because a hand-drawn stroke
                        // has no corners and a mitre on a tight curve spikes.
                        .with_line_cap(canvas::LineCap::Round)
                        .with_line_join(canvas::LineJoin::Round),
                );
            }
        }

        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preview_with_nothing_in_it_draws_nothing() {
        let empty = GesturePreview {
            points: Vec::new(),
            quads: Vec::new(),
            color: (1.0, 0.0, 0.0),
            opacity: 1.0,
            width: 2.0,
        };
        assert!(empty.is_empty());

        let dot = GesturePreview {
            points: vec![PagePoint::new(10.0, 10.0)],
            ..empty.clone()
        };
        assert!(!dot.is_empty(), "a dot is a mark somebody meant to make");
    }
}
