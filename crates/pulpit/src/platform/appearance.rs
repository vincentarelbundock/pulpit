//! Light, dark, and following the system.

use serde::{Deserialize, Serialize};

/// The user's appearance preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Appearance {
    /// Follow the system preference where it can be detected.
    #[default]
    System,
    /// The reference control-room appearance.
    Dark,
    Light,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Dark, Appearance::Light];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Dark => "Dark",
            Appearance::Light => "Light",
        }
    }
}

/// What the system says, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SystemAppearance {
    /// The platform has no preference, or none we can read.
    #[default]
    Unknown,
    Light,
    Dark,
    /// A high-contrast mode, which takes precedence over any palette choice.
    HighContrast,
}

impl SystemAppearance {
    /// Resolve a preference against what the system reports.
    ///
    /// High contrast wins over everything: a user who asked the operating
    /// system for high contrast did not mean "except in this application".
    /// When the system cannot be read, `System` falls back to Dark — the
    /// documented default — and the caller records the fallback.
    pub fn resolve(self, preference: Appearance) -> Resolved {
        match (self, preference) {
            (SystemAppearance::HighContrast, _) => Resolved::HighContrast,
            (_, Appearance::Dark) => Resolved::Dark,
            (_, Appearance::Light) => Resolved::Light,
            (SystemAppearance::Light, Appearance::System) => Resolved::Light,
            (SystemAppearance::Dark, Appearance::System) => Resolved::Dark,
            (SystemAppearance::Unknown, Appearance::System) => Resolved::Dark,
        }
    }

    /// Did resolving fall back because the system could not be read?
    pub fn fell_back(self, preference: Appearance) -> bool {
        self == SystemAppearance::Unknown && preference == Appearance::System
    }
}

/// The palette actually in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    Dark,
    Light,
    HighContrast,
}

impl Resolved {
    pub fn label(self) -> &'static str {
        match self {
            Resolved::Dark => "dark",
            Resolved::Light => "light",
            Resolved::HighContrast => "high contrast",
        }
    }
}

/// Whether the desktop has asked for motion to be kept to a minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MotionPreference {
    /// The platform has no preference, or none we can read.
    #[default]
    Unknown,
    Full,
    Reduced,
}

/// What the application does about motion, once the user's own setting and
/// the system preference are both taken into account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motion {
    #[default]
    Full,
    Reduced,
}

impl Motion {
    pub fn is_reduced(self) -> bool {
        self == Motion::Reduced
    }

    /// Resolve the user's choice against what the system reports.
    ///
    /// An explicit choice in pulpit's own settings wins, because someone
    /// who reached for it meant this application in particular. `System`
    /// follows the desktop, and falls back to full motion when the desktop
    /// says nothing — reducing motion nobody asked to reduce would quietly
    /// stop an author's animated slide from playing.
    pub fn resolve(system: MotionPreference, preference: MotionSetting) -> Motion {
        match (preference, system) {
            (MotionSetting::Full, _) => Motion::Full,
            (MotionSetting::Reduced, _) => Motion::Reduced,
            (MotionSetting::System, MotionPreference::Reduced) => Motion::Reduced,
            (MotionSetting::System, _) => Motion::Full,
        }
    }
}

/// The motion setting pulpit itself stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MotionSetting {
    /// Follow the desktop.
    #[default]
    System,
    Full,
    Reduced,
}

impl MotionSetting {
    pub const ALL: [MotionSetting; 3] = [
        MotionSetting::System,
        MotionSetting::Full,
        MotionSetting::Reduced,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MotionSetting::System => "Follow the system",
            MotionSetting::Full => "Full motion",
            MotionSetting::Reduced => "Reduce motion",
        }
    }
}

#[cfg(test)]
mod motion_tests {
    use super::*;

    #[test]
    fn an_explicit_choice_beats_whatever_the_desktop_says() {
        assert_eq!(
            Motion::resolve(MotionPreference::Reduced, MotionSetting::Full),
            Motion::Full
        );
        assert_eq!(
            Motion::resolve(MotionPreference::Full, MotionSetting::Reduced),
            Motion::Reduced
        );
    }

    #[test]
    fn following_the_system_honours_a_reduced_motion_desktop() {
        assert_eq!(
            Motion::resolve(MotionPreference::Reduced, MotionSetting::System),
            Motion::Reduced
        );
        assert!(Motion::resolve(MotionPreference::Reduced, MotionSetting::System).is_reduced());
    }

    #[test]
    fn an_unreadable_desktop_preference_leaves_motion_alone() {
        // Reducing motion nobody asked to reduce would stop an author's
        // animated slide from playing for no stated reason.
        assert_eq!(
            Motion::resolve(MotionPreference::Unknown, MotionSetting::System),
            Motion::Full
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_choice_is_honoured() {
        assert_eq!(
            SystemAppearance::Light.resolve(Appearance::Dark),
            Resolved::Dark
        );
        assert_eq!(
            SystemAppearance::Dark.resolve(Appearance::Light),
            Resolved::Light
        );
    }

    #[test]
    fn system_follows_the_system() {
        assert_eq!(
            SystemAppearance::Light.resolve(Appearance::System),
            Resolved::Light
        );
        assert_eq!(
            SystemAppearance::Dark.resolve(Appearance::System),
            Resolved::Dark
        );
    }

    #[test]
    fn high_contrast_beats_every_preference() {
        for preference in Appearance::ALL {
            assert_eq!(
                SystemAppearance::HighContrast.resolve(preference),
                Resolved::HighContrast,
                "{preference:?} must not override a high-contrast system"
            );
        }
    }

    #[test]
    fn an_undetectable_system_falls_back_to_dark_and_says_so() {
        assert_eq!(
            SystemAppearance::Unknown.resolve(Appearance::System),
            Resolved::Dark
        );
        assert!(SystemAppearance::Unknown.fell_back(Appearance::System));
        assert!(!SystemAppearance::Unknown.fell_back(Appearance::Light));
        assert!(!SystemAppearance::Dark.fell_back(Appearance::System));
    }
}
