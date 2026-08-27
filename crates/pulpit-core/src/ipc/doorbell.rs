//! A one-slot doorbell: "something happened", coalesced, never blocking.
//!
//! Four of these existed before this module, character for character apart
//! from the type name — one per supervisor, one for the reader link, one for
//! the document watcher. They are all the same object because they all answer
//! the same question: a worker thread has put something on a channel and wants
//! the event loop to come and look, without either side waiting on the other.
//!
//! Deliberately carries nothing. It is a doorbell, not a delivery: the
//! messages stay on the owner's own channel, which only the event-loop thread
//! may drain, so nothing here can race a dispatch or duplicate an event. A
//! caller that misses a ring loses nothing, because the next drain takes
//! everything waiting — which is exactly what lets the sink drop a signal
//! rather than block the thread that rang it.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::Mutex;
use std::time::Duration;

/// What a [`Doorbell::wait`] came back with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wakeup {
    /// Someone rang: go and drain.
    Ring,
    /// Nothing was said within the timeout.
    Idle,
    /// The other end is gone. Every later call returns this immediately, so a
    /// loop that mistakes it for a ring spins.
    Closed,
}

/// The waiting end of the doorbell.
pub struct Doorbell {
    inbox: Mutex<Receiver<()>>,
}

impl Doorbell {
    /// Wait up to `timeout` for a ring.
    ///
    /// Blocking: this is meant for a thread of the caller's own, not for the
    /// event loop, which must stay free to draw. One waiter at a time — the
    /// handle is taken once — and a second caller finding the inbox held is
    /// told the same thing as a caller finding it closed, because in both
    /// cases waiting here will never produce anything.
    pub fn wait(&self, timeout: Duration) -> Wakeup {
        let Ok(inbox) = self.inbox.try_lock() else {
            return Wakeup::Closed;
        };
        match inbox.recv_timeout(timeout) {
            Ok(()) => Wakeup::Ring,
            Err(RecvTimeoutError::Timeout) => Wakeup::Idle,
            Err(RecvTimeoutError::Disconnected) => Wakeup::Closed,
        }
    }
}

/// The ringing end, held by whichever threads produce work.
#[derive(Clone)]
pub struct Sink(SyncSender<()>);

impl Sink {
    /// Ring, unless it is already ringing.
    ///
    /// `try_send` on a one-deep channel is the whole coalescing rule: a burst
    /// of finished work wakes the waiter once, and a producer thread never
    /// blocks on a waiter that has not got round to looking yet.
    pub fn ring(&self) {
        let _ = self.0.try_send(());
    }

    /// Put `message` on `channel`, then ring — in that order.
    ///
    /// The order is the one thing about a doorbell that is easy to get wrong.
    /// Ringing first opens a window where the waiter wakes, drains an empty
    /// channel, and goes back to sleep, while the message that prompted the
    /// ring arrives just behind it and waits for a ring that has already been
    /// spent. Every copy of this carried a comment saying so; saying it in
    /// code instead is why this method exists.
    pub fn send_then_ring<T>(&self, channel: &std::sync::mpsc::Sender<T>, message: T) -> bool {
        if channel.send(message).is_err() {
            return false;
        }
        self.ring();
        true
    }
}

/// A doorbell and its sink.
pub fn doorbell() -> (Sink, Doorbell) {
    // One deep: the slot means "there is something to look at", and a second
    // signal while the first is unread would say nothing the first did not.
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    (
        Sink(send),
        Doorbell {
            inbox: Mutex::new(receive),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANT: Duration = Duration::from_millis(50);

    #[test]
    fn a_ring_wakes_a_waiter() {
        let (sink, bell) = doorbell();
        sink.ring();
        assert_eq!(bell.wait(INSTANT), Wakeup::Ring);
    }

    #[test]
    fn silence_times_out_rather_than_blocking_forever() {
        let (_sink, bell) = doorbell();
        assert_eq!(bell.wait(INSTANT), Wakeup::Idle);
    }

    #[test]
    fn a_burst_of_rings_wakes_the_waiter_once() {
        // The coalescing rule: the waiter is told "there is work", not how
        // many times work arrived, so a hundred rings cost one wake-up and the
        // ringing threads never block.
        let (sink, bell) = doorbell();
        for _ in 0..100 {
            sink.ring();
        }
        assert_eq!(bell.wait(INSTANT), Wakeup::Ring);
        assert_eq!(
            bell.wait(INSTANT),
            Wakeup::Idle,
            "a hundred rings leave one signal, not a hundred"
        );
    }

    #[test]
    fn a_dropped_sink_closes_the_doorbell() {
        let (sink, bell) = doorbell();
        drop(sink);
        assert_eq!(bell.wait(INSTANT), Wakeup::Closed);
    }

    #[test]
    fn a_second_waiter_is_told_the_same_thing_as_a_closed_one() {
        let (sink, bell) = doorbell();
        let held = bell.inbox.try_lock().expect("the first waiter takes it");
        assert_eq!(bell.wait(INSTANT), Wakeup::Closed);
        drop(held);
        sink.ring();
        assert_eq!(bell.wait(INSTANT), Wakeup::Ring);
    }

    #[test]
    fn the_message_is_on_the_channel_before_the_ring() {
        // What `send_then_ring` exists to guarantee: by the time a waiter is
        // woken, the thing it will go looking for is already there.
        let (sink, bell) = doorbell();
        let (send, receive) = std::sync::mpsc::channel();
        assert!(sink.send_then_ring(&send, "work"));
        assert_eq!(bell.wait(INSTANT), Wakeup::Ring);
        assert_eq!(receive.try_recv(), Ok("work"));
    }

    #[test]
    fn a_gone_receiver_is_reported_and_rings_nothing() {
        let (sink, bell) = doorbell();
        let (send, receive) = std::sync::mpsc::channel();
        drop(receive);
        assert!(!sink.send_then_ring(&send, "work"));
        assert_eq!(
            bell.wait(INSTANT),
            Wakeup::Idle,
            "a message that never landed must not ring"
        );
    }
}
