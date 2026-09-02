#![forbid(unsafe_code)]

//! Minimal PDF object model and incremental update writer.
//!
//! This module provides byte-level PDF writing without cryptographic knowledge.
//! It handles:
//!
//! - Byte-range placeholder writing and back-patching (§23.2-23.4)
//! - Incremental update writing (§24)
//! - Dictionary and object serialization (§25.1-25.3)
//!
//! The module MUST NOT depend on the `sign` module, PDFium, or any crypto crates.

use std::io::{self, Seek, SeekFrom, Write};

/// Error types for PDF writing operations.
#[derive(Debug, thiserror::Error)]
pub enum PdfWriteError {
    #[error("signature reservation is odd-sized ({0} bytes); must be even")]
    OddReservationSize(usize),

    #[error("signature too large: {required} bytes needed, only {reserved} reserved")]
    SignatureTooLarge { required: usize, reserved: usize },

    #[error("the byte-range placeholder does not fit in 62 bytes (needs {0})")]
    ByteRangeOverflow(usize),

    #[error("incremental write failed: {0}")]
    IncrementalWriteFailed(String),

    #[error("hybrid-reference PDFs (/XRefStm) are not supported")]
    HybridXrefRefused,

    #[error("failed to parse PDF structure: {0}")]
    ParseError(String),

    /// A document this writer understands but cannot represent.
    #[error("{0}")]
    Unsupported(String),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, PdfWriteError>;

/// Represents the state after placeholders have been written.
/// Used to track byte offsets for back-patching.
#[derive(Debug, Clone)]
pub struct PlaceholderOffsets {
    /// Byte offset of the `[` in `/ByteRange`
    pub byterange_start: u64,
    /// Byte offset of the `<` in `/Contents`
    pub sig_start: u64,
    /// Byte offset just past the `>` in `/Contents`
    pub sig_end: u64,
    /// The number of hex characters reserved for the signature
    pub bytes_reserved: usize,
}

impl PlaceholderOffsets {
    /// Validate that bytes_reserved is even (required for hex encoding).
    pub fn validate(&self) -> Result<()> {
        if !self.bytes_reserved.is_multiple_of(2) {
            return Err(PdfWriteError::OddReservationSize(self.bytes_reserved));
        }
        Ok(())
    }
}

/// After the document has been completely written to a stream.
#[derive(Debug)]
pub struct BackPatchContext {
    pub byterange_start: u64,
    pub sig_start: u64,
    pub sig_end: u64,
}

impl BackPatchContext {
    /// Back-patch the `/ByteRange` with the actual offsets. The two spans a
    /// caller must hash to produce the document digest are `[0..sig_start)`
    /// and `[sig_end..eof)`, which it already has from the offsets it built
    /// this context with.
    pub fn finish<W: Write + Seek>(&self, eof: u64, output: &mut W) -> Result<()> {
        // Seek to the ByteRange offset
        output
            .seek(SeekFrom::Start(self.byterange_start))
            .map_err(PdfWriteError::Io)?;

        // Format: [0 sig_start sig_end eof-sig_end]
        let byterange_content = format!(
            "[0 {} {} {}]",
            self.sig_start,
            self.sig_end,
            eof - self.sig_end
        );

        // Must fit in 62 bytes
        if byterange_content.len() > 62 {
            return Err(PdfWriteError::ByteRangeOverflow(byterange_content.len()));
        }

        // Write and pad with spaces
        output
            .write_all(byterange_content.as_bytes())
            .map_err(PdfWriteError::Io)?;
        let padding = 62 - byterange_content.len();
        for _ in 0..padding {
            output.write_all(b" ").map_err(PdfWriteError::Io)?;
        }

        Ok(())
    }
}

/// Minimal PDF object representation for writing, using deterministic ordering.
#[derive(Debug, Clone)]
pub enum PdfObject {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    /// Application-generated UTF-8 text. The writer will encode non-ASCII
    /// as UTF-16BE with a BOM prefix, keeping ASCII as a literal string.
    /// For FINDING #5b compatibility, use RawString for raw PDF bytes.
    String(Vec<u8>),
    /// Raw PDF bytes from a parsed document. These are emitted verbatim as
    /// a hex string to prevent corruption. Used for field names and other
    /// document values that may contain PDFDocEncoding or other raw bytes
    /// that must survive round-tripping (FINDING #5b).
    RawString(Vec<u8>),
    Name(String),
    Array(Vec<PdfObject>),
    /// Dictionary with deterministic order: Vec preserves insertion order
    Dictionary(Vec<(String, PdfObject)>),
    IndirectRef {
        obj_num: u32,
        gen_num: u16,
    },
    HexString(Vec<u8>),
    /// Bytes emitted verbatim, with no escaping or reformatting.
    ///
    /// This exists so that a caller can place a fixed-width placeholder — a
    /// padded `/ByteRange` array or a `/Contents` hex reservation (§23.2) —
    /// inside an otherwise ordinary dictionary, and then find and back-patch
    /// it by offset afterwards.
    Raw(Vec<u8>),
}

impl PdfObject {
    /// Serialize this object to PDF syntax.
    pub fn serialize(&self, writer: &mut dyn Write) -> Result<()> {
        match self {
            PdfObject::Null => writer.write_all(b"null").map_err(PdfWriteError::Io)?,
            PdfObject::Boolean(b) => writer
                .write_all(if *b { b"true" } else { b"false" })
                .map_err(PdfWriteError::Io)?,
            PdfObject::Integer(i) => write!(writer, "{}", i).map_err(PdfWriteError::Io)?,
            PdfObject::Real(f) => {
                // Format as fixed-point with up to 6 decimal places, no exponent
                let formatted = if f.is_finite() {
                    // Use fixed-point format, avoiding scientific notation
                    let s = format!("{:.6}", f);
                    // Remove trailing zeros after decimal point
                    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
                    trimmed.to_string()
                } else {
                    return Err(PdfWriteError::ParseError(
                        "Non-finite real number".to_string(),
                    ));
                };
                write!(writer, "{}", formatted).map_err(PdfWriteError::Io)?
            }
            PdfObject::String(s) => {
                // A BOM-less literal string is read as PDFDocEncoding by a
                // conforming viewer, so raw UTF-8 would garble any non-ASCII
                // signer name or reason. ASCII is a subset of PDFDocEncoding
                // and is emitted unchanged (keeping existing output
                // byte-exact); anything else is emitted as UTF-16BE with a
                // leading BOM, as a hex string.
                if s.is_ascii() {
                    writer.write_all(b"(").map_err(PdfWriteError::Io)?;
                    for &byte in s {
                        if byte == b'(' || byte == b')' || byte == b'\\' {
                            writer.write_all(b"\\").map_err(PdfWriteError::Io)?;
                        }
                        writer.write_all(&[byte]).map_err(PdfWriteError::Io)?;
                    }
                    writer.write_all(b")").map_err(PdfWriteError::Io)?;
                } else {
                    // Non-UTF-8 bytes are not text we can transcode; they are
                    // preserved verbatim inside a hex string instead.
                    let text = std::str::from_utf8(s).ok();
                    writer.write_all(b"<").map_err(PdfWriteError::Io)?;
                    match text {
                        Some(text) => {
                            write!(writer, "FEFF").map_err(PdfWriteError::Io)?;
                            for unit in text.encode_utf16() {
                                write!(writer, "{:04X}", unit).map_err(PdfWriteError::Io)?;
                            }
                        }
                        None => {
                            for byte in s {
                                write!(writer, "{:02X}", byte).map_err(PdfWriteError::Io)?;
                            }
                        }
                    }
                    writer.write_all(b">").map_err(PdfWriteError::Io)?;
                }
            }
            PdfObject::RawString(s) => {
                // Raw PDF bytes from a parsed document. Emit as hex string to preserve
                // bytes exactly as they are, without any transcoding or interpretation.
                // This prevents corruption of PDFDocEncoding or other raw bytes (FINDING #5b).
                writer.write_all(b"<").map_err(PdfWriteError::Io)?;
                for byte in s {
                    write!(writer, "{:02X}", byte).map_err(PdfWriteError::Io)?;
                }
                writer.write_all(b">").map_err(PdfWriteError::Io)?;
            }
            PdfObject::Name(n) => {
                serialize_name(writer, n)?;
            }
            PdfObject::HexString(h) => {
                writer.write_all(b"<").map_err(PdfWriteError::Io)?;
                for byte in h {
                    write!(writer, "{:02X}", byte).map_err(PdfWriteError::Io)?;
                }
                writer.write_all(b">").map_err(PdfWriteError::Io)?;
            }
            PdfObject::Array(arr) => {
                writer.write_all(b"[").map_err(PdfWriteError::Io)?;
                for (i, obj) in arr.iter().enumerate() {
                    if i > 0 {
                        writer.write_all(b" ").map_err(PdfWriteError::Io)?;
                    }
                    obj.serialize(writer)?;
                }
                writer.write_all(b"]").map_err(PdfWriteError::Io)?;
            }
            PdfObject::Dictionary(entries) => {
                writer.write_all(b"<<").map_err(PdfWriteError::Io)?;
                for (key, value) in entries {
                    serialize_name(writer, key)?;
                    writer.write_all(b" ").map_err(PdfWriteError::Io)?;
                    value.serialize(writer)?;
                }
                writer.write_all(b">>").map_err(PdfWriteError::Io)?;
            }
            PdfObject::IndirectRef { obj_num, gen_num } => {
                write!(writer, "{} {} R", obj_num, gen_num).map_err(PdfWriteError::Io)?
            }
            PdfObject::Raw(bytes) => writer.write_all(bytes).map_err(PdfWriteError::Io)?,
        }
        Ok(())
    }
}

fn serialize_name(writer: &mut dyn Write, name: &str) -> Result<()> {
    writer.write_all(b"/").map_err(PdfWriteError::Io)?;
    for byte in name.as_bytes() {
        // PDF 2.0, 7.3.5: whitespace, delimiters, `#`, controls and bytes
        // outside printable ASCII must use `#xx` escaping.
        match *byte {
            b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'('
            | b')'
            | b'#'
            | b'%'
            | 0..=0x20
            | 0x7F..=0xFF => {
                write!(writer, "#{:02X}", byte).map_err(PdfWriteError::Io)?;
            }
            _ => writer.write_all(&[*byte]).map_err(PdfWriteError::Io)?,
        }
    }
    Ok(())
}

/// Simple PDF tokenizer for reading existing PDF structures.
pub struct PdfTokenizer<'a> {
    data: &'a [u8],
    pos: usize,
}

/// The characters that end a PDF token: §7.2.3's six whitespace characters and
/// its eight delimiters.
///
/// Three were missing, and the omissions were not equivalent. `\0` and `\x0c`
/// are whitespace the specification names, so `/Sig\x0c` tokenized as the name
/// `Sig\u{c}` — and `extract_signature_field` compares that against `"Sig"` to
/// decide whether a field is a signature at all, so the field was dropped from
/// discovery entirely rather than reported as anything. `%` opens a comment,
/// which `skip_whitespace` already knew and this did not, so a token running
/// into one swallowed it.
///
/// `verify::objects`'s lexer has always had the full set. Two parsers reading
/// the same bytes by different rules is the divergence this crate keeps
/// finding; here the stricter one was the one that decided.
fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        // Whitespace (§7.2.3, table 1).
        b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' '
        // Delimiters (§7.2.3, table 2).
        | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

impl<'a> PdfTokenizer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        PdfTokenizer { data, pos: 0 }
    }

    /// Skip whitespace and comments.
    fn skip_whitespace(&mut self) {
        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            if matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ') {
                self.pos += 1;
            } else if byte == b'%' {
                // Skip comment until end of line
                while self.pos < self.data.len() && self.data[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Get the next token.
    /// Advance to the next delimiter, the end of the current token.
    ///
    /// A name and a bare token end the same way -- at whitespace or at one of
    /// the characters PDF reserves as a delimiter -- and only their first byte
    /// tells them apart. Writing the delimiter set twice is how the two come
    /// to disagree about what ends a token.
    fn scan_to_delimiter(&mut self) {
        while self.pos < self.data.len() {
            if is_delimiter(self.data[self.pos]) {
                break;
            }
            self.pos += 1;
        }
    }

    pub fn next_token(&mut self) -> Result<Option<Vec<u8>>> {
        self.skip_whitespace();
        if self.pos >= self.data.len() {
            return Ok(None);
        }

        let start = self.pos;
        let first_byte = self.data[self.pos];

        match first_byte {
            b'<' | b'>' | b'[' | b']' | b'{' | b'}' => {
                // Paired or single character tokens
                if first_byte == b'<'
                    && self.pos + 1 < self.data.len()
                    && self.data[self.pos + 1] == b'<'
                {
                    self.pos += 2;
                    Ok(Some(b"<<".to_vec()))
                } else if first_byte == b'<' {
                    // Hex string: <hex_chars>
                    // Read until we find the closing >
                    self.pos += 1; // skip opening <
                    let mut result = vec![b'<'];
                    while self.pos < self.data.len() && self.data[self.pos] != b'>' {
                        result.push(self.data[self.pos]);
                        self.pos += 1;
                    }
                    if self.pos < self.data.len() && self.data[self.pos] == b'>' {
                        result.push(b'>');
                        self.pos += 1;
                        Ok(Some(result))
                    } else {
                        // Malformed hex string without closing >
                        // Return what we have
                        Ok(Some(result))
                    }
                } else if first_byte == b'>'
                    && self.pos + 1 < self.data.len()
                    && self.data[self.pos + 1] == b'>'
                {
                    self.pos += 2;
                    Ok(Some(b">>".to_vec()))
                } else {
                    self.pos += 1;
                    Ok(Some(vec![first_byte]))
                }
            }
            b'/' => {
                // PDF name - starts with / and continues until a delimiter
                self.pos += 1;
                self.scan_to_delimiter();
                Ok(Some(self.data[start..self.pos].to_vec()))
            }
            b'(' => {
                // String literal
                self.pos += 1;
                let mut result = vec![b'('];
                let mut paren_depth = 1;
                while self.pos < self.data.len() && paren_depth > 0 {
                    let byte = self.data[self.pos];
                    result.push(byte);
                    if byte == b'\\' && self.pos + 1 < self.data.len() {
                        self.pos += 1;
                        result.push(self.data[self.pos]);
                    } else if byte == b'(' {
                        paren_depth += 1;
                    } else if byte == b')' {
                        paren_depth -= 1;
                    }
                    self.pos += 1;
                }
                Ok(Some(result))
            }
            b')' => {
                // Closing paren as single token
                self.pos += 1;
                Ok(Some(vec![first_byte]))
            }
            _ => {
                // Regular token
                self.scan_to_delimiter();
                if self.pos > start {
                    Ok(Some(self.data[start..self.pos].to_vec()))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Seek to a specific byte position.
    pub fn seek(&mut self, pos: usize) {
        if pos <= self.data.len() {
            self.pos = pos;
        }
    }

    /// Get current position.
    pub fn position(&self) -> usize {
        self.pos
    }
}

/// Incremental update writer for appending to existing PDFs.
///
/// §78.3: borrows the source rather than copying it. `open` used to take
/// `bytes: &[u8]` and immediately clone it into `original_bytes: Vec<u8>`,
/// so every signing pass paid for a full copy of the document it was
/// signing while the caller was already holding one live.
pub struct IncrementalWriter<'a> {
    original_bytes: &'a [u8],
    #[allow(dead_code)]
    original_eof: u64,
    prev_startxref: u64, // The startxref offset being extended, for /Prev
    xref_kind: XRefKind,
    trailer_dict: TrailerDict,
    // The first object number this writer will allocate: one past the
    // higher of the trailer's declared `/Size` and the highest object
    // number actually present anywhere in the cross-reference chain. See
    // `next_object_number` for why the declared `/Size` alone is not
    // trusted.
    next_object_number: u32,
}

/// Whether to use classic xref table or xref stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XRefKind {
    Table,
    Stream,
}

/// Parsed trailer dictionary from existing PDF.
#[derive(Debug, Clone)]
pub struct TrailerDict {
    pub root: Option<(u32, u16)>, // obj_num, gen_num
    pub info: Option<(u32, u16)>,
    pub size: u32,
    pub prev: Option<u64>,
    pub id: Option<Vec<Vec<u8>>>,
    pub has_xref_stm: bool,
}

/// One past the highest object number in `objects`, refusing rather than
/// overflowing.
///
/// The `+ 1` used to be unchecked. Every input to it is either an object
/// number this writer allocated from the source document's `/Size` or the
/// `/Size` itself, so a source that declares `u32::MAX` reached it — panicking
/// a debug build, and in release wrapping to zero.
fn one_past_highest(objects: &[(u32, u16, PdfObject)]) -> Result<u32> {
    let highest = objects.iter().map(|(n, _, _)| *n).max();
    match highest {
        None => Ok(1),
        Some(n) => n.checked_add(1).ok_or_else(|| {
            PdfWriteError::Unsupported(format!(
                "object number {n} leaves no room for the cross-reference stream's own \
                 object number; the source document is unchanged"
            ))
        }),
    }
}

impl<'a> IncrementalWriter<'a> {
    /// Open an existing PDF for incremental update.
    pub fn open(bytes: &'a [u8]) -> Result<Self> {
        // Find the last startxref
        let prev_startxref = find_startxref(bytes)?;

        // Detect xref kind and parse trailer
        let (xref_kind, trailer_dict) = parse_trailer(bytes, prev_startxref)?;

        // Check for hybrid xref (XRefStm in trailer)
        if trailer_dict.has_xref_stm {
            return Err(PdfWriteError::HybridXrefRefused);
        }

        // `/Size` is one past the highest object number, and it comes from the
        // source document. Every object number this writer allocates is
        // `/Size` or above, and each of the xref writers computes `n + 1` from
        // one. A `/Size` of u32::MAX therefore panics a debug build and, in a
        // release one, wraps a `/Size` to 0 beside an object numbered
        // 4294967295 — which is a file no reader can open, produced silently.
        // §7.5.4 also caps the object number at 8388607 in a conforming file,
        // so nothing legal is anywhere near this.
        if trailer_dict.size == u32::MAX {
            return Err(PdfWriteError::Unsupported(format!(
                "the document's trailer declares /Size {}, which leaves no object number \
                 free to allocate; the source document is unchanged",
                trailer_dict.size
            )));
        }

        // `/Size` is a producer's claim, not a fact: a document whose xref
        // chain actually holds objects at or above the declared `/Size` is
        // ordinary real-world damage the lenient reader (`ObjectResolver`)
        // tolerates. Trusting `/Size` alone here would let the signature
        // dictionary, appearance XObject or new field this writer appends
        // land on an object number that a live page or content stream
        // already occupies, silently overwriting it in the signed output.
        // Cross-check against the highest object number the cross-reference
        // chain itself reports and take the higher of the two. `/Size 0`
        // (missing or unparseable) is refused outright: with no reliable
        // floor at all, allocating from the xref chain alone without a
        // sanity check from the trailer is not a case worth writing into.
        if trailer_dict.size == 0 {
            return Err(PdfWriteError::Unsupported(
                "the document's trailer declares /Size 0 (missing or unparseable), so there \
                 is no reliable floor for allocating new object numbers; the source document \
                 is unchanged"
                    .to_string(),
            ));
        }
        let highest_xref_entry = crate::verify::objects::XrefIndex::build(bytes)
            .ok()
            .and_then(|index| index.entries().keys().next_back().copied());
        let next_object_number = match highest_xref_entry {
            Some(highest) => std::cmp::max(trailer_dict.size, highest.saturating_add(1)),
            None => trailer_dict.size,
        };

        Ok(IncrementalWriter {
            original_bytes: bytes,
            original_eof: bytes.len() as u64,
            prev_startxref,
            xref_kind,
            trailer_dict,
            next_object_number,
        })
    }

    /// The trailer dictionary of the revision being extended.
    pub fn trailer(&self) -> &TrailerDict {
        &self.trailer_dict
    }

    /// The first object number that is free to allocate: one past the
    /// higher of the trailer's declared `/Size` and the highest object
    /// number the cross-reference chain actually reports. A `/Size` that
    /// undercounts the real chain — ordinary damage, not necessarily
    /// hostile — used to be trusted outright and could steer newly
    /// allocated objects onto numbers a live object already occupied.
    pub fn next_object_number(&self) -> u32 {
        self.next_object_number
    }

    /// Append multiple objects and finalize the PDF with proper xref.
    /// objects: slice of (obj_num, gen_num, PdfObject) tuples
    /// new_id2: 16 bytes for new /ID second element
    ///
    /// Returns each object's `(obj_num, gen_num, offset)`, `offset` being
    /// where its `"{obj_num} {gen_num} obj"` header starts in `writer`, in
    /// the same order as `objects`. A caller that needs to find a byte range
    /// inside one of the objects it just wrote — the signature reservation,
    /// say — computes it from the object's own known layout starting at that
    /// offset, rather than searching the appended bytes for a literal that
    /// untrusted content elsewhere in the same revision could also contain.
    pub fn append_objects<W: Write + Seek>(
        self,
        writer: &mut W,
        objects: &[(u32, u16, PdfObject)],
        new_id2: &[u8; 16],
    ) -> Result<Vec<(u32, u16, u64)>> {
        // Write the original bytes
        writer
            .write_all(self.original_bytes)
            .map_err(PdfWriteError::Io)?;

        // Track object offsets for xref: (obj_num, gen_num, offset). §77.9: a
        // re-emitted *existing* object (the catalog, a field, a page) keeps
        // whatever generation it already has in the source document — an
        // incremental update never bumps it (§7.5.6) — and a trailer whose
        // `/Root` still says `5 1 R` after this writer rewrote object 5 as
        // `5 0 obj` pointed the reader at an entry the new xref never wrote,
        // so the "newest revision wins" catalog silently reverted. The
        // generation the caller passed in for each object is now the one
        // written both in the object's own header and in its xref row.
        let mut obj_offsets: Vec<(u32, u16, u64)> = Vec::new();

        // Write each object and record its offset
        for (obj_num, gen_num, obj) in objects {
            let offset = writer.stream_position().map_err(PdfWriteError::Io)?;
            obj_offsets.push((*obj_num, *gen_num, offset));

            writeln!(writer, "{} {} obj", obj_num, gen_num).map_err(PdfWriteError::Io)?;
            obj.serialize(writer)?;
            writeln!(writer, "\nendobj").map_err(PdfWriteError::Io)?;
        }

        // Now we know xref location (where xref keyword or stream object starts)
        let xref_offset = writer.stream_position().map_err(PdfWriteError::Io)?;

        // Write xref section based on kind
        match self.xref_kind {
            XRefKind::Table => {
                self.write_xref_table(writer, &obj_offsets)?;
            }
            XRefKind::Stream => {
                // Allocate next object number for the xref stream itself
                let xref_stream_obj_num =
                    std::cmp::max(self.next_object_number, one_past_highest(objects)?);
                self.write_xref_stream(writer, &obj_offsets, xref_stream_obj_num, new_id2)?;
                // Return early; xref stream writing handles trailer and startxref
                return Ok(obj_offsets);
            }
        }

        // Write trailer (for classic xref only; xref stream returns above)
        let new_size = std::cmp::max(self.next_object_number, one_past_highest(objects)?);

        writeln!(writer, "trailer").map_err(PdfWriteError::Io)?;
        writeln!(writer, "<<").map_err(PdfWriteError::Io)?;
        writeln!(writer, "/Prev {}", self.prev_startxref).map_err(PdfWriteError::Io)?;
        if let Some((root_num, root_gen)) = self.trailer_dict.root {
            writeln!(writer, "/Root {} {} R", root_num, root_gen).map_err(PdfWriteError::Io)?;
        }
        if let Some((info_num, info_gen)) = self.trailer_dict.info {
            writeln!(writer, "/Info {} {} R", info_num, info_gen).map_err(PdfWriteError::Io)?;
        }
        writeln!(writer, "/Size {}", new_size).map_err(PdfWriteError::Io)?;

        self.write_id_array(writer, new_id2)?;

        writeln!(writer, ">>").map_err(PdfWriteError::Io)?;
        writeln!(writer, "startxref").map_err(PdfWriteError::Io)?;
        writeln!(writer, "{}", xref_offset).map_err(PdfWriteError::Io)?;
        writeln!(writer, "%%EOF").map_err(PdfWriteError::Io)?;

        Ok(obj_offsets)
    }

    /// The `/ID` entry an incremental update carries: the file's first
    /// element preserved verbatim, a freshly generated second element after
    /// it.
    ///
    /// A classic trailer and an xref stream dictionary disagree about the
    /// order of everything around this entry, but not about the entry itself,
    /// so both go through here. Preserving the first element is what lets a
    /// verifier tie the update back to the file it was made from; a document
    /// that arrived without an `/ID` gets none, rather than an invented one.
    fn write_id_array<W: Write>(&self, writer: &mut W, new_id2: &[u8; 16]) -> Result<()> {
        let Some(id_array) = &self.trailer_dict.id else {
            return Ok(());
        };
        write!(writer, "/ID [").map_err(PdfWriteError::Io)?;
        if !id_array.is_empty() {
            write_hex_string(writer, &id_array[0])?;
        }
        write_hex_string(writer, new_id2)?;
        writeln!(writer, "]").map_err(PdfWriteError::Io)?;
        Ok(())
    }

    fn write_xref_stream<W: Write + Seek>(
        &self,
        writer: &mut W,
        obj_offsets: &[(u32, u16, u64)],
        xref_stream_obj_num: u32,
        new_id2: &[u8; 16],
    ) -> Result<()> {
        // Collect every entry this section describes, keyed by object number.
        //
        // The rows of an xref stream are positional: they are read in the order
        // given by the `/Index` ranges. Emitting a single range that spans the
        // appended object numbers is only correct when those numbers are
        // consecutive, which they are not in general — signing rewrites the
        // catalog, the AcroForm and a page (low numbers) alongside freshly
        // allocated ones taken from the trailer's `/Size`. So the entries are
        // sorted by object number and split into consecutive runs, one
        // `/Index` pair per run, with the rows written in exactly that order.
        let xref_stream_offset = writer.stream_position().map_err(PdfWriteError::Io)?;

        // (obj_num, type, offset, gen)
        let mut entries: Vec<(u32, u8, u64, u16)> = Vec::with_capacity(obj_offsets.len() + 2);
        entries.push((0, 0, 0, 65535)); // object 0: head of the free list
        for (obj_num, gen_num, offset) in obj_offsets {
            // §77.9: the generation is the caller's, not always 0 — a
            // re-emitted existing object (e.g. the catalog) keeps its
            // existing generation, and an xref row of 0 for it would
            // disagree with a trailer `/Root` that still points at gen 1.
            entries.push((*obj_num, 1, *offset, *gen_num));
        }
        entries.push((xref_stream_obj_num, 1, xref_stream_offset, 0));

        entries.sort_by_key(|(n, _, _, _)| *n);
        entries.dedup_by_key(|(n, _, _, _)| *n);

        // Split into consecutive runs; each run becomes one /Index pair.
        let mut index_array: Vec<u32> = Vec::new();
        let mut run_start = entries[0].0;
        let mut run_len = 0u32;
        for (i, (obj_num, _, _, _)) in entries.iter().enumerate() {
            // `checked_add` because the previous object number is bounded only
            // by the source document's `/Size`; the run simply ends when there
            // is no successor to be consecutive with.
            if i > 0 && Some(*obj_num) != entries[i - 1].0.checked_add(1) {
                index_array.push(run_start);
                index_array.push(run_len);
                run_start = *obj_num;
                run_len = 0;
            }
            run_len += 1;
        }
        index_array.push(run_start);
        index_array.push(run_len);

        // Rows, in exactly the order the /Index ranges enumerate.
        //
        // /W [1 4 2] = 7 bytes per entry: one byte of type, four of offset,
        // two of generation or in-stream index. Four bytes address a 4 GiB
        // file, which is past anything this signer will meet — but `as u32`
        // on a larger offset would truncate silently and produce a document
        // whose cross-reference data points into the middle of itself. A
        // signature over a file that no reader can open is worse than a
        // refusal, so the width is checked rather than assumed.
        let mut xref_data = Vec::with_capacity(entries.len() * 7);
        for (obj_num, kind, offset, generation) in &entries {
            let Ok(narrow) = u32::try_from(*offset) else {
                return Err(PdfWriteError::Unsupported(format!(
                    "object {obj_num} lies at offset {offset}, past the 4 GiB a /W [1 4 2] \
                     cross-reference stream can address; the source document is unchanged and \
                     this file is too large for pulpit to sign"
                )));
            };
            xref_data.push(*kind);
            xref_data.extend_from_slice(&narrow.to_be_bytes());
            xref_data.extend_from_slice(&generation.to_be_bytes());
        }

        // Calculate new size. `/Size` is one past the highest object number,
        // so every term here is an increment that a `/Size` of u32::MAX in the
        // source document would overflow: a debug panic, or a release wrap to
        // `/Size 0` written beside an object numbered 4294967295. Refuse.
        let highest = obj_offsets
            .iter()
            .map(|(n, _, _)| *n)
            .chain(std::iter::once(xref_stream_obj_num))
            .max()
            .unwrap_or(0);
        let new_size = std::cmp::max(
            self.next_object_number,
            highest.checked_add(1).ok_or_else(|| {
                PdfWriteError::Unsupported(format!(
                    "object number {highest} leaves no room for a /Size one past it; \
                     the source document is unchanged"
                ))
            })?,
        );

        // Write the xref stream object
        writeln!(writer, "{} 0 obj", xref_stream_obj_num).map_err(PdfWriteError::Io)?;
        writeln!(writer, "<<").map_err(PdfWriteError::Io)?;
        writeln!(writer, "/Type /XRef").map_err(PdfWriteError::Io)?;
        writeln!(writer, "/Size {}", new_size).map_err(PdfWriteError::Io)?;
        writeln!(writer, "/W [1 4 2]").map_err(PdfWriteError::Io)?;

        // Write /Index array
        write!(writer, "/Index [").map_err(PdfWriteError::Io)?;
        for (i, val) in index_array.iter().enumerate() {
            if i > 0 {
                write!(writer, " ").map_err(PdfWriteError::Io)?;
            }
            write!(writer, "{}", val).map_err(PdfWriteError::Io)?;
        }
        writeln!(writer, "]").map_err(PdfWriteError::Io)?;

        if let Some((root_num, root_gen)) = self.trailer_dict.root {
            writeln!(writer, "/Root {} {} R", root_num, root_gen).map_err(PdfWriteError::Io)?;
        }

        writeln!(writer, "/Prev {}", self.prev_startxref).map_err(PdfWriteError::Io)?;

        self.write_id_array(writer, new_id2)?;

        if let Some((info_num, info_gen)) = self.trailer_dict.info {
            writeln!(writer, "/Info {} {} R", info_num, info_gen).map_err(PdfWriteError::Io)?;
        }

        writeln!(writer, "/Length {}", xref_data.len()).map_err(PdfWriteError::Io)?;
        writeln!(writer, ">>").map_err(PdfWriteError::Io)?;
        writeln!(writer, "stream").map_err(PdfWriteError::Io)?;
        writer.write_all(&xref_data).map_err(PdfWriteError::Io)?;
        writeln!(writer).map_err(PdfWriteError::Io)?;
        writeln!(writer, "endstream").map_err(PdfWriteError::Io)?;
        writeln!(writer, "endobj").map_err(PdfWriteError::Io)?;

        // Write startxref pointing to the xref stream object
        writeln!(writer, "startxref").map_err(PdfWriteError::Io)?;
        writeln!(writer, "{}", xref_stream_offset).map_err(PdfWriteError::Io)?;
        writeln!(writer, "%%EOF").map_err(PdfWriteError::Io)?;

        Ok(())
    }

    fn write_xref_table<W: Write>(
        &self,
        writer: &mut W,
        obj_offsets: &[(u32, u16, u64)],
    ) -> Result<()> {
        writeln!(writer, "xref").map_err(PdfWriteError::Io)?;

        // Always emit subsection for object 0 (free list head)
        writeln!(writer, "0 1").map_err(PdfWriteError::Io)?;
        writeln!(writer, "0000000000 65535 f ").map_err(PdfWriteError::Io)?;

        // Group consecutive object numbers into subsections
        if !obj_offsets.is_empty() {
            let mut subsection_start = obj_offsets[0].0;
            let mut subsection_entries = vec![obj_offsets[0]];

            for i in 1..obj_offsets.len() {
                if Some(obj_offsets[i].0) == obj_offsets[i - 1].0.checked_add(1) {
                    // Consecutive object number - add to current subsection
                    subsection_entries.push(obj_offsets[i]);
                } else {
                    // Gap found - emit current subsection and start a new one
                    writeln!(writer, "{} {}", subsection_start, subsection_entries.len())
                        .map_err(PdfWriteError::Io)?;
                    for (_obj_num, gen_num, offset) in &subsection_entries {
                        writeln!(writer, "{:010} {:05} n ", offset, gen_num)
                            .map_err(PdfWriteError::Io)?;
                    }
                    subsection_start = obj_offsets[i].0;
                    subsection_entries = vec![obj_offsets[i]];
                }
            }

            // Emit final subsection
            writeln!(writer, "{} {}", subsection_start, subsection_entries.len())
                .map_err(PdfWriteError::Io)?;
            for (_obj_num, gen_num, offset) in &subsection_entries {
                writeln!(writer, "{:010} {:05} n ", offset, gen_num).map_err(PdfWriteError::Io)?;
            }
        }

        Ok(())
    }
}

/// Find the byte offset of the last startxref directive.
/// One PDF hex string, `<…>`, upper-case and two digits to the byte.
fn write_hex_string<W: Write>(writer: &mut W, bytes: &[u8]) -> Result<()> {
    writer.write_all(b"<").map_err(PdfWriteError::Io)?;
    for byte in bytes {
        write!(writer, "{:02X}", byte).map_err(PdfWriteError::Io)?;
    }
    writer.write_all(b">").map_err(PdfWriteError::Io)?;
    Ok(())
}

/// How far back from the end of a file `startxref` is looked for.
///
/// The writer, the revision walk and the object resolver all have to agree on
/// this. They did not: two searched a kilobyte and the third four, so a file
/// whose `startxref` sat between those two distances from the end was a file
/// the resolver could read and the revision walk could not — two halves of
/// verification reading different documents, which is the failure the
/// `/XRefStm` scan and the `/Prev` depth guard also exist to prevent. Four
/// kilobytes is the wider of the two that were already in use, so agreeing on
/// it refuses nothing that used to be accepted.
pub const STARTXREF_SEARCH_WINDOW: usize = 4096;

/// The offset the file's last `startxref` names, or `None` when there is no
/// readable one within [`STARTXREF_SEARCH_WINDOW`] of the end.
///
/// The callers map `None` onto their own errors, which is the only reason this
/// does not return one itself.
pub fn find_startxref_offset(bytes: &[u8]) -> Option<u64> {
    let window = std::cmp::min(bytes.len(), STARTXREF_SEARCH_WINDOW);
    let start = bytes.len().saturating_sub(window);
    let pos = bytes[start..].windows(9).rposition(|w| w == b"startxref")?;
    // The value is a single integer literal, not general PDF syntax, so a
    // full tokenizer pass is more machinery than this needs (§78.2): skip
    // whitespace, then read the run of digits that follows.
    let mut i = start + pos + 9;
    while matches!(bytes.get(i), Some(b' ' | b'\t' | b'\r' | b'\n' | 0x0c | 0)) {
        i += 1;
    }
    let digits_start = i;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    std::str::from_utf8(&bytes[digits_start..i])
        .ok()?
        .parse()
        .ok()
}

fn find_startxref(bytes: &[u8]) -> Result<u64> {
    find_startxref_offset(bytes)
        .ok_or_else(|| PdfWriteError::ParseError("startxref not found".to_string()))
}

/// Parse the trailer dictionary and detect xref kind.
///
/// §78.3: reads through `verify::objects`'s cross-reference section parser
/// instead of tokenizing the bytes a fourth time. The classic/stream
/// dispatch, the nested-dictionary depth tracking a deflated xref stream's
/// `/DecodeParms <</Predictor 12 …>>` routinely needs, and the
/// `/Prev`/`/XRefStm` reads are exactly what `RevisionMap::build` already
/// does with the same bytes — this was the fifth trailer reader named in
/// §78.1, and the one most likely to describe a different document than the
/// other four, since `IncrementalWriter::open` is what decides where new
/// objects land.
///
/// One behaviour change: `/Info` is now actually read. The token loop this
/// replaces initialised `TrailerDict::info` to `None` and never matched an
/// `"Info"` key, so an extended document's `/Info` dictionary was silently
/// dropped from every revision this writer produced.
fn parse_trailer(bytes: &[u8], startxref: u64) -> Result<(XRefKind, TrailerDict)> {
    let mut budget = crate::verify::objects::DecodeBudget::new();
    let section =
        crate::verify::objects::xref_section_object_numbers(bytes, startxref, &mut budget)
            .map_err(|e| PdfWriteError::ParseError(e.to_string()))?;

    let xref_kind = match section.kind {
        crate::verify::objects::XrefSectionKind::Table => XRefKind::Table,
        crate::verify::objects::XrefSectionKind::Stream => XRefKind::Stream,
    };

    let dict = section.trailer;
    let root = dict.get("Root").and_then(|v| v.as_ref_pair());
    let info = dict.get("Info").and_then(|v| v.as_ref_pair());
    let size = dict
        .get("Size")
        .and_then(|v| v.as_i64())
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    let id = dict
        .get("ID")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| match v {
                    crate::verify::objects::PdfValue::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty());

    Ok((
        xref_kind,
        TrailerDict {
            root,
            info,
            size,
            prev: section.prev,
            id,
            has_xref_stm: section.xref_stm.is_some(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    // The fixture builder is test-only code and lives under `tests/`, where
    // nothing can compile it into the shipped crate. These unit tests are the
    // one place inside `src/` that needs it, so they name the file directly.
    #[allow(dead_code)]
    mod builder {
        include!("../../tests/testkit/builder.rs");
    }
    use self::builder::Pdf;
    use std::io::Read;

    #[test]
    fn test_odd_reservation_rejection() {
        let offsets = PlaceholderOffsets {
            byterange_start: 100,
            sig_start: 200,
            sig_end: 400,
            bytes_reserved: 257, // odd
        };
        assert!(offsets.validate().is_err());
    }

    #[test]
    fn test_even_reservation_acceptance() {
        let offsets = PlaceholderOffsets {
            byterange_start: 100,
            sig_start: 200,
            sig_end: 400,
            bytes_reserved: 256, // even
        };
        assert!(offsets.validate().is_ok());
    }

    #[test]
    fn test_byterange_overflow() {
        let ctx = BackPatchContext {
            byterange_start: 100,
            sig_start: 9_223_372_036_854_775_807u64,
            sig_end: 9_223_372_036_854_775_807u64,
        };
        let mut buffer = io::Cursor::new(vec![0u8; 1000]);
        let result = ctx.finish(18_446_744_073_709_551_615u64, &mut buffer);
        assert!(result.is_err());
    }

    #[test]
    fn test_pdf_object_null() {
        let obj = PdfObject::Null;
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"null");
    }

    #[test]
    fn test_pdf_object_boolean() {
        let obj = PdfObject::Boolean(true);
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"true");
    }

    #[test]
    fn test_pdf_object_integer() {
        let obj = PdfObject::Integer(42);
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"42");
    }

    #[test]
    fn test_pdf_object_name() {
        let obj = PdfObject::Name("Type".to_string());
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"/Type");
    }

    #[test]
    fn test_pdf_object_array() {
        let obj = PdfObject::Array(vec![
            PdfObject::Integer(1),
            PdfObject::Integer(2),
            PdfObject::Integer(3),
        ]);
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"[1 2 3]");
    }

    #[test]
    fn test_pdf_object_hex_string() {
        let obj = PdfObject::HexString(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"<DEADBEEF>");
    }

    #[test]
    fn test_pdf_object_dictionary_ordered() {
        let dict = vec![
            ("Type".to_string(), PdfObject::Name("Sig".to_string())),
            (
                "Contents".to_string(),
                PdfObject::HexString(vec![0xAB, 0xCD]),
            ),
        ];

        let obj = PdfObject::Dictionary(dict);
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        let result_str = String::from_utf8_lossy(&result);
        let type_pos = result_str.find("/Type").unwrap();
        let contents_pos = result_str.find("/Contents").unwrap();
        assert!(type_pos < contents_pos);
    }

    #[test]
    fn test_pdf_object_real_fixed_point() {
        let obj = PdfObject::Real(0.001);
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        let result_str = String::from_utf8_lossy(&result);
        assert!(!result_str.contains('e'));
        assert!(!result_str.contains('E'));
    }

    #[test]
    fn test_pdf_tokenizer_simple() {
        let data = b"[1 2 3]";
        let mut tokenizer = PdfTokenizer::new(data);
        assert_eq!(tokenizer.next_token().unwrap(), Some(b"[".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), Some(b"1".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), Some(b"2".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), Some(b"3".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), Some(b"]".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), None);
    }

    #[test]
    fn test_pdf_tokenizer_dict() {
        let data = b"<</Type /Catalog>>";
        let mut tokenizer = PdfTokenizer::new(data);
        assert_eq!(tokenizer.next_token().unwrap(), Some(b"<<".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), Some(b"/Type".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), Some(b"/Catalog".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), Some(b">>".to_vec()));
        assert_eq!(tokenizer.next_token().unwrap(), None);
    }

    /// §7.2.3's whitespace and delimiters all end a name, including the three
    /// this tokenizer used to miss.
    ///
    /// `\x0c` is the one that mattered. `extract_signature_field` decides
    /// whether a field is a signature by comparing its `/FT` against `"Sig"`,
    /// so a form feed after the name yielded `Sig\u{c}`, failed that
    /// comparison, and the field was dropped from discovery — not reported
    /// broken, simply absent. `verify::objects`'s lexer always had the full
    /// set, so the two parsers disagreed about the same bytes and the weaker
    /// one was the one that decided.
    #[test]
    fn every_delimiter_the_specification_names_ends_a_name() {
        for (data, first) in [
            (b"/Sig\x0c/V".as_slice(), "/Sig"),
            (b"/Sig\0/V".as_slice(), "/Sig"),
            (b"/Sig%comment\n/V".as_slice(), "/Sig"),
            (b"/Sig /V".as_slice(), "/Sig"),
            (b"/Sig\t/V".as_slice(), "/Sig"),
            (b"/Sig\r\n/V".as_slice(), "/Sig"),
        ] {
            let mut tokenizer = PdfTokenizer::new(data);
            let token = tokenizer.next_token().unwrap().expect("a name is read");
            assert_eq!(
                String::from_utf8_lossy(&token),
                first,
                "{:?} must end the name",
                String::from_utf8_lossy(data)
            );
            // And whatever ended it is skipped, rather than becoming a token
            // of its own that the next key would be read out of.
            let next = tokenizer.next_token().unwrap().expect("a second token");
            assert_eq!(String::from_utf8_lossy(&next), "/V");
        }
    }

    /// The `/Contents` extent in `verify` is computed as
    /// `tokenizer.position() - token.len()`, which is only correct while a
    /// token's length equals the bytes it consumed. Widening the delimiter set
    /// shortens some tokens, so the invariant is pinned here rather than left
    /// to hold by accident.
    #[test]
    fn a_tokens_length_is_the_bytes_it_consumed() {
        let data = b"<</FT/Sig\x0c/Contents <00FF> /X 1>>".as_slice();
        let mut tokenizer = PdfTokenizer::new(data);
        let mut previous_end = 0usize;
        while let Some(token) = tokenizer.next_token().unwrap() {
            let end = tokenizer.position();
            let start = end - token.len();
            assert!(
                start >= previous_end,
                "token {:?} starts at {start}, before the previous token ended at {previous_end}",
                String::from_utf8_lossy(&token)
            );
            assert_eq!(
                &data[start..end],
                token.as_slice(),
                "the bytes a token spans must be the token"
            );
            previous_end = end;
        }
    }

    #[test]
    fn test_byterange_back_patch() {
        let mut buffer = io::Cursor::new(vec![0u8; 500]);

        buffer.seek(SeekFrom::Start(100)).unwrap();
        buffer.write_all(&[b' '; 62]).unwrap();

        let ctx = BackPatchContext {
            byterange_start: 100,
            sig_start: 200,
            sig_end: 300,
        };

        ctx.finish(400, &mut buffer).unwrap();

        buffer.seek(SeekFrom::Start(100)).unwrap();
        let mut content = vec![0u8; 62];
        buffer.read_exact(&mut content).unwrap();

        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.starts_with("[0 200 300 100]"));
    }

    /// A hybrid-reference file must be refused *for being hybrid*.
    ///
    /// The fixture used to declare `startxref 60` while its `xref` keyword sat
    /// at 43, so the trailer was never reached, `/XRefStm` was never seen, and
    /// the bare `is_err()` assertion passed on an unrelated parse failure. The
    /// offset is computed here, and the refusal is named.
    #[test]
    fn test_incremental_writer_hybrid_xref_refused() {
        let head = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\n";
        let xref_at = head.len();
        let mut pdf_bytes = head.to_vec();
        pdf_bytes.extend_from_slice(
            b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
              trailer\n<<\n/Size 2\n/Root 1 0 R\n/XRefStm 3\n>>\nstartxref\n",
        );
        pdf_bytes.extend_from_slice(format!("{xref_at}\n%%EOF").as_bytes());
        assert_eq!(&pdf_bytes[xref_at..xref_at + 4], b"xref");

        let Err(err) = IncrementalWriter::open(&pdf_bytes) else {
            panic!("a hybrid-reference file must be refused");
        };
        assert!(
            matches!(err, PdfWriteError::HybridXrefRefused),
            "the refusal must name the hybrid cross-reference, got: {err}"
        );
    }

    /// A behaviour change on the classic path worth pinning: `open` now
    /// succeeds on a trailer that contains a nested dictionary, and reads the
    /// keys that follow it. Stopping at the first `>>` used to end the scan
    /// inside the nested dictionary, so `/Root`, `/Size` and `/ID` written
    /// after it went unseen — and an appended revision was written with a
    /// `/Size` of 0 and no `/Root`.
    #[test]
    fn a_nested_dictionary_in_a_classic_trailer_does_not_hide_later_keys() {
        let head = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\n";
        let xref_at = head.len();
        let mut pdf_bytes = head.to_vec();
        pdf_bytes
            .extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n");
        // `/Info` is nested and comes first; everything that matters follows.
        pdf_bytes.extend_from_slice(
            b"<</Info <</Producer (Test) /Nested <</Deeper true>>>> \
              /Size 2 /Root 1 0 R /ID [<0102> <0304>]>>\nstartxref\n",
        );
        pdf_bytes.extend_from_slice(format!("{xref_at}\n%%EOF").as_bytes());

        let writer = IncrementalWriter::open(&pdf_bytes).expect("a nested trailer dict opens");
        let trailer = writer.trailer();
        assert_eq!(trailer.root, Some((1, 0)), "/Root follows the nested dict");
        assert_eq!(trailer.size, 2, "/Size follows the nested dict");
        assert!(trailer.id.is_some(), "/ID follows the nested dict");
        assert!(!trailer.has_xref_stm);
    }

    /// A trailer whose `/Size` is `u32::MAX` leaves no object number free.
    /// Every xref writer computes `n + 1` from it: unchecked, that panicked a
    /// debug build and in release wrapped to `/Size 0` beside an object
    /// numbered 4294967295, a file no reader can open — written silently, and
    /// past the >4 GiB guard that lives beside it.
    #[test]
    fn a_size_that_leaves_no_object_number_free_is_refused() {
        let head = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\n";
        let xref_at = head.len();
        let mut pdf_bytes = head.to_vec();
        pdf_bytes.extend_from_slice(
            b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
              trailer\n<<\n/Size 4294967295\n/Root 1 0 R\n>>\nstartxref\n",
        );
        pdf_bytes.extend_from_slice(format!("{xref_at}\n%%EOF").as_bytes());

        let Err(err) = IncrementalWriter::open(&pdf_bytes) else {
            panic!("a /Size of u32::MAX must be refused, not wrapped");
        };
        let message = err.to_string();
        assert!(
            message.contains("4294967295") && message.contains("unchanged"),
            "the refusal must name the /Size and say the source is unchanged, got: {message}"
        );

        // One below the cap is still accepted, so the refusal is about the
        // overflow and not about the fixture.
        let ok = String::from_utf8(pdf_bytes.clone())
            .unwrap()
            .replace("/Size 4294967295", "/Size 4294967294");
        assert!(IncrementalWriter::open(ok.as_bytes()).is_ok());
    }

    #[test]
    fn open_refuses_a_size_of_zero() {
        // `/Size 0` is what `parse_trailer` leaves in place when the trailer
        // has no `/Size` at all or the value did not parse. With no reliable
        // floor to allocate object numbers from, `open` MUST refuse rather
        // than silently start allocation at object number 1, which is
        // certain to collide with the catalog.
        let head = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\n";
        let xref_at = head.len();
        let mut pdf_bytes = head.to_vec();
        pdf_bytes.extend_from_slice(
            b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
              trailer\n<<\n/Root 1 0 R\n>>\nstartxref\n",
        );
        pdf_bytes.extend_from_slice(format!("{xref_at}\n%%EOF").as_bytes());

        let Err(err) = IncrementalWriter::open(&pdf_bytes) else {
            panic!("a missing/unparseable /Size (Size 0) must be refused");
        };
        let message = err.to_string();
        assert!(
            message.contains("Size 0") && message.contains("unchanged"),
            "the refusal must name /Size 0 and say the source is unchanged, got: {message}"
        );
    }

    #[test]
    fn object_allocation_starts_past_the_real_highest_object_not_just_declared_size() {
        // §76.5: a document whose trailer declares a `/Size` far smaller than
        // the objects the cross-reference table actually lists is ordinary
        // real-world damage; the lenient reader elsewhere in this crate
        // tolerates it. Trusting `/Size` alone to seed allocation would let
        // this writer hand out an object number that a live page or content
        // stream already occupies, silently overwriting it in the signed
        // output. Build a fixture with 40 real objects but a trailer that
        // claims `/Size 3`, and check allocation starts at 41, not 3.
        let mut pdf = builder::Pdf::new();
        for i in 0..40 {
            pdf.add(format!("<< /Type /Test /N {i} >>"));
        }
        let bytes = pdf.build_with_trailer("/Size 3 /Root 1 0 R");

        let writer = IncrementalWriter::open(&bytes).expect("a lenient trailer is still openable");
        assert_eq!(
            writer.next_object_number(),
            41,
            "allocation must start past the real highest object number (40), not the \
             under-declared /Size (3)"
        );

        // Confirm end to end: appending a new object must not reuse any
        // object number already present in the source document.
        let mut out = Vec::new();
        let new_obj = writer.next_object_number();
        writer
            .append_objects(
                &mut std::io::Cursor::new(&mut out),
                &[(new_obj, 0, PdfObject::Name("Test".to_string()))],
                &[0u8; 16],
            )
            .expect("append_objects must succeed");
        assert!(
            new_obj > 40,
            "the appended object number ({new_obj}) must not collide with any of the 40 \
             existing objects"
        );
    }

    /// A minimal valid classic-xref PDF, and the offset of its cross-reference
    /// table: what an appended revision has to point its /Prev at.
    ///
    /// The testkit computes the offsets, and the offset this hands back is the
    /// one it recorded, so nothing here counts bytes.
    /// The three objects every "minimal document" fixture in this module
    /// builds: a catalog, a one-page page tree, and the page itself. Shared
    /// so the classic-xref and xref-stream fixture builders below — which
    /// cannot otherwise share code, since one uses the `Pdf` test builder
    /// and the other hand-writes a raw cross-reference stream — at least
    /// agree on the one thing that was drifting apart: the document itself.
    const MINIMAL_DOCUMENT_OBJECTS: [&str; 3] = [
        "<</Type /Catalog /Pages 2 0 R>>",
        "<</Type /Pages /Kids [3 0 R] /Count 1>>",
        "<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>",
    ];

    fn build_classic_fixture() -> (Vec<u8>, u64) {
        let mut pdf = Pdf::new();
        for object in MINIMAL_DOCUMENT_OBJECTS {
            pdf.add(object);
        }
        let bytes = pdf.build_with_trailer(
            "/Size {size} /Root 1 0 R \
             /ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]",
        );
        let xref_offset = find_startxref(&bytes).expect("the builder writes a startxref");
        (bytes, xref_offset)
    }

    #[test]
    fn test_classic_xref_append_two_objects() {
        let (fixture_bytes, _original_xref_offset) = build_classic_fixture();

        // Parse the fixture
        let writer = IncrementalWriter::open(&fixture_bytes).expect("failed to open fixture");

        // Verify we detected classic xref
        assert_eq!(writer.xref_kind, XRefKind::Table);
        assert_eq!(writer.trailer_dict.size, 4);

        // Create output buffer
        let mut output = io::Cursor::new(Vec::new());

        // Create two new objects
        let obj4 = PdfObject::Dictionary(vec![
            ("Type".to_string(), PdfObject::Name("Obj".to_string())),
            ("Value".to_string(), PdfObject::Integer(42)),
        ]);
        let obj5 = PdfObject::Dictionary(vec![
            ("Type".to_string(), PdfObject::Name("Obj".to_string())),
            ("Value".to_string(), PdfObject::Integer(99)),
        ]);

        let new_id2 = [0x21u8; 16]; // New ID second element

        // Append objects
        writer
            .append_objects(&mut output, &[(4, 0, obj4), (5, 0, obj5)], &new_id2)
            .expect("failed to append objects");

        let output_bytes = output.into_inner();

        // Verify fixture bytes are preserved
        assert!(
            output_bytes.starts_with(&fixture_bytes),
            "original bytes not preserved"
        );

        // Verify new content after fixture
        assert!(output_bytes.len() > fixture_bytes.len());

        // Parse the result to verify structure
        let parsed = IncrementalWriter::open(&output_bytes).expect("failed to parse output");

        // Check /Prev chain
        assert_eq!(parsed.trailer_dict.prev, Some(_original_xref_offset));

        // Check /ID: first element must be unchanged, second must be new
        if let Some(id_array) = parsed.trailer_dict.id {
            assert_eq!(id_array.len(), 2);
            // First ID should be preserved
            assert_eq!(
                id_array[0],
                vec![
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
                    0x0E, 0x0F, 0x10
                ]
            );
            // Second ID should be new
            assert_eq!(id_array[1], vec![0x21; 16]);
        }
    }

    #[test]
    fn test_classic_xref_double_append_chain() {
        let (fixture_bytes, original_xref) = build_classic_fixture();

        // First append
        let writer1 = IncrementalWriter::open(&fixture_bytes).expect("failed to open fixture");
        // Verify that writer1's prev_startxref points to the original fixture
        assert_eq!(writer1.prev_startxref, original_xref);

        let mut output1 = io::Cursor::new(Vec::new());
        let obj4 = PdfObject::Dictionary(vec![
            ("Type".to_string(), PdfObject::Name("Obj".to_string())),
            ("Value".to_string(), PdfObject::Integer(42)),
        ]);
        let obj5 = PdfObject::Dictionary(vec![
            ("Type".to_string(), PdfObject::Name("Obj".to_string())),
            ("Value".to_string(), PdfObject::Integer(99)),
        ]);
        let new_id2_1 = [0x21u8; 16];
        writer1
            .append_objects(&mut output1, &[(4, 0, obj4), (5, 0, obj5)], &new_id2_1)
            .expect("failed to append objects");

        let output1_bytes = output1.into_inner();

        // Parse first output to get its startxref and /Prev chain
        let parsed1 = IncrementalWriter::open(&output1_bytes).expect("failed to parse output1");
        // The parsed1.prev_startxref is the NEW xref offset in output1
        // The parsed1.trailer_dict.prev should be the original fixture's xref
        assert_eq!(parsed1.trailer_dict.prev, Some(original_xref));

        let output1_xref = parsed1.prev_startxref;

        // Second append: re-open output1 and append again
        let writer2 = IncrementalWriter::open(&output1_bytes).expect("failed to open output1");
        // writer2's prev_startxref should point to output1's new xref
        assert_eq!(writer2.prev_startxref, output1_xref);

        let mut output2 = io::Cursor::new(Vec::new());
        let obj6 = PdfObject::Dictionary(vec![
            ("Type".to_string(), PdfObject::Name("Obj".to_string())),
            ("Value".to_string(), PdfObject::Integer(123)),
        ]);
        let new_id2_2 = [0x22u8; 16];
        writer2
            .append_objects(&mut output2, &[(6, 0, obj6)], &new_id2_2)
            .expect("failed to append objects in second round");

        let output2_bytes = output2.into_inner();

        // Parse final output to verify /Prev chain
        let parsed2 = IncrementalWriter::open(&output2_bytes).expect("failed to parse output2");

        // Verify /Prev chain: output2 -> output1 -> fixture
        // parsed2.trailer_dict.prev should point to output1's xref
        assert_eq!(parsed2.trailer_dict.prev, Some(output1_xref));

        // parsed2.prev_startxref is the new xref offset in output2
        let output2_xref = parsed2.prev_startxref;
        assert!(
            output2_xref > output1_xref,
            "/Prev chain not in ascending order"
        );

        // Verify sizes make sense
        assert!(output2_bytes.len() > output1_bytes.len());
        assert!(output1_bytes.len() > fixture_bytes.len());
    }

    /// Build a minimal valid xref-stream PDF fixture with 3 objects.
    /// Returns (pdf_bytes, original_startxref_offset).
    fn build_xref_stream_fixture() -> (Vec<u8>, u64) {
        let mut buf = Vec::new();

        // Write header
        buf.extend_from_slice(b"%PDF-1.4\n");

        // Objects 1-3: catalog, pages, page — the same minimal document
        // `build_classic_fixture` builds.
        let [obj1, obj2, obj3] = MINIMAL_DOCUMENT_OBJECTS;
        let obj1_offset = buf.len() as u64;
        buf.extend_from_slice(format!("1 0 obj\n{obj1}\nendobj\n").as_bytes());
        let obj2_offset = buf.len() as u64;
        buf.extend_from_slice(format!("2 0 obj\n{obj2}\nendobj\n").as_bytes());
        let obj3_offset = buf.len() as u64;
        buf.extend_from_slice(format!("3 0 obj\n{obj3}\nendobj\n").as_bytes());

        // Object 4: xref stream (uncompressed)
        // 7 bytes per entry: 1 byte type + 4 byte offset + 2 byte generation
        let mut xref_stream_data = Vec::new();

        // Type 0: free object (generation 65535)
        xref_stream_data.push(0u8);
        xref_stream_data.extend_from_slice(&[0, 0, 0, 0]); // offset
        xref_stream_data.extend_from_slice(&[255, 255]); // gen

        // Type 1: object 1
        xref_stream_data.push(1u8);
        xref_stream_data.extend_from_slice(&((obj1_offset as u32).to_be_bytes()));
        xref_stream_data.extend_from_slice(&[0, 0]); // gen 0

        // Type 1: object 2
        xref_stream_data.push(1u8);
        xref_stream_data.extend_from_slice(&((obj2_offset as u32).to_be_bytes()));
        xref_stream_data.extend_from_slice(&[0, 0]); // gen 0

        // Type 1: object 3
        xref_stream_data.push(1u8);
        xref_stream_data.extend_from_slice(&((obj3_offset as u32).to_be_bytes()));
        xref_stream_data.extend_from_slice(&[0, 0]); // gen 0

        let xref_offset = buf.len() as u64;
        buf.extend_from_slice(b"4 0 obj\n");
        buf.extend_from_slice(
            b"<</Type /XRef /Size 4 /Root 1 0 R /W [1 4 2] /Index [0 4] /Length ",
        );
        buf.extend_from_slice(format!("{}", xref_stream_data.len()).as_bytes());
        buf.extend_from_slice(
            b" /ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]",
        );
        buf.extend_from_slice(b">>\nstream\n");
        buf.extend_from_slice(&xref_stream_data);
        buf.extend_from_slice(b"\nendstream\nendobj\n");

        // trailer points to xref stream
        buf.extend_from_slice(b"startxref\n");
        buf.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
        buf.extend_from_slice(b"%%EOF");

        (buf, xref_offset)
    }

    #[test]
    fn test_xref_stream_detection() {
        let (_fixture_bytes, _xref_offset) = build_xref_stream_fixture();
        // This test verifies the fixture builds without panicking
        // Full xref-stream append tests are implemented below
    }

    #[test]
    fn test_xref_stream_append_single_object() {
        let (fixture_bytes, _xref_offset) = build_xref_stream_fixture();

        let writer =
            IncrementalWriter::open(&fixture_bytes).expect("failed to open xref-stream fixture");

        // Verify we detected xref stream
        assert_eq!(writer.xref_kind, XRefKind::Stream);

        // Try to append a new object
        let mut output = io::Cursor::new(Vec::new());
        let obj5 = PdfObject::Dictionary(vec![(
            "Type".to_string(),
            PdfObject::Name("Obj".to_string()),
        )]);
        let new_id2 = [0x21u8; 16];

        let result = writer.append_objects(&mut output, &[(5, 0, obj5)], &new_id2);
        assert!(result.is_ok(), "xref-stream append should succeed");

        let output_bytes = output.into_inner();

        // Verify fixture bytes are preserved
        assert!(
            output_bytes.starts_with(&fixture_bytes),
            "original bytes not preserved"
        );

        // Verify new content after fixture
        assert!(output_bytes.len() > fixture_bytes.len());

        // Parse the result to verify structure
        let parsed = IncrementalWriter::open(&output_bytes).expect("failed to parse output");

        // Check that we can re-parse as xref stream
        assert_eq!(parsed.xref_kind, XRefKind::Stream);

        // Check /Prev chain
        assert!(
            parsed.trailer_dict.prev.is_some(),
            "missing /Prev in xref stream"
        );

        // Verify no "trailer" keyword appears after the original EOF
        let trailer_after_fixture = &output_bytes[fixture_bytes.len()..];
        assert!(
            !trailer_after_fixture.windows(7).any(|w| w == b"trailer"),
            "xref stream should not contain trailer keyword"
        );
    }

    /// Extract the `/Index` array and the raw stream rows from the last xref
    /// stream in `bytes`, using the file's own `startxref`.
    fn parse_last_xref_stream(bytes: &[u8]) -> (Vec<u32>, Vec<(u8, u64, u16)>) {
        let startxref = find_startxref(bytes).expect("startxref") as usize;
        let tail = &bytes[startxref..];

        let dict_end = tail
            .windows(2)
            .position(|w| w == b">>")
            .expect("dictionary end");
        let dict = std::str::from_utf8(&tail[..dict_end]).expect("dict is ascii");

        let index_start = dict.find("/Index [").expect("/Index") + "/Index [".len();
        let index_end = index_start + dict[index_start..].find(']').expect("/Index end");
        let index: Vec<u32> = dict[index_start..index_end]
            .split_whitespace()
            .map(|t| t.parse().expect("index integer"))
            .collect();

        let stream_kw = tail
            .windows(7)
            .position(|w| w == b"stream\n")
            .expect("stream keyword");
        let data_start = stream_kw + 7;
        let data_end = data_start
            + tail[data_start..]
                .windows(9)
                .position(|w| w == b"endstream")
                .expect("endstream")
            - 1; // trailing newline written after the data

        let data = &tail[data_start..data_end];
        assert_eq!(data.len() % 7, 0, "rows are not a whole number of entries");
        let mut rows = Vec::new();
        let mut i = 0;
        while i + 7 <= data.len() {
            let c = &data[i..i + 7];
            let offset = u32::from_be_bytes([c[1], c[2], c[3], c[4]]) as u64;
            let generation = u16::from_be_bytes([c[5], c[6]]);
            rows.push((c[0], offset, generation));
            i += 7;
        }

        (index, rows)
    }

    #[test]
    fn test_xref_stream_append_non_consecutive_object_numbers() {
        let (fixture_bytes, _xref_offset) = build_xref_stream_fixture();

        let writer =
            IncrementalWriter::open(&fixture_bytes).expect("failed to open xref-stream fixture");
        assert_eq!(writer.xref_kind, XRefKind::Stream);

        // A signing-shaped rewrite: the catalog (1) and page (3) are replaced,
        // and new objects are allocated above the trailer's /Size. The numbers
        // are deliberately gapped.
        let mut output = io::Cursor::new(Vec::new());
        let mk = |v: i64| PdfObject::Dictionary(vec![("Value".to_string(), PdfObject::Integer(v))]);
        let new_id2 = [0x21u8; 16];
        writer
            .append_objects(
                &mut output,
                &[(1, 0, mk(1)), (3, 0, mk(3)), (7, 0, mk(7)), (8, 0, mk(8))],
                &new_id2,
            )
            .expect("append should succeed");

        let output_bytes = output.into_inner();
        let (index, rows) = parse_last_xref_stream(&output_bytes);

        // Runs: {0,1} then {3} then {7,8,9} (9 is the xref stream itself,
        // allocated as max(size, max_obj+1) = 9, adjacent to 8).
        assert_eq!(index, vec![0, 2, 3, 1, 7, 3], "/Index runs are wrong");

        // Expand the /Index ranges: this, and not the order the objects were
        // handed to `append_objects`, is the object number each row describes.
        let mut expected_nums: Vec<u32> = Vec::new();
        for pair in index.chunks(2) {
            for k in 0..pair[1] {
                expected_nums.push(pair[0] + k);
            }
        }
        assert_eq!(
            expected_nums,
            vec![0, 1, 3, 7, 8, 9],
            "/Index does not enumerate the objects that were written"
        );
        assert_eq!(
            rows.len(),
            expected_nums.len(),
            "/Index does not cover the rows"
        );

        // Row 0 is the free-list head; every other row must point at the
        // `N 0 obj` header for its own object number.
        assert_eq!(rows[0], (0, 0, 65535));
        for (row, obj_num) in rows.iter().zip(&expected_nums).skip(1) {
            let (kind, offset, generation) = *row;
            assert_eq!(kind, 1, "object {} should be in use", obj_num);
            assert_eq!(generation, 0);
            let header = format!("{} 0 obj", obj_num);
            let at = &output_bytes[offset as usize..];
            assert!(
                at.starts_with(header.as_bytes()),
                "row for object {} points at {:?}, not {:?}",
                obj_num,
                String::from_utf8_lossy(&at[..header.len().min(at.len())]),
                header
            );
        }
    }

    #[test]
    fn test_pdf_object_string_ascii_is_literal() {
        let obj = PdfObject::String(b"Alice (the signer) \\ Ltd".to_vec());
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"(Alice \\(the signer\\) \\\\ Ltd)");
    }

    #[test]
    fn test_pdf_object_string_non_ascii_is_utf16be_with_bom() {
        let obj = PdfObject::String("Émile".as_bytes().to_vec());
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"<FEFF00C9006D0069006C0065>");

        // Round-trip: strip the delimiters, decode the hex, check the BOM and
        // recover the original text.
        let hex = std::str::from_utf8(&result[1..result.len() - 1]).unwrap();
        let units: Vec<u16> = (0..hex.len())
            .step_by(4)
            .map(|i| u16::from_str_radix(&hex[i..i + 4], 16).unwrap())
            .collect();
        assert_eq!(units[0], 0xFEFF);
        assert_eq!(String::from_utf16(&units[1..]).unwrap(), "Émile");
    }

    /// FINDING #5: a value parsed OUT of a document must be re-emitted with its
    /// bytes intact. `RawString` carries PDF bytes whose encoding we do not get
    /// to reinterpret; `String` carries pulpit's own UTF-8 text and is
    /// legitimately transcoded. Guessing between the two by testing whether the
    /// bytes happen to parse as UTF-8 silently rewrote documents: PDFDocEncoded
    /// `C3 A9` was re-emitted as UTF-16BE `00E9`, changing the text of a
    /// document that was then signed. These assert on meaning, not on today's
    /// byte pattern — a test pinned to current output would have passed on the
    /// bug.
    #[test]
    fn raw_document_bytes_are_re_emitted_verbatim() {
        for raw in [
            &b"Caf\xE9"[..],     // 0xE9 alone is not valid UTF-8
            &b"Caf\xC3\xA9"[..], // these ARE valid UTF-8 and must NOT be transcoded
            &b"Cafe"[..],        // pure ASCII
        ] {
            let mut out = Vec::new();
            PdfObject::RawString(raw.to_vec())
                .serialize(&mut out)
                .unwrap();
            let hex = std::str::from_utf8(&out[1..out.len() - 1]).unwrap();
            let decoded: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            assert_eq!(
                decoded, raw,
                "raw document bytes {raw:?} must survive re-emission unchanged"
            );
        }
    }

    #[test]
    fn test_pdf_object_string_astral_plane_round_trips() {
        let obj = PdfObject::String("a\u{1F512}".as_bytes().to_vec());
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        assert_eq!(result, b"<FEFF0061D83DDD12>");
    }

    #[test]
    fn test_xref_stream_double_append_chain() {
        let (fixture_bytes, _original_xref) = build_xref_stream_fixture();

        // First append
        let writer1 = IncrementalWriter::open(&fixture_bytes).expect("failed to open fixture");
        let mut output1 = io::Cursor::new(Vec::new());
        let obj5 = PdfObject::Dictionary(vec![(
            "Type".to_string(),
            PdfObject::Name("Obj".to_string()),
        )]);
        let new_id2_1 = [0x21u8; 16];
        writer1
            .append_objects(&mut output1, &[(5, 0, obj5)], &new_id2_1)
            .expect("failed to append objects");

        let output1_bytes = output1.into_inner();

        // Parse first output
        let parsed1 = IncrementalWriter::open(&output1_bytes).expect("failed to parse output1");
        assert_eq!(parsed1.xref_kind, XRefKind::Stream);
        // The prev_startxref of parsed1 is the location of output1's new xref stream
        let output1_xref_offset = parsed1.prev_startxref;

        // Second append: re-open output1 and append again
        let writer2 = IncrementalWriter::open(&output1_bytes).expect("failed to open output1");
        // writer2.prev_startxref should point to output1's xref stream
        assert_eq!(writer2.prev_startxref, output1_xref_offset);

        let mut output2 = io::Cursor::new(Vec::new());
        let obj6 = PdfObject::Dictionary(vec![(
            "Type".to_string(),
            PdfObject::Name("Obj".to_string()),
        )]);
        let new_id2_2 = [0x22u8; 16];
        writer2
            .append_objects(&mut output2, &[(6, 0, obj6)], &new_id2_2)
            .expect("failed to append objects in second round");

        let output2_bytes = output2.into_inner();

        // Parse final output to verify /Prev chain
        let parsed2 = IncrementalWriter::open(&output2_bytes).expect("failed to parse output2");

        // Verify it's still xref stream
        assert_eq!(parsed2.xref_kind, XRefKind::Stream);

        // Verify /Prev in output2 points to output1's xref stream
        assert_eq!(parsed2.trailer_dict.prev, Some(output1_xref_offset));

        // Verify sizes make sense
        assert!(output2_bytes.len() > output1_bytes.len());
        assert!(output1_bytes.len() > fixture_bytes.len());

        // Verify no "trailer" keyword anywhere after original EOF
        let updated_after_fixture = &output2_bytes[fixture_bytes.len()..];
        assert!(
            !updated_after_fixture.windows(7).any(|w| w == b"trailer"),
            "xref stream chain should not contain trailer keyword"
        );
    }
}
