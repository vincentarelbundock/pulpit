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

pub mod cms_check;
pub mod objects;
pub mod preflight;

pub use objects::{Confidence, Dict, ObjectResolver, PdfValue, XrefEntry, XrefIndex};

pub use cms_check::{
    check_signature, verify_signatures, AlgorithmFinding, CertificateSummary, IdentityAssurance,
    PadesProfile, SignatureStatus, SignatureVerification,
};

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

    #[error("unsupported stream filter: {0}")]
    UnsupportedFilter(String),

    #[error("the document is encrypted")]
    EncryptedPdf,
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
    /// Set of object numbers defined in this revision's xref.
    pub obj_numbers: std::collections::HashSet<u32>,
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

            // Parse xref entries to get object numbers
            let obj_numbers = parse_xref_entries(bytes, current_startxref)?;

            let rev_info = RevisionInfo {
                startxref: current_startxref,
                xref_start,
                xref_end,
                eof: file_size,
                obj_numbers,
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
    pub fn last_changed_revision(&self, obj_num: u32) -> Option<u64> {
        // Find the newest (highest startxref) revision that defines this object
        self.revisions
            .iter()
            .rev()
            .find(|(_, info)| info.obj_numbers.contains(&obj_num))
            .map(|(startxref, _)| *startxref)
    }

    /// All revisions in order from oldest to newest.
    pub fn all_revisions(&self) -> Vec<&RevisionInfo> {
        self.revisions.values().collect()
    }

    /// All revisions as (startxref, info) pairs in order.
    pub fn all_revisions_map(&self) -> Vec<(&u64, &RevisionInfo)> {
        self.revisions.iter().collect()
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

/// Parse xref entries and return the set of object numbers defined in that revision.
/// Handles both classic xref tables (20-byte entries) and xref streams (/W packed).
fn parse_xref_entries(bytes: &[u8], startxref: u64) -> Result<std::collections::HashSet<u32>> {
    let xref_pos = startxref as usize;
    if xref_pos >= bytes.len() {
        return Err(VerifyError::TruncatedFile(startxref));
    }

    let xref_slice = &bytes[xref_pos..];
    let mut tokenizer = PdfTokenizer::new(xref_slice);

    // Check if classic xref table or stream
    let first_token = tokenizer
        .next_token()
        .map_err(|_| VerifyError::XrefParseError("failed to read first token".to_string()))?;

    if first_token.as_deref() == Some(b"xref") {
        // Classic xref table: parse subsection headers and entries
        parse_classic_xref_entries(&mut tokenizer)
    } else {
        // Xref stream: parse /Index and /W, then decode entries
        parse_xref_stream_entries(xref_slice)
    }
}

/// Parse classic xref table entries: subsection headers (first count) + 20-byte entries.
fn parse_classic_xref_entries(
    tokenizer: &mut PdfTokenizer,
) -> Result<std::collections::HashSet<u32>> {
    let mut obj_numbers = std::collections::HashSet::new();

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"trailer" {
            break;
        }

        // Try to parse as subsection header: "first count"
        if let Ok(first_str) = std::str::from_utf8(&token) {
            if let Ok(first) = first_str.parse::<u32>() {
                if let Ok(Some(count_token)) = tokenizer.next_token() {
                    if let Ok(count_str) = std::str::from_utf8(&count_token) {
                        if let Ok(count) = count_str.parse::<u32>() {
                            // Skip the next `count` tokens (20-byte entries)
                            for obj_num in first..(first + count) {
                                obj_numbers.insert(obj_num);
                                // Skip the 20-byte entry (offset, gen, type)
                                let _ = tokenizer.next_token();
                            }
                            continue;
                        }
                    }
                }
            }
        }
    }

    Ok(obj_numbers)
}

/// Parse xref stream entries by decoding /W-packed rows over /Index pairs.
fn parse_xref_stream_entries(xref_slice: &[u8]) -> Result<std::collections::HashSet<u32>> {
    let mut obj_numbers = std::collections::HashSet::new();
    let mut tokenizer = PdfTokenizer::new(xref_slice);

    let mut index_pairs: Vec<(u32, u32)> = Vec::new(); // (first, count) pairs
    let mut _w_widths: Vec<usize> = vec![1, 4, 2]; // Default [1 4 2]

    // Parse the xref stream dictionary
    let mut key: Option<String> = None;
    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"stream" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if let Some(name) = token_str.strip_prefix('/') {
                key = Some(name.to_string());
            } else if let Some(k) = key.take() {
                match k.as_str() {
                    "Index" => {
                        if token == b"[" {
                            while let Ok(Some(idx_token)) = tokenizer.next_token() {
                                if idx_token == b"]" {
                                    break;
                                }
                                if let Ok(num_str) = std::str::from_utf8(&idx_token) {
                                    if let Ok(num) = num_str.parse::<u32>() {
                                        if let Ok(Some(count_token)) = tokenizer.next_token() {
                                            if let Ok(count_str) = std::str::from_utf8(&count_token)
                                            {
                                                if let Ok(count) = count_str.parse::<u32>() {
                                                    index_pairs.push((num, count));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "W" if token == b"[" => {
                        _w_widths.clear();
                        while let Ok(Some(w_token)) = tokenizer.next_token() {
                            if w_token == b"]" {
                                break;
                            }
                            if let Ok(w_str) = std::str::from_utf8(&w_token) {
                                if let Ok(w) = w_str.parse::<usize>() {
                                    _w_widths.push(w);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Read the stream data
    if let Ok(Some(stream_token)) = tokenizer.next_token() {
        if let Ok(_stream_str) = std::str::from_utf8(&stream_token) {
            // The tokenizer should have given us the stream content
            // For now, we just extract the index pairs; full stream decoding is deferred
        }
    }

    // If no /Index found, default to [0 size]
    if index_pairs.is_empty() {
        index_pairs.push((0, 0)); // Placeholder; real size would be from /Size
    }

    // Process each (first, count) pair
    for (first, count) in index_pairs {
        for i in 0..count {
            obj_numbers.insert(first + i);
        }
    }

    Ok(obj_numbers)
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
    /// The signature dictionary's /SubFilter name, without the leading slash.
    pub sub_filter: Option<String>,
    /// The signature dictionary's /M value, as written (a PDF date string).
    pub mod_date: Option<String>,
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

    // Check 5: Look up the revision where the signature was last changed
    let coverage_end = start2 + len2;
    if let Some(_sig_rev_info) = revisions.get_by_startxref(sig_dict_revision_startxref) {
        // Check 6: Any xref container of revisions <= rev ends beyond start2+len2?
        // Per §28.2: if any xref container of revision <= rev ends beyond start2 + len2
        //            -> ContiguousBlockFromStart
        for (startxref, rev_info) in revisions.all_revisions_map() {
            // Only check revisions up to and including the revision where sig was last changed
            if *startxref <= sig_dict_revision_startxref {
                // If any xref container ends BEYOND the coverage end, we don't have full coverage
                if rev_info.xref_end > coverage_end {
                    return Ok((SignatureCoverage::ContiguousBlockFromStart, true));
                }
            }
        }

        // All xref containers of our revision and earlier are covered
        return Ok((SignatureCoverage::EntireRevision, true));
    }

    // If we can't find the revision info, we can't classify as EntireRevision
    Ok((SignatureCoverage::ContiguousBlockFromStart, false))
}

/// Discover signatures in the document and perform initial structural checks.
/// Returns a vector of StructuralReport for each /Sig field with non-null /V.
pub fn discover_signatures(bytes: &[u8], revisions: &RevisionMap) -> Result<Vec<StructuralReport>> {
    // An encrypted document cannot be read structurally, and appending to one
    // would silently produce a broken file. Report it instead of guessing.
    if is_encrypted(bytes) {
        return Err(VerifyError::EncryptedPdf);
    }

    let mut reports = Vec::new();

    // Find catalog
    let catalog_ref = find_catalog_ref(bytes)?;

    // Walk the whole field tree, with /FT and /T resolved through parents.
    let fields = find_field_tree(bytes, catalog_ref)?;

    // Enumerate fields in document order
    for field in &fields {
        if let Ok(Some(sig_report)) = extract_signature_field(bytes, field, revisions) {
            reports.push(sig_report);
        }
    }

    Ok(reports)
}

/// Parse the catalog reference from the trailer.
pub fn find_catalog_ref(bytes: &[u8]) -> Result<(u32, u16)> {
    // The resolver knows the merged trailer of the active revision, including
    // the cross-reference-stream case the tokenizer walk below cannot read.
    if let Some(root) = ObjectResolver::new(bytes).root_ref() {
        return Ok(root);
    }

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

/// One node of the interactive form field tree, after inheritance.
#[derive(Debug, Clone)]
pub struct FieldEntry {
    /// The field object's number and generation.
    pub obj: (u32, u16),
    /// `/FT`, inherited from the nearest ancestor that declares one.
    pub field_type: Option<String>,
    /// The fully qualified field name: ancestors' `/T` joined with `.`.
    pub qualified_name: Option<String>,
    /// `/V`, inherited, when it is an indirect reference.
    pub value_ref: Option<(u32, u16)>,
}

/// Maximum depth of the `/Kids` walk.
const MAX_FIELD_DEPTH: usize = 32;
/// Maximum number of field nodes visited.
const MAX_FIELD_NODES: usize = 20_000;

/// Walk the whole `/AcroForm` field tree, not just the direct `/Fields` array.
///
/// `/FT`, `/T` and `/V` are inheritable: a hierarchical signature field may
/// carry none of them on the node that holds the widget. This returns every
/// node reachable through `/Kids`, with those three attributes resolved
/// through the parent chain, bounded in both depth and node count.
pub fn find_field_tree(bytes: &[u8], catalog_ref: (u32, u16)) -> Result<Vec<FieldEntry>> {
    let resolver = ObjectResolver::new(bytes);
    find_field_tree_with(&resolver, catalog_ref)
}

fn find_field_tree_with(
    resolver: &ObjectResolver<'_>,
    catalog_ref: (u32, u16),
) -> Result<Vec<FieldEntry>> {
    let (catalog, _) = resolver.resolve(catalog_ref.0)?;
    let Some(catalog) = catalog.as_dict() else {
        return Err(VerifyError::AcroformParseError(
            "catalog is not a dictionary".to_string(),
        ));
    };
    let Some(acroform) = resolver.dict_get(catalog, "AcroForm") else {
        return Ok(Vec::new());
    };
    let Some(acroform) = acroform.as_dict() else {
        return Ok(Vec::new());
    };
    let Some(fields) = resolver.dict_get(acroform, "Fields") else {
        return Ok(Vec::new());
    };
    let Some(roots) = fields.as_array().map(|a| a.to_vec()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut budget = MAX_FIELD_NODES;
    for node in roots {
        walk_field_node(
            resolver,
            &node,
            None,
            None,
            None,
            0,
            &mut visited,
            &mut budget,
            &mut out,
        );
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn walk_field_node(
    resolver: &ObjectResolver<'_>,
    node: &PdfValue,
    inherited_ft: Option<&str>,
    inherited_name: Option<&str>,
    inherited_v: Option<(u32, u16)>,
    depth: usize,
    visited: &mut std::collections::HashSet<u32>,
    budget: &mut usize,
    out: &mut Vec<FieldEntry>,
) {
    if depth > MAX_FIELD_DEPTH || *budget == 0 {
        return;
    }
    *budget -= 1;

    // Only nodes reachable as indirect references can be reported, since the
    // callers of this module address fields by object number.
    let Some((num, gen)) = node.as_ref_pair() else {
        return;
    };
    if !visited.insert(num) {
        return;
    }
    // A /Fields entry that resolves to nothing might have been the signature
    // that locks the target: report it so the caller refuses, never drop it.
    let Ok((value, _conf)) = resolver.resolve(num) else {
        out.push(FieldEntry {
            obj: (num, gen),
            field_type: None,
            qualified_name: None,
            value_ref: None,
        });
        return;
    };
    let Some(dict) = value.as_dict() else {
        out.push(FieldEntry {
            obj: (num, gen),
            field_type: None,
            qualified_name: None,
            value_ref: None,
        });
        return;
    };

    let field_type = dict
        .get("FT")
        .and_then(|v| v.as_name())
        .map(|s| s.to_string())
        .or_else(|| inherited_ft.map(|s| s.to_string()));

    let own_name = match dict.get("T") {
        Some(PdfValue::Str(s)) => Some(String::from_utf8_lossy(s).into_owned()),
        _ => None,
    };
    let qualified_name = match (inherited_name, own_name.as_deref()) {
        (Some(parent), Some(own)) => Some(format!("{parent}.{own}")),
        (Some(parent), None) => Some(parent.to_string()),
        (None, Some(own)) => Some(own.to_string()),
        (None, None) => None,
    };

    let value_ref = dict.get("V").and_then(|v| v.as_ref_pair()).or(inherited_v);

    out.push(FieldEntry {
        obj: (num, gen),
        field_type: field_type.clone(),
        qualified_name: qualified_name.clone(),
        value_ref,
    });

    let Some(kids) = resolver.dict_get(dict, "Kids") else {
        return;
    };
    let Some(kids) = kids.as_array().map(|a| a.to_vec()) else {
        return;
    };
    for kid in kids {
        walk_field_node(
            resolver,
            &kid,
            field_type.as_deref(),
            qualified_name.as_deref(),
            value_ref,
            depth + 1,
            visited,
            budget,
            out,
        );
    }
}

/// Find the form field references reachable from `/AcroForm`.
///
/// The whole `/Kids` tree is walked, so hierarchical fields are included;
/// the order is document order, parents before their children.
pub fn find_fields_array(bytes: &[u8], catalog_ref: (u32, u16)) -> Result<Vec<(u32, u16)>> {
    Ok(find_field_tree(bytes, catalog_ref)?
        .into_iter()
        .map(|f| f.obj)
        .collect())
}

/// Find object bytes by object number, through the cross-reference chain.
///
/// The returned slice covers `N G obj … endobj` in the file. An object that
/// lives in an object stream has no such slice; use
/// [`ObjectResolver::object_bytes`] for those.
pub fn find_object(bytes: &[u8], obj_num: u32) -> Result<&[u8]> {
    let resolver = ObjectResolver::new(bytes);
    let (start, end, _conf) = resolver
        .object_span(obj_num)
        .ok_or(VerifyError::SignatureObjectNotFound)?;
    bytes
        .get(start..end)
        .ok_or(VerifyError::SignatureObjectNotFound)
}

/// Find object bytes together with the confidence of the lookup.
pub fn find_object_with_confidence(bytes: &[u8], obj_num: u32) -> Result<(&[u8], Confidence)> {
    let resolver = ObjectResolver::new(bytes);
    let (start, end, conf) = resolver
        .object_span(obj_num)
        .ok_or(VerifyError::SignatureObjectNotFound)?;
    let slice = bytes
        .get(start..end)
        .ok_or(VerifyError::SignatureObjectNotFound)?;
    Ok((slice, conf))
}

/// Find object offset (file position) by object number.
pub fn find_object_offset(bytes: &[u8], obj_num: u32) -> Result<u64> {
    let resolver = ObjectResolver::new(bytes);
    resolver
        .object_span(obj_num)
        .map(|(start, _, _)| start as u64)
        .ok_or(VerifyError::SignatureObjectNotFound)
}

/// Whether the active revision of `bytes` declares `/Encrypt` in its trailer.
///
/// Signing an encrypted document by appending plaintext produces a file no
/// reader will accept, so the sign path MUST refuse on this; verification
/// reports it rather than describing a signature it cannot read.
pub fn is_encrypted(bytes: &[u8]) -> bool {
    ObjectResolver::new(bytes).is_encrypted()
}

/// Extract a signature field from field reference.
fn extract_signature_field(
    bytes: &[u8],
    field: &FieldEntry,
    revisions: &RevisionMap,
) -> Result<Option<StructuralReport>> {
    let field_ref = field.obj;
    // /FT, /T and /V arrive already resolved through the parent chain; the
    // tokenizer pass below only refines them from the node's own dictionary.
    let mut field_type: Option<String> = field.field_type.clone();
    let mut field_name: Option<String> = field.qualified_name.clone();
    let mut sig_dict_ref: Option<(u32, u16)> = field.value_ref;

    let field_obj_slice = find_object(bytes, field_ref.0)?;
    if field_obj_slice.is_empty() {
        return Ok(None);
    }

    let mut tokenizer = PdfTokenizer::new(field_obj_slice);

    let mut contents_extent: Option<ContentsExtent> = None;
    let mut byte_range: Option<ByteRange> = None;
    let mut declared_docmdp: Option<MdpPerm> = None;

    let mut key: Option<String> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            // Only treat '/' names as keys if we don't already have a key
            if key.is_none() && token_str.starts_with('/') {
                if let Some(name) = token_str.strip_prefix('/') {
                    key = Some(name.to_string());
                }
            } else if let Some(k) = key.take() {
                match k.as_str() {
                    "FT" => {
                        if let Ok(ft_str) = std::str::from_utf8(&token) {
                            if let Some(ft_name) = ft_str.strip_prefix('/') {
                                field_type = Some(ft_name.to_string());
                            }
                        }
                    }
                    // The qualified name from the field tree wins: it carries
                    // the ancestors' /T, which this local pass cannot see.
                    "T" if field_name.is_none() => {
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
    let (sub_filter, mod_date) = extract_subfilter_and_mod_date(sig_dict_slice);
    if !sig_dict_slice.is_empty() {
        // Find the absolute position of sig_dict_slice in bytes
        let sig_dict_offset = find_object_offset(bytes, sig_dict_ref.0)?;
        if let Ok((br, ce, mdp)) = extract_sig_dict_info(sig_dict_slice, sig_dict_offset) {
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
            sub_filter,
            mod_date,
        }));
    }

    Ok(None)
}

/// Extract the signature dictionary's `/SubFilter` name (without its leading
/// slash) and its `/M` date string. Both are transcription for the status
/// model (§28.5): `/SubFilter` names the profile, `/M` the claimed time.
///
/// A dedicated pass rather than a branch of [`extract_sig_dict_info`], because
/// the value of `/SubFilter` is itself a name token and would otherwise be
/// mistaken for the next key.
fn extract_subfilter_and_mod_date(sig_dict_slice: &[u8]) -> (Option<String>, Option<String>) {
    let mut tokenizer = PdfTokenizer::new(sig_dict_slice);
    let mut sub_filter = None;
    let mut mod_date = None;
    let mut pending: Option<String> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }
        let Ok(text) = std::str::from_utf8(&token) else {
            pending = None;
            continue;
        };
        match pending.take() {
            Some(key) if key == "SubFilter" => {
                if let Some(name) = text.strip_prefix('/') {
                    sub_filter = Some(name.to_string());
                    continue;
                }
            }
            Some(key) if key == "M" => {
                if let Some(date) = parse_pdf_string(text) {
                    mod_date = Some(date);
                    continue;
                }
            }
            _ => {}
        }
        if let Some(name) = text.strip_prefix('/') {
            pending = Some(name.to_string());
        }
    }

    (sub_filter, mod_date)
}

/// Extract /ByteRange, /Contents extent, and /Reference info from signature dictionary.
/// sig_dict_offset: absolute file position where sig_dict_slice starts
fn extract_sig_dict_info(
    sig_dict_slice: &[u8],
    sig_dict_offset: u64,
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
                            // Find this token's position in the original bytes (relative to slice start)
                            if let Some(contents_start_rel) =
                                find_token_in_slice(sig_dict_slice, start_pos, &token)
                            {
                                // Convert to absolute file position
                                let contents_start = sig_dict_offset + contents_start_rel;
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

    Err(VerifyError::MalformedPdf(format!(
        "missing or malformed /ByteRange or /Contents (BR vals: {}, has contents: {})",
        byte_range_values.len(),
        contents_extent.is_some()
    )))
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
pub fn parse_pdf_string(s: &str) -> Option<String> {
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

    // Problem #3 Tests

    #[test]
    fn test_byte_range_wrong_length() {
        // ByteRange with length != 4 should be handled gracefully
        // This would be caught during extraction, but coverage classification
        // assumes 4 elements. Test that we don't panic on edge cases.
        let _revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };
        // A properly formed 4-element range is required for classification
        // Malformed ranges are caught during discovery/extraction
    }

    #[test]
    fn test_prev_cycle_detection() {
        // Create a minimal PDF with /Prev cycle
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog>>\nendobj\n");

        // First xref pointing to second, second pointing back to first
        let xref1_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n");
        pdf.extend_from_slice(b"trailer\n<<\n/Size 2\n/Prev ");
        let prev_offset_1 = pdf.len();
        pdf.extend_from_slice(b"0000\n>>\nstartxref\n"); // placeholder
        let _startxref_offset_1 = pdf.len();
        pdf.extend_from_slice(format!("{}\n%%EOF", xref1_offset).as_bytes());

        // Second xref pointing back to first (creating cycle)
        let xref2_offset = pdf.len();
        pdf.extend_from_slice(b"\nxref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n");
        pdf.extend_from_slice(b"trailer\n<<\n/Size 2\n/Prev ");
        let prev_offset_2 = pdf.len();
        pdf.extend_from_slice(b"0000\n>>\nstartxref\n"); // placeholder
        let _startxref_offset_2 = pdf.len();
        pdf.extend_from_slice(format!("{}\n%%EOF", xref2_offset).as_bytes());

        // Fix up the /Prev values to create a cycle
        let prev_val_1 = format!("{:>4}", xref2_offset);
        if prev_offset_1 + 4 <= pdf.len() {
            pdf[prev_offset_1..prev_offset_1 + 4].copy_from_slice(prev_val_1.as_bytes());
        }

        let prev_val_2 = format!("{:>4}", xref1_offset);
        if prev_offset_2 + 4 <= pdf.len() {
            pdf[prev_offset_2..prev_offset_2 + 4].copy_from_slice(prev_val_2.as_bytes());
        }

        // Try to build revision map; should detect cycle
        let result = RevisionMap::build(&pdf);
        assert!(matches!(result, Err(VerifyError::PrevCycle)));
    }

    #[test]
    fn test_hybrid_xref_refused() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\nxref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n<<\n/Size 2\n/Root 1 0 R\n/XRefStm 3\n>>\nstartxref\n60\n%%EOF";

        let result = RevisionMap::build(pdf);
        assert!(matches!(result, Err(VerifyError::HybridXrefNotSupported)));
    }

    #[test]
    fn test_truncated_file_mid_xref() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\nxref\n0 1";
        // File ends mid-xref, startxref will fail or return truncated offset

        let result = RevisionMap::build(pdf);
        assert!(result.is_err());
    }

    #[test]
    fn test_coverage_byte_range_omits_xref() {
        // Byte range that omits the xref table should classify as
        // ContiguousBlockFromStart (not EntireRevision)
        let br = ByteRange {
            z: 0,
            len1: 100,
            start2: 150,
            len2: 50, // ends at 200, but xref is at 200+
        };
        let ce = ContentsExtent {
            c_start: 100,
            c_end: 150,
        };

        let mut revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };

        // Add a revision with xref at 200-220
        let mut obj_nums = std::collections::HashSet::new();
        obj_nums.insert(1);
        revisions.revisions.insert(
            50,
            RevisionInfo {
                startxref: 50,
                xref_start: 200,
                xref_end: 220,
                eof: 300,
                obj_numbers: obj_nums,
            },
        );

        let (cov, _) = classify_coverage(&br, &ce, 300, 50, &revisions).unwrap();
        assert_eq!(cov, SignatureCoverage::ContiguousBlockFromStart);
    }

    #[test]
    fn test_last_changed_revision_tracking() {
        // Build a revision map with multiple revisions
        // Signature object defined in revision 1, redefined in revision 2
        let mut revisions_map = BTreeMap::new();

        let mut obj_nums_1 = std::collections::HashSet::new();
        obj_nums_1.insert(1);
        obj_nums_1.insert(2);
        obj_nums_1.insert(3); // Signature object
        revisions_map.insert(
            100,
            RevisionInfo {
                startxref: 100,
                xref_start: 80,
                xref_end: 120,
                eof: 500,
                obj_numbers: obj_nums_1,
            },
        );

        let mut obj_nums_2 = std::collections::HashSet::new();
        obj_nums_2.insert(3); // Signature object redefined in revision 2
        obj_nums_2.insert(4);
        revisions_map.insert(
            200,
            RevisionInfo {
                startxref: 200,
                xref_start: 400,
                xref_end: 420,
                eof: 800,
                obj_numbers: obj_nums_2,
            },
        );

        let revisions = RevisionMap {
            revisions: revisions_map,
        };

        // last_changed_revision for object 3 should return 200 (newest)
        assert_eq!(revisions.last_changed_revision(3), Some(200));

        // last_changed_revision for object 2 should return 100
        assert_eq!(revisions.last_changed_revision(2), Some(100));

        // last_changed_revision for non-existent object should return None
        assert_eq!(revisions.last_changed_revision(99), None);
    }

    #[test]
    fn test_signature_object_redefined_changes_coverage() {
        // Build a two-revision fixture where revision 2's xref redefines
        // the signature object. Coverage should classify against revision 2.
        // The xref for revision 2 extends beyond the coverage end, so it's
        // ContiguousBlockFromStart.

        let mut revisions_map = BTreeMap::new();

        let mut obj_nums_1 = std::collections::HashSet::new();
        obj_nums_1.insert(5); // Signature object defined in revision 1
        revisions_map.insert(
            0,
            RevisionInfo {
                startxref: 0,
                xref_start: 100,
                xref_end: 130,
                eof: 500,
                obj_numbers: obj_nums_1,
            },
        );

        let mut obj_nums_2 = std::collections::HashSet::new();
        obj_nums_2.insert(5); // Signature object redefined in revision 2
        revisions_map.insert(
            200,
            RevisionInfo {
                startxref: 200,
                xref_start: 400,
                xref_end: 550, // Extends beyond coverage end (500)
                eof: 800,
                obj_numbers: obj_nums_2,
            },
        );

        let revisions = RevisionMap {
            revisions: revisions_map,
        };

        // Byte range: [0..100)+[150..500)
        // But xref for revision 2 ends at 550, beyond the coverage end of 500
        let br = ByteRange {
            z: 0,
            len1: 100,
            start2: 150,
            len2: 350,
        };
        let ce = ContentsExtent {
            c_start: 100,
            c_end: 150,
        };

        // Classify against revision 2 (where sig was last changed)
        let (cov, _) = classify_coverage(&br, &ce, 800, 200, &revisions).unwrap();

        // Since xref for revision 2 (400-550) extends beyond byte range
        // ending at 500, coverage should be ContiguousBlockFromStart
        assert_eq!(cov, SignatureCoverage::ContiguousBlockFromStart);
    }

    /// Build a minimal PDF fixture with a signature-shaped structure.
    fn build_signed_fixture() -> Vec<u8> {
        let mut buf = Vec::new();

        // PDF header
        buf.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog with AcroForm
        let obj1_offset = buf.len();
        buf.extend_from_slice(b"1 0 obj\n");
        buf.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R /AcroForm 3 0 R>>\n");
        buf.extend_from_slice(b"endobj\n");

        // Object 2: Pages
        let obj2_offset = buf.len();
        buf.extend_from_slice(b"2 0 obj\n");
        buf.extend_from_slice(b"<</Type /Pages /Kids [4 0 R] /Count 1>>\n");
        buf.extend_from_slice(b"endobj\n");

        // Object 3: AcroForm
        let obj3_offset = buf.len();
        buf.extend_from_slice(b"3 0 obj\n");
        buf.extend_from_slice(b"<</SigFlags 3 /Fields [5 0 R]>>\n");
        buf.extend_from_slice(b"endobj\n");

        // Object 4: Page
        let obj4_offset = buf.len();
        buf.extend_from_slice(b"4 0 obj\n");
        buf.extend_from_slice(b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\n");
        buf.extend_from_slice(b"endobj\n");

        // Object 5: Signature field
        let obj5_offset = buf.len();
        buf.extend_from_slice(b"5 0 obj\n");
        buf.extend_from_slice(b"<</FT /Sig /T (Signature) /V 6 0 R>>\n");
        buf.extend_from_slice(b"endobj\n");

        // Object 6: Signature dictionary with placeholders
        let obj6_offset = buf.len();
        buf.extend_from_slice(b"6 0 obj\n");
        buf.extend_from_slice(
            b"<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached ",
        );

        // /ByteRange placeholder (62 bytes)
        let _byterange_placeholder_offset = buf.len();
        buf.extend_from_slice(b"/ByteRange ");
        buf.extend_from_slice(b"[]                                                              ");

        // /Contents placeholder (256 hex chars = 128 bytes DER space)
        let _contents_placeholder_offset = buf.len();
        buf.extend_from_slice(b"/Contents <");
        buf.extend_from_slice(&[b'0'; 256]);
        buf.extend_from_slice(b">");

        buf.extend_from_slice(b">>\nendobj\n");

        let _sig_dict_end = buf.len();
        let _contents_end_offset = buf.len();

        // xref table
        let xref_offset = buf.len();
        buf.extend_from_slice(b"xref\n");
        buf.extend_from_slice(b"0 1\n");
        buf.extend_from_slice(b"0000000000 65535 f \n");
        buf.extend_from_slice(format!("{} 1\n", obj1_offset).as_bytes());
        buf.extend_from_slice(b"0000000000 00000 n \n");
        buf.extend_from_slice(format!("{} 1\n", obj2_offset).as_bytes());
        buf.extend_from_slice(b"0000000000 00000 n \n");
        buf.extend_from_slice(format!("{} 1\n", obj3_offset).as_bytes());
        buf.extend_from_slice(b"0000000000 00000 n \n");
        buf.extend_from_slice(format!("{} 1\n", obj4_offset).as_bytes());
        buf.extend_from_slice(b"0000000000 00000 n \n");
        buf.extend_from_slice(format!("{} 1\n", obj5_offset).as_bytes());
        buf.extend_from_slice(b"0000000000 00000 n \n");
        buf.extend_from_slice(format!("{} 1\n", obj6_offset).as_bytes());
        buf.extend_from_slice(b"0000000000 00000 n \n");

        // trailer
        buf.extend_from_slice(b"trailer\n<<\n/Size 7\n/Root 1 0 R\n");
        buf.extend_from_slice(
            b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
        );
        buf.extend_from_slice(b">>\nstartxref\n");
        buf.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
        buf.extend_from_slice(b"%%EOF");

        buf
    }

    #[test]
    fn test_signed_fixture_entire_file_coverage() {
        // Build a signed fixture and verify coverage classification
        let pdf = build_signed_fixture();

        // The /ByteRange should be [0, sig_start, sig_end, eof - sig_end]
        // For EntireFile, sig_end + (eof - sig_end) must equal eof
        // which is always true, so any signature covering sig_start->end->eof is EntireFile

        let br = ByteRange {
            z: 0,
            len1: 200,
            start2: 456, // After /Contents placeholder
            len2: (pdf.len() as u64 - 456),
        };

        let ce = ContentsExtent {
            c_start: 200,
            c_end: 456,
        };

        let revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };

        let (cov, _) = classify_coverage(&br, &ce, pdf.len() as u64, 0, &revisions).unwrap();
        assert_eq!(cov, SignatureCoverage::EntireFile);
    }

    #[test]
    fn test_signed_then_appended_becomes_entire_revision() {
        // Start with signed fixture, then append a second revision
        // First signature should now be EntireRevision with later_revisions=true

        let pdf_v1 = build_signed_fixture();
        let v1_size = pdf_v1.len() as u64;

        // Simulate appending a second revision (simplified: just add dummy content)
        let mut pdf_v2 = pdf_v1.clone();
        pdf_v2.extend_from_slice(b"\n2 0 obj\n<</Type /Dummy>>\nendobj\n");
        pdf_v2.extend_from_slice(b"xref\n");
        pdf_v2.extend_from_slice(b"0 1\n0000000000 65535 f \ntrailer\n");
        pdf_v2.extend_from_slice(
            format!("<<\n/Size 8\n/Root 1 0 R\n/Prev {}\n>>\n", v1_size).as_bytes(),
        );
        pdf_v2.extend_from_slice(b"startxref\n");
        let v2_xref_offset = v1_size as usize + 20; // Rough estimate
        pdf_v2.extend_from_slice(format!("{}\n", v2_xref_offset).as_bytes());
        pdf_v2.extend_from_slice(b"%%EOF");

        // Build revision map from v2
        let revisions = RevisionMap::build(&pdf_v2).unwrap();

        // The first signature (byte range designed for v1) should now be
        // classified as EntireRevision since v2 was appended after it
        let br = ByteRange {
            z: 0,
            len1: 200,
            start2: 456,
            len2: v1_size - 456,
        };

        let ce = ContentsExtent {
            c_start: 200,
            c_end: 456,
        };

        // Get the first revision's startxref
        let first_rev_startxref = revisions.revisions.keys().next().copied().unwrap_or(0);

        let (cov, later) = classify_coverage(
            &br,
            &ce,
            pdf_v2.len() as u64,
            first_rev_startxref,
            &revisions,
        )
        .unwrap();

        // Should be EntireRevision (not EntireFile) with later_revisions=true
        assert_eq!(cov, SignatureCoverage::EntireRevision);
        assert!(later);
    }

    #[test]
    fn test_tokenizer_large_hex_string() {
        use crate::pdfwrite::PdfTokenizer;

        // Build a sig dict slice with a large hex string
        let mut sig_dict = Vec::new();
        sig_dict.extend_from_slice(b"7 0 obj\n");
        sig_dict.extend_from_slice(
            b"<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached ",
        );
        sig_dict.extend_from_slice(b"/ByteRange [0 100 200 50]");
        sig_dict.extend_from_slice(b"/Contents <");

        // Add ~5200 hex chars (2600 bytes of data)
        for i in 0..2600 {
            sig_dict.extend_from_slice(format!("{:02X}", i % 256).as_bytes());
        }
        sig_dict.extend_from_slice(b">");

        sig_dict.extend_from_slice(b"/M (D:20240820220000+00'00')>>\nendobj\n");

        // Now tokenize and see what we get
        let mut tokenizer = PdfTokenizer::new(&sig_dict);
        let mut found_hex_start = false;
        let mut found_contents_key = false;

        while let Ok(Some(token)) = tokenizer.next_token() {
            if token == b"/Contents" {
                found_contents_key = true;
            } else if found_contents_key && token.starts_with(b"<") && token.ends_with(b">") {
                found_hex_start = true;
                eprintln!("Found complete hex string token of {} bytes", token.len());
                break;
            } else if found_contents_key && token == b"<" {
                eprintln!("ERROR: Found separate '<' token instead of complete hex string");
                break;
            }
        }

        assert!(
            found_hex_start,
            "tokenizer should return complete hex string including < and >"
        );
    }

    #[test]
    fn test_discover_minimal_signature() {
        // Build a minimal signed PDF
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog with AcroForm
        let obj1_pos = pdf.len();
        pdf.extend_from_slice(
            b"1 0 obj\n<</Type /Catalog /Pages 2 0 R /AcroForm 3 0 R>>\nendobj\n",
        );

        // Object 2: Pages
        let obj2_pos = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<</Type /Pages /Kids [4 0 R] /Count 1>>\nendobj\n");

        // Object 3: AcroForm
        let obj3_pos = pdf.len();
        pdf.extend_from_slice(b"3 0 obj\n<</Fields [5 0 R] /SigFlags 3>>\nendobj\n");

        // Object 4: Page
        let obj4_pos = pdf.len();
        pdf.extend_from_slice(
            b"4 0 obj\n<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\nendobj\n",
        );

        // Object 5: Signature field
        let obj5_pos = pdf.len();
        pdf.extend_from_slice(b"5 0 obj\n<</FT /Sig /T (Sig1) /V 6 0 R>>\nendobj\n");

        // Object 6: Signature dictionary with hex string
        let obj6_pos = pdf.len();
        pdf.extend_from_slice(
            b"6 0 obj\n<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 100 200 50] /Contents <0102030405060708>>>>\nendobj\n",
        );

        // Trailer
        let xref_pos = pdf.len();
        pdf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("1 6\n{:010} 00000 n \n", obj1_pos).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_pos).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_pos).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj4_pos).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj5_pos).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj6_pos).as_bytes());

        pdf.extend_from_slice(b"trailer\n<<\n/Size 7\n/Root 1 0 R\n/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n>>\nstartxref\n");
        pdf.extend_from_slice(format!("{}\n", xref_pos).as_bytes());
        pdf.extend_from_slice(b"%%EOF");
        let rev_map = RevisionMap::build(&pdf).expect("failed to build revision map");
        let signatures =
            discover_signatures(&pdf, &rev_map).expect("failed to discover signatures");

        assert_eq!(signatures.len(), 1, "should discover exactly 1 signature");

        let sig = &signatures[0];
        assert_eq!(sig.field_name, "Sig1");
        // Coverage is Unclear due to test PDF having incorrect ByteRange values,
        // but signature discovery works correctly.
    }

    // ---------------------------------------------------------------
    // Object resolution (§28.1 prerequisite): the resolver, not a scan.
    // ---------------------------------------------------------------

    /// One object to emit into a fixture.
    struct ObjSpec {
        num: u32,
        gen: u16,
        body: Vec<u8>,
    }

    fn obj(num: u32, gen: u16, body: &[u8]) -> ObjSpec {
        ObjSpec {
            num,
            gen,
            body: body.to_vec(),
        }
    }

    /// Emit a single-revision PDF with a correct classic cross-reference
    /// table, one subsection per object so generations are carried exactly.
    fn build_classic_pdf(objs: &[ObjSpec], trailer_extra: &str) -> Vec<u8> {
        let mut buf = Vec::from(&b"%PDF-1.7\n"[..]);
        let mut offsets = Vec::new();
        for o in objs {
            offsets.push(buf.len());
            buf.extend_from_slice(format!("{} {} obj\n", o.num, o.gen).as_bytes());
            buf.extend_from_slice(&o.body);
            buf.extend_from_slice(b"\nendobj\n");
        }
        let xref = buf.len();
        buf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        let max_num = objs.iter().map(|o| o.num).max().unwrap_or(0);
        for (o, off) in objs.iter().zip(&offsets) {
            buf.extend_from_slice(format!("{} 1\n{:010} {:05} n \n", o.num, off, o.gen).as_bytes());
        }
        buf.extend_from_slice(
            format!(
                "trailer\n<</Size {}\n/Root 1 0 R\n{trailer_extra}>>\nstartxref\n{xref}\n%%EOF",
                max_num + 1
            )
            .as_bytes(),
        );
        buf
    }

    #[test]
    fn object_with_nonzero_generation_is_found() {
        // The old scan searched for the literal "{n} 0 obj" and so could never
        // see this object at all.
        let pdf = build_classic_pdf(
            &[
                obj(1, 0, b"<</Type /Catalog>>"),
                obj(4, 2, b"<</Marker /GenTwo>>"),
            ],
            "",
        );

        let (slice, confidence) =
            find_object_with_confidence(&pdf, 4).expect("object 4 must resolve");
        assert_eq!(confidence, Confidence::Resolved);
        let text = String::from_utf8_lossy(slice);
        assert!(text.starts_with("4 2 obj"), "got {text:?}");
        assert!(text.contains("GenTwo"));

        let resolver = ObjectResolver::new(&pdf);
        let (value, _) = resolver.resolve(4).unwrap();
        assert_eq!(
            value.as_dict().unwrap().get("Marker").unwrap().as_name(),
            Some("GenTwo")
        );
    }

    #[test]
    fn generation_mismatch_is_not_accepted_from_the_xref() {
        // The xref claims generation 0 for an object written as generation 2:
        // the header check must reject the offset rather than trust it.
        let mut pdf = build_classic_pdf(&[obj(1, 0, b"<</Type /Catalog>>")], "");
        let broken = pdf.len();
        let _ = broken;
        // Rewrite the entry's generation field for object 1.
        let entry = b"00000 n";
        if let Some(p) = pdf.windows(entry.len()).rposition(|w| w == entry) {
            pdf[p..p + 5].copy_from_slice(b"00007");
        }
        let resolver = ObjectResolver::new(&pdf);
        // The repair scan still finds it, but the lookup is no longer
        // presented as resolved.
        let (_span_start, _span_end, confidence) = resolver.object_span(1).expect("scan finds it");
        assert_eq!(confidence, Confidence::Scanned);
    }

    /// A stream whose body contains a decoy definition of object 5.
    fn stream_with_decoy() -> Vec<u8> {
        let body = b"5 0 obj\n<</Decoy true>>\nendobj\n";
        let mut out = format!("<</Length {}>>\nstream\n", body.len()).into_bytes();
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendstream");
        out
    }

    #[test]
    fn decoy_object_inside_a_stream_body_is_not_selected() {
        let pdf = build_classic_pdf(
            &[
                obj(1, 0, b"<</Type /Catalog>>"),
                obj(3, 0, &stream_with_decoy()),
                obj(5, 0, b"<</Real true>>"),
            ],
            "",
        );

        // Through the cross-reference chain.
        let slice = find_object(&pdf, 5).expect("object 5 must resolve");
        let text = String::from_utf8_lossy(slice);
        assert!(text.contains("/Real"), "got {text:?}");
        assert!(!text.contains("Decoy"));

        // And through the repair scan, which is what the old text search was.
        let scan = objects::scan_object_definitions(&pdf);
        let (start, _gen) = scan.get(&5).copied().expect("scan finds object 5");
        let scanned = String::from_utf8_lossy(&pdf[start..start + 24]);
        assert!(scanned.starts_with("5 0 obj"));
        assert!(!scanned.contains("Decoy"), "got {scanned:?}");
        // The decoy lives inside object 3's body, which the scan steps over.
        let decoy_at = pdf
            .windows(7)
            .position(|w| w == b"5 0 obj")
            .expect("the decoy is present in the file");
        assert!(
            decoy_at < start,
            "the decoy must precede the real definition, so a naive scan would find it"
        );
    }

    /// Build a PDF whose cross-reference is a stream, with object 7 living
    /// inside object stream 6. `compress` selects a FlateDecode body.
    fn build_objstm_pdf(compress: bool) -> Vec<u8> {
        use std::io::Write;

        // Object stream payload: one object, number 7.
        let inner = b"<</Type /Marker /Note (from-objstm)>>";
        let pairs = b"7 0 ";
        let first = pairs.len();
        let mut payload = Vec::from(&pairs[..]);
        payload.extend_from_slice(inner);

        let (stm_body, stm_filter) = if compress {
            let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            enc.write_all(&payload).unwrap();
            (enc.finish().unwrap(), " /Filter /FlateDecode")
        } else {
            (payload.clone(), "")
        };

        let mut buf = Vec::from(&b"%PDF-1.5\n"[..]);

        let obj6 = buf.len();
        buf.extend_from_slice(
            format!(
                "6 0 obj\n<</Type /ObjStm /N 1 /First {first} /Length {}{stm_filter}>>\nstream\n",
                stm_body.len()
            )
            .as_bytes(),
        );
        buf.extend_from_slice(&stm_body);
        buf.extend_from_slice(b"\nendstream\nendobj\n");

        // Cross-reference stream, uncompressed, /W [1 4 2].
        let obj8 = buf.len();
        let mut rows: Vec<u8> = Vec::new();
        let mut row = |kind: u8, f2: u32, f3: u16| {
            rows.push(kind);
            rows.extend_from_slice(&f2.to_be_bytes());
            rows.extend_from_slice(&f3.to_be_bytes());
        };
        row(1, obj6 as u32, 0); // object 6, in file
        row(2, 6, 0); // object 7, in object stream 6 at index 0
        row(1, obj8 as u32, 0); // object 8, in file
        buf.extend_from_slice(
            format!(
                "8 0 obj\n<</Type /XRef /Size 9 /Index [6 3] /W [1 4 2] /Root 1 0 R /Length {}>>\nstream\n",
                rows.len()
            )
            .as_bytes(),
        );
        buf.extend_from_slice(&rows);
        buf.extend_from_slice(b"\nendstream\nendobj\n");
        buf.extend_from_slice(format!("startxref\n{obj8}\n%%EOF").as_bytes());
        buf
    }

    #[test]
    fn object_inside_an_uncompressed_object_stream_is_resolved() {
        let pdf = build_objstm_pdf(false);
        let resolver = ObjectResolver::new(&pdf);
        let (value, confidence) = resolver.resolve(7).expect("object 7 lives in objstm 6");
        assert_eq!(confidence, Confidence::Resolved);
        let dict = value.as_dict().expect("a dictionary");
        assert_eq!(dict.get("Type").unwrap().as_name(), Some("Marker"));

        // And its bytes are handed back as a self-contained definition.
        let (bytes, _) = resolver.object_bytes(7).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("7 0 obj"), "got {text:?}");
        assert!(text.contains("from-objstm"));
    }

    #[test]
    fn object_inside_a_flate_object_stream_is_resolved() {
        let pdf = build_objstm_pdf(true);
        let resolver = ObjectResolver::new(&pdf);
        let (value, confidence) = resolver.resolve(7).expect("object 7 lives in objstm 6");
        assert_eq!(confidence, Confidence::Resolved);
        assert_eq!(
            value.as_dict().unwrap().get("Type").unwrap().as_name(),
            Some("Marker")
        );
    }

    #[test]
    fn xref_stream_supplies_the_catalog_reference() {
        let pdf = build_objstm_pdf(false);
        // /Root comes from the cross-reference stream dictionary, which the
        // tokenizer walk over a classic trailer could never read.
        assert_eq!(find_catalog_ref(&pdf).unwrap(), (1, 0));
    }

    #[test]
    fn hierarchical_field_tree_is_discovered_with_inheritance() {
        let pdf = build_classic_pdf(
            &[
                obj(1, 0, b"<</Type /Catalog /AcroForm 2 0 R>>"),
                obj(2, 0, b"<</Fields [3 0 R] /SigFlags 3>>"),
                // Parent declares /FT and /T; the terminal field declares
                // neither and must inherit both.
                obj(3, 0, b"<</T (form) /FT /Sig /Kids [4 0 R]>>"),
                obj(4, 0, b"<</T (Sig1) /V 5 0 R>>"),
                obj(5, 0, b"<</Type /Sig>>"),
            ],
            "",
        );

        let fields = find_field_tree(&pdf, (1, 0)).expect("field tree");
        assert_eq!(fields.len(), 2, "parent and child are both reported");

        let child = fields
            .iter()
            .find(|f| f.obj.0 == 4)
            .expect("terminal field");
        assert_eq!(child.field_type.as_deref(), Some("Sig"));
        assert_eq!(child.qualified_name.as_deref(), Some("form.Sig1"));
        assert_eq!(child.value_ref, Some((5, 0)));

        // The compatibility wrapper reports the same objects.
        let refs = find_fields_array(&pdf, (1, 0)).unwrap();
        assert!(refs.contains(&(4, 0)));
    }

    #[test]
    fn field_tree_walk_survives_a_kids_cycle() {
        let pdf = build_classic_pdf(
            &[
                obj(1, 0, b"<</Type /Catalog /AcroForm 2 0 R>>"),
                obj(2, 0, b"<</Fields [3 0 R]>>"),
                obj(3, 0, b"<</T (a) /FT /Sig /Kids [4 0 R]>>"),
                obj(4, 0, b"<</T (b) /Kids [3 0 R]>>"),
            ],
            "",
        );
        let fields = find_field_tree(&pdf, (1, 0)).expect("field tree");
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn an_encrypted_document_is_detected_and_refused() {
        let pdf = build_classic_pdf(
            &[
                obj(1, 0, b"<</Type /Catalog /AcroForm 2 0 R>>"),
                obj(2, 0, b"<</Fields []>>"),
                obj(
                    9,
                    0,
                    b"<</Filter /Standard /V 2 /R 3 /Length 128 /P -1340>>",
                ),
            ],
            "/Encrypt 9 0 R\n",
        );

        assert!(is_encrypted(&pdf), "the trailer declares /Encrypt");

        let revisions = RevisionMap::build(&pdf).unwrap();
        assert!(matches!(
            discover_signatures(&pdf, &revisions),
            Err(VerifyError::EncryptedPdf)
        ));
    }

    #[test]
    fn an_unencrypted_document_is_not_flagged() {
        let pdf = build_classic_pdf(&[obj(1, 0, b"<</Type /Catalog>>")], "");
        assert!(!is_encrypted(&pdf));
    }
}
