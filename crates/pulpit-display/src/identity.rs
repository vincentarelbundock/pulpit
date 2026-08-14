//! Monitor identity ladder.
//!
//! The backend's native monitor handle is a weak, revocable capability, never
//! application state. Persisted roles store only the descriptors below, and
//! records are schema-versioned so a stronger identifier can be adopted later
//! without losing the user's choices.

use serde::{Deserialize, Serialize};

/// Schema version of persisted identity records.
pub const IDENTITY_SCHEMA: u32 = 1;

/// Identity strength, strongest first. Tier 1 requires native adapters
/// (EDID/DRM, `QueryDisplayConfig`, CoreGraphics) and is Phase 4 work; the
/// Linux MVP runs on tiers 2 and 3 with ambiguity surfaced, never guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IdentityTier {
    /// 1. Platform-stable identifier or EDID-derived identity.
    Stable,
    /// 2. Connector plus manufacturer and model.
    Connector,
    /// 3. Manufacturer, model, physical size and position.
    Geometric,
    /// 4. Session-local handle. Never persisted usefully.
    Session,
}

impl IdentityTier {
    pub fn is_persistable(self) -> bool {
        !matches!(self, IdentityTier::Session)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "tier", rename_all = "kebab-case")]
pub enum MonitorIdentity {
    /// Tier 1: EDID serial or platform-stable identifier.
    Stable { id: String },
    /// Tier 2: connector name plus make/model.
    Connector {
        connector: String,
        make: String,
        model: String,
    },
    /// Tier 3: make/model plus physical size and desktop position.
    Geometric {
        make: String,
        model: String,
        width_mm: u32,
        height_mm: u32,
        x: i32,
        y: i32,
    },
    /// Tier 4: session-local handle from this run only.
    Session { handle: u64 },
}

impl MonitorIdentity {
    pub fn tier(&self) -> IdentityTier {
        match self {
            MonitorIdentity::Stable { .. } => IdentityTier::Stable,
            MonitorIdentity::Connector { .. } => IdentityTier::Connector,
            MonitorIdentity::Geometric { .. } => IdentityTier::Geometric,
            MonitorIdentity::Session { .. } => IdentityTier::Session,
        }
    }

    /// Human-readable label for the display selector.
    pub fn label(&self) -> String {
        match self {
            MonitorIdentity::Stable { id } => id.clone(),
            MonitorIdentity::Connector {
                connector,
                make,
                model,
            } => {
                format!("{connector} — {make} {model}")
            }
            MonitorIdentity::Geometric {
                make, model, x, y, ..
            } => {
                format!("{make} {model} @ {x},{y}")
            }
            MonitorIdentity::Session { handle } => format!("monitor {handle}"),
        }
    }
}

/// A persisted identity record, versioned so stronger identifiers can be
/// adopted later without discarding user choices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityRecord {
    pub schema: u32,
    pub identity: MonitorIdentity,
    /// A weaker identity kept alongside so a tier-1 record still matches when
    /// the native adapter is unavailable in a later session.
    #[serde(default)]
    pub fallback: Option<MonitorIdentity>,
}

impl IdentityRecord {
    pub fn new(identity: MonitorIdentity) -> Self {
        Self {
            schema: IDENTITY_SCHEMA,
            identity,
            fallback: None,
        }
    }

    pub fn with_fallback(mut self, fallback: MonitorIdentity) -> Self {
        self.fallback = Some(fallback);
        self
    }

    pub fn candidates(&self) -> impl Iterator<Item = &MonitorIdentity> {
        std::iter::once(&self.identity).chain(self.fallback.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_are_ordered_strongest_first() {
        assert!(IdentityTier::Stable < IdentityTier::Connector);
        assert!(IdentityTier::Connector < IdentityTier::Geometric);
        assert!(IdentityTier::Geometric < IdentityTier::Session);
        assert!(!IdentityTier::Session.is_persistable());
    }

    #[test]
    fn records_carry_a_weaker_fallback() {
        let record = IdentityRecord::new(MonitorIdentity::Stable {
            id: "EDID-ABC".into(),
        })
        .with_fallback(MonitorIdentity::Connector {
            connector: "HDMI-1".into(),
            make: "ACME".into(),
            model: "P1".into(),
        });
        assert_eq!(record.schema, IDENTITY_SCHEMA);
        assert_eq!(record.candidates().count(), 2);
    }
}
