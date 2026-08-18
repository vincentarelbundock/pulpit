//! The layout library and the layout editor.
//!
//! Two regions in the editor: the widget library on the left and the canvas
//! filling the rest. There is no properties panel and no checks panel — the
//! canvas draws the real widgets, so the arrangement is judged by looking at
//! it, and validation has its say when the layout is saved.

use iced::widget::{
    button, canvas, checkbox, column, container, mouse_area, row, scrollable, space, stack, text,
    text_input, tooltip, Column, Row,
};
use iced::{Alignment, Element, Length, Padding, Point, Rectangle};

use crate::layout::fit::FittedSlide;
use crate::layout::thumbnail::{Content, Thumbnail, ThumbnailCell};
use crate::layout::{
    AspectRatio, Direction, Layout, LayoutId, LayoutStore, Node, NodeId, Severity,
};
use crate::theme::Palette;
use crate::widgets::{Family, WidgetGroup, WidgetKind};

use crate::app::Message;
use crate::designer::{Designer, Dialog, DragSource, DropChoice::*, Edge, Msg};
use crate::theme;
use crate::theme::space as gap;
use crate::theme::target;

fn msg(message: Msg) -> Message {
    Message::Designer(message)
}

// ---------------------------------------------------------------- library

/// The library of layout cards, shown before the editor is entered.
pub fn library<'a>(store: &'a LayoutStore, active: Option<&'a LayoutId>) -> Element<'a, Message> {
    let header = row![
        button(
            row![
                theme::icon::icon(theme::Icon::ArrowLeft, 14.0),
                text("Presenter").size(14)
            ]
            .spacing(6)
            .align_y(Alignment::Center)
        )
        .style(theme::ambient::tool_button)
        .padding(8)
        .on_press(Message::ShowPresenter),
        space::horizontal(),
        button(text("New layout").size(14))
            .style(theme::ambient::selected_button)
            .padding(8)
            .on_press(Message::NewLayout),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let mut body = column![
        header,
        text("Layouts").size(26),
        text("A presenter screen is a layout. Choose one, or build your own.")
            .size(13)
            .color(theme::ambient::muted()),
        section_heading("Built-in Layouts"),
    ]
    .spacing(14)
    .padding(20);

    let mut built_in = Row::new().spacing(14);
    for layout in store.built_in() {
        built_in = built_in.push(layout_card(layout, active, true));
    }
    body = body.push(built_in.wrap());

    body = body.push(section_heading("My Layouts"));
    if store.custom().is_empty() {
        body = body.push(
            container(
                text("No custom layouts yet. Duplicate a built-in layout to start from a design that already works.")
                    .size(13)
                    .color(theme::ambient::muted()),
            )
            .padding(16)
            .style(theme::ambient::empty_cell)
            .width(Length::Fill),
        );
    } else {
        let mut custom = Row::new().spacing(14);
        for layout in store.custom() {
            custom = custom.push(layout_card(layout, active, false));
        }
        body = body.push(custom.wrap());
    }

    scrollable(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn section_heading<'a>(label: &'a str) -> Element<'a, Message> {
    row![
        text(label).size(16).color(theme::ambient::text()),
        container(space::horizontal().height(Length::Fixed(1.0)))
            .width(Length::Fill)
            .style(theme::ambient::separator),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .into()
}

fn layout_card<'a>(
    layout: &'a Layout,
    active: Option<&'a LayoutId>,
    built_in: bool,
) -> Element<'a, Message> {
    let is_active = active == Some(&layout.id);

    // Name, what it is, and three actions. The thumbnail already says which
    // widgets are in it; listing them again made every card a different
    // height and told the reader nothing they could not see.
    let mut actions = Row::new().spacing(6);
    actions = actions.push(
        button(text(if is_active { "In use" } else { "Use" }).size(12))
            .padding(6)
            .style(if is_active {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            })
            .on_press(Message::UseLayout(layout.id.clone())),
    );
    actions = actions.push(
        button(text("Preview").size(12))
            .padding(6)
            .style(theme::ambient::tool_button)
            .on_press(Message::PreviewLayout(layout.id.clone())),
    );
    actions = actions.push(
        button(text("Copy").size(12))
            .padding(6)
            .style(theme::ambient::tool_button)
            .on_press(Message::DuplicateLayout(layout.id.clone())),
    );
    if !built_in {
        actions = actions.push(
            button(text("Edit").size(12))
                .padding(6)
                .style(theme::ambient::tool_button)
                .on_press(Message::EditLayout(layout.id.clone())),
        );
        // Delete looks like its neighbours: it asks before it acts, and a red
        // button on a shelf of layouts reads as a warning about the layout
        // rather than about the action.
        actions = actions.push(
            button(text("Delete").size(12))
                .padding(6)
                .style(theme::ambient::tool_button)
                .on_press(Message::DeleteLayout(layout.id.clone())),
        );
    }

    container(
        column![
            thumbnail(layout),
            text(layout.name.clone())
                .size(16)
                .color(theme::ambient::text()),
            text(if built_in { "Read-only" } else { "Custom" })
                .size(11)
                .color(theme::ambient::muted()),
            space::vertical(),
            actions,
        ]
        .spacing(8),
    )
    .padding(12)
    .width(Length::Fixed(310.0))
    // Every card the same size, so a shelf of them reads as a shelf.
    .height(Length::Fixed(260.0))
    .style(move |theme: &iced::Theme| {
        let mut style = crate::theme::ambient::surface(theme);
        if is_active {
            style.border.color = theme::ambient::accent();
        }
        style
    })
    .into()
}

/// A miniature of the layout, drawn from the tree itself.
///
/// A library card is where a layout is chosen, so the thumbnail is drawn for
/// the deck shape the layout was designed at: the bars a card shows are the
/// bars that layout will produce.
fn thumbnail(layout: &Layout) -> Element<'_, Message> {
    layout_thumbnail(&layout.root, layout.design_ratio, 120.0)
}

/// A layout drawn to scale at `height`, slide bounds and letterboxing
/// included.
fn layout_thumbnail<'a>(
    root: &Node,
    slide_ratio: AspectRatio,
    height: f32,
) -> Element<'a, Message> {
    container(
        canvas(Sketch {
            root: root.clone(),
            slide_ratio,
            palette: theme::ambient::palette(),
        })
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(height))
    .padding(6)
    .style(theme::ambient::canvas)
    .into()
}

// ------------------------------------------------------------- thumbnails

/// The drawing half of [`crate::layout::thumbnail`].
///
/// Every decision — which rectangle, which widget, where the slide lands —
/// is made by the model at the size the canvas reports; this only paints.
/// The tree is cloned because a thumbnail may be drawn for a layout that is
/// not being held anywhere, such as a recommended built-in.
struct Sketch {
    root: Node,
    slide_ratio: AspectRatio,
    palette: Palette,
}

impl<Message> canvas::Program<Message> for Sketch {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let model = Thumbnail::of(&self.root, bounds.width, bounds.height, self.slide_ratio);
        for cell in &model.cells {
            paint_cell(&mut frame, self.palette, cell);
        }
        for divider in &model.dividers {
            paint_divider(&mut frame, self.palette, divider);
        }
        vec![frame.into_geometry()]
    }
}

fn rectangle(frame: &crate::layout::Frame) -> (iced::Point, iced::Size) {
    (
        iced::Point::new(frame.x, frame.y),
        iced::Size::new(frame.width.max(0.0), frame.height.max(0.0)),
    )
}

fn paint_cell(frame: &mut canvas::Frame, palette: Palette, cell: &ThumbnailCell) {
    let (origin, size) = rectangle(&cell.frame);
    let pane = canvas::Path::rectangle(origin, size);

    match cell.slide {
        // A slide pane is drawn as the presenter will see it: the pane is
        // the space, the bright rectangle inside it is the slide, and the
        // difference between them is the letterbox nobody can use.
        Some(slide) => {
            frame.fill(&pane, Palette::tinted(palette.muted, 0.16));
            let (origin, size) = rectangle(&slide.drawn);
            frame.fill(&canvas::Path::rectangle(origin, size), palette.slide_canvas);
        }
        None => frame.fill(&pane, palette.surface),
    }
    paint_content(frame, palette, cell);
}

fn paint_divider(frame: &mut canvas::Frame, palette: Palette, divider: &crate::layout::Divider) {
    let gutter = &divider.frame;
    let path = match divider.direction {
        Direction::Horizontal => {
            let x = gutter.x + gutter.width / 2.0;
            canvas::Path::line(
                iced::Point::new(x, gutter.y),
                iced::Point::new(x, gutter.y + gutter.height),
            )
        }
        Direction::Vertical => {
            let y = gutter.y + gutter.height / 2.0;
            canvas::Path::line(
                iced::Point::new(gutter.x, y),
                iced::Point::new(gutter.x + gutter.width, y),
            )
        }
    };
    frame.stroke(
        &path,
        canvas::Stroke::default()
            .with_color(palette.border())
            .with_width(crate::widgets::tokens::CELL_SEPARATOR_WIDTH),
    );
}

/// The representative marks that tell a notes pane from a clock at a glance.
fn paint_content(frame: &mut canvas::Frame, palette: Palette, cell: &ThumbnailCell) {
    // A margin proportional to the pane, so the sketch stays inside panes of
    // very different sizes.
    let inset = (cell.frame.width.min(cell.frame.height) * 0.12).clamp(1.0, 6.0);
    let x = cell.frame.x + inset;
    let y = cell.frame.y + inset;
    let width = cell.frame.width - inset * 2.0;
    let height = cell.frame.height - inset * 2.0;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let ink = Palette::tinted(palette.muted, 0.75);
    let bar = |frame: &mut canvas::Frame, x: f32, y: f32, w: f32, h: f32, colour| {
        if w > 0.0 && h > 0.0 {
            frame.fill(
                &canvas::Path::rectangle(iced::Point::new(x, y), iced::Size::new(w, h)),
                colour,
            );
        }
    };

    match cell.content {
        Content::Empty | Content::Slide => {}
        Content::Lines(count) => {
            let count = count.max(1) as f32;
            let step = height / count;
            let thickness = (step * 0.4).clamp(1.0, 3.0);
            for line in 0..count as usize {
                // Ragged right edges: prose, not a table.
                let share = [1.0, 0.86, 0.94, 0.62][line % 4];
                bar(
                    frame,
                    x,
                    y + step * line as f32,
                    width * share,
                    thickness,
                    ink,
                );
            }
        }
        Content::Readout => {
            let h = (height * 0.55).min(height);
            bar(
                frame,
                x + width * 0.1,
                y + (height - h) / 2.0,
                width * 0.8,
                h,
                Palette::tinted(palette.text, 0.7),
            );
        }
        Content::Buttons(count) => {
            let count = count.max(1) as f32;
            let gap = width * 0.08;
            let each = (width - gap * (count - 1.0)) / count;
            let h = (height * 0.7).min(height);
            for index in 0..count as usize {
                bar(
                    frame,
                    x + (each + gap) * index as f32,
                    y + (height - h) / 2.0,
                    each,
                    h,
                    Palette::tinted(palette.accent, 0.55),
                );
            }
        }
        Content::Track => {
            let thickness = (height * 0.18).clamp(1.0, 3.0);
            bar(
                frame,
                x,
                y + (height - thickness) / 2.0,
                width,
                thickness,
                ink,
            );
            frame.fill(
                &canvas::Path::circle(
                    iced::Point::new(x + width * 0.4, y + height / 2.0),
                    (height * 0.3).clamp(1.5, 4.0),
                ),
                palette.accent,
            );
        }
        Content::Caption => {
            let thickness = (height * 0.3).clamp(1.0, 3.0);
            bar(
                frame,
                x,
                y + (height - thickness) / 2.0,
                width * 0.7,
                thickness,
                ink,
            );
        }
        Content::Marks => {
            let path = canvas::Path::new(|builder| {
                builder.move_to(iced::Point::new(x, y + height));
                builder.quadratic_curve_to(
                    iced::Point::new(x + width * 0.5, y - height * 0.4),
                    iced::Point::new(x + width, y + height * 0.4),
                );
            });
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_color(palette.accent)
                    .with_width(1.5),
            );
        }
    }
}

/// The slide's real bounds inside one editor pane, and the bars around them.
///
/// Drawn over the pane rather than computed into it: the editor lays panes
/// out with fill portions, so the only place the pane's true size is known is
/// at draw time, which is exactly where this asks for the fit.
struct SlideBounds {
    slide_ratio: AspectRatio,
    palette: Palette,
}

impl<Message> canvas::Program<Message> for SlideBounds {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let pane = crate::layout::Frame::new(0.0, 0.0, bounds.width, bounds.height);
        let Some(fitted) = FittedSlide::fit(pane, self.slide_ratio.ratio()) else {
            return Vec::new();
        };

        // The letterbox is hatched with a wash rather than a solid fill: the
        // widget underneath must stay legible, since judging the arrangement
        // is what the editor is for.
        if fitted.bars().is_some() {
            let shade = canvas::Path::new(|builder| {
                builder.rectangle(iced::Point::ORIGIN, bounds.size());
                let (origin, size) = rectangle(&fitted.drawn);
                builder.rectangle(origin, size);
            });
            frame.fill(
                &shade,
                canvas::Fill {
                    style: canvas::Style::Solid(Palette::tinted(self.palette.muted, 0.35)),
                    rule: canvas::fill::Rule::EvenOdd,
                },
            );
        }

        let (origin, size) = rectangle(&fitted.drawn);
        frame.stroke(
            &canvas::Path::rectangle(origin, size),
            canvas::Stroke::default()
                .with_color(self.palette.accent)
                .with_width(1.5),
        );
        vec![frame.into_geometry()]
    }
}

// ----------------------------------------------------------------- editor

/// The editor page.
pub fn editor<'a>(
    designer: &'a Designer,
    context: &crate::widgets::Context<'_>,
) -> Element<'a, Message> {
    let body = row![
        container(widget_library(designer))
            .width(Length::Fixed(230.0))
            .height(Length::Fill),
        container(canvas_region(designer, context))
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .spacing(12);

    let mut page = column![header(designer)].spacing(12).padding(12);
    if designer.is_editable() {
        page = page.push(
            text(
                "Select a widget in the left pane and left click on the region where you want \
                 to insert it. Drag the separators to resize region splits. Right click on a \
                 widget, on an empty cell, or on a split to remove it.",
            )
            .size(12)
            .color(theme::ambient::muted()),
        );
    }
    let page = page.push(body).width(Length::Fill).height(Length::Fill);

    match dialog(designer) {
        Some(modal) => stack![page, modal].into(),
        None => page.into(),
    }
}

fn header<'a>(designer: &'a Designer) -> Element<'a, Message> {
    let layout = designer.layout();
    let editable = designer.is_editable();

    let mut actions = Row::new().spacing(6).align_y(Alignment::Center);
    let enabled = |condition: bool, message: Msg| condition.then_some(msg(message));

    // The two rotation arrows rather than the words: they are the same pair
    // everywhere else, they take a quarter of the room, and the tooltip
    // still says which is which.
    actions = actions.push(tooltip(
        button(theme::icon::icon(theme::Icon::Undo, 16.0))
            .padding(7)
            .style(theme::ambient::tool_button)
            .on_press_maybe(enabled(editable && designer.history.can_undo(), Msg::Undo)),
        container(text("Undo").size(11))
            .padding(6)
            .style(theme::ambient::dialog),
        tooltip::Position::Bottom,
    ));
    actions = actions.push(tooltip(
        button(theme::icon::icon(theme::Icon::Redo, 16.0))
            .padding(7)
            .style(theme::ambient::tool_button)
            .on_press_maybe(enabled(editable && designer.history.can_redo(), Msg::Redo)),
        container(text("Redo").size(11))
            .padding(6)
            .style(theme::ambient::dialog),
        tooltip::Position::Bottom,
    ));

    // Adding a pane is the commonest thing anyone does here, so the four
    // directions are in the toolbar rather than a panel. Removal sits beside
    // undo, with the other single-glyph history actions, since it is what
    // undo takes back — a wastebasket rather than a cross, because a cross
    // beside two arrows reads as "close the editor".
    if editable {
        let target = designer
            .selected
            .unwrap_or_else(|| designer.layout().root.id());
        actions = actions.push(tooltip(
            button(theme::icon::icon(theme::Icon::Trash, 16.0))
                .padding(7)
                .style(theme::ambient::tool_button)
                .on_press(msg(Msg::RequestDelete(target))),
            container(text("Remove the widget, then the pane").size(11))
                .padding(6)
                .style(theme::ambient::dialog),
            tooltip::Position::Bottom,
        ));
        actions = actions.push(add_pane_row(target));
    }

    if !editable {
        actions = actions.push(
            button(text("Duplicate to Customize").size(13))
                .padding(7)
                .style(theme::ambient::selected_button)
                .on_press(msg(Msg::DuplicateToCustomize)),
        );
    }
    // No save button and no discard button. Leaving asks, and that is the
    // moment the question means something: a toolbar that can throw work
    // away, or scatter half-finished versions on disk, is two mis-clicks
    // looking for somewhere to happen. `Ctrl/Cmd+S` still saves outright for
    // anyone who wants it.

    // The title is the rename control. A pencil beside it, in the muted
    // colour, is the cue — enough to say "this is a thing you can press"
    // without turning the heading into a button that shouts. A built-in
    // layout's name cannot be changed, so it stays plain text.
    let title: Element<'a, Message> = if editable {
        tooltip(
            button(
                row![
                    text(layout.name.clone()).size(18),
                    theme::icon::muted(theme::Icon::Pencil, 12.0),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([2.0, 6.0]))
            .style(theme::ambient::tool_button)
            .on_press(msg(Msg::StartRenameLayout)),
            container(text("Rename this layout").size(11))
                .padding(6)
                .style(theme::ambient::dialog),
            tooltip::Position::Bottom,
        )
        .into()
    } else {
        text(layout.name.clone()).size(18).into()
    };

    let mut bar = column![row![
        button(
            row![
                theme::icon::icon(theme::Icon::ArrowLeft, 13.0),
                text("Back").size(13)
            ]
            .spacing(6)
            .align_y(Alignment::Center)
        )
        .padding(7)
        .style(theme::ambient::tool_button)
        .on_press(msg(Msg::Back)),
        title,
        space::horizontal(),
        actions,
    ]
    .spacing(12)
    .align_y(Alignment::Center)]
    .spacing(8);

    if let Some(status) = &designer.status {
        bar = bar.push(
            container(
                row![
                    text(status).size(12),
                    space::horizontal(),
                    button(text("dismiss").size(11))
                        .style(theme::ambient::tool_button)
                        .padding(4)
                        .on_press(msg(Msg::DismissStatus)),
                ]
                .align_y(Alignment::Center),
            )
            .padding(8)
            .width(Length::Fill)
            .style(theme::ambient::notice),
        );
    }
    bar.into()
}

// --------------------------------------------------------- widget library

fn widget_library<'a>(designer: &'a Designer) -> Element<'a, Message> {
    let mut content = column![text("Widgets").size(15)].spacing(10).padding(4);

    // One walk of the layout for the whole library: asking `already_placed`
    // per card re-walked the tree — and allocated the occupies-vectors of
    // every placed widget — once per kind, per view pass.
    let placed: std::collections::HashSet<WidgetKind> = designer
        .layout()
        .cells()
        .into_iter()
        .filter_map(|cell| cell.widget.as_ref())
        .flat_map(|widget| widget.kind().occupies())
        .collect();
    let is_placed = |kind: WidgetKind| {
        kind.occupies()
            .into_iter()
            .any(|occupied| !occupied.multi_instance() && placed.contains(&occupied))
    };

    for group in WidgetGroup::ALL {
        content = content.push(text(group.label()).size(12).color(theme::ambient::muted()));
        for kind in WidgetKind::ALL
            .into_iter()
            // Search is application chrome now. Keep the legacy kind readable
            // in stored layouts, but do not offer new permanent search cells.
            .filter(|kind| *kind != WidgetKind::Search)
            .filter(|kind| kind.group() == group)
        {
            content = content.push(widget_card(designer, kind, is_placed(kind)));
        }
    }

    // Room on the right for the scroll bar, so a name is never under it.
    scrollable(container(content).padding(iced::Padding {
        right: gap::L,
        ..iced::Padding::from(0.0)
    }))
    .height(Length::Fill)
    .into()
}

fn widget_card<'a>(designer: &'a Designer, kind: WidgetKind, placed: bool) -> Element<'a, Message> {
    let dragging = designer.dragging == Some(DragSource::Library(kind));

    // A list of names, not a stack of cards: the cards were wide enough to
    // slide under the scroll bar, and a boxed name is no more draggable than
    // a plain one.
    let card = button(
        row![
            text(kind.label()).size(13),
            space::horizontal(),
            text(if placed { "in layout" } else { "" })
                .size(10)
                .color(theme::ambient::alert()),
        ]
        .spacing(gap::S)
        .align_y(Alignment::Center),
    )
    .padding(iced::Padding::from([6.0, 8.0]))
    .width(Length::Fill)
    .style(if dragging {
        theme::ambient::selected_button
    } else {
        theme::ambient::tool_button
    });

    // A card that is already placed cannot be dragged.
    let card: Element<'a, Message> = if placed || !designer.is_editable() {
        card.into()
    } else {
        mouse_area(card.on_press(msg(Msg::StartDrag(DragSource::Library(kind)))))
            .on_release(msg(Msg::CancelDrag))
            .into()
    };

    tooltip(
        card,
        container(text(kind.tooltip()).size(11))
            .padding(6)
            .style(theme::ambient::dialog),
        tooltip::Position::Right,
    )
    .into()
}

// ------------------------------------------------------------------ canvas

fn canvas_region<'a>(
    designer: &'a Designer,
    context: &crate::widgets::Context<'_>,
) -> Element<'a, Message> {
    let ratio = designer.canvas_ratio.ratio();
    let inner = canvas_node(designer, context, &designer.layout().root);

    let framed = container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(6)
        .style(theme::ambient::canvas);

    // Pointer tracking for divider drags lives on the whole canvas so a fast
    // drag that leaves the gutter still resizes.
    let tracked = mouse_area(framed)
        .on_move(|point: Point| msg(Msg::PointerMoved(point)))
        // A release the panes did not take is the end of whatever was going
        // on: a divider drag, or a widget drag that landed nowhere.
        .on_release(msg(Msg::PointerReleased));

    let aspect = container(tracked)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);

    let hint = row![
        text(format!(
            "Designed at {} · previewing {}",
            designer.layout().design_ratio.label(),
            designer.canvas_ratio.label()
        ))
        .size(11)
        .color(theme::ambient::muted()),
        space::horizontal(),
        checkbox(designer.snap)
            .label("Snap dividers")
            .size(14)
            .text_size(11)
            .on_toggle(|value| msg(Msg::ToggleSnap(value))),
        text(format!("{ratio:.2}:1"))
            .size(11)
            .color(theme::ambient::muted()),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    column![aspect, hint, space_report(designer)]
        .spacing(6)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// What this layout does with its space for the deck being previewed: the
/// deck's shape, what that costs, and which built-ins cost less.
///
/// Nothing here changes the layout. The deck shape changes what is drawn; the
/// recommendations are a list to read, and adopting one is a press of its own.
fn space_report<'a>(designer: &'a Designer) -> Element<'a, Message> {
    let mut ratios = Row::new().spacing(gap::S).align_y(Alignment::Center);
    ratios = ratios.push(text("Deck shape").size(11).color(theme::ambient::muted()));
    for ratio in AspectRatio::PRESETS {
        let chosen = designer.slide_ratio == ratio;
        ratios = ratios.push(
            button(text(ratio.label()).size(11))
                .padding(5)
                .style(if chosen {
                    theme::ambient::selected_button
                } else {
                    theme::ambient::tool_button
                })
                .on_press(msg(Msg::SetSlideRatio(ratio))),
        );
    }

    let mut panel = column![ratios].spacing(gap::S);

    if let Some(warning) = designer.space_warning() {
        panel = panel.push(
            container(text(warning).size(11).color(theme::ambient::alert()))
                .padding(gap::S)
                .width(Length::Fill)
                .style(theme::ambient::notice),
        );
    }

    // Ranked, labelled, and inert until pressed: the arithmetic can say which
    // built-in fits a 4:3 deck best, but it cannot know that the rail on the
    // right is where the presenter keeps their notes.
    let built_ins = crate::layout::builtin::built_in_layouts();
    let mut suggestions = Row::new().spacing(gap::S).align_y(Alignment::Center);
    suggestions = suggestions.push(
        text("Built-ins for this deck")
            .size(11)
            .color(theme::ambient::muted()),
    );
    for recommendation in designer.recommendations().into_iter().take(3) {
        let Some(layout) = built_ins
            .iter()
            .find(|layout| layout.name == recommendation.layout)
        else {
            continue;
        };
        suggestions = suggestions.push(tooltip(
            button(
                text(format!(
                    "{} — {}",
                    recommendation.layout, recommendation.reason
                ))
                .size(11),
            )
            .padding(5)
            .style(
                if designer.previewed_recommendation.as_ref() == Some(&layout.id) {
                    theme::ambient::selected_button
                } else {
                    theme::ambient::tool_button
                },
            )
            .on_press(msg(Msg::PreviewRecommendation(layout.id.clone()))),
            container(text("Show this layout. Nothing changes until you use it.").size(11))
                .padding(6)
                .style(theme::ambient::dialog),
            tooltip::Position::Top,
        ));
    }
    panel = panel.push(suggestions.wrap());

    if let Some(previewed) = designer
        .previewed_recommendation
        .as_ref()
        .and_then(|id| built_ins.iter().find(|layout| &layout.id == id))
    {
        panel = panel.push(recommendation_preview(designer, previewed));
    }

    container(panel).width(Length::Fill).into()
}

/// A recommended built-in, shown beside the layout being edited so the two
/// can be compared before anything is decided.
fn recommendation_preview<'a>(designer: &Designer, layout: &Layout) -> Element<'a, Message> {
    let report = crate::layout::fit::SpaceReport::measure(
        designer.design_area(),
        designer.slide_ratio.ratio(),
        &crate::layout::thumbnail::slide_cells(&layout.root, designer.design_area()),
    );
    let empty = (report.wasted_fraction() * 100.0).round() as u32;

    container(
        row![
            container(layout_thumbnail(&layout.root, designer.slide_ratio, 90.0))
                .width(Length::Fixed(170.0)),
            column![
                text(layout.name.clone()).size(13),
                text(format!(
                    "{empty}% of its slide panes would be empty for a {} deck.",
                    designer.slide_ratio.label()
                ))
                .size(11)
                .color(theme::ambient::muted()),
                row![
                    button(text("Use this layout").size(11))
                        .padding(5)
                        .style(theme::ambient::selected_button)
                        .on_press(Message::UseLayout(layout.id.clone())),
                    button(text("Dismiss").size(11))
                        .padding(5)
                        .style(theme::ambient::tool_button)
                        .on_press(msg(Msg::DismissRecommendation)),
                ]
                .spacing(gap::S),
            ]
            .spacing(gap::S),
        ]
        .spacing(gap::L)
        .align_y(Alignment::Center),
    )
    .padding(gap::S)
    .width(Length::Fill)
    .style(theme::ambient::surface)
    .into()
}

fn canvas_node<'a>(
    designer: &'a Designer,
    context: &crate::widgets::Context<'_>,
    node: &'a Node,
) -> Element<'a, Message> {
    match node {
        Node::Leaf(cell) => canvas_cell(designer, context, cell),
        Node::Split(split) => {
            let portion = |index: usize| (split.sizes[index] * 1000.0).max(1.0) as u16;
            let selected_divider = designer.selected_divider;
            let mut children: Vec<Element<'a, Message>> = Vec::new();

            for (index, child) in split.children.iter().enumerate() {
                children.push(
                    container(canvas_node(designer, context, child))
                        .width(match split.direction {
                            Direction::Horizontal => Length::FillPortion(portion(index)),
                            Direction::Vertical => Length::Fill,
                        })
                        .height(match split.direction {
                            Direction::Horizontal => Length::Fill,
                            Direction::Vertical => Length::FillPortion(portion(index)),
                        })
                        .into(),
                );
                if index + 1 < split.children.len() {
                    children.push(divider_handle(
                        split.id,
                        index,
                        split.direction,
                        selected_divider == Some((split.id, index)),
                        designer
                            .dragging_divider
                            .as_ref()
                            .is_some_and(|drag| drag.holds(split.id, index)),
                    ));
                }
            }

            match split.direction {
                Direction::Horizontal => Row::with_children(children)
                    .spacing(2)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
                Direction::Vertical => Column::with_children(children)
                    .spacing(2)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
            }
        }
    }
}

/// A draggable gutter. Hovering brightens it; dragging resizes the two
/// adjacent cells and shows their proportions.
fn divider_handle<'a>(
    split: NodeId,
    index: usize,
    direction: Direction,
    selected: bool,
    dragging: bool,
) -> Element<'a, Message> {
    let thickness = Length::Fixed(crate::widgets::tokens::CELL_SEPARATOR_WIDTH);
    // No numbers while dragging: the panes themselves show the proportion,
    // and a figure floating over the canvas is one more thing to read at the
    // moment you are looking at the shape.
    let bar = container(space::horizontal())
        .width(match direction {
            Direction::Horizontal => thickness,
            Direction::Vertical => Length::Fill,
        })
        .height(match direction {
            Direction::Horizontal => Length::Fill,
            Direction::Vertical => thickness,
        })
        .style(theme::ambient::divider(selected || dragging));

    // The visible rule stays thin; the region that accepts the press is the
    // whole minimum target. Hit area and visual weight are separate concerns.
    let grab = Length::Fixed(target::MINIMUM);
    let body = container(bar)
        .width(match direction {
            Direction::Horizontal => grab,
            Direction::Vertical => Length::Fill,
        })
        .height(match direction {
            Direction::Horizontal => Length::Fill,
            Direction::Vertical => grab,
        })
        .align_x(iced::Alignment::Center)
        .align_y(iced::Alignment::Center);

    // Press to take hold, drag to resize, release to let go. Reacting to the
    // pointer merely passing over would mean a divider moves when nobody
    // asked it to.
    mouse_area(body)
        .on_press(msg(Msg::DividerGrab(split, index)))
        // Right-click on the gutter removes the split it belongs to.
        .on_right_press(msg(Msg::RequestDelete(split)))
        .on_release(msg(Msg::DividerReleased))
        .on_double_click(msg(Msg::Equalize(split)))
        .into()
}

fn canvas_cell<'a>(
    designer: &'a Designer,
    context: &crate::widgets::Context<'_>,
    cell: &'a crate::layout::Cell,
) -> Element<'a, Message> {
    let selected = designer.selected == Some(cell.id);
    let highlighted = designer.hovered_cell == Some(cell.id) && designer.dragging.is_some();

    // The widget draws itself, exactly as it will on the night. A grey box
    // with its name in it tells you nothing about whether the arrangement
    // works, which is the only question the editor exists to answer.
    let inner: Element<'a, Message> = match &cell.widget {
        Some(widget) => crate::layout_renderer::widget(widget, context, None, |_| Message::Ignore),
        None => container(
            text("Drop a widget here")
                .size(12)
                .color(theme::ambient::muted()),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(theme::ambient::empty_cell)
        .into(),
    };

    // A pane holding a slide shows where the slide actually lands and the
    // space it cannot use. The bars are not a property of the pane but of the
    // deck being previewed, which is why they are drawn here rather than
    // being something the pane could be styled into.
    let holds_slide = cell
        .widget
        .as_ref()
        .is_some_and(|widget| widget.kind().family() == Family::Slides);
    let inner: Element<'a, Message> = if holds_slide {
        stack![
            inner,
            canvas(SlideBounds {
                slide_ratio: designer.slide_ratio,
                palette: theme::ambient::palette(),
            })
            .width(Length::Fill)
            .height(Length::Fill),
        ]
        .into()
    } else {
        inner
    };

    let content: Element<'a, Message> = if designer.dragging.is_some() {
        stack![inner, drop_zones(designer, cell.id)].into()
    } else {
        inner
    };

    let styled = container(content)
        .padding(Padding::from(cell.padding.min(12.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::ambient::cell_style(
            cell.background,
            selected,
            highlighted,
        ));

    // Right-click removes, in the same two steps as the toolbar cross: the
    // widget first, then the pane. Deleting is the action people reach for
    // most while laying a screen out, and walking to a panel for it every
    // time is a chore. One press, one message: attaching `on_press` twice
    // only kept the last one, which is why pressing a filled pane began a
    // drag and never selected it.
    let mut area = mouse_area(styled)
        .on_press(msg(Msg::PressCell(cell.id)))
        .on_right_press(msg(Msg::RequestDelete(cell.id)))
        .on_enter(msg(Msg::Hover(cell.id)));
    if designer.dragging.is_some() {
        area = area.on_release(msg(Msg::Drop(cell.id)));
    }
    area.into()
}

/// The four ways to add a pane beside the selected one.
///
/// This is the whole structural vocabulary: everything the ready-made shapes
/// used to offer is two presses of these, and where the new pane lands is
/// always obvious from which button you pressed.
fn add_pane_row<'a>(cell: NodeId) -> Element<'a, Message> {
    let mut row = Row::new().spacing(6);
    for edge in Edge::ALL {
        row = row.push(tooltip(
            button(
                column![
                    text(edge.glyph()).size(16),
                    text(edge.add_label())
                        .size(9)
                        .color(theme::ambient::muted()),
                ]
                .spacing(2)
                .align_x(Alignment::Center),
            )
            .padding(6)
            .style(theme::ambient::tool_button)
            .on_press(msg(Msg::AddPane(cell, edge))),
            container(text(edge.add_label()).size(11))
                .padding(6)
                .style(theme::ambient::dialog),
            tooltip::Position::Bottom,
        ));
    }
    row.wrap().into()
}

/// The five places a widget can land on a pane.
///
/// The middle fills the pane. Each edge splits it and puts the widget on that
/// side — one gesture instead of "split, find the new pane, aim again".
fn drop_zones(designer: &Designer, cell: NodeId) -> Element<'_, Message> {
    let over =
        |edge: Option<Edge>| designer.hovered_cell == Some(cell) && designer.hovered_edge == edge;
    let zone = |edge: Option<Edge>| {
        let target = match edge {
            Some(edge) => mouse_area(
                container(space::vertical())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::ambient::drop_zone(over(Some(edge)))),
            )
            .on_enter(msg(Msg::HoverEdge(cell, edge)))
            .on_release(msg(Msg::DropOnEdge(cell, edge))),
            None => mouse_area(
                container(space::vertical())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::ambient::drop_zone(over(None))),
            )
            .on_enter(msg(Msg::Hover(cell)))
            .on_release(msg(Msg::Drop(cell))),
        };
        Element::from(target)
    };

    // A quarter of each dimension is edge, the rest is centre: big enough to
    // hit without aiming, small enough that the middle is still the default.
    const EDGE: u16 = 24;
    const MIDDLE: u16 = 52;
    column![
        container(zone(Some(Edge::Top))).height(Length::FillPortion(EDGE)),
        container(row![
            container(zone(Some(Edge::Left))).width(Length::FillPortion(EDGE)),
            container(zone(None)).width(Length::FillPortion(MIDDLE)),
            container(zone(Some(Edge::Right))).width(Length::FillPortion(EDGE)),
        ])
        .height(Length::FillPortion(MIDDLE)),
        container(zone(Some(Edge::Bottom))).height(Length::FillPortion(EDGE)),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// --------------------------------------------------- structure + properties

// ----------------------------------------------------------------- dialogs

fn dialog<'a>(designer: &'a Designer) -> Option<Element<'a, Message>> {
    let dialog = designer.dialog.as_ref()?;
    let body: Element<'a, Message> = match dialog {
        Dialog::Drop {
            target, allow_swap, ..
        } => {
            let name = designer
                .layout()
                .cell(*target)
                .and_then(|cell| cell.widget.as_ref())
                .map(|widget| widget.label())
                .unwrap_or("this cell");
            let mut choices = Row::new().spacing(8);
            choices = choices.push(dialog_button(
                "Replace Existing Widget",
                Msg::ResolveDrop(Replace),
                true,
            ));
            if *allow_swap {
                choices =
                    choices.push(dialog_button("Swap Widgets", Msg::ResolveDrop(Swap), false));
            }
            choices = choices.push(dialog_button("Cancel", Msg::ResolveDrop(Cancel), false));
            column![
                text("That cell is occupied").size(16),
                text(format!("It currently holds {name}."))
                    .size(12)
                    .color(theme::ambient::muted()),
                choices,
            ]
            .spacing(12)
            .into()
        }
        Dialog::NameLayout { name, .. } => column![
            text("Name this layout").size(16),
            text_input("Conference Layout", name)
                .size(14)
                .padding(8)
                .on_input(|text| msg(Msg::NameInput(text)))
                .on_submit(msg(Msg::CommitName)),
            row![
                dialog_button("Save", Msg::CommitName, true),
                dialog_button("Cancel", Msg::CloseDialog, false),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .into(),
        Dialog::RenameLayout { name } => column![
            text("Rename this layout").size(16),
            text_input("Conference Layout", name)
                .size(14)
                .padding(8)
                .on_input(|text| msg(Msg::RenameLayoutInput(text)))
                .on_submit(msg(Msg::CommitRenameLayout)),
            row![
                dialog_button("Rename", Msg::CommitRenameLayout, true),
                dialog_button("Cancel", Msg::CloseDialog, false),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .into(),
        Dialog::UnsavedChanges => column![
            text("Save your changes?").size(16),
            text("This layout has unsaved changes.")
                .size(12)
                .color(theme::ambient::muted()),
            row![
                dialog_button("Save", Msg::SaveAndLeave, true),
                dialog_button("Discard", Msg::ForceBack, false),
                dialog_button("Cancel", Msg::CloseDialog, false),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .into(),
        Dialog::Validation { issues, name } => {
            let mut list = Column::new().spacing(6);
            for issue in issues {
                list = list.push(
                    column![
                        text(issue.message.clone())
                            .size(12)
                            .color(match issue.severity {
                                Severity::Blocking | Severity::Warning => theme::ambient::alert(),
                            }),
                        text(issue.consequence.clone())
                            .size(10)
                            .color(theme::ambient::muted()),
                    ]
                    .spacing(2),
                );
            }
            column![
                text(format!("“{name}” cannot be saved")).size(16),
                list,
                dialog_button("Close", Msg::CloseDialog, false),
            ]
            .spacing(12)
            .into()
        }
    };

    // The same panel the presenter window's dialogs are drawn in, so that the
    // ways out are the same in both halves of the application.
    //
    // Leaving with unsaved work is the exception: it is a question about a
    // layout someone has been editing, and a press that lands beside the panel
    // must not be read as an answer to it. Everything else here is a step that
    // can be abandoned — a drop resolved, a name asked for, a report read.
    let dismiss = match dialog {
        Dialog::UnsavedChanges => None,
        _ => Some(msg(Msg::CloseDialog)),
    };
    Some(crate::panel::panel(body, dismiss))
}

fn dialog_button<'a>(label: &'a str, message: Msg, primary: bool) -> Element<'a, Message> {
    button(text(label).size(13))
        .padding(9)
        .style(if primary {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        })
        .on_press(msg(message))
        .into()
}
