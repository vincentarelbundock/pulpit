//! The presentation decisions that are the same for every widget.
//!
//! A layout is a choice about *where things go*, not about typography. Every
//! widget already sizes itself to the cell it was given, so a per-widget
//! variant, scale, or alignment adds a knob without adding a
//! capability — and a page of knobs is what made the designer hard to use.
//!
//! The values live here rather than being scattered through the drawing code
//! so that giving any of them back to the presenter later is a matter of
//! reading a field instead of a constant, in one place per token. The
//! configuration types themselves are still carried on every widget and still
//! round-trip through the layout file, so nothing has to be re-invented on
//! that day.

use crate::widgets::common::{Align, Variant};
use crate::widgets::slides::model::SlideFit;

/// Widgets are centred in their cell, horizontally and vertically. A reading
/// that fills the space it was given looks placed; the same reading pinned to
/// a corner looks like a mistake.
pub const ALIGNMENT: Align = Align::Center;

/// How boldly a reading fills its cell. See `common::view::fitted_size`.
pub const VARIANT: Variant = Variant::Standard;

/// A multiplier on the fitted size. One means "as large as fits".
pub const SCALE: f32 = 1.0;

/// Slides are shown whole. Cropping the presenter's own reference copy is not
/// something to offer by accident.
pub const SLIDE_FIT: SlideFit = SlideFit::Fit;

/// The space between the panes of a split, in points.
pub const SPLIT_GAP: f32 = 8.0;

/// The quiet rule centred in a split gutter.
///
/// Cell separation is an internal relationship, not a frame around either
/// widget, so it stays hairline-thin in both the live layout and the editor.
pub const CELL_SEPARATOR_WIDTH: f32 = 1.0;
