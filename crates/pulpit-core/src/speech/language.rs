//! Which language a page is written in, and which installed voice can read it.
//!
//! Two jobs that look like one. Detection answers "what is this text"; lookup
//! answers "given that answer, which of the voices this session actually has
//! should speak it". They are separate because they fail separately: a
//! confident `de` with no German voice installed is a download prompt, while
//! an unconfident guess on a page of equations is a reason to keep using
//! whatever voice is already speaking.
//!
//! Detection is deliberately dependency-free. A trigram model would be more
//! accurate on short spans, but the accurate ones ship tens of megabytes of
//! n-grams, and this crate is the one that has to stay pure and cheap. What is
//! here instead is script identification — which is exact, not statistical,
//! for every non-Latin writing system — plus function-word scoring for the
//! Latin-script languages, where the script alone cannot decide. That is
//! reliable on a page of prose, which is the unit this is asked about, and it
//! reports low confidence rather than guessing when it is not.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A language tag, in the small subset of BCP 47 that voices are named by.
///
/// Piper voices are `fr_FR`, `pt_BR`, `en_GB`; Kokoro's are a one-letter
/// prefix. Neither is a full BCP 47 tag and neither needs to be: the useful
/// content is a primary subtag and an optional region, and matching those two
/// correctly is the whole job (RFC 4647 lookup).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LanguageTag {
    /// ISO 639 primary subtag, lowercased: `en`, `de`, `zh`.
    primary: String,
    /// ISO 3166 region, uppercased: `US`, `BR`. Absent means "any region".
    region: Option<String>,
}

/// Serialised as the string it is written as everywhere else — `"en-US"`, not
/// a nested object. The settings file is edited by hand often enough that its
/// readability is a feature, and the voice catalog is authored as plain tags.
impl Serialize for LanguageTag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for LanguageTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        LanguageTag::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!("{text:?} is not a language tag such as \"en-US\""))
        })
    }
}

impl LanguageTag {
    /// Parse `en`, `en-US`, `en_US` or `EN-us`. Separators and case are
    /// normalised, because voice filenames and PDF `/Lang` entries disagree
    /// about both and neither is wrong.
    pub fn parse(text: &str) -> Option<LanguageTag> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut parts = text.split(['-', '_']).filter(|part| !part.is_empty());
        let primary = parts.next()?;
        if primary.len() < 2
            || primary.len() > 3
            || !primary.chars().all(|c| c.is_ascii_alphabetic())
        {
            return None;
        }
        // Take the first part that looks like a region and ignore the rest: a
        // script subtag (`zh-Hans-CN`) is not something any voice is named by,
        // and dropping it is better than refusing the tag.
        let region = parts.find_map(|part| {
            let is_region = (part.len() == 2 && part.chars().all(|c| c.is_ascii_alphabetic()))
                || (part.len() == 3 && part.chars().all(|c| c.is_ascii_digit()));
            is_region.then(|| part.to_ascii_uppercase())
        });
        Some(LanguageTag {
            primary: primary.to_ascii_lowercase(),
            region,
        })
    }

    /// A tag with no region: `en`, matching any English voice.
    pub fn language(primary: &str) -> LanguageTag {
        LanguageTag {
            primary: primary.to_ascii_lowercase(),
            region: None,
        }
    }

    pub fn primary(&self) -> &str {
        &self.primary
    }

    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }

    /// This tag with the region dropped — the fallback rung in lookup.
    pub fn without_region(&self) -> LanguageTag {
        LanguageTag {
            primary: self.primary.clone(),
            region: None,
        }
    }

    /// Same language, ignoring region.
    pub fn same_language(&self, other: &LanguageTag) -> bool {
        self.primary == other.primary
    }
}

impl fmt::Display for LanguageTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.region {
            Some(region) => write!(f, "{}-{}", self.primary, region),
            None => write!(f, "{}", self.primary),
        }
    }
}

/// How well a candidate matched what was asked for.
///
/// Ordered worst-first so a search can keep the best seen with `>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchQuality {
    /// Same primary subtag, different region: a `pt-PT` voice for `pt-BR`
    /// text. Understandable, audibly not local.
    Language,
    /// Region matched too, or neither side named one.
    Exact,
}

/// RFC 4647 lookup: the best of `available` for `wanted`, if any.
///
/// Returns the index rather than the value so the caller keeps whatever it
/// was storing alongside the tag. Ties go to the earliest candidate, which
/// makes the result stable against reordering the voice list.
pub fn lookup<'a, I>(wanted: &LanguageTag, available: I) -> Option<(usize, MatchQuality)>
where
    I: IntoIterator<Item = &'a LanguageTag>,
{
    let mut best: Option<(usize, MatchQuality)> = None;
    for (index, candidate) in available.into_iter().enumerate() {
        if !candidate.same_language(wanted) {
            continue;
        }
        let quality = match (wanted.region(), candidate.region()) {
            (Some(a), Some(b)) if a == b => MatchQuality::Exact,
            (None, None) => MatchQuality::Exact,
            // Asking for `en` and finding `en-GB` is as good as it gets when
            // the request named no region; there is nothing better to hold
            // out for.
            (None, Some(_)) | (Some(_), None) => MatchQuality::Language,
            (Some(_), Some(_)) => MatchQuality::Language,
        };
        if best.is_none_or(|(_, seen)| quality > seen) {
            best = Some((index, quality));
        }
        if quality == MatchQuality::Exact {
            break;
        }
    }
    best
}

/// How much to trust a detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Confidence {
    /// Too little text, or no signal. Do not switch voices on this.
    Low,
    /// Enough to prefer, not enough to override an explicit choice.
    Medium,
    /// A page of prose that scored decisively.
    High,
}

/// What [`detect`] concluded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub tag: LanguageTag,
    pub confidence: Confidence,
}

/// Below this many letters, detection reports [`Confidence::Low`] whatever it
/// found. Roughly a short sentence: function-word scoring needs a handful of
/// function words to have seen any.
pub const MIN_LETTERS_FOR_CONFIDENCE: usize = 60;

/// Writing systems that identify a language, or narrow it to a few.
///
/// Exact, not statistical: a page of Greek letters is Greek. The Latin case is
/// the only one that needs the scoring below, which is why it is the only one
/// that returns no tag here.
fn script_of(c: char) -> Option<Script> {
    let code = c as u32;
    Some(match code {
        0x0041..=0x024F => Script::Latin,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        0x0400..=0x052F => Script::Cyrillic,
        0x0530..=0x058F => Script::Armenian,
        0x0590..=0x05FF => Script::Hebrew,
        0x0600..=0x06FF | 0x0750..=0x077F => Script::Arabic,
        0x0900..=0x097F => Script::Devanagari,
        0x0980..=0x09FF => Script::Bengali,
        0x0B80..=0x0BFF => Script::Tamil,
        0x0C00..=0x0C7F => Script::Telugu,
        0x0D00..=0x0D7F => Script::Malayalam,
        0x0E00..=0x0E7F => Script::Thai,
        0x10A0..=0x10FF => Script::Georgian,
        0x1100..=0x11FF | 0xAC00..=0xD7AF | 0x3130..=0x318F => Script::Hangul,
        0x3040..=0x30FF => Script::Kana,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF => Script::Han,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Script {
    Latin,
    Greek,
    Cyrillic,
    Armenian,
    Hebrew,
    Arabic,
    Devanagari,
    Bengali,
    Tamil,
    Telugu,
    Malayalam,
    Thai,
    Georgian,
    Hangul,
    Kana,
    Han,
}

impl Script {
    /// The language a script implies on its own, where it implies one.
    ///
    /// Cyrillic is left to scoring because Russian, Ukrainian and Serbian
    /// share it and a reader would notice the wrong one immediately. Han is
    /// Chinese *unless* kana appeared, which the caller checks first: Japanese
    /// prose is mostly Han characters with kana between them, so counting Han
    /// alone would call every Japanese page Chinese.
    fn implied(self) -> Option<&'static str> {
        Some(match self {
            Script::Latin | Script::Cyrillic => return None,
            Script::Greek => "el",
            Script::Armenian => "hy",
            Script::Hebrew => "he",
            Script::Arabic => "ar",
            Script::Devanagari => "hi",
            Script::Bengali => "bn",
            Script::Tamil => "ta",
            Script::Telugu => "te",
            Script::Malayalam => "ml",
            Script::Thai => "th",
            Script::Georgian => "ka",
            Script::Hangul => "ko",
            Script::Kana => "ja",
            Script::Han => "zh",
        })
    }
}

/// Function words, per language, that are common in that language and rare in
/// its neighbours.
///
/// Chosen for discrimination rather than frequency: `de` scores nothing for
/// German because Dutch and Spanish also have it, while `und`, `nicht` and
/// `auch` are decisive. Words are matched whole and lowercased, so short
/// entries cannot fire inside longer words.
const LATIN_MARKERS: &[(&str, &[&str])] = &[
    (
        "en",
        &[
            "the", "and", "of", "to", "that", "with", "which", "this", "from", "have", "was",
            "are", "were", "been", "their", "would", "there", "about",
        ],
    ),
    (
        "de",
        &[
            "und", "nicht", "auch", "eine", "einer", "sich", "werden", "wird", "durch", "sind",
            "aber", "diese", "dass", "über", "oder", "zwischen", "wenn",
        ],
    ),
    (
        "fr",
        &[
            "les", "des", "une", "est", "dans", "que", "pour", "sur", "avec", "pas", "plus",
            "cette", "sont", "leur", "être", "aux", "ainsi", "nous",
        ],
    ),
    (
        "es",
        &[
            "los", "las", "una", "por", "con", "para", "como", "más", "pero", "sus", "este",
            "esta", "son", "entre", "también", "todo", "cuando",
        ],
    ),
    (
        "it",
        &[
            "che", "non", "per", "una", "sono", "come", "anche", "nella", "degli", "delle",
            "questo", "questa", "essere", "alla", "dei", "più", "quando",
        ],
    ),
    (
        "pt",
        &[
            "não", "uma", "para", "com", "que", "por", "mais", "como", "está", "são", "pelo",
            "pela", "dos", "das", "isso", "quando", "também",
        ],
    ),
    (
        "nl",
        &[
            "het", "een", "van", "niet", "zijn", "worden", "wordt", "deze", "voor", "maar", "door",
            "ook", "naar", "bij", "over", "tussen", "wanneer",
        ],
    ),
    (
        "pl",
        &[
            "nie",
            "jest",
            "się",
            "przez",
            "oraz",
            "który",
            "która",
            "tego",
            "jako",
            "tylko",
            "może",
            "przy",
            "wszystkie",
            "jeśli",
            "bardzo",
        ],
    ),
    (
        "sv",
        &[
            "och", "att", "det", "som", "för", "inte", "med", "till", "den", "har", "kan", "eller",
            "över", "från", "vara",
        ],
    ),
    (
        "da",
        &[
            "og", "til", "det", "ikke", "som", "med", "der", "kan", "eller", "har", "være",
            "efter", "sig", "hvis",
        ],
    ),
    (
        "no",
        &[
            "ikke", "og", "til", "som", "med", "for", "har", "kan", "eller", "være", "etter",
            "fra", "denne", "over",
        ],
    ),
    (
        "fi",
        &[
            "että", "ovat", "myös", "sekä", "voi", "kuin", "mutta", "tämä", "niin", "kun", "sen",
            "joka", "ei",
        ],
    ),
    (
        "cs",
        &[
            "není", "jako", "také", "která", "který", "pro", "podle", "však", "aby", "před",
            "nebo", "jsou", "když",
        ],
    ),
    (
        "tr",
        &[
            "bir", "ile", "için", "olarak", "daha", "değil", "olan", "gibi", "sonra", "kadar",
            "veya", "ancak",
        ],
    ),
    (
        "hu",
        &[
            "hogy", "nem", "egy", "azonban", "vagy", "mint", "után", "által", "amely", "ezek",
            "csak", "még",
        ],
    ),
    (
        "ro",
        &[
            "care", "este", "pentru", "sunt", "din", "prin", "dar", "acest", "această", "mai",
            "fost", "între",
        ],
    ),
    (
        "ca",
        &[
            "amb", "per", "una", "els", "les", "aquest", "aquesta", "són", "però", "també", "més",
            "quan",
        ],
    ),
    (
        "id",
        &[
            "yang", "dan", "dengan", "untuk", "pada", "dari", "tidak", "adalah", "akan", "ini",
            "atau", "dalam",
        ],
    ),
    (
        "vi",
        &[
            "của", "và", "trong", "được", "các", "một", "người", "những", "cho", "không", "này",
            "với",
        ],
    ),
];

/// Marker words for the languages that share the Cyrillic script.
const CYRILLIC_MARKERS: &[(&str, &[&str])] = &[
    (
        "ru",
        &[
            "это",
            "как",
            "что",
            "для",
            "или",
            "которые",
            "может",
            "были",
            "если",
            "также",
            "при",
            "более",
        ],
    ),
    (
        "uk",
        &[
            "це",
            "як",
            "що",
            "для",
            "або",
            "які",
            "може",
            "були",
            "якщо",
            "також",
            "при",
            "більш",
            "та",
        ],
    ),
    (
        "sr",
        &[
            "је",
            "су",
            "као",
            "или",
            "које",
            "може",
            "били",
            "ако",
            "такође",
            "при",
            "више",
            "између",
        ],
    ),
    (
        "bg",
        &[
            "това",
            "като",
            "или",
            "които",
            "може",
            "бяха",
            "ако",
            "също",
            "при",
            "повече",
            "между",
            "със",
        ],
    ),
];

/// Identify the language of a run of text.
///
/// Returns `None` only when there is nothing to go on at all — no letters in
/// any script this knows. Anything else comes back with a tag and a
/// confidence, and it is the confidence, not the presence of an answer, that
/// callers should be gating on.
pub fn detect(text: &str) -> Option<Detection> {
    let mut counts: std::collections::HashMap<Script, usize> = std::collections::HashMap::new();
    let mut letters = 0usize;
    for c in text.chars() {
        if let Some(script) = script_of(c) {
            *counts.entry(script).or_default() += 1;
            letters += 1;
        }
    }
    if letters == 0 {
        return None;
    }

    // Japanese before Chinese: kana are exclusive to Japanese, and Japanese
    // prose is majority Han by character count, so whichever is *most* common
    // is the wrong question. Any real kana presence settles it.
    let kana = counts.get(&Script::Kana).copied().unwrap_or(0);
    if kana * 40 >= letters {
        return Some(Detection {
            tag: LanguageTag::language("ja"),
            confidence: confidence_for(letters, 1.0),
        });
    }

    let (&dominant, &dominant_count) = counts.iter().max_by_key(|(_, &n)| n)?;
    let share = dominant_count as f32 / letters as f32;

    // A script that names its language needs no scoring.
    if let Some(primary) = dominant.implied() {
        return Some(Detection {
            tag: LanguageTag::language(primary),
            confidence: confidence_for(letters, share),
        });
    }

    let markers = match dominant {
        Script::Latin => LATIN_MARKERS,
        Script::Cyrillic => CYRILLIC_MARKERS,
        // `implied` covered every other case.
        _ => return None,
    };
    Some(score_markers(text, markers, letters, share))
}

/// Score whole-word marker hits and return the winner.
fn score_markers(text: &str, markers: &[(&str, &[&str])], letters: usize, share: f32) -> Detection {
    let lowered = text.to_lowercase();
    let words: Vec<&str> = lowered
        .split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|word| !word.is_empty())
        .collect();

    let mut best: (&str, usize) = (markers[0].0, 0);
    let mut runner_up = 0usize;
    for (tag, list) in markers {
        let hits = words.iter().filter(|word| list.contains(word)).count();
        if hits > best.1 {
            runner_up = best.1;
            best = (tag, hits);
        } else if hits > runner_up {
            runner_up = hits;
        }
    }

    // Separation matters as much as the raw count. Two languages scoring
    // equally is exactly the Norwegian/Danish case, and answering it
    // confidently would be worse than answering it hesitantly.
    let decisive = best.1 >= 3 && best.1 >= runner_up * 2;
    let confidence = if best.1 == 0 {
        Confidence::Low
    } else if decisive {
        confidence_for(letters, share)
    } else {
        Confidence::Low.max(confidence_for(letters, share).min(Confidence::Medium))
    };

    Detection {
        tag: LanguageTag::language(best.0),
        confidence,
    }
}

fn confidence_for(letters: usize, share: f32) -> Confidence {
    if letters < MIN_LETTERS_FOR_CONFIDENCE {
        return Confidence::Low;
    }
    if share > 0.9 && letters >= MIN_LETTERS_FOR_CONFIDENCE * 3 {
        Confidence::High
    } else {
        Confidence::Medium
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_normalise_separator_and_case() {
        let underscore = LanguageTag::parse("pt_BR").unwrap();
        let hyphen = LanguageTag::parse("PT-br").unwrap();
        assert_eq!(underscore, hyphen);
        assert_eq!(underscore.to_string(), "pt-BR");
        assert_eq!(LanguageTag::parse("en").unwrap().to_string(), "en");
    }

    #[test]
    fn a_script_subtag_is_dropped_rather_than_refused() {
        let tag = LanguageTag::parse("zh-Hans-CN").unwrap();
        assert_eq!(tag.primary(), "zh");
        assert_eq!(tag.region(), Some("CN"));
    }

    #[test]
    fn tags_round_trip_through_json_as_plain_strings() {
        let tag = LanguageTag::parse("pt-BR").unwrap();
        let json = serde_json::to_string(&tag).unwrap();
        assert_eq!(json, "\"pt-BR\"", "readable in a settings file");
        assert_eq!(serde_json::from_str::<LanguageTag>(&json).unwrap(), tag);

        // A hand-edited settings file with rubbish in it says what is wrong
        // rather than failing to parse the whole file mysteriously.
        let error = serde_json::from_str::<LanguageTag>("\"not a tag\"").unwrap_err();
        assert!(error.to_string().contains("en-US"), "got {error}");
    }

    #[test]
    fn nonsense_is_not_a_tag() {
        assert!(LanguageTag::parse("").is_none());
        assert!(LanguageTag::parse("english").is_none());
        assert!(LanguageTag::parse("1").is_none());
    }

    #[test]
    fn lookup_prefers_the_region_but_accepts_the_language() {
        let available = [
            LanguageTag::parse("en-GB").unwrap(),
            LanguageTag::parse("en-US").unwrap(),
            LanguageTag::parse("de-DE").unwrap(),
        ];
        let wanted = LanguageTag::parse("en-US").unwrap();
        assert_eq!(lookup(&wanted, &available), Some((1, MatchQuality::Exact)));

        // Brazilian text, only European Portuguese installed: still a match,
        // and the caller is told it is only a language-level one.
        let pt = [LanguageTag::parse("pt-PT").unwrap()];
        let wanted = LanguageTag::parse("pt-BR").unwrap();
        assert_eq!(lookup(&wanted, &pt), Some((0, MatchQuality::Language)));

        let wanted = LanguageTag::parse("pl").unwrap();
        assert_eq!(lookup(&wanted, &available), None);
    }

    #[test]
    fn scripts_identify_their_language_without_scoring() {
        for (sample, expected) in [
            ("Το κείμενο αυτό είναι γραμμένο στα ελληνικά.", "el"),
            ("هذا النص مكتوب باللغة العربية.", "ar"),
            ("यह पाठ हिंदी में लिखा गया है।", "hi"),
            ("이 텍스트는 한국어로 작성되었습니다.", "ko"),
            ("ეს ტექსტი ქართულად არის დაწერილი.", "ka"),
        ] {
            let found = detect(sample).expect("a script was present");
            assert_eq!(found.tag.primary(), expected, "for {sample}");
        }
    }

    #[test]
    fn japanese_is_not_reported_as_chinese() {
        // Majority Han by character count, with kana between — the case that
        // a naive "most common script wins" gets backwards.
        let japanese = "この文書は日本語で書かれています。表示装置の設定を確認してください。";
        assert_eq!(detect(japanese).unwrap().tag.primary(), "ja");

        let chinese = "这份文件是用中文写的。请检查显示设置。";
        assert_eq!(detect(chinese).unwrap().tag.primary(), "zh");
    }

    #[test]
    fn latin_languages_are_told_apart_by_function_words() {
        let cases = [
            (
                "This document describes the reconciliation function and the \
                 rules that govern it, which are the same rules that apply to \
                 the audience window when it is connected to a projector.",
                "en",
            ),
            (
                "Dieses Dokument beschreibt die Funktion und die Regeln, die \
                 dafür gelten, und auch nicht zuletzt die Frage, ob eine \
                 Verbindung zwischen den Geräten besteht oder wird.",
                "de",
            ),
            (
                "Ce document décrit les règles qui sont appliquées dans le \
                 cas des écrans, avec une attention plus particulière pour \
                 cette question, ainsi que pour les projecteurs.",
                "fr",
            ),
            (
                "Este documento describe las reglas que se aplican para los \
                 proyectores, con una atención más particular por esta \
                 cuestión, pero también entre todos los casos.",
                "es",
            ),
            (
                "Questo documento descrive le regole che sono applicate per \
                 gli schermi, con una attenzione più particolare per questa \
                 questione, anche nella maggior parte dei casi.",
                "it",
            ),
        ];
        for (sample, expected) in cases {
            let found = detect(sample).expect("letters were present");
            assert_eq!(found.tag.primary(), expected, "for {expected} sample");
            assert!(
                found.confidence >= Confidence::Medium,
                "a paragraph should not be low confidence"
            );
        }
    }

    #[test]
    fn short_text_is_never_confident() {
        let found = detect("The quick brown fox.").unwrap();
        assert_eq!(found.confidence, Confidence::Low);
    }

    #[test]
    fn a_page_of_equations_has_nothing_to_detect() {
        assert!(detect("1 + 2 = 3   ∑ ∫ ≤ ≥ 42 %").is_none());
        assert!(detect("").is_none());
    }

    #[test]
    fn a_language_with_no_markers_scores_low_rather_than_guessing_loudly() {
        // Latin script, no marker words from any list: an answer comes back,
        // but not one worth switching voices on.
        let found = detect(
            "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do \
             eiusmod tempor incididunt ut labore et dolore magna aliqua.",
        )
        .unwrap();
        assert_eq!(found.confidence, Confidence::Low);
    }
}
