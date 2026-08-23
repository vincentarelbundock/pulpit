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

use iced::mouse;
use iced::widget::canvas as canvas_widget;
use iced::widget::canvas::{self, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use pulpit_core::page::{PagePoint, PageRect};

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

    /// The page rectangle this covers, stroke width included.
    ///
    /// Used to decide whether a partial repaint already contains the mark this
    /// preview is standing in for, so it is the *painted* extent rather than
    /// the path's: half the stroke width lies either side of the line.
    pub fn bounds(&self) -> Option<pulpit_core::page::PageRect> {
        let corners = self
            .quads
            .iter()
            .flat_map(|quad| {
                let bounds = quad.bounds();
                [
                    pulpit_core::page::PagePoint::new(bounds.left, bounds.top),
                    pulpit_core::page::PagePoint::new(bounds.right, bounds.bottom),
                ]
            })
            .chain(self.points.iter().copied());
        let bounds = pulpit_core::page::PageRect::enclosing(corners)?;
        Some(if self.points.is_empty() {
            bounds
        } else {
            bounds.inflated(self.width / 2.0)
        })
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
    origin: (f32, f32),
    drawn: (f32, f32),
) -> Element<'a, Message> {
    canvas_widget::Canvas::new(Painter {
        preview,
        canonical,
        origin,
        drawn,
    })
    .width(Length::Fixed(drawn.0))
    .height(Length::Fixed(drawn.1))
    .into()
}

/// Draw the selected mark's outline and grips over one sheet (§8.4).
///
/// Separate from [`layer`] because it is not a gesture and does not follow the
/// same rules: a gesture preview is a picture of a mark that does not exist
/// yet, and this is a picture of *where* a mark that does exist is. It is
/// chrome, it is drawn in the accent rather than in the mark's own colour, and
/// nothing about it is ever written to the document.
pub fn selection_layer<'a, Message: 'a>(
    selection: crate::widgets::context::SelectedMark,
    accent: Color,
    canonical: (f32, f32),
    origin: (f32, f32),
    drawn: (f32, f32),
) -> Element<'a, Message> {
    canvas_widget::Canvas::new(SelectionPainter {
        selection,
        accent,
        canonical,
        origin,
        drawn,
    })
    .width(Length::Fixed(drawn.0))
    .height(Length::Fixed(drawn.1))
    .into()
}

/// Draw the marquee the reader is dragging, or the one they are being asked
/// about, over one sheet (§8.1).
///
/// Chrome like the selection and not a mark: it is a rectangle describing a
/// *view*, nothing about it is ever written to the document, and it is drawn
/// dashed so it cannot be mistaken for a box somebody drew on the page.
pub fn marquee_layer<'a, Message: 'a>(
    rect: PageRect,
    canonical: (f32, f32),
    origin: (f32, f32),
    drawn: (f32, f32),
) -> Element<'a, Message> {
    canvas_widget::Canvas::new(MarqueePainter {
        rect,
        canonical,
        origin,
        drawn,
    })
    .width(Length::Fixed(drawn.0))
    .height(Length::Fixed(drawn.1))
    .into()
}

/// Mark the widgets on one sheet that are drawn but can never be filled
/// (§6.4).
///
/// Chrome, like the selection outline and for the same reason it is allowed:
/// nothing about it is ever written to the document, and it says something
/// about the file that the file itself never says. A signature field and a
/// file-selection field both look exactly like a box to click at, and the only
/// way to find out that pulpit refuses them is to click.
///
/// Deliberately quiet: the muted role and the smallest type, so a form with
/// twenty signature lines still reads as a form and not as an error report.
pub fn dead_field_layer<'a, Message: 'a>(
    fields: Vec<crate::widgets::context::DeadField>,
    muted: Color,
    canonical: (f32, f32),
    origin: (f32, f32),
    drawn: (f32, f32),
    // `None` in every mode but Live (`Mode::interactive`): a badge is still
    // chrome worth drawing in a preview, but a click that started a modal
    // Sign flow over inert editor controls would be a click the rest of the
    // surface cannot act on.
    on_event: Option<fn(crate::widgets::WidgetEvent) -> Message>,
) -> Element<'a, Message> {
    canvas_widget::Canvas::new(DeadFieldPainter {
        fields,
        muted,
        canonical,
        origin,
        drawn,
        on_event,
    })
    .width(Length::Fixed(drawn.0))
    .height(Length::Fixed(drawn.1))
    .into()
}

/// How big a badge's words are drawn, on screen. Smaller than any type in the
/// chrome: this is a footnote on the page, not a label in the interface.
const BADGE_TEXT: f32 = 9.0;

struct DeadFieldPainter<Message> {
    fields: Vec<crate::widgets::context::DeadField>,
    muted: Color,
    canonical: (f32, f32),
    /// As [`SelectionPainter::origin`].
    origin: (f32, f32),
    drawn: (f32, f32),
    on_event: Option<fn(crate::widgets::WidgetEvent) -> Message>,
}

impl<Message> DeadFieldPainter<Message> {
    /// The screen-space rect one field's badge and hit area occupy — the
    /// same conversion `draw` and `update` must agree on, or a click would
    /// land somewhere other than where the badge is drawn.
    fn scale(&self) -> (f32, f32) {
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
        (scale_x, scale_y)
    }

    fn field_rect(&self, field: &crate::widgets::context::DeadField) -> (Point, Size) {
        let (scale_x, scale_y) = self.scale();
        let rect = field.bounds;
        let top_left = Point::new(
            (rect.left - self.origin.0) * scale_x,
            (rect.top - self.origin.1) * scale_y,
        );
        let size = Size::new(
            (rect.width() * scale_x).max(1.0),
            (rect.height() * scale_y).max(1.0),
        );
        (top_left, size)
    }
}

impl<Message> canvas::Program<Message> for DeadFieldPainter<Message> {
    // Whether this layer captured the press it is currently waiting on a
    // matching release for. Without this, capturing a press with no
    // corresponding release capture leaves `mouse_area` seeing a
    // `PageReleased` with no `PagePressed` before it — an orphan release
    // the page surface was never designed to receive on its own.
    type State = bool;

    fn update(
        &self,
        pressed: &mut bool,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<iced::widget::Action<Message>> {
        use iced::widget::Action;

        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let on_event = self.on_event?;
                let position = cursor.position_in(bounds)?;
                // Only a signature field answers a click (§31.1) — a
                // file-select dead field has nothing pulpit can do with
                // one, so the press falls through to the page surface
                // underneath, exactly as if there were no badge here at
                // all.
                let name = self.fields.iter().find_map(|field| {
                    let (top_left, size) = self.field_rect(field);
                    let hit = Rectangle::new(top_left, size).contains(position);
                    hit.then(|| field.signature_field.clone()).flatten()
                })?;
                *pressed = true;
                Some(
                    Action::publish(on_event(crate::widgets::WidgetEvent::Read(
                        crate::widgets::event::ReadCommand::SignField(name),
                    )))
                    .and_capture(),
                )
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if *pressed => {
                // Pair with the press this layer captured above, rather
                // than let it reach `mouse_area` as an orphan.
                *pressed = false;
                Some(Action::capture())
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let Some(position) = cursor.position_in(bounds) else {
            return mouse::Interaction::None;
        };
        let clickable = self.on_event.is_some()
            && self.fields.iter().any(|field| {
                if field.signature_field.is_none() {
                    return false;
                }
                let (top_left, size) = self.field_rect(field);
                Rectangle::new(top_left, size).contains(position)
            });
        if clickable {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        for field in &self.fields {
            let (top_left, size) = self.field_rect(field);
            // A hairline round the widget and a light wash inside: enough to
            // say "this box is not the same as the ones you can type in",
            // without covering whatever the producer drew in it.
            frame.fill_rectangle(
                top_left,
                size,
                Color {
                    a: 0.06,
                    ..self.muted
                },
            );
            frame.stroke(
                &Path::rectangle(top_left, size),
                Stroke::default()
                    .with_color(Color {
                        a: 0.55,
                        ..self.muted
                    })
                    .with_width(1.0),
            );
            // The words sit inside the widget's own top-left corner, so a
            // badge never reaches over a neighbouring field to be read as
            // that one's.
            frame.fill_text(canvas::Text {
                content: field.label.to_string(),
                position: Point::new(top_left.x + 2.0, top_left.y + 1.0),
                color: self.muted,
                size: BADGE_TEXT.into(),
                ..canvas::Text::default()
            });
        }
        vec![frame.into_geometry()]
    }
}

struct MarqueePainter {
    rect: PageRect,
    canonical: (f32, f32),
    origin: (f32, f32),
    drawn: (f32, f32),
}

impl<Message> canvas::Program<Message> for MarqueePainter {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
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
        let top_left = Point::new(
            (self.rect.left - self.origin.0) * scale_x,
            (self.rect.top - self.origin.1) * scale_y,
        );
        let size = Size::new(
            (self.rect.width() * scale_x).max(1.0),
            (self.rect.height() * scale_y).max(1.0),
        );
        let accent = theme.extended_palette().primary.base.color;
        // A light wash inside and a firm edge: the wash says which side of the
        // line is being kept, which is the one thing a bare outline leaves the
        // reader to guess.
        frame.fill_rectangle(top_left, size, Color { a: 0.12, ..accent });
        frame.stroke(
            &Path::rectangle(top_left, size),
            Stroke::default().with_color(accent).with_width(1.5),
        );
        vec![frame.into_geometry()]
    }
}

/// How big a corner grip is drawn, on screen.
const HANDLE_SIZE: f32 = 7.0;

struct SelectionPainter {
    selection: crate::widgets::context::SelectedMark,
    accent: Color,
    canonical: (f32, f32),
    /// The page point the sheet's top-left corner stands for: the crop
    /// window's own corner, or the page's origin when nothing is cropped.
    origin: (f32, f32),
    drawn: (f32, f32),
}

impl<Message> canvas::Program<Message> for SelectionPainter {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
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
        let place = |point: PagePoint| {
            Point::new(
                (point.x - self.origin.0) * scale_x,
                (point.y - self.origin.1) * scale_y,
            )
        };

        let rect = self.selection.bounds;
        let top_left = place(PagePoint::new(rect.left, rect.top));
        let size = Size::new(
            (rect.width() * scale_x).max(1.0),
            (rect.height() * scale_y).max(1.0),
        );

        // A held mark is drawn lighter than a resting one: the outline under
        // the pointer is a proposal the release has not committed yet, and it
        // should not read as firmly as the selection it started from.
        let (outline, fill) = if self.selection.dragging {
            (0.75, 0.10)
        } else {
            (1.0, 0.06)
        };
        frame.fill_rectangle(
            top_left,
            size,
            Color {
                a: fill,
                ..self.accent
            },
        );
        frame.stroke(
            &Path::rectangle(top_left, size),
            Stroke::default()
                .with_color(Color {
                    a: outline,
                    ..self.accent
                })
                .with_width(1.5),
        );

        // The grips, on the corners that can actually be dragged. A mark with
        // none — a highlight, a note — gets the outline and no grips, which is
        // how it says "selected, but not reshapable" without a word of text.
        for corner in &self.selection.handles {
            let at = place(corner.of(rect));
            let grip = Size::new(HANDLE_SIZE, HANDLE_SIZE);
            let top_left = Point::new(at.x - HANDLE_SIZE / 2.0, at.y - HANDLE_SIZE / 2.0);
            frame.fill_rectangle(top_left, grip, Color::WHITE);
            frame.stroke(
                &Path::rectangle(top_left, grip),
                Stroke::default().with_color(self.accent).with_width(1.5),
            );
        }

        vec![frame.into_geometry()]
    }
}

struct Painter {
    preview: GesturePreview,
    canonical: (f32, f32),
    /// As [`SelectionPainter::origin`].
    origin: (f32, f32),
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
        let place = |point: &PagePoint| {
            Point::new(
                (point.x - self.origin.0) * scale_x,
                (point.y - self.origin.1) * scale_y,
            )
        };

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
