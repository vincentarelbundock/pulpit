//! Third-party source carried in this tree rather than depended on.
//!
//! Everything under here is somebody else's code, under somebody else's
//! licence, and is treated as such: each directory keeps a `README.md`
//! recording where it came from, at which version, and every change made to
//! it, while the upstream licence text lives with all the others in
//! `LICENSES/`. `LICENSES/README.md` says what covers what, and is the list a
//! package has to carry.
//!
//! Nothing here is pulpit's house style, and it should not be edited into
//! it. A vendored file is a snapshot of an upstream one; the more it drifts,
//! the harder the next upstream fix is to take.

// Upstream carries pieces this crate does not call — a colour constant, a
// helper for a widget that was not vendored. Silenced rather than deleted:
// deleting them is a diff against upstream to maintain for ever, and the code
// is not this crate's to tidy.
#[allow(dead_code, unused_imports)]
pub mod iced_aw;
