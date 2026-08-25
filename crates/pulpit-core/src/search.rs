//! Finding a string in a document, in any view.
//!
//! Search is one model with several sources. Page text comes from the render
//! worker a chunk of pages at a time, because a five-hundred-page deck must
//! not be scanned inside one IPC round trip; speaker notes and the outline are
//! already in this process and are searched synchronously, which is what makes
//! the box feel instant while the pages are still arriving.
//!
//! Everything here is pure. No PDF library type, no clock read, no UI type:
//! the hard cases — a stale chunk landing after the query changed, a retried
//! chunk arriving twice, a cursor that has to wrap — are ordinary unit tests.

use serde::{Deserialize, Serialize};

use crate::navigation::Outline;
use crate::page::{PageIndex, PageQuad};
use crate::pdfpc::TextNotes;

/// The longest query pulpit will run.
///
/// A query is scanned against every page of the document; an unbounded one is
/// a way to make that scan quadratic in something the user pasted.
pub const MAX_QUERY_CHARS: usize = 128;

/// The most hits one search will accumulate.
///
/// Past this the answer is "refine the query", not a longer list: nobody reads
/// the two-thousandth occurrence of "the", and the hits are held in memory and
/// drawn as overlays on every visible page.
pub const MAX_HITS: usize = 2_048;

/// How many characters of surrounding text a hit carries for its list entry.
pub const CONTEXT_CHARS: usize = 40;

/// How many pages one worker request covers, at most.
///
/// Small enough that the first hits appear while a long document is still
/// being scanned, and that cancelling a superseded query wastes little work;
/// large enough that a hundred-page deck is a handful of round trips.
pub const PAGES_PER_CHUNK: usize = 32;

/// How many pages the *first* request of a scan covers.
///
/// The first chunk decides whether the box feels instant, so it is small:
/// four pages come back in a round trip rather than in a scan. Each
/// subsequent request doubles, up to [`PAGES_PER_CHUNK`], so a long document
/// still costs a handful of trips rather than a hundred.
pub const FIRST_CHUNK_PAGES: usize = 4;

/// How many requests may be outstanding at once.
///
/// One in flight means every chunk pays a full round trip before the next is
/// even asked for, and the scan runs at the latency of the link rather than
/// at the speed of the worker. Three keeps the worker fed without making a
/// superseded query expensive to abandon.
pub const MAX_CHUNKS_IN_FLIGHT: usize = 3;

/// How many pages the request after `issued` earlier ones covers.
fn chunk_pages(issued: usize) -> usize {
    match 1usize.checked_shl(issued.min(16) as u32) {
        Some(factor) => FIRST_CHUNK_PAGES
            .saturating_mul(factor)
            .min(PAGES_PER_CHUNK),
        None => PAGES_PER_CHUNK,
    }
}

/// A monotonically increasing search generation.
///
/// Every change to the query advances it. Chunks carrying an older generation
/// are discarded, which is the same discipline rendering uses and the reason
/// typing in the search box cannot interleave two queries' results.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct SearchGeneration(pub u64);

impl SearchGeneration {
    pub const ZERO: SearchGeneration = SearchGeneration(0);

    pub fn advance(&mut self) -> SearchGeneration {
        self.0 += 1;
        *self
    }

    /// True when `self` is at least as new as `other`.
    pub fn is_current_for(self, other: SearchGeneration) -> bool {
        self.0 >= other.0
    }
}

impl std::fmt::Display for SearchGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "search{}", self.0)
    }
}

/// What the user typed, and how they want it matched.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Query {
    text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Interpret the text as a Rust regular expression rather than literally.
    #[serde(default)]
    pub regex: bool,
}

impl Query {
    /// Build a query, truncating an over-long one rather than refusing it —
    /// the user is typing, and a paste is not an error.
    pub fn new(text: &str, case_sensitive: bool, whole_word: bool) -> Query {
        let text = match text.char_indices().nth(MAX_QUERY_CHARS) {
            Some((at, _)) => text[..at].to_string(),
            None => text.to_string(),
        };
        Query {
            text,
            case_sensitive,
            whole_word,
            regex: false,
        }
    }

    /// Build an explicitly regular-expression query.
    pub fn regex(text: &str, case_sensitive: bool, whole_word: bool) -> Query {
        let mut query = Query::new(text, case_sensitive, whole_word);
        query.regex = true;
        query
    }

    /// Compile a regular expression before a document scan is started.
    pub fn validate(&self) -> Result<(), String> {
        if !self.regex || self.is_empty() {
            return Ok(());
        }
        self.regular_expression()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// A query with nothing to look for. Searching one is not an error; it
    /// simply has no hits, which is the state the box is in before the first
    /// keystroke.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Every match of this query in `haystack`, as `(char offset, char len)`.
    ///
    /// Matches do not overlap: scanning resumes after the previous match, so
    /// "aa" in "aaa" is one hit, not two.
    pub fn matches_in(&self, haystack: &str) -> Vec<TextMatch> {
        self.prepare().matches_in(haystack)
    }

    /// Prepare this query for matching more than one run of text.
    ///
    /// Regex compilation and literal folding happen once here rather than
    /// once per page, note, or bookmark. The prepared value borrows the query
    /// so the serializable protocol type remains small and unsurprising.
    pub fn prepare(&self) -> PreparedQuery<'_> {
        let regex = self.regex.then(|| self.regular_expression().ok()).flatten();
        let needle = (!self.regex && !self.is_empty()).then(|| self.folded(self.text.trim()));
        PreparedQuery {
            query: self,
            regex,
            needle,
        }
    }

    fn regular_expression(&self) -> Result<regex::Regex, regex::Error> {
        let pattern = if self.whole_word {
            format!(r"\b(?:{})\b", self.text.trim())
        } else {
            self.text.trim().to_string()
        };
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .size_limit(1 << 20)
            .build()
    }

    fn folded(&self, text: &str) -> Vec<char> {
        text.chars()
            .map(|c| {
                if self.case_sensitive {
                    c
                } else {
                    // Simple folding: a character that lowercases to several
                    // (German ß, say) keeps its first, which keeps offsets in
                    // the folded string aligned with the original's.
                    c.to_lowercase().next().unwrap_or(c)
                }
            })
            .collect()
    }

    fn word_bounded(&self, hay: &[char], at: usize, len: usize) -> bool {
        if !self.whole_word {
            return true;
        }
        let before = at.checked_sub(1).map(|i| hay[i]);
        let after = hay.get(at + len).copied();
        !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
    }
}

/// A query whose reusable matching work has already been performed.
///
/// Construct this once for a document scan with [`Query::prepare`].
pub struct PreparedQuery<'a> {
    query: &'a Query,
    regex: Option<regex::Regex>,
    needle: Option<Vec<char>>,
}

impl PreparedQuery<'_> {
    pub fn query(&self) -> &Query {
        self.query
    }

    /// Every non-overlapping match in `haystack`, in Rust character offsets.
    pub fn matches_in(&self, haystack: &str) -> Vec<TextMatch> {
        if self.query.is_empty() {
            return Vec::new();
        }
        if self.query.regex {
            let Some(expression) = self.regex.as_ref() else {
                return Vec::new();
            };
            return expression
                .find_iter(haystack)
                .filter(|found| found.start() < found.end())
                .take(MAX_HITS)
                .scan((0, 0), |(previous_end, char_offset), found| {
                    *char_offset += haystack[*previous_end..found.start()].chars().count();
                    let len = haystack[found.start()..found.end()].chars().count();
                    let matched = TextMatch {
                        offset: *char_offset,
                        len,
                    };
                    *char_offset += len;
                    *previous_end = found.end();
                    Some(matched)
                })
                .collect();
        }
        let literal = self.query.text.trim();
        if literal.is_ascii() && haystack.is_ascii() {
            return self.matches_ascii(haystack, literal);
        }
        let needle = self
            .needle
            .as_deref()
            .expect("a non-empty literal query has a prepared needle");
        let hay: Vec<char> = self.query.folded(haystack);
        if needle.is_empty() || hay.len() < needle.len() {
            return Vec::new();
        }

        let mut matches = Vec::new();
        let mut at = 0;
        while at + needle.len() <= hay.len() {
            if hay[at..at + needle.len()] == needle[..]
                && self.query.word_bounded(&hay, at, needle.len())
            {
                matches.push(TextMatch {
                    offset: at,
                    len: needle.len(),
                });
                at += needle.len();
            } else {
                at += 1;
            }
        }
        matches
    }

    /// The overwhelmingly common deck-text case needs neither Unicode
    /// folding nor a character-offset map: in ASCII, byte and character
    /// offsets are identical. Keep the Unicode path above as the authority
    /// for every other input.
    fn matches_ascii(&self, haystack: &str, needle: &str) -> Vec<TextMatch> {
        let hay = haystack.as_bytes();
        let needle = needle.as_bytes();
        let mut matches = Vec::new();
        let mut at = 0;
        while at + needle.len() <= hay.len() {
            let candidate = &hay[at..at + needle.len()];
            let equal = if self.query.case_sensitive {
                candidate == needle
            } else {
                candidate.eq_ignore_ascii_case(needle)
            };
            let bounded = !self.query.whole_word
                || (!at
                    .checked_sub(1)
                    .and_then(|before| hay.get(before))
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    && !hay
                        .get(at + needle.len())
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_'));
            if equal && bounded {
                matches.push(TextMatch {
                    offset: at,
                    len: needle.len(),
                });
                at += needle.len();
            } else {
                at += 1;
            }
        }
        matches
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One match inside a run of text, in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextMatch {
    pub offset: usize,
    pub len: usize,
}

/// Where a hit was found.
///
/// The source decides what activating it does and how it is drawn: a page-text
/// hit has quads to highlight, a notes or outline hit only orders a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HitSource {
    /// The text layer of the page itself.
    PageText,
    /// The speaker notes attached to the page.
    Notes,
    /// A bookmark title pointing at the page.
    Outline,
}

impl HitSource {
    pub fn label(self) -> &'static str {
        match self {
            HitSource::PageText => "page",
            HitSource::Notes => "notes",
            HitSource::Outline => "outline",
        }
    }
}

/// One occurrence of the query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub page: PageIndex,
    pub source: HitSource,
    /// Which occurrence this is within its page and source, counted from
    /// zero. Together with `page` and `source` it identifies the hit, which is
    /// how a chunk that arrives twice is absorbed rather than duplicated.
    pub ordinal: usize,
    /// One quadrilateral per contiguous run of the match, in canonical page
    /// space. Empty for a hit that is not on the page surface.
    pub quads: Vec<PageQuad>,
    /// The match with a little of its surroundings, for the results list.
    pub context: String,
    /// Where the match sits inside `context`, in characters.
    pub highlight: TextMatch,
}

/// Stable identity of one occurrence across incrementally arriving chunks.
pub type HitKey = (PageIndex, HitSource, usize);

/// One character index shared by every hit found in the same run of text.
///
/// Matches use character offsets so snippets stay Unicode-correct. Building
/// that index once prevents a page with many matches from decoding and
/// allocating its complete text once per result.
#[derive(Debug, Clone)]
pub struct IndexedText {
    chars: Vec<char>,
}

impl IndexedText {
    pub fn new(text: &str) -> Self {
        Self {
            chars: text.chars().collect(),
        }
    }

    fn context_window(&self, found: TextMatch) -> (String, TextMatch) {
        context_window(&self.chars, found)
    }
}

impl Hit {
    /// The identity of a hit: what makes two of them the same occurrence.
    pub fn key(&self) -> HitKey {
        (self.page, self.source, self.ordinal)
    }

    /// Build a hit from a match in a run of text, carrying a window of the
    /// surrounding characters as context.
    pub fn from_text(
        page: PageIndex,
        source: HitSource,
        ordinal: usize,
        haystack: &str,
        found: TextMatch,
        quads: Vec<PageQuad>,
    ) -> Hit {
        Self::from_indexed_text(
            page,
            source,
            ordinal,
            &IndexedText::new(haystack),
            found,
            quads,
        )
    }

    /// Build a hit while reusing the character index for its text run.
    pub fn from_indexed_text(
        page: PageIndex,
        source: HitSource,
        ordinal: usize,
        text: &IndexedText,
        found: TextMatch,
        quads: Vec<PageQuad>,
    ) -> Hit {
        let (context, highlight) = text.context_window(found);
        Hit {
            page,
            source,
            ordinal,
            quads,
            context,
            highlight,
        }
    }
}

/// A window of `chars` around `found`, with the match's position inside it.
///
/// Whitespace is collapsed first: a PDF text layer is full of hard newlines
/// that mean nothing, and a results list is one line per hit.
fn context_window(chars: &[char], found: TextMatch) -> (String, TextMatch) {
    let start = found.offset.saturating_sub(CONTEXT_CHARS);
    let end = (found.offset + found.len + CONTEXT_CHARS).min(chars.len());
    let clipped_start = found.offset.min(chars.len());
    let clipped_end = (found.offset + found.len).min(chars.len());

    let mut context = String::new();
    let mut highlight = TextMatch { offset: 0, len: 0 };
    if start > 0 {
        context.push('…');
        highlight.offset += 1;
    }

    let mut written = 0;
    let mut last_was_space = false;
    for (index, c) in chars[start..end].iter().enumerate() {
        let index = start + index;
        let c = if c.is_whitespace() { ' ' } else { *c };
        if c == ' ' && last_was_space {
            // A collapsed run still has to move the highlight along with it.
            if index < clipped_start {
                highlight.offset = highlight.offset.min(context.chars().count());
            }
            continue;
        }
        last_was_space = c == ' ';
        if index == clipped_start {
            highlight.offset = context.chars().count();
        }
        context.push(c);
        if index >= clipped_start && index < clipped_end {
            written += 1;
        }
    }
    highlight.len = written;
    if end < chars.len() {
        context.push('…');
    }
    (context.trim_end().to_string(), highlight)
}

/// One instalment of results for one generation of one query.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HitChunk {
    /// The pages this chunk covers, as a half-open range of physical pages.
    pub from_page: usize,
    pub to_page: usize,
    pub hits: Vec<Hit>,
    /// True when the backend cut the answer short at a protocol bound.
    pub truncated: bool,
}

/// Why a search produced nothing useful, when it is not simply "no matches".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SearchProblem {
    /// The backend cannot read text out of this document at all.
    ///
    /// Distinct from a document with no text layer, and distinct from no
    /// matches: a scanned page and a backend that cannot search must not look
    /// the same to the person typing.
    Unsupported(String),
    /// The regular expression could not be compiled.
    InvalidPattern(String),
    /// The scan stopped early because [`MAX_HITS`] was reached.
    TooManyHits,
    /// The worker failed mid-scan.
    Failed(String),
}

impl std::fmt::Display for SearchProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchProblem::Unsupported(why) => write!(f, "this document cannot be searched: {why}"),
            SearchProblem::InvalidPattern(why) => write!(f, "invalid regular expression: {why}"),
            SearchProblem::TooManyHits => {
                write!(f, "more than {MAX_HITS} matches; narrow the search")
            }
            SearchProblem::Failed(why) => write!(f, "the search failed: {why}"),
        }
    }
}

/// Everything a view needs to show a search, and the only place hits live.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchState {
    query: Query,
    generation: SearchGeneration,
    hits: std::sync::Arc<Vec<Hit>>,
    cursor: Option<usize>,
    /// Where this scan began, and where it will wrap back around to.
    ///
    /// A scan starts at the page the reader is on rather than at the front of
    /// the document: the hit somebody searching from page 300 wants is
    /// usually near page 300, and making them wait for pages 1 to 299 first
    /// is making them wait for the answer they did not ask for.
    start: usize,
    /// The page the *next* scan will begin at, remembered across restarts so
    /// every keystroke of one query scans outwards from the same place.
    origin: usize,
    /// How many pages have been asked about, counted from `start` and
    /// wrapping. Drives what to request next.
    requested: usize,
    /// How many pages have been answered for. Drives progress, and is what
    /// "still scanning" means: a page that has been asked about but not
    /// answered has not been searched.
    scanned: usize,
    /// How many requests this scan has issued, which sets the size of the
    /// next one.
    issued: usize,
    page_count: usize,
    /// Requests that are out, by the page each one starts at.
    in_flight: Vec<usize>,
    problem: Option<SearchProblem>,
}

impl SearchState {
    pub fn new() -> SearchState {
        SearchState::default()
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    pub fn generation(&self) -> SearchGeneration {
        self.generation
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// Immutable, cheap-to-clone view data for responsive UI closures.
    pub fn hits_snapshot(&self) -> std::sync::Arc<Vec<Hit>> {
        self.hits.clone()
    }

    pub fn problem(&self) -> Option<&SearchProblem> {
        self.problem.as_ref()
    }

    /// True while page text is still being scanned, so a view can say "42 so
    /// far" rather than "42".
    pub fn scanning(&self) -> bool {
        self.scanned < self.page_count
    }

    /// How far through the document the scan is, as a fraction.
    pub fn progress(&self) -> f32 {
        if self.page_count == 0 {
            return 1.0;
        }
        (self.scanned as f32 / self.page_count as f32).clamp(0.0, 1.0)
    }

    /// The hit the user is on, if any.
    pub fn current(&self) -> Option<&Hit> {
        self.cursor.and_then(|at| self.hits.get(at))
    }

    /// Where the current hit sits in the list, counted from one, for "3 of 17".
    pub fn position(&self) -> Option<usize> {
        self.cursor.map(|at| at + 1)
    }

    /// Point the search at a document. Any hits from the previous one go.
    pub fn open(&mut self, page_count: usize) -> SearchGeneration {
        self.page_count = page_count;
        self.restart()
    }

    /// Scan outwards from `page` rather than from the front of the document.
    ///
    /// Takes effect at the next restart, so that the page a search was opened
    /// on stays the origin for every keystroke of it rather than following the
    /// cursor as hits are stepped through.
    pub fn begin_at(&mut self, page: PageIndex) {
        self.origin = page.0;
    }

    /// Set the query. Returns the generation the caller must stamp on the
    /// requests it sends; results carrying anything older will be dropped.
    ///
    /// Setting the same query again is a no-op, so a keystroke that does not
    /// change the text — a modifier, a repeated paste — does not restart a
    /// scan that is halfway through a long document.
    pub fn set_query(&mut self, query: Query) -> SearchGeneration {
        if query == self.query {
            return self.generation;
        }
        self.query = query;
        self.restart()
    }

    /// Forget the query and everything found for it.
    pub fn clear(&mut self) -> SearchGeneration {
        self.query = Query::default();
        self.restart()
    }

    fn restart(&mut self) -> SearchGeneration {
        self.hits = std::sync::Arc::new(Vec::new());
        self.cursor = None;
        self.problem = None;
        self.in_flight.clear();
        self.issued = 0;
        self.start = if self.page_count == 0 {
            0
        } else {
            self.origin.min(self.page_count - 1)
        };
        let done = if self.query.is_empty() {
            self.page_count
        } else {
            0
        };
        self.requested = done;
        self.scanned = done;
        self.generation.advance()
    }

    /// Mark the scan finished, whatever is left unasked. Used when the answer
    /// cannot get better: the hit bound was reached, or the worker failed.
    fn finish(&mut self) {
        self.requested = self.page_count;
        self.scanned = self.page_count;
        self.in_flight.clear();
    }

    /// The next range of pages to ask a worker for, if any, marked in flight.
    ///
    /// Returns `None` when every page has been asked about, when the link is
    /// already carrying as much as it should, or when there is nothing to look
    /// for. Call it until it says `None`: several requests may be outstanding,
    /// which is what keeps the worker busy instead of the link.
    pub fn next_request(&mut self) -> Option<(SearchGeneration, std::ops::Range<usize>)> {
        if self.query.is_empty() || self.page_count == 0 {
            return None;
        }
        if self.in_flight.len() >= MAX_CHUNKS_IN_FLIGHT || self.requested >= self.page_count {
            return None;
        }
        if self.hits.len() >= MAX_HITS {
            return None;
        }
        let from = (self.start + self.requested) % self.page_count;
        // A chunk never wraps: the worker is asked for a run of pages, and
        // "300 to 12" is not one. The wrap simply falls on a chunk boundary.
        let pages = chunk_pages(self.issued)
            .min(self.page_count - self.requested)
            .min(self.page_count - from);
        let to = from + pages;
        self.requested += pages;
        self.issued += 1;
        self.in_flight.push(from);
        Some((self.generation, from..to))
    }

    /// Take in one instalment of results.
    ///
    /// Returns false — and changes nothing — when the chunk belongs to a
    /// superseded query. A worker that has not yet noticed a cancellation
    /// keeps answering, and those answers must land nowhere.
    pub fn accept(&mut self, generation: SearchGeneration, chunk: HitChunk) -> bool {
        if generation != self.generation {
            return false;
        }
        self.merge_hits(chunk.hits);
        // Only a chunk that was actually out counts towards the scan: a
        // retried request arriving twice must not report its pages searched
        // twice, or a scan would finish before it had read the document.
        let outstanding = self.in_flight.iter().position(|at| *at == chunk.from_page);
        if let Some(at) = outstanding {
            self.in_flight.remove(at);
            self.scanned = self
                .scanned
                .saturating_add(chunk.to_page.saturating_sub(chunk.from_page))
                .min(self.page_count);
        }
        if chunk.truncated || self.hits.len() >= MAX_HITS {
            self.problem = Some(SearchProblem::TooManyHits);
            self.finish();
        }
        if self.cursor.is_none() && !self.hits.is_empty() {
            self.cursor = Some(0);
        }
        true
    }

    /// Record that the scan cannot proceed. Whatever was found stays: a
    /// partial answer is more use than none, as long as it says it is partial.
    pub fn fail(&mut self, generation: SearchGeneration, problem: SearchProblem) -> bool {
        if generation != self.generation {
            return false;
        }
        self.problem = Some(problem);
        self.finish();
        true
    }

    /// Add hits found in this process — notes, outline — for the current
    /// generation. These arrive before any page text and are held in the same
    /// ordering, so the results list is one list.
    pub fn absorb(&mut self, hits: impl IntoIterator<Item = Hit>) {
        self.merge_hits(hits);
        if self.cursor.is_none() && !self.hits.is_empty() {
            self.cursor = Some(0);
        }
    }

    /// Merge an instalment in document order, replacing repeated hits.
    ///
    /// Page hits interleave with notes already held for later pages. Inserting
    /// them one at a time shifts that tail once per hit; sorting the bounded
    /// instalment and merging shifts each value only once.
    fn merge_hits(&mut self, hits: impl IntoIterator<Item = Hit>) {
        if self.hits.len() >= MAX_HITS {
            return;
        }
        let current = self.current().map(Hit::key);
        let mut incoming: Vec<_> = hits.into_iter().collect();
        incoming.sort_by_key(Hit::key);
        incoming.dedup_by_key(|hit| hit.key());
        if incoming.is_empty() {
            return;
        }

        let held = std::mem::take(&mut self.hits);
        let held = std::sync::Arc::try_unwrap(held).unwrap_or_else(|shared| (*shared).clone());
        let mut held = held.into_iter().peekable();
        let mut incoming = incoming.into_iter().peekable();
        let mut merged = Vec::with_capacity((held.len() + incoming.len()).min(MAX_HITS));
        while merged.len() < MAX_HITS {
            match (held.peek(), incoming.peek()) {
                (Some(old), Some(new)) => match old.key().cmp(&new.key()) {
                    std::cmp::Ordering::Less => {
                        merged.push(held.next().expect("a peeked hit is present"));
                    }
                    std::cmp::Ordering::Equal => {
                        held.next();
                        merged.push(incoming.next().expect("a peeked hit is present"));
                    }
                    std::cmp::Ordering::Greater => {
                        merged.push(incoming.next().expect("a peeked hit is present"));
                    }
                },
                (Some(_), None) => {
                    merged.extend(held.by_ref().take(MAX_HITS - merged.len()));
                }
                (None, Some(_)) => {
                    merged.extend(incoming.by_ref().take(MAX_HITS - merged.len()));
                }
                (None, None) => break,
            }
        }
        self.hits = std::sync::Arc::new(merged);
        self.cursor =
            current.and_then(|key| self.hits.binary_search_by(|hit| hit.key().cmp(&key)).ok());
    }

    /// Move to the next hit, wrapping at the end. Wrapping rather than
    /// stopping because the scan may still be running: "no next hit" would be
    /// a lie about a document that is still being read.
    pub fn advance(&mut self) -> Option<&Hit> {
        if self.hits.is_empty() {
            self.cursor = None;
            return None;
        }
        self.cursor = Some(match self.cursor {
            Some(at) => (at + 1) % self.hits.len(),
            None => 0,
        });
        self.current()
    }

    /// Move to the previous hit, wrapping at the start.
    pub fn retreat(&mut self) -> Option<&Hit> {
        if self.hits.is_empty() {
            self.cursor = None;
            return None;
        }
        self.cursor = Some(match self.cursor {
            Some(at) => (at + self.hits.len() - 1) % self.hits.len(),
            None => self.hits.len() - 1,
        });
        self.current()
    }

    /// Put the cursor on the first hit at or after `page`, so that opening the
    /// search from page 40 starts there rather than at the top of the document.
    /// Falls back to the first hit when everything found is before `page`.
    pub fn focus_near(&mut self, page: PageIndex) -> Option<&Hit> {
        if self.hits.is_empty() {
            self.cursor = None;
            return None;
        }
        let at = self
            .hits
            .iter()
            .position(|hit| hit.page >= page)
            .unwrap_or(0);
        self.cursor = Some(at);
        self.current()
    }

    /// Put the cursor on a particular hit, for a click in the results list.
    pub fn focus(&mut self, index: usize) -> Option<&Hit> {
        if index >= self.hits.len() {
            return None;
        }
        self.cursor = Some(index);
        self.current()
    }

    /// Focus an occurrence by identity rather than by its current position in
    /// a list that may still be receiving earlier hits.
    pub fn focus_key(&mut self, key: HitKey) -> Option<&Hit> {
        let index = self.hits.binary_search_by_key(&key, Hit::key).ok()?;
        self.cursor = Some(index);
        self.current()
    }

    /// The hits to draw on one page, for the overlay.
    pub fn hits_on(&self, page: PageIndex) -> impl Iterator<Item = &Hit> {
        self.hits
            .iter()
            .filter(move |hit| hit.page == page && !hit.quads.is_empty())
    }
}

/// Search the speaker notes, which are already in this process.
///
/// In the presenter this is often the more useful half: "which slide was the
/// one about X" is usually answered by what the speaker wrote, not by what is
/// printed on the slide.
pub fn search_notes(query: &Query, notes: &TextNotes, page_count: usize) -> Vec<Hit> {
    let mut hits = Vec::new();
    let prepared = query.prepare();
    for page in 0..page_count {
        let Some(text) = notes.for_page(page) else {
            continue;
        };
        let indexed = IndexedText::new(text);
        for (ordinal, found) in prepared.matches_in(text).into_iter().enumerate() {
            hits.push(Hit::from_indexed_text(
                PageIndex(page),
                HitSource::Notes,
                ordinal,
                &indexed,
                found,
                Vec::new(),
            ));
            if hits.len() >= MAX_HITS {
                return hits;
            }
        }
    }
    hits
}

/// Search bookmark titles. A hit orders the page the bookmark points at;
/// bookmarks that leave the document order nothing and are skipped.
pub fn search_outline(query: &Query, outline: &Outline) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    let mut ordinals: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let prepared = query.prepare();
    for entry in outline.flattened() {
        let Some(page) = entry.page() else {
            continue;
        };
        let indexed = IndexedText::new(&entry.title);
        for found in prepared.matches_in(&entry.title) {
            let ordinal = ordinals.entry(page).or_default();
            hits.push(Hit::from_indexed_text(
                PageIndex(page),
                HitSource::Outline,
                *ordinal,
                &indexed,
                found,
                Vec::new(),
            ));
            *ordinal += 1;
            if hits.len() >= MAX_HITS {
                return hits;
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::{PageQuad as Quad, PageRect};

    fn query(text: &str) -> Query {
        Query::new(text, false, false)
    }

    fn page_hit(page: usize, ordinal: usize) -> Hit {
        Hit {
            page: PageIndex(page),
            source: HitSource::PageText,
            ordinal,
            quads: vec![Quad::from_rect(PageRect::new(0.0, 0.0, 10.0, 10.0))],
            context: "context".into(),
            highlight: TextMatch { offset: 0, len: 3 },
        }
    }

    #[test]
    fn matching_is_case_insensitive_by_default() {
        assert_eq!(query("pdf").matches_in("A PDF and a pdf").len(), 2);
        assert_eq!(
            Query::new("pdf", true, false)
                .matches_in("A PDF and a pdf")
                .len(),
            1
        );
    }

    #[test]
    fn ascii_fast_path_keeps_offsets_case_and_word_boundaries() {
        let insensitive_query = Query::new("FORM", false, true);
        let insensitive = insensitive_query.prepare();
        assert_eq!(
            insensitive.matches_in("a form, performance; FORM"),
            [
                TextMatch { offset: 2, len: 4 },
                TextMatch { offset: 21, len: 4 },
            ]
        );

        let sensitive_query = Query::new("FORM", true, false);
        let sensitive = sensitive_query.prepare();
        assert_eq!(
            sensitive.matches_in("form FORM"),
            [TextMatch { offset: 5, len: 4 }]
        );
    }

    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(query("aa").matches_in("aaa").len(), 1);
    }

    #[test]
    fn whole_word_needs_boundaries() {
        let whole = Query::new("form", false, true);
        assert_eq!(whole.matches_in("a form here").len(), 1);
        assert_eq!(whole.matches_in("performance").len(), 0);
        assert_eq!(whole.matches_in("(form)").len(), 1);
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert!(query("").matches_in("anything").is_empty());
        assert!(query("   ").matches_in("anything").is_empty());
        assert!(query("").is_empty());
    }

    #[test]
    fn regular_expressions_match_notes_and_page_text_with_character_offsets() {
        let expression = Query::regex(r"colou?r\s+theory", false, false);
        assert_eq!(
            expression.matches_in("color theory and colour theory"),
            vec![
                TextMatch { offset: 0, len: 12 },
                TextMatch {
                    offset: 17,
                    len: 13
                },
            ]
        );
    }

    #[test]
    fn a_prepared_query_has_the_same_matches_across_many_texts() {
        let queries = [
            Query::new("PDF", false, false),
            Query::new("form", true, true),
            Query::regex(r"colou?r", false, false),
            Query::regex(r"\p{Greek}+", true, true),
        ];
        let texts = [
            "A PDF and a pdf",
            "a form, but not performance",
            "Color, colour, and COLOR",
            "Latin Ελληνικά punctuation",
            "",
        ];

        for query in queries {
            let prepared = query.prepare();
            for text in texts {
                assert_eq!(prepared.matches_in(text), query.matches_in(text));
            }
        }
    }

    #[test]
    fn an_invalid_regular_expression_is_reported_before_scanning() {
        assert!(Query::regex("[unfinished", false, false)
            .validate()
            .is_err());
    }

    #[test]
    fn a_long_query_is_truncated_not_refused() {
        let long = "x".repeat(MAX_QUERY_CHARS * 2);
        assert_eq!(query(&long).text().chars().count(), MAX_QUERY_CHARS);
    }

    #[test]
    fn context_collapses_whitespace_and_marks_the_match() {
        let hay = "the   quick\nbrown fox";
        let found = query("brown").matches_in(hay)[0];
        let hit = Hit::from_text(PageIndex(0), HitSource::PageText, 0, hay, found, Vec::new());
        assert_eq!(hit.context, "the quick brown fox");
        let marked: String = hit
            .context
            .chars()
            .skip(hit.highlight.offset)
            .take(hit.highlight.len)
            .collect();
        assert_eq!(marked, "brown");
    }

    #[test]
    fn context_is_windowed_with_ellipses() {
        let hay = format!("{}needle{}", "a".repeat(200), "b".repeat(200));
        let found = query("needle").matches_in(&hay)[0];
        let hit = Hit::from_text(
            PageIndex(0),
            HitSource::PageText,
            0,
            &hay,
            found,
            Vec::new(),
        );
        assert!(hit.context.starts_with('…'));
        assert!(hit.context.ends_with('…'));
        let marked: String = hit
            .context
            .chars()
            .skip(hit.highlight.offset)
            .take(hit.highlight.len)
            .collect();
        assert_eq!(marked, "needle");
    }

    #[test]
    fn one_character_index_builds_unicode_correct_contexts_for_many_hits() {
        let hay = "préface needle\nμετά needle conclusion";
        let found = query("needle").matches_in(hay);
        let indexed = IndexedText::new(hay);

        let hits: Vec<_> = found
            .iter()
            .copied()
            .enumerate()
            .map(|(ordinal, found)| {
                Hit::from_indexed_text(
                    PageIndex(0),
                    HitSource::PageText,
                    ordinal,
                    &indexed,
                    found,
                    Vec::new(),
                )
            })
            .collect();

        assert_eq!(hits.len(), 2);
        for hit in hits {
            let marked: String = hit
                .context
                .chars()
                .skip(hit.highlight.offset)
                .take(hit.highlight.len)
                .collect();
            assert_eq!(marked, "needle");
        }
    }

    #[test]
    fn a_stale_chunk_lands_nowhere() {
        let mut state = SearchState::new();
        state.open(100);
        let old = state.set_query(query("first"));
        let new = state.set_query(query("second"));
        assert_ne!(old, new);

        let chunk = HitChunk {
            from_page: 0,
            to_page: 32,
            hits: vec![page_hit(1, 0)],
            truncated: false,
        };
        assert!(!state.accept(old, chunk.clone()));
        assert!(state.hits().is_empty());
        assert!(state.accept(new, chunk));
        assert_eq!(state.hits().len(), 1);
    }

    #[test]
    fn setting_the_same_query_does_not_restart_the_scan() {
        let mut state = SearchState::new();
        state.open(100);
        let first = state.set_query(query("pdf"));
        state.accept(
            first,
            HitChunk {
                from_page: 0,
                to_page: 32,
                hits: vec![page_hit(1, 0)],
                truncated: false,
            },
        );
        let again = state.set_query(query("pdf"));
        assert_eq!(first, again);
        assert_eq!(state.hits().len(), 1);
    }

    #[test]
    fn a_repeated_chunk_is_absorbed_not_duplicated() {
        let mut state = SearchState::new();
        state.open(100);
        let generation = state.set_query(query("pdf"));
        let chunk = HitChunk {
            from_page: 0,
            to_page: 32,
            hits: vec![page_hit(1, 0), page_hit(1, 1)],
            truncated: false,
        };
        assert!(state.accept(generation, chunk.clone()));
        assert!(state.accept(generation, chunk));
        assert_eq!(state.hits().len(), 2);
    }

    #[test]
    fn hits_are_held_in_document_order_however_they_arrive() {
        let mut state = SearchState::new();
        state.open(100);
        let generation = state.set_query(query("pdf"));
        state.accept(
            generation,
            HitChunk {
                from_page: 32,
                to_page: 64,
                hits: vec![page_hit(40, 0)],
                truncated: false,
            },
        );
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 32,
                hits: vec![page_hit(2, 0)],
                truncated: false,
            },
        );
        let pages: Vec<usize> = state.hits().iter().map(|h| h.page.get()).collect();
        assert_eq!(pages, vec![2, 40]);
    }

    #[test]
    fn the_cursor_follows_its_hit_when_earlier_ones_arrive() {
        let mut state = SearchState::new();
        state.open(100);
        let generation = state.set_query(query("pdf"));
        state.accept(
            generation,
            HitChunk {
                from_page: 32,
                to_page: 64,
                hits: vec![page_hit(40, 0)],
                truncated: false,
            },
        );
        assert_eq!(state.current().unwrap().page, PageIndex(40));
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 32,
                hits: vec![page_hit(2, 0)],
                truncated: false,
            },
        );
        assert_eq!(state.current().unwrap().page, PageIndex(40));
        assert_eq!(state.position(), Some(2));
    }

    #[test]
    fn a_result_can_be_focused_by_stable_identity() {
        let mut state = SearchState::new();
        state.open(100);
        let generation = state.set_query(query("pdf"));
        let wanted = page_hit(40, 2);
        let key = wanted.key();
        state.accept(
            generation,
            HitChunk {
                from_page: 32,
                to_page: 64,
                hits: vec![wanted],
                truncated: false,
            },
        );
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 32,
                hits: vec![page_hit(2, 0)],
                truncated: false,
            },
        );

        assert_eq!(state.focus_key(key).map(Hit::key), Some(key));
        assert_eq!(state.position(), Some(2));
    }

    #[test]
    fn the_cursor_wraps_in_both_directions() {
        let mut state = SearchState::new();
        state.open(10);
        let generation = state.set_query(query("pdf"));
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 10,
                hits: vec![page_hit(1, 0), page_hit(2, 0)],
                truncated: false,
            },
        );
        assert_eq!(state.position(), Some(1));
        assert_eq!(state.advance().unwrap().page, PageIndex(2));
        assert_eq!(state.advance().unwrap().page, PageIndex(1));
        assert_eq!(state.retreat().unwrap().page, PageIndex(2));
    }

    #[test]
    fn focus_near_starts_where_the_reader_is() {
        let mut state = SearchState::new();
        state.open(100);
        let generation = state.set_query(query("pdf"));
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 100,
                hits: vec![page_hit(1, 0), page_hit(50, 0), page_hit(80, 0)],
                truncated: false,
            },
        );
        assert_eq!(state.focus_near(PageIndex(40)).unwrap().page, PageIndex(50));
        // Everything found is behind the reader: wrap to the top.
        assert_eq!(state.focus_near(PageIndex(90)).unwrap().page, PageIndex(1));
    }

    #[test]
    fn requests_start_small_and_grow_so_the_first_hits_arrive_first() {
        let mut state = SearchState::new();
        state.open(200);
        let generation = state.set_query(query("pdf"));

        let (gen, first) = state.next_request().unwrap();
        assert_eq!(gen, generation);
        assert_eq!(first, 0..FIRST_CHUNK_PAGES, "the first trip is a short one");
        assert_eq!(
            state.next_request().unwrap().1,
            FIRST_CHUNK_PAGES..FIRST_CHUNK_PAGES * 3,
            "the second doubles"
        );
        assert_eq!(
            state.next_request().unwrap().1,
            FIRST_CHUNK_PAGES * 3..FIRST_CHUNK_PAGES * 7
        );
        // Three is as many as the link carries at once.
        assert!(state.next_request().is_none());

        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: FIRST_CHUNK_PAGES,
                hits: Vec::new(),
                truncated: false,
            },
        );
        assert!(
            state.next_request().is_some(),
            "answering one frees the slot for the next"
        );
    }

    #[test]
    fn a_scan_covers_every_page_exactly_once() {
        for pages in [1usize, 7, 70, 200] {
            for origin in [0usize, 1, 3, 50] {
                if origin >= pages {
                    continue;
                }
                let mut state = SearchState::new();
                state.open(pages);
                state.begin_at(PageIndex(origin));
                let generation = state.set_query(query("pdf"));

                let mut covered = vec![0usize; pages];
                let mut guard = 0;
                while state.scanning() {
                    guard += 1;
                    assert!(guard < 1_000, "the scan of {pages} pages did not converge");
                    let mut answered = false;
                    while let Some((_, range)) = state.next_request() {
                        assert!(range.start < range.end, "an empty request asks nothing");
                        assert!(range.end <= pages, "a request walked off the document");
                        for page in range.clone() {
                            covered[page] += 1;
                        }
                        state.accept(
                            generation,
                            HitChunk {
                                from_page: range.start,
                                to_page: range.end,
                                hits: Vec::new(),
                                truncated: false,
                            },
                        );
                        answered = true;
                    }
                    assert!(answered, "the scan stalled with pages left to read");
                }
                assert!(
                    covered.iter().all(|times| *times == 1),
                    "{pages} pages from {origin} were covered {covered:?}"
                );
                assert!(state.next_request().is_none());
                assert_eq!(state.progress(), 1.0);
            }
        }
    }

    #[test]
    fn a_scan_begins_at_the_page_the_reader_is_on_and_wraps() {
        let mut state = SearchState::new();
        state.open(100);
        state.begin_at(PageIndex(60));
        state.set_query(query("pdf"));
        assert_eq!(state.next_request().unwrap().1.start, 60);

        // …and the origin outlives the restart a keystroke causes, so the
        // second letter does not scan from somewhere else.
        state.set_query(query("pdfium"));
        assert_eq!(state.next_request().unwrap().1.start, 60);
    }

    #[test]
    fn a_chunk_that_arrives_twice_does_not_count_its_pages_twice() {
        let mut state = SearchState::new();
        state.open(100);
        let generation = state.set_query(query("pdf"));
        let (_, range) = state.next_request().unwrap();
        let chunk = HitChunk {
            from_page: range.start,
            to_page: range.end,
            hits: Vec::new(),
            truncated: false,
        };
        assert!(state.accept(generation, chunk.clone()));
        let after = state.progress();
        assert!(state.accept(generation, chunk));
        assert_eq!(state.progress(), after, "a retry re-read no pages");
    }

    #[test]
    fn an_empty_query_asks_for_nothing() {
        let mut state = SearchState::new();
        state.open(100);
        state.set_query(query(""));
        assert!(state.next_request().is_none());
        assert!(!state.scanning());
    }

    #[test]
    fn a_failed_scan_keeps_what_it_found_and_says_so() {
        let mut state = SearchState::new();
        state.open(100);
        let generation = state.set_query(query("pdf"));
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 32,
                hits: vec![page_hit(1, 0)],
                truncated: false,
            },
        );
        assert!(state.fail(generation, SearchProblem::Failed("worker died".into())));
        assert_eq!(state.hits().len(), 1);
        assert!(!state.scanning());
        assert!(matches!(state.problem(), Some(SearchProblem::Failed(_))));
    }

    #[test]
    fn an_unsupported_backend_is_not_no_matches() {
        let mut state = SearchState::new();
        state.open(10);
        let generation = state.set_query(query("pdf"));
        state.fail(
            generation,
            SearchProblem::Unsupported("the fixture backend has no text layer".into()),
        );
        assert!(state.hits().is_empty());
        assert!(state.problem().is_some());
    }

    #[test]
    fn too_many_hits_stops_the_scan() {
        let mut state = SearchState::new();
        state.open(10_000);
        let generation = state.set_query(query("the"));
        let hits = (0..MAX_HITS + 10).map(|i| page_hit(i / 4, i % 4)).collect();
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 32,
                hits,
                truncated: false,
            },
        );
        assert_eq!(state.hits().len(), MAX_HITS);
        assert!(!state.scanning());
        assert_eq!(state.problem(), Some(&SearchProblem::TooManyHits));
    }

    #[test]
    fn overlay_hits_are_only_the_ones_on_that_page_with_geometry() {
        let mut state = SearchState::new();
        state.open(10);
        let generation = state.set_query(query("pdf"));
        let mut noteless = page_hit(1, 1);
        noteless.source = HitSource::Notes;
        noteless.quads.clear();
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 10,
                hits: vec![page_hit(1, 0), noteless, page_hit(2, 0)],
                truncated: false,
            },
        );
        assert_eq!(state.hits_on(PageIndex(1)).count(), 1);
    }

    #[test]
    fn notes_are_searched_in_process() {
        let notes = TextNotes::parse("[notes]\n### 1\nOn regression\n### 2\nOn matching\n")
            .expect("pdfpc notes");
        let hits = search_notes(&query("regression"), &notes, 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page, PageIndex(0));
        assert!(hits.iter().all(|h| h.source == HitSource::Notes));
        assert!(hits.iter().all(|h| h.quads.is_empty()));
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut state = SearchState::new();
        state.open(10);
        let generation = state.set_query(query("pdf"));
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: 10,
                hits: vec![page_hit(1, 0)],
                truncated: false,
            },
        );
        let after = state.clear();
        assert!(state.hits().is_empty());
        assert!(state.current().is_none());
        assert!(state.query().is_empty());
        assert!(after.is_current_for(generation) && after != generation);
    }
}
