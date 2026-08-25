//! Document ownership: debounced watching, failure-safe reload, and the
//! atomic promotion rule that keeps a projector from ever going blank because
//! a build was still running.

// These modules were standalone library crates until the workspace was
// consolidated. They keep their complete, tested APIs — the parts the
// application does not happen to call yet are exercised by the tests beside
// them, and pruning them would throw away working, documented behaviour to
// satisfy a lint about a boundary that no longer exists.
#![allow(dead_code)]

pub mod images;
pub mod manager;
pub mod watcher;

pub use images::{ImageDocumentState, Positions};
pub use manager::{DocumentManager, ReloadPolicy};
pub use watcher::DocumentWatcher;
