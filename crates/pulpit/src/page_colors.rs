//! Colour modes for rendered pages (issue #17).
//!
//! A white page read at night, and a white deck thrown at a bright hall, are
//! both worse than they need to be. The transform runs in the application, on
//! the pixels a frame already delivered: the cache keeps the frame as the
//! renderer made it, and a mode change re-mints image handles from the cached
//! bytes rather than re-rendering anything. That is what keeps the mode out
//! of `FrameKey` and off the workers, and what lets it change under the
//! reader's hand for the price of a memcpy per resident frame.

/// How a page's pixels are recoloured on their way to the screen.
///
/// Marks the presenter draws live in canvas layers over the picture and keep
/// their own colours; anything the renderer baked into the frame — a
/// document's own annotations, committed ink on the reader path — is
/// recoloured with the page. That is deliberate for the first and accepted
/// for the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// The page as the document says it is.
    #[default]
    Normal,
    /// Every colour channel flipped. Wrecks photographs, saves eyes; the
    /// gentler luma variants can join later without disturbing this one.
    Inverted,
    /// White multiplied down to the chosen paper colour, so the page reads
    /// like print on stock instead of a lit panel. Black stays black.
    Paper,
}

/// The default paper: a warm off-white, the sepia most readers reach for.
pub const DEFAULT_PAPER: [u8; 3] = [0xF4, 0xEC, 0xD8];
pub const DEFAULT_PAPER_HEX: &str = "#F4ECD8";

impl ColorMode {
    /// The next mode round the loop. One key cycles all three, because the
    /// reason to want a different one — the lights just changed — arrives
    /// suddenly, and pressing on always comes back round to normal.
    pub fn next(self) -> Self {
        match self {
            ColorMode::Normal => ColorMode::Inverted,
            ColorMode::Inverted => ColorMode::Paper,
            ColorMode::Paper => ColorMode::Normal,
        }
    }
}

/// One colour through the same transform the page pixels get, so a surface
/// that stands in for a page — the sheet before its frame arrives, the
/// surround beside a page narrower than its window — matches what the pages
/// themselves look like instead of shining light grey into an inverted read.
pub fn recolor(mode: ColorMode, paper: [u8; 3], color: iced::Color) -> iced::Color {
    let mut pixel =
        [color.r, color.g, color.b].map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8);
    let mut rgba = [pixel[0], pixel[1], pixel[2], 255];
    apply(mode, paper, &mut rgba);
    pixel = [rgba[0], rgba[1], rgba[2]];
    iced::Color {
        r: f32::from(pixel[0]) / 255.0,
        g: f32::from(pixel[1]) / 255.0,
        b: f32::from(pixel[2]) / 255.0,
        a: color.a,
    }
}

/// Recolour tightly packed RGBA8 in place. Alpha is untouched.
///
/// Paper is a per-channel multiply in the same 8.8 fixed point the wash
/// compositor uses; `(v · (c + 1)) >> 8` maps 255 to exactly `c` and 0 to 0,
/// so white becomes the paper and ink stays ink.
pub fn apply(mode: ColorMode, paper: [u8; 3], pixels: &mut [u8]) {
    match mode {
        ColorMode::Normal => {}
        ColorMode::Inverted => {
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[0] = 255 - pixel[0];
                pixel[1] = 255 - pixel[1];
                pixel[2] = 255 - pixel[2];
            }
        }
        ColorMode::Paper => {
            let factor = paper.map(|channel| u32::from(channel) + 1);
            for pixel in pixels.as_chunks_mut::<4>().0 {
                for channel in 0..3 {
                    pixel[channel] = ((u32::from(pixel[channel]) * factor[channel]) >> 8) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_leaves_pixels_alone() {
        let mut pixels = vec![10, 20, 30, 40, 250, 251, 252, 253];
        let original = pixels.clone();
        apply(ColorMode::Normal, DEFAULT_PAPER, &mut pixels);
        assert_eq!(pixels, original);
    }

    #[test]
    fn inverting_twice_is_the_identity_and_leaves_alpha_alone() {
        let mut pixels = vec![0, 128, 255, 7, 33, 66, 99, 200];
        let original = pixels.clone();
        apply(ColorMode::Inverted, DEFAULT_PAPER, &mut pixels);
        assert_eq!(pixels[3], 7, "alpha must not be inverted");
        assert_eq!(pixels[0], 255);
        apply(ColorMode::Inverted, DEFAULT_PAPER, &mut pixels);
        assert_eq!(pixels, original);
    }

    #[test]
    fn paper_maps_white_to_the_paper_and_keeps_black_black() {
        let paper = [0xF4, 0xEC, 0xD8];
        let mut pixels = vec![255, 255, 255, 255, 0, 0, 0, 255];
        apply(ColorMode::Paper, paper, &mut pixels);
        assert_eq!(&pixels[0..4], &[0xF4, 0xEC, 0xD8, 255]);
        assert_eq!(&pixels[4..8], &[0, 0, 0, 255]);
    }

    #[test]
    fn the_cycle_visits_every_mode_and_comes_home() {
        let mut mode = ColorMode::Normal;
        let mut seen = Vec::new();
        for _ in 0..3 {
            mode = mode.next();
            seen.push(mode);
        }
        assert_eq!(
            seen,
            vec![ColorMode::Inverted, ColorMode::Paper, ColorMode::Normal]
        );
    }
}
