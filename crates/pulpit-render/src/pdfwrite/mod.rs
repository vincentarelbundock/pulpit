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

use std::collections::HashMap;
use std::io::{self, Seek, Write};

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
            .seek(io::SeekFrom::Start(self.byterange_start))
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

/// Fill the /Contents reservation with hex-encoded signature bytes.
pub fn fill_signature_reservation<W: Write + Seek>(
    writer: &mut W,
    sig_start: u64,
    sig_end: u64,
    cms_bytes: &[u8],
) -> Result<()> {
    let bytes_reserved = sig_end as usize - sig_start as usize - 2; // -2 for < and >
    let hex_needed = cms_bytes.len() * 2;

    if hex_needed > bytes_reserved {
        return Err(PdfWriteError::SignatureTooLarge {
            required: hex_needed,
            reserved: bytes_reserved,
        });
    }

    // Seek to just after the <
    writer
        .seek(io::SeekFrom::Start(sig_start + 1))
        .map_err(PdfWriteError::Io)?;

    // Write uppercase hex
    for byte in cms_bytes {
        write!(writer, "{:02X}", byte).map_err(PdfWriteError::Io)?;
    }

    Ok(())
}

/// Minimal PDF object representation for writing.
#[derive(Debug, Clone)]
pub enum PdfObject {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    String(Vec<u8>),
    Name(String),
    Array(Vec<PdfObject>),
    Dictionary(HashMap<String, PdfObject>),
    IndirectRef { obj_num: u32, gen_num: u16 },
    HexString(Vec<u8>),
}

impl PdfObject {
    /// Serialize this object to PDF syntax.
    pub fn serialize(&self, writer: &mut dyn Write) -> Result<()> {
        match self {
            PdfObject::Null => writer.write_all(b"null")?,
            PdfObject::Boolean(b) => writer.write_all(if *b { b"true" } else { b"false" })?,
            PdfObject::Integer(i) => write!(writer, "{}", i)?,
            PdfObject::Real(f) => write!(writer, "{}", f)?,
            PdfObject::String(s) => {
                writer.write_all(b"(")?;
                for &byte in s {
                    if byte == b'(' || byte == b')' || byte == b'\\' {
                        writer.write_all(b"\\")?;
                    }
                    writer.write_all(&[byte])?;
                }
                writer.write_all(b")")?;
            }
            PdfObject::Name(n) => {
                write!(writer, "/{}", n)?;
            }
            PdfObject::HexString(h) => {
                writer.write_all(b"<")?;
                for byte in h {
                    write!(writer, "{:02X}", byte)?;
                }
                writer.write_all(b">")?;
            }
            PdfObject::Array(arr) => {
                writer.write_all(b"[")?;
                for (i, obj) in arr.iter().enumerate() {
                    if i > 0 {
                        writer.write_all(b" ")?;
                    }
                    obj.serialize(writer)?;
                }
                writer.write_all(b"]")?;
            }
            PdfObject::Dictionary(dict) => {
                writer.write_all(b"<<")?;
                for (key, value) in dict {
                    write!(writer, "/{}", key)?;
                    writer.write_all(b" ")?;
                    value.serialize(writer)?;
                }
                writer.write_all(b">>")?;
            }
            PdfObject::IndirectRef { obj_num, gen_num } => {
                write!(writer, "{} {} R", obj_num, gen_num)?;
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
        let mut buffer = io::Cursor::new(Vec::new());
        let result = fill_signature_reservation(&mut buffer, 100, 150, &cms_bytes);
        // 200 bytes = 400 hex chars, but only 48 bytes (150-100-2) available
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
            sig_start: 9_223_372_036_854_775_807u64, // u64::MAX / 2
            sig_end: 9_223_372_036_854_775_807u64,
        };
        let mut buffer = io::Cursor::new(vec![0u8; 1000]);
        let result = ctx.finish(18_446_744_073_709_551_615u64, &mut buffer);
        // The formatted string will be "[0 9223372036854775807 9223372036854775807 9223372036854775808]"
        // which is 65 bytes, exceeding 62 byte limit
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
        buffer.seek(io::SeekFrom::Start(100)).unwrap();
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
        buffer.seek(io::SeekFrom::Start(100)).unwrap();
        let mut content = vec![0u8; 62];
        buffer.read_exact(&mut content).unwrap();

        let content_str = String::from_utf8_lossy(&content);
        assert!(content_str.starts_with("[0 200 300 100]"));
    }
}
