//! The per-keystroke bookkeeping behind form editing, with no pixels in it.
//!
//! Typing into a form field is a conversation with a serial worker: the event
//! goes out, PDFium answers with the rectangles it dirtied, a crop of those
//! rectangles is asked for, and the crop is held over the page's frame until a
//! full frame containing the same state arrives (§9.4). Every part of that is
//! ordinary bookkeeping — a queue, a union of rectangles, a label, a revision
//! comparison — and all of it used to live in `App`, where nothing could test
//! it. The three bugs that motivated moving it here are the three the tests at
//! the bottom of this file now hold shut:
//!
//! - A request marked outstanding before it was sent. There was no link to
//!   send on, nothing ever answered, and the page's patches were held behind
//!   the phantom for the rest of the session.
//! - A patch labelled with a *later* event's form state. One render answers a
//!   run of keystrokes, and reading its answer with the wrong label either
//!   blinks half-typed text away at the next full frame or pins a committed
//!   rectangle over the page for ever.
//! - A patch refused for straddling a retained preview, refused again for the
//!   same reason, and so on until the form stopped showing typing at all.
//!
//! The invariants this type holds, in one place so they can be read at once:
//!
//! - **One request out per page, newest waiting.** What is held back is
//!   superseded, never lost, because the scope only grows.
//! - **A slot is taken by a send that happened.** The caller reports the send;
//!   an ask that never went out leaves nothing outstanding.
//! - **The scope is monotone until a full frame catches up.** One patch
//!   replaces the last, so it must keep covering what that one covered.
//! - **The `uncommitted` label belongs to the request, not the page.** It is
//!   stored as it was asked for and never re-derived.
//! - **A straddle grows the scope at most once per preview.** A regrow that
//!   changes nothing is not asked for again, which is what makes it terminate.
//! - **A mutation waits while an event that might commit is out**, so the
//!   revision it names is honest by construction.
//!
//! Nothing here knows what a texture is. The caller owns the pixels and the
//! link; this owns the rectangles, the revisions, the labels and the queues.

use std::collections::{HashMap, VecDeque};

use pulpit_core::notes::Region;
use pulpit_core::page::{PageIndex, PageRect};
use pulpit_render::document::DocumentRevision;

/// A patch a page is holding, minus its pixels.
#[derive(Clone, Copy, Debug)]
struct HeldPatch {
    /// Where it belongs, as a fraction of the upright page.
    region: Region,
    revision: DocumentRevision,
    /// True when the pixels show form-field state PDFium holds *uncommitted* —
    /// typing in progress, a value not yet under `/V`. No snapshot contains
    /// that state, so a full frame at the same revision must not take this
    /// patch down.
    uncommitted: bool,
    /// The full-page frame size this crop was rendered against. A zoom or a
    /// resize while a field is open leaves the rectangle stretched, and the
    /// page asks again at the new size rather than living with it.
    frame_size: (u32, u32),
}

/// A patch the worker has been asked for and has not answered.
///
/// The label is per request, not per page: a page keeps one request out at a
/// time, so in practice the queue holds one — it is a queue because the rule
/// that matches an answer to its request is the order they were asked in.
#[derive(Clone, Copy, Debug)]
struct PendingPatch {
    uncommitted: bool,
    frame_size: (u32, u32),
}

/// A patch a page wants and could not ask for yet, because one is already out.
#[derive(Clone, Copy, Debug)]
struct WaitingPatch {
    /// What the deferred events dirtied, unioned.
    dirty: PageRect,
    /// Whether the newest deferred event left form state uncommitted. The
    /// newest wins, not the strongest: one render answers the whole run, so
    /// the pixels show the state after the last of them.
    uncommitted: bool,
}

/// A rectangle of a page the caller should ask the worker to draw now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatchAsk {
    pub page: PageIndex,
    /// Everything patched on the page since its frame last caught up, which is
    /// what the crop has to cover — not only what the newest event dirtied.
    pub dirty: PageRect,
    pub uncommitted: bool,
}

/// A rectangle a page wants asked for again, either because it was held back
/// or because the frame it was drawn against is the wrong size now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatchReask {
    pub page: PageIndex,
    pub dirty: PageRect,
    pub uncommitted: bool,
}

/// What the caller made of an answer, before this type is told about it.
///
/// The caller resolves it because only the caller can: whether a crop can be
/// reconciled with the previews drawn over the page is the reader's question,
/// not this one's.
#[derive(Clone, Copy, Debug)]
pub enum PatchAnswer {
    /// Usable, and the previews it contains have come down.
    Taken {
        region: Region,
        revision: DocumentRevision,
    },
    /// A retained preview lies half inside the rectangle and half outside, so
    /// this crop cannot be drawn. `preview` is what it straddled.
    Straddled { preview: PageRect },
    /// Nothing a different rectangle would fix: an inconsistent frame, or a
    /// page with no geometry to place a crop against.
    Unusable,
}

/// What the caller should do with an answer it has just reported.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Landing {
    /// Nothing at all. The answer was matched and discarded.
    Nothing,
    /// Mint the crop as a texture and hold it over the page's frame.
    Hold,
    /// Ask again for a bigger rectangle, which is the only cure for a straddle.
    Regrow(PatchReask),
}

/// The per-keystroke state of form editing: what has been asked for, what is
/// held, what is waiting, and what is being held back behind it.
#[derive(Default)]
pub struct FormFlow {
    patches: HashMap<PageIndex, HeldPatch>,
    pending: HashMap<PageIndex, VecDeque<PendingPatch>>,
    /// Everything patched on a page since its frame last caught up, in page
    /// points. One patch per page means each new patch *replaces* the last, so
    /// it has to keep covering what earlier ones covered: PDFium draws a combo
    /// box's open list into the page, a hover then invalidates only the two
    /// rows that changed, and a patch of just those rows would take the rest
    /// of the list back to a frame that never had it.
    scope: HashMap<PageIndex, PageRect>,
    waiting: HashMap<PageIndex, WaitingPatch>,
    /// One entry per form event in flight, oldest first, saying whether that
    /// event could commit. A queue rather than a count because the answers
    /// come back over one link in the order they were asked for.
    events_in_flight: VecDeque<bool>,
    /// Mutations held back while such an event is out, in the order they were
    /// made.
    deferred: Vec<pulpit_render::document::DocumentTransaction>,
}

impl FormFlow {
    /// Ask for one rectangle of one page.
    ///
    /// `placeable` is the caller's answer to "can this page take a crop at
    /// all" — it has a frame on screen, at a size, with geometry to scale by.
    /// The scope grows before that is consulted, so a request that cannot go
    /// out now is still covered by the one that does.
    ///
    /// `Some(ask)` is a request to send now; `None` means it is either
    /// waiting behind the outstanding one or there is nowhere to put it.
    pub fn ask_patch(
        &mut self,
        page: PageIndex,
        dirty: PageRect,
        uncommitted: bool,
        placeable: bool,
    ) -> Option<PatchAsk> {
        let dirty = self.grow_scope(page, dirty);
        if !placeable {
            return None;
        }
        // One request out per page. A keystroke is a patch, and typing at
        // speed sent one render per character to a serial worker: each render
        // drew a state the next one had already superseded.
        if self.pending.contains_key(&page) {
            self.waiting
                .insert(page, WaitingPatch { dirty, uncommitted });
            return None;
        }
        Some(PatchAsk {
            page,
            dirty,
            uncommitted,
        })
    }

    /// A request went out, so the page's slot is taken until it is answered.
    ///
    /// Reported after the send rather than assumed before it: a slot taken by
    /// a request that never went out is a slot nothing will ever release.
    pub fn ask_sent(&mut self, ask: &PatchAsk, frame_size: (u32, u32)) {
        self.pending
            .entry(ask.page)
            .or_default()
            .push_back(PendingPatch {
                uncommitted: ask.uncommitted,
                frame_size,
            });
    }

    /// Is an answer owed for this page? Asked before the reader is consulted,
    /// because consulting it takes retained previews down and an answer to
    /// nothing must not do that.
    pub fn has_pending(&self, page: PageIndex) -> bool {
        self.pending.contains_key(&page)
    }

    /// An answer arrived and the caller has made what it could of it.
    ///
    /// The answers come back over one link in the order they were asked for,
    /// so the oldest request outstanding is the one this answers — and its own
    /// label, not a later one's, is what the patch is stored with.
    pub fn patch_answered(&mut self, page: PageIndex, answer: PatchAnswer) -> Landing {
        let Some(asked) = self.take_pending(page) else {
            return Landing::Nothing;
        };
        match answer {
            PatchAnswer::Unusable => Landing::Nothing,
            PatchAnswer::Straddled { preview } => {
                // A bigger crop rather than giving up: with a monotone scope, a
                // page that refused once would refuse every patch for the rest
                // of the session, and the form would stop showing typing.
                //
                // Only when the scope actually grew, which is what makes this
                // terminate: a preview the patch cannot be made to contain
                // grows nothing and is asked for once.
                let before = self.scope.get(&page).copied();
                let grown = self.grow_scope(page, preview);
                if before == Some(grown) {
                    return Landing::Nothing;
                }
                Landing::Regrow(PatchReask {
                    page,
                    dirty: grown,
                    uncommitted: asked.uncommitted,
                })
            }
            PatchAnswer::Taken { region, revision } => {
                self.patches.insert(
                    page,
                    HeldPatch {
                        region,
                        revision,
                        uncommitted: asked.uncommitted,
                        frame_size: asked.frame_size,
                    },
                );
                Landing::Hold
            }
        }
    }

    /// A request that will never be answered stops being outstanding.
    pub fn patch_refused(&mut self, page: PageIndex) {
        let _ = self.take_pending(page);
    }

    /// The patch a page has been waiting to ask for, if the page is free to
    /// ask: the answer to the outstanding request is what makes room.
    pub fn waiting_for(&mut self, page: PageIndex) -> Option<PatchReask> {
        if self.pending.contains_key(&page) {
            return None;
        }
        let waiting = self.waiting.remove(&page)?;
        Some(PatchReask {
            page,
            dirty: waiting.dirty,
            uncommitted: waiting.uncommitted,
        })
    }

    /// A full frame landed for a page. `true` when the patch it was standing
    /// in for has come down and the caller should drop its pixels.
    ///
    /// A full frame is the baseline every partial repaint was standing in for
    /// (§9.4): once one contains the patch's revision, the patch is a second
    /// copy of pixels the frame already has. Unless it shows *uncommitted*
    /// typing, which no snapshot contains — taking it down here made
    /// half-typed values blink out whenever a deferred frame landed behind
    /// them.
    pub fn frame_landed(&mut self, page: PageIndex, revision: DocumentRevision) -> bool {
        let survives = self
            .patches
            .get(&page)
            .is_some_and(|patch| patch.revision <= revision && !patch.uncommitted);
        if !survives {
            return false;
        }
        self.patches.remove(&page);
        self.scope.remove(&page);
        true
    }

    /// Every patch drawn against a frame size the page has since left.
    ///
    /// `frame_size_of` says how big the page's frame is now; `None` for a page
    /// with no frame on screen. Nothing is dropped — a patch is placed by its
    /// region and scaled — so the failure this ends is a soft rectangle, and
    /// the rectangle stays on screen for the whole round trip.
    pub fn resized_patches(
        &self,
        frame_size_of: impl Fn(PageIndex) -> Option<(u32, u32)>,
    ) -> Vec<PatchReask> {
        self.patches
            .iter()
            .filter(|(page, patch)| {
                frame_size_of(**page).is_some_and(|size| size != patch.frame_size)
            })
            .filter_map(|(page, patch)| {
                // The scope is what the page is showing, which is exactly what
                // has to be redrawn at the new size.
                Some(PatchReask {
                    page: *page,
                    dirty: self.scope.get(page).copied()?,
                    uncommitted: patch.uncommitted,
                })
            })
            .collect()
    }

    /// Where a page's patch belongs, as a fraction of the upright page.
    pub fn patch_region(&self, page: PageIndex) -> Option<Region> {
        self.patches.get(&page).map(|patch| patch.region)
    }

    /// One form event went out. `may_commit` is whether it *could* move the
    /// revision, which is known before it is sent; whether it *did* is only
    /// known from the answer.
    pub fn form_event_sent(&mut self, may_commit: bool) {
        self.events_in_flight.push_back(may_commit);
    }

    /// One form event answered, whatever it answered with.
    pub fn form_event_answered(&mut self) {
        self.events_in_flight.pop_front();
    }

    /// Could a form event still in flight move the revision?
    pub fn a_commit_may_be_in_flight(&self) -> bool {
        self.events_in_flight.iter().any(|may| *may)
    }

    /// A mutation wants to go out. `Some` is one to send now; `None` means it
    /// is held until the form is quiet.
    ///
    /// A form event in flight may commit a value, and a commit is a revision.
    /// A mutation sent now would name a revision it cannot know, and be
    /// refused for a conflict that is nobody's mistake.
    pub fn commit_requested(
        &mut self,
        transaction: pulpit_render::document::DocumentTransaction,
    ) -> Option<pulpit_render::document::DocumentTransaction> {
        if self.a_commit_may_be_in_flight() {
            self.deferred.push(transaction);
            return None;
        }
        Some(transaction)
    }

    /// Mutations held back that can go now. Empty while the form is still
    /// busy: they wait one more tick rather than being sent against a revision
    /// nobody knows.
    pub fn released_commits(&mut self) -> Vec<pulpit_render::document::DocumentTransaction> {
        if self.deferred.is_empty() || self.a_commit_may_be_in_flight() {
            return Vec::new();
        }
        std::mem::take(&mut self.deferred)
    }

    /// Is there work here nothing has been asked for yet? Read by the app's
    /// liveness: what is *out* is counted by the link that carries it, but
    /// what is held back is only known here, and the tick that sends it has to
    /// keep running.
    pub fn is_waiting(&self) -> bool {
        !self.waiting.is_empty() || !self.deferred.is_empty()
    }

    /// The document or the worker holding it is gone: nothing outstanding will
    /// be answered and nothing held back is worth sending.
    pub fn forget_document(&mut self) {
        self.patches.clear();
        self.pending.clear();
        self.scope.clear();
        self.waiting.clear();
        self.events_in_flight.clear();
        self.deferred.clear();
    }

    /// The oldest outstanding request for a page, no longer outstanding.
    fn take_pending(&mut self, page: PageIndex) -> Option<PendingPatch> {
        let queue = self.pending.get_mut(&page)?;
        let asked = queue.pop_front();
        if queue.is_empty() {
            self.pending.remove(&page);
        }
        asked
    }

    /// Everything patched on a page since its frame last caught up, with
    /// `dirty` added to it. Monotone: one rectangle replaces the last one
    /// drawn, so it must keep covering what that one covered.
    fn grow_scope(&mut self, page: PageIndex, dirty: PageRect) -> PageRect {
        match self.scope.entry(page) {
            std::collections::hash_map::Entry::Occupied(mut scope) => {
                let grown = scope.get().union(&dirty);
                scope.insert(grown);
                grown
            }
            std::collections::hash_map::Entry::Vacant(scope) => *scope.insert(dirty),
        }
    }

    #[cfg(test)]
    fn is_holding(&self, page: PageIndex) -> bool {
        self.patches.contains_key(&page)
    }

    #[cfg(test)]
    fn scope_of(&self, page: PageIndex) -> Option<PageRect> {
        self.scope.get(&page).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: PageIndex = PageIndex(3);
    const SIZE: (u32, u32) = (800, 1000);

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> PageRect {
        PageRect::new(left, top, right, bottom)
    }

    /// A caret in a text field, one character wide, `n` characters along.
    fn caret(n: f32) -> PageRect {
        rect(10.0 + n, 20.0, 11.0 + n, 32.0)
    }

    fn region() -> Region {
        Region::new(0.0, 0.0, 0.5, 0.5)
    }

    /// Ask and, if it goes out, report the send. What `App` does around this
    /// type when there is a worker to send on.
    fn ask_and_send(
        flow: &mut FormFlow,
        page: PageIndex,
        dirty: PageRect,
        uncommitted: bool,
    ) -> Option<PatchAsk> {
        let ask = flow.ask_patch(page, dirty, uncommitted, true)?;
        flow.ask_sent(&ask, SIZE);
        Some(ask)
    }

    #[test]
    fn a_typing_burst_keeps_one_request_out_and_asks_for_the_union() {
        let mut flow = FormFlow::default();
        let first = ask_and_send(&mut flow, PAGE, caret(0.0), true).expect("the first goes out");
        assert_eq!(first.dirty, caret(0.0));
        // Nineteen more characters while that one render is out: every one of
        // them is superseded, none of them is lost.
        for n in 1..20 {
            assert!(
                flow.ask_patch(PAGE, caret(n as f32), true, true).is_none(),
                "character {n} went out while a request was outstanding"
            );
        }
        assert!(flow.is_waiting());
        // The answer to the first frees the slot, and what goes out then
        // covers every rectangle it stood in for.
        assert_eq!(
            flow.patch_answered(
                PAGE,
                PatchAnswer::Taken {
                    region: region(),
                    revision: DocumentRevision(1),
                }
            ),
            Landing::Hold
        );
        let next = flow.waiting_for(PAGE).expect("the newest is waiting");
        assert_eq!(next.dirty, caret(0.0).union(&caret(19.0)));
        assert!(next.uncommitted);
        assert!(!flow.is_waiting());
    }

    #[test]
    fn a_refusal_mid_burst_releases_the_slot() {
        let mut flow = FormFlow::default();
        ask_and_send(&mut flow, PAGE, caret(0.0), true).expect("the first goes out");
        assert!(flow.ask_patch(PAGE, caret(1.0), true, true).is_none());
        // The render failed. Nothing will answer it, so the page must not stay
        // latched behind it.
        flow.patch_refused(PAGE);
        assert!(!flow.has_pending(PAGE));
        let next = flow.waiting_for(PAGE).expect("what waited can go now");
        assert_eq!(next.dirty, caret(0.0).union(&caret(1.0)));
    }

    #[test]
    fn a_send_the_caller_could_not_make_leaves_nothing_outstanding() {
        let mut flow = FormFlow::default();
        // Handed back, and never reported sent: there was no link.
        let ask = flow
            .ask_patch(PAGE, caret(0.0), true, true)
            .expect("handed back to send");
        assert_eq!(ask.page, PAGE);
        assert!(!flow.has_pending(PAGE));
        assert!(!flow.is_waiting());
        // …so the next one goes out rather than waiting for an answer to a
        // request nobody asked for.
        assert!(flow.ask_patch(PAGE, caret(1.0), true, true).is_some());
    }

    #[test]
    fn a_page_that_cannot_place_a_crop_still_grows_its_scope() {
        let mut flow = FormFlow::default();
        assert!(flow.ask_patch(PAGE, caret(0.0), true, false).is_none());
        assert!(!flow.has_pending(PAGE));
        assert!(!flow.is_waiting());
        // Scrolled back into view: the request that goes out covers what was
        // dirtied while there was nowhere to draw it.
        let ask = flow
            .ask_patch(PAGE, caret(5.0), true, true)
            .expect("goes out now");
        assert_eq!(ask.dirty, caret(0.0).union(&caret(5.0)));
    }

    #[test]
    fn an_answer_for_a_page_that_asked_for_nothing_is_ignored() {
        let mut flow = FormFlow::default();
        // The page scrolled away and the document was reopened under the
        // answer; nothing is owed for this page.
        assert!(!flow.has_pending(PAGE));
        assert_eq!(
            flow.patch_answered(
                PAGE,
                PatchAnswer::Taken {
                    region: region(),
                    revision: DocumentRevision(1),
                }
            ),
            Landing::Nothing
        );
        assert!(!flow.is_holding(PAGE));
    }

    #[test]
    fn a_commit_racing_a_keystroke_waits_for_it_and_goes_in_order() {
        let mut flow = FormFlow::default();
        // A pointer move cannot commit; a key can.
        flow.form_event_sent(false);
        assert!(!flow.a_commit_may_be_in_flight());
        let first = pulpit_render::document::DocumentTransaction(Vec::new());
        assert!(
            flow.commit_requested(first).is_some(),
            "nothing that could commit is out"
        );
        flow.form_event_sent(true);
        assert!(flow.a_commit_may_be_in_flight());
        assert!(flow
            .commit_requested(pulpit_render::document::DocumentTransaction(Vec::new()))
            .is_none());
        assert!(flow.is_waiting());
        assert!(
            flow.released_commits().is_empty(),
            "the key has not answered"
        );
        // The move answers first — the queue is what says which one that is.
        flow.form_event_answered();
        assert!(flow.a_commit_may_be_in_flight());
        assert!(flow.released_commits().is_empty());
        flow.form_event_answered();
        assert!(!flow.a_commit_may_be_in_flight());
        assert_eq!(flow.released_commits().len(), 1);
        assert!(!flow.is_waiting());
        assert!(flow.released_commits().is_empty());
    }

    #[test]
    fn a_straddle_grows_once_and_then_stops() {
        let mut flow = FormFlow::default();
        let preview = rect(0.0, 0.0, 200.0, 200.0);
        ask_and_send(&mut flow, PAGE, caret(0.0), true).expect("the first goes out");
        let Landing::Regrow(again) = flow.patch_answered(PAGE, PatchAnswer::Straddled { preview })
        else {
            panic!("a straddle asks again, bigger");
        };
        assert_eq!(again.dirty, caret(0.0).union(&preview));
        assert!(again.uncommitted, "the label is the request's own");
        ask_and_send(&mut flow, PAGE, again.dirty, again.uncommitted).expect("the regrow goes out");
        // Straddled again on a scope the preview no longer grows: asking a
        // third time would be the session-long loop this rule exists to stop.
        assert_eq!(
            flow.patch_answered(PAGE, PatchAnswer::Straddled { preview }),
            Landing::Nothing
        );
        assert!(!flow.is_holding(PAGE));
    }

    #[test]
    fn a_resize_mid_edit_asks_again_at_the_new_size() {
        let mut flow = FormFlow::default();
        let ask = ask_and_send(&mut flow, PAGE, caret(0.0), true).expect("goes out");
        assert_eq!(ask.dirty, caret(0.0));
        assert_eq!(
            flow.patch_answered(
                PAGE,
                PatchAnswer::Taken {
                    region: region(),
                    revision: DocumentRevision(1),
                }
            ),
            Landing::Hold
        );
        // Same size: nothing to redo.
        assert!(flow.resized_patches(|_| Some(SIZE)).is_empty());
        // No frame on screen for it: nothing to redo either.
        assert!(flow.resized_patches(|_| None).is_empty());
        // Zoomed. The whole scope is redrawn at the size the page is now at,
        // and the label the patch is holding comes with it.
        let stale = flow.resized_patches(|_| Some((1200, 1500)));
        assert_eq!(
            stale,
            vec![PatchReask {
                page: PAGE,
                dirty: caret(0.0),
                uncommitted: true,
            }]
        );
    }

    #[test]
    fn a_full_frame_takes_a_committed_patch_down_and_leaves_a_typed_one_up() {
        for uncommitted in [false, true] {
            let mut flow = FormFlow::default();
            ask_and_send(&mut flow, PAGE, caret(0.0), uncommitted).expect("goes out");
            assert_eq!(
                flow.patch_answered(
                    PAGE,
                    PatchAnswer::Taken {
                        region: region(),
                        revision: DocumentRevision(4),
                    }
                ),
                Landing::Hold
            );
            // A frame older than the patch never takes it down.
            assert!(!flow.frame_landed(PAGE, DocumentRevision(3)));
            assert!(flow.is_holding(PAGE));
            // At the patch's own revision the committed one is a second copy
            // of pixels the frame already has; the uncommitted one is not in
            // any frame at all, and blinks out if it is dropped.
            assert_eq!(flow.frame_landed(PAGE, DocumentRevision(4)), !uncommitted);
            assert_eq!(flow.is_holding(PAGE), uncommitted);
            assert_eq!(flow.scope_of(PAGE).is_some(), uncommitted);
        }
    }

    #[test]
    fn losing_the_worker_mid_fill_leaves_nothing_held_and_nothing_owed() {
        let mut flow = FormFlow::default();
        flow.form_event_sent(true);
        ask_and_send(&mut flow, PAGE, caret(0.0), true).expect("goes out");
        assert!(flow.ask_patch(PAGE, caret(1.0), true, true).is_none());
        assert_eq!(
            flow.patch_answered(
                PAGE,
                PatchAnswer::Taken {
                    region: region(),
                    revision: DocumentRevision(1),
                }
            ),
            Landing::Hold
        );
        ask_and_send(&mut flow, PAGE, caret(2.0), true).expect("goes out");
        assert!(flow
            .commit_requested(pulpit_render::document::DocumentTransaction(Vec::new()))
            .is_none());

        flow.forget_document();

        assert!(!flow.has_pending(PAGE));
        assert!(!flow.is_holding(PAGE));
        assert!(!flow.is_waiting());
        assert!(!flow.a_commit_may_be_in_flight());
        assert!(flow.patch_region(PAGE).is_none());
        assert!(flow.waiting_for(PAGE).is_none());
        assert!(flow.released_commits().is_empty());
        assert!(flow.resized_patches(|_| Some((1200, 1500))).is_empty());
        // …and the scope is gone with it, so the first patch of the next
        // document is not grown to cover a rectangle of the last one.
        assert!(flow.scope_of(PAGE).is_none());
    }
}
