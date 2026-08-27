//! Diagnostics: structured logging and the display bug-report bundle.
//!
//! Acceptance criterion 10: a bundle identifies the platform, backend,
//! monitor topology, selected roles, relevant window events and
//! reconciliation decisions — and contains no document contents.

use std::collections::VecDeque;
use std::path::PathBuf;

use pulpit_display::{DisplaySnapshot, Outcome, Role, RoleTarget, Warning};

/// Sets up `tracing`, optionally with a rotating file in the state directory.
pub struct Logging {
    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    pub log_directory: Option<PathBuf>,
    _guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl Logging {
    pub fn init(level: &str, persistent: bool) -> Logging {
        use tracing_subscriber::prelude::*;

        let filter = tracing_subscriber::EnvFilter::try_from_env("PULPIT_LOG")
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
        let stderr = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

        // The rotating file is best-effort. Whether it was not asked for or
        // its directory could not be made, stderr alone still has to work, so
        // both cases land on the same one-layer registry below.
        let file = persistent
            .then(log_directory)
            .filter(|directory| std::fs::create_dir_all(directory).is_ok())
            .map(|directory| {
                let appender = tracing_appender::rolling::daily(&directory, "pulpit.log");
                let (writer, guard) = tracing_appender::non_blocking(appender);
                let layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer);
                (directory, guard, layer)
            });

        match file {
            Some((directory, guard, layer)) => {
                let _ = tracing_subscriber::registry()
                    .with(filter)
                    .with(stderr)
                    .with(layer)
                    .try_init();
                Logging {
                    log_directory: Some(directory),
                    _guard: Some(guard),
                }
            }
            None => {
                let _ = tracing_subscriber::registry()
                    .with(filter)
                    .with(stderr)
                    .try_init();
                Logging {
                    log_directory: None,
                    _guard: None,
                }
            }
        }
    }
}

/// Where the log file goes. The platform crate owns the conventions — this
/// crate must not carry a second, drifting copy of them.
pub fn log_directory() -> PathBuf {
    crate::platform::Directories::detect().logs
}

/// A ring buffer of the decisions that matter when a projector misbehaves.
#[derive(Debug, Default)]
pub struct DiagnosticsBundle {
    pub platform: String,
    pub display_backend: String,
    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub pdf_backend: String,
    pub capabilities: String,
    snapshot: Option<DisplaySnapshot>,
    roles: Vec<(Role, String)>,
    events: VecDeque<String>,
}

const MAX_EVENTS: usize = 400;

impl DiagnosticsBundle {
    pub fn new(platform: impl Into<String>) -> Self {
        Self {
            platform: platform.into(),
            ..Self::default()
        }
    }

    pub fn record_snapshot(&mut self, snapshot: DisplaySnapshot) {
        self.note(format!(
            "topology #{}: {} monitor(s)",
            snapshot.sequence,
            snapshot.monitors.len()
        ));
        self.snapshot = Some(snapshot);
    }

    pub fn record_roles(&mut self, presenter: &RoleTarget, audience: &RoleTarget) {
        let describe = |target: &RoleTarget| match target {
            RoleTarget::Auto => "auto".to_string(),
            RoleTarget::Monitor(record) => record.identity.label(),
        };
        self.roles = vec![
            (Role::Presenter, describe(presenter)),
            (Role::Audience, describe(audience)),
        ];
    }

    pub fn record_outcome(&mut self, outcome: &Outcome) {
        for action in &outcome.actions {
            self.note(format!("action: {action:?}"));
        }
        for warning in &outcome.warnings {
            self.note(format!("warning: {}", describe_warning(warning)));
        }
    }

    /// Record an event. Callers must not pass document contents; only page
    /// numbers and counts, which is all the bundle ever needs.
    pub fn note(&mut self, event: impl Into<String>) {
        self.events.push_back(event.into());
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn events(&self) -> impl Iterator<Item = &String> {
        self.events.iter()
    }

    /// Render the bundle as plain text for a bug report.
    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn to_report(&self) -> String {
        self.to_report_with_backend(&self.pdf_backend)
    }

    /// The bundle, with the PDF backend supplied by a caller that can ask the
    /// renderer rather than from this struct's own field, which nothing in
    /// the application ever sets.
    pub fn to_report_with_backend(&self, pdf_backend: &str) -> String {
        let mut out = String::new();
        out.push_str("# pulpit diagnostics\n\n");
        out.push_str(&format!("platform: {}\n", self.platform));
        out.push_str(&format!("display backend: {}\n", self.display_backend));
        out.push_str(&format!("pdf backend: {pdf_backend}\n"));
        out.push_str(&format!("capabilities: {}\n", self.capabilities));
        out.push_str(&format!("version: {}\n", env!("CARGO_PKG_VERSION")));
        // Which build this is, stated before any number below it.
        //
        // Every timing in this report is meaningless without it, and the
        // specification says so outright: debug builds never set targets.
        // PDFium is a prebuilt optimised library, so rasterising looks the
        // same in both, while everything around it — the frame copies, the
        // encode and decode of every worker response — is unoptimised. A
        // report that did not say this was read for several rounds as
        // evidence about the application's design.
        out.push_str(&format!(
            "build: {}\n\n",
            if cfg!(debug_assertions) {
                "debug — timings below are not representative"
            } else {
                "release"
            }
        ));

        out.push_str("## Roles\n");
        for (role, target) in &self.roles {
            out.push_str(&format!("- {}: {target}\n", role.as_str()));
        }

        out.push_str("\n## Monitors\n");
        match &self.snapshot {
            Some(snapshot) => {
                for (index, monitor) in snapshot.monitors.iter().enumerate() {
                    let (pw, ph) = monitor.physical_pixels();
                    out.push_str(&format!(
                        "- [{index}] {} · identity {:?} · builtin {} · primary {} · {pw}×{ph}px\n",
                        monitor.label(),
                        monitor.identity,
                        monitor.builtin,
                        monitor.primary,
                    ));
                }
                for overlap in snapshot.overlaps() {
                    out.push_str(&format!(
                        "- overlap: {} and {} (nested: {})\n",
                        overlap.a, overlap.b, overlap.nested
                    ));
                }
            }
            None => out.push_str("- (no snapshot taken yet)\n"),
        }

        out
    }

    /// The event log, rendered separately so a caller can put it *after* its
    /// own summaries.
    ///
    /// It is the longest section by a wide margin and the least often the
    /// answer, and the report is read through a box a few lines tall: printed
    /// before the summaries, as it once was, several hundred events stood
    /// between the reader and every number that had been added for them to
    /// read. An appendix goes at the back.
    pub fn events_report(&self) -> String {
        let mut out = String::from("\n## Recent events\n");
        for event in &self.events {
            out.push_str(&format!("- {event}\n"));
        }
        out
    }

    /// The whole bundle, summaries then appendix, for a caller with nothing
    /// of its own to add.
    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn to_full_report(&self) -> String {
        let mut out = self.to_report();
        out.push_str(&self.events_report());
        out
    }
}

pub fn describe_warning(warning: &Warning) -> String {
    match warning {
        Warning::NoDisplays => "no displays are reported".into(),
        Warning::NoSecondaryDisplay => {
            "no separate audience display is connected; the audience view is a window".into()
        }
        Warning::AmbiguousAutomaticRoles { candidates } => format!(
            "cannot tell which display should face the audience ({} candidates); choose one",
            candidates.len()
        ),
        Warning::AmbiguousSelection { role, candidates } => format!(
            "the saved {} display matches {} monitors; choose one",
            role.as_str(),
            candidates.len()
        ),
        Warning::SelectedDisplayMissing { role } => {
            format!("the selected {} display is not connected", role.as_str())
        }
        Warning::SharedDisplay => "both windows are on the same display".into(),
        Warning::OverlappingOutputs { nested, .. } => {
            if *nested {
                "two outputs overlap (one contains the other); both remain selectable".into()
            } else {
                "two outputs overlap; both remain selectable".into()
            }
        }
        Warning::WindowRecovered { role } => {
            format!(
                "the {} window was recovered onto an available display",
                role.as_str()
            )
        }
        Warning::AwaitingFirstFrame => "waiting for the first rendered frame".into(),
        Warning::CannotLeaveFullscreen { role } => format!(
            "this compositor cannot safely unfullscreen the {} window; leaving it as it is",
            role.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_display::{
        Capabilities, DisplayRoles, IdentityRecord, Monitor, MonitorIdentity, Rect, Windows,
    };

    fn snapshot() -> DisplaySnapshot {
        let monitor = Monitor {
            identity: MonitorIdentity::Connector {
                connector: "HDMI-1".into(),
                make: "ACME".into(),
                model: "Projector".into(),
            },
            fallback_identity: None,
            connector: Some("HDMI-1".into()),
            make: Some("ACME".into()),
            model: Some("Projector".into()),
            geometry: Rect::new(0, 0, 1920, 1080),
            scale_factor: 1.0,
            physical_size_mm: Some((600, 340)),
            builtin: false,
            primary: true,
            handle: 42,
        };
        DisplaySnapshot::new(vec![monitor], 3)
    }

    #[test]
    fn a_report_names_the_platform_topology_and_decisions() {
        let mut bundle = DiagnosticsBundle::new("linux/x11");
        bundle.display_backend = "x11-randr".into();
        bundle.pdf_backend = "pdfium".into();
        bundle.record_snapshot(snapshot());
        bundle.record_roles(
            &RoleTarget::Auto,
            &RoleTarget::Monitor(Box::new(IdentityRecord::new(MonitorIdentity::Stable {
                id: "ACM-1234".into(),
            }))),
        );
        let outcome = pulpit_display::reconcile(
            &snapshot(),
            &DisplayRoles::default(),
            Capabilities::X11,
            &Windows::default(),
        );
        bundle.record_outcome(&outcome);

        let report = bundle.to_full_report();
        assert!(report.contains("linux/x11"));
        assert!(report.contains("x11-randr"));
        assert!(report.contains("HDMI-1"));
        assert!(report.contains("audience: ACM-1234"));
        assert!(report.contains("topology #3"));
        assert!(report.contains("warning:"));
    }

    /// The split exists so a caller can put the log after its own summaries.
    /// If `to_report` ever grew the log back, every such caller would print
    /// it twice — and a caller that only wanted the summaries would be back
    /// to burying them.
    #[test]
    fn the_event_log_is_an_appendix_not_part_of_the_summary() {
        let mut bundle = DiagnosticsBundle::new("test");
        bundle.note("something happened");

        assert!(!bundle.to_report().contains("Recent events"));
        assert!(bundle.events_report().contains("something happened"));
        let full = bundle.to_full_report();
        assert!(full.contains("pulpit diagnostics"));
        assert!(full.contains("something happened"));
        assert_eq!(
            full.matches("## Recent events").count(),
            1,
            "the log appears once"
        );
    }

    #[test]
    fn the_event_log_is_bounded() {
        let mut bundle = DiagnosticsBundle::new("test");
        for i in 0..2000 {
            bundle.note(format!("event {i}"));
        }
        assert_eq!(bundle.events().count(), MAX_EVENTS);
        assert!(bundle.to_full_report().contains("event 1999"));
    }

    #[test]
    fn every_warning_has_a_human_explanation() {
        let warnings = [
            Warning::NoDisplays,
            Warning::NoSecondaryDisplay,
            Warning::AmbiguousAutomaticRoles {
                candidates: vec![0, 1],
            },
            Warning::AmbiguousSelection {
                role: Role::Audience,
                candidates: vec![0, 1],
            },
            Warning::SelectedDisplayMissing {
                role: Role::Presenter,
            },
            Warning::SharedDisplay,
            Warning::OverlappingOutputs {
                a: 0,
                b: 1,
                nested: true,
            },
            Warning::WindowRecovered {
                role: Role::Audience,
            },
            Warning::AwaitingFirstFrame,
            Warning::CannotLeaveFullscreen {
                role: Role::Audience,
            },
        ];
        for warning in warnings {
            let text = describe_warning(&warning);
            assert!(text.len() > 10, "{warning:?} has no useful explanation");
            assert!(
                !text.contains("{"),
                "{warning:?} explanation looks like a debug dump"
            );
        }
    }
}
