use serde::{Deserialize, Serialize};

/// A monotonically increasing render generation.
///
/// Every document load/reload, DPI change, notes-mapping change or relevant
/// resize advances the generation. Renderer results carrying an older
/// generation are discarded on both sides of the IPC protocol.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct RenderGeneration(pub u64);

impl RenderGeneration {
    pub const ZERO: RenderGeneration = RenderGeneration(0);

    #[must_use]
    pub fn next(self) -> RenderGeneration {
        RenderGeneration(self.0 + 1)
    }

    pub fn advance(&mut self) -> RenderGeneration {
        self.0 += 1;
        *self
    }

    /// True when `self` is at least as new as `other`.
    pub fn is_current_for(self, other: RenderGeneration) -> bool {
        self.0 >= other.0
    }
}

impl std::fmt::Display for RenderGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "gen{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generations_are_monotonic() {
        let mut g = RenderGeneration::ZERO;
        assert_eq!(g.advance(), RenderGeneration(1));
        assert_eq!(g.advance(), RenderGeneration(2));
        assert!(g.is_current_for(RenderGeneration(1)));
        assert!(!RenderGeneration(1).is_current_for(g));
    }
}
