//! Drawing the annotation palette, and the marks themselves.
//!
//! Two things live here because they are two halves of one idea: the palette
//! says what the pointer is about to do, and [`marks`] draws what it did. The
//! marks are a `canvas` layer stacked over the slide by
//! [`crate::widgets::slides::view`] and over the audience picture by
//! [`crate::view`], so both windows draw the same geometry from the same
//! function and cannot disagree about where a stroke is.

use iced::widget::{
    button, canvas, column, container, mouse_area, responsive, row, scrollable, slider, space,
    text, tooltip, Row,
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
        .style(if overflow_open {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        });
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
            let (red, green, blue) = colour_of(armable, options).rgb();
            crate::vendor::iced_aw::ColorPicker::new(
                true,
                iced::Color::from_rgb(red, green, blue),
                trigger,
                on(WidgetEvent::Annotate(AnnotationCommand::OpenColorWheel(
                    None,
                ))),
                move |colour| {
                    on(WidgetEvent::Annotate(AnnotationCommand::SetColor(
                        armable,
                        InkColor::from_rgb(colour.r, colour.g, colour.b),
                    )))
                },
            )
            .into()
        }
        _ => trigger,
    };

    let panel = overflow_open.then(|| overflow_menu(hidden, open, options, mode, on));
    super::popover::Popover::new(trigger, panel).into()
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
    let mut menu = column![row![
        text("More").size(theme::type_scale::LABEL),
        space::horizontal().width(Length::Fill),
        button(theme::icon::icon(theme::Icon::Close, glyph))
            .padding(2)
            .style(theme::ambient::tool_button)
            .on_press_maybe(mode.interactive().then(|| on(WidgetEvent::Annotate(
                AnnotationCommand::OpenOverflow(false)
            )))),
    ]
    .align_y(Alignment::Center)]
    .spacing(theme::space::XS);
    for slot in hidden {
        match *slot {
            Slot::Tool {
                tool,
                armable,
                selected,
                command,
            } => {
                let icon = match armable {
                    AnnotationTool::Ink => tool_glyph(theme::Icon::Pen, glyph, options.ink_color),
                    AnnotationTool::Highlighter => {
                        tool_glyph(theme::Icon::Highlighter, glyph, options.highlight_color)
                    }
                    AnnotationTool::Pointer => {
                        tool_glyph(theme::Icon::Pointer, glyph, options.pointer_color)
                    }
                    AnnotationTool::Spotlight => theme::icon::icon(theme::Icon::Spotlight, glyph),
                    AnnotationTool::Eraser => theme::icon::icon(theme::Icon::Eraser, glyph),
                    AnnotationTool::Text => {
                        tool_glyph(theme::Icon::Type, glyph, options.text_color)
                    }
                    AnnotationTool::Note => {
                        tool_glyph(theme::Icon::StickyNote, glyph, options.text_color)
                    }
                    AnnotationTool::Stamp => theme::icon::icon(theme::Icon::Stamp, glyph),
                    AnnotationTool::Select => theme::icon::icon(theme::Icon::Select, glyph),
                };
                let mut arm = button(
                    row![icon, text(armable.label()).size(theme::type_scale::CAPTION)]
                        .spacing(theme::space::S)
                        .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding(Padding::from([4.0, theme::space::S]))
                .style(if selected {
                    theme::ambient::selected_button
                } else {
                    theme::ambient::tool_button
                });
                // The options open *inside* the menu rather than in a second
                // popover over it: a panel hanging off a panel is hard to
                // dismiss, and this one is already a list with room in it.
                let mut arrow = button(theme::icon::icon(theme::Icon::ChevronDown, glyph))
                    .padding(theme::space::XS)
                    .style(theme::ambient::tool_button);
                if mode.interactive() {
                    arm = arm.on_press(on(WidgetEvent::Annotate(command)));
                    arrow = arrow.on_press(on(WidgetEvent::Annotate(
                        AnnotationCommand::OpenOptions((open != Some(tool)).then_some(tool)),
                    )));
                }
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
                let mut item = button(
                    row![
                        theme::icon::icon(icon, glyph),
                        text(label).size(theme::type_scale::CAPTION),
                    ]
                    .spacing(theme::space::S)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding(Padding::from([4.0, theme::space::S]))
                .style(if selected {
                    theme::ambient::selected_button
                } else {
                    theme::ambient::tool_button
                });
                if mode.interactive() && enabled {
                    item = item.on_press(on(WidgetEvent::Annotate(command)));
                }
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
    let icon = match armable {
        AnnotationTool::Ink => tool_glyph(theme::Icon::Pen, glyph, options.ink_color),
        AnnotationTool::Highlighter => {
            tool_glyph(theme::Icon::Highlighter, glyph, options.highlight_color)
        }
        AnnotationTool::Pointer => tool_glyph(theme::Icon::Pointer, glyph, options.pointer_color),
        AnnotationTool::Spotlight => theme::icon::icon(theme::Icon::Spotlight, glyph),
        AnnotationTool::Eraser => theme::icon::icon(theme::Icon::Eraser, glyph),
        AnnotationTool::Text => tool_glyph(theme::Icon::Type, glyph, options.text_color),
        AnnotationTool::Note => tool_glyph(theme::Icon::StickyNote, glyph, options.text_color),
        AnnotationTool::Stamp => theme::icon::icon(theme::Icon::Stamp, glyph),
        AnnotationTool::Select => theme::icon::icon(theme::Icon::Select, glyph),
    };
    let mut main = button(icon)
        .padding(Padding::from([size * 0.12, size * 0.2]))
        .height(Length::Fixed(size))
        .style(if selected {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        });
    if mode.interactive() {
        main = main.on_press(on(WidgetEvent::Annotate(command)));
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

    // The colour wheel hangs off the tool's own button, so it opens over the
    // control it belongs to and the panel it was reached from is gone.
    let trigger: Element<'static, Message> = if mode.interactive() && wheel == Some(tool) {
        let (red, green, blue) = colour_of(armable, options).rgb();
        crate::vendor::iced_aw::ColorPicker::new(
            true,
            iced::Color::from_rgb(red, green, blue),
            trigger,
            on(WidgetEvent::Annotate(AnnotationCommand::OpenColorWheel(
                None,
            ))),
            move |colour| {
                on(WidgetEvent::Annotate(AnnotationCommand::SetColor(
                    armable,
                    InkColor::from_rgb(colour.r, colour.g, colour.b),
                )))
            },
        )
        .into()
    } else {
        trigger
    };

    // The options panel exists only while its popover is open: building all
    // five closed panels per view pass was widget-tree churn for UI nobody
    // could see.
    let panel = (mode.interactive() && open == Some(tool))
        .then(|| options_panel(tool, armable, options, mode, on));
    super::popover::Popover::new(trigger, panel).into()
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
        // The eraser, the spotlight, the stamp and the selection arrow lay no
        // colour down; they answer with the ink's so the wheel has something
        // to open on.
        AnnotationTool::Spotlight
        | AnnotationTool::Eraser
        | AnnotationTool::Stamp
        | AnnotationTool::Select => options.ink_color,
    }
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
        // Document-mode tools, which the presenter palette never draws an
        // options panel for. The text measures keep the slider well formed.
        AnnotationTool::Note | AnnotationTool::Stamp | AnnotationTool::Select => {
            (options.text_size, (0.008, 0.12))
        }
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

    let mut panel = column![
        row![
            text(armable.label()).size(theme::type_scale::LABEL),
            space::horizontal().width(Length::Fill),
            button(theme::icon::icon(
                theme::Icon::Close,
                theme::type_scale::BODY
            ))
            .padding(2)
            .style(theme::ambient::tool_button)
            .on_press_maybe(
                mode.interactive()
                    .then(|| on(WidgetEvent::Annotate(AnnotationCommand::OpenOptions(None))))
            ),
        ]
        .align_y(Alignment::Center),
        text("Size").size(theme::type_scale::CAPTION),
        size.width(Length::Fill),
    ]
    .spacing(theme::space::S);

    // The pointer control is two things, and which one it is belongs in its
    // own panel: a presenter who wants the spotlight is already looking here
    // for the size of it.
    if tool == AnnotationTool::Pointer {
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
            .padding(Padding::from([4.0, theme::space::S]))
            .style(if chosen {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            });
            if mode.interactive() {
                choice = choice.on_press(on(WidgetEvent::Annotate(
                    AnnotationCommand::SetPointerSpotlight(spotlight),
                )));
            }
            modes = modes.push(choice);
        }
        panel = panel
            .push(text("Mode").size(theme::type_scale::CAPTION))
            .push(modes);
    }

    // The spotlight is a hole in the dimming rather than something drawn, so
    // it has no colour to pick; everything else that lays down colour does.
    if matches!(
        armable,
        AnnotationTool::Ink
            | AnnotationTool::Highlighter
            | AnnotationTool::Pointer
            | AnnotationTool::Text
    ) {
        let selected = match armable {
            AnnotationTool::Ink => options.ink_color,
            AnnotationTool::Highlighter => options.highlight_color,
            AnnotationTool::Text => options.text_color,
            _ => options.pointer_color,
        };
        let mut swatches = Row::new().spacing(theme::space::XS);
        for colour in InkColor::ALL {
            let (r, g, b) = colour.rgb();
            let mut swatch = button(space::horizontal())
                .width(Length::Fixed(28.0))
                .height(Length::Fixed(28.0))
                .padding(0)
                .style(theme::color_swatch_button(
                    iced::Color::from_rgb(r, g, b),
                    selected == colour,
                ));
            if mode.interactive() {
                swatch = swatch.on_press(on(WidgetEvent::Annotate(AnnotationCommand::SetColor(
                    armable, colour,
                ))));
            }
            swatches = swatches.push(swatch);
        }

        // The sixth swatch is the way out of the five: it fills with the mixed
        // colour once there is one, and always carries the pencil — the five
        // fixed swatches are bare, so the pencil is what says "this one is
        // yours to change", whether or not it holds a colour already.
        let (r, g, b) = selected.rgb();
        let fill = iced::Color::from_rgb(r, g, b);
        let pencil = if selected.is_custom() {
            theme::icon::tinted(
                theme::Icon::Pencil,
                theme::type_scale::CAPTION,
                theme::ambient::palette().text_on(fill),
            )
        } else {
            theme::icon::icon(theme::Icon::Pencil, theme::type_scale::CAPTION)
        };
        let face: Element<'static, Message> = container(pencil)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
        let mut mixer = button(face)
            .width(Length::Fixed(28.0))
            .height(Length::Fixed(28.0))
            .padding(0);
        mixer = if selected.is_custom() {
            mixer.style(theme::color_swatch_button(fill, true))
        } else {
            mixer.style(theme::ambient::tool_button)
        };
        if mode.interactive() {
            mixer = mixer.on_press(on(WidgetEvent::Annotate(
                AnnotationCommand::OpenColorWheel(Some(tool)),
            )));
        }
        swatches = swatches.push(
            tooltip(
                mixer,
                text("Mix a colour").size(theme::type_scale::CAPTION),
                tooltip::Position::Top,
            )
            .gap(4)
            .style(theme::ambient::surface),
        );

        panel = panel
            .push(text("Color").size(theme::type_scale::CAPTION))
            .push(swatches);
    }

    container(panel)
        .width(Length::Fixed(232.0))
        .padding(theme::space::M)
        .style(theme::ambient::surface)
        .into()
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
        .style(if selected {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        });
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

/// A tool's icon, drawn in the ink that tool is about to lay down.
fn tool_glyph<'a, Message: 'a>(
    glyph: theme::Icon,
    size: f32,
    colour: InkColor,
) -> Element<'a, Message> {
    let (red, green, blue) = colour.rgb();
    theme::icon::tinted(glyph, size, iced::Color::from_rgb(red, green, blue))
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
        let mut any = false;
        let path = canvas::Path::new(|builder| {
            for run in &selection.runs {
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
        if !any {
            return;
        }
        let (red, green, blue) = selection.color.rgb();
        frame.fill(
            &path,
            canvas::Fill {
                style: canvas::Style::Solid(iced::Color::from_rgba(
                    red,
                    green,
                    blue,
                    selection.opacity,
                )),
                rule: canvas::fill::Rule::NonZero,
            },
        );
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
                    let available_width =
                        ((page.x + page.width).min(panel.width) - rect.x).max(0.0);
                    let available_height =
                        ((page.y + page.height).min(panel.height) - rect.y).max(0.0);
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
