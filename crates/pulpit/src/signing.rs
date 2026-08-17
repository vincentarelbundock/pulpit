//! The Sign flow's state machine and its fixed copy (SPEC-signing.md §31).
//!
//! This module is pure: no window handles, no file I/O, no clock reads. The
//! application drives it by pattern-matching [`SigningFlow`] in `app.rs` and
//! calling into `pulpit_render::sign` / `pulpit_render::verify` around it.
//! Keeping the state machine here — rather than inline in `app.rs` — is what
//! lets the transitions and the disclosure strings be unit-tested without a
//! display (`CLAUDE.md`: "domain crates are pure"; this crate is not a domain
//! crate, but the same discipline pays for itself here).
//!
//! ## Scope of this v1
//!
//! - Visible signatures place a text-only appearance (§25.5's default
//!   template) via a small set of position/size presets rather than a
//!   free-form box-drawing interaction — see [`Placement`]'s doc comment for
//!   why the annotation system's rubber-band gesture (`SPEC-document.md`
//!   §8.4) is not reused here. No ink is composited: there is no
//!   signature-mark capture UI (a single freehand glyph scoped to a
//!   signature widget, distinct from the page-lifetime annotation ink
//!   tool) to draw it from, so `AppearanceContent::InkAndText` is never
//!   built by this module.
//! - The engine only accepts `page_index == 0` (`pulpit-render`'s
//!   `SignApplyError::Unsupported`), so v1 offers the Visible choice only
//!   while the reader is showing the first page; elsewhere it is disabled
//!   with the reason shown, rather than letting the user place a box that
//!   would then be refused at Sign time.
//! - No certification (`NO_CHANGES`) and no timestamp authority: v1 always
//!   produces an approval signature with no TSA call, matching
//!   `pulpit-render`'s current `sign_document_file`.

use std::path::PathBuf;

use pulpit_render::sign::{Credential, CredentialSummary, SignReport, SignTarget};
use pulpit_render::verify::SignatureVerification;

/// §31.2, verbatim. Shown, non-dismissably, at every confirmation.
pub const IDENTITY_DISCLOSURE: &str = "pulpit verifies that a signature is intact and that it matches the certificate embedded in it. It does not check whether that certificate is genuine. Other software may or may not accept this signature.";

/// §31.3's required disclosure, shown in addition to
/// [`IDENTITY_DISCLOSURE`] when the target is an existing signature field
/// (countersigning a document that already carries a signature).
pub const COUNTERSIGN_DISCLOSURE: &str = "This document already contains a signature. Adding yours will cause some software — including pulpit — to report that the document changed after the earlier signature was made. This is expected. Software that analyses the change in detail, such as Acrobat, will report both signatures correctly.";

/// §31.3's append-only offer, shown when a document already carrying a
/// signature is opened.
pub const APPEND_ONLY_OFFER: &str = "This document already contains a signature. pulpit can keep it open in append-only mode — view, verify, and countersign into an existing empty field, with editing turned off — or you can edit it anyway, which will cause the existing signature to be reported as changed after signing once you save (§28.4). This is not a bug in either mode.";

/// Whether a document opened this session is being kept append-only (§31.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppendOnlyMode {
    /// The default answer: annotation, form-filling and Save-As mutation
    /// paths are refused; viewing, the signature panel and countersigning
    /// into an existing empty field remain available.
    #[default]
    AppendOnly,
    /// The user declined and may edit normally. Existing signatures will
    /// report §28.4's changed-after-signing verdict once the document is
    /// saved.
    EditAnyway,
}

impl AppendOnlyMode {
    pub fn blocks_mutation(self) -> bool {
        matches!(self, AppendOnlyMode::AppendOnly)
    }
}

/// One target the Options step can offer, computed from a preflight pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetChoice {
    /// The document carries no signature yet: a fresh invisible field.
    NewField,
    /// Countersigning: one of the document's existing empty `/Sig` fields.
    ExistingField(String),
}

impl From<&TargetChoice> for SignTarget {
    fn from(choice: &TargetChoice) -> Self {
        match choice {
            TargetChoice::NewField => SignTarget::NewInvisibleField { name: None },
            TargetChoice::ExistingField(name) => SignTarget::ExistingField(name.clone()),
        }
    }
}

impl TargetChoice {
    /// Whether confirming this target requires §31.3's countersign
    /// disclosure in addition to the identity disclosure.
    pub fn is_countersign(&self) -> bool {
        matches!(self, TargetChoice::ExistingField(_))
    }
}

/// Reason, location and contact — the free-text half of §31.1 step 5.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SigningOptions {
    pub reason: String,
    pub location: String,
    pub contact: String,
    pub target: Option<TargetChoice>,
    /// `true` once the user has chosen Visible over the default Invisible.
    /// Only meaningful together with `placement`, which is `Some` exactly
    /// when this is `true` (see [`SigningOptions::set_visible`]).
    pub visible_requested: bool,
    /// The page-relative box, set together with `visible_requested`. `None`
    /// for an invisible signature.
    pub placement: Option<Placement>,
}

impl SigningOptions {
    /// Turn the visible/invisible choice on or off, keeping `placement` in
    /// lock-step so a caller can never observe `visible_requested: true`
    /// with `placement: None` or vice versa. Turning it on for the first
    /// time seeds [`Placement::default`]; turning it off drops whatever
    /// preset was chosen, so re-enabling starts fresh rather than resurfacing
    /// a stale box.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible_requested = visible;
        self.placement = if visible {
            Some(self.placement.unwrap_or_default())
        } else {
            None
        };
    }
}

/// A page-relative placement preset for a visible signature (§31.1 step 6).
///
/// §31.1 step 6 calls for "the same box-drawing interaction as other
/// annotations (`SPEC-document.md` §8.4)". `SPEC-document.md` §8.4's
/// rubber-band interaction is `AnnotationTool::Select`: it drags over the
/// page to *pick up existing marks* for deletion, driven by pointer events
/// the reader surface already routes for that purpose. Placing a *new* box
/// for a signature widget while a modal Sign dialog is open is a different
/// gesture — it would need the reader's pointer pipeline taught a second,
/// placement-shaped meaning it has no other user, purely for this one v1
/// dialog. That is more interaction-system surface than this task's scope
/// justifies, so v1 takes the fallback named in the task: a small set of
/// position and size presets, computed directly into PDF coordinates by
/// [`Placement::rect`]. Revisit reuse if `SPEC-document.md` ever grows a
/// general "place a new object" drag that both callers can share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

impl PlacementPosition {
    pub const ALL: [PlacementPosition; 5] = [
        PlacementPosition::TopLeft,
        PlacementPosition::TopRight,
        PlacementPosition::BottomLeft,
        PlacementPosition::BottomRight,
        PlacementPosition::Center,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PlacementPosition::TopLeft => "Top left",
            PlacementPosition::TopRight => "Top right",
            PlacementPosition::BottomLeft => "Bottom left",
            PlacementPosition::BottomRight => "Bottom right",
            PlacementPosition::Center => "Center",
        }
    }
}

/// A signature box size, in points, before margin clamping in
/// [`Placement::rect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementSize {
    Small,
    Medium,
    Large,
}

impl PlacementSize {
    pub const ALL: [PlacementSize; 3] = [
        PlacementSize::Small,
        PlacementSize::Medium,
        PlacementSize::Large,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PlacementSize::Small => "Small",
            PlacementSize::Medium => "Medium",
            PlacementSize::Large => "Large",
        }
    }

    fn dims_pt(self) -> (f64, f64) {
        match self {
            PlacementSize::Small => (150.0, 50.0),
            PlacementSize::Medium => (200.0, 70.0),
            PlacementSize::Large => (260.0, 90.0),
        }
    }
}

/// Margin kept clear around a placed box: half an inch.
const PLACEMENT_MARGIN_PT: f64 = 36.0;

/// Where a visible signature's box lands: a corner or the center, at one of
/// three sizes. See [`PlacementPosition`]'s doc comment for why this is a
/// preset choice rather than a drawn rectangle in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub position: PlacementPosition,
    pub size: PlacementSize,
}

impl Default for Placement {
    fn default() -> Self {
        Placement {
            position: PlacementPosition::BottomRight,
            size: PlacementSize::Medium,
        }
    }
}

impl Placement {
    /// The widget rect in PDF page coordinates, `[x0, y0, x1, y1]`, for a
    /// page whose displayed size (`PageGeometry::width`/`height`) is
    /// `page_width` by `page_height` points. The box is clamped to fit
    /// inside the margin on pages too small for the chosen preset, rather
    /// than producing a rect that overhangs the page.
    pub fn rect(&self, page_width: f64, page_height: f64) -> [f64; 4] {
        let (preset_w, preset_h) = self.size.dims_pt();
        let w = preset_w.min((page_width - 2.0 * PLACEMENT_MARGIN_PT).max(1.0));
        let h = preset_h.min((page_height - 2.0 * PLACEMENT_MARGIN_PT).max(1.0));
        let (x0, y0) = match self.position {
            PlacementPosition::TopLeft => {
                (PLACEMENT_MARGIN_PT, page_height - PLACEMENT_MARGIN_PT - h)
            }
            PlacementPosition::TopRight => (
                page_width - PLACEMENT_MARGIN_PT - w,
                page_height - PLACEMENT_MARGIN_PT - h,
            ),
            PlacementPosition::BottomLeft => (PLACEMENT_MARGIN_PT, PLACEMENT_MARGIN_PT),
            PlacementPosition::BottomRight => {
                (page_width - PLACEMENT_MARGIN_PT - w, PLACEMENT_MARGIN_PT)
            }
            PlacementPosition::Center => ((page_width - w) / 2.0, (page_height - h) / 2.0),
        };
        [x0, y0, x0 + w, y0 + h]
    }
}

/// The `CN=` component of a subject distinguished name, or the whole string
/// if none is found. `pulpit-render`'s `CredentialSummary::subject` hands
/// back the DN already formatted as text rather than parsed RDNs, so this is
/// a best-effort split on `,`/`CN=` rather than a real ASN.1 walk — good
/// enough for a label, never used for anything security-relevant.
pub fn subject_common_name(subject: &str) -> &str {
    subject
        .split(',')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("CN="))
        .unwrap_or(subject)
}

/// §25.5's default text template, verbatim, split across the two lines
/// `AppearanceContent::Text` draws:
///
/// ```text
/// Digitally signed by %(signer)s.
/// Timestamp: %(ts)s.
/// ```
///
/// No ink is composited (`AppearanceContent::InkAndText` is not built here)
/// — see this module's top-level doc comment for why.
pub fn text_appearance_content(
    signer_cn: &str,
    signing_time_label: &str,
) -> pulpit_render::sign::AppearanceContent {
    pulpit_render::sign::AppearanceContent::Text {
        signer_name: format!("Digitally signed by {signer_cn}."),
        time_label: format!("Timestamp: {signing_time_label}."),
    }
}

/// Build the `SignAppearance` for `options`, or `None` for an invisible
/// signature. `page_width`/`page_height` are the current page's displayed
/// size in points (`PageGeometry::width`/`height`); the caller is
/// responsible for only calling this when the target page is page 0 — the
/// only page index `pulpit-render` accepts (§25.5).
pub fn appearance_for(
    options: &SigningOptions,
    signer_cn: &str,
    signing_time_label: &str,
    page_width: f64,
    page_height: f64,
) -> Option<pulpit_render::sign::SignAppearance> {
    let placement = options.placement?;
    Some(pulpit_render::sign::SignAppearance {
        rect: placement.rect(page_width, page_height),
        page_index: 0,
        content: text_appearance_content(signer_cn, signing_time_label),
    })
}

/// Everything the credential step has learned about the loaded PKCS#12.
#[derive(Debug, Clone)]
pub struct CredentialInfo {
    pub summary: CredentialSummary,
    pub expired: bool,
    pub not_yet_valid: bool,
    /// Set once the user has explicitly overridden an expired or
    /// not-yet-valid warning (§33 last paragraph).
    pub override_validity: bool,
}

impl CredentialInfo {
    pub fn from_summary(summary: CredentialSummary, now_unix: i64) -> Self {
        let expired = parse_unix_ish(&summary.not_after)
            .map(|not_after| not_after < now_unix)
            .unwrap_or(false);
        let not_yet_valid = parse_unix_ish(&summary.not_before)
            .map(|not_before| not_before > now_unix)
            .unwrap_or(false);
        CredentialInfo {
            summary,
            expired,
            not_yet_valid,
            override_validity: false,
        }
    }

    /// Whether the flow may proceed past the credential step.
    pub fn may_proceed(&self) -> bool {
        (!self.expired && !self.not_yet_valid) || self.override_validity
    }
}

/// [`CredentialSummary`]'s validity fields are formatted strings (RFC 3339 or
/// similar), not unix seconds; `pulpit-render` does not expose a parsed
/// variant. Best-effort: an unparseable value is treated as "cannot tell",
/// which is the same as not warning — the confirmation step still shows the
/// raw string either way, so nothing is hidden.
fn parse_unix_ish(value: &str) -> Option<i64> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|dt| dt.unix_timestamp())
}

/// One step of the Sign flow (§31.1).
///
/// Not `Clone`: [`Credential`] holds zeroize-on-drop key material and
/// deliberately implements neither `Clone` nor `Copy` (§30.2).
#[derive(Debug)]
pub enum SigningFlow {
    /// Step 3: unsaved edits exist, and Save As is running before signing
    /// can start. The flow resumes at [`SigningFlow::ChooseCredential`] once
    /// the save completes.
    SavingFirst,
    /// Step 4, before a credential file has been chosen.
    ChooseCredential,
    /// Step 4: a `.p12`/`.pfx` was chosen; waiting for a passphrase.
    EnterPassphrase {
        credential_path: PathBuf,
        passphrase: String,
        /// Set after a failed load, so the passphrase box can say why
        /// without losing the file that was chosen (§33: state what failed,
        /// what to do next).
        error: Option<String>,
    },
    /// Step 4, in progress: `load_pkcs12` dispatched off the UI thread.
    LoadingCredential { credential_path: PathBuf },
    /// Step 4 shown: subject, issuer, validity, fingerprint.
    CredentialSummary {
        credential_path: PathBuf,
        credential: std::sync::Arc<Credential>,
        info: CredentialInfo,
    },
    /// Step 5: reason/location/contact, target field.
    Options {
        credential: std::sync::Arc<Credential>,
        info: CredentialInfo,
        options: SigningOptions,
        /// Every empty `/Sig` field a preflight pass over the document
        /// found, for the target picker. One entry (`NewField` or a single
        /// `ExistingField`) in the ordinary case; more than one when
        /// countersigning a document preflight reported
        /// `AmbiguousSignatureField` for.
        candidates: Vec<TargetChoice>,
    },
    /// Step 7: the confirmation dialog. Non-dismissable except its own
    /// Cancel/Sign buttons.
    Confirm {
        credential: std::sync::Arc<Credential>,
        info: CredentialInfo,
        options: SigningOptions,
        candidates: Vec<TargetChoice>,
    },
    /// Step 8, in progress. `credential` and `options` are held rather than
    /// dropped so a failed attempt could retry without reloading the
    /// credential or re-asking the options — not wired up in v1, so the
    /// view only reads `destination`.
    Signing {
        #[allow(dead_code)]
        credential: std::sync::Arc<Credential>,
        #[allow(dead_code)]
        options: SigningOptions,
        destination: PathBuf,
    },
    /// Step 9: the produced file has been reopened and re-verified.
    Result {
        report: SignReport,
        destination: PathBuf,
        verification: Vec<SignatureVerification>,
    },
    /// A step failed. The source file is always unaffected (§33).
    Failed { detail: String },
}

impl SigningFlow {
    pub fn start() -> Self {
        SigningFlow::ChooseCredential
    }
}

/// What the Sign dialog can ask `app.rs` to do. Grouped under one
/// `Message::Sign(SignMsg)` variant, the way `TimerCommand` and
/// `ReadCommand` already group their own popups' messages.
#[derive(Debug, Clone)]
pub enum SignMsg {
    /// The toolbar's Sign button, or the panel's "sign into this field".
    Start,
    /// Cancel at any step. Always safe (§33): nothing has been written yet,
    /// or the write was to a temporary file that is now discarded.
    Cancel,
    ChooseCredentialFile,
    CredentialFileChosen(Option<PathBuf>),
    PassphraseChanged(String),
    PassphraseSubmit,
    /// `Err` carries a message already worded per §33 (states what failed,
    /// that the source is unchanged, what to do next).
    CredentialLoaded(Result<std::sync::Arc<Credential>, String>),
    OverrideValidity,
    ContinueToOptions,
    ReasonChanged(String),
    LocationChanged(String),
    ContactChanged(String),
    /// Sent by the Options view's target picker when more than one
    /// candidate field exists (see `SigningFlow::Options::candidates`).
    TargetChosen(TargetChoice),
    /// Visible/invisible toggle. Refused by the app when the reader is not
    /// showing page 0 — see `crate::signing`'s module doc comment.
    VisibleChanged(bool),
    PlacementPositionChosen(PlacementPosition),
    PlacementSizeChosen(PlacementSize),
    ContinueToConfirm,
    BackToOptions,
    Confirm,
    /// Not sent by the shipped dialog — `Confirm` reaches the same
    /// destination picker directly — but kept as its own message so a
    /// future "pick a different destination" retry has somewhere to land
    /// without a new variant.
    #[allow(dead_code)]
    ChooseDestination,
    DestinationChosen(Option<PathBuf>),
    Completed(Result<SignOutcome, String>),
    Done,
}

/// A successful run of the flow's Sign + verify steps (§31.1 steps 8–9).
#[derive(Debug, Clone)]
pub struct SignOutcome {
    pub report: std::sync::Arc<SignReport>,
    pub destination: PathBuf,
    pub verification: std::sync::Arc<Vec<SignatureVerification>>,
}

// --- §20.2 three-state copy -------------------------------------------------

/// The three states §20.2 requires, named so a caller cannot accidentally
/// collapse "broken" and "ink mark" into "not signed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureLine {
    /// A drawn appearance with no cryptography behind it. Not yet wired to
    /// a caller: the annotation surface this state belongs to
    /// (`SPEC-document.md`'s ink/stamp tools) is outside this task's scope,
    /// which is why nothing in `pulpit` constructs this variant today.
    #[allow(dead_code)]
    InkMark,
    /// Intact, valid CMS whose identity pulpit has not corroborated.
    SignedIdentityNotVerified {
        signer: String,
        sha256_fingerprint: String,
    },
    /// Byte range or CMS failed to verify, or coverage is unclear.
    NotValid { reason: String },
}

impl SignatureLine {
    /// The exact line the signature panel's summary shows (§28.5: "never
    /// more confident than the weakest component").
    pub fn summary_text(&self) -> String {
        match self {
            SignatureLine::InkMark => "handwritten mark".to_string(),
            SignatureLine::SignedIdentityNotVerified {
                signer,
                sha256_fingerprint,
            } => format!(
                "Signed by {signer} — identity not verified by pulpit (fingerprint {sha256_fingerprint})"
            ),
            SignatureLine::NotValid { reason } => format!("Signature is not valid: {reason}"),
        }
    }
}

/// Derive the §20.2 line from a checked [`pulpit_render::verify::SignatureStatus`].
///
/// `broken` coverage, a failed `intact`/`valid` check, or `Unclear` coverage
/// all collapse to [`SignatureLine::NotValid`] — the summary must never claim
/// more than the weakest component establishes (§28.5).
pub fn signature_line_for(status: &pulpit_render::verify::SignatureStatus) -> SignatureLine {
    use pulpit_render::verify::{IdentityAssurance, SignatureCoverage};

    if status.coverage == SignatureCoverage::Unclear {
        return SignatureLine::NotValid {
            reason: "the signed byte range does not clearly cover the file".to_string(),
        };
    }
    if !status.intact {
        return SignatureLine::NotValid {
            reason: "the signed bytes do not match what was signed".to_string(),
        };
    }
    if !status.valid {
        return SignatureLine::NotValid {
            reason: "the cryptographic signature does not verify".to_string(),
        };
    }
    let IdentityAssurance::NotVerified { .. } = status.identity;
    SignatureLine::SignedIdentityNotVerified {
        signer: status.signer_subject.clone(),
        sha256_fingerprint: status.signer_cert.sha256_fingerprint.clone(),
    }
}

/// Derive the §20.2 line for a [`SignatureVerification`], which may be
/// `Broken` before any status even exists.
pub fn signature_line_for_verification(verification: &SignatureVerification) -> SignatureLine {
    match verification {
        SignatureVerification::Checked(status) => signature_line_for(status),
        SignatureVerification::Broken { reason, .. } => SignatureLine::NotValid {
            reason: reason.clone(),
        },
    }
}

/// §31.4's plain-text report, suitable for pasting into an email.
pub fn plain_text_report(field_name: &str, line: &SignatureLine, coverage: &str) -> String {
    format!(
        "pulpit signature report\nField: {field_name}\nStatus: {}\nCoverage: {coverage}\n\n\
         pulpit checked that this signature is intact and that it matches the certificate \
         embedded in it. It did not check whether that certificate is genuine.",
        line.summary_text()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disclosure_strings_are_the_spec_text() {
        assert_eq!(
            IDENTITY_DISCLOSURE,
            "pulpit verifies that a signature is intact and that it matches the certificate \
             embedded in it. It does not check whether that certificate is genuine. Other \
             software may or may not accept this signature."
        );
        assert_eq!(
            COUNTERSIGN_DISCLOSURE,
            "This document already contains a signature. Adding yours will cause some \
             software — including pulpit — to report that the document changed after the \
             earlier signature was made. This is expected. Software that analyses the change \
             in detail, such as Acrobat, will report both signatures correctly."
        );
    }

    #[test]
    fn target_choice_selects_a_countersign_disclosure() {
        assert!(!TargetChoice::NewField.is_countersign());
        assert!(TargetChoice::ExistingField("Sig1".into()).is_countersign());
    }

    #[test]
    fn target_choice_converts_to_the_engine_s_sign_target() {
        assert_eq!(
            SignTarget::from(&TargetChoice::NewField),
            SignTarget::NewInvisibleField { name: None }
        );
        assert_eq!(
            SignTarget::from(&TargetChoice::ExistingField("Sig1".into())),
            SignTarget::ExistingField("Sig1".into())
        );
    }

    #[test]
    fn flow_starts_at_choose_credential() {
        assert!(matches!(
            SigningFlow::start(),
            SigningFlow::ChooseCredential
        ));
    }

    #[test]
    fn append_only_is_the_default_and_blocks_mutation() {
        assert_eq!(AppendOnlyMode::default(), AppendOnlyMode::AppendOnly);
        assert!(AppendOnlyMode::AppendOnly.blocks_mutation());
        assert!(!AppendOnlyMode::EditAnyway.blocks_mutation());
    }

    #[test]
    fn expired_credential_may_not_proceed_without_override() {
        let summary = CredentialSummary {
            subject: "CN=Test".into(),
            issuer: "CN=Test CA".into(),
            serial: "01".into(),
            not_before: "2000-01-01T00:00:00Z".into(),
            not_after: "2001-01-01T00:00:00Z".into(),
            sha256_fingerprint: "aa:bb".into(),
            key_algorithm: "RSA".into(),
            key_bits: Some(2048),
        };
        let now = 1_700_000_000; // long after 2001
        let mut info = CredentialInfo::from_summary(summary, now);
        assert!(info.expired);
        assert!(!info.not_yet_valid);
        assert!(!info.may_proceed());
        info.override_validity = true;
        assert!(info.may_proceed());
    }

    #[test]
    fn not_yet_valid_credential_is_told_apart_from_expired() {
        let summary = CredentialSummary {
            subject: "CN=Test".into(),
            issuer: "CN=Test CA".into(),
            serial: "01".into(),
            not_before: "2999-01-01T00:00:00Z".into(),
            not_after: "3000-01-01T00:00:00Z".into(),
            sha256_fingerprint: "aa:bb".into(),
            key_algorithm: "RSA".into(),
            key_bits: Some(2048),
        };
        let info = CredentialInfo::from_summary(summary, 1_700_000_000);
        assert!(!info.expired);
        assert!(info.not_yet_valid);
        assert!(!info.may_proceed());
    }

    #[test]
    fn signature_line_never_says_verified_or_trusted() {
        for line in [
            SignatureLine::InkMark,
            SignatureLine::SignedIdentityNotVerified {
                signer: "Jane Doe".into(),
                sha256_fingerprint: "de:ad:be:ef".into(),
            },
            SignatureLine::NotValid {
                reason: "byte range mismatch".into(),
            },
        ] {
            let text = line.summary_text();
            let lower = text.to_lowercase();
            // "identity not verified" and "not valid" both contain
            // "verified"/"valid" only inside a negation; the bare, unqualified
            // words this bans are "trusted" and a standalone "verified" not
            // immediately preceded by "not".
            assert!(!lower.contains("trusted"), "{text:?}");
            if lower.contains("verified") {
                assert!(lower.contains("not verified"), "{text:?}");
            }
        }
    }

    #[test]
    fn ink_mark_is_never_called_signed() {
        assert_eq!(SignatureLine::InkMark.summary_text(), "handwritten mark");
    }

    #[test]
    fn signed_identity_not_verified_shows_signer_and_fingerprint() {
        let line = SignatureLine::SignedIdentityNotVerified {
            signer: "Jane Doe".into(),
            sha256_fingerprint: "de:ad:be:ef".into(),
        };
        assert_eq!(
            line.summary_text(),
            "Signed by Jane Doe — identity not verified by pulpit (fingerprint de:ad:be:ef)"
        );
    }

    #[test]
    fn not_valid_states_its_reason() {
        let line = SignatureLine::NotValid {
            reason: "the signed bytes do not match what was signed".into(),
        };
        assert_eq!(
            line.summary_text(),
            "Signature is not valid: the signed bytes do not match what was signed"
        );
    }

    #[test]
    fn set_visible_keeps_placement_in_lock_step() {
        let mut options = SigningOptions::default();
        assert!(!options.visible_requested);
        assert_eq!(options.placement, None);

        options.set_visible(true);
        assert!(options.visible_requested);
        assert_eq!(options.placement, Some(Placement::default()));

        options.set_visible(false);
        assert!(!options.visible_requested);
        assert_eq!(options.placement, None);
    }

    #[test]
    fn set_visible_true_twice_keeps_the_chosen_preset() {
        let mut options = SigningOptions::default();
        options.set_visible(true);
        options.placement = Some(Placement {
            position: PlacementPosition::TopLeft,
            size: PlacementSize::Large,
        });
        // Re-toggling on (a no-op in the view, but exercised here directly)
        // must not reset a preset the user already chose.
        options.set_visible(true);
        assert_eq!(
            options.placement,
            Some(Placement {
                position: PlacementPosition::TopLeft,
                size: PlacementSize::Large,
            })
        );
    }

    #[test]
    fn placement_rect_sits_inside_the_margin_at_each_corner() {
        let page_w = 612.0;
        let page_h = 792.0;
        for position in PlacementPosition::ALL {
            let placement = Placement {
                position,
                size: PlacementSize::Medium,
            };
            let [x0, y0, x1, y1] = placement.rect(page_w, page_h);
            assert!(x0 >= PLACEMENT_MARGIN_PT - 1e-9, "{position:?} x0={x0}");
            assert!(y0 >= PLACEMENT_MARGIN_PT - 1e-9, "{position:?} y0={y0}");
            assert!(
                x1 <= page_w - PLACEMENT_MARGIN_PT + 1e-9,
                "{position:?} x1={x1}"
            );
            assert!(
                y1 <= page_h - PLACEMENT_MARGIN_PT + 1e-9,
                "{position:?} y1={y1}"
            );
            assert!(x1 > x0);
            assert!(y1 > y0);
        }
    }

    #[test]
    fn placement_rect_clamps_to_a_tiny_page_without_overhanging() {
        let placement = Placement {
            position: PlacementPosition::Center,
            size: PlacementSize::Large,
        };
        let [x0, y0, x1, y1] = placement.rect(100.0, 100.0);
        assert!(x0 >= 0.0);
        assert!(y0 >= 0.0);
        assert!(x1 <= 100.0);
        assert!(y1 <= 100.0);
    }

    #[test]
    fn subject_common_name_extracts_cn_from_a_dn() {
        assert_eq!(
            subject_common_name("CN=Jane Doe,O=Example,C=US"),
            "Jane Doe"
        );
        assert_eq!(subject_common_name("O=Example, CN=Jane Doe"), "Jane Doe");
    }

    #[test]
    fn subject_common_name_falls_back_to_the_whole_subject() {
        assert_eq!(subject_common_name("O=Example,C=US"), "O=Example,C=US");
    }

    #[test]
    fn text_appearance_content_matches_section_25_5_verbatim() {
        use pulpit_render::sign::AppearanceContent;
        let content = text_appearance_content("Jane Doe", "2026-08-16T00:00:00Z");
        let AppearanceContent::Text {
            signer_name,
            time_label,
        } = content
        else {
            panic!("expected Text content");
        };
        assert_eq!(signer_name, "Digitally signed by Jane Doe.");
        assert_eq!(time_label, "Timestamp: 2026-08-16T00:00:00Z.");
    }

    #[test]
    fn appearance_for_is_none_without_a_placement() {
        let options = SigningOptions::default();
        assert!(appearance_for(&options, "Jane Doe", "now", 612.0, 792.0).is_none());
    }

    #[test]
    fn appearance_for_targets_page_zero_with_the_chosen_rect() {
        let mut options = SigningOptions::default();
        options.set_visible(true);
        let appearance = appearance_for(&options, "Jane Doe", "now", 612.0, 792.0)
            .expect("visible options produce an appearance");
        assert_eq!(appearance.page_index, 0);
        assert_eq!(
            appearance.rect,
            options.placement.unwrap().rect(612.0, 792.0)
        );
    }

    #[test]
    fn plain_text_report_includes_field_status_and_coverage() {
        let line = SignatureLine::NotValid {
            reason: "byte range mismatch".into(),
        };
        let report = plain_text_report("Sig1", &line, "EntireRevision");
        assert!(report.contains("Sig1"));
        assert!(report.contains("EntireRevision"));
        assert!(report.contains("Signature is not valid: byte range mismatch"));
    }
}
