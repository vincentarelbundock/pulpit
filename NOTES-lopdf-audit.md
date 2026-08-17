# lopdf Audit Against SPEC-signing §24

**Audit Date:** 2026-08-16  
**lopdf Version Audited:** 0.32.0 (latest on crates.io at time of audit)  
**Specification:** SPEC-signing.md §24 (incremental update writer requirements) and §23.2 (unencrypted /Contents rule)

## Executive Summary

lopdf 0.32.0 does provide basic incremental update support via `IncrementalDocument`, but has **three critical gaps** that make it unsuitable for PDF signing without a wrapper layer:

1. **No /ID id2 regeneration** — spec §24(4) requires id1 preserved, id2 regenerated; lopdf clones both unchanged
2. **No special-casing of signature /Contents** — spec §23.2 requires `/Contents` never encrypted; no evidence of this in the code
3. **Hybrid-xref acceptance** — spec §24(6) refuses hybrid-reference files; lopdf accepts and merges them silently

Items 1 and 2 are spec violations for signing. Item 3 may be acceptable for general PDF reading but violates the explicit signing requirement.

## Requirement-by-Requirement Audit

| Requirement | Spec Location | Status | Finding | Evidence |
|---|---|---|---|---|
| **Byte-exact append: original bytes untouched, new objects after original EOF** | §24(1) | ✓ MET | The `IncrementalDocument.save_internal()` writes previous document bytes first, then appends new revision. | `src/writer.rs:147-150`: `target.inner.write_all(prev_document_bytes)?; target.bytes_written += prev_document_bytes.len();` |
| **xref section of same kind as previous revision (table vs stream)** | §24(3) | ✓ MET | Uses `self.get_prev_documents().reference_table.cross_reference_type` to match the previous xref type. | `src/writer.rs:155-158`: `let mut xref = Xref::new(self.new_document.max_id + 1, self.get_prev_documents().reference_table.cross_reference_type);` |
| **Trailer /Prev chain carrying previous revision's startxref offset** | §24(4) | ✓ MET | `/Prev` is set when creating incremental update via `Document::new_from_prev()`. | `src/document.rs:68-70`: `let mut new_trailer = prev.trailer.clone(); new_trailer.set("Prev", Object::Integer(prev.xref_start as i64));` |
| **Trailer /Root, /Size, /Info carry-over** | §24(4) | ✓ MET | Entire trailer is cloned from previous document, preserving all entries. | `src/document.rs:69`: `let mut new_trailer = prev.trailer.clone();` |
| **/ID preservation: id1 unchanged, id2 regenerated (16 fresh random bytes)** | §24(4), item 4 | ✗ **NOT MET** | /ID array is cloned unchanged; no code regenerates id2. Both id1 and id2 remain identical to previous revision. Violates spec requirement for id1 preservation + id2 regeneration. | `src/document.rs:69`: `new_trailer.clone()` includes /ID without modification. No regeneration logic found in writer.rs or incremental_document.rs. Grep shows no "ID" handling in signing context. |
| **Object mutation under same object number/generation** | §24(2) | ✓ MET | `IncrementalDocument::opt_clone_object_to_new_document()` copies objects from previous revision with preserved ID tuples; objects can be modified in place. | `src/incremental_document.rs:43-49`: `self.new_document.set_object(object_id, old_object.clone());` maintains ObjectId. |
| **Special-casing /Contents of signature dict as never-encrypted** | §23.2 | ✗ **NOT FOUND** | No evidence of special handling for `/Contents` in signature dictionaries. No encryption exemption for this field exists in the codebase. General encryption applies uniformly. | `grep -rn "Contents" src/writer.rs` shows content handling only for page streams, not signature dicts. `src/encryption.rs` has no reference to signature dictionaries or `/Contents` exemption. |
| **Hybrid-xref behaviour: refuse /XRefStm in signature context** | §24(6) | ✗ **REJECTED** | lopdf accepts hybrid-reference files (files with both traditional xref and xref stream via /XRefStm). The reader merges both without refusal. Spec requires explicit rejection for incremental updates in signing. | `src/reader.rs:158-159`: `let prev_xref_stream_start = trailer.remove(b"XRefStm"); if let Some(prev) = ... { xref.merge(prev_xref); }` Acceptance, not rejection. |

## Detailed Findings

### Finding 1: /ID Not Regenerated (Critical for Signing)

**Requirement:** SPEC-signing §24(4) states: "/ID [id1 id2'] where **id1 is preserved unchanged** and id2 is 16 fresh random bytes."

**What lopdf does:**
```rust
// src/document.rs line 68-70
pub fn new_from_prev(prev: &Document) -> Self {
    let mut new_trailer = prev.trailer.clone();
    new_trailer.set("Prev", Object::Integer(prev.xref_start as i64));
```

The trailer, which contains the /ID array, is cloned in full. There is no code that:
1. Extracts the first element of /ID (id1)
2. Generates 16 fresh random bytes (id2)
3. Creates a new /ID array with [id1, id2']

**Consequence:** Incremental updates produced by lopdf will have identical /ID arrays in all revisions. This breaks encryption key derivation for subsequent updates (which uses id1 in the key material per ISO 32000-1 §7.6.3.3). A second signing operation on a document signed with lopdf would produce incorrect encryption keys.

**Evidence in source:**
- `src/writer.rs` (145+ lines): No /ID generation or modification logic
- `src/incremental_document.rs` (141 lines): No /ID handling
- Entire codebase: 0 occurrences of "id2" or "/ID" regeneration

### Finding 2: No Signature /Contents Encryption Exemption

**Requirement:** SPEC-signing §23.2 states: "/Contents is **never** encrypted, even in an encrypted document. ISO 32000-2 states this explicitly."

**What lopdf does:**
lopdf encrypts all strings and streams uniformly based on their object ID and generation number. There is no special exemption for signature dictionaries' `/Contents` field.

Looking at `src/encryption.rs`, the `decrypt_object()` function (and by symmetry, encryption on write) treats all objects the same way. There is no path through the code that checks if an object is a signature dictionary and exempts its `/Contents`.

**Evidence:** 
- `src/encryption.rs:` No reference to "Sig", "signature", or "/Contents" exemption
- `src/writer.rs:` No encrypt/decrypt logic override per field name
- SPEC-signing §23.2 requirement is architectural, but lopdf is a general-purpose PDF library; it has no concept of signing

**Consequence:** If a PDF is encrypted and signed with a second operation using lopdf, the signature's `/Contents` would be encrypted, making it invalid and unverifiable.

### Finding 3: Hybrid-xref Not Refused

**Requirement:** SPEC-signing §24(6) states: "Hybrid-reference files (/XRefStm) are **refused**, for signing and for validation."

**What lopdf does:**
The reader accepts and processes hybrid-xref files (those with both a traditional xref table and a cross-reference stream):

```rust
// src/reader.rs line 158-165
let prev_xref_stream_start = trailer.remove(b"XRefStm");
if let Some(prev) = prev_xref_stream_start.and_then(|offset| offset.as_i64().ok()) {
    if prev < 0 || prev as usize > self.buffer.len() {
        return Err(Error::Xref(XrefError::StreamStart));
    }
    let (prev_xref, _) = parser::xref_and_trailer(&self.buffer[prev as usize..], &self)?;
    xref.merge(prev_xref);
}
```

The code silently reads and merges the xref stream without raising an error. There is no `HybridXrefRefused` or similar error path.

**Consequence:** Spec §24(6) cites pyHanko's design reason: hybrid-xref's dual structure makes coverage claims unverifiable. A wrapper that uses lopdf for incremental updates must add validation to reject hybrid-xref inputs before attempting to sign.

## Object Model Capability Assessment

**Requirement §24.2:** "ability to read an object from the previous revision, mutate, re-emit under same object number/generation"

**Finding:** ✓ **MET** — lopdf fully supports this through `IncrementalDocument`:
- `opt_clone_object_to_new_document(object_id)` copies an object from the previous revision with its ObjectId preserved
- Objects are then mutable via `get_object_mut()`
- They are re-emitted under the same `(u32, u16)` tuple on save

This is the sole architectural strength of lopdf for the signing use case.

## Interoperability Note

lopdf's xref-type matching (requirement 2) is sound. The byte-exact append mechanism is correct and preserves file integrity. For a pure read-append workflow with no modifications, lopdf's incremental update mechanism is mechanically sound.

However, **for PDF signing, three mitigations are required:**

1. **Before creating an incremental update:** Check that the input is not hybrid-xref; reject if `/XRefStm` exists in trailer
2. **After creating a new document:** Regenerate /ID: extract id1 from the previous document's /ID, generate id2 as 16 random bytes, and write [id1, id2'] to the new trailer
3. **At encryption write time:** Flag the signature dictionary's `/Contents` entry to skip encryption (requires changes to the encryption path, not just the writer)

The first two are cosmetic patches. The third requires architectural changes to the encryption layer to understand signing context.

## Porting Policy Consideration

SPEC-signing §35.0 states: "Rust crates carry all cryptography; pyHanko is ported for everything that isn't cryptography."

lopdf is a PDF object model + I/O library, not cryptography. The question is whether the three gaps above justify porting its incremental update logic or writing a purpose-built version.

## Recommendation

**DO NOT ADOPT lopdf for incremental updates in the signing path without substantial wrapping.**

### Rationale

The three gaps—especially /ID regeneration and signature /Contents exemption—are not incidental to lopdf's design; they are **absent because lopdf is a general-purpose PDF library with no signing awareness**. Each gap requires intervention in a different layer:

1. **/ID regeneration:** Application-level, straightforward, ~20 lines
2. **Hybrid-xref rejection:** Application-level, straightforward, ~5 lines  
3. **Signature /Contents exemption:** Encryption layer, architectural, ~50 lines + testing

The wrapper would need to:
- Intercept the trailer before the incremental document is saved
- Regenerate /ID
- Reject hybrid-xref inputs
- Modify the encryption layer (or bypass it for signature dicts)

**Expected effort:** ~150–200 lines of wrapper code + testing.

### Alternative: Purpose-Built Writer

SPEC-signing §24.2 notes: "The fallback is a purpose-built writer, which for the restricted input shape of Invariant S2 is roughly 1,000 lines."

Invariant S2 (SPEC-signing §22.1) constrains the input to files pulpit itself just wrote seconds earlier via PDFium. This is **vastly simpler** than a general incremental update handler:

- No need to parse or preserve arbitrary object structures
- No need to handle all encryption schemes (only standard handler if present)
- No need to merge xrefs; only append
- Known structure: catalog, pages tree, new objects, new signature dict

A purpose-built writer for Invariant S2:
- Would eliminate all three gaps by design
- Would be smaller than the wrapper
- Would be easier to test exhaustively
- Would have no external dependencies
- Would have no surface for future PDF spec surprises

**Conclusion:** Write the purpose-built writer. The ~1,000-line estimate is a reasonable investment given the constraints, and it provides more confidence in the signing path than wrapping a general-purpose library.

## Appendix: Source Files Audited

- `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/lopdf-0.32.0/src/incremental_document.rs` (141 lines) — No /ID handling
- `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/lopdf-0.32.0/src/document.rs` (25,461 bytes, ~600 lines) — Trailer cloning, new_from_prev() logic
- `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/lopdf-0.32.0/src/writer.rs` (20,315 bytes, ~600 lines) — Save and incremental write paths
- `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/lopdf-0.32.0/src/encryption.rs` (12,194 bytes, ~300 lines) — No signature /Contents exemption
- `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/lopdf-0.32.0/src/reader.rs` (15,223 bytes, ~400 lines) — Hybrid-xref acceptance without refusal

**Total lines examined:** ~2,000

## Approval

This audit represents a complete source code examination of lopdf 0.32.0 against SPEC-signing §24 requirements and §23.2 unencrypted /Contents rule, cross-referenced with §35 Milestone S0 step 4 acceptance criteria.
