//! Drawing the annotation palette, and the marks themselves.
//!
//! Two things live here because they are two halves of one idea: the palette
//! says what the pointer is about to do, and [`marks`] draws what it did. The
//! marks are a `canvas` layer stacked over the slide by
//! [`crate::widgets::slides::view`] and over the audience picture by
//! [`crate::view`], so both windows draw the same geometry from the same
//! function and cannot disagree about where a stroke is.

use iced::widget::{
    button, canvas, column, container, mouse_area, responsive, row, scrollable, slider, text,
    tooltip, Row,
};
use iced::{Alignment, ContentFit, Element, Length, Padding, Rectangle, Size};

use pulpit_core::annotation::{
    AnnotationStyle, AnnotationTool, Annotations, InkColor, StrokeKind, ERASER_RADIUS_RANGE,
    HIGHLIGHT_WIDTH_RANGE, INK_WIDTH_RANGE, POINTER_RADIUS_RANGE, SPOTLIGHT_RADIUS_RANGE,
};
use pulpit_core::notes::Region;

use crate::media::overlay::{place, PageBox};
use crate::theme;
use crate::widgets::context::Mode;
use crate::widgets::event::{AnnotationCommand, WidgetEvent};
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{AnnotationControls, Widget, WidgetKind};

pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let annotations = ctx.context.slides.annotations;
    let controls = ctx.context.slides.annotation_controls;
    let mode = ctx.context.mode;
    let on = ctx.on_event;
    match widget.kind() {
        WidgetKind::Annotations => palette(widget, annotations, controls, mode, on),
        other => crate::widgets::common::view::misdirected(other),
    }
}

/// Ink, highlighter, eraser, undo, clear, and who can see the result.
///
/// The armed tool is shown as a selected toggle rather than as a mode
/// indicator somewhere else: the presenter has to be able to tell at a glance
/// whether the next press will draw on the slide or follow a link on it.
fn palette<Message: Clone + 'static>(
    widget: &Widget,
    annotations: &Annotations,
    controls: AnnotationControls,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let options = if mode.interactive() {
        controls.options
    } else {
        widget.annotations()
    };
    // In the editor the palette shows the configured tool, because there is
    // no live pointer to arm anything.
    let armed = if mode.interactive() {
        annotations.tool
    } else {
        None
    };
    let audience_visible = if mode.interactive() {
        annotations.audience_visible
    } else {
        options.audience_visible
    };
    let has_marks = mode.is_sample() || !annotations.is_empty();
    // Undo and redo are offered when there is something to take back or put
    // back — which is not the same as there being marks on the slide, because
    // undoing an erasure puts marks back onto an empty one.
    let has_strokes = controls.can_undo || annotations.has_open_gesture() || mode.is_sample();
    let has_undone = controls.can_redo || mode.is_sample();
    let open = controls.open;
    let wheel = if mode.interactive() {
        controls.wheel
    } else {
        None
    };
    let overflow_open = mode.interactive() && controls.overflow;

    // Every control the palette can draw, in the order it draws them. What
    // does not fit is not dropped: it moves into the overflow menu, from the
    // end backwards, so the tools survive longest and the audience toggle —
    // the one pressed once a talk — is the first to move.
    let mut slots: Vec<Slot> = Vec::new();
    // The hand comes first because it is where a presentation starts and what
    // every tool goes back to: nothing armed, so a press follows the deck's
    // own links, plays its media, and turns its pages. Disarming was only ever
    // reachable by pressing the armed tool a second time, which is not
    // something a presenter finds mid-talk.
    slots.push(Slot::Command {
        label: "Interact with the slide",
        icon: theme::Icon::Hand,
        command: AnnotationCommand::Arm(None),
        selected: armed.is_none(),
        enabled: true,
    });
    for tool in AnnotationTool::ALL {
        // The pointer control arms whichever of the two things it is set to
        // be, so its button is "the spotlight" when that is the mode.
        let armable = if tool == AnnotationTool::Pointer {
            options.pointer_tool()
        } else {
            tool
        };
        // Pressing the armed tool again disarms it, which is how the pointer
        // goes back to links and media overlays without hunting for an "off"
        // button.
        let selected = armed == Some(armable);
        slots.push(Slot::Tool {
            tool,
            armable,
            selected,
            command: AnnotationCommand::Arm(if selected { None } else { Some(armable) }),
        });
    }
    for (label, icon, command, selected, enabled) in [
        (
            "Undo last stroke",
            theme::Icon::Undo,
            AnnotationCommand::Undo,
            false,
            has_strokes,
        ),
        (
            "Redo",
            theme::Icon::Redo,
            AnnotationCommand::Redo,
            false,
            has_undone,
        ),
        (
            "Clear all marks",
            theme::Icon::Trash,
            AnnotationCommand::Clear,
            false,
            has_marks,
        ),
        // Saving writes the document, marks and all: there is no separate
        // annotated copy, because the marks are the document’s own
        // annotations (A1). Offered whenever there is something unsaved,
        // wherever in the deck it was made.
        (
            "Save the document",
            theme::Icon::Save,
            AnnotationCommand::Save,
            false,
            controls.can_save || mode.is_sample(),
        ),
        // The audience toggle draws its *current* state rather than one fixed
        // glyph: a struck-through eye says the marks are the presenter's
        // alone, which is the thing worth being sure of before drawing on a
        // slide in front of a room.
        (
            if audience_visible {
                "Audience sees the marks"
            } else {
                "Marks are private"
            },
            if audience_visible {
                theme::Icon::Eye
            } else {
                theme::Icon::EyeOff
            },
            AnnotationCommand::ToggleAudience,
            // The eye/eye-off glyph already says which way the toggle sits, so
            // it does not also wear the selected highlight.
            false,
            true,
        ),
    ] {
        slots.push(Slot::Command {
            label,
            icon,
            command,
            selected,
            enabled,
        });
    }

    // The palette is a row of square controls, so it can only ever be as tall
    // as it is wide: the buttons grow into the pane until they stop fitting
    // side by side. That is what lets the cell be short without the icons
    // becoming unreachable, and tall without them floating in a void.
    responsive(move |area| {
        let (size, shown) = palette_layout(&slots, area);

        let mut items = Row::new()
            .spacing(theme::space::XS)
            .align_y(Alignment::Center);
        for slot in &slots[..shown] {
            items = items.push(match *slot {
                Slot::Tool {
                    tool,
                    armable,
                    selected,
                    command,
                } => tool_control(
                    tool, armable, command, selected, open, wheel, options, mode, size, on,
                ),
                Slot::Command {
                    label,
                    icon,
                    command,
                    selected,
                    enabled,
                } => control(label, icon, command, selected, enabled, mode, size, on),
            });
        }
        if shown < slots.len() {
            items = items.push(overflow_control(
                &slots[shown..],
                overflow_open,
                open,
                wheel,
                options,
                mode,
                size,
                on,
            ));
        }

        container(items)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    })
    .into()
}

/// One control the palette draws — or, when the pane is too narrow for it,
/// one line of the overflow menu.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Tool {
        tool: AnnotationTool,
        armable: AnnotationTool,
        selected: bool,
        command: AnnotationCommand,
    },
    Command {
        label: &'static str,
        icon: theme::Icon,
        command: AnnotationCommand,
        selected: bool,
        enabled: bool,
    },
}

impl Slot {
    /// How wide this control is drawn at a given button size.
    fn width(&self, size: f32) -> f32 {
        match self {
            // A tool carries its options arrow beside it.
            Slot::Tool { .. } => size * (1.0 + ARROW_FRACTION),
            Slot::Command { .. } => size,
        }
    }
}

/// How many of the controls fit across `available`, given that anything left
/// over needs a button of its own to reach.
///
/// Nothing is silently dropped: the caller draws the rest behind the overflow
/// control, so the count returned is only ever "what is on the row", never
/// "what the presenter can use".
fn fitting(slots: &[Slot], available: f32, size: f32) -> usize {
    let gap = theme::space::XS;
    let total: f32 =
        slots.iter().map(|slot| slot.width(size)).sum::<f32>() + gap * (slots.len() - 1) as f32;
    if total <= available {
        return slots.len();
    }

    // One slot's worth of room goes to the "…" button, and the rest is filled
    // from the front.
    let mut used = size;
    let mut shown = 0;
    for slot in slots {
        let next = used + gap + slot.width(size);
        if next > available {
            break;
        }
        used = next;
        shown += 1;
    }
    shown
}

/// How much width the options arrow adds beside a tool button.
const ARROW_FRACTION: f32 = 0.5;

/// Button size and visible-slot count for one palette pane.
fn palette_layout(slots: &[Slot], area: Size) -> (f32, usize) {
    let count = slots.len() as f32;
    let width = (area.width - theme::space::XS * (count - 1.0)) / count;
    // A tool control carries a narrow arrow gutter beside its button.
    let size = area
        .height
        .min(width / (1.0 + ARROW_FRACTION))
        .clamp(theme::target::MINIMUM * 0.75, 96.0);
    (size, fitting(slots, area.width, size))
}

/// The "…" button, and the menu of everything that did not fit.
///
/// The menu is a list rather than a second row of icons: it is read once,
/// under time pressure, by someone who could not find the control they wanted
/// on the row — so each line says what it is in words.
#[allow(clippy::too_many_arguments)]
fn overflow_control<Message: Clone + 'static>(
    hidden: &[Slot],
    overflow_open: bool,
    open: Option<AnnotationTool>,
    wheel: Option<AnnotationTool>,
    options: crate::widgets::AnnotationOptions,
    mode: Mode,
    size: f32,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let mut trigger = button(theme::icon::icon(theme::Icon::Ellipsis, size * 0.65))
        .padding(size * 0.18)
        .height(Length::Fixed(size))
        .style(theme::ambient::toggle_button(overflow_open));
    if mode.interactive() {
        trigger = trigger.on_press(on(WidgetEvent::Annotate(AnnotationCommand::OpenOverflow(
            !overflow_open,
        ))));
    }
    let trigger: Element<'static, Message> = tooltip(
        trigger,
        text("More annotation controls").size(theme::type_scale::CAPTION),
        tooltip::Position::Top,
    )
    .gap(4)
    .style(theme::ambient::surface)
    .into();

    // A tool reached through this menu has no button of its own on the row,
    // so its colour wheel hangs off the "…" instead. Without this the mixer
    // inside the menu would open a wheel with nothing to open from.
    let hidden_wheel = wheel.filter(|wanted| {
        hidden
            .iter()
            .any(|slot| matches!(slot, Slot::Tool { tool, .. } if tool == wanted))
    });
    let trigger: Element<'static, Message> = match hidden_wheel {
        Some(tool) if mode.interactive() => {
            let armable = if tool == AnnotationTool::Pointer {
                options.pointer_tool()
            } else {
                tool
            };
            crate::widgets::common::color::wheel(
                trigger,
                true,
                colour_of(armable, options),
                on(WidgetEvent::Annotate(AnnotationCommand::OpenColorWheel(
                    None,
                ))),
                move |ink| {
                    on(WidgetEvent::Annotate(AnnotationCommand::SetColor(
                        armable, ink,
                    )))
                },
            )
        }
        _ => trigger,
    };

    let panel = overflow_open.then(|| overflow_menu(hidden, open, options, mode, on));
    crate::widgets::common::popover::Popover::new(trigger, panel)
        .on_dismiss(on(WidgetEvent::Annotate(AnnotationCommand::OpenOverflow(
            false,
        ))))
        .into()
}

/// One row of the overflow menu: an icon, a label, filling the row, styled
/// selected or not — the shape `Slot::Tool` and `Slot::Command` both draw
/// (§80.10).
fn menu_item<'a, Message: Clone + 'a>(
    icon: Element<'a, Message>,
    label: &str,
    selected: bool,
    on_press: Option<Message>,
) -> iced::widget::Button<'a, Message> {
    let mut item = button(
        row![
            icon,
            text(label.to_string()).size(theme::type_scale::CAPTION)
        ]
        .spacing(theme::space::S)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(theme::controls::TOOLBAR_BUTTON)
    .style(theme::ambient::toggle_button(selected));
    if let Some(message) = on_press {
        item = item.on_press(message);
    }
    item
}

/// The small chevron that opens a control's options, in the overflow menu.
fn options_arrow<Message: Clone + 'static>(
    size: f32,
    on_press: Option<Message>,
) -> iced::widget::Button<'static, Message> {
    let mut arrow = button(theme::icon::icon(theme::Icon::ChevronDown, size))
        .padding(theme::space::XS)
        .style(theme::ambient::tool_button);
    if let Some(message) = on_press {
        arrow = arrow.on_press(message);
    }
    arrow
}

fn overflow_menu<Message: Clone + 'static>(
    hidden: &[Slot],
    open: Option<AnnotationTool>,
    options: crate::widgets::AnnotationOptions,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let glyph = theme::type_scale::BODY;
    // A way out in the corner, matching the one on the tool option panels.
    // The menu is opened mid-talk by someone who could not find a control;
    // it must not then be a thing they have to work out how to get rid of.
    let mut menu = column![crate::widgets::common::view::panel_header(
        "More",
        mode.interactive()
            .then(|| on(WidgetEvent::Annotate(AnnotationCommand::OpenOverflow(
                false
            )))),
    )]
    .spacing(theme::space::XS);
    for slot in hidden {
        match *slot {
            Slot::Tool {
                tool,
                armable,
                selected,
                command,
            } => {
                let icon = armable_tool_glyph(armable, options, glyph);
                let arm_press = mode
                    .interactive()
                    .then(|| on(WidgetEvent::Annotate(command)));
                let arm = menu_item(icon, armable.label(), selected, arm_press);
                // The options open *inside* the menu rather than in a second
                // popover over it: a panel hanging off a panel is hard to
                // dismiss, and this one is already a list with room in it.
                let arrow_press = mode.interactive().then(|| {
                    on(WidgetEvent::Annotate(AnnotationCommand::OpenOptions(
                        (open != Some(tool)).then_some(tool),
                    )))
                });
                let arrow = options_arrow(glyph, arrow_press);
                menu = menu.push(
                    row![arm, arrow]
                        .spacing(theme::space::XS)
                        .align_y(Alignment::Center),
                );
                if open == Some(tool) {
                    menu = menu.push(options_panel(tool, armable, options, mode, on));
                }
            }
            Slot::Command {
                label,
                icon,
                command,
                selected,
                enabled,
            } => {
                let press =
                    (mode.interactive() && enabled).then(|| on(WidgetEvent::Annotate(command)));
                let item = menu_item(theme::icon::icon(icon, glyph), label, selected, press);
                menu = menu.push(item);
            }
        }
    }

    container(menu)
        .width(Length::Fixed(232.0))
        .padding(theme::space::S)
        .style(theme::ambient::surface)
        .into()
}

#[allow(clippy::too_many_arguments)]
fn tool_control<Message: Clone + 'static>(
    tool: AnnotationTool,
    // What this control's button arms, which for the pointer is whichever of
    // the dot and the spotlight its options are set to.
    armable: AnnotationTool,
    command: AnnotationCommand,
    selected: bool,
    open: Option<AnnotationTool>,
    wheel: Option<AnnotationTool>,
    options: crate::widgets::AnnotationOptions,
    mode: Mode,
    size: f32,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    // Ink, the highlighter and the pointer are drawn in the colour they are
    // about to lay down; the eraser and the spotlight take no colour, so they
    // stay the palette's own. That tint is the only thing that says what
    // colour is armed without opening the options panel.
    let glyph = size * 0.74;
    let icon = armable_tool_glyph(armable, options, glyph);
    let mut main = button(icon)
        .padding(Padding::from([size * 0.12, size * 0.2]))
        .height(Length::Fixed(size))
        .style(theme::ambient::toggle_button(selected));
    if mode.interactive() {
        main = main.on_press(on(WidgetEvent::Annotate(command)));
    }

    // A tool with nothing to configure carries no arrow and opens no panel.
    // Only the stamp is such a tool: what it puts down is chosen from its own
    // palette. The band has no colour and no size — a rubber band is a shape
    // the hand makes — but it does have a kind, so it keeps its arrow.
    if !armable.has_options() {
        return palette_hint(main.into(), armable.label());
    }

    let toggle = if open == Some(tool) { None } else { Some(tool) };
    let gutter = size * ARROW_FRACTION;
    let mut arrow = button(theme::icon::icon(
        theme::Icon::ChevronDown,
        (gutter * 0.7).max(9.0),
    ))
    .padding(0)
    .width(Length::Fixed(gutter))
    .height(Length::Fixed(gutter))
    .style(theme::ambient::tool_button);
    if mode.interactive() {
        arrow = arrow.on_press(on(WidgetEvent::Annotate(AnnotationCommand::OpenOptions(
            toggle,
        ))));
    }

    let corner = container(arrow)
        .height(Length::Fixed(size))
        .align_y(Alignment::End);
    let trigger: Element<'static, Message> = row![main, corner]
        .spacing(0)
        .align_y(Alignment::Center)
        .into();
    let trigger: Element<'static, Message> = if mode.interactive() {
        mouse_area(trigger)
            .on_right_press(on(WidgetEvent::Annotate(AnnotationCommand::OpenOptions(
                Some(tool),
            ))))
            .into()
    } else {
        trigger
    };
    let trigger: Element<'static, Message> = tooltip(
        trigger,
        text(armable.label()).size(theme::type_scale::CAPTION),
        tooltip::Position::Top,
    )
    .gap(4)
    .style(theme::ambient::surface)
    .into();

    let trigger = crate::widgets::common::color::wheel(
        trigger,
        mode.interactive() && wheel == Some(tool),
        colour_of(armable, options),
        on(WidgetEvent::Annotate(AnnotationCommand::OpenColorWheel(
            None,
        ))),
        move |colour| {
            on(WidgetEvent::Annotate(AnnotationCommand::SetColor(
                armable, colour,
            )))
        },
    );

    // The options panel exists only while its popover is open: building all
    // five closed panels per view pass was widget-tree churn for UI nobody
    // could see.
    let panel = (mode.interactive() && open == Some(tool))
        .then(|| options_panel(tool, armable, options, mode, on));
    crate::widgets::common::popover::Popover::new(trigger, panel)
        .on_dismiss(on(WidgetEvent::Annotate(AnnotationCommand::OpenOptions(
            None,
        ))))
        .into()
}

/// The colour a tool is about to lay down. The eraser and the spotlight lay
/// none, and answer with the ink's so the wheel has something to open on.
fn colour_of(tool: AnnotationTool, options: crate::widgets::AnnotationOptions) -> InkColor {
    match tool {
        AnnotationTool::Ink => options.ink_color,
        AnnotationTool::Highlighter => options.highlight_color,
        AnnotationTool::Pointer => options.pointer_color,
        AnnotationTool::Text => options.text_color,
        AnnotationTool::Note => options.text_color,
        // The eraser, the spotlight, the stamp, the selection arrow and the
        // text selection lay no colour down; they answer with the ink's so
        // the wheel has something to open on.
        AnnotationTool::Spotlight
        | AnnotationTool::Eraser
        | AnnotationTool::Stamp
        | AnnotationTool::Select
        | AnnotationTool::SelectText => options.ink_color,
        // The shape tool draws in the pen's ink, and this palette does not
        // offer it at all.
        AnnotationTool::Shape => options.ink_color,
    }
}

/// The palette sits at the foot of the presenter screen, so its labels rise
/// above the control rather than dropping below it.
fn palette_hint<Message: 'static>(
    control: Element<'static, Message>,
    label: &str,
) -> Element<'static, Message> {
    tooltip(
        control,
        text(label.to_string()).size(theme::type_scale::CAPTION),
        tooltip::Position::Top,
    )
    .gap(4)
    .style(theme::ambient::surface)
    .into()
}

fn options_panel<Message: Clone + 'static>(
    tool: AnnotationTool,
    armable: AnnotationTool,
    options: crate::widgets::AnnotationOptions,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    // Each measure is read and written in its own range. Getting this wrong
    // is not a cosmetic mistake: a slider whose range is not the range the
    // model bounds the value to snaps back the moment it is let go.
    let (value, range) = match armable {
        AnnotationTool::Ink => (options.ink_width, INK_WIDTH_RANGE),
        AnnotationTool::Highlighter => (options.highlight_width, HIGHLIGHT_WIDTH_RANGE),
        AnnotationTool::Eraser => (options.eraser_radius, ERASER_RADIUS_RANGE),
        AnnotationTool::Pointer => (options.pointer_radius, POINTER_RADIUS_RANGE),
        AnnotationTool::Spotlight => (options.spotlight_radius, SPOTLIGHT_RADIUS_RANGE),
        AnnotationTool::Text => (options.text_size, (0.008, 0.12)),
        // Tools the presenter palette never draws an options panel for. The
        // text measures keep the slider well formed.
        AnnotationTool::Note
        | AnnotationTool::Stamp
        | AnnotationTool::Shape
        | AnnotationTool::Select
        | AnnotationTool::SelectText => (options.text_size, (0.008, 0.12)),
    };
    // The track carries a position from 0 to 1 rather than the measure
    // itself, because these ranges span an order of magnitude: laid out
    // evenly, the whole useful half of the spotlight sits in the first
    // centimetre of travel and everything past a third of the way along is
    // the same "covers the slide". Each step is a fixed *proportion* of the
    // measure instead, so the same nudge of the hand is the same visible
    // change at either end.
    //
    // (A slider also steps by one by default, which would round every one of
    // these fractions-of-a-page to nothing. The position is what it steps
    // through, and the step is given explicitly.)
    let size = if mode.interactive() {
        slider(0.0..=1.0, size_position(value, range), move |position| {
            on(WidgetEvent::Annotate(AnnotationCommand::SetSize(
                armable,
                size_at(position, range),
            )))
        })
    } else {
        slider(0.0..=1.0, size_position(value, range), move |_| {
            on(WidgetEvent::Ignored)
        })
    }
    .step(0.005_f32);

    let mut panel = crate::widgets::common::options::Options::new(armable.label()).on_close(
        mode.interactive()
            .then(|| on(WidgetEvent::Annotate(AnnotationCommand::OpenOptions(None)))),
    );

    // The pointer control is two things, and which one it is belongs in its
    // own panel: a presenter who wants the spotlight is already looking here
    // for the size of it.
    let mode_row = (tool == AnnotationTool::Pointer).then(|| {
        let mut modes = Row::new().spacing(theme::space::S);
        for (label, glyph, spotlight) in [
            ("Dot", theme::Icon::Pointer, false),
            ("Spotlight", theme::Icon::Spotlight, true),
        ] {
            let chosen = options.pointer_spotlight == spotlight;
            let mut choice = button(
                row![
                    theme::icon::icon(glyph, theme::type_scale::BODY),
                    text(label).size(theme::type_scale::CAPTION),
                ]
                .spacing(theme::space::XS)
                .align_y(Alignment::Center),
            )
            .padding(theme::controls::TOOLBAR_BUTTON)
            .style(theme::ambient::toggle_button(chosen));
            if mode.interactive() {
                choice = choice.on_press(on(WidgetEvent::Annotate(
                    AnnotationCommand::SetPointerSpotlight(spotlight),
                )));
            }
            modes = modes.push(choice);
        }
        modes
    });

    // The band is three things, the same way the pointer control is two, and
    // which one it is belongs in the same place: the tool's own panel.
    let kind_row = (armable == AnnotationTool::Select).then(|| {
        let mut kinds = Row::new().spacing(theme::space::S);
        for kind in pulpit_core::annotation::SelectKind::ALL {
            let glyph = crate::widgets::common::view::select_kind_glyph(kind);
            let chosen = options.select_kind == kind;
            // Icon-only, like the palette's own buttons: the panel is three
            // choices wide and the words made it read as three commands. The
            // tooltip carries the word instead.
            let mut choice = button(theme::icon::icon(glyph, theme::type_scale::BODY))
                .padding(theme::controls::TOOLBAR_BUTTON)
                .style(theme::ambient::toggle_button(chosen));
            if mode.interactive() {
                choice = choice.on_press(on(WidgetEvent::Annotate(
                    AnnotationCommand::SetSelectKind(kind),
                )));
            }
            kinds = kinds.push(palette_hint(choice.into(), kind.label()));
        }
        kinds
    });

    // The highlighter is three things, the same way the band is, and which
    // one it is belongs in the same place: beside the colour it will be laid
    // down in, because choosing to underline and choosing what to underline
    // in are one decision made in one sitting.
    let markup_row = (armable == AnnotationTool::Highlighter).then(|| {
        let mut kinds = Row::new().spacing(theme::space::S);
        for kind in pulpit_core::annotation::MarkupKind::ALL {
            let glyph = crate::widgets::common::view::markup_kind_glyph(kind);
            let chosen = options.markup_kind == kind;
            let mut choice = button(theme::icon::icon(glyph, theme::type_scale::BODY))
                .padding(theme::controls::TOOLBAR_BUTTON)
                .style(theme::ambient::toggle_button(chosen));
            if mode.interactive() {
                choice = choice.on_press(on(WidgetEvent::Annotate(
                    AnnotationCommand::SetMarkupKind(kind),
                )));
            }
            kinds = kinds.push(palette_hint(choice.into(), kind.label()));
        }
        kinds
    });

    // The spotlight is a hole in the dimming rather than something drawn, so
    // it has no colour to pick; everything else that lays down colour does.
    let color_row = matches!(
        armable,
        AnnotationTool::Ink
            | AnnotationTool::Highlighter
            | AnnotationTool::Pointer
            | AnnotationTool::Text
    )
    .then(|| {
        let selected = match armable {
            AnnotationTool::Ink => options.ink_color,
            AnnotationTool::Highlighter => options.highlight_color,
            AnnotationTool::Text => options.text_color,
            _ => options.pointer_color,
        };
        crate::widgets::common::color::swatches(
            selected,
            crate::widgets::common::color::SWATCH,
            mode.interactive(),
            move |colour| {
                on(WidgetEvent::Annotate(AnnotationCommand::SetColor(
                    armable, colour,
                )))
            },
            on(WidgetEvent::Annotate(AnnotationCommand::OpenColorWheel(
                Some(tool),
            ))),
            palette_hint,
        )
    });

    if let Some(swatches) = color_row {
        panel = panel.row("Color", swatches);
    }
    // A band has no width: it is a shape the hand makes, and the slider above
    // is built from a measure borrowed to keep it well formed. Drawing it
    // would offer a control that changes nothing.
    if armable != AnnotationTool::Select {
        panel = panel.row(
            "Size",
            size.width(Length::Fixed(
                crate::widgets::common::options::CONTROL_WIDTH,
            )),
        );
    }
    if let Some(modes) = mode_row {
        panel = panel.row("Mode", modes);
    }
    if let Some(kinds) = kind_row {
        panel = panel.row("Takes", kinds);
    }
    if let Some(kinds) = markup_row {
        panel = panel.row("Marks", kinds);
    }
    panel.into()
}

/// One closed shape of the highlight wash.
fn outline(builder: &mut canvas::path::Builder, points: &[iced::Point]) {
    let Some((first, rest)) = points.split_first() else {
        return;
    };
    builder.move_to(*first);
    for point in rest {
        builder.line_to(*point);
    }
    builder.close();
}

/// The rectangle a segment of a thick line covers.
///
/// `None` for a segment of no length, which has no direction to be thick in;
/// the disc at its end covers it anyway.
fn segment_quad(from: iced::Point, to: iced::Point, radius: f32) -> Option<[iced::Point; 4]> {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let length = dx.hypot(dy);
    if length <= f32::EPSILON || !length.is_finite() {
        return None;
    }
    let (nx, ny) = (-dy / length * radius, dx / length * radius);
    Some([
        iced::Point::new(from.x + nx, from.y + ny),
        iced::Point::new(to.x + nx, to.y + ny),
        iced::Point::new(to.x - nx, to.y - ny),
        iced::Point::new(from.x - nx, from.y - ny),
    ])
}

/// The disc that rounds a joint, wound the same way [`segment_quad`] winds.
///
/// Direction matters here and nowhere else in the file: the non-zero rule
/// counts crossings *with their sign*, so a disc traced the other way round
/// would cancel the quad it overlaps and punch a hole in the wash at every
/// joint.
fn disc(centre: iced::Point, radius: f32) -> Vec<iced::Point> {
    // Enough sides that the curve reads as a curve at the sizes a
    // highlighter is used at, and few enough to be cheap per joint.
    const SIDES: usize = 24;
    (0..SIDES)
        .map(|side| {
            let angle = -(side as f32) / (SIDES as f32) * std::f32::consts::TAU;
            iced::Point::new(
                centre.x + radius * angle.cos(),
                centre.y + radius * angle.sin(),
            )
        })
        .collect()
}

/// Twice the signed area of a closed polygon: positive one way round the
/// shape, negative the other. Only the sign is ever wanted, and only by the
/// test that holds the wash's pieces to one winding.
#[cfg(test)]
fn signed_area(points: &[iced::Point]) -> f32 {
    let mut total = 0.0;
    for index in 0..points.len() {
        let here = points[index];
        let next = points[(index + 1) % points.len()];
        total += here.x * next.y - next.x * here.y;
    }
    total
}

/// Where along its track a measure sits, from `0.0` to `1.0`.
///
/// Geometric rather than linear: doubling is the same distance everywhere on
/// the track, which is how a size actually reads to the eye.
fn size_position(value: f32, range: (f32, f32)) -> f32 {
    let (low, high) = range;
    if !(value.is_finite() && low > 0.0 && high > low) {
        return 0.0;
    }
    let position = (value.max(low) / low).ln() / (high / low).ln();
    position.clamp(0.0, 1.0)
}

/// The measure at a position on the track: the inverse of [`size_position`].
fn size_at(position: f32, range: (f32, f32)) -> f32 {
    let (low, high) = range;
    if !(position.is_finite() && low > 0.0 && high > low) {
        return low;
    }
    (low * (high / low).powf(position.clamp(0.0, 1.0))).clamp(low, high)
}

#[allow(clippy::too_many_arguments)]
fn control<Message: Clone + 'static>(
    label: &'static str,
    glyph: theme::Icon,
    command: AnnotationCommand,
    selected: bool,
    enabled: bool,
    mode: Mode,
    size: f32,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let icon = theme::icon::icon(glyph, size * 0.65);
    let mut control = button(icon)
        .padding(size * 0.18)
        .height(Length::Fixed(size))
        .style(theme::ambient::toggle_button(selected));
    if mode.interactive() && enabled {
        control = control.on_press(on(WidgetEvent::Annotate(command)));
    }
    tooltip(
        control,
        text(label).size(theme::type_scale::CAPTION),
        tooltip::Position::Top,
    )
    .gap(4)
    .style(theme::ambient::surface)
    .into()
}

/// A tool's icon, in the palette's own text colour like every other glyph.
///
/// It used to be drawn in the ink the tool was about to lay down, which read
/// as a swatch rather than as an icon: a yellow highlighter on a light theme
/// was a faint smudge, and a black pen on a dark one vanished. An icon says
/// *which tool*; the colour it will lay down is what the swatches in its
/// options say, and they say it against a background chosen to show it.
fn tool_glyph<'a, Message: 'a>(glyph: theme::Icon, size: f32) -> Element<'a, Message> {
    theme::icon::icon(glyph, size)
}

/// The glyph an armable tool draws on its own button, wherever that button
/// is: on the row, or reached through the overflow menu when it did not fit.
///
/// Ink, the highlighter and the pointer are drawn in [`tool_glyph`]'s neutral
/// ink because they are the tools that lay down colour — the swatch in their
/// options panel is what says which colour, not the icon — while every other
/// tool keeps [`theme::icon::icon`]'s own rendering because it takes no
/// colour to begin with.
fn armable_tool_glyph<'a, Message: 'a>(
    armable: AnnotationTool,
    options: crate::widgets::AnnotationOptions,
    glyph: f32,
) -> Element<'a, Message> {
    match armable {
        AnnotationTool::Ink => tool_glyph(theme::Icon::Pen, glyph),
        AnnotationTool::Highlighter => tool_glyph(
            crate::widgets::common::view::markup_kind_glyph(options.markup_kind),
            glyph,
        ),
        AnnotationTool::Pointer => tool_glyph(theme::Icon::Pointer, glyph),
        AnnotationTool::Spotlight => theme::icon::icon(theme::Icon::Spotlight, glyph),
        AnnotationTool::Eraser => theme::icon::icon(theme::Icon::Eraser, glyph),
        AnnotationTool::Text => tool_glyph(theme::Icon::Type, glyph),
        AnnotationTool::Note => tool_glyph(theme::Icon::StickyNote, glyph),
        AnnotationTool::Stamp => theme::icon::icon(theme::Icon::Stamp, glyph),
        // Never drawn by this palette: shapes are document mode's, and
        // arriving here at all would be a stale message.
        AnnotationTool::Shape => theme::icon::icon(theme::Icon::Rectangle, glyph),
        AnnotationTool::Select => theme::icon::icon(theme::Icon::Select, glyph),
        AnnotationTool::SelectText => theme::icon::icon(theme::Icon::TextCursor, glyph),
    }
}

/// One geometry cache per place a [`Marks`] layer is drawn.
///
/// The layer re-tessellates only when the annotations (or the panel size)
/// change; between changes the cached geometry is replayed. The caches are
/// per *site* because a cache holds one size — the audience window and the
/// presenter panel would otherwise invalidate each other every frame.
#[derive(Debug, Clone, Default)]
pub struct MarksCaches {
    pub audience: std::rc::Rc<canvas::Cache>,
    pub live: std::rc::Rc<canvas::Cache>,
    pub sample: std::rc::Rc<canvas::Cache>,
}

impl MarksCaches {
    /// The annotations changed: every site draws fresh geometry next frame.
    pub fn invalidate(&self) {
        self.audience.clear();
        self.live.clear();
        self.sample.clear();
    }
}

/// The marks over one slide, in the panel's own pixels.
///
/// Nothing here knows how big the panel is until it is drawn: every
/// coordinate goes through [`place`], the same function the media overlays
/// use, so a stroke follows the letterbox and a `/FitR` crop exactly as an
/// embedded video does.
///
/// The annotations are shared, not copied: this struct is rebuilt on every
/// view pass, and a deep copy of every stroke per pass was most of what the
/// annotation feature cost.
#[derive(Debug)]
pub struct Marks {
    annotations: std::sync::Arc<Annotations>,
    cache: std::rc::Rc<canvas::Cache>,
    style: AnnotationStyle,
    aspect: f32,
    fit: ContentFit,
    crop: Region,
}

impl Marks {
    pub fn new(
        annotations: std::sync::Arc<Annotations>,
        cache: std::rc::Rc<canvas::Cache>,
        style: AnnotationStyle,
        aspect: f32,
        fit: ContentFit,
        crop: Region,
    ) -> Self {
        Self {
            annotations,
            cache,
            style,
            aspect,
            fit,
            crop,
        }
    }

    /// A page point in panel pixels. A zero-sized region is a point, and
    /// placing one is exactly what mapping a coordinate means.
    fn point(&self, panel: Size, point: (f32, f32)) -> Option<iced::Point> {
        let rectangle: Rectangle = place(
            panel,
            self.aspect,
            self.fit,
            self.crop,
            Region::new(point.0, point.1, 0.0, 0.0),
        )?;
        (rectangle.x.is_finite() && rectangle.y.is_finite())
            .then_some(iced::Point::new(rectangle.x, rectangle.y))
    }

    /// A length given as a fraction of the page width, in panel pixels.
    fn length(&self, panel: Size, fraction: f32) -> Option<f32> {
        let page = PageBox::fit(panel, self.aspect, self.fit)?;
        (self.crop.width > 0.0).then(|| fraction * page.width / self.crop.width)
    }
}

impl<Message> canvas::Program<Message> for Marks {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let panel = bounds.size();
        // Tessellation happens only when the cache was invalidated (the
        // annotations changed) or the panel size differs; otherwise the
        // recorded geometry is replayed as-is.
        let geometry = self.cache.draw(renderer, panel, |frame| {
            self.draw_marks(frame, panel);
        });
        vec![geometry]
    }
}

impl Marks {
    /// Every highlight of one colour, laid down as a single translucent
    /// wash.
    ///
    /// A highlighter marks a pixel once. Drawing the strokes one after
    /// another instead — each translucent, each overlapping the last — is
    /// what made a second pass over the same words come out darker than the
    /// first, and made the round joins inside a single stroke show up as
    /// blotches along it. So all the marks of a colour go into *one* path and
    /// are filled *once*, with the non-zero rule: overlapping shapes describe
    /// one region, and a region is painted once however many times the hand
    /// went over it.
    ///
    /// This means a highlight is drawn as the area it covers rather than as a
    /// stroked line — a quad for each segment, and a disc at each joint to
    /// round the corners.
    fn draw_highlights(&self, frame: &mut canvas::Frame, panel: Size) {
        // Grouped by colour, in the order each colour was first used, so two
        // colours still layer the way the presenter drew them.
        let mut colours: Vec<InkColor> = Vec::new();
        for stroke in &self.annotations.strokes {
            if stroke.kind == StrokeKind::Highlight && !colours.contains(&stroke.color) {
                colours.push(stroke.color);
            }
        }

        for colour in colours {
            let strokes =
                self.annotations.strokes.iter().filter(|stroke| {
                    stroke.kind == StrokeKind::Highlight && stroke.color == colour
                });
            let mut anything = false;
            let wash = canvas::Path::new(|builder| {
                for stroke in strokes {
                    let Some(width) = self.length(panel, stroke.width) else {
                        continue;
                    };
                    let radius = (width / 2.0).max(0.5);
                    let points: Vec<iced::Point> = stroke
                        .points
                        .iter()
                        .filter_map(|point| self.point(panel, *point))
                        .collect();
                    if points.is_empty() {
                        continue;
                    }
                    anything = true;
                    for pair in points.windows(2) {
                        // The segment's own thickness, as a quad along it.
                        if let Some(quad) = segment_quad(pair[0], pair[1], radius) {
                            outline(builder, &quad);
                        }
                    }
                    // The joints, which are also the caps at either end, and
                    // the whole of a mark that never moved.
                    for point in &points {
                        outline(builder, &disc(*point, radius));
                    }
                }
            });
            if !anything {
                continue;
            }
            let (red, green, blue) = colour.rgb();
            frame.fill(
                &wash,
                canvas::Fill {
                    style: canvas::Style::Solid(iced::Color {
                        a: StrokeKind::Highlight.opacity(),
                        ..iced::Color::from_rgb(red, green, blue)
                    }),
                    rule: canvas::fill::Rule::NonZero,
                },
            );
        }
    }

    /// The text the highlighter is sweeping right now, before it is a mark.
    ///
    /// Invariant A2: the overlay draws the open gesture itself, because a
    /// selection that only appeared once the page had been re-rasterised would
    /// follow the hand a round trip late. It is only ever the open gesture —
    /// the release clears it and the committed `/Highlight` is what remains —
    /// so it can never outlive a commit and become a second copy of the mark
    /// (A1).
    ///
    /// One path for every run, filled once with the non-zero rule, for the
    /// same reason the committed highlights are: two runs that touch describe
    /// one region, and a region is painted once.
    fn draw_selection(&self, frame: &mut canvas::Frame, panel: Size) {
        let Some(selection) = &self.annotations.selection else {
            return;
        };
        self.draw_runs(
            frame,
            panel,
            &selection.runs,
            selection.kind,
            selection.color,
            selection.opacity,
        );
    }

    /// The runs of one text markup, washed or ruled according to its kind.
    ///
    /// Shared by the live sweep and the committed marks so the two can never
    /// disagree about what a strikeout looks like — a difference between them
    /// would show as a jump at the moment of release.
    ///
    /// A wash is one path over every run, filled once with the non-zero rule:
    /// two runs that touch describe one region, and a region painted twice is
    /// visibly darker where they overlap. Rules are drawn per run, because a
    /// rule is a line along a run and not a region at all.
    fn draw_runs(
        &self,
        frame: &mut canvas::Frame,
        panel: Size,
        runs: &[[(f32, f32); 4]],
        kind: pulpit_core::annotation::MarkupKind,
        color: InkColor,
        opacity: f32,
    ) {
        let (red, green, blue) = color.rgb();
        // Held away from invisible: a mark drawn as nothing reads as a bug.
        let color = iced::Color::from_rgba(red, green, blue, opacity.clamp(0.1, 1.0));
        let Some(at) = kind.rule_at() else {
            let mut any = false;
            let path = canvas::Path::new(|builder| {
                for run in runs {
                    let corners: Option<Vec<iced::Point>> = run
                        .iter()
                        .map(|corner| self.point(panel, *corner))
                        .collect();
                    let Some(corners) = corners else {
                        continue;
                    };
                    builder.move_to(corners[0]);
                    for corner in &corners[1..] {
                        builder.line_to(*corner);
                    }
                    builder.close();
                    any = true;
                }
            });
            if any {
                frame.fill(
                    &path,
                    canvas::Fill {
                        style: canvas::Style::Solid(color),
                        rule: canvas::fill::Rule::NonZero,
                    },
                );
            }
            return;
        };
        for run in runs {
            let corners: Option<Vec<iced::Point>> = run
                .iter()
                .map(|corner| self.point(panel, *corner))
                .collect();
            let Some(corners) = corners else {
                continue;
            };
            // The quad's corners are clockwise from upper-left, so the rule
            // runs from a point down the left edge to the matching point down
            // the right one. Interpolating along the edges rather than
            // splitting a bounding box keeps a rule on a rotated page at the
            // angle of the text it belongs to.
            let along = |from: iced::Point, to: iced::Point| {
                iced::Point::new(from.x + (to.x - from.x) * at, from.y + (to.y - from.y) * at)
            };
            let left = along(corners[0], corners[3]);
            let right = along(corners[1], corners[2]);
            let height = (corners[3].y - corners[0].y)
                .hypot(corners[3].x - corners[0].x)
                .max(f32::EPSILON);
            let thickness = (height * pulpit_core::annotation::MarkupKind::RULE_THICKNESS).max(1.0);
            frame.stroke(
                &canvas::Path::new(|builder| {
                    builder.move_to(left);
                    builder.line_to(right);
                }),
                canvas::Stroke::default()
                    .with_color(color)
                    .with_width(thickness),
            );
        }
    }

    /// The highlights the document holds for this slide.
    ///
    /// Drawn by the overlay rather than by the page's own pixels, because a
    /// slide is rendered without annotations so that the sweep under the hand
    /// and the mark it becomes are never on the screen at once. Each mark is
    /// one path filled once with the non-zero rule, for the reason
    /// `draw_highlights` gives: two runs that touch describe one region, and a
    /// region is painted once.
    fn draw_committed_highlights(&self, frame: &mut canvas::Frame, panel: Size) {
        for highlight in &self.annotations.highlights {
            self.draw_runs(
                frame,
                panel,
                &highlight.runs,
                highlight.kind,
                highlight.color,
                // The mark's own opacity, which is what makes a highlight a
                // highlight in a PDF.
                highlight.opacity,
            );
        }
    }

    /// The sticky notes the document holds for this slide.
    ///
    /// An icon and nothing more. What a note says is behind it, and a slide
    /// that spilled that onto the projector would be showing the audience
    /// something the reader chose to fold away.
    fn draw_notes(&self, frame: &mut canvas::Frame, panel: Size) {
        for note in &self.annotations.notes {
            let (Some(corner), Some(far)) = (
                self.point(panel, note.position),
                self.point(
                    panel,
                    (note.position.0 + note.size.0, note.position.1 + note.size.1),
                ),
            ) else {
                continue;
            };
            let width = (far.x - corner.x).max(6.0);
            let height = (far.y - corner.y).max(6.0);
            let (red, green, blue) = note.color.rgb();
            let colour = iced::Color::from_rgb(red, green, blue);
            let body = canvas::Path::new(|builder| {
                builder.rectangle(corner, Size::new(width, height));
            });
            frame.fill(&body, iced::Color { a: 0.85, ..colour });
            frame.stroke(
                &body,
                canvas::Stroke::default()
                    .with_color(iced::Color {
                        a: 0.9,
                        ..iced::Color::BLACK
                    })
                    .with_width(1.0),
            );
            // Two rules for "there are words in here", which is all the icon
            // has room to say at slide size.
            for line in 1..3 {
                let y = corner.y + height * (line as f32) / 3.0;
                let rule = canvas::Path::new(|builder| {
                    builder.move_to(iced::Point::new(corner.x + width * 0.2, y));
                    builder.line_to(iced::Point::new(corner.x + width * 0.8, y));
                });
                frame.stroke(
                    &rule,
                    canvas::Stroke::default()
                        .with_color(iced::Color {
                            a: 0.7,
                            ..iced::Color::BLACK
                        })
                        .with_width(1.0),
                );
            }
        }
    }

    /// The rubber band being dragged, and the marks it is holding.
    ///
    /// The band is drawn in the interface's accent rather than in a mark
    /// colour: it is not a mark, it is a question about which marks, and it
    /// disappears at the release. What it is holding is outlined in the same
    /// accent, which is the only thing on the slide that says a press of
    /// delete will take those and not everything.
    fn draw_band(&self, frame: &mut canvas::Frame, panel: Size) {
        let accent = theme::ambient::palette().accent;
        for held in self.held_bounds(panel) {
            let outline = canvas::Path::new(|builder| {
                builder.rectangle(held.0, held.1);
            });
            frame.fill(&outline, iced::Color { a: 0.12, ..accent });
            frame.stroke(
                &outline,
                canvas::Stroke::default()
                    .with_color(iced::Color { a: 0.8, ..accent })
                    .with_width(1.5),
            );
        }
        let Some((from, to)) = self.annotations.band else {
            return;
        };
        let (Some(from), Some(to)) = (self.point(panel, from), self.point(panel, to)) else {
            return;
        };
        let corner = iced::Point::new(from.x.min(to.x), from.y.min(to.y));
        let size = Size::new((to.x - from.x).abs(), (to.y - from.y).abs());
        let band = canvas::Path::new(|builder| {
            builder.rectangle(corner, size);
        });
        frame.fill(&band, iced::Color { a: 0.1, ..accent });
        frame.stroke(
            &band,
            canvas::Stroke::default()
                .with_color(iced::Color { a: 0.9, ..accent })
                .with_width(1.5),
        );
    }

    /// Where each held mark is on the panel, as a corner and a size.
    ///
    /// Bounding boxes rather than the marks' own outlines: what this says is
    /// "these are the ones", and a stroke traced in a second colour reads as
    /// a second stroke.
    fn held_bounds(&self, panel: Size) -> Vec<(iced::Point, Size)> {
        if self.annotations.selected.is_empty() {
            return Vec::new();
        }
        let held = |id: &pulpit_core::annotate::AnnotationId| {
            self.annotations.selected.iter().any(|other| other == id)
        };
        let mut boxes = Vec::new();
        let mut add = |points: Vec<(f32, f32)>| {
            let corners: Vec<iced::Point> = points
                .into_iter()
                .filter_map(|point| self.point(panel, point))
                .collect();
            let Some(first) = corners.first() else {
                return;
            };
            let mut left = first.x;
            let mut top = first.y;
            let mut right = first.x;
            let mut bottom = first.y;
            for corner in &corners[1..] {
                left = left.min(corner.x);
                top = top.min(corner.y);
                right = right.max(corner.x);
                bottom = bottom.max(corner.y);
            }
            const PADDING: f32 = 3.0;
            boxes.push((
                iced::Point::new(left - PADDING, top - PADDING),
                Size::new(
                    (right - left) + PADDING * 2.0,
                    (bottom - top) + PADDING * 2.0,
                ),
            ));
        };
        for stroke in &self.annotations.strokes {
            if stroke.id.as_ref().is_some_and(&held) {
                add(stroke.points.clone());
            }
        }
        for mark in &self.annotations.texts {
            if mark.annotation.as_ref().is_some_and(&held) {
                let fit = mark.fit.unwrap_or((mark.size * 4.0, mark.size * 1.6));
                add(vec![
                    mark.position,
                    (mark.position.0 + fit.0, mark.position.1 + fit.1),
                ]);
            }
        }
        for highlight in &self.annotations.highlights {
            if held(&highlight.id) {
                add(highlight.runs.iter().flatten().copied().collect());
            }
        }
        for note in &self.annotations.notes {
            if held(&note.id) {
                add(vec![
                    note.position,
                    (note.position.0 + note.size.0, note.position.1 + note.size.1),
                ]);
            }
        }
        boxes
    }

    fn draw_marks(&self, frame: &mut canvas::Frame, panel: Size) {
        // The spotlight goes underneath the ink: dimming what has just been
        // circled would defeat the circle.
        if let Some(centre) = self.annotations.spotlight {
            if let (Some(centre), Some(radius)) = (
                self.point(panel, centre),
                self.length(panel, self.style.spotlight_radius),
            ) {
                // One path holding the whole panel and the lit circle, filled
                // with the even-odd rule: the circle is a hole in the dimming
                // rather than a second shape drawn over it, which is what
                // keeps the slide itself untouched inside it.
                let shade = canvas::Path::new(|builder| {
                    builder.rectangle(iced::Point::ORIGIN, panel);
                    builder.circle(centre, radius.max(1.0));
                });
                frame.fill(
                    &shade,
                    canvas::Fill {
                        style: canvas::Style::Solid(iced::Color {
                            a: self.style.dim,
                            ..iced::Color::BLACK
                        }),
                        rule: canvas::fill::Rule::EvenOdd,
                    },
                );
            }
        }

        self.draw_committed_highlights(frame, panel);
        self.draw_highlights(frame, panel);
        self.draw_selection(frame, panel);

        for stroke in &self.annotations.strokes {
            if stroke.kind == StrokeKind::Highlight {
                continue;
            }
            let (red, green, blue) = stroke.color.rgb();
            let colour = iced::Color {
                a: stroke.kind.opacity(),
                ..iced::Color::from_rgb(red, green, blue)
            };
            let Some(width) = self.length(panel, stroke.width) else {
                continue;
            };
            let points: Vec<iced::Point> = stroke
                .points
                .iter()
                .filter_map(|point| self.point(panel, *point))
                .collect();
            match points.as_slice() {
                [] => {}
                // A press that never moved is a dot, and a zero-length line
                // draws nothing at all.
                [only] => frame.fill(&canvas::Path::circle(*only, (width / 2.0).max(1.0)), colour),
                points => {
                    let path = canvas::Path::new(|builder| {
                        builder.move_to(points[0]);
                        for point in &points[1..] {
                            builder.line_to(*point);
                        }
                    });
                    frame.stroke(
                        &path,
                        canvas::Stroke::default()
                            .with_color(colour)
                            .with_width(width.max(1.0))
                            .with_line_join(canvas::LineJoin::Round)
                            .with_line_cap(canvas::LineCap::Round),
                    );
                }
            }
        }

        // Notes over the ink: an icon is a small target, and one under a
        // stroke would be an icon nobody can see is there.
        self.draw_notes(frame, panel);
        self.draw_band(frame, panel);

        // The pointer dot last: it is where the presenter is looking now, so
        // nothing is allowed to be drawn over it.
        if let Some(position) = self.annotations.pointer {
            if let (Some(centre), Some(radius)) = (
                self.point(panel, position),
                self.length(panel, self.style.pointer_radius),
            ) {
                let radius = radius.max(2.0);
                // The presenter's chosen colour, not the interface's accent:
                // the dot is drawn on somebody else's slide, and a dot the
                // colour of the chrome is the one that disappears into it.
                let (red, green, blue) = self.style.pointer_color.rgb();
                let colour = iced::Color::from_rgb(red, green, blue);
                frame.fill(
                    &canvas::Path::circle(centre, radius * 1.6),
                    iced::Color { a: 0.25, ..colour },
                );
                frame.fill(&canvas::Path::circle(centre, radius), colour);
            }
        }
    }
}

/// The padding inside the writing box, which is a fraction of the text rather
/// than a constant.
///
/// Six pixels is comfortable around twelve-point text and looks like a
/// mistake around sixty-point text, and annotation text is sized as a
/// fraction of the page, so the same box has to hold both.
fn text_editor_padding(font_size: f32) -> f32 {
    (font_size * 0.35).clamp(8.0, 28.0)
}

/// How wide and tall the writing box has to be to show what is in it.
///
/// The width per character is deliberately generous. The text is drawn in a
/// proportional font, so a character is anywhere between about a third of an
/// em and a full em wide; estimating with the average hides the tail of any
/// line that leans on capitals or wide letters, and a box that hides what you
/// just typed is worse than one that is wider than it needed to be. The extra
/// character on the end is the caret's room, which is otherwise the first
/// thing to fall off the edge.
fn text_editor_size(content: &str, font_size: f32, padding: f32) -> Size {
    const EM_PER_CHARACTER: f32 = 0.68;
    // Iced's default, and the box is sized to what iced will draw.
    const LINE_HEIGHT: f32 = 1.3;
    let mut line_count = 0_usize;
    let mut longest = 0_usize;
    for line in content.split('\n') {
        line_count += 1;
        longest = longest.max(line.chars().count());
    }
    Size::new(
        ((longest.max(4) + 1) as f32 * font_size * EM_PER_CHARACTER) + padding * 2.0,
        (line_count.max(1) as f32 * font_size * LINE_HEIGHT) + padding * 2.0,
    )
}

/// Where the writing box sits so that it stays on the page.
///
/// A box is anchored to the mark, but a mark near the right or bottom edge
/// leaves no room to the right or below it. Clamping the box to the space
/// that remains is what hid the text: place a mark an inch from the edge and
/// the box became an inch wide. So the box slides back onto the page instead,
/// and only gives up size when the page itself is smaller than the box.
fn text_editor_placement(
    anchor: (f32, f32),
    desired: Size,
    page_origin: (f32, f32),
    page_end: (f32, f32),
) -> (f32, f32, f32, f32) {
    let width = desired.width.min((page_end.0 - page_origin.0).max(1.0));
    let height = desired.height.min((page_end.1 - page_origin.1).max(1.0));
    let left = anchor.0.min(page_end.0 - width).max(page_origin.0);
    let top = anchor.1.min(page_end.1 - height).max(page_origin.1);
    (left, top, width, height)
}

/// The marks as an element to stack over a slide picture.
///
/// `None` when there is nothing to draw, so a slide with no annotations
/// carries no extra layer — and, more to the point, no transparent layer over
/// the picture that could take a press meant for a link.
#[allow(clippy::too_many_arguments)]
pub fn marks<'a, Message: 'a>(
    annotations: &std::sync::Arc<Annotations>,
    cache: &std::rc::Rc<canvas::Cache>,
    style: AnnotationStyle,
    aspect: f32,
    fit: ContentFit,
    crop: Region,
    show_text_editor: bool,
    rendered_text: &std::sync::Arc<
        std::collections::HashMap<u64, crate::typst_annotation::RenderedText>,
    >,
) -> Option<Element<'a, Message>> {
    if annotations.is_empty() {
        return None;
    }
    let annotations = std::sync::Arc::clone(annotations);
    let cache = std::rc::Rc::clone(cache);
    let rendered_text = std::sync::Arc::clone(rendered_text);
    Some(
        responsive(move |panel| {
            let ink = canvas(Marks::new(
                std::sync::Arc::clone(&annotations),
                std::rc::Rc::clone(&cache),
                style,
                aspect,
                fit,
                crop,
            ))
            .width(Length::Fill)
            .height(Length::Fill);
            let mut layers = iced::widget::Stack::new().push(ink);
            for (index, mark) in annotations.texts.iter().enumerate() {
                if show_text_editor && annotations.typing_index() == Some(index) {
                    let region = Region::new(mark.position.0, mark.position.1, 1.0, 1.0);
                    let (Some(rect), Some(page)) = (
                        place(panel, aspect, fit, crop, region),
                        PageBox::fit(panel, aspect, fit),
                    ) else {
                        continue;
                    };
                    let font_size = (mark.size * page.width / crop.width.max(0.001)).max(8.0);
                    let padding = text_editor_padding(font_size);
                    let desired = text_editor_size(&mark.text, font_size, padding);
                    // The box is offset by its own padding so the text inside
                    // it starts where the mark is, not where the frame is.
                    let (left, top, width, height) = text_editor_placement(
                        (rect.x - padding, rect.y - padding),
                        desired,
                        (page.x.max(0.0), page.y.max(0.0)),
                        (
                            (page.x + page.width).min(panel.width),
                            (page.y + page.height).min(panel.height),
                        ),
                    );
                    let (red, green, blue) = mark.color.rgb();
                    let source = text(if mark.text.is_empty() {
                        " ".to_owned()
                    } else {
                        mark.text.clone()
                    })
                    .size(font_size)
                    .color(iced::Color::from_rgb(red, green, blue));
                    let palette = theme::ambient::palette();
                    let editor = container(
                        scrollable(source)
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .style(theme::ambient::scrollbar),
                    )
                    .padding(padding)
                    .width(Length::Fixed(width))
                    .height(Length::Fixed(height))
                    .style(move |_| container::Style {
                        background: Some(iced::Background::Color(iced::Color {
                            a: 0.58,
                            ..palette.surface
                        })),
                        border: iced::Border {
                            color: iced::Color {
                                a: 0.9,
                                ..palette.accent
                            },
                            width: 2.0,
                            radius: theme::radius::SMALL.into(),
                        },
                        ..container::Style::default()
                    });
                    layers = layers.push(
                        container(editor)
                            .padding(Padding {
                                top,
                                right: 0.0,
                                bottom: 0.0,
                                left,
                            })
                            .width(Length::Fill)
                            .height(Length::Fill),
                    );
                    continue;
                }
                let Some(rendered) = rendered_text.get(&mark.id) else {
                    continue;
                };
                let region = Region::new(mark.position.0, mark.position.1, 1.0, 1.0);
                let Some(rect) = place(panel, aspect, fit, crop, region) else {
                    continue;
                };
                if let Some(handle) = &rendered.handle {
                    let Some(page) = PageBox::fit(panel, aspect, fit) else {
                        continue;
                    };
                    // A label read back out of the document is drawn in the
                    // box the annotation occupies, because that box is what
                    // the file records — the type size it was set at is not.
                    // One being written here is drawn at the size it was set
                    // at, in the room that is left to the edge of the page.
                    let (available_width, available_height) = match mark.fit {
                        Some(box_fit) => {
                            let far = Region::new(
                                mark.position.0 + box_fit.0,
                                mark.position.1 + box_fit.1,
                                1.0,
                                1.0,
                            );
                            match place(panel, aspect, fit, crop, far) {
                                Some(far) => ((far.x - rect.x).max(0.0), (far.y - rect.y).max(0.0)),
                                None => continue,
                            }
                        }
                        None => (
                            ((page.x + page.width).min(panel.width) - rect.x).max(0.0),
                            ((page.y + page.height).min(panel.height) - rect.y).max(0.0),
                        ),
                    };
                    let (width, height) =
                        fit_svg_viewport(available_width, available_height, rendered.aspect);
                    let picture = iced::widget::svg(handle.clone())
                        .width(Length::Fixed(width))
                        .height(Length::Fixed(height));
                    layers = layers.push(
                        container(picture)
                            .padding(Padding {
                                top: rect.y,
                                right: 0.0,
                                bottom: 0.0,
                                left: rect.x,
                            })
                            .width(Length::Fill)
                            .height(Length::Fill),
                    );
                }
                if show_text_editor {
                    if let Some(error) = &rendered.error {
                        let diagnostic = text(format!("Typst: {error}"))
                            .size(theme::type_scale::CAPTION)
                            .color(theme::ambient::alert());
                        layers = layers.push(
                            container(diagnostic)
                                .padding(Padding {
                                    top: rect.y + 8.0,
                                    right: 8.0,
                                    bottom: 0.0,
                                    left: rect.x + 8.0,
                                })
                                .width(Length::Fill)
                                .height(Length::Fill),
                        );
                    }
                }
            }
            layers.into()
        })
        .into(),
    )
}

fn fit_svg_viewport(max_width: f32, max_height: f32, aspect: f32) -> (f32, f32) {
    let max_width = max_width.max(0.0);
    let max_height = max_height.max(0.0);
    let aspect = aspect.max(0.001);
    let natural_height = max_width / aspect;
    if natural_height > max_height {
        (max_height * aspect, max_height)
    } else {
        (max_width, natural_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::annotation::InkColor;

    const WIDE: f32 = 16.0 / 9.0;

    fn marks_over(panel: Size) -> (Marks, Size) {
        (
            Marks::new(
                std::sync::Arc::new(Annotations::default()),
                std::rc::Rc::new(canvas::Cache::new()),
                AnnotationStyle::default(),
                WIDE,
                ContentFit::Contain,
                Region::FULL,
            ),
            panel,
        )
    }

    fn a_palette_of(tools: usize, commands: usize) -> Vec<Slot> {
        let mut slots: Vec<Slot> = (0..tools)
            .map(|_| Slot::Tool {
                tool: AnnotationTool::Ink,
                armable: AnnotationTool::Ink,
                selected: false,
                command: AnnotationCommand::Undo,
            })
            .collect();
        slots.extend((0..commands).map(|_| Slot::Command {
            label: "",
            icon: theme::Icon::Undo,
            command: AnnotationCommand::Undo,
            selected: false,
            enabled: true,
        }));
        slots
    }

    #[test]
    fn a_narrow_palette_keeps_the_tools_and_moves_the_rest_behind_one_button() {
        let slots = a_palette_of(4, 4);
        let size = 40.0;
        let gap = theme::space::XS;
        let full: f32 =
            slots.iter().map(|slot| slot.width(size)).sum::<f32>() + gap * (slots.len() - 1) as f32;

        // Room for everything: nothing goes behind the "…".
        assert_eq!(fitting(&slots, full, size), slots.len());
        assert_eq!(fitting(&slots, full + 200.0, size), slots.len());

        // Take away one control's worth of room, and what moves is the tail —
        // the audience toggle, not the pen.
        let shown = fitting(&slots, full - size - gap, size);
        assert!(shown < slots.len(), "nothing moved into the menu");
        assert!(
            shown >= 4,
            "the tools went before the commands did: {shown} shown"
        );

        // The row it draws, plus the button that reaches the rest, still fits.
        let drawn: f32 = slots[..shown]
            .iter()
            .map(|slot| slot.width(size))
            .sum::<f32>()
            + size
            + gap * shown as f32;
        assert!(drawn <= full - size - gap, "the row itself overflows");
    }

    #[test]
    fn the_presenter_palettes_half_band_keeps_every_control_on_the_row() {
        // Five tools plus the hand and five document commands is the complete
        // live palette. These are the usable cell dimensions in a 1280×720
        // Presenter window after the split gaps and cell padding.
        let slots = a_palette_of(5, 6);
        let (_, shown) = palette_layout(&slots, Size::new(624.0, 53.0));
        assert_eq!(shown, slots.len());

        // The former 16% pane is why the palette always collapsed.
        let (_, formerly_shown) = palette_layout(&slots, Size::new(196.0, 53.0));
        assert!(formerly_shown < slots.len());
    }

    #[test]
    fn a_palette_with_no_room_at_all_still_offers_the_menu() {
        let slots = a_palette_of(4, 4);
        assert_eq!(fitting(&slots, 10.0, 40.0), 0);
    }

    #[test]
    fn every_piece_of_a_highlight_is_wound_the_same_way_round() {
        // The wash is one filled path under the non-zero rule, so a piece
        // wound against the others would subtract from them: a hole in the
        // highlight at every joint, which is the sort of thing that only
        // shows up on a projector.
        let disc = disc(iced::Point::new(10.0, 10.0), 4.0);
        let disc_area = signed_area(&disc);
        assert!(disc_area.abs() > 0.0);

        for (from, to) in [
            ((0.0, 0.0), (10.0, 0.0)),
            ((10.0, 0.0), (0.0, 0.0)),
            ((0.0, 0.0), (0.0, 10.0)),
            ((3.0, 7.0), (-4.0, -2.0)),
        ] {
            let quad = segment_quad(
                iced::Point::new(from.0, from.1),
                iced::Point::new(to.0, to.1),
                2.0,
            )
            .expect("a segment with length");
            let area = signed_area(&quad);
            assert!(area.abs() > 0.0);
            assert_eq!(
                area.is_sign_positive(),
                disc_area.is_sign_positive(),
                "the quad from {from:?} to {to:?} runs against the discs"
            );
        }

        assert!(
            segment_quad(iced::Point::ORIGIN, iced::Point::ORIGIN, 2.0).is_none(),
            "a segment that went nowhere has no direction to be thick in"
        );
    }

    #[test]
    fn a_size_track_spends_its_travel_where_the_sizes_are_useful() {
        use pulpit_core::annotation::SPOTLIGHT_RADIUS_RANGE as SPOTLIGHT;

        // The ends are the ends.
        assert!((size_at(0.0, SPOTLIGHT) - SPOTLIGHT.0).abs() < 1e-6);
        assert!((size_at(1.0, SPOTLIGHT) - SPOTLIGHT.1).abs() < 1e-6);

        // A third of the way along is a circle to read a paragraph through,
        // not one that lights the slide: well under half the widest radius.
        let third = size_at(1.0 / 3.0, SPOTLIGHT);
        assert!(
            third < SPOTLIGHT.1 / 2.0,
            "a third of the track gave {third}"
        );

        // The middle of the track doubles in the same number of pixels the
        // start does, which is what makes the track read evenly.
        let ratio = |a: f32, b: f32| size_at(b, SPOTLIGHT) / size_at(a, SPOTLIGHT);
        assert!((ratio(0.0, 0.25) - ratio(0.5, 0.75)).abs() < 1e-3);

        for range in [INK_WIDTH_RANGE, HIGHLIGHT_WIDTH_RANGE, SPOTLIGHT] {
            let value = size_at(0.42, range);
            assert!(
                (size_position(value, range) - 0.42).abs() < 1e-3,
                "{range:?}"
            );
        }
    }

    #[test]
    fn a_page_point_lands_inside_the_letterbox_not_the_panel() {
        // A 16:9 page in a square panel is drawn 200×112.5 at y = 43.75.
        let (marks, panel) = marks_over(Size::new(200.0, 200.0));
        let top_left = marks.point(panel, (0.0, 0.0)).unwrap();
        assert!(top_left.x.abs() < 1e-3);
        assert!((top_left.y - 43.75).abs() < 1e-3);

        let centre = marks.point(panel, (0.5, 0.5)).unwrap();
        assert!((centre.x - 100.0).abs() < 1e-3);
        assert!((centre.y - 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_length_is_a_fraction_of_the_drawn_page_and_grows_with_a_zoom() {
        let (marks, panel) = marks_over(Size::new(1600.0, 900.0));
        assert!((marks.length(panel, 0.1).unwrap() - 160.0).abs() < 1e-2);

        // Showing half the page doubles everything drawn on it.
        let zoomed = Marks::new(
            std::sync::Arc::new(Annotations::default()),
            std::rc::Rc::new(canvas::Cache::new()),
            AnnotationStyle::default(),
            WIDE,
            ContentFit::Contain,
            Region::new(0.0, 0.0, 0.5, 1.0),
        );
        assert!((zoomed.length(panel, 0.1).unwrap() - 320.0).abs() < 1e-2);
    }

    #[test]
    fn degenerate_geometry_produces_no_coordinates_rather_than_a_panic() {
        let (marks, _) = marks_over(Size::new(0.0, 0.0));
        assert!(marks.point(Size::new(0.0, 0.0), (0.5, 0.5)).is_none());
        assert!(marks.length(Size::new(0.0, 0.0), 0.1).is_none());
    }

    #[test]
    fn a_slide_with_no_marks_carries_no_layer_over_the_picture() {
        let empty = std::sync::Arc::new(Annotations::default());
        let cache = std::rc::Rc::new(canvas::Cache::new());
        let rendered = std::sync::Arc::new(std::collections::HashMap::new());
        let layer: Option<Element<'_, ()>> = marks(
            &empty,
            &cache,
            AnnotationStyle::default(),
            WIDE,
            ContentFit::Contain,
            Region::FULL,
            true,
            &rendered,
        );
        assert!(layer.is_none());

        let mut drawn = Annotations::default();
        drawn.begin_stroke((0.2, 0.2), 0.004, InkColor::Red);
        let _ = drawn.end_stroke();
        let drawn = std::sync::Arc::new(drawn);
        let layer: Option<Element<'_, ()>> = marks(
            &drawn,
            &cache,
            AnnotationStyle::default(),
            WIDE,
            ContentFit::Contain,
            Region::FULL,
            true,
            &rendered,
        );
        assert!(layer.is_some());
    }

    #[test]
    fn the_text_editor_grows_with_words_and_lines() {
        let empty = text_editor_size("", 20.0, 6.0);
        let word = text_editor_size("a longer label", 20.0, 6.0);
        let lines = text_editor_size("a longer label\nsecond line", 20.0, 6.0);
        assert!(word.width > empty.width);
        assert!(lines.height > word.height);
        // Wide enough for capitals and a caret, not for the average letter.
        assert!(word.width > "a longer label".len() as f32 * 20.0 * 0.6 + 12.0);
        // Tall enough for the line iced actually draws, both of them.
        assert!(lines.height >= 2.0 * 20.0 * 1.3 + 12.0);
    }

    /// The box grows with the text, so its breathing room has to grow too:
    /// the padding that suits small text reads as a mistake around large.
    #[test]
    fn the_writing_box_pads_in_proportion_to_its_text() {
        assert!(text_editor_padding(60.0) > text_editor_padding(12.0));
        // And never disappears, however small the text on the page is.
        assert!(text_editor_padding(4.0) >= 8.0);
    }

    /// A mark near an edge used to clamp its box to the sliver of page left
    /// beside it, which hid the text. The box moves instead.
    #[test]
    fn a_box_near_an_edge_slides_back_onto_the_page_instead_of_shrinking() {
        let desired = Size::new(200.0, 60.0);
        let (left, top, width, height) =
            text_editor_placement((980.0, 30.0), desired, (0.0, 0.0), (1000.0, 500.0));
        assert_eq!((width, height), (200.0, 60.0), "full size is kept");
        assert_eq!(left, 800.0, "slid back inside the right edge");
        assert_eq!(top, 30.0, "and left alone where there was room");

        // The bottom edge behaves the same way.
        let (_, top, _, _) =
            text_editor_placement((10.0, 480.0), desired, (0.0, 0.0), (1000.0, 500.0));
        assert_eq!(top, 440.0);

        // Only a page smaller than the box costs the box its size, and then
        // it sits at the page's own origin rather than off the top left.
        let (left, top, width, height) =
            text_editor_placement((60.0, 60.0), desired, (50.0, 50.0), (150.0, 90.0));
        assert_eq!((width, height), (100.0, 40.0));
        assert_eq!((left, top), (50.0, 50.0));
    }

    #[test]
    fn a_tall_svg_is_scaled_to_fit_without_clipping() {
        assert_eq!(fit_svg_viewport(300.0, 100.0, 1.5), (150.0, 100.0));
        assert_eq!(fit_svg_viewport(300.0, 200.0, 1.5), (300.0, 200.0));
    }
}
