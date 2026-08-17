# pulpit cryptographic signature specification

Companion to `SPEC-document.md`. This document specifies the cryptographic
signing and signature-verification feature, which `SPEC-document.md` §3.3
defers. It supersedes nothing in `SPEC-document.md`; it adds §20–§36 below,
the numbering leaving room for that specification to grow.

Adapted from pdfform's `SPEC-SIGNING.md` during the fold (`SPEC-document.md`
§14.1). The mechanics — byte ranges, CMS, verification, the deferrals of §36 —
carry over unchanged. What changed is the host: section references point into
`SPEC-document.md`, the export pipeline is §11.3's Save As, the visual-item
model is native annotations, and §31.3's append-only mode is reconciled with
invariant A9, which warns on — rather than forbids — editing a signed
document.

The design is derived from a close reading of **pyHanko** (MIT, Matthias
Valvekens), the most complete open implementation of PAdES. Section references
of the form `pyhanko: sign/signers/pdf_byterange.py:52` point at the code the
requirement was read off. pyHanko is a *reference and a source*, not a
dependency: its file-format decisions, its size-estimation strategy, its
interruption protocol, and above all its validation model are adopted
deliberately, and porting its logic into Rust is explicitly permitted — pyHanko
is MIT-licensed, so ports are derivative works and its copyright notice goes
into `LICENSES/` with a line in `LICENSES/README.md` naming the derived parts.
The same applies to `certomancer` (MIT, same author), used for test PKI. The
division of labour is fixed by §35's porting policy: crates carry the
cryptography; pyHanko is ported for everything that is judgement rather than
cryptography. The line numbers were read from a specific checkout and will
drift; before Milestone S0 starts, pin the exact pyHanko commit in the
repository (a `NOTES` file or the CI oracle's lockfile) so the citations stay
resolvable.

**Scope note.** An earlier draft of this document specified the full PAdES ladder
through B-LTA, certificate path validation, revocation checking, incremental
difference analysis, and PKCS#11 tokens. That draft was not deliverable: the
subsystems it treated as single bullet points are, in pyHanko, 11,711 lines of
certificate path validation and 3,126 lines of difference analysis sitting on top
of 2,658 lines of revision-scoped PDF reading — and the first of those has no
usable Rust equivalent. This document specifies what pulpit will actually build.
§36 records everything that was cut, why, and what each piece would cost, so the
deferral is a decision rather than an oversight.

---

## 20. Product definition for signing

### 20.1 What this feature is

The ability to apply a **cryptographic digital signature** to a PDF document
using a private key held by the user, such that any standards-compliant viewer
(Acrobat, Foxit, Okular, LibreOffice Draw) reports the document as signed, names
the signer, and detects post-signature tampering.

The identity claim is **corroborated by those other viewers, not by pulpit**.
See §20.3.

### 20.2 Three distinct states, never two

`SPEC-document.md` §1 requires that the application never imply that a visible
handwritten signature is a cryptographic digital signature. Adding real signing
turns that binary distinction into a ternary one, and every surface that
mentions "signature" must respect it:

| State | Meaning | UI treatment |
|---|---|---|
| **Ink mark** | A drawn appearance. No cryptography. | Never called "signed". Labelled "handwritten mark". |
| **Signed, identity not verified** | Valid CMS, intact byte range, coverage understood — but pulpit has not established that the certificate belongs to who it says. | "Signed by X — identity not verified by pulpit", with the certificate fingerprint. |
| **Broken** | Byte range does not verify, CMS does not verify, or coverage is unclear. | "Signature is not valid", with the specific reason. |

### 20.3 pulpit does not validate identity

**This release performs no certificate path validation, no revocation checking,
and ships no trust store.** Every signature pulpit verifies lands in the middle
state above. This is a deliberate, permanent-until-revisited decision, for two
reasons:

1. RFC 5280 path building with time-shifted validation is an enormous subsystem
   with no permissive Rust implementation suitable for document signing
   (§36.1). Writing a partial one would be worse than writing none: a path
   validator that is subtly wrong produces confident, false "verified" badges.
2. The middle state is the honest and *common* case anyway. Users signing their
   own documents with their own credential — the actual use case — are served
   completely by integrity verification plus a fingerprint they can compare.

The UI consequence is absolute: **pulpit never displays a green check, a
"trusted" badge, or the word "verified" unqualified.** It reports what it
checked and what it did not.

### 20.4 Non-goals

- Issuing certificates, or acting as a CA.
- Cloud signing services (CSC/eIDAS remote signing).
- XAdES or XML signatures.
- Signature-based access control (`/Perms /UR3`, usage rights).
- Editing, form-filling, or annotating a document that already contains a
  signature. Countersigning into an existing empty signature field *is*
  supported; see §31.3.
- Everything in §36.

---

## 21. Standards and profiles

### 21.1 Normative references

| Standard | Role |
|---|---|
| ISO 32000-1 §12.8, ISO 32000-2 §12.8 | PDF signature dictionaries, `/ByteRange`, DocMDP, FieldMDP, seed values |
| RFC 5652 | CMS `SignedData` |
| RFC 5035 | ESS `signingCertificateV2` |
| RFC 6211 | CMS algorithm protection attribute |
| RFC 3161 / RFC 5816 | Timestamp protocol |
| ETSI EN 319 142-1 | PAdES baseline profiles B-B / B-T |
| ISO/TS 32001, 32002 | SHA-3/SHAKE256 and EdDSA extensions to PDF 2.0 |

### 21.2 Profiles pulpit produces

Two `/SubFilter` values are supported; nothing else is ever written.

- **`/adbe.pkcs7.detached`** — the legacy Adobe profile. Maximum viewer
  compatibility. Default when the user expresses no preference, matching
  pyHanko (`pyhanko: sign/signers/constants.py:26`).
- **`/ETSI.CAdES.detached`** — the PAdES profile. Recommended in the UI whenever
  a timestamp authority is configured.

`/ETSI.RFC3161` is written only if document timestamps are added later (§36.3).
`/adbe.x509.rsa_sha1` is never produced and is reported as unsupported on read.

### 21.3 Profile ladder

| Profile | Requires | Status |
|---|---|---|
| B-B | Signature only | Milestone S1 |
| B-T | + RFC 3161 signature timestamp as an unsigned attribute | Milestone S2 |
| B-LT, B-LTA | + DSS, revocation data, archival timestamps | Deferred, §36.3 |

Documents pulpit produces are forward-compatible with B-LT/B-LTA: the upgrade is
a pure append and can be performed later, by pulpit or by another tool, without
the signing key.

### 21.4 Version and extension declaration

Writing a PAdES signature forces the output version to at least PDF 1.7, and,
for outputs below PDF 2.0, registers the ESIC developer extension
(`/ESIC`, base `/1.7`, level 1) in the catalog's `/Extensions`
(`pyhanko: sign/signers/pdf_signer.py:476`). Using SHA-3 or SHAKE256 forces
PDF 2.0 and registers ISO/TS 32001; Ed25519 registers ISO/TS 32002; Ed448
registers both; ECDSA over the six curves named in ISO/TS 32002 registers 32002
(`pyhanko: sign/signers/pdf_signer.py:1188`).

---

## 22. Architecture

### 22.1 Why PDFium cannot do this

PDFium exposes no signing API, and — critically — **its save path rewrites the
document**. A signed PDF's integrity depends on the exact bytes preceding the
signature remaining untouched. Therefore:

> **Invariant S1.** Once a document carries a signature, PDFium is never used to
> write it again.

> **Invariant S2.** Signing is a byte-level operation applied *after* the
> Save As pipeline of `SPEC-document.md` §11.3 has produced and validated a
> complete PDF (its steps 1–7). The signer consumes the validated temporary
> file and appends to it, before the atomic rename of step 8.

Invariant S2 is also the feature's biggest simplification, and it is worth
stating positively: **pulpit only ever appends to a file it wrote itself,
seconds earlier, with a known structure.** Much of pyHanko's bulk exists to
handle arbitrary third-party PDFs on the write path. pulpit's writer has one
input shape to support.

PDFium remains in the loop for two things: rendering the signed result for the
post-save verification pass (`SPEC-document.md` §11.3 steps 3–7), and rendering the signature
widget's appearance for on-screen display.

### 22.2 Modules, not crates

**No new crate is added for this feature.** The signing code lives in the
existing `pulpit-render` crate — the crate that already owns the PDF domain —
as three top-level modules:

```text
crates/pulpit-render/src/
  sign/       cryptography: CMS, X.509 parsing, TSA, key sources
  pdfwrite/   minimal PDF object model + incremental update writer
  verify/     signature discovery, coverage, integrity, status
```

The `sign` module has no PDF knowledge beyond "here is a digest, here is a CMS
blob". The `pdfwrite` module has no cryptographic knowledge beyond "reserve this
many bytes and tell me the offsets". This separation is what makes the
interruption protocol of §29 possible, and it mirrors pyHanko's split between
`pdf_byterange.py` and `pdf_cms.py`.

The knowledge separation is normative at the module boundary: `sign` MUST NOT
depend on `pdfwrite` or `verify`, and `pdfwrite` MUST NOT depend on `sign`.
Module placement inside `pulpit-render` does not change process placement —
§22.4 still applies: these modules run in the supervisor, never in the PDFium
worker, and none of them may touch the PDFium bindings.

### 22.3 Dependencies

All permissive (MIT or Apache-2.0 or looser). No GPL/AGPL/LGPL anywhere in the
signing path.

| Concern | Crate | License |
|---|---|---|
| CMS `SignedData` | `cms` | Apache-2.0 OR MIT |
| X.509 parsing | `x509-cert` | Apache-2.0 OR MIT |
| DER | `der` | Apache-2.0 OR MIT |
| RSA (PKCS#1 v1.5, PSS) | `rsa` | MIT OR Apache-2.0 |
| ECDSA P-256/384/521 | `p256`, `p384`, `p521` | Apache-2.0 OR MIT |
| Ed25519 | `ed25519-dalek` | BSD-3-Clause |
| Digests | `sha2` | MIT OR Apache-2.0 |
| SHA-3 / SHAKE256 (ISO 32001, Ed448) | `sha3` | MIT OR Apache-2.0 |
| Key containers | `pkcs8`, `pkcs12` (or `p12`) | Apache-2.0 OR MIT |
| HTTP for TSA | `ureq` | MIT OR Apache-2.0 |
| Secret hygiene | `zeroize` | Apache-2.0 OR MIT |

`cms` is at `0.3.0-pre` at time of writing; pin `0.2.x` until 0.3 is released,
and isolate its use behind the `sign` module's own types so the upgrade is
local.

Two gaps the table does not cover:

- **RFC 3161 structures.** No listed crate parses `TimeStampReq`/`TimeStampResp`.
  RustCrypto's `x509-tsp` exists but is immature; audit it at the start of
  Milestone S2, and budget for hand-rolling the two structures on `der` (they
  are small) if the audit rejects it.
- **Ed448.** There is no mature, permissively licensed Ed448 crate. This is the
  concrete reason §26.2 marks Ed448/SHAKE256 support as optional and droppable.

Note what is *absent*: no path validation crate, no OCSP crate, no PKCS#11. See
§36.

### 22.4 Process placement

The `verify` module — signature discovery, the xref/`startxref` bookkeeping of
§28.2, and CMS parsing — runs in the **supervisor**, not the PDFium worker. It
is a parser of hostile input (§34.4 calls it attacker-reachable on any opened
file), and the worker boundary of `SPEC-document.md` §12 exists precisely to contain
such parsers, so this placement needs justification rather than silence:

1. The worker boundary contains *native* code that crashes on malformed input.
   The `verify` module and the parsing paths of `sign` are 100% safe Rust:
   `pulpit-render` as a whole cannot forbid unsafe code (the PDFium bindings
   need it), so each of the three modules carries a module-level
   `#![forbid(unsafe_code)]` inner attribute instead, and their failure mode
   is a typed error, not memory corruption.
2. §30.2's rule — a process that parses hostile input must not hold keys — is
   satisfied by *ordering*, not by process separation: verification of a
   third-party file involves no key material at all, and the post-sign
   verification of §32 begins only after §30.2 has zeroized and dropped the
   key. At no instant does the supervisor both hold a private key and parse
   bytes it did not itself just write.

Both modules are fuzzed per §34.4; that is the load-bearing mitigation and the
reason the `forbid(unsafe_code)` attributes are a requirement, not a style
preference.

---

## 23. Byte-range mechanics

This is the load-bearing part of the feature. Everything else can be wrong and
recoverable; this cannot.

### 23.1 The problem

A signature covers the whole file except its own `/Contents` value. The
`/ByteRange` array that describes that hole must itself be inside the covered
region, and both it and `/Contents` must be written before their final values
are known. The only workable approach is **fixed-width placeholders overwritten
in place**, so that no offset ever shifts.

### 23.2 Placeholder writing

When serialising the signature dictionary:

1. `/ByteRange` is emitted as the two bytes `[]` followed by exactly **60**
   spaces (`pyhanko: sign/signers/pdf_byterange.py:37,73`). The stream offset of
   the `[` is recorded. 62 bytes is comfortably more than the longest plausible
   `[0 nnnnnnnnnn nnnnnnnnnn nnnnnnnnnn]` for files below 2^31 bytes; the writer
   asserts the final rendering fits and fails loudly if it does not.
2. `/Contents` is emitted as `<`, then `bytes_reserved` ASCII `0` characters,
   then `>`. The offsets of the `<` and of the byte just past the `>` are
   recorded as `(sig_start, sig_end)`
   (`pyhanko: sign/signers/pdf_byterange.py:105`).
3. `bytes_reserved` must be even, since it is a hex encoding of an octet string.

`/Contents` is **never** encrypted, even in an encrypted document. ISO 32000-2
states this explicitly; ISO 32000-1 left it ambiguous
(`pyhanko: sign/signers/pdf_byterange.py:202`). pulpit's writer must special-case
this entry.

### 23.3 Offset back-patching and digest

After the complete document has been written to the output stream:

```text
eof     := stream length
range   := [0, sig_start, sig_end, eof - sig_end]
```

Seek to the recorded `/ByteRange` offset and overwrite with
`[0 <sig_start> <sig_end> <eof - sig_end>]`, left-aligned, remaining bytes
staying as spaces (`pyhanko: sign/signers/pdf_byterange.py:52`). The overwrite
spans the **full 62-byte region**, not just the placeholder's `[]` — the
placeholder's closing bracket at offset+1 is overwritten and the new closing
bracket lands wherever the rendering ends:

```text
before:  [](60 spaces......................................................)
after:   [0 495276 505518 1082](40 spaces..................................)
```

Then digest `stream[0..sig_start]` followed by `stream[sig_end..eof]` with the
selected message digest. For a memory-mapped or in-memory buffer, slice
directly; otherwise stream in `chunk_size` blocks. The result is the **document
digest** and it is the only input the cryptographic layer needs.

### 23.4 Filling the reservation

Given DER-encoded CMS bytes `c`:

```text
bytes_reserved := sig_end - sig_start - 2
hex            := uppercase_hex(c)
if len(hex) > bytes_reserved: fail (SignatureTooLarge)
seek(sig_start + 1); write(hex)
padding        := bytes_reserved/2 - len(c)     // trailing NUL octets
```

The padding is *not* written — the placeholder `0` characters remain — but the
notional value of `/Contents` is `c ++ [0u8; padding]`
(`pyhanko: sign/signers/pdf_byterange.py:171`). Keep this in mind if DSS support
is ever added: the VRI index key is a hash over the *padded* contents
(`pyhanko: sign/validation/dss.py:179`), and it is a silent-failure trap.

The reserved region is deliberately over-sized, so trailing zero bytes after the
DER SEQUENCE are normal and every conforming parser stops at the DER length.

Bytes inside the reservation are, by construction, **not integrity-protected**:
the byte range excludes them and the DER parser ignores everything past the
declared length, so flipping a padding character invalidates nothing — in
pulpit, Acrobat, or pyHanko. This is inherent to the format, and it is why
acceptance criterion 21 speaks of modifying a *signed* byte.

### 23.5 Size estimation

`bytes_reserved` must be chosen before the digest exists, so pulpit performs a
**dry run**: it builds the complete CMS object with a placeholder signature of
the correct length, a zero digest, and — if a timestamp is requested — a real
`dummy_response` fetched once from the TSA and cached
(`pyhanko: sign/signers/pdf_signer.py:2071`).

```text
test_len       := 2 * len(DER(dry_run_cms))
bytes_reserved := test_len + 2*(test_len/4)      // +50%, rounded even
```

The 50% margin exists because external TSAs do not return responses of stable
length. A `tight_size_estimates` option drops the margin; it is only safe when
no external party contributes to the container and the mechanism has fixed-size
output. pulpit exposes it only in the CLI, never in the GUI.

If the estimate is nonetheless too small, signing fails cleanly with
`SignatureTooLarge` and the temporary output is discarded. **The source and the
exported file are unaffected**; the operation is retried with a doubled
reservation, at most twice, before surfacing an error.

---

## 24. Incremental update writer

The `pdfwrite` module must produce a byte-exact append. Its input is always a file
pulpit itself just wrote via PDFium (Invariant S2), which bounds the structures
it must handle.

1. The original bytes are copied verbatim, or the file is opened for append and
   never rewritten. Not one byte before the original EOF may change.
2. New and modified objects are serialised after the original EOF.
3. A cross-reference section follows, of the **same kind as the previous
   revision**: a classic `xref` table if the document used tables, a
   cross-reference stream if it used streams.
4. The trailer (or xref stream dictionary) carries:
   - `/Prev` = the previous revision's `startxref` offset
     (`pyhanko: pdf_utils/incremental_writer.py:186`);
   - `/Root`, `/Size`, and `/Info` if present;
   - `/ID [id1 id2']` where **`id1` is preserved unchanged** and `id2` is 16
     fresh random bytes. `id1` participates in the standard security handler's
     key derivation and must never be regenerated for an update
     (`pyhanko: pdf_utils/incremental_writer.py:103`).
5. `startxref` and `%%EOF` terminate the update.
6. Hybrid-reference files (`/XRefStm`) are **refused**, for signing and for
   validation. Their dual xref structure makes coverage claims unverifiable
   (`pyhanko: sign/signers/pdf_signer.py:1252`,
   `sign/validation/pdf_embedded.py:510`). PDFium does not produce them, so this
   only affects validation of third-party files.
7. If the source is encrypted, the update must reuse the source's encryption
   settings and the credential must still be held. Encryption cannot be removed
   by an incremental update.

### 24.2 Object model requirements

The writer needs enough of a PDF object model to *modify* the catalog and the
AcroForm without reserialising them wholesale: read an object from the previous
revision, mutate it, and re-emit it under the same object number with the
generation preserved.

`lopdf` (MIT) is the candidate. Its incremental-update support must be audited
against the requirements above — in particular §24(4)'s `/ID` rule and §23.2's
unencrypted-`/Contents` rule, both of which are easy for a general-purpose
library to get wrong — before adoption. The fallback is a purpose-built writer,
which for the restricted input shape of Invariant S2 is roughly 1,000 lines.
**This audit is the first task of Milestone S1**, because the answer determines
the milestone's size.

---

## 25. Signature fields and dictionaries

### 25.1 Signature field

A `/Sig` form field, either freshly created or an existing empty one selected by
name. The field dictionary:

```text
/FT /Sig
/T  (field name)
/TU (human-readable name)          optional, tooltip + accessibility
/V  <indirect ref to sig dict>     set at signing time
/Lock <dict>                       optional, see §25.4
```

The widget annotation is by default *merged into* the field dictionary
(`combine_annotation = true`), which is what most producers do and what most
consumers expect. When merging is disabled, the field gets a single-element
`/Kids` array. Code that reads an annotation from a field must handle both
shapes and reject fields with more than one kid
(`pyhanko: pdf_utils/form_tools.py:23`).

Widget entries:

```text
/Type /Annot  /Subtype /Widget
/Rect [...]                        [0 0 0 0] if invisible
/P    <page ref>
/F    <flags>
/AP   << /N <form xobject> >>      appearance, see §25.5
```

Annotation flags: bit 8 (value 128, "Locked") is always set. For an invisible
field, Print (4) is set by default for PDF/A compatibility, and Hidden (2) is an
opt-in for PDF/UA; the two are mutually exclusive in practice, and the third
escape hatch is to place the zero-size box at (-9999,-9999) outside the crop box
(`pyhanko: sign/fields.py:1211,1684`). For a visible field, Print is set, and
NoZoom (8) / NoRotate (16) are set when the user disables scaling or rotation
with the page.

### 25.2 AcroForm

Creating or filling a signature field requires:

- `/AcroForm` present in the catalog, with a `/Fields` array;
- `/SigFlags` set to **3** (SignaturesExist | AppendOnly);
- `/NeedAppearances` **removed** if present (`pyhanko: sign/fields.py:1437`).

`/NeedAppearances` is deleted rather than set to false: leaving it instructs
viewers to regenerate field appearances, which changes rendering of a signed
document and is a well-known source of "document has been altered" reports.

Field names are dotted paths; a name containing `.` creates or traverses
intermediate non-terminal field nodes with `/Kids` and `/Parent` links
(`pyhanko: sign/fields.py:1386`).

### 25.3 Signature dictionary

```text
/Type          /Sig
/Filter        /Adobe.PPKLite
/SubFilter     /adbe.pkcs7.detached | /ETSI.CAdES.detached
/ByteRange     [0 a b c]                    placeholder, §23
/Contents      <hex...>                     placeholder, §23
/M             (D:YYYYMMDDHHmmSS+HH'mm')    claimed signing time
/Name          (signer)                     optional; omit to let viewers use the cert
/Location      (text)                       optional
/Reason        (text)                       optional
/ContactInfo   (text)                       optional
/Reference     [ ... ]                      §25.4
/Prop_Build    << /App << /Name /pulpit /REx (version) >> >>
```

`/Name` is left unset by default: viewers should derive the displayed name from
the certificate subject, and a mismatch between `/Name` and the subject is a
finding some validators report (`pyhanko: sign/signers/pdf_byterange.py:406`).

`/M` is an unauthenticated claim. The UI must present it as "claimed" unless a
timestamp token corroborates it (§27).

`/Prop_Build` identifies pulpit and its version. It is informational and must
never be trusted on read.

### 25.4 DocMDP and FieldMDP

In v1, pulpit **reads** these — the three pre-flight checks below are load-
bearing for countersigning — but does not **write** them: producing
certification signatures and authoring DocMDP/FieldMDP transforms is deferred
(§36.7). The writing requirements in this section are normative for that
future work. pulpit does not **enforce** them on read either (§28.4). Other
viewers do enforce them, which is the point: they are how a signer communicates
intent to Acrobat and its peers.

**DocMDP (certification).** A document may contain at most one certification
signature and it must be the first signature in the document:

```text
Root /Perms /DocMDP -> <ref to the signature dictionary>

/Reference [ << /Type /SigRef
                /TransformMethod /DocMDP
                /TransformParams << /Type /TransformParams /V /1.2 /P n >> >> ]
```

where `n ∈ {1: no changes, 2: form filling and signing, 3: + annotations}`
(`pyhanko: sign/signers/cms_embedder.py:38`, `sign/fields.py:62`).

Because pulpit countersigns (§31.3), the ordering rule needs three explicit
pre-flight checks, all performed before any byte is written
(`pyhanko: sign/signers/pdf_signer.py:1114`):

1. **Certifying requires an unsigned document.** Enumerate signature fields with
   a non-null `/V`. If any exists, certification is refused with
   `CertificationNotAllowed`. A certification signature must be the first
   signature in the document, and pulpit will not produce a document that
   violates that.
2. **Any signing requires that no prior certification forbids it.** Read
   `Root /Perms /DocMDP`, follow it to the signature dictionary, and extract the
   DocMDP level. If it is `NO_CHANGES`, signing is refused with
   `DocumentLockedByPriorSignature`. At `FILL_FORMS` or `ANNOTATE`, adding a
   signature is permitted — that is precisely what those levels exist to allow.
3. **Filling a signature field requires that no prior signature's FieldMDP
   locks it.** For each existing signature, read its `/Reference` array. A
   `/FieldMDP` transform locks the target field when `/Action` is `/All`, when
   it is `/Include` and `/Fields` names the field, or when it is `/Exclude` and
   `/Fields` does not name it. If any prior signature locks the target field,
   signing is refused with `FieldLockedByPriorSignature`. (The target field's
   *own* `/Lock` dictionary is not a lock against signing it — it describes
   locks that activate when the field is signed — and is covered by the seed
   value / lock handling below.)

**FieldMDP (locking).** Locks named fields against further modification:

```text
/Reference [ << /Type /SigRef
                /TransformMethod /FieldMDP
                /Data <ref to catalog>
                /TransformParams << /Type /TransformParams /V /1.2
                                    /Action /All | /Include | /Exclude
                                    /Fields [ (names) ] >> >> ]
```

The field's own `/Lock` dictionary mirrors this with `/Type /SigFieldLock`.

Two behaviours must be replicated for Acrobat interoperability, both deviations
from a literal reading of the spec, both adopted consciously by pyHanko:

1. When a `/Lock` dictionary and a DocMDP permission are both in play, the `/P`
   value is copied into the **FieldMDP** transform params as well
   (`pyhanko: sign/signers/cms_embedder.py:208`, comment: "this is NOT
   spec-compatible, but emulates Acrobat behaviour").
2. For a non-certifying signature that nonetheless declares a DocMDP permission
   level, a `/Lock` dictionary is synthesised with `/Action /Include /Fields []`
   — a lock that locks nothing — purely as a carrier for `/P`
   (`pyhanko: sign/signers/pdf_signer.py:1923`). DocMDP on approval signatures
   is a PDF 2.0 feature and older readers will ignore it.

Seed value dictionaries (`/SV`) are **not** consulted (§36.5). If the selected
field carries one, pulpit reports that it is ignoring it and offers to cancel.

### 25.5 Appearance

A visible signature field's `/AP /N` is a form XObject whose bounding box equals
the widget rect. `/AS` must be deleted if present
(`pyhanko: pdf_utils/content.py:430`).

pulpit reuses its ink appearance generation (`SPEC-document.md` §7.1) to draw the user's
handwritten mark inside the appearance stream, optionally composited with a text
block. This is the one place where the ink model and the cryptographic model
meet, and the UI copy must be exact: the ink is decoration; the CMS is the
signature. A signature field with a drawn appearance and no valid CMS is not a
signature, and pulpit must never produce one.

Default text template, when no ink is supplied:

```text
Digitally signed by %(signer)s.
Timestamp: %(ts)s.
```

An empty visible field is given either an empty appearance stream or a neutral
grey box with a hairline border; ISO 32000-2 requires *some* appearance stream on
a widget, but most viewers substitute their own rendering for unfilled signature
fields anyway (`pyhanko: sign/fields.py:1614`).

---

## 26. CMS construction

### 26.1 Structure

A detached `SignedData` with exactly one `SignerInfo`, no encapsulated content:

```text
ContentInfo
  contentType    id-signedData
  content SignedData
    version           v1
    digestAlgorithms  { the SignerInfo's digest algorithm }
    encapContentInfo  { contentType: id-data, content: ABSENT }
    certificates      signer cert + chain
    signerInfos       { SignerInfo }
```

`SignerInfo`:

```text
version             v1
sid                 issuerAndSerialNumber        (never subjectKeyIdentifier)
digestAlgorithm     the document digest algorithm
signedAttrs         §26.3
signatureAlgorithm  §26.2
signature           over DER(signedAttrs) re-tagged as universal SET OF
unsignedAttrs       §26.4
```

Version is `v1` for content type `data`; the `v3`/`v4` cases
(`pyhanko: sign/signers/pdf_cms.py:141`) arise only with attribute certificates,
which pulpit does not produce.

The certificate set contains the signer's certificate plus whatever chain the
user supplied. Root certificates are included by default but can be omitted
(`embed_roots = false`); when omitting, self-signed certificates are filtered out
of the set (`pyhanko: sign/signers/pdf_cms.py:666`).

**The signature input is `DER(signedAttrs)` with the implicit `[0]` context tag
replaced by the universal `SET OF` tag.** Getting this wrong produces signatures
that verify nowhere. On the verification side, re-encoding must not normalise the
attributes in any other way; tolerate non-DER input from third-party signers
rather than re-serialising (`pyhanko: sign/validation/generic_cms.py:343`).

### 26.2 Mechanism and digest selection

When the user has not chosen explicitly, derive the mechanism from the signing
certificate's public key (`pyhanko: sign/signers/pdf_cms.py:395`):

| Key | Mechanism | Notes |
|---|---|---|
| RSA | `<md>_rsa` (PKCS#1 v1.5) | `rsassa_pss` when PSS is preferred is deferred (§36.8); PSS params derived from key size and digest |
| EC | `<md>_ecdsa` | RFC 5753 requires the digest be encoded into the mechanism OID |
| Ed25519 | `ed25519` | digest fixed at SHA-512 |
| Ed448 | `ed448` | digest fixed at SHAKE256, encoded as `shake256_len` with parameter 512 (RFC 8419) |
| DSA | — | accepted on read, never offered for signing |

Default digest, by key strength (`pyhanko: sign/signers/pdf_cms.py:1677`):

| Key | Digest |
|---|---|
| RSA ≤ 2048 | SHA-256 |
| RSA ≤ 3072 | SHA-384 |
| RSA > 3072 | SHA-512 |
| EC ≤ 256 | SHA-256 |
| EC ≤ 384 | SHA-384 |
| EC > 384 | SHA-512 |
| Ed25519 | SHA-512 |
| Ed448 | SHAKE256 |
| fallback | SHA-256 |

If the chosen mechanism implies a digest (Ed25519, Ed448) that disagrees with the
requested one, fail rather than silently reconcile
(`pyhanko: sign/signers/pdf_cms.py:708`).

SHA-1 is never offered for signing. It is accepted on read with a prominent
"weak algorithm" finding.

Ed448/SHAKE256 and RSA-PSS signing are deferred out of v1 (§36.8); the tables
record the correct behaviour for whenever they are added. Both are accepted on
read.

### 26.3 Signed attributes

Always:

| Attribute | Value |
|---|---|
| `content-type` | `id-data` |
| `message-digest` | the document digest from §23.3 |
| `signing-certificate-v2` | ESS `SigningCertificateV2` over the signer's certificate |

Conditionally:

| Attribute | Condition |
|---|---|
| `signing-time` | **Only for `/adbe.pkcs7.detached`.** PAdES forbids it — the `/M` entry and the timestamp token carry time instead (`pyhanko: sign/signers/pdf_cms.py:1632`). |
| `cms-algorithm-protection` (RFC 6211) | Only for non-PAdES signatures; its interaction with CAdES's attribute rules is unsettled (`pyhanko: sign/signers/pdf_cms.py:1561`). |

The Adobe `adbe-revocationInfoArchival` attribute is not produced (it is the
LTV mechanism, §36.3).

Attributes with the same OID are merged into a single `CMSAttribute` with
multiple values, never emitted twice (`pyhanko: sign/signers/pdf_cms.py:212`).

### 26.4 Unsigned attributes

| Attribute | Content |
|---|---|
| `signature-time-stamp-token` | An RFC 3161 token over `H(signature)` — the hash of the raw `signature` field of the `SignerInfo`, **not** of the document digest (`pyhanko: sign/attributes.py:151`, `sign/signers/pdf_cms.py:1657`). |

This distinction is the difference between B-B and B-T and is a common
implementation error.

---

## 27. Timestamping

### 27.1 TSA client

```text
TimeStampReq {
  version         1
  messageImprint  { hashAlgorithm, hashedMessage }
  nonce           64-bit, high byte forced to 0x01 for fixed width
  certReq         TRUE
}
```

POST to the TSA URL with `Content-Type: application/timestamp-query` and
`Accept: application/timestamp-reply`. On response
(`pyhanko: sign/timestamps/common_utils.py:55`):

1. `status` must be `granted`; otherwise surface `statusString` and `failInfo`.
2. The returned nonce must equal the one sent. A mismatch is a hard failure — it
   is the only replay protection in the protocol.
3. Extract the token. The TSA's certificates come back inside it because
   `certReq` was set (`pyhanko: sign/timestamps/common_utils.py:31`); they are
   embedded in the token and carried along, but pulpit does not validate them
   (§20.3).

The digest algorithm used for the message imprint should match the one used
elsewhere in the signature (RFC 8933). Note the empirical caveat pyHanko
records: some TSAs return invalid tokens for SHA-512 requests
(`pyhanko: sign/signers/constants.py:31`). Default the TSA digest to SHA-256 and
make it configurable.

A **dummy response** is fetched once per digest algorithm and cached, purely to
size the reservation (§23.5). Signing with a timestamp therefore makes two
network calls the first time and one thereafter.

### 27.2 What a timestamp does and does not establish

A timestamp token proves the signature existed at the time the TSA asserts —
*if* the TSA is trustworthy and its certificate is valid. pulpit does not
validate the TSA's certificate (§20.3). The UI states the attested time and names
the TSA, and describes it as "attested by <TSA name>, not verified by pulpit".

This is still strictly more than a `/M` entry, which is an unverifiable local
clock reading, and it is what makes the document upgradeable to B-LT later.

---

## 28. Verification

Verification is not a bonus feature; it is what makes signing meaningful, and it
is the harder half. pulpit must verify what it produces and correctly refuse to
bless what it cannot.

### 28.1 Discovery

Enumerate `/Sig` fields with a non-null `/V`, in document order. For each, record
the revision in which the signature object was **last changed**, not the one in
which it was introduced (`pyhanko: sign/validation/pdf_embedded.py:190`). A
signature object retroactively overwritten in a later revision is an attack, and
using last-change means the coverage check below catches it.

### 28.2 Coverage

Coverage is a purely structural property, computed before any cryptography
(`pyhanko: sign/validation/pdf_embedded.py:438`).

During parse, the reader records the exact byte extent `[c_start, c_end)` of
this signature's `/Contents` string in the file — from the `<` to just past the
`>`. This is a **lexical offset recorded by the tokenizer, not a length
reconstructed from the parsed DER**: the stored string is the padded
reservation of §23.4 and is longer than the DER it contains. Computing the
expected gap from `len(DER)` would misclassify every file pulpit itself
produces.

```text
let [z, len1, start2, len2] = byte_range

if byte_range.len() != 4 || z != 0             -> Unclear
if any value negative, or overflow, or
   start2 + len2 > file_size                   -> Unclear
if len1 != c_start || start2 != c_end          -> Unclear   // gap must coincide
                                                            // exactly with /Contents
if start2 + len2 == file_size                  -> EntireFile
if startxref@(start2+len2) != expected(rev)    -> ContiguousBlockFromStart
if any xref container of revision <= rev ends beyond start2 + len2
                                               -> ContiguousBlockFromStart
otherwise                                      -> EntireRevision
```

The gap-coincidence check runs **before** any classification, `EntireFile`
included. This ordering is load-bearing: a byte range whose gap has the right
size but sits somewhere other than over `/Contents` is an unsigned, freely
mutable window in the file, and an algorithm that returns `EntireFile` on a
size match alone would bless exactly that construction. Here it lands in
`Unclear`, which the UI presents as broken.

```rust
pub enum SignatureCoverage {
    Unclear = 0,
    ContiguousBlockFromStart = 1,
    EntireRevision = 2,
    EntireFile = 3,
}
```

Anything below `EntireRevision` is presented as a broken signature regardless of
what the cryptography says. A signature whose byte range does not cover its own
revision's cross-reference table is the classic incremental-saving attack, and no
amount of valid CMS redeems it.

This check requires the reader to know each revision's `startxref` and the byte
extent of each xref container — a modest amount of bookkeeping during parse, and
far less than the revision-scoped *object* resolution that §36.2 would need.

### 28.3 Cryptographic checks

pulpit writes exactly one `SignerInfo` (§26.1), but third-party files are not
so obliged. On read:

- A `SignedData` with zero or more than one `SignerInfo` is reported as
  **broken**, with "unsupported signature structure: N signers" as the reason
  — matching pyHanko, which requires exactly one.
- The signer certificate is selected from the certificate set by matching the
  `sid`. Both forms are accepted on read — `issuerAndSerialNumber` and
  `subjectKeyIdentifier` — even though pulpit only ever writes the former. If
  no certificate in the set matches, the signature is **broken**, with "signer
  certificate not present in the signature" as the reason; there is nothing to
  verify step 6 against.

For each signature (`pyhanko: sign/validation/generic_cms.py:293`):

1. Recompute the document digest over the declared `/ByteRange`.
2. Verify the `content-type` signed attribute equals `id-data`.
3. Verify the `message-digest` signed attribute equals the recomputed digest.
   This yields `intact`.
4. Verify `cms-algorithm-protection`, if present, agrees with the declared digest
   and signature algorithms. A mismatch is a hard failure.
5. Verify `signing-certificate-v2` matches the selected signer certificate by ESS
   CertID. A mismatch is a hard failure.
6. Verify the signature over `DER(signedAttrs)` using the public key **from the
   embedded certificate**. This yields `valid`.
7. Report, without judging: the signature algorithm, the digest, and any weak
   algorithm findings.

`intact` and `valid` are reported separately. `intact && !valid` means the bytes
are unchanged but the key does not match the certificate; `!intact` means the
document was modified. Users need the distinction, and collapsing it loses the
diagnosis.

For a timestamp token, additionally check the message imprint against the hash of
the signature bytes it claims to timestamp.

**Step 6 uses the embedded certificate with no chain check.** That is the whole
of §20.3 in one line, and every status string must reflect it.

### 28.4 Modifications after signing

A signature with coverage `EntireRevision` but not `EntireFile` has revisions
appended after it. Classifying whether those revisions are benign requires
incremental difference analysis, which is deferred (§36.2).

The fallback pulpit ships is coarse but sound, and is pyHanko's own behaviour
when its diff analysis is skipped (`pyhanko: sign/validation/pdf_embedded.py:338`):

```text
coverage == EntireFile      -> no modifications after signing
coverage == EntireRevision  -> "this document was changed after it was signed;
                               pulpit cannot determine whether the changes were
                               permitted"
otherwise                   -> broken
```

Countersigning (§31.3) produces exactly this state, deliberately: after pulpit
adds a second signature, the *first* signature is reported as changed-and-
unclassifiable, even though the change was pulpit's own well-formed
countersignature. This is not a defect to be worked around by special-casing our
own output — a verifier that trusts a modification because it recognises the
producer is not verifying. Acrobat, which does run the analysis, will report both
signatures precisely; pulpit reports what it can establish.

The declared DocMDP level is **read and displayed** — "the signer permitted:
form filling only" — but not enforced against an observed modification level,
because there is no observed modification level.

> **Requirement V1.** pulpit must never report a partially-covered signature as
> simply valid. The middle line above is a required, prominent status, not a
> footnote.

### 28.5 Status model

```rust
pub struct SignatureStatus {
    pub field_name: String,
    pub signer_subject: String,
    pub signer_cert: CertificateSummary,     // subject, issuer, serial, validity, SHA-256 fingerprint
    pub cert_chain: Vec<CertificateSummary>, // as embedded; unvalidated
    pub coverage: SignatureCoverage,
    pub intact: bool,
    pub valid: bool,
    pub later_revisions: bool,
    pub declared_docmdp: Option<MdpPerm>,    // displayed, not enforced
    pub claimed_time: Option<OffsetDateTime>,   // /M or signing-time
    pub attested_time: Option<AttestedTime>,    // from a timestamp token, with TSA name
    pub algorithm_findings: Vec<AlgorithmFinding>,
    pub identity: IdentityAssurance,
    pub profile: Option<PadesProfile>,       // B-B | B-T
}

/// This release has exactly one inhabitant. The enum exists so that adding
/// chain validation later is an additive change, not a rewrite.
pub enum IdentityAssurance {
    NotVerified { reason: &'static str },    // "pulpit does not perform certificate path validation"
}
```

A single summary line is derived from this, but the detail panel shows every
field. The summary must never be more confident than the weakest component.

---

## 29. Split signing

The signing operation is structured so that digest production and signature
production are separable. In this release both happen in the same process; the
structure exists because it costs nothing now and is the prerequisite for
hardware tokens later (§36.4), and because it makes the "no writes after digest"
rule enforceable by the type system.

```rust
#[derive(Serialize, Deserialize)]
pub struct PreparedByteRangeDigest {
    pub document_digest: Vec<u8>,
    pub reserved_region_start: u64,
    pub reserved_region_end: u64,
}
```

(`pyhanko: sign/signers/pdf_byterange.py:115`.)

A typestate machine mirroring pyHanko's coroutine protocol
(`pyhanko: sign/signers/cms_embedder.py:365`):

```rust
SigningSession           // field created/selected, digest algorithm chosen
  .prepare_tbs()      -> TbsDocument          // sig dict + appearance + MDP written
  .digest()           -> (PreparedByteRangeDigest, Output)
  .sign(cms_bytes)    -> PostSignatureDocument
  .finish()           -> ()
```

Once `digest()` has returned, **no object may be added to or modified in the
writer**. Violating this changes offsets and silently invalidates the signature,
which is why `digest()` consumes `TbsDocument` by value.

`PostSignatureDocument::resume(output, prepared, cms)` is a free function
requiring no session, so a signature obtained elsewhere can be applied to a
document produced in an earlier process.

---

## 30. Key material

### 30.1 Sources

| Source | Notes |
|---|---|
| PKCS#12 (`.p12`/`.pfx`) | Passphrase prompted, never stored, zeroized after use |

PEM/DER key material is deferred (§36.8); hardware tokens and platform
keychains are deferred (§36.4).

### 30.2 Handling rules

- Private keys and passphrases live in `Zeroizing` buffers and are dropped as
  soon as the signature bytes exist.
- Key material **never** enters the recovery journal, the crash log, or any
  telemetry. `SPEC-document.md` §11.4's journal already excludes passwords; this
  extends the rule to signing credentials.
- Key material never crosses into the PDFium worker process. Signing happens in
  the supervisor. The worker's crash-containment boundary exists to survive
  malformed PDFs, and a process that parses hostile input must not hold keys.
- The passphrase prompt is modal, and cancelling it aborts cleanly with the
  source and export untouched.

---

## 31. UI specification

### 31.1 Signing flow

1. The user completes the document normally and chooses **Sign**.
2. If the document already contains a signature, the flow narrows to
   countersigning: only pre-existing empty signature fields are offered as
   targets, and steps 5–6 lose the options that would require modifying content
   (§31.3).
3. If unsaved annotation or field edits exist, the document is saved first via
   Save As (`SPEC-document.md` §11.3) — the
   signature is applied to the saved bytes, never to a mutable working state.
   In the countersigning case there are no unsaved edits by construction, and
   the signature is appended to the file as opened.
4. Credential chooser: subject, issuer, validity window, key usage, SHA-256
   fingerprint, and a prominent warning for anything expired or not yet valid.
5. Options: reason, location, contact, visible or invisible, page and position if
   visible, ink or text appearance, timestamp authority. (Certify vs approve and
   DocMDP level return with §36.7; v1 always produces an approval signature.)
6. Placement, if visible: the same box-drawing interaction as other
   annotations (`SPEC-document.md` §8.4).
7. Confirmation summarising exactly what will be produced, including the profile
   (B-B or B-T) and whether the document will be locked.
8. Sign. Progress is shown for the TSA call, which is the slow step and the one
   that can fail.
9. Verify the result by reopening it (§32) and show the resulting status.

### 31.2 Required disclosures

Two, both non-dismissable and both present in the confirmation of step 7:

- **Identity.** "pulpit verifies that a signature is intact and that it matches
  the certificate embedded in it. It does not check whether that certificate is
  genuine. Other software may or may not accept this signature." With a link to
  a help page explaining what to do about it.
- **Certification.** Certifying with `NO_CHANGES` makes the document permanently
  unfillable and unsignable by anyone else. Irreversible from the recipient's
  point of view, and confirmed with a dialog that says so in those words.

### 31.3 Signed documents: append-only, no content changes

When a document containing any signature is opened, pulpit warns before any
mutation, as `SPEC-document.md` A9 requires, and offers **append-only mode**
as the default answer. The user MAY decline and edit the document as
`SPEC-document.md` specifies; from that point every existing signature is
reported under §28.4 and never as valid after saving — that is A9's other
half, not a contradiction of it. The rest of this section describes
append-only mode.

The distinction append-only mode draws is between *adding a signature*, which
requires no judgement about existing content, and *changing content*, which
does.

Permitted:

- View, verify, and "Export unsigned copy" (which strips signatures and says so).
- **Countersigning**: filling a *pre-existing, empty* `/Sig` field that no
  earlier signature's FieldMDP locks (§25.4 check 3), written as an incremental
  update (§24).

"Export unsigned copy" is the one operation that routes a signed document back
through PDFium's rewriting save path, and Invariant S1 permits it because the
*output* carries no signature — S1 protects signed bytes, and the copy has
none. Mechanism: the `pdfwrite` module first appends an incremental update to a
temporary copy that sets each signature field's `/V` to null and deletes
`Root /Perms /DocMDP` (catalog surgery PDFium's API does not expose); PDFium
then opens that copy and performs a full rewriting save, which flattens the
revision history. The result goes through the normal Save As validation of
`SPEC-document.md` §11.3.

Forbidden, regardless of what the DocMDP level nominally permits:

- Filling or changing any form field value.
- Adding, moving, editing, or deleting any annotation.
- Creating a new signature field. Only fields the document already contains may
  be signed.
- Re-signing a field that already has a `/V`.
- Certifying (§25.4 check 1).
- Signing at all when a prior certification declares `NO_CHANGES`
  (§25.4 check 2).
- Signing a field that an earlier signature's FieldMDP locks (§25.4 check 3).

#### Rationale

Countersigning is the workflow this feature exists to serve — a sender certifies
a document at `FILL_FORMS` with an empty signature field, a recipient signs it,
possibly a second recipient after that. Refusing it would exclude the most common
signing workflow there is.

It is admissible under the constraints of this release because **producing** a
countersignature requires nothing pulpit lacks: the field already exists, the
append is the same §23–§26 machinery, and no existing object's meaning is
changed. Difference analysis (§36.2) is needed to *verify* the result, not to
produce it, and only for the earlier signature — a job the recipient's Acrobat
will do correctly.

Creating a *new* signature field is excluded even though the standards permit it
under `FILL_FORMS`, because it modifies the AcroForm and a page's annotation
array. That is a content change, and the line this section draws is content
changes, not signature counts.

#### Required disclosure

Before a countersignature, the confirmation dialog states: "This document already
contains a signature. Adding yours will cause some software — including pulpit —
to report that the document changed after the earlier signature was made. This is
expected. Software that analyses the change in detail, such as Acrobat, will
report both signatures correctly."

Without this, §28.4's honest-but-alarming verdict on the first signature reads as
a bug the user just caused.

#### Consequence

The receive-a-form-then-fill-and-sign workflow works only when the sender left
the form fields *unfilled and unsigned* — that is, when the document is not yet
signed at all, and pulpit fills and signs in one pass (§31.1). If the sender
certified the document first and expects the recipient to fill fields *and* sign,
pulpit can do the signing half but not the filling half. That case waits on
§36.2.

### 31.4 Signature panel

A per-signature panel showing the fields of §28.5, with a certificate detail view
and the full embedded chain marked as unvalidated. Two copy-to-clipboard
affordances: the certificate SHA-256 fingerprint, and a plain-text verification
report suitable for pasting into an email.

---

## 32. Integration with the existing export pipeline

`SPEC-document.md` §11.3's Save As gains terminal stages before its atomic
rename:

```text
source document
  -> ... existing pipeline ...
  -> validate structure and expected edits
  -> render every affected page
  -> [NEW] if signing: append signature revision (§23-§27)
  -> [NEW] if signing: reopen, verify signature (§28), confirm coverage == EntireFile
  -> flush and atomically rename
```

Verification requirements added to `SPEC-document.md` §11.3's validation
(steps 3–7):

- The signed output parses, and the number of signatures found is exactly one
  more than the input contained.
- **The new signature's** coverage is `EntireFile`.
- It is `intact` and `valid` against its embedded certificate.
- The digest recomputed from the file equals the digest that was signed.
- The bytes preceding the appended revision are identical to the pre-signature
  file. Checked directly, by comparison, not inferred.
- Every signature that was present in the input is still `intact` and `valid`.
  Their coverage is expected to have dropped to `EntireRevision`; that is the
  normal consequence of appending (§28.4). A drop in `intact` or `valid`, by
  contrast, means the append corrupted something and is a hard failure.
- **No output is promoted if any of the above fails.**

---

## 33. Error model

The signing failures, extending the workspace's explicit-outcome convention:

```rust
pub enum SigningError {
    CertificationNotAllowed { existing_signatures: usize },
    DocumentLockedByPriorSignature { level: MdpPerm },
    FieldLockedByPriorSignature { field: String, locked_by: String },
    NoEmptySignatureField,          // countersigning a doc with no free field
    ContentChangeInAppendOnlyMode,  // internal guard; should be unreachable from the UI
    NoSignatureField,
    AmbiguousSignatureField { candidates: Vec<String> },
    FieldAlreadySigned { field: String },
    SeedValueDictionaryIgnored { field: String },   // user-cancellable warning
    KeyLoadFailed,
    WrongPassphrase,
    UnsupportedKeyAlgorithm { algorithm: String },
    DigestMechanismMismatch { implied: String, requested: String },
    CertificateExpired { not_after: OffsetDateTime },
    CertificateNotYetValid { not_before: OffsetDateTime },
    TimestampUnavailable { detail: String },
    TimestampRejected { status: String, fail_info: String },
    TimestampNonceMismatch,
    SignatureTooLarge { reserved: usize, required: usize },
    IncrementalWriteFailed,
    HybridXrefRefused,
    EncryptedWithoutCredential,
    PostSignVerificationFailed { detail: String },
}
```

Every one of these must state what failed, that the source is
unchanged, whether recovery data exists, and what to do next. The signing path
has a strong property worth stating in the copy: **no failure mode of this
feature can damage an existing file**, because every write is either to a
temporary file or an append that is discarded on failure.

Note that expired and not-yet-valid certificates are *errors the user can
override*, not hard failures — pulpit is not the arbiter of validity (§20.3),
and signing with an expired certificate is legitimate in some workflows. The
override is explicit and recorded in the status.

---

## 34. Testing

### 34.1 Unit

- Byte-range placeholder arithmetic: reserved sizes, odd-length rejection,
  overflow detection, padding computation.
- `/ByteRange` back-patching at boundary sizes, including a signature at the very
  end of a file and one followed by a large trailer.
- Size estimation with and without a timestamp, tight and loose.
- Mechanism and digest selection for every row of §26.2.
- Signed-attribute construction, including the PAdES prohibition on
  `signing-time`.
- SET OF re-tagging of `signedAttrs`.
- DocMDP/FieldMDP dictionary construction, including both Acrobat quirks of
  §25.4.
- Coverage classification against synthetic byte ranges.

### 34.2 Integration

- Sign a generated fixture; verify with pulpit; **verify with pyHanko's CLI as
  an independent oracle**, run in CI against every fixture pulpit produces. This
  is the single highest-value test in the plan: it checks our output against a
  mature implementation of the same standards.
- Round trip for B-B and B-T against a local TSA.
- Certification at each DocMDP level; assert the dictionaries match what pyHanko
  writes for the same request. (Deferred with §36.7; until then the
  certification-reading tests below run against pyHanko-produced fixtures.)
- Sign an encrypted document; assert `/Contents` is not encrypted and the
  signature verifies.
- Hybrid-xref input is refused.
- A signed document warns and offers append-only mode; in it every editing
  tool is disabled, and Sign is offered only when an empty signature field
  exists.
- **Countersigning round trip.** Sign a fixture, then countersign it in a second
  pass. Assert: both signatures are `intact` and `valid`; the second covers
  `EntireFile`; the first covers `EntireRevision` and is reported as
  changed-and-unclassifiable; pyHanko's CLI validates *both*, including its
  difference analysis judging the appended revision benign. That last assertion
  is the real test — it confirms our append is well-formed in the eyes of an
  implementation that actually classifies it.
- Countersigning is refused when the prior certification declares `NO_CHANGES`.
- Countersigning is refused when a prior signature's FieldMDP locks the target
  field, for each of `/All`, `/Include` naming it, and `/Exclude` not naming it.
- Certification is refused on an already-signed document.
- Countersigning is refused when no empty signature field exists — pulpit must
  not create one.
- A CMS with two `SignerInfo`s, and one whose `sid` matches no embedded
  certificate, are each reported broken with the §28.3 reasons.
- Attack corpus: byte range that omits the xref table; signature object
  overwritten in a later revision; a second `/Contents` gap; a `/ByteRange` whose
  second segment overlaps the first; **a gap of exactly the right size that does
  not coincide with `/Contents`** — the case the §28.2 ordering exists for.
  Each must produce the correct refusal, and none may panic.

### 34.3 Interoperability

Release qualification opens every fixture in Acrobat Reader, Foxit, Okular, and
LibreOffice Draw and records what each reports. The matrix is checked in and
diffed between releases; a regression in what a third-party viewer says about our
output is a release blocker even when our own verifier is happy.

Expect these viewers to report "signature validity is unknown" for self-signed
credentials. That is the correct outcome and the matrix records it as a pass.

### 34.4 Fuzzing

- CMS parsing.
- Byte-range parsing and coverage computation, which run before any signature
  check and are therefore attacker-reachable on any opened file.
- TSA response parsing.
- Incremental-update writer output, round-tripped through the reader.

---

## 35. Delivery

### 35.0 Porting policy

The plan divides the work by a single rule: **Rust crates carry all
cryptography; pyHanko is ported for everything that isn't cryptography.**

- Primitives and container parsing are never implemented in this workspace.
  The RustCrypto stack of §22.3 covers them; DER encoding and signature-scheme
  bugs are exactly where hand-rolling hurts.
- What is ported from pyHanko is decisions and algorithms, not plumbing:
  placeholder arithmetic and the back-patch (§23), size estimation (§23.5),
  coverage classification (§28.2), the Acrobat MDP quirks (§25.4), the SET OF
  re-tag rule (§26.1), the TSA nonce protocol (§27.1). Its plumbing — its DER
  layer, its PDF reader — is replaced by the crates and by `pulpit-render`'s
  existing reader work.
- Hand-rolling is reserved for the two audited gaps, both on top of `der`:
  RFC 3161 structures if `x509-tsp` fails its audit, and the `pdfwrite`
  module if `lopdf` fails its.

Each port carries pyHanko's MIT notice per the licensing note in the preamble.

### Milestone S0 — de-risk

In order; each step gates the next.

1. Pin the pyHanko commit in a `NOTES` file and add its MIT notice (and
   certomancer's) to `LICENSES/`, with `LICENSES/README.md` entries.
2. Stand up the **CI oracle first**: a `make`-driven harness running pyHanko's
   CLI against every fixture in a directory, with a pinned Python environment.
   Generate test credentials with certomancer — self-signed and chained
   PKCS#12, generated in CI or checked in.
3. Produce a signed fixture **by any means** — even a hard-coded single-page
   append — and get the oracle green once. This proves the byte-range and CMS
   mechanics end to end before any architecture exists.
4. Audit `lopdf` against §24: incremental update, `/ID` preservation,
   xref-kind matching, unencrypted `/Contents`. Decide: adopt, or write the
   ~1,000-line purpose-built writer. Expected outcome: the purpose-built
   writer, because Invariant S2 bounds the input to files pulpit just wrote,
   and auditing a general library for byte-exactness is often more work than
   writing the narrow thing.
5. Probe whether PDFium's save preserves an encrypted source's encryption
   parameters in a form the incremental writer can extend per §24(7). If not,
   signing encrypted documents is refused in S1 rather than half-supported.
6. Pin `cms` 0.2.x and wrap it behind the `sign` module's own types
   immediately, so the 0.3 upgrade stays local (§22.3).

### Milestone S1 — B-B (v1)

Byte-range mechanics; signature field and dictionary; CMS with PKCS#12
credentials; invisible and ink-appearance visible signatures; verification with
coverage, integrity, and the §28.4 fallback; post-export verification; signing UI
with the §31.2 identity disclosure; append-only mode for signed documents,
including countersigning into an existing empty field (§31.3).

Three parallel workstreams inside `pulpit-render`, meeting at integration:

- **`pdfwrite`** (gated on S0 step 4): incremental writer (§24); field and
  dictionary emission (§25.1–25.3); placeholder write and back-patch
  (§23.2–23.3); the §29 typestate, built first — it is cheap now and enforces
  no-writes-after-digest by construction.
- **`sign`**: PKCS#12 loading with zeroization (§30); `SignedData` via the
  `cms` crate with signed attributes per §26.3; mechanism and digest selection
  per §26.2, v1 rows only; the SET OF re-tag; size estimation (§23.5). Every
  §26.2 row is unit-tested and oracle-verified.
- **`verify`**: discovery with last-changed-revision tracking (§28.1);
  coverage classification (§28.2), ported faithfully with the gap-coincidence
  check first; cryptographic checks 1–7 (§28.3); `SignatureStatus` (§28.5).
  The §34.2 attack corpus is built alongside the classifier, not after — each
  attack becomes a fixture the moment the classifier exists — and the §34.4
  fuzz targets come up with the parsers, since those paths are
  attacker-reachable on any opened file.

Integration order: §32 pipeline stages in Save As → §25.4 pre-flight checks
(read side) → append-only mode and countersigning (§31.3) → UI (§31.1 flow,
§31.2 disclosure, §31.4 panel, §25.5 ink appearance reusing the existing ink
pipeline) → countersigning round trip with pyHanko validating both signatures
including its difference analysis (§34.2's real test) → the §34.3 interop
matrix.

Exit: acceptance criteria 18–21 and 23–27 green (22 is S2's).

S1 is the v1 release of the feature. Deliberately *not* in it, each recorded
in §36 so the deferral stays a decision:

- producing certification signatures and authoring DocMDP/FieldMDP transforms
  (§36.7) — v1 writes approval signatures only, though it still *reads* prior
  certifications and field locks for the §25.4 pre-flight checks;
- PEM/DER key loading, Ed448, and RSA-PSS (§36.8) — PKCS#12 with RSA PKCS#1
  v1.5, ECDSA, and Ed25519 covers the actual user base;
- timestamps (S2, below).

### Milestone S2 — B-T

Opens with the `x509-tsp` audit (§22.3); on rejection, hand-roll
`TimeStampReq`/`TimeStampResp` on `der`. Then: TSA client over `ureq` per
§27.1, nonce mismatch as a hard failure; dummy-response caching for sizing
(§23.5); the timestamp as an unsigned attribute over `H(signature)` — the
§26.4 trap; attested-vs-claimed time in the UI (§27.2). CI gains a local TSA
(certomancer / pyHanko test tooling can play this role).

### Risk register

The three dependency audits are the schedule risks, and all land at milestone
starts by design: `lopdf` (S0; fallback: the bounded custom writer), `cms`
0.2→0.3 (S0; fallback: isolated behind the `sign` module's types), `x509-tsp`
(S2 start; fallback: a small hand-roll). The one unbounded risk is interop
surprises in the §34.3 viewer matrix — which is why the oracle runs from S0
step 2 onward, not from release qualification.

### Acceptance criteria

Extending `SPEC-document.md` §16:

18. A user can sign a saved PDF with a PKCS#12 credential.
19. pyHanko's CLI verifies the signature independently, in CI.
20. Acrobat Reader reports the signature as present and names the signer.
21. Modifying any *signed* byte of the output — any byte outside the
    `/Contents` reservation — causes pulpit, Acrobat, and pyHanko to all
    report it as broken. (Bytes inside the reservation are not
    integrity-protected in any implementation; see §23.4.)
22. A timestamped signature reports an attested time distinct from the claimed
    one, and names the TSA.
23. Opening a signed document warns before any mutation (A9), offers
    append-only mode as the default, and in append-only mode every editing
    tool is disabled.
24. A document with one signature and one empty signature field can be
    countersigned, and pyHanko validates both signatures afterwards.
25. Certification is not offered in v1 (§36.7), and all signing is refused
    when a prior certification declares `NO_CHANGES`.
26. No failure path of the signing feature modifies the source document or a
    previously saved file.
27. An ink mark is never described as a digital signature anywhere in the UI,
    and no signature is ever described as verified, trusted, or valid without the
    §20.3 qualification.

---

## 36. Deferred scope

Each item below was specified in an earlier draft and removed. Recorded here with
its cost so that deferral stays a decision. Sizes are pyHanko's Python line
counts excluding tests, as a lower bound on the Rust equivalent.

### 36.1 Certificate path validation and trust — *the blocker*

**What:** RFC 5280 path building, name constraints, policy processing, EKU
checking, and validation *as of a past instant* (a signature made in 2024 must
validate against the world as it was in 2024, not today).

**Cost:** `pyhanko-certvalidator` is **11,711 lines**. There is no permissive
Rust crate that does this job. `rustls-webpki` is TLS-shaped: server-auth EKU,
present-time validation, no general CRL handling, no policy processing. Building
a correct one is a project in its own right; building an incorrect one is
actively harmful, because it produces confident false "verified" results.

**When it makes sense:** Delegate to the platform — `Security.framework` on
macOS, CryptoAPI on Windows, and on Linux either a bundled anchor set or nothing.
That inherits the user's existing trust configuration, which for a desktop
application is arguably the right answer regardless of cost. Estimated at weeks
per platform rather than months, and additive: `IdentityAssurance` gains a
variant and nothing else changes.

**Until then:** §20.3 governs.

### 36.2 Incremental difference analysis

**What:** classifying post-signature revisions as `LtaUpdates` / `FormFilling` /
`Annotations` / `Other`, so that DocMDP and FieldMDP can be enforced on read and
a signature can be reported valid despite legitimate later revisions.

**Cost:** `sign/diff_analysis` is **3,126 lines** — but the prerequisite is
worse. It needs revision-scoped object resolution: pyHanko's `HistoricalResolver`
(`pdf_utils/reader.py:832`) plus the per-revision xref cache
(`pdf_utils/xref.py`, 1,412 lines), which together are **2,658 lines** of reader
machinery `lopdf` does not have. The rule set also carries an orphan-detection
pass for objects added but unreachable from the previous revision — the obvious
way to smuggle content past a naive differ.

**Consequence of deferring:** §28.4's coarse fallback; §31.3's append-only mode,
which permits countersigning but forbids every content change; and the disclosure
§31.3 requires, since our own verdict on an earlier signature goes vague as soon
as anything is appended after it.

### 36.3 B-LT and B-LTA: DSS, revocation data, archival timestamps

**What:** the `/DSS` catalog entry with `/Certs`, `/OCSPs`, `/CRLs` and `/VRI`;
OCSP and CRL fetching; `/DocTimeStamp` revisions; the archival chain refresh that
keeps a signature verifiable after its certificates expire.

**Cost:** moderate on its own — `sign/validation/dss.py` is 718 lines and the
timestamp machinery is largely shared with §27 — but it is **worthless without
§36.1**. Embedding revocation data pulpit cannot itself evaluate is filing
cabinets for a librarian who cannot read. It should follow path validation, not
precede it.

**Forward compatibility is preserved:** upgrading a B-T signature to B-LT is a
pure append needing no private key, so documents signed today can be upgraded
later by pulpit or by pyHanko.

Two details worth preserving for that day, both easy to get silently wrong: the
VRI index key is the uppercase SHA-1 of the **padded** `/Contents`
(`pyhanko: sign/validation/dss.py:179`, and §23.4 above), and enabling VRI
forecloses writing the DSS in the same revision as the signature, because the key
depends on the signature that does not exist yet
(`pyhanko: sign/signers/pdf_signer.py:128`).

### 36.4 Hardware tokens and platform keychains

**What:** PKCS#11 via `cryptoki`, and macOS Keychain / Windows CNG.

**Cost:** the code is small — §29's split-signing protocol exists precisely so
that a token is one function mapping a digest to signature bytes. The cost is
compatibility: every token, driver, and national eID middleware is its own
adventure, and PIN handling has a destructive failure mode (locking the token)
that needs careful UI.

**Prerequisite:** none, technically. It is deferred because it is breadth without
depth: it serves more credential types without making any signature more
trustworthy.

### 36.5 Seed value dictionary enforcement

**What:** honouring `/SV` constraints on a signature field — mandated subfilters,
digest algorithms, permitted reasons, required timestamps, certificate
constraints, `/LockDocument`.

**Cost:** ~400 lines (`pyhanko: sign/fields.py:746`,
`sign/signers/pdf_signer.py:1941`), plus the certificate-constraint checks which
partly depend on §36.1.

**Consequence of deferring:** §25.4's rule — if a field carries an `/SV`
dictionary, pulpit says so and offers to cancel rather than silently violating
the document author's constraints.

### 36.6 Content changes to signed documents

**What:** filling form fields, annotating, or adding signature fields to a signed
document under a permissive DocMDP, written as an incremental update. Note that
§31.3 already permits the *signing* half of this; what remains deferred is
changing content.

**Cost:** two things, and the second is the expensive one. It gives pulpit a
write backend that must reproduce arbitrary form-field updates without PDFium —
where signing only ever adds new objects, form filling must correctly rewrite
existing widget appearance streams. And it is only *safe* with §36.2 in place,
because permitting a change under a DocMDP level means being able to check that
the change stayed within that level.

**Consequence of deferring:** the case named at the end of §31.3 — a sender who
certifies a form and expects the recipient to both fill and sign it. pulpit can
do the signing half only.

### 36.7 Certification signatures and MDP authoring

**What:** producing certification signatures, and writing the DocMDP and
FieldMDP transforms of §25.4 — including the two Acrobat quirks and the
`NO_CHANGES` irreversibility dialog of §31.2. v1 produces approval signatures
only.

**Cost:** small in code — the dictionaries are specified in §25.4 and the tests
in §34 — but not in surface: certification adds a certify-vs-approve choice, a
DocMDP level choice, and a destructive-action confirmation to the signing flow,
and its value accrues mostly to the *sender* side of the countersigning
workflow, which is not the §20.3 user.

**What v1 keeps:** the read side in full. The three §25.4 pre-flight checks,
the display of a prior signature's declared DocMDP level (§28.4), and refusal
to sign under a prior `NO_CHANGES` certification are all in v1, because
countersigning is.

**Consequence of deferring:** pulpit can be the recipient in a
certify-then-countersign workflow, but not the sender.

### 36.8 Additional key sources and algorithms

**What:** PEM/DER key + certificate + chain loading; Ed448/SHAKE256 signing;
RSA-PSS signing.

**Cost:** PEM/DER is small and purely additive. Ed448 is blocked on the crate
ecosystem (§22.3). PSS is parameter plumbing plus interop testing against
viewers that handle it unevenly.

**What v1 keeps:** PKCS#12 with RSA PKCS#1 v1.5, ECDSA (P-256/384/521), and
Ed25519 for signing; everything in §26.2's tables accepted on read, with SHA-1
reported as weak.

**Consequence of deferring:** users with OpenSSL-shaped material must repackage
it (`openssl pkcs12 -export`) — an acceptable ask, stated on the help page —
and the §26.2 tables remain the normative reference for the deferred rows.
