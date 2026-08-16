//! The reader family: the page surface and the controls around it.
//!
//! Five widgets over one open document — the page, the navigation band, the
//! outline rail, the field inspector and the annotation toolbar. As everywhere
//! else, `model.rs` compiles without Iced because the layout tree, persistence
//! and validation depend on it, and `view.rs` is free to draw.

pub mod model;
pub mod view;
