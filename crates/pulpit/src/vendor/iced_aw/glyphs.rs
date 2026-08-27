//! The labels the picker puts on its own buttons.
//!
//! Upstream these come from `iced_fonts`, which loads an icon font shipped
//! inside `iced_aw` and returns a private-use codepoint for each glyph. This
//! crate draws its icons as SVG rather than as characters — deliberately, so
//! a glyph cannot land at a different weight than the drawing beside it — and
//! carrying a whole font for two symbols to do the opposite would be
//! strange. Words instead, in the interface's own font.
//!
//! The signature is upstream's on purpose: a `(content, font, shaping)`
//! triple, so the call sites in the vendored overlays are untouched and a
//! later upstream fix still applies cleanly.

use iced_core::text::Shaping;
use iced_core::Font;

/// Dismiss without choosing.
pub fn cancel() -> (String, Font, Shaping) {
    label("Cancel")
}

/// Accept what is dialled in.
pub fn ok() -> (String, Font, Shaping) {
    label("OK")
}

fn label(text: &str) -> (String, Font, Shaping) {
    // Advanced shaping, as upstream: these labels are Latin-1 and would
    // shape either way, but the triple is upstream's and a later fix that
    // returns a non-Latin-1 label should not silently draw a box.
    (text.to_string(), Font::DEFAULT, Shaping::Advanced)
}
