//! Preflight and diagnostics (`docs-src/internals.typ`).
//!
//! Preflight is a presenter-facing surface, not a log: it exists so that a
//! missing browser, a denied network request or a degraded runtime is
//! discovered *before* the deck is on a projector. It is also where a
//! bundle's network request is approved, because a manifest alone is never
//! approval.

use std::collections::BTreeMap;

use pulpit_core::overlay::{ContentKind, OverlayWarning};
use pulpit_core::OverlayId;
use serde::{Deserialize, Serialize};

use crate::capability::{Limitation, RuntimeProbe};
use crate::protocol::RuntimeId;
use crate::selection::Selection;

/// What preflight says about one overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OverlayStatus {
    /// Ready, with the runtime that will show it.
    Ready {
        runtime: RuntimeId,
        version: Option<String>,
    },
    /// Ready, but not at full fidelity.
    Degraded {
        runtime: RuntimeId,
        limitations: Vec<Limitation>,
    },
    /// No usable runtime; the poster or the PDF page stands in.
    StaticFallback { reason: String },
    /// Refused on security grounds. Never resolved by finding a laxer
    /// runtime.
    Blocked { reason: String },
    /// The asset itself is unusable.
    Malformed { reason: String },
}

impl OverlayStatus {
    pub fn is_ready(&self) -> bool {
        matches!(
            self,
            OverlayStatus::Ready { .. } | OverlayStatus::Degraded { .. }
        )
    }

    pub fn headline(&self) -> String {
        match self {
            OverlayStatus::Ready { runtime, version } => match version {
                Some(version) => format!("ready — {runtime} ({version})"),
                None => format!("ready — {runtime}"),
            },
            OverlayStatus::Degraded {
                runtime,
                limitations,
            } => {
                let detail = limitations
                    .iter()
                    .map(Limitation::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("ready with limits — {runtime}: {detail}")
            }
            OverlayStatus::StaticFallback { reason } => format!("showing a still — {reason}"),
            OverlayStatus::Blocked { reason } => format!("blocked — {reason}"),
            OverlayStatus::Malformed { reason } => format!("unusable — {reason}"),
        }
    }
}

/// A network origin a bundle asked for, and whether it has been approved.
///
/// Approval is scoped to one document identity and survives restarts until
/// the manifest's request changes, at which point it must be presented again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkRequest {
    pub overlay: OverlayId,
    pub origin: String,
    pub approved: bool,
}

/// One overlay's line in the preflight report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayReport {
    pub overlay: OverlayId,
    pub kind: ContentKind,
    /// Physical PDF pages this one overlay spans; more than one means the
    /// producer used incremental reveals.
    pub pages: Vec<usize>,
    pub status: OverlayStatus,
    /// Why each candidate runtime was or was not chosen.
    pub attempts: Vec<String>,
    /// Problems found while parsing the declaration itself.
    pub warnings: Vec<OverlayWarning>,
    pub network: Vec<NetworkRequest>,
}

impl OverlayReport {
    /// Does the presenter need to do something before going on stage?
    pub fn needs_attention(&self) -> bool {
        !self.status.is_ready()
            || !self.warnings.is_empty()
            || self.network.iter().any(|request| !request.approved)
    }

    /// One line describing how this overlay's pages were grouped.
    pub fn grouping(&self) -> String {
        match self.pages.as_slice() {
            [] => "no page".to_string(),
            [single] => format!("page {}", single + 1),
            [first, .., last] => format!(
                "pages {}–{} (one overlay across {} reveal steps)",
                first + 1,
                last + 1,
                self.pages.len()
            ),
        }
    }
}

/// The whole preflight report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Preflight {
    pub overlays: Vec<OverlayReport>,
    /// The effective candidate order per content kind, after policy.
    pub candidate_order: BTreeMap<String, Vec<String>>,
    /// Every runtime probed, whether or not it was usable.
    pub runtimes: Vec<RuntimeSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSummary {
    pub runtime: RuntimeId,
    pub available: bool,
    pub detail: String,
    pub version: Option<String>,
    pub executable: Option<String>,
    pub limitations: Vec<String>,
}

impl RuntimeSummary {
    pub fn from_probe(probe: &RuntimeProbe) -> Self {
        Self {
            runtime: probe.id,
            available: probe.is_available(),
            detail: probe.availability.detail().to_string(),
            version: probe.version.clone(),
            executable: probe
                .executable
                .as_ref()
                .map(|path| path.display().to_string()),
            limitations: probe
                .limitations
                .iter()
                .map(Limitation::to_string)
                .collect(),
        }
    }
}

impl Preflight {
    /// Overlays the presenter should look at before starting.
    pub fn attention(&self) -> impl Iterator<Item = &OverlayReport> {
        self.overlays
            .iter()
            .filter(|report| report.needs_attention())
    }

    pub fn all_ready(&self) -> bool {
        self.overlays.iter().all(|report| report.status.is_ready())
    }

    /// Origins still waiting for a decision. Nothing reaches the network
    /// until these are approved.
    pub fn pending_approvals(&self) -> Vec<&NetworkRequest> {
        self.overlays
            .iter()
            .flat_map(|report| report.network.iter())
            .filter(|request| !request.approved)
            .collect()
    }

    /// A plain-text summary, suitable for a log or a `--check` run.
    pub fn to_report(&self) -> String {
        let mut out = String::new();
        out.push_str("Media preflight\n===============\n\n");

        out.push_str("Runtimes\n");
        for runtime in &self.runtimes {
            out.push_str(&format!(
                "  {:<18} {}\n",
                runtime.runtime.slug(),
                if runtime.available {
                    runtime
                        .version
                        .clone()
                        .unwrap_or_else(|| "available".to_string())
                } else {
                    runtime.detail.clone()
                }
            ));
            for limitation in &runtime.limitations {
                out.push_str(&format!("  {:<18}   · {limitation}\n", ""));
            }
        }

        if !self.candidate_order.is_empty() {
            out.push_str("\nCandidate order\n");
            for (kind, order) in &self.candidate_order {
                out.push_str(&format!("  {kind}: {}\n", order.join(" → ")));
            }
        }

        out.push_str("\nOverlays\n");
        if self.overlays.is_empty() {
            out.push_str("  (this document declares none)\n");
        }
        for report in &self.overlays {
            out.push_str(&format!(
                "  {} · {} · {}\n    {}\n",
                report.overlay,
                report.kind.label(),
                report.grouping(),
                report.status.headline()
            ));
            for warning in &report.warnings {
                out.push_str(&format!("    ! {warning}\n"));
            }
            for request in &report.network {
                out.push_str(&format!(
                    "    network {} — {}\n",
                    request.origin,
                    if request.approved {
                        "approved"
                    } else {
                        "awaiting approval; denied until then"
                    }
                ));
            }
            for attempt in &report.attempts {
                out.push_str(&format!("    · {attempt}\n"));
            }
        }
        out
    }
}

/// Turn a selection into the status preflight reports.
pub fn status_from_selection(selection: &Selection, probe: Option<&RuntimeProbe>) -> OverlayStatus {
    match selection.selected {
        Some(runtime) => {
            let limitations = probe
                .map(|probe| probe.limitations.clone())
                .unwrap_or_default();
            // A compressed frame transport is a real property of the runtime
            // but not something a presenter can act on, so it does not by
            // itself demote an overlay to "degraded".
            let actionable: Vec<Limitation> = limitations
                .into_iter()
                .filter(|limitation| !matches!(limitation, Limitation::CompressedFrames { .. }))
                .collect();
            if actionable.is_empty() {
                OverlayStatus::Ready {
                    runtime,
                    version: probe.and_then(|probe| probe.version.clone()),
                }
            } else {
                OverlayStatus::Degraded {
                    runtime,
                    limitations: actionable,
                }
            }
        }
        None => OverlayStatus::StaticFallback {
            reason: selection
                .attempts
                .first()
                .map(|attempt| attempt.outcome.describe())
                .unwrap_or_else(|| "no runtime was considered".to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Availability;
    use crate::selection::{Attempt, AttemptOutcome};

    fn ready_report() -> OverlayReport {
        OverlayReport {
            overlay: OverlayId(1),
            kind: ContentKind::Web,
            pages: vec![3],
            status: OverlayStatus::Ready {
                runtime: RuntimeId::ExternalChromium,
                version: Some("Chrome/140".into()),
            },
            attempts: vec!["external-chromium: selected".into()],
            warnings: Vec::new(),
            network: Vec::new(),
        }
    }

    #[test]
    fn a_ready_overlay_needs_no_attention() {
        assert!(!ready_report().needs_attention());
    }

    #[test]
    fn an_overlay_with_a_parse_warning_still_needs_a_look() {
        let report = OverlayReport {
            warnings: vec![OverlayWarning::UnknownParameter("sparkle".into())],
            ..ready_report()
        };
        assert!(report.needs_attention());
    }

    #[test]
    fn an_unapproved_network_request_needs_attention_even_when_the_runtime_is_ready() {
        let report = OverlayReport {
            network: vec![NetworkRequest {
                overlay: OverlayId(1),
                origin: "https://example.com".into(),
                approved: false,
            }],
            ..ready_report()
        };
        assert!(report.needs_attention());
        let preflight = Preflight {
            overlays: vec![report],
            ..Default::default()
        };
        assert_eq!(preflight.pending_approvals().len(), 1);
    }

    #[test]
    fn a_reveal_sequence_is_described_as_one_overlay_across_several_pages() {
        let report = OverlayReport {
            pages: vec![3, 4, 5],
            ..ready_report()
        };
        let grouping = report.grouping();
        assert!(grouping.contains("4–6"), "{grouping}");
        assert!(grouping.contains("3 reveal steps"), "{grouping}");
    }

    #[test]
    fn a_single_page_overlay_is_described_plainly() {
        assert_eq!(ready_report().grouping(), "page 4");
    }

    #[test]
    fn a_selection_with_no_runtime_becomes_a_static_fallback_with_its_reason() {
        let selection = Selection {
            selected: None,
            fallbacks: Vec::new(),
            attempts: vec![Attempt {
                runtime: RuntimeId::ExternalChromium,
                outcome: AttemptOutcome::Skipped(
                    crate::capability::UnmetRequirement::Unavailable {
                        detail: "no browser installed".into(),
                    },
                ),
            }],
        };
        let status = status_from_selection(&selection, None);
        match status {
            OverlayStatus::StaticFallback { reason } => {
                assert!(reason.contains("no browser installed"), "{reason}")
            }
            other => panic!("expected a static fallback, got {other:?}"),
        }
    }

    #[test]
    fn an_actionable_limitation_demotes_an_overlay_to_degraded() {
        let probe = RuntimeProbe {
            limitations: vec![Limitation::NoAudio],
            ..RuntimeProbe::unavailable(RuntimeId::WebKitGtk, Availability::Available)
        };
        let selection = Selection {
            selected: Some(RuntimeId::WebKitGtk),
            fallbacks: Vec::new(),
            attempts: Vec::new(),
        };
        assert!(matches!(
            status_from_selection(&selection, Some(&probe)),
            OverlayStatus::Degraded { .. }
        ));
    }

    #[test]
    fn a_compressed_frame_transport_alone_does_not_count_as_degraded() {
        let probe = RuntimeProbe {
            version: Some("Chrome/140".into()),
            limitations: vec![Limitation::CompressedFrames {
                codec: "JPEG".into(),
            }],
            ..RuntimeProbe::unavailable(RuntimeId::ExternalChromium, Availability::Available)
        };
        let selection = Selection {
            selected: Some(RuntimeId::ExternalChromium),
            fallbacks: Vec::new(),
            attempts: Vec::new(),
        };
        assert!(
            matches!(
                status_from_selection(&selection, Some(&probe)),
                OverlayStatus::Ready { .. }
            ),
            "the presenter cannot act on the frame transport, so it is not a warning"
        );
    }

    #[test]
    fn the_report_names_every_runtime_and_every_overlay() {
        let preflight = Preflight {
            overlays: vec![ready_report()],
            candidate_order: [("web".to_string(), vec!["external-chromium".to_string()])]
                .into_iter()
                .collect(),
            runtimes: vec![RuntimeSummary {
                runtime: RuntimeId::WebKitGtk,
                available: false,
                detail: "not built into this package".into(),
                version: None,
                executable: None,
                limitations: Vec::new(),
            }],
        };
        let report = preflight.to_report();
        assert!(report.contains("webkitgtk"));
        assert!(report.contains("not built into this package"));
        assert!(report.contains("external-chromium"));
        assert!(report.contains("overlay#1"));
    }

    #[test]
    fn a_document_with_no_overlays_says_so_rather_than_printing_nothing() {
        let report = Preflight::default().to_report();
        assert!(report.contains("declares none"));
        assert!(Preflight::default().all_ready());
    }

    #[test]
    fn a_blocked_overlay_is_never_reported_as_ready() {
        let status = OverlayStatus::Blocked {
            reason: "the bundle asked for network access that was not approved".into(),
        };
        assert!(!status.is_ready());
        assert!(status.headline().starts_with("blocked"));
    }
}
