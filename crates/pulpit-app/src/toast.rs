//! Toasts.
//!
//! Status must not eat the presenter's screen. Notices appear as small
//! overlays in the corner of the **presenter** window — never the audience
//! window — and routine ones fade by themselves.
//!
//! A toast is never the only record of a failure: everything shown here is
//! also written to the diagnostics bundle, and anything presentation-critical
//! stays on screen until the presenter dismisses it, so a message cannot be
//! missed while they are looking at the projector.

use std::time::{Duration, Instant};

/// How much attention a message deserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Something worked, or is merely informative.
    Info,
    /// Something the presenter should know but which is not blocking.
    Warning,
    /// A failure that affects the presentation.
    Error,
}

impl Intent {
    /// How long a toast of this kind stays up. Errors do not expire: a
    /// presentation-critical failure needs a durable notice.
    pub fn lifetime(self) -> Option<Duration> {
        match self {
            Intent::Info => Some(Duration::from_secs(4)),
            Intent::Warning => Some(Duration::from_secs(8)),
            Intent::Error => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Intent::Info => "Info",
            Intent::Warning => "Warning",
            Intent::Error => "Problem",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub intent: Intent,
    /// What happened, in one line.
    pub message: String,
    /// What the presenter can do about it, when there is something.
    pub action: Option<String>,
    pub expires_at: Option<Instant>,
    /// Set when this toast reports a standing condition rather than an event.
    /// The key identifies the condition so it can be updated or withdrawn.
    pub condition: Option<&'static str>,
}

impl Toast {
    pub fn is_sticky(&self) -> bool {
        self.expires_at.is_none()
    }
}

/// The live toast stack.
#[derive(Debug, Default)]
pub struct Toasts {
    toasts: Vec<Toast>,
    next_id: u64,
    /// Conditions the presenter has dismissed, kept until they clear.
    dismissed_conditions: Vec<String>,
}

/// More than this on screen at once and they stop being glanceable.
const MAX_VISIBLE: usize = 4;

impl Toasts {
    pub fn new() -> Toasts {
        Toasts::default()
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.toasts.iter()
    }

    /// Show a message. Identical messages are not stacked: a display that
    /// reconnects three times should not produce three identical toasts.
    pub fn push(
        &mut self,
        intent: Intent,
        message: impl Into<String>,
        action: Option<String>,
        now: Instant,
    ) -> u64 {
        let message = message.into();
        if let Some(existing) = self
            .toasts
            .iter_mut()
            .find(|toast| toast.message == message && toast.intent == intent)
        {
            // Refresh it instead, so a repeat keeps it visible rather than
            // piling up.
            existing.expires_at = intent.lifetime().map(|life| now + life);
            return existing.id;
        }

        self.next_id += 1;
        let id = self.next_id;
        self.toasts.push(Toast {
            id,
            intent,
            message,
            action,
            expires_at: intent.lifetime().map(|life| now + life),
            condition: None,
        });

        // Oldest expiring toasts go first; a sticky error is never pushed out
        // by routine chatter.
        while self.toasts.len() > MAX_VISIBLE {
            let victim = self
                .toasts
                .iter()
                .position(|toast| !toast.is_sticky())
                .unwrap_or(0);
            self.toasts.remove(victim);
        }
        id
    }

    pub fn info(&mut self, message: impl Into<String>, now: Instant) -> u64 {
        self.push(Intent::Info, message, None, now)
    }

    pub fn warning(&mut self, message: impl Into<String>, now: Instant) -> u64 {
        self.push(Intent::Warning, message, None, now)
    }

    pub fn error(
        &mut self,
        message: impl Into<String>,
        action: Option<String>,
        now: Instant,
    ) -> u64 {
        self.push(Intent::Error, message, action, now)
    }

    /// Report the conditions that are true *right now*.
    ///
    /// Standing conditions — no second display, a saved display missing, a
    /// compositor that will not place windows — are not events, so they are
    /// not given a lifetime: the notice is up exactly while the condition
    /// holds, and withdraws itself when the presenter fixes it. Conditions
    /// the presenter has dismissed by hand stay dismissed until they clear
    /// and recur, so a notice can be acknowledged without it coming back on
    /// the next reconciliation.
    pub fn set_conditions(&mut self, conditions: &[(&'static str, String, Option<String>)]) {
        let live: Vec<&'static str> = conditions.iter().map(|(key, _, _)| *key).collect();

        self.toasts
            .retain(|toast| toast.condition.is_none_or(|key| live.contains(&key)));
        self.dismissed_conditions
            .retain(|key| live.contains(&key.as_str()));

        for (key, message, action) in conditions {
            if self.dismissed_conditions.iter().any(|seen| seen == key) {
                continue;
            }
            match self
                .toasts
                .iter_mut()
                .find(|toast| toast.condition == Some(*key))
            {
                // Still true, but the wording may have changed (a different
                // count of candidate displays, say).
                Some(existing) => {
                    existing.message.clone_from(message);
                    existing.action.clone_from(action);
                }
                None => {
                    self.next_id += 1;
                    self.toasts.push(Toast {
                        id: self.next_id,
                        intent: Intent::Warning,
                        message: message.clone(),
                        action: action.clone(),
                        expires_at: None,
                        condition: Some(key),
                    });
                }
            }
        }
    }

    pub fn dismiss(&mut self, id: u64) {
        if let Some(key) = self
            .toasts
            .iter()
            .find(|toast| toast.id == id)
            .and_then(|toast| toast.condition)
        {
            self.dismissed_conditions.push(key.to_string());
        }
        self.toasts.retain(|toast| toast.id != id);
    }

    pub fn dismiss_all(&mut self) {
        for toast in &self.toasts {
            if let Some(key) = toast.condition {
                self.dismissed_conditions.push(key.to_string());
            }
        }
        self.toasts.clear();
    }

    /// Drop whatever has expired. Called from the update loop.
    pub fn tick(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts
            .retain(|toast| toast.expires_at.is_none_or(|expiry| expiry > now));
        before != self.toasts.len()
    }

    /// How many need dismissing by hand.
    pub fn sticky_count(&self) -> usize {
        self.toasts.iter().filter(|toast| toast.is_sticky()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> Instant {
        // A fixed base so the arithmetic in these tests is exact.
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_secs(seconds)
    }

    #[test]
    fn routine_messages_expire_on_their_own() {
        let mut toasts = Toasts::new();
        toasts.info("Reloaded the deck", at(0));
        assert_eq!(toasts.iter().count(), 1);

        assert!(!toasts.tick(at(1)), "not yet");
        assert!(toasts.tick(at(10)), "gone");
        assert!(toasts.is_empty());
    }

    #[test]
    fn a_failure_stays_until_it_is_dismissed() {
        let mut toasts = Toasts::new();
        let id = toasts.error(
            "The renderer stopped",
            Some("Rendering restarted automatically".into()),
            at(0),
        );
        toasts.tick(at(10_000));
        assert_eq!(toasts.iter().count(), 1, "a critical failure is durable");
        assert_eq!(toasts.sticky_count(), 1);

        toasts.dismiss(id);
        assert!(toasts.is_empty());
    }

    #[test]
    fn a_repeated_message_refreshes_rather_than_stacking() {
        let mut toasts = Toasts::new();
        let first = toasts.warning("No audience display", at(0));
        let second = toasts.warning("No audience display", at(3));
        assert_eq!(first, second, "the same toast was refreshed");
        assert_eq!(toasts.iter().count(), 1);

        // Refreshed at 3s with an 8s life, so it is still up at 10s.
        toasts.tick(at(10));
        assert_eq!(toasts.iter().count(), 1);
        toasts.tick(at(12));
        assert!(toasts.is_empty());
    }

    #[test]
    fn routine_chatter_never_pushes_out_a_failure() {
        let mut toasts = Toasts::new();
        toasts.error("The projector vanished", None, at(0));
        for index in 0..10 {
            toasts.info(format!("Rendered page {index}"), at(0));
        }
        assert!(toasts.iter().count() <= 4, "the stack stays glanceable");
        assert_eq!(toasts.sticky_count(), 1, "the failure is still there");
        assert!(toasts
            .iter()
            .any(|toast| toast.message.contains("projector")));
    }

    #[test]
    fn an_error_carries_what_to_do_next() {
        let mut toasts = Toasts::new();
        toasts.error(
            "Could not open the document",
            Some("Check the file still exists, then press F5".into()),
            at(0),
        );
        let toast = toasts.iter().next().unwrap();
        assert_eq!(toast.intent, Intent::Error);
        assert!(toast.action.as_ref().unwrap().contains("F5"));
    }

    fn condition(key: &'static str, message: &str) -> (&'static str, String, Option<String>) {
        (key, message.to_string(), None)
    }

    #[test]
    fn a_standing_condition_does_not_fade_away() {
        let mut toasts = Toasts::new();
        toasts.set_conditions(&[condition(
            "no-secondary-display",
            "no separate audience display is connected",
        )]);

        // The presenter looks up an hour into the talk: it is still there,
        // because the projector is still not plugged in.
        toasts.tick(at(3_600));
        assert_eq!(toasts.iter().count(), 1);
        assert!(toasts.iter().next().unwrap().condition.is_some());
    }

    #[test]
    fn a_condition_withdraws_itself_once_it_is_fixed() {
        let mut toasts = Toasts::new();
        toasts.set_conditions(&[condition("no-secondary-display", "no audience display")]);
        assert_eq!(toasts.iter().count(), 1);

        // The projector is plugged in and reconciliation reports nothing.
        toasts.set_conditions(&[]);
        assert!(toasts.is_empty(), "the notice goes when the problem does");
    }

    #[test]
    fn a_condition_is_not_duplicated_by_repeated_reports() {
        let mut toasts = Toasts::new();
        for _ in 0..5 {
            toasts.set_conditions(&[condition("shared-display", "both windows share a display")]);
        }
        assert_eq!(toasts.iter().count(), 1);
    }

    #[test]
    fn rewording_a_live_condition_updates_it_in_place() {
        let mut toasts = Toasts::new();
        toasts.set_conditions(&[condition("ambiguous-selection", "matches 2 monitors")]);
        let first = toasts.iter().next().unwrap().id;
        toasts.set_conditions(&[condition("ambiguous-selection", "matches 3 monitors")]);

        let toast = toasts.iter().next().unwrap();
        assert_eq!(toast.id, first, "the same notice, not a second one");
        assert_eq!(toast.message, "matches 3 monitors");
    }

    #[test]
    fn a_dismissed_condition_stays_dismissed_until_it_clears() {
        let mut toasts = Toasts::new();
        let live = [condition("no-secondary-display", "no audience display")];
        toasts.set_conditions(&live);
        let id = toasts.iter().next().unwrap().id;

        // Acknowledged: presenting on one screen on purpose.
        toasts.dismiss(id);
        toasts.set_conditions(&live);
        assert!(toasts.is_empty(), "acknowledging it must mean something");

        // Plugged in, then unplugged again: that is news once more.
        toasts.set_conditions(&[]);
        toasts.set_conditions(&live);
        assert_eq!(toasts.iter().count(), 1);
    }

    #[test]
    fn intents_have_the_documented_lifetimes() {
        assert!(Intent::Info.lifetime().unwrap() < Intent::Warning.lifetime().unwrap());
        assert_eq!(Intent::Error.lifetime(), None);
    }
}
