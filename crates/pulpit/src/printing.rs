//! What to print, and what the print should contain.
//!
//! This module is the part of printing that has no desktop in it: which pages
//! the reader asked for, whether the paper should carry the marks they have
//! made, what the document's own permission bits say about any of it, and the
//! name of the temporary copy a marked-up print is spooled from. The platform
//! half — the spooler, the printers, the drivers — is
//! [`crate::platform::services::PlatformServices::print`].
//!
//! ## Why pulpit hands a file over rather than rasterising pages itself
//!
//! The renderer worker can already draw any page at any scale, so pushing
//! bitmaps at a printer is within reach. It is still the wrong half of the
//! job to take on: duplex, paper sizes, tray selection, margins, colour
//! management and the dialog that lets someone choose between them are the
//! platform's, and are not improved by being written a second time here. So
//! pulpit decides only the two things nobody else can — which pages, and
//! whether the reader's marks are on them — writes a PDF that says exactly
//! that, and hands it over.
//!
//! ## Why the marked-up print is a separate file
//!
//! "As I have marked it up" means the annotations and the form values as they
//! are *on screen*, which is not what is on disk until a Save As has been
//! made. Rather than make the reader save first, printing writes the same
//! copy Save As would — to a scratch directory, under a name that says what
//! it is ([`spool_name`]) — spools that, and deletes it. It is never offered
//! as the document, and for a signed document it is not the signed one: it is
//! a new file with new bytes, and it says so in its name.

use std::ops::RangeInclusive;
use std::path::Path;

use pulpit_core::page::PageIndex;

/// Which pages the reader asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageChoice {
    #[default]
    All,
    /// The page the reader is looking at, resolved when the print is made
    /// rather than when the dialog opened.
    Current,
    /// Whatever is typed in the range box, read by [`parse_pages`].
    Custom,
}

impl PageChoice {
    pub fn label(self) -> &'static str {
        match self {
            PageChoice::All => "All pages",
            PageChoice::Current => "Current page",
            PageChoice::Custom => "Pages",
        }
    }
}

/// Whether the paper carries what the reader has done to the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Marks {
    /// The file as it sits on disk. Nothing is written and nothing is
    /// flattened: the source itself is spooled.
    AsOnDisk,
    /// The document as it is on screen — annotations drawn, form fields as
    /// they have been filled — which means writing a copy first.
    #[default]
    AsMarkedUp,
}

impl Marks {
    pub fn label(self) -> &'static str {
        match self {
            Marks::AsOnDisk => "As saved on disk",
            Marks::AsMarkedUp => "With my marks and entries",
        }
    }
}

/// The pages of a print job, one-based and in the order they were asked for.
///
/// Empty means the whole document, which is not the same as "no pages": a
/// spooler with nothing to say about a range prints everything, and that is
/// exactly what an empty list asks it to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pages(Vec<RangeInclusive<u32>>);

impl Pages {
    /// Every page in the document.
    pub fn everything() -> Pages {
        Pages(Vec::new())
    }

    pub fn is_everything(&self) -> bool {
        self.0.is_empty()
    }

    pub fn ranges(&self) -> &[RangeInclusive<u32>] {
        &self.0
    }

    /// How many sheets of paper this is, before copies.
    pub fn sheets(&self, page_count: usize) -> usize {
        if self.is_everything() {
            return page_count;
        }
        self.0
            .iter()
            .map(|range| (range.end() - range.start() + 1) as usize)
            .sum()
    }

    /// One page, for "current page".
    pub fn just(page: PageIndex) -> Pages {
        let one = page.0 as u32 + 1;
        // One range holding one page, not a vector of page numbers.
        Pages(std::vec::from_elem(one..=one, 1))
    }
}

/// Why a typed page range could not be read.
///
/// Each one names the offending text rather than saying "invalid": the reader
/// is looking at the box they typed it into, and the useful thing to tell
/// them is which part of it did not work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageListError {
    Empty,
    NotANumber(String),
    Backwards { from: u32, to: u32 },
    OutOfRange { page: u32, page_count: usize },
}

impl PageListError {
    pub fn message(&self) -> String {
        match self {
            PageListError::Empty => "Name at least one page.".into(),
            PageListError::NotANumber(text) => {
                format!("“{text}” is not a page number or a range.")
            }
            PageListError::Backwards { from, to } => {
                format!("{from}–{to} runs backwards.")
            }
            PageListError::OutOfRange { page, page_count } => {
                format!("This document ends at page {page_count}, so there is no page {page}.")
            }
        }
    }
}

/// Read `1-3, 7, 9-11` into ranges, checked against the document's length.
///
/// Whitespace is ignored anywhere, an en dash is accepted for a hyphen
/// because that is what a paste from a document brings with it, and the
/// ranges are left in the order they were typed: a reader who asks for
/// `5,1` means the pages in that order, and reordering them silently would
/// be pulpit deciding it knew better.
pub fn parse_pages(text: &str, page_count: usize) -> Result<Pages, PageListError> {
    let text = text.replace(['\u{2013}', '\u{2014}'], "-");
    let mut ranges = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let number = |value: &str| -> Result<u32, PageListError> {
            value
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|page| *page > 0)
                .ok_or_else(|| PageListError::NotANumber(part.to_string()))
        };
        // Split on the first hyphen only: `3-5-7` is a typo, not a nested
        // range, and reporting it beats guessing which two numbers were meant.
        let range = match part.split_once('-') {
            Some((from, to)) if !to.contains('-') => {
                let (from, to) = (number(from)?, number(to)?);
                if from > to {
                    return Err(PageListError::Backwards { from, to });
                }
                from..=to
            }
            Some(_) => return Err(PageListError::NotANumber(part.to_string())),
            None => {
                let page = number(part)?;
                page..=page
            }
        };
        if *range.end() as usize > page_count {
            return Err(PageListError::OutOfRange {
                page: *range.end(),
                page_count,
            });
        }
        ranges.push(range);
    }
    if ranges.is_empty() {
        return Err(PageListError::Empty);
    }
    Ok(Pages(ranges))
}

/// What the document's `/P` bits say about printing, and what pulpit does
/// about it.
///
/// Reported, never quietly obeyed and never quietly ignored. The permission
/// bits are a request made by whoever produced the file, to a viewer that is
/// not obliged to honour them and could not be made to — every other reader
/// on the machine will print the same file. So pulpit says what the document
/// asked for and makes the reader answer it, which is the one behaviour that
/// is neither a lie to the reader nor a pretence of enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// The document says nothing, or says yes.
    Allowed,
    /// The document asks that it not be printed.
    Withheld,
    /// It may be printed, but only at reduced resolution (`/P` bit 12).
    /// Named because it is worth saying; nothing is downsampled for it.
    LowResolutionOnly,
}

impl Permission {
    pub fn read(permissions: &pulpit_render::document::DocumentPermissions) -> Permission {
        if !permissions.print {
            Permission::Withheld
        } else if !permissions.print_high_quality {
            Permission::LowResolutionOnly
        } else {
            Permission::Allowed
        }
    }

    /// The sentence the dialog shows, or nothing when there is nothing to say.
    pub fn caution(self) -> Option<&'static str> {
        match self {
            Permission::Allowed => None,
            Permission::Withheld => {
                Some("This document asks not to be printed. pulpit will print it if you say so.")
            }
            Permission::LowResolutionOnly => Some(
                "This document asks that it only be printed at reduced quality. pulpit sends \
                 it to the printer as it is.",
            ),
        }
    }

    /// Whether the reader has to say so before the job is sent.
    pub fn needs_an_answer(self) -> bool {
        matches!(self, Permission::Withheld)
    }
}

/// The name of the scratch copy a marked-up print is spooled from.
///
/// Three things are true of it at once and the name has to carry all of them:
/// it is recognisably this document, it is recognisably *not* the file the
/// reader opened, and a second print while the first is still spooling must
/// not land on the same bytes. So the stem, the words that say what it is,
/// and the process's own identifier — which is unique among the pulpits that
/// could be writing into this directory.
pub fn spool_name(source: &Path, salt: u32) -> String {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "document".to_string());
    format!("{stem} (to print {salt}).pdf")
}

/// Everything the print dialog holds while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintDialog {
    pub choice: PageChoice,
    /// The range box, as typed. Kept as text so a half-typed `1-` is not an
    /// error the reader has to look at while still typing it.
    pub custom: String,
    pub marks: Marks,
    pub copies: u16,
    /// The chosen printer's name, or `None` for the system's default.
    pub destination: Option<String>,
    /// What the platform said it could print to when the dialog opened.
    pub destinations: Vec<String>,
    /// The document's own answer, once it is known. `None` while the
    /// properties round trip is still out.
    pub permission: Option<Permission>,
    /// Set when the reader has answered a withheld permission.
    pub permission_answered: bool,
}

/// The largest number of copies the dialog will take. Not a printer limit —
/// a guard on a typo that would otherwise become a ream.
pub const MAX_COPIES: u16 = 99;

impl PrintDialog {
    pub fn open(destinations: Vec<String>, has_edits: bool) -> PrintDialog {
        PrintDialog {
            choice: PageChoice::All,
            custom: String::new(),
            // A document with nothing written on it prints the same either
            // way, and spooling a copy of it would be a write for no reason.
            marks: if has_edits {
                Marks::AsMarkedUp
            } else {
                Marks::AsOnDisk
            },
            copies: 1,
            destination: None,
            destinations,
            permission: None,
            permission_answered: false,
        }
    }

    /// The copy count, as typed, clamped to something a person meant.
    pub fn set_copies(&mut self, text: &str) {
        let digits: String = text.chars().filter(char::is_ascii_digit).collect();
        // Parsed wide and then clamped: a `u16` parse of "100000" fails
        // outright, and falling back to one copy lands on the wrong end of
        // the mistake — the reader leaned on a key, they did not ask for one.
        self.copies = digits
            .parse::<u64>()
            .unwrap_or(1)
            .clamp(1, MAX_COPIES as u64) as u16;
    }

    /// Whether the Print button may be pressed, and why not when it may not.
    pub fn blocked(&self, current: Option<PageIndex>, page_count: usize) -> Option<String> {
        if page_count == 0 {
            return Some("There is no document open to print.".into());
        }
        if self.permission.is_some_and(Permission::needs_an_answer) && !self.permission_answered {
            return Some("This document asks not to be printed.".into());
        }
        match self.pages(current, page_count) {
            Ok(_) => None,
            Err(error) => Some(error.message()),
        }
    }

    /// The pages this dialog currently describes.
    pub fn pages(
        &self,
        current: Option<PageIndex>,
        page_count: usize,
    ) -> Result<Pages, PageListError> {
        match self.choice {
            PageChoice::All => Ok(Pages::everything()),
            // No current page is every page, not none: the reader asked for
            // "the one I am looking at" and there is no reason to refuse.
            PageChoice::Current => Ok(current.map(Pages::just).unwrap_or_default()),
            PageChoice::Custom => parse_pages(&self.custom, page_count),
        }
    }

    /// The job this dialog describes, or why it cannot describe one.
    ///
    /// `source` is the document being printed; its file name becomes the
    /// title the print queue shows.
    pub fn plan(
        &self,
        source: &Path,
        current: Option<PageIndex>,
        page_count: usize,
    ) -> Result<PrintPlan, PageListError> {
        Ok(PrintPlan {
            title: source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Document".to_string()),
            pages: self.pages(current, page_count)?,
            copies: self.copies,
            destination: self.destination.clone(),
            needs_a_copy: self.marks == Marks::AsMarkedUp,
        })
    }

    /// What the dialog says under the buttons: the paper this will use.
    pub fn summary(&self, current: Option<PageIndex>, page_count: usize) -> String {
        let Ok(pages) = self.pages(current, page_count) else {
            return String::new();
        };
        let sheets = pages.sheets(page_count) * self.copies as usize;
        let sides = if sheets == 1 { "side" } else { "sides" };
        format!("{sheets} {sides}")
    }
}

/// A print job, resolved: everything about it except the file, which for a
/// marked-up print does not exist yet.
///
/// Held by the application across the worker round trip that writes the
/// scratch copy: the dialog is gone by the time the answer comes back, so
/// what it decided has to outlive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintPlan {
    /// What the printer's queue should call the job. The *document's* name,
    /// never the scratch file's — nobody wants "(to print 4213)" in their
    /// queue.
    pub title: String,
    pub pages: Pages,
    pub copies: u16,
    pub destination: Option<String>,
    /// Whether a copy has to be written before anything can be spooled.
    pub needs_a_copy: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same string the adapter sends, so a test asserting on a range is
    /// asserting on what actually reaches the spooler.
    fn cups(pages: &Pages) -> Option<String> {
        crate::platform::services::cups_range(pages.ranges())
    }

    #[test]
    fn a_typed_range_is_read_in_the_order_it_was_typed() {
        let pages = parse_pages("1-3, 7, 9-11", 12).expect("a range");
        assert_eq!(cups(&pages).as_deref(), Some("1-3,7,9-11"));
        assert_eq!(pages.sheets(12), 3 + 1 + 3);
        // Not reordered: `5,1` is two pages in that order, and quietly
        // sorting them would be pulpit overruling the reader.
        assert_eq!(
            cups(&parse_pages("5,1", 12).unwrap()).as_deref(),
            Some("5,1")
        );
    }

    #[test]
    fn an_en_dash_and_stray_spaces_are_read_as_a_range() {
        // What a paste out of a document actually contains.
        let pages = parse_pages("  2 \u{2013} 4 ,, 8 ", 10).expect("a range");
        assert_eq!(cups(&pages).as_deref(), Some("2-4,8"));
    }

    #[test]
    fn every_way_of_getting_a_range_wrong_says_which_part_was_wrong() {
        assert_eq!(parse_pages("", 10), Err(PageListError::Empty));
        assert_eq!(parse_pages("  ,, ", 10), Err(PageListError::Empty));
        assert_eq!(
            parse_pages("1,cat", 10),
            Err(PageListError::NotANumber("cat".into()))
        );
        // A page number is one-based, so zero is not one of them.
        assert_eq!(
            parse_pages("0", 10),
            Err(PageListError::NotANumber("0".into()))
        );
        assert_eq!(
            parse_pages("3-5-7", 10),
            Err(PageListError::NotANumber("3-5-7".into()))
        );
        assert_eq!(
            parse_pages("9-4", 10),
            Err(PageListError::Backwards { from: 9, to: 4 })
        );
        assert_eq!(
            parse_pages("8-12", 10),
            Err(PageListError::OutOfRange {
                page: 12,
                page_count: 10
            })
        );
        for error in [
            PageListError::Empty,
            PageListError::NotANumber("cat".into()),
            PageListError::Backwards { from: 9, to: 4 },
            PageListError::OutOfRange {
                page: 12,
                page_count: 10,
            },
        ] {
            assert!(error.message().len() > 10, "{error:?} explains nothing");
        }
    }

    #[test]
    fn the_whole_document_passes_no_range_at_all() {
        let all = Pages::everything();
        assert!(all.is_everything());
        assert_eq!(cups(&all), None);
        assert_eq!(all.sheets(40), 40);
    }

    #[test]
    fn the_current_page_is_resolved_when_the_print_is_made() {
        let mut dialog = PrintDialog::open(Vec::new(), false);
        dialog.choice = PageChoice::Current;
        let pages = dialog.pages(Some(PageIndex(6)), 20).expect("one page");
        // One-based on the paper, zero-based in the document.
        assert_eq!(cups(&pages).as_deref(), Some("7"));
        assert_eq!(pages.sheets(20), 1);
        // Nowhere in particular is everywhere, not nowhere.
        assert!(dialog.pages(None, 20).unwrap().is_everything());
    }

    #[test]
    fn a_document_with_nothing_written_on_it_prints_the_file_on_disk() {
        // Spooling a copy of a document nobody has touched is a write for
        // nothing, so the default follows whether there is anything to carry.
        assert_eq!(PrintDialog::open(Vec::new(), false).marks, Marks::AsOnDisk);
        assert_eq!(PrintDialog::open(Vec::new(), true).marks, Marks::AsMarkedUp);
    }

    #[test]
    fn a_withheld_permission_has_to_be_answered_before_the_button_works() {
        let mut permissions = pulpit_render::document::DocumentPermissions::UNRESTRICTED;
        permissions.print = false;
        let mut dialog = PrintDialog::open(Vec::new(), false);
        dialog.permission = Some(Permission::read(&permissions));
        assert_eq!(dialog.permission, Some(Permission::Withheld));
        assert!(dialog.blocked(None, 10).is_some());
        // Answered, not enforced: every other reader on the machine will
        // print this file, and pretending otherwise helps nobody.
        dialog.permission_answered = true;
        assert!(dialog.blocked(None, 10).is_none());
    }

    #[test]
    fn low_resolution_only_is_said_but_never_stands_in_the_way() {
        let mut permissions = pulpit_render::document::DocumentPermissions::UNRESTRICTED;
        permissions.print_high_quality = false;
        let permission = Permission::read(&permissions);
        assert_eq!(permission, Permission::LowResolutionOnly);
        assert!(permission.caution().is_some());
        assert!(!permission.needs_an_answer());
        assert_eq!(
            Permission::read(&pulpit_render::document::DocumentPermissions::UNRESTRICTED),
            Permission::Allowed
        );
        assert_eq!(Permission::Allowed.caution(), None);
    }

    #[test]
    fn a_bad_range_blocks_the_button_and_says_why() {
        let mut dialog = PrintDialog::open(Vec::new(), false);
        dialog.choice = PageChoice::Custom;
        dialog.custom = "40".into();
        let blocked = dialog.blocked(None, 10).expect("a reason");
        assert!(blocked.contains("page 40"), "{blocked}");
        dialog.custom = "4".into();
        assert!(dialog.blocked(None, 10).is_none());
        // …and with nothing open at all, nothing is printable.
        assert!(dialog.blocked(None, 0).is_some());
    }

    #[test]
    fn a_typo_in_the_copy_count_cannot_become_a_ream() {
        let mut dialog = PrintDialog::open(Vec::new(), false);
        dialog.set_copies("3");
        assert_eq!(dialog.copies, 3);
        dialog.set_copies("100000");
        assert_eq!(dialog.copies, MAX_COPIES);
        dialog.set_copies("");
        assert_eq!(dialog.copies, 1);
        dialog.set_copies("2x");
        assert_eq!(dialog.copies, 2);
    }

    #[test]
    fn the_summary_counts_the_paper_this_will_use() {
        let mut dialog = PrintDialog::open(Vec::new(), false);
        assert_eq!(dialog.summary(None, 12), "12 sides");
        dialog.copies = 2;
        assert_eq!(dialog.summary(None, 12), "24 sides");
        dialog.choice = PageChoice::Current;
        dialog.copies = 1;
        assert_eq!(dialog.summary(Some(PageIndex(0)), 12), "1 side");
    }

    #[test]
    fn the_plan_names_the_document_and_not_the_scratch_copy() {
        let source = Path::new("/home/reader/Lease agreement.pdf");
        let mut dialog = PrintDialog::open(vec!["office".into()], true);
        dialog.destination = Some("office".into());
        dialog.copies = 3;
        dialog.choice = PageChoice::Custom;
        dialog.custom = "2-4".into();
        let plan = dialog.plan(source, Some(PageIndex(0)), 10).expect("a plan");
        // What the print queue shows is the document, not the file pulpit is
        // about to write and delete.
        assert_eq!(plan.title, "Lease agreement.pdf");
        assert_eq!(cups(&plan.pages).as_deref(), Some("2-4"));
        assert_eq!(plan.copies, 3);
        assert_eq!(plan.destination.as_deref(), Some("office"));
        // The marks are on, so a copy has to be written first.
        assert!(plan.needs_a_copy);

        // …and with the file as it is on disk, nothing is written at all.
        dialog.marks = Marks::AsOnDisk;
        assert!(!dialog.plan(source, None, 10).unwrap().needs_a_copy);
    }

    #[test]
    fn a_plan_from_a_bad_range_is_an_error_rather_than_a_whole_document() {
        // The failure mode this guards is the expensive one: a range that
        // could not be read becoming "print everything".
        let mut dialog = PrintDialog::open(Vec::new(), false);
        dialog.choice = PageChoice::Custom;
        dialog.custom = "seven".into();
        assert!(dialog.plan(Path::new("/tmp/a.pdf"), None, 10).is_err());
    }

    #[test]
    fn the_scratch_copy_can_never_be_mistaken_for_the_document() {
        let name = spool_name(Path::new("/home/reader/Lease agreement.pdf"), 4213);
        assert!(name.starts_with("Lease agreement"), "{name}");
        assert!(name.contains("to print"), "{name}");
        assert!(name.ends_with(".pdf"), "{name}");
        // Two prints of the same document in flight must not share bytes.
        assert_ne!(
            name,
            spool_name(Path::new("/home/reader/Lease agreement.pdf"), 9)
        );
        // A path with no stem still gets a name rather than a bare suffix.
        assert!(spool_name(Path::new("/tmp/.pdf"), 1).starts_with(".pdf ("));
    }
}
