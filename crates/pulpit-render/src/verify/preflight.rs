#![forbid(unsafe_code)]

//! PDF pre-flight checks for signing as per SPEC-signing.md §25.4.
//!
//! Three explicit pre-flight checks must be performed before any byte is written:
//!
//! 1. **Certifying requires an unsigned document.** If any /Sig field carries a
//!    non-null /V, certification is refused with CertificationNotAllowed.
//!
//! 2. **Any signing requires that no prior certification forbids it.** If
//!    Root /Perms /DocMDP declares NO_CHANGES, signing is refused with
//!    DocumentLockedByPriorSignature.
//!
//! 3. **Filling a signature field requires that no prior signature's FieldMDP
//!    locks it.** For each existing signature, read its /Reference array and
//!    check /FieldMDP transforms. If any prior signature locks the target field,
//!    refuse with FieldLockedByPriorSignature.
//!
//! This module provides specialized READ-side checks on an opened document.
//! The sign module must not depend on preflight, and preflight must not depend on sign.

use super::objects::{self, PdfValue};
use super::{find_catalog_ref_with, find_field_tree_with, FieldEntry, MdpPerm, ObjectResolver};
use thiserror::Error;

/// Preflight check failures.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PreflightRefusal {
    #[error("Certification requires an unsigned document, but {existing_signatures} signature field(s) already exist")]
    CertificationNotAllowed { existing_signatures: usize },

    #[error(
        "Document is locked by prior certification with {level:?}; further signing is forbidden"
    )]
    DocumentLockedByPriorSignature { level: MdpPerm },

    #[error("Signature field '{field}' is locked by prior signature '{locked_by}'")]
    FieldLockedByPriorSignature { field: String, locked_by: String },

    #[error("Signature field '{field}' is already signed")]
    FieldAlreadySigned { field: String },

    /// The signing path appends plaintext objects and has no encryption layer
    /// (SPEC-signing §23.2: `/Contents` is never encrypted). Appending to an
    /// encrypted document therefore produces a file no reader will accept, so
    /// §35 Milestone S0 step 5 requires refusing it outright rather than
    /// half-supporting it.
    #[error("This document is encrypted, and pulpit cannot sign an encrypted document")]
    EncryptedDocument,

    #[error("No empty signature field exists in the document")]
    NoEmptySignatureField,

    #[error("Multiple empty signature fields exist; target is ambiguous: {candidates:?}")]
    AmbiguousSignatureField { candidates: Vec<String> },

    #[error("Signature field '{field}' does not exist")]
    NoSuchField { field: String },

    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// A construct this build cannot interpret was found in a place whose
    /// meaning decides whether signing is permitted. Signing must not
    /// proceed on a guess.
    #[error("Unsupported: {0}")]
    Unsupported(String),
}

/// Map a low-level read failure into a refusal. Every such failure means the
/// document's lock state could not be determined, and per the fail-closed
/// rule that blocks signing rather than permitting it.
fn undetermined(context: &str, e: impl std::fmt::Display) -> PreflightRefusal {
    PreflightRefusal::InvalidState(format!(
        "could not determine whether the document is locked: {context}: {e}"
    ))
}

/// Successful preflight result.
#[derive(Debug, Clone)]
pub struct PreflightOk {
    /// The name of the target signature field (either selected or auto-detected).
    pub target_field: String,

    /// Set if the target field carries a seed value dictionary.
    /// Per §25.4 last paragraph, this is a warning the caller may surface.
    pub seed_value_ignored: bool,
}

pub type Result<T> = std::result::Result<T, PreflightRefusal>;

/// The whole `/AcroForm` field tree both preflight passes read through —
/// refusing up front if the document is encrypted (no pass may proceed on
/// an encrypted document) or if the catalog cannot be found. Both callers
/// need exactly this, in exactly this order, before they diverge into their
/// own checks.
///
/// §76.6: this used to discard `find_field_tree_with`'s inheritance-resolved
/// [`FieldEntry`] and keep only object numbers, so a hierarchical field whose
/// `/FT` lives on an ancestor was invisible here even though
/// `verify::discover_signatures` saw it fine. Keeping the resolved entries
/// lets `extract_field_info` build `FieldInfo` from them directly instead of
/// re-tokenising each node in isolation.
fn fields_with(resolver: &ObjectResolver<'_>, bytes: &[u8]) -> Result<Vec<FieldEntry>> {
    if resolver.is_encrypted() {
        return Err(PreflightRefusal::EncryptedDocument);
    }

    let catalog_ref = find_catalog_ref_with(resolver, bytes)
        .map_err(|e| PreflightRefusal::InvalidState(format!("Failed to find catalog: {}", e)))?;

    find_field_tree_with(resolver, catalog_ref)
        .map_err(|e| PreflightRefusal::InvalidState(format!("Failed to find fields array: {}", e)))
}

/// Check 1: Certifying requires an unsigned document.
/// If any /Sig field with a non-null /V exists, refuse with CertificationNotAllowed.
pub fn preflight_certify(bytes: &[u8]) -> Result<()> {
    preflight_certify_with(&ObjectResolver::new(bytes), bytes)
}

/// [`preflight_certify`], against a resolver the caller already built.
///
/// §78.3: signing rebuilt a resolver — decoding the whole cross-reference
/// chain again — for roughly ten separate reads per pass; this is one of
/// them. `sign_document_file_inner` now builds one and passes it down.
pub fn preflight_certify_with(resolver: &ObjectResolver<'_>, bytes: &[u8]) -> Result<()> {
    let field_entries = fields_with(resolver, bytes)?;

    let mut signed_count = 0;
    for entry in &field_entries {
        // Fail closed: an entry that cannot be parsed might be a signature,
        // and certification requires proof that there is none.
        if let Some(field_info) = extract_field_info(resolver, entry)? {
            if field_info.field_type == "Sig" && field_info.has_signature {
                signed_count += 1;
            }
        }
    }

    if signed_count > 0 {
        Err(PreflightRefusal::CertificationNotAllowed {
            existing_signatures: signed_count,
        })
    } else {
        Ok(())
    }
}

/// Check 2 & 3: Pre-flight for signing.
/// - Checks that no prior certification declares NO_CHANGES.
/// - Checks that the target field is not locked by prior signature FieldMDP transforms.
/// - Returns the target field name and whether seed value dict exists.
pub fn preflight_sign(bytes: &[u8], target_field: Option<&str>) -> Result<PreflightOk> {
    preflight_sign_with(&ObjectResolver::new(bytes), bytes, target_field)
}

/// [`preflight_sign`], against a resolver the caller already built. See
/// [`preflight_certify_with`].
pub fn preflight_sign_with(
    resolver: &ObjectResolver<'_>,
    bytes: &[u8],
    target_field: Option<&str>,
) -> Result<PreflightOk> {
    // Refuse up front. Without this the run still fails safe — the §32 gate
    // cannot re-read the candidate, so nothing is promoted — but only after
    // writing one, and it reports "cannot re-read" rather than naming the
    // actual reason.
    let field_entries = fields_with(resolver, bytes)?;

    // Collect all field infos
    let mut field_infos = Vec::new();
    for entry in &field_entries {
        // Fail closed: an unparsable entry may be the prior signature whose
        // FieldMDP locks the target, so it must not be dropped.
        if let Some(info) = extract_field_info(resolver, entry)? {
            field_infos.push(info);
        }
    }

    // Check 2: Read Root /Perms /DocMDP level
    check_prior_docmdp(resolver, bytes)?;

    // If target_field is None, auto-detect empty signature field
    let target_field_name = if let Some(target) = target_field {
        // Verify that the named field exists
        if !field_infos.iter().any(|info| info.field_name == target) {
            return Err(PreflightRefusal::NoSuchField {
                field: target.to_string(),
            });
        }
        target.to_string()
    } else {
        // Auto-detect: find empty /Sig fields
        let empty_sig_fields: Vec<String> = field_infos
            .iter()
            .filter(|info| info.field_type == "Sig" && !info.has_signature)
            .map(|info| info.field_name.clone())
            .collect();

        match empty_sig_fields.len() {
            0 => return Err(PreflightRefusal::NoEmptySignatureField),
            1 => empty_sig_fields.into_iter().next().unwrap(),
            _ => {
                return Err(PreflightRefusal::AmbiguousSignatureField {
                    candidates: empty_sig_fields,
                })
            }
        }
    };

    // Get the field info for target
    let target_info = field_infos
        .iter()
        .find(|info| info.field_name == target_field_name)
        .cloned()
        .ok_or_else(|| PreflightRefusal::NoSuchField {
            field: target_field_name.clone(),
        })?;

    // Field must be a /Sig field
    if target_info.field_type != "Sig" {
        return Err(PreflightRefusal::InvalidState(format!(
            "Field '{}' is not a signature field",
            target_field_name
        )));
    }

    // Field must not be already signed
    if target_info.has_signature {
        return Err(PreflightRefusal::FieldAlreadySigned {
            field: target_field_name.clone(),
        });
    }

    // Check 3: For each existing signature, check if its FieldMDP locks this field
    check_field_mdp_locks(resolver, &target_field_name, &field_infos)?;

    // Check for /SV seed value dictionary (warning, not refusal)
    let seed_value_ignored = target_info.has_seed_value;

    Ok(PreflightOk {
        target_field: target_field_name,
        seed_value_ignored,
    })
}

/// Field information extracted during preflight.
#[derive(Debug, Clone)]
struct FieldInfo {
    field_name: String,
    field_type: String,
    has_signature: bool,
    has_seed_value: bool,
    /// The signature dictionary `/V` points at, inherited like everything
    /// else `FieldEntry` resolves. `check_field_mdp_locks` reads a signed
    /// field's `/Reference` array through this rather than re-deriving it.
    value_ref: Option<(u32, u16)>,
}

/// Build `FieldInfo` from a [`FieldEntry`] the tree walk already resolved
/// with inheritance.
///
/// §76.6: this used to re-tokenise each field's own dictionary in isolation,
/// which meant a `/Sig` whose `/FT` lived on a parent node (a legitimate,
/// hierarchical field) had no `/FT` of its own and was read as "not a field"
/// — invisible to `preflight_certify` and to the FieldMDP lock check below,
/// while `verify::discover_signatures` (which does walk the tree with
/// inheritance) saw it correctly. `field_type`, the qualified name and `/V`
/// now come straight from `entry`; the one thing `FieldEntry` does not carry
/// is `/SV`, so that alone still needs a read through the resolver.
///
/// Everything this sees was reached from the AcroForm field tree, so by
/// construction it is a form field. It may nevertheless carry `/Type /Annot
/// /Subtype /Widget`: merging the field dictionary with its single widget
/// annotation is permitted by §12.7.3.1 and is what Acrobat writes for
/// essentially every real form. An earlier version of this function set an
/// `is_annot_only` flag on `/Type /Annot` and auto-detection excluded those
/// entries — which excluded every field a real form has. What that flag was
/// reaching for, an entry that is not a field at all, is already caught
/// correctly below: a widget with no field attributes of its own has no
/// `/FT`, and a node with no `/FT` returns `None`.
fn extract_field_info(
    resolver: &ObjectResolver<'_>,
    entry: &FieldEntry,
) -> Result<Option<FieldInfo>> {
    // Resolved once, for two reasons: `/SV` is not inheritance-resolved by
    // `FieldEntry`, so it still needs a direct read; and its `Err` is also
    // how a node the tree walk could not read at all (a dangling reference,
    // a decode failure) is told apart from a node that resolved fine but
    // carries no `/FT` of its own or inherited — an ordinary widget-only
    // annotation, not a field. `FieldEntry::field_type` is `None` in both
    // cases, but only the first is "undetermined": the walk pushes a stub
    // entry rather than dropping a node precisely because it might be the
    // signature whose `/FT` this build could not read, and §25.4 requires
    // failing closed on that, not skipping it.
    let resolved = resolver.resolve(entry.obj.0);

    let Some(field_type) = entry.field_type.clone() else {
        if let Err(e) = resolved {
            return Err(undetermined(
                &format!("form field object {} could not be read", entry.obj.0),
                e,
            ));
        }
        return Ok(None);
    };

    let field_name = entry
        .qualified_name
        .clone()
        .unwrap_or_else(|| format!("Field_{}", entry.obj.0));
    let has_signature = entry.value_ref.is_some();

    // `/SV` only ever produces a warning, never a refusal, so a read failure
    // here (distinct from the one above: this node's `/FT` did resolve)
    // defaults to "not ignored" rather than blocking the field.
    let has_seed_value = resolved
        .ok()
        .and_then(|(value, _)| value.as_dict().cloned())
        .is_some_and(|dict| resolver.dict_get(&dict, "SV").is_some());

    Ok(Some(FieldInfo {
        field_name,
        field_type,
        has_signature,
        has_seed_value,
        value_ref: entry.value_ref,
    }))
}

/// Check that no prior certification declares NO_CHANGES (Check 2).
fn check_prior_docmdp(resolver: &ObjectResolver<'_>, bytes: &[u8]) -> Result<()> {
    // Read Root /Perms /DocMDP reference. A read failure here is not
    // "unlocked": it is "unknown", and unknown blocks signing.
    if extract_docmdp_perm_level(resolver, bytes)? == Some(MdpPerm::NoChanges) {
        return Err(PreflightRefusal::DocumentLockedByPriorSignature {
            level: MdpPerm::NoChanges,
        });
    }

    Ok(())
}

/// Extract DocMDP permission level from Root /Perms /DocMDP.
fn extract_docmdp_perm_level(
    resolver: &ObjectResolver<'_>,
    bytes: &[u8],
) -> Result<Option<MdpPerm>> {
    let catalog_ref = find_catalog_ref_with(resolver, bytes)
        .map_err(|e| undetermined("the catalog could not be found", e))?;
    let (catalog_value, _) = resolver
        .resolve(catalog_ref.0)
        .map_err(|e| undetermined("the catalog object could not be read", e))?;
    let Some(catalog) = catalog_value.as_dict() else {
        return Ok(None);
    };

    // Root /Perms: absent means no certification. Present means its value
    // decides whether signing is permitted, so it must be understood — and,
    // unlike an ordinary lookup, an indirect /Perms that fails to resolve is
    // not "absent": `dict_get` would fold that failure into `None` here,
    // reading a dangling /Perms reference as "unlocked" instead of
    // "undetermined". `resolver.resolve` propagates the failure instead.
    let Some(perms_raw) = catalog.get("Perms") else {
        return Ok(None);
    };
    let perms_owned;
    let perms = match perms_raw {
        PdfValue::Dict(d) => d,
        PdfValue::Ref(n, _) => {
            let (v, _) = resolver
                .resolve(*n)
                .map_err(|e| undetermined("the /Perms dictionary could not be read", e))?;
            perms_owned = v;
            match perms_owned.as_dict() {
                Some(d) => d,
                None => {
                    return Err(PreflightRefusal::Unsupported(
                        "Root /Perms does not resolve to a dictionary; could not determine \
                         whether the document is locked"
                            .to_string(),
                    ))
                }
            }
        }
        _ => {
            return Err(PreflightRefusal::Unsupported(
                "Root /Perms is neither a dictionary nor an indirect reference to one; could \
                 not determine whether the document is locked"
                    .to_string(),
            ))
        }
    };

    let Some(docmdp) = perms.get("DocMDP") else {
        return Ok(None);
    };
    // §25.4: /DocMDP must be an indirect reference to the certifying
    // signature dictionary — its own /Reference array is what carries the
    // permission level, and only an indirect signature object has file
    // offsets a signature can cover.
    let Some((sig_dict_num, _gen)) = docmdp.as_ref_pair() else {
        return Err(PreflightRefusal::Unsupported(
            "/Perms /DocMDP is not an indirect reference to a signature dictionary; could not \
             determine whether the document is locked"
                .to_string(),
        ));
    };
    extract_docmdp_p_level(resolver, sig_dict_num)
}

/// Extract /P value from DocMDP signature dictionary.
fn extract_docmdp_p_level(
    resolver: &ObjectResolver<'_>,
    sig_dict_num: u32,
) -> Result<Option<MdpPerm>> {
    let (value, _) = resolver.resolve(sig_dict_num).map_err(|e| {
        undetermined(
            &format!("the DocMDP signature dictionary (object {sig_dict_num}) could not be read"),
            e,
        )
    })?;
    let Some(dict) = value.as_dict() else {
        return Ok(None);
    };

    let Some(sig_ref) = objects::parse_reference_array(resolver, dict)
        .into_iter()
        .find(|r| r.transform_method.as_deref() == Some("DocMDP"))
    else {
        return Ok(None);
    };
    let Some(p_value) = sig_ref.transform_params.as_ref().and_then(|tp| tp.get("P")) else {
        return Ok(None);
    };

    // /P decides whether signing is permitted at all. An unreadable or
    // out-of-range value is refused, never read as "no restriction".
    let level = p_value.as_i64().ok_or_else(|| {
        PreflightRefusal::InvalidState(
            "DocMDP /P is not a readable number; could not determine whether the document is \
             locked"
                .to_string(),
        )
    })?;
    match level {
        1 => Ok(Some(MdpPerm::NoChanges)),
        2 => Ok(Some(MdpPerm::FillForms)),
        3 => Ok(Some(MdpPerm::Annotate)),
        other => Err(PreflightRefusal::Unsupported(format!(
            "DocMDP /P value {other} is not one of 1, 2 or 3; could not determine whether the \
             document is locked"
        ))),
    }
}

/// Check that no prior signature's FieldMDP locks the target field (Check 3).
///
/// §76.6: `target_field` and `locked_by` are now the same fully qualified
/// names `find_field_tree_with` builds — ancestors' `/T` joined with `.` —
/// which is also how a `/Reference` array's `/Fields` names its targets. A
/// per-node name (the previous behaviour) never matched a qualified lock:
/// a `/Include`/`/Exclude` array naming `form.sig2` could not match a target
/// resolved only as `sig2`.
fn check_field_mdp_locks(
    resolver: &ObjectResolver<'_>,
    target_field: &str,
    field_infos: &[FieldInfo],
) -> Result<()> {
    // Collect all signed fields (existing signatures)
    let signed_fields: Vec<_> = field_infos
        .iter()
        .filter(|info| info.field_type == "Sig" && info.has_signature)
        .collect();

    for field_info in signed_fields {
        // `has_signature` is true, so a missing `/V` — even an inherited one
        // — means the transform cannot be read: refuse rather than treat
        // this field as unlocked.
        let Some((sig_dict_num, _gen)) = field_info.value_ref else {
            return Err(PreflightRefusal::InvalidState(format!(
                "signature field '{}' appears signed but carries no /V; could not determine \
                 whether it locks other fields",
                field_info.field_name
            )));
        };

        // Extract every FieldMDP lock from this signature's /Reference
        // array — it may carry more than one alongside a DocMDP entry — and
        // check the target against each on its own. A failure to read the
        // transform is not "no lock": it is "unknown", and unknown blocks.
        for (locked_fields, action) in extract_field_mdp_locks(resolver, sig_dict_num)? {
            if is_field_locked(&locked_fields, action, target_field) {
                return Err(PreflightRefusal::FieldLockedByPriorSignature {
                    field: target_field.to_string(),
                    locked_by: field_info.field_name.clone(),
                });
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldMdpAction {
    All,
    Include,
    Exclude,
}

/// Extract every FieldMDP lock a signature dictionary's /Reference array
/// carries.
///
/// §77.9 / §78.3: `/Reference` is an array of *independent* signature
/// reference dictionaries (§12.8.1 Table 253) — a document with a
/// DocMDP-certifying signature and an independent FieldMDP lock in the same
/// `/Reference` array is ordinary, not malformed. Each element is parsed by
/// [`objects::parse_reference_array`] and evaluated on its own, returning
/// every FieldMDP lock found rather than just one.
fn extract_field_mdp_locks(
    resolver: &ObjectResolver<'_>,
    sig_dict_num: u32,
) -> Result<Vec<(Vec<String>, FieldMdpAction)>> {
    let (value, _) = resolver.resolve(sig_dict_num).map_err(|e| {
        undetermined(
            &format!("signature dictionary object {sig_dict_num} could not be read"),
            e,
        )
    })?;
    let Some(dict) = value.as_dict() else {
        return Ok(Vec::new());
    };

    let mut locks = Vec::new();
    for sig_ref in objects::parse_reference_array(resolver, dict) {
        if sig_ref.transform_method.as_deref() != Some("FieldMDP") {
            continue;
        }
        let transform_params = sig_ref.transform_params.as_ref();

        // A FieldMDP transform with no readable /Action leaves the lock
        // undetermined; refuse rather than treat the field as free.
        let action_value = transform_params.and_then(|tp| tp.get("Action"));
        let action = match action_value.and_then(|v| v.as_name()) {
            Some("All") => FieldMdpAction::All,
            Some("Include") => FieldMdpAction::Include,
            Some("Exclude") => FieldMdpAction::Exclude,
            Some(other) => {
                return Err(PreflightRefusal::Unsupported(format!(
                    "FieldMDP /Action '{other}' is not one of /All, /Include or /Exclude; \
                     could not determine whether the field is locked"
                )))
            }
            None => {
                return Err(PreflightRefusal::Unsupported(
                    "a prior signature carries a /FieldMDP transform with no /Action; could \
                     not determine whether the field is locked"
                        .to_string(),
                ))
            }
        };

        // A locked field's name is a text string too, and it is compared
        // against the decoded target name. A non-string element contributes
        // nothing, the same way it did in the token-search reader.
        let locked_fields = transform_params
            .and_then(|tp| tp.get("Fields"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|f| match f {
                        PdfValue::Str(s) => Some(crate::pdftext::decode_text_string(s)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        locks.push((locked_fields, action));
    }
    Ok(locks)
}

/// Check if a field is locked by a FieldMDP transform.
fn is_field_locked(locked_fields: &[String], action: FieldMdpAction, target_field: &str) -> bool {
    match action {
        FieldMdpAction::All => {
            // /All locks all fields
            true
        }
        FieldMdpAction::Include => {
            // /Include locks only named fields
            locked_fields.iter().any(|f| f == target_field)
        }
        FieldMdpAction::Exclude => {
            // /Exclude locks all fields NOT named
            !locked_fields.iter().any(|f| f == target_field)
        }
    }
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

    #[test]
    fn test_preflight_certify_unsigned_doc() {
        // An unsigned document should pass certify check
        let pdf = create_minimal_unsigned_pdf();
        assert!(preflight_certify(&pdf).is_ok());
    }

    #[test]
    fn test_preflight_certify_signed_doc() {
        // A document with a signed field should fail certify check
        let pdf = create_pdf_with_signed_field();
        let result = preflight_certify(&pdf);
        assert!(matches!(
            result,
            Err(PreflightRefusal::CertificationNotAllowed {
                existing_signatures: 1
            })
        ));
    }

    #[test]
    fn test_preflight_sign_auto_detect_empty_field() {
        let pdf = create_pdf_with_empty_sig_field();
        let result = preflight_sign(&pdf, None);
        assert!(result.is_ok());
        if let Ok(ok) = result {
            assert_eq!(ok.target_field, "Sig1");
        }
    }

    #[test]
    fn test_preflight_sign_target_already_signed() {
        let pdf = create_pdf_with_signed_field();
        let result = preflight_sign(&pdf, Some("Sig1"));
        assert!(matches!(
            result,
            Err(PreflightRefusal::FieldAlreadySigned { .. })
        ));
    }

    #[test]
    fn test_preflight_sign_no_empty_field() {
        let pdf = create_pdf_with_signed_field();
        let result = preflight_sign(&pdf, None);
        assert!(matches!(
            result,
            Err(PreflightRefusal::NoEmptySignatureField)
        ));
    }

    #[test]
    fn test_preflight_sign_ambiguous_empty_fields() {
        let pdf = create_pdf_with_multiple_empty_sig_fields();
        let result = preflight_sign(&pdf, None);
        assert!(matches!(
            result,
            Err(PreflightRefusal::AmbiguousSignatureField { .. })
        ));
    }

    #[test]
    fn test_preflight_sign_nonexistent_field() {
        let pdf = create_pdf_with_empty_sig_field();
        let result = preflight_sign(&pdf, Some("NonExistent"));
        assert!(matches!(result, Err(PreflightRefusal::NoSuchField { .. })));
    }

    // §34.2 Refusal matrix tests

    #[test]
    fn test_prior_docmdp_p1_no_changes_refuses_signing() {
        let pdf = create_pdf_with_docmdp_signature(1); // P=1 (NO_CHANGES)
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(matches!(
            result,
            Err(PreflightRefusal::DocumentLockedByPriorSignature {
                level: MdpPerm::NoChanges
            })
        ));
    }

    #[test]
    fn test_prior_docmdp_p2_fill_forms_allows_signing() {
        let pdf = create_pdf_with_docmdp_signature(2); // P=2 (FILL_FORMS)
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_prior_docmdp_p3_annotate_allows_signing() {
        let pdf = create_pdf_with_docmdp_signature(3); // P=3 (ANNOTATE)
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(result.is_ok());
    }

    /// §77.9 / §78.3: `/Reference` is an array of *independent* signature
    /// reference dictionaries (§12.8.1 Table 253) — a certifying signature's
    /// DocMDP entry and a FieldMDP lock in the same array is ordinary, not
    /// malformed. Before the fix, `/TransformMethod`, `/Action` and
    /// `/Fields` were one flat set of variables shared across the whole
    /// array, so whichever element parsed *last* decided the outcome: with
    /// the DocMDP entry after the FieldMDP one (as here), the final
    /// `/TransformMethod` read as `/DocMDP` and the FieldMDP lock vanished.
    #[test]
    fn a_fieldmdp_lock_beside_a_docmdp_entry_in_the_same_reference_array_is_honoured() {
        let mut pdf = base(" /AcroForm 4 0 R /Perms 8 0 R");
        pdf.add("<</Fields [5 0 R 6 0 R] /SigFlags 3>>");
        pdf.add("<</FT /Sig /T (Sig1) /V 7 0 R>>");
        pdf.add("<</FT /Sig /T (Sig2)>>");
        pdf.add(
            "<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
             /Reference [\
               <</Type /SigRef /TransformMethod /FieldMDP /Data 1 0 R \
                 /TransformParams <</Type /TransformParams /V /1.2 /Action /All>>>> \
               <</Type /SigRef /TransformMethod /DocMDP /Data 1 0 R \
                 /TransformParams <</Type /TransformParams /V /1.2 /P 2>>>>\
             ] \
             /ByteRange [0 100 200 50] /Contents <00000000000000000000000000000000>>>",
        );
        pdf.add("<</DocMDP 7 0 R>>");
        let pdf = pdf.build_with_trailer(TRAILER);

        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(
            matches!(
                result,
                Err(PreflightRefusal::FieldLockedByPriorSignature { ref field, ref locked_by })
                    if field == "Sig2" && locked_by == "Sig1"
            ),
            "a FieldMDP lock listed before a DocMDP entry in the same /Reference array must \
             still be read, got {result:?}"
        );
    }

    #[test]
    fn test_fieldmdp_action_all_locks_target() {
        let pdf = create_pdf_with_fieldmdp_signature("Sig1", "All", vec![], None);
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(matches!(
            result,
            Err(PreflightRefusal::FieldLockedByPriorSignature {
                field,
                locked_by
            }) if field == "Sig2" && locked_by == "Sig1"
        ));
    }

    #[test]
    fn test_fieldmdp_action_include_names_target_locks() {
        let pdf =
            create_pdf_with_fieldmdp_signature("Sig1", "Include", vec!["Sig2".to_string()], None);
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(matches!(
            result,
            Err(PreflightRefusal::FieldLockedByPriorSignature {
                field,
                locked_by
            }) if field == "Sig2" && locked_by == "Sig1"
        ));
    }

    #[test]
    fn test_fieldmdp_action_exclude_not_naming_target_locks() {
        let pdf = create_pdf_with_fieldmdp_signature(
            "Sig1",
            "Exclude",
            vec!["Sig1".to_string()], // Excludes only Sig1, so Sig2 is locked
            None,
        );
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(matches!(
            result,
            Err(PreflightRefusal::FieldLockedByPriorSignature {
                field,
                locked_by
            }) if field == "Sig2" && locked_by == "Sig1"
        ));
    }

    #[test]
    fn test_fieldmdp_action_include_not_naming_target_allows() {
        let pdf = create_pdf_with_fieldmdp_signature(
            "Sig1",
            "Include",
            vec!["Other".to_string()], // Includes only Other, Sig2 is not locked
            None,
        );
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_fieldmdp_action_exclude_naming_target_allows() {
        let pdf = create_pdf_with_fieldmdp_signature(
            "Sig1",
            "Exclude",
            vec!["Sig2".to_string()], // Excludes Sig2, so Sig2 is NOT locked
            None,
        );
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_seed_value_dictionary_warning() {
        let pdf = create_pdf_with_seed_value_dict();
        let result = preflight_sign(&pdf, None);
        assert!(result.is_ok());
        if let Ok(ok) = result {
            assert!(ok.seed_value_ignored);
        }
    }

    /// `/SV 6 0 R` is a seed value dictionary just as much as an inline one.
    /// The inline-only arm left `has_seed_value` false for the indirect form,
    /// so the caller was told a field carried no seed value when it did — a
    /// check that fails open.
    #[test]
    fn an_indirect_seed_value_dictionary_is_still_a_seed_value_dictionary() {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R] /SigFlags 3>>");
        pdf.add("<</FT /Sig /T (Sig1) /SV 6 0 R>>");
        pdf.add("<</Type /SigFieldV /Ff 1>>");
        let pdf = pdf.build_with_trailer(TRAILER);

        let ok = preflight_sign(&pdf, None).expect("the field is signable");
        assert!(
            ok.seed_value_ignored,
            "an indirect /SV must be reported the same as an inline one"
        );
    }

    // §25.4 fail-closed tests: an undetermined lock state must block signing.

    #[test]
    fn malformed_docmdp_p_value_blocks_signing() {
        // /P is not a number at all: the certification level is unknown, so
        // signing must be refused rather than permitted.
        let pdf = create_pdf_with_docmdp_p_text("/Whatever");
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(
            matches!(result, Err(PreflightRefusal::InvalidState(_))),
            "unparsable DocMDP /P must block signing, got {result:?}"
        );
    }

    #[test]
    fn out_of_range_docmdp_p_value_blocks_signing() {
        let pdf = create_pdf_with_docmdp_p_text("9");
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(
            matches!(result, Err(PreflightRefusal::Unsupported(_))),
            "an unknown DocMDP /P level must block signing, got {result:?}"
        );
    }

    #[test]
    fn unreadable_perms_dictionary_blocks_signing() {
        // /Perms points at an object that is not in the file.
        let mut pdf = create_pdf_with_docmdp_signature(2);
        replace_once(&mut pdf, b"/Perms 7 0 R", b"/Perms 9 0 R");
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(
            matches!(result, Err(PreflightRefusal::InvalidState(_))),
            "an unreadable /Perms must block signing, got {result:?}"
        );
    }

    #[test]
    fn malformed_field_entry_blocks_signing() {
        // A /Fields entry that resolves to nothing might have been the
        // signature that locks the target; it must not be silently dropped.
        let mut pdf = create_pdf_with_empty_sig_field();
        replace_once(&mut pdf, b"/Fields [5 0 R]", b"/Fields [9 0 R]");
        let result = preflight_sign(&pdf, None);
        assert!(
            matches!(result, Err(PreflightRefusal::InvalidState(_))),
            "an unreadable field entry must block signing, got {result:?}"
        );
    }

    #[test]
    fn malformed_field_entry_blocks_certification() {
        let mut pdf = create_pdf_with_empty_sig_field();
        replace_once(&mut pdf, b"/Fields [5 0 R]", b"/Fields [9 0 R]");
        let result = preflight_certify(&pdf);
        assert!(
            matches!(result, Err(PreflightRefusal::InvalidState(_))),
            "an unreadable field entry must block certification, got {result:?}"
        );
    }

    #[test]
    fn unknown_fieldmdp_action_blocks_signing() {
        let pdf = create_pdf_with_fieldmdp_signature("Sig1", "Sideways", vec![], None);
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(
            matches!(result, Err(PreflightRefusal::Unsupported(_))),
            "an unknown FieldMDP /Action must block signing, got {result:?}"
        );
    }

    #[test]
    fn fieldmdp_without_action_blocks_signing() {
        let pdf = create_pdf_with_fieldmdp_signature("Sig1", "All", vec![], None);
        let mut pdf = pdf;
        replace_once(&mut pdf, b"/Action /All", b"/Bction /All");
        let result = preflight_sign(&pdf, Some("Sig2"));
        assert!(
            matches!(result, Err(PreflightRefusal::Unsupported(_))),
            "a FieldMDP transform with no /Action must block signing, got {result:?}"
        );
    }

    /// Replace the first occurrence of `from` with `to`. Both must be the
    /// same length so that the fixture's xref offsets stay valid.
    fn replace_once(bytes: &mut [u8], from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len());
        let position = bytes
            .windows(from.len())
            .position(|w| w == from)
            .expect("pattern present in fixture");
        bytes[position..position + to.len()].copy_from_slice(to);
    }

    // Test helpers: create minimal PDFs for testing
    // Test helpers: minimal signing fixtures, built with the testkit's PDF
    // assembler so that no test here counts cross-reference offsets by hand.
    // What each fixture is *for* is the object graph above the page tree; the
    // xref table underneath it is the same in all of them.

    /// The trailer these fixtures share. Preflight reads `/ID`, so every one
    /// of them carries the same fixed pair rather than none.
    const TRAILER: &str = "/Size {size} /Root 1 0 R \
         /ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]";

    /// Objects 1 to 3 of every fixture below: a catalog carrying `catalog`'s
    /// extra entries, a page tree, and one page. Later objects are added by
    /// the caller, which is where the fixtures differ from each other.
    fn base(catalog: &str) -> Pdf {
        let mut pdf = Pdf::new();
        pdf.add(format!("<</Type /Catalog /Pages 2 0 R{catalog}>>"));
        pdf.add("<</Type /Pages /Kids [3 0 R] /Count 1>>");
        pdf.add("<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>");
        pdf
    }

    fn create_minimal_unsigned_pdf() -> Vec<u8> {
        base("").build_with_trailer(TRAILER)
    }

    /// A document whose trailer declares `/Encrypt`, which the sign path has no
    /// layer to honour (§23.2).
    fn create_encrypted_pdf() -> Vec<u8> {
        let mut pdf = base("");
        pdf.add("<</Filter /Standard /V 2 /R 3 /Length 128 /P -1340>>");
        pdf.build_with_trailer(&format!("{TRAILER} /Encrypt 4 0 R"))
    }

    /// §35 S0 step 5: refuse an encrypted document up front rather than
    /// half-supporting it. Without this the run still failed safe — the §32
    /// gate cannot re-read the candidate — but only after writing one, and it
    /// blamed "cannot re-read" instead of naming the reason.
    #[test]
    fn an_encrypted_document_is_refused_before_anything_is_written() {
        let pdf = create_encrypted_pdf();
        assert_eq!(
            preflight_sign(&pdf, None).unwrap_err(),
            PreflightRefusal::EncryptedDocument
        );
        assert_eq!(
            preflight_certify(&pdf).unwrap_err(),
            PreflightRefusal::EncryptedDocument
        );
        // The same document without /Encrypt must still be signable, so the
        // check is not just refusing everything.
        assert!(preflight_certify(&create_minimal_unsigned_pdf()).is_ok());
    }

    fn create_pdf_with_empty_sig_field() -> Vec<u8> {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R] /SigFlags 3>>");
        pdf.add("<</FT /Sig /T (Sig1)>>");
        pdf.build_with_trailer(TRAILER)
    }

    fn create_pdf_with_signed_field() -> Vec<u8> {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R] /SigFlags 3>>");
        pdf.add("<</FT /Sig /T (Sig1) /V 6 0 R>>");
        // A stub: what makes the field count as signed is that /V resolves.
        pdf.add("<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached>>");
        pdf.build_with_trailer(TRAILER)
    }

    fn create_pdf_with_multiple_empty_sig_fields() -> Vec<u8> {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R 6 0 R] /SigFlags 3>>");
        pdf.add("<</FT /Sig /T (Sig1)>>");
        pdf.add("<</FT /Sig /T (Sig2)>>");
        pdf.build_with_trailer(TRAILER)
    }

    fn create_pdf_with_docmdp_signature(p_level: u8) -> Vec<u8> {
        create_pdf_with_docmdp_p_text(&p_level.to_string())
    }

    /// As above, but the /P value is written verbatim so that a malformed one
    /// can be exercised.
    fn create_pdf_with_docmdp_p_text(p_level: &str) -> Vec<u8> {
        let mut pdf = base(" /AcroForm 4 0 R /Perms 7 0 R");
        pdf.add("<</Fields [5 0 R 6 0 R] /SigFlags 3>>");
        // Sig1 is certified — it is the one /Perms points at — and Sig2 is
        // the empty field a later signature would take.
        pdf.add("<</FT /Sig /T (Sig1) /V 8 0 R>>");
        pdf.add("<</FT /Sig /T (Sig2)>>");
        pdf.add("<</DocMDP 8 0 R>>");
        pdf.add(format!(
            "<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
             /Reference [<</Type /SigRef /TransformMethod /DocMDP /TransformParams \
             <</Type /TransformParams /V /1.2 /P {p_level}>>>>] \
             /ByteRange [0 100 200 50] /Contents <00000000000000000000000000000000>>>"
        ));
        pdf.build_with_trailer(TRAILER)
    }

    fn create_pdf_with_fieldmdp_signature(
        sig_field_name: &str,
        action: &str,
        fields: Vec<String>,
        _p_level: Option<u8>,
    ) -> Vec<u8> {
        let named = fields
            .iter()
            .map(|field| format!("({field}) "))
            .collect::<String>();
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R 6 0 R] /SigFlags 3>>");
        pdf.add(format!("<</FT /Sig /T ({sig_field_name}) /V 7 0 R>>"));
        pdf.add("<</FT /Sig /T (Sig2)>>");
        pdf.add(format!(
            "<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
             /Reference [<</Type /SigRef /TransformMethod /FieldMDP /Data 1 0 R \
             /TransformParams <</Type /TransformParams /V /1.2 /Action /{action} \
             /Fields [{named}]>>>>] \
             /ByteRange [0 100 200 50] /Contents <00000000000000000000000000000000>>>"
        ));
        pdf.build_with_trailer(TRAILER)
    }

    // --- The shape a real form actually has ---------------------------------
    //
    // Every fixture above writes a bare field dictionary. Acrobat writes the
    // field merged with its single widget annotation — `/Type /Annot /Subtype
    // /Widget` on the same dictionary the AcroForm `/Fields` array points at
    // — and spells any non-ASCII `/T` as UTF-16BE. Both are ordinary, and
    // both used to defeat pre-flight: auto-detection excluded anything
    // carrying `/Type /Annot`, so a real form reported no empty field at all,
    // and a UTF-16BE `/T` decoded to nothing, so the field could not be named
    // either.

    /// `name` as a UTF-16BE literal string with octal escapes, the way
    /// Acrobat writes a name that is not plain ASCII.
    fn utf16_literal(name: &str) -> String {
        let mut bytes = vec![0xFEu8, 0xFF];
        for unit in name.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        let mut out = String::from("(");
        for byte in bytes {
            match byte {
                b'(' | b')' | b'\\' => {
                    out.push('\\');
                    out.push(byte as char);
                }
                0x20..=0x7E => out.push(byte as char),
                other => out.push_str(&format!("\\{other:03o}")),
            }
        }
        out.push(')');
        out
    }

    /// `name` as a UTF-16BE hex string.
    fn utf16_hex(name: &str) -> String {
        let mut out = String::from("<FEFF");
        for unit in name.encode_utf16() {
            out.push_str(&format!("{unit:04X}"));
        }
        out.push('>');
        out
    }

    /// A document whose fields are merged field/widget dictionaries, each
    /// `/T` written as the caller spelled it.
    fn create_pdf_with_merged_widgets(names: &[String]) -> Vec<u8> {
        let refs: Vec<String> = (0..names.len()).map(|i| format!("{} 0 R", 5 + i)).collect();
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add(format!("<</Fields [{}] /SigFlags 3>>", refs.join(" ")));
        for name in names {
            pdf.add(format!(
                "<</Type /Annot /Subtype /Widget /FT /Sig /T {name} \
                 /Rect [0 0 0 0] /F 132 /P 3 0 R>>"
            ));
        }
        pdf.build_with_trailer(TRAILER)
    }

    #[test]
    fn a_merged_field_widget_is_a_field_auto_detection_can_find() {
        let pdf = create_pdf_with_merged_widgets(&["(Signature TGDE)".to_string()]);
        let ok = preflight_sign(&pdf, None).expect("a merged widget is still a field");
        assert_eq!(ok.target_field, "Signature TGDE");
    }

    #[test]
    fn ambiguous_candidates_are_named_with_their_accents_intact() {
        let pdf = create_pdf_with_merged_widgets(&[
            "(Signature TGDE)".to_string(),
            utf16_literal("Président-rapporteur"),
            utf16_hex("Membre jury"),
        ]);
        let Err(PreflightRefusal::AmbiguousSignatureField { candidates }) =
            preflight_sign(&pdf, None)
        else {
            panic!(
                "three empty fields are ambiguous, got {:?}",
                preflight_sign(&pdf, None)
            );
        };
        assert_eq!(
            candidates,
            vec!["Signature TGDE", "Président-rapporteur", "Membre jury"],
            "the candidates the UI shows must be the names Acrobat shows"
        );
    }

    #[test]
    fn a_utf16be_field_name_matches_the_utf8_name_the_application_passes() {
        for spelled in [
            utf16_literal("Président-rapporteur"),
            utf16_hex("Président-rapporteur"),
        ] {
            let pdf =
                create_pdf_with_merged_widgets(&[spelled.clone(), "(Signature8)".to_string()]);
            let ok = preflight_sign(&pdf, Some("Président-rapporteur"))
                .unwrap_or_else(|e| panic!("{spelled} should be targetable, got {e:?}"));
            assert_eq!(ok.target_field, "Président-rapporteur");
        }
    }

    /// The `/T` decode is not only about names. The token after a UTF-16BE
    /// `/T` used to be read as that key's value because the key was never
    /// consumed, so `/V` — the next entry Acrobat writes — went unseen and a
    /// signed field read as empty. Certification would then have been allowed
    /// over an already-signed document.
    #[test]
    fn a_signed_field_with_a_utf16be_name_is_still_seen_as_signed() {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R] /SigFlags 3>>");
        pdf.add(format!(
            "<</Type /Annot /Subtype /Widget /FT /Sig /T {} /V 6 0 R>>",
            utf16_literal("Président-rapporteur")
        ));
        pdf.add("<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached>>");
        let pdf = pdf.build_with_trailer(TRAILER);

        assert_eq!(
            preflight_sign(&pdf, None).unwrap_err(),
            PreflightRefusal::NoEmptySignatureField
        );
        assert!(matches!(
            preflight_sign(&pdf, Some("Président-rapporteur")),
            Err(PreflightRefusal::FieldAlreadySigned { .. })
        ));
        assert_eq!(
            preflight_certify(&pdf).unwrap_err(),
            PreflightRefusal::CertificationNotAllowed {
                existing_signatures: 1
            }
        );
    }

    /// A `/Fields` entry that is a widget and nothing else is still not a
    /// field, and must not be offered as a signing target. This is what the
    /// removed `is_annot_only` flag was reaching for; having no `/FT` is the
    /// predicate that actually says it.
    #[test]
    fn a_widget_with_no_field_attributes_is_not_a_candidate() {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R] /SigFlags 3>>");
        pdf.add("<</Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R>>");
        let pdf = pdf.build_with_trailer(TRAILER);
        assert_eq!(
            preflight_sign(&pdf, None).unwrap_err(),
            PreflightRefusal::NoEmptySignatureField
        );
    }

    /// A `/FieldMDP` `/Fields` entry is a text string too, so a lock naming a
    /// UTF-16BE field must be recognised as locking it.
    #[test]
    fn a_fieldmdp_lock_naming_a_utf16be_field_still_locks_it() {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R 6 0 R] /SigFlags 3>>");
        pdf.add("<</FT /Sig /T (Sig1) /V 7 0 R>>");
        pdf.add(format!(
            "<</Type /Annot /Subtype /Widget /FT /Sig /T {}>>",
            utf16_literal("Président-rapporteur")
        ));
        pdf.add(format!(
            "<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
             /Reference [<</Type /SigRef /TransformMethod /FieldMDP /Data 1 0 R \
             /TransformParams <</Type /TransformParams /V /1.2 /Action /Include \
             /Fields [{}]>>>>] \
             /ByteRange [0 100 200 50] /Contents <00000000000000000000000000000000>>>",
            utf16_literal("Président-rapporteur")
        ));
        let pdf = pdf.build_with_trailer(TRAILER);

        assert!(matches!(
            preflight_sign(&pdf, Some("Président-rapporteur")),
            Err(PreflightRefusal::FieldLockedByPriorSignature { field, .. })
                if field == "Président-rapporteur"
        ));
    }

    fn create_pdf_with_seed_value_dict() -> Vec<u8> {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R] /SigFlags 3>>");
        pdf.add("<</FT /Sig /T (Sig1) /SV <</Type /SigFieldV>>>>");
        pdf.build_with_trailer(TRAILER)
    }

    // --- §76.6: field inheritance ------------------------------------------
    //
    // `/FT` and `/T` are inheritable (§12.7.3.2): a hierarchical field's
    // terminal node may carry neither, and a `/FieldMDP` lock's `/Fields`
    // array names fields by their *fully qualified* name (ancestors' `/T`
    // joined with `.`). A field discovered through `find_fields_array`
    // (which walks the tree with inheritance, as `discover_signatures`
    // does) but read here node-by-node used to be invisible to
    // certification and unmatchable by a qualified lock.

    /// A signature field one level under a parent that carries the `/FT`:
    /// object 5 is the parent (`/T (form) /FT /Sig`), object 6 is the
    /// signed terminal field (`/T (Sig1)`, inheriting `/FT` and
    /// contributing to the qualified name `form.Sig1`), object 7 is its
    /// signature dictionary.
    fn create_pdf_with_hierarchical_signed_field() -> Vec<u8> {
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R] /SigFlags 3>>");
        pdf.add("<</T (form) /FT /Sig /Kids [6 0 R]>>");
        pdf.add("<</T (Sig1) /V 7 0 R>>");
        pdf.add(
            "<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
             /ByteRange [0 100 200 50] /Contents <00000000000000000000000000000000>>>",
        );
        pdf.build_with_trailer(TRAILER)
    }

    #[test]
    fn a_hierarchical_signed_field_is_seen_by_preflight_certify() {
        // Before the fix, `resolver_and_fields` discarded the tree walk's
        // inheritance and re-read each node in isolation, so `form.Sig1`
        // (whose `/FT` lives on its parent, object 5) had no `/FT` of its
        // own and was silently not a field at all: certifying over it was
        // permitted, exactly the failure §25.4 rule 1 exists to prevent.
        let pdf = create_pdf_with_hierarchical_signed_field();
        assert_eq!(
            preflight_certify(&pdf),
            Err(PreflightRefusal::CertificationNotAllowed {
                existing_signatures: 1
            }),
            "a signed hierarchical field must still block certification"
        );
    }

    /// §78.4: discovery and preflight enumerate signature fields two
    /// different ways — `discover_signatures` walks `find_field_tree_with`
    /// and calls `extract_signature_field` per node, while preflight walks
    /// the same tree through `fields_with`/`extract_field_info` — and both
    /// used to disagree about a hierarchical field before §76.6. Pinning
    /// that they read the same set, rather than only that each individually
    /// sees the field (the two tests above), is what actually closes the
    /// "two readers disagree" class §78.1 names.
    #[test]
    fn discovery_and_preflight_see_the_same_hierarchical_signature_fields() {
        let pdf = create_pdf_with_hierarchical_signed_field();

        let resolver = ObjectResolver::new(&pdf);
        let field_entries = fields_with(&resolver, &pdf).expect("the field tree resolves");
        let mut preflight_names: Vec<String> = field_entries
            .iter()
            .filter_map(|entry| extract_field_info(&resolver, entry).ok().flatten())
            .filter(|info| info.field_type == "Sig" && info.has_signature)
            .map(|info| info.field_name)
            .collect();
        preflight_names.sort();

        let revisions = crate::verify::RevisionMap::build(&pdf).expect("the revision map builds");
        let mut discovery_names: Vec<String> = crate::verify::discover_signatures(&pdf, &revisions)
            .expect("discovery succeeds")
            .into_iter()
            .map(|report| report.field_name)
            .collect();
        discovery_names.sort();

        assert_eq!(
            preflight_names, discovery_names,
            "discovery and preflight must read the same set of signature fields from a \
             hierarchical field tree"
        );
        assert_eq!(
            preflight_names,
            vec!["form.Sig1".to_string()],
            "the fixture's one signed field, sanity-checked so the assertion above cannot \
             pass by both sides being empty"
        );
    }

    #[test]
    fn a_fieldmdp_lock_on_a_qualified_name_locks_the_hierarchical_target() {
        // Two children of the same parent "form": Sig1 (already signed,
        // carrying a FieldMDP that locks "form.Sig2" — the fully qualified
        // name a `/Fields` array actually names) and Sig2 (empty, the
        // signing target). Neither child carries its own `/FT`; both
        // inherit it from the parent at object 5. Before the fix, the
        // target's name was read as the bare "Sig2" (per-node `/T` only),
        // which a lock naming "form.Sig2" could never match.
        let mut pdf = base(" /AcroForm 4 0 R");
        pdf.add("<</Fields [5 0 R] /SigFlags 3>>");
        pdf.add("<</T (form) /FT /Sig /Kids [6 0 R 8 0 R]>>");
        pdf.add("<</T (Sig1) /V 7 0 R>>");
        pdf.add(
            "<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
             /Reference [<</Type /SigRef /TransformMethod /FieldMDP /Data 1 0 R \
             /TransformParams <</Type /TransformParams /V /1.2 /Action /Include \
             /Fields [(form.Sig2)]>>>>] \
             /ByteRange [0 100 200 50] /Contents <00000000000000000000000000000000>>>",
        );
        pdf.add("<</T (Sig2)>>");
        let pdf = pdf.build_with_trailer(TRAILER);

        let result = preflight_sign(&pdf, Some("form.Sig2"));
        assert!(
            matches!(
                &result,
                Err(PreflightRefusal::FieldLockedByPriorSignature { field, locked_by })
                    if field == "form.Sig2" && locked_by == "form.Sig1"
            ),
            "a FieldMDP naming the qualified field 'form.Sig2' must lock it, got {result:?}"
        );

        // The unqualified name is not a field this document has, so it must
        // not be reachable at all — confirming the match above is on the
        // qualified name and not some looser fallback.
        assert!(matches!(
            preflight_sign(&pdf, Some("Sig2")),
            Err(PreflightRefusal::NoSuchField { .. })
        ));
    }
}
