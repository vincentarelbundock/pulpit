//! Audience window lifecycle and display reconciliation (§79.4): the one
//! `reconcile()` call the three rules require, opening and closing the
//! audience window, and the placement retries a window that refused a
//! move gets queued onto.
//!
//! No `App` fields move here — `placement_retries` and the window ids
//! stay in app.rs — the same shape as the other `app::*` extractions.

use iced::{window, Task};

use pulpit_display::{
    apply_outcome, Action as DisplayAction, Reconciliation, Role, WindowMode, WindowState,
};

use crate::display;
use crate::settings::diagnostics::describe_warning;

use super::{
    advice, shows_a_notice, App, Message, PlacementRetry, PLACEMENT_RETRY_DELAY,
    PLACEMENT_VERIFY_DELAYS, PRESENTER_REFOCUS_DELAYS,
};

impl App {
    /// Reconcile displays and perform the resulting actions.
    pub(super) fn reconcile(&mut self) -> Task<Message> {
        if self.coordinator.snapshot.is_empty() {
            self.coordinator.refresh();
        }
        let snapshot = self.coordinator.snapshot.clone();
        let roles = self.coordinator.roles.clone();
        let capabilities = self.coordinator.capabilities;
        let windows = self.coordinator.windows.clone();

        let mut outcome =
            match self
                .coordinator
                .reconciler
                .reconcile(&snapshot, &roles, capabilities, &windows)
            {
                Reconciliation::Applied(outcome) => outcome,
                Reconciliation::Unchanged => return Task::none(),
                Reconciliation::Stale { sequence, newest } => {
                    self.diagnostics.note(format!(
                        "ignored stale topology #{sequence} (newest #{newest})"
                    ));
                    return Task::none();
                }
            };

        // The pure reconciler always models two roles. Before Start (and
        // after Stop), keep resolving those roles for the menu but suppress
        // every action or warning that assumes an audience window was asked
        // for. This is the application-level lifecycle boundary.
        if !self.audience_started {
            outcome
                .actions
                .retain(|action| action.role() != Role::Audience);
            outcome.warnings.retain(|warning| {
                !matches!(
                    warning,
                    pulpit_display::Warning::NoSecondaryDisplay
                        | pulpit_display::Warning::SharedDisplay
                        | pulpit_display::Warning::AmbiguousSelection {
                            role: Role::Audience,
                            ..
                        }
                        | pulpit_display::Warning::SelectedDisplayMissing {
                            role: Role::Audience
                        }
                        | pulpit_display::Warning::CannotLeaveFullscreen {
                            role: Role::Audience
                        }
                        | pulpit_display::Warning::AwaitingFirstFrame
                )
            });
        }

        self.coordinator.resolved = outcome.resolved;
        self.diagnostics
            .record_roles(&roles.presenter, &roles.audience);
        self.diagnostics.record_outcome(&outcome);
        // Display warnings go to the corner and to the diagnostics bundle,
        // which is the durable record. Standing conditions — no second
        // display, a saved display missing — stay up while they are true;
        // events fade.
        let mut conditions = Vec::new();
        for warning in &outcome.warnings {
            let text = describe_warning(warning);
            tracing::warn!(target: "pulpit::display", "{text}");
            self.diagnostics.note(format!("display: {text}"));
            if !shows_a_notice(warning) {
                continue;
            }
            if warning.is_condition() {
                conditions.push((warning.key(), text, advice(warning)));
            } else {
                self.toasts.warning(text, self.now);
            }
        }
        self.toasts.set_conditions(&conditions);

        // Does this outcome change what the audience window is doing? Asked
        // before the actions are carried out, because the answer decides
        // whether the focus has to be pulled back afterwards.
        let audience_mode_changed = outcome.actions.iter().any(|action| match action {
            DisplayAction::Place { role, mode, .. } => {
                *role == Role::Audience && *mode != self.coordinator.windows.audience.mode
            }
            DisplayAction::Show { role } => *role == Role::Audience,
        });

        let mut tasks = Vec::new();
        for action in &outcome.actions {
            match action {
                DisplayAction::Place {
                    role,
                    identity,
                    mode,
                    ..
                } => {
                    let outcome = match self.coordinator.native(*role) {
                        Some(native) => self.coordinator.backend.place(native, identity, *mode),
                        // The window is not mapped yet, so there is no native
                        // id to place. Queue it: this is the pre-map case.
                        None => pulpit_display::PlacementOutcome::Refused,
                    };
                    let placed = matches!(outcome, pulpit_display::PlacementOutcome::Applied);
                    if !placed && self.coordinator.capabilities.can_place() {
                        // Retry after the window is mapped rather than giving
                        // up on the first refusal.
                        self.placement_retries.retain(|retry| retry.role != *role);
                        self.placement_retries.push(PlacementRetry {
                            role: *role,
                            identity: identity.clone(),
                            mode: *mode,
                            attempt: 1,
                            due: self.now
                                + if matches!(outcome, pulpit_display::PlacementOutcome::Pending) {
                                    PLACEMENT_VERIFY_DELAYS[0]
                                } else {
                                    PLACEMENT_RETRY_DELAY
                                },
                            verifying: matches!(outcome, pulpit_display::PlacementOutcome::Pending),
                        });
                    } else if !placed {
                        // Backends that cannot place (Wayland, tiling) still
                        // get their window mode set below, so no error toast
                        // here.
                        if let Some(message) = display::describe_placement(&outcome) {
                            self.diagnostics.note(format!("display: {message}"));
                        }
                    }
                    // Whether or not targeted placement worked, the window's
                    // own mode is still ours to set.
                    if let Some(id) = self.window_id(*role) {
                        tasks.push(window::set_mode::<Message>(id, display::iced_mode(*mode)));
                    }
                    if !placed && *mode == WindowMode::Fullscreen {
                        self.diagnostics
                            .note("targeted placement unavailable; used toolkit fullscreen");
                    }
                }
                DisplayAction::Show { role } => {
                    if let Some(id) = self.window_id(*role) {
                        // Showing is a mode change in Iced. Preserve the mode
                        // planned in this same reconciliation instead of
                        // briefly undoing fullscreen while mapping.
                        let mode = outcome
                            .actions
                            .iter()
                            .find_map(|action| match action {
                                DisplayAction::Place {
                                    role: placed_role,
                                    mode,
                                    ..
                                } if placed_role == role => Some(*mode),
                                _ => None,
                            })
                            .unwrap_or(WindowMode::Windowed);
                        tasks.push(window::set_mode::<Message>(id, display::iced_mode(mode)));
                    }
                    // A window that has not been mapped yet may not be
                    // placeable. Re-assert its placement just after mapping,
                    // so the audience window reaches the selected display
                    // without ever flashing an empty frame.
                    if self.coordinator.capabilities.can_place()
                        && !self.coordinator.capabilities.place_before_map
                    {
                        let planned = outcome.actions.iter().find_map(|action| match action {
                            DisplayAction::Place {
                                role: placed_role,
                                identity,
                                mode,
                                ..
                            } if placed_role == role => Some((identity.clone(), *mode)),
                            _ => None,
                        });
                        let current = self.coordinator.windows.get(*role);
                        let placement = planned.or_else(|| {
                            current
                                .monitor
                                .clone()
                                .map(|identity| (identity, current.mode))
                        });
                        if let Some((identity, mode)) = placement {
                            self.placement_retries.retain(|retry| retry.role != *role);
                            self.placement_retries.push(PlacementRetry {
                                role: *role,
                                identity,
                                mode,
                                attempt: 1,
                                due: self.now + PLACEMENT_RETRY_DELAY,
                                verifying: false,
                            });
                        }
                    }
                }
            }
        }

        // Changing the audience window's mode (fullscreen in particular)
        // makes most window managers focus it. Schedule focus repair after
        // mapping settles; doing it in this same task batch loses the race.
        if audience_mode_changed {
            self.schedule_presenter_refocus();
        }

        let mut windows = self.coordinator.windows.clone();
        apply_outcome(&mut windows, &outcome);
        self.coordinator.windows = windows;
        self.coordinator
            .reconciler
            .note_windows(&self.coordinator.windows);

        self.apply_inhibition();

        Task::batch(tasks)
    }

    /// Ask the session to stay awake, or stop asking.
    ///
    /// Two reasons, one claim. The audience output being fullscreen is the
    /// old one: a projector must not blank mid-talk. A running autoadvance is
    /// the new one, and it is the reason this had to become a function — an
    /// unattended loop in a *windowed* reader, with no audience window at
    /// all, was the one case where nobody was pressing a key and nothing was
    /// fullscreen. Still a capability rather than an assumption: the
    /// `Outcome` the inhibitor reports is what the diagnostics record.
    pub(super) fn apply_inhibition(&mut self) {
        if !self.settings.display.inhibit_screensaver {
            return;
        }
        let fullscreen = self.coordinator.windows.audience.mode == WindowMode::Fullscreen;
        let state = self
            .inhibitor
            .set_desired(
                fullscreen || self.autoadvance.is_on(),
                self.platform.services.as_ref(),
            )
            .clone();
        self.diagnostics.note(state.describe());
    }

    pub(super) fn window_id(&self, role: Role) -> Option<window::Id> {
        match role {
            Role::Presenter => self.presenter_window,
            Role::Audience => self.audience_window,
        }
    }

    pub(super) fn schedule_presenter_refocus(&mut self) {
        self.presenter_refocus_deadlines.clear();
        self.presenter_refocus_deadlines.extend(
            PRESENTER_REFOCUS_DELAYS
                .into_iter()
                .map(|delay| self.now + delay),
        );
    }

    /// Map the prepared audience toplevel and let ordinary reconciliation put
    /// it on the selected display.
    /// Take the claim on the audience window, or say who has it.
    ///
    /// Several copies of pulpit may run at once, and everything else they do
    /// is independent. The projector is not: two audience windows on one
    /// screen leave the window manager flipping between them many times a
    /// second, which is a violently flickering screen in the middle of a talk.
    /// So the second copy is told where the first one is instead.
    ///
    /// A claim that cannot be recorded at all stands down. A missing guard is
    /// a risk; a guard that stops a presenter from presenting is a certainty.
    fn claim_audience(&mut self) -> bool {
        if self.audience_claim.is_some() {
            return true;
        }
        match crate::platform::acquire_claim(&self.audience_claim_path) {
            crate::platform::Instance::Acquired(claim) => {
                self.audience_claim = Some(claim);
                true
            }
            crate::platform::Instance::AlreadyRunning { pid, .. } => {
                let who = match pid {
                    Some(pid) => format!(" (process {pid})"),
                    None => String::new(),
                };
                self.notify(format!(
                    "Another copy of pulpit is presenting{who}. \
                     Stop its audience window first — two on one projector \
                     would flicker against each other."
                ));
                false
            }
            crate::platform::Instance::Unknown { reason } => {
                tracing::warn!(reason, "presenting without recording the audience claim");
                true
            }
        }
    }

    pub(super) fn start_audience(&mut self, windowed: bool) -> Task<Message> {
        self.audience_start_menu_open = false;
        if self.audience_started {
            return Task::none();
        }
        if self.state.document().is_none() {
            self.notify("Open a document before starting the audience window.".into());
            return Task::none();
        }
        if !self.claim_audience() {
            return Task::none();
        }

        if windowed {
            self.coordinator.roles.audience_fullscreen = false;
        }
        self.audience_started = true;
        self.mark_audience_frame();
        self.request_renders();
        self.diagnostics.note(if windowed {
            "audience started windowed"
        } else {
            "audience started"
        });
        if self.audience_window.is_none() {
            self.open_audience_window()
        } else {
            self.reconcile()
        }
    }

    /// Destroy the audience toplevel. A later Start creates it afresh on the
    /// desktop context active at that moment.
    pub(super) fn stop_audience(&mut self) -> Task<Message> {
        let was_active = self.audience_started;
        self.audience_gone();
        if was_active {
            self.notify_done("Audience stopped.".into());
        }
        self.audience_window
            .take()
            .map(window::close::<Message>)
            .unwrap_or_else(Task::none)
    }

    /// Everything that must be true once the audience window is gone,
    /// however it went — WM-closed out from under the app, or deliberately
    /// stopped. §77.6: `Message::WindowClosed` used to reset only the window
    /// state, leaving `audience_claim` held (so another copy could not use
    /// the projector), `roles.audience_fullscreen` at whatever a one-run
    /// "start windowed" left it, and `placement_retries` for a window that no
    /// longer exists. This is what `stop_audience` already did, factored out
    /// so both paths do it.
    pub(super) fn audience_gone(&mut self) {
        // The projector is free for another copy the moment this one stops
        // using it, not when this process ends.
        self.audience_claim = None;
        self.audience_started = false;
        self.audience_start_menu_open = false;
        self.presenter_refocus_deadlines.clear();
        // "Start windowed" is a one-run placement aid, not a preference
        // change. Restore the saved default for the next ordinary Start.
        self.coordinator.roles.audience_fullscreen =
            self.settings.display.roles.audience_fullscreen;
        self.coordinator.roles.allow_shared_display =
            self.settings.display.roles.allow_shared_display;
        self.placement_retries
            .retain(|retry| retry.role != Role::Audience);
        *self.coordinator.window_state_mut(Role::Audience) = WindowState::default();
        self.coordinator.set_native(Role::Audience, None);
        self.coordinator
            .reconciler
            .note_windows(&self.coordinator.windows);
        self.inhibitor.release(self.platform.services.as_ref());
    }

    /// Create the audience hidden. Reconciliation places it and only reveals
    /// it once a complete slide frame is ready.
    fn open_audience_window(&mut self) -> Task<Message> {
        let (id, opened) = window::open(display::identify_window(
            window::Settings {
                size: self.audience_size,
                decorations: false,
                visible: false,
                ..window::Settings::default()
            },
            Role::Audience,
        ));
        self.audience_window = Some(id);
        opened.map(move |id| Message::WindowOpened {
            role: Role::Audience,
            id,
        })
    }
}
