//! The theme layer: semantic tokens, and the widget styles built from them.
//!
//! No view contains a literal colour, radius, or spacing number. Styles are
//! built from a [`Palette`] chosen at runtime from the appearance setting and
//! the system preference, so switching to light or high contrast changes
//! every surface at once and cannot miss one.
//!
//! Each interactive style defines active, hovered, pressed, focused, disabled
//! and selected appearances, and focus is always distinct from selection.

pub mod controls;
pub mod icon;
pub mod tokens;

pub use icon::Icon;

use iced::widget::{button, container, overlay::menu, pick_list, scrollable, text_input};
use iced::{Background, Border, Color, Theme};

use crate::layout::CellBackground;
use crate::platform::appearance::Resolved;
pub use tokens::{font, radius, space, target, type_scale, ColorRole, Palette};

/// The palette in use, plus how it was chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeState {
    pub palette: Palette,
    pub resolved: Resolved,
    /// True when the system preference could not be read and Dark was used.
    pub fell_back: bool,
}

impl Default for ThemeState {
    fn default() -> Self {
        ThemeState {
            palette: tokens::DARK,
            resolved: Resolved::Dark,
            fell_back: false,
        }
    }
}

impl ThemeState {
    pub fn new(
        resolved: Resolved,
        fell_back: bool,
        colors: &crate::settings::ColorSettings,
    ) -> ThemeState {
        let palette = match resolved {
            Resolved::Dark => colors.palette(crate::settings::ColorScheme::Dark),
            Resolved::Light => colors.palette(crate::settings::ColorScheme::Light),
            Resolved::HighContrast => tokens::HIGH_CONTRAST,
        };
        ThemeState {
            palette,
            resolved,
            fell_back,
        }
    }
}

/// The Iced theme, so built-in widgets (sliders, checkboxes, text inputs)
/// pick up the same palette as our own styles.
pub fn iced_theme(palette: Palette) -> Theme {
    Theme::custom(
        "pulpit".to_string(),
        iced::theme::Palette {
            background: palette.canvas,
            text: palette.text,
            primary: palette.accent,
            success: palette.accent,
            warning: palette.alert,
            danger: palette.alert,
        },
    )
}

// ------------------------------------------------------------- containers

pub fn surface(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(palette.surface)),
        border: Border {
            color: palette.border(),
            width: 1.0,
            radius: radius::MEDIUM.into(),
        },
        text_color: Some(palette.text),
        ..container::Style::default()
    }
}

/// A bordered colour sample.
///
/// Every colour the interface shows is one the presenter can now change —
/// Settings opens a wheel on its swatches, the annotation palette on its
/// sixth — so there is no longer such a thing as a swatch that is only to be
/// looked at, and this is the one way of drawing one.
pub fn color_swatch_button(
    color: Color,
    selected: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let border = if selected || matches!(status, button::Status::Hovered) {
            ambient::text()
        } else {
            ambient::border()
        };
        button::Style {
            background: Some(color.into()),
            border: Border {
                color: border,
                width: if selected { 3.0 } else { 1.0 },
                radius: radius::SMALL.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A drop target in the designer. Only the zone under the pointer is filled,
/// so where the widget will land is never a guess.
pub fn drop_zone(palette: Palette, active: bool) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: active.then(|| Background::Color(Palette::tinted(palette.accent, 0.35))),
        border: Border {
            color: if active {
                palette.accent
            } else {
                Color::TRANSPARENT
            },
            width: if active { 2.0 } else { 0.0 },
            radius: radius::SMALL.into(),
        },
        ..container::Style::default()
    }
}

/// The canvas a slide preview sits on: black bars, same as the live panel, so
/// a light theme does not put white margins around a page.
pub fn slide_letterbox(_palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Color::BLACK.into()),
        ..container::Style::default()
    }
}

/// A layout cell, from its own properties plus editor state.
pub fn cell_style(
    palette: Palette,
    background: CellBackground,
    selected: bool,
    highlighted: bool,
) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| {
        let fill = match background {
            CellBackground::None => Color::TRANSPARENT,
            CellBackground::Panel => palette.surface,
            CellBackground::Canvas => palette.slide_canvas,
        };
        // Selection and drop-target highlighting look the same on purpose:
        // both mean "this cell is what your next action affects".
        let (colour, width) = if selected || highlighted {
            (palette.accent, 2.0)
        } else {
            // Passive cells are never framed. Their split owns the one muted
            // rule between neighbours, so nested layouts cannot accumulate
            // double edges or draw a box around the outside.
            (Color::TRANSPARENT, 0.0)
        };
        container::Style {
            background: Some(Background::Color(fill)),
            border: Border {
                color: colour,
                width,
                radius: radius::SMALL.into(),
            },
            ..container::Style::default()
        }
    }
}

pub fn canvas(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(palette.canvas)),
        border: Border {
            color: palette.strong_border(),
            width: 1.0,
            radius: radius::SMALL.into(),
        },
        ..container::Style::default()
    }
}

pub fn empty_cell(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(Palette::tinted(palette.surface, 0.4))),
        text_color: Some(palette.muted),
        ..container::Style::default()
    }
}

pub fn divider(palette: Palette, active: bool) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(if active {
            palette.accent
        } else {
            palette.border()
        })),
        border: Border {
            radius: 2.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

pub fn dialog(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(palette.surface)),
        border: Border {
            color: palette.strong_border(),
            width: 1.0,
            radius: radius::DIALOG.into(),
        },
        text_color: Some(palette.text),
        ..container::Style::default()
    }
}

/// A compact keyboard token: quieter than a button, but distinct from prose.
pub fn keycap(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(palette.canvas)),
        border: Border {
            color: palette.border(),
            width: 1.0,
            radius: radius::SMALL.into(),
        },
        text_color: Some(palette.text),
        ..container::Style::default()
    }
}

/// A drop-down, in this application's colours rather than the library's.
///
/// Iced styles an unstyled `pick_list` from its own built-in theme, whose
/// primary is an indigo that belongs to no palette here — so the one drop-down
/// on the settings page arrived wearing a colour nothing else uses. This says
/// what every other control on the page already says: surface, one hairline,
/// and the accent only where the pointer is.
pub fn drop_down(
    palette: Palette,
) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style + Copy {
    move |_, status| {
        let border = match status {
            pick_list::Status::Hovered | pick_list::Status::Opened { .. } => palette.accent,
            pick_list::Status::Active => palette.strong_border(),
        };
        pick_list::Style {
            background: Background::Color(palette.surface),
            text_color: palette.text,
            placeholder_color: palette.muted,
            handle_color: palette.muted,
            border: Border {
                color: border,
                width: 1.0,
                radius: radius::SMALL.into(),
            },
        }
    }
}

/// The list that drops out of one, in the same colours as the panels do.
pub fn drop_down_menu(palette: Palette) -> impl Fn(&Theme) -> menu::Style + Copy {
    move |_| menu::Style {
        background: Background::Color(palette.surface),
        text_color: palette.text,
        // The row under the pointer takes the accent the same way a tool
        // button does, so picking from a list feels like pressing one.
        selected_background: Background::Color(Palette::tinted(palette.accent, 0.24)),
        selected_text_color: palette.text,
        border: Border {
            color: palette.strong_border(),
            width: 1.0,
            radius: radius::MEDIUM.into(),
        },
        // The border carries the edge; a list that floats over the page needs
        // no drop shadow to say it is above it.
        shadow: iced::Shadow::default(),
    }
}

/// The shared editable-field recipe: surface fill, quiet edge at rest, and
/// the accent reserved for keyboard focus.
pub fn text_field(
    palette: Palette,
) -> impl Fn(&Theme, text_input::Status) -> text_input::Style + Copy {
    move |_, status| text_input::Style {
        background: Background::Color(palette.surface),
        border: Border {
            color: match status {
                text_input::Status::Focused { .. } => palette.accent,
                text_input::Status::Hovered => palette.strong_border(),
                text_input::Status::Disabled | text_input::Status::Active => palette.border(),
            },
            width: if matches!(status, text_input::Status::Focused { .. }) {
                2.0
            } else {
                1.0
            },
            radius: radius::SMALL.into(),
        },
        icon: palette.muted,
        placeholder: palette.muted,
        value: if matches!(status, text_input::Status::Disabled) {
            palette.muted
        } else {
            palette.text
        },
        selection: Palette::tinted(palette.accent, 0.32),
    }
}

/// Quiet at rest, clearer under the pointer, and accented only while dragged.
pub fn scrollbar(
    palette: Palette,
) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style + Copy {
    move |theme, status| {
        let mut style = scrollable::default(theme, status);
        let colour = match status {
            scrollable::Status::Dragged { .. } => palette.accent,
            scrollable::Status::Hovered { .. } => palette.strong_border(),
            scrollable::Status::Active { .. } => palette.border(),
        };
        let rail = scrollable::Rail {
            background: None,
            border: Border::default(),
            scroller: scrollable::Scroller {
                background: Background::Color(colour),
                border: Border {
                    radius: radius::SMALL.into(),
                    ..Border::default()
                },
            },
        };
        style.vertical_rail = rail;
        style.horizontal_rail = rail;
        style
    }
}

pub fn scrim(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(Palette::tinted(palette.canvas, 0.78))),
        ..container::Style::default()
    }
}

/// A toast: intent-coloured border, opaque surface, readable text.
pub fn toast(palette: Palette, intent: Color) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(palette.surface)),
        border: Border {
            color: intent,
            width: 1.0,
            radius: radius::MEDIUM.into(),
        },
        text_color: Some(palette.text),
        shadow: iced::Shadow {
            color: Palette::tinted(palette.canvas, 0.35),
            offset: iced::Vector::new(0.0, 2.0),
            blur_radius: 12.0,
        },
        ..container::Style::default()
    }
}

pub fn notice(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(Palette::tinted(palette.alert, 0.14))),
        border: Border {
            color: palette.alert,
            width: 1.0,
            radius: radius::SMALL.into(),
        },
        text_color: Some(palette.text),
        ..container::Style::default()
    }
}

/// The slim accent line beside the presentation title.
pub fn accent_rule(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(palette.accent)),
        border: Border {
            radius: 2.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

pub fn separator(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(palette.border())),
        ..container::Style::default()
    }
}

/// A status dot, used with a text label so colour is never the only cue.
pub fn dot(colour: Color) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(colour)),
        border: Border {
            radius: 5.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

// ---------------------------------------------------------------- buttons

/// The navigation buttons.
///
/// Back and forward are the same colour: they are two halves of one control,
/// and the presenter reaching for them in a dark room is aiming by position,
/// not by hue. Forward keeps its greater width, which is the emphasis that
/// matters when you are not looking at the screen.
pub fn forward_button(palette: Palette) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let base = palette.accent;
        let background = match status {
            button::Status::Hovered => lighten(base, 0.08),
            button::Status::Pressed => darken(base, 0.08),
            button::Status::Disabled => Palette::tinted(base, 0.4),
            button::Status::Active => base,
        };
        button::Style {
            background: Some(Background::Color(background)),
            // Against what is actually drawn, not against the token: the
            // disabled fill is translucent, and reading it as opaque accent
            // put an unreadable label on a control that says "not now".
            text_color: palette.text_on(Palette::over(background, palette.canvas)),
            border: Border {
                color: if matches!(status, button::Status::Hovered) {
                    palette.text
                } else {
                    Color::TRANSPARENT
                },
                width: 2.0,
                radius: radius::SMALL.into(),
            },
            ..button::Style::default()
        }
    }
}

/// Back is deliberately quieter than Forward: it remains fully visible and
/// equally sized, but does not spend the one primary-action accent.
pub fn back_button(palette: Palette) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let background = match status {
            button::Status::Hovered => Palette::tinted(palette.accent, 0.14),
            button::Status::Pressed => Palette::tinted(palette.accent, 0.24),
            button::Status::Disabled => Palette::tinted(palette.surface, 0.45),
            button::Status::Active => palette.surface,
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: if matches!(status, button::Status::Disabled) {
                palette.muted
            } else {
                palette.text
            },
            border: Border {
                color: palette.strong_border(),
                width: 1.0,
                radius: radius::SMALL.into(),
            },
            ..button::Style::default()
        }
    }
}

/// Ordinary commands: menu entries, toolbar buttons, list rows.
pub fn tool_button(palette: Palette) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    // No outline at rest. A menu or a toolbar is a list of words; boxing each
    // one draws a grid the reader has to look past. The pointer's own
    // position is the affordance, so the fill appears on hover and the
    // outline only when it carries information — pressed, or focused.
    move |_, status| {
        let (background, border, text) = match status {
            button::Status::Hovered => (
                Palette::tinted(palette.accent, 0.14),
                Color::TRANSPARENT,
                palette.text,
            ),
            button::Status::Pressed => (
                Palette::tinted(palette.accent, 0.24),
                palette.accent,
                palette.text,
            ),
            button::Status::Disabled => (Color::TRANSPARENT, Color::TRANSPARENT, palette.muted),
            button::Status::Active => (Color::TRANSPARENT, Color::TRANSPARENT, palette.text),
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: text,
            border: Border {
                color: border,
                width: 1.0,
                radius: radius::SMALL.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A selected toggle. Distinct from focus, which uses the focus ring.
pub fn selected_button(
    palette: Palette,
) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let intensity = match status {
            button::Status::Hovered | button::Status::Pressed => 0.32,
            button::Status::Disabled => 0.12,
            button::Status::Active => 0.22,
        };
        button::Style {
            background: Some(Background::Color(Palette::tinted(
                palette.accent,
                intensity,
            ))),
            text_color: match status {
                button::Status::Disabled => palette.muted,
                _ => palette.accent,
            },
            // The accent fill already says "this is the one"; an outline as
            // well is the same fact twice.
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: radius::SMALL.into(),
            },
            ..button::Style::default()
        }
    }
}

/// Keyboard focus inside a composite panel. Unlike selection, this is a
/// transient navigation affordance and therefore keeps the normal text color
/// while drawing a persistent accent edge.
pub fn focus_button(palette: Palette) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let intensity = match status {
            button::Status::Hovered | button::Status::Pressed => 0.22,
            button::Status::Disabled => 0.06,
            button::Status::Active => 0.12,
        };
        button::Style {
            background: Some(Background::Color(Palette::tinted(
                palette.accent,
                intensity,
            ))),
            text_color: match status {
                button::Status::Disabled => palette.muted,
                _ => palette.text,
            },
            border: Border {
                color: palette.accent,
                width: 1.0,
                radius: radius::SMALL.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A destructive action: reachable, never adjacent-by-accident.
pub fn alert_button(palette: Palette) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let intensity = match status {
            button::Status::Hovered | button::Status::Pressed => 0.3,
            button::Status::Disabled => 0.08,
            button::Status::Active => 0.16,
        };
        button::Style {
            background: Some(Background::Color(Palette::tinted(palette.alert, intensity))),
            text_color: match status {
                button::Status::Disabled => palette.muted,
                _ => palette.alert,
            },
            border: Border {
                color: if matches!(status, button::Status::Disabled) {
                    palette.border()
                } else {
                    palette.alert
                },
                width: 1.0,
                radius: radius::SMALL.into(),
            },
            ..button::Style::default()
        }
    }
}

fn lighten(colour: Color, amount: f32) -> Color {
    Color {
        r: (colour.r + amount).min(1.0),
        g: (colour.g + amount).min(1.0),
        b: (colour.b + amount).min(1.0),
        a: colour.a,
    }
}

fn darken(colour: Color, amount: f32) -> Color {
    Color {
        r: (colour.r - amount).max(0.0),
        g: (colour.g - amount).max(0.0),
        b: (colour.b - amount).max(0.0),
        a: colour.a,
    }
}

#[cfg(test)]
mod interaction_state_tests {
    use super::*;
    use crate::theme::tokens::{contrast, DARK, HIGH_CONTRAST, LIGHT};

    fn palettes() -> [(&'static str, Palette); 3] {
        [
            ("dark", DARK),
            ("light", LIGHT),
            ("high contrast", HIGH_CONTRAST),
        ]
    }

    const STATES: [(&str, button::Status); 4] = [
        ("active", button::Status::Active),
        ("hovered", button::Status::Hovered),
        ("pressed", button::Status::Pressed),
        ("disabled", button::Status::Disabled),
    ];

    /// What a reader actually sees behind the label: the styles use tinted
    /// accents, so testing the token would pass while the pixels failed.
    fn fill_of(style: &button::Style, behind: Color) -> Color {
        match style.background {
            Some(Background::Color(colour)) => Palette::over(colour, behind),
            _ => behind,
        }
    }

    #[test]
    fn every_button_state_keeps_its_label_readable() {
        // A control whose label vanishes on hover is unusable exactly when it
        // is being used, and high contrast is the palette where a tinted fill
        // is most likely to swallow it.
        for (name, palette) in palettes() {
            let tool = tool_button(palette);
            let forward = forward_button(palette);
            // The shape Iced takes for a button's style, named so the array
            // below reads as the pair of buttons it is.
            type StyleOf<'a> = &'a dyn Fn(&Theme, button::Status) -> button::Style;
            let styles: [(&str, StyleOf); 2] = [("tool", &tool), ("forward", &forward)];
            for (label, style_of) in styles {
                for (state, status) in STATES {
                    let style = style_of(&Theme::Dark, status);
                    let fill = fill_of(&style, palette.canvas);
                    let ratio = contrast(style.text_color, fill);
                    // Disabled text is deliberately quiet, but must still be
                    // legible enough to read what is unavailable.
                    let required = if state == "disabled" { 3.0 } else { 4.5 };
                    assert!(
                        ratio >= required,
                        "{name}/{label}/{state}: label is {ratio:.2}:1 on its fill, needs {required}:1"
                    );
                }
            }
        }
    }

    #[test]
    fn hover_and_press_are_distinguishable_from_rest_and_from_each_other() {
        // Two states that look identical are one state as far as a user is
        // concerned, which is the whole point of having them.
        for (name, palette) in palettes() {
            let style_of = tool_button(palette);
            let fill = |status| fill_of(&style_of(&Theme::Dark, status), palette.canvas);
            let rest = fill(button::Status::Active);
            let hovered = fill(button::Status::Hovered);
            let pressed = fill(button::Status::Pressed);

            for (label, a, b) in [
                ("rest and hover", rest, hovered),
                ("hover and press", hovered, pressed),
            ] {
                let difference = (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs();
                assert!(
                    difference > 0.02,
                    "{name}: {label} differ by only {difference:.3}"
                );
            }
        }
    }

    #[test]
    fn a_pressed_control_carries_an_outline_that_can_be_seen() {
        // The fill alone is a tint; the outline is what survives a
        // high-contrast palette and a projector's gamma.
        for (name, palette) in palettes() {
            let style = tool_button(palette)(&Theme::Dark, button::Status::Pressed);
            assert!(style.border.width > 0.0, "{name}: no pressed outline");
            let ratio = contrast(style.border.color, palette.canvas);
            assert!(
                ratio >= 3.0,
                "{name}: pressed outline is {ratio:.2}:1, needs 3:1"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::appearance::{Appearance, SystemAppearance};
    use crate::theme::tokens::contrast;

    #[test]
    fn every_appearance_resolves_to_a_palette_with_a_reason() {
        let state = ThemeState::new(
            SystemAppearance::Unknown.resolve(Appearance::System),
            SystemAppearance::Unknown.fell_back(Appearance::System),
            &crate::settings::ColorSettings::default(),
        );
        assert_eq!(state.resolved, Resolved::Dark);
        assert!(state.fell_back, "an undetectable system is recorded");

        let light = ThemeState::new(
            SystemAppearance::Light.resolve(Appearance::System),
            false,
            &crate::settings::ColorSettings::default(),
        );
        assert_eq!(light.palette, tokens::LIGHT);
    }

    #[test]
    fn button_states_are_all_distinct_where_it_matters() {
        for palette in [tokens::DARK, tokens::LIGHT, tokens::HIGH_CONTRAST] {
            let style = tool_button(palette);
            let theme = iced_theme(palette);
            let active = style(&theme, button::Status::Active);
            let hovered = style(&theme, button::Status::Hovered);
            let disabled = style(&theme, button::Status::Disabled);

            assert_ne!(active.background, hovered.background, "hover is invisible");
            assert_ne!(
                active.text_color, disabled.text_color,
                "a disabled control looks enabled"
            );
            // A disabled control still has to be readable enough to explain
            // itself.
            assert!(contrast(disabled.text_color, palette.canvas) >= 2.5);
        }
    }

    #[test]
    fn the_forward_button_keeps_readable_text_in_every_state() {
        for palette in [tokens::DARK, tokens::LIGHT, tokens::HIGH_CONTRAST] {
            let theme = iced_theme(palette);
            let style = forward_button(palette)(&theme, button::Status::Active);
            let Some(Background::Color(background)) = style.background else {
                panic!("the forward button must have a solid fill");
            };
            assert!(
                contrast(style.text_color, background) >= 4.5,
                "forward-button text is unreadable in this palette"
            );
        }
    }

    #[test]
    fn alert_stays_distinct_from_the_accent() {
        for palette in [tokens::DARK, tokens::LIGHT, tokens::HIGH_CONTRAST] {
            assert_ne!(palette.accent, palette.alert);
        }
    }

    #[test]
    fn fields_share_geometry_and_reserve_accent_for_focus() {
        for palette in [tokens::DARK, tokens::LIGHT, tokens::HIGH_CONTRAST] {
            let theme = iced_theme(palette);
            let active = text_field(palette)(&theme, text_input::Status::Active);
            let focused =
                text_field(palette)(&theme, text_input::Status::Focused { is_hovered: false });
            assert_eq!(active.border.radius.top_left, radius::SMALL);
            assert_eq!(active.border.width, 1.0);
            assert_ne!(active.border.color, palette.accent);
            assert_eq!(focused.border.color, palette.accent);
            assert_eq!(focused.border.width, 2.0);
        }
    }

    #[test]
    fn scrollbar_strength_tracks_rest_hover_and_drag() {
        let palette = tokens::DARK;
        let theme = iced_theme(palette);
        let style = scrollbar(palette);
        let active = style(
            &theme,
            scrollable::Status::Active {
                is_horizontal_scrollbar_disabled: true,
                is_vertical_scrollbar_disabled: false,
            },
        );
        let dragged = style(
            &theme,
            scrollable::Status::Dragged {
                is_horizontal_scrollbar_dragged: false,
                is_vertical_scrollbar_dragged: true,
                is_horizontal_scrollbar_disabled: true,
                is_vertical_scrollbar_disabled: false,
            },
        );
        assert_eq!(active.vertical_rail.background, None);
        assert_ne!(
            active.vertical_rail.scroller.background,
            Background::Color(palette.accent)
        );
        assert_eq!(
            dragged.vertical_rail.scroller.background,
            Background::Color(palette.accent)
        );
    }

    #[test]
    fn passive_layout_cells_never_draw_a_perimeter_border() {
        for palette in [tokens::DARK, tokens::LIGHT, tokens::HIGH_CONTRAST] {
            let theme = iced_theme(palette);
            for background in CellBackground::ALL {
                let style = cell_style(palette, background, false, false)(&theme);
                assert_eq!(
                    style.border.width, 0.0,
                    "{background:?} cells must be separated by gutters, not framed"
                );
            }
        }
    }

    #[test]
    fn views_do_not_construct_their_own_colours() {
        let themed_views = [
            ("designer", include_str!("../designer_view.rs")),
            ("layout renderer", include_str!("../layout_renderer.rs")),
            ("common widgets", include_str!("../widgets/common/view.rs")),
            (
                "navigation widgets",
                include_str!("../widgets/navigation/view.rs"),
            ),
            ("notes widgets", include_str!("../widgets/notes/view.rs")),
            ("slide widgets", include_str!("../widgets/slides/view.rs")),
            ("status widgets", include_str!("../widgets/status/view.rs")),
            ("timing widgets", include_str!("../widgets/timing/view.rs")),
        ];
        for (name, source) in themed_views {
            for forbidden in ["Color::", "Color { r:", "from_rgb", "from_rgba"] {
                assert!(
                    !source.contains(forbidden),
                    "{name} bypasses the seven color roles with {forbidden}"
                );
            }
        }

        // The shell contains the audience output as well as application
        // chrome. Exact black and white blanking are its only literal-color
        // exception; PDF/image pixels are data rather than theme colors.
        let shell = include_str!("../view.rs");
        // Counted as literal colour *constructions*, not as any identifier
        // ending in `Color::` — `BlankColor::White` names a setting, not a
        // colour the shell mixed for itself.
        let constructed = shell.matches("Color::WHITE").count()
            + shell.matches("Color::BLACK").count()
            + shell.matches("Color::TRANSPARENT").count();
        assert_eq!(constructed, 2);
        assert!(shell.contains("Blank::White => Color::WHITE"));
        assert!(shell.contains("_ => Color::BLACK"));
        assert!(!shell.contains("from_rgb"));
        assert!(!shell.contains("Color { r:"));
    }

    #[test]
    fn views_use_tokens_for_non_optical_type_spacing_and_padding() {
        let views = [
            ("shell", include_str!("../view.rs")),
            ("designer", include_str!("../designer_view.rs")),
            ("common", include_str!("../widgets/common/view.rs")),
            ("document", include_str!("../widgets/document/view.rs")),
            ("navigation", include_str!("../widgets/navigation/view.rs")),
            ("notes", include_str!("../widgets/notes/view.rs")),
            ("search", include_str!("../widgets/search/view.rs")),
            ("slides", include_str!("../widgets/slides/view.rs")),
            ("status", include_str!("../widgets/status/view.rs")),
            ("timing", include_str!("../widgets/timing/view.rs")),
        ];
        for (name, source) in views {
            for method in [".size(", ".spacing(", ".padding("] {
                for suffix in source.split(method).skip(1) {
                    let literal: String = suffix
                        .chars()
                        .take_while(|character| character.is_ascii_digit() || *character == '.')
                        .collect();
                    if literal.is_empty() {
                        continue;
                    }
                    let value: f32 = literal.parse().expect("a numeric style literal");
                    assert!(
                        value <= 2.0,
                        "{name} uses {method}{literal}); use a design token unless this is 0–2px optical geometry"
                    );
                }
            }
        }
    }

    #[test]
    fn views_draw_icons_rather_than_typing_them() {
        // A glyph in a `text` widget is whatever the system font decides it
        // is: a different weight and a different optical size than the Lucide
        // drawing beside it, and on a machine missing the character, a
        // tofu box. Every one of these has an [`icon::Icon`] instead.
        let typed = [
            '✕', '✖', '×', '☰', '↺', '↻', '▾', '▴', '✎', '←', '→', '↑', '↓', '‹', '›', '▲', '▼',
            '✓',
        ];
        let views = [
            ("shell", include_str!("../view.rs")),
            ("designer", include_str!("../designer_view.rs")),
            (
                "annotation widgets",
                include_str!("../widgets/annotations/view.rs"),
            ),
            ("common widgets", include_str!("../widgets/common/view.rs")),
            (
                "navigation widgets",
                include_str!("../widgets/navigation/view.rs"),
            ),
            ("notes widgets", include_str!("../widgets/notes/view.rs")),
            ("slide widgets", include_str!("../widgets/slides/view.rs")),
            ("status widgets", include_str!("../widgets/status/view.rs")),
            ("timing widgets", include_str!("../widgets/timing/view.rs")),
        ];
        for (name, source) in views {
            for line in source.lines() {
                // Comments and test fixtures may say "200×112.5"; only a
                // string handed to a widget is a glyph the user would see.
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                for glyph in typed {
                    assert!(
                        !line.contains(glyph),
                        "{name} draws {glyph} as text; use theme::Icon instead\n  {trimmed}"
                    );
                }
            }
        }
    }
}

/// The palette of the view pass currently being built.
///
/// Iced builds a window's view on one thread, and every widget in that pass
/// belongs to the same window and therefore the same palette. Rather than
/// thread a `Palette` through several hundred call sites — where it would be
/// the same value every time — the view entry point sets it once with
/// [`ambient::set`] and the helpers below read it.
///
/// The explicit builders above remain the real implementation and are what
/// the tests exercise; this module is a convenience over them, not a second
/// source of truth.
pub mod ambient {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static CURRENT: Cell<Palette> = const { Cell::new(tokens::DARK) };
    }

    /// Set the palette for this view pass. Called once, at the top of it.
    pub fn set(palette: Palette) {
        CURRENT.with(|current| current.set(palette));
    }

    pub fn palette() -> Palette {
        CURRENT.with(|current| current.get())
    }

    // Colours, named by meaning.
    pub fn text() -> Color {
        palette().text
    }
    pub fn muted() -> Color {
        palette().muted
    }
    pub fn accent() -> Color {
        palette().accent
    }
    pub fn alert() -> Color {
        palette().alert
    }
    pub fn border() -> Color {
        palette().border()
    }

    // Styles, usable directly as Iced style callbacks.
    pub fn surface(theme: &Theme) -> container::Style {
        super::surface(palette())(theme)
    }
    pub fn slide_letterbox(theme: &Theme) -> container::Style {
        super::slide_letterbox(palette())(theme)
    }
    pub fn drop_zone(active: bool) -> impl Fn(&Theme) -> container::Style + Copy {
        super::drop_zone(palette(), active)
    }
    pub fn canvas(theme: &Theme) -> container::Style {
        super::canvas(palette())(theme)
    }
    pub fn empty_cell(theme: &Theme) -> container::Style {
        super::empty_cell(palette())(theme)
    }
    pub fn dialog(theme: &Theme) -> container::Style {
        super::dialog(palette())(theme)
    }
    pub fn keycap(theme: &Theme) -> container::Style {
        super::keycap(palette())(theme)
    }
    pub fn drop_down(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
        super::drop_down(palette())(theme, status)
    }
    pub fn text_field(theme: &Theme, status: text_input::Status) -> text_input::Style {
        super::text_field(palette())(theme, status)
    }
    pub fn scrollbar(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
        super::scrollbar(palette())(theme, status)
    }
    pub fn drop_down_menu(theme: &Theme) -> menu::Style {
        super::drop_down_menu(palette())(theme)
    }
    pub fn scrim(theme: &Theme) -> container::Style {
        super::scrim(palette())(theme)
    }
    pub fn notice(theme: &Theme) -> container::Style {
        super::notice(palette())(theme)
    }
    pub fn accent_rule(theme: &Theme) -> container::Style {
        super::accent_rule(palette())(theme)
    }
    pub fn separator(theme: &Theme) -> container::Style {
        super::separator(palette())(theme)
    }
    pub fn cell_style(
        background: CellBackground,
        selected: bool,
        highlighted: bool,
    ) -> impl Fn(&Theme) -> container::Style + Copy {
        super::cell_style(palette(), background, selected, highlighted)
    }
    pub fn divider(active: bool) -> impl Fn(&Theme) -> container::Style + Copy {
        super::divider(palette(), active)
    }
    pub fn toast(intent: Color) -> impl Fn(&Theme) -> container::Style + Copy {
        super::toast(palette(), intent)
    }
    pub fn dot(colour: Color) -> impl Fn(&Theme) -> container::Style + Copy {
        super::dot(colour)
    }

    pub fn forward_button(theme: &Theme, status: button::Status) -> button::Style {
        super::forward_button(palette())(theme, status)
    }
    pub fn back_button(theme: &Theme, status: button::Status) -> button::Style {
        super::back_button(palette())(theme, status)
    }
    pub fn tool_button(theme: &Theme, status: button::Status) -> button::Style {
        super::tool_button(palette())(theme, status)
    }
    pub fn selected_button(theme: &Theme, status: button::Status) -> button::Style {
        super::selected_button(palette())(theme, status)
    }
    pub fn focus_button(theme: &Theme, status: button::Status) -> button::Style {
        super::focus_button(palette())(theme, status)
    }
    pub fn alert_button(theme: &Theme, status: button::Status) -> button::Style {
        super::alert_button(palette())(theme, status)
    }
}
