//! Sleep and idle inhibition, as a value.
//!
//! The *state* lives here so the application can display it; the mechanism
//! lives in the platform adapter. Every path must be releasable, and a
//! process-based mechanism is preferred where the platform has one, because
//! the kernel reaps it if pulpit dies mid-talk.

/// Which mechanism is holding the inhibition, if any.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InhibitState {
    #[default]
    Released,
    Held {
        mechanism: &'static str,
        /// Opaque handle the adapter needs in order to release it.
        token: InhibitToken,
    },
    /// No mechanism is available. Reported, never hidden.
    Unavailable {
        reason: String,
        /// Everything that was tried, for diagnostics.
        attempts: Vec<String>,
    },
}

/// What an adapter needs to hand back to release an inhibition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InhibitToken {
    /// A D-Bus object path or similar.
    Handle(String),
    /// A numeric cookie.
    Cookie(u32),
    /// A child process id.
    Process(u32),
    None,
}

impl InhibitState {
    pub fn is_held(&self) -> bool {
        matches!(self, InhibitState::Held { .. })
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, InhibitState::Unavailable { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            InhibitState::Released => "screensaver inhibition released".into(),
            InhibitState::Held { mechanism, .. } => {
                format!("screensaver inhibited via {mechanism}")
            }
            InhibitState::Unavailable { reason, .. } => {
                format!("screensaver inhibition unavailable: {reason}")
            }
        }
    }

    /// Everything that was tried and failed.
    pub fn attempts(&self) -> &[String] {
        match self {
            InhibitState::Unavailable { attempts, .. } => attempts,
            _ => &[],
        }
    }
}

/// A small owner that keeps acquire/release balanced.
///
/// Acquiring twice holds one inhibition; releasing when nothing is held is
/// safe; dropping releases.
#[derive(Debug)]
pub struct Inhibitor {
    state: InhibitState,
}

impl Default for Inhibitor {
    fn default() -> Self {
        Inhibitor {
            state: InhibitState::Released,
        }
    }
}

impl Inhibitor {
    pub fn new() -> Inhibitor {
        Inhibitor::default()
    }

    pub fn state(&self) -> &InhibitState {
        &self.state
    }

    /// Follow the desired state, asking `services` only when it changes.
    pub fn set_desired(
        &mut self,
        wanted: bool,
        services: &dyn crate::platform::services::PlatformServices,
    ) -> &InhibitState {
        match (wanted, self.state.is_held()) {
            (true, false) => self.state = services.inhibit(),
            (false, true) => {
                let _ = services.release_inhibit(&self.state);
                self.state = InhibitState::Released;
            }
            _ => {}
        }
        &self.state
    }

    pub fn release(&mut self, services: &dyn crate::platform::services::PlatformServices) {
        if self.state.is_held() {
            let _ = services.release_inhibit(&self.state);
        }
        self.state = InhibitState::Released;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::null::NullPlatform;

    #[test]
    fn an_unavailable_inhibition_explains_itself_and_lists_attempts() {
        let state = InhibitState::Unavailable {
            reason: "no mechanism answered".into(),
            attempts: vec!["portal: not running".into()],
        };
        assert!(state.is_unavailable());
        assert!(state.describe().contains("unavailable"));
        assert_eq!(state.attempts().len(), 1);
    }

    #[test]
    fn acquiring_twice_holds_one_inhibition() {
        let services = NullPlatform::new("test").holding_inhibition();
        let mut inhibitor = Inhibitor::new();

        inhibitor.set_desired(true, &services);
        assert!(inhibitor.state().is_held());
        assert_eq!(services.inhibit_calls(), 1);

        inhibitor.set_desired(true, &services);
        assert_eq!(services.inhibit_calls(), 1, "no second acquisition");

        inhibitor.set_desired(false, &services);
        assert!(!inhibitor.state().is_held());
        assert_eq!(services.release_calls(), 1);

        // Releasing again is safe and does nothing.
        inhibitor.release(&services);
        assert_eq!(services.release_calls(), 1);
    }

    #[test]
    fn an_unavailable_mechanism_is_not_retried_as_if_held() {
        let services = NullPlatform::new("test");
        let mut inhibitor = Inhibitor::new();
        let state = inhibitor.set_desired(true, &services).clone();
        assert!(state.is_unavailable());
        assert!(!inhibitor.state().is_held());
    }
}
