//! Shared tonal, split, segmented, menu, and panel controls.
//!
//! These were once adapters over `material-ui-rs`. The library gave three
//! style functions and a handful of numbers, and asked in exchange that every
//! dialog it drew be generic over *its* theme type — which this application
//! does not use — while pulling five more crates of the iced ecosystem into
//! the upgrade path and tying its widgets to the wgpu backend. The look was
//! worth keeping; the coupling was not, so the recipes live here, written
//! against [`Palette`] like every other style in this module.
//!
//! The numbers below are Material 3's published tokens, named as such so that
//! a reader can check them against the specification rather than against a
//! dependency that is no longer here.

use iced::widget::{button, container};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

use super::tokens::mix;
use super::Palette;

/// Material's own measurements, kept as tokens rather than scattered numbers.
pub const BUTTON_HEIGHT: f32 = 40.0;
pub const MENU_ITEM_HEIGHT: f32 = 48.0;
/// A dialog is this wide and no wider, and holds its content this far in.
pub const DIALOG_MAX_WIDTH: f32 = 560.0;
pub const DIALOG_PADDING: f32 = 24.0;

/// Corner radii. `FULL` is a pill: any radius past half the height rounds to
/// the same shape, and Material says so with a number far larger than any
/// control.
const CORNER_FULL: f32 = 9999.0;
const CORNER_EXTRA_SMALL: f32 = 4.0;

/// How strongly the content colour is laid over a control the pointer is on.
/// Material calls these state layers; they are what makes a hover read as the
/// same control rather than a different one.
const HOVER_LAYER: f32 = 0.08;
const PRESSED_LAYER: f32 = 0.10;
/// What is left of a control that cannot be pressed.
const DISABLED_CONTAINER: f32 = 0.12;
const DISABLED_TEXT: f32 = 0.38;

/// A quiet tonal button, used for
/// the controls that are pressed often and are not the one dangerous action.
pub fn filled_tonal(palette: Palette) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| filled_tonal_style(palette, status)
}

/// The left and right halves of one filled-tonal split button. The shared edge
/// is square so the two read as one control that has been divided.
pub fn split_left(palette: Palette) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let mut style = filled_tonal_style(palette, status);
        style.border.radius.top_right = 0.0;
        style.border.radius.bottom_right = 0.0;
        style
    }
}

pub fn split_right(palette: Palette) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let mut style = filled_tonal_style(palette, status);
        style.border.radius.top_left = 0.0;
        style.border.radius.bottom_left = 0.0;
        style
    }
}

/// A selected tonal button, or its unselected text-button counterpart: one
/// control with two states, so that "this is the current choice" is a fill
/// rather than a mark beside the label.
pub fn selectable(
    palette: Palette,
    selected: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        if selected {
            filled_tonal_style(palette, status)
        } else {
            text_style(palette, status)
        }
    }
}

/// One member of a segmented button. Adjacent segments share their outline,
/// and only the ends are rounded.
pub fn segment(
    palette: Palette,
    selected: bool,
    index: usize,
    len: usize,
) -> impl Fn(&Theme, button::Status) -> button::Style + Copy {
    move |_, status| {
        let container = selected.then(|| secondary_container(palette));
        let content = if selected {
            palette.text
        } else {
            palette.muted
        };
        let background = match status {
            button::Status::Hovered => Some(state_background(container, content, HOVER_LAYER)),
            button::Status::Pressed => Some(state_background(container, content, PRESSED_LAYER)),
            _ => container,
        };

        button::Style {
            background: background.map(Background::Color),
            text_color: match status {
                button::Status::Disabled => faded(palette.text, DISABLED_TEXT),
                _ => content,
            },
            border: Border {
                color: match status {
                    button::Status::Disabled => faded(palette.text, DISABLED_CONTAINER),
                    _ => palette.strong_border(),
                },
                width: 1.0,
                radius: segment_radius(index, len),
            },
            ..button::Style::default()
        }
    }
}

/// The menu and popup surface, with the shadow that lifts it off the page.
pub fn menu_surface(palette: Palette) -> impl Fn(&Theme) -> container::Style + Copy {
    move |_| container::Style {
        background: Some(Background::Color(surface_container(palette))),
        text_color: Some(palette.text),
        border: Border {
            radius: CORNER_EXTRA_SMALL.into(),
            ..Border::default()
        },
        // Material elevation level 2.
        shadow: elevation(2),
        ..container::Style::default()
    }
}

/// The tonal fill: the accent mixed into the surface far enough to read as a
/// control, not far enough to read as the one thing on the screen.
fn secondary_container(palette: Palette) -> Color {
    mix(palette.surface, palette.accent, 0.16)
}

/// Material's surface-container tone, which a menu sits on.
fn surface_container(palette: Palette) -> Color {
    mix(palette.surface, palette.text, 0.03)
}

fn filled_tonal_style(palette: Palette, status: button::Status) -> button::Style {
    let background = secondary_container(palette);
    let foreground = palette.text;

    let style = button::Style {
        background: Some(Background::Color(background)),
        text_color: foreground,
        border: Border {
            radius: super::radius::SMALL.into(),
            ..Border::default()
        },
        ..button::Style::default()
    };

    match status {
        button::Status::Active | button::Status::Pressed => style,
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(mix(background, foreground, HOVER_LAYER))),
            // Material lifts a tonal button by one level under the pointer.
            shadow: elevation(1),
            ..style
        },
        button::Status::Disabled => button::Style {
            background: Some(Background::Color(faded(foreground, DISABLED_CONTAINER))),
            text_color: faded(foreground, DISABLED_TEXT),
            ..style
        },
    }
}

/// The text button: no fill until the pointer is on it, which is what makes a
/// row of them read as a list rather than as a wall of controls.
fn text_style(palette: Palette, status: button::Status) -> button::Style {
    let foreground = palette.text;
    let style = button::Style {
        background: None,
        text_color: foreground,
        border: Border {
            radius: super::radius::SMALL.into(),
            ..Border::default()
        },
        ..button::Style::default()
    };

    match status {
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(faded(foreground, HOVER_LAYER))),
            ..style
        },
        button::Status::Disabled => button::Style {
            text_color: faded(foreground, DISABLED_TEXT),
            ..style
        },
        _ => style,
    }
}

/// A state layer over whatever is already there: over a fill it darkens it,
/// over nothing it is the faintest wash of the content colour.
fn state_background(container: Option<Color>, content: Color, opacity: f32) -> Color {
    match container {
        Some(color) => mix(color, content, opacity),
        None => faded(content, opacity),
    }
}

fn segment_radius(index: usize, len: usize) -> iced::border::Radius {
    let full = CORNER_FULL;
    match (index, len) {
        (_, 0 | 1) => full.into(),
        (0, _) => iced::border::Radius {
            top_left: full,
            bottom_left: full,
            top_right: 0.0,
            bottom_right: 0.0,
        },
        (index, len) if index + 1 == len => iced::border::Radius {
            top_left: 0.0,
            bottom_left: 0.0,
            top_right: full,
            bottom_right: full,
        },
        _ => 0.0.into(),
    }
}

/// Material's elevation shadows, by level. Only the ambient layer: the key
/// light's contribution is a second shadow iced's containers cannot draw, and
/// at these sizes it is the ambient one that reads.
fn elevation(level: u8) -> Shadow {
    let (y, blur) = match level {
        0 => (0.0, 0.0),
        1 => (1.0, 3.0),
        2 => (2.0, 6.0),
        3 => (4.0, 8.0),
        4 => (6.0, 10.0),
        _ => (8.0, 12.0),
    };
    Shadow {
        color: Color {
            a: 0.15,
            ..Color::BLACK
        },
        offset: Vector::new(0.0, y),
        blur_radius: blur,
    }
}

fn faded(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason the styles are written against the palette rather than
    /// against a theme of their own: a control has to stay readable in both.
    #[test]
    fn control_labels_remain_readable_in_dark_and_light_modes() {
        for palette in [super::super::tokens::DARK, super::super::tokens::LIGHT] {
            for status in [
                button::Status::Active,
                button::Status::Hovered,
                button::Status::Pressed,
            ] {
                let tonal = filled_tonal_style(palette, status);
                let Some(Background::Color(background)) = tonal.background else {
                    panic!("a tonal button must have a solid fill");
                };
                assert!(
                    super::super::tokens::contrast(tonal.text_color, background) >= 4.5,
                    "tonal label lost contrast in {status:?}"
                );

                let plain = text_style(palette, status);
                let behind = match plain.background {
                    Some(Background::Color(color)) => Palette::over(color, palette.canvas),
                    _ => palette.canvas,
                };
                let ratio = super::super::tokens::contrast(plain.text_color, behind);
                assert!(
                    ratio >= 4.5,
                    "plain label lost contrast in {status:?}: {ratio:.2}:1"
                );
            }
        }
    }

    #[test]
    fn a_split_button_is_square_where_its_halves_meet() {
        let palette = super::super::tokens::DARK;
        let left = split_left(palette)(&Theme::Dark, button::Status::Active);
        let right = split_right(palette)(&Theme::Dark, button::Status::Active);
        assert_eq!(left.border.radius.top_right, 0.0);
        assert_eq!(left.border.radius.bottom_right, 0.0);
        assert_eq!(right.border.radius.top_left, 0.0);
        assert_eq!(right.border.radius.bottom_left, 0.0);
        assert!(
            left.border.radius.top_left > 0.0,
            "the outer edge stays round"
        );
    }

    #[test]
    fn only_the_ends_of_a_segmented_button_are_rounded() {
        let first = segment_radius(0, 3);
        let middle = segment_radius(1, 3);
        let last = segment_radius(2, 3);
        assert!(first.top_left > 0.0 && first.top_right == 0.0);
        assert_eq!(middle.top_left, 0.0);
        assert_eq!(middle.top_right, 0.0);
        assert!(last.top_right > 0.0 && last.top_left == 0.0);
        // A lone segment is a button, and is round on both ends.
        assert!(segment_radius(0, 1).top_left > 0.0);
    }
}
