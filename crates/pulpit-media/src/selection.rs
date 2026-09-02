//! Candidate ordering and fallback (`docs-src/internals.typ`).
//!
//! Selection is pure: given the probes and a policy it produces an ordered
//! list of attempts and a record of why each candidate was skipped. Nothing
//! here launches a process, so every ordering rule and every fallback trigger
//! is testable without a browser installed.

use pulpit_core::overlay::ContentKind;
use serde::{Deserialize, Serialize};

use crate::capability::{RuntimeProbe, UnmetRequirement};
use crate::protocol::{CapabilityRequest, MediaErrorKind, RuntimeId};

/// What the user or packager asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RuntimePolicy {
    /// Follow the default candidate order.
    #[default]
    Auto,
    /// Move one candidate to the front, but keep falling back past it.
    Prefer(RuntimeId),
    /// Try only this candidate, then the static fallback.
    Require(RuntimeId),
}

impl RuntimePolicy {
    pub fn parse(value: &str) -> Option<RuntimePolicy> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("auto") {
            return Some(RuntimePolicy::Auto);
        }
        if let Some(rest) = value.strip_prefix('!') {
            return RuntimeId::from_slug(rest).map(RuntimePolicy::Require);
        }
        RuntimeId::from_slug(value).map(RuntimePolicy::Prefer)
    }
}

/// The default automatic order, per content kind (`docs-src/internals.typ`).
///
/// An installed Chromium-family browser plays all three content kinds, so it
/// appears in every order — leading for web, and behind libmpv for plain
/// media, which libmpv decodes far more cheaply.
pub fn default_order(kind: ContentKind) -> Vec<RuntimeId> {
    match kind {
        // Plain media prefers an installed ffmpeg: it decodes straight to
        // RGBA with no JPEG round trip and no browser process, an order of
        // magnitude cheaper than the screencast path. The browser stays as
        // the fallback that already decodes everything.
        ContentKind::AnimatedImage => vec![RuntimeId::LibMpv, RuntimeId::ExternalChromium],
        ContentKind::Video => vec![RuntimeId::LibMpv, RuntimeId::ExternalChromium],
        ContentKind::Web => vec![RuntimeId::ExternalChromium],
    }
}

/// The order in which Chromium-family browsers are looked for. An explicitly
/// configured executable always leads; Chrome Stable is the first-class
/// implementation and the only one covered by required CI.
/// `msedge` is Windows' name for Edge and matches none of the Unix spellings;
/// without it the browser preinstalled on every Windows machine is not
/// recognised as Chromium-family even once it has been found. The `.exe`
/// suffix is added by [`crate::runtime::which`] rather than listed here, so
/// each browser appears once.
pub const CHROMIUM_EXECUTABLES: &[&str] = &[
    "google-chrome-stable",
    "google-chrome",
    "chrome",
    "microsoft-edge-stable",
    "microsoft-edge",
    "msedge",
    "chromium",
    "chromium-browser",
    "brave-browser",
    "brave",
];

/// Firefox and Safari are not Chromium-family and must never be launched
/// through the CDP adapter, however tempting the name similarity.
pub fn is_chromium_family(executable: &str) -> bool {
    let name = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .to_ascii_lowercase();
    if name.contains("firefox") || name.contains("safari") || name.contains("librewolf") {
        return false;
    }
    CHROMIUM_EXECUTABLES
        .iter()
        .any(|candidate| name.starts_with(candidate))
}

/// One candidate's fate during selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub runtime: RuntimeId,
    pub outcome: AttemptOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttemptOutcome {
    /// Chosen; this is the runtime that will be launched.
    Selected,
    /// Skipped before launch because a required capability was missing.
    Skipped(UnmetRequirement),
    /// Launched and failed. Recorded by the supervisor, not by ranking.
    Failed {
        kind: MediaErrorKind,
        detail: String,
    },
    /// Not reached: an earlier candidate was selected.
    NotReached,
}

impl AttemptOutcome {
    pub fn describe(&self) -> String {
        match self {
            AttemptOutcome::Selected => "selected".to_string(),
            AttemptOutcome::Skipped(reason) => format!("skipped — {reason}"),
            AttemptOutcome::Failed { detail, .. } => format!("failed — {detail}"),
            AttemptOutcome::NotReached => "not reached".to_string(),
        }
    }
}

/// The result of ranking candidates for one overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    /// The chosen runtime, if any candidate satisfied the request.
    pub selected: Option<RuntimeId>,
    /// Remaining candidates to try, in order, should the selection fail.
    pub fallbacks: Vec<RuntimeId>,
    /// Every candidate considered, in the order considered, with its fate.
    pub attempts: Vec<Attempt>,
}

impl Selection {
    /// Nothing usable: the overlay shows its poster or the PDF page.
    pub fn is_static_fallback(&self) -> bool {
        self.selected.is_none()
    }

    /// A one-line explanation for preflight.
    pub fn describe(&self) -> String {
        match self.selected {
            Some(runtime) => format!("using {runtime}"),
            None => "no usable runtime; showing the poster or PDF page".to_string(),
        }
    }
}

/// The candidate order a policy produces, before capabilities are consulted.
pub fn candidate_order(kind: ContentKind, policy: RuntimePolicy) -> Vec<RuntimeId> {
    let default = default_order(kind);
    match policy {
        RuntimePolicy::Auto => default,
        RuntimePolicy::Require(runtime) => vec![runtime],
        RuntimePolicy::Prefer(runtime) => {
            let mut order = vec![runtime];
            order.extend(default.into_iter().filter(|other| *other != runtime));
            order
        }
    }
}

/// Rank the candidates for one overlay.
///
/// `probe_for` supplies each candidate's probe. A candidate lacking a
/// required capability is skipped with a structured reason rather than
/// launched and allowed to fail on stage.
pub fn select(
    kind: ContentKind,
    policy: RuntimePolicy,
    request: &CapabilityRequest,
    probe_for: impl Fn(RuntimeId) -> RuntimeProbe,
) -> Selection {
    let mut attempts = Vec::new();
    let mut usable = Vec::new();

    for runtime in candidate_order(kind, policy) {
        match probe_for(runtime).satisfies(request) {
            Ok(()) => {
                attempts.push(Attempt {
                    runtime,
                    outcome: if usable.is_empty() {
                        AttemptOutcome::Selected
                    } else {
                        AttemptOutcome::NotReached
                    },
                });
                usable.push(runtime);
            }
            Err(reason) => attempts.push(Attempt {
                runtime,
                outcome: AttemptOutcome::Skipped(reason),
            }),
        }
    }

    let mut usable = usable.into_iter();
    let selected = usable.next();
    Selection {
        selected,
        fallbacks: usable.collect(),
        attempts,
    }
}

/// Does this failure justify trying the next candidate?
///
/// A security-policy denial never does: finding a laxer runtime is exactly
/// the outcome the policy exists to prevent.
pub fn should_fall_back(kind: MediaErrorKind) -> bool {
    kind.allows_fallback()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Availability, ContentCapabilities, InputCapabilities};

    fn probe(id: RuntimeId, kinds: &[ContentKind], pointer: bool) -> RuntimeProbe {
        RuntimeProbe {
            content: ContentCapabilities {
                kinds: kinds.to_vec(),
                continuous_frames: true,
                ..Default::default()
            },
            input: InputCapabilities {
                pointer,
                ..Default::default()
            },
            ..RuntimeProbe::unavailable(id, Availability::Available)
        }
    }

    fn everything_works(id: RuntimeId) -> RuntimeProbe {
        probe(
            id,
            &[
                ContentKind::AnimatedImage,
                ContentKind::Video,
                ContentKind::Web,
            ],
            true,
        )
    }

    fn nothing_works(id: RuntimeId) -> RuntimeProbe {
        RuntimeProbe::unavailable(
            id,
            Availability::NotInstalled {
                detail: "not installed".into(),
            },
        )
    }

    #[test]
    fn the_default_order_is_the_one_the_specification_prescribes() {
        assert_eq!(
            default_order(ContentKind::AnimatedImage),
            vec![RuntimeId::LibMpv, RuntimeId::ExternalChromium]
        );
        assert_eq!(
            default_order(ContentKind::Video),
            vec![RuntimeId::LibMpv, RuntimeId::ExternalChromium],
            "plain media decodes natively first; the browser is the fallback"
        );
        assert_eq!(
            default_order(ContentKind::Web)[0],
            RuntimeId::ExternalChromium
        );
    }

    #[test]
    fn prefer_moves_one_candidate_up_without_removing_the_rest() {
        let order = candidate_order(ContentKind::Web, RuntimePolicy::Prefer(RuntimeId::LibMpv));
        assert_eq!(order[0], RuntimeId::LibMpv);
        assert!(
            order.contains(&RuntimeId::ExternalChromium),
            "preferring must not discard the default leader"
        );
    }

    #[test]
    fn require_tries_exactly_one_candidate() {
        let order = candidate_order(ContentKind::Web, RuntimePolicy::Require(RuntimeId::LibMpv));
        assert_eq!(order, vec![RuntimeId::LibMpv]);
    }

    #[test]
    fn selection_picks_the_first_capable_candidate_and_keeps_the_rest_as_fallbacks() {
        let selection = select(
            ContentKind::Video,
            RuntimePolicy::Auto,
            &CapabilityRequest::for_kind(ContentKind::Video),
            everything_works,
        );
        assert_eq!(selection.selected, Some(RuntimeId::LibMpv));
        assert_eq!(selection.fallbacks, vec![RuntimeId::ExternalChromium]);
        assert!(!selection.is_static_fallback());
    }

    #[test]
    fn an_incapable_leader_is_skipped_with_a_reason_and_the_next_wins() {
        let selection = select(
            ContentKind::Video,
            RuntimePolicy::Auto,
            &CapabilityRequest::for_kind(ContentKind::Video),
            |id| {
                if id == RuntimeId::LibMpv {
                    nothing_works(id)
                } else {
                    everything_works(id)
                }
            },
        );
        assert_eq!(selection.selected, Some(RuntimeId::ExternalChromium));
        assert!(matches!(
            selection.attempts[0].outcome,
            AttemptOutcome::Skipped(UnmetRequirement::Unavailable { .. })
        ));
    }

    #[test]
    fn when_nothing_is_capable_the_overlay_falls_back_statically() {
        let selection = select(
            ContentKind::Web,
            RuntimePolicy::Auto,
            &CapabilityRequest::for_kind(ContentKind::Web),
            nothing_works,
        );
        assert!(selection.is_static_fallback());
        assert_eq!(selection.fallbacks, Vec::new());
        assert_eq!(
            selection.attempts.len(),
            default_order(ContentKind::Web).len()
        );
        assert!(selection.describe().contains("poster"));
    }

    #[test]
    fn require_falls_back_statically_rather_than_to_another_runtime() {
        let selection = select(
            ContentKind::Web,
            RuntimePolicy::Require(RuntimeId::LibMpv),
            &CapabilityRequest::for_kind(ContentKind::Web),
            |id| {
                if id == RuntimeId::LibMpv {
                    nothing_works(id)
                } else {
                    everything_works(id)
                }
            },
        );
        assert!(
            selection.is_static_fallback(),
            "Require means that runtime or nothing"
        );
        assert_eq!(selection.attempts.len(), 1);
    }

    #[test]
    fn every_candidate_is_recorded_even_the_ones_never_reached() {
        let selection = select(
            ContentKind::AnimatedImage,
            RuntimePolicy::Auto,
            &CapabilityRequest::for_kind(ContentKind::AnimatedImage),
            everything_works,
        );
        assert_eq!(
            selection.attempts.len(),
            default_order(ContentKind::AnimatedImage).len()
        );
        assert_eq!(selection.attempts[0].outcome, AttemptOutcome::Selected);
        assert!(selection.attempts[1..]
            .iter()
            .all(|attempt| attempt.outcome == AttemptOutcome::NotReached));
    }

    #[test]
    fn a_policy_denial_stops_the_fallback_chain() {
        assert!(!should_fall_back(MediaErrorKind::PolicyDenied));
        assert!(should_fall_back(MediaErrorKind::LaunchFailed));
        assert!(should_fall_back(MediaErrorKind::DecodeFailed));
    }

    #[test]
    fn firefox_and_safari_are_never_chromium_family() {
        assert!(is_chromium_family("/usr/bin/google-chrome-stable"));
        assert!(is_chromium_family("chromium"));
        assert!(is_chromium_family("/opt/brave.com/brave/brave-browser"));
        assert!(!is_chromium_family("/usr/bin/firefox"));
        assert!(!is_chromium_family("Safari"));
        assert!(!is_chromium_family("/usr/bin/librewolf"));
        assert!(!is_chromium_family("/usr/bin/some-other-thing"));
    }

    /// Windows spells these differently, and the check splits on `\` as well
    /// as `/` precisely so a Windows path reaches the same answer.
    #[test]
    fn the_windows_spellings_are_chromium_family_too() {
        assert!(is_chromium_family(
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
        ));
        assert!(is_chromium_family(
            r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        ));
        assert!(is_chromium_family(
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe"
        ));
        // Case is not a Windows user's concern, and neither is it ours.
        assert!(is_chromium_family(r"C:\PROGRAM FILES\GOOGLE\CHROME.EXE"));
        assert!(!is_chromium_family(
            r"C:\Program Files\Mozilla Firefox\firefox.exe"
        ));
    }

    #[test]
    fn runtime_policy_parses_the_settings_vocabulary() {
        assert_eq!(RuntimePolicy::parse("auto"), Some(RuntimePolicy::Auto));
        assert_eq!(
            RuntimePolicy::parse("external-chromium"),
            Some(RuntimePolicy::Prefer(RuntimeId::ExternalChromium))
        );
        assert_eq!(
            RuntimePolicy::parse("!libmpv"),
            Some(RuntimePolicy::Require(RuntimeId::LibMpv))
        );
        assert_eq!(RuntimePolicy::parse("nonesuch"), None);
        // A retired runtime slug (e.g. from an old settings file) parses to
        // nothing rather than a runtime that no longer exists; the caller
        // treats that the same as any other unrecognised slug.
        assert_eq!(RuntimePolicy::parse("webkitgtk"), None);
    }
}
