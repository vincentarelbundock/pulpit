//! The reader family: the page surface and the controls around it.
//!
//! Five widgets over one open document — the page, the navigation band, the
//! outline rail, the field inspector and the annotation toolbar. As everywhere
//! else, `model.rs` compiles without Iced because the layout tree, persistence
//! and validation depend on it, and `view.rs` is free to draw.

pub mod model;
pub mod preview;
pub mod view;

/// How many screen pixels one page unit occupies, on each axis.
///
/// `shown` is the page's own size and `drawn` the size it was painted at. A
/// page that reports no size -- one still loading, or a field on a page that
/// has not been measured yet -- scales by one rather than dividing by zero, so
/// a badge lands somewhere the reader can see instead of at infinity.
///
/// Every painter and every flyout placement must agree on this conversion, or
/// a click lands somewhere other than where its target was drawn.
pub(crate) fn page_to_screen(shown: (f32, f32), drawn: (f32, f32)) -> (f32, f32) {
    let axis = |shown: f32, drawn: f32| if shown > 0.0 { drawn / shown } else { 1.0 };
    (axis(shown.0, drawn.0), axis(shown.1, drawn.1))
}
