//! The limits, declared once and checked on both sides of the worker
//! protocol (§9.5, §12).
//!
//! Every one of these bounds something that is allocated, mapped, decoded or
//! iterated from data a document controls. They are constants rather than
//! configuration on purpose: a limit a hostile file can raise is not a limit,
//! and a limit a user can raise is a support burden with no upside — the
//! values below are past what any real document reaches.
//!
//! They are also *duplicated nowhere*. The supervisor validates a request
//! against these before sending it and the worker validates again on receipt,
//! because the two are separate processes that can disagree after an upgrade
//! (§5.2), but both read the same constants.

use pulpit_core::annotate::{MAX_ANNOTATION_TEXT, MAX_INK_POINTS};

/// The most annotations one page may carry.
///
/// A heavily marked-up page of a reviewed manuscript runs to a few hundred;
/// documents in the wild with tens of thousands on one page exist and are
/// generator output, not something a person will edit.
pub const MAX_ANNOTATIONS_PER_PAGE: usize = 4_096;

/// The most annotations one document may carry, across every page.
pub const MAX_ANNOTATIONS_PER_DOCUMENT: usize = 200_000;

/// The most points one ink annotation may carry. Shared with the domain
/// crate, which enforces it while the stroke is still a gesture.
pub const MAX_POINTS_PER_INK: usize = MAX_INK_POINTS;

/// The most points one transaction may carry across all its commands. An
/// eraser sweep's deletes carry none, so this is really a bound on a compound
/// replacement.
pub const MAX_POINTS_PER_TRANSACTION: usize = 4 * MAX_INK_POINTS;

/// The most quadrilaterals one text-markup annotation may carry.
pub const MAX_QUADS_PER_ANNOTATION: usize = 4_096;

/// The most quadrilaterals one selection query may return. Larger than the
/// annotation limit because a selection is reported before it is committed and
/// the UI has to be able to say "that is more than a highlight can hold".
pub const MAX_QUADS_PER_SELECTION: usize = 8_192;

/// The most characters a selection query returns, or an annotation carries.
pub const MAX_TEXT_BYTES: usize = MAX_ANNOTATION_TEXT;

/// The most bytes of pulpit's own namespaced metadata — the Typst source of a
/// generated annotation, principally (§7.4).
pub const MAX_METADATA_BYTES: usize = 64 * 1024;

/// The most bytes one generated appearance stream may occupy.
///
/// An ink stroke of the maximum point count encodes to roughly a megabyte of
/// path operators; four is slack for a rasterised Typst appearance.
pub const MAX_APPEARANCE_BYTES: usize = 4 << 20;

/// The most commands one transaction may carry. An eraser sweep across a
/// densely marked page is the case that needs the room.
pub const MAX_OPERATIONS_PER_TRANSACTION: usize = 256;

/// The most form fields one document may expose.
pub const MAX_FORM_FIELDS: usize = 8_192;

/// The most characters one field value may hold. Long enough for a comment
/// box, short enough that a generated document cannot make the inspector
/// allocate without bound.
pub const MAX_FIELD_VALUE_BYTES: usize = 16 * 1024;

/// The most options a choice field may offer.
pub const MAX_FIELD_OPTIONS: usize = 4_096;

/// The most widgets one field may have across the document.
pub const MAX_FIELD_WIDGETS: usize = 1_024;

/// The most entries the recovery journal keeps before it is compacted.
///
/// A session's worth of edits, bounded so a runaway loop cannot fill a disk
/// with a journal (§11.5).
pub const MAX_JOURNAL_ENTRIES: usize = 100_000;

/// Why something was refused by a limit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{what} exceeds the limit of {limit}")]
pub struct LimitExceeded {
    pub what: &'static str,
    pub limit: usize,
}

/// Check `count` against `limit`, naming what was counted.
pub fn within(what: &'static str, count: usize, limit: usize) -> Result<(), LimitExceeded> {
    if count > limit {
        return Err(LimitExceeded { what, limit });
    }
    Ok(())
}

/// Every limit leaves room for a real document.
///
/// Not an arbitrary assertion: these are the numbers §12 says are chosen from
/// corpus measurement rather than inherited, and a change that quietly drops
/// one below what a real file carries should fail here rather than in
/// somebody's hands. Checked at compile time, because a constant that is wrong
/// is wrong before anything runs.
const _: () = {
    assert!(MAX_ANNOTATIONS_PER_PAGE >= 1_000);
    assert!(MAX_ANNOTATIONS_PER_DOCUMENT >= MAX_ANNOTATIONS_PER_PAGE);
    assert!(MAX_QUADS_PER_SELECTION >= MAX_QUADS_PER_ANNOTATION);
    assert!(MAX_POINTS_PER_TRANSACTION >= MAX_POINTS_PER_INK);
    assert!(MAX_OPERATIONS_PER_TRANSACTION >= 32);
    assert!(MAX_FIELD_VALUE_BYTES >= 1_024);
    // The gesture refuses a stroke the protocol would refuse anyway; if these
    // ever drift, a user would draw a stroke the UI accepted and the worker
    // rejected, which is the worst of both.
    assert!(MAX_POINTS_PER_INK == MAX_INK_POINTS);
    assert!(MAX_TEXT_BYTES == MAX_ANNOTATION_TEXT);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_at_the_limit_is_allowed_and_one_past_it_is_not() {
        assert!(within("things", 8, 8).is_ok());
        assert_eq!(
            within("things", 9, 8),
            Err(LimitExceeded {
                what: "things",
                limit: 8
            })
        );
        assert_eq!(
            within("things", 9, 8).unwrap_err().to_string(),
            "things exceeds the limit of 8"
        );
    }
}
