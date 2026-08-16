//! The single idempotent reconciliation function.
//!
//! Startup, hot-plug, resume, display selection, fullscreen and swap all pass
//! through [`reconcile`]. Calling it repeatedly with the same inputs produces
//! no additional actions. It never touches native handles: it consumes an
//! immutable snapshot and emits actions the caller performs, resolving a live
//! handle only immediately before the native call.

use crate::identity::MonitorIdentity;
use crate::roles::{DisplayRoles, Role, RoleTarget};
use crate::snapshot::{DisplaySnapshot, Rect, Resolution};

/// What the platform is actually able to do. Reported by the display
/// extension, not assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Arbitrary top-level window positioning (X11 yes, Wayland no).
    pub arbitrary_position: bool,
    /// Whether leaving fullscreen is safe: on Wayland unfullscreening can
    /// strand a window outside the remaining monitor bounds.
    pub unfullscreen_safe: bool,
    /// Whether placement requests are honoured before the window is mapped.
    pub place_before_map: bool,
}

impl Capabilities {
    /// X11 with a conventional window manager.
    pub const X11: Capabilities = Capabilities {
        arbitrary_position: true,
        unfullscreen_safe: true,
        place_before_map: false,
    };

    /// Wayland: the compositor owns placement, so the audience window goes
    /// fullscreen wherever it already is.
    pub const WAYLAND: Capabilities = Capabilities {
        arbitrary_position: false,
        unfullscreen_safe: false,
        place_before_map: false,
    };

    /// A tiling compositor that ignores client placement entirely.
    pub const TILING: Capabilities = Capabilities {
        arbitrary_position: false,
        unfullscreen_safe: true,
        place_before_map: false,
    };

    pub fn can_place(&self) -> bool {
        self.arbitrary_position
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Windowed,
    Fullscreen,
    Hidden,
}

/// What the caller believes a window currently is. Rebuilt from real window
/// events, never from a cached monitor snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    /// Identity of the monitor the window is currently on, if known.
    pub monitor: Option<MonitorIdentity>,
    pub mode: WindowMode,
    pub visible: bool,
    /// Whether the window has a valid rendered frame. The audience window is
    /// never shown without one.
    pub has_frame: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            monitor: None,
            mode: WindowMode::Hidden,
            visible: false,
            has_frame: false,
        }
    }
}

impl WindowState {
    pub fn windowed_on(monitor: MonitorIdentity) -> Self {
        Self {
            monitor: Some(monitor),
            mode: WindowMode::Windowed,
            visible: true,
            has_frame: true,
        }
    }

    pub fn fullscreen_on(monitor: MonitorIdentity) -> Self {
        Self {
            monitor: Some(monitor),
            mode: WindowMode::Fullscreen,
            visible: true,
            has_frame: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Windows {
    pub presenter: WindowState,
    pub audience: WindowState,
}

impl Windows {
    pub fn get(&self, role: Role) -> &WindowState {
        match role {
            Role::Presenter => &self.presenter,
            Role::Audience => &self.audience,
        }
    }

    pub fn get_mut(&mut self, role: Role) -> &mut WindowState {
        match role {
            Role::Presenter => &mut self.presenter,
            Role::Audience => &mut self.audience,
        }
    }
}

/// An action for the caller to perform through the display extension.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Place a window on a monitor. `monitor_index` indexes the snapshot that
    /// produced this outcome; the identity is carried so the caller can
    /// re-resolve it if the topology changed in the meantime.
    Place {
        role: Role,
        monitor_index: usize,
        identity: MonitorIdentity,
        geometry: Rect,
        scale_factor: f64,
        mode: WindowMode,
    },
    /// Show a window that already has a valid frame and a resolved placement.
    Show { role: Role },
    /// Leave fullscreen (only emitted where unfullscreening is safe).
    Unfullscreen { role: Role },
}

impl Action {
    pub fn role(&self) -> Role {
        match self {
            Action::Place { role, .. } | Action::Show { role } | Action::Unfullscreen { role } => {
                *role
            }
        }
    }
}

/// Something the user needs to know, or confirm.
#[derive(Debug, Clone, PartialEq)]
pub enum Warning {
    /// No monitors at all were reported.
    NoDisplays,
    /// Only one logical display: the audience view stays a window.
    NoSecondaryDisplay,
    /// The automatic choice is genuinely ambiguous (typically a desktop with
    /// no recognisable built-in panel). Ask, do not guess.
    AmbiguousAutomaticRoles { candidates: Vec<usize> },
    /// A persisted selection matched several monitors.
    AmbiguousSelection { role: Role, candidates: Vec<usize> },
    /// A selected display is not connected; the window was recovered.
    SelectedDisplayMissing { role: Role },
    /// Both roles resolved to the same logical display.
    SharedDisplay,
    /// Outputs overlap without being an exact mirror; both remain targetable.
    OverlappingOutputs { a: usize, b: usize, nested: bool },
    /// A window was moved off a disappeared display to stay reachable.
    WindowRecovered { role: Role },
    /// The audience window is ready but has no frame yet, so it stays hidden.
    AwaitingFirstFrame,
    /// The window should leave fullscreen but the compositor makes that
    /// unsafe (it could be stranded outside the remaining monitor bounds).
    /// The user is told what to do instead.
    CannotLeaveFullscreen { role: Role },
}

impl Warning {
    /// A stable name for this kind of warning, independent of its payload.
    ///
    /// Used to recognise the same warning across reconciliations, so a notice
    /// about a standing condition is updated rather than duplicated.
    pub fn key(&self) -> &'static str {
        match self {
            Warning::NoDisplays => "no-displays",
            Warning::NoSecondaryDisplay => "no-secondary-display",
            Warning::AmbiguousAutomaticRoles { .. } => "ambiguous-automatic-roles",
            Warning::AmbiguousSelection { .. } => "ambiguous-selection",
            Warning::SelectedDisplayMissing { .. } => "selected-display-missing",
            Warning::SharedDisplay => "shared-display",
            Warning::OverlappingOutputs { .. } => "overlapping-outputs",
            Warning::WindowRecovered { .. } => "window-recovered",
            Warning::AwaitingFirstFrame => "awaiting-first-frame",
            Warning::CannotLeaveFullscreen { .. } => "cannot-leave-fullscreen",
        }
    }

    /// Is this a standing condition rather than a momentary event?
    ///
    /// A condition stays true until the topology or the role choice changes —
    /// "there is no second display" is not news that should scroll away after
    /// a few seconds, because the presenter who plugs in a projector wants to
    /// see the notice disappear, not wonder whether it ever appeared. An
    /// event ("the window was recovered", "waiting for the first frame") has
    /// already happened and is safe to let fade.
    pub fn is_condition(&self) -> bool {
        !matches!(
            self,
            Warning::WindowRecovered { .. } | Warning::AwaitingFirstFrame
        )
    }
}

/// Which snapshot monitor each role resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResolvedRoles {
    pub presenter: Option<usize>,
    pub audience: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    pub actions: Vec<Action>,
    pub warnings: Vec<Warning>,
    pub resolved: ResolvedRoles,
    /// Sequence number of the snapshot this outcome was computed from.
    pub sequence: u64,
}

impl Outcome {
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn has_warning(&self, warning: &Warning) -> bool {
        self.warnings.contains(warning)
    }

    pub fn action_for(&self, role: Role) -> Option<&Action> {
        self.actions.iter().find(|a| a.role() == role)
    }
}

/// Resolve one role to a monitor index, reporting how it went.
fn resolve_role(
    snapshot: &DisplaySnapshot,
    roles: &DisplayRoles,
    role: Role,
    warnings: &mut Vec<Warning>,
) -> Option<usize> {
    match roles.target(role) {
        RoleTarget::Auto => None,
        RoleTarget::Monitor(record) => match snapshot.resolve(record) {
            Resolution::Unique(index) => Some(index),
            Resolution::Ambiguous(candidates) => {
                warnings.push(Warning::AmbiguousSelection { role, candidates });
                None
            }
            Resolution::Missing => {
                warnings.push(Warning::SelectedDisplayMissing { role });
                None
            }
        },
    }
}

/// The automatic policy: built-in panel presents, external displays face the
/// audience. When no built-in panel is recognisable and more than one display
/// is present, the choice is surfaced rather than decided by enumeration
/// order.
fn automatic_roles(
    snapshot: &DisplaySnapshot,
    windows: &Windows,
    presenter: Option<usize>,
    audience: Option<usize>,
    warnings: &mut Vec<Warning>,
) -> (Option<usize>, Option<usize>) {
    let targets = snapshot.logical_targets();
    if targets.is_empty() {
        return (None, None);
    }
    let explicit = (presenter.is_some(), audience.is_some());

    let mut presenter = presenter;
    let mut audience = audience;

    if presenter.is_none() {
        let builtin = snapshot.builtin().filter(|index| Some(*index) != audience);
        // Keep the presenter where it already is when that is still a real
        // display: moving it for no reason is user-hostile.
        let current = windows
            .presenter
            .monitor
            .as_ref()
            .and_then(|identity| index_of_identity(snapshot, identity))
            .map(|index| logical_target(snapshot, index))
            .filter(|index| Some(*index) != audience);
        let distinct = targets
            .iter()
            .copied()
            .find(|index| Some(*index) != audience);

        if snapshot.builtin().is_none() && targets.len() > 1 && !explicit.0 && !explicit.1 {
            warnings.push(Warning::AmbiguousAutomaticRoles {
                candidates: targets.clone(),
            });
        }
        // Last resort: share the audience display rather than leave the
        // presenter window with nowhere to be.
        presenter = builtin
            .or(current)
            .or(distinct)
            .or_else(|| targets.first().copied());
    }

    if audience.is_none() {
        audience = snapshot
            .external_targets()
            .into_iter()
            .find(|index| Some(*index) != presenter)
            .or_else(|| {
                targets
                    .iter()
                    .copied()
                    .find(|index| Some(*index) != presenter)
            })
            // One logical display: the audience view is a window on it.
            .or(presenter);
    }

    (presenter, audience)
}

fn index_of_identity(snapshot: &DisplaySnapshot, identity: &MonitorIdentity) -> Option<usize> {
    snapshot
        .monitors
        .iter()
        .position(|m| &m.identity == identity || m.fallback_identity.as_ref() == Some(identity))
}

/// Collapse a monitor index to the representative of its mirror group, so
/// mirrored outputs behave as one placement target.
fn logical_target(snapshot: &DisplaySnapshot, index: usize) -> usize {
    snapshot
        .mirror_groups()
        .into_iter()
        .find(|group| group.members.contains(&index))
        .map(|group| group.members[0])
        .unwrap_or(index)
}

/// The one reconciliation entry point.
pub fn reconcile(
    snapshot: &DisplaySnapshot,
    roles: &DisplayRoles,
    capabilities: Capabilities,
    windows: &Windows,
) -> Outcome {
    let mut warnings = Vec::new();
    let mut actions = Vec::new();

    if snapshot.is_empty() {
        // Nothing can be placed. Keep both windows exactly as they are: a
        // topology blip must never hide the presenter.
        return Outcome {
            actions,
            warnings: vec![Warning::NoDisplays],
            resolved: ResolvedRoles::default(),
            sequence: snapshot.sequence,
        };
    }

    for overlap in snapshot.overlaps() {
        warnings.push(Warning::OverlappingOutputs {
            a: overlap.a,
            b: overlap.b,
            nested: overlap.nested,
        });
    }

    let explicit_presenter = resolve_role(snapshot, roles, Role::Presenter, &mut warnings);
    let explicit_audience = resolve_role(snapshot, roles, Role::Audience, &mut warnings);
    let (presenter, audience) = automatic_roles(
        snapshot,
        windows,
        explicit_presenter,
        explicit_audience,
        &mut warnings,
    );

    let presenter = presenter.map(|i| logical_target(snapshot, i));
    let audience = audience.map(|i| logical_target(snapshot, i));

    let shared = match (presenter, audience) {
        (Some(p), Some(a)) => p == a,
        _ => false,
    };
    if shared {
        warnings.push(Warning::SharedDisplay);
    }
    if snapshot.logical_targets().len() < 2 {
        warnings.push(Warning::NoSecondaryDisplay);
    }

    // The presenter window is always visible and windowed unless the user
    // asked otherwise; it is the control surface and must stay reachable.
    if let Some(index) = presenter {
        plan_window(
            snapshot,
            windows,
            capabilities,
            Role::Presenter,
            index,
            WindowMode::Windowed,
            &mut actions,
            &mut warnings,
        );
    }

    if let Some(index) = audience {
        let wants_fullscreen = roles.audience_fullscreen && (!shared || roles.allow_shared_display);
        // Fullscreen needs no placement rights: the toolkit can always
        // fullscreen a window on the output it already occupies (Wayland
        // included). Only choosing that output is gated on capabilities,
        // which plan_window reports.
        let mode = if wants_fullscreen {
            WindowMode::Fullscreen
        } else {
            WindowMode::Windowed
        };
        plan_window(
            snapshot,
            windows,
            capabilities,
            Role::Audience,
            index,
            mode,
            &mut actions,
            &mut warnings,
        );
    }

    Outcome {
        actions,
        warnings,
        resolved: ResolvedRoles {
            presenter,
            audience,
        },
        sequence: snapshot.sequence,
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_window(
    snapshot: &DisplaySnapshot,
    windows: &Windows,
    capabilities: Capabilities,
    role: Role,
    index: usize,
    mode: WindowMode,
    actions: &mut Vec<Action>,
    warnings: &mut Vec<Warning>,
) {
    let monitor = &snapshot.monitors[index];
    let window = windows.get(role);

    let on_a_dead_display = window
        .monitor
        .as_ref()
        .is_some_and(|identity| index_of_identity(snapshot, identity).is_none());
    if on_a_dead_display {
        warnings.push(Warning::WindowRecovered { role });
    }

    let already_there = window.monitor.as_ref().is_some_and(|identity| {
        index_of_identity(snapshot, identity).map(|i| logical_target(snapshot, i)) == Some(index)
    });
    let mode_matches = window.mode == mode;

    // Leaving fullscreen is only requested where it is safe to do so.
    // Staying on the same output is always safe: the stranding hazard is a
    // window unfullscreened onto a monitor that no longer exists.
    if window.mode == WindowMode::Fullscreen
        && mode == WindowMode::Windowed
        && !capabilities.unfullscreen_safe
        && !already_there
    {
        // Keep it fullscreen where it is rather than stranding the window,
        // and say so: the user may need to act.
        warnings.push(Warning::CannotLeaveFullscreen { role });
        return;
    }

    if !(already_there && mode_matches) && capabilities.can_place() {
        actions.push(Action::Place {
            role,
            monitor_index: index,
            identity: monitor.identity.clone(),
            geometry: monitor.geometry,
            scale_factor: monitor.scale_factor,
            mode,
        });
    } else if !capabilities.can_place() {
        // Placement is off the table, but the window's own mode is still
        // ours: the toolkit fullscreens on whatever output the window is on.
        if !mode_matches {
            actions.push(Action::Place {
                role,
                monitor_index: index,
                identity: monitor.identity.clone(),
                geometry: monitor.geometry,
                scale_factor: monitor.scale_factor,
                mode,
            });
        }
    }

    if !window.visible {
        // The audience window is created hidden, assigned, and only shown
        // once it has a valid frame; it must never flash on a wrong display.
        if role == Role::Audience && !window.has_frame {
            warnings.push(Warning::AwaitingFirstFrame);
        } else {
            actions.push(Action::Show { role });
        }
    }
}

/// Applies outcomes to a `Windows` value. Real code drives real windows; this
/// mirrors what a successful application of every action produces and is what
/// the idempotence tests assert against.
pub fn apply_outcome(windows: &mut Windows, outcome: &Outcome) {
    for action in &outcome.actions {
        match action {
            Action::Place {
                role,
                identity,
                mode,
                ..
            } => {
                let window = windows.get_mut(*role);
                window.monitor = Some(identity.clone());
                window.mode = *mode;
            }
            Action::Show { role } => {
                let window = windows.get_mut(*role);
                window.visible = true;
                if window.mode == WindowMode::Hidden {
                    window.mode = WindowMode::Windowed;
                }
            }
            Action::Unfullscreen { role } => {
                windows.get_mut(*role).mode = WindowMode::Windowed;
            }
        }
    }
}

/// Debounce and staleness guard around [`reconcile`].
///
/// Topology notifications arrive in bursts and out of order. The reconciler
/// drops snapshots older than the newest one already seen, and skips work
/// entirely when the topology, roles, capabilities and window state are all
/// unchanged.
#[derive(Debug, Default)]
pub struct Reconciler {
    last_snapshot: Option<DisplaySnapshot>,
    last_roles: Option<DisplayRoles>,
    last_windows: Option<Windows>,
    highest_sequence: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reconciliation {
    /// The snapshot was older than one already applied.
    Stale {
        sequence: u64,
        newest: u64,
    },
    /// Nothing changed; no work was done.
    Unchanged,
    Applied(Outcome),
}

impl Reconciler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn last_snapshot(&self) -> Option<&DisplaySnapshot> {
        self.last_snapshot.as_ref()
    }

    pub fn reconcile(
        &mut self,
        snapshot: &DisplaySnapshot,
        roles: &DisplayRoles,
        capabilities: Capabilities,
        windows: &Windows,
    ) -> Reconciliation {
        if snapshot.sequence < self.highest_sequence {
            return Reconciliation::Stale {
                sequence: snapshot.sequence,
                newest: self.highest_sequence,
            };
        }
        self.highest_sequence = snapshot.sequence;

        let unchanged = self
            .last_snapshot
            .as_ref()
            .is_some_and(|previous| previous.same_topology(snapshot))
            && self.last_roles.as_ref() == Some(roles)
            && self.last_windows.as_ref() == Some(windows);
        if unchanged {
            return Reconciliation::Unchanged;
        }

        let outcome = reconcile(snapshot, roles, capabilities, windows);
        self.last_snapshot = Some(snapshot.clone());
        self.last_roles = Some(roles.clone());
        self.last_windows = Some(windows.clone());
        Reconciliation::Applied(outcome)
    }

    /// Called after actions were applied so the next identical notification
    /// is recognised as a no-op.
    pub fn note_windows(&mut self, windows: &Windows) {
        self.last_windows = Some(windows.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::IdentityRecord;
    use crate::snapshot::{is_builtin_connector, Monitor};

    fn monitor(connector: &str, model: &str, geometry: Rect) -> Monitor {
        Monitor {
            identity: MonitorIdentity::Connector {
                connector: connector.into(),
                make: "ACME".into(),
                model: model.into(),
            },
            fallback_identity: None,
            connector: Some(connector.into()),
            make: Some("ACME".into()),
            model: Some(model.into()),
            geometry,
            scale_factor: 1.0,
            physical_size_mm: Some((600, 340)),
            builtin: is_builtin_connector(connector),
            primary: false,
            handle: 0,
        }
    }

    fn laptop() -> Monitor {
        monitor("eDP-1", "Panel", Rect::new(0, 0, 1920, 1200))
    }

    fn projector() -> Monitor {
        monitor("HDMI-1", "Projector", Rect::new(1920, 0, 1920, 1080))
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

    fn settle(
        snapshot: &DisplaySnapshot,
        roles: &DisplayRoles,
        caps: Capabilities,
        windows: &mut Windows,
    ) -> Outcome {
        let outcome = reconcile(snapshot, roles, caps, windows);
        apply_outcome(windows, &outcome);
        outcome
    }

    fn identity_of(monitor: &Monitor) -> MonitorIdentity {
        monitor.identity.clone()
    }

    fn explicit(monitor: &Monitor) -> RoleTarget {
        RoleTarget::Monitor(Box::new(IdentityRecord::new(identity_of(monitor))))
    }

    #[test]
    fn two_displays_place_presenter_on_the_panel_and_fullscreen_the_projector() {
        let snapshot = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        let mut windows = ready_windows();
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );

        assert_eq!(
            outcome.resolved,
            ResolvedRoles {
                presenter: Some(0),
                audience: Some(1)
            }
        );
        assert!(matches!(
            outcome.action_for(Role::Audience),
            Some(Action::Place {
                mode: WindowMode::Fullscreen,
                monitor_index: 1,
                ..
            })
        ));
        assert_eq!(windows.presenter.mode, WindowMode::Windowed);
        assert!(windows.presenter.visible && windows.audience.visible);
    }

    #[test]
    fn reconciliation_is_idempotent() {
        let snapshot = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        let roles = DisplayRoles::default();
        let mut windows = ready_windows();
        settle(&snapshot, &roles, Capabilities::X11, &mut windows);

        for _ in 0..5 {
            let again = reconcile(&snapshot, &roles, Capabilities::X11, &windows);
            assert!(
                again.is_noop(),
                "repeat reconciliation produced {:?}",
                again.actions
            );
        }
    }

    #[test]
    fn repeated_identical_notifications_do_not_oscillate() {
        let snapshot = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        let roles = DisplayRoles::default();
        let caps = Capabilities::X11;
        let mut windows = ready_windows();
        let mut reconciler = Reconciler::new();

        let first = reconciler.reconcile(&snapshot, &roles, caps, &windows);
        let Reconciliation::Applied(outcome) = first else {
            panic!("expected work")
        };
        apply_outcome(&mut windows, &outcome);
        reconciler.note_windows(&windows);

        for _ in 0..10 {
            assert_eq!(
                reconciler.reconcile(&snapshot, &roles, caps, &windows),
                Reconciliation::Unchanged
            );
        }
    }

    #[test]
    fn a_stale_delayed_notification_is_ignored() {
        let old = DisplaySnapshot::new(vec![laptop(), projector()], 7);
        let new = DisplaySnapshot::new(vec![laptop()], 9);
        let roles = DisplayRoles::default();
        let mut windows = ready_windows();
        let mut reconciler = Reconciler::new();

        let Reconciliation::Applied(outcome) =
            reconciler.reconcile(&new, &roles, Capabilities::X11, &windows)
        else {
            panic!("expected work")
        };
        apply_outcome(&mut windows, &outcome);

        assert_eq!(
            reconciler.reconcile(&old, &roles, Capabilities::X11, &windows),
            Reconciliation::Stale {
                sequence: 7,
                newest: 9
            }
        );
    }

    #[test]
    fn one_display_keeps_the_audience_windowed_and_says_so() {
        let snapshot = DisplaySnapshot::new(vec![laptop()], 1);
        let mut windows = ready_windows();
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );

        assert!(outcome.has_warning(&Warning::NoSecondaryDisplay));
        assert_eq!(windows.audience.mode, WindowMode::Windowed);
        assert!(windows.presenter.visible, "presenter must never be hidden");
    }

    #[test]
    fn one_to_two_displays_restores_audience_fullscreen_without_touching_the_presenter() {
        let roles = DisplayRoles::default();
        let mut windows = ready_windows();
        let single = DisplaySnapshot::new(vec![laptop()], 1);
        settle(&single, &roles, Capabilities::X11, &mut windows);
        assert_eq!(windows.audience.mode, WindowMode::Windowed);

        let both = DisplaySnapshot::new(vec![laptop(), projector()], 2);
        let outcome = settle(&both, &roles, Capabilities::X11, &mut windows);
        assert_eq!(windows.audience.mode, WindowMode::Fullscreen);
        assert!(
            outcome.action_for(Role::Presenter).is_none(),
            "presenter is already correctly placed"
        );
    }

    #[test]
    fn two_to_one_recovers_the_audience_window_onto_a_live_display() {
        let roles = DisplayRoles::default();
        let mut windows = ready_windows();
        let both = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        settle(&both, &roles, Capabilities::X11, &mut windows);

        let single = DisplaySnapshot::new(vec![laptop()], 2);
        let outcome = settle(&single, &roles, Capabilities::X11, &mut windows);

        assert!(outcome.has_warning(&Warning::WindowRecovered {
            role: Role::Audience
        }));
        assert_eq!(windows.audience.monitor, Some(identity_of(&laptop())));
        assert_eq!(windows.audience.mode, WindowMode::Windowed);
        assert!(
            windows.presenter.visible && windows.audience.visible,
            "nothing becomes unreachable"
        );
    }

    #[test]
    fn losing_the_presenter_display_recovers_the_presenter_without_stealing_the_audience() {
        let roles = DisplayRoles {
            presenter: explicit(&laptop()),
            audience: explicit(&projector()),
            ..DisplayRoles::default()
        };
        let mut windows = ready_windows();
        let both = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        settle(&both, &roles, Capabilities::X11, &mut windows);

        let only_projector = DisplaySnapshot::new(vec![projector()], 2);
        let outcome = settle(&only_projector, &roles, Capabilities::X11, &mut windows);

        assert!(outcome.has_warning(&Warning::SelectedDisplayMissing {
            role: Role::Presenter
        }));
        assert_eq!(windows.presenter.monitor, Some(identity_of(&projector())));
        assert_eq!(windows.presenter.mode, WindowMode::Windowed);
        assert!(windows.presenter.visible);
        assert_eq!(
            windows.audience.monitor,
            Some(identity_of(&projector())),
            "audience keeps its selected display"
        );
    }

    #[test]
    fn projector_reconnect_at_a_new_index_resolution_and_scale_uses_fresh_parameters() {
        let roles = DisplayRoles {
            audience: explicit(&projector()),
            ..DisplayRoles::default()
        };
        let mut windows = ready_windows();
        settle(
            &DisplaySnapshot::new(vec![laptop(), projector()], 1),
            &roles,
            Capabilities::X11,
            &mut windows,
        );
        settle(
            &DisplaySnapshot::new(vec![laptop()], 2),
            &roles,
            Capabilities::X11,
            &mut windows,
        );

        // Same projector returns first in enumeration, 4K, at scale 2.
        let mut returned = projector();
        returned.geometry = Rect::new(-3840, -200, 3840, 2160);
        returned.scale_factor = 2.0;
        let snapshot = DisplaySnapshot::new(vec![returned.clone(), laptop()], 3);
        let outcome = settle(&snapshot, &roles, Capabilities::X11, &mut windows);

        let Some(Action::Place {
            geometry,
            scale_factor,
            monitor_index,
            mode,
            ..
        }) = outcome.action_for(Role::Audience)
        else {
            panic!(
                "expected the audience to be re-placed, got {:?}",
                outcome.actions
            )
        };
        assert_eq!(
            *monitor_index, 0,
            "index changed and is read from the fresh snapshot"
        );
        assert_eq!(*geometry, returned.geometry);
        assert_eq!(*scale_factor, 2.0);
        assert_eq!(*mode, WindowMode::Fullscreen);
    }

    #[test]
    fn standing_conditions_are_told_apart_from_events() {
        // A single laptop screen is a *condition*: it stays true until a
        // projector is plugged in, so the UI must be able to keep the notice
        // up rather than letting it scroll away.
        let snapshot = DisplaySnapshot::new(vec![laptop()], 1);
        let mut windows = ready_windows();
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );

        let single = outcome
            .warnings
            .iter()
            .find(|warning| matches!(warning, Warning::NoSecondaryDisplay))
            .expect("one display means no audience display");
        assert!(single.is_condition());
        assert_eq!(single.key(), "no-secondary-display");

        assert!(!Warning::AwaitingFirstFrame.is_condition());
        assert!(!Warning::WindowRecovered {
            role: Role::Audience
        }
        .is_condition());
    }

    #[test]
    fn a_warning_key_does_not_depend_on_its_payload() {
        assert_eq!(
            Warning::SelectedDisplayMissing {
                role: Role::Presenter
            }
            .key(),
            Warning::SelectedDisplayMissing {
                role: Role::Audience
            }
            .key(),
            "the same condition, reported about a different role"
        );
    }

    #[test]
    fn one_screen_keeps_the_audience_windowed_until_it_is_asked_for() {
        // Fullscreen on the only screen covers the presenter view, so the
        // default is to refuse it. But a presenter who asks anyway — they may
        // be about to mirror, or just checking — is allowed to have it.
        let snapshot = DisplaySnapshot::new(vec![laptop()], 1);
        let mut roles = DisplayRoles {
            audience_fullscreen: true,
            ..DisplayRoles::default()
        };

        let mut windows = ready_windows();
        let outcome = settle(&snapshot, &roles, Capabilities::X11, &mut windows);
        assert_eq!(
            windows.audience.mode,
            WindowMode::Windowed,
            "by itself, asking for fullscreen on one screen is not enough"
        );
        assert!(outcome.has_warning(&Warning::SharedDisplay));

        roles.allow_shared_display = true;
        let mut windows = ready_windows();
        settle(&snapshot, &roles, Capabilities::X11, &mut windows);
        assert_eq!(
            windows.audience.mode,
            WindowMode::Fullscreen,
            "having said yes to sharing the screen, the presenter gets it"
        );

        // And turning it off comes back.
        roles.audience_fullscreen = false;
        roles.allow_shared_display = false;
        settle(&snapshot, &roles, Capabilities::X11, &mut windows);
        assert_eq!(windows.audience.mode, WindowMode::Windowed);
    }

    #[test]
    fn mirrored_displays_collapse_to_one_target() {
        let mut clone = projector();
        clone.geometry = laptop().geometry;
        let snapshot = DisplaySnapshot::new(vec![laptop(), clone], 1);
        let mut windows = ready_windows();
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );

        assert!(outcome.has_warning(&Warning::SharedDisplay));
        assert!(outcome.has_warning(&Warning::NoSecondaryDisplay));
        assert_eq!(
            windows.audience.mode,
            WindowMode::Windowed,
            "would otherwise hide the presenter"
        );
    }

    #[test]
    fn unequal_mirror_keeps_both_targets_and_reports_the_overlap() {
        let mut panel = laptop();
        panel.geometry = Rect::new(0, 0, 3840, 2160);
        let mut projector = projector();
        projector.geometry = Rect::new(0, 0, 1920, 1080);
        let snapshot = DisplaySnapshot::new(vec![panel, projector], 1);
        let mut windows = ready_windows();
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );

        assert!(outcome
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::OverlappingOutputs { nested: true, .. })));
        assert_eq!(
            outcome.resolved,
            ResolvedRoles {
                presenter: Some(0),
                audience: Some(1)
            }
        );
        assert_eq!(windows.audience.mode, WindowMode::Fullscreen);
    }

    #[test]
    fn a_desktop_with_no_builtin_panel_asks_instead_of_guessing() {
        let a = monitor("DP-1", "Left", Rect::new(0, 0, 2560, 1440));
        let b = monitor("DP-2", "Right", Rect::new(2560, 0, 2560, 1440));
        let snapshot = DisplaySnapshot::new(vec![a, b], 1);
        let mut windows = ready_windows();
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );

        assert!(outcome
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::AmbiguousAutomaticRoles { .. })));
        assert!(
            windows.presenter.visible && windows.audience.visible,
            "still usable meanwhile"
        );
    }

    #[test]
    fn an_explicit_selection_removes_the_ambiguity() {
        let a = monitor("DP-1", "Left", Rect::new(0, 0, 2560, 1440));
        let b = monitor("DP-2", "Right", Rect::new(2560, 0, 2560, 1440));
        let roles = DisplayRoles {
            audience: explicit(&b),
            ..DisplayRoles::default()
        };
        let snapshot = DisplaySnapshot::new(vec![a, b], 1);
        let mut windows = ready_windows();
        let outcome = settle(&snapshot, &roles, Capabilities::X11, &mut windows);

        assert!(!outcome
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::AmbiguousAutomaticRoles { .. })));
        assert_eq!(
            outcome.resolved,
            ResolvedRoles {
                presenter: Some(0),
                audience: Some(1)
            }
        );
    }

    #[test]
    fn identical_models_produce_an_ambiguity_warning_rather_than_a_wrong_choice() {
        let a = monitor("DP-1", "Twin", Rect::new(0, 0, 1920, 1080));
        let mut b = monitor("DP-2", "Twin", Rect::new(1920, 0, 1920, 1080));
        b.identity = a.identity.clone();
        let roles = DisplayRoles {
            audience: explicit(&a),
            ..DisplayRoles::default()
        };
        let snapshot = DisplaySnapshot::new(vec![a, b], 1);
        let mut windows = ready_windows();
        let outcome = settle(&snapshot, &roles, Capabilities::X11, &mut windows);

        assert!(outcome.warnings.iter().any(|w| matches!(
            w,
            Warning::AmbiguousSelection {
                role: Role::Audience,
                ..
            }
        )));
    }

    #[test]
    fn primary_reported_nowhere_or_not_at_index_zero_changes_nothing() {
        let mut a = monitor("DP-1", "Left", Rect::new(0, 0, 1920, 1080));
        let mut b = laptop();
        b.primary = true; // built-in panel enumerated second and marked primary
        a.primary = false;
        let snapshot = DisplaySnapshot::new(vec![a, b], 1);
        let mut windows = ready_windows();
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );
        assert_eq!(
            outcome.resolved.presenter,
            Some(1),
            "built-in panel presents"
        );
        assert_eq!(outcome.resolved.audience, Some(0));
    }

    #[test]
    fn swap_is_a_role_exchange_followed_by_ordinary_reconciliation() {
        let snapshot = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        let roles = DisplayRoles {
            presenter: explicit(&laptop()),
            audience: explicit(&projector()),
            ..DisplayRoles::default()
        };
        let mut windows = ready_windows();
        settle(&snapshot, &roles, Capabilities::X11, &mut windows);

        let swapped = roles.swapped();
        let outcome = settle(&snapshot, &swapped, Capabilities::X11, &mut windows);
        assert_eq!(
            outcome.resolved,
            ResolvedRoles {
                presenter: Some(1),
                audience: Some(0)
            }
        );
        assert_eq!(windows.presenter.monitor, Some(identity_of(&projector())));
        assert_eq!(windows.audience.monitor, Some(identity_of(&laptop())));
        assert_eq!(windows.audience.mode, WindowMode::Fullscreen);
    }

    #[test]
    fn swap_during_a_topology_change_converges() {
        let roles = DisplayRoles {
            presenter: explicit(&laptop()),
            audience: explicit(&projector()),
            ..DisplayRoles::default()
        };
        let mut windows = ready_windows();
        settle(
            &DisplaySnapshot::new(vec![laptop(), projector()], 1),
            &roles,
            Capabilities::X11,
            &mut windows,
        );

        // The projector vanishes at the moment the user hits swap.
        let swapped = roles.swapped();
        let outcome = settle(
            &DisplaySnapshot::new(vec![laptop()], 2),
            &swapped,
            Capabilities::X11,
            &mut windows,
        );
        assert!(outcome.has_warning(&Warning::SelectedDisplayMissing {
            role: Role::Presenter
        }));
        assert!(
            windows.presenter.visible,
            "the operator can still drive the talk"
        );

        // ...and comes back.
        let outcome = settle(
            &DisplaySnapshot::new(vec![laptop(), projector()], 3),
            &swapped,
            Capabilities::X11,
            &mut windows,
        );
        assert_eq!(
            outcome.resolved,
            ResolvedRoles {
                presenter: Some(1),
                audience: Some(0)
            }
        );
        let stable = reconcile(
            &DisplaySnapshot::new(vec![laptop(), projector()], 4),
            &swapped,
            Capabilities::X11,
            &windows,
        );
        assert!(stable.is_noop(), "converged");
    }

    #[test]
    fn the_audience_window_is_not_shown_before_it_has_a_frame() {
        let snapshot = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        let mut windows = Windows {
            presenter: WindowState {
                has_frame: true,
                ..WindowState::default()
            },
            audience: WindowState::default(), // no frame yet
        };
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );

        assert!(outcome.has_warning(&Warning::AwaitingFirstFrame));
        assert!(
            !windows.audience.visible,
            "hidden until a valid frame exists"
        );
        assert!(
            matches!(
                outcome.action_for(Role::Audience),
                Some(Action::Place {
                    mode: WindowMode::Fullscreen,
                    ..
                })
            ),
            "but it is already assigned to the right display"
        );

        windows.audience.has_frame = true;
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );
        assert!(outcome.actions.contains(&Action::Show {
            role: Role::Audience
        }));
        assert!(windows.audience.visible);
    }

    #[test]
    fn a_tiling_compositor_falls_back_without_placing() {
        let snapshot = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        let mut windows = ready_windows();
        let outcome = settle(
            &snapshot,
            &DisplayRoles::default(),
            Capabilities::TILING,
            &mut windows,
        );

        assert!(
            outcome.warnings.is_empty(),
            "an unplaceable compositor is not a warning"
        );
        assert!(
            windows.presenter.visible && windows.audience.visible,
            "both stay usable"
        );
    }

    #[test]
    fn wayland_does_not_unfullscreen_into_nowhere() {
        let snapshot = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        let roles = DisplayRoles::default();
        let mut windows = ready_windows();
        settle(&snapshot, &roles, Capabilities::WAYLAND, &mut windows);
        assert_eq!(windows.audience.mode, WindowMode::Fullscreen);

        // Projector gone: on Wayland we leave the fullscreen state alone
        // rather than stranding the window.
        let single = DisplaySnapshot::new(vec![laptop()], 2);
        settle(&single, &roles, Capabilities::WAYLAND, &mut windows);
        assert_eq!(windows.audience.mode, WindowMode::Fullscreen);
        assert!(windows.presenter.visible);
    }

    #[test]
    fn no_displays_at_all_changes_nothing() {
        let mut windows = ready_windows();
        settle(
            &DisplaySnapshot::new(vec![laptop(), projector()], 1),
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );
        let before = windows.clone();
        let outcome = settle(
            &DisplaySnapshot::new(vec![], 2),
            &DisplayRoles::default(),
            Capabilities::X11,
            &mut windows,
        );
        assert!(outcome.has_warning(&Warning::NoDisplays));
        assert_eq!(windows, before, "a topology blip must not move anything");
    }

    #[test]
    fn both_windows_are_never_hidden_and_roles_never_silently_collide() {
        // Property-ish sweep over the topologies the spec enumerates.
        let topologies = [
            vec![laptop()],
            vec![laptop(), projector()],
            vec![projector(), laptop()],
            vec![
                laptop(),
                projector(),
                monitor("DP-2", "Third", Rect::new(-1920, 0, 1920, 1080)),
            ],
            vec![
                monitor("DP-1", "A", Rect::new(0, 0, 1920, 1080)),
                monitor("DP-2", "B", Rect::new(0, 0, 1920, 1080)),
            ],
        ];
        for caps in [
            Capabilities::X11,
            Capabilities::WAYLAND,
            Capabilities::TILING,
        ] {
            let mut windows = ready_windows();
            for (sequence, monitors) in topologies.iter().enumerate() {
                let snapshot = DisplaySnapshot::new(monitors.clone(), sequence as u64 + 1);
                let outcome = settle(&snapshot, &DisplayRoles::default(), caps, &mut windows);
                assert!(windows.presenter.visible, "presenter hidden with {caps:?}");
                if outcome.resolved.presenter.is_some() && outcome.resolved.audience.is_some() {
                    let shared = outcome.resolved.presenter == outcome.resolved.audience;
                    if shared {
                        assert!(
                            outcome.has_warning(&Warning::SharedDisplay),
                            "a shared display must always be reported"
                        );
                        assert!(
                            windows.audience.mode != WindowMode::Fullscreen
                                || outcome.has_warning(&Warning::CannotLeaveFullscreen {
                                    role: Role::Audience
                                }),
                            "either the audience does not cover the presenter, or the \
                             compositor refused to let go of fullscreen and said so"
                        );
                    }
                }
            }
        }
    }
}
