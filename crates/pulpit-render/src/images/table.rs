//! The page table of an image directory: which files are pages, in what
//! order, and a digest that says whether the directory has moved since.
//!
//! `SPEC-images.md` §40, §41 and §42. Everything here is pure apart from the
//! one `read_dir` in [`list_directory`], which is what lets the application
//! and the renderer worker derive the *same* table independently (§42.2)
//! without either sending the other a four-thousand-entry list.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use pulpit_core::document::MAX_TRACKED_PAGE_SIZES;

/// Every extension an image document may be built from (§41.2).
///
/// **The one definition.** The page table, the watcher's filename predicate
/// and the file-dialog filters all read this constant rather than restating
/// it; three hand-maintained copies would drift, and the symptom — a file
/// that appears in the contact sheet but never triggers a reload — is
/// invisible until it matters (§41.5).
///
/// SVG is deliberately absent: it is vector content needing a full renderer,
/// which is a different decision with a different dependency (§41.4).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "qoi", "tga", "ico", "pnm", "pgm",
    "ppm", "pbm",
];

/// Is this the name of a file an image document can carry?
///
/// Decided by **extension alone, never by sniffing content** (§41.1). Listing
/// a directory must be cheap, and must not depend on whether a file happens
/// to be readable at the instant it is listed — a half-copied photograph is
/// still page 7.
pub fn is_supported_image(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(OsStr::to_str) else {
        return false;
    };
    IMAGE_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

/// Why a directory could not become a document.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error("cannot list {path}: {reason}")]
    Unreadable { path: String, reason: String },
    /// §40.5. Truncating would silently drop pages and sampling would answer
    /// with a confident wrong aspect ratio (§46.3), so the whole directory is
    /// refused and the count is named.
    #[error(
        "{path} holds {count} images, more than the {MAX_TRACKED_PAGE_SIZES} \
         a single document can track"
    )]
    TooManyImages { path: String, count: usize },
}

/// One page of an image document, as the filesystem describes it.
///
/// `len` and `modified` are only ever fed to the digest: nothing decides
/// anything from them directly, so a filesystem with a coarse mtime degrades
/// into "the digest moves less often", never into a wrong answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEntry {
    pub name: OsString,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// The ordered pages of one image directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTable {
    directory: PathBuf,
    entries: Vec<ImageEntry>,
    digest: u64,
}

impl PageTable {
    /// Build a table from entries that are already in the order they should
    /// be presented in. Only [`list_directory`] and tests construct one.
    pub fn from_entries(directory: impl Into<PathBuf>, mut entries: Vec<ImageEntry>) -> PageTable {
        entries.sort_by(|a, b| page_order(&a.name, &b.name));
        let digest = digest_of(&entries);
        PageTable {
            directory: directory.into(),
            entries,
            digest,
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[ImageEntry] {
        &self.entries
    }

    /// The name of one page, which is its identity across a reload (§43.3).
    pub fn name(&self, page: usize) -> Option<&OsStr> {
        self.entries.get(page).map(|entry| entry.name.as_os_str())
    }

    /// Where a page's file is.
    pub fn path(&self, page: usize) -> Option<PathBuf> {
        self.name(page).map(|name| self.directory.join(name))
    }

    /// The index a name sits at, or `None` when the directory no longer holds
    /// it.
    pub fn index_of(&self, name: &OsStr) -> Option<usize> {
        self.entries.iter().position(|entry| entry.name == name)
    }

    /// A digest over the ordered `(name, len, mtime)` triples (§42.3).
    ///
    /// Two independent listings of a directory can disagree, because the
    /// directory can change between them. Comparing digests is what makes
    /// that disagreement *detectable*, so the recovery is the ordinary
    /// candidate/promote path rather than a silently mismatched table putting
    /// the wrong picture on the projector.
    pub fn digest(&self) -> u64 {
        self.digest
    }

    /// Translate a position from an older table to this one by **name**
    /// (§43.3).
    ///
    /// A missing name falls to the nearest surviving neighbour by sort
    /// position rather than to page 0 (§43.5): deleting the picture on screen
    /// should advance to the next one, which is what a presenter expects and
    /// what an index clamp cannot express.
    pub fn reindex_from(&self, previous: &PageTable, position: usize) -> usize {
        reindex(&previous.names(), &self.names(), position)
    }

    fn names(&self) -> Vec<OsString> {
        self.entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }
}

/// The name-anchored translation of §43.3, over two orderings of names.
///
/// Split out from [`PageTable`] so it is an ordinary unit test over two
/// vectors of names — `pulpit-core` never learns what a file name is (§43.4),
/// and `replace_document` keeps its index semantics unchanged.
pub fn reindex(previous: &[OsString], current: &[OsString], position: usize) -> usize {
    if current.is_empty() {
        return 0;
    }
    let Some(name) = previous.get(position) else {
        // Nothing to anchor on: clamp, which is what an index alone can say.
        return position.min(current.len() - 1);
    };
    if let Some(index) = current.iter().position(|candidate| candidate == name) {
        return index;
    }
    // The file is gone. Its neighbours in the *old* order are the nearest
    // surviving pages by sort position, so walk outwards from where it was:
    // forwards first, because deleting the picture on screen should advance
    // to the next one.
    for distance in 1..=previous.len() {
        if let Some(after) = previous.get(position + distance) {
            if let Some(index) = current.iter().position(|candidate| candidate == after) {
                return index;
            }
        }
        if let Some(before) = position
            .checked_sub(distance)
            .and_then(|index| previous.get(index))
        {
            if let Some(index) = current.iter().position(|candidate| candidate == before) {
                return index;
            }
        }
    }
    // Nothing that was there is there any more: this is a different folder in
    // the same place, and a clamp is the only honest answer left.
    position.min(current.len() - 1)
}

/// List a directory as a page table.
///
/// **Non-recursively**: subdirectories are ignored (§40.1). `readdir` order
/// never reaches the table — it is stable neither across platforms nor across
/// runs, and page identity depends on the order being reproducible (§40.4).
pub fn list_directory(directory: &Path) -> Result<PageTable, ListError> {
    let entries = read_entries(directory)?;
    if entries.len() > MAX_TRACKED_PAGE_SIZES {
        return Err(ListError::TooManyImages {
            path: directory.display().to_string(),
            count: entries.len(),
        });
    }
    Ok(PageTable::from_entries(directory, entries))
}

/// The count and digest of a directory, **without** the §40.5 cap.
///
/// For the stability probe alone. A directory over the cap is still a
/// directory that settles and stops settling, and the probe answering "no
/// stamp" for one would retry for ever without ever reaching the open that
/// refuses it by name. Refusal belongs to the open, which is where somebody
/// is told the count.
pub fn directory_stamp(directory: &Path) -> Option<(usize, u64)> {
    let mut entries = read_entries(directory).ok()?;
    entries.sort_by(|a, b| page_order(&a.name, &b.name));
    Some((entries.len(), digest_of(&entries)))
}

fn read_entries(directory: &Path) -> Result<Vec<ImageEntry>, ListError> {
    let reader = std::fs::read_dir(directory).map_err(|e| ListError::Unreadable {
        path: directory.display().to_string(),
        reason: e.to_string(),
    })?;
    let mut entries = Vec::new();
    for entry in reader {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        if !is_supported_image(Path::new(&name)) {
            continue;
        }
        // A supported extension on a directory is not a page. Followed
        // through symlinks on purpose: a folder of links into a photo library
        // is an ordinary way to assemble a talk, and the link's own size and
        // mtime describe nothing anybody cares about.
        let metadata = match std::fs::metadata(entry.path()) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(metadata) => Some(metadata),
            // Unreadable *right now* is not the same as absent: the file is
            // still a page, it will simply fail its own render (§49).
            Err(_) => None,
        };
        entries.push(ImageEntry {
            name,
            len: metadata.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: metadata.as_ref().and_then(|m| m.modified().ok()),
        });
    }
    Ok(entries)
}

/// What opening `path` means for an image document (§40.1, §40.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    /// The directory that *is* the document.
    pub directory: PathBuf,
    /// The file the presenter actually picked, when they picked a file rather
    /// than a folder. The initial committed page, and the reason §40.3 makes
    /// the resolution visible before any navigation happens.
    pub picked: Option<OsString>,
}

impl ResolvedSource {
    /// True when the document is larger than what the presenter asked for.
    pub fn was_widened(&self) -> bool {
        self.picked.is_some()
    }
}

/// Does this path open as an image document, and as which directory?
///
/// A directory is itself the document. An image *file* resolves to its parent
/// directory with that file as the initial page, which is what makes "open a
/// screenshot, get an image viewer" work — and what §40.3 then has to say out
/// loud. Anything else is not an image document.
pub fn resolve_source(path: &Path) -> Option<ResolvedSource> {
    if path.is_dir() {
        return Some(ResolvedSource {
            directory: path.to_path_buf(),
            picked: None,
        });
    }
    if !is_supported_image(path) {
        return None;
    }
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Some(ResolvedSource {
        directory,
        picked: Some(path.file_name()?.to_os_string()),
    })
}

/// Deterministic natural order over two file names (§40.4).
///
/// `img2` before `img10`, with case folded for comparison and the raw name
/// breaking ties — so `A.png` and `a.png` are ordered, and ordered the same
/// way on every platform and every run.
pub fn page_order(a: &OsStr, b: &OsStr) -> std::cmp::Ordering {
    natural_cmp(&a.to_string_lossy(), &b.to_string_lossy())
        .then_with(|| a.as_encoded_bytes().cmp(b.as_encoded_bytes()))
}

/// Natural, case-folded comparison of two strings.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let one = take_digits(&mut left);
                let two = take_digits(&mut right);
                // Value first — `2` before `10` — then the written form, so
                // `01` and `1` still have a defined order.
                let value = one
                    .trim_start_matches('0')
                    .len()
                    .cmp(&two.trim_start_matches('0').len())
                    .then_with(|| one.trim_start_matches('0').cmp(two.trim_start_matches('0')))
                    .then_with(|| one.len().cmp(&two.len()));
                if value != Ordering::Equal {
                    return value;
                }
            }
            (Some(x), Some(y)) => {
                let folded = fold(x).cmp(&fold(y));
                if folded != Ordering::Equal {
                    return folded;
                }
                left.next();
                right.next();
            }
        }
    }
}

fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut run = String::new();
    while chars.peek().is_some_and(char::is_ascii_digit) {
        run.push(chars.next().expect("peeked"));
    }
    run
}

/// One folded character. `char::to_lowercase` can expand, so the first
/// character of the expansion is used: the raw-name tie-break in
/// [`page_order`] is what makes the order total, not this.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// FNV-1a over the ordered `(name, len, mtime)` triples.
///
/// Written out rather than taken from `DefaultHasher`, whose output std does
/// not promise to keep stable: the application and the worker compare digests
/// across a process boundary, and across a version upgrade one side may be
/// the old binary for the length of a restart.
fn digest_of(entries: &[ImageEntry]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    for entry in entries {
        eat(entry.name.as_encoded_bytes());
        eat(&[0]);
        eat(&entry.len.to_le_bytes());
        let stamp = entry
            .modified
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|since| since.as_nanos() as u64)
            .unwrap_or(0);
        eat(&stamp.to_le_bytes());
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(name: &str, len: u64) -> ImageEntry {
        ImageEntry {
            name: OsString::from(name),
            len,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        }
    }

    fn names(names: &[&str]) -> Vec<OsString> {
        names.iter().map(OsString::from).collect()
    }

    // §51.1
    #[test]
    fn ten_sorts_after_two() {
        let table = PageTable::from_entries(
            "/pictures",
            vec![
                entry("img10.png", 1),
                entry("img2.png", 1),
                entry("img1.png", 1),
            ],
        );
        let order: Vec<_> = table
            .entries()
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(order, ["img1.png", "img2.png", "img10.png"]);
    }

    #[test]
    fn case_is_folded_for_comparison_and_the_raw_name_breaks_the_tie() {
        let table = PageTable::from_entries(
            "/pictures",
            vec![entry("b.png", 1), entry("A.png", 1), entry("a.png", 1)],
        );
        let order: Vec<_> = table
            .entries()
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            order,
            ["A.png", "a.png", "b.png"],
            "case folding orders a before b; the raw name separates A from a"
        );
    }

    #[test]
    fn leading_zeros_do_not_change_the_number_but_do_break_the_tie() {
        assert_eq!(natural_cmp("img007", "img7"), std::cmp::Ordering::Greater);
        assert_eq!(natural_cmp("img007", "img8"), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp("img20", "img100"), std::cmp::Ordering::Less);
    }

    #[test]
    fn ordering_is_total_and_the_same_however_the_input_arrives() {
        let one = PageTable::from_entries(
            "/p",
            vec![entry("c.png", 1), entry("a.png", 1), entry("b.png", 1)],
        );
        let two = PageTable::from_entries(
            "/p",
            vec![entry("b.png", 1), entry("c.png", 1), entry("a.png", 1)],
        );
        assert_eq!(one, two, "readdir order must not reach the page table");
    }

    // §51.2
    #[test]
    fn the_digest_is_stable_for_the_same_directory() {
        let one = PageTable::from_entries("/p", vec![entry("a.png", 10), entry("b.png", 20)]);
        let two = PageTable::from_entries("/p", vec![entry("b.png", 20), entry("a.png", 10)]);
        assert_eq!(one.digest(), two.digest());
    }

    /// The regression §44.2 exists for: a directory's mtime does *not* move
    /// when a member is overwritten in place, so a probe that watched only
    /// the directory would never notice an export rewriting `slide03.png`.
    #[test]
    fn a_member_overwritten_in_place_moves_the_digest() {
        let before = PageTable::from_entries("/p", vec![entry("a.png", 10), entry("b.png", 20)]);

        let mut rewritten = entry("b.png", 20);
        rewritten.modified = rewritten
            .modified
            .map(|time| time + std::time::Duration::from_secs(1));
        let after = PageTable::from_entries("/p", vec![entry("a.png", 10), rewritten]);
        assert_ne!(
            before.digest(),
            after.digest(),
            "same names, same count, same lengths — only the mtime moved"
        );

        let regrown = PageTable::from_entries("/p", vec![entry("a.png", 10), entry("b.png", 21)]);
        assert_ne!(before.digest(), regrown.digest(), "only the length moved");
    }

    #[test]
    fn adding_and_removing_a_member_moves_the_digest() {
        let before = PageTable::from_entries("/p", vec![entry("a.png", 10)]);
        let after = PageTable::from_entries("/p", vec![entry("a.png", 10), entry("b.png", 20)]);
        assert_ne!(before.digest(), after.digest());
    }

    // §51.3
    #[test]
    fn an_insert_earlier_in_sort_order_keeps_the_page_on_screen() {
        let before = names(&["b.png", "c.png"]);
        let after = names(&["a.png", "b.png", "c.png"]);
        assert_eq!(
            reindex(&before, &after, 0),
            1,
            "b.png was index 0 and is now index 1; the audience must not change picture"
        );
        assert_eq!(reindex(&before, &after, 1), 2);
    }

    #[test]
    fn a_rename_is_a_delete_and_an_insert_and_falls_to_a_neighbour() {
        let before = names(&["a.png", "b.png", "c.png"]);
        let after = names(&["a.png", "c.png", "z.png"]);
        assert_eq!(
            reindex(&before, &after, 1),
            1,
            "b.png is gone; c.png is the nearest survivor after it"
        );
    }

    #[test]
    fn deleting_the_page_on_screen_advances_to_the_next_one() {
        let before = names(&["a.png", "b.png", "c.png"]);
        let after = names(&["a.png", "c.png"]);
        assert_eq!(reindex(&before, &after, 1), 1, "which is c.png, not a.png");
    }

    #[test]
    fn deleting_the_last_page_falls_back_to_the_one_before_it() {
        let before = names(&["a.png", "b.png", "c.png"]);
        let after = names(&["a.png", "b.png"]);
        assert_eq!(reindex(&before, &after, 2), 1);
    }

    #[test]
    fn a_reorder_follows_the_name_rather_than_the_index() {
        let before = names(&["img2.png", "img10.png"]);
        let after = names(&["img10.png", "img2.png"]);
        assert_eq!(reindex(&before, &after, 0), 1);
        assert_eq!(reindex(&before, &after, 1), 0);
    }

    #[test]
    fn an_entirely_different_folder_clamps_rather_than_failing() {
        let before = names(&["a.png", "b.png", "c.png"]);
        let after = names(&["x.png"]);
        assert_eq!(reindex(&before, &after, 2), 0);
        assert_eq!(reindex(&before, &[], 2), 0);
    }

    // §41
    #[test]
    fn the_supported_set_is_decided_by_extension_alone_and_ignores_case() {
        for name in ["a.png", "a.JPG", "a.Jpeg", "a.tiff", "a.pbm", "a.qoi"] {
            assert!(is_supported_image(Path::new(name)), "{name}");
        }
        for name in ["a.svg", "a.pdf", "a", "a.png.tmp", "a.heic", ".png"] {
            assert!(!is_supported_image(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn opening_a_file_resolves_to_its_parent_directory() {
        let resolved = resolve_source(Path::new("/pictures/talk/slide03.png")).unwrap();
        assert_eq!(resolved.directory, Path::new("/pictures/talk"));
        assert_eq!(resolved.picked.as_deref(), Some(OsStr::new("slide03.png")));
        assert!(resolved.was_widened());
        assert!(resolve_source(Path::new("/decks/talk.pdf")).is_none());
    }

    #[test]
    fn a_bare_file_name_resolves_to_the_working_directory() {
        let resolved = resolve_source(Path::new("shot.png")).unwrap();
        assert_eq!(resolved.directory, Path::new("."));
    }

    // §40.1, §40.5, §51.5
    #[test]
    fn a_real_directory_is_listed_non_recursively_and_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/deep.png"), b"x").unwrap();
        std::fs::write(dir.path().join("img10.png"), b"xx").unwrap();
        std::fs::write(dir.path().join("img2.PNG"), b"xxx").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"xxxx").unwrap();

        let table = list_directory(dir.path()).unwrap();
        let order: Vec<_> = table
            .entries()
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(order, ["img2.PNG", "img10.png"]);
        assert_eq!(table.index_of(OsStr::new("img10.png")), Some(1));
    }

    #[test]
    fn an_empty_directory_is_a_table_with_no_pages() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_directory(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_directory_over_the_cap_is_refused_and_names_the_count() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..MAX_TRACKED_PAGE_SIZES + 1 {
            std::fs::write(dir.path().join(format!("{index}.png")), b"x").unwrap();
        }
        let error = list_directory(dir.path()).unwrap_err();
        let ListError::TooManyImages { count, .. } = &error else {
            panic!("expected a refusal, got {error}");
        };
        assert_eq!(*count, MAX_TRACKED_PAGE_SIZES + 1);
        assert_eq!(
            directory_stamp(dir.path()).map(|(entries, _)| entries),
            Some(MAX_TRACKED_PAGE_SIZES + 1),
            "the probe still has a stamp for it, so the refusal happens at the \
             open — where somebody is told the count — rather than as a silent \
             retry for ever"
        );
        assert!(error
            .to_string()
            .contains(&(MAX_TRACKED_PAGE_SIZES + 1).to_string()));
    }
}
