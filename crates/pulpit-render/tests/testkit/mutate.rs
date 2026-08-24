//! A deterministic mutator, for finding panics without a fuzzing toolchain.
//!
//! `cargo fuzz` is the right tool for this and the `fuzz/` directory holds
//! proper targets for it, but it needs a nightly compiler and a deliberate
//! run. Most breakage is not subtle enough to require that: a truncated file,
//! a digit flipped inside a `/Length`, a `(` deleted from a string. Those are
//! reachable by mutating good PDFs with a fixed seed, in the time an ordinary
//! test takes, on every commit.
//!
//! Determinism is the point. A random seed would make failures unreproducible
//! and turn a red build into a coin flip; every run here mutates exactly the
//! same bytes in exactly the same order, so a failure names a case and a case
//! can be replayed.

/// A small reproducible pseudo-random source (xorshift64*), used rather than
/// the `rand` crate so the byte sequence is pinned to this code and cannot
/// change under a dependency update.
pub struct Rng(u64);

impl Rng {
    pub fn seeded(seed: u64) -> Self {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F491_4F6CDD1D)
    }

    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }
}

/// How a case was damaged, so a failure can say what it was doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    /// Cut the file short.
    Truncate,
    /// Replace one byte with another.
    FlipByte,
    /// Delete a run of bytes from the middle.
    Splice,
    /// Repeat a run of bytes, which can make a stream longer than its
    /// `/Length` claims.
    Duplicate,
    /// Replace a digit, which is how offsets and lengths go wrong in the wild.
    CorruptNumber,
    /// Remove a structural keyword.
    RemoveKeyword,
    /// Insert a nested array, to probe recursive descent.
    NestDeeply,
}

/// How deeply [`Damage::NestDeeply`] nests.
///
/// Deliberately modest. lopdf parses arrays by recursion, and its limit is far
/// lower than it looks: about a hundred levels on a debug build's test thread,
/// roughly a thousand on release. Past that the process does not panic, it
/// overflows the stack and aborts, which no `catch_unwind` can contain.
///
/// That is a real property of the parser and it is tested deliberately, in
/// the renderer worker, where the assertion is that the *subprocess* absorbs it.
/// It is not tested by accident here, because an abort takes the whole test
/// binary with it and every other case in the run is lost.
pub const NESTING_DEPTH: usize = 64;

/// A document whose `/Opt` array is nested `depth` levels deep.
///
/// Past a certain depth this is not a document any more but an attack on the
/// parser, which is the point: the caller decides which side of the line to
/// stand on.
pub fn deeply_nested_pdf(depth: usize) -> Vec<u8> {
    let mut pdf = super::builder::Pdf::new();
    let mut body = b"<< /Type /Annot /Subtype /Widget /T (nested) /FT /Ch /Opt ".to_vec();
    body.extend(std::iter::repeat_n(b'[', depth));
    body.extend(std::iter::repeat_n(b']', depth));
    body.extend_from_slice(b" >>");
    pdf.add(body);
    pdf.build()
}

impl Damage {
    pub const ALL: [Damage; 7] = [
        Damage::Truncate,
        Damage::FlipByte,
        Damage::Splice,
        Damage::Duplicate,
        Damage::CorruptNumber,
        Damage::RemoveKeyword,
        Damage::NestDeeply,
    ];
}

/// Damage `original` in the named way, using `rng` to choose where.
pub fn mutate(original: &[u8], damage: Damage, rng: &mut Rng) -> Vec<u8> {
    if original.is_empty() {
        return Vec::new();
    }
    let mut bytes = original.to_vec();
    match damage {
        Damage::Truncate => {
            let keep = rng.below(bytes.len());
            bytes.truncate(keep);
        }
        Damage::FlipByte => {
            let at = rng.below(bytes.len());
            bytes[at] = (rng.next_u64() & 0xFF) as u8;
        }
        Damage::Splice => {
            let at = rng.below(bytes.len());
            let length = rng.below((bytes.len() - at).min(64)).max(1);
            bytes.drain(at..at + length);
        }
        Damage::Duplicate => {
            let at = rng.below(bytes.len());
            let length = rng.below((bytes.len() - at).min(64)).max(1);
            let run = bytes[at..at + length].to_vec();
            bytes.splice(at..at, run);
        }
        Damage::CorruptNumber => {
            let digits: Vec<usize> = bytes
                .iter()
                .enumerate()
                .filter(|(_, byte)| byte.is_ascii_digit())
                .map(|(index, _)| index)
                .collect();
            if !digits.is_empty() {
                let at = digits[rng.below(digits.len())];
                bytes[at] = b'0' + (rng.next_u64() % 10) as u8;
            }
        }
        Damage::RemoveKeyword => {
            const KEYWORDS: [&[u8]; 6] = [
                b"endobj",
                b"endstream",
                b"stream",
                b"xref",
                b"trailer",
                b"startxref",
            ];
            let keyword = KEYWORDS[rng.below(KEYWORDS.len())];
            if let Some(at) = bytes
                .windows(keyword.len())
                .position(|window| window == keyword)
            {
                bytes.splice(
                    at..at + keyword.len(),
                    std::iter::repeat_n(b' ', keyword.len()),
                );
            }
        }
        Damage::NestDeeply => {
            let at = rng.below(bytes.len());
            let mut nest = Vec::new();
            nest.extend(std::iter::repeat_n(b'[', NESTING_DEPTH));
            nest.extend(std::iter::repeat_n(b']', NESTING_DEPTH));
            bytes.splice(at..at, nest);
        }
    }
    bytes
}
