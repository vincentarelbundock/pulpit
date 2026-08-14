//! What the audience screen is doing, and whether there is one.

use pulpit_core::Blank;

/// How the audience output should be described, if at all.
///
/// `None` is deliberate: presenting into a window on one screen is a normal
/// way to work, and a permanent line saying so is a label on the wall rather
/// than news. Blanking is the case worth interrupting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudienceReading {
    pub text: &'static str,
    pub intent: StatusIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusIntent {
    Good,
    Warning,
}

/// What the audience-status widget says, given what the audience sees.
pub fn audience_reading(blank: Blank, fullscreen: bool) -> Option<AudienceReading> {
    match (blank, fullscreen) {
        (Blank::Black, _) => Some(AudienceReading {
            text: "Audience blanked (black)",
            intent: StatusIntent::Warning,
        }),
        (Blank::White, _) => Some(AudienceReading {
            text: "Audience blanked (white)",
            intent: StatusIntent::Warning,
        }),
        (Blank::Off, true) => Some(AudienceReading {
            text: "Live on the audience display",
            intent: StatusIntent::Good,
        }),
        (Blank::Off, false) => None,
    }
}

/// What the connection widget says.
pub fn connection_reading(connected: bool) -> AudienceReading {
    if connected {
        AudienceReading {
            text: "Audience display connected",
            intent: StatusIntent::Good,
        }
    } else {
        AudienceReading {
            text: "No audience display",
            intent: StatusIntent::Warning,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanking_is_always_worth_saying() {
        for blank in [Blank::Black, Blank::White] {
            for fullscreen in [true, false] {
                let reading = audience_reading(blank, fullscreen).expect("blanking is reported");
                assert_eq!(reading.intent, StatusIntent::Warning);
            }
        }
    }

    #[test]
    fn an_ordinary_window_says_nothing() {
        assert_eq!(audience_reading(Blank::Off, false), None);
        assert_eq!(
            audience_reading(Blank::Off, true).map(|reading| reading.intent),
            Some(StatusIntent::Good)
        );
    }

    #[test]
    fn the_connection_reading_follows_the_topology() {
        assert_eq!(connection_reading(true).intent, StatusIntent::Good);
        assert_eq!(connection_reading(false).intent, StatusIntent::Warning);
    }
}
