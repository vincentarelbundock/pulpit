//! One request in flight, the newest one waiting.
//!
//! The document worker is serial, and several things the reader does sample
//! far faster than a round trip: the pointer moving over a form, the pointer
//! sweeping out a text selection, a rectangle repainted per keystroke. Sending
//! one request per sample queues renders of states nobody will ever see, and
//! the answer the user is actually waiting for — the release that commits a
//! highlight, the character just typed — arrives behind the whole backlog.
//!
//! The rule everywhere is the same, so it is written once here: at most one
//! request is out, at most one waits, and the one that waits is always the
//! newest. What is held back is not lost, it is superseded.
//!
//! Sending can fail — there may be no link to send on — so the slot is taken
//! by [`Coalesced::sent`] after the send succeeded rather than by
//! [`Coalesced::offer`] before it. A slot taken by a request that never went
//! out is a slot nothing will ever release, which is exactly the latch this
//! type exists to prevent.

/// A one-in-flight, newest-waiting slot for requests of type `T`.
pub struct Coalesced<T> {
    in_flight: bool,
    waiting: Option<T>,
}

impl<T> Default for Coalesced<T> {
    fn default() -> Self {
        Coalesced {
            in_flight: false,
            waiting: None,
        }
    }
}

impl<T> std::fmt::Debug for Coalesced<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coalesced")
            .field("in_flight", &self.in_flight)
            .field("waiting", &self.waiting.is_some())
            .finish()
    }
}

impl<T> Coalesced<T> {
    /// Offer work. `Some(work)` is work the caller should send now; `None`
    /// means it has been stored as the newest thing waiting, replacing
    /// whatever was waiting before.
    pub fn offer(&mut self, work: T) -> Option<T> {
        if self.in_flight {
            self.waiting = Some(work);
            return None;
        }
        Some(work)
    }

    /// Offer work that is never held back — a release that commits, which is
    /// the newest position by definition. Whatever was waiting is superseded.
    pub fn offer_now(&mut self, work: T) -> T {
        self.waiting = None;
        work
    }

    /// A send succeeded, so the slot is taken until it is answered.
    pub fn sent(&mut self) {
        self.in_flight = true;
    }

    /// The outstanding request was answered. Returns whatever was waiting,
    /// which the caller should send now.
    pub fn answered(&mut self) -> Option<T> {
        self.in_flight = false;
        self.waiting.take()
    }

    /// Nothing outstanding will be answered, and nothing waiting is worth
    /// sending — the link died, or the document was replaced.
    pub fn abandon(&mut self) {
        self.in_flight = false;
        self.waiting = None;
    }

    /// Is something waiting to go out? Read by [`crate::app::App::is_live`]:
    /// a request that is *out* is counted by the link that carries it, but a
    /// request nothing has sent yet is only known here, and the tick that
    /// sends it has to keep running.
    pub fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    #[cfg(test)]
    pub fn is_in_flight(&self) -> bool {
        self.in_flight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_offer_goes_out_and_the_next_ones_wait() {
        let mut slot = Coalesced::default();
        assert_eq!(slot.offer(1), Some(1));
        slot.sent();
        assert_eq!(slot.offer(2), None);
        // The newest wins: 2 is superseded before it ever goes out.
        assert_eq!(slot.offer(3), None);
        assert!(slot.is_waiting());
        assert_eq!(slot.answered(), Some(3));
        assert!(!slot.is_waiting());
    }

    #[test]
    fn a_send_that_did_not_happen_does_not_take_the_slot() {
        // The latch this type exists to prevent: `offer` handed the work
        // back, the caller could not send it, and nothing marked the slot
        // taken — so the next offer goes out rather than waiting for an
        // answer to a request that was never asked.
        let mut slot = Coalesced::default();
        assert_eq!(slot.offer(1), Some(1));
        // …no `sent()`: there was no link.
        assert_eq!(slot.offer(2), Some(2));
        assert!(!slot.is_in_flight());
    }

    #[test]
    fn an_answer_with_nothing_waiting_leaves_the_slot_free() {
        let mut slot: Coalesced<u8> = Coalesced::default();
        assert_eq!(slot.offer(1), Some(1));
        slot.sent();
        assert_eq!(slot.answered(), None);
        assert!(!slot.is_in_flight());
        assert_eq!(slot.offer(2), Some(2));
    }

    #[test]
    fn work_that_is_never_held_back_supersedes_what_waits() {
        let mut slot = Coalesced::default();
        assert_eq!(slot.offer(1), Some(1));
        slot.sent();
        assert_eq!(slot.offer(2), None);
        assert_eq!(slot.offer_now(9), 9);
        assert!(!slot.is_waiting());
    }

    #[test]
    fn abandoning_frees_the_slot_and_drops_what_waits() {
        let mut slot = Coalesced::default();
        assert_eq!(slot.offer(1), Some(1));
        slot.sent();
        assert_eq!(slot.offer(2), None);
        slot.abandon();
        assert!(!slot.is_in_flight());
        assert!(!slot.is_waiting());
        assert_eq!(slot.offer(3), Some(3));
    }
}
