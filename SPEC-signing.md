# pulpit cryptographic signature specification

Companion to `SPEC-document.md`, which defers this feature at §3.3. Adds
§20–§36; the numbering is preserved because `Cargo.toml` and
`LICENSES/README.md` cite these section numbers.

Derived from a close reading of **pyHanko** (MIT, Matthias Valvekens), used as a
reference and a source rather than a dependency: porting its logic is permitted
and its notice is in `LICENSES/`, along with `certomancer`'s. Porting policy
(§35.0): **Rust crates carry all cryptography; pyHanko is ported for everything
that is judgement rather than cryptography.**

pulpit signs and verifies. It does **not** validate identity: no certificate
path validation, no trust decisions, no revocation checking (§20.3, §36.1).
Nothing may be described as verified, trusted or valid without that
qualification.

The full prose specification of the implemented mechanics is in the git history
(`SPEC-signing.md` at `70bd669`) — this document is now a status record.

---

## Implemented

**Byte-range mechanics (§23), incremental writer (§24)**
- Placeholder write, offset back-patch, digest, reservation fill; CMS size
  estimation.
- Purpose-built writer in `pdfwrite`, chosen over `lopdf` per §35 S0 step 4:
  lopdf 0.32.0 cloned both `/ID` elements rather than regenerating id2, had no
  `/Contents` encryption exemption, and merged hybrid-reference files instead of
  refusing them — none of them incidental, since it is a general-purpose library
  with no signing awareness. `/ID` id1 preserved and id2 regenerated; xref kind
  matched; hybrid-reference files refused; encrypted documents refused outright,
  so `/Contents` is never encrypted.
- §29 split-signing typestate (`SigningSession → TbsDocument → digest → sign →
  finish`), so no object can be written after the digest.

**Fields and dictionaries (§25)**
- Signature field, AcroForm, signature dictionary emission.
- DocMDP/FieldMDP **read** for pre-flight; signing refused under a prior
  `NO_CHANGES` certification, and encrypted documents refused up front per §35
  S0 step 5 (`verify/preflight.rs`).
- Visible signatures using a profile's saved ink, text, or combined
  appearance, or §25.5's default text template; invisible ones for a profile
  whose saved default says so. The Sign flow itself does not ask: visibility
  and the box preset are a profile setting.

**CMS (§26)**
- `SignedData` via the `cms` crate, pinned at 0.2.x behind the module's own
  types; signed attributes per §26.3; SET OF re-tag.
- Mechanisms: RSA PKCS#1 v1.5 (2048/3072/4096), ECDSA P-256/P-384/P-521,
  Ed25519, with §26.2 digest selection.
- Profiles: `/adbe.pkcs7.detached` and `/ETSI.CAdES.detached`.

**Key material (§30)**
- PKCS#12 loading with zeroization; reusable signing profiles.

**Verification (§28)**
- Discovery with last-changed-revision tracking; coverage classification;
  cryptographic checks; `SignatureStatus`.
- Refuses to infer the signature primitive from the certificate; recovers
  original `signedAttrs` bytes rather than re-encoding; rejects duplicated
  security-critical attributes.
- Bounded parsers: value depth, entry counts, stream bytes, object streams, and
  xref counts bounded by what the file can encode.
- An undecodable signature field reports as broken, never as absent.

**Integration and UI (§31, §32)**
- Save As pipeline stages; the §32 gate reads the candidate back from disk and
  refuses to promote on any failure.
- Signing flow, §31.2 identity disclosure, signature panel. The flow asks only
  what it cannot look up: which profile (when more than one is saved), its
  passphrase (when the session does not already hold the credential), §33's
  override for an expired certificate, and where to save the signed copy. In
  the common case the platform's save dialog is the only thing it shows. §31.2
  and §31.3's texts live on the signature panel, which outlives the corner
  notice raised when a signature is written; with no saved profile, signing
  refuses and names Settings rather than offering a `.p12` picker.
- Append-only mode for signed documents, including countersigning into an
  existing empty field (§31.3). An existing empty `/Sig` field is offered as
  a target on signed and unsigned documents alike, ordered ahead of "new
  field", and reachable either from the toolbar or by clicking the field on
  the page.

**Testing (§34)**
- Unit and integration tests across the above.
- pyHanko oracle harness: `make sign-oracle-setup`, `make sign-oracle`, wired
  into CI after the Rust integration tests generate signed fixtures. The
  pyHanko implementation is pinned to source commit
  `0945f9bc64ef6ef386500943ee2b5941b5f142cd`, and `pyhanko-cli` is pinned in
  `tools/sign-oracle/requirements.txt`.
- Fuzz targets for the revision map, CMS, discovery and full verification, with
  committed seed inputs outside cargo-fuzz's generated corpus.

---

## Still to do

**Milestone S1 remainder**
- §34.3 interoperability matrix, and acceptance criterion 20 (Acrobat Reader
  reports the signature and names the signer) — neither is evidenced in the
  repository.

**Milestone S2 — B-T (timestamping)**
Not started; no TSA code exists.
- Audit `x509-tsp`; on rejection hand-roll `TimeStampReq`/`TimeStampResp` on
  `der`.
- TSA client over `ureq` per §27.1, nonce mismatch a hard failure.
- Dummy-response caching for sizing (§23.5).
- Timestamp as an *unsigned* attribute over `H(signature)` — the §26.4 trap.
- Attested-vs-claimed time in the UI (§27.2). Acceptance criterion 22.
- CI gains a local TSA (certomancer or pyHanko test tooling).

**Known gaps in shipped code**
- SHA-1 is reported as a finding, not refused. RSA signing requires at least a
  2048-bit modulus; verification still checks weaker legacy signatures but
  reports their key strength as a finding.

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
