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
//!
//! # Behaviour changes on the classic-cross-reference path
//!
//! Two of the fixes below move real behaviour for plain classic files, not
//! only for cross-reference streams, and both are pinned by fixtures:
//!
//! * **Coverage classification is genuinely stricter.** A classic table's
//!   `xref_end` is now the *absolute* file offset past its `trailer`
//!   dictionary. It used to be reported relative to `startxref`, which
//!   under-reports for every revision but the first, so a signature that
//!   stopped short of its own cross-reference data could pass as covering its
//!   whole revision. Documents that classified as `EntireRevision` before may
//!   now classify as `ContiguousBlockFromStart` — correctly.
//!   (`a_classic_files_coverage_is_classified_against_its_real_container_end`)
//!
//! * **A nested dictionary in a trailer no longer truncates the scan.** Both
//!   both `objects::xref_section_object_numbers` and `pdfwrite::IncrementalWriter::open`
//!   count dictionary depth, so `/Root`, `/Size`, `/ID` and `/XRefStm` written
//!   after an inline `/Info` are seen. `open` therefore now *succeeds* on
//!   documents it used to open with an empty trailer — and hybrid files it
//!   used to read as plain classic are now refused as hybrid.
//!   (`a_nested_dictionary_in_a_classic_trailer_does_not_hide_xrefstm`,
//!   `a_nested_dictionary_in_a_classic_trailer_does_not_hide_later_keys`)

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
    /// Set of object numbers defined in this revision's xref.
    pub obj_numbers: std::collections::HashSet<u32>,
    /// Position in the `/Prev` chain: `0` is the revision the final
    /// `startxref` names, and each `/Prev` step is one older.
    ///
    /// Revision order used to be proxied by the `startxref` *offset*, on the
    /// assumption that a later revision is always written further into the
    /// file. A `/Prev` that points forward — which a linearized document does
    /// legitimately, and which a crafted one does on purpose — breaks that
    /// assumption, and a section that sorted as if it were older had its
    /// `xref_end` filtered out of the coverage loop and never compared against
    /// the signature's coverage end. Order is now the chain itself, which is
    /// what "later revision" means.
    pub chain_position: usize,
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
    /// and hybrid xrefs. Caps revisions at [`objects::MAX_XREF_CHAIN`].
    pub fn build(bytes: &[u8]) -> Result<Self> {
        let file_size = bytes.len() as u64;

        if file_size == 0 {
            return Err(VerifyError::FileTooSmall);
        }

        let mut revisions = BTreeMap::new();
        let mut current_startxref = find_startxref(bytes)?;
        let mut seen = std::collections::HashSet::new();
        // One cap, shared with the resolver. They used to disagree — 1024 here
        // against 256 there — so a document with a chain between the two got a
        // revision map while `ObjectResolver` silently degraded to a repair
        // scan, and the two halves of verification then described different
        // documents.
        let max_revisions = objects::MAX_XREF_CHAIN;
        // One decompression budget for the whole pass, so a chain of sections
        // that each decode to the per-stream cap cannot multiply into
        // gigabytes of inflate.
        let mut budget = objects::DecodeBudget::new();
        let mut chain_position = 0usize;

        loop {
            if revisions.len() >= max_revisions {
                return Err(VerifyError::MalformedPdf(format!(
                    "too many revisions (capped at {max_revisions})"
                )));
            }

            if seen.contains(&current_startxref) {
                return Err(VerifyError::PrevCycle);
            }
            seen.insert(current_startxref);

            let current_startxref_usize = current_startxref as usize;
            if current_startxref_usize >= bytes.len() {
                return Err(VerifyError::TruncatedFile(current_startxref));
            }

            // Parse the whole cross-reference section once: its entries,
            // its own container end, and its `/Prev`/`/XRefStm` in one pass
            // through the real parser, rather than a second token search for
            // the extent and a third for `/Prev` that could each read the
            // trailer differently than this one does.
            let section =
                objects::xref_section_object_numbers(bytes, current_startxref, &mut budget)?;

            if section.xref_stm.is_some() {
                return Err(VerifyError::HybridXrefNotSupported);
            }

            let xref_start = current_startxref;
            let xref_end = section.end;
            let obj_numbers = section
                .entries
                .into_iter()
                .map(|(number, _)| number)
                .collect();
            let prev = section.prev;

            let rev_info = RevisionInfo {
                startxref: current_startxref,
                xref_start,
                xref_end,
                obj_numbers,
                chain_position,
            };

            revisions.insert(current_startxref, rev_info);

            // Look for /Prev in trailer.
            //
            // A `/Prev` that points *forward* is not refused, and that is a
            // deliberate choice rather than an omission. It reads like an
            // impossibility — an incremental update appends, so the section it
            // supersedes must lie behind it — but ISO 32000-1 Annex F makes it
            // ordinary: a linearized document puts its first-page
            // cross-reference table at the *front* of the file and gives that
            // table a `/Prev` pointing at the main table near the end.
            // Refusing a forward `/Prev` would refuse every web-optimised PDF.
            //
            // What the defect actually was is order: revision order was
            // proxied by the `startxref` offset, so a section written earlier
            // in the file than the one superseding it sorted as if it were
            // newer and dropped out of the coverage comparison. Order is now
            // `chain_position`, which is the chain itself. See
            // `RevisionInfo::chain_position`.
            if let Some(prev_startxref) = prev {
                current_startxref = prev_startxref;
                chain_position += 1;
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
    ///
    /// "Newest" is the smallest [`RevisionInfo::chain_position`], not the
    /// largest offset: see that field for why the two are not the same thing.
    pub fn last_changed_revision(&self, obj_num: u32) -> Option<u64> {
        self.revisions
            .values()
            .filter(|info| info.obj_numbers.contains(&obj_num))
            .min_by_key(|info| info.chain_position)
            .map(|info| info.startxref)
    }

    /// All revisions in chain order, from oldest to newest.
    pub fn all_revisions(&self) -> Vec<&RevisionInfo> {
        self.all_revisions_map()
            .into_iter()
            .map(|(_, info)| info)
            .collect()
    }

    /// All revisions as (startxref, info) pairs, oldest to newest.
    pub fn all_revisions_map(&self) -> Vec<(&u64, &RevisionInfo)> {
        let mut out: Vec<(&u64, &RevisionInfo)> = self.revisions.iter().collect();
        // Oldest first: the largest chain position is the deepest `/Prev`.
        out.sort_by_key(|(_, info)| std::cmp::Reverse(info.chain_position));
        out
    }
}

/// Find the byte offset of the final startxref directive.
///
/// The window and the parse come from `pdfwrite`, which owns the tokenizer
/// this file already borrows: the revision walk and the object resolver have
/// to find the same `startxref`, or they describe different documents.
fn find_startxref(bytes: &[u8]) -> Result<u64> {
    if bytes.len() < 10 {
        return Err(VerifyError::FileTooSmall);
    }
    crate::pdfwrite::find_startxref_offset(bytes).ok_or(VerifyError::StartxrefNotFound)
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
    if let Some(sig_rev_info) = revisions.get_by_startxref(sig_dict_revision_startxref) {
        // Check 6: Any xref container of revisions <= rev ends beyond start2+len2?
        // Per §28.2: if any xref container of revision <= rev ends beyond start2 + len2
        //            -> ContiguousBlockFromStart
        //
        // "Revision <= rev" is a position in the `/Prev` chain, not a byte
        // offset. Comparing offsets let a section written earlier in the file
        // than the one that supersedes it drop out of this loop entirely, so
        // its `xref_end` was never compared against the coverage end.
        for rev_info in revisions.all_revisions() {
            // Only check revisions up to and including the revision where sig was last changed
            if rev_info.chain_position >= sig_rev_info.chain_position {
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
    // One resolver for the whole pass. Building it decodes every
    // cross-reference stream in the chain and caches every object stream it
    // touches, so building one per field lookup — which is what calling the
    // free functions in a loop did — re-did all of that up to MAX_FIELD_NODES
    // times. A crafted 1.3 MB document with 32 chained cross-reference streams
    // took hours; with one resolver it is a single pass.
    discover_signatures_with(&ObjectResolver::new(bytes), bytes, revisions)
}

/// [`discover_signatures`], against a resolver the caller already built.
///
/// §78.3: the signing path calls this (through [`count_signatures`]) as one
/// of about ten `ObjectResolver::new` calls per signing pass, each redoing
/// the same cross-reference chain and object-stream decoding. Sharing one
/// resolver end to end removes this one from that count.
pub fn discover_signatures_with(
    resolver: &ObjectResolver<'_>,
    bytes: &[u8],
    revisions: &RevisionMap,
) -> Result<Vec<StructuralReport>> {
    // An encrypted document cannot be read structurally, and appending to one
    // would silently produce a broken file. Report it instead of guessing.
    if resolver.is_encrypted() {
        return Err(VerifyError::EncryptedPdf);
    }

    let mut reports = Vec::new();

    // Find catalog
    let catalog_ref = find_catalog_ref_with(resolver, bytes)?;

    // Walk the whole field tree, with /FT and /T resolved through parents.
    let fields = find_field_tree_with(resolver, catalog_ref)?;

    // Enumerate fields in document order
    for field in &fields {
        match extract_signature_field(resolver, bytes, field, revisions) {
            Ok(Some(sig_report)) => {
                reports.push(sig_report);
            }
            Ok(None) => {
                // Not a signature field, skip it
            }
            // A field that is definitively some other kind — a text box, a
            // button, a choice — must not surface as a broken signature just
            // because its object is malformed: an unsigned document would
            // display a signature warning it never earned. `None` is NOT
            // excluded here, because a field whose object could not be read
            // has an undetermined `/FT`, and silently dropping it is exactly
            // the vanishing-signature hole this arm exists to close.
            Err(_)
                if field
                    .field_type
                    .as_deref()
                    .is_some_and(|kind| kind != "Sig") => {}
            Err(_) => {
                // A signature field was present but could not be fully decoded.
                // Report it as broken rather than dropping it silently.
                // This prevents tampered signatures from appearing as unsigned.
                let field_name = field
                    .qualified_name
                    .clone()
                    .unwrap_or_else(|| format!("Field_{}", field.obj.0));
                reports.push(StructuralReport {
                    field_name,
                    coverage: SignatureCoverage::Unclear,
                    later_revisions: false,
                    contents_extent: ContentsExtent {
                        c_start: 0,
                        c_end: 0,
                    },
                    byte_range: ByteRange {
                        z: 0,
                        len1: 0,
                        start2: 0,
                        len2: 0,
                    },
                    sig_dict_revision: revisions.last_changed_revision(field.obj.0).unwrap_or(0),
                    declared_docmdp: None,
                    sub_filter: None,
                    mod_date: None,
                });
            }
        }
    }

    Ok(reports)
}

/// Parse the catalog reference from the trailer.
pub fn find_catalog_ref(bytes: &[u8]) -> Result<(u32, u16)> {
    find_catalog_ref_with(&ObjectResolver::new(bytes), bytes)
}

/// [`find_catalog_ref`], against a resolver the caller already built.
///
/// Building an `ObjectResolver` re-parses — and re-inflates — the whole
/// cross-reference chain. Every `_with` entry point in this module exists so
/// that one verification or pre-flight pass pays that once instead of once per
/// object it looks up; see [`object_definition`] for what the per-lookup shape
/// cost on a chain-heavy document.
pub fn find_catalog_ref_with(resolver: &ObjectResolver<'_>, bytes: &[u8]) -> Result<(u32, u16)> {
    // The resolver knows the merged trailer of the active revision, including
    // the cross-reference-stream case the tokenizer walk below cannot read.
    if let Some(root) = resolver.root_ref() {
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

/// [`find_field_tree`], against a resolver the caller already built.
pub fn find_field_tree_with(
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

    // `/T` is a text string, not UTF-8 bytes: reading it lossily turned a
    // UTF-16BE name into mojibake in the signature panel and in every name
    // comparison downstream.
    let own_name = match dict.get("T") {
        Some(PdfValue::Str(s)) => Some(crate::pdftext::decode_text_string(s)),
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

/// [`find_fields_array`], against a resolver the caller already built.
pub fn find_fields_array_with(
    resolver: &ObjectResolver<'_>,
    catalog_ref: (u32, u16),
) -> Result<Vec<(u32, u16)>> {
    Ok(find_field_tree_with(resolver, catalog_ref)?
        .into_iter()
        .map(|f| f.obj)
        .collect())
}

/// Find object bytes by object number, through the cross-reference chain.
///
/// The returned slice covers `N G obj … endobj` **in the file**, so the caller
/// also learns where the object is. That is what a signature dictionary needs
/// — `/ByteRange` describes file offsets, so the `/Contents` reservation has
/// to be addressable — and it is why this cannot be replaced wholesale by
/// [`object_definition`].
///
/// An object that lives in an object stream has no such slice and is reported
/// as not found. A caller that only reads an object's dictionary should use
/// [`object_definition`], which handles both.
pub fn find_object(bytes: &[u8], obj_num: u32) -> Result<&[u8]> {
    let resolver = ObjectResolver::new(bytes);
    let (start, end, _conf) = resolver
        .object_span(obj_num)
        .ok_or(VerifyError::SignatureObjectNotFound)?;
    bytes
        .get(start..end)
        .ok_or(VerifyError::SignatureObjectNotFound)
}

/// The definition of object `obj_num`, wherever the cross-reference chain says
/// it lives.
///
/// This is the one lookup every caller that only needs to *read* an object
/// should use. [`find_object`] can only ever return a slice of the file, so it
/// cannot see an object packed into a `/Type /ObjStm` — which is where a PDF
/// 1.5+ producer puts every dictionary in the document. This returns an owned
/// `N 0 obj … endobj` definition instead: for an in-file object it is the file
/// bytes verbatim, and for one inside an object stream it is the object's
/// lexical content re-wrapped in a header and trailer, so a caller that
/// tokenises the result sees the same thing either way.
///
/// The cost is a copy per lookup. It buys the property that classic tables,
/// cross-reference streams and object streams are indistinguishable above this
/// line, which is what makes the sign path shape-agnostic.
pub fn object_definition(bytes: &[u8], obj_num: u32) -> Result<Vec<u8>> {
    ObjectResolver::new(bytes)
        .object_bytes(obj_num)
        .map(|(definition, _confidence)| definition)
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
    resolver: &ObjectResolver<'_>,
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

    // The field dictionary may live in an object stream even in a signed
    // document — only its `/V` signature dictionary is pinned to a file
    // offset, because its `/Contents` has to be addressable by `/ByteRange`.
    let (field_obj_definition, _) = resolver.object_bytes(field_ref.0)?;
    let field_obj_slice = field_obj_definition.as_slice();
    if field_obj_slice.is_empty() {
        // Field object exists but is empty: this is a broken /Sig field that should
        // be reported, not dropped. Return error to trigger broken-report path.
        return Err(VerifyError::MalformedPdf(
            "signature field object is empty".to_string(),
        ));
    }

    let mut tokenizer = PdfTokenizer::new(field_obj_slice);

    let mut key: Option<String> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        // Keys are matched on bytes. A `/T` whose value is a UTF-16BE string
        // is not valid UTF-8, and a pass that skipped the whole token on that
        // ground also failed to consume the key — so the *next* key was eaten
        // as this one's value and `/V` went unseen, which reads a signed
        // field as unsigned.
        // Only treat '/' names as keys if we don't already have a key
        if key.is_none() && token.starts_with(b"/") {
            key = Some(String::from_utf8_lossy(&token[1..]).into_owned());
        } else if let Some(k) = key.take() {
            match k.as_str() {
                "FT" => {
                    if let Some(ft_name) = token.strip_prefix(b"/".as_slice()) {
                        field_type = Some(String::from_utf8_lossy(ft_name).into_owned());
                    }
                }
                // The qualified name from the field tree wins: it carries
                // the ancestors' /T, which this local pass cannot see.
                "T" if field_name.is_none() => {
                    if let Some(name_val) = parse_pdf_string(&token) {
                        field_name = Some(name_val);
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

    // Check if this is actually a signature field.
    // If /FT is not /Sig, this is a non-signature field: skip it (return Ok(None)).
    if field_type.as_deref() != Some("Sig") {
        return Ok(None);
    }

    // At this point, the field IS /Sig but may have no /V (never signed).
    // An empty signature field is legal PDF and should be skipped.
    if sig_dict_ref.is_none() {
        return Ok(None);
    }

    let field_name = field_name.unwrap_or_else(|| format!("Field_{}", field_ref.0));
    let sig_dict_ref = sig_dict_ref.unwrap();

    // Extract the signature dictionary, as a span in the file: `/ByteRange`
    // describes file offsets, so the `/Contents` extent has to be one too.
    //
    // The confidence is load-bearing and used to be discarded. A dictionary
    // the cross-reference chain could not account for, found only because the
    // repair scan swept the file for `N G obj`, is not a signature that can be
    // verified: the scan's answer is a guess, and a guess about which bytes
    // are the signature decides `/ByteRange`, the `/Contents` extent, the
    // `/SubFilter` and the DocMDP level. It is refused, which surfaces the
    // field as broken rather than presenting a guess as a verified signature.
    let (start, end, confidence) = resolver
        .object_span(sig_dict_ref.0)
        .ok_or(VerifyError::SignatureObjectNotFound)?;
    if confidence != Confidence::Resolved {
        return Err(VerifyError::MalformedPdf(format!(
            "the signature dictionary (object {}) is not accounted for by the document's \
             cross-reference data and was located only by a repair scan; a signature whose \
             own dictionary cannot be resolved is not verifiable",
            sig_dict_ref.0
        )));
    }
    let sig_dict_slice = bytes
        .get(start..end)
        .ok_or(VerifyError::SignatureObjectNotFound)?;
    let (sub_filter, mod_date) = extract_subfilter_and_mod_date(sig_dict_slice);
    if sig_dict_slice.is_empty() {
        // Signature dictionary exists but is empty: broken signature.
        return Err(VerifyError::MalformedPdf(
            "signature dictionary object is empty".to_string(),
        ));
    }

    // Find the absolute position of sig_dict_slice in bytes
    let sig_dict_offset = start as u64;
    // If extraction fails, propagate the error instead of silently skipping.
    // This ensures tampered signatures are reported as broken, not dropped.
    let (br, ce, mdp) = extract_sig_dict_info(sig_dict_slice, sig_dict_offset)?;

    let sig_dict_rev = revisions.last_changed_revision(sig_dict_ref.0).unwrap_or(0);

    let (coverage, later_revisions) =
        classify_coverage(&br, &ce, bytes.len() as u64, sig_dict_rev, revisions)?;

    Ok(Some(StructuralReport {
        field_name,
        coverage,
        later_revisions,
        contents_extent: ce,
        byte_range: br,
        sig_dict_revision: sig_dict_rev,
        declared_docmdp: mdp,
        sub_filter,
        mod_date,
    }))
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
        match pending.take() {
            Some(key) if key == "SubFilter" => {
                if let Some(name) = token.strip_prefix(b"/".as_slice()) {
                    sub_filter = Some(String::from_utf8_lossy(name).into_owned());
                    continue;
                }
            }
            Some(key) if key == "M" => {
                // `/M` is a text string like any other, so it goes through the
                // same decode rather than assuming the bytes are UTF-8.
                if let Some(date) = parse_pdf_string(&token) {
                    mod_date = Some(date);
                    continue;
                }
            }
            _ => {}
        }
        if let Some(name) = token.strip_prefix(b"/".as_slice()) {
            pending = Some(String::from_utf8_lossy(name).into_owned());
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

    // The DocMDP level is a property of the dictionary, not of any token in
    // it, so it is read once here rather than from inside the loop below.
    let docmdp = extract_docmdp_level(sig_dict_slice).unwrap_or(None);
    let mut byte_range_values: Vec<u64> = Vec::new();
    let mut contents_extent: Option<ContentsExtent> = None;
    let mut declared_docmdp: Option<MdpPerm> = None;

    let mut key: Option<String> = None;
    let mut in_byte_range = false;
    let mut in_reference = false;

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
                            // Record the exact byte extent using the tokenizer's position.
                            // The tokenizer's position() is the byte offset after reading the token,
                            // so the token starts at (position - token.len()).
                            let token_end_pos = tokenizer.position();
                            if token_end_pos >= token.len() {
                                let contents_start_rel = token_end_pos - token.len();
                                // Convert to absolute file position
                                let contents_start = sig_dict_offset + contents_start_rel as u64;
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
                } else {
                    // Computed once, above: its argument is the whole slice,
                    // so it cannot depend on the token being looked at. It used
                    // to be called here, which re-tokenized the entire
                    // dictionary for every token inside `/Reference` to arrive
                    // at the same answer each time.
                    declared_docmdp = docmdp;
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

/// Parse a PDF string token — `(…)` or `<…>` — into UTF-8.
///
/// The argument is bytes, not `&str`, and that is the whole point: a text
/// string is UTF-16BE or PDFDocEncoding (§7.9.2.2), so a caller that tried to
/// view the token as UTF-8 first would drop every non-ASCII name on the
/// floor. Escapes, the byte-order mark and the encoding tables all live in
/// [`crate::pdftext`], so that pre-flight, verification and the signing path
/// cannot disagree about what a field is called.
pub fn parse_pdf_string(token: &[u8]) -> Option<String> {
    crate::pdftext::decode_text_string_token(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Classify one `/ByteRange` against the reservation every case here
    /// shares -- `/Contents` at 100..200, in a file with no earlier revision.
    ///
    /// Each test varies the byte range and where the file ends; repeating the
    /// fixed half five times buried what was actually under test.
    fn coverage_of(byte_range: &ByteRange, eof: u64) -> (SignatureCoverage, bool) {
        let extent = ContentsExtent {
            c_start: 100,
            c_end: 200,
        };
        let revisions = RevisionMap {
            revisions: BTreeMap::new(),
        };
        classify_coverage(byte_range, &extent, eof, 0, &revisions).unwrap()
    }

    #[test]
    fn test_coverage_z_nonzero() {
        let br = ByteRange {
            z: 1,
            len1: 100,
            start2: 200,
            len2: 50,
        };

        let (cov, _) = coverage_of(&br, 250);
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

        let (cov, _) = coverage_of(&br, 250);
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

        let (cov, _) = coverage_of(&br, 260);
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

        let (cov, later) = coverage_of(&br, 250);
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

        let (cov, _) = coverage_of(&br, 250);
        assert_eq!(cov, SignatureCoverage::ContiguousBlockFromStart);
    }

    // Problem #3 Tests

    #[test]
    fn test_byte_range_wrong_length() {
        // Coverage classification assumes four elements, so extraction is
        // where a range that does not have four has to stop. A short range
        // that was let through would be read against the wrong fields and
        // report coverage the signature never had, which is the one kind of
        // wrong answer verification must not give.
        for values in ["[0 100 200]", "[0 100]", "[0 100 200 50 400 20]", "[]"] {
            let dictionary = format!(
                "<</Type /Sig /ByteRange {values} \
                 /Contents <00000000000000000000000000000000>>>"
            );
            let result = extract_sig_dict_info(dictionary.as_bytes(), 0);
            assert!(
                matches!(result, Err(VerifyError::MalformedPdf(_))),
                "/ByteRange {values} was not refused: {result:?}"
            );
        }

        // The same dictionary with four elements is accepted, so the refusals
        // above are about the length and not about the fixture.
        let good = "<</Type /Sig /ByteRange [0 100 200 50] \
                    /Contents <00000000000000000000000000000000>>>";
        let (br, _, _) = extract_sig_dict_info(good.as_bytes(), 0)
            .expect("a four-element /ByteRange should be accepted");
        assert_eq!((br.z, br.len1, br.start2, br.len2), (0, 100, 200, 50));
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
        // `startxref` names the real offset of the `xref` keyword (43): the
        // old token-search parser tolerated a bogus offset here by scanning
        // forward for `<<`/`>>`/`/XRefStm` through whatever bytes it landed
        // on, which is exactly the leniency §78 removes. The real lexer
        // requires the offset to name a genuine cross-reference section.
        let pdf = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\nxref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n<<\n/Size 2\n/Root 1 0 R\n/XRefStm 3\n>>\nstartxref\n43\n%%EOF";

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
                obj_numbers: obj_nums,
                chain_position: 0,
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
                obj_numbers: obj_nums_1,
                chain_position: 1,
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
                obj_numbers: obj_nums_2,
                chain_position: 0,
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
                obj_numbers: obj_nums_1,
                chain_position: 1,
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
                obj_numbers: obj_nums_2,
                chain_position: 0,
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
        let v1_xref = find_startxref(&pdf_v1).expect("the signed fixture has an xref");

        // Append a valid second revision containing a dummy object.
        let mut pdf_v2 = pdf_v1.clone();
        pdf_v2.push(b'\n');
        let obj2_offset = pdf_v2.len();
        pdf_v2.extend_from_slice(b"2 0 obj\n<</Type /Dummy>>\nendobj\n");
        let v2_xref_offset = pdf_v2.len();
        pdf_v2.extend_from_slice(b"xref\n");
        pdf_v2.extend_from_slice(format!("2 1\n{obj2_offset:010} 00000 n \ntrailer\n").as_bytes());
        pdf_v2.extend_from_slice(
            format!("<<\n/Size 8\n/Root 1 0 R\n/Prev {v1_xref}\n>>\n").as_bytes(),
        );
        pdf_v2.extend_from_slice(b"startxref\n");
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
    fn revision_map_decodes_xref_stream_rows() {
        let pdf = build_objstm_pdf(true);
        let map = RevisionMap::build(&pdf).expect("xref stream should build a revision map");
        let revision = map
            .all_revisions()
            .into_iter()
            .next()
            .expect("the document has one revision");

        assert!(revision.obj_numbers.contains(&6));
        assert!(revision.obj_numbers.contains(&7));
        assert!(revision.obj_numbers.contains(&8));
        assert!(!revision.obj_numbers.contains(&0));
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

    // Regression tests for security findings

    #[test]
    fn huge_xref_count_does_not_hang() {
        // Malicious PDF with subsection header "0 4294967290" (huge count).
        // This should not hang or exhaust memory, but return quickly.
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog>>\nendobj\n");

        // Malicious xref with huge count: "0 4294967290"
        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n");
        pdf.extend_from_slice(b"0 4294967290\n"); // Huge subsection header
                                                  // Don't bother adding 4B entries; the parser should handle this without hanging

        // Minimal trailer
        pdf.extend_from_slice(b"trailer\n<</Size 1 /Root 1 0 R>>\n");
        pdf.extend_from_slice(b"startxref\n");
        pdf.extend_from_slice(format!("{}\n", xref_offset).as_bytes());
        pdf.extend_from_slice(b"%%EOF");

        // This should return quickly, not hang or panic
        let start = std::time::Instant::now();
        let result = RevisionMap::build(&pdf);
        let elapsed = start.elapsed();

        // Should complete in well under 1 second (malicious file hangs for 90+ seconds)
        assert!(
            elapsed.as_secs() < 1,
            "parsing should return quickly, but took {:?}",
            elapsed
        );

        // May succeed or error, but should not hang
        let _ = result;
    }

    /// The classic table and the cross-reference stream are two parsers for
    /// one job, and bounding only the first left this one — reached by every
    /// PDF 1.5+ — able to spin for billions of iterations on an /Index the
    /// file simply asserts. A ~180 byte document hung for minutes.
    #[test]
    fn huge_xref_stream_index_count_does_not_hang() {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_at = pdf.len();
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /XRef /Index [0 4000000000] /W [1 4 2] \
              /Root 1 0 R /Size 1 >>\nstream\n\nendstream\nendobj\n",
        );
        pdf.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());

        let started = std::time::Instant::now();
        let _ = RevisionMap::build(&pdf);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "an /Index count the file cannot back with bytes must be refused, \
             not walked: took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn trailer_with_nested_dict_before_prev_is_parsed_correctly() {
        // A PDF where the trailer has a nested dictionary before the /Prev entry.
        // This tests that nesting depth is tracked correctly and doesn't truncate
        // the revision chain on the nested dict's closing >>.
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog>>\nendobj\n");

        // First revision
        let xref1_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n");
        pdf.extend_from_slice(b"trailer\n");
        pdf.extend_from_slice(b"<< /Size 2 /Root 1 0 R ");
        // Nested dictionary before /Prev
        pdf.extend_from_slice(b"/Info << /Producer (Test) >> ");
        pdf.extend_from_slice(b">>\n");
        pdf.extend_from_slice(b"startxref\n");
        pdf.extend_from_slice(format!("{}\n", xref1_offset).as_bytes());
        pdf.extend_from_slice(b"%%EOF");

        // This should parse without truncating at the nested dict's closing >>
        let result = RevisionMap::build(&pdf);
        assert!(
            result.is_ok(),
            "parsing PDF with nested trailer dict should succeed"
        );

        let revisions = result.unwrap();
        assert_eq!(revisions.all_revisions().len(), 1);
    }

    /// A `/Prev` inside a nested dictionary is not the trailer's `/Prev`.
    ///
    /// The test above pins that nesting does not *truncate* the scan. This
    /// pins the other half: the scan must not pick a key up out of the nested
    /// dictionary either. `ObjectResolver` parses the trailer into a real
    /// dictionary and can only ever see the top-level key, so a `/Prev` reader
    /// that reads the nested one walks a different revision chain than the
    /// rest of verification does — and it fails in the dangerous direction,
    /// because a revision missing from the map cannot be one the classifier
    /// finds extending past a signature.
    #[test]
    fn a_prev_inside_a_nested_dictionary_is_not_the_trailers_prev() {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog>>\nendobj\n");

        // A first revision, so that a real `/Prev` has somewhere to point.
        let first = pdf.len();
        pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n");
        pdf.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R >>\n");
        pdf.extend_from_slice(b"startxref\n");
        pdf.extend_from_slice(format!("{first}\n").as_bytes());
        pdf.extend_from_slice(b"%%EOF\n");

        // A second revision whose trailer carries a decoy `/Prev` in a nested
        // dictionary, before the real one. 999999 is past the end of the file:
        // if it were followed, the walk could not resolve it.
        let second = pdf.len();
        pdf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        pdf.extend_from_slice(b"trailer\n");
        pdf.extend_from_slice(b"<< /Size 2 /Root 1 0 R /Info << /Prev 999999 >> ");
        pdf.extend_from_slice(format!("/Prev {first} >>\n").as_bytes());
        pdf.extend_from_slice(b"startxref\n");
        pdf.extend_from_slice(format!("{second}\n").as_bytes());
        pdf.extend_from_slice(b"%%EOF");

        let mut budget = objects::DecodeBudget::new();
        let section = objects::xref_section_object_numbers(&pdf, second as u64, &mut budget)
            .expect("the trailer parses");
        assert_eq!(
            section.prev,
            Some(first as u64),
            "the trailer's own /Prev is the one at depth 1, not the one in /Info"
        );

        // And the chain the classifier is fed has both revisions in it, rather
        // than losing the earlier one to an offset that resolves to nothing.
        let revisions = RevisionMap::build(&pdf).expect("the chain walks");
        assert_eq!(
            revisions.all_revisions().len(),
            2,
            "following the decoy would drop a revision from the map"
        );
    }

    // ---------------------------------------------------------------
    // `objects::xref_section_object_numbers` against real bytes.
    //
    // Every `classify_coverage` test above hand-builds a `RevisionInfo`, so
    // nothing pinned the offsets the classifier is actually fed. These do.
    // ---------------------------------------------------------------

    /// A `/W [1 4 2]` cross-reference stream body from `(type, second, third)`.
    fn xref_rows(entries: &[(u8, u32, u16)]) -> Vec<u8> {
        let mut out = Vec::with_capacity(entries.len() * 7);
        for (kind, second, third) in entries {
            out.push(*kind);
            out.extend_from_slice(&second.to_be_bytes());
            out.extend_from_slice(&third.to_be_bytes());
        }
        out
    }

    /// A classic table's container ends past its `trailer` dictionary, at an
    /// **absolute** file offset. It used to be reported relative to
    /// `startxref`, which under-reports for every revision but the first and
    /// lets a signature that stops short of its own cross-reference data pass
    /// as if it covered it.
    #[test]
    fn a_classic_tables_container_end_is_an_absolute_file_offset() {
        let pdf = build_classic_pdf(&[obj(1, 0, b"<</Type /Catalog>>")], "");
        let startxref = find_startxref(&pdf).expect("the fixture has a startxref");
        let mut budget = objects::DecodeBudget::new();
        let section = objects::xref_section_object_numbers(&pdf, startxref, &mut budget)
            .expect("the classic table parses");
        let (xref_end, hybrid) = (section.end, section.xref_stm.is_some());

        assert!(!hybrid);

        // The trailer's closing `>>` is the last one before `startxref`.
        let startxref_at = pdf
            .windows(9)
            .rposition(|w| w == b"startxref")
            .expect("the fixture has a startxref keyword");
        let close_at = pdf[..startxref_at]
            .windows(2)
            .rposition(|w| w == b">>")
            .expect("the trailer closes");
        assert_eq!(
            xref_end,
            (close_at + 2) as u64,
            "the container end must be the absolute offset past the trailer's `>>`, \
             not an offset relative to startxref"
        );
        assert!(xref_end > startxref);
    }

    /// Coverage classification on a real classic file, both ways.
    ///
    /// This is a behaviour change worth pinning: `xref_end` for a classic
    /// table is now an absolute file offset, so classification against it is
    /// genuinely stricter than it was. A signature that stops before the
    /// table's trailer no longer passes as covering its own revision.
    #[test]
    fn a_classic_files_coverage_is_classified_against_its_real_container_end() {
        let pdf = build_classic_pdf(&[obj(1, 0, b"<</Type /Catalog>>")], "");
        let revisions = RevisionMap::build(&pdf).expect("the revision map builds");
        let startxref = find_startxref(&pdf).expect("the fixture has a startxref");
        let info = revisions
            .get_by_startxref(startxref)
            .expect("the revision is recorded");
        assert!(info.xref_end > startxref, "an absolute container end");
        assert!(
            info.xref_end < pdf.len() as u64,
            "and it is inside the file"
        );

        let ce = ContentsExtent {
            c_start: 10,
            c_end: 20,
        };

        // Stops at the `xref` keyword: it does not cover the table at all.
        let short = ByteRange {
            z: 0,
            len1: 10,
            start2: 20,
            len2: startxref - 20,
        };
        let (coverage, _) =
            classify_coverage(&short, &ce, pdf.len() as u64, startxref, &revisions).unwrap();
        assert_eq!(coverage, SignatureCoverage::ContiguousBlockFromStart);

        // Reaches past the trailer's `>>`: it covers the whole revision.
        let full = ByteRange {
            z: 0,
            len1: 10,
            start2: 20,
            len2: info.xref_end - 20,
        };
        let (coverage, later) =
            classify_coverage(&full, &ce, pdf.len() as u64, startxref, &revisions).unwrap();
        assert_eq!(coverage, SignatureCoverage::EntireRevision);
        assert!(later, "bytes follow the covered region");

        // And all the way to the end is the whole file.
        let whole = ByteRange {
            z: 0,
            len1: 10,
            start2: 20,
            len2: pdf.len() as u64 - 20,
        };
        let (coverage, later) =
            classify_coverage(&whole, &ce, pdf.len() as u64, startxref, &revisions).unwrap();
        assert_eq!(coverage, SignatureCoverage::EntireFile);
        assert!(!later);
    }

    /// A cross-reference stream's container is the whole indirect object, so
    /// it ends past `endobj` — and the `endobj` that ends it is the lexer's,
    /// not the first byte sequence spelling `endobj` after the dictionary.
    /// Searching for that literal ran *through* the stream body, which is
    /// attacker-controlled: an `endobj` planted in it moved the container end
    /// backwards, and a signature that stops short of its own cross-reference
    /// stream then classified as EntireRevision.
    #[test]
    fn an_endobj_planted_in_a_stream_body_does_not_shorten_the_container() {
        let mut pdf = Vec::from(&b"%PDF-1.5\n"[..]);
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog>>\nendobj\n");
        let xref_at = pdf.len();

        // Four rows; the third is six bytes of `endobj` and a pad byte, so the
        // literal appears inside the compressed-looking body.
        let mut body = xref_rows(&[(0, 0, 0xFFFF), (1, 9, 0)]);
        body.extend_from_slice(b"endobj\x00");
        body.extend_from_slice(&xref_rows(&[(1, xref_at as u32, 0)]));
        assert_eq!(body.len(), 28);

        pdf.extend_from_slice(
            format!(
                "2 0 obj\n<</Type /XRef /Size 4 /W [1 4 2] /Root 1 0 R /Length {}>>\nstream\n",
                body.len()
            )
            .as_bytes(),
        );
        let body_at = pdf.len();
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF").as_bytes());

        let planted = body_at
            + body
                .windows(6)
                .position(|w| w == b"endobj")
                .expect("the decoy is in the body");
        let real = pdf
            .windows(6)
            .rposition(|w| w == b"endobj")
            .expect("the object closes");
        assert!(planted < real, "the decoy must precede the real `endobj`");

        let mut budget = objects::DecodeBudget::new();
        let xref_end = objects::xref_section_object_numbers(&pdf, xref_at as u64, &mut budget)
            .expect("the cross-reference stream parses")
            .end;
        assert_eq!(
            xref_end,
            (real + b"endobj".len()) as u64,
            "the container must end past the object's own `endobj`, not the planted one"
        );

        // And the classification that rests on it: a signature that stops at
        // the decoy does not cover its own cross-reference stream.
        let revisions = RevisionMap::build(&pdf).expect("the revision map builds");
        let short = (planted + b"endobj".len()) as u64;
        let br = ByteRange {
            z: 0,
            len1: 10,
            start2: 20,
            len2: short - 20,
        };
        let ce = ContentsExtent {
            c_start: 10,
            c_end: 20,
        };
        let (coverage, _) =
            classify_coverage(&br, &ce, pdf.len() as u64, xref_at as u64, &revisions).unwrap();
        assert_eq!(
            coverage,
            SignatureCoverage::ContiguousBlockFromStart,
            "a signature that stops inside its own cross-reference stream must not \
             classify as covering the whole revision"
        );

        // The same document, signed all the way past the container, does.
        let br = ByteRange {
            z: 0,
            len1: 10,
            start2: 20,
            len2: xref_end - 20,
        };
        let (coverage, later) =
            classify_coverage(&br, &ce, pdf.len() as u64, xref_at as u64, &revisions).unwrap();
        assert_eq!(coverage, SignatureCoverage::EntireRevision);
        assert!(later);
    }

    /// A nested dictionary in a classic trailer used to end the scan at its
    /// own `>>`, which truncated the container end **and** hid every key that
    /// followed. `/XRefStm` among them: the hybrid refusal was bypassed and
    /// the file read as plain classic, while `ObjectResolver` went on
    /// following the `/XRefStm` it could still see.
    #[test]
    fn a_nested_dictionary_in_a_classic_trailer_does_not_hide_xrefstm() {
        let head = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\n";
        let xref_at = head.len();
        let mut pdf = head.to_vec();
        pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n");
        pdf.extend_from_slice(b"<</Size 2 /Root 1 0 R /Foo <</A 1>> /XRefStm 9>>\n");
        pdf.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF").as_bytes());

        assert!(
            matches!(
                RevisionMap::build(&pdf),
                Err(VerifyError::HybridXrefNotSupported)
            ),
            "a /XRefStm behind a nested dictionary is still a hybrid file"
        );

        // Without the /XRefStm the same shape parses, and the container end
        // reaches past the *trailer's* `>>` rather than the nested one's.
        let plain = String::from_utf8(pdf.clone())
            .unwrap()
            .replace(" /XRefStm 9", "");
        let plain = plain.as_bytes();
        let mut budget = objects::DecodeBudget::new();
        let section = objects::xref_section_object_numbers(plain, xref_at as u64, &mut budget)
            .expect("the classic table parses");
        let (xref_end, hybrid) = (section.end, section.xref_stm.is_some());
        assert!(!hybrid);
        let nested_close = plain
            .windows(2)
            .position(|w| w == b">>")
            .expect("the nested dictionary closes");
        assert!(
            xref_end > (nested_close + 2) as u64,
            "the scan must not stop at the nested dictionary's `>>`"
        );
        assert!(RevisionMap::build(plain).is_ok());
    }

    /// Revision order is the `/Prev` chain, not the `startxref` offset.
    ///
    /// A `/Prev` that points *forward* — which a linearized document does
    /// legitimately and a crafted one does on purpose — made an older section
    /// sort as if it were newer, so its container end dropped out of the
    /// coverage loop and was never compared against the signature's coverage
    /// end.
    #[test]
    fn a_forward_prev_does_not_reorder_the_revisions() {
        let mut pdf = Vec::from(&b"%PDF-1.5\n"[..]);
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog>>\nendobj\n");

        // The newest section, at the *lower* offset, whose /Prev points
        // forward at the older one. The offset is patched in once known.
        let newest_at = pdf.len();
        let body_new = xref_rows(&[(0, 0, 0xFFFF), (1, 9, 0)]);
        pdf.extend_from_slice(
            format!(
                "2 0 obj\n<</Type /XRef /Size 4 /W [1 4 2] /Root 1 0 R /Prev 0000000000 \
                 /Length {}>>\nstream\n",
                body_new.len()
            )
            .as_bytes(),
        );
        let prev_field = pdf
            .windows(10)
            .rposition(|w| w == b"0000000000")
            .expect("the placeholder is there");
        pdf.extend_from_slice(&body_new);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let oldest_at = pdf.len();
        let body_old = xref_rows(&[(0, 0, 0xFFFF), (1, 9, 0), (1, oldest_at as u32, 0)]);
        pdf.extend_from_slice(
            format!(
                "3 0 obj\n<</Type /XRef /Size 4 /W [1 4 2] /Root 1 0 R /Length {}>>\nstream\n",
                body_old.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&body_old);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(format!("startxref\n{newest_at}\n%%EOF").as_bytes());
        pdf[prev_field..prev_field + 10].copy_from_slice(format!("{oldest_at:010}").as_bytes());

        let revisions = RevisionMap::build(&pdf).expect("the chain is walkable");
        let ordered = revisions.all_revisions();
        assert_eq!(ordered.len(), 2);
        assert_eq!(
            ordered[0].startxref, oldest_at as u64,
            "oldest first is chain order, not offset order"
        );
        assert_eq!(ordered[0].chain_position, 1);
        assert_eq!(ordered[1].startxref, newest_at as u64);
        assert_eq!(ordered[1].chain_position, 0);

        // The older section sits at the higher offset, so ordering on offsets
        // filtered it out of the coverage loop entirely.
        let older_end = ordered[0].xref_end;
        let newer_end = ordered[1].xref_end;
        assert!(older_end > newer_end);

        let br = ByteRange {
            z: 0,
            len1: 10,
            start2: 20,
            len2: newer_end - 20,
        };
        let ce = ContentsExtent {
            c_start: 10,
            c_end: 20,
        };
        let (coverage, _) =
            classify_coverage(&br, &ce, pdf.len() as u64, newest_at as u64, &revisions).unwrap();
        assert_eq!(
            coverage,
            SignatureCoverage::ContiguousBlockFromStart,
            "a section the signature does not cover must be compared against it \
             whatever offset it was written at"
        );
    }

    /// A signature dictionary the cross-reference chain places inside an
    /// object stream has no file span, and `/ByteRange` needs one. Falling
    /// through to the repair scan handed back a stale in-file copy that no
    /// conforming reader ever sees — a shadow signature dictionary, read for
    /// `/ByteRange`, `/Contents`, `/SubFilter` and the DocMDP level.
    #[test]
    fn a_signature_dictionary_the_xref_puts_in_an_object_stream_is_not_found() {
        let decoy = b"5 0 obj\n<</Type /Sig /ByteRange [0 9999 9999 1] /Contents <00> \
                      /DECOY true>>\nendobj\n";
        let inner = "5 0 <</Type /Sig /AUTHORITATIVE true>>";
        let first = 4; // "5 0 " before the value

        let mut pdf = Vec::from(&b"%PDF-1.5\n"[..]);
        let decoy_at = pdf.len();
        pdf.extend_from_slice(decoy);
        let objstm_at = pdf.len();
        pdf.extend_from_slice(
            format!(
                "4 0 obj\n<</Type /ObjStm /N 1 /First {first} /Length {}>>\nstream\n{inner}\n\
                 endstream\nendobj\n",
                inner.len()
            )
            .as_bytes(),
        );
        let xref_at = pdf.len();
        let body = xref_rows(&[
            (0, 0, 0xFFFF),
            (1, 9, 0),
            (1, 9, 0),
            (1, 9, 0),
            (1, objstm_at as u32, 0),
            (2, 4, 0),
            (1, xref_at as u32, 0),
        ]);
        pdf.extend_from_slice(
            format!(
                "6 0 obj\n<</Type /XRef /Size 7 /W [1 4 2] /Root 1 0 R /Length {}>>\nstream\n",
                body.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF").as_bytes());

        // The decoy is really there, and a repair scan really does find it.
        assert!(objects::scan_object_definitions(&pdf).contains_key(&5));
        assert_eq!(objects::scan_object_definitions(&pdf)[&5].0, decoy_at);

        let resolver = ObjectResolver::new(&pdf);
        assert!(matches!(
            resolver.xref().and_then(|i| i.get(5)),
            Some(XrefEntry::InStream { .. })
        ));
        assert!(
            resolver.object_span(5).is_none(),
            "an object the xref places in an object stream has no file span"
        );
        assert!(matches!(
            find_object(&pdf, 5),
            Err(VerifyError::SignatureObjectNotFound)
        ));
        assert!(matches!(
            find_object_offset(&pdf, 5),
            Err(VerifyError::SignatureObjectNotFound)
        ));

        // `object_bytes` was already xref-authoritative, and stays so: it
        // returns the object-stream copy, never the shadowed in-file one.
        let (definition, _) = resolver.object_bytes(5).expect("the objstm copy resolves");
        let text = String::from_utf8_lossy(&definition);
        assert!(text.contains("AUTHORITATIVE"), "got {text}");
        assert!(!text.contains("DECOY"), "got {text}");
    }

    /// A signature dictionary the cross-reference data does not account for,
    /// found only by the repair scan, is a guess — and a guess about which
    /// bytes are the signature decides `/ByteRange`, the `/Contents` extent
    /// and the DocMDP level. The confidence used to be discarded.
    #[test]
    fn a_signature_dictionary_found_only_by_a_repair_scan_is_refused() {
        let mut pdf = build_classic_pdf(
            &[
                obj(1, 0, b"<</Type /Catalog /AcroForm 2 0 R>>"),
                obj(2, 0, b"<</Fields [3 0 R] /SigFlags 3>>"),
                obj(3, 0, b"<</FT /Sig /T (Sig1) /V 4 0 R>>"),
                obj(
                    4,
                    0,
                    b"<</Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 10 20 30] \
                      /Contents <0102030405060708>>>",
                ),
            ],
            "",
        );

        // With an intact table the signature is resolved and reported.
        let revisions = RevisionMap::build(&pdf).expect("the revision map builds");
        let resolver = ObjectResolver::new(&pdf);
        assert_eq!(
            resolver.object_span(4).expect("object 4 resolves").2,
            Confidence::Resolved
        );

        // Break only object 4's xref entry, so the chain can no longer account
        // for the signature dictionary and the scan is the only way to it.
        let entry = format!("4 1\n{:010} {:05} n \n", 0, 0);
        let old = pdf
            .windows(4)
            .position(|w| w == b"4 1\n")
            .expect("the subsection header for object 4 is there");
        let end = old + entry.len();
        pdf[old..end].copy_from_slice(entry.as_bytes());

        let resolver = ObjectResolver::new(&pdf);
        assert_eq!(
            resolver.object_span(4).expect("the scan still finds it").2,
            Confidence::Scanned,
            "the fixture must exercise the scan path"
        );

        let reports = discover_signatures(&pdf, &revisions).expect("discovery runs");
        assert_eq!(reports.len(), 1, "the field must not vanish");
        assert_eq!(
            reports[0].coverage,
            SignatureCoverage::Unclear,
            "a signature whose own dictionary was only guessed at must be reported \
             as broken, not silently trusted"
        );
    }

    /// The revision cap and the resolver's chain cap used to disagree — 1024
    /// against 256 — so a document with a chain between the two got a revision
    /// map while `ObjectResolver` silently degraded to a repair scan, and the
    /// two halves of verification described different documents.
    /// One resolver per pass, not one per lookup.
    ///
    /// Every object lookup used to build its own `ObjectResolver`, which
    /// re-parses the whole cross-reference chain — decoding every
    /// cross-reference stream in it — and throws away the object-stream cache
    /// on the way out. With up to MAX_FIELD_NODES fields to read, a document
    /// with a long chain multiplied that into hours of work in the process
    /// that owns the user interface. The chain here is deliberately expensive
    /// to walk; the pass must pay for it a fixed number of times.
    #[test]
    fn a_long_cross_reference_chain_is_walked_once_per_pass() {
        const SECTIONS: usize = 24;
        const ROWS: usize = 20_000;
        const FIELDS: u32 = 64;

        // Objects: 1 catalog, 2 acroform, then FIELDS terminal fields.
        let mut pdf = Vec::from(&b"%PDF-1.5\n"[..]);
        let mut offsets: Vec<(u32, usize)> = Vec::new();

        offsets.push((1, pdf.len()));
        let field_refs: Vec<String> = (0..FIELDS).map(|i| format!("{} 0 R", 3 + i)).collect();
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog /AcroForm 2 0 R>>\nendobj\n");
        offsets.push((2, pdf.len()));
        pdf.extend_from_slice(
            format!(
                "2 0 obj\n<</Fields [{}] /SigFlags 3>>\nendobj\n",
                field_refs.join(" ")
            )
            .as_bytes(),
        );
        for i in 0..FIELDS {
            let num = 3 + i;
            offsets.push((num, pdf.len()));
            pdf.extend_from_slice(
                format!("{num} 0 obj\n<</FT /Tx /T (Field{i})>>\nendobj\n").as_bytes(),
            );
        }

        // A chain of uncompressed cross-reference streams. Each one carries
        // the real entries plus a long tail of filler rows, so walking the
        // chain is measurably expensive without any decompression at all.
        let mut rows: Vec<u8> = xref_rows(&[(0, 0, 0xFFFF)]);
        for (num, at) in &offsets {
            let _ = num;
            rows.extend_from_slice(&xref_rows(&[(1, *at as u32, 0)]));
        }
        let real_rows = rows.len() / 7;
        rows.extend_from_slice(&xref_rows(&vec![(1, 9, 0); ROWS]));

        let mut section_offsets: Vec<usize> = Vec::new();
        for i in 0..SECTIONS {
            let at = pdf.len();
            section_offsets.push(at);
            let prev = match i {
                0 => String::new(),
                _ => format!("/Prev {} ", section_offsets[i - 1]),
            };
            let index = format!("/Index [0 {real_rows} 100000 {ROWS}]");
            pdf.extend_from_slice(
                format!(
                    "{} 0 obj\n<</Type /XRef /Size 200000 /W [1 4 2] {index} /Root 1 0 R \
                     {prev}/Length {}>>\nstream\n",
                    1000 + i,
                    rows.len()
                )
                .as_bytes(),
            );
            pdf.extend_from_slice(&rows);
            pdf.extend_from_slice(b"\nendstream\nendobj\n");
        }
        pdf.extend_from_slice(
            format!("startxref\n{}\n%%EOF", section_offsets[SECTIONS - 1]).as_bytes(),
        );

        let revisions = RevisionMap::build(&pdf).expect("the revision map builds");
        assert_eq!(revisions.all_revisions().len(), SECTIONS);

        let started = std::time::Instant::now();
        let reports = discover_signatures(&pdf, &revisions).expect("discovery runs");
        let elapsed = started.elapsed();
        assert!(reports.is_empty(), "the fixture has no signatures");

        // Measured on this fixture in a debug build: one fresh resolver costs
        // ~130 ms, so the per-lookup shape takes >8 s for the field objects
        // alone, while the whole threaded pass takes ~0.15 s. The bound sits
        // between them with an order of magnitude of headroom either way, so
        // it discriminates without pinning a machine's speed.
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "discovery over {FIELDS} fields on a {SECTIONS}-section chain took {elapsed:?}; \
             the cross-reference chain is being rebuilt per lookup"
        );
    }

    #[test]
    fn the_revision_cap_and_the_resolver_chain_cap_agree() {
        // A chain one longer than the shared cap, of classic sections.
        let sections = objects::MAX_XREF_CHAIN + 1;
        let mut pdf = Vec::from(&b"%PDF-1.4\n"[..]);
        pdf.extend_from_slice(b"1 0 obj\n<</Type /Catalog>>\nendobj\n");
        let mut offsets: Vec<usize> = Vec::new();
        for i in 0..sections {
            let at = pdf.len();
            offsets.push(at);
            pdf.extend_from_slice(
                b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n",
            );
            let prev = match i {
                0 => String::new(),
                _ => format!("/Prev {} ", offsets[i - 1]),
            };
            pdf.extend_from_slice(format!("<</Size 2 /Root 1 0 R {prev}>>\n").as_bytes());
        }
        pdf.extend_from_slice(format!("startxref\n{}\n%%EOF", offsets[sections - 1]).as_bytes());

        let revision_map = RevisionMap::build(&pdf);
        let resolver_index = XrefIndex::build(&pdf);
        assert!(
            revision_map.is_err() && resolver_index.is_err(),
            "one cap must not admit a chain the other refuses: revision map {:?}, \
             resolver index {:?}",
            revision_map.map(|m| m.all_revisions().len()),
            resolver_index.map(|i| i.entries().len())
        );
    }

    #[test]
    fn broken_signature_field_is_reported_not_dropped() {
        // A PDF with a signature field that has a malformed /ByteRange
        // (wrong number of elements). This should be reported as broken,
        // not silently dropped.
        let pdf = build_classic_pdf(
            &[
                obj(1, 0, b"<</Type /Catalog /AcroForm 2 0 R>>"),
                obj(2, 0, b"<</Fields [3 0 R]>>"),
                obj(3, 0, b"<</FT /Sig /T (Sig1) /V 4 0 R>>"),
                // Malformed signature dict with /ByteRange having only 3 elements
                obj(
                    4,
                    0,
                    b"<</Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 100 200] /Contents <0102>>>",
                ),
            ],
            "",
        );

        let revisions = RevisionMap::build(&pdf).expect("build revision map");
        let signatures = discover_signatures(&pdf, &revisions).expect("discover signatures");

        // The signature should be reported (not dropped)
        assert_eq!(
            signatures.len(),
            1,
            "malformed signature field should be reported"
        );

        // It should be marked as broken/unclear coverage
        let sig = &signatures[0];
        assert_eq!(sig.field_name, "Sig1");
        assert_eq!(sig.coverage, SignatureCoverage::Unclear);
    }
}
