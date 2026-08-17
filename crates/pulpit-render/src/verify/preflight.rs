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

use super::{find_catalog_ref, find_fields_array, find_object, MdpPerm};
use crate::pdfwrite::PdfTokenizer;
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

    #[error("No empty signature field exists in the document")]
    NoEmptySignatureField,

    #[error("Multiple empty signature fields exist; target is ambiguous: {candidates:?}")]
    AmbiguousSignatureField { candidates: Vec<String> },

    #[error("Signature field '{field}' does not exist")]
    NoSuchField { field: String },

    #[error("Invalid state: {0}")]
    InvalidState(String),
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
    let catalog_ref = find_catalog_ref(bytes)
        .map_err(|e| PreflightRefusal::InvalidState(format!("Failed to find catalog: {}", e)))?;

    let fields_array = find_fields_array(bytes, catalog_ref).map_err(|e| {
        PreflightRefusal::InvalidState(format!("Failed to find fields array: {}", e))
    })?;

    let mut signed_count = 0;
    for field_ref in fields_array {
        if let Ok(Some(field_info)) = extract_field_info(bytes, field_ref) {
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
    let catalog_ref = find_catalog_ref(bytes)
        .map_err(|e| PreflightRefusal::InvalidState(format!("Failed to find catalog: {}", e)))?;

    let fields_array = find_fields_array(bytes, catalog_ref).map_err(|e| {
        PreflightRefusal::InvalidState(format!("Failed to find fields array: {}", e))
    })?;

    // Collect all field infos
    let mut field_infos = Vec::new();
    for field_ref in fields_array {
        if let Ok(Some(info)) = extract_field_info(bytes, field_ref) {
            field_infos.push((field_ref.0, info));
        }
    }

    // Check 2: Read Root /Perms /DocMDP level
    check_prior_docmdp(bytes)?;

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
            .filter(|(_, info)| {
                info.field_type == "Sig" && !info.has_signature && !info.is_annot_only
            })
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
    check_field_mdp_locks(bytes, &target_field_name, &field_infos)?;

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
    is_annot_only: bool,
}

/// Extract field type, name, signature presence, and /SV presence.
fn extract_field_info(bytes: &[u8], field_ref: (u32, u16)) -> super::Result<Option<FieldInfo>> {
    let field_obj_slice = find_object(bytes, field_ref.0)?;
    if field_obj_slice.is_empty() {
        return Ok(None);
    }

    let mut tokenizer = PdfTokenizer::new(field_obj_slice);
    let mut field_type = String::new();
    let mut field_name = String::new();
    let mut has_signature = false;
    let mut has_seed_value = false;
    let mut is_annot_only = false;

    let mut key: Option<String> = None;
    let mut depth = 0;

    while let Ok(Some(token)) = tokenizer.next_token() {
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
                    "FT" => {
                        if let Ok(ft_str) = std::str::from_utf8(&token) {
                            if let Some(ft_name) = ft_str.strip_prefix('/') {
                                field_type = ft_name.to_string();
                            }
                        }
                    }
                    "T" => {
                        if let Ok(name_str) = std::str::from_utf8(&token) {
                            if let Some(name_val) = super::parse_pdf_string(name_str) {
                                field_name = name_val;
                            }
                        }
                    }
                    "V" => {
                        // Any non-null /V means the field is signed
                        if token != b"null" {
                            has_signature = true;
                        }
                    }
                    "SV" => {
                        // Seed value dictionary
                        if token == b"<<" {
                            has_seed_value = true;
                            depth = 1;
                            // Skip until matching >>
                            while depth > 0 && tokenizer.next_token().is_ok() {
                                // Track depth; this is simplified
                                depth = 0; // simplified: just mark as present
                            }
                        }
                    }
                    "Type" if token == b"/Annot" => {
                        // This is an annotation-only entry, no /Sig fields typically
                        is_annot_only = true;
                    }
                    _ => {}
                }
            } else if depth > 0 {
                if token == b"<<" {
                    depth += 1;
                } else if token == b">>" {
                    depth -= 1;
                }
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
        is_annot_only,
    }))
}

/// Check that no prior certification declares NO_CHANGES (Check 2).
fn check_prior_docmdp(bytes: &[u8]) -> Result<()> {
    // Read Root /Perms /DocMDP reference
    if let Ok(docmdp_level) = extract_docmdp_perm_level(bytes) {
        if docmdp_level == Some(MdpPerm::NoChanges) {
            return Err(PreflightRefusal::DocumentLockedByPriorSignature {
                level: MdpPerm::NoChanges,
            });
        }
    }

    Ok(())
}

/// Extract DocMDP permission level from Root /Perms /DocMDP.
fn extract_docmdp_perm_level(bytes: &[u8]) -> super::Result<Option<MdpPerm>> {
    let catalog_ref = find_catalog_ref(bytes)?;
    let catalog_obj_slice = find_object(bytes, catalog_ref.0)?;
    if catalog_obj_slice.is_empty() {
        return Ok(None);
    }

    let mut tokenizer = PdfTokenizer::new(catalog_obj_slice);
    let mut key: Option<String> = None;
    let mut perms_ref: Option<(u32, u16)> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if key.is_none() && token_str.starts_with('/') {
                if let Some(name) = token_str.strip_prefix('/') {
                    key = Some(name.to_string());
                }
            } else if let Some(k) = key.take() {
                if k == "Perms" {
                    if let Ok(num_str) = std::str::from_utf8(&token) {
                        if let Ok(num) = num_str.parse::<u32>() {
                            if let Ok(Some(gen_token)) = tokenizer.next_token() {
                                if let Ok(gen_str) = std::str::from_utf8(&gen_token) {
                                    if let Ok(gen) = gen_str.parse::<u16>() {
                                        if let Ok(Some(r_token)) = tokenizer.next_token() {
                                            if r_token == b"R" {
                                                perms_ref = Some((num, gen));
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

    if let Some((perms_obj_num, _)) = perms_ref {
        let perms_obj_slice = find_object(bytes, perms_obj_num)?;
        if !perms_obj_slice.is_empty() {
            let mut perms_tokenizer = PdfTokenizer::new(perms_obj_slice);
            let mut key: Option<String> = None;

            while let Ok(Some(token)) = perms_tokenizer.next_token() {
                if token == b"endobj" {
                    break;
                }

                if let Ok(token_str) = std::str::from_utf8(&token) {
                    if key.is_none() && token_str.starts_with('/') {
                        if let Some(name) = token_str.strip_prefix('/') {
                            key = Some(name.to_string());
                        }
                    } else if let Some(k) = key.take() {
                        if k == "DocMDP" {
                            if let Ok(num_str) = std::str::from_utf8(&token) {
                                if let Ok(num) = num_str.parse::<u32>() {
                                    if let Ok(Some(gen_token)) = perms_tokenizer.next_token() {
                                        if let Ok(gen_str) = std::str::from_utf8(&gen_token) {
                                            if let Ok(_gen) = gen_str.parse::<u16>() {
                                                if let Ok(Some(r_token)) =
                                                    perms_tokenizer.next_token()
                                                {
                                                    if r_token == b"R" {
                                                        // Found DocMDP signature ref, extract its /P value
                                                        return extract_docmdp_p_level(bytes, num);
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
        }
    }

    Ok(None)
}

/// Extract /P value from DocMDP signature dictionary.
fn extract_docmdp_p_level(bytes: &[u8], sig_dict_num: u32) -> super::Result<Option<MdpPerm>> {
    let sig_dict_slice = find_object(bytes, sig_dict_num)?;
    if sig_dict_slice.is_empty() {
        return Ok(None);
    }

    let mut tokenizer = PdfTokenizer::new(sig_dict_slice);
    let mut key: Option<String> = None;
    let mut in_transform_params = false;

    while let Ok(Some(token)) = tokenizer.next_token() {
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
                        if let Ok(level_str) = std::str::from_utf8(&token) {
                            if let Ok(level) = level_str.parse::<u8>() {
                                return Ok(match level {
                                    1 => Some(MdpPerm::NoChanges),
                                    2 => Some(MdpPerm::FillForms),
                                    3 => Some(MdpPerm::Annotate),
                                    _ => None,
                                });
                            }
                        }
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
    bytes: &[u8],
    target_field: &str,
    field_infos: &[(u32, FieldInfo)],
) -> Result<()> {
    // Collect all signed fields (existing signatures)
    let signed_fields: Vec<_> = field_infos
        .iter()
        .filter(|(_, info)| info.field_type == "Sig" && info.has_signature)
        .collect();

    for (field_obj_num, field_info) in signed_fields {
        // Extract FieldMDP locks from this signature
        if let Ok(Some((locked_fields, action))) = extract_field_mdp_locks(bytes, *field_obj_num) {
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
    bytes: &[u8],
    field_obj_num: u32,
) -> super::Result<Option<(Vec<String>, FieldMdpAction)>> {
    let field_obj_slice = find_object(bytes, field_obj_num)?;
    if field_obj_slice.is_empty() {
        return Ok(None);
    }

    // Get the /V reference (signature dictionary)
    let mut tokenizer = PdfTokenizer::new(field_obj_slice);
    let mut key: Option<String> = None;
    let mut sig_dict_ref: Option<(u32, u16)> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
        if token == b"endobj" {
            break;
        }

        if let Ok(token_str) = std::str::from_utf8(&token) {
            if key.is_none() && token_str.starts_with('/') {
                if let Some(name) = token_str.strip_prefix('/') {
                    key = Some(name.to_string());
                }
            } else if let Some(k) = key.take() {
                if k == "V" {
                    if let Ok(num_str) = std::str::from_utf8(&token) {
                        if let Ok(num) = num_str.parse::<u32>() {
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
            }
        }
    }

    if let Some((sig_dict_num, _)) = sig_dict_ref {
        let sig_dict_slice = find_object(bytes, sig_dict_num)?;
        if !sig_dict_slice.is_empty() {
            return extract_fieldmdp_from_sig_dict(sig_dict_slice);
        }
    }

    Ok(None)
}

/// Extract FieldMDP info from a signature dictionary's /Reference array.
fn extract_fieldmdp_from_sig_dict(
    sig_dict_slice: &[u8],
) -> super::Result<Option<(Vec<String>, FieldMdpAction)>> {
    let mut tokenizer = PdfTokenizer::new(sig_dict_slice);
    let mut key: Option<String> = None;
    let mut in_reference = false;
    let mut in_transform_params = false;
    let mut action: Option<FieldMdpAction> = None;
    let mut locked_fields = Vec::new();
    let mut transform_method: Option<String> = None;

    while let Ok(Some(token)) = tokenizer.next_token() {
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
                        if let Ok(action_str) = std::str::from_utf8(&token) {
                            if let Some(action_name) = action_str.strip_prefix('/') {
                                action = match action_name {
                                    "All" => Some(FieldMdpAction::All),
                                    "Include" => Some(FieldMdpAction::Include),
                                    "Exclude" => Some(FieldMdpAction::Exclude),
                                    _ => None,
                                };
                            }
                        }
                    }
                    "Fields" if token == b"[" => {
                        // Parse field names array
                        while let Ok(Some(field_token)) = tokenizer.next_token() {
                            if field_token == b"]" {
                                break;
                            }
                            if let Ok(field_str) = std::str::from_utf8(&field_token) {
                                if let Some(field_name) = super::parse_pdf_string(field_str) {
                                    locked_fields.push(field_name);
                                }
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

    // Only return if this is a FieldMDP transform
    if transform_method.as_deref() == Some("FieldMDP") {
        if let Some(action) = action {
            return Ok(Some((locked_fields, action)));
        }
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

    // Test helpers: create minimal PDFs for testing

    fn create_minimal_unsigned_pdf() -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog
        let obj1_pos = output.len();
        output.extend_from_slice(b"1 0 obj\n");
        output.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 2: Pages
        let obj2_pos = output.len();
        output.extend_from_slice(b"2 0 obj\n");
        output.extend_from_slice(b"<</Type /Pages /Kids [3 0 R] /Count 1>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 3: Page
        let obj3_pos = output.len();
        output.extend_from_slice(b"3 0 obj\n");
        output.extend_from_slice(b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\n");
        output.extend_from_slice(b"endobj\n");

        // xref and trailer
        let xref_start = output.len();
        output.extend_from_slice(b"xref\n");
        output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
        output.extend_from_slice(format!("1 3\n{:010} 00000 n \n", obj1_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj2_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj3_pos).as_bytes());
        output.extend_from_slice(b"trailer\n");
        output.extend_from_slice(b"<<\n");
        output.extend_from_slice(b"/Size 4\n");
        output.extend_from_slice(b"/Root 1 0 R\n");
        output.extend_from_slice(
            b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
        );
        output.extend_from_slice(b">>\n");
        output.extend_from_slice(b"startxref\n");
        output.extend_from_slice(format!("{}\n", xref_start).as_bytes());
        output.extend_from_slice(b"%%EOF");

        output
    }

    fn create_pdf_with_empty_sig_field() -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog
        let obj1_pos = output.len();
        output.extend_from_slice(b"1 0 obj\n");
        output.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R /AcroForm 4 0 R>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 2: Pages
        let obj2_pos = output.len();
        output.extend_from_slice(b"2 0 obj\n");
        output.extend_from_slice(b"<</Type /Pages /Kids [3 0 R] /Count 1>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 3: Page
        let obj3_pos = output.len();
        output.extend_from_slice(b"3 0 obj\n");
        output.extend_from_slice(b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 4: AcroForm
        let obj4_pos = output.len();
        output.extend_from_slice(b"4 0 obj\n");
        output.extend_from_slice(b"<</Fields [5 0 R] /SigFlags 3>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 5: Signature field (empty, no /V)
        let obj5_pos = output.len();
        output.extend_from_slice(b"5 0 obj\n");
        output.extend_from_slice(b"<</FT /Sig /T (Sig1)>>\n");
        output.extend_from_slice(b"endobj\n");

        // xref and trailer
        let xref_start = output.len();
        output.extend_from_slice(b"xref\n");
        output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
        output.extend_from_slice(format!("1 5\n{:010} 00000 n \n", obj1_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj2_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj3_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj4_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj5_pos).as_bytes());
        output.extend_from_slice(b"trailer\n");
        output.extend_from_slice(b"<<\n");
        output.extend_from_slice(b"/Size 6\n");
        output.extend_from_slice(b"/Root 1 0 R\n");
        output.extend_from_slice(
            b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
        );
        output.extend_from_slice(b">>\n");
        output.extend_from_slice(b"startxref\n");
        output.extend_from_slice(format!("{}\n", xref_start).as_bytes());
        output.extend_from_slice(b"%%EOF");

        output
    }

    fn create_pdf_with_signed_field() -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog
        let obj1_pos = output.len();
        output.extend_from_slice(b"1 0 obj\n");
        output.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R /AcroForm 4 0 R>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 2: Pages
        let obj2_pos = output.len();
        output.extend_from_slice(b"2 0 obj\n");
        output.extend_from_slice(b"<</Type /Pages /Kids [3 0 R] /Count 1>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 3: Page
        let obj3_pos = output.len();
        output.extend_from_slice(b"3 0 obj\n");
        output.extend_from_slice(b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 4: AcroForm
        let obj4_pos = output.len();
        output.extend_from_slice(b"4 0 obj\n");
        output.extend_from_slice(b"<</Fields [5 0 R] /SigFlags 3>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 5: Signature field (with /V reference)
        let obj5_pos = output.len();
        output.extend_from_slice(b"5 0 obj\n");
        output.extend_from_slice(b"<</FT /Sig /T (Sig1) /V 6 0 R>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 6: Signature dictionary (stub)
        let obj6_pos = output.len();
        output.extend_from_slice(b"6 0 obj\n");
        output.extend_from_slice(
            b"<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached>>\n",
        );
        output.extend_from_slice(b"endobj\n");

        // xref and trailer
        let xref_start = output.len();
        output.extend_from_slice(b"xref\n");
        output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
        output.extend_from_slice(format!("1 6\n{:010} 00000 n \n", obj1_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj2_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj3_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj4_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj5_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj6_pos).as_bytes());
        output.extend_from_slice(b"trailer\n");
        output.extend_from_slice(b"<<\n");
        output.extend_from_slice(b"/Size 7\n");
        output.extend_from_slice(b"/Root 1 0 R\n");
        output.extend_from_slice(
            b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
        );
        output.extend_from_slice(b">>\n");
        output.extend_from_slice(b"startxref\n");
        output.extend_from_slice(format!("{}\n", xref_start).as_bytes());
        output.extend_from_slice(b"%%EOF");

        output
    }

    fn create_pdf_with_multiple_empty_sig_fields() -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog
        let obj1_pos = output.len();
        output.extend_from_slice(b"1 0 obj\n");
        output.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R /AcroForm 4 0 R>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 2: Pages
        let obj2_pos = output.len();
        output.extend_from_slice(b"2 0 obj\n");
        output.extend_from_slice(b"<</Type /Pages /Kids [3 0 R] /Count 1>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 3: Page
        let obj3_pos = output.len();
        output.extend_from_slice(b"3 0 obj\n");
        output.extend_from_slice(b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 4: AcroForm
        let obj4_pos = output.len();
        output.extend_from_slice(b"4 0 obj\n");
        output.extend_from_slice(b"<</Fields [5 0 R 6 0 R] /SigFlags 3>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 5: Signature field 1 (empty)
        let obj5_pos = output.len();
        output.extend_from_slice(b"5 0 obj\n");
        output.extend_from_slice(b"<</FT /Sig /T (Sig1)>>\n");
        output.extend_from_slice(b"endobj\n");

        // Object 6: Signature field 2 (empty)
        let obj6_pos = output.len();
        output.extend_from_slice(b"6 0 obj\n");
        output.extend_from_slice(b"<</FT /Sig /T (Sig2)>>\n");
        output.extend_from_slice(b"endobj\n");

        // xref and trailer
        let xref_start = output.len();
        output.extend_from_slice(b"xref\n");
        output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
        output.extend_from_slice(format!("1 6\n{:010} 00000 n \n", obj1_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj2_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj3_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj4_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj5_pos).as_bytes());
        output.extend_from_slice(format!("{:010} 00000 n \n", obj6_pos).as_bytes());
        output.extend_from_slice(b"trailer\n");
        output.extend_from_slice(b"<<\n");
        output.extend_from_slice(b"/Size 7\n");
        output.extend_from_slice(b"/Root 1 0 R\n");
        output.extend_from_slice(
            b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
        );
        output.extend_from_slice(b">>\n");
        output.extend_from_slice(b"startxref\n");
        output.extend_from_slice(format!("{}\n", xref_start).as_bytes());
        output.extend_from_slice(b"%%EOF");

        output
    }
}
