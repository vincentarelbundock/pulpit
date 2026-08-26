//! Choosing the voice: the `Auto` setting, and what it does when it is wrong.
//!
//! Detection produces a language; this decides whether to act on it. The two
//! are separate because acting on every detection is worse than acting on
//! none. A paper in English that quotes two lines of German should not switch
//! voices for the quote and switch back after it; a bilingual proceedings
//! volume should switch, once, at the page where the language changes. The
//! difference is confidence and hysteresis, and both live here.

use serde::{Deserialize, Serialize};

use super::language::{lookup, Confidence, Detection, LanguageTag, MatchQuality};

/// A voice the session can actually use, as far as this crate cares.
///
/// Deliberately not the engine's voice type: the domain needs an identifier
/// and a language, and nothing about model files or sample rates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceRef {
    pub id: String,
    pub language: LanguageTag,
}

impl VoiceRef {
    pub fn new(id: impl Into<String>, language: LanguageTag) -> VoiceRef {
        VoiceRef {
            id: id.into(),
            language,
        }
    }
}

/// What the settings page holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LanguageSetting {
    /// Follow the document. The default, because a reader who has not chosen
    /// is better served by a voice that follows the page than by whichever
    /// language happened to be installed first.
    #[default]
    Auto,
    /// Always this language, whatever the page says.
    Explicit(LanguageTag),
}

/// What to do about the page just examined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Speak with this voice. `index` is into the slice that was passed in.
    Use {
        index: usize,
        quality: MatchQuality,
        language: LanguageTag,
    },
    /// The language was identified confidently and there is no voice for it.
    ///
    /// This is the download prompt, and the reason it is a distinct answer
    /// rather than a silent fallback: reading Polish in an American accent is
    /// a worse outcome than asking.
    Missing { language: LanguageTag },
    /// Nothing was decided — too little text, an unconfident guess, or a
    /// language already in use. Carry on with whatever is speaking.
    Keep,
}

/// The `Auto` state machine.
///
/// Holds the document-level prior established when reading started, and the
/// language currently in use, so that a per-page detection is judged against
/// both rather than in isolation.
#[derive(Debug, Clone, Default)]
pub struct LanguagePolicy {
    setting: LanguageSetting,
    prior: Option<LanguageTag>,
    current: Option<LanguageTag>,
}

impl LanguagePolicy {
    pub fn new(setting: LanguageSetting) -> LanguagePolicy {
        LanguagePolicy {
            setting,
            prior: None,
            current: None,
        }
    }

    pub fn setting(&self) -> &LanguageSetting {
        &self.setting
    }

    /// Change the setting. Clears what was in use, so the next page is
    /// judged fresh rather than against a language the reader just overrode.
    pub fn set(&mut self, setting: LanguageSetting) {
        self.setting = setting;
        self.current = None;
    }

    /// The language currently being spoken, for the settings page to show
    /// beside `Auto`. `Auto` that will not say what it resolved to is worse
    /// than an explicit wrong choice, because the reader cannot see why.
    pub fn current(&self) -> Option<&LanguageTag> {
        self.current.as_ref()
    }

    /// Establish the document-level prior, from the catalog `/Lang` entry or
    /// from a sample of the document's text.
    ///
    /// A `/Lang` entry is authoritative when present, but Word stamps
    /// `en-US` on everything it exports regardless of content, so it is a
    /// prior and not a verdict: a confident detection later overrides it.
    pub fn observe_document(&mut self, declared: Option<LanguageTag>, sample: Option<Detection>) {
        self.prior = declared.or_else(|| {
            sample
                .filter(|found| found.confidence >= Confidence::Medium)
                .map(|found| found.tag)
        });
    }

    /// Decide the voice for a page.
    ///
    /// `detected` is what [`super::language::detect`] said about this page's
    /// text; `voices` is what the session can actually speak with.
    pub fn resolve(&mut self, detected: Option<Detection>, voices: &[VoiceRef]) -> Resolution {
        let wanted = match &self.setting {
            // An explicit choice is not re-litigated per page. That is the
            // whole point of choosing it.
            LanguageSetting::Explicit(tag) => tag.clone(),
            LanguageSetting::Auto => match self.auto_target(detected) {
                Some(tag) => tag,
                None => return Resolution::Keep,
            },
        };

        // Already speaking this language: nothing to change, and in
        // particular no re-selection that could pick a different voice for
        // the same language mid-document.
        if self.current.as_ref() == Some(&wanted) {
            return Resolution::Keep;
        }

        let tags: Vec<&LanguageTag> = voices.iter().map(|voice| &voice.language).collect();
        match lookup(&wanted, tags) {
            Some((index, quality)) => {
                self.current = Some(wanted.clone());
                Resolution::Use {
                    index,
                    quality,
                    language: wanted,
                }
            }
            None => Resolution::Missing { language: wanted },
        }
    }

    /// What `Auto` wants for this page, or `None` to leave things alone.
    fn auto_target(&self, detected: Option<Detection>) -> Option<LanguageTag> {
        let Some(found) = detected else {
            // No letters at all — a page of figures. Keep speaking whatever
            // was speaking; the next page of prose will decide.
            return self.current.clone().or_else(|| self.prior.clone());
        };

        match found.confidence {
            // Never switch on a guess. This is the rule that stops a page
            // with one German quotation from flipping the voice.
            Confidence::Low => self.current.clone().or_else(|| self.prior.clone()),
            Confidence::Medium => {
                // Enough to choose when nothing is chosen yet, not enough to
                // overrule a language already established.
                if self.current.is_some() {
                    self.current.clone()
                } else {
                    Some(found.tag)
                }
            }
            Confidence::High => Some(found.tag),
        }
    }
}

/// How fast to speak, as a multiple of the voice's natural rate.
///
/// A newtype because the useful range is not obvious and the clamp has to
/// happen somewhere that cannot be forgotten: engines differ in what they do
/// with an out-of-range value, and one of the options is "refuse".
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SpeechRate(f32);

/// Through [`SpeechRate::new`], not around it. A derived transparent
/// `Deserialize` would admit whatever number a hand-edited settings file
/// holds, which is precisely the path the clamp exists for — the one input
/// no UI control ever sanitised.
impl<'de> Deserialize<'de> for SpeechRate {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(SpeechRate::new(f32::deserialize(deserializer)?))
    }
}

impl SpeechRate {
    pub const SLOWEST: f32 = 0.5;
    pub const FASTEST: f32 = 3.0;
    pub const NORMAL: SpeechRate = SpeechRate(1.0);

    /// Clamped on the way in, so an out-of-range value cannot be stored and
    /// then handed to an engine that refuses it.
    pub fn new(multiple: f32) -> SpeechRate {
        if !multiple.is_finite() {
            return SpeechRate::NORMAL;
        }
        SpeechRate(multiple.clamp(SpeechRate::SLOWEST, SpeechRate::FASTEST))
    }

    pub fn get(self) -> f32 {
        self.0
    }

    /// Piper expresses speed as a length scale: seconds of audio per unit of
    /// input, so *larger* is slower and it is the reciprocal of a rate.
    /// Getting this backwards is a bug that sounds like the slider is
    /// inverted, which is exactly what it would be.
    pub fn length_scale(self) -> f32 {
        1.0 / self.0
    }

    /// A label for the settings row.
    pub fn label(self) -> String {
        format!("{:.2}×", self.0)
    }
}

impl Default for SpeechRate {
    fn default() -> Self {
        SpeechRate::NORMAL
    }
}

#[cfg(test)]
mod tests {
    use super::super::language::detect;
    use super::*;

    fn voices() -> Vec<VoiceRef> {
        vec![
            VoiceRef::new("en_US-lessac-medium", LanguageTag::parse("en-US").unwrap()),
            VoiceRef::new("en_GB-alba-medium", LanguageTag::parse("en-GB").unwrap()),
            VoiceRef::new("fr_FR-siwis-medium", LanguageTag::parse("fr-FR").unwrap()),
        ]
    }

    fn english() -> Option<Detection> {
        detect(
            "This document describes the reconciliation function and the rules \
             that govern it, which are the same rules that apply to the audience \
             window when it is connected to a projector or to a second display.",
        )
    }

    /// A page of German prose, not a sentence of it: switching languages
    /// mid-document requires high confidence, and high confidence requires
    /// enough text to have earned it.
    fn german() -> Option<Detection> {
        detect(
            "Dieses Dokument beschreibt die Funktion und die Regeln, die dafür \
             gelten, und auch nicht zuletzt die Frage, ob eine Verbindung \
             zwischen den Geräten besteht oder hergestellt wird, sowie diese \
             Einstellungen. Wenn eine Anzeige getrennt wird, werden die \
             Fenster nicht verschoben, sondern die Rollen werden neu \
             zugewiesen, und auch dann bleibt das Bild für das Publikum \
             erhalten. Diese Regel gilt zwischen allen Geräten, oder sie gilt \
             überhaupt nicht, denn eine Ausnahme wäre nicht nachvollziehbar.",
        )
    }

    #[test]
    fn auto_picks_a_voice_for_a_confident_page() {
        let mut policy = LanguagePolicy::new(LanguageSetting::Auto);
        let resolution = policy.resolve(english(), &voices());
        assert!(matches!(resolution, Resolution::Use { index: 0, .. }));
        assert_eq!(policy.current().unwrap().primary(), "en");
    }

    #[test]
    fn auto_does_not_flip_on_a_short_quotation() {
        let mut policy = LanguagePolicy::new(LanguageSetting::Auto);
        policy.resolve(english(), &voices());
        // A page whose detection is unconfident must not move the voice.
        let quote = detect("Der Mensch ist frei.");
        assert_eq!(policy.resolve(quote, &voices()), Resolution::Keep);
        assert_eq!(policy.current().unwrap().primary(), "en");
    }

    #[test]
    fn auto_switches_for_a_confidently_different_page() {
        let mut policy = LanguagePolicy::new(LanguageSetting::Auto);
        policy.resolve(english(), &voices());
        // No German voice installed: the honest answer is a download prompt,
        // not English pronouncing German.
        assert_eq!(
            policy.resolve(german(), &voices()),
            Resolution::Missing {
                language: LanguageTag::language("de")
            }
        );
    }

    #[test]
    fn a_medium_confidence_page_does_not_overrule_an_established_language() {
        // The deliberate conservative rung: enough evidence to choose when
        // nothing is chosen, not enough to switch away from a language that
        // is already reading well. A French abstract on an English paper is
        // the case this gets "wrong" on purpose — one paragraph in the wrong
        // accent beats a voice that flips back and forth all document.
        let short_german = detect(
            "Dieses Dokument beschreibt die Regeln, die dafür gelten, und auch \
             die Frage, ob eine Verbindung besteht oder nicht.",
        )
        .expect("letters present");
        assert_eq!(short_german.confidence, Confidence::Medium);

        let mut policy = LanguagePolicy::new(LanguageSetting::Auto);
        policy.resolve(english(), &voices());
        assert_eq!(
            policy.resolve(Some(short_german), &voices()),
            Resolution::Keep
        );

        // With nothing established yet, the same page decides.
        let mut fresh = LanguagePolicy::new(LanguageSetting::Auto);
        let medium = detect(
            "Ce document décrit les règles qui sont appliquées dans le cas des \
             écrans, avec une attention plus particulière pour cette question.",
        );
        assert!(matches!(
            fresh.resolve(medium, &voices()),
            Resolution::Use { index: 2, .. }
        ));
    }

    #[test]
    fn a_page_with_no_text_keeps_the_current_voice() {
        let mut policy = LanguagePolicy::new(LanguageSetting::Auto);
        policy.resolve(english(), &voices());
        assert_eq!(policy.resolve(None, &voices()), Resolution::Keep);
        assert_eq!(policy.current().unwrap().primary(), "en");
    }

    #[test]
    fn an_explicit_setting_ignores_the_page_entirely() {
        let mut policy =
            LanguagePolicy::new(LanguageSetting::Explicit(LanguageTag::parse("fr").unwrap()));
        let resolution = policy.resolve(english(), &voices());
        assert!(matches!(resolution, Resolution::Use { index: 2, .. }));
    }

    #[test]
    fn a_declared_language_seeds_the_prior_but_a_confident_page_wins() {
        let mut policy = LanguagePolicy::new(LanguageSetting::Auto);
        // Word stamps en-US on everything; the document is really French.
        policy.observe_document(Some(LanguageTag::parse("en-US").unwrap()), None);
        let french = detect(
            "Ce document décrit les règles qui sont appliquées dans le cas des \
             écrans, avec une attention plus particulière pour cette question, \
             ainsi que pour les projecteurs et les autres appareils.",
        );
        let resolution = policy.resolve(french, &voices());
        assert!(matches!(resolution, Resolution::Use { index: 2, .. }));
    }

    #[test]
    fn changing_the_setting_re_decides_the_next_page() {
        let mut policy = LanguagePolicy::new(LanguageSetting::Auto);
        policy.resolve(english(), &voices());
        policy.set(LanguageSetting::Explicit(
            LanguageTag::parse("fr-FR").unwrap(),
        ));
        assert!(policy.current().is_none());
        let resolution = policy.resolve(english(), &voices());
        assert!(matches!(resolution, Resolution::Use { index: 2, .. }));
    }

    #[test]
    fn a_regional_mismatch_is_reported_as_such() {
        let mut policy = LanguagePolicy::new(LanguageSetting::Explicit(
            LanguageTag::parse("fr-CA").unwrap(),
        ));
        let resolution = policy.resolve(None, &voices());
        assert_eq!(
            resolution,
            Resolution::Use {
                index: 2,
                quality: MatchQuality::Language,
                language: LanguageTag::parse("fr-CA").unwrap(),
            }
        );
    }

    #[test]
    fn a_hand_edited_rate_is_clamped_on_load_not_trusted() {
        // The settings file is the one input no slider ever sanitised.
        let absurd: SpeechRate = serde_json::from_str("100.0").unwrap();
        assert_eq!(absurd.get(), SpeechRate::FASTEST);
        let negative: SpeechRate = serde_json::from_str("-3.0").unwrap();
        assert_eq!(negative.get(), SpeechRate::SLOWEST);
        // And a sane value round-trips untouched.
        let fine: SpeechRate = serde_json::from_str("1.25").unwrap();
        assert_eq!(fine.get(), 1.25);
        assert_eq!(serde_json::to_string(&fine).unwrap(), "1.25");
    }

    #[test]
    fn rate_is_clamped_and_never_inverted_by_accident() {
        assert_eq!(SpeechRate::new(10.0).get(), SpeechRate::FASTEST);
        assert_eq!(SpeechRate::new(0.0).get(), SpeechRate::SLOWEST);
        assert_eq!(SpeechRate::new(f32::NAN), SpeechRate::NORMAL);
        // Faster speech is a *smaller* length scale.
        assert!(SpeechRate::new(2.0).length_scale() < SpeechRate::NORMAL.length_scale());
        assert_eq!(SpeechRate::NORMAL.label(), "1.00×");
    }
}
