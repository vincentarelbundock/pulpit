//! Settings and session persistence (§79.4): marking settings dirty and
//! flushing them to disk on a helper thread, and the throttled
//! crash-recovery session snapshot.
//!
//! No `App` fields move here — `settings_dirty`, `settings_writer`,
//! `session_throttle` and the rest stay in app.rs — the same shape as
//! the other `app::*` extractions.

use std::time::Instant;

use super::App;

impl App {
    /// Mark the settings changed. The write happens from the tick, at most
    /// every couple of seconds, and unconditionally on quit — a keystroke in
    /// the colour editor must not cost a TOML serialise and an fsync.
    pub(super) fn persist(&mut self) {
        self.settings_dirty = true;
    }

    /// Write the settings out if they changed. The write itself — TOML,
    /// temp file, fsync, rename — happens on a helper thread: durability
    /// discipline is worth keeping, paying for it on the UI thread is not.
    /// Writes are throttled to seconds apart, so two can never race.
    pub(super) fn flush_settings(&mut self) {
        if !std::mem::take(&mut self.settings_dirty) {
            return;
        }
        let store = self.store.clone();
        let settings = self.settings.clone();
        self.settings_writer = Some(std::thread::spawn(move || {
            if let Err(e) = store.save(&settings) {
                tracing::warn!(error = %e, "cannot save settings");
            }
        }));
    }

    /// Write the crash-recovery snapshot, at most once per interval and only
    /// when something actually changed.
    ///
    /// Nothing is written while startup still holds an unapplied restore:
    /// overwriting the snapshot with a fresh, empty session would silently
    /// destroy the state being recovered.
    pub(super) fn save_session(&mut self, now: Instant) {
        if self.pending_restore.is_some() || !self.session_throttle.due(now) {
            return;
        }
        // Fingerprinting is a metadata syscall — milliseconds on a network
        // mount — so it happens when the document (generation) changes, not
        // on every periodic save. External edits reach us through the file
        // watcher as a new generation anyway.
        let generation = self.state.generation();
        let document_path = self.state.document().map(|document| document.path.clone());
        let fingerprint = match (&self.session_fingerprint, &document_path) {
            (Some((cached, path, fingerprint)), Some(current))
                if *cached == generation && path == current =>
            {
                fingerprint.clone()
            }
            (_, None) => None,
            (_, Some(current)) => {
                let fingerprint = crate::session::fingerprint(current);
                self.session_fingerprint = Some((generation, current.clone(), fingerprint.clone()));
                fingerprint
            }
        };
        let snapshot = crate::session::SessionSnapshot::capture(
            &self.state,
            Some(self.active_layout.id.0.clone()),
            &self.coordinator.roles,
            fingerprint,
            now,
        );
        if !snapshot.is_worth_offering() {
            return;
        }
        if self
            .last_session
            .as_ref()
            .is_some_and(|last| last.matches_content(&snapshot))
        {
            return;
        }
        // The durable write happens off the UI thread; the snapshot is
        // remembered optimistically. A failed write logs from the helper
        // and the next interval retries, which is exactly what the retry
        // would have been anyway. Writes are seconds apart, so two cannot
        // race.
        let session = self.session.clone();
        let to_write = snapshot.clone();
        self.session_writer = Some(std::thread::spawn(move || {
            if let Err(e) = session.save(&to_write) {
                tracing::warn!(error = %e, "cannot save the session snapshot");
            }
        }));
        self.last_session = Some(snapshot);
    }
}
