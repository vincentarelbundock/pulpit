//! The substrate the worker processes are built on: message framing, spawning,
//! the wake-up doorbell, and shared-memory naming.
//!
//! # Why this is in the domain crate
//!
//! It does not belong here on merit. It is here because it is the only place
//! every consumer can reach: `pulpit-render` and `pulpit-media` are siblings
//! that cannot see each other, and `pulpit` sits above both. Four copies of
//! this code existed before it did, and the copies had already come apart in
//! two ways that mattered — a shared-memory sweep that reclaimed one crate's
//! files and silently skipped the other's, and a fork-bomb marker that one of
//! four spawn sites had quietly stopped setting.
//!
//! The alternative was a sixth published crate for six hundred lines of pipe
//! plumbing, which buys a Cargo boundary nobody consumes.
//!
//! # What this costs, stated plainly
//!
//! The rest of this crate is pure: no clocks, no processes, no files, so the
//! hard cases — a reconnect at a new index, an unequal mirror, a partial
//! write, a stale delayed notification — are ordinary unit tests. This module
//! is none of those things. It spawns children, maps files and blocks on a
//! clock.
//!
//! What that purity actually buys is fast, deterministic domain tests, and
//! that is preserved: **no module outside `ipc` may depend on `ipc`**. The
//! domain is still pure; the crate is not. The visible price is that
//! `cargo test -p pulpit-core` now also touches the filesystem.

pub mod doorbell;
pub mod framing;
pub mod shm;
pub mod worker;

pub use doorbell::{doorbell, Doorbell, Sink, Wakeup};
pub use framing::{read_message, write_message, ProtocolError};
pub use worker::{as_worker, WorkerCommand, WORKER_MARKER};
