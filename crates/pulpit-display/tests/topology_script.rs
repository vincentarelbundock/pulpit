//! Scripted topology transitions.
//!
//! Every file in `tests/topology/` is replayed through the real
//! reconciliation state machine, under every capability profile, with the
//! product invariants asserted after each transition. No display server, no
//! GPU, no privileges — so this runs on every commit.
//!
//! The same file format is what `pulpit-topology` dumps from a live
//! session, so a topology captured from an awkward dock or a borrowed
//! projector becomes a permanent regression test by committing it here.

use std::path::{Path, PathBuf};

use pulpit_display::reconcile::Reconciliation;
use pulpit_display::scenario::Scenario;
use pulpit_display::{
    apply_outcome, reconcile, Action, Capabilities, DisplayRoles, IdentityRecord, Outcome,
    Reconciler, Role, RoleTarget, Warning, WindowMode, WindowState, Windows,
};

fn scenario_files() -> Vec<PathBuf> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/topology");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", directory.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "txt"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no topology scenarios found");
    files
}

fn ready_windows() -> Windows {
    Windows {
        presenter: WindowState {
            has_frame: true,
            ..WindowState::default()
        },
        audience: WindowState {
            has_frame: true,
            ..WindowState::default()
        },
    }
}

fn profiles() -> Vec<(&'static str, Capabilities)> {
    vec![
        ("x11", Capabilities::X11),
        ("wayland", Capabilities::WAYLAND),
        ("tiling", Capabilities::TILING),
    ]
}

/// Assertions that must hold after *every* transition, on every platform.
fn check_invariants(label: &str, outcome: &Outcome, windows: &Windows, monitors: usize) {
    // 3. No window may become permanently inaccessible.
    assert!(
        windows.presenter.visible || monitors == 0,
        "{label}: the presenter window is not visible"
    );

    // Both roles on one display is allowed, but never silently, and never
    // with the audience covering the operator's own controls.
    if let (Some(presenter), Some(audience)) =
        (outcome.resolved.presenter, outcome.resolved.audience)
    {
        if presenter == audience {
            assert!(
                outcome.has_warning(&Warning::SharedDisplay),
                "{label}: both windows share a display without saying so"
            );
            assert!(
                windows.audience.mode != WindowMode::Fullscreen
                    || outcome.has_warning(&Warning::CannotLeaveFullscreen {
                        role: Role::Audience
                    }),
                "{label}: the audience window covers the presenter"
            );
        }
    }

    // Placement always names a monitor that exists in the snapshot it came
    // from: no action may reference a stale index.
    for action in &outcome.actions {
        if let Action::Place { monitor_index, .. } = action {
            assert!(
                *monitor_index < monitors,
                "{label}: placement targets monitor {monitor_index} of {monitors}"
            );
        }
    }

    // Every warning is explainable to a human.
    for warning in &outcome.warnings {
        let text = pulpit_settings_describe(warning);
        assert!(!text.is_empty(), "{label}: {warning:?} has no explanation");
    }
}

/// The display crate does not depend on the settings crate, so the
/// human-readable text lives there; this mirrors the mapping so the harness
/// can assert that no warning is unexplainable.
fn pulpit_settings_describe(warning: &Warning) -> &'static str {
    match warning {
        Warning::NoDisplays => "no displays",
        Warning::NoSecondaryDisplay => "no secondary display",
        Warning::AmbiguousAutomaticRoles { .. } => "ambiguous automatic roles",
        Warning::AmbiguousSelection { .. } => "ambiguous selection",
        Warning::SelectedDisplayMissing { .. } => "selected display missing",
        Warning::SharedDisplay => "shared display",
        Warning::OverlappingOutputs { .. } => "overlapping outputs",
        Warning::WindowRecovered { .. } => "window recovered",
        Warning::AwaitingFirstFrame => "awaiting first frame",
        Warning::CannotLeaveFullscreen { .. } => "cannot leave fullscreen",
    }
}

/// Walk one scenario under one capability profile.
fn replay(scenario: &Scenario, profile: &str, capabilities: Capabilities, roles: &DisplayRoles) {
    let mut windows = ready_windows();
    let mut reconciler = Reconciler::new();

    for (index, step) in scenario.steps.iter().enumerate() {
        let snapshot = step.snapshot(index as u64 + 1);
        let label = format!("{profile}/{}", step.name);

        let outcome = match reconciler.reconcile(&snapshot, roles, capabilities, &windows) {
            Reconciliation::Applied(outcome) => outcome,
            Reconciliation::Unchanged => continue,
            Reconciliation::Stale { .. } => panic!("{label}: a forward step was treated as stale"),
        };
        let before = windows.clone();
        apply_outcome(&mut windows, &outcome);
        reconciler.note_windows(&windows);

        check_invariants(&label, &outcome, &windows, snapshot.len());

        // An empty topology is a transient blip and must move nothing.
        if snapshot.is_empty() {
            assert_eq!(windows, before, "{label}: a blackout moved a window");
        }

        // Idempotence: applying the same topology again does nothing.
        let repeat = reconcile(&snapshot, roles, capabilities, &windows);
        assert!(
            repeat.is_noop(),
            "{label}: not converged, would do {:?}",
            repeat.actions
        );
    }
}

#[test]
fn every_scenario_holds_the_invariants_under_every_capability_profile() {
    for path in scenario_files() {
        let text = std::fs::read_to_string(&path).unwrap();
        let scenario = Scenario::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for (profile, capabilities) in profiles() {
            let label = format!(
                "{} [{profile}]",
                path.file_name().unwrap().to_string_lossy()
            );
            std::panic::catch_unwind(|| {
                replay(&scenario, profile, capabilities, &DisplayRoles::default());
            })
            .unwrap_or_else(|_| panic!("scenario failed: {label}"));
        }
    }
}

#[test]
fn every_scenario_holds_with_explicit_display_choices() {
    // The same walk, but with the user having pinned the audience to the
    // projector — the configuration that matters most in practice.
    for path in scenario_files() {
        let text = std::fs::read_to_string(&path).unwrap();
        let scenario = Scenario::parse(&text).unwrap();
        let Some(projector) = scenario
            .steps
            .iter()
            .flat_map(|step| step.monitors.iter())
            .find(|monitor| !monitor.builtin)
        else {
            continue;
        };
        let roles = DisplayRoles {
            audience: RoleTarget::Monitor(Box::new(IdentityRecord::new(
                projector.identity.clone(),
            ))),
            ..DisplayRoles::default()
        };
        for (profile, capabilities) in profiles() {
            replay(&scenario, profile, capabilities, &roles);
        }
    }
}

#[test]
fn a_swap_mid_scenario_still_converges() {
    for path in scenario_files() {
        let text = std::fs::read_to_string(&path).unwrap();
        let scenario = Scenario::parse(&text).unwrap();
        let mut roles = DisplayRoles::default();
        let mut windows = ready_windows();
        let mut reconciler = Reconciler::new();

        for (index, step) in scenario.steps.iter().enumerate() {
            // Swap on every other step: the operator hitting "s" at the worst
            // possible moment is a supported thing to do.
            if index % 2 == 1 {
                roles = roles.swapped();
            }
            let snapshot = step.snapshot(index as u64 + 1);
            if let Reconciliation::Applied(outcome) =
                reconciler.reconcile(&snapshot, &roles, Capabilities::X11, &windows)
            {
                apply_outcome(&mut windows, &outcome);
                reconciler.note_windows(&windows);
                check_invariants(
                    &format!("swap/{}", step.name),
                    &outcome,
                    &windows,
                    snapshot.len(),
                );
            }
        }
    }
}

#[test]
fn out_of_order_notifications_are_dropped() {
    // Replay each scenario forwards, then feed an older snapshot again: a
    // delayed notification from a previous topology must not be applied.
    for path in scenario_files() {
        let text = std::fs::read_to_string(&path).unwrap();
        let scenario = Scenario::parse(&text).unwrap();
        if scenario.steps.len() < 2 {
            continue;
        }
        let roles = DisplayRoles::default();
        let mut windows = ready_windows();
        let mut reconciler = Reconciler::new();

        for (index, step) in scenario.steps.iter().enumerate() {
            if let Reconciliation::Applied(outcome) = reconciler.reconcile(
                &step.snapshot(index as u64 + 1),
                &roles,
                Capabilities::X11,
                &windows,
            ) {
                apply_outcome(&mut windows, &outcome);
                reconciler.note_windows(&windows);
            }
        }

        let settled = windows.clone();
        let stale = scenario.steps[0].snapshot(1);
        assert!(
            matches!(
                reconciler.reconcile(&stale, &roles, Capabilities::X11, &windows),
                Reconciliation::Stale { .. }
            ),
            "{}: a stale notification was not recognised",
            path.display()
        );
        assert_eq!(windows, settled, "a stale notification moved a window");
    }
}

/// §77.2 regression: A and B are an exact mirror, C is a third free display,
/// and none of the three is built-in. Pinning the audience explicitly to B
/// (one half of the mirror) used to collapse the presenter onto the same
/// logical display as the audience, because the explicit resolution's raw
/// snapshot index was compared against the automatic policy's *logical*
/// (mirror-collapsed) candidate list without ever being mapped through it —
/// so B's raw index never excluded the A/B group from being handed to the
/// presenter, and only the redundant mapping applied afterwards folded both
/// roles onto the same logical target. C was free the whole time.
#[test]
fn an_explicit_mirrored_audience_does_not_strand_the_presenter_on_a_free_third_display() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/topology/09-explicit-mirrored-audience-frees-the-presenter.txt");
    let text = std::fs::read_to_string(&path).unwrap();
    let scenario = Scenario::parse(&text).unwrap();
    let step = &scenario.steps[0];
    let snapshot = step.snapshot(1);

    let b = step
        .monitors
        .iter()
        .find(|m| m.model.as_deref() == Some("B"))
        .expect("scenario has a monitor B");
    let c_index = step
        .monitors
        .iter()
        .position(|m| m.model.as_deref() == Some("C"))
        .expect("scenario has a monitor C");

    let roles = DisplayRoles {
        audience: RoleTarget::Monitor(Box::new(IdentityRecord::new(b.identity.clone()))),
        ..DisplayRoles::default()
    };
    let windows = ready_windows();
    let outcome = reconcile(&snapshot, &roles, Capabilities::X11, &windows);

    assert_eq!(
        outcome.resolved.presenter,
        Some(c_index),
        "the presenter must land on the free display C, not collapse onto \
         the mirrored audience: resolved {:?}",
        outcome.resolved
    );
    assert_ne!(outcome.resolved.presenter, outcome.resolved.audience);
    assert!(!outcome.has_warning(&Warning::SharedDisplay));
}
