//! A colour picker and a time picker, vendored from `iced_aw`.
//!
//! Vendored rather than depended on. `iced_aw` is one crate carrying twenty
//! widgets, it is versioned against iced and its `main` branch has already
//! moved to the next iced release — so a dependency on it would put an iced
//! upgrade behind somebody else's release schedule, to get two widgets out of
//! twenty. Copying the two costs a one-time port and nothing after that.
//!
//! Provenance and the licence are in `README.md` and `LICENSE` beside this
//! file; what was changed is listed there too. Keep edits here to the
//! minimum that makes the code build in this crate: the smaller the diff
//! against upstream, the cheaper it is to take a later fix.

pub mod core;
pub mod glyphs;
pub mod style;
pub mod widget;

pub use widget::color_picker::ColorPicker;
