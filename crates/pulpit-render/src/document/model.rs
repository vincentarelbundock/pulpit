//! The document API's data types: what an annotation looks like from outside
//! the engine, what a form field is, what a mutation asks for and what it
//! reports back.
//!
//! None of these carries a PDFium handle or an indirect object number (A3),
//! which is what lets every one of them cross the worker protocol, be written
//! to the recovery journal and be replayed against a freshly opened document.

use pulpit_core::annotate::{
    AnnotationCommand, AnnotationDraft, AnnotationId, AnnotationKind, MarkStyle,
};
use pulpit_core::page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect};
use serde::{Deserialize, Serialize};

use super::limits;

/// A session-local monotonic counter, incremented by every successful
/// mutation (§9.1).
///
/// Not PDF metadata and not a persistent version identifier: it exists so a
/// render result can name exactly which state it contains, and so a delayed
/// message cannot overwrite a later change (A7).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct DocumentRevision(pub u64);

impl DocumentRevision {
    /// What an open document starts at.
    pub const INITIAL: DocumentRevision = DocumentRevision(0);

    pub fn next(self) -> DocumentRevision {
        DocumentRevision(self.0 + 1)
    }
}

impl std::fmt::Display for DocumentRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// How much of a document pulpit can honour (§3.4). Displayed to the user, so
/// a limitation is a stated one rather than a surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityLevel {
    /// Opens, renders, fields recognised, no unsupported required actions.
    Native,
    /// Fields editable; some JavaScript, validation, formatting, submission or
    /// appearance behaviour unavailable.
    NativeWithLimitations,
    /// No usable form semantics, but the page renders and every annotation
    /// tool works.
    ///
    /// The default, because it is the *least* pulpit promises about a document
    /// it has not surveyed yet. Promising more and withdrawing it is worse
    /// than promising less and finding more.
    #[default]
    AnnotateOnly,
    /// Renders and turns its pages, and nothing else.
    ///
    /// A folder of images (`SPEC-images.md` §48): annotations, form fields,
    /// text selection, save and signing are PDF semantics and there is
    /// nothing honest to map them onto. Distinct from
    /// [`Self::Unsupported`], which is a document that does not render at
    /// all — this one renders perfectly and is simply not a PDF. The UI
    /// reads this rather than offering controls that refuse when pressed
    /// (§48.3).
    ViewOnly,
    /// Does not render, or cannot be opened safely.
    Unsupported,
}

impl CompatibilityLevel {
    pub fn label(self) -> &'static str {
        match self {
            CompatibilityLevel::Native => "Native",
            CompatibilityLevel::NativeWithLimitations => "Native with limitations",
            CompatibilityLevel::AnnotateOnly => "Annotate only",
            CompatibilityLevel::ViewOnly => "View only",
            CompatibilityLevel::Unsupported => "Unsupported",
        }
    }

    /// May this document be annotated at all?
    pub fn allows_annotation(self) -> bool {
        !matches!(
            self,
            CompatibilityLevel::Unsupported | CompatibilityLevel::ViewOnly
        )
    }

    /// Is this a document pulpit can only show?
    ///
    /// Separate from [`Self::allows_annotation`] because the two answers
    /// coincide for exactly one other level, and a reader that dims a control
    /// wants to know *why*: a folder of images has nothing to annotate, and a
    /// document that will not open has nothing at all.
    pub fn is_view_only(self) -> bool {
        matches!(self, CompatibilityLevel::ViewOnly)
    }

    /// May its form fields be filled?
    pub fn allows_form_filling(self) -> bool {
        matches!(
            self,
            CompatibilityLevel::Native | CompatibilityLevel::NativeWithLimitations
        )
    }
}

/// Something about a document the user is told before they start editing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentWarning {
    /// The document is encrypted. Permissions are honoured and mutation may be
    /// refused outright.
    Encrypted,
    /// The document carries an XFA form, whose dynamic behaviour is deferred
    /// (§3.3). The AcroForm shadow, where there is one, still fills.
    XfaForm,
    /// The document carries JavaScript, which pulpit never executes (§12).
    JavaScript,
    /// The document is cryptographically signed. Editing it can invalidate
    /// that signature (A9), and the user is told *before* the first mutation.
    Signed,
    /// Whether the document is signed could not be established. Treated as if
    /// it might be (A9): a missed signature costs the user the belief that one
    /// survived their edits, and a warning they did not need costs a
    /// dismissal.
    SignatureUnknown,
    /// The document sets permissions that forbid annotation or form filling.
    MutationForbidden,
    /// The document has a form, and PDFium would not give pulpit an
    /// environment to fill it through. The pages still render and every
    /// annotation tool still works; the fields are read-only.
    FormUnavailable,
    /// A field script calls out of the document — submitting, mailing or
    /// opening a URL. pulpit refuses every one of those, and says so here
    /// rather than refusing silently.
    ScriptReachesOut,
    /// A form button carries an action — submitting, resetting, or running a
    /// script. pulpit performs none of them, and which one it is cannot be
    /// told apart through PDFium's public API.
    ButtonAction,
}

impl DocumentWarning {
    pub fn message(&self) -> &'static str {
        match self {
            DocumentWarning::Encrypted => {
                "This document is encrypted. Some edits may be refused by its own permissions."
            }
            DocumentWarning::XfaForm => {
                "This document uses an XFA form. Its dynamic behaviour is not available; \
                 any ordinary AcroForm fields it also carries can still be filled."
            }
            DocumentWarning::JavaScript => {
                "This document contains JavaScript. Its field formatting, validation \
                 and calculations run; anything it asks to send, open or print does not."
            }
            DocumentWarning::Signed => {
                "This document is signed. Saving a modified copy will not carry the \
                 signature's validity with it."
            }
            DocumentWarning::SignatureUnknown => {
                "pulpit could not tell whether this document is signed. If it is, saving a \
                 modified copy will not carry the signature's validity with it."
            }
            DocumentWarning::MutationForbidden => {
                "This document's permissions do not allow it to be changed."
            }
            DocumentWarning::FormUnavailable => {
                "This document's form fields cannot be filled. The pages still render \
                 and every annotation tool still works."
            }
            DocumentWarning::ScriptReachesOut => {
                "This form tries to send itself somewhere. pulpit fills it and saves it \
                 locally; nothing is submitted, mailed or opened over the network."
            }
            DocumentWarning::ButtonAction => {
                "This form has buttons pulpit does not press — submit, reset or script. \
                 Fill the fields and save a copy instead."
            }
        }
    }
}

/// One string a document wrote about itself — a title, an author, a producer.
///
/// Bounded and reported the way [`AnnotationContents`] bounds `/Contents`, and
/// for the same reason: these are attacker-controlled strings that end up on
/// screen. Nothing here is interpreted — no markup, no escapes, no line
/// structure. Control characters are turned into spaces and runs of whitespace
/// collapsed, so a producer cannot lay a title out across the dialog or pad one
/// with a screenful of newlines to push a permission row out of sight.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InfoText {
    pub text: String,
    /// True when the value was cut to fit [`limits::MAX_INFO_TEXT_BYTES`], so a
    /// properties view can say so rather than show a silently shortened line.
    pub truncated: bool,
}

impl InfoText {
    /// Take one raw `/Info` string, or `None` when the document has nothing to
    /// say under that key.
    ///
    /// An absent key and a key holding only spaces are the same answer — the
    /// document did not say — and both are reported as absent so the view can
    /// leave the row out rather than draw an empty one.
    pub fn read(raw: &str) -> Option<InfoText> {
        let mut text = String::with_capacity(raw.len());
        let mut pending_space = false;
        for character in raw.chars() {
            if character.is_whitespace() || character.is_control() {
                pending_space = !text.is_empty();
                continue;
            }
            if pending_space {
                text.push(' ');
                pending_space = false;
            }
            text.push(character);
        }
        if text.is_empty() {
            return None;
        }
        if text.len() <= limits::MAX_INFO_TEXT_BYTES {
            return Some(InfoText {
                text,
                truncated: false,
            });
        }
        // On a character boundary, never inside one: the bound is in bytes and
        // the string is UTF-8.
        let mut cut = limits::MAX_INFO_TEXT_BYTES;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        Some(InfoText {
            text,
            truncated: true,
        })
    }
}

impl std::fmt::Display for InfoText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)?;
        if self.truncated {
            f.write_str("…")?;
        }
        Ok(())
    }
}

/// A date a document wrote about itself, as far as it can be believed.
///
/// The raw string is kept whatever happens: a `/CreationDate` that does not
/// parse is still what the file says, and showing it is more honest than
/// dropping the row and implying the document gave no date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDate {
    pub raw: InfoText,
    /// The parts of `D:YYYYMMDDHHmmSSOHH'mm'` that were present and in range.
    pub parsed: Option<CivilTime>,
}

impl DocumentDate {
    /// Read a PDF date string (`D:20240115103000+01'00'`), keeping the raw
    /// text whether or not the shape is one this understands.
    pub fn read(raw: &str) -> Option<DocumentDate> {
        let text = InfoText::read(raw)?;
        Some(DocumentDate {
            parsed: CivilTime::parse(&text.text),
            raw: text,
        })
    }
}

impl std::fmt::Display for DocumentDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.parsed {
            Some(time) => write!(f, "{time}"),
            None => write!(f, "{}", self.raw),
        }
    }
}

/// A wall-clock date and time as a document wrote it down, with no attempt to
/// resolve it against a real calendar.
///
/// Deliberately not a timestamp. The domain crates read no clock and pulpit
/// carries no time-zone database; what a `/ModDate` records is the local time
/// its producer believed it was, and turning that into an instant would be an
/// invention. It is displayed as it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CivilTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Minutes east of UTC, when the document said. `None` is a date with no
    /// offset, which the specification calls local time of unknown zone.
    pub offset_minutes: Option<i16>,
}

impl CivilTime {
    /// `D:YYYYMMDDHHmmSSOHH'mm'`, with everything after the year optional.
    ///
    /// Defaults are the specification's: an absent field is the smallest legal
    /// value. Anything malformed answers `None` rather than a date built from
    /// the digits that did parse — a half-read date is a wrong one.
    pub fn parse(raw: &str) -> Option<CivilTime> {
        let digits = raw.strip_prefix("D:").unwrap_or(raw);
        let digits = digits.as_bytes();
        if digits.len() < 4 || !digits[..4].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let field = |at: usize, default: u8| -> Option<u8> {
            if digits.len() < at + 2 {
                return Some(default);
            }
            let pair = &digits[at..at + 2];
            if !pair.iter().all(u8::is_ascii_digit) {
                return None;
            }
            Some((pair[0] - b'0') * 10 + (pair[1] - b'0'))
        };
        let year = digits[..4]
            .iter()
            .fold(0u16, |value, digit| value * 10 + u16::from(digit - b'0'));
        let month = field(4, 1)?;
        let day = field(6, 1)?;
        let hour = field(8, 0)?;
        let minute = field(10, 0)?;
        let second = field(12, 0)?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        // 60 for a leap second, which a producer may legally have written.
        if hour > 23 || minute > 59 || second > 60 {
            return None;
        }
        let offset_minutes = match digits.get(14) {
            None => None,
            Some(b'Z') => Some(0),
            Some(sign @ (b'+' | b'-')) => {
                let sign = if *sign == b'-' { -1 } else { 1 };
                let hours = i16::from(field(15, 0)?);
                // The minutes are written after an apostrophe, and some
                // producers leave it — and them — out entirely.
                let minutes = match digits.get(17) {
                    Some(b'\'') => i16::from(field(18, 0)?),
                    Some(byte) if byte.is_ascii_digit() => i16::from(field(17, 0)?),
                    _ => 0,
                };
                if hours > 23 || minutes > 59 {
                    return None;
                }
                Some(sign * (hours * 60 + minutes))
            }
            Some(_) => return None,
        };
        Some(CivilTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
            offset_minutes,
        })
    }
}

impl std::fmt::Display for CivilTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )?;
        match self.offset_minutes {
            None => Ok(()),
            Some(0) => f.write_str(" UTC"),
            Some(offset) => write!(
                f,
                " UTC{}{:02}:{:02}",
                if offset < 0 { '-' } else { '+' },
                offset.abs() / 60,
                offset.abs() % 60
            ),
        }
    }
}

/// What a document's own permission flags allow, once it is encrypted.
///
/// The bit numbers are the specification's, counted from one. An unencrypted
/// document has no permission flags at all and every one of these is true —
/// which is not the same as a document that grants everything, and the
/// properties view says which case it is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentPermissions {
    /// Bit 3.
    pub print: bool,
    /// Bit 4: change the content.
    pub modify: bool,
    /// Bit 5: copy text and graphics out.
    pub copy: bool,
    /// Bit 6: add or change annotations, and fill existing fields.
    pub annotate: bool,
    /// Bit 9: fill form fields even where bit 6 is clear.
    pub fill_forms: bool,
    /// Bit 10: extract text for accessibility.
    pub accessibility: bool,
    /// Bit 11: insert, rotate or delete pages.
    pub assemble: bool,
    /// Bit 12: print at full resolution rather than a degraded image.
    pub print_high_quality: bool,
}

impl Default for DocumentPermissions {
    fn default() -> Self {
        DocumentPermissions::UNRESTRICTED
    }
}

impl DocumentPermissions {
    /// What an unencrypted document allows: everything, because it declared
    /// nothing.
    pub const UNRESTRICTED: DocumentPermissions = DocumentPermissions {
        print: true,
        modify: true,
        copy: true,
        annotate: true,
        fill_forms: true,
        accessibility: true,
        assemble: true,
        print_high_quality: true,
    };

    /// Read the flags out of `/P`, whose bits are numbered from one.
    pub fn from_bits(bits: u32) -> DocumentPermissions {
        let allowed = |bit: u32| bits & (1 << (bit - 1)) != 0;
        DocumentPermissions {
            print: allowed(3),
            modify: allowed(4),
            copy: allowed(5),
            annotate: allowed(6),
            fill_forms: allowed(9),
            accessibility: allowed(10),
            assemble: allowed(11),
            print_high_quality: allowed(12),
        }
    }

    /// Every operation, with what the document says about it. In the order a
    /// reader wants to scan them: what pulpit is about to do first.
    pub fn each(&self) -> [(&'static str, bool); 8] {
        [
            ("Annotate", self.annotate),
            ("Fill form fields", self.fill_forms),
            ("Change the content", self.modify),
            ("Print", self.print),
            ("Print at full resolution", self.print_high_quality),
            ("Copy text out", self.copy),
            ("Extract for accessibility", self.accessibility),
            ("Insert, rotate or delete pages", self.assemble),
        ]
    }

    pub fn is_unrestricted(&self) -> bool {
        *self == DocumentPermissions::UNRESTRICTED
    }
}

/// How a document is encrypted, when it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Encryption {
    /// PDFium's security-handler revision. Negative means no handler, which is
    /// why an [`Encryption`] only exists for a document that has one.
    pub revision: i32,
}

impl Encryption {
    /// What the revision number implies about the algorithm, said as loosely
    /// as it is actually known: the revision names the handler, and the
    /// handler's `/CF` decides the cipher, which is not read here.
    pub fn label(&self) -> &'static str {
        match self.revision {
            2 => "40-bit RC4",
            3 => "128-bit RC4",
            4 => "128-bit, RC4 or AES",
            5 | 6 => "256-bit AES",
            _ => "an unrecognised security handler",
        }
    }
}

/// The PDF version a document declares in its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfVersion(pub u32);

impl std::fmt::Display for PdfVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.0 / 10, self.0 % 10)
    }
}

/// A page's size, in the units a person asks the question in.
///
/// Millimetres first because that is what paper is sold in, the paper's name
/// when it has one, and points last because that is what the file says and
/// what a producer's own dialog will show. `/UserUnit` is applied: a page that
/// scales its points is physically that much larger, and reporting the raw
/// numbers would describe a sheet nobody could print.
pub fn describe_page_size(page: &PageGeometry) -> String {
    let unit = if page.user_unit > 0.0 {
        page.user_unit
    } else {
        1.0
    };
    let (width, height) = (page.width * unit, page.height * unit);
    let millimetres = |points: f32| points * 25.4 / 72.0;
    let (width_mm, height_mm) = (millimetres(width), millimetres(height));
    let orientation = if (width - height).abs() < 1.0 {
        "square"
    } else if width > height {
        "landscape"
    } else {
        "portrait"
    };
    match paper_name(width_mm, height_mm) {
        Some(name) => format!(
            "{width_mm:.0} × {height_mm:.0} mm ({name} {orientation}), {width:.0} × {height:.0} pt"
        ),
        None => format!(
            "{width_mm:.0} × {height_mm:.0} mm ({orientation}), {width:.0} × {height:.0} pt"
        ),
    }
}

/// The name of a standard sheet this size, when there is one.
///
/// Named in either orientation, and only within a couple of millimetres:
/// producers round their page boxes, and a document two tenths of a millimetre
/// off A4 is A4. Anything else is reported by its measurements rather than
/// forced into the nearest name.
fn paper_name(width_mm: f32, height_mm: f32) -> Option<&'static str> {
    /// How far off a nominal size a sheet may be and still be called by its
    /// name.
    const TOLERANCE: f32 = 2.0;
    const SHEETS: [(&str, f32, f32); 11] = [
        ("A0", 841.0, 1189.0),
        ("A1", 594.0, 841.0),
        ("A2", 420.0, 594.0),
        ("A3", 297.0, 420.0),
        ("A4", 210.0, 297.0),
        ("A5", 148.0, 210.0),
        ("A6", 105.0, 148.0),
        ("B5", 176.0, 250.0),
        ("Letter", 216.0, 279.0),
        ("Legal", 216.0, 356.0),
        ("Tabloid", 279.0, 432.0),
    ];
    let (short, long) = if width_mm <= height_mm {
        (width_mm, height_mm)
    } else {
        (height_mm, width_mm)
    };
    SHEETS
        .iter()
        .find(|(_, sheet_short, sheet_long)| {
            (short - sheet_short).abs() <= TOLERANCE && (long - sheet_long).abs() <= TOLERANCE
        })
        .map(|(name, _, _)| *name)
}

/// Whether every page of a document is the size of its first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PageSizes {
    /// Every page measured the same as the first.
    Uniform,
    /// At least one page is a different size or rotation.
    Mixed,
    /// Not established. A document longer than
    /// [`MAX_PAGES_MEASURED_FOR_PROPERTIES`] is not walked to answer a
    /// question nobody is waiting on, and a backend that measures nothing
    /// says so rather than claiming uniformity it did not check.
    #[default]
    Unmeasured,
}

/// How many pages the properties scan will measure before it stops and reports
/// [`PageSizes::Unmeasured`].
///
/// The measurements are almost always already cached — the reader asked for
/// every page's geometry when the document opened — so this bounds the case
/// where they are not: a thousand-page document whose properties are opened
/// before its layout finished.
pub const MAX_PAGES_MEASURED_FOR_PROPERTIES: usize = 4_096;

/// What a document *is*: everything the properties view shows.
///
/// Read on demand rather than at open. It is one cheap call per key, but it is
/// a question about the document and the document lives in a worker, so it
/// costs a round trip — and a presenter opening a deck never asks it. Every
/// string here has been through [`InfoText`], and every field is optional
/// because a document that said nothing must be reported as having said
/// nothing rather than as an empty row.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DocumentProperties {
    pub title: Option<InfoText>,
    pub author: Option<InfoText>,
    pub subject: Option<InfoText>,
    pub keywords: Option<InfoText>,
    /// The application that produced the original document.
    pub creator: Option<InfoText>,
    /// The one that converted it to PDF.
    pub producer: Option<InfoText>,
    pub created: Option<DocumentDate>,
    pub modified: Option<DocumentDate>,
    pub page_count: usize,
    /// The first page's geometry, and what the rest of the document does with
    /// it.
    ///
    /// Measured rather than assumed: a document that mixes portrait and
    /// landscape is reported as mixed instead of being described by its first
    /// page, which is the case a presenter most wants to know about.
    pub first_page: PageGeometry,
    pub page_sizes: PageSizes,
    /// Absent for a document that is not a PDF at all — a folder of images —
    /// or one whose header could not be read.
    pub version: Option<PdfVersion>,
    pub encryption: Option<Encryption>,
    pub permissions: DocumentPermissions,
    pub level: CompatibilityLevel,
    pub warnings: Vec<DocumentWarning>,
}

impl DocumentProperties {
    /// Every document-controlled string in here, for the one place that has to
    /// check them all: the protocol's bound on an answer from a worker that
    /// has just parsed a hostile file.
    ///
    /// A method rather than a list written out at the call site, so a field
    /// added above cannot be forgotten by the check.
    pub fn strings(&self) -> impl Iterator<Item = &InfoText> {
        [
            self.title.as_ref(),
            self.author.as_ref(),
            self.subject.as_ref(),
            self.keywords.as_ref(),
            self.creator.as_ref(),
            self.producer.as_ref(),
            self.created.as_ref().map(|date| &date.raw),
            self.modified.as_ref().map(|date| &date.raw),
        ]
        .into_iter()
        .flatten()
    }

    /// What can be said about a document from its [`OpenDocumentInfo`] alone.
    ///
    /// The honest answer for a backend with no `/Info` dictionary to read —
    /// a folder of images, or the fixture. It reports the shape of the
    /// document and claims nothing about its metadata.
    pub fn from_info(info: &OpenDocumentInfo) -> DocumentProperties {
        DocumentProperties {
            page_count: info.page_count,
            first_page: info.first_page,
            page_sizes: if info.page_count <= 1 {
                PageSizes::Uniform
            } else {
                PageSizes::Unmeasured
            },
            level: info.level,
            warnings: info.warnings.clone(),
            ..DocumentProperties::default()
        }
    }
}

/// What the engine knows about an open document up front.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenDocumentInfo {
    pub page_count: usize,
    pub level: CompatibilityLevel,
    pub warnings: Vec<DocumentWarning>,
    /// The page pulpit measured geometry for, up to the tracked bound; pages
    /// beyond it are measured on demand.
    pub first_page: PageGeometry,
    pub has_form: bool,
}

/// How well pulpit understands one imported annotation (§10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationSupport {
    /// Round-trips through the model without known loss.
    Editable,
    /// Understood and selectable, but editing it would lose data.
    ReadOnlySupported,
    /// Preserved and rendered, with bounded summary metadata only.
    Unsupported,
    /// Ignored or rendered only as far as is safe, with a diagnostic.
    Malformed,
}

impl AnnotationSupport {
    pub fn is_editable(self) -> bool {
        matches!(self, AnnotationSupport::Editable)
    }
}

/// The contents of an annotation, as much as fits in a summary.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AnnotationContents {
    /// `/Contents`, bounded by [`limits::MAX_TEXT_BYTES`].
    pub text: String,
    /// True when the text was cut to fit the bound, so an inspector can say
    /// so rather than showing a silently truncated sentence.
    pub truncated: bool,
    /// pulpit's own namespaced metadata — Typst source, principally — when
    /// the annotation carries it (§7.4).
    pub pulpit_source: Option<String>,
}

/// One annotation, as the application sees it (§6.1).
///
/// Carries enough geometry for hit-testing and inspectors, and no raw object
/// reference. Large ink arrays and quad lists may be absent from an
/// enumeration and fetched by id, which is what keeps a page of ten thousand
/// strokes inside the protocol's message bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationSummary {
    pub id: AnnotationId,
    pub page: PageIndex,
    pub kind: AnnotationKind,
    pub bounds: PageRect,
    pub style: MarkStyle,
    pub contents: AnnotationContents,
    pub support: AnnotationSupport,
    pub revision: DocumentRevision,
    /// The stroke's centre line, when it was cheap enough to include.
    pub path: Vec<PagePoint>,
    /// The marked text runs, when they were cheap enough to include.
    pub quads: Vec<PageQuad>,
    /// True when `path` or `quads` was left out of this summary and must be
    /// fetched by id. An empty vector and an omitted one are different things,
    /// and a hit-test that confused them would silently stop selecting.
    pub geometry_elided: bool,
    /// Which mark a `/Stamp` shows, when the file says so.
    ///
    /// A stamp records an *appearance* and nothing else: there is nothing in
    /// one to read back that tells a check from a cross from a rasterised
    /// picture. So pulpit writes `/Name` — the entry PDF 12.5.6.12 has for
    /// exactly this — when it places a check or a cross, and this is that
    /// name read back.
    ///
    /// `None` for every other kind, and for a stamp whose picture pulpit did
    /// not draw: a mark pulpit cannot rebuild is one it must not offer to
    /// rewrite (A5), because every edit clears the appearance PDFium is
    /// holding and only what pulpit can draw again would come back.
    pub stamp: Option<pulpit_core::annotation::StampChoice>,
}

impl AnnotationSummary {
    pub fn editable(&self) -> bool {
        self.support.is_editable()
    }

    /// The modelled content, when pulpit understands this annotation well
    /// enough to describe it.
    ///
    /// `None` for a kind that is preserved rather than modelled (§10.2) —
    /// which is also a kind the editor never offers to change, so nothing can
    /// build a replacement out of one by accident.
    pub fn to_draft(&self) -> Option<AnnotationDraft> {
        use pulpit_core::annotate::{
            FreeTextDraft, HighlightDraft, InkDraft, InkPoint, NoteDraft, ShapeDraft, ShapeOutline,
            StampDraft, TextSource,
        };

        let style = self.style;
        match self.kind {
            AnnotationKind::Ink => Some(AnnotationDraft::Ink(InkDraft {
                page: self.page,
                points: self.path.iter().map(|at| InkPoint { at: *at }).collect(),
                style,
            })),
            // A box and an ellipse are their rectangle and nothing else, so
            // the summary carries everything a replacement needs.
            AnnotationKind::Square | AnnotationKind::Circle => {
                Some(AnnotationDraft::Shape(ShapeDraft {
                    page: self.page,
                    outline: if self.kind == AnnotationKind::Square {
                        ShapeOutline::Box
                    } else {
                        ShapeOutline::Ellipse
                    },
                    rect: self.bounds,
                    style,
                }))
            }
            // One draft for all three text markups: they differ in the
            // subtype the draft's own kind chooses, and in nothing else.
            AnnotationKind::Highlight | AnnotationKind::Underline | AnnotationKind::StrikeOut => {
                Some(AnnotationDraft::Highlight(HighlightDraft {
                    page: self.page,
                    kind: self.kind.markup().expect("a text markup kind"),
                    quads: self.quads.clone(),
                    text: self.contents.text.clone(),
                    style,
                }))
            }
            AnnotationKind::FreeText => Some(AnnotationDraft::FreeText(FreeTextDraft {
                page: self.page,
                rect: self.bounds,
                text: self.contents.text.clone(),
                source: if self.contents.pulpit_source.is_some() {
                    TextSource::Typst
                } else {
                    TextSource::Plain
                },
                style,
            })),
            AnnotationKind::Note => Some(AnnotationDraft::Note(NoteDraft {
                page: self.page,
                at: PagePoint::new(self.bounds.left, self.bounds.top),
                text: self.contents.text.clone(),
                style,
            })),
            // Only the marks pulpit drew itself. A `/Stamp` records an
            // appearance and nothing that says what drew it, so a stamp whose
            // `/Name` pulpit did not write — a rasterised Typst mark, another
            // producer's picture — cannot be described here at all. Saying
            // otherwise would be worse than saying nothing: every edit clears
            // the appearance the engine is holding, so a mark rewritten from
            // a guess comes back as the guess, and one rewritten from nothing
            // does not come back.
            AnnotationKind::Stamp => self.stamp.map(|mark| {
                AnnotationDraft::Stamp(StampDraft {
                    page: self.page,
                    rect: self.bounds,
                    mark: mark.into(),
                    style,
                    // Whatever markup generated this mark, so reopening it for
                    // editing shows the source rather than the picture (§7.4).
                    source: self.contents.pulpit_source.clone(),
                })
            }),
            AnnotationKind::Other => None,
        }
    }

    /// The shape [`pulpit_core::annotate::hit`] tests against.
    pub fn to_hit(&self) -> pulpit_core::annotate::AnnotationHit {
        pulpit_core::annotate::AnnotationHit {
            id: self.id.clone(),
            kind: self.kind,
            bounds: self.bounds,
            path: self.path.clone(),
            quads: self.quads.clone(),
            editable: self.editable(),
            width: self.style.width,
        }
    }
}

/// What kind of control a form field is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldKind {
    Text,
    Checkbox,
    RadioGroup,
    ComboBox,
    ListBox,
    PushButton,
    /// A signature field. Displayed, never filled — signing is
    /// `SPEC-signing.md`'s subject, not this one's.
    Signature,
    /// A field whose type pulpit does not recognise. Displayed read-only so
    /// the user can see it exists rather than wondering where it went.
    Unknown,
}

impl FieldKind {
    /// Can a value be typed or chosen for this kind?
    pub fn is_fillable(self) -> bool {
        matches!(
            self,
            FieldKind::Text
                | FieldKind::Checkbox
                | FieldKind::RadioGroup
                | FieldKind::ComboBox
                | FieldKind::ListBox
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            FieldKind::Text => "Text",
            FieldKind::Checkbox => "Checkbox",
            FieldKind::RadioGroup => "Radio group",
            FieldKind::ComboBox => "Dropdown",
            FieldKind::ListBox => "List",
            FieldKind::PushButton => "Button",
            FieldKind::Signature => "Signature",
            FieldKind::Unknown => "Field",
        }
    }
}

/// Where one field is drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldWidget {
    pub page: PageIndex,
    pub bounds: PageRect,
    /// The value this widget stands for when a field's widgets mean different
    /// things — a radio group's options. `None` when pressing the widget means
    /// the field rather than one of its values.
    pub option: Option<String>,
}

/// What a text field's format script makes of the value typed into it.
///
/// Read from the script itself, because PDF has no other way to say it: the
/// Acrobat form-field format categories are implemented as calls to a standard
/// JavaScript library — `AFDate_FormatEx`, `AFNumber_Format` and friends — and
/// the pattern is the argument. A field pulpit could only describe as "text"
/// is one it cannot tell a person what to type into.
///
/// Not an interpretation of the value: PDFium runs these scripts, and what a
/// date field holds after a commit is whatever they made of it. This is only
/// what the field is *for*.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldFormat {
    /// No format script, or one pulpit does not recognise. Plain text.
    #[default]
    Plain,
    /// A date, with the pattern the script names — `dd mmmm yyyy`, `m/d/yy`.
    ///
    /// The pattern is Acrobat's own vocabulary, passed through unchanged
    /// rather than translated: it is what the document author wrote, and it is
    /// what an Acrobat user would see in the field's properties.
    Date { pattern: String },
    /// A number, per `AFNumber_Format`.
    ///
    /// Only the two arguments a person typing into the field can act on are
    /// kept: how many decimals the value is rewritten to, and the currency
    /// symbol it is shown with. The separator and negative styles change how
    /// the *engine* draws the committed value, and repeating them in a hint
    /// would teach a shape nobody has to type.
    Number {
        #[serde(default)]
        decimals: u8,
        /// The `strCurrency` argument — `$`, `€`, `CHF`. Empty when the
        /// script named none.
        #[serde(default)]
        currency: String,
    },
    /// A percentage, per `AFPercent_Format`, with the decimals it asks for.
    Percent {
        #[serde(default)]
        decimals: u8,
    },
    /// A time of day, per `AFTime_Format`.
    ///
    /// The pattern is Acrobat's own vocabulary — `HH:MM`, `h:MM tt` — from
    /// its fixed four-entry preset table, translated the way the date presets
    /// are. Empty only for a preset that table does not know.
    Time { pattern: String },
    /// A telephone number, postcode or similar, per `AFSpecial_Format` or an
    /// explicit `AFSpecial_KeystrokeEx` mask.
    Special { kind: SpecialFormat },
}

/// Which of Acrobat's "special" formats a field asks for.
///
/// `AFSpecial_Format` takes a number from a fixed table Acrobat has carried
/// unchanged for decades; `AFSpecial_KeystrokeEx` carries an arbitrary mask in
/// Acrobat's mask vocabulary — `9` a digit, `A` a letter, `O` either, `X` any
/// character, anything else itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpecialFormat {
    /// `AFSpecial_Format(0)`: a five-digit postcode.
    Zip,
    /// `AFSpecial_Format(1)`: a nine-digit postcode.
    ZipPlusFour,
    /// `AFSpecial_Format(2)`: a telephone number.
    Phone,
    /// `AFSpecial_Format(3)`: a Social Security number.
    Ssn,
    /// `AFSpecial_KeystrokeEx("...")`, with the mask the script names.
    Mask { mask: String },
    /// An `AFSpecial` call whose argument could not be read.
    Unknown,
}

impl SpecialFormat {
    /// What to tell someone about to type into this field.
    pub fn hint(&self) -> String {
        match self {
            SpecialFormat::Zip => "a ZIP code".into(),
            SpecialFormat::ZipPlusFour => "a ZIP+4 code".into(),
            SpecialFormat::Phone => "a phone number".into(),
            SpecialFormat::Ssn => "a Social Security number".into(),
            SpecialFormat::Mask { mask } => format!("a value shaped {mask}"),
            SpecialFormat::Unknown => "a formatted value".into(),
        }
    }
}

impl FieldFormat {
    /// What to tell someone about to type into this field.
    pub fn hint(&self) -> Option<String> {
        match self {
            FieldFormat::Plain => None,
            // A numbered preset the table below did not know leaves no pattern
            // to show, and "date, as " is worse than "a date".
            FieldFormat::Date { pattern } if pattern.is_empty() => Some("a date".into()),
            FieldFormat::Date { pattern } => Some(format!("date, as {pattern}")),
            FieldFormat::Number { decimals, currency } => {
                let subject = if currency.is_empty() {
                    "number".to_string()
                } else {
                    format!("number in {currency}")
                };
                Some(match decimals_phrase(*decimals) {
                    Some(decimals) => format!("{subject}, {decimals}"),
                    None if currency.is_empty() => "a number".into(),
                    None => format!("a {subject}"),
                })
            }
            FieldFormat::Percent { decimals } => Some(match decimals_phrase(*decimals) {
                Some(decimals) => format!("percentage, {decimals}"),
                None => "a percentage".into(),
            }),
            FieldFormat::Time { pattern } if pattern.is_empty() => Some("a time".into()),
            FieldFormat::Time { pattern } => Some(format!("time, as {pattern}")),
            FieldFormat::Special { kind } => Some(kind.hint()),
        }
    }

    /// The time pattern this field asks for, if it asks for a time at all.
    pub fn time_pattern(&self) -> Option<&str> {
        match self {
            FieldFormat::Time { pattern } => Some(pattern.as_str()),
            _ => None,
        }
    }
}

/// "2 decimals", or nothing at all for a whole number.
///
/// Nothing rather than "0 decimals" because a field that takes no fraction is
/// a field that takes a number, and saying so in the negative is a longer way
/// of saying less.
fn decimals_phrase(decimals: u8) -> Option<String> {
    match decimals {
        0 => None,
        1 => Some("1 decimal".into()),
        many => Some(format!("{many} decimals")),
    }
}

/// One AcroForm field (§6.4).
///
/// This is pdfform's `FormValue`/`WidgetRect` with `NormalizedRect` replaced
/// by canonical [`PageRect`] per A4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormField {
    pub name: String,
    pub kind: FieldKind,
    pub value: String,
    /// Which of [`Self::options`] are chosen, by index, in the order the file
    /// lists them.
    ///
    /// A choice field that takes several selections has no single value, and
    /// [`Self::value`] can only ever name one of them — so what was chosen is
    /// said here, where a list of three things can be a list of three things.
    /// Empty for every other kind, and for a choice field with nothing chosen.
    #[serde(default)]
    pub selected: Vec<u32>,
    pub read_only: bool,
    /// What the field's own format script makes of its value.
    ///
    /// A date field in a PDF is not a distinct field type: it is a text field
    /// whose `/AA /F` script calls `AFDate_FormatEx("dd mmmm yyyy")`. So
    /// [`Self::kind`] stays faithful to `/FT` — it really is `Text` — and what
    /// the field *means* is said here, which is also how Acrobat models it.
    #[serde(default)]
    pub format: FieldFormat,
    pub options: Vec<String>,
    pub allows_custom_value: bool,
    pub multiple_selection: bool,
    /// `/Ff` Required: the document says this field must hold a value before
    /// the form is submitted. Surfaced so a save can say what is still empty;
    /// never enforced, because pulpit is not the form's submit button.
    #[serde(default)]
    pub required: bool,
    /// A text field with the Password flag. PDFium already draws and edits it
    /// masked; this is for every place *pulpit* would otherwise echo the value.
    #[serde(default)]
    pub password: bool,
    /// A text field with the FileSelect flag: its value is a path a viewer
    /// fills through a file picker pulpit refuses to open (§8.6). Filling it
    /// can never succeed, so it is shown and not edited.
    #[serde(default)]
    pub file_select: bool,
    /// A text field with the RichText flag. It fills, but the styled `/RV`
    /// the document carries is not rewritten alongside `/V`, so another
    /// viewer may keep showing the old styled text.
    #[serde(default)]
    pub rich_text: bool,
    /// The document's value for this field is longer than pulpit carries, so
    /// [`Self::value`] is a prefix of it.
    ///
    /// Reported rather than hidden because the difference is one nothing else
    /// can recover: writing a prefix back over the field would throw away the
    /// rest, so [`Self::is_editable`] says no and the inspector can say why.
    /// PDFium's own editor is unaffected — it holds the whole value, and
    /// typing into the field on the page still works — this is only about
    /// what *pulpit* may claim to know.
    #[serde(default)]
    pub truncated: bool,
    /// The widget's `/F` marks it Hidden or NoView: no viewer paints it and no
    /// pointer can reach it.
    ///
    /// Listed anyway, because a field that exists is a fact an inspector may
    /// want, and refused as an editing target by [`Self::is_editable`],
    /// because an editor over a widget nobody can see is an editor for a blank
    /// patch of page.
    #[serde(default)]
    pub hidden: bool,
    /// Where the field is drawn. Empty when neither the producer nor the
    /// reader of the document could say — the inspector is still a way in.
    pub widgets: Vec<FieldWidget>,
}

impl FormField {
    /// Where this field's editor goes on `page`: at its first widget there.
    ///
    /// A field with more than one rectangle on one page is a radio group's
    /// options or mirrored copies of one value, and neither wants a second
    /// editor.
    pub fn anchor_on(&self, page: PageIndex) -> Option<PageRect> {
        self.widgets
            .iter()
            .find(|widget| widget.page == page)
            .map(|widget| widget.bounds)
    }

    /// Every page this field appears on, in order, without repeats.
    pub fn pages(&self) -> Vec<PageIndex> {
        let mut pages: Vec<PageIndex> = Vec::new();
        for widget in &self.widgets {
            if !pages.contains(&widget.page) {
                pages.push(widget.page);
            }
        }
        pages
    }

    /// Can the user change this one?
    ///
    /// Four ways to be told no, and each of them is an edit that could not
    /// take rather than a preference:
    ///
    /// - a read-only field, which the document says so about;
    /// - a kind that holds no value — a button, a signature;
    /// - a file-select field, whose value is a path chosen through a file
    ///   picker the worker refuses to open;
    /// - a field whose value is longer than pulpit read, where writing back
    ///   what was read would cut the rest of it off.
    ///
    /// A hidden widget is deliberately *not* on that list. It cannot be
    /// clicked into, because nothing draws it, but a field list can still walk
    /// to it and a document can still mean it to be filled; what refuses it is
    /// [`Self::is_reachable`], which is about where an editor can go.
    pub fn is_editable(&self) -> bool {
        !self.read_only && self.kind.is_fillable() && !self.file_select && !self.truncated
    }

    /// Can the user get to this one on the page?
    ///
    /// Editable and drawn. This is what the tab order, the field navigator's
    /// jump and the focus ring ask, because each of them puts something on the
    /// page at the widget's rectangle — and a widget the document hid is a
    /// rectangle with nothing in it.
    pub fn is_reachable(&self) -> bool {
        self.is_editable() && !self.hidden
    }

    /// Does the document ask for this field and hold nothing in it (§6.4)?
    ///
    /// Reachable, not merely editable: a required field the document hides is
    /// one nobody can fill, and listing it before a save would be asking the
    /// reader to go and type into something that is not on the page. Kept
    /// here, once, rather than as parallel rules at each caller — a reader
    /// session's live check and a save's after-the-fact one had already
    /// drifted on exactly this point.
    pub fn is_unfilled_required(&self) -> bool {
        self.required && self.is_reachable() && self.value.is_empty() && self.selected.is_empty()
    }
}

/// How a text selection was asked for (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextSelection {
    Range { anchor: PagePoint, head: PagePoint },
    Word { at: PagePoint },
    Line { at: PagePoint },
}

/// What the page's text layer said (§6.3).
///
/// A page with no extractable text returns an empty result rather than an
/// error, and the UI reports that the highlighter is unavailable there.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TextSelectionResult {
    /// One quadrilateral per contiguous run, in reading order.
    pub quads: Vec<PageQuad>,
    pub text: String,
    /// True when the selection was cut to fit a protocol bound.
    pub truncated: bool,
}

impl TextSelectionResult {
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
}

/// One thing to do to the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentCommand {
    Annotation(AnnotationCommand),
    SetField {
        name: String,
        value: String,
        /// Which options are chosen, by index, for a choice field that takes
        /// several.
        ///
        /// The same fact [`UndoOperation::SetField`] carries and for the same
        /// reason: one string cannot name three selections, so a transaction
        /// that fills a multi-select list box has to say so here or lose two
        /// of them. Empty for every other kind, and absent from journals
        /// written before it existed — which is why it defaults rather than
        /// being required.
        #[serde(default)]
        selected: Vec<u32>,
    },
    /// An edit to the document's outline tree (§12.3.3 of the PDF spec: the
    /// bookmarks). Addressed by tree path, which the revision check makes
    /// safe — see [`pulpit_core::navigation::BookmarkPath`].
    Bookmark(pulpit_core::navigation::BookmarkCommand),
}

impl DocumentCommand {
    pub fn label(&self) -> String {
        match self {
            DocumentCommand::Annotation(command) => command.label(),
            DocumentCommand::SetField { name, .. } => format!("Fill {name}"),
            DocumentCommand::Bookmark(command) => command.label().to_string(),
        }
    }
}

/// One atomic user action: a single command for an ordinary edit, several for
/// an eraser sweep or a compound replacement.
///
/// One transaction is one revision increment and one undo entry (§9.1), and it
/// is applied whole or not at all (§9.5).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DocumentTransaction(pub Vec<DocumentCommand>);

impl DocumentTransaction {
    pub fn one(command: DocumentCommand) -> DocumentTransaction {
        DocumentTransaction(vec![command])
    }

    /// Every annotation command a gesture produced, as one transaction.
    pub fn from_annotations(
        commands: impl IntoIterator<Item = AnnotationCommand>,
    ) -> DocumentTransaction {
        DocumentTransaction(
            commands
                .into_iter()
                .map(DocumentCommand::Annotation)
                .collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// What the undo menu calls this action.
    ///
    /// A sweep that erased eleven marks is "Erase", not eleven entries: the
    /// user made one gesture.
    pub fn label(&self) -> String {
        match self.0.as_slice() {
            [] => "Nothing".to_string(),
            [single] => single.label(),
            [first, ..] => first.label(),
        }
    }

    /// Check the transaction against the declared limits before anything is
    /// allocated for it (A8).
    pub fn validate(&self) -> Result<(), limits::LimitExceeded> {
        limits::within(
            "operations per transaction",
            self.0.len(),
            limits::MAX_OPERATIONS_PER_TRANSACTION,
        )?;
        let mut points = 0usize;
        for command in &self.0 {
            match command {
                DocumentCommand::Annotation(annotation) => {
                    if let Some(AnnotationDraft::Ink(ink)) = annotation.draft() {
                        points = points.saturating_add(ink.points.len());
                        limits::within("ink points", ink.points.len(), limits::MAX_POINTS_PER_INK)?;
                    }
                    if let Some(AnnotationDraft::Highlight(highlight)) = annotation.draft() {
                        limits::within(
                            "quadrilaterals",
                            highlight.quads.len(),
                            limits::MAX_QUADS_PER_ANNOTATION,
                        )?;
                    }
                }
                DocumentCommand::SetField { value, .. } => {
                    limits::within("field value", value.len(), limits::MAX_FIELD_VALUE_BYTES)?;
                }
                DocumentCommand::Bookmark(bookmark) => {
                    use pulpit_core::navigation::{
                        BookmarkCommand, MAX_OUTLINE_DEPTH, MAX_OUTLINE_TITLE_CHARS,
                    };
                    let (path, title) = match bookmark {
                        BookmarkCommand::Create { path, title, .. }
                        | BookmarkCommand::Rename { path, title } => (path, Some(title)),
                        BookmarkCommand::Delete { path } => (path, None),
                    };
                    limits::within("bookmark path depth", path.len(), MAX_OUTLINE_DEPTH)?;
                    if let Some(title) = title {
                        limits::within(
                            "bookmark title",
                            title.chars().count(),
                            MAX_OUTLINE_TITLE_CHARS,
                        )?;
                    }
                }
            }
        }
        limits::within(
            "ink points per transaction",
            points,
            limits::MAX_POINTS_PER_TRANSACTION,
        )
    }
}

/// What one command in a transaction did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppliedEffect {
    /// An annotation was created or replaced; this is what it is now.
    Annotation(Box<AnnotationSummary>),
    /// An annotation was deleted.
    Deleted(AnnotationId),
    /// A field now holds this value. The engine's value, not the requested
    /// one: a checkbox asked for "yes" reports the export value it actually
    /// took.
    Field { name: String, value: String },
    /// The outline tree was edited; this is the whole of what it now is.
    ///
    /// The whole tree rather than a delta, because a bookmark edit repaints no
    /// page — nothing else would tell the rail what to show, and the tree is
    /// bounded to [`pulpit_core::navigation::MAX_OUTLINE_ENTRIES`].
    Outline(Box<pulpit_core::navigation::Outline>),
}

/// What the engine has to keep in order to reverse one transaction (§6.2).
///
/// Opaque and serialisable: a lossless before-image, never a PDFium pointer.
/// It preserves unrecognised dictionary data, so undoing an edit to an
/// imported annotation puts back what was there rather than what pulpit
/// understood of it (A5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentUndo {
    /// The operations that reverse the transaction, in the order to apply
    /// them — the inverse of the forward order, so a sweep is put back the way
    /// it was taken.
    pub operations: Vec<UndoOperation>,
    /// The revision this undoes back to, for diagnostics and for the journal's
    /// replay order. Not a precondition: applying an undo checks the *current*
    /// revision like any other mutation.
    pub restores: DocumentRevision,
    /// What the undo menu calls the action being reversed.
    pub label: String,
}

/// One step of an undo.
///
/// Deliberately not a [`DocumentCommand`]. Reversing a delete has to put the
/// annotation back under *its own identity*, and `Create` cannot: it mints a
/// new one. An undo/redo cycle that renamed every annotation it touched would
/// break A3, and with it every reference the session holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UndoOperation {
    /// Put a deleted annotation back, with its identity and its unrecognised
    /// dictionary entries (A5).
    RestoreAnnotation {
        id: AnnotationId,
        before: Box<AnnotationBeforeImage>,
    },
    /// Put an edited annotation back the way it was.
    ReplaceAnnotation {
        id: AnnotationId,
        before: Box<AnnotationBeforeImage>,
    },
    /// Remove an annotation that was created.
    DeleteAnnotation { id: AnnotationId },
    /// Put a field's previous value back.
    SetField {
        name: String,
        value: String,
        /// Which options were chosen, by index, for a choice field that takes
        /// several. One string cannot name three selections, which is exactly
        /// the case [`FormField::selected`] exists for; empty for every other
        /// kind, and absent from journals written before it existed.
        #[serde(default)]
        selected: Vec<u32>,
    },
    /// Put a deleted bookmark back where it was, subtree and all.
    InsertBookmark {
        path: pulpit_core::navigation::BookmarkPath,
        before: Box<pulpit_core::navigation::OutlineEntry>,
    },
    /// Remove a bookmark that was created.
    RemoveBookmark {
        path: pulpit_core::navigation::BookmarkPath,
    },
    /// Put a bookmark's previous title back.
    RetitleBookmark {
        path: pulpit_core::navigation::BookmarkPath,
        title: String,
    },
}

/// A lossless copy of what an annotation was before it was changed.
///
/// `preserved` is the part that matters for A5: any dictionary entry pulpit
/// did not model, kept as opaque bytes so undoing an edit to an imported
/// annotation restores what the other producer wrote rather than pulpit's
/// understanding of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationBeforeImage {
    pub page: PageIndex,
    /// The modelled part, when pulpit understood the annotation well enough to
    /// describe it. `None` for an annotation that was preserved rather than
    /// modelled — which is also an annotation the editor never offers to
    /// change, so nothing can produce this case today.
    pub draft: Option<AnnotationDraft>,
    /// Everything pulpit did not model, as the engine chooses to encode it.
    pub preserved: Vec<u8>,
}

/// What a successful mutation reports (§6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    /// One per command, in order.
    pub effects: Vec<AppliedEffect>,
    pub document_revision: DocumentRevision,
    /// A rectangle covering the whole transaction, for a partial repaint.
    /// Full-page rendering stays the correct baseline (§9.4).
    pub dirty_region: Option<PageRect>,
    /// The pages the transaction touched, so a caller knows what to re-render.
    pub dirty_pages: Vec<PageIndex>,
    /// The operation that reverses this one. Applying it is itself a mutation
    /// and returns an `Applied` whose `undo` redoes it, so redo needs no
    /// request of its own (§9.5).
    pub undo: DocumentUndo,
}

/// What to do when saving (§11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SaveOptions {
    /// Write incrementally, appending a new revision rather than rewriting the
    /// file. Preserves any existing signature's byte ranges, at the cost of a
    /// larger file.
    pub incremental: bool,
    /// Re-open the written file and check it before renaming into place. On by
    /// default in every path that matters; off only for a test that wants the
    /// raw bytes.
    pub verify: bool,
}

impl SaveOptions {
    /// The options every ordinary Save As uses.
    pub fn verified() -> SaveOptions {
        SaveOptions {
            incremental: false,
            verify: true,
        }
    }
}

/// What a completed save reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedDocument {
    pub path: std::path::PathBuf,
    /// The revision installed in the output (§9.1).
    pub revision: DocumentRevision,
    pub bytes: u64,
    /// The identities of every annotation the verification pass found in the
    /// reopened file. Empty when verification was not asked for.
    pub verified_ids: Vec<AnnotationId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::annotate::{IdGenerator, InkDraft, InkPoint};

    fn ink_command(points: usize) -> DocumentCommand {
        DocumentCommand::Annotation(AnnotationCommand::Create(AnnotationDraft::Ink(InkDraft {
            page: PageIndex(0),
            points: vec![InkPoint::new(1.0, 1.0); points],
            style: MarkStyle::default(),
        })))
    }

    #[test]
    fn revisions_start_at_zero_and_only_go_up() {
        let start = DocumentRevision::INITIAL;
        assert_eq!(start.0, 0);
        assert_eq!(start.next(), DocumentRevision(1));
        assert!(start.next() > start);
        assert_eq!(DocumentRevision(7).to_string(), "r7");
    }

    #[test]
    fn a_transaction_is_checked_against_the_limits_before_it_is_sent() {
        assert!(DocumentTransaction(vec![ink_command(10)])
            .validate()
            .is_ok());

        let too_many = DocumentTransaction(vec![
            ink_command(1);
            limits::MAX_OPERATIONS_PER_TRANSACTION + 1
        ]);
        assert!(too_many.validate().is_err());

        let too_long = DocumentTransaction(vec![ink_command(limits::MAX_POINTS_PER_INK + 1)]);
        assert!(too_long.validate().is_err());

        // Each stroke is legal on its own; together they are not.
        let strokes = limits::MAX_POINTS_PER_TRANSACTION / limits::MAX_POINTS_PER_INK + 1;
        let too_much = DocumentTransaction(vec![ink_command(limits::MAX_POINTS_PER_INK); strokes]);
        assert!(too_much.validate().is_err());
    }

    #[test]
    fn an_over_long_field_value_is_refused_by_the_transaction() {
        let transaction = DocumentTransaction::one(DocumentCommand::SetField {
            name: "comments".into(),
            value: "x".repeat(limits::MAX_FIELD_VALUE_BYTES + 1),
            selected: Vec::new(),
        });
        assert!(transaction.validate().is_err());
    }

    #[test]
    fn one_sweep_is_labelled_as_one_action_however_many_marks_it_took() {
        let mut generator = IdGenerator::new(0);
        let sweep =
            DocumentTransaction::from_annotations((0..11).map(|_| AnnotationCommand::Delete {
                id: generator.next_id(),
            }));
        assert_eq!(sweep.len(), 11);
        assert_eq!(sweep.label(), "Erase");
        assert_eq!(DocumentTransaction::default().label(), "Nothing");
    }

    #[test]
    fn a_fields_editor_goes_at_its_first_widget_on_the_page() {
        let field = FormField {
            name: "choice".into(),
            kind: FieldKind::RadioGroup,
            value: "b".into(),
            read_only: false,
            format: FieldFormat::Plain,
            options: vec!["a".into(), "b".into()],
            allows_custom_value: false,
            multiple_selection: false,
            required: false,
            password: false,
            file_select: false,
            rich_text: false,
            truncated: false,
            hidden: false,
            selected: Vec::new(),
            widgets: vec![
                FieldWidget {
                    page: PageIndex(1),
                    bounds: PageRect::new(10.0, 10.0, 24.0, 24.0),
                    option: Some("a".into()),
                },
                FieldWidget {
                    page: PageIndex(1),
                    bounds: PageRect::new(10.0, 40.0, 24.0, 54.0),
                    option: Some("b".into()),
                },
                FieldWidget {
                    page: PageIndex(3),
                    bounds: PageRect::new(90.0, 40.0, 104.0, 54.0),
                    option: None,
                },
            ],
        };
        assert_eq!(
            field.anchor_on(PageIndex(1)),
            Some(PageRect::new(10.0, 10.0, 24.0, 24.0))
        );
        assert_eq!(field.anchor_on(PageIndex(0)), None);
        assert_eq!(field.pages(), vec![PageIndex(1), PageIndex(3)]);
        assert!(field.is_editable());
    }

    #[test]
    fn a_read_only_or_unfillable_field_is_shown_and_not_edited() {
        let mut field = FormField {
            name: "total".into(),
            kind: FieldKind::Text,
            value: "42".into(),
            read_only: true,
            format: FieldFormat::Plain,
            options: Vec::new(),
            allows_custom_value: false,
            multiple_selection: false,
            required: false,
            password: false,
            file_select: false,
            rich_text: false,
            truncated: false,
            hidden: false,
            selected: Vec::new(),
            widgets: Vec::new(),
        };
        assert!(!field.is_editable());
        field.read_only = false;
        assert!(field.is_editable());
        field.kind = FieldKind::Signature;
        assert!(
            !field.is_editable(),
            "a signature field is not filled by the form editor"
        );
        assert!(!FieldKind::PushButton.is_fillable());
    }

    /// A required text field, empty, nothing else marked — the base case the
    /// test below flips one bit on at a time.
    fn unfilled_required_field() -> FormField {
        FormField {
            name: "signer-name".into(),
            kind: FieldKind::Text,
            value: String::new(),
            read_only: false,
            format: FieldFormat::Plain,
            options: Vec::new(),
            allows_custom_value: false,
            multiple_selection: false,
            required: true,
            password: false,
            file_select: false,
            rich_text: false,
            truncated: false,
            hidden: false,
            selected: Vec::new(),
            widgets: Vec::new(),
        }
    }

    #[test]
    fn is_unfilled_required_is_the_one_rule_both_callers_share() {
        // The rule this covers used to be written twice — once for the live
        // reader session, once for the after-save report — and had drifted
        // to disagree on a hidden field. Now there is one function, and this
        // is its test: every caller that agrees with it agrees with each
        // other for free.
        assert!(unfilled_required_field().is_unfilled_required());

        let mut filled = unfilled_required_field();
        filled.value = "Ada".into();
        assert!(!filled.is_unfilled_required());

        let mut not_required = unfilled_required_field();
        not_required.required = false;
        assert!(!not_required.is_unfilled_required());

        // Reachable, not merely editable: a required field the document
        // hides is one nobody can fill, and must not be reported as
        // something to go and type into.
        let mut hidden = unfilled_required_field();
        hidden.hidden = true;
        assert!(!hidden.is_unfilled_required());

        let mut chosen = unfilled_required_field();
        chosen.kind = FieldKind::ListBox;
        chosen.selected = vec![1];
        assert!(!chosen.is_unfilled_required());
    }

    #[test]
    fn compatibility_levels_say_what_they_permit() {
        assert!(CompatibilityLevel::Native.allows_form_filling());
        assert!(CompatibilityLevel::NativeWithLimitations.allows_form_filling());
        assert!(!CompatibilityLevel::AnnotateOnly.allows_form_filling());
        assert!(CompatibilityLevel::AnnotateOnly.allows_annotation());
        assert!(!CompatibilityLevel::Unsupported.allows_annotation());
        for level in [
            CompatibilityLevel::Native,
            CompatibilityLevel::NativeWithLimitations,
            CompatibilityLevel::AnnotateOnly,
            CompatibilityLevel::Unsupported,
        ] {
            assert!(!level.label().is_empty());
        }
    }

    #[test]
    fn every_warning_says_something_a_user_can_act_on() {
        for warning in [
            DocumentWarning::Encrypted,
            DocumentWarning::XfaForm,
            DocumentWarning::JavaScript,
            DocumentWarning::Signed,
            DocumentWarning::MutationForbidden,
            DocumentWarning::FormUnavailable,
            DocumentWarning::ScriptReachesOut,
            DocumentWarning::ButtonAction,
        ] {
            assert!(warning.message().len() > 30, "{warning:?}");
        }
        // A9: the signature warning must not claim a saved copy stays valid.
        assert!(DocumentWarning::Signed.message().contains("not carry"));
    }

    #[test]
    fn a_summary_converts_to_the_shape_hit_testing_wants() {
        let summary = AnnotationSummary {
            id: IdGenerator::new(1).next_id(),
            page: PageIndex(0),
            kind: AnnotationKind::Ink,
            bounds: PageRect::new(0.0, 0.0, 10.0, 10.0),
            style: MarkStyle::default(),
            contents: AnnotationContents::default(),
            support: AnnotationSupport::Editable,
            revision: DocumentRevision(3),
            path: vec![PagePoint::new(0.0, 0.0), PagePoint::new(10.0, 10.0)],
            quads: Vec::new(),
            geometry_elided: false,
            stamp: None,
        };
        let hit = summary.to_hit();
        assert_eq!(hit.id, summary.id);
        assert!(hit.editable);
        assert_eq!(hit.width, summary.style.width);
        assert!(summary.editable());
    }

    /// A box and an ellipse are their rectangle, so the summary carries
    /// everything a move or a resize needs to build the replacement with.
    #[test]
    fn a_shape_round_trips_through_the_summary_and_moves_and_resizes() {
        use pulpit_core::annotate::{AnnotationDraft, ShapeOutline};

        for (kind, outline) in [
            (AnnotationKind::Square, ShapeOutline::Box),
            (AnnotationKind::Circle, ShapeOutline::Ellipse),
        ] {
            let summary = AnnotationSummary {
                id: IdGenerator::new(1).next_id(),
                page: PageIndex(2),
                kind,
                bounds: PageRect::new(10.0, 20.0, 110.0, 70.0),
                style: MarkStyle::default(),
                contents: AnnotationContents::default(),
                support: AnnotationSupport::Editable,
                revision: DocumentRevision(3),
                path: Vec::new(),
                quads: Vec::new(),
                geometry_elided: false,
                stamp: None,
            };
            let draft = summary.to_draft().expect("a shape pulpit models");
            let AnnotationDraft::Shape(shape) = &draft else {
                panic!("{kind:?} drafts as a shape")
            };
            assert_eq!(shape.outline, outline);
            assert_eq!(shape.rect, summary.bounds);
            assert_eq!(shape.page, PageIndex(2));

            // Freely movable, and moving it moves the rectangle that is the
            // whole of it (§8.4).
            assert!(kind.is_freely_movable());
            let moved = draft.translated(5.0, -5.0).expect("a shape moves");
            assert_eq!(moved.bounds(), Some(PageRect::new(15.0, 15.0, 115.0, 65.0)));
            let bigger = PageRect::new(10.0, 20.0, 210.0, 120.0);
            assert!(draft.is_resizable());
            assert_eq!(
                draft
                    .resized(summary.bounds, bigger)
                    .and_then(|d| d.bounds()),
                Some(bigger)
            );
        }
    }

    #[test]
    fn an_info_string_is_collapsed_to_one_line_and_never_laid_out() {
        // A producer cannot push a permission row off the dialog with
        // newlines, and cannot draw anything with control characters.
        let value = InfoText::read("  Quarterly\n\n\treport\u{7}  ").expect("some text");
        assert_eq!(value.text, "Quarterly report");
        assert!(!value.truncated);
        assert_eq!(value.to_string(), "Quarterly report");
    }

    #[test]
    fn a_key_that_says_nothing_is_absent_rather_than_empty() {
        assert!(InfoText::read("").is_none());
        assert!(InfoText::read(" \n\t ").is_none());
        assert!(InfoText::read("\u{0}\u{1}").is_none());
    }

    #[test]
    fn a_long_info_string_is_cut_at_the_bound_and_says_so() {
        let long = "é".repeat(limits::MAX_INFO_TEXT_BYTES);
        let value = InfoText::read(&long).expect("some text");
        assert!(value.truncated);
        assert!(value.text.len() <= limits::MAX_INFO_TEXT_BYTES);
        // Cut on a character boundary: the string is still valid UTF-8 and
        // ends in a whole character.
        assert!(value.text.chars().all(|character| character == 'é'));
        assert!(value.to_string().ends_with('…'));
    }

    #[test]
    fn a_pdf_date_is_read_as_the_local_time_its_producer_wrote() {
        let date = DocumentDate::read("D:20240115103005+01'30'").expect("a date");
        let time = date.parsed.expect("a parsed date");
        assert_eq!(time.year, 2024);
        assert_eq!(time.month, 1);
        assert_eq!(time.day, 15);
        assert_eq!(time.offset_minutes, Some(90));
        assert_eq!(date.to_string(), "2024-01-15 10:30:05 UTC+01:30");
        // The optional tail, the two shorthands, and no offset at all.
        assert_eq!(
            DocumentDate::read("D:2024").expect("a date").to_string(),
            "2024-01-01 00:00:00"
        );
        assert_eq!(
            DocumentDate::read("D:20240115103005Z")
                .expect("a date")
                .to_string(),
            "2024-01-15 10:30:05 UTC"
        );
        assert_eq!(
            DocumentDate::read("D:20240115103005-0500")
                .expect("a date")
                .to_string(),
            "2024-01-15 10:30:05 UTC-05:00"
        );
    }

    #[test]
    fn a_date_that_does_not_parse_is_shown_as_the_document_wrote_it() {
        // Never a date assembled from the digits that happened to parse: a
        // half-read date is a wrong one, and the raw string is at least true.
        for raw in ["yesterday", "D:2024AB15", "D:20241315", "D:20240100"] {
            let date = DocumentDate::read(raw).expect("a raw string");
            assert!(date.parsed.is_none(), "{raw}");
            assert_eq!(date.to_string(), raw);
        }
        assert!(DocumentDate::read("  ").is_none());
    }

    #[test]
    fn permissions_are_read_from_the_specifications_bit_numbers() {
        // Bit 6 clear is the one `survey` already turns into
        // `MutationForbidden`; the properties view names the other seven.
        let permissions = DocumentPermissions::from_bits(!(1u32 << 5));
        assert!(!permissions.annotate);
        assert!(permissions.print && permissions.modify && permissions.fill_forms);
        assert!(!permissions.is_unrestricted());
        assert!(DocumentPermissions::from_bits(!0).is_unrestricted());
        assert!(DocumentPermissions::UNRESTRICTED.is_unrestricted());
        // Print is bit 3, and nothing else is on when only it is.
        let print_only = DocumentPermissions::from_bits(1 << 2);
        assert!(print_only.print);
        assert_eq!(
            print_only
                .each()
                .iter()
                .filter(|(_, allowed)| *allowed)
                .count(),
            1
        );
    }

    #[test]
    fn a_backend_with_no_metadata_reports_the_shape_and_claims_nothing_else() {
        let info = OpenDocumentInfo {
            page_count: 12,
            level: CompatibilityLevel::ViewOnly,
            warnings: vec![DocumentWarning::Encrypted],
            first_page: PageGeometry::upright(612.0, 792.0),
            has_form: false,
        };
        let properties = DocumentProperties::from_info(&info);
        assert_eq!(properties.page_count, 12);
        assert_eq!(properties.level, CompatibilityLevel::ViewOnly);
        assert_eq!(properties.warnings, info.warnings);
        assert!(properties.title.is_none() && properties.producer.is_none());
        assert!(properties.version.is_none() && properties.encryption.is_none());
        // Not measured, rather than claimed uniform.
        assert_eq!(properties.page_sizes, PageSizes::Unmeasured);
        assert!(properties.permissions.is_unrestricted());
    }

    #[test]
    fn a_page_is_described_in_millimetres_by_name_and_in_points() {
        let a4 = describe_page_size(&PageGeometry::upright(595.0, 842.0));
        assert_eq!(a4, "210 × 297 mm (A4 portrait), 595 × 842 pt");
        // The same sheet turned over is still A4.
        let landscape = describe_page_size(&PageGeometry::upright(842.0, 595.0));
        assert!(landscape.contains("A4 landscape"), "{landscape}");
        // US Letter, and a size with no name at all.
        assert!(describe_page_size(&PageGeometry::upright(612.0, 792.0)).contains("Letter"));
        let odd = describe_page_size(&PageGeometry::upright(400.0, 400.0));
        assert_eq!(odd, "141 × 141 mm (square), 400 × 400 pt");
    }

    #[test]
    fn a_user_unit_makes_the_page_physically_larger() {
        // A drawing that scales its points is that much bigger on paper, and
        // saying otherwise would describe a sheet nobody could print.
        let mut oversized = PageGeometry::upright(595.0, 842.0);
        oversized.user_unit = 2.0;
        let described = describe_page_size(&oversized);
        assert!(described.starts_with("420 × 594 mm (A2"), "{described}");
    }

    #[test]
    fn an_unsupported_annotation_is_never_reported_editable() {
        for support in [
            AnnotationSupport::ReadOnlySupported,
            AnnotationSupport::Unsupported,
            AnnotationSupport::Malformed,
        ] {
            assert!(!support.is_editable(), "{support:?}");
        }
        assert!(AnnotationSupport::Editable.is_editable());
    }
}
