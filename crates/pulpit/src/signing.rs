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
//! - Visible signatures place either the selected profile's saved ink,
//!   text, or combined appearance, or §25.5's default text template for an
//!   ad-hoc credential. Profile ink is captured in Settings by a
//!   signature-specific pad, distinct from the page annotation ink tool.
//! - Where the mark goes depends on what is being signed, and both cases are
//!   named by [`AppearancePlan`]. Signing into an existing field whose box
//!   the application could locate draws inside that box, on whatever page
//!   the document already puts it on: the sender drew the rectangle, so
//!   there is nothing left to place. Everywhere else — a new field, or an
//!   existing field with no usable `/Rect` — placement uses a small set of
//!   position and size presets against one captured page, rather than a
//!   free-form box-drawing interaction; see [`PlacementPosition`]'s doc
//!   comment for why the annotation system's rubber-band gesture
//!   (`SPEC-document.md` §8.4) is not reused here. The page is captured when
//!   the Visible choice is made, so scrolling afterwards cannot move the
//!   mark out from under the choice the reader saw.
//! - No certification (`NO_CHANGES`) and no timestamp authority: v1 always
//!   produces an approval signature with no TSA call, matching
//!   `pulpit-render`'s current `sign_document_file`.

use std::path::PathBuf;

use pulpit_core::page::{PageGeometry, PageRect, PageRotation};
use pulpit_render::sign::{Credential, CredentialSummary, SignReport, SignTarget};
use pulpit_render::verify::SignatureVerification;

/// §31.2, verbatim. Shown, non-dismissably, at every confirmation.
pub const IDENTITY_DISCLOSURE: &str = "pulpit verifies that a signature is intact and that it matches the certificate embedded in it. It does not check whether that certificate is genuine. Other software may or may not accept this signature.";

/// §31.3's required disclosure, shown in addition to
/// [`IDENTITY_DISCLOSURE`] when the document already carries a signature
/// (`SigningOptions::countersigning`) — not merely when the target is an
/// existing field, since an existing empty `/Sig` field is now also offered
/// on unsigned documents, where this disclosure would be false.
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

/// §31.1 step 5's target selection: the field a page-surface click asked to
/// sign into (`SignMsg::StartInField`), if it still names one of
/// `candidates`, otherwise the first candidate — the candidate order makes
/// this the document's own empty field when it has one, since an existing
/// empty field is always ordered ahead of `NewField` (see
/// `App::sign_target_candidates_from_signals`).
///
/// A mismatch (the clicked field got signed by someone else meanwhile, or
/// preflight no longer offers it) falls back silently rather than erroring:
/// the Options/Confirm steps show whatever was actually selected, so nothing
/// is hidden from the reader.
pub fn pick_signing_target(
    candidates: &[TargetChoice],
    prefill: Option<&str>,
) -> Option<TargetChoice> {
    let prefilled = prefill.and_then(|name| {
        candidates.iter().find(
            |candidate| matches!(candidate, TargetChoice::ExistingField(field) if field == name),
        )
    });
    prefilled.or_else(|| candidates.first()).cloned()
}

/// Reason, location and contact — the free-text half of §31.1 step 5.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SigningOptions {
    pub reason: String,
    pub location: String,
    pub contact: String,
    pub target: Option<TargetChoice>,
    /// Whether the document already carries a signature — set once at
    /// `ContinueToOptions` from the same preflight pass that computed the
    /// candidates, not derived from `target`: an existing empty field is now
    /// offered on unsigned documents too, so "the target is an existing
    /// field" no longer implies "the document is already signed" (§31.3's
    /// countersign disclosure gates on this, not on the target's shape).
    pub countersigning: bool,
    /// `true` once the user has chosen Visible over the default Invisible.
    /// Only meaningful together with `placement`, which is `Some` exactly
    /// when this is `true` (see [`SigningOptions::set_visible`]).
    pub visible_requested: bool,
    /// Where the mark goes, set together with `visible_requested`. `None`
    /// for an invisible signature.
    pub placement: Option<AppearancePlan>,
}

impl SigningOptions {
    /// Turn the visible/invisible choice on or off, keeping `placement` in
    /// lock-step so a caller can never observe `visible_requested: true`
    /// with `placement: None` or vice versa. Turning it on resolves
    /// `context` — what the application knows about the selected target and
    /// the page the reader is showing — into a plan; turning it off drops it,
    /// so re-enabling asks the application again rather than resurfacing a
    /// box captured against a page that is no longer showing.
    ///
    /// A preset already chosen survives being re-resolved against the same
    /// kind of context, so re-selecting a target or re-pressing Visible does
    /// not reset position and size.
    pub fn set_visible(&mut self, visible: bool, context: &PlacementContext) {
        self.visible_requested = visible;
        self.placement = visible.then(|| context.plan(self.chosen_preset()));
    }

    /// Re-resolve an already-made visible choice against a new `context`,
    /// leaving an invisible signature invisible. Selecting a different target
    /// field changes where the mark would land but not whether the reader
    /// asked for one, so the visible flag is carried across.
    pub fn retarget(&mut self, context: &PlacementContext) {
        if self.visible_requested {
            self.set_visible(true, context);
        }
    }

    /// The preset the reader picked, if the current plan uses presets at all.
    pub fn chosen_preset(&self) -> Option<Placement> {
        match self.placement.as_ref() {
            Some(AppearancePlan::Preset { placement, .. }) => Some(*placement),
            _ => None,
        }
    }

    /// The page the appearance will be drawn on, for the caller that has to
    /// look up that page's geometry before building the rect. `None` for an
    /// invisible signature.
    pub fn appearance_page_index(&self) -> Option<usize> {
        self.placement.as_ref().map(AppearancePlan::page_index)
    }
}

/// Where a visible signature's appearance is drawn.
///
/// The two variants are the two `pulpit_render::sign::AppearancePlacement`
/// cases, carrying the extra context the dialog needs to say where the mark
/// will land before it exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppearancePlan {
    /// Inside the target field's own box, wherever the document already puts
    /// it. The engine reads the rect from the field itself
    /// (`AppearancePlacement::FieldRect`) and never moves it; `field` and
    /// `page_index` are carried only so the dialog can name the destination.
    InFieldBox { field: String, page_index: usize },
    /// A preset box on `page_index` — the page the reader was showing when
    /// the Visible choice was made, captured then rather than read again at
    /// Sign time so scrolling in between cannot move the mark.
    Preset {
        page_index: usize,
        placement: Placement,
    },
}

impl AppearancePlan {
    pub fn page_index(&self) -> usize {
        match self {
            AppearancePlan::InFieldBox { page_index, .. } => *page_index,
            AppearancePlan::Preset { page_index, .. } => *page_index,
        }
    }
}

/// What the application knows, at the moment a visible choice is made, about
/// where the mark could go: the selected target's own box when it has a
/// usable one, and otherwise the page the reader is looking at.
///
/// Computed by the application (only it can see the reader and the document's
/// fields) and passed in, which is what keeps this module free of both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementContext {
    /// The selected target is an existing field whose widget the application
    /// located, with a box big enough to draw in.
    FieldBox { field: String, page_index: usize },
    /// No located box: a new field, or an existing field the application
    /// could not place. Presets apply, against `page_index`.
    Presets { page_index: usize },
}

impl PlacementContext {
    /// Resolve into a plan, keeping `chosen` when presets still apply.
    fn plan(&self, chosen: Option<Placement>) -> AppearancePlan {
        match self {
            PlacementContext::FieldBox { field, page_index } => AppearancePlan::InFieldBox {
                field: field.clone(),
                page_index: *page_index,
            },
            PlacementContext::Presets { page_index } => AppearancePlan::Preset {
                page_index: *page_index,
                placement: chosen.unwrap_or_default(),
            },
        }
    }

    /// Whether the target's own box decides the placement, in which case the
    /// dialog offers no presets: the sender drew the rectangle.
    pub fn is_field_box(&self) -> bool {
        matches!(self, PlacementContext::FieldBox { .. })
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

    /// The same choice mid-sentence: "…at the top left of page 3."
    pub fn spot_label(self) -> &'static str {
        match self {
            PlacementPosition::TopLeft => "top left",
            PlacementPosition::TopRight => "top right",
            PlacementPosition::BottomLeft => "bottom left",
            PlacementPosition::BottomRight => "bottom right",
            PlacementPosition::Center => "center",
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
    /// The widget box in *canonical page space*: y-down, origin at the top
    /// left of the page **as displayed**, which is the space the reader's
    /// choice is made in and the space `PageGeometry::width`/`height`
    /// measure. The box is clamped to fit inside the margin on pages too
    /// small for the chosen preset, rather than overhanging the page.
    ///
    /// This is deliberately not a PDF `/Rect`: the two differ by the crop
    /// origin, by `/Rotate`, and by the direction y runs in. Use
    /// [`Placement::user_space_rect`] for anything that will be written to a
    /// file.
    pub fn display_rect(&self, geometry: &PageGeometry) -> PageRect {
        let page_width = f64::from(geometry.width);
        let page_height = f64::from(geometry.height);
        let (preset_w, preset_h) = self.size.dims_pt();
        let w = preset_w.min((page_width - 2.0 * PLACEMENT_MARGIN_PT).max(1.0));
        let h = preset_h.min((page_height - 2.0 * PLACEMENT_MARGIN_PT).max(1.0));
        // Chosen against the sheet the way a reader describes it — "bottom
        // right" is near the bottom edge — and then flipped, once, here.
        let (left, bottom_up) = match self.position {
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
        let top = page_height - bottom_up - h;
        PageRect::new(left as f32, top as f32, (left + w) as f32, (top + h) as f32)
    }

    /// The widget rect as a PDF `/Rect`: `[x0, y0, x1, y1]` in **user
    /// space**, which is what a `/Widget` annotation carries and what every
    /// conformant viewer positions the mark by.
    ///
    /// The conversion is not cosmetic. A page whose crop box starts away from
    /// the user-space origin, or that carries a `/Rotate`, displays at an
    /// offset and an orientation the reader never sees; a box measured
    /// on screen and written straight into `/Rect` lands somewhere else. The
    /// mapping goes corner by corner through
    /// [`PageGeometry::rect_to_user_space`], which normalises the result —
    /// a quarter turn permutes the corners, so the smaller coordinate is not
    /// the one it started as.
    pub fn user_space_rect(&self, geometry: &PageGeometry) -> [f64; 4] {
        geometry
            .rect_to_user_space(self.display_rect(geometry))
            .map(f64::from)
    }
}

/// The engine's name for a page's `/Rotate`, which it cannot read for itself.
fn appearance_rotation(rotation: PageRotation) -> pulpit_render::sign::AppearanceRotation {
    use pulpit_render::sign::AppearanceRotation;
    match rotation {
        PageRotation::None => AppearanceRotation::None,
        PageRotation::Clockwise90 => AppearanceRotation::Cw90,
        PageRotation::Clockwise180 => AppearanceRotation::Cw180,
        PageRotation::Clockwise270 => AppearanceRotation::Cw270,
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

/// Translate a plan into the engine's placement. `geometry` is that of the
/// page [`SigningOptions::appearance_page_index`] names — the caller looks it
/// up from that index rather than guessing, which is what keeps the rect
/// sized against, and expressed in the space of, the page it will land on.
/// Ignored for [`AppearancePlan::InFieldBox`], where the engine reads the
/// rect from the field itself.
fn placement_for(
    plan: &AppearancePlan,
    geometry: &PageGeometry,
) -> pulpit_render::sign::AppearancePlacement {
    match plan {
        AppearancePlan::InFieldBox { .. } => pulpit_render::sign::AppearancePlacement::FieldRect,
        AppearancePlan::Preset {
            page_index,
            placement,
        } => pulpit_render::sign::AppearancePlacement::Rect {
            page_index: *page_index,
            rect: placement.user_space_rect(geometry),
        },
    }
}

/// Build the `SignAppearance` for `options`, or `None` for an invisible
/// signature, with §25.5's default text template as its content. See
/// [`placement_for`] for which page's geometry to pass.
#[cfg(test)]
pub fn appearance_for(
    options: &SigningOptions,
    signer_cn: &str,
    signing_time_label: &str,
    geometry: &PageGeometry,
) -> Option<pulpit_render::sign::SignAppearance> {
    let plan = options.placement.as_ref()?;
    Some(pulpit_render::sign::SignAppearance {
        placement: placement_for(plan, geometry),
        content: text_appearance_content(signer_cn, signing_time_label),
        page_rotation: appearance_rotation(geometry.rotation),
    })
}

/// Seed the per-document options with what the target itself implies, then
/// with a profile's saved visibility and box presets.
///
/// Signing into a field whose box the application located is visible by
/// default whether or not a profile is in play, and whether or not that
/// profile saved a visible default: the sender drew a rectangle for a mark,
/// so leaving it empty would be the surprising answer. Presets never apply
/// there, so a profile's saved position and size are simply not consulted.
pub fn apply_target_defaults(
    options: &mut SigningOptions,
    profile: Option<&crate::settings::StoredSignatureAppearance>,
    context: &PlacementContext,
) {
    if context.is_field_box() {
        options.set_visible(true, context);
        return;
    }
    match profile {
        Some(appearance) => apply_profile_defaults(options, appearance, context),
        None => options.set_visible(false, context),
    }
}

/// Seed the per-document options with a profile's saved visibility and box
/// presets.
pub fn apply_profile_defaults(
    options: &mut SigningOptions,
    appearance: &crate::settings::StoredSignatureAppearance,
    context: &PlacementContext,
) {
    if !appearance.visible {
        options.set_visible(false, context);
        return;
    }
    options.set_visible(true, context);
    let preset = Placement {
        position: match appearance.position {
            crate::settings::StoredSignaturePosition::TopLeft => PlacementPosition::TopLeft,
            crate::settings::StoredSignaturePosition::TopRight => PlacementPosition::TopRight,
            crate::settings::StoredSignaturePosition::BottomLeft => PlacementPosition::BottomLeft,
            crate::settings::StoredSignaturePosition::BottomRight => PlacementPosition::BottomRight,
            crate::settings::StoredSignaturePosition::Center => PlacementPosition::Center,
        },
        size: match appearance.size {
            crate::settings::StoredSignatureSize::Small => PlacementSize::Small,
            crate::settings::StoredSignatureSize::Medium => PlacementSize::Medium,
            crate::settings::StoredSignatureSize::Large => PlacementSize::Large,
        },
    };
    if let Some(AppearancePlan::Preset { placement, .. }) = options.placement.as_mut() {
        *placement = preset;
    }
}

/// Build an appearance using a saved profile's content. Passing `None`
/// preserves the ad-hoc credential flow's text-only appearance.
pub fn appearance_for_profile(
    options: &SigningOptions,
    profile: Option<&crate::settings::StoredSignatureAppearance>,
    signer_cn: &str,
    signing_time_label: &str,
    geometry: &PageGeometry,
) -> Option<pulpit_render::sign::SignAppearance> {
    let plan = options.placement.as_ref()?;
    let text = || text_appearance_content(signer_cn, signing_time_label);
    let content = match profile {
        None => text(),
        Some(appearance) => {
            let strokes = appearance
                .strokes
                .iter()
                .map(|stroke| {
                    stroke
                        .iter()
                        .map(|point| (f64::from(point.x), f64::from(point.y)))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            match appearance.content {
                crate::settings::StoredSignatureContent::Ink => {
                    pulpit_render::sign::AppearanceContent::Ink {
                        strokes,
                        stroke_width: f64::from(appearance.stroke_width),
                    }
                }
                crate::settings::StoredSignatureContent::Text => text(),
                crate::settings::StoredSignatureContent::InkAndText => {
                    let pulpit_render::sign::AppearanceContent::Text {
                        signer_name,
                        time_label,
                    } = text()
                    else {
                        unreachable!("text appearance constructor returned non-text content")
                    };
                    pulpit_render::sign::AppearanceContent::InkAndText {
                        strokes,
                        stroke_width: f64::from(appearance.stroke_width),
                        signer_name,
                        time_label,
                    }
                }
            }
        }
    };
    Some(pulpit_render::sign::SignAppearance {
        placement: placement_for(plan, geometry),
        content,
        page_rotation: appearance_rotation(geometry.rotation),
    })
}

/// The confirmation step's target line (§31.1 step 7): what is being signed
/// into, whether a mark will be visible, and where that mark will land. The
/// last part is what makes the line honest — "visible" alone does not say
/// whether the mark lands in the box the sender drew or somewhere this
/// dialog chose.
pub fn confirm_target_line(options: &SigningOptions) -> String {
    // 1-based, the way every page number a reader sees is.
    let page_no = |index: usize| index + 1;
    let visibility = if options.visible_requested {
        "visible"
    } else {
        "invisible"
    };
    match (options.target.as_ref(), options.placement.as_ref()) {
        (
            Some(TargetChoice::ExistingField(name)),
            Some(AppearancePlan::InFieldBox { page_index, .. }),
        ) => format!(
            "Signing into field “{name}”, visible inside its own box on page {}.",
            page_no(*page_index)
        ),
        (
            Some(TargetChoice::ExistingField(name)),
            Some(AppearancePlan::Preset {
                page_index,
                placement,
            }),
        ) => format!(
            "Signing into field “{name}”, visible at the {} of page {}.",
            placement.position.spot_label(),
            page_no(*page_index)
        ),
        (Some(TargetChoice::ExistingField(name)), None) => {
            format!("Signing into field “{name}”, {visibility}.")
        }
        (
            Some(TargetChoice::NewField),
            Some(AppearancePlan::Preset {
                page_index,
                placement,
            }),
        ) => format!(
            "A new, visible signature field will be created at the {} of page {}.",
            placement.position.spot_label(),
            page_no(*page_index)
        ),
        // A new field has no box of its own to draw inside, so the
        // application never builds `InFieldBox` for one; say the plain thing
        // rather than invent a page number this arm cannot vouch for.
        (Some(TargetChoice::NewField), _) => {
            format!("A new, {visibility} signature field will be created.")
        }
        // Unreachable: `ContinueToConfirm` diverts to `Failed` rather than
        // let the Confirm step build with no target.
        (None, _) => String::new(),
    }
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
    /// Step 4 when at least one saved profile is known.
    ChooseProfile,
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
    pub fn start(has_profiles: bool) -> Self {
        if has_profiles {
            SigningFlow::ChooseProfile
        } else {
            SigningFlow::ChooseCredential
        }
    }
}

/// What the Sign dialog can ask `app.rs` to do. Grouped under one
/// `Message::Sign(SignMsg)` variant, the way `TimerCommand` and
/// `ReadCommand` already group their own popups' messages.
#[derive(Debug, Clone)]
pub enum SignMsg {
    /// The toolbar's Sign button.
    Start,
    /// The page surface's signature dead-field was clicked (§31.1, with the
    /// field pre-selected in step 5's target picker — see
    /// `pick_signing_target`'s prefill handling).
    StartInField(String),
    /// Cancel at any step. Always safe (§33): nothing has been written yet,
    /// or the write was to a temporary file that is now discarded.
    Cancel,
    ProfileChosen(String),
    UseAnotherCredential,
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
    /// Visible/invisible toggle. Turning it on is what captures the
    /// [`PlacementContext`] — the target's own box, or the page the reader is
    /// showing — so the answer cannot drift afterwards.
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
    fn an_existing_field_target_alone_does_not_imply_the_countersign_disclosure() {
        // `countersigning` is set only from the preflight pass that computed
        // the candidates (`App::sign_target_candidates_from_signals`, whose
        // tests pin when it is true) — never derived from the target's
        // shape: an existing empty field is offered on unsigned documents
        // too, where §31.3's countersign disclosure would be false.
        let options = SigningOptions {
            target: Some(TargetChoice::ExistingField("Sig1".into())),
            ..Default::default()
        };
        assert!(!options.countersigning);
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
    fn pick_signing_target_prefers_a_matching_prefill() {
        let candidates = vec![
            TargetChoice::ExistingField("Sig1".to_string()),
            TargetChoice::ExistingField("Sig2".to_string()),
            TargetChoice::NewField,
        ];
        assert_eq!(
            pick_signing_target(&candidates, Some("Sig2")),
            Some(TargetChoice::ExistingField("Sig2".to_string()))
        );
    }

    #[test]
    fn pick_signing_target_falls_back_to_the_first_candidate_without_a_prefill() {
        let candidates = vec![
            TargetChoice::ExistingField("Sig1".to_string()),
            TargetChoice::NewField,
        ];
        assert_eq!(
            pick_signing_target(&candidates, None),
            Some(TargetChoice::ExistingField("Sig1".to_string()))
        );
    }

    #[test]
    fn pick_signing_target_falls_back_when_the_prefill_no_longer_matches() {
        let candidates = vec![
            TargetChoice::ExistingField("Sig1".to_string()),
            TargetChoice::NewField,
        ];
        assert_eq!(
            pick_signing_target(&candidates, Some("Sig9")),
            Some(TargetChoice::ExistingField("Sig1".to_string()))
        );
    }

    #[test]
    fn pick_signing_target_is_none_with_no_candidates() {
        assert_eq!(pick_signing_target(&[], Some("Sig1")), None);
    }

    #[test]
    fn flow_starts_at_choose_credential() {
        assert!(matches!(
            SigningFlow::start(false),
            SigningFlow::ChooseCredential
        ));
        assert!(matches!(
            SigningFlow::start(true),
            SigningFlow::ChooseProfile
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

    /// Presets against page 3 (0-based), the shape the reader gets on a
    /// new field while looking at the fourth page.
    fn presets_on(page_index: usize) -> PlacementContext {
        PlacementContext::Presets { page_index }
    }

    fn field_box_on(field: &str, page_index: usize) -> PlacementContext {
        PlacementContext::FieldBox {
            field: field.to_string(),
            page_index,
        }
    }

    #[test]
    fn set_visible_keeps_placement_in_lock_step() {
        let context = presets_on(0);
        let mut options = SigningOptions::default();
        assert!(!options.visible_requested);
        assert_eq!(options.placement, None);

        options.set_visible(true, &context);
        assert!(options.visible_requested);
        assert_eq!(
            options.placement,
            Some(AppearancePlan::Preset {
                page_index: 0,
                placement: Placement::default(),
            })
        );

        options.set_visible(false, &context);
        assert!(!options.visible_requested);
        assert_eq!(options.placement, None);
    }

    #[test]
    fn set_visible_captures_the_page_the_reader_is_showing() {
        let mut options = SigningOptions::default();
        options.set_visible(true, &presets_on(4));
        assert_eq!(options.appearance_page_index(), Some(4));
    }

    #[test]
    fn set_visible_true_twice_keeps_the_chosen_preset() {
        let context = presets_on(2);
        let mut options = SigningOptions::default();
        options.set_visible(true, &context);
        options.placement = Some(AppearancePlan::Preset {
            page_index: 2,
            placement: Placement {
                position: PlacementPosition::TopLeft,
                size: PlacementSize::Large,
            },
        });
        // Re-toggling on (a no-op in the view, but exercised here directly)
        // must not reset a preset the user already chose.
        options.set_visible(true, &context);
        assert_eq!(
            options.chosen_preset(),
            Some(Placement {
                position: PlacementPosition::TopLeft,
                size: PlacementSize::Large,
            })
        );
    }

    #[test]
    fn a_located_existing_field_defaults_to_a_visible_mark_in_its_own_box() {
        let mut options = SigningOptions {
            target: Some(TargetChoice::ExistingField("Sig1".into())),
            ..Default::default()
        };
        apply_target_defaults(&mut options, None, &field_box_on("Sig1", 2));
        assert!(options.visible_requested);
        assert_eq!(
            options.placement,
            Some(AppearancePlan::InFieldBox {
                field: "Sig1".into(),
                page_index: 2,
            })
        );
    }

    #[test]
    fn a_located_existing_field_is_visible_even_for_a_profile_that_saved_invisible() {
        let profile = crate::settings::StoredSignatureAppearance {
            visible: false,
            ..Default::default()
        };
        let mut options = SigningOptions::default();
        apply_target_defaults(&mut options, Some(&profile), &field_box_on("Sig1", 0));
        assert!(options.visible_requested);
        assert!(matches!(
            options.placement,
            Some(AppearancePlan::InFieldBox { .. })
        ));
    }

    #[test]
    fn an_unlocatable_field_falls_back_to_the_invisible_default() {
        // The application could not place the field — no widget, or a
        // degenerate `/Rect` the engine would refuse — so it hands over a
        // presets context and nothing turns Visible on by itself.
        let mut options = SigningOptions {
            target: Some(TargetChoice::ExistingField("Sig1".into())),
            ..Default::default()
        };
        apply_target_defaults(&mut options, None, &presets_on(3));
        assert!(!options.visible_requested);
        assert_eq!(options.placement, None);
    }

    #[test]
    fn a_located_field_appearance_draws_inside_the_field_rect_with_the_profile_s_ink() {
        let profile = crate::settings::StoredSignatureAppearance {
            content: crate::settings::StoredSignatureContent::Ink,
            strokes: vec![vec![crate::settings::SignaturePoint { x: 0.1, y: 0.2 }]],
            stroke_width: 2.0,
            visible: true,
            ..Default::default()
        };
        let mut options = SigningOptions {
            target: Some(TargetChoice::ExistingField("Sig1".into())),
            ..Default::default()
        };
        apply_target_defaults(&mut options, Some(&profile), &field_box_on("Sig1", 1));
        let appearance = appearance_for_profile(
            &options,
            Some(&profile),
            "Jane Doe",
            "now",
            &PageGeometry::upright(612.0, 792.0),
        )
        .expect("a located field is visible by default");
        assert_eq!(
            appearance.placement,
            pulpit_render::sign::AppearancePlacement::FieldRect
        );
        assert!(matches!(
            appearance.content,
            pulpit_render::sign::AppearanceContent::Ink { .. }
        ));
    }

    #[test]
    fn a_preset_appearance_targets_the_captured_page() {
        let mut options = SigningOptions {
            target: Some(TargetChoice::NewField),
            ..Default::default()
        };
        options.set_visible(true, &presets_on(3));
        let appearance = appearance_for_profile(
            &options,
            None,
            "Jane Doe",
            "now",
            &PageGeometry::upright(612.0, 792.0),
        )
        .expect("visible options produce an appearance");
        assert_eq!(
            appearance.placement,
            pulpit_render::sign::AppearancePlacement::Rect {
                page_index: 3,
                rect: Placement::default().user_space_rect(&PageGeometry::upright(612.0, 792.0)),
            }
        );
    }

    #[test]
    fn retarget_keeps_the_visible_choice_and_moves_the_mark() {
        let mut options = SigningOptions::default();
        options.set_visible(true, &presets_on(0));
        options.retarget(&field_box_on("Sig2", 5));
        assert!(options.visible_requested);
        assert_eq!(
            options.placement,
            Some(AppearancePlan::InFieldBox {
                field: "Sig2".into(),
                page_index: 5,
            })
        );
    }

    #[test]
    fn retarget_leaves_an_invisible_signature_invisible() {
        let mut options = SigningOptions::default();
        options.retarget(&field_box_on("Sig2", 5));
        assert!(!options.visible_requested);
        assert_eq!(options.placement, None);
    }

    #[test]
    fn the_confirm_line_names_where_the_mark_lands() {
        let mut options = SigningOptions {
            target: Some(TargetChoice::ExistingField("Sig1".into())),
            ..Default::default()
        };
        assert_eq!(
            confirm_target_line(&options),
            "Signing into field “Sig1”, invisible."
        );
        options.set_visible(true, &field_box_on("Sig1", 2));
        assert_eq!(
            confirm_target_line(&options),
            "Signing into field “Sig1”, visible inside its own box on page 3."
        );

        let mut options = SigningOptions {
            target: Some(TargetChoice::NewField),
            ..Default::default()
        };
        assert_eq!(
            confirm_target_line(&options),
            "A new, invisible signature field will be created."
        );
        options.set_visible(true, &presets_on(4));
        assert_eq!(
            confirm_target_line(&options),
            "A new, visible signature field will be created at the bottom right of page 5."
        );
    }

    #[test]
    fn placement_rect_sits_inside_the_margin_at_each_corner() {
        let page_w = 612.0;
        let page_h = 792.0;
        let geometry = PageGeometry::upright(page_w as f32, page_h as f32);
        for position in PlacementPosition::ALL {
            let placement = Placement {
                position,
                size: PlacementSize::Medium,
            };
            let [x0, y0, x1, y1] = placement.user_space_rect(&geometry);
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

    /// The regression guard for the conversion below: on the page almost
    /// every document is made of — upright, crop box at the origin — the
    /// user-space rect must be *exactly* the numbers this flow produced
    /// before user space was ever mentioned.
    #[test]
    fn an_upright_page_at_the_origin_places_the_box_where_it_always_did() {
        let geometry = PageGeometry::upright(612.0, 792.0);
        // Bottom right, medium: 200x70 half an inch in from the corner.
        assert_eq!(
            Placement::default().user_space_rect(&geometry),
            [376.0, 36.0, 576.0, 106.0]
        );
        assert_eq!(
            Placement {
                position: PlacementPosition::TopLeft,
                size: PlacementSize::Small,
            }
            .user_space_rect(&geometry),
            [36.0, 706.0, 186.0, 756.0]
        );
    }

    /// A crop box that does not start at the user-space origin: the reader
    /// measures against the crop box, `/Rect` is measured against user space,
    /// and the difference is the crop origin.
    #[test]
    fn a_crop_offset_page_places_the_box_in_user_space() {
        let geometry = PageGeometry::new(50.0, 20.0, 512.0, 692.0, PageRotation::None, 1.0);
        assert_eq!(geometry.width, 512.0);
        assert_eq!(geometry.height, 692.0);
        // Displayed: x from 512-36-200=276 to 476, y from 36 to 106 up from
        // the crop box's bottom. In user space that is the same box shifted
        // by the crop origin (50, 20).
        assert_eq!(
            Placement::default().user_space_rect(&geometry),
            [326.0, 56.0, 526.0, 126.0]
        );
    }

    /// A quarter-turned page: the box the reader sees at the bottom right of
    /// a 792x612 landscape display is, on the sheet itself, a *portrait* box
    /// near the top right — and its width and height are exchanged.
    #[test]
    fn a_rotated_page_places_the_box_in_user_space() {
        let geometry = PageGeometry::new(0.0, 0.0, 612.0, 792.0, PageRotation::Clockwise90, 1.0);
        assert_eq!((geometry.width, geometry.height), (792.0, 612.0));
        // Canonical (y-down) box: left 556, top 506, right 756, bottom 576.
        // Clockwise90: (x, y) -> user (y, crop_height - (crop_height - x)) =
        // (y, x). So (556, 506) -> (506, 556) and (756, 576) -> (576, 756).
        assert_eq!(
            Placement::default().user_space_rect(&geometry),
            [506.0, 556.0, 576.0, 756.0]
        );
        let [x0, y0, x1, y1] = Placement::default().user_space_rect(&geometry);
        assert_eq!((x1 - x0, y1 - y0), (70.0, 200.0), "the axes exchange");
        assert!(x1 <= 612.0 && y1 <= 792.0, "the box must be on the sheet");
    }

    #[test]
    fn a_rotated_page_carries_its_rotation_to_the_engine() {
        let mut options = SigningOptions::default();
        options.set_visible(true, &presets_on(0));
        let geometry = PageGeometry::new(0.0, 0.0, 612.0, 792.0, PageRotation::Clockwise270, 1.0);
        let appearance = appearance_for(&options, "Jane Doe", "now", &geometry)
            .expect("visible options produce an appearance");
        assert_eq!(
            appearance.page_rotation,
            pulpit_render::sign::AppearanceRotation::Cw270
        );
        // The field-box path draws on the field's own page, so it needs the
        // same treatment: the rect is the sender's, the orientation is not.
        options.retarget(&field_box_on("Sig1", 0));
        let appearance = appearance_for(&options, "Jane Doe", "now", &geometry).unwrap();
        assert_eq!(
            appearance.placement,
            pulpit_render::sign::AppearancePlacement::FieldRect
        );
        assert_eq!(
            appearance.page_rotation,
            pulpit_render::sign::AppearanceRotation::Cw270
        );
    }

    #[test]
    fn placement_rect_clamps_to_a_tiny_page_without_overhanging() {
        let placement = Placement {
            position: PlacementPosition::Center,
            size: PlacementSize::Large,
        };
        let [x0, y0, x1, y1] = placement.user_space_rect(&PageGeometry::upright(100.0, 100.0));
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
        assert!(appearance_for(
            &options,
            "Jane Doe",
            "now",
            &PageGeometry::upright(612.0, 792.0)
        )
        .is_none());
    }

    #[test]
    fn appearance_for_targets_the_captured_page_with_the_chosen_rect() {
        let mut options = SigningOptions::default();
        options.set_visible(true, &presets_on(1));
        let appearance = appearance_for(
            &options,
            "Jane Doe",
            "now",
            &PageGeometry::upright(612.0, 792.0),
        )
        .expect("visible options produce an appearance");
        assert_eq!(
            appearance.placement,
            pulpit_render::sign::AppearancePlacement::Rect {
                page_index: 1,
                rect: options
                    .chosen_preset()
                    .unwrap()
                    .user_space_rect(&PageGeometry::upright(612.0, 792.0)),
            }
        );
    }

    #[test]
    fn profile_defaults_and_ink_are_carried_into_the_pdf_appearance() {
        let profile = crate::settings::StoredSignatureAppearance {
            content: crate::settings::StoredSignatureContent::InkAndText,
            strokes: vec![vec![
                crate::settings::SignaturePoint { x: 0.1, y: 0.2 },
                crate::settings::SignaturePoint { x: 0.8, y: 0.7 },
            ]],
            stroke_width: 3.0,
            visible: true,
            position: crate::settings::StoredSignaturePosition::TopLeft,
            size: crate::settings::StoredSignatureSize::Large,
        };
        let mut options = SigningOptions::default();
        apply_profile_defaults(&mut options, &profile, &presets_on(0));
        assert_eq!(
            options.chosen_preset(),
            Some(Placement {
                position: PlacementPosition::TopLeft,
                size: PlacementSize::Large,
            })
        );
        let appearance = appearance_for_profile(
            &options,
            Some(&profile),
            "Jane Doe",
            "now",
            &PageGeometry::upright(612.0, 792.0),
        )
        .unwrap();
        match appearance.content {
            pulpit_render::sign::AppearanceContent::InkAndText {
                strokes,
                stroke_width,
                signer_name,
                ..
            } => {
                assert_eq!(strokes.len(), 1);
                assert_eq!(strokes[0].len(), 2);
                for (actual, expected) in strokes[0].iter().zip([(0.1, 0.2), (0.8, 0.7)]) {
                    assert!((actual.0 - expected.0).abs() < 1e-6);
                    assert!((actual.1 - expected.1).abs() < 1e-6);
                }
                assert_eq!(stroke_width, 3.0);
                assert_eq!(signer_name, "Digitally signed by Jane Doe.");
            }
            other => panic!("expected combined profile appearance, got {other:?}"),
        }
    }

    #[test]
    fn a_profile_that_saved_invisible_stays_invisible() {
        let profile = crate::settings::StoredSignatureAppearance {
            visible: false,
            ..Default::default()
        };
        let mut options = SigningOptions::default();
        apply_profile_defaults(&mut options, &profile, &presets_on(3));
        assert!(!options.visible_requested);
        assert!(options.placement.is_none());
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
