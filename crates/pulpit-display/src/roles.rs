use serde::{Deserialize, Serialize};

use crate::identity::IdentityRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    Presenter,
    Audience,
}

impl Role {
    pub fn other(self) -> Role {
        match self {
            Role::Presenter => Role::Audience,
            Role::Audience => Role::Presenter,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Presenter => "presenter",
            Role::Audience => "audience",
        }
    }
}

/// Which display a role wants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum RoleTarget {
    /// Let the coordinator choose: built-in panel for the presenter, external
    /// display for the audience, ambiguity surfaced rather than guessed.
    #[default]
    Auto,
    /// An explicit user selection, stored as a versioned identity record.
    Monitor(Box<IdentityRecord>),
}

impl RoleTarget {
    pub fn record(&self) -> Option<&IdentityRecord> {
        match self {
            RoleTarget::Auto => None,
            RoleTarget::Monitor(record) => Some(record),
        }
    }

    pub fn is_explicit(&self) -> bool {
        matches!(self, RoleTarget::Monitor(_))
    }
}

/// The user's desired display configuration. This is persisted; it contains
/// no native handles and no geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayRoles {
    pub presenter: RoleTarget,
    pub audience: RoleTarget,
    /// Whether the audience window should be borderless-fullscreen on its
    /// display once one is available.
    pub audience_fullscreen: bool,
    /// Escape hatch equivalent to pdfpc's `--windowed=both`: allow the
    /// audience window to go fullscreen even when it would land on the
    /// presenter's display. Off by default, because it hides the presenter.
    pub allow_shared_display: bool,
}

impl Default for DisplayRoles {
    fn default() -> Self {
        Self {
            presenter: RoleTarget::Auto,
            audience: RoleTarget::Auto,
            audience_fullscreen: true,
            allow_shared_display: false,
        }
    }
}

impl DisplayRoles {
    /// Swapping is a role exchange followed by ordinary reconciliation, never
    /// a sequence of ad-hoc window moves.
    #[must_use]
    pub fn swapped(&self) -> DisplayRoles {
        DisplayRoles {
            presenter: self.audience.clone(),
            audience: self.presenter.clone(),
            ..self.clone()
        }
    }

    pub fn target(&self, role: Role) -> &RoleTarget {
        match role {
            Role::Presenter => &self.presenter,
            Role::Audience => &self.audience,
        }
    }

    pub fn set_target(&mut self, role: Role, target: RoleTarget) {
        match role {
            Role::Presenter => self.presenter = target,
            Role::Audience => self.audience = target,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::MonitorIdentity;

    fn record(id: &str) -> RoleTarget {
        RoleTarget::Monitor(Box::new(IdentityRecord::new(MonitorIdentity::Stable {
            id: id.to_string(),
        })))
    }

    #[test]
    fn swap_exchanges_only_the_targets() {
        let roles = DisplayRoles {
            presenter: record("A"),
            audience: record("B"),
            audience_fullscreen: true,
            allow_shared_display: false,
        };
        let swapped = roles.swapped();
        assert_eq!(swapped.presenter, roles.audience);
        assert_eq!(swapped.audience, roles.presenter);
        assert!(swapped.audience_fullscreen);
        assert_eq!(swapped.swapped(), roles, "swap is an involution");
    }
}
