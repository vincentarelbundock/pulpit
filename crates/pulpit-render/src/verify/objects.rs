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
use std::cell::RefCell;
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

fn find_bytes_from(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
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
            let section = parse_xref_section(bytes, offset)?;
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

struct XrefSection {
    entries: Vec<(u32, XrefEntry)>,
    trailer: Dict,
    prev: Option<u64>,
    xref_stm: Option<u64>,
}

fn find_startxref_offset(bytes: &[u8]) -> Result<u64> {
    if bytes.len() < 10 {
        return Err(VerifyError::FileTooSmall);
    }
    let window = std::cmp::min(bytes.len(), 4096);
    let base = bytes.len() - window;
    let tail = &bytes[base..];
    let pos = tail
        .windows(9)
        .rposition(|w| w == b"startxref")
        .ok_or(VerifyError::StartxrefNotFound)?;
    let mut lex = Lexer::new(bytes, base + pos + 9);
    lex.read_uint().ok_or(VerifyError::StartxrefNotFound)
}

fn parse_xref_section(bytes: &[u8], offset: u64) -> Result<XrefSection> {
    let pos = usize::try_from(offset).map_err(|_| VerifyError::TruncatedFile(offset))?;
    if pos >= bytes.len() {
        return Err(VerifyError::TruncatedFile(offset));
    }
    let mut lex = Lexer::new(bytes, pos);
    lex.skip_ws();
    if bytes[lex.pos..].starts_with(b"xref") {
        parse_classic_section(bytes, lex.pos + 4)
    } else {
        parse_xref_stream_section(bytes, pos)
    }
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
    })
}

fn non_neg(v: i64) -> Option<u64> {
    if v >= 0 {
        Some(v as u64)
    } else {
        None
    }
}

fn parse_xref_stream_section(bytes: &[u8], pos: usize) -> Result<XrefSection> {
    let mut lex = Lexer::new(bytes, pos);
    let (_num, _gen, value, _end) = lex.parse_indirect_object()?;
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
    let data = decode_stream(&bytes[body_start..body_start + body_len], dict)?;

    let widths: Vec<usize> = dict
        .get("W")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_i64())
                .map(|v| v.max(0) as usize)
                .collect()
        })
        .unwrap_or_default();
    if widths.len() < 3 {
        return Err(VerifyError::XrefParseError("/W must have 3 widths".into()));
    }
    let row = widths.iter().sum::<usize>();
    if row == 0 {
        return Err(VerifyError::XrefParseError("/W widths are all zero".into()));
    }

    let size = dict
        .get("Size")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0) as u64;
    let index: Vec<u64> = match dict.get("Index").and_then(|v| v.as_array()) {
        Some(a) => a
            .iter()
            .filter_map(|v| v.as_i64())
            .map(|v| v.max(0) as u64)
            .collect(),
        None => vec![0, size],
    };

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
                    v = (v << 8) | data[at] as u64;
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
    let bpp = (colors * bpc).div_ceil(8).max(1);
    let row_len = (colors * bpc * columns).div_ceil(8);
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
        if let Some(XrefEntry::InFile { offset, gen }) =
            self.index.as_ref().and_then(|i| i.get(obj_num))
        {
            if let Some(end) = self.validate_header(offset, obj_num, gen) {
                return Some((offset as usize, end, Confidence::Resolved));
            }
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
            Err(_) => find_bytes_from(self.bytes, start, b"endobj").map(|p| p + b"endobj".len()),
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
        if let Some((start, end, conf)) = self.object_span(obj_num) {
            if end <= self.bytes.len() {
                return Ok((self.bytes[start..end].to_vec(), conf));
            }
        }
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
        let Some(XrefEntry::InFile { offset, gen }) =
            self.index.as_ref().and_then(|i| i.get(stream))
        else {
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
        let data = decode_stream(&self.bytes[body_start..body_start + body_len], &dict)?;

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
            Err(_) => find_bytes_from(bytes, start, b"endobj").map(|e| e + b"endobj".len()),
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
}
