#![forbid(unsafe_code)]

//! Minimal PDF object model and incremental update writer.
//!
//! This module provides byte-level PDF writing without cryptographic knowledge.
//! It handles:
//!
//! - Byte-range placeholder writing and back-patching (§23.2-23.4)
//! - Incremental update writing (§24)
//! - Dictionary and object serialization (§25.1-25.3)
//! - Typestate machine for split signing (§29)
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

/// The spans that must be hashed to produce the document digest.
#[derive(Debug, Clone)]
pub struct DigestSpans {
    /// First span: from start of file to sig_start
    pub first_end: u64,
    /// Second span: from sig_end to end of file
    pub second_start: u64,
}

/// Result of the digest operation: the byte spans to hash and a handle for further writing.
#[derive(Debug)]
pub struct PreparedByteRange {
    pub digest_spans: DigestSpans,
    pub eof: u64,
}

/// After the document has been completely written to a stream.
#[derive(Debug)]
pub struct BackPatchContext {
    pub byterange_start: u64,
    pub sig_start: u64,
    pub sig_end: u64,
}

impl BackPatchContext {
    /// Compute the digest spans: [0..sig_start) and [sig_end..eof)
    pub fn digest_spans(&self, _eof: u64) -> DigestSpans {
        DigestSpans {
            first_end: self.sig_start,
            second_start: self.sig_end,
        }
    }

    /// Back-patch the /ByteRange with the actual offsets.
    /// Returns the two spans to be hashed.
    pub fn finish<W: Write + Seek>(&self, eof: u64, output: &mut W) -> Result<DigestSpans> {
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

        Ok(self.digest_spans(eof))
    }
}

/// Emit /ByteRange and /Contents placeholders.
/// Returns the offsets for back-patching later.
pub fn emit_placeholders<W: Write + Seek>(
    writer: &mut W,
    bytes_reserved: usize,
) -> Result<PlaceholderOffsets> {
    // Validate bytes_reserved is even
    if !bytes_reserved.is_multiple_of(2) {
        return Err(PdfWriteError::OddReservationSize(bytes_reserved));
    }

    // Emit /ByteRange placeholder: [] + 60 spaces = 62 bytes
    let byterange_start = current_position(writer)?;
    writer.write_all(b"[]").map_err(PdfWriteError::Io)?;
    writer.write_all(&[b' '; 60]).map_err(PdfWriteError::Io)?;

    // Emit /Contents placeholder: < + bytes_reserved 0s + >
    let sig_start = current_position(writer)?;
    writer.write_all(b"<").map_err(PdfWriteError::Io)?;
    for _ in 0..bytes_reserved {
        writer.write_all(b"0").map_err(PdfWriteError::Io)?;
    }
    writer.write_all(b">").map_err(PdfWriteError::Io)?;
    let sig_end = current_position(writer)?;

    Ok(PlaceholderOffsets {
        byterange_start,
        sig_start,
        sig_end,
        bytes_reserved,
    })
}

/// Get current position in a writer. Requires seeking.
fn current_position<W: Write + Seek>(writer: &mut W) -> Result<u64> {
    writer.stream_position().map_err(PdfWriteError::Io)
}

/// Fill the /Contents reservation with hex-encoded signature bytes.
pub fn fill_signature_reservation<W: Write + Seek>(
    writer: &mut W,
    offsets: &PlaceholderOffsets,
    cms_bytes: &[u8],
) -> Result<()> {
    // Validate the offsets
    offsets.validate()?;

    let bytes_reserved = offsets.sig_end as usize - offsets.sig_start as usize - 2; // -2 for < and >
    let hex_needed = cms_bytes.len() * 2;

    if hex_needed > bytes_reserved {
        return Err(PdfWriteError::SignatureTooLarge {
            required: hex_needed,
            reserved: bytes_reserved,
        });
    }

    // Seek to just after the <
    writer
        .seek(SeekFrom::Start(offsets.sig_start + 1))
        .map_err(PdfWriteError::Io)?;

    // Write uppercase hex
    for byte in cms_bytes {
        write!(writer, "{:02X}", byte).map_err(PdfWriteError::Io)?;
    }

    Ok(())
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
                writer.write_all(b"/").map_err(PdfWriteError::Io)?;
                for byte in n.as_bytes() {
                    // PDF names require escaping for:
                    // - Whitespace and delimiters: space, tab, newline, carriage return, <, >, [, ], {, }, /, (, )
                    // - Special character #
                    // - Non-ASCII bytes
                    match *byte {
                        b' ' | b'\t' | b'\n' | b'\r' | b'<' | b'>' | b'[' | b']' | b'{' | b'}'
                        | b'/' | b'(' | b')' | b'#' => {
                            write!(writer, "#{:02X}", byte).map_err(PdfWriteError::Io)?;
                        }
                        0..=0x20 | 0x7F..=0xFF => {
                            write!(writer, "#{:02X}", byte).map_err(PdfWriteError::Io)?;
                        }
                        _ => {
                            writer.write_all(&[*byte]).map_err(PdfWriteError::Io)?;
                        }
                    }
                }
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
                    write!(writer, "/{}", key).map_err(PdfWriteError::Io)?;
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

/// Simple PDF tokenizer for reading existing PDF structures.
pub struct PdfTokenizer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PdfTokenizer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        PdfTokenizer { data, pos: 0 }
    }

    /// Skip whitespace and comments.
    fn skip_whitespace(&mut self) {
        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            if byte == b' ' || byte == b'\t' || byte == b'\n' || byte == b'\r' {
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
                while self.pos < self.data.len() {
                    let byte = self.data[self.pos];
                    if byte == b' '
                        || byte == b'\t'
                        || byte == b'\n'
                        || byte == b'\r'
                        || byte == b'<'
                        || byte == b'>'
                        || byte == b'['
                        || byte == b']'
                        || byte == b'{'
                        || byte == b'}'
                        || byte == b'/'
                        || byte == b'('
                        || byte == b')'
                    {
                        break;
                    }
                    self.pos += 1;
                }
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
                while self.pos < self.data.len() {
                    let byte = self.data[self.pos];
                    if byte == b' '
                        || byte == b'\t'
                        || byte == b'\n'
                        || byte == b'\r'
                        || byte == b'<'
                        || byte == b'>'
                        || byte == b'['
                        || byte == b']'
                        || byte == b'{'
                        || byte == b'}'
                        || byte == b'/'
                        || byte == b'('
                        || byte == b')'
                    {
                        break;
                    }
                    self.pos += 1;
                }
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

/// Typestate machine for split signing: preparation → digest → signing.
pub struct SigningSession;

impl SigningSession {
    /// Create a new signing session.
    pub fn new() -> Self {
        SigningSession
    }

    /// Prepare the document: write signature dictionaries and placeholders.
    pub fn prepare_tbs<W: Write + Seek>(
        self,
        writer: &mut W,
        bytes_reserved: usize,
    ) -> Result<TbsDocument> {
        // Emit placeholders
        let offsets = emit_placeholders(writer, bytes_reserved)?;

        Ok(TbsDocument { offsets })
    }
}

impl Default for SigningSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Document prepared for digest computation: ready to hash the byte ranges.
pub struct TbsDocument {
    offsets: PlaceholderOffsets,
}

impl TbsDocument {
    /// Consume this document and return the digest spans and context for back-patching.
    /// Caller is responsible for hashing the two spans.
    pub fn digest(self, eof: u64) -> Result<PreparedByteRangeDigest> {
        self.offsets.validate()?;
        Ok(PreparedByteRangeDigest {
            offsets: self.offsets,
            eof,
        })
    }
}

/// Result of the digest operation: the byte spans to hash and context for finalization.
pub struct PreparedByteRangeDigest {
    offsets: PlaceholderOffsets,
    eof: u64,
}

impl PreparedByteRangeDigest {
    /// Get the two byte ranges that must be hashed to produce the document digest.
    pub fn digest_spans(&self) -> DigestSpans {
        DigestSpans {
            first_end: self.offsets.sig_start,
            second_start: self.offsets.sig_end,
        }
    }

    /// Resume signing: fill the signature and back-patch the ByteRange.
    pub fn resume<W: Write + Seek>(self, writer: &mut W, cms_bytes: &[u8]) -> Result<()> {
        // Fill the signature reservation
        fill_signature_reservation(writer, &self.offsets, cms_bytes)?;

        // Back-patch the ByteRange
        let ctx = BackPatchContext {
            byterange_start: self.offsets.byterange_start,
            sig_start: self.offsets.sig_start,
            sig_end: self.offsets.sig_end,
        };
        ctx.finish(self.eof, writer)?;

        Ok(())
    }
}

/// Incremental update writer for appending to existing PDFs.
pub struct IncrementalWriter {
    original_bytes: Vec<u8>,
    #[allow(dead_code)]
    original_eof: u64,
    prev_startxref: u64, // The startxref offset being extended, for /Prev
    xref_kind: XRefKind,
    trailer_dict: TrailerDict,
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

impl IncrementalWriter {
    /// Open an existing PDF for incremental update.
    pub fn open(bytes: &[u8]) -> Result<Self> {
        // Find the last startxref
        let prev_startxref = find_startxref(bytes)?;

        // Detect xref kind and parse trailer
        let (xref_kind, trailer_dict) = parse_trailer(bytes, prev_startxref)?;

        // Check for hybrid xref (XRefStm in trailer)
        if trailer_dict.has_xref_stm {
            return Err(PdfWriteError::HybridXrefRefused);
        }

        Ok(IncrementalWriter {
            original_bytes: bytes.to_vec(),
            original_eof: bytes.len() as u64,
            prev_startxref,
            xref_kind,
            trailer_dict,
        })
    }

    /// The trailer dictionary of the revision being extended.
    pub fn trailer(&self) -> &TrailerDict {
        &self.trailer_dict
    }

    /// The first object number that is free to allocate, taken from the
    /// trailer's `/Size`.
    pub fn next_object_number(&self) -> u32 {
        self.trailer_dict.size.max(1)
    }

    /// Append multiple objects and finalize the PDF with proper xref.
    /// objects: slice of (obj_num, gen_num, PdfObject) tuples
    /// new_id2: 16 bytes for new /ID second element
    pub fn append_objects<W: Write + Seek>(
        self,
        writer: &mut W,
        objects: &[(u32, u16, PdfObject)],
        new_id2: &[u8; 16],
    ) -> Result<()> {
        // Write the original bytes
        writer
            .write_all(&self.original_bytes)
            .map_err(PdfWriteError::Io)?;

        // Track object offsets for xref: (obj_num, offset)
        let mut obj_offsets: Vec<(u32, u64)> = Vec::new();

        // Write each object and record its offset
        for (obj_num, _gen_num, obj) in objects {
            let offset = writer.stream_position().map_err(PdfWriteError::Io)?;
            obj_offsets.push((*obj_num, offset));

            writeln!(writer, "{} 0 obj", obj_num).map_err(PdfWriteError::Io)?;
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
                let xref_stream_obj_num = std::cmp::max(
                    self.trailer_dict.size,
                    objects.iter().map(|(n, _, _)| n + 1).max().unwrap_or(1),
                );
                self.write_xref_stream(writer, &obj_offsets, xref_stream_obj_num, new_id2)?;
                // Return early; xref stream writing handles trailer and startxref
                return Ok(());
            }
        }

        // Write trailer (for classic xref only; xref stream returns above)
        let new_size = std::cmp::max(
            self.trailer_dict.size,
            objects.iter().map(|(n, _, _)| n + 1).max().unwrap_or(1),
        );

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

        Ok(())
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
        obj_offsets: &[(u32, u64)],
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
        for (obj_num, offset) in obj_offsets {
            entries.push((*obj_num, 1, *offset, 0));
        }
        entries.push((xref_stream_obj_num, 1, xref_stream_offset, 0));

        entries.sort_by_key(|(n, _, _, _)| *n);
        entries.dedup_by_key(|(n, _, _, _)| *n);

        // Split into consecutive runs; each run becomes one /Index pair.
        let mut index_array: Vec<u32> = Vec::new();
        let mut run_start = entries[0].0;
        let mut run_len = 0u32;
        for (i, (obj_num, _, _, _)) in entries.iter().enumerate() {
            if i > 0 && *obj_num != entries[i - 1].0 + 1 {
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
        // /W [1 4 2] = 7 bytes per entry
        let mut xref_data = Vec::with_capacity(entries.len() * 7);
        for (_obj_num, kind, offset, generation) in &entries {
            xref_data.push(*kind);
            xref_data.extend_from_slice(&(*offset as u32).to_be_bytes());
            xref_data.extend_from_slice(&generation.to_be_bytes());
        }

        // Calculate new size
        let new_size = std::cmp::max(
            self.trailer_dict.size,
            std::cmp::max(
                obj_offsets.iter().map(|(n, _)| n + 1).max().unwrap_or(1),
                xref_stream_obj_num + 1,
            ),
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

    fn write_xref_table<W: Write>(&self, writer: &mut W, obj_offsets: &[(u32, u64)]) -> Result<()> {
        writeln!(writer, "xref").map_err(PdfWriteError::Io)?;

        // Always emit subsection for object 0 (free list head)
        writeln!(writer, "0 1").map_err(PdfWriteError::Io)?;
        writeln!(writer, "0000000000 65535 f ").map_err(PdfWriteError::Io)?;

        // Group consecutive object numbers into subsections
        if !obj_offsets.is_empty() {
            let mut subsection_start = obj_offsets[0].0;
            let mut subsection_entries = vec![obj_offsets[0]];

            for i in 1..obj_offsets.len() {
                if obj_offsets[i].0 == obj_offsets[i - 1].0 + 1 {
                    // Consecutive object number - add to current subsection
                    subsection_entries.push(obj_offsets[i]);
                } else {
                    // Gap found - emit current subsection and start a new one
                    writeln!(writer, "{} {}", subsection_start, subsection_entries.len())
                        .map_err(PdfWriteError::Io)?;
                    for (_obj_num, offset) in &subsection_entries {
                        writeln!(writer, "{:010} 00000 n ", offset).map_err(PdfWriteError::Io)?;
                    }
                    subsection_start = obj_offsets[i].0;
                    subsection_entries = vec![obj_offsets[i]];
                }
            }

            // Emit final subsection
            writeln!(writer, "{} {}", subsection_start, subsection_entries.len())
                .map_err(PdfWriteError::Io)?;
            for (_obj_num, offset) in &subsection_entries {
                writeln!(writer, "{:010} 00000 n ", offset).map_err(PdfWriteError::Io)?;
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

fn find_startxref(bytes: &[u8]) -> Result<u64> {
    // Search backwards from the end for "startxref"
    let search_window = std::cmp::min(bytes.len(), 1024);
    let start_pos = bytes.len().saturating_sub(search_window);
    let search_slice = &bytes[start_pos..];

    if let Some(pos) = search_slice.windows(9).rposition(|w| w == b"startxref") {
        let abs_pos = start_pos + pos;
        // Parse the number after startxref
        let mut tokenizer = PdfTokenizer::new(&bytes[abs_pos + 9..]);
        if let Some(token) = tokenizer.next_token()? {
            if let Ok(s) = std::str::from_utf8(&token) {
                if let Ok(offset) = s.parse::<u64>() {
                    return Ok(offset);
                }
            }
        }
    }

    Err(PdfWriteError::ParseError("startxref not found".to_string()))
}

/// Parse the trailer dictionary and detect xref kind.
fn parse_trailer(bytes: &[u8], startxref: u64) -> Result<(XRefKind, TrailerDict)> {
    let xref_pos = startxref as usize;
    if xref_pos >= bytes.len() {
        return Err(PdfWriteError::ParseError(
            "invalid startxref position".to_string(),
        ));
    }

    let xref_slice = &bytes[xref_pos..];
    let mut tokenizer = PdfTokenizer::new(xref_slice);

    // Check if it's a classic xref table or an xref stream
    // Classic table starts with "xref" keyword
    // Stream starts with an object number
    let first_token = tokenizer.next_token()?;
    let xref_kind = if first_token.as_deref() == Some(b"xref") {
        XRefKind::Table
    } else {
        XRefKind::Stream
    };

    // Parse trailer dictionary (simplified)
    let mut trailer_dict = TrailerDict {
        root: None,
        info: None,
        size: 0,
        prev: None,
        id: None,
        has_xref_stm: false,
    };

    // Look for "trailer" keyword (classic xref only)
    if xref_kind == XRefKind::Table {
        loop {
            match tokenizer.next_token()? {
                Some(token) if token == b"trailer" => break,
                None => break,
                _ => continue,
            }
        }
    }

    // Parse the trailer dictionary
    let mut key: Option<String> = None;

    while let Some(token) = tokenizer.next_token()? {
        if token == b"<<" {
            continue;
        }
        if token == b">>" {
            break;
        }

        if let Ok(key_str) = std::str::from_utf8(&token) {
            if let Some(name) = key_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                match k.as_str() {
                    "XRefStm" => {
                        trailer_dict.has_xref_stm = true;
                    }
                    "Root" => {
                        if let Ok(num_str) = std::str::from_utf8(&token) {
                            if let Ok(num) = num_str.parse::<u32>() {
                                if let Some(gen_token) = tokenizer.next_token()? {
                                    if let Ok(gen_str) = std::str::from_utf8(&gen_token) {
                                        if let Ok(gen) = gen_str.parse::<u16>() {
                                            if let Some(r_token) = tokenizer.next_token()? {
                                                if r_token == b"R" {
                                                    trailer_dict.root = Some((num, gen));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "Size" => {
                        if let Ok(num_str) = std::str::from_utf8(&token) {
                            if let Ok(num) = num_str.parse::<u32>() {
                                trailer_dict.size = num;
                            }
                        }
                    }
                    "Prev" => {
                        if let Ok(num_str) = std::str::from_utf8(&token) {
                            if let Ok(num) = num_str.parse::<u64>() {
                                trailer_dict.prev = Some(num);
                            }
                        }
                    }
                    "ID" => {
                        // Parse ID array: look for [ followed by hex strings followed by ]
                        if let Some(arr_token) = tokenizer.next_token()? {
                            if arr_token == b"[" {
                                let mut id_array = Vec::new();
                                while let Some(id_token) = tokenizer.next_token()? {
                                    if id_token == b"]" {
                                        break;
                                    }
                                    if let Ok(id_str) = std::str::from_utf8(&id_token) {
                                        if let Some(hex) = id_str
                                            .strip_prefix('<')
                                            .and_then(|s| s.strip_suffix('>'))
                                        {
                                            // Parse hex string
                                            let mut bytes = Vec::new();
                                            for i in (0..hex.len()).step_by(2) {
                                                if i + 1 < hex.len() {
                                                    if let Ok(b) =
                                                        u8::from_str_radix(&hex[i..i + 2], 16)
                                                    {
                                                        bytes.push(b);
                                                    }
                                                }
                                            }
                                            if !bytes.is_empty() {
                                                id_array.push(bytes);
                                            }
                                        }
                                    }
                                }
                                if !id_array.is_empty() {
                                    trailer_dict.id = Some(id_array);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Ok((xref_kind, trailer_dict))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_testkit::Pdf;
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
    fn test_signature_too_large() {
        let cms_bytes = vec![0u8; 200];
        let offsets = PlaceholderOffsets {
            byterange_start: 100,
            sig_start: 100,
            sig_end: 150,
            bytes_reserved: 48,
        };
        let mut buffer = io::Cursor::new(vec![0u8; 1000]);
        let result = fill_signature_reservation(&mut buffer, &offsets, &cms_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_digest_spans() {
        let ctx = BackPatchContext {
            byterange_start: 100,
            sig_start: 200,
            sig_end: 300,
        };
        let spans = ctx.digest_spans(500);
        assert_eq!(spans.first_end, 200);
        assert_eq!(spans.second_start, 300);
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

        let spans = ctx.finish(400, &mut buffer).unwrap();
        assert_eq!(spans.first_end, 200);
        assert_eq!(spans.second_start, 300);

        buffer.seek(SeekFrom::Start(100)).unwrap();
        let mut content = vec![0u8; 62];
        buffer.read_exact(&mut content).unwrap();

        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.starts_with("[0 200 300 100]"));
    }

    #[test]
    fn test_emit_placeholders() {
        let mut buffer = io::Cursor::new(Vec::new());
        let offsets = emit_placeholders(&mut buffer, 256).unwrap();

        offsets.validate().unwrap();
        assert_eq!(offsets.bytes_reserved, 256);

        let written = buffer.into_inner();
        let expected_len = 2 + 60 + 1 + 256 + 1;
        assert_eq!(written.len(), expected_len);
    }

    #[test]
    fn test_typing_session() {
        let mut buffer = io::Cursor::new(Vec::new());
        let session = SigningSession::new();
        let tbs = session.prepare_tbs(&mut buffer, 256).unwrap();

        let prepared = tbs.digest(1000).unwrap();
        let spans = prepared.digest_spans();
        assert_eq!(spans.first_end, 62);
        assert_eq!(spans.second_start, 62 + 1 + 256 + 1);
    }

    #[test]
    fn test_incremental_writer_hybrid_xref_refused() {
        let pdf_bytes = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\nxref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n<<\n/Size 2\n/Root 1 0 R\n/XRefStm 3\n>>\nstartxref\n60\n%%EOF";
        let result = IncrementalWriter::open(pdf_bytes);
        assert!(result.is_err());
    }

    /// A minimal valid classic-xref PDF, and the offset of its cross-reference
    /// table: what an appended revision has to point its /Prev at.
    ///
    /// The testkit computes the offsets, and the offset this hands back is the
    /// one it recorded, so nothing here counts bytes.
    fn build_classic_fixture() -> (Vec<u8>, u64) {
        let mut pdf = Pdf::new();
        pdf.add("<</Type /Catalog /Pages 2 0 R>>");
        pdf.add("<</Type /Pages /Kids [3 0 R] /Count 1>>");
        pdf.add("<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>");
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

        // Object 1: Catalog
        let obj1_offset = buf.len() as u64;
        buf.extend_from_slice(b"1 0 obj\n");
        buf.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R>>\n");
        buf.extend_from_slice(b"endobj\n");

        // Object 2: Pages
        let obj2_offset = buf.len() as u64;
        buf.extend_from_slice(b"2 0 obj\n");
        buf.extend_from_slice(b"<</Type /Pages /Kids [3 0 R] /Count 1>>\n");
        buf.extend_from_slice(b"endobj\n");

        // Object 3: Page
        let obj3_offset = buf.len() as u64;
        buf.extend_from_slice(b"3 0 obj\n");
        buf.extend_from_slice(b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\n");
        buf.extend_from_slice(b"endobj\n");

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
