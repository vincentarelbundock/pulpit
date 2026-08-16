//! Stable annotation identity (invariant A3).
//!
//! An `AnnotationId` is what the PDF's `/NM` entry carries, which is the only
//! identity that survives a save: object numbers are renumbered by any writer
//! that compacts a file, so an object number is never the sole durable
//! identity. Once written, an `/NM` is never rewritten — that is what lets a
//! round-trip test reopen a saved document and enumerate by id.

use serde::{Deserialize, Serialize};

/// The identity of one editable annotation.
///
/// Stored as a short ASCII string because that is what goes into `/NM`, and a
/// `/NM` other software reads should be a plain name rather than an escaped
/// binary blob. Values pulpit generates are 24 lowercase hex characters after
/// a `pulpit-` tag; values imported from another producer are whatever that
/// producer wrote, bounded and validated on the way in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AnnotationId(String);

impl AnnotationId {
    /// The longest `/NM` pulpit will accept from a document. Names beyond this
    /// are treated as missing and the annotation gets a session identity
    /// instead (A3), which bounds every map keyed by id (A8).
    pub const MAX_LEN: usize = 128;

    /// What pulpit's own identifiers start with, so a diagnostic can say which
    /// annotations this application wrote.
    pub const PREFIX: &'static str = "pulpit-";

    /// Adopt a name found in a document, if it is one that can be used.
    ///
    /// Rejects the empty name, over-long names and names carrying control
    /// characters — all three appear in the wild, and all three would either
    /// break a diagnostic or make two annotations indistinguishable.
    pub fn imported(name: &str) -> Option<AnnotationId> {
        let name = name.trim();
        if name.is_empty() || name.len() > Self::MAX_LEN {
            return None;
        }
        if name.chars().any(|c| c.is_control()) {
            return None;
        }
        Some(AnnotationId(name.to_string()))
    }

    /// A name pulpit generated. Only [`IdGenerator`] builds these.
    fn generated(value: String) -> AnnotationId {
        AnnotationId(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Did pulpit write this one? Not a security property — another producer
    /// may of course copy the prefix — only a hint for diagnostics.
    pub fn looks_generated(&self) -> bool {
        self.0.starts_with(Self::PREFIX)
    }
}

impl std::fmt::Display for AnnotationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hands out identities that do not collide with each other or with names
/// already in the document.
///
/// Seeded by the caller rather than by a clock read, because this crate does
/// not read clocks: the worker passes something session-unique (a start time
/// and a process id, mixed) and the generator turns it into a stream. Two
/// pulpit processes editing two copies of the same file get different seeds
/// and therefore different names, which matters the moment those copies are
/// merged by hand.
#[derive(Debug, Clone)]
pub struct IdGenerator {
    state: u64,
    issued: u64,
}

impl IdGenerator {
    pub fn new(seed: u64) -> IdGenerator {
        // SplitMix64's initialiser. The stream only has to be well spread and
        // reproducible from the seed; it is not a source of secrets.
        IdGenerator {
            state: seed ^ 0x9e37_79b9_7f4a_7c15,
            issued: 0,
        }
    }

    /// The next identity. Never returns the same value twice for one
    /// generator, because the counter is mixed in alongside the stream.
    pub fn next_id(&mut self) -> AnnotationId {
        let mixed = self.next_u64();
        self.issued += 1;
        AnnotationId::generated(format!(
            "{}{mixed:016x}{:08x}",
            AnnotationId::PREFIX,
            self.issued as u32
        ))
    }

    /// How many identities this generator has handed out.
    pub fn issued(&self) -> u64 {
        self.issued
    }

    fn next_u64(&mut self) -> u64 {
        // SplitMix64.
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_identities_are_unique_and_recognisable() {
        let mut generator = IdGenerator::new(7);
        let ids: HashSet<AnnotationId> = (0..10_000).map(|_| generator.next_id()).collect();
        assert_eq!(ids.len(), 10_000, "a generator repeated itself");
        assert_eq!(generator.issued(), 10_000);
        assert!(ids.iter().all(|id| id.looks_generated()));
        assert!(ids
            .iter()
            .all(|id| id.as_str().len() <= AnnotationId::MAX_LEN));
    }

    #[test]
    fn two_seeds_do_not_produce_the_same_stream() {
        let mut a = IdGenerator::new(1);
        let mut b = IdGenerator::new(2);
        let first: HashSet<AnnotationId> = (0..64).map(|_| a.next_id()).collect();
        let second: HashSet<AnnotationId> = (0..64).map(|_| b.next_id()).collect();
        assert!(
            first.is_disjoint(&second),
            "two sessions must not hand out the same names"
        );
    }

    #[test]
    fn a_seed_reproduces_its_own_stream() {
        let mut a = IdGenerator::new(42);
        let mut b = IdGenerator::new(42);
        assert_eq!(a.next_id(), b.next_id());
    }

    #[test]
    fn imported_names_are_adopted_only_when_usable() {
        assert_eq!(
            AnnotationId::imported("acrobat-1234").unwrap().as_str(),
            "acrobat-1234"
        );
        // Surrounding whitespace is a producer's slip, not a different name.
        assert_eq!(AnnotationId::imported("  a  ").unwrap().as_str(), "a");
        assert!(AnnotationId::imported("").is_none());
        assert!(AnnotationId::imported("   ").is_none());
        assert!(AnnotationId::imported("a\u{0}b").is_none());
        assert!(AnnotationId::imported("line\nbreak").is_none());
        assert!(AnnotationId::imported(&"x".repeat(AnnotationId::MAX_LEN)).is_some());
        assert!(AnnotationId::imported(&"x".repeat(AnnotationId::MAX_LEN + 1)).is_none());
    }

    #[test]
    fn an_imported_name_is_not_claimed_as_pulpits_own() {
        assert!(!AnnotationId::imported("acrobat-1")
            .unwrap()
            .looks_generated());
        assert!(IdGenerator::new(0).next_id().looks_generated());
    }
}
