//! The type ladder, as text rather than as numbers.
//!
//! Views ask for a *role* — a title, a heading, prose, a control's label —
//! and get back a `Text` already carrying the size, weight and colour that
//! role has everywhere in the application. Sizing text by hand is how a
//! dialog ends up with a header smaller than its own body, or with three
//! sizes doing one job across four dialogs, so the sizes below are the only
//! place a number is chosen.
//!
//! There are five steps and each has exactly one job:
//!
//! | Role | Step | Used for |
//! | --- | --- | --- |
//! | [`title`] | `TITLE` | The name of a dialog, overlay or page. One per surface, always its first line. |
//! | [`heading`] | `HEADING` | A section within a surface. Never a surface's own name. |
//! | [`body`], [`note`] | `BODY` | Prose. Everything read rather than operated. |
//! | [`label`] | `LABEL` | Text that is part of a control: buttons, field labels, chips. |
//! | [`caption`] | `CAPTION` | Subordinate metadata: fingerprints, hints under a field, timestamps. |
//!
//! Header roles carry weight as well as size, because seventeen points beside
//! fourteen is a difference a reader has to measure rather than see.
//!
//! [`label`] deliberately sets no colour. A button owns its own foreground
//! through its style, across hover, pressed and disabled; a label that
//! painted itself would freeze one of those states over the other three.

use iced::widget::{text, Text};

use super::ambient;
use super::tokens::{font, type_scale};

/// The name of a dialog, overlay or page.
pub fn title<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content)
        .size(type_scale::TITLE)
        .font(font::EMPHASIS)
        .color(ambient::text())
}

/// A section header inside a surface that already has a title.
pub fn heading<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content)
        .size(type_scale::HEADING)
        .font(font::EMPHASIS)
        .color(ambient::text())
}

/// Prose: what a dialog is actually telling someone.
pub fn body<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content).size(type_scale::BODY).color(ambient::text())
}

/// Prose that supports the line above it rather than carrying the message.
/// The same size as [`body`] — a quieter colour, not a smaller one, is what
/// makes it secondary.
pub fn note<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content).size(type_scale::BODY).color(ambient::muted())
}

/// The text inside a control. Colourless on purpose; see the module note.
pub fn label<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content).size(type_scale::LABEL)
}

/// Metadata beside or beneath the thing it describes.
pub fn caption<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content)
        .size(type_scale::CAPTION)
        .color(ambient::muted())
}

/// A short coloured word standing for a state — a toast's intent, a status
/// word beside a row. It reads as a badge rather than as a heading, so it is
/// allowed to be smaller than the prose it introduces; weight and colour are
/// what make it the first thing seen.
pub fn tag<'a>(content: impl text::IntoFragment<'a>, colour: iced::Color) -> Text<'a> {
    text(content)
        .size(type_scale::LABEL)
        .font(font::EMPHASIS)
        .color(colour)
}

/// The label above a control, or above a group of them, in a dialog or form.
///
/// Set at the control's own size, in the primary colour, one weight up. A
/// label smaller and quieter than the field it names reads as a footnote to
/// that field rather than as its name — which is what a muted caption over a
/// text input looks like. Weight, not size, is what lifts it clear.
///
/// This is not [`heading`]: a heading introduces a section of a page, a field
/// label names one control. A page's sections are set in `HEADING`; a
/// dialog's fields are set here.
pub fn field<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content)
        .size(type_scale::LABEL)
        .font(font::EMPHASIS)
        .color(ambient::text())
}
