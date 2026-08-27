//! Semantic design tokens.
//!
//! Views use the same seven colour-role names shown in Settings — `canvas`,
//! `surface`, `slide_canvas`, `text`, `muted`, `accent`, and `alert` — never
//! literal colours. Borders, overlays, interaction states, and foregrounds
//! on coloured fills are derived here instead of becoming more roles.
//!
//! Every palette is checked by the tests below against the contrast ratios
//! the design specification requires, so a colour cannot be adjusted into
//! illegibility without a test failing.

use iced::Color;
use serde::{Deserialize, Serialize};

/// The complete, deliberately small colour vocabulary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub canvas: Color,
    pub surface: Color,
    pub slide_canvas: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub alert: Color,
}

/// A role has one spelling in code, settings, and the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorRole {
    Canvas,
    Surface,
    SlideCanvas,
    Text,
    Muted,
    Accent,
    Alert,
}

impl ColorRole {
    pub const ALL: [ColorRole; 7] = [
        ColorRole::Canvas,
        ColorRole::Surface,
        ColorRole::SlideCanvas,
        ColorRole::Text,
        ColorRole::Muted,
        ColorRole::Accent,
        ColorRole::Alert,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ColorRole::Canvas => "Canvas",
            ColorRole::Surface => "Surface",
            ColorRole::SlideCanvas => "Slide canvas",
            ColorRole::Text => "Text",
            ColorRole::Muted => "Muted",
            ColorRole::Accent => "Accent",
            ColorRole::Alert => "Alert",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            ColorRole::Canvas => "Window and page backgrounds",
            ColorRole::Surface => "Panels, menus, dialogs, and reading areas",
            ColorRole::SlideCanvas => "Neutral surround behind slide content",
            ColorRole::Text => "Primary text and icons",
            ColorRole::Muted => "Secondary and disabled information",
            ColorRole::Accent => "Selection, focus, preview, and live state",
            ColorRole::Alert => "Warnings, errors, overtime, and destructive actions",
        }
    }
}

const fn rgb(r: f32, g: f32, b: f32) -> Color {
    Color { r, g, b, a: 1.0 }
}

const fn hex(value: u32) -> Color {
    rgb(
        ((value >> 16) & 0xff) as f32 / 255.0,
        ((value >> 8) & 0xff) as f32 / 255.0,
        (value & 0xff) as f32 / 255.0,
    )
}

/// Radix-style sRGB scales. The light Slate and Indigo rows are the exact
/// opaque values supplied with pulpit's reference palette; dark and Red
/// use their matching Radix scales.
///
/// Keeping the complete scales here makes the step-to-role mapping explicit
/// and leaves the interaction steps available when more chrome moves onto the
/// shared design system. Iced colors are sRGB values, so the package's P3
/// overrides are deliberately not copied.
mod radix {
    use iced::Color;

    use super::hex;

    pub const SLATE_LIGHT: [Color; 12] = [
        hex(0xfcfcfd),
        hex(0xf9f9fb),
        hex(0xeff0f3),
        hex(0xe7e8ec),
        hex(0xe0e1e6),
        hex(0xd8d9e0),
        hex(0xcdced7),
        hex(0xb9bbc6),
        hex(0x8b8d98),
        hex(0x80828d),
        hex(0x62636c),
        hex(0x1e1f24),
    ];

    pub const SLATE_DARK: [Color; 12] = [
        hex(0x111113),
        hex(0x18191b),
        hex(0x212225),
        hex(0x272a2d),
        hex(0x2e3135),
        hex(0x363a3f),
        hex(0x43484e),
        hex(0x5a6169),
        hex(0x696e77),
        hex(0x777b84),
        hex(0xb0b4ba),
        hex(0xedeef0),
    ];

    pub const RED_LIGHT: [Color; 12] = [
        hex(0xfffcfc),
        hex(0xfff7f7),
        hex(0xfeebec),
        hex(0xffdbdc),
        hex(0xffcdce),
        hex(0xfdbdbe),
        hex(0xf4a9aa),
        hex(0xeb8e90),
        hex(0xe5484d),
        hex(0xdc3e42),
        hex(0xce2c31),
        hex(0x641723),
    ];

    pub const RED_DARK: [Color; 12] = [
        hex(0x191111),
        hex(0x201314),
        hex(0x3b1219),
        hex(0x500f1c),
        hex(0x611623),
        hex(0x72232d),
        hex(0x8c333a),
        hex(0xb54548),
        hex(0xe5484d),
        hex(0xec5d5e),
        hex(0xff9592),
        hex(0xffd1d9),
    ];

    pub const INDIGO_LIGHT: [Color; 12] = [
        hex(0xfdfdfe),
        hex(0xf7f9ff),
        hex(0xedf2fe),
        hex(0xdfeaff),
        hex(0xd0dfff),
        hex(0xbdd1ff),
        hex(0xa6bff9),
        hex(0x87a5ef),
        hex(0x3d63dd),
        hex(0x3657c3),
        hex(0x395bc7),
        hex(0x1d2e5c),
    ];

    pub const INDIGO_DARK: [Color; 12] = [
        hex(0x11131f),
        hex(0x141726),
        hex(0x182449),
        hex(0x1d2e62),
        hex(0x253974),
        hex(0x304384),
        hex(0x3a4f97),
        hex(0x435db1),
        hex(0x3d63dd),
        hex(0x5472e4),
        hex(0x9eb1ff),
        hex(0xd6e1ff),
    ];
}

/// The reference "Professional cool" appearance: Radix Slate neutrals,
/// Indigo actions, and Red alerts. Step 1 is the app background, step 2 is a
/// raised surface, steps 11 and 12 are secondary and primary text, and Indigo
/// step 9 is the solid action/focus color.
///
/// Selection, focus and live state read through tone and weight, which leaves
/// the alert red as the only colour on the screen. That is the point: a
/// presenter glancing down sees one coloured thing, and it always means the
/// same thing.
pub const DARK: Palette = Palette {
    canvas: radix::SLATE_DARK[0],
    surface: radix::SLATE_DARK[1],
    slide_canvas: radix::SLATE_DARK[11],
    text: radix::SLATE_DARK[11],
    muted: radix::SLATE_DARK[10],
    accent: radix::INDIGO_DARK[8],
    alert: radix::RED_DARK[10],
};

/// The same Radix vocabulary for a bright room or a light desktop. Surfaces
/// use steps 1–2 and readable chrome uses steps 11–12, so the role names do not
/// change meaning when appearance changes.
pub const LIGHT: Palette = Palette {
    canvas: radix::SLATE_LIGHT[1],
    surface: radix::SLATE_LIGHT[0],
    slide_canvas: radix::SLATE_LIGHT[0],
    text: radix::SLATE_LIGHT[11],
    muted: radix::SLATE_LIGHT[10],
    accent: radix::INDIGO_LIGHT[8],
    alert: radix::RED_LIGHT[10],
};

/// Maximum separation, for a system in high-contrast mode.
pub const HIGH_CONTRAST: Palette = Palette {
    canvas: rgb(0.0, 0.0, 0.0),
    surface: rgb(0.0, 0.0, 0.0),
    slide_canvas: rgb(1.0, 1.0, 1.0),
    text: rgb(1.0, 1.0, 1.0),
    muted: rgb(0.92, 0.92, 0.92),
    accent: rgb(0.0, 0.80, 0.80),
    alert: rgb(1.0, 0.85, 0.0),
};

impl Palette {
    /// A translucent version of a colour, for fills behind text.
    pub fn tinted(colour: Color, alpha: f32) -> Color {
        Color { a: alpha, ..colour }
    }

    pub fn color(self, role: ColorRole) -> Color {
        match role {
            ColorRole::Canvas => self.canvas,
            ColorRole::Surface => self.surface,
            ColorRole::SlideCanvas => self.slide_canvas,
            ColorRole::Text => self.text,
            ColorRole::Muted => self.muted,
            ColorRole::Accent => self.accent,
            ColorRole::Alert => self.alert,
        }
    }

    pub fn with(mut self, role: ColorRole, color: Color) -> Palette {
        match role {
            ColorRole::Canvas => self.canvas = color,
            ColorRole::Surface => self.surface = color,
            ColorRole::SlideCanvas => self.slide_canvas = color,
            ColorRole::Text => self.text = color,
            ColorRole::Muted => self.muted = color,
            ColorRole::Accent => self.accent = color,
            ColorRole::Alert => self.alert = color,
        }
        self
    }

    /// A low-emphasis edge computed from the two roles it separates.
    pub fn border(self) -> Color {
        mix(self.surface, self.text, 0.24)
    }

    /// An edge that must remain visible around a slide or dialog.
    pub fn strong_border(self) -> Color {
        mix(self.surface, self.text, 0.52)
    }

    /// Choose an accessible monochrome foreground for a coloured fill.
    /// Composite a possibly-translucent fill over what sits behind it.
    ///
    /// A tinted fill is not the colour anyone sees; the blend is. Choosing a
    /// text colour against the token rather than the blend is how a disabled
    /// control ends up with a label nobody can read.
    pub fn over(fill: Color, behind: Color) -> Color {
        Color {
            r: fill.r * fill.a + behind.r * (1.0 - fill.a),
            g: fill.g * fill.a + behind.g * (1.0 - fill.a),
            b: fill.b * fill.a + behind.b * (1.0 - fill.a),
            a: 1.0,
        }
    }

    pub fn text_on(self, fill: Color) -> Color {
        let black = rgb(0.0, 0.0, 0.0);
        let white = rgb(1.0, 1.0, 1.0);
        if contrast(black, fill) >= contrast(white, fill) {
            black
        } else {
            white
        }
    }
}

/// Linear blend from `from` toward `to`; `amount` is the distance travelled.
pub(crate) fn mix(from: Color, to: Color, amount: f32) -> Color {
    Color {
        r: from.r + (to.r - from.r) * amount,
        g: from.g + (to.g - from.g) * amount,
        b: from.b + (to.b - from.b) * amount,
        a: from.a + (to.a - from.a) * amount,
    }
}

/// Spacing steps, in logical pixels.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    pub const L: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
}

/// Corner radii.
pub mod radius {
    pub const SMALL: f32 = 4.0;
    pub const MEDIUM: f32 = 8.0;
    pub const DIALOG: f32 = 12.0;
}

/// Type scale, in logical pixels.
///
/// Five steps, each with one job. `theme::typography` turns them into text,
/// and views should ask it for a role rather than reach for a number here;
/// what is left for the constants is sizing something that is not text, such
/// as an icon meant to match the line beside it.
///
/// Chrome only. A widget's own readings — the timer, the clock, the slide
/// counter, the title — are sized to the pane they are given, so a fixed
/// "timer size" would be a number nobody could honour.
pub mod type_scale {
    /// Subordinate metadata: fingerprints, hints under a field, timestamps.
    pub const CAPTION: f32 = 11.0;
    /// Text that is part of a control: buttons, field labels, chips.
    pub const LABEL: f32 = 12.0;
    /// Prose. Everything a person reads rather than operates, and the size a
    /// dialog's own sentences are set in.
    pub const BODY: f32 = 14.0;
    /// A section within a surface: clearly a level above body text, clearly
    /// below the surface's own name. Never a dialog title.
    pub const HEADING: f32 = 17.0;
    /// The name of a dialog, overlay or page. One per surface.
    pub const TITLE: f32 = 22.0;
}

/// Font roles. Application prose and controls deliberately inherit Iced's
/// platform UI font; compact numeric readouts use the bundled DejaVu Sans
/// Mono so columns and changing digits do not jump or vary between machines,
/// without imposing a branded face on the rest of the interface.
pub mod font {
    pub const READOUT: iced::Font = iced::Font::with_name("DejaVu Sans Mono");
    /// The same platform face, one weight up. Titles and section headings
    /// take it so a header reads as a header at a glance, rather than as
    /// body text that happens to be three points larger.
    pub const EMPHASIS: iced::Font = iced::Font {
        weight: iced::font::Weight::Semibold,
        ..iced::Font::DEFAULT
    };
}

/// Hit-target sizes, in logical pixels.
pub mod target {
    /// Every pointer target must be at least this in each dimension.
    pub const MINIMUM: f32 = 32.0;
    /// Live-presentation controls.
    #[allow(dead_code)]
    pub const PRIMARY: f32 = 40.0;
}

/// Relative luminance, per WCAG. Nothing at runtime needs this — it exists so
/// the palettes below can be held to the contrast ratios the design
/// specification requires, by the tests at the bottom of this file.
#[allow(dead_code)]
pub fn luminance(colour: Color) -> f32 {
    fn channel(value: f32) -> f32 {
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(colour.r) + 0.7152 * channel(colour.g) + 0.0722 * channel(colour.b)
}

/// Contrast ratio between two opaque colours, 1.0 to 21.0. Test-only, as above.
#[allow(dead_code)]
pub fn contrast(foreground: Color, background: Color) -> f32 {
    let (a, b) = (luminance(foreground), luminance(background));
    let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palettes() -> [(&'static str, Palette); 3] {
        [
            ("dark", DARK),
            ("light", LIGHT),
            ("high contrast", HIGH_CONTRAST),
        ]
    }

    #[test]
    fn reference_palettes_keep_the_documented_radix_role_mapping() {
        assert_eq!(DARK.canvas, hex(0x111113)); // Slate Dark 1
        assert_eq!(DARK.surface, hex(0x18191b)); // Slate Dark 2
        assert_eq!(DARK.muted, hex(0xb0b4ba)); // Slate Dark 11
        assert_eq!(DARK.text, hex(0xedeef0)); // Slate Dark 12
        assert_eq!(DARK.accent, hex(0x3d63dd)); // Supplied Indigo 9
        assert_eq!(DARK.alert, hex(0xff9592)); // Red Dark 11

        assert_eq!(LIGHT.canvas, hex(0xf9f9fb)); // Slate 2
        assert_eq!(LIGHT.surface, hex(0xfcfcfd)); // Slate 1
        assert_eq!(LIGHT.muted, hex(0x62636c)); // Supplied Slate 11
        assert_eq!(LIGHT.text, hex(0x1e1f24)); // Supplied Slate 12
        assert_eq!(LIGHT.accent, hex(0x3d63dd)); // Supplied Indigo 9
        assert_eq!(LIGHT.alert, hex(0xce2c31)); // Red 11
    }

    #[test]
    fn contrast_maths_matches_known_values() {
        let white = rgb(1.0, 1.0, 1.0);
        let black = rgb(0.0, 0.0, 0.0);
        assert!((contrast(white, black) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 0.001);
    }

    #[test]
    fn body_text_meets_four_and_a_half_to_one_on_every_surface() {
        for (name, palette) in palettes() {
            for (surface_name, surface) in
                [("canvas", palette.canvas), ("surface", palette.surface)]
            {
                let ratio = contrast(palette.text, surface);
                assert!(
                    ratio >= 4.5,
                    "{name}: text on {surface_name} is {ratio:.2}:1, needs 4.5:1"
                );
            }
        }
    }

    #[test]
    fn muted_text_stays_readable() {
        for (name, palette) in palettes() {
            let ratio = contrast(palette.muted, palette.canvas);
            assert!(
                ratio >= 4.5,
                "{name}: muted text is {ratio:.2}:1, needs 4.5:1"
            );
        }
    }

    #[test]
    fn accent_and_alert_meet_three_to_one() {
        for (name, palette) in palettes() {
            for (label, colour) in [
                ("accent", palette.accent),
                ("alert", palette.alert),
                ("strong border", palette.strong_border()),
            ] {
                let ratio = contrast(colour, palette.canvas);
                assert!(
                    ratio >= 3.0,
                    "{name}: {label} is {ratio:.2}:1 against the background, needs 3:1"
                );
            }
        }
    }

    #[test]
    fn text_on_an_accent_fill_is_readable() {
        for (name, palette) in palettes() {
            for (label, fill) in [("accent", palette.accent), ("alert", palette.alert)] {
                let ratio = contrast(palette.text_on(fill), fill);
                assert!(
                    ratio >= 4.5,
                    "{name}: text on {label} is {ratio:.2}:1, needs 4.5:1"
                );
            }
        }
    }

    #[test]
    fn slide_canvases_stay_neutral_and_separate_from_chrome() {
        for (name, palette) in palettes() {
            // Slide content must never merge into controller chrome. On a
            // light palette both are near-white by design, so the separation
            // is allowed to come from the border drawn around the canvas
            // instead of from the fill.
            let by_fill = contrast(palette.slide_canvas, palette.surface);
            let by_border = contrast(palette.strong_border(), palette.slide_canvas);
            assert!(
                by_fill >= 3.0 || by_border >= 3.0,
                "{name}: the slide canvas does not stand out from the panel \
                 (fill {by_fill:.2}:1, border {by_border:.2}:1)"
            );
            // Neutral: no colour cast that would tint a slide.
            let Color { r, g, b, .. } = palette.slide_canvas;
            let spread = r.max(g).max(b) - r.min(g).min(b);
            assert!(spread < 0.05, "{name}: the slide canvas has a colour cast");
        }
    }

    #[test]
    fn the_spacing_scale_is_the_documented_one() {
        assert_eq!(
            [
                space::XS,
                space::S,
                space::M,
                space::L,
                space::XL,
                space::XXL
            ],
            [4.0, 8.0, 12.0, 16.0, 24.0, 32.0]
        );
    }

    #[test]
    fn hit_targets_meet_the_minimum() {
        const { assert!(target::MINIMUM >= 32.0) };
        const { assert!(target::PRIMARY >= target::MINIMUM) };
        const { assert!(target::PRIMARY >= 40.0) };
    }
}

#[cfg(test)]
mod type_scale_tests {
    use super::{font, type_scale};

    /// The ladder is what stops a dialog from setting a header smaller than
    /// its own body. Each step has one job, and the jobs are ordered; a new
    /// step that is not strictly larger than the one below it would give two
    /// roles the same voice and put the choice between them back into the
    /// views.
    #[test]
    fn every_step_is_larger_than_the_one_below_it() {
        let ladder = [
            ("caption", type_scale::CAPTION),
            ("label", type_scale::LABEL),
            ("body", type_scale::BODY),
            ("heading", type_scale::HEADING),
            ("title", type_scale::TITLE),
        ];
        for pair in ladder.windows(2) {
            let (below, above) = (pair[0], pair[1]);
            assert!(
                above.1 > below.1,
                "{} ({}) must be larger than {} ({})",
                above.0,
                above.1,
                below.0,
                below.1,
            );
        }
    }

    /// Headers carry weight as well as size: seventeen points beside fourteen
    /// is a difference a reader measures rather than sees, and the emphasis
    /// face is what makes a heading read as one at a glance.
    #[test]
    fn the_emphasis_face_is_the_platform_face_one_weight_up() {
        assert_eq!(font::EMPHASIS.family, iced::Font::DEFAULT.family);
        assert_ne!(font::EMPHASIS.weight, iced::Font::DEFAULT.weight);
        assert_eq!(font::EMPHASIS.weight, iced::font::Weight::Semibold);
    }
}
