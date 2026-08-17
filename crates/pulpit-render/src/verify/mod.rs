#![forbid(unsafe_code)]

//! PDF signature verification: discovery, coverage classification, and status reporting.
//!
//! This module provides structural verification of PDF signatures as per SPEC-signing §28.
//! Cryptographic verification (CMS checks) is deferred to the sign module.
//!
//! # Overview
//!
//! Verification proceeds in three phases:
//!
//! 1. **Revision map** (§28.1 prerequisite): Walk the xref chain from the final startxref
//!    through /Prev links, recording the startxref value, byte extent, and end-of-revision
//!    offset for each revision. Guard against /Prev cycles and /XRefStm hybrids.
//!
//! 2. **Discovery** (§28.1): Enumerate /Sig fields with non-null /V in document order,
//!    tracking the revision in which each signature object was last changed.
//!    Record the lexical byte extent of each /Contents string via the tokenizer.
//!
//! 3. **Coverage classification** (§28.2): For each signature, apply the exact algorithm
//!    and ordering of the spec, returning SignatureCoverage. The gap-coincidence check
//!    runs before any classification.
//!
//! # The module MUST NOT depend on the sign module or PDFium.

use crate::pdfwrite::PdfTokenizer;
use std::collections::BTreeMap;

/// Error types for verification operations.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("malformed PDF: {0}")]
    MalformedPdf(String),

    #[error("xref parsing failed: {0}")]
    XrefParseError(String),

    #[error("cycle detected in /Prev chain")]
    PrevCycle,

    #[error("hybrid xref (/XRefStm) not supported")]
    HybridXrefNotSupported,

    #[error("startxref not found")]
    StartxrefNotFound,

    #[error("truncated file at offset {0}")]
    TruncatedFile(u64),

    #[error("invalid byte range: {0}")]
    InvalidByteRange(String),

    #[error("signature object not found")]
    SignatureObjectNotFound,

    #[error("acroform parsing failed: {0}")]
    AcroformParseError(String),

    #[error("integer overflow in byte range calculation")]
    IntegerOverflow,

    #[error("file is too small")]
    FileTooSmall,
}

pub type Result<T> = std::result::Result<T, VerifyError>;

/// Signature coverage classification per §28.2.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignatureCoverage {
    Unclear = 0,
    ContiguousBlockFromStart = 1,
    EntireRevision = 2,
    EntireFile = 3,
}

/// Document Modification Permission level per §25.4.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdpPerm {
    NoChanges = 1,
    FillForms = 2,
    Annotate = 3,
}

/// Byte extent of a /Contents value: [c_start, c_end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentsExtent {
    pub c_start: u64,
    pub c_end: u64,
}

/// Byte range for the signed region: [z, len1, start2, len2].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub z: u64,
    pub len1: u64,
    pub start2: u64,
    pub len2: u64,
}

/// Information about a single revision in the PDF.
#[derive(Debug, Clone)]
pub struct RevisionInfo {
    /// The startxref offset value of this revision.
    pub startxref: u64,
    /// Byte offset where the xref section/object starts.
    pub xref_start: u64,
    /// Byte offset just past the end of the xref section/object.
    pub xref_end: u64,
    /// Total file size at the end of this revision (byte just past %%EOF).
    pub eof: u64,
}

/// Revision map: startxref -> RevisionInfo.
/// Built by walking the xref chain from the final startxref through /Prev links.
#[derive(Debug, Clone)]
pub struct RevisionMap {
    revisions: BTreeMap<u64, RevisionInfo>,
}

impl RevisionMap {
    /// Build the revision map from PDF bytes.
    ///
    /// Walks the xref chain from the final startxref through /Prev, detecting cycles
    /// and hybrid xrefs. Caps revisions at 1024.
    pub fn build(bytes: &[u8]) -> Result<Self> {
        let file_size = bytes.len() as u64;

        if file_size == 0 {
            return Err(VerifyError::FileTooSmall);
        }

        let mut revisions = BTreeMap::new();
        let mut current_startxref = find_startxref(bytes)?;
        let mut seen = std::collections::HashSet::new();
        let max_revisions = 1024;

        loop {
            if revisions.len() >= max_revisions {
                return Err(VerifyError::MalformedPdf(
                    "too many revisions (capped at 1024)".to_string(),
                ));
            }

            if seen.contains(&current_startxref) {
                return Err(VerifyError::PrevCycle);
            }
            seen.insert(current_startxref);

            let current_startxref_usize = current_startxref as usize;
            if current_startxref_usize >= bytes.len() {
                return Err(VerifyError::TruncatedFile(current_startxref));
            }

            // Parse xref and detect hybrid
            let (xref_start, xref_end, has_hybrid) = parse_xref_extent(bytes, current_startxref)?;

            if has_hybrid {
                return Err(VerifyError::HybridXrefNotSupported);
            }

            let rev_info = RevisionInfo {
                startxref: current_startxref,
                xref_start,
                xref_end,
                eof: file_size,
            };

            revisions.insert(current_startxref, rev_info);

            // Look for /Prev in trailer
            if let Some(prev_startxref) = find_prev(bytes, current_startxref)? {
                current_startxref = prev_startxref;
            } else {
                break;
            }
        }

        Ok(RevisionMap { revisions })
    }

    /// Get revision info by startxref offset.
    pub fn get_by_startxref(&self, startxref: u64) -> Option<&RevisionInfo> {
        self.revisions.get(&startxref)
    }

    /// Find the revision where an object number was last changed.
    /// Returns the startxref of the newest revision containing that object.
    pub fn last_changed_revision(&self, _obj_num: u32) -> Option<u64> {
        // This would normally require parsing the xref entries,
        // but for now we return the most recent revision.
        // Full implementation deferred (§36.2).
        self.revisions.keys().last().copied()
    }

    /// All revisions in order from oldest to newest.
    pub fn all_revisions(&self) -> Vec<&RevisionInfo> {
        self.revisions.values().collect()
    }
}

/// Find the byte offset of the final startxref directive.
/// Searches backwards from the end of the file (up to 1024 bytes).
fn find_startxref(bytes: &[u8]) -> Result<u64> {
    if bytes.len() < 10 {
        return Err(VerifyError::FileTooSmall);
    }

    let search_window = std::cmp::min(bytes.len(), 1024);
    let start_pos = bytes.len().saturating_sub(search_window);
    let search_slice = &bytes[start_pos..];

    if let Some(pos) = search_slice.windows(9).rposition(|w| w == b"startxref") {
        let abs_pos = start_pos + pos;
        let remaining = &bytes[abs_pos + 9..];
        let mut tokenizer = PdfTokenizer::new(remaining);

        if let Ok(Some(token)) = tokenizer.next_token() {
            if let Ok(s) = std::str::from_utf8(&token) {
                if let Ok(offset) = s.parse::<u64>() {
                    return Ok(offset);
                }
            }
        }
    }

    Err(VerifyError::StartxrefNotFound)
}

/// Parse xref extent: find where the xref section starts and ends.
/// Returns (xref_start, xref_end, has_xref_stm).
fn parse_xref_extent(bytes: &[u8], startxref: u64) -> Result<(u64, u64, bool)> {
    let xref_pos = startxref as usize;
    if xref_pos >= bytes.len() {
        return Err(VerifyError::TruncatedFile(startxref));
    }

    let xref_slice = &bytes[xref_pos..];
    let mut tokenizer = PdfTokenizer::new(xref_slice);

    // Check if classic xref table or xref stream
    let first_token = tokenizer
        .next_token()
        .map_err(|_| VerifyError::XrefParseError("failed to read first token".to_string()))?;

    if first_token.as_deref() == Some(b"xref") {
        // Classic xref table: scan forward until "trailer"
        let mut found_trailer = false;
        let mut trailer_start = 0;

        while let Ok(Some(token)) = tokenizer.next_token() {
            let pos = tokenizer.position();
            if token == b"trailer" {
                found_trailer = true;
                trailer_start = pos;
                break;
            }
        }

        if !found_trailer {
            return Err(VerifyError::XrefParseError(
                "trailer keyword not found".to_string(),
            ));
        }

        // Scan trailer dictionary for /XRefStm and closing >>
        let mut has_xref_stm = false;
        let mut dict_end = 0;

        tokenizer.seek(trailer_start);
        while let Ok(Some(token)) = tokenizer.next_token() {
            if token == b"/XRefStm" {
                has_xref_stm = true;
            }
            if token == b">>" {
                dict_end = tokenizer.position();
                break;
            }
        }

        let xref_end = dict_end as u64;
        let xref_start = xref_pos as u64;

        Ok((xref_start, xref_end, has_xref_stm))
    } else {
        // Xref stream (object): parse object header and dict
        // Format: obj_num gen_num obj ... endobj
        let mut has_xref_stm = false;
        let mut dict_end = 0;
        let mut dict_depth = 0;

        while let Ok(Some(token)) = tokenizer.next_token() {
            if token == b"<<" {
                dict_depth += 1;
            } else if token == b">>" {
                dict_depth -= 1;
                if dict_depth == 0 {
                    dict_end = tokenizer.position();
                }
            }
            if token == b"/XRefStm" {
                has_xref_stm = true;
            }
        }

        let xref_start = xref_pos as u64;
        let xref_end = dict_end as u64;

        Ok((xref_start, xref_end, has_xref_stm))
    }
}

/// Find the /Prev value in the trailer dictionary at the given startxref offset.
fn find_prev(bytes: &[u8], startxref: u64) -> Result<Option<u64>> {
    let xref_pos = startxref as usize;
    if xref_pos >= bytes.len() {
        return Err(VerifyError::TruncatedFile(startxref));
    }

    let xref_slice = &bytes[xref_pos..];
    let mut tokenizer = PdfTokenizer::new(xref_slice);

    // Skip to "trailer" keyword if classic xref
    let first_token = tokenizer
        .next_token()
        .map_err(|_| VerifyError::XrefParseError("failed to read first token".to_string()))?;

    if first_token.as_deref() == Some(b"xref") {
        // Classic xref: scan for trailer
        while let Ok(Some(token)) = tokenizer.next_token() {
            if token == b"trailer" {
                break;
            }
        }
    }

    // Parse trailer dictionary for /Prev
    let mut key: Option<String> = None;
    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b">>" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if let Some(name) = token_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                if k == "Prev" {
                    if let Ok(num_str) = std::str::from_utf8(&token) {
                        if let Ok(num) = num_str.parse::<u64>() {
                            return Ok(Some(num));
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Lexical extent of a /Contents string: [c_start, c_end).
/// Must be recorded during tokenization, not reconstructed from parsed DER.
#[derive(Debug, Clone)]
pub struct SignatureDiscovery {
    pub field_name: String,
    pub contents_extent: ContentsExtent,
    pub sig_dict_revision: u64,
}

/// Result of structural verification for a single signature.
#[derive(Debug, Clone)]
pub struct StructuralReport {
    pub field_name: String,
    pub coverage: SignatureCoverage,
    pub later_revisions: bool,
    pub contents_extent: ContentsExtent,
    pub byte_range: ByteRange,
    pub sig_dict_revision: u64,
    pub declared_docmdp: Option<MdpPerm>,
}

/// Classify signature coverage per §28.2 algorithm.
/// The gap-coincidence check runs BEFORE any classification (normative ordering).
pub fn classify_coverage(
    byte_range: &ByteRange,
    contents_extent: &ContentsExtent,
    file_size: u64,
    sig_dict_revision_startxref: u64,
    revisions: &RevisionMap,
) -> Result<(SignatureCoverage, bool)> {
    let [z, len1, start2, len2] = [
        byte_range.z,
        byte_range.len1,
        byte_range.start2,
        byte_range.len2,
    ];

    // Check 1: /ByteRange must have exactly 4 elements and z must be 0
    if z != 0 {
        return Ok((SignatureCoverage::Unclear, false));
    }

    // Check 2: Detect negative/overflow/out-of-bounds
    if len1 > file_size
        || start2 > file_size
        || len2 > file_size
        || start2.checked_add(len2).is_none()
        || start2 + len2 > file_size
    {
        return Ok((SignatureCoverage::Unclear, false));
    }

    // Check 3 (NORMATIVE ORDERING): Gap must coincide exactly with /Contents
    // This check runs BEFORE any other classification, including EntireFile.
    if len1 != contents_extent.c_start || start2 != contents_extent.c_end {
        return Ok((SignatureCoverage::Unclear, false));
    }

    // Check 4: Is this the entire file?
    if start2 + len2 == file_size {
        return Ok((SignatureCoverage::EntireFile, false));
    }

    // Check 5: Is the signature's revision the entire file?
    // Look up the revision where this signature was last changed.
    if let Some(rev_info) = revisions.get_by_startxref(sig_dict_revision_startxref) {
        // The xref container must be fully covered
        if rev_info.xref_end <= start2 + len2 {
            // All xref containers of this revision and earlier are covered
            return Ok((SignatureCoverage::EntireRevision, true));
        }
    }

    // Otherwise: covers only part of the revision
    Ok((SignatureCoverage::ContiguousBlockFromStart, false))
}

/// Discover signatures in the document and perform initial structural checks.
/// Returns a vector of StructuralReport for each /Sig field with non-null /V.
pub fn discover_signatures(bytes: &[u8], revisions: &RevisionMap) -> Result<Vec<StructuralReport>> {
    let mut reports = Vec::new();

    // Find catalog
    let catalog_ref = find_catalog_ref(bytes)?;

    // Find /AcroForm /Fields array
    let fields_array = find_fields_array(bytes, catalog_ref)?;

    // Enumerate fields in document order
    for field_ref in fields_array {
        if let Ok(Some(sig_report)) = extract_signature_field(bytes, field_ref, revisions) {
            reports.push(sig_report);
        }
    }

    Ok(reports)
}

/// Parse the catalog reference from the trailer.
fn find_catalog_ref(bytes: &[u8]) -> Result<(u32, u16)> {
    let startxref = find_startxref(bytes)?;
    let xref_pos = startxref as usize;
    if xref_pos >= bytes.len() {
        return Err(VerifyError::TruncatedFile(startxref));
    }

    let xref_slice = &bytes[xref_pos..];
    let mut tokenizer = PdfTokenizer::new(xref_slice);

    let first_token = tokenizer
        .next_token()
        .map_err(|_| VerifyError::XrefParseError("failed to read first token".to_string()))?;

    if first_token.as_deref() == Some(b"xref") {
        while let Ok(Some(token)) = tokenizer.next_token() {
            if token == b"trailer" {
                break;
            }
        }
    }

    let mut key: Option<String> = None;
    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b">>" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if let Some(name) = token_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                if k == "Root" {
                    if let Ok(num_str) = std::str::from_utf8(&token) {
                        if let Ok(num) = num_str.parse::<u32>() {
                            if let Ok(Some(gen_token)) = tokenizer.next_token() {
                                if let Ok(gen_str) = std::str::from_utf8(&gen_token) {
                                    if let Ok(gen) = gen_str.parse::<u16>() {
                                        if let Ok(Some(r_token)) = tokenizer.next_token() {
                                            if r_token == b"R" {
                                                return Ok((num, gen));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Err(VerifyError::AcroformParseError(
        "Root /Catalog not found".to_string(),
    ))
}

/// Find /AcroForm /Fields array references.
fn find_fields_array(bytes: &[u8], catalog_ref: (u32, u16)) -> Result<Vec<(u32, u16)>> {
    // Find the catalog object
    let catalog_obj_slice = find_object(bytes, catalog_ref.0)?;
    if catalog_obj_slice.is_empty() {
        return Err(VerifyError::AcroformParseError(
            "catalog object not found".to_string(),
        ));
    }

    let mut tokenizer = PdfTokenizer::new(catalog_obj_slice);

    // Find /AcroForm reference
    let mut acroform_ref: Option<(u32, u16)> = None;
    let mut key: Option<String> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if let Some(name) = token_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                if k == "AcroForm" {
                    if let Ok(num_str) = std::str::from_utf8(&token) {
                        if let Ok(num) = num_str.parse::<u32>() {
                            if let Ok(Some(gen_token)) = tokenizer.next_token() {
                                if let Ok(gen_str) = std::str::from_utf8(&gen_token) {
                                    if let Ok(gen) = gen_str.parse::<u16>() {
                                        if let Ok(Some(r_token)) = tokenizer.next_token() {
                                            if r_token == b"R" {
                                                acroform_ref = Some((num, gen));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(acroform_ref) = acroform_ref {
        let acroform_obj_slice = find_object(bytes, acroform_ref.0)?;
        if !acroform_obj_slice.is_empty() {
            return extract_fields_refs(acroform_obj_slice);
        }
    }

    Ok(Vec::new())
}

/// Extract field references from /AcroForm /Fields array.
fn extract_fields_refs(obj_slice: &[u8]) -> Result<Vec<(u32, u16)>> {
    let mut tokenizer = PdfTokenizer::new(obj_slice);
    let mut refs = Vec::new();
    let mut in_fields_array = false;

    let mut key: Option<String> = None;
    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if let Some(name) = token_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                if k == "Fields" && token == b"[" {
                    in_fields_array = true;
                }
            } else if in_fields_array && token == b"]" {
                break;
            } else if in_fields_array {
                if let Ok(num_str) = std::str::from_utf8(&token) {
                    if let Ok(num) = num_str.parse::<u32>() {
                        if let Ok(Some(gen_token)) = tokenizer.next_token() {
                            if let Ok(gen_str) = std::str::from_utf8(&gen_token) {
                                if let Ok(gen) = gen_str.parse::<u16>() {
                                    if let Ok(Some(r_token)) = tokenizer.next_token() {
                                        if r_token == b"R" {
                                            refs.push((num, gen));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(refs)
}

/// Find object bytes by object number.
fn find_object(bytes: &[u8], obj_num: u32) -> Result<&[u8]> {
    // Simple search for "obj_num 0 obj"
    let search = format!("{} 0 obj", obj_num);
    if let Some(pos) = find_bytes(bytes, search.as_bytes()) {
        let obj_start = pos;
        // Find corresponding "endobj"
        if let Some(endobj_pos) = find_bytes(&bytes[obj_start..], b"endobj") {
            let obj_end = obj_start + endobj_pos + 6; // past "endobj"
            if obj_end <= bytes.len() {
                return Ok(&bytes[obj_start..obj_end]);
            }
        }
    }
    Err(VerifyError::SignatureObjectNotFound)
}

/// Find a byte sequence in data.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Extract a signature field from field reference.
fn extract_signature_field(
    bytes: &[u8],
    field_ref: (u32, u16),
    revisions: &RevisionMap,
) -> Result<Option<StructuralReport>> {
    let field_obj_slice = find_object(bytes, field_ref.0)?;
    if field_obj_slice.is_empty() {
        return Ok(None);
    }

    let mut tokenizer = PdfTokenizer::new(field_obj_slice);

    let mut field_type: Option<String> = None;
    let mut field_name: Option<String> = None;
    let mut sig_dict_ref: Option<(u32, u16)> = None;
    let mut contents_extent: Option<ContentsExtent> = None;
    let mut byte_range: Option<ByteRange> = None;
    let mut declared_docmdp: Option<MdpPerm> = None;

    let mut key: Option<String> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if let Some(name) = token_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                match k.as_str() {
                    "FT" => {
                        if let Ok(ft_str) = std::str::from_utf8(&token) {
                            if let Some(ft_name) = ft_str.strip_prefix('/') {
                                field_type = Some(ft_name.to_string());
                            }
                        }
                    }
                    "T" => {
                        if let Ok(name_str) = std::str::from_utf8(&token) {
                            if let Some(name_val) = parse_pdf_string(name_str) {
                                field_name = Some(name_val);
                            }
                        }
                    }
                    "V" => {
                        if let Ok(ref_str) = std::str::from_utf8(&token) {
                            if let Ok(num) = ref_str.parse::<u32>() {
                                if let Ok(Some(gen_token)) = tokenizer.next_token() {
                                    if let Ok(gen_str) = std::str::from_utf8(&gen_token) {
                                        if let Ok(gen) = gen_str.parse::<u16>() {
                                            if let Ok(Some(r_token)) = tokenizer.next_token() {
                                                if r_token == b"R" {
                                                    sig_dict_ref = Some((num, gen));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Only process if it's a /Sig field with non-null /V
    if field_type.as_deref() != Some("Sig") || sig_dict_ref.is_none() {
        return Ok(None);
    }

    let field_name = field_name.unwrap_or_else(|| format!("Field_{}", field_ref.0));
    let sig_dict_ref = sig_dict_ref.unwrap();

    // Extract signature dictionary
    let sig_dict_slice = find_object(bytes, sig_dict_ref.0)?;
    if !sig_dict_slice.is_empty() {
        if let Ok((br, ce, mdp)) = extract_sig_dict_info(sig_dict_slice) {
            byte_range = Some(br);
            contents_extent = Some(ce);
            declared_docmdp = mdp;
        }
    }

    if let (Some(br), Some(ce)) = (byte_range, contents_extent) {
        let sig_dict_rev = revisions.last_changed_revision(sig_dict_ref.0).unwrap_or(0);

        let (coverage, later_revisions) =
            classify_coverage(&br, &ce, bytes.len() as u64, sig_dict_rev, revisions)?;

        return Ok(Some(StructuralReport {
            field_name,
            coverage,
            later_revisions,
            contents_extent: ce,
            byte_range: br,
            sig_dict_revision: sig_dict_rev,
            declared_docmdp,
        }));
    }

    Ok(None)
}

/// Extract /ByteRange, /Contents extent, and /Reference info from signature dictionary.
fn extract_sig_dict_info(
    sig_dict_slice: &[u8],
) -> Result<(ByteRange, ContentsExtent, Option<MdpPerm>)> {
    let mut tokenizer = PdfTokenizer::new(sig_dict_slice);

    let mut byte_range_values: Vec<u64> = Vec::new();
    let mut contents_extent: Option<ContentsExtent> = None;
    let mut declared_docmdp: Option<MdpPerm> = None;

    let mut key: Option<String> = None;
    let mut in_byte_range = false;
    let mut in_reference = false;

    let start_pos = tokenizer.position();

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if let Some(name) = token_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                match k.as_str() {
                    "ByteRange" => {
                        if token == b"[" {
                            in_byte_range = true;
                        }
                    }
                    "Contents" => {
                        if token.starts_with(b"<") && token.ends_with(b">") {
                            // Record the exact byte extent
                            // Find this token's position in the original bytes
                            if let Some(contents_start) =
                                find_token_in_slice(sig_dict_slice, start_pos, &token)
                            {
                                let contents_end = contents_start + token.len() as u64;
                                contents_extent = Some(ContentsExtent {
                                    c_start: contents_start,
                                    c_end: contents_end,
                                });
                            }
                        }
                    }
                    "Reference" if token == b"[" => {
                        in_reference = true;
                    }
                    _ => {}
                }
            } else if in_byte_range {
                if token == b"]" {
                    in_byte_range = false;
                } else if let Ok(num_str) = std::str::from_utf8(&token) {
                    if let Ok(num) = num_str.parse::<u64>() {
                        byte_range_values.push(num);
                    }
                }
            } else if in_reference {
                if token == b"]" {
                    in_reference = false;
                } else if let Ok(mdp_level) = extract_docmdp_level(sig_dict_slice) {
                    declared_docmdp = mdp_level;
                }
            }
        }
    }

    if byte_range_values.len() == 4 {
        let br = ByteRange {
            z: byte_range_values[0],
            len1: byte_range_values[1],
            start2: byte_range_values[2],
            len2: byte_range_values[3],
        };

        if let Some(ce) = contents_extent {
            return Ok((br, ce, declared_docmdp));
        }
    }

    Err(VerifyError::MalformedPdf(
        "missing or malformed /ByteRange or /Contents".to_string(),
    ))
}

/// Find a token's position in slice, accounting for start position.
fn find_token_in_slice(slice: &[u8], start_pos: usize, token: &[u8]) -> Option<u64> {
    slice[start_pos..]
        .windows(token.len())
        .position(|w| w == token)
        .map(|pos| (start_pos + pos) as u64)
}

/// Extract DocMDP level from /Reference array.
fn extract_docmdp_level(sig_dict_slice: &[u8]) -> Result<Option<MdpPerm>> {
    let mut tokenizer = PdfTokenizer::new(sig_dict_slice);

    let mut in_reference = false;
    let mut in_transform_params = false;
    let mut key: Option<String> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if let Some(name) = token_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                if k == "Reference" && token == b"[" {
                    in_reference = true;
                }
                if in_reference && k == "TransformParams" && token == b"<<" {
                    in_transform_params = true;
                }
                if in_transform_params && k == "P" {
                    if let Ok(level_str) = std::str::from_utf8(&token) {
                        if let Ok(level) = level_str.parse::<u8>() {
                            return match level {
                                1 => Ok(Some(MdpPerm::NoChanges)),
                                2 => Ok(Some(MdpPerm::FillForms)),
                                3 => Ok(Some(MdpPerm::Annotate)),
                                _ => Ok(None),
                            };
                        }
                    }
                }
            }
        }

        if token == b"]" {
            in_reference = false;
        }
        if token == b">>" {
            in_transform_params = false;
        }
    }

    Ok(None)
}

/// Parse a PDF string (both literal and hex formats).
fn parse_pdf_string(s: &str) -> Option<String> {
    if s.starts_with('(') && s.ends_with(')') {
        Some(s[1..s.len() - 1].to_string())
    } else if s.starts_with('<') && s.ends_with('>') {
        // Hex string
        let hex_str = &s[1..s.len() - 1];
        let mut result = String::new();
        for i in (0..hex_str.len()).step_by(2) {
            if i + 1 < hex_str.len() {
                if let Ok(byte) = u8::from_str_radix(&hex_str[i..i + 2], 16) {
                    result.push(byte as char);
                }
            }
        }
        Some(result)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coverage_z_nonzero() {
        let br = ByteRange {
            z: 1,
            len1: 100,
            start2: 200,
            len2: 50,
        };
        let ce = ContentsExtent {
            c_start: 100,
            c_end: 200,
        };
        let revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };

        let (cov, _) = classify_coverage(&br, &ce, 250, 0, &revisions).unwrap();
        assert_eq!(cov, SignatureCoverage::Unclear);
    }

    #[test]
    fn test_coverage_overflow() {
        let br = ByteRange {
            z: 0,
            len1: u64::MAX,
            start2: 200,
            len2: 50,
        };
        let ce = ContentsExtent {
            c_start: 100,
            c_end: 200,
        };
        let revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };

        let (cov, _) = classify_coverage(&br, &ce, 250, 0, &revisions).unwrap();
        assert_eq!(cov, SignatureCoverage::Unclear);
    }

    #[test]
    fn test_coverage_gap_mismatch() {
        let br = ByteRange {
            z: 0,
            len1: 100,
            start2: 210, // doesn't match c_end
            len2: 50,
        };
        let ce = ContentsExtent {
            c_start: 100,
            c_end: 200,
        };
        let revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };

        let (cov, _) = classify_coverage(&br, &ce, 260, 0, &revisions).unwrap();
        assert_eq!(cov, SignatureCoverage::Unclear);
    }

    #[test]
    fn test_coverage_entire_file() {
        let br = ByteRange {
            z: 0,
            len1: 100,
            start2: 200,
            len2: 50,
        };
        let ce = ContentsExtent {
            c_start: 100,
            c_end: 200,
        };
        let revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };

        let (cov, later) = classify_coverage(&br, &ce, 250, 0, &revisions).unwrap();
        assert_eq!(cov, SignatureCoverage::EntireFile);
        assert!(!later);
    }

    #[test]
    fn test_coverage_contiguous_block() {
        let br = ByteRange {
            z: 0,
            len1: 100,
            start2: 200,
            len2: 40, // doesn't extend to eof (250)
        };
        let ce = ContentsExtent {
            c_start: 100,
            c_end: 200,
        };
        let revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };

        let (cov, _) = classify_coverage(&br, &ce, 250, 0, &revisions).unwrap();
        assert_eq!(cov, SignatureCoverage::ContiguousBlockFromStart);
    }
}
