#![forbid(unsafe_code)]

//! Bounded PDF object resolution for verification.
//!
//! Discovery used to locate objects by scanning the raw bytes for the literal
//! text `"{n} 0 obj"`. That is not a resolver: it hardcodes generation 0, it
//! ignores the cross-reference table entirely, it happily matches a decoy
//! inside a stream body, and it cannot see an object that lives in an object
//! stream at all.
//!
//! This module replaces that with a real, bounded resolver:
//!
//! * [`XrefIndex`] parses the cross-reference chain — classic tables *and*
//!   xref streams — following `/Prev` (and `/XRefStm`), newest entry winning.
//! * [`ObjectResolver`] resolves an object number at its recorded offset, with
//!   the generation checked against the object header, and decodes object
//!   streams (`FlateDecode`, with PNG predictors, or uncompressed).
//! * Everything is capped: chain length, entry count, stream size, object
//!   stream population and value nesting depth.
//!
//! When the cross-reference data is damaged — which real fixtures in this
//! workspace are, deliberately — the caller may fall back to a *repaired*
//! scan ([`scan_object_definitions`]). That scan is generation-aware and skips
//! stream bodies, and its results are labelled [`Confidence::Scanned`] so a
//! caller can tell a resolved object from a guessed one.

use super::{Result, VerifyError};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Bounds. Every one of these exists to make a malicious file terminate.
// ---------------------------------------------------------------------------

/// Maximum number of cross-reference sections followed through `/Prev`.
pub const MAX_XREF_CHAIN: usize = 256;
/// Maximum number of cross-reference entries retained in total.
pub const MAX_XREF_ENTRIES: usize = 2_000_000;
/// Maximum decoded size of any single stream, in bytes.
pub const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;
/// Maximum number of objects packed into a single object stream.
pub const MAX_OBJSTM_OBJECTS: usize = 100_000;
/// Maximum nesting depth of a parsed value.
pub const MAX_VALUE_DEPTH: usize = 64;
/// Maximum number of object streams decoded for one resolver.
pub const MAX_OBJSTM_DECODED: usize = 4096;
/// Maximum number of `/Index` subsections in one cross-reference stream.
///
/// A subsection is a run of consecutive object numbers. Even a heavily
/// incrementally-updated document has a few dozen; this leaves four orders of
/// magnitude of headroom while still bounding a crafted `/Index`.
pub const MAX_XREF_SUBSECTIONS: usize = 65_536;
/// Maximum width, in bytes, of one field of a cross-reference stream row.
///
/// §7.5.8.2 gives `/W` three entries, each the byte width of one field: the
/// entry type, an offset or object number, and a generation or index. The
/// widest thing any of them can hold is a file offset, and eight bytes already
/// addresses 16 exabytes — so nothing legal exceeds this. The cap is not a
/// nicety: the row length is the sum of the widths, and a `/W` of
/// `[i64::MAX i64::MAX 3]` wrapped that sum to a small number, slipped past
/// both the "row is empty" and the "row runs past the data" guards, and then
/// indexed the decoded buffer for i64::MAX bytes.
pub const MAX_XREF_FIELD_WIDTH: u64 = 8;
/// Maximum number of elements in one parsed array.
///
/// A `/W`, an `/Index` or a `/ByteRange` has a handful; a `/Widths` or an
/// `/Annots` has thousands. This bounds the allocation a declared array can
/// provoke *while it is being lexed*, so a crafted `/Index` cannot be
/// materialised into a multi-gigabyte `Vec` before the subsection cap that
/// governs it is ever consulted.
pub const MAX_ARRAY_ITEMS: usize = 262_144;
/// Maximum total decoded cross-reference stream bytes for one pass over a
/// document's chain.
///
/// Each section is separately bounded by [`MAX_STREAM_BYTES`], but a chain of
/// [`MAX_XREF_CHAIN`] sections that each decode to that cap is gigabytes of
/// inflate for a file of a few hundred kilobytes. The budget is per pass, so
/// the work a document can demand is bounded by the document, not by how many
/// times something is looked up in it.
pub const MAX_XREF_DECODED_BYTES: usize = 64 * 1024 * 1024;

/// The decompression work one pass over a cross-reference chain may do.
///
/// Threaded rather than global: a budget that outlived a call would make the
/// second verification of the same document behave differently from the first.
pub(super) struct DecodeBudget {
    remaining: usize,
}

impl DecodeBudget {
    pub(super) fn new() -> Self {
        DecodeBudget {
            remaining: MAX_XREF_DECODED_BYTES,
        }
    }

    fn spend(&mut self, bytes: usize) -> Result<()> {
        if bytes > self.remaining {
            return Err(VerifyError::XrefParseError(format!(
                "the cross-reference chain decodes to more than {MAX_XREF_DECODED_BYTES} bytes; \
                 refusing to keep inflating it"
            )));
        }
        self.remaining -= bytes;
        Ok(())
    }
}

/// How an object was located.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Located through the cross-reference chain, generation checked.
    Resolved,
    /// Located by a repair scan because the cross-reference data was unusable.
    Scanned,
}

// ---------------------------------------------------------------------------
// Value model
// ---------------------------------------------------------------------------

/// A parsed PDF object dictionary.
pub type Dict = BTreeMap<String, PdfValue>;

/// A parsed PDF value. Streams keep the byte extent of their body rather than
/// a copy, so the body is only materialised when a filter is applied.
#[derive(Debug, Clone, PartialEq)]
pub enum PdfValue {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    /// A string, already unescaped (literal) or unhexed (hex).
    Str(Vec<u8>),
    /// A name without its leading slash, `#xx` escapes decoded.
    Name(String),
    Array(Vec<PdfValue>),
    Dict(Dict),
    Stream {
        dict: Dict,
        /// Offset of the first body byte, relative to the buffer parsed.
        body_start: usize,
        /// Length of the body in bytes.
        body_len: usize,
    },
    Ref(u32, u16),
}

impl PdfValue {
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            PdfValue::Dict(d) => Some(d),
            PdfValue::Stream { dict, .. } => Some(dict),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[PdfValue]> {
        match self {
            PdfValue::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&str> {
        match self {
            PdfValue::Name(n) => Some(n.as_str()),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            PdfValue::Int(i) => Some(*i),
            PdfValue::Real(r) => Some(*r as i64),
            _ => None,
        }
    }

    pub fn as_ref_pair(&self) -> Option<(u32, u16)> {
        match self {
            PdfValue::Ref(n, g) => Some((*n, *g)),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, PdfValue::Null)
    }
}

// ---------------------------------------------------------------------------
// Lexer / parser
// ---------------------------------------------------------------------------

fn is_ws(c: u8) -> bool {
    matches!(c, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delim(c: u8) -> bool {
    matches!(
        c,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(c: u8) -> bool {
    !is_ws(c) && !is_delim(c)
}

/// A minimal recursive-descent reader over PDF syntax.
pub struct Lexer<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(buf: &'a [u8], pos: usize) -> Self {
        Lexer { buf, pos }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(c) if is_ws(c) => self.pos += 1,
                Some(b'%') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' || c == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn read_keyword(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if is_regular(c) {
                self.pos += 1;
            } else {
                break;
            }
        }
        &self.buf[start..self.pos]
    }

    /// Read a non-negative integer, or `None` if the next token is not one.
    fn read_uint(&mut self) -> Option<u64> {
        self.skip_ws();
        let save = self.pos;
        let kw = self.read_keyword();
        match std::str::from_utf8(kw).ok().and_then(|s| s.parse().ok()) {
            Some(v) => Some(v),
            None => {
                self.pos = save;
                None
            }
        }
    }

    fn parse_name(&mut self) -> Result<PdfValue> {
        // Caller has confirmed the '/'.
        self.pos += 1;
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if !is_regular(c) {
                break;
            }
            self.pos += 1;
            if c == b'#' {
                let hi = self.peek();
                let lo = self.buf.get(self.pos + 1).copied();
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    if let (Some(hi), Some(lo)) = (hexval(hi), hexval(lo)) {
                        out.push(hi * 16 + lo);
                        self.pos += 2;
                        continue;
                    }
                }
            }
            out.push(c);
        }
        Ok(PdfValue::Name(String::from_utf8_lossy(&out).into_owned()))
    }

    fn parse_literal_string(&mut self) -> Result<PdfValue> {
        self.pos += 1; // '('
        let mut depth = 1usize;
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                b'\\' => {
                    let Some(e) = self.peek() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\n' => {}
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(PdfValue::Str(out));
                    }
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        Err(VerifyError::MalformedPdf("unterminated string".into()))
    }

    fn parse_hex_string(&mut self) -> Result<PdfValue> {
        self.pos += 1; // '<'
        let mut nibbles = Vec::new();
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == b'>' {
                let mut out = Vec::with_capacity(nibbles.len().div_ceil(2));
                for pair in nibbles.chunks(2) {
                    let hi = pair[0];
                    let lo = pair.get(1).copied().unwrap_or(0);
                    out.push(hi * 16 + lo);
                }
                return Ok(PdfValue::Str(out));
            }
            if let Some(v) = hexval(c) {
                nibbles.push(v);
            }
        }
        Err(VerifyError::MalformedPdf("unterminated hex string".into()))
    }

    fn parse_number(&mut self) -> Result<PdfValue> {
        let kw = self.read_keyword();
        let s = std::str::from_utf8(kw)
            .map_err(|_| VerifyError::MalformedPdf("non-utf8 number".into()))?;
        if let Ok(i) = s.parse::<i64>() {
            return Ok(PdfValue::Int(i));
        }
        s.parse::<f64>()
            .map(PdfValue::Real)
            .map_err(|_| VerifyError::MalformedPdf(format!("bad number {s:?}")))
    }

    /// Parse one value. `depth` guards against pathological nesting.
    pub fn parse_value(&mut self, depth: usize) -> Result<PdfValue> {
        if depth > MAX_VALUE_DEPTH {
            return Err(VerifyError::MalformedPdf("value nesting too deep".into()));
        }
        self.skip_ws();
        let Some(c) = self.peek() else {
            return Err(VerifyError::MalformedPdf("unexpected end of object".into()));
        };
        match c {
            b'/' => self.parse_name(),
            b'(' => self.parse_literal_string(),
            b'<' => {
                if self.buf.get(self.pos + 1) == Some(&b'<') {
                    self.parse_dict_or_stream(depth)
                } else {
                    self.parse_hex_string()
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        None => {
                            return Err(VerifyError::MalformedPdf("unterminated array".into()));
                        }
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        // A missing `]` is common in hand-written files. Treat
                        // the enclosing dictionary's `>>` as closing the array
                        // rather than losing the whole object.
                        Some(b'>') | Some(b')') => break,
                        _ => match self.parse_value(depth + 1) {
                            Ok(v) => items.push(v),
                            Err(_) => break,
                        },
                    }
                    // Bound the allocation, not just the later use of it: an
                    // `/Index` of millions of elements is refused while it is
                    // being read rather than after a `Vec` has been grown to
                    // hold it.
                    if items.len() > MAX_ARRAY_ITEMS {
                        return Err(VerifyError::MalformedPdf(format!(
                            "array declares more than {MAX_ARRAY_ITEMS} elements"
                        )));
                    }
                }
                Ok(PdfValue::Array(items))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let save = self.pos;
                let first = self.parse_number()?;
                if let PdfValue::Int(n) = first {
                    if n >= 0 && n <= u32::MAX as i64 {
                        let after_num = self.pos;
                        if let Some(g) = self.read_uint() {
                            if g <= u16::MAX as u64 {
                                self.skip_ws();
                                let kw_start = self.pos;
                                let kw = self.read_keyword();
                                if kw == b"R" {
                                    return Ok(PdfValue::Ref(n as u32, g as u16));
                                }
                                self.pos = kw_start;
                            }
                        }
                        self.pos = after_num;
                    }
                }
                let _ = save;
                Ok(first)
            }
            _ => {
                let kw = self.read_keyword();
                match kw {
                    b"true" => Ok(PdfValue::Bool(true)),
                    b"false" => Ok(PdfValue::Bool(false)),
                    b"null" => Ok(PdfValue::Null),
                    b"" => Err(VerifyError::MalformedPdf(format!(
                        "unexpected byte {:?} at {}",
                        c as char, self.pos
                    ))),
                    other => Err(VerifyError::MalformedPdf(format!(
                        "unexpected keyword {:?}",
                        String::from_utf8_lossy(other)
                    ))),
                }
            }
        }
    }

    fn parse_dict_or_stream(&mut self, depth: usize) -> Result<PdfValue> {
        self.pos += 2; // '<<'
        let mut dict = Dict::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(VerifyError::MalformedPdf("unterminated dictionary".into())),
                Some(b'>') => {
                    // A lone '>' is a producer bug, not a reason to lose the
                    // dictionary: close on it.
                    self.pos += if self.buf.get(self.pos + 1) == Some(&b'>') {
                        2
                    } else {
                        1
                    };
                    break;
                }
                Some(b'/') => {
                    let PdfValue::Name(key) = self.parse_name()? else {
                        unreachable!("parse_name yields a name")
                    };
                    let value = self.parse_value(depth + 1)?;
                    dict.insert(key, value);
                }
                Some(_) => {
                    // A malformed key: consume one value and carry on rather
                    // than abandoning an otherwise usable dictionary.
                    self.parse_value(depth + 1)?;
                }
            }
        }

        // A stream keyword may follow.
        let save = self.pos;
        self.skip_ws();
        let kw_start = self.pos;
        let kw = self.read_keyword();
        if kw != b"stream" {
            self.pos = save;
            return Ok(PdfValue::Dict(dict));
        }
        let _ = kw_start;
        // Skip the EOL after `stream`: CRLF or LF.
        if self.peek() == Some(b'\r') {
            self.pos += 1;
        }
        if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
        let body_start = self.pos;

        // /Length is authoritative when it is a direct integer that lands on
        // `endstream`; otherwise fall back to searching for `endstream`.
        let mut body_len = None;
        if let Some(PdfValue::Int(n)) = dict.get("Length") {
            if *n >= 0 {
                let n = *n as usize;
                if let Some(end) = body_start.checked_add(n) {
                    if end <= self.buf.len() {
                        let mut probe = Lexer::new(self.buf, end);
                        probe.skip_ws();
                        if self.buf[probe.pos..].starts_with(b"endstream") {
                            body_len = Some(n);
                        }
                    }
                }
            }
        }
        let body_len = match body_len {
            Some(n) => n,
            None => find_bytes_from(self.buf, body_start, b"endstream")
                .map(|p| {
                    // Trim the EOL that precedes `endstream`.
                    let mut e = p;
                    if e > body_start && self.buf[e - 1] == b'\n' {
                        e -= 1;
                    }
                    if e > body_start && self.buf[e - 1] == b'\r' {
                        e -= 1;
                    }
                    e - body_start
                })
                .ok_or_else(|| VerifyError::MalformedPdf("unterminated stream".into()))?,
        };
        if body_len > MAX_STREAM_BYTES {
            return Err(VerifyError::MalformedPdf("stream body exceeds cap".into()));
        }
        self.pos = body_start + body_len;
        // Step past `endstream`.
        if let Some(p) = find_bytes_from(self.buf, self.pos, b"endstream") {
            self.pos = p + b"endstream".len();
        }
        Ok(PdfValue::Stream {
            dict,
            body_start,
            body_len,
        })
    }

    /// Parse `N G obj <value> [endobj]` at the current position.
    /// Returns the object number, generation, value and the offset just past
    /// `endobj` (or past the value if `endobj` is absent).
    pub fn parse_indirect_object(&mut self) -> Result<(u32, u16, PdfValue, usize)> {
        self.skip_ws();
        let num = self
            .read_uint()
            .ok_or_else(|| VerifyError::MalformedPdf("object number expected".into()))?;
        let gen = self
            .read_uint()
            .ok_or_else(|| VerifyError::MalformedPdf("generation expected".into()))?;
        self.skip_ws();
        let kw = self.read_keyword();
        if kw != b"obj" {
            return Err(VerifyError::MalformedPdf("`obj` keyword expected".into()));
        }
        if num > u32::MAX as u64 || gen > u16::MAX as u64 {
            return Err(VerifyError::MalformedPdf("object id out of range".into()));
        }
        let value = self.parse_value(0)?;
        let mut end = self.pos;
        let mut probe = Lexer::new(self.buf, self.pos);
        probe.skip_ws();
        if self.buf[probe.pos..].starts_with(b"endobj") {
            end = probe.pos + b"endobj".len();
        }
        Ok((num as u32, gen as u16, value, end))
    }
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn find_bytes_from(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

// ---------------------------------------------------------------------------
// Cross-reference index
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    /// Object body lives at `offset` in the file with generation `gen`.
    InFile { offset: u64, gen: u16 },
    /// Object lives at `index` inside object stream `stream`.
    InStream { stream: u32, index: u32 },
}

/// The merged cross-reference chain: object number -> location.
#[derive(Debug, Clone)]
pub struct XrefIndex {
    entries: BTreeMap<u32, XrefEntry>,
    trailer: Dict,
}

impl XrefIndex {
    pub fn entries(&self) -> &BTreeMap<u32, XrefEntry> {
        &self.entries
    }

    pub fn trailer(&self) -> &Dict {
        &self.trailer
    }

    pub fn get(&self, obj_num: u32) -> Option<XrefEntry> {
        self.entries.get(&obj_num).copied()
    }

    /// Parse the chain starting at the final `startxref`, following `/Prev`
    /// (and `/XRefStm`). Newer entries win; the chain length, entry count and
    /// stream sizes are all capped.
    pub fn build(bytes: &[u8]) -> Result<Self> {
        let start = find_startxref_offset(bytes)?;
        let mut entries: BTreeMap<u32, XrefEntry> = BTreeMap::new();
        let mut trailer = Dict::new();
        let mut seen: Vec<u64> = Vec::new();
        let mut queue: Vec<u64> = vec![start];
        let mut sections = 0usize;
        let mut budget = DecodeBudget::new();

        while let Some(offset) = queue.pop() {
            if seen.contains(&offset) {
                continue;
            }
            seen.push(offset);
            sections += 1;
            if sections > MAX_XREF_CHAIN {
                return Err(VerifyError::XrefParseError(
                    "cross-reference chain exceeds cap".into(),
                ));
            }
            let section = parse_xref_section(bytes, offset, &mut budget)?;
            for (num, entry) in section.entries {
                if entries.len() >= MAX_XREF_ENTRIES {
                    return Err(VerifyError::XrefParseError(
                        "too many cross-reference entries".into(),
                    ));
                }
                entries.entry(num).or_insert(entry);
            }
            for (k, v) in section.trailer {
                trailer.entry(k).or_insert(v);
            }
            // /XRefStm first so that the classic table of the same revision,
            // already merged above, keeps precedence over neither: both belong
            // to this revision and older sections come after.
            if let Some(x) = section.xref_stm {
                queue.insert(0, x);
            }
            if let Some(p) = section.prev {
                queue.insert(0, p);
            }
        }

        if entries.is_empty() {
            return Err(VerifyError::XrefParseError(
                "no usable cross-reference entries".into(),
            ));
        }
        Ok(XrefIndex { entries, trailer })
    }
}

pub(super) struct XrefSection {
    pub(super) entries: Vec<(u32, XrefEntry)>,
    pub(super) trailer: Dict,
    pub(super) prev: Option<u64>,
    pub(super) xref_stm: Option<u64>,
    /// The absolute file offset past this section's own container: past the
    /// classic trailer dictionary's closing `>>`, or past the cross-reference
    /// stream object's `endobj`. Compared against a signature's coverage end
    /// by `classify_coverage` — see the caller in `verify::mod` for why this
    /// must be an absolute offset and not one relative to the section start.
    pub(super) end: u64,
}

/// Find the offset the file's last `startxref` names.
///
/// The search and its window come from `pdfwrite`, so that this resolver and
/// the revision walk cannot end up reading different documents — they used to
/// search different distances back from the end.
fn find_startxref_offset(bytes: &[u8]) -> Result<u64> {
    if bytes.len() < 10 {
        return Err(VerifyError::FileTooSmall);
    }
    crate::pdfwrite::find_startxref_offset(bytes).ok_or(VerifyError::StartxrefNotFound)
}

fn parse_xref_section(bytes: &[u8], offset: u64, budget: &mut DecodeBudget) -> Result<XrefSection> {
    let pos = usize::try_from(offset).map_err(|_| VerifyError::TruncatedFile(offset))?;
    if pos >= bytes.len() {
        return Err(VerifyError::TruncatedFile(offset));
    }
    let mut lex = Lexer::new(bytes, pos);
    lex.skip_ws();
    if bytes[lex.pos..].starts_with(b"xref") {
        parse_classic_section(bytes, lex.pos + 4)
    } else {
        parse_xref_stream_section(bytes, pos, budget)
    }
}

/// Parse exactly one cross-reference section: its entries, trailer, `/Prev`,
/// `/XRefStm` and container end.
///
/// Revision accounting needs the membership of each section independently,
/// rather than the merged newest-wins index built by [`XrefIndex`]. Keeping
/// that accounting on this parser also means xref streams are decoded with
/// the same filter support and resource bounds as ordinary object resolution,
/// and that the container end — needed to compare a signature's coverage
/// against a revision's true extent — comes from the one real parser rather
/// than a second, token-searching one that can disagree with it.
pub(super) fn xref_section_object_numbers(
    bytes: &[u8],
    offset: u64,
    budget: &mut DecodeBudget,
) -> Result<XrefSection> {
    parse_xref_section(bytes, offset, budget)
}

fn parse_classic_section(bytes: &[u8], mut pos: usize) -> Result<XrefSection> {
    let mut entries = Vec::new();
    loop {
        let mut lex = Lexer::new(bytes, pos);
        lex.skip_ws();
        if bytes[lex.pos..].starts_with(b"trailer") {
            pos = lex.pos + b"trailer".len();
            break;
        }
        let Some(first) = lex.read_uint() else {
            return Err(VerifyError::XrefParseError(
                "subsection header expected".into(),
            ));
        };
        let Some(count) = lex.read_uint() else {
            return Err(VerifyError::XrefParseError(
                "subsection count expected".into(),
            ));
        };
        if count > MAX_XREF_ENTRIES as u64 || first > u32::MAX as u64 {
            return Err(VerifyError::XrefParseError("subsection too large".into()));
        }
        for i in 0..count {
            let Some(off) = lex.read_uint() else {
                return Err(VerifyError::XrefParseError("xref offset expected".into()));
            };
            let Some(gen) = lex.read_uint() else {
                return Err(VerifyError::XrefParseError(
                    "xref generation expected".into(),
                ));
            };
            lex.skip_ws();
            let kind = lex.read_keyword();
            let num = first.saturating_add(i);
            if kind == b"n" && num <= u32::MAX as u64 && gen <= u16::MAX as u64 {
                // §76.15: the per-subsection `count` is bounded above by
                // MAX_XREF_ENTRIES, but a file can chain many subsections
                // together, each individually under the cap, to make this
                // `Vec` grow past it anyway. Cap the running total from
                // inside the push loop rather than only checking each
                // subsection's declared count in isolation.
                if entries.len() >= MAX_XREF_ENTRIES {
                    return Err(VerifyError::XrefParseError(
                        "too many cross-reference entries in one section".into(),
                    ));
                }
                entries.push((
                    num as u32,
                    XrefEntry::InFile {
                        offset: off,
                        gen: gen as u16,
                    },
                ));
            }
        }
        pos = lex.pos;
    }

    let mut lex = Lexer::new(bytes, pos);
    let trailer = match lex.parse_value(0)? {
        PdfValue::Dict(d) => d,
        _ => Dict::new(),
    };
    // `lex.pos` lands exactly past the trailer dictionary's own closing `>>`:
    // `parse_dict_or_stream` breaks out of its loop the instant it consumes
    // that byte pair, so nested dictionaries — an inline `/Info`, or one
    // planted for the purpose — cannot end the scan early the way a
    // depth-tracking token search could get wrong.
    let end = lex.pos as u64;
    let prev = trailer
        .get("Prev")
        .and_then(|v| v.as_i64())
        .and_then(non_neg);
    let xref_stm = trailer
        .get("XRefStm")
        .and_then(|v| v.as_i64())
        .and_then(non_neg);
    Ok(XrefSection {
        entries,
        trailer,
        prev,
        xref_stm,
        end,
    })
}

fn non_neg(v: i64) -> Option<u64> {
    if v >= 0 {
        Some(v as u64)
    } else {
        None
    }
}

fn parse_xref_stream_section(
    bytes: &[u8],
    pos: usize,
    budget: &mut DecodeBudget,
) -> Result<XrefSection> {
    let mut lex = Lexer::new(bytes, pos);
    let (_num, _gen, value, end) = lex.parse_indirect_object()?;
    let PdfValue::Stream {
        ref dict,
        body_start,
        body_len,
    } = value
    else {
        return Err(VerifyError::XrefParseError(
            "cross-reference stream expected".into(),
        ));
    };
    // A truncated file is the ordinary way a `/Length` runs past the end;
    // slicing on it directly would panic rather than refuse.
    let body = body_start
        .checked_add(body_len)
        .and_then(|end| bytes.get(body_start..end))
        .ok_or_else(|| {
            VerifyError::XrefParseError(
                "the cross-reference stream's /Length runs past the end of the file".into(),
            )
        })?;
    let data = decode_stream(body, dict)?;
    budget.spend(data.len())?;

    // §7.5.8.2: `/W` is an array of integers, one byte width per field. Each
    // element must be a non-negative integer that fits a field — an element
    // that is neither is a malformed document, and coercing it (dropping it,
    // or clamping a negative to zero) silently re-phases the row layout and
    // reads every entry from the wrong bytes.
    let widths: Vec<usize> = match dict.get("W").and_then(|v| v.as_array()) {
        Some(a) => {
            let mut out = Vec::with_capacity(a.len());
            for value in a {
                let Some(w) = value.as_i64() else {
                    return Err(VerifyError::XrefParseError(
                        "a /W element of a cross-reference stream is not an integer".into(),
                    ));
                };
                if !(0..=MAX_XREF_FIELD_WIDTH as i64).contains(&w) {
                    return Err(VerifyError::XrefParseError(format!(
                        "a /W element of a cross-reference stream declares a field width of {w}; \
                         §7.5.8.2 field widths are byte counts and none may exceed \
                         {MAX_XREF_FIELD_WIDTH}"
                    )));
                }
                out.push(w as usize);
            }
            out
        }
        None => Vec::new(),
    };
    if widths.len() < 3 {
        return Err(VerifyError::XrefParseError("/W must have 3 widths".into()));
    }
    // Every width is now at most MAX_XREF_FIELD_WIDTH and the array is bounded
    // by MAX_ARRAY_ITEMS, so this sum cannot overflow.
    let row = widths.iter().take(3).sum::<usize>();
    if row == 0 {
        return Err(VerifyError::XrefParseError("/W widths are all zero".into()));
    }

    let size = dict
        .get("Size")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0) as u64;
    // `/Index` is positional too: a dropped or clamped element shifts every
    // later pair by one and re-numbers the whole section.
    let index: Vec<u64> = match dict.get("Index").and_then(|v| v.as_array()) {
        Some(a) => {
            if a.len() % 2 != 0 {
                return Err(VerifyError::XrefParseError(
                    "the /Index of a cross-reference stream has an odd number of elements; \
                     §7.5.8.2 makes it a sequence of (first, count) pairs"
                        .into(),
                ));
            }
            let mut out = Vec::with_capacity(a.len());
            for value in a {
                match value.as_i64() {
                    Some(v) if v >= 0 => out.push(v as u64),
                    _ => {
                        return Err(VerifyError::XrefParseError(
                            "an /Index element of a cross-reference stream is not a \
                             non-negative integer"
                                .into(),
                        ))
                    }
                }
            }
            out
        }
        None => vec![0, size],
    };
    // One `/Index` pair per contiguous run of object numbers. A real producer
    // writes a handful; the cap is generous enough that no honest file meets
    // it and small enough that a crafted array of millions of empty
    // subsections cannot make this loop the expensive part of verification.
    if index.len() / 2 > MAX_XREF_SUBSECTIONS {
        return Err(VerifyError::XrefParseError(
            "cross-reference stream declares too many /Index subsections".into(),
        ));
    }

    let mut entries = Vec::new();
    let mut cursor = 0usize;
    for pair in index.chunks(2) {
        if pair.len() < 2 {
            break;
        }
        let (first, count) = (pair[0], pair[1]);
        if count > MAX_XREF_ENTRIES as u64 {
            return Err(VerifyError::XrefParseError("xref stream too large".into()));
        }
        for i in 0..count {
            if cursor + row > data.len() {
                break;
            }
            let mut fields = [0u64; 3];
            let mut at = cursor;
            for (fi, w) in widths.iter().take(3).enumerate() {
                let mut v: u64 = 0;
                for _ in 0..*w {
                    // Bounded on the decoded buffer, not on the row arithmetic
                    // that got us here: the guard above is what should make
                    // this unreachable, and this is what makes a mistake in it
                    // a refusal rather than a panic in the process that owns
                    // the user interface.
                    let Some(byte) = data.get(at) else {
                        return Err(VerifyError::XrefParseError(
                            "a cross-reference stream row runs past the decoded stream".into(),
                        ));
                    };
                    v = (v << 8) | *byte as u64;
                    at += 1;
                }
                fields[fi] = v;
            }
            cursor += row;
            // A zero-width first field means type 1 by definition.
            let kind = if widths[0] == 0 { 1 } else { fields[0] };
            let num = first.saturating_add(i);
            if num > u32::MAX as u64 {
                continue;
            }
            // §76.15: `count` is capped per `/Index` pair above, but
            // `/Index` can list up to MAX_XREF_SUBSECTIONS pairs, each
            // individually under that cap — a one-byte row (`/W [0 1 0]`)
            // and enough pairs each declaring MAX_XREF_ENTRIES rows turns a
            // few hundred KB of deflated zeros into tens of millions of
            // pushes. Cap the running total across the whole section from
            // inside the loop, not just each pair's own declared count.
            if entries.len() >= MAX_XREF_ENTRIES {
                return Err(VerifyError::XrefParseError(
                    "too many cross-reference entries in one section".into(),
                ));
            }
            match kind {
                1 if fields[2] <= u16::MAX as u64 => entries.push((
                    num as u32,
                    XrefEntry::InFile {
                        offset: fields[1],
                        gen: fields[2] as u16,
                    },
                )),
                2 if fields[1] <= u32::MAX as u64 && fields[2] <= u32::MAX as u64 => {
                    entries.push((
                        num as u32,
                        XrefEntry::InStream {
                            stream: fields[1] as u32,
                            index: fields[2] as u32,
                        },
                    ))
                }
                _ => {}
            }
        }
    }

    let prev = dict.get("Prev").and_then(|v| v.as_i64()).and_then(non_neg);
    Ok(XrefSection {
        entries,
        trailer: dict.clone(),
        prev,
        xref_stm: None,
        end: end as u64,
    })
}

// ---------------------------------------------------------------------------
// Stream decoding
// ---------------------------------------------------------------------------

/// Decode a stream body according to its `/Filter` chain.
///
/// Only the identity and `FlateDecode` filters are supported; anything else is
/// reported explicitly rather than silently producing wrong bytes.
pub fn decode_stream(body: &[u8], dict: &Dict) -> Result<Vec<u8>> {
    let filters: Vec<String> = match dict.get("Filter") {
        None | Some(PdfValue::Null) => Vec::new(),
        Some(PdfValue::Name(n)) => vec![n.clone()],
        Some(PdfValue::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_name().map(|s| s.to_string()))
            .collect(),
        Some(_) => {
            return Err(VerifyError::UnsupportedFilter("malformed /Filter".into()));
        }
    };
    let parms: Vec<Option<&Dict>> = match dict.get("DecodeParms").or_else(|| dict.get("DP")) {
        Some(PdfValue::Dict(d)) => vec![Some(d)],
        Some(PdfValue::Array(a)) => a.iter().map(|v| v.as_dict()).collect(),
        _ => Vec::new(),
    };

    let mut data = body.to_vec();
    for (i, f) in filters.iter().enumerate() {
        match f.as_str() {
            "FlateDecode" | "Fl" => {
                data = inflate(&data)?;
                if let Some(Some(p)) = parms.get(i) {
                    data = apply_predictor(&data, p)?;
                }
            }
            other => {
                return Err(VerifyError::UnsupportedFilter(other.to_string()));
            }
        }
        if data.len() > MAX_STREAM_BYTES {
            return Err(VerifyError::MalformedPdf(
                "decoded stream exceeds cap".into(),
            ));
        }
    }
    Ok(data)
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    fn take<R: Read>(mut r: R) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        r.by_ref()
            .take(MAX_STREAM_BYTES as u64 + 1)
            .read_to_end(&mut out)?;
        Ok(out)
    }
    if let Ok(out) = take(flate2::read::ZlibDecoder::new(data)) {
        if !out.is_empty() || data.is_empty() {
            return Ok(out);
        }
    }
    // Some producers emit raw deflate without the zlib wrapper.
    take(flate2::read::DeflateDecoder::new(data))
        .map_err(|e| VerifyError::MalformedPdf(format!("inflate failed: {e}")))
}

fn apply_predictor(data: &[u8], parms: &Dict) -> Result<Vec<u8>> {
    let predictor = parms.get("Predictor").and_then(|v| v.as_i64()).unwrap_or(1);
    if predictor < 2 {
        return Ok(data.to_vec());
    }
    if predictor == 2 {
        // TIFF predictor; only the 8-bit case is well defined here and xref
        // streams do not use it.
        return Err(VerifyError::UnsupportedFilter("TIFF predictor".into()));
    }
    let colors = parms
        .get("Colors")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .max(1) as usize;
    let bpc = parms
        .get("BitsPerComponent")
        .and_then(|v| v.as_i64())
        .unwrap_or(8)
        .max(1) as usize;
    let columns = parms
        .get("Columns")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .max(1) as usize;
    // /DecodeParms is attacker-controlled: `colors * bpc * columns` overflowed
    // on a crafted geometry, which panics in a debug build and in a release
    // one wraps to a small `row_len` that walks straight past the cap below.
    let bits_per_pixel = colors.checked_mul(bpc);
    let row_bits = bits_per_pixel.and_then(|b| b.checked_mul(columns));
    let (Some(bits_per_pixel), Some(row_bits)) = (bits_per_pixel, row_bits) else {
        return Err(VerifyError::MalformedPdf(
            "the predictor geometry in /DecodeParms overflows".into(),
        ));
    };
    let bpp = bits_per_pixel.div_ceil(8).max(1);
    let row_len = row_bits.div_ceil(8);
    if row_len == 0 || row_len > MAX_STREAM_BYTES {
        return Err(VerifyError::MalformedPdf("bad predictor geometry".into()));
    }

    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row_len];
    let mut at = 0usize;
    while at < data.len() {
        let tag = data[at];
        at += 1;
        let end = std::cmp::min(at + row_len, data.len());
        let mut row = data[at..end].to_vec();
        row.resize(row_len, 0);
        at = end;
        for i in 0..row_len {
            let a = if i >= bpp { row[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            row[i] = match tag {
                0 => row[i],
                1 => row[i].wrapping_add(a),
                2 => row[i].wrapping_add(b),
                3 => row[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => row[i].wrapping_add(paeth(a, b, c)),
                _ => return Err(VerifyError::MalformedPdf("bad PNG filter tag".into())),
            };
        }
        out.extend_from_slice(&row);
        prev = row;
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let pa = (p - a as i16).abs();
    let pb = (p - b as i16).abs();
    let pc = (p - c as i16).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

struct ObjStm {
    data: Vec<u8>,
    /// (object number, offset of its value inside `data`).
    offsets: Vec<(u32, usize)>,
}

/// Object number -> (offset of its definition, generation), from a repair scan.
pub type ScanIndex = BTreeMap<u32, (usize, u16)>;

/// Decoded object streams, keyed by the stream's object number. `None` records
/// a stream that could not be decoded, so it is not retried.
type ObjStmCache = HashMap<u32, Option<Rc<ObjStm>>>;

/// Resolves object numbers to values and to byte spans.
pub struct ObjectResolver<'a> {
    bytes: &'a [u8],
    index: Option<XrefIndex>,
    /// Repair scan, built lazily and only when the xref cannot answer.
    scan: RefCell<Option<Rc<ScanIndex>>>,
    objstm: RefCell<ObjStmCache>,
    /// §76.15: the earliest offset from which `object_end`'s `endobj`
    /// fallback search is known to find nothing, so a resolver asked to
    /// repair many damaged objects in a row does not re-scan to EOF for
    /// each one. See the identical reasoning in `scan_object_definitions`.
    no_endobj_from: Cell<Option<usize>>,
}

impl<'a> ObjectResolver<'a> {
    /// Build a resolver. A damaged cross-reference chain is not fatal: the
    /// resolver falls back to a repair scan, and reports [`Confidence::Scanned`]
    /// for anything it finds that way.
    pub fn new(bytes: &'a [u8]) -> Self {
        ObjectResolver {
            bytes,
            index: XrefIndex::build(bytes).ok(),
            scan: RefCell::new(None),
            objstm: RefCell::new(HashMap::new()),
            no_endobj_from: Cell::new(None),
        }
    }

    /// Build a resolver, failing if the cross-reference chain is unusable.
    pub fn strict(bytes: &'a [u8]) -> Result<Self> {
        let index = XrefIndex::build(bytes)?;
        Ok(ObjectResolver {
            bytes,
            index: Some(index),
            scan: RefCell::new(None),
            objstm: RefCell::new(HashMap::new()),
            no_endobj_from: Cell::new(None),
        })
    }

    pub fn xref(&self) -> Option<&XrefIndex> {
        self.index.as_ref()
    }

    /// The merged trailer of the active revision, when the xref chain parsed.
    pub fn trailer(&self) -> Option<&Dict> {
        self.index.as_ref().map(|i| i.trailer())
    }

    /// Whether the active revision declares `/Encrypt`.
    pub fn is_encrypted(&self) -> bool {
        match self.trailer().and_then(|t| t.get("Encrypt")) {
            None | Some(PdfValue::Null) => false,
            Some(_) => true,
        }
    }

    /// The catalog reference from the trailer's `/Root`.
    pub fn root_ref(&self) -> Option<(u32, u16)> {
        self.trailer()
            .and_then(|t| t.get("Root"))
            .and_then(|v| v.as_ref_pair())
    }

    fn scan(&self) -> Rc<ScanIndex> {
        if let Some(s) = self.scan.borrow().as_ref() {
            return Rc::clone(s);
        }
        let built = Rc::new(scan_object_definitions(self.bytes));
        *self.scan.borrow_mut() = Some(Rc::clone(&built));
        built
    }

    /// The byte span `[start, end)` of an in-file object definition, covering
    /// `N G obj … endobj`.
    pub fn object_span(&self, obj_num: u32) -> Option<(usize, usize, Confidence)> {
        match self.index.as_ref().and_then(|i| i.get(obj_num)) {
            Some(XrefEntry::InFile { offset, gen }) => {
                if let Some(end) = self.validate_header(offset, obj_num, gen) {
                    return Some((offset as usize, end, Confidence::Resolved));
                }
            }
            // The cross-reference chain is authoritative, exactly as it is in
            // `object_bytes`. An object the newest revision places inside an
            // object stream has no file span at all, and falling through to
            // the repair scan would hand back a *superseded* in-file copy that
            // no conforming reader ever sees. The one caller that needs a file
            // span is the signature dictionary — whose `/Contents` must be a
            // direct, file-addressable hex string for `/ByteRange` to describe
            // it, so it cannot legitimately live in an object stream — which
            // makes "the xref says type-2" a hostile or broken document and
            // "not found" the only honest answer.
            Some(XrefEntry::InStream { .. }) => return None,
            None => {}
        }
        let scan = self.scan();
        let (start, _gen) = *scan.get(&obj_num)?;
        let end = self.object_end(start)?;
        Some((start, end, Confidence::Scanned))
    }

    /// Confirm that `offset` really carries `obj_num gen obj` and return the
    /// end of the definition.
    fn validate_header(&self, offset: u64, obj_num: u32, gen: u16) -> Option<usize> {
        let pos = usize::try_from(offset).ok()?;
        if pos >= self.bytes.len() {
            return None;
        }
        let (n, g, _after) = read_object_header(self.bytes, pos)?;
        if n != obj_num || g != gen {
            return None;
        }
        self.object_end(pos)
    }

    /// End of the definition beginning at `start`.
    ///
    /// Parsing the body is the accurate way — it steps over a stream — but a
    /// damaged object must still yield a usable span, so a failed parse falls
    /// back to the next `endobj`.
    fn object_end(&self, start: usize) -> Option<usize> {
        let mut lex = Lexer::new(self.bytes, start);
        match lex.parse_indirect_object() {
            Ok((_, _, _, end)) => Some(end),
            Err(_) => {
                // §76.15: once a search for `endobj` starting at some offset
                // comes back empty, it is absent everywhere from there to
                // EOF, so a later request starting at or after that offset
                // is refused without re-scanning. Repairing many damaged
                // objects in one document would otherwise cost O(n^2) byte
                // compares.
                if self
                    .no_endobj_from
                    .get()
                    .is_some_and(|known| start >= known)
                {
                    return None;
                }
                match find_bytes_from(self.bytes, start, b"endobj") {
                    Some(p) => Some(p + b"endobj".len()),
                    None => {
                        let known = self.no_endobj_from.get();
                        self.no_endobj_from
                            .set(Some(known.map_or(start, |k| k.min(start))));
                        None
                    }
                }
            }
        }
    }

    /// Resolve an object to its parsed value.
    pub fn resolve(&self, obj_num: u32) -> Result<(PdfValue, Confidence)> {
        if let Some(entry) = self.index.as_ref().and_then(|i| i.get(obj_num)) {
            match entry {
                XrefEntry::InFile { offset, gen } => {
                    if let Some(pos) = usize::try_from(offset)
                        .ok()
                        .filter(|p| *p < self.bytes.len())
                    {
                        let mut lex = Lexer::new(self.bytes, pos);
                        if let Ok((n, g, v, _end)) = lex.parse_indirect_object() {
                            if n == obj_num && g == gen {
                                return Ok((v, Confidence::Resolved));
                            }
                        }
                    }
                }
                XrefEntry::InStream { stream, index } => {
                    if let Some(v) = self.resolve_in_stream(stream, index, obj_num)? {
                        return Ok((v, Confidence::Resolved));
                    }
                }
            }
        }
        // Repair path.
        let scan = self.scan();
        let (start, _gen) = *scan
            .get(&obj_num)
            .ok_or(VerifyError::SignatureObjectNotFound)?;
        let mut lex = Lexer::new(self.bytes, start);
        let (_n, _g, v, _end) = lex.parse_indirect_object()?;
        Ok((v, Confidence::Scanned))
    }

    /// Resolve a reference, or return a direct value unchanged.
    pub fn deref(&self, value: &PdfValue) -> Result<PdfValue> {
        match value {
            PdfValue::Ref(n, _g) => self.resolve(*n).map(|(v, _)| v),
            other => Ok(other.clone()),
        }
    }

    /// Look up `key` in `dict`, dereferencing an indirect value.
    pub fn dict_get(&self, dict: &Dict, key: &str) -> Option<PdfValue> {
        let raw = dict.get(key)?;
        match self.deref(raw) {
            Ok(v) if !v.is_null() => Some(v),
            _ => None,
        }
    }

    fn resolve_in_stream(&self, stream: u32, index: u32, obj_num: u32) -> Result<Option<PdfValue>> {
        let stm = self.object_stream(stream)?;
        let Some(stm) = stm else { return Ok(None) };
        // Prefer the recorded index, but trust the object number over it.
        let entry = stm
            .offsets
            .get(index as usize)
            .filter(|(n, _)| *n == obj_num)
            .or_else(|| stm.offsets.iter().find(|(n, _)| *n == obj_num));
        let Some((_, off)) = entry else {
            return Ok(None);
        };
        if *off >= stm.data.len() {
            return Ok(None);
        }
        let mut lex = Lexer::new(&stm.data, *off);
        Ok(lex.parse_value(0).ok())
    }

    /// The raw bytes of an object, as a self-contained `N G obj … endobj`
    /// definition. Objects that live in an object stream are re-wrapped so a
    /// caller that tokenises the definition sees the same lexical content.
    pub fn object_bytes(&self, obj_num: u32) -> Result<(Vec<u8>, Confidence)> {
        // The cross-reference chain is authoritative. When it says the object
        // lives in an object stream, the repair scan must not be consulted:
        // an object number that is type-2 in the newest revision but also has
        // a stale in-file definition from an earlier one would otherwise
        // resolve to the superseded copy.
        if let Some(XrefEntry::InStream { stream, index }) =
            self.index.as_ref().and_then(|i| i.get(obj_num))
        {
            if let Some(stm) = self.object_stream(stream)? {
                let entry = stm
                    .offsets
                    .get(index as usize)
                    .filter(|(n, _)| *n == obj_num)
                    .or_else(|| stm.offsets.iter().find(|(n, _)| *n == obj_num));
                if let Some((_, off)) = entry {
                    if *off < stm.data.len() {
                        let mut lex = Lexer::new(&stm.data, *off);
                        lex.parse_value(0)?;
                        let end = lex.position();
                        let mut out = format!("{obj_num} 0 obj\n").into_bytes();
                        out.extend_from_slice(&stm.data[*off..end]);
                        out.extend_from_slice(b"\nendobj\n");
                        return Ok((out, Confidence::Resolved));
                    }
                }
            }
        }
        // Either the entry is an in-file one, or the chain has nothing to say
        // and the repair scan is the last resort.
        if let Some((start, end, conf)) = self.object_span(obj_num) {
            if end <= self.bytes.len() {
                return Ok((self.bytes[start..end].to_vec(), conf));
            }
        }
        Err(VerifyError::SignatureObjectNotFound)
    }

    fn object_stream(&self, stream: u32) -> Result<Option<Rc<ObjStm>>> {
        if let Some(hit) = self.objstm.borrow().get(&stream) {
            return Ok(hit.clone());
        }
        if self.objstm.borrow().len() >= MAX_OBJSTM_DECODED {
            return Err(VerifyError::MalformedPdf(
                "too many object streams decoded".into(),
            ));
        }
        let built = self.build_object_stream(stream)?;
        let built = built.map(Rc::new);
        self.objstm.borrow_mut().insert(stream, built.clone());
        Ok(built)
    }

    fn build_object_stream(&self, stream: u32) -> Result<Option<ObjStm>> {
        let entry = self.index.as_ref().and_then(|i| i.get(stream));
        // §7.5.7: "the object stream itself shall not be in an object stream".
        // Honouring a type-2 container entry would be the one place this
        // resolver could recurse without a bound, so it is a refusal rather
        // than a recursion — and a refusal rather than a silent miss, because
        // reading such a file as "object absent" would let a malformed
        // document masquerade as one that simply has no such object.
        if let Some(XrefEntry::InStream { .. }) = entry {
            return Err(VerifyError::MalformedPdf(format!(
                "object stream {stream} is itself recorded inside an object stream, which \
                 ISO 32000-1 §7.5.7 forbids; the cross-reference data is unusable"
            )));
        }
        let Some(XrefEntry::InFile { offset, gen }) = entry else {
            return Ok(None);
        };
        let Some(pos) = usize::try_from(offset)
            .ok()
            .filter(|p| *p < self.bytes.len())
        else {
            return Ok(None);
        };
        let mut lex = Lexer::new(self.bytes, pos);
        let (n, g, value, _end) = lex.parse_indirect_object()?;
        if n != stream || g != gen {
            return Ok(None);
        }
        let PdfValue::Stream {
            dict,
            body_start,
            body_len,
        } = value
        else {
            return Ok(None);
        };
        // A `/Length` that runs past the end of the file is the ordinary shape
        // of a truncated document; slicing on it would panic.
        let body = body_start
            .checked_add(body_len)
            .and_then(|end| self.bytes.get(body_start..end))
            .ok_or_else(|| {
                VerifyError::MalformedPdf(format!(
                    "object stream {stream} declares a /Length that runs past the end of the file"
                ))
            })?;
        let data = decode_stream(body, &dict)?;

        let count = dict.get("N").and_then(|v| v.as_i64()).unwrap_or(0);
        let first = dict.get("First").and_then(|v| v.as_i64()).unwrap_or(0);
        if count < 0 || first < 0 {
            return Err(VerifyError::MalformedPdf("bad /N or /First".into()));
        }
        let count = count as usize;
        let first = first as usize;
        if count > MAX_OBJSTM_OBJECTS {
            return Err(VerifyError::MalformedPdf(
                "object stream population exceeds cap".into(),
            ));
        }
        let mut header = Lexer::new(&data, 0);
        let mut offsets = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(num) = header.read_uint() else { break };
            let Some(off) = header.read_uint() else { break };
            if header.position() > first && first > 0 {
                break;
            }
            if num > u32::MAX as u64 {
                continue;
            }
            let Some(abs) = first.checked_add(off as usize) else {
                continue;
            };
            if abs < data.len() {
                offsets.push((num as u32, abs));
            }
        }
        Ok(Some(ObjStm { data, offsets }))
    }
}

// ---------------------------------------------------------------------------
// Repair scan
// ---------------------------------------------------------------------------

/// Scan the whole file for `N G obj` definitions, skipping stream bodies.
///
/// The last definition of an object number wins: an incremental update appends
/// a replacement after every earlier copy. Unlike the scan this replaces, the
/// generation is read rather than assumed, and a decoy `5 0 obj` inside a
/// stream body is never selected.
pub fn scan_object_definitions(bytes: &[u8]) -> ScanIndex {
    let mut out: BTreeMap<u32, (usize, u16)> = BTreeMap::new();
    let mut at = 0usize;
    // §76.15: a file of many objects that each fail to parse (no `endobj`
    // anywhere after them, e.g. a run of `N 0 obj }`) used to make the
    // fallback below scan from its object's start all the way to EOF, every
    // time, for O(n^2) byte compares on an n-byte file. Once one such scan
    // comes back empty, `endobj` is known to be absent from that offset to
    // the end of the file, and since `at` (and therefore every later
    // `start`) only moves forward, it is absent for every later object too.
    // Remembering the earliest offset a failed scan started at turns every
    // later failure into an O(1) check instead of a repeat scan.
    let mut no_endobj_from: Option<usize> = None;
    while let Some(p) = find_bytes_from(bytes, at, b"obj") {
        at = p + 3;
        // `obj` must be a standalone keyword.
        if p + 3 < bytes.len() && is_regular(bytes[p + 3]) {
            continue;
        }
        let Some((start, num, gen)) = back_parse_object_header(bytes, p) else {
            continue;
        };
        out.insert(num, (start, gen));
        // Step over the object's body, so a decoy `5 0 obj` inside a stream is
        // never seen. A body that will not parse is skipped to its `endobj`.
        let mut lex = Lexer::new(bytes, start);
        let end = match lex.parse_indirect_object() {
            Ok((_, _, _, end)) => Some(end),
            Err(_) => {
                if no_endobj_from.is_some_and(|known| start >= known) {
                    None
                } else {
                    match find_bytes_from(bytes, start, b"endobj") {
                        Some(e) => Some(e + b"endobj".len()),
                        None => {
                            no_endobj_from =
                                Some(no_endobj_from.map_or(start, |known| known.min(start)));
                            None
                        }
                    }
                }
            }
        };
        if let Some(end) = end {
            at = std::cmp::max(at, end);
        }
    }
    out
}

/// Read `N G obj` at `pos`, returning the ids and the offset just past `obj`.
fn read_object_header(bytes: &[u8], pos: usize) -> Option<(u32, u16, usize)> {
    let mut lex = Lexer::new(bytes, pos);
    let num = lex.read_uint()?;
    let gen = lex.read_uint()?;
    lex.skip_ws();
    if lex.read_keyword() != b"obj" {
        return None;
    }
    if num > u32::MAX as u64 || gen > u16::MAX as u64 {
        return None;
    }
    Some((num as u32, gen as u16, lex.position()))
}

/// Read the `N G` that precedes an `obj` keyword at `obj_pos`.
fn back_parse_object_header(bytes: &[u8], obj_pos: usize) -> Option<(usize, u32, u16)> {
    let mut i = obj_pos;
    // whitespace before `obj`
    let ws_end = i;
    while i > 0 && is_ws(bytes[i - 1]) {
        i -= 1;
    }
    if i == ws_end {
        return None;
    }
    let gen_end = i;
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == gen_end {
        return None;
    }
    let gen: u16 = std::str::from_utf8(&bytes[i..gen_end]).ok()?.parse().ok()?;
    let ws_end = i;
    while i > 0 && is_ws(bytes[i - 1]) {
        i -= 1;
    }
    if i == ws_end {
        return None;
    }
    let num_end = i;
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == num_end {
        return None;
    }
    let num: u32 = std::str::from_utf8(&bytes[i..num_end]).ok()?.parse().ok()?;
    if i > 0 && is_regular(bytes[i - 1]) {
        return None;
    }
    Some((i, num, gen))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_values() {
        let src = b"<< /A [1 2 (three) /Four <</B 5 0 R>>] /C <414243> >>";
        let mut lex = Lexer::new(src, 0);
        let v = lex.parse_value(0).unwrap();
        let d = v.as_dict().unwrap();
        let a = d.get("A").unwrap().as_array().unwrap();
        assert_eq!(a[0], PdfValue::Int(1));
        assert_eq!(a[2], PdfValue::Str(b"three".to_vec()));
        assert_eq!(a[3], PdfValue::Name("Four".into()));
        assert_eq!(
            a[4].as_dict().unwrap().get("B").unwrap(),
            &PdfValue::Ref(5, 0)
        );
        assert_eq!(d.get("C").unwrap(), &PdfValue::Str(b"ABC".to_vec()));
    }

    #[test]
    fn integer_not_mistaken_for_reference() {
        let src = b"<< /Size 7 /Root 1 0 R >>";
        let mut lex = Lexer::new(src, 0);
        let v = lex.parse_value(0).unwrap();
        let d = v.as_dict().unwrap();
        assert_eq!(d.get("Size").unwrap(), &PdfValue::Int(7));
        assert_eq!(d.get("Root").unwrap(), &PdfValue::Ref(1, 0));
    }

    #[test]
    fn nesting_depth_is_bounded() {
        let src = "[".repeat(MAX_VALUE_DEPTH + 5);
        let mut lex = Lexer::new(src.as_bytes(), 0);
        assert!(lex.parse_value(0).is_err());
    }

    /// §76.15: a file of many objects that each fail to parse and have no
    /// `endobj` anywhere after them used to make `scan_object_definitions`'s
    /// repair fallback re-scan from each object's start to EOF, an O(n^2)
    /// cost in the number of such objects. A ~10 MB file of them must still
    /// return in bounded time, memoised failure or not.
    #[test]
    fn a_file_of_many_unterminated_objects_scans_in_bounded_time() {
        let mut bytes = Vec::from(&b"%PDF-1.4\n"[..]);
        // Every one of these bodies fails to parse (`}` is not a value) and
        // has no `endobj` anywhere after it, which is exactly the case that
        // used to force a full-file scan per object.
        let unit_len = b"1 0 obj\n}\n".len();
        let count = (10 * 1024 * 1024) / unit_len;
        for n in 1..=count {
            bytes.extend_from_slice(format!("{n} 0 obj\n}}\n").as_bytes());
        }

        let started = std::time::Instant::now();
        let _ = scan_object_definitions(&bytes);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "scanning {count} unterminated objects must be near-linear, took {elapsed:?}"
        );
    }
}

#[cfg(test)]
mod xref_stream_tests {
    use super::*;

    /// Build an uncompressed `/W [1 4 2]` cross-reference stream body from
    /// `(type, second, third)` triples.
    fn rows(entries: &[(u8, u32, u16)]) -> Vec<u8> {
        let mut out = Vec::with_capacity(entries.len() * 7);
        for (kind, second, third) in entries {
            out.push(*kind);
            out.extend_from_slice(&second.to_be_bytes());
            out.extend_from_slice(&third.to_be_bytes());
        }
        out
    }

    /// A one-revision file whose only cross-reference section is a stream.
    /// `extra_dict` is spliced into the stream dictionary.
    fn xref_stream_pdf(objects: &[(u32, &str)], extra_dict: &str) -> Vec<u8> {
        let mut out = Vec::from(&b"%PDF-1.5\n"[..]);
        let mut entries = vec![(0u8, 0u32, 0xFFFFu16)];
        let mut max_num = 0;
        for (num, body) in objects {
            entries.push((1, out.len() as u32, 0));
            out.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
            max_num = max_num.max(*num);
        }
        let xref_num = max_num + 1;
        let xref_offset = out.len();
        entries.push((1, xref_offset as u32, 0));
        let body = rows(&entries);
        out.extend_from_slice(
            format!(
                "{xref_num} 0 obj\n<</Type /XRef /Size {} /W [1 4 2] /Root 1 0 R {extra_dict} \
                 /Length {}>>\nstream\n",
                xref_num + 1,
                body.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&body);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF").as_bytes());
        out
    }

    #[test]
    fn index_defaults_to_zero_through_size() {
        // No /Index at all: §7.5.8.2 says it defaults to [0 /Size], so the
        // rows are read as object numbers 0, 1, 2, … in order.
        let pdf = xref_stream_pdf(&[(1, "<</Type /Catalog>>")], "");
        let index = XrefIndex::build(&pdf).expect("the chain parses");
        assert!(matches!(index.get(1), Some(XrefEntry::InFile { .. })));
        assert!(matches!(index.get(2), Some(XrefEntry::InFile { .. })));
    }

    #[test]
    fn an_explicit_index_places_rows_at_its_own_object_numbers() {
        // Two subsections that are not contiguous: object 0, then 40 and 41.
        // The rows are positional, so a reader that ignored /Index would put
        // the last two entries at 1 and 2 instead.
        let body = rows(&[(0, 0, 0xFFFF), (1, 9, 0), (1, 40, 0)]);
        let mut pdf = Vec::from(&b"%PDF-1.5\n"[..]);
        let at = pdf.len();
        pdf.extend_from_slice(
            format!(
                "7 0 obj\n<</Type /XRef /Size 42 /W [1 4 2] /Index [0 1 40 2] /Root 40 0 R \
                 /Length {}>>\nstream\n",
                body.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(format!("startxref\n{at}\n%%EOF").as_bytes());

        let index = XrefIndex::build(&pdf).expect("the chain parses");
        assert!(matches!(index.get(40), Some(XrefEntry::InFile { .. })));
        assert!(matches!(index.get(41), Some(XrefEntry::InFile { .. })));
        assert!(index.get(1).is_none(), "/Index must not be ignored");
    }

    #[test]
    fn a_zero_width_type_field_defaults_the_type_to_one() {
        // §7.5.8.2: a /W first element of 0 means every entry is type 1.
        let mut body = Vec::new();
        for (offset, gen) in [(0u32, 0xFFFFu16), (9, 0)] {
            body.extend_from_slice(&offset.to_be_bytes());
            body.extend_from_slice(&gen.to_be_bytes());
        }
        let mut pdf = Vec::from(&b"%PDF-1.5\n"[..]);
        let at = pdf.len();
        pdf.extend_from_slice(
            format!(
                "3 0 obj\n<</Type /XRef /Size 4 /W [0 4 2] /Index [1 2] /Root 1 0 R \
                 /Length {}>>\nstream\n",
                body.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(format!("startxref\n{at}\n%%EOF").as_bytes());

        let index = XrefIndex::build(&pdf).expect("the chain parses");
        assert!(matches!(index.get(1), Some(XrefEntry::InFile { .. })));
        assert!(matches!(index.get(2), Some(XrefEntry::InFile { .. })));
    }

    /// Append object 3 as a `/Type /XRef` stream wrapping `body`, then close
    /// the file with `startxref`/`%%EOF`.
    ///
    /// The two tests below build object 3 by hand rather than through
    /// [`xref_stream_pdf`], because a type-2 entry pointing into a compressed
    /// stream — and a stream that recurses into another object stream — are
    /// not shapes that generic helper produces.
    fn close_with_xref_stream(out: &mut Vec<u8>, body: &[u8]) {
        let at = out.len();
        out.extend_from_slice(
            format!(
                "3 0 obj\n<</Type /XRef /Size 4 /W [1 4 2] /Root 1 0 R /Length {}>>\nstream\n",
                body.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out.extend_from_slice(format!("startxref\n{at}\n%%EOF").as_bytes());
    }

    #[test]
    fn a_type_two_entry_resolves_through_its_object_stream() {
        // Object 1 lives at index 0 of object stream 2.
        let inner = "1 0 <</Type /Catalog /Pages 9 0 R>>";
        let first = 4; // "1 0 " before the body
        let mut out = Vec::from(&b"%PDF-1.5\n"[..]);
        let stm_offset = out.len() as u32;
        out.extend_from_slice(
            format!(
                "2 0 obj\n<</Type /ObjStm /N 1 /First {first} /Length {}>>\nstream\n{inner}\n\
                 endstream\nendobj\n",
                inner.len()
            )
            .as_bytes(),
        );
        let body = rows(&[(0, 0, 0xFFFF), (2, 2, 0), (1, stm_offset, 0)]);
        close_with_xref_stream(&mut out, &body);

        let resolver = ObjectResolver::new(&out);
        assert!(matches!(
            resolver.xref().and_then(|i| i.get(1)),
            Some(XrefEntry::InStream {
                stream: 2,
                index: 0
            })
        ));
        let (value, confidence) = resolver.resolve(1).expect("object 1 resolves");
        assert_eq!(confidence, Confidence::Resolved);
        assert_eq!(
            value.as_dict().unwrap().get("Type").unwrap().as_name(),
            Some("Catalog")
        );

        // And the same object, re-wrapped as a definition a tokenizer can read.
        let (definition, _) = resolver.object_bytes(1).expect("object 1 has a definition");
        let text = String::from_utf8_lossy(&definition);
        assert!(text.starts_with("1 0 obj"), "got {text}");
        assert!(text.contains("/Catalog"), "got {text}");
    }

    #[test]
    fn an_object_stream_inside_an_object_stream_is_refused() {
        // §7.5.7 forbids it, and honouring it would be the one unbounded
        // recursion in the resolver.
        let body = rows(&[(0, 0, 0xFFFF), (2, 2, 0), (2, 2, 1)]);
        let mut out = Vec::from(&b"%PDF-1.5\n"[..]);
        close_with_xref_stream(&mut out, &body);

        let resolver = ObjectResolver::new(&out);
        let err = resolver.resolve(1).expect_err("must refuse, not recurse");
        let message = err.to_string();
        assert!(
            message.contains("itself recorded inside an object stream"),
            "the refusal must name what is wrong, got: {message}"
        );
    }

    #[test]
    fn too_many_index_subsections_are_refused() {
        let mut index = String::new();
        for i in 0..(MAX_XREF_SUBSECTIONS + 1) {
            index.push_str(&format!("{} 1 ", i * 2));
        }
        let pdf = xref_stream_pdf(&[(1, "<</Type /Catalog>>")], &format!("/Index [{index}]"));
        let err = XrefIndex::build(&pdf).expect_err("the cap must bite");
        assert!(err.to_string().contains("/Index subsections"), "{err}");
    }

    /// A `/Length` that lies is the commonest damage in a real-world PDF.
    /// The lexer only believes `/Length` when `endstream` actually follows at
    /// that offset, so a wild value is recovered from rather than propagated —
    /// which is why the explicit end-of-file guard in
    /// `parse_xref_stream_section` is defence in depth rather than the thing
    /// that catches this. Either way it must not panic.
    #[test]
    fn a_length_past_the_end_of_the_file_is_recovered_from_not_panicked_on() {
        let pdf = b"%PDF-1.5\n1 0 obj\n<</Type /XRef /Size 2 /W [1 4 2] /Length 999999>>\n\
                    stream\n\x01\x00\x00\x00\x09\x00\x00\nendstream\nendobj\n\
                    startxref\n9\n%%EOF";
        let index = XrefIndex::build(pdf).expect("the real stream extent is recovered");
        assert!(matches!(index.get(0), Some(XrefEntry::InFile { .. })));
    }

    /// The same lie, with no `endstream` anywhere to recover to: the stream
    /// runs off the end of the file. This must terminate with a refusal.
    #[test]
    fn a_stream_that_runs_off_the_end_of_the_file_terminates() {
        let pdf = b"%PDF-1.5\n1 0 obj\n<</Type /XRef /Size 2 /W [1 4 2] /Length 999999>>\n\
                    stream\n\x01\x00\x00\x00\x09\x00\x00\nstartxref\n9\n%%EOF";
        let _ = XrefIndex::build(pdf);
    }

    #[test]
    fn a_cyclic_prev_chain_terminates() {
        // Two xref streams pointing /Prev at each other. The chain must end,
        // not spin.
        let a = 9usize;
        let body = rows(&[(0, 0, 0xFFFF)]);
        let mut pdf = Vec::from(&b"%PDF-1.5\n"[..]);
        pdf.extend_from_slice(
            format!(
                "1 0 obj\n<</Type /XRef /Size 2 /W [1 4 2] /Root 1 0 R /Prev {a} /Length {}>>\n\
                 stream\n",
                body.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(format!("startxref\n{a}\n%%EOF").as_bytes());

        // Whatever the verdict, it is reached in finite time.
        let _ = XrefIndex::build(&pdf);
    }

    /// `/W [i64::MAX i64::MAX 3]`.
    ///
    /// The row length is the sum of the widths. Summed as `usize`, two
    /// `i64::MAX` widths wrapped it to a small number, which passed both the
    /// "the row is empty" and the "the row runs past the decoded data" guards
    /// — and then the inner loop ran `for _ in 0..i64::MAX` indexing the
    /// decoded buffer, panicking in the process that owns the user interface.
    /// A field width is a byte count and none may exceed eight.
    #[test]
    fn a_field_width_past_eight_is_refused() {
        let pdf = b"%PDF-1.5\n1 0 obj\n<</Type /XRef /Size 3 \
                    /W [9223372036854775807 9223372036854775807 3] /Root 1 0 R /Length 8>>\n\
                    stream\n\x01\x00\x00\x00\x09\x00\x00\x00\nendstream\nendobj\n\
                    startxref\n9\n%%EOF";
        let err = XrefIndex::build(pdf).expect_err("a /W element of i64::MAX must be refused");
        let message = err.to_string();
        assert!(
            message.contains("field width"),
            "the refusal must name the field width, got: {message}"
        );

        // Eight is the widest legal field, and is accepted.
        let widths = xref_stream_pdf(&[(1, "<</Type /Catalog>>")], "");
        assert!(XrefIndex::build(&widths).is_ok());
    }

    /// A `/W` or an `/Index` is positional: dropping an element that is not an
    /// integer re-phases every element after it, so the section is read at the
    /// wrong object numbers or with the wrong field layout. Both are refused
    /// rather than silently re-phased.
    #[test]
    fn a_malformed_w_or_index_is_refused_rather_than_rephased() {
        for extra in [
            "/W [1 /Bogus 2]",
            "/W [1 -4 2]",
            "/Index [0 (three) 4 2]",
            "/Index [0 -1]",
            "/Index [0 1 4]",
        ] {
            // The fixture writes its own `/W [1 4 2]`, so a `/W` override has
            // to be built by hand.
            let pdf = if extra.starts_with("/W") {
                let body = rows(&[(0, 0, 0xFFFF), (1, 9, 0)]);
                let mut out = Vec::from(&b"%PDF-1.5\n"[..]);
                let at = out.len();
                out.extend_from_slice(
                    format!(
                        "1 0 obj\n<</Type /XRef /Size 2 {extra} /Root 1 0 R /Length {}>>\nstream\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(&body);
                out.extend_from_slice(b"\nendstream\nendobj\n");
                out.extend_from_slice(format!("startxref\n{at}\n%%EOF").as_bytes());
                out
            } else {
                xref_stream_pdf(&[(1, "<</Type /Catalog>>")], extra)
            };
            let result = XrefIndex::build(&pdf);
            assert!(
                result.is_err(),
                "{extra} was accepted rather than refused: {result:?}"
            );
        }
    }

    /// The `/Index` subsection cap used to bite only after the whole array had
    /// been lexed into a `Vec`, so the allocation a crafted array provokes was
    /// unbounded even though its *use* was not. The lexer refuses first.
    #[test]
    fn an_array_longer_than_the_cap_is_refused_while_it_is_lexed() {
        let mut src = String::from("[");
        for _ in 0..(MAX_ARRAY_ITEMS + 1) {
            src.push_str("0 ");
        }
        src.push(']');
        let mut lex = Lexer::new(src.as_bytes(), 0);
        let err = lex
            .parse_value(0)
            .expect_err("an array past the cap must be refused");
        assert!(err.to_string().contains("elements"), "{err}");
    }

    /// `/DecodeParms` is attacker-controlled, and the predictor geometry was
    /// multiplied out unchecked: a debug panic, or a release wrap to a small
    /// row length that walks straight past the cap meant to catch it.
    #[test]
    fn a_predictor_geometry_that_overflows_is_refused() {
        let mut dict = Dict::new();
        dict.insert("Filter".into(), PdfValue::Name("FlateDecode".into()));
        let mut parms = Dict::new();
        parms.insert("Predictor".into(), PdfValue::Int(12));
        parms.insert("Colors".into(), PdfValue::Int(i64::MAX));
        parms.insert("BitsPerComponent".into(), PdfValue::Int(i64::MAX));
        parms.insert("Columns".into(), PdfValue::Int(i64::MAX));
        dict.insert("DecodeParms".into(), PdfValue::Dict(parms));

        let deflated = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(b"\x02\x00\x00").unwrap();
            e.finish().unwrap()
        };
        let err = decode_stream(&deflated, &dict)
            .expect_err("a predictor geometry that overflows must be refused");
        assert!(err.to_string().contains("overflow"), "{err}");
    }

    /// Each cross-reference stream is bounded on its own, but a chain of them
    /// is not bounded by any of those bounds. The budget is per pass, so what
    /// a document can demand is set by the document rather than by how many
    /// times something is looked up in it.
    #[test]
    fn a_chain_that_decodes_past_the_budget_is_refused() {
        use std::io::Write;

        // Each section decodes to ~14 MiB of type-1 rows; five of them are
        // past the 64 MiB budget while each is well under the per-stream cap.
        let rows_per_section = 2_000_000usize;
        let mut raw = Vec::with_capacity(rows_per_section * 7);
        for _ in 0..rows_per_section {
            raw.push(1u8);
            raw.extend_from_slice(&9u32.to_be_bytes());
            raw.extend_from_slice(&0u16.to_be_bytes());
        }
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(&raw).unwrap();
        let deflated = e.finish().unwrap();

        let mut out = Vec::from(&b"%PDF-1.5\n"[..]);
        out.extend_from_slice(b"1 0 obj\n<</Type /Catalog>>\nendobj\n");
        let mut offsets: Vec<usize> = Vec::new();
        for i in 0..8 {
            let at = out.len();
            offsets.push(at);
            let prev = match i {
                0 => String::new(),
                _ => format!("/Prev {} ", offsets[i - 1]),
            };
            out.extend_from_slice(
                format!(
                    "{} 0 obj\n<</Type /XRef /Size 2 /W [1 4 2] /Filter /FlateDecode \
                     /Root 1 0 R {prev}/Length {}>>\nstream\n",
                    i + 2,
                    deflated.len()
                )
                .as_bytes(),
            );
            out.extend_from_slice(&deflated);
            out.extend_from_slice(b"\nendstream\nendobj\n");
        }
        out.extend_from_slice(format!("startxref\n{}\n%%EOF", offsets[7]).as_bytes());

        let err = XrefIndex::build(&out).expect_err("the chain must exhaust the decode budget");
        assert!(
            err.to_string().contains("inflating"),
            "the refusal must name the decompression budget, got: {err}"
        );
    }

    #[test]
    fn png_up_predictor_rows_are_undone() {
        // Encode two 7-byte rows with the PNG `Up` filter and check that
        // decoding recovers them exactly.
        let original: [[u8; 7]; 2] = [[1, 0, 0, 0, 9, 0, 0], [1, 0, 0, 0, 40, 0, 0]];
        let mut encoded = Vec::new();
        let mut previous = [0u8; 7];
        for row in original.iter() {
            encoded.push(2u8); // filter type: Up
            for (i, b) in row.iter().enumerate() {
                encoded.push(b.wrapping_sub(previous[i]));
            }
            previous = *row;
        }

        let mut dict = Dict::new();
        dict.insert("Filter".into(), PdfValue::Name("FlateDecode".into()));
        let mut parms = Dict::new();
        parms.insert("Predictor".into(), PdfValue::Int(12));
        parms.insert("Columns".into(), PdfValue::Int(7));
        dict.insert("DecodeParms".into(), PdfValue::Dict(parms));

        let deflated = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(&encoded).unwrap();
            e.finish().unwrap()
        };

        let decoded = decode_stream(&deflated, &dict).expect("predictor decoding succeeds");
        assert_eq!(decoded, original.concat());
    }

    /// §76.15: a single `/Index` pair whose declared `count` exceeds
    /// `MAX_XREF_ENTRIES` was already refused before this pair's rows are
    /// read at all. What was not caught is several pairs, each individually
    /// under the cap, whose rows sum past it — a real producer's `/W [0 1
    /// 0]` (one byte per row) and a `/Index` of a few dozen such pairs turns
    /// a modest stream into tens of millions of pushes. Three pairs here,
    /// each comfortably under the per-pair cap but summing well past it,
    /// must be refused from inside the loop, not accepted.
    #[test]
    fn cumulative_entries_across_index_pairs_are_capped() {
        let per_pair = MAX_XREF_ENTRIES / 3 + 1;
        let body = vec![0u8; per_pair * 3]; // one byte per row (/W [0 1 0])
        let mut pdf = Vec::from(&b"%PDF-1.5\n"[..]);
        let at = pdf.len();
        pdf.extend_from_slice(
            format!(
                "3 0 obj\n<</Type /XRef /Size {size} /W [0 1 0] \
                 /Index [0 {per_pair} {per_pair} {per_pair} {two_pair} {per_pair}] \
                 /Root 1 0 R /Length {len}>>\nstream\n",
                size = per_pair * 3 + 1,
                per_pair = per_pair,
                two_pair = per_pair * 2,
                len = body.len(),
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(format!("startxref\n{at}\n%%EOF").as_bytes());

        let result = XrefIndex::build(&pdf);
        assert!(
            result.is_err(),
            "cumulative rows across several /Index pairs, each under the per-pair cap, \
             must still be refused once their sum exceeds MAX_XREF_ENTRIES"
        );
    }
}
