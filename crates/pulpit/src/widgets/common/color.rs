//! Choosing a colour, in the one place both toolbars draw it from.
//!
//! The Reader's toolbar and the presenter's annotation palette ask the same
//! question about the same tools — what colour does this pen lay down? — and
//! for a while each answered it with its own copy of the swatches, its own
//! mixer button and its own wheel. Two copies of a control is two versions of
//! the answer: the Reader's copy fell behind and lost the mixer entirely, so
//! a reader could not reach a colour that was not one of the five.
//!
//! What differs between the two is genuinely theirs: the commands they send,
//! the size their toolbar draws at, and which side their tooltips fall on.
//! Those are parameters here, and everything else is shared.

use iced::widget::{button, container, space, Row};
use iced::{Element, Length};

use pulpit_core::annotation::InkColor;

use crate::theme;

/// How a toolbar labels a control, since the two sit at opposite edges of the
/// window and their tooltips fall on opposite sides of it.
pub type Hint<Message> = fn(Element<'static, Message>, &str) -> Element<'static, Message>;

/// One swatch, square, in an options panel.
///
/// One size for both toolbars rather than one each. The swatch row is the
/// widest thing an options panel holds, so it is what sets the panel's width
/// — and two sizes meant the presenter's panel and the Reader's were
/// different widths for no reason a reader could see.
pub const SWATCH: f32 = 24.0;

/// The width the swatch row occupies: every fixed colour, the mixer, and the
/// gaps between them.
///
/// Derived rather than written down, so adding a colour widens the panels
/// that hold it instead of overflowing them.
pub const ROW_WIDTH: f32 =
    (InkColor::ALL.len() + 1) as f32 * SWATCH + InkColor::ALL.len() as f32 * theme::space::XS;

/// The fixed swatches, then the mixer that reaches every other colour.
///
/// The last swatch is the way out of the fixed five: it fills with the mixed
/// colour once there is one, and always carries the pencil — the fixed
/// swatches are bare, so the pencil is what says "this one is yours to
/// change", whether or not it holds a colour already.
///
/// A toolbar that is not `live` draws the controls inert rather than hiding
/// them: a palette that changes shape with the mode teaches two palettes.
pub fn swatches<Message: Clone + 'static>(
    selected: InkColor,
    size: f32,
    live: bool,
    on_pick: impl Fn(InkColor) -> Message,
    on_mix: Message,
    hint: Hint<Message>,
) -> Element<'static, Message> {
    let mut row = Row::new().spacing(theme::space::XS);
    for colour in InkColor::ALL {
        let (red, green, blue) = colour.rgb();
        let swatch = button(space::horizontal())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .padding(0)
            .style(theme::color_swatch_button(
                iced::Color::from_rgb(red, green, blue),
                selected == colour,
            ))
            .on_press_maybe(live.then(|| on_pick(colour)));
        row = row.push(swatch);
    }

    let (red, green, blue) = selected.rgb();
    let fill = iced::Color::from_rgb(red, green, blue);
    let pencil = if selected.is_custom() {
        theme::icon::tinted(
            theme::Icon::Pencil,
            theme::type_scale::CAPTION,
            theme::ambient::palette().text_on(fill),
        )
    } else {
        theme::icon::icon(theme::Icon::Pencil, theme::type_scale::CAPTION)
    };
    let mixer = button(
        container(pencil)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .padding(0)
    .on_press_maybe(live.then_some(on_mix));
    // Styles are distinct closure types, so the choice is made on the button
    // rather than on the style handed to it.
    let mixer = if selected.is_custom() {
        mixer.style(theme::color_swatch_button(fill, true))
    } else {
        mixer.style(theme::ambient::tool_button)
    };

    row.push(hint(mixer.into(), "Mix a colour")).into()
}

/// The wheel, hung off the control whose colour it sets.
///
/// It opens over that control rather than in the middle of the window,
/// because the mark it is about is under the pointer, and it replaces the
/// options panel it was reached from rather than covering it: the panel would
/// be underneath, showing the colour being changed.
pub fn wheel<Message: Clone + 'static>(
    trigger: Element<'static, Message>,
    open: bool,
    selected: InkColor,
    on_cancel: Message,
    on_pick: impl Fn(InkColor) -> Message + 'static,
) -> Element<'static, Message> {
    if !open {
        return trigger;
    }
    let (red, green, blue) = selected.rgb();
    crate::vendor::iced_aw::ColorPicker::new(
        true,
        iced::Color::from_rgb(red, green, blue),
        trigger,
        on_cancel,
        move |colour| on_pick(InkColor::from_rgb(colour.r, colour.g, colour.b)),
    )
    .into()
}
