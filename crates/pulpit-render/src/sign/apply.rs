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
    self, find_catalog_ref, find_fields_array, find_object, SignatureCoverage,
    SignatureVerification, VerifyError,
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
    /// The widget's rect in PDF page coordinates: `[x0, y0, x1, y1]`.
    pub rect: [f64; 4],
    /// Which page carries the widget. Only `0` is currently supported;
    /// anything else is a typed [`SignApplyError::Unsupported`], never a
    /// silently wrong page.
    pub page_index: usize,
    pub content: AppearanceContent,
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
    let temporary = temporary_path(destination);
    write_and_sync(&temporary, &candidate)?;

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
    let writer = IncrementalWriter::open(source)?;
    let mut next_object = writer.next_object_number();
    let allocate = |next_object: &mut u32| {
        let n = *next_object;
        *next_object += 1;
        n
    };

    if let Some(appearance) = &request.appearance {
        if appearance.page_index != 0 {
            return Err(SignApplyError::Unsupported(format!(
                "signature appearance requested on page_index {}, but only page_index 0 \
                 is currently supported for visible signature appearances",
                appearance.page_index
            )));
        }
    }

    let signature_object = allocate(&mut next_object);
    let appearance_object = request
        .appearance
        .as_ref()
        .map(|_| allocate(&mut next_object));
    let mut objects: Vec<(u32, u16, PdfObject)> = Vec::new();

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
                apply_appearance_to_widget(&mut entries, appearance, xobject_num);
            }
            objects.push((field_object, 0, PdfObject::Dictionary(entries)));
        }
        None => {
            let catalog = find_catalog_ref(source)?;
            let mut catalog_entries = parse_object_dictionary(source, catalog.0)?;
            let page_object = first_page_object(source, &catalog_entries)?;
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
                apply_appearance_to_widget(&mut field_entries, appearance, xobject_num);
            }

            objects.push((catalog.0, 0, PdfObject::Dictionary(catalog_entries)));
            objects.push((acroform_object, 0, PdfObject::Dictionary(acroform_entries)));
            objects.push((page_object, 0, PdfObject::Dictionary(page_entries)));
            objects.push((field_object, 0, PdfObject::Dictionary(field_entries)));
        }
    }

    if let (Some(appearance), Some(xobject_num)) = (&request.appearance, appearance_object) {
        objects.push((
            xobject_num,
            0,
            PdfObject::Raw(build_appearance_xobject(appearance)),
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

/// Set `/Rect` to the appearance's rect, point `/AP /N` at the freshly
/// appended form XObject, and delete `/AS` if present — required whenever a
/// widget carries an appearance stream (§25.5).
fn apply_appearance_to_widget(
    entries: &mut Vec<(String, PdfObject)>,
    appearance: &SignAppearance,
    xobject_num: u32,
) {
    set_entry(
        entries,
        "Rect",
        PdfObject::Array(
            appearance
                .rect
                .iter()
                .map(|v| PdfObject::Real(*v))
                .collect(),
        ),
    );
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
fn build_appearance_xobject(appearance: &SignAppearance) -> Vec<u8> {
    let width = appearance.rect[2] - appearance.rect[0];
    let height = appearance.rect[3] - appearance.rect[1];
    let (content, has_text) = appearance_content_stream(&appearance.content, width, height);

    let mut dict = Vec::new();
    dict.extend_from_slice(b"<< /Type /XObject /Subtype /Form /BBox [0 0 ");
    dict.extend_from_slice(fmt_num(width).as_bytes());
    dict.push(b' ');
    dict.extend_from_slice(fmt_num(height).as_bytes());
    dict.extend_from_slice(b"] /Resources <<");
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

/// A hidden temporary file in the destination's own directory, named the way
/// `pdf::pdfium::write_atomically` names its own, so that the two leave the
/// same debris behind if a process dies mid-write.
fn temporary_path(destination: &Path) -> PathBuf {
    let directory = destination.parent().unwrap_or_else(|| Path::new("."));
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    directory.join(format!(".pulpit-sign-{}-{ticket}", std::process::id()))
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<(), SignApplyError> {
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(path)?;
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
    let slice = find_object(bytes, obj_num)?;
    let tokens = tokenize(slice)?;
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
    let (object, _) = parse_value(&tokens, start)?;
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

/// Parse one value starting at `index`, returning it and the index just past
/// it. Streams are not handled: none of the objects this module re-emits —
/// catalog, AcroForm, page node, field — carries one.
fn parse_value(tokens: &[Vec<u8>], index: usize) -> Result<(PdfObject, usize), SignApplyError> {
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
            let name = std::str::from_utf8(key)
                .ok()
                .and_then(|s| s.strip_prefix('/'))
                .ok_or_else(malformed)?
                .to_string();
            let (value, next) = parse_value(tokens, i + 1)?;
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
            let (value, after) = parse_value(tokens, i)?;
            items.push(value);
            i = after;
        }
    }

    let text = String::from_utf8_lossy(token).to_string();

    if let Some(name) = text.strip_prefix('/') {
        return Ok((PdfObject::Name(name.to_string()), index + 1));
    }
    if text.starts_with('(') {
        let inner = text
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(&text);
        return Ok((PdfObject::String(unescape(inner)), index + 1));
    }
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

fn unescape(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut escaped = false;
    for byte in text.bytes() {
        if escaped {
            out.push(byte);
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else {
            out.push(byte);
        }
    }
    out
}

/// The object number of the document's first page.
fn first_page_object(
    bytes: &[u8],
    catalog_entries: &[(String, PdfObject)],
) -> Result<u32, SignApplyError> {
    let mut node = match entry(catalog_entries, "Pages") {
        Some(PdfObject::IndirectRef { obj_num, .. }) => *obj_num,
        _ => {
            return Err(SignApplyError::Unsupported(
                "the catalog has no indirect /Pages tree".into(),
            ))
        }
    };
    // Descend the leftmost spine. The depth bound is a guard against a cycle,
    // not a real limit: page trees this deep do not occur.
    for _ in 0..64 {
        let entries = parse_object_dictionary(bytes, node)?;
        let kids = match entry(&entries, "Kids") {
            Some(PdfObject::Array(kids)) => kids.clone(),
            _ => return Ok(node),
        };
        match kids.first() {
            Some(PdfObject::IndirectRef { obj_num, .. }) => node = *obj_num,
            _ => return Ok(node),
        }
    }
    Err(SignApplyError::Unsupported(
        "the page tree is deeper than 64 levels or contains a cycle".into(),
    ))
}

/// The object number of the field named `name`, if the AcroForm lists it.
fn find_field_object(bytes: &[u8], name: &str) -> Result<Option<u32>, SignApplyError> {
    let catalog = find_catalog_ref(bytes)?;
    for (obj_num, _) in find_fields_array(bytes, catalog)? {
        let entries = parse_object_dictionary(bytes, obj_num)?;
        if let Some(PdfObject::String(field_name)) = entry(&entries, "T") {
            if field_name.as_slice() == name.as_bytes() {
                return Ok(Some(obj_num));
            }
        }
    }
    Ok(None)
}

fn existing_field_names(bytes: &[u8]) -> Result<Vec<String>, SignApplyError> {
    let catalog = find_catalog_ref(bytes)?;
    let mut names = Vec::new();
    for (obj_num, _) in find_fields_array(bytes, catalog)? {
        let entries = parse_object_dictionary(bytes, obj_num)?;
        if let Some(PdfObject::String(field_name)) = entry(&entries, "T") {
            names.push(String::from_utf8_lossy(field_name).to_string());
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
        let (object, _) = parse_value(&tokens, start).unwrap();
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
}
