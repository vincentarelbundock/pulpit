//! A colour picker, vendored from `iced_aw`.
//!
//! Vendored rather than depended on. `iced_aw` is one crate carrying twenty
//! widgets, it is versioned against iced and its `main` branch has already
//! moved to the next iced release — so a dependency on it would put an iced
//! upgrade behind somebody else's release schedule, to get one widget out of
//! twenty. Copying the one costs a one-time port and nothing after that.
//!
//! Provenance is in `README.md` beside this file and the licence text is in
//! `LICENSES/ICED_AW-LICENSE`; what was changed is listed in the README too.
//! Keep edits here to the minimum that makes the code build in this crate:
//! the smaller the diff against upstream, the cheaper it is to take a later
//! fix.

pub mod core;
pub mod glyphs;
pub mod style;
pub mod widget;

pub use widget::color_picker::ColorPicker;
