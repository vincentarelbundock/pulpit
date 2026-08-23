#![forbid(unsafe_code)]
//! Applying a signature to a file on disk: the orchestration layer.
//!
//! This is the one module in the signing feature that is *allowed* to know
//! about all three others. §22.2 makes the knowledge separation normative at
//! the leaf modules — `sign` must not depend on `pdfwrite` or `verify`, and
//! `pdfwrite` must not depend on `sign` — precisely so that something above
//! them can compose them. That something is this module: it reads the
//! document with [`crate::verify`], assembles the signing revision with
//! [`crate::pdfwrite`], and produces the CMS with the rest of
//! [`crate::sign`]. Nothing here may be pulled back down into a leaf.
//!
//! What it does, in order (§23.3, §31.3, §32):
//!
//! 1. Read the source and pre-flight it. A *new* field is only ever created
//!    on a document that carries no signature at all; filling an existing
//!    empty field is the countersigning path, and the pre-flight refusals
//!    (already-signed field, FieldMDP lock, prior `NO_CHANGES`
//!    certification) travel out to the caller unchanged.
//! 2. Assemble one incremental update containing the signature dictionary
//!    with its `/ByteRange` and `/Contents` placeholders, plus the minimum
//!    set of re-emitted objects the target requires.
//! 3. Back-patch `/ByteRange`, digest the two spans, build the CMS, fill the
//!    reservation. One retry with a doubled reservation, two attempts total
//!    (§23.5).
//! 4. Write the candidate to a temporary file beside the destination and run
//!    the §32 gate against the bytes *as they are on disk*. Every clause of
//!    it must pass.
//! 5. Only then fsync and rename into place. On any failure the temporary
//!    file is removed and neither the source nor the destination is touched.
//!
//! Two things this module deliberately does not do, because `pulpit-render`
//! reads neither the clock nor an entropy source: it takes `signing_time` as
//! unix seconds and the trailer's new `/ID` second element as 16 caller-
//! supplied bytes. Both belong to the caller (the application layer).

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::pdfwrite::{
    BackPatchContext, IncrementalWriter, PdfObject, PdfTokenizer, PdfWriteError, PlaceholderOffsets,
};
use crate::sign::{
    build_cms, digest_algorithm_for, estimate_cms_size, Credential, DigestAlgorithm, SigningError,
    SigningProfile,
};
use crate::verify::preflight::{preflight_sign, PreflightRefusal};
use crate::verify::{
    self, find_catalog_ref, SignatureCoverage, SignatureVerification, VerifyError,
};

/// Which signature field the revision targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignTarget {
    /// Create a fresh invisible signature field. Permitted only on a document
    /// that carries no signature yet (§31.3): creating a field edits the
    /// AcroForm and a page's annotation array, which is a content change.
    NewInvisibleField {
        /// Field name; a unique `SignatureN` is chosen when absent.
        name: Option<String>,
    },
    /// Fill an existing, empty signature field — the countersigning path.
    ExistingField(String),
}

/// Everything the caller decides about one signing operation.
#[derive(Debug, Clone)]
pub struct SignRequest {
    /// §21.2 profile. `AdbePkcs7Detached` unless the caller says otherwise.
    pub profile: SigningProfile,
    /// Claimed signing time, unix seconds. Supplied by the caller: this
    /// crate never reads the clock.
    pub signing_time: i64,
    /// The field to sign.
    pub field: SignTarget,
    pub reason: Option<String>,
    pub location: Option<String>,
    pub contact: Option<String>,
    /// The new second element of the trailer's `/ID`. Supplied by the
    /// caller: this crate never draws randomness.
    ///
    /// There is no useful default. [`SignRequest::default`] leaves this all
    /// zero, and signing **refuses** an all-zero value rather than writing a
    /// constant `/ID` shared by every document signed from a defaulted
    /// request. Set it to 16 fresh random bytes.
    pub id2: [u8; 16],
    /// Reserve exactly the estimated size instead of the 50% margin (§23.5).
    pub tight_size_estimates: bool,
    /// A visible signature appearance (§25.5). `None` keeps the current
    /// invisible-field behaviour unchanged.
    pub appearance: Option<SignAppearance>,
}

impl Default for SignRequest {
    fn default() -> Self {
        SignRequest {
            profile: SigningProfile::AdbePkcs7Detached,
            signing_time: 0,
            field: SignTarget::NewInvisibleField { name: None },
            reason: None,
            location: None,
            contact: None,
            id2: [0u8; 16],
            tight_size_estimates: false,
            appearance: None,
        }
    }
}

/// A visible appearance drawn into the signature widget's `/AP /N` form
/// XObject (§25.5).
///
/// This is decoration, not proof: the drawn ink or text is never consulted by
/// the §32 verification gate, and a document with a drawn appearance and no
/// valid CMS is not a signature. Nothing here changes what is cryptographically
/// asserted; it only changes what a viewer renders inside the widget rect.
#[derive(Debug, Clone)]
pub struct SignAppearance {
    /// Where the appearance is drawn.
    pub placement: AppearancePlacement,
    pub content: AppearanceContent,
    /// The `/Rotate` of the page the appearance lands on, which the caller
    /// supplies because it already knows the page's geometry.
    ///
    /// This crate does not read the page tree for it: `/Rotate` is
    /// inheritable, so answering it properly means walking `/Parent` chains,
    /// and the caller — which measured the page to place the box in the first
    /// place — has the answer already. Left [`AppearanceRotation::None`], the
    /// content is drawn straight into the box, which is right for the
    /// overwhelmingly common upright page and wrong for every other one.
    pub page_rotation: AppearanceRotation,
}

/// The rotation of the page an appearance is drawn on, in degrees clockwise
/// — the sense `/Rotate` uses.
///
/// A viewer draws a page's content in user space and then turns the whole
/// sheet clockwise by `/Rotate` before showing it. Content drawn straight
/// into a widget's box therefore arrives on screen turned by the same amount:
/// a signature on a `/Rotate 90` page reads sideways. Naming the rotation
/// lets the appearance stream carry a `/Matrix` that turns the content the
/// other way first, so what the reader sees is upright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppearanceRotation {
    #[default]
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl AppearanceRotation {
    /// Does the page's rotation exchange the box's width and height between
    /// user space and what the reader sees?
    fn swaps_axes(self) -> bool {
        matches!(self, AppearanceRotation::Cw90 | AppearanceRotation::Cw270)
    }

    /// The form XObject's `/Matrix`, as `[a b c d e f]`, or `None` for an
    /// upright page, where no matrix is written at all.
    ///
    /// The matrix rotates the content counter-clockwise by the page's own
    /// clockwise rotation, so the two cancel and the mark stands upright on
    /// screen. Its translation is zero: a viewer maps an appearance stream
    /// onto `/Rect` by transforming the `/BBox` through `/Matrix`, taking the
    /// bounding box of the result and scaling *that* onto the rect (PDF 32000
    /// §12.5.5), so any translation here is absorbed.
    fn matrix(self) -> Option<[f64; 6]> {
        match self {
            AppearanceRotation::None => None,
            // Rotation by +90°: (x, y) -> (-y, x).
            AppearanceRotation::Cw90 => Some([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
            AppearanceRotation::Cw180 => Some([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
            // Rotation by -90°: (x, y) -> (y, -x).
            AppearanceRotation::Cw270 => Some([0.0, -1.0, 1.0, 0.0, 0.0, 0.0]),
        }
    }
}

/// Which box the appearance is drawn into (§25.5).
///
/// The two modes answer two different questions. `FieldRect` is "draw inside
/// the box the document's author already drew", which is the sign-here flow:
/// the widget keeps its own `/Rect` and nothing moves. `Rect` is "put a box
/// here", which is the preset-placement flow: the widget's `/Rect` is set to
/// the caller's rect, moving an existing field if that is what the caller
/// asked for.
///
/// Whichever rect ends up in force sizes the appearance XObject's `/BBox`, so
/// normalized ink coordinates and text scale to the box a viewer actually
/// renders. On a rotated page the two are exchanged and a `/Matrix` turns the
/// content back; see [`SignAppearance::page_rotation`].
///
/// The rect is in PDF *user* space, not in the space the reader measures
/// against. A page with a crop box away from the origin, or a `/Rotate`, puts
/// those two spaces apart, and a caller that hands over a rect measured on
/// screen will see the mark land somewhere else in every conformant viewer.
/// `pulpit_core::page::PageGeometry::rect_to_user_space` is the conversion.
#[derive(Debug, Clone, PartialEq)]
pub enum AppearancePlacement {
    /// Draw inside the target field's existing `/Rect`, leaving it untouched.
    ///
    /// Only meaningful for [`SignTarget::ExistingField`] — a field that does
    /// not exist yet has no rect to draw inside, and asking for this on
    /// [`SignTarget::NewInvisibleField`] is a typed refusal. A field whose
    /// `/Rect` is missing or zero-area is refused too, rather than being
    /// silently given an appearance stream no viewer can show.
    FieldRect,
    /// Draw into an explicit rect, and set the widget's `/Rect` to it.
    ///
    /// For a new field, `page_index` also decides which page's `/Annots` the
    /// widget joins. For an existing field the widget stays on the page it is
    /// already on — the index is still checked against the document's page
    /// count, so an out-of-range request is refused rather than ignored.
    Rect {
        /// Zero-based page index.
        page_index: usize,
        /// The rect in PDF page coordinates: `[x0, y0, x1, y1]`.
        rect: [f64; 4],
    },
}

/// What is drawn inside the appearance's bounding box.
#[derive(Debug, Clone)]
pub enum AppearanceContent {
    /// Freehand strokes, normalized to `0.0..=1.0` within the rect (origin at
    /// the rect's bottom-left, matching PDF user space).
    Ink {
        strokes: Vec<Vec<(f64, f64)>>,
        stroke_width: f64,
    },
    /// A two-line text block: signer name, then a time label.
    ///
    /// Drawn with the built-in Helvetica name only, with no font metrics
    /// available to this crate: text is not measured against the rect width,
    /// so a long name or label can overflow the box. Callers that need exact
    /// fit should keep the strings short or size the rect generously.
    Text {
        signer_name: String,
        time_label: String,
    },
    /// Ink strokes composited with the same two-line text block.
    InkAndText {
        strokes: Vec<Vec<(f64, f64)>>,
        stroke_width: f64,
        signer_name: String,
        time_label: String,
    },
}

/// What a successful signing operation produced.
#[derive(Debug, Clone)]
pub struct SignReport {
    /// The field that now carries the signature.
    pub field_name: String,
    /// Number of signatures in the promoted file, source count plus one.
    pub signature_count: usize,
    /// Hex characters reserved for `/Contents`.
    pub bytes_reserved: usize,
    /// Length of the CMS actually written, in bytes.
    pub cms_len: usize,
    /// 1, or 2 when the first reservation was too small (§23.5).
    pub attempts: usize,
    /// Size of the promoted file.
    pub output_len: u64,
    /// The target field carried a seed value dictionary, which is ignored
    /// (§25.4, §36.5). Worth surfacing to the user.
    pub seed_value_ignored: bool,
}

/// Failures of the composed operation.
///
/// This enum lives here rather than in `sign::errors` on purpose: it names a
/// [`PreflightRefusal`], and `sign`'s leaf modules must not know that
/// `verify` exists (§22.2).
#[derive(Debug, thiserror::Error)]
pub enum SignApplyError {
    /// A §25.4 pre-flight refusal, passed through unchanged.
    #[error("{0}")]
    Refused(#[from] PreflightRefusal),

    /// Requested a content change the append-only rules forbid (§31.3).
    #[error("content change refused in append-only mode: {detail}")]
    ContentChangeInAppendOnlyMode { detail: String },

    /// The §32 gate rejected the candidate; nothing was promoted.
    #[error("post-sign verification failed: {detail}; the source is unchanged and no output was written")]
    PostSignVerificationFailed { detail: String },

    #[error("{0}")]
    Signing(#[from] SigningError),

    #[error("{0}")]
    Write(#[from] PdfWriteError),

    #[error("{0}")]
    Verify(#[from] VerifyError),

    #[error("the document cannot be signed: {0}")]
    Unsupported(String),

    /// The requested visible appearance cannot be placed (§25.5). Nothing is
    /// written: the source is unchanged and no candidate reaches the gate.
    #[error("the signature appearance cannot be placed: {0}")]
    AppearancePlacement(String),

    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Sign `source` into `destination`, leaving `source` untouched.
///
/// `destination` is written only if every §32 check passes, and it is written
/// atomically: the candidate goes to a hidden temporary file in the same
/// directory, is fsynced, and is then renamed over the destination.
pub fn sign_document_file(
    source: &Path,
    destination: &Path,
    credential: &Credential,
    request: &SignRequest,
) -> Result<SignReport, SignApplyError> {
    sign_document_file_inner(source, destination, credential, request, None)
}

/// A hook that may mutate the candidate bytes before the §32 gate.
type Tamper<'a> = &'a dyn Fn(&mut Vec<u8>);

/// As [`sign_document_file`], but with a hook that may mutate the candidate
/// bytes after signing and before the §32 gate.
///
/// This exists so that tests can prove the gate is load-bearing — that a
/// corrupted candidate is refused and nothing is promoted. It is not part of
/// the ordinary signing path and no production caller should use it.
#[doc(hidden)]
pub fn sign_document_file_with_tamper(
    source: &Path,
    destination: &Path,
    credential: &Credential,
    request: &SignRequest,
    tamper: Tamper<'_>,
) -> Result<SignReport, SignApplyError> {
    sign_document_file_inner(source, destination, credential, request, Some(tamper))
}

fn sign_document_file_inner(
    source: &Path,
    destination: &Path,
    credential: &Credential,
    request: &SignRequest,
    tamper: Option<Tamper<'_>>,
) -> Result<SignReport, SignApplyError> {
    // The promise is that the source is untouched. Signing in place would
    // break it — the rename at the end would replace the source with the
    // candidate — so it is refused before anything is read.
    if is_same_file(source, destination)? {
        return Err(SignApplyError::Refused(PreflightRefusal::InvalidState(
            format!(
                "the destination is the source file ({}); signing in place is refused because \
                 the source must be left untouched",
                source.display()
            ),
        )));
    }

    let source_bytes = std::fs::read(source).map_err(|e| SignApplyError::Io {
        path: source.to_path_buf(),
        source: e,
    })?;

    let plan = plan_revision(&source_bytes, request)?;

    // Assemble, sign, and if the reservation was too tight do it once more
    // with double the room. Two attempts, never more (§23.5).
    let digest_algorithm = digest_algorithm_for(credential)?;
    let mut bytes_reserved = estimate_cms_size(
        credential,
        &vec![0u8; digest_algorithm.hash_len()],
        request.profile,
        false,
        request.tight_size_estimates,
        Some(request.signing_time),
    )?;

    let mut attempts = 0usize;
    let (mut candidate, cms_len) = loop {
        attempts += 1;
        let assembled = assemble_revision(&source_bytes, request, &plan, bytes_reserved)?;
        match sign_assembled(assembled, credential, request, digest_algorithm) {
            Ok((bytes, cms_len)) => break (bytes, cms_len),
            Err(SignApplyError::Signing(SigningError::SignatureTooLarge { .. }))
            | Err(SignApplyError::Write(PdfWriteError::SignatureTooLarge { .. }))
                if attempts < 2 =>
            {
                bytes_reserved *= 2;
            }
            Err(e) => return Err(e),
        }
    };

    if let Some(tamper) = tamper {
        tamper(&mut candidate);
    }

    // Write the candidate beside the destination, under the same hidden
    // temporary-file convention `pdf::pdfium::write_atomically` uses, so an
    // interrupted signing leaves the destination either untouched or holding
    // a complete signed PDF.
    let (temporary, mut temporary_file) = create_temporary(destination)?;
    write_and_sync(&temporary, &mut temporary_file, &candidate)?;
    drop(temporary_file);

    // §32: the gate reads the file back from disk. Anything less would be
    // checking what we meant to write rather than what we wrote.
    let promoted = std::fs::read(&temporary).map_err(|e| SignApplyError::Io {
        path: temporary.clone(),
        source: e,
    })?;
    let verified = match verification_gate(
        &source_bytes,
        &promoted,
        plan.previous_signatures,
        &plan.field_name,
    ) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(e);
        }
    };

    if let Err(e) = std::fs::rename(&temporary, destination) {
        let _ = std::fs::remove_file(&temporary);
        return Err(SignApplyError::Io {
            path: destination.to_path_buf(),
            source: e,
        });
    }

    Ok(SignReport {
        field_name: plan.field_name,
        signature_count: verified,
        bytes_reserved,
        cms_len,
        attempts,
        output_len: promoted.len() as u64,
        seed_value_ignored: plan.seed_value_ignored,
    })
}

// --- The §32 gate ---------------------------------------------------------

/// Run every §32 check against a candidate. Returns the signature count on
/// success.
///
/// Public because it is the one part of this module worth calling on its own:
/// it is the definition of "this output may be promoted".
pub fn verification_gate(
    source: &[u8],
    candidate: &[u8],
    previous_signatures: usize,
    new_field: &str,
) -> Result<usize, SignApplyError> {
    let fail = |detail: String| SignApplyError::PostSignVerificationFailed { detail };

    // The bytes preceding the appended revision are identical. Compared
    // directly, not inferred from a digest.
    if candidate.len() < source.len() {
        return Err(fail(format!(
            "the output is shorter than the source ({} < {} bytes)",
            candidate.len(),
            source.len()
        )));
    }
    if &candidate[..source.len()] != source {
        return Err(fail(
            "the bytes preceding the appended revision differ from the source".to_string(),
        ));
    }

    let verifications =
        verify::verify_signatures(candidate).map_err(|e| fail(format!("cannot re-read: {e}")))?;

    let expected = previous_signatures + 1;
    if verifications.len() != expected {
        return Err(fail(format!(
            "expected {expected} signature(s) in the output, found {}",
            verifications.len()
        )));
    }

    let mut saw_new_field = false;
    for verification in verifications.iter() {
        let status = match verification {
            SignatureVerification::Checked(status) => status,
            SignatureVerification::Broken { field_name, reason } => {
                return Err(fail(format!(
                    "signature '{field_name}' is broken after signing: {reason}"
                )))
            }
        };
        if !status.intact {
            return Err(fail(format!(
                "signature '{}' is not intact after signing",
                status.field_name
            )));
        }
        if !status.valid {
            return Err(fail(format!(
                "signature '{}' is not valid after signing",
                status.field_name
            )));
        }
        let newest = status.field_name == new_field;
        saw_new_field |= newest;
        let required = if newest {
            SignatureCoverage::EntireFile
        } else {
            SignatureCoverage::EntireRevision
        };
        if status.coverage != required {
            return Err(fail(format!(
                "signature '{}' has coverage {:?}, expected {:?}",
                status.field_name, status.coverage, required
            )));
        }
    }

    if !saw_new_field {
        return Err(fail(format!(
            "the output does not report a signature on field '{new_field}'"
        )));
    }

    Ok(verifications.len())
}

// --- Planning -------------------------------------------------------------

/// What the revision has to contain, decided before a byte is written.
struct RevisionPlan {
    field_name: String,
    previous_signatures: usize,
    seed_value_ignored: bool,
    existing_field: Option<u32>,
}

fn plan_revision(source: &[u8], request: &SignRequest) -> Result<RevisionPlan, SignApplyError> {
    // Every target reaches this point, but only the ExistingField branch runs
    // `preflight_sign`, so the refusal has to sit here to cover creating a new
    // field as well. We append plaintext and carry no encryption layer
    // (§23.2), so signing an encrypted document could only ever produce a file
    // no reader accepts.
    if crate::verify::is_encrypted(source) {
        return Err(PreflightRefusal::EncryptedDocument.into());
    }

    let previous_signatures = count_signatures(source)?;

    match &request.field {
        SignTarget::ExistingField(name) => {
            let ok = preflight_sign(source, Some(name.as_str()))?;
            let field_obj = find_field_object(source, &ok.target_field)?.ok_or_else(|| {
                SignApplyError::Unsupported(format!(
                    "signature field '{}' could not be located",
                    ok.target_field
                ))
            })?;
            Ok(RevisionPlan {
                field_name: ok.target_field,
                previous_signatures,
                seed_value_ignored: ok.seed_value_ignored,
                existing_field: Some(field_obj),
            })
        }
        SignTarget::NewInvisibleField { name } => {
            // §31.3: a new field may only be created on an unsigned document.
            if previous_signatures > 0 {
                return Err(SignApplyError::ContentChangeInAppendOnlyMode {
                    detail: format!(
                        "the document already carries {previous_signatures} signature(s); \
                         creating a signature field edits the AcroForm and a page's \
                         annotations, so only an existing empty field may be signed"
                    ),
                });
            }
            // Check 2 still applies: a prior certification may forbid signing
            // outright. A document with no signature at all has no /Perms, so
            // this is cheap insurance rather than a common path.
            let taken = existing_field_names(source)?;
            let field_name = match name {
                Some(name) => {
                    if taken.iter().any(|t| t == name) {
                        return Err(SignApplyError::Unsupported(format!(
                            "a field named '{name}' already exists"
                        )));
                    }
                    name.clone()
                }
                None => {
                    let mut n = 1usize;
                    loop {
                        let candidate = format!("Signature{n}");
                        if !taken.contains(&candidate) {
                            break candidate;
                        }
                        n += 1;
                    }
                }
            };
            Ok(RevisionPlan {
                field_name,
                previous_signatures,
                seed_value_ignored: false,
                existing_field: None,
            })
        }
    }
}

fn count_signatures(bytes: &[u8]) -> Result<usize, SignApplyError> {
    let revisions = verify::RevisionMap::build(bytes)?;
    Ok(verify::discover_signatures(bytes, &revisions)?.len())
}

// --- Assembly -------------------------------------------------------------

/// A candidate file with the placeholders still unfilled.
struct Assembled {
    bytes: Vec<u8>,
    offsets: PlaceholderOffsets,
}

fn assemble_revision(
    source: &[u8],
    request: &SignRequest,
    plan: &RevisionPlan,
    bytes_reserved: usize,
) -> Result<Assembled, SignApplyError> {
    // `/ID` must change with every revision (§14.4): it is what a reader uses
    // to tell one revision of a document from another, and two revisions that
    // share one are indistinguishable to anything that keys on it. This crate
    // never draws randomness, so the caller supplies it — and
    // `SignRequest::default()` has to put *something* in the field. An
    // all-zero array is that something, and writing it would be a silent,
    // constant `/ID` shared by every document signed from a defaulted request.
    // A caller that forgot to set it is refused rather than obliged.
    if request.id2 == [0u8; 16] {
        return Err(SignApplyError::Unsupported(
            "the new /ID second element is all zero bytes, which is the unset value of \
             SignRequest::id2; this crate never draws randomness, so the caller must supply \
             16 fresh random bytes. The source document is unchanged and nothing was written."
                .to_string(),
        ));
    }
    let writer = IncrementalWriter::open(source)?;
    let mut next_object = writer.next_object_number();
    let allocate = |next_object: &mut u32| {
        let n = *next_object;
        *next_object += 1;
        n
    };

    // A placement mode that does not fit the target is caller misuse, and it
    // is refused before a byte is assembled rather than quietly reinterpreted.
    if let Some(appearance) = &request.appearance {
        if matches!(appearance.placement, AppearancePlacement::FieldRect)
            && plan.existing_field.is_none()
        {
            return Err(SignApplyError::AppearancePlacement(
                "FieldRect draws inside the rect a field already has, but this request creates \
                 a new signature field, which has none yet; the source is unchanged — either \
                 target an existing field, or place the new field's appearance with \
                 AppearancePlacement::Rect { page_index, rect }"
                    .to_string(),
            ));
        }
    }

    let signature_object = allocate(&mut next_object);
    let appearance_object = request
        .appearance
        .as_ref()
        .map(|_| allocate(&mut next_object));
    let mut objects: Vec<(u32, u16, PdfObject)> = Vec::new();
    // The box the appearance stream is drawn in, decided per branch below and
    // used verbatim as the XObject's /BBox.
    let mut appearance_size: Option<(f64, f64)> = None;

    match plan.existing_field {
        Some(field_object) => {
            // The minimal content-change line of §31.3: the only object that
            // changes meaning is the field, which gains a /V.
            let mut entries = parse_object_dictionary(source, field_object)?;
            set_entry(
                &mut entries,
                "V",
                PdfObject::IndirectRef {
                    obj_num: signature_object,
                    gen_num: 0,
                },
            );
            if let (Some(appearance), Some(xobject_num)) = (&request.appearance, appearance_object)
            {
                let rect = match &appearance.placement {
                    // The sender drew the box; signing draws inside it and
                    // leaves /Rect exactly as it was.
                    AppearancePlacement::FieldRect => {
                        existing_widget_rect(&entries, &plan.field_name)?
                    }
                    AppearancePlacement::Rect { page_index, rect } => {
                        // The widget stays on its own page — only the box
                        // moves — but a page the document does not have is
                        // still a mistake worth naming.
                        check_page_index(source, *page_index)?;
                        set_entry(
                            &mut entries,
                            "Rect",
                            PdfObject::Array(rect.iter().map(|v| PdfObject::Real(*v)).collect()),
                        );
                        *rect
                    }
                };
                appearance_size = Some(rect_size(rect));
                point_widget_at_appearance(&mut entries, xobject_num);
            }
            objects.push((field_object, 0, PdfObject::Dictionary(entries)));
        }
        None => {
            let catalog = find_catalog_ref(source)?;
            let mut catalog_entries = parse_object_dictionary(source, catalog.0)?;
            // The widget joins the /Annots of the page the appearance names.
            // Without an appearance the field is invisible and page 0 is as
            // good a home as any, which is where it has always gone.
            let target_page = match &request.appearance {
                Some(SignAppearance {
                    placement: AppearancePlacement::Rect { page_index, .. },
                    ..
                }) => *page_index,
                _ => 0,
            };
            let page_object = page_object_at(source, &catalog_entries, target_page)?;
            let field_object = allocate(&mut next_object);

            // AcroForm: reuse the existing one when it is indirect, promote a
            // direct one to its own object, otherwise create it.
            let (acroform_object, mut acroform_entries) =
                match entry(&catalog_entries, "AcroForm").cloned() {
                    Some(PdfObject::IndirectRef { obj_num, .. }) => {
                        (obj_num, parse_object_dictionary(source, obj_num)?)
                    }
                    Some(PdfObject::Dictionary(entries)) => (allocate(&mut next_object), entries),
                    Some(_) | None => (allocate(&mut next_object), Vec::new()),
                };

            let mut fields = match entry(&acroform_entries, "Fields") {
                Some(PdfObject::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            fields.push(PdfObject::IndirectRef {
                obj_num: field_object,
                gen_num: 0,
            });
            set_entry(&mut acroform_entries, "Fields", PdfObject::Array(fields));
            // /SigFlags 3 = SignaturesExist | AppendOnly (§25.2).
            set_entry(&mut acroform_entries, "SigFlags", PdfObject::Integer(3));
            // Deleted, never set to false: it tells viewers to regenerate
            // appearances, which changes how a signed document renders.
            acroform_entries.retain(|(k, _)| k != "NeedAppearances");

            set_entry(
                &mut catalog_entries,
                "AcroForm",
                PdfObject::IndirectRef {
                    obj_num: acroform_object,
                    gen_num: 0,
                },
            );

            let mut page_entries = parse_object_dictionary(source, page_object)?;
            let mut annots = match entry(&page_entries, "Annots") {
                Some(PdfObject::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            annots.push(PdfObject::IndirectRef {
                obj_num: field_object,
                gen_num: 0,
            });
            set_entry(&mut page_entries, "Annots", PdfObject::Array(annots));

            // Field and widget merged into one dictionary, which is what most
            // producers write and most consumers expect (§25.1).
            let mut field_entries = vec![
                ("FT".into(), PdfObject::Name("Sig".into())),
                (
                    "T".into(),
                    PdfObject::String(plan.field_name.clone().into()),
                ),
                (
                    "V".into(),
                    PdfObject::IndirectRef {
                        obj_num: signature_object,
                        gen_num: 0,
                    },
                ),
                ("Type".into(), PdfObject::Name("Annot".into())),
                ("Subtype".into(), PdfObject::Name("Widget".into())),
                (
                    "Rect".into(),
                    PdfObject::Array(vec![
                        PdfObject::Integer(0),
                        PdfObject::Integer(0),
                        PdfObject::Integer(0),
                        PdfObject::Integer(0),
                    ]),
                ),
                // Locked (128) | Print (4): invisible, but printable, which
                // is what PDF/A wants (§25.1).
                ("F".into(), PdfObject::Integer(132)),
                (
                    "P".into(),
                    PdfObject::IndirectRef {
                        obj_num: page_object,
                        gen_num: 0,
                    },
                ),
            ];
            if let (Some(appearance), Some(xobject_num)) = (&request.appearance, appearance_object)
            {
                // `FieldRect` was refused above, so this is always an
                // explicit rect, on the page whose /Annots the widget joined.
                let AppearancePlacement::Rect { rect, .. } = &appearance.placement else {
                    unreachable!("FieldRect is refused for a new field before assembly");
                };
                set_entry(
                    &mut field_entries,
                    "Rect",
                    PdfObject::Array(rect.iter().map(|v| PdfObject::Real(*v)).collect()),
                );
                appearance_size = Some(rect_size(*rect));
                point_widget_at_appearance(&mut field_entries, xobject_num);
            }

            objects.push((catalog.0, 0, PdfObject::Dictionary(catalog_entries)));
            objects.push((acroform_object, 0, PdfObject::Dictionary(acroform_entries)));
            objects.push((page_object, 0, PdfObject::Dictionary(page_entries)));
            objects.push((field_object, 0, PdfObject::Dictionary(field_entries)));
        }
    }

    if let (Some(appearance), Some(xobject_num), Some((width, height))) =
        (&request.appearance, appearance_object, appearance_size)
    {
        objects.push((
            xobject_num,
            0,
            PdfObject::Raw(build_appearance_xobject(
                &appearance.content,
                width,
                height,
                appearance.page_rotation,
            )),
        ));
    }

    objects.push((
        signature_object,
        0,
        signature_dictionary(request, bytes_reserved),
    ));
    objects.sort_by_key(|(n, _, _)| *n);

    let mut cursor = Cursor::new(Vec::new());
    writer.append_objects(&mut cursor, &objects, &request.id2)?;
    let bytes = cursor.into_inner();

    let offsets = locate_placeholders(&bytes, source.len(), bytes_reserved)?;
    Ok(Assembled { bytes, offsets })
}

fn signature_dictionary(request: &SignRequest, bytes_reserved: usize) -> PdfObject {
    let sub_filter = match request.profile {
        SigningProfile::AdbePkcs7Detached => "adbe.pkcs7.detached",
        SigningProfile::EtsiCadesDetached => "ETSI.CAdES.detached",
    };

    let mut byte_range = b"[]".to_vec();
    byte_range.extend(std::iter::repeat_n(b' ', 60));
    let mut contents = Vec::with_capacity(bytes_reserved + 2);
    contents.push(b'<');
    contents.extend(std::iter::repeat_n(b'0', bytes_reserved));
    contents.push(b'>');

    let mut entries = vec![
        ("Type".to_string(), PdfObject::Name("Sig".into())),
        (
            "Filter".to_string(),
            PdfObject::Name("Adobe.PPKLite".into()),
        ),
        ("SubFilter".to_string(), PdfObject::Name(sub_filter.into())),
        ("ByteRange".to_string(), PdfObject::Raw(byte_range)),
        ("Contents".to_string(), PdfObject::Raw(contents)),
        (
            "M".to_string(),
            PdfObject::String(pdf_date(request.signing_time).into()),
        ),
    ];
    // /Name is deliberately left unset: viewers should take the displayed
    // name from the certificate subject (§25.3).
    if let Some(reason) = &request.reason {
        entries.push(("Reason".into(), PdfObject::String(reason.clone().into())));
    }
    if let Some(location) = &request.location {
        entries.push((
            "Location".into(),
            PdfObject::String(location.clone().into()),
        ));
    }
    if let Some(contact) = &request.contact {
        entries.push((
            "ContactInfo".into(),
            PdfObject::String(contact.clone().into()),
        ));
    }
    entries.push((
        "Prop_Build".into(),
        PdfObject::Dictionary(vec![(
            "App".into(),
            PdfObject::Dictionary(vec![("Name".into(), PdfObject::Name("pulpit".into()))]),
        )]),
    ));

    PdfObject::Dictionary(entries)
}

// --- Appearances (§25.5) ---------------------------------------------------

/// The width and height of a rect, whichever corner order it was written in.
fn rect_size(rect: [f64; 4]) -> (f64, f64) {
    ((rect[2] - rect[0]).abs(), (rect[3] - rect[1]).abs())
}

/// The `/Rect` a widget already carries, for [`AppearancePlacement::FieldRect`].
///
/// A missing or degenerate rect is refused rather than drawn into: an
/// appearance stream with a zero-area `/BBox` renders as nothing, and a
/// signature that silently shows nothing is worse than one the caller was
/// told to make invisible on purpose.
fn existing_widget_rect(
    entries: &[(String, PdfObject)],
    field_name: &str,
) -> Result<[f64; 4], SignApplyError> {
    let refuse = |detail: String| SignApplyError::AppearancePlacement(detail);

    let Some(PdfObject::Array(items)) = entry(entries, "Rect") else {
        return Err(refuse(format!(
            "the appearance was to be drawn inside field '{field_name}', but that field carries \
             no /Rect array; the source is unchanged — sign it without an appearance (an \
             invisible signature), or supply AppearancePlacement::Rect with an explicit box"
        )));
    };
    if items.len() != 4 {
        return Err(refuse(format!(
            "field '{field_name}' has a /Rect of {} element(s) rather than 4; the source is \
             unchanged — sign it without an appearance, or supply AppearancePlacement::Rect \
             with an explicit box",
            items.len()
        )));
    }
    let mut rect = [0.0f64; 4];
    for (slot, item) in rect.iter_mut().zip(items) {
        *slot = match item {
            PdfObject::Integer(v) => *v as f64,
            PdfObject::Real(v) => *v,
            other => {
                return Err(refuse(format!(
                    "field '{field_name}' has a /Rect whose entries are not all numbers \
                     ({other:?}); the source is unchanged — sign it without an appearance, or \
                     supply AppearancePlacement::Rect with an explicit box"
                )))
            }
        };
    }

    let (width, height) = rect_size(rect);
    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return Err(refuse(format!(
            "field '{field_name}' has a zero-area /Rect [{} {} {} {}], so an appearance drawn \
             inside it would render as nothing; the source is unchanged — sign this field \
             without an appearance (an invisible signature is what this field was built for), \
             or supply AppearancePlacement::Rect with an explicit box",
            fmt_num(rect[0]),
            fmt_num(rect[1]),
            fmt_num(rect[2]),
            fmt_num(rect[3])
        )));
    }
    Ok(rect)
}

/// Point `/AP /N` at the freshly appended form XObject and delete `/AS` if
/// present — required whenever a widget carries an appearance stream (§25.5).
///
/// `/Rect` is the caller's business: `FieldRect` keeps the widget's own, and
/// `Rect` overwrites it before this runs.
fn point_widget_at_appearance(entries: &mut Vec<(String, PdfObject)>, xobject_num: u32) {
    set_entry(
        entries,
        "AP",
        PdfObject::Dictionary(vec![(
            "N".into(),
            PdfObject::IndirectRef {
                obj_num: xobject_num,
                gen_num: 0,
            },
        )]),
    );
    entries.retain(|(k, _)| k != "AS");
}

/// Build the bytes of the appearance's form XObject: `<< dict >>\nstream\n...\nendstream`.
/// The caller wraps this with the `N 0 obj` header and `endobj` trailer via
/// [`PdfObject::Raw`], the same convention `IncrementalWriter::append_objects`
/// uses for every other appended object.
///
/// `width` and `height` are the *rect's* size in user space. On a rotated
/// page the box the reader sees has those two exchanged, so the content is
/// drawn against the exchanged pair — an upright box — and `/Matrix` turns
/// the whole thing back into the rect's own orientation. The transformed
/// `/BBox` then measures exactly `width` by `height`, so the viewer's fit of
/// the appearance onto `/Rect` is a translation and the content is neither
/// squashed nor stretched.
fn build_appearance_xobject(
    content: &AppearanceContent,
    width: f64,
    height: f64,
    rotation: AppearanceRotation,
) -> Vec<u8> {
    // The box as the reader sees it, which is what the content is composed
    // against: normalized ink runs left to right on screen, and text reads
    // along the same axis.
    let (upright_width, upright_height) = if rotation.swaps_axes() {
        (height, width)
    } else {
        (width, height)
    };
    let (content, has_text) = appearance_content_stream(content, upright_width, upright_height);

    let mut dict = Vec::new();
    dict.extend_from_slice(b"<< /Type /XObject /Subtype /Form /BBox [0 0 ");
    dict.extend_from_slice(fmt_num(upright_width).as_bytes());
    dict.push(b' ');
    dict.extend_from_slice(fmt_num(upright_height).as_bytes());
    dict.extend_from_slice(b"]");
    if let Some(matrix) = rotation.matrix() {
        dict.extend_from_slice(b" /Matrix [");
        for (index, value) in matrix.iter().enumerate() {
            if index > 0 {
                dict.push(b' ');
            }
            dict.extend_from_slice(fmt_num(*value).as_bytes());
        }
        dict.extend_from_slice(b"]");
    }
    dict.extend_from_slice(b" /Resources <<");
    if has_text {
        dict.extend_from_slice(
            b" /Font << /F0 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        );
    }
    dict.extend_from_slice(b" >> /Length ");
    dict.extend_from_slice(content.len().to_string().as_bytes());
    dict.extend_from_slice(b" >>\nstream\n");
    dict.extend_from_slice(&content);
    dict.extend_from_slice(b"\nendstream");
    dict
}

fn appearance_content_stream(
    content: &AppearanceContent,
    width: f64,
    height: f64,
) -> (Vec<u8>, bool) {
    match content {
        AppearanceContent::Ink {
            strokes,
            stroke_width,
        } => (ink_ops(strokes, *stroke_width, width, height), false),
        AppearanceContent::Text {
            signer_name,
            time_label,
        } => (text_ops(signer_name, time_label, height), true),
        AppearanceContent::InkAndText {
            strokes,
            stroke_width,
            signer_name,
            time_label,
        } => {
            let mut ops = ink_ops(strokes, *stroke_width, width, height);
            ops.extend_from_slice(&text_ops(signer_name, time_label, height));
            (ops, true)
        }
    }
}

/// `1 J 1 j <w> w` then, for each stroke, an `m`/`l` path terminated with `S`.
/// Coordinates are normalized `0.0..=1.0` within the rect and mapped to
/// `[0, width] x [0, height]` BBox space.
fn ink_ops(strokes: &[Vec<(f64, f64)>], stroke_width: f64, width: f64, height: f64) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("1 J 1 j ");
    out.push_str(&fmt_num(stroke_width));
    out.push_str(" w\n");
    for stroke in strokes {
        for (i, (nx, ny)) in stroke.iter().enumerate() {
            let x = nx * width;
            let y = ny * height;
            out.push_str(&fmt_num(x));
            out.push(' ');
            out.push_str(&fmt_num(y));
            out.push_str(if i == 0 { " m\n" } else { " l\n" });
        }
        if !stroke.is_empty() {
            out.push_str("S\n");
        }
    }
    out.into_bytes()
}

/// Two lines of Helvetica text: signer name, then the time label. Font size
/// is `min(bbox_height / 3, 12)`, floored at `4`. This crate has no font
/// metrics for Helvetica, so the text is never measured against the BBox
/// width — a long name or time label can overflow the box; that is a named
/// limitation, not a bug to chase here.
fn text_ops(signer_name: &str, time_label: &str, height: f64) -> Vec<u8> {
    let size = (height / 3.0).clamp(4.0, 12.0);
    let line_gap = size + 2.0;
    let x = 2.0;
    let y = (height - size - 2.0).max(0.0);

    let mut out = String::new();
    out.push_str("BT\n");
    out.push_str("/F0 ");
    out.push_str(&fmt_num(size));
    out.push_str(" Tf\n");
    out.push_str(&fmt_num(x));
    out.push(' ');
    out.push_str(&fmt_num(y));
    out.push_str(" Td\n");
    out.push('(');
    out.push_str(&escape_pdf_string(signer_name));
    out.push_str(") Tj\n");
    out.push_str("0 ");
    out.push_str(&fmt_num(-line_gap));
    out.push_str(" Td\n");
    out.push('(');
    out.push_str(&escape_pdf_string(time_label));
    out.push_str(") Tj\n");
    out.push_str("ET\n");
    out.into_bytes()
}

/// Escape `(`, `)` and `\` for a PDF literal string, the same rule
/// [`PdfObject::String`] serialization uses.
fn escape_pdf_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '(' || c == ')' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Render a coordinate/length as fixed-point PDF syntax, trimming trailing
/// zeros, matching [`PdfObject::Real`]'s own formatting.
fn fmt_num(v: f64) -> String {
    let s = format!("{:.3}", v);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Find the placeholders in the assembled bytes. They are unique within the
/// appended revision, which is everything past the source's length.
fn locate_placeholders(
    bytes: &[u8],
    search_from: usize,
    bytes_reserved: usize,
) -> Result<PlaceholderOffsets, SignApplyError> {
    let find = |needle: &[u8]| -> Option<usize> {
        bytes[search_from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| search_from + p)
    };
    let byterange_key = b"/ByteRange [";
    let contents_key = b"/Contents <";
    let byterange_start = find(byterange_key)
        .map(|p| p + byterange_key.len() - 1)
        .ok_or_else(|| {
            SignApplyError::Unsupported("the /ByteRange placeholder went missing".into())
        })? as u64;
    let sig_start = find(contents_key)
        .map(|p| p + contents_key.len() - 1)
        .ok_or_else(|| {
            SignApplyError::Unsupported("the /Contents placeholder went missing".into())
        })? as u64;

    let offsets = PlaceholderOffsets {
        byterange_start,
        sig_start,
        sig_end: sig_start + bytes_reserved as u64 + 2,
        bytes_reserved,
    };
    offsets.validate()?;
    if bytes.get(offsets.sig_end as usize - 1) != Some(&b'>') {
        return Err(SignApplyError::Unsupported(
            "the /Contents reservation is not the size it was written at".into(),
        ));
    }
    Ok(offsets)
}

/// §23.3, in order: back-patch `/ByteRange` *before* digesting, digest the two
/// spans, build the CMS over that digest, then fill the reservation.
fn sign_assembled(
    assembled: Assembled,
    credential: &Credential,
    request: &SignRequest,
    digest_algorithm: DigestAlgorithm,
) -> Result<(Vec<u8>, usize), SignApplyError> {
    let Assembled { bytes, offsets } = assembled;
    let eof = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);

    let context = BackPatchContext {
        byterange_start: offsets.byterange_start,
        sig_start: offsets.sig_start,
        sig_end: offsets.sig_end,
    };
    context.finish(eof, &mut cursor)?;
    let mut bytes = cursor.into_inner();

    let first = &bytes[..offsets.sig_start as usize];
    let second = &bytes[offsets.sig_end as usize..eof as usize];
    let document_digest = match digest_algorithm {
        DigestAlgorithm::Sha256 => digest_spans::<Sha256>(first, second),
        DigestAlgorithm::Sha384 => digest_spans::<Sha384>(first, second),
        DigestAlgorithm::Sha512 => digest_spans::<Sha512>(first, second),
    };

    let cms = build_cms(
        credential,
        &document_digest,
        request.profile,
        false,
        None,
        Some(request.signing_time),
    )?;

    if cms.len() * 2 > offsets.bytes_reserved {
        return Err(SigningError::SignatureTooLarge {
            reserved: offsets.bytes_reserved,
            required: cms.len() * 2,
        }
        .into());
    }

    // The reservation's remaining hex digits stay '0' (§23.4).
    for (i, byte) in cms.iter().enumerate() {
        let position = offsets.sig_start as usize + 1 + i * 2;
        let hex = format!("{:02X}", byte);
        bytes[position] = hex.as_bytes()[0];
        bytes[position + 1] = hex.as_bytes()[1];
    }

    let cms_len = cms.len();
    Ok((bytes, cms_len))
}

fn digest_spans<D: Digest>(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut hasher = D::new();
    hasher.update(first);
    hasher.update(second);
    hasher.finalize().to_vec()
}

// --- Files ----------------------------------------------------------------

/// Do `source` and `destination` name the same file?
///
/// The destination need not exist yet, so it is resolved as its canonical
/// parent directory plus its final component. A path whose parent cannot be
/// resolved is not the source by definition — the source was resolvable.
fn is_same_file(source: &Path, destination: &Path) -> Result<bool, SignApplyError> {
    let source_canonical = source.canonicalize().map_err(|e| SignApplyError::Io {
        path: source.to_path_buf(),
        source: e,
    })?;

    if let Ok(destination_canonical) = destination.canonicalize() {
        return Ok(destination_canonical == source_canonical);
    }

    let (Some(parent), Some(name)) = (destination.parent(), destination.file_name()) else {
        return Ok(false);
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    match parent.canonicalize() {
        Ok(parent) => Ok(parent.join(name) == source_canonical),
        Err(_) => Ok(false),
    }
}

/// A per-attempt name for the temporary file. Uniqueness, not secrecy: the
/// secrecy comes from `O_EXCL`, which is what makes a pre-planted file or
/// symlink at the same name a failure rather than a hijack.
fn temporary_name() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};

    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);

    // `RandomState` is seeded per process by the standard library; hashing
    // the ticket and the clock through it gives a name an attacker cannot
    // predict from the pid alone.
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(ticket);
    hasher.write_u64(nanos);
    hasher.write_u32(std::process::id());
    format!(
        ".pulpit-sign-{}-{:016x}",
        std::process::id(),
        hasher.finish()
    )
}

/// Create `path`, and only create it: `O_CREAT|O_EXCL`, mode `0o600`.
///
/// An existing file, or a symlink pointing anywhere at all, makes this fail
/// with `AlreadyExists` instead of opening — and nothing is truncated.
fn open_exclusive(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Create the hidden temporary file in the destination's own directory.
///
/// `create_new` is `O_EXCL|O_CREAT`: it never follows a symlink and never
/// truncates an existing file, so a name planted by another user is a
/// refusal rather than a write through to whatever it points at. The mode is
/// `0o600` from the first instant the file exists, not after the fact.
fn create_temporary(destination: &Path) -> Result<(PathBuf, std::fs::File), SignApplyError> {
    let directory = destination.parent().unwrap_or_else(|| Path::new("."));
    let directory = if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    };

    let mut last_error = None;
    for _ in 0..32 {
        let path = directory.join(temporary_name());
        match open_exclusive(&path) {
            Ok(file) => return Ok((path, file)),
            // The name was taken: draw another one rather than touch it.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_error = Some(e),
            Err(e) => return Err(SignApplyError::Io { path, source: e }),
        }
    }

    Err(SignApplyError::Io {
        path: directory.to_path_buf(),
        source: last_error.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "no free temporary file name",
            )
        }),
    })
}

/// Write through the handle that was created, never by reopening the path:
/// reopening would hand the window back to whoever can write the directory.
fn write_and_sync(
    path: &Path,
    file: &mut std::fs::File,
    bytes: &[u8],
) -> Result<(), SignApplyError> {
    let mut write = || -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(path);
        SignApplyError::Io {
            path: path.to_path_buf(),
            source: e,
        }
    })
}

// --- Reading the source ---------------------------------------------------

fn entry<'a>(entries: &'a [(String, PdfObject)], key: &str) -> Option<&'a PdfObject> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Set a key, in place when it already exists so that the dictionary's
/// original order survives the round trip.
fn set_entry(entries: &mut Vec<(String, PdfObject)>, key: &str, value: PdfObject) {
    match entries.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => entries.push((key.to_string(), value)),
    }
}

/// Parse the dictionary of object `obj_num` as it stands in the newest
/// revision that defines it.
fn parse_object_dictionary(
    bytes: &[u8],
    obj_num: u32,
) -> Result<Vec<(String, PdfObject)>, SignApplyError> {
    parse_object_dictionary_with(&crate::verify::ObjectResolver::new(bytes), obj_num)
}

/// [`parse_object_dictionary`], against a resolver the caller already built.
///
/// Building an `ObjectResolver` re-parses the whole cross-reference chain, so
/// a loop over the AcroForm's fields must build one, not one per field.
fn parse_object_dictionary_with(
    resolver: &crate::verify::ObjectResolver<'_>,
    obj_num: u32,
) -> Result<Vec<(String, PdfObject)>, SignApplyError> {
    // Through the resolution layer, so that a dictionary packed into a
    // /Type /ObjStm reads exactly like one at its own file offset.
    let (definition, _) = resolver.object_bytes(obj_num)?;
    let tokens = tokenize(&definition)?;
    // Skip the "N 0 obj" header.
    let start = tokens
        .iter()
        .position(|t| t == b"obj")
        .map(|p| p + 1)
        .unwrap_or(0);
    if tokens.get(start).map(Vec::as_slice) != Some(b"<<".as_slice()) {
        return Err(SignApplyError::Unsupported(format!(
            "object {obj_num} is not a dictionary; this document cannot be signed by pulpit"
        )));
    }
    let (object, _) = parse_value(&tokens, start, 0)?;
    match object {
        PdfObject::Dictionary(entries) => Ok(entries),
        _ => Err(SignApplyError::Unsupported(format!(
            "object {obj_num} is not a dictionary"
        ))),
    }
}

fn tokenize(slice: &[u8]) -> Result<Vec<Vec<u8>>, SignApplyError> {
    let mut tokenizer = PdfTokenizer::new(slice);
    let mut tokens = Vec::new();
    while let Some(token) = tokenizer.next_token()? {
        if token == b"endobj" || token == b"stream" {
            break;
        }
        tokens.push(token);
    }
    Ok(tokens)
}

/// Maximum nesting depth of a parsed value (FINDING #4: guard against unbounded recursion).
const MAX_VALUE_DEPTH: usize = 64;

/// Parse one value starting at `index`, returning it and the index just past
/// it. Streams are not handled: none of the objects this module re-emits —
/// catalog, AcroForm, page node, field — carries one.
///
/// The `depth` parameter guards against pathological nesting (FINDING #4).
fn parse_value(
    tokens: &[Vec<u8>],
    index: usize,
    depth: usize,
) -> Result<(PdfObject, usize), SignApplyError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(SignApplyError::Unsupported(
            "value nesting too deep".to_string(),
        ));
    }

    let malformed =
        || SignApplyError::Unsupported("the document's object structure is malformed".to_string());
    let token = tokens.get(index).ok_or_else(malformed)?;

    if token.as_slice() == b"<<" {
        let mut entries = Vec::new();
        let mut i = index + 1;
        loop {
            let key = tokens.get(i).ok_or_else(malformed)?;
            if key.as_slice() == b">>" {
                return Ok((PdfObject::Dictionary(entries), i + 1));
            }
            let name = decode_pdf_name(key).ok_or_else(malformed)?;
            let (value, next) = parse_value(tokens, i + 1, depth + 1)?;
            entries.push((name, value));
            i = next;
        }
    }

    if token.as_slice() == b"[" {
        let mut items = Vec::new();
        let mut i = index + 1;
        loop {
            let next = tokens.get(i).ok_or_else(malformed)?;
            if next.as_slice() == b"]" {
                return Ok((PdfObject::Array(items), i + 1));
            }
            let (value, after) = parse_value(tokens, i, depth + 1)?;
            items.push(value);
            i = after;
        }
    }

    // FINDING #5: Preserve raw bytes for string values instead of lossy UTF-8
    // conversion. For literal strings (starting with '('), extract the raw bytes
    // between '(' and ')' without converting to string first.
    if !token.is_empty() && token[0] == b'(' {
        // Literal string: extract raw bytes between '(' and ')'
        let mut string_bytes = Vec::new();
        let mut i = 1usize;
        let mut paren_depth = 1usize;
        while i < token.len() {
            match token[i] {
                b'\\' if i + 1 < token.len() => {
                    // Escape sequence: keep the backslash and the next byte
                    string_bytes.push(token[i]);
                    string_bytes.push(token[i + 1]);
                    i += 2;
                }
                b'(' => {
                    paren_depth += 1;
                    string_bytes.push(token[i]);
                    i += 1;
                }
                b')' => {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        // End of string
                        return Ok((
                            PdfObject::RawString(unescape_bytes(&string_bytes)),
                            index + 1,
                        ));
                    }
                    string_bytes.push(token[i]);
                    i += 1;
                }
                _ => {
                    string_bytes.push(token[i]);
                    i += 1;
                }
            }
        }
        // Unterminated string: treat the whole token as the string
        return Ok((PdfObject::String(unescape_bytes(&string_bytes)), index + 1));
    }

    if token.starts_with(b"/") {
        let name = decode_pdf_name(token).ok_or_else(malformed)?;
        return Ok((PdfObject::Name(name), index + 1));
    }
    // Names and strings have already taken their byte-preserving paths. The
    // remaining token kinds are ASCII PDF syntax and numbers.
    let text = std::str::from_utf8(token)
        .map_err(|_| malformed())?
        .to_string();
    if text.starts_with('<') {
        let inner: String = text
            .trim_start_matches('<')
            .trim_end_matches('>')
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .collect();
        let bytes = (0..inner.len() / 2)
            .filter_map(|i| u8::from_str_radix(&inner[i * 2..i * 2 + 2], 16).ok())
            .collect();
        return Ok((PdfObject::HexString(bytes), index + 1));
    }
    match text.as_str() {
        "true" => return Ok((PdfObject::Boolean(true), index + 1)),
        "false" => return Ok((PdfObject::Boolean(false), index + 1)),
        "null" => return Ok((PdfObject::Null, index + 1)),
        _ => {}
    }
    if let Ok(number) = text.parse::<i64>() {
        // "n g R" is an indirect reference; anything else is just a number.
        let is_reference = tokens
            .get(index + 1)
            .and_then(|t| std::str::from_utf8(t).ok())
            .and_then(|s| s.parse::<u16>().ok())
            .is_some()
            && tokens.get(index + 2).map(Vec::as_slice) == Some(b"R".as_slice());
        if is_reference && number >= 0 {
            let gen_num = String::from_utf8_lossy(&tokens[index + 1])
                .parse::<u16>()
                .unwrap_or(0);
            return Ok((
                PdfObject::IndirectRef {
                    obj_num: number as u32,
                    gen_num,
                },
                index + 3,
            ));
        }
        return Ok((PdfObject::Integer(number), index + 1));
    }
    if let Ok(real) = text.parse::<f64>() {
        return Ok((PdfObject::Real(real), index + 1));
    }
    Err(malformed())
}

/// Decode one PDF name token, including `#xx` byte escapes.
///
/// The writer escapes these bytes again. Decoding here prevents an existing
/// `/A#20B` from becoming `/A#2320B` when a containing dictionary is signed
/// and re-emitted. The current object model represents names as UTF-8, so an
/// arbitrary-byte name is refused instead of silently replaced.
fn decode_pdf_name(token: &[u8]) -> Option<String> {
    let bytes = token.strip_prefix(b"/")?;
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'#' {
            let pair = bytes.get(index + 1..index + 3)?;
            let hex = std::str::from_utf8(pair).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[allow(dead_code)]
fn unescape(text: &str) -> Vec<u8> {
    unescape_bytes(text.as_bytes())
}

/// Unescape PDF literal string bytes, preserving non-ASCII bytes (§7.3.4.2).
///
/// Shared with pre-flight and verification. Dropping the backslash and
/// keeping the byte after it — which is what this did — is right for `\(`,
/// `\)` and `\\` and wrong for everything else: `\351` became the three
/// characters `351` rather than the byte `0xE9`, so a name written with
/// octal escapes never matched. Acrobat writes UTF-16BE names exactly that
/// way, since almost every byte of one is unprintable.
fn unescape_bytes(bytes: &[u8]) -> Vec<u8> {
    crate::pdftext::unescape_literal(bytes)
}

/// Maximum depth of the `/Kids` walk, and the number of nodes it may visit.
/// Both are guards against a malformed or hostile page tree, not real limits.
const MAX_PAGE_TREE_DEPTH: usize = 64;
const MAX_PAGE_TREE_NODES: usize = 50_000;

/// The object numbers of the document's page leaves, in document order.
///
/// A leaf is a node with no `/Kids` array; `/Type` is not trusted, because a
/// producer that omits it still expects the node to be a page. The walk is
/// bounded in depth and node count and refuses to revisit a node, so a cycle
/// is a refusal rather than a hang.
fn page_objects(
    bytes: &[u8],
    catalog_entries: &[(String, PdfObject)],
) -> Result<Vec<u32>, SignApplyError> {
    let root = match entry(catalog_entries, "Pages") {
        Some(PdfObject::IndirectRef { obj_num, .. }) => *obj_num,
        _ => {
            return Err(SignApplyError::Unsupported(
                "the catalog has no indirect /Pages tree".into(),
            ))
        }
    };

    let mut pages = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![(root, 0usize)];
    let mut budget = MAX_PAGE_TREE_NODES;
    // One resolver for the walk: building one rebuilds the whole
    // cross-reference chain, and this loop visits up to MAX_PAGE_TREE_NODES
    // nodes.
    let resolver = crate::verify::ObjectResolver::new(bytes);
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_PAGE_TREE_DEPTH {
            return Err(SignApplyError::Unsupported(format!(
                "the page tree is deeper than {MAX_PAGE_TREE_DEPTH} levels"
            )));
        }
        if budget == 0 {
            return Err(SignApplyError::Unsupported(format!(
                "the page tree has more than {MAX_PAGE_TREE_NODES} nodes"
            )));
        }
        budget -= 1;
        if !visited.insert(node) {
            // A node reachable twice is a cycle or a shared subtree; either
            // way it has already contributed its pages.
            continue;
        }
        let entries = parse_object_dictionary_with(&resolver, node)?;
        match entry(&entries, "Kids") {
            Some(PdfObject::Array(kids)) => {
                for kid in kids.iter().rev() {
                    if let PdfObject::IndirectRef { obj_num, .. } = kid {
                        stack.push((*obj_num, depth + 1));
                    }
                }
            }
            _ => pages.push(node),
        }
    }
    Ok(pages)
}

/// The object number of page `index`, zero-based.
fn page_object_at(
    bytes: &[u8],
    catalog_entries: &[(String, PdfObject)],
    index: usize,
) -> Result<u32, SignApplyError> {
    let pages = page_objects(bytes, catalog_entries)?;
    pages
        .get(index)
        .copied()
        .ok_or_else(|| out_of_range(index, pages.len()))
}

/// Check that `index` names a page this document has, without caring which
/// object it is — the existing-field path needs the refusal, not the page.
fn check_page_index(bytes: &[u8], index: usize) -> Result<(), SignApplyError> {
    let catalog = find_catalog_ref(bytes)?;
    let catalog_entries = parse_object_dictionary(bytes, catalog.0)?;
    let pages = page_objects(bytes, &catalog_entries)?;
    if index >= pages.len() {
        return Err(out_of_range(index, pages.len()));
    }
    Ok(())
}

fn out_of_range(index: usize, page_count: usize) -> SignApplyError {
    let advice = if page_count == 0 {
        "the source is unchanged — this document has no pages at all, so it cannot carry a \
         visible signature; sign it without an appearance"
            .to_string()
    } else {
        format!(
            "the source is unchanged — use a page index between 0 and {}, or sign without an \
             appearance",
            page_count - 1
        )
    };
    SignApplyError::AppearancePlacement(format!(
        "the appearance was requested on page index {index}, but the document has {page_count} \
         page(s); {advice}"
    ))
}

/// A field's `/T`, decoded, whichever string variant it parsed to.
///
/// `/T` is a text string: PDFDocEncoding, or UTF-16BE behind a byte-order
/// mark, and a producer may write either as a literal `(…)` or as a hex
/// `<…>` string. The two spellings parse to different variants here, so
/// matching only `RawString` missed a hex-written name entirely — including
/// the UTF-16BE names Acrobat gives any field not named in plain ASCII.
///
/// The decode itself is [`crate::pdftext`], shared with pre-flight and
/// verification: name matching is only meaningful if every path agrees on
/// what a field is called.
fn field_name_of(entries: &[(String, PdfObject)]) -> Option<String> {
    match entry(entries, "T") {
        Some(PdfObject::RawString(bytes)) | Some(PdfObject::HexString(bytes)) => {
            Some(crate::pdftext::decode_text_string(bytes))
        }
        _ => None,
    }
}

/// The object number of the field named `name`, if the AcroForm lists it.
fn find_field_object(bytes: &[u8], name: &str) -> Result<Option<u32>, SignApplyError> {
    let resolver = crate::verify::ObjectResolver::new(bytes);
    let catalog = crate::verify::find_catalog_ref_with(&resolver, bytes)?;
    for (obj_num, _) in crate::verify::find_fields_array_with(&resolver, catalog)? {
        let entries = parse_object_dictionary_with(&resolver, obj_num)?;
        // Compare decoded names: the caller's `name` is UTF-8 from the
        // application, the document's is not.
        if field_name_of(&entries).as_deref() == Some(name) {
            return Ok(Some(obj_num));
        }
    }
    Ok(None)
}

fn existing_field_names(bytes: &[u8]) -> Result<Vec<String>, SignApplyError> {
    let resolver = crate::verify::ObjectResolver::new(bytes);
    let catalog = crate::verify::find_catalog_ref_with(&resolver, bytes)?;
    let mut names = Vec::new();
    for (obj_num, _) in crate::verify::find_fields_array_with(&resolver, catalog)? {
        let entries = parse_object_dictionary_with(&resolver, obj_num)?;
        // `/T` is document bytes, not our own text. Matching `String` here
        // once silently matched nothing, so this returned an empty list for
        // every document and both collision checks in `plan_revision` went
        // dead — an existing name was no longer refused and the auto-namer
        // always picked `Signature1`, producing two fields sharing a `/T`.
        if let Some(name) = field_name_of(&entries) {
            names.push(name);
        }
    }
    Ok(names)
}

// --- Dates ----------------------------------------------------------------

/// `D:YYYYMMDDHHmmSS+00'00'` (§25.3). Always UTC: the caller hands over unix
/// seconds and this crate has no notion of a local zone.
fn pdf_date(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}+00'00'",
        year,
        month,
        day,
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// Howard Hinnant's `civil_from_days`, with the era shifted so that day 0 is
/// 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_date_matches_the_spec_form() {
        assert_eq!(pdf_date(1_724_191_200), "D:20240820220000+00'00'");
        assert_eq!(pdf_date(0), "D:19700101000000+00'00'");
        // A leap day, which is where a hand-rolled calendar goes wrong.
        assert_eq!(pdf_date(1_709_164_800), "D:20240229000000+00'00'");
    }

    #[test]
    fn parsing_a_dictionary_keeps_references_apart_from_numbers() {
        let source =
            b"7 0 obj\n<< /Type /Page /Parent 2 0 R /Count 3 /Rect [0 0 612 792] >>\nendobj";
        let tokens = tokenize(source).unwrap();
        let start = tokens.iter().position(|t| t == b"obj").unwrap() + 1;
        let (object, _) = parse_value(&tokens, start, 0).unwrap();
        let PdfObject::Dictionary(entries) = object else {
            panic!("expected a dictionary");
        };
        assert!(matches!(
            entry(&entries, "Parent"),
            Some(PdfObject::IndirectRef { obj_num: 2, .. })
        ));
        assert!(matches!(
            entry(&entries, "Count"),
            Some(PdfObject::Integer(3))
        ));
        assert!(matches!(entry(&entries, "Rect"), Some(PdfObject::Array(a)) if a.len() == 4));
    }

    #[test]
    fn escaped_pdf_names_are_decoded_once_and_reemitted_canonically() {
        let tokens = vec![
            b"<<".to_vec(),
            b"/Key#20With#23Escapes".to_vec(),
            b"/Value#2FPart".to_vec(),
            b">>".to_vec(),
        ];
        let (object, _) = parse_value(&tokens, 0, 0).expect("dictionary parses");
        let PdfObject::Dictionary(entries) = &object else {
            panic!("expected a dictionary");
        };
        assert_eq!(entries[0].0, "Key With#Escapes");
        assert!(matches!(
            &entries[0].1,
            PdfObject::Name(name) if name == "Value/Part"
        ));

        let mut emitted = Vec::new();
        object.serialize(&mut emitted).expect("dictionary emits");
        let emitted = String::from_utf8(emitted).expect("PDF syntax is ASCII");
        assert!(emitted.contains("/Key#20With#23Escapes"), "{emitted}");
        assert!(emitted.contains("/Value#2FPart"), "{emitted}");
        assert!(!emitted.contains("#2320"), "{emitted}");
    }

    #[test]
    fn setting_a_key_that_exists_keeps_its_position() {
        let mut entries = vec![
            ("Type".to_string(), PdfObject::Name("Catalog".into())),
            ("Pages".to_string(), PdfObject::Integer(1)),
        ];
        set_entry(&mut entries, "Type", PdfObject::Name("Other".into()));
        set_entry(&mut entries, "AcroForm", PdfObject::Integer(9));
        assert_eq!(entries[0].0, "Type");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2].0, "AcroForm");
    }

    // --- The temporary file and the in-place guard ------------------------

    #[test]
    fn a_planted_file_at_the_temporary_name_is_not_truncated() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let planted = directory.path().join(".pulpit-sign-planted");
        std::fs::write(&planted, b"victim contents").expect("plant a file");

        let error = open_exclusive(&planted).expect_err("O_EXCL refuses an existing name");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&planted).expect("read back"),
            b"victim contents",
            "the planted file must not have been truncated"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_at_the_temporary_name_is_not_followed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let victim = directory.path().join("victim");
        std::fs::write(&victim, b"victim contents").expect("write the victim");
        let planted = directory.path().join(".pulpit-sign-planted");
        std::os::unix::fs::symlink(&victim, &planted).expect("plant a symlink");

        let error = open_exclusive(&planted).expect_err("O_EXCL refuses a symlink");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&victim).expect("read back"),
            b"victim contents",
            "the symlink's target must not have been written through"
        );
    }

    #[test]
    fn the_temporary_file_is_fresh_private_and_unpredictable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let destination = directory.path().join("signed.pdf");

        let (first, _first_file) = create_temporary(&destination).expect("create a temporary");
        let (second, _second_file) = create_temporary(&destination).expect("create another");

        assert_ne!(first, second, "two temporaries must not share a name");
        assert!(first.exists() && second.exists());
        assert_eq!(first.parent(), Some(directory.path()));
        assert_eq!(std::fs::read(&first).expect("read back"), b"");

        // The name must not be derivable from the pid and a counter alone.
        let predictable = format!(".pulpit-sign-{}-0", std::process::id());
        assert_ne!(first.file_name().unwrap().to_string_lossy(), predictable);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first)
                .expect("stat")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the candidate must be private");
        }
    }

    #[test]
    fn signing_a_file_onto_itself_is_refused() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let source = directory.path().join("deck.pdf");
        std::fs::write(&source, b"%PDF-1.4\n").expect("write the source");

        // The same path, spelled three ways that all resolve to one file.
        let indirect = directory.path().join(".").join("deck.pdf");
        #[cfg(unix)]
        let link = directory.path().join("link.pdf");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, &link).expect("plant a symlink");

        assert!(is_same_file(&source, &source).expect("resolve"));
        assert!(is_same_file(&source, &indirect).expect("resolve"));
        #[cfg(unix)]
        assert!(is_same_file(&source, &link).expect("resolve"));

        // A destination that does not exist yet is still compared correctly.
        let fresh = directory.path().join("signed.pdf");
        assert!(!is_same_file(&source, &fresh).expect("resolve"));
    }

    #[test]
    fn a_shorter_output_is_refused_by_the_gate() {
        let error = verification_gate(b"0123456789", b"01234", 0, "Signature1").unwrap_err();
        assert!(matches!(
            error,
            SignApplyError::PostSignVerificationFailed { .. }
        ));
    }

    #[test]
    fn changed_leading_bytes_are_refused_by_the_gate() {
        let error = verification_gate(b"0123456789", b"012456789abc", 0, "Signature1").unwrap_err();
        assert!(matches!(
            error,
            SignApplyError::PostSignVerificationFailed { .. }
        ));
    }

    // --- Regression tests for HIGH-severity findings ---------------------

    /// FINDING #4: Deeply nested arrays should error cleanly, not overflow stack.
    #[test]
    fn deeply_nested_array_errors_cleanly() {
        // Construct tokens representing deeply nested arrays: [[[[[...
        let mut tokens = vec![];
        for _ in 0..MAX_VALUE_DEPTH + 10 {
            tokens.push(b"[".to_vec());
        }
        tokens.push(b"42".to_vec());
        for _ in 0..MAX_VALUE_DEPTH + 10 {
            tokens.push(b"]".to_vec());
        }

        let result = parse_value(&tokens, 0, 0);
        assert!(
            result.is_err(),
            "deeply nested value should error, not overflow"
        );
        match result {
            Err(SignApplyError::Unsupported(msg)) if msg.contains("nesting too deep") => {
                // Expected
            }
            other => panic!("expected nesting depth error, got {:?}", other),
        }
    }

    /// FINDING #5: Non-ASCII string bytes should survive parse/re-emit round trip.
    /// Tests that `(Café)` with raw byte 0xE9 is preserved, not converted to replacement char.
    #[test]
    fn non_ascii_string_bytes_preserved_in_parse() {
        // Create tokens for a literal string containing non-ASCII bytes.
        // Représent (Café) where é is 0xE9 (PDFDocEncoding).
        let cafe_bytes = b"(Caf\xE9)".to_vec();
        let tokens = vec![cafe_bytes];

        let (object, _) = parse_value(&tokens, 0, 0).expect("parse should succeed");
        match object {
            PdfObject::RawString(bytes) => {
                // The string content (without the parentheses) should be [C, a, f, 0xE9]
                assert_eq!(bytes, b"Caf\xE9", "non-ASCII bytes must be preserved");
            }
            other => panic!("expected PdfObject::String, got {:?}", other),
        }
    }

    /// The name of the field in `/T` must come out the same however the
    /// producer spelled the string, because `find_field_object` compares it
    /// against a UTF-8 name from the application and `existing_field_names`
    /// uses it for both collision checks in `plan_revision`.
    ///
    /// Two ways this went wrong. `existing_field_names` was left matching
    /// `String` when the string variants were split; the arm matched nothing,
    /// it returned an empty list for every document, and both collision
    /// checks silently went dead — the auto-namer always chose `Signature1`
    /// and an explicit duplicate was never refused, so pulpit would sign a
    /// document carrying two fields with one `/T`. Later, matching only
    /// `RawString` missed a name written as a hex string, and unescaping that
    /// dropped only the backslashes, so `\376\377` — how Acrobat writes the
    /// UTF-16BE byte-order mark — never became a byte-order mark at all.
    #[test]
    fn a_field_name_decodes_the_same_from_every_spelling() {
        // A literal string in PDFDocEncoding.
        assert_eq!(
            name_from_token(&b"(Caf\xE9 Sig)"[..]).as_deref(),
            Some("Café Sig")
        );
        // The same name as a UTF-16BE literal written with octal escapes,
        // which is what Acrobat produces.
        assert_eq!(
            name_from_token(
                &b"(\\376\\377\\000C\\000a\\000f\\000\\351\\000 \\000S\\000i\\000g)"[..]
            )
            .as_deref(),
            Some("Café Sig")
        );
        // And as a UTF-16BE hex string.
        assert_eq!(
            name_from_token(&b"<FEFF00430061006600E90020005300690067>"[..]).as_deref(),
            Some("Café Sig")
        );
    }

    /// Parse `<< /T token /FT /Sig >>` and read the name back out the way
    /// `find_field_object` and `existing_field_names` do.
    fn name_from_token(token: &[u8]) -> Option<String> {
        let tokens: Vec<Vec<u8>> = [
            &b"<<"[..],
            &b"/T"[..],
            token,
            &b"/FT"[..],
            &b"/Sig"[..],
            &b">>"[..],
        ]
        .iter()
        .map(|t| t.to_vec())
        .collect();
        let (object, _) = parse_value(&tokens, 0, 0).expect("the dictionary parses");
        let PdfObject::Dictionary(entries) = object else {
            panic!("expected a dictionary");
        };
        field_name_of(&entries)
    }

    /// FINDING #4: Deeply nested dictionaries should also error cleanly.
    #[test]
    fn deeply_nested_dictionary_errors_cleanly() {
        // Construct tokens representing deeply nested dicts: <<</A<<
        let mut tokens = vec![];
        for _ in 0..MAX_VALUE_DEPTH + 10 {
            tokens.push(b"<<".to_vec());
            tokens.push(b"/A".to_vec());
        }
        tokens.push(b"1".to_vec()); // inner value
        for _ in 0..MAX_VALUE_DEPTH + 10 {
            tokens.push(b">>".to_vec());
        }

        let result = parse_value(&tokens, 0, 0);
        assert!(
            result.is_err(),
            "deeply nested dictionary should error, not overflow"
        );
        match result {
            Err(SignApplyError::Unsupported(msg)) if msg.contains("nesting too deep") => {
                // Expected
            }
            other => panic!("expected nesting depth error, got {:?}", other),
        }
    }
}
