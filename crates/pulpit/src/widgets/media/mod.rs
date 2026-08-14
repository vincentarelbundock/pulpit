//! The media transport: play, pause and scrub the media on the current slide.
//!
//! It exists because the presenter and the audience consume the same overlay
//! frames, so a control drawn inside the content is a control the room sees.
//! These controls live on the presenter's layout instead, and reach the
//! content through the media protocol.

pub mod model;
pub mod view;
