//! How big a box a text mark needs, in page points.
//!
//! A `/FreeText` annotation is a *box*, and its `/Rect` is the whole of it:
//! the appearance stream is clipped to that rectangle, the resize grips sit on
//! it, and a rubber band has to enclose it to pick the mark up (§8.4). Only
//! the words inside it are drawn, so a box guessed larger than its text is
//! empty space the reader cannot see and cannot aim at, and one guessed
//! smaller silently clips what they typed.
//!
//! Both failures come from guessing, so this measures instead. The mark is set
//! in Helvetica — the face `/DA` names, and one of the fourteen every
//! conforming viewer has without embedding — and Helvetica's advance widths
//! are a fixed table, so the width of a line is arithmetic rather than a
//! rendering question. That keeps the measurement here, in the pure crate,
//! beside the geometry it decides.

/// Helvetica advance widths for the printable ASCII range, in 1/1000 em, as
/// the Adobe Core 14 metrics give them. Indexed from `0x20`.
///
/// The table stops at `~` because that is where the encoding stops being one
/// character to one glyph: everything above it depends on the encoding the
/// appearance is written in, and is charged at [`FALLBACK_WIDTH`] per byte
/// instead.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278,
    278, // ' ' – '/'
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, // '0' – '9'
    278, 278, 584, 584, 584, 556, 1015, // ':' – '@'
    667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667,
    611, 722, 667, 944, 667, 667, 611, // 'A' – 'Z'
    278, 278, 278, 469, 556, 222, // '[' – '`'
    556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500,
    278, 556, 500, 722, 500, 500, 500, // 'a' – 'z'
    334, 260, 334, 584, // '{' – '~'
];

/// What one byte of anything outside printable ASCII is charged, in 1/1000 em.
///
/// The width of Helvetica's lowercase letters, which is the commonest width in
/// the face and the middle of the Latin-1 range. Charged *per UTF-8 byte*
/// rather than per character, because the appearance stream is written as
/// bytes and a viewer draws one glyph for each of them.
const FALLBACK_WIDTH: u16 = 556;

/// The baseline-to-baseline distance, as a multiple of the font size.
///
/// The leading the appearance writer sets lines at, and the leading the
/// editor's own box was sized for, so what is typed and what is drawn agree.
pub const LEADING: f32 = 1.2;

/// How far below the last baseline a descender reaches, as a multiple of the
/// font size. Helvetica's is 0.212; the extra is slack, because a box that
/// ends exactly on the descender clips it the moment a viewer rounds.
const DESCENT: f32 = 0.25;

/// Slack past the last glyph, as a multiple of the font size.
///
/// The text is drawn from the box's left edge, so the last character ends
/// exactly on the right edge of a box measured to the character. That is
/// where a viewer's own rounding takes a pixel off it.
const TRAILING: f32 = 0.1;

/// The smallest a measured box may come out, in page points, so a box holding
/// one narrow letter is still something the reader can see and grab.
const MIN_SIDE: f32 = 8.0;

/// The width of one line set in Helvetica at `font_size`, in page points.
pub fn line_width(line: &str, font_size: f32) -> f32 {
    let thousandths: u32 = line
        .chars()
        .map(|character| match character {
            ' '..='~' => u32::from(HELVETICA_WIDTHS[character as usize - 0x20]),
            other => u32::from(FALLBACK_WIDTH) * other.len_utf8() as u32,
        })
        .sum();
    thousandths as f32 / 1000.0 * font_size.max(0.0)
}

/// The box `text` needs, in page points: as wide as its widest line and as
/// tall as the lines it has.
///
/// Blank lines count towards the height. The appearance writer draws nothing
/// for them but still advances past them, so a mark with a gap in it is as
/// tall as the gap.
pub fn fit(text: &str, font_size: f32) -> (f32, f32) {
    let font_size = font_size.max(0.0);
    let lines = text.lines().count().max(1);
    let width = text
        .lines()
        .map(|line| line_width(line, font_size))
        .fold(0.0f32, f32::max)
        + font_size * TRAILING;
    let height = font_size * (LEADING * lines as f32 + DESCENT);
    (width.max(MIN_SIDE), height.max(MIN_SIDE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_measured_from_the_faces_own_widths() {
        // Helvetica: 'H' is 722/1000 em and 'i' is 222, so "Hi" at 10 points
        // is 9.44 points of glyph.
        assert!((line_width("Hi", 10.0) - 9.44).abs() < 0.001);
        assert_eq!(line_width("", 12.0), 0.0);
        // A space is a glyph like any other: text is not trimmed, because the
        // appearance draws what was typed.
        assert!(line_width(" ", 12.0) > 0.0);
    }

    #[test]
    fn a_box_is_as_wide_as_its_widest_line_and_as_tall_as_its_lines() {
        let (narrow, one_line) = fit("i", 12.0);
        let (wide, two_lines) = fit("i\nWWWWWWWWWW", 12.0);
        assert!(wide > narrow, "the long line decides the width");
        assert!(
            two_lines > one_line,
            "the second line decides the height: {two_lines} > {one_line}"
        );
        // One line is a line of leading plus the descender under it.
        assert!((one_line - 12.0 * (LEADING + 0.25)).abs() < 0.001);
    }

    #[test]
    fn a_blank_line_still_takes_its_height() {
        let (_, gapped) = fit("above\n\nbelow", 12.0);
        let (_, packed) = fit("above\nbelow", 12.0);
        assert!(gapped > packed, "the gap is part of the mark");
    }

    #[test]
    fn the_box_is_never_wide_enough_to_clip_what_it_holds() {
        // The property that matters: the appearance draws from the left edge,
        // so the box must reach past the last glyph of every line.
        for text in [
            "a",
            "Wg",
            "the quick brown fox",
            "MMMMMMMM",
            "one\ntwo three four",
        ] {
            let (width, _) = fit(text, 11.0);
            for line in text.lines() {
                assert!(
                    width > line_width(line, 11.0),
                    "{text:?} is clipped: {width} <= {}",
                    line_width(line, 11.0)
                );
            }
        }
    }

    #[test]
    fn a_box_is_never_too_small_to_find() {
        let (width, height) = fit("", 1.0);
        assert!(width >= MIN_SIDE && height >= MIN_SIDE);
    }

    #[test]
    fn text_outside_the_table_is_charged_rather_than_ignored() {
        // Not in the width table, and not free either: a mark whose text was
        // not measured would be a mark clipped to nothing.
        assert!(line_width("é", 12.0) > 0.0);
        assert!(line_width("日本語", 12.0) > line_width("é", 12.0));
    }
}
