//! Presenter layouts: the tree model, the widget vocabulary, validation,
//! persistence and the built-in designs.
//!
//! Everything here is pure: no UI types, no rendering, no clock. The layout
//! designer and the presenter view are both projections of these values, which
//! is what lets every structural rule — canonical flattening, instance limits,
//! resize behaviour, validation, import migration — be tested without a
//! display.

// These modules were standalone library crates until the workspace was
// consolidated. They keep their complete, tested APIs — the parts the
// application does not happen to call yet are exercised by the tests beside
// them, and pruning them would throw away working, documented behaviour to
// satisfy a lint about a boundary that no longer exists.
#![allow(dead_code)]

pub mod builtin;
pub mod fit;
pub mod history;
pub mod model;
pub mod panels;
pub mod store;
pub mod thumbnail;
pub mod tree;
pub mod validate;

pub use history::History;
pub use model::{AspectRatio, Change, Layout, LayoutId, Origin, Side};
pub use store::LayoutStore;
pub use tree::{Cell, CellBackground, Direction, Divider, EmptyBehavior, Frame, Node, NodeId};
pub use validate::{validate, Issue, Severity};
