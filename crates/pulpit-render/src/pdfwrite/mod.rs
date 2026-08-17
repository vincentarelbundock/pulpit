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
    String(Vec<u8>),
    Name(String),
    Array(Vec<PdfObject>),
    /// Dictionary with deterministic order: Vec preserves insertion order
    Dictionary(Vec<(String, PdfObject)>),
    IndirectRef {
        obj_num: u32,
        gen_num: u16,
    },
    HexString(Vec<u8>),
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
                writer.write_all(b"(").map_err(PdfWriteError::Io)?;
                for &byte in s {
                    if byte == b'(' || byte == b')' || byte == b'\\' {
                        writer.write_all(b"\\").map_err(PdfWriteError::Io)?;
                    }
                    writer.write_all(&[byte]).map_err(PdfWriteError::Io)?;
                }
                writer.write_all(b")").map_err(PdfWriteError::Io)?;
            }
            PdfObject::Name(n) => write!(writer, "/{}", n).map_err(PdfWriteError::Io)?,
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
    original_eof: u64,
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
            xref_kind,
            trailer_dict,
        })
    }

    /// Append a new signature object and finalize the PDF.
    pub fn append_signature<W: Write + Seek>(
        self,
        writer: &mut W,
        sig_obj_num: u32,
        sig_obj: &PdfObject,
        new_id2: &[u8],
    ) -> Result<()> {
        // Write the original bytes
        writer
            .write_all(&self.original_bytes)
            .map_err(PdfWriteError::Io)?;

        let new_xref_offset = self.original_eof;

        // Write the new signature object
        let obj_start = self.original_eof;
        writeln!(writer, "{} 0 obj", sig_obj_num).map_err(PdfWriteError::Io)?;
        sig_obj.serialize(writer)?;
        write!(writer, "\nendobj\n").map_err(PdfWriteError::Io)?;

        // Write xref section
        match self.xref_kind {
            XRefKind::Table => {
                writeln!(writer, "xref").map_err(PdfWriteError::Io)?;
                writeln!(writer, "0 {}", self.trailer_dict.size + 1).map_err(PdfWriteError::Io)?;
                writeln!(writer, "0000000000 65535 f ").map_err(PdfWriteError::Io)?;
                writeln!(writer, "{:010} 00000 n ", obj_start).map_err(PdfWriteError::Io)?;
            }
            XRefKind::Stream => {
                // For now, emit a simple xref stream object
                // A complete implementation would use /Filter /FlateDecode
                writeln!(writer, "{} 0 obj", sig_obj_num + 1).map_err(PdfWriteError::Io)?;
                write!(
                    writer,
                    "<<\n/Type /XRef\n/Size {}\n/W [1 1 2]\n/Index [0 {}]\nstream\n",
                    self.trailer_dict.size + 1,
                    self.trailer_dict.size + 1
                )
                .map_err(PdfWriteError::Io)?;

                // XRef stream entries (1 byte type + 1 byte field1 + 2 bytes field2)
                writer.write_all(&[0, 0, 0]).map_err(PdfWriteError::Io)?; // free entry
                writer
                    .write_all(&[
                        1,
                        ((obj_start >> 16) & 0xFF) as u8,
                        (obj_start & 0xFFFF) as u16 as u8,
                    ])
                    .map_err(PdfWriteError::Io)?; // used entry
                write!(writer, "\nendstream\nendobj\n").map_err(PdfWriteError::Io)?;
            }
        }

        // Write trailer
        write!(writer, "trailer\n<<\n").map_err(PdfWriteError::Io)?;
        if let Some(prev) = self.trailer_dict.prev {
            writeln!(writer, "/Prev {}", prev).map_err(PdfWriteError::Io)?;
        }
        if let Some((root_num, root_gen)) = self.trailer_dict.root {
            writeln!(writer, "/Root {} {} R", root_num, root_gen).map_err(PdfWriteError::Io)?;
        }
        if let Some((info_num, info_gen)) = self.trailer_dict.info {
            writeln!(writer, "/Info {} {} R", info_num, info_gen).map_err(PdfWriteError::Io)?;
        }
        writeln!(writer, "/Size {}", self.trailer_dict.size + 1).map_err(PdfWriteError::Io)?;

        // Write /ID array with preserved id1 and new id2
        if let Some(id_array) = self.trailer_dict.id {
            write!(writer, "/ID [").map_err(PdfWriteError::Io)?;
            if !id_array.is_empty() {
                writer.write_all(b"<").map_err(PdfWriteError::Io)?;
                for byte in &id_array[0] {
                    write!(writer, "{:02X}", byte).map_err(PdfWriteError::Io)?;
                }
                writer.write_all(b">").map_err(PdfWriteError::Io)?;
            }
            writer.write_all(b"<").map_err(PdfWriteError::Io)?;
            for byte in new_id2 {
                write!(writer, "{:02X}", byte).map_err(PdfWriteError::Io)?;
            }
            writer.write_all(b">").map_err(PdfWriteError::Io)?;
            writeln!(writer, "]").map_err(PdfWriteError::Io)?;
        }

        writeln!(writer, ">>").map_err(PdfWriteError::Io)?;
        write!(writer, "startxref\n{}\n%%EOF\n", new_xref_offset).map_err(PdfWriteError::Io)?;

        Ok(())
    }
}

/// Find the byte offset of the last startxref directive.
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
    let first_token = tokenizer.next_token()?;
    let xref_kind = match first_token.as_deref() {
        Some(b"xref") => XRefKind::Table,
        _ => XRefKind::Stream,
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

    // Look for "trailer" keyword
    loop {
        match tokenizer.next_token()? {
            Some(token) if token == b"trailer" => break,
            None => break,
            _ => continue,
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
        // 200 bytes = 400 hex chars, but only 48 bytes available
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
        let mut dict = Vec::new();
        dict.push(("Type".to_string(), PdfObject::Name("Sig".to_string())));
        dict.push((
            "Contents".to_string(),
            PdfObject::HexString(vec![0xAB, 0xCD]),
        ));

        let obj = PdfObject::Dictionary(dict);
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        // Verify order is preserved
        let result_str = String::from_utf8_lossy(&result);
        let type_pos = result_str.find("/Type").unwrap();
        let contents_pos = result_str.find("/Contents").unwrap();
        assert!(type_pos < contents_pos); // Type comes before Contents
    }

    #[test]
    fn test_pdf_object_real_fixed_point() {
        let obj = PdfObject::Real(0.001);
        let mut result = Vec::new();
        obj.serialize(&mut result).unwrap();
        let result_str = String::from_utf8_lossy(&result);
        // Should be "0.001", not "1e-3"
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

        // Pre-fill with spaces at position 100
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

        // Verify the content
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

        // Verify the content
        let written = buffer.into_inner();
        // Should have [] + 60 spaces + < + 256 0s + >
        let expected_len = 2 + 60 + 1 + 256 + 1;
        assert_eq!(written.len(), expected_len);
    }

    #[test]
    fn test_typing_session() {
        let mut buffer = io::Cursor::new(Vec::new());
        let session = SigningSession::new();
        let tbs = session.prepare_tbs(&mut buffer, 256).unwrap();

        // tbs should be ready for digest
        let prepared = tbs.digest(1000).unwrap();
        let spans = prepared.digest_spans();
        assert_eq!(spans.first_end, 62); // offset of <
        assert_eq!(spans.second_start, 62 + 1 + 256 + 1); // offset of > + 1
    }

    #[test]
    fn test_incremental_writer_hybrid_xref_refused() {
        // Create a minimal PDF with /XRefStm
        let pdf_bytes = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\nxref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n<<\n/Size 2\n/Root 1 0 R\n/XRefStm 3\n>>\nstartxref\n60\n%%EOF";
        let result = IncrementalWriter::open(pdf_bytes);
        assert!(result.is_err());
    }
}
