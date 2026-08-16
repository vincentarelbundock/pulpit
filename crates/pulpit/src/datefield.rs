//! Dates for form fields: the calendar behind the picker, and the one place
//! that turns a chosen day into the text a field expects (§8.6).
//!
//! A date field in a PDF is a text field whose format script names an Acrobat
//! pattern — `dd mmmm yyyy`, `m/d/yy`. Acrobat and PDF Studio each answer that
//! with a calendar of their own; the pattern is all the file says, so a viewer
//! that wants a picker draws one.
//!
//! The pattern says the *shape* of a date and never the language. Acrobat
//! writes it in the viewer's locale, so one file reads `16 août 2026` to one
//! person and `16 August 2026` to another, and pulpit does the same — from the
//! system's own locale data rather than from a table written here, so it is
//! every locale the machine has rather than the handful somebody remembered.
//!
//! Nothing here reads the clock or touches PDFium. The month a picker opens on
//! is passed in, and what comes out is a string handed to PDFium's own editor
//! like any other typed value — so there is still one implementation of what a
//! value looks like in a field, and it is still PDFium's.

use std::str::FromStr;

use chrono::NaiveDate;

/// The locale a date is written in.
///
/// A thin wrapper over `chrono`'s, for two reasons: it gives this module a
/// place to put the parsing of a POSIX locale string, which `chrono` does not
/// do, and it keeps the rest of pulpit from depending on which calendar
/// library is underneath.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locale(chrono::Locale);

impl Default for Locale {
    /// The language the pattern vocabulary itself is written in, which is the
    /// honest answer when the machine will not say.
    fn default() -> Locale {
        Locale(chrono::Locale::en_US)
    }
}

impl Locale {
    /// The locale to write dates in, from the usual environment variables.
    ///
    /// `LC_ALL` overrides everything by POSIX rule; `LC_TIME` is the one that
    /// means "how dates are written" specifically; `LANG` is the fallback.
    /// `C` and `POSIX` are not locales anyone wants a month name from.
    pub fn from_environment() -> Locale {
        for name in ["LC_ALL", "LC_TIME", "LANG"] {
            let Ok(value) = std::env::var(name) else {
                continue;
            };
            if value.is_empty() || value == "C" || value == "POSIX" {
                continue;
            }
            if let Some(locale) = Locale::parse(&value) {
                return locale;
            }
        }
        Locale::default()
    }

    /// Read a POSIX locale string — `fr_CA.UTF-8`, `pt_BR@euro`, `de-DE`.
    ///
    /// Returns `None` for anything with no date data behind it, so a caller
    /// can tell "not understood" from "understood as English". The tail after
    /// `.` or `@` is a character set and a modifier, neither of which changes
    /// a month's name; a dash is accepted because BCP 47 writes one where
    /// POSIX writes an underscore, and both turn up in these variables.
    pub fn parse(tag: &str) -> Option<Locale> {
        let head = tag
            .split(['.', '@'])
            .next()
            .unwrap_or(tag)
            .replace('-', "_");
        if head.is_empty() {
            return None;
        }
        if let Ok(locale) = chrono::Locale::from_str(&head) {
            return Some(Locale(locale));
        }
        // `LANG=fr`, with no territory. The locale data is keyed by language
        // *and* territory, so a bare language has to be given one — and any
        // territory of that language has that language's month names, which is
        // all that is being asked for.
        //
        // The guess is the language doubled, which is right for most of Europe
        // (`fr_FR`, `de_DE`, `es_ES`, `nl_NL`), with a short list for the ones
        // where it is not. That list is territories, *not* translations: the
        // month names still come from the system's own data for every locale
        // it has, so a language missing from here degrades to English rather
        // than to a name somebody typed in by hand.
        let language = head.split('_').next()?;
        if language.len() < 2 {
            return None;
        }
        const TERRITORIES: [(&str, &str); 12] = [
            ("en", "US"),
            ("ja", "JP"),
            ("zh", "CN"),
            ("ko", "KR"),
            ("sv", "SE"),
            ("da", "DK"),
            ("nb", "NO"),
            ("cs", "CZ"),
            ("el", "GR"),
            ("uk", "UA"),
            ("he", "IL"),
            ("hi", "IN"),
        ];
        let territory = TERRITORIES
            .into_iter()
            .find(|(name, _)| *name == language)
            .map(|(_, territory)| territory.to_owned())
            .unwrap_or_else(|| language.to_uppercase());
        chrono::Locale::from_str(&format!("{language}_{territory}"))
            .ok()
            .map(Locale)
    }

    /// A date rendered with one `strftime` specifier, in this locale.
    fn render(self, date: NaiveDate, specifier: &str) -> String {
        date.format_localized(specifier, self.0).to_string()
    }

    /// A date inside `month`, for asking the locale its name.
    fn in_month(month: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2001, month.clamp(1, 12), 1).unwrap_or_default()
    }

    /// The full month name, 1-based.
    pub fn month_name(self, month: u32) -> String {
        self.render(Locale::in_month(month), "%B")
    }

    /// The abbreviated month name Acrobat's `mmm` asks for.
    pub fn month_abbreviation(self, month: u32) -> String {
        self.render(Locale::in_month(month), "%b")
    }

    /// The full weekday name Acrobat's `dddd` asks for.
    pub fn weekday_name(self, weekday: Weekday) -> String {
        self.render(weekday.representative(), "%A")
    }

    /// The heading a calendar column takes.
    ///
    /// The locale's own abbreviated weekday rather than the first letter of
    /// the full one: an initial is only unambiguous by accident — English
    /// gives T, T and S, S — and every locale already ships a short form
    /// designed for exactly this.
    pub fn weekday_initial(self, weekday: Weekday) -> String {
        self.render(weekday.representative(), "%a")
    }
}

/// A day of the week, Monday first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    pub const ALL: [Weekday; 7] = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ];

    /// Some date that falls on this weekday, for asking the locale its name.
    ///
    /// The first week of 2024, which began on a Monday.
    fn representative(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(2024, 1, 1 + self as u32).unwrap_or_default()
    }
}

/// A date, with no clock and no time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    /// 1–12.
    pub month: u32,
    /// 1–31.
    pub day: u32,
}

impl Date {
    pub fn new(year: i32, month: u32, day: u32) -> Date {
        Date { year, month, day }
    }

    /// Whether a year has 366 days, by the Gregorian rule.
    pub fn is_leap_year(year: i32) -> bool {
        (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
    }

    /// How many days a month has, 1-based. Zero for a month out of range.
    pub fn days_in_month(year: i32, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if Date::is_leap_year(year) => 29,
            2 => 28,
            _ => 0,
        }
    }

    /// Which day of the week this date falls on.
    ///
    /// Sakamoto's method, which is a table lookup and a division. Kept rather
    /// than deferred to `chrono` because the calendar grid is laid out from it
    /// and a grid is worth being able to test without constructing dates that
    /// can fail to exist.
    pub fn weekday(self) -> Weekday {
        const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let mut year = self.year;
        if self.month < 3 {
            year -= 1;
        }
        let index = (year + year / 4 - year / 100
            + year / 400
            + OFFSETS[(self.month.clamp(1, 12) - 1) as usize]
            + self.day as i32)
            .rem_euclid(7);
        // Sakamoto counts from Sunday; this counts from Monday.
        match index {
            0 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            _ => Weekday::Saturday,
        }
    }

    /// Whether this is a date the calendar actually has.
    ///
    /// Not reached from the picker, which can only offer days a month has —
    /// it is the rule those days are generated by, asserted directly.
    #[allow(
        dead_code,
        reason = "the calendar cannot produce an invalid day; this is the rule it obeys"
    )]
    pub fn is_valid(self) -> bool {
        self.month >= 1
            && self.month <= 12
            && self.day >= 1
            && self.day <= Date::days_in_month(self.year, self.month)
    }

    /// Write this date the way `pattern` asks for it.
    ///
    /// `pattern` is Acrobat's vocabulary, because that is what the field's own
    /// format script names and what its author chose. The tokens are matched
    /// longest-first — `mmmm` before `mmm` before `mm` before `m` — and
    /// anything that is not a token is copied through, so the separators and
    /// any literal words in the pattern survive.
    ///
    /// Time tokens are not handled: a date picker has no time to put in them,
    /// and rendering them as zeroes would write a precision the person never
    /// chose. A pattern that carries one keeps it verbatim, which is visibly
    /// wrong rather than quietly wrong.
    pub fn format(self, pattern: &str, locale: Locale) -> String {
        let mut out = String::new();
        let characters: Vec<char> = pattern.chars().collect();
        let mut index = 0;
        while index < characters.len() {
            let rest: String = characters[index..].iter().collect();
            let token = [
                "mmmm", "mmm", "mm", "m", "dddd", "ddd", "dd", "d", "yyyy", "yy",
            ]
            .into_iter()
            .find(|token| rest.starts_with(token));
            let Some(token) = token else {
                out.push(characters[index]);
                index += 1;
                continue;
            };
            match token {
                "mmmm" => out.push_str(&locale.month_name(self.month)),
                "mmm" => out.push_str(&locale.month_abbreviation(self.month)),
                "mm" => out.push_str(&format!("{:02}", self.month)),
                "m" => out.push_str(&self.month.to_string()),
                "dddd" => out.push_str(&locale.weekday_name(self.weekday())),
                "ddd" => out.push_str(&locale.weekday_initial(self.weekday())),
                "dd" => out.push_str(&format!("{:02}", self.day)),
                "d" => out.push_str(&self.day.to_string()),
                "yyyy" => out.push_str(&format!("{:04}", self.year)),
                "yy" => out.push_str(&format!("{:02}", self.year.rem_euclid(100))),
                _ => unreachable!("the token came from the list above"),
            }
            index += token.chars().count();
        }
        // A pattern that named nothing — or an empty one, which is what
        // `AFDate_Format(2)`'s numbered preset comes back as — still has to
        // produce a date the field's own keystroke script will accept. ISO is
        // what PDFium parses whatever the pattern says.
        if out.trim().is_empty() {
            return format!("{:04}-{:02}-{:02}", self.year, self.month, self.day);
        }
        out
    }
}

/// One month, laid out the way a calendar draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarMonth {
    pub year: i32,
    /// 1–12.
    pub month: u32,
}

impl CalendarMonth {
    pub fn new(year: i32, month: u32) -> CalendarMonth {
        CalendarMonth { year, month }.normalised()
    }

    pub fn of(date: Date) -> CalendarMonth {
        CalendarMonth::new(date.year, date.month)
    }

    /// Bring a month back inside 1–12, carrying into the year, so stepping
    /// past December is arithmetic rather than a special case at every call.
    fn normalised(self) -> CalendarMonth {
        let zero = self.month as i64 - 1 + self.year as i64 * 12;
        CalendarMonth {
            year: zero.div_euclid(12) as i32,
            month: zero.rem_euclid(12) as u32 + 1,
        }
    }

    pub fn next(self) -> CalendarMonth {
        CalendarMonth {
            year: self.year,
            month: self.month + 1,
        }
        .normalised()
    }

    pub fn previous(self) -> CalendarMonth {
        CalendarMonth {
            year: self.year,
            month: self.month.wrapping_sub(1),
        }
        .normalised()
    }

    pub fn title(self, locale: Locale) -> String {
        format!("{} {}", locale.month_name(self.month), self.year)
    }

    /// The month as rows of seven, Monday first.
    ///
    /// `None` is a cell before the first or after the last day — the blanks a
    /// calendar leaves at its corners. Always whole weeks, so the grid does
    /// not change width and the rows below it do not move as the month does.
    pub fn grid(self) -> Vec<[Option<u32>; 7]> {
        let first = Date::new(self.year, self.month, 1);
        let lead = first.weekday() as usize;
        let days = Date::days_in_month(self.year, self.month);
        let mut rows = Vec::new();
        let mut row = [None; 7];
        let mut column = lead;
        for day in 1..=days {
            row[column] = Some(day);
            column += 1;
            if column == 7 {
                rows.push(row);
                row = [None; 7];
                column = 0;
            }
        }
        if column != 0 {
            rows.push(row);
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locale(tag: &str) -> Locale {
        Locale::parse(tag).unwrap_or_else(|| panic!("{tag} should have date data"))
    }

    #[test]
    fn a_pattern_is_filled_in_the_readers_own_language() {
        let date = Date::new(2026, 8, 16);
        // The pattern from the form this was written for.
        assert_eq!(date.format("dd mmmm yyyy", locale("fr_CA")), "16 août 2026");
        assert_eq!(
            date.format("dd mmmm yyyy", locale("en_CA")),
            "16 August 2026"
        );
    }

    #[test]
    fn a_language_nobody_wrote_a_table_for_still_works() {
        // The point of taking month names from the system's locale data
        // rather than from a list here: none of these was ever enumerated
        // anywhere in pulpit, and all of them come out right.
        let date = Date::new(2026, 8, 16);
        assert_eq!(
            date.format("dd mmmm yyyy", locale("de_DE")),
            "16 August 2026"
        );
        assert_eq!(
            date.format("dd mmmm yyyy", locale("es_ES")),
            "16 agosto 2026"
        );
        assert_eq!(
            date.format("dd mmmm yyyy", locale("nl_NL")),
            "16 augustus 2026"
        );
        // Not a Latin script, and not a month *name* at all — which is the
        // case a table of translated names would have got wrong.
        assert_eq!(date.format("mmmm", locale("ja_JP")), "8月");
    }

    #[test]
    fn a_bare_language_is_given_a_territory_rather_than_refused() {
        // `LANG=fr` is legal and common, and the locale data is keyed by
        // language *and* territory — so a bare language has to be matched to
        // one, because any territory's month names are that language's.
        assert_eq!(Date::new(2026, 8, 16).format("mmmm", locale("fr")), "août");
    }

    #[test]
    fn a_locale_string_is_read_the_way_the_environment_writes_one() {
        // The encoding and the modifier say nothing about a month's name.
        assert_eq!(Locale::parse("fr_CA.UTF-8"), Locale::parse("fr_CA"));
        assert_eq!(Locale::parse("pt_BR@euro"), Locale::parse("pt_BR"));
        // BCP 47 writes a dash where POSIX writes an underscore, and both
        // turn up in these variables.
        assert_eq!(Locale::parse("fr-FR"), Locale::parse("fr_FR"));
        // Nothing to go on is said so, rather than quietly meaning English.
        assert!(Locale::parse("").is_none());
        assert!(Locale::parse("zzz_ZZ").is_none());
    }

    #[test]
    fn every_token_means_what_acrobat_says_it_means() {
        let date = Date::new(2026, 8, 6);
        let english = locale("en_CA");
        assert_eq!(date.format("d/m/yy", english), "6/8/26");
        assert_eq!(date.format("dd/mm/yyyy", english), "06/08/2026");
        // Longest-first matching: `mmmm` must not be read as `mmm` then `m`.
        assert_eq!(date.format("mmmm", english), "August");
        assert_eq!(date.format("mmm", english), "Aug");
        assert_eq!(date.format("mm", english), "08");
        assert_eq!(date.format("m", english), "8");
    }

    #[test]
    fn literal_text_in_a_pattern_survives_it() {
        let date = Date::new(2026, 8, 16);
        assert_eq!(date.format("yyyy-mm-dd", locale("en_CA")), "2026-08-16");
    }

    #[test]
    fn a_pattern_that_names_nothing_still_produces_a_date() {
        // `AFDate_Format(2)` uses a numbered preset, so pulpit has no pattern
        // to follow — and a picker that wrote an empty string would clear the
        // field it was meant to fill.
        let date = Date::new(2026, 8, 16);
        assert_eq!(date.format("", locale("fr_CA")), "2026-08-16");
        assert_eq!(date.format("   ", locale("fr_CA")), "2026-08-16");
    }

    #[test]
    fn the_weekday_of_a_known_date_is_the_one_the_calendar_has() {
        assert_eq!(Date::new(2026, 8, 16).weekday(), Weekday::Sunday);
        assert_eq!(Date::new(2000, 1, 1).weekday(), Weekday::Saturday);
        assert_eq!(Date::new(1970, 1, 1).weekday(), Weekday::Thursday);
        // A leap day, which is where an off-by-one in the year adjustment
        // shows up.
        assert_eq!(Date::new(2024, 2, 29).weekday(), Weekday::Thursday);
    }

    #[test]
    fn the_representative_dates_really_fall_on_their_weekdays() {
        // The weekday names come from a date chosen to fall on that day, so
        // an off-by-one here would name every column wrong.
        for weekday in Weekday::ALL {
            let date = weekday.representative();
            let mine = Date::new(
                chrono::Datelike::year(&date),
                chrono::Datelike::month(&date),
                chrono::Datelike::day(&date),
            );
            assert_eq!(mine.weekday(), weekday, "{date} is not {weekday:?}");
        }
    }

    #[test]
    fn february_has_the_days_the_gregorian_rule_gives_it() {
        assert_eq!(Date::days_in_month(2024, 2), 29);
        assert_eq!(Date::days_in_month(2025, 2), 28);
        // The two rules that catch a naive "every four years".
        assert_eq!(Date::days_in_month(1900, 2), 28);
        assert_eq!(Date::days_in_month(2000, 2), 29);
    }

    #[test]
    fn a_date_the_calendar_does_not_have_is_not_valid() {
        assert!(Date::new(2026, 8, 16).is_valid());
        assert!(!Date::new(2025, 2, 29).is_valid());
        assert!(!Date::new(2026, 13, 1).is_valid());
        assert!(!Date::new(2026, 0, 1).is_valid());
        assert!(!Date::new(2026, 1, 0).is_valid());
    }

    #[test]
    fn stepping_past_december_carries_into_the_next_year() {
        let december = CalendarMonth::new(2026, 12);
        assert_eq!(december.next(), CalendarMonth::new(2027, 1));
        let january = CalendarMonth::new(2026, 1);
        assert_eq!(january.previous(), CalendarMonth::new(2025, 12));
    }

    #[test]
    fn a_month_is_laid_out_in_whole_weeks_starting_on_monday() {
        // August 2026 begins on a Saturday, so the first row has five blanks.
        let grid = CalendarMonth::new(2026, 8).grid();
        assert_eq!(grid[0][..5], [None; 5]);
        assert_eq!(grid[0][5], Some(1));
        assert_eq!(grid[0][6], Some(2));
        let days: Vec<u32> = grid.iter().flatten().flatten().copied().collect();
        assert_eq!(days.len(), 31);
        assert_eq!(days.first(), Some(&1));
        assert_eq!(days.last(), Some(&31));
    }

    #[test]
    fn a_month_that_begins_on_a_monday_leaves_no_blank_corner() {
        // June 2026 begins on a Monday: the grid must not open with an empty
        // week, which is what an off-by-one in the lead would produce.
        let grid = CalendarMonth::new(2026, 6).grid();
        assert_eq!(grid[0][0], Some(1));
    }

    #[test]
    fn a_calendars_column_headings_are_the_locales_own_short_names() {
        // Not the first letter of the full name: English would give T, T and
        // S, S, and a locale that does not write days as words would give
        // nothing useful at all.
        let english = locale("en_CA");
        let headings: Vec<String> = Weekday::ALL
            .into_iter()
            .map(|day| english.weekday_initial(day))
            .collect();
        assert!(headings.iter().all(|heading| !heading.is_empty()));
        assert_eq!(
            headings
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            7,
            "two columns headed the same is a calendar nobody can read: {headings:?}"
        );
    }
}
