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

use super::{find_catalog_ref_with, find_fields_array_with, MdpPerm, ObjectResolver};
use crate::pdfwrite::PdfTokenizer;
use thiserror::Error;

/// Read one object's definition through the pass's resolver.
///
/// Every read in this module goes through here so that a pre-flight pass
/// builds the cross-reference index once. Calling the free `object_definition`
/// per field rebuilt it — re-inflating every cross-reference stream in the
/// chain — on every lookup, which a document with a long chain turns into
/// hours of work in the process that owns the user interface.
fn definition(resolver: &ObjectResolver<'_>, obj_num: u32) -> super::Result<Vec<u8>> {
    resolver.object_bytes(obj_num).map(|(bytes, _)| bytes)
}

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

/// Map a tokenizer failure the same way.
fn tokenizer_failed(context: &str, e: crate::pdfwrite::PdfWriteError) -> PreflightRefusal {
    PreflightRefusal::Unsupported(format!(
        "could not determine whether the document is locked: {context} could not be tokenized: {e}"
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

/// Check 1: Certifying requires an unsigned document.
/// If any /Sig field with a non-null /V exists, refuse with CertificationNotAllowed.
pub fn preflight_certify(bytes: &[u8]) -> Result<()> {
    // One resolver for the whole pass; see `definition` above.
    let resolver = ObjectResolver::new(bytes);
    if resolver.is_encrypted() {
        return Err(PreflightRefusal::EncryptedDocument);
    }

    let catalog_ref = find_catalog_ref_with(&resolver, bytes)
        .map_err(|e| PreflightRefusal::InvalidState(format!("Failed to find catalog: {}", e)))?;

    let fields_array = find_fields_array_with(&resolver, catalog_ref).map_err(|e| {
        PreflightRefusal::InvalidState(format!("Failed to find fields array: {}", e))
    })?;

    let mut signed_count = 0;
    for field_ref in fields_array {
        // Fail closed: an entry that cannot be parsed might be a signature,
        // and certification requires proof that there is none.
        if let Some(field_info) = extract_field_info(&resolver, field_ref)? {
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
    // Refuse up front. Without this the run still fails safe — the §32 gate
    // cannot re-read the candidate, so nothing is promoted — but only after
    // writing one, and it reports "cannot re-read" rather than naming the
    // actual reason.
    // One resolver for the whole pass; see `definition` above.
    let resolver = ObjectResolver::new(bytes);
    if resolver.is_encrypted() {
        return Err(PreflightRefusal::EncryptedDocument);
    }

    let catalog_ref = find_catalog_ref_with(&resolver, bytes)
        .map_err(|e| PreflightRefusal::InvalidState(format!("Failed to find catalog: {}", e)))?;

    let fields_array = find_fields_array_with(&resolver, catalog_ref).map_err(|e| {
        PreflightRefusal::InvalidState(format!("Failed to find fields array: {}", e))
    })?;

    // Collect all field infos
    let mut field_infos = Vec::new();
    for field_ref in fields_array {
        // Fail closed: an unparsable entry may be the prior signature whose
        // FieldMDP locks the target, so it must not be dropped.
        if let Some(info) = extract_field_info(&resolver, field_ref)? {
            field_infos.push((field_ref.0, info));
        }
    }

    // Check 2: Read Root /Perms /DocMDP level
    check_prior_docmdp(&resolver, bytes)?;

    // If target_field is None, auto-detect empty signature field
    let target_field_name = if let Some(target) = target_field {
        // Verify that the named field exists
        if !field_infos
            .iter()
            .any(|(_, info)| info.field_name == target)
        {
            return Err(PreflightRefusal::NoSuchField {
                field: target.to_string(),
            });
        }
        target.to_string()
    } else {
        // Auto-detect: find empty /Sig fields
        let empty_sig_fields: Vec<String> = field_infos
            .iter()
            .filter(|(_, info)| info.field_type == "Sig" && !info.has_signature)
            .map(|(_, info)| info.field_name.clone())
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
        .find(|(_, info)| info.field_name == target_field_name)
        .map(|(_, info)| info.clone())
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
    check_field_mdp_locks(&resolver, &target_field_name, &field_infos)?;

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
}

/// Extract field type, name, signature presence, and /SV presence.
///
/// Everything this sees was reached from the AcroForm `/Fields` array, so by
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
    field_ref: (u32, u16),
) -> Result<Option<FieldInfo>> {
    let field_obj_definition = definition(resolver, field_ref.0).map_err(|e| {
        undetermined(
            &format!("form field object {} could not be read", field_ref.0),
            e,
        )
    })?;
    let field_obj_slice = field_obj_definition.as_slice();
    if field_obj_slice.is_empty() {
        return Ok(None);
    }

    let mut tokenizer = PdfTokenizer::new(field_obj_slice);
    let mut field_type = String::new();
    let mut field_name = String::new();
    let mut has_signature = false;
    let mut has_seed_value = false;

    let mut key: Option<String> = None;

    while let Some(token) = tokenizer
        .next_token()
        .map_err(|e| tokenizer_failed(&format!("form field object {}", field_ref.0), e))?
    {
        if token == b"endobj" {
            break;
        }

        // Matched on bytes throughout. A UTF-16BE `/T` is not valid UTF-8,
        // and a pass that skipped such a token also failed to consume its
        // key, so the following key was eaten as this one's value and `/V`
        // went unseen — a signed field read as unsigned.
        if key.is_none() && token.starts_with(b"/") {
            key = Some(String::from_utf8_lossy(&token[1..]).into_owned());
        } else if let Some(k) = key.take() {
            match k.as_str() {
                "FT" => {
                    if let Some(ft_name) = token.strip_prefix(b"/".as_slice()) {
                        field_type = String::from_utf8_lossy(ft_name).into_owned();
                    }
                }
                "T" => {
                    if let Some(name_val) = super::parse_pdf_string(&token) {
                        field_name = name_val;
                    }
                }
                "V" => {
                    // Any non-null /V means the field is signed
                    if token != b"null" {
                        has_signature = true;
                    }
                }
                // Seed value dictionary
                "SV" if token == b"<<" => {
                    has_seed_value = true;
                    // Skip the seed value dictionary, tracking depth so
                    // that a nested dictionary cannot end it early.
                    let mut sv_depth = 1usize;
                    while sv_depth > 0 {
                        match tokenizer.next_token().map_err(|e| {
                            tokenizer_failed(&format!("/SV dictionary of field {}", field_ref.0), e)
                        })? {
                            Some(t) if t == b"<<" => sv_depth += 1,
                            Some(t) if t == b">>" => sv_depth -= 1,
                            Some(t) if t == b"endobj" => {
                                return Err(PreflightRefusal::Unsupported(format!(
                                    "field {} has an unterminated /SV dictionary; \
                                     could not determine whether it is locked",
                                    field_ref.0
                                )))
                            }
                            Some(_) => {}
                            // A truncated object: refuse rather than
                            // treat the field as readable.
                            None => {
                                return Err(PreflightRefusal::Unsupported(format!(
                                    "field {} has an unterminated /SV dictionary; \
                                     could not determine whether it is locked",
                                    field_ref.0
                                )))
                            }
                        }
                    }
                }
                // A `/SV` written as an indirect reference — `/SV 9 0 R` — is
                // just as much a seed value dictionary as an inline one, and
                // the inline-only arm above left `has_seed_value` false for
                // it. That is a check that fails *open*: the caller is told
                // the field carries no seed value when it does. The reference
                // is not followed, because the warning does not depend on what
                // the dictionary contains, only that there is one.
                "SV" if std::str::from_utf8(&token)
                    .ok()
                    .is_some_and(|t| t.parse::<u32>().is_ok()) =>
                {
                    has_seed_value = true;
                }
                _ => {}
            }
        }
    }

    if field_type.is_empty() {
        return Ok(None);
    }

    Ok(Some(FieldInfo {
        field_name: if field_name.is_empty() {
            format!("Field_{}", field_ref.0)
        } else {
            field_name
        },
        field_type,
        has_signature,
        has_seed_value,
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
    let catalog_obj_definition = definition(resolver, catalog_ref.0)
        .map_err(|e| undetermined("the catalog object could not be read", e))?;
    let catalog_obj_slice = catalog_obj_definition.as_slice();
    if catalog_obj_slice.is_empty() {
        return Ok(None);
    }

    // Root /Perms: absent means no certification. Present means its value
    // decides whether signing is permitted, so it must be understood.
    let mut tokenizer = PdfTokenizer::new(catalog_obj_slice);
    let perms = match find_dict_value(&mut tokenizer, "Perms", "the catalog")? {
        None => return Ok(None),
        Some(v) => v,
    };

    let perms_slice = match perms {
        DictValue::Reference(num) => Some(
            definition(resolver, num)
                .map_err(|e| undetermined("the /Perms dictionary could not be read", e))?,
        ),
        DictValue::Dictionary => None,
        DictValue::Other(token) => {
            return Err(PreflightRefusal::Unsupported(format!(
                "Root /Perms is neither a dictionary nor an indirect reference (found {}); \
                 could not determine whether the document is locked",
                String::from_utf8_lossy(&token)
            )))
        }
    };

    // Either open the referenced object, or continue in the catalog's own
    // token stream when /Perms was written inline.
    let docmdp = match perms_slice {
        Some(slice) => {
            if slice.is_empty() {
                return Ok(None);
            }
            let mut perms_tokenizer = PdfTokenizer::new(slice.as_slice());
            find_dict_value(&mut perms_tokenizer, "DocMDP", "the /Perms dictionary")?
        }
        None => find_dict_value(&mut tokenizer, "DocMDP", "the inline /Perms dictionary")?,
    };

    match docmdp {
        None => Ok(None),
        Some(DictValue::Reference(num)) => extract_docmdp_p_level(resolver, num),
        Some(other) => Err(PreflightRefusal::Unsupported(format!(
            "/Perms /DocMDP is not an indirect reference to a signature dictionary ({}); \
             could not determine whether the document is locked",
            other.describe()
        ))),
    }
}

/// A dictionary value, only as far as this module needs to tell values apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DictValue {
    /// `N G R`.
    Reference(u32),
    /// `<<` — an inline dictionary; the tokenizer is left just inside it.
    Dictionary,
    /// Anything else, kept so that a refusal can name it.
    Other(Vec<u8>),
}

impl DictValue {
    fn describe(&self) -> String {
        match self {
            DictValue::Reference(n) => format!("reference to object {n}"),
            DictValue::Dictionary => "inline dictionary".to_string(),
            DictValue::Other(t) => String::from_utf8_lossy(t).into_owned(),
        }
    }
}

/// Scan `tokenizer` for `key` and return its value.
///
/// Every tokenizer failure is propagated: a scan that stopped early would
/// report "key absent" for a document that may well carry it.
fn find_dict_value(
    tokenizer: &mut PdfTokenizer<'_>,
    key: &str,
    context: &str,
) -> Result<Option<DictValue>> {
    let mut pending: Option<String> = None;

    while let Some(token) = tokenizer
        .next_token()
        .map_err(|e| tokenizer_failed(context, e))?
    {
        if token == b"endobj" {
            break;
        }

        let token_str = match std::str::from_utf8(&token) {
            Ok(s) => s,
            Err(_) => {
                pending = None;
                continue;
            }
        };

        let this_key = match pending.take() {
            None => {
                if let Some(name) = token_str.strip_prefix('/') {
                    pending = Some(name.to_string());
                }
                continue;
            }
            Some(k) => k,
        };

        // A name in value position starts the next key/value pair instead.
        if this_key != key {
            if let Some(name) = token_str.strip_prefix('/') {
                pending = Some(name.to_string());
            }
            continue;
        }

        if token == b"<<" {
            return Ok(Some(DictValue::Dictionary));
        }
        let number: u32 = match token_str.parse() {
            Ok(n) => n,
            Err(_) => return Ok(Some(DictValue::Other(token))),
        };
        // Confirm the `G R` that makes it an indirect reference.
        let generation = tokenizer
            .next_token()
            .map_err(|e| tokenizer_failed(context, e))?;
        let r_token = tokenizer
            .next_token()
            .map_err(|e| tokenizer_failed(context, e))?;
        let generation_ok = generation
            .as_deref()
            .and_then(|g| std::str::from_utf8(g).ok())
            .and_then(|g| g.parse::<u16>().ok())
            .is_some();
        if generation_ok && r_token.as_deref() == Some(b"R".as_slice()) {
            return Ok(Some(DictValue::Reference(number)));
        }
        return Err(PreflightRefusal::Unsupported(format!(
            "/{key} in {context} is a number that is not an indirect reference; \
             could not determine whether the document is locked"
        )));
    }

    Ok(None)
}

/// Extract /P value from DocMDP signature dictionary.
fn extract_docmdp_p_level(
    resolver: &ObjectResolver<'_>,
    sig_dict_num: u32,
) -> Result<Option<MdpPerm>> {
    let sig_dict_definition = definition(resolver, sig_dict_num).map_err(|e| {
        undetermined(
            &format!("the DocMDP signature dictionary (object {sig_dict_num}) could not be read"),
            e,
        )
    })?;
    let sig_dict_slice = sig_dict_definition.as_slice();
    if sig_dict_slice.is_empty() {
        return Ok(None);
    }

    let context = "the DocMDP signature dictionary";
    let mut tokenizer = PdfTokenizer::new(sig_dict_slice);
    let mut key: Option<String> = None;
    let mut in_transform_params = false;

    while let Some(token) = tokenizer
        .next_token()
        .map_err(|e| tokenizer_failed(context, e))?
    {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if key.is_none() && token_str.starts_with('/') {
                if let Some(name) = token_str.strip_prefix('/') {
                    key = Some(name.to_string());
                }
            } else if let Some(k) = key.take() {
                match k.as_str() {
                    "TransformParams" if token == b"<<" => {
                        in_transform_params = true;
                    }
                    "P" if in_transform_params => {
                        // /P decides whether signing is permitted at all. An
                        // unreadable or out-of-range value is refused, never
                        // read as "no restriction".
                        let level_str = std::str::from_utf8(&token).map_err(|_| {
                            PreflightRefusal::InvalidState(
                                "DocMDP /P is not a readable number; could not determine \
                                 whether the document is locked"
                                    .to_string(),
                            )
                        })?;
                        let level: u8 = level_str.parse().map_err(|_| {
                            PreflightRefusal::InvalidState(format!(
                                "DocMDP /P value '{level_str}' is not an integer; could not \
                                 determine whether the document is locked"
                            ))
                        })?;
                        return match level {
                            1 => Ok(Some(MdpPerm::NoChanges)),
                            2 => Ok(Some(MdpPerm::FillForms)),
                            3 => Ok(Some(MdpPerm::Annotate)),
                            other => Err(PreflightRefusal::Unsupported(format!(
                                "DocMDP /P value {other} is not one of 1, 2 or 3; could not \
                                 determine whether the document is locked"
                            ))),
                        };
                    }
                    _ => {}
                }
            }
        }

        if token == b">>" {
            in_transform_params = false;
        }
    }

    Ok(None)
}

/// Check that no prior signature's FieldMDP locks the target field (Check 3).
fn check_field_mdp_locks(
    resolver: &ObjectResolver<'_>,
    target_field: &str,
    field_infos: &[(u32, FieldInfo)],
) -> Result<()> {
    // Collect all signed fields (existing signatures)
    let signed_fields: Vec<_> = field_infos
        .iter()
        .filter(|(_, info)| info.field_type == "Sig" && info.has_signature)
        .collect();

    for (field_obj_num, field_info) in signed_fields {
        // Extract FieldMDP locks from this signature. A failure to read the
        // transform is not "no lock": it is "unknown", and unknown blocks.
        if let Some((locked_fields, action)) = extract_field_mdp_locks(resolver, *field_obj_num)? {
            // Check if target field is locked by this signature
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

/// Extract FieldMDP lock information from a signature field's /Reference array.
/// Returns (locked_field_names, action) if FieldMDP exists.
fn extract_field_mdp_locks(
    resolver: &ObjectResolver<'_>,
    field_obj_num: u32,
) -> Result<Option<(Vec<String>, FieldMdpAction)>> {
    let context = "a signed signature field";
    let field_obj_definition = definition(resolver, field_obj_num).map_err(|e| {
        undetermined(
            &format!("signature field object {field_obj_num} could not be read"),
            e,
        )
    })?;
    let field_obj_slice = field_obj_definition.as_slice();
    if field_obj_slice.is_empty() {
        return Ok(None);
    }

    // Get the /V reference (signature dictionary). The caller only asks about
    // fields it has already established are signed, so a /V that cannot be
    // resolved means the transform cannot be read: refuse.
    let mut tokenizer = PdfTokenizer::new(field_obj_slice);
    let sig_dict_num = match find_dict_value(&mut tokenizer, "V", context)? {
        Some(DictValue::Reference(num)) => num,
        Some(other) => {
            return Err(PreflightRefusal::Unsupported(format!(
                "signature field object {field_obj_num} has a /V that is not an indirect \
                 reference ({}); could not determine whether it locks other fields",
                other.describe()
            )))
        }
        None => {
            return Err(PreflightRefusal::InvalidState(format!(
                "signature field object {field_obj_num} appears signed but carries no /V; \
                 could not determine whether it locks other fields"
            )))
        }
    };

    let sig_dict_definition = definition(resolver, sig_dict_num).map_err(|e| {
        undetermined(
            &format!("signature dictionary object {sig_dict_num} could not be read"),
            e,
        )
    })?;
    let sig_dict_slice = sig_dict_definition.as_slice();
    if sig_dict_slice.is_empty() {
        return Ok(None);
    }
    extract_fieldmdp_from_sig_dict(sig_dict_slice)
}

/// Extract FieldMDP info from a signature dictionary's /Reference array.
fn extract_fieldmdp_from_sig_dict(
    sig_dict_slice: &[u8],
) -> Result<Option<(Vec<String>, FieldMdpAction)>> {
    let context = "a signature dictionary's /Reference array";
    let mut tokenizer = PdfTokenizer::new(sig_dict_slice);
    let mut key: Option<String> = None;
    let mut in_reference = false;
    let mut in_transform_params = false;
    let mut action: Option<FieldMdpAction> = None;
    let mut locked_fields = Vec::new();
    let mut transform_method: Option<String> = None;

    while let Some(token) = tokenizer
        .next_token()
        .map_err(|e| tokenizer_failed(context, e))?
    {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if key.is_none() && token_str.starts_with('/') {
                if let Some(name) = token_str.strip_prefix('/') {
                    key = Some(name.to_string());
                }
            } else if let Some(k) = key.take() {
                match k.as_str() {
                    "Reference" if token == b"[" => {
                        in_reference = true;
                    }
                    "TransformMethod" => {
                        if let Ok(method_str) = std::str::from_utf8(&token) {
                            if let Some(method) = method_str.strip_prefix('/') {
                                transform_method = Some(method.to_string());
                            }
                        }
                    }
                    "TransformParams" if token == b"<<" => {
                        in_transform_params = true;
                    }
                    "Action" if in_transform_params => {
                        // An /Action this build does not know is not "no
                        // lock": its meaning decides whether the target field
                        // may be filled, so it is Unsupported.
                        let action_str = std::str::from_utf8(&token).map_err(|_| {
                            PreflightRefusal::Unsupported(
                                "FieldMDP /Action is not a readable name; could not determine \
                                 whether the field is locked"
                                    .to_string(),
                            )
                        })?;
                        let action_name = action_str.strip_prefix('/').ok_or_else(|| {
                            PreflightRefusal::Unsupported(format!(
                                "FieldMDP /Action value '{action_str}' is not a name; could not \
                                 determine whether the field is locked"
                            ))
                        })?;
                        action = Some(match action_name {
                            "All" => FieldMdpAction::All,
                            "Include" => FieldMdpAction::Include,
                            "Exclude" => FieldMdpAction::Exclude,
                            other => {
                                return Err(PreflightRefusal::Unsupported(format!(
                                    "FieldMDP /Action '{other}' is not one of /All, /Include or \
                                     /Exclude; could not determine whether the field is locked"
                                )))
                            }
                        });
                    }
                    "Fields" if token == b"[" => {
                        // Parse field names array
                        while let Some(field_token) = tokenizer
                            .next_token()
                            .map_err(|e| tokenizer_failed(context, e))?
                        {
                            if field_token == b"]" {
                                break;
                            }
                            // A locked field's name is a text string too, and
                            // it is compared against the decoded target name.
                            if let Some(field_name) = super::parse_pdf_string(&field_token) {
                                locked_fields.push(field_name);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if token == b"]" && in_reference {
            in_reference = false;
        }
        if token == b">>" && in_transform_params {
            in_transform_params = false;
        }
    }

    // Only return if this is a FieldMDP transform.
    if transform_method.as_deref() == Some("FieldMDP") {
        // A FieldMDP transform with no readable /Action leaves the lock
        // undetermined; refuse rather than treat the field as free.
        let action = action.ok_or_else(|| {
            PreflightRefusal::Unsupported(
                "a prior signature carries a /FieldMDP transform with no /Action; could not \
                 determine whether the field is locked"
                    .to_string(),
            )
        })?;
        return Ok(Some((locked_fields, action)));
    }

    Ok(None)
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
}
