//! The application's own chrome, as widgets.
//!
//! The menu button and the audience window's Start and Stop were a fixed
//! strip above the layout. They are widgets now, for the same reason
//! everything else is: where a control sits at the lectern is the
//! presenter's decision, not the application's. What they do is unchanged —
//! the strip's code moved here rather than being rewritten.

pub mod view;
