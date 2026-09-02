//! Presenter annotations: temporary ink, highlighting, text and erasing.
//!
//! Everything here is in the same normalised top-left page coordinates as
//! [`crate::notes::Region`], so an annotation drawn on the presenter screen
//! lands on exactly the same part of the page on the audience screen, through
//! whatever letterboxing or `/FitR` crop each window happens to be applying.
//! Storing pixels instead would tie a mark to the panel it was drawn in, and
//! the two panels are never the same size.
//!
//! What lives here is the *unfinished* gesture, and a view of the finished
//! ones. A stroke under the pen, the pointer, the spotlight and a label being
//! typed are this type's own and never reach the file (A2, criterion 6). A
//! stroke that is finished is an annotation in the open document, and what is
//! held here is a copy of it for drawing — named with the annotation it shows,
//! and read back out of the document on a page turn rather than stashed
//! beside it.
//!
//! That is a change from what this file used to be. Presenter marks were once
//! temporary by design, kept in a per-slide cache and written out, if at all,
//! by stamping them into a copy of the deck. There is one representation now
//! (A1): the same mark is the same annotation in presentation and in document
//! mode, and both edit it through the same engine, so a mark made at the
//! lectern can be found, moved and deleted afterwards like any other.

use serde::{Deserialize, Serialize};

/// What the presenter's pointer is currently doing to the slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationTool {
    /// A dot that follows the pointer: "this line here".
    Pointer,
    /// A lit circle with the rest of the page dimmed around it.
    ///
    /// Not a palette button of its own: it is the other thing the pointer
    /// control can be, chosen in that control's options. Pointing at a line
    /// and lighting up the paragraph around it are the same gesture with the
    /// dimming turned up, so they share one slot rather than competing for
    /// two.
    Spotlight,
    /// Freehand strokes that stay until the slide changes.
    Ink,
    /// A broad translucent stroke that leaves slide content readable.
    Highlighter,
    /// Removes a stroke touched by the eraser gesture.
    Eraser,
    /// Places a text label, then sends keyboard input to it until committed.
    Text,
    /// Places a sticky note: an icon on the page with text behind it.
    ///
    /// Offered in both modes. The note itself is for the reader — a mark that
    /// has to be opened to be read is not one an audience can read — but a
    /// presenter making a note *for afterwards*, out of something that came up
    /// in the room, is the same gesture and lands in the same file.
    Note,
    /// Places a check, a cross or a visible signature.
    ///
    /// Document mode only, and never described as a cryptographic signature
    /// (§1 of `SPEC-document.md`).
    Stamp,
    /// Draws a box, an ellipse, a line or an arrow by dragging, according to
    /// the [`ShapeKind`] it is set to.
    ///
    /// One tool with a mode rather than four tools, for the same reason the
    /// highlighter has three nibs and the band has three kinds: the gesture is
    /// identical — press, drag, let go — and only the mark left behind
    /// differs. Four more buttons would make the palette a rail of icons.
    ///
    /// Document mode only, like the stamp. A box around a figure is a mark
    /// made while reading a paper; drawing one at the lectern is the
    /// presenter's transient painting path, which is a separate engine (§5.3)
    /// and a separate decision.
    Shape,
    /// Drags a rubber band over the page, and does one of three things with
    /// what it encloses, according to the [`SelectKind`] it is set to.
    ///
    /// Picking up *one* mark needs no tool: the hand does that with nothing
    /// armed at all. This is the tool for the things the hand cannot do —
    /// taking several marks at once to delete them in one press (§8.4), and
    /// taking a region of the page itself off to the clipboard.
    ///
    /// Offered in both modes, and it behaves the same in both. A band that
    /// copies a figure is a band the audience watches being drawn, exactly as
    /// they watch one that gathers marks up; there is no reason for the two
    /// windows to disagree about what a rectangle is.
    Select,
    /// Sweeps the page's own text exactly as the highlighter does, and then
    /// leaves no mark: what it produces is a selection that outlives the
    /// drag, for the clipboard and for reading aloud.
    ///
    /// A separate tool rather than a fourth [`MarkupKind`], because the other
    /// three all commit an annotation and this one must never touch the
    /// document; and separate from [`SelectKind::Text`], because that one
    /// bounds an *area* and takes what falls inside it, where this one sweeps
    /// characters the way every text editor's cursor does.
    SelectText,
}

/// What [`AnnotationTool::Select`]'s band does with the region it encloses.
///
/// A mode inside one tool rather than three tools, for the same reason the
/// pointer and the spotlight share a control: the gesture is identical — pull
/// a rectangle over the page — and only the answer differs. Chosen in the
/// tool's options, the way the highlighter's colour is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectKind {
    /// Hold every annotation the band encloses, to move, resize or delete
    /// them together. What the band has always done, and still the default:
    /// it is the only one of the three that can edit the document.
    #[default]
    Marks,
    /// Put the region on the clipboard as an image, rendered fresh at a
    /// chosen scale rather than lifted off the screen. What is on screen is
    /// at the zoom the reader happens to be using, which is not a resolution
    /// anybody chose to paste at.
    Image,
    /// Put the text the region covers on the clipboard.
    ///
    /// A different question from the one a text drag asks: this one bounds an
    /// area and takes what falls inside it, which is what gets one column out
    /// of a two-column page.
    Text,
}

impl SelectKind {
    /// Every kind, in the order the options panel offers them: the one that
    /// works on any page at all comes first.
    pub const ALL: [SelectKind; 3] = [SelectKind::Marks, SelectKind::Image, SelectKind::Text];

    /// The word the option's tooltip says. The buttons themselves are
    /// icon-only, so this one word is the whole of the explanation and it
    /// names what the band takes rather than how: "Annotations", not "hold
    /// the marks the band encloses".
    pub fn label(self) -> &'static str {
        match self {
            SelectKind::Marks => "Annotations",
            SelectKind::Image => "Image",
            SelectKind::Text => "Text",
        }
    }

    /// Does this kind put something on the clipboard rather than gather marks?
    ///
    /// The one question the gesture code asks: a band that copies never
    /// touches the selection, and a band that holds marks never reaches the
    /// clipboard.
    pub fn copies(self) -> bool {
        !matches!(self, SelectKind::Marks)
    }
}

/// Which of the three text-markup marks the highlighter lays down.
///
/// A mode inside one tool rather than three tools, for the same reason
/// [`SelectKind`] is: the gesture is identical — sweep the pointer across text
/// and let go — and only the mark left behind differs. Chosen in the tool's
/// options, beside the colour, because a reader who wants the words underlined
/// rather than washed is already looking there for the colour to underline
/// them in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MarkupKind {
    /// `/Highlight`: a translucent wash over the words. The default, and the
    /// only one of the three that leaves the page looking marked from across a
    /// room.
    #[default]
    Highlight,
    /// `/Underline`: a rule along the baseline of each run.
    Underline,
    /// `/StrikeOut`: a rule through the middle of each run.
    StrikeOut,
}

impl MarkupKind {
    /// Every kind, in the order the options panel offers them: the wash the
    /// tool is named for comes first.
    pub const ALL: [MarkupKind; 3] = [
        MarkupKind::Highlight,
        MarkupKind::Underline,
        MarkupKind::StrikeOut,
    ];

    /// How thick a rule is, as a fraction of the run's height.
    pub const RULE_THICKNESS: f32 = 0.07;

    /// The word the option's tooltip says. The buttons are icon-only, like the
    /// band's, so this one word is the whole of the explanation.
    pub fn label(self) -> &'static str {
        match self {
            MarkupKind::Highlight => "Highlight",
            MarkupKind::Underline => "Underline",
            MarkupKind::StrikeOut => "Strikeout",
        }
    }

    /// The opacity the mark is laid down at.
    ///
    /// A wash has to let the words through it, so it is translucent. A rule is
    /// not drawn over the text at all — it sits under it or across it — and a
    /// translucent one only looks faded.
    pub fn opacity(self) -> f32 {
        match self {
            MarkupKind::Highlight => 0.4,
            MarkupKind::Underline | MarkupKind::StrikeOut => 1.0,
        }
    }

    /// Does this kind fill the run, or draw a rule across it?
    pub fn is_wash(self) -> bool {
        matches!(self, MarkupKind::Highlight)
    }

    /// Where the rule sits inside a run, as a fraction of the run's height
    /// measured from its top, for the kinds that draw one.
    ///
    /// A quad from text extraction is the line's box, not its glyphs: the
    /// baseline sits a little above the bottom of it, and the middle of the
    /// lower-case letters a little above the middle. These are those two
    /// places, and they are what stops an underline reading as the next line's
    /// overline.
    pub fn rule_at(self) -> Option<f32> {
        match self {
            MarkupKind::Highlight => None,
            MarkupKind::Underline => Some(0.88),
            MarkupKind::StrikeOut => Some(0.52),
        }
    }
}

/// Which mark [`AnnotationTool::Shape`] draws.
///
/// A mode inside one tool rather than four tools, for the same reason
/// [`MarkupKind`] and [`SelectKind`] are: the gesture is one drag, and only
/// what it leaves behind differs. Chosen in the tool's options, beside the
/// colour and the width it is drawn with.
///
/// The four are not one family in the file, and deliberately so. A box and an
/// ellipse become the `/Square` and `/Circle` annotations PDF has for exactly
/// them, which Okular and Acrobat show as shapes and let their own users
/// edit. A line and an arrow become `/Ink`: `/Line` carries its endpoints in
/// `/L` and its arrowheads in `/LE`, both of them arrays, and PDFium's
/// annotation API can write neither — a `/Line` without `/L` is malformed,
/// and a malformed annotation travels worse than an honest stroke. An arrow
/// drawn as ink is a real, editable, universally drawn mark in every viewer,
/// which a rasterised stamp of one would not be.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShapeKind {
    /// A box: `/Square`. The default, and what "put a box around this figure"
    /// means.
    #[default]
    Rectangle,
    /// An ellipse inscribed in the drag: `/Circle`.
    Ellipse,
    /// A straight line from where the drag began to where it ended: `/Ink`.
    Line,
    /// A line with a head at the end the drag finished on: `/Ink`.
    ///
    /// The end, not the start, because an arrow is aimed: the hand starts
    /// away from the thing and finishes on it.
    Arrow,
}

impl ShapeKind {
    /// Every kind, in the order the options panel offers them: the box first,
    /// because it is what the tool is reached for.
    pub const ALL: [ShapeKind; 4] = [
        ShapeKind::Rectangle,
        ShapeKind::Ellipse,
        ShapeKind::Line,
        ShapeKind::Arrow,
    ];

    /// The word the option's tooltip says. The buttons are icon-only, like the
    /// highlighter's and the band's, so this one word is the explanation.
    pub fn label(self) -> &'static str {
        match self {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Line => "Line",
            ShapeKind::Arrow => "Arrow",
        }
    }
}

/// Which mark [`AnnotationTool::Stamp`] puts down.
///
/// The two a button can hold, and only those. A stamp can also carry a
/// picture — `StampMark::Image` — but a picture is something a reader
/// supplies rather than a mode a palette offers, so it is not one of these.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StampChoice {
    /// A tick: "read", "done", "yes".
    #[default]
    Check,
    /// A cross. Never "wrong" in the interface's own words — what a mark
    /// means is the reader's, and the tooltip says what it draws.
    Cross,
}

impl StampChoice {
    pub const ALL: [StampChoice; 2] = [StampChoice::Check, StampChoice::Cross];

    pub fn label(self) -> &'static str {
        match self {
            StampChoice::Check => "Check",
            StampChoice::Cross => "Cross",
        }
    }
}

impl AnnotationTool {
    /// The tools the presenter's palette offers a control for, in the order it
    /// draws them. [`AnnotationTool::Spotlight`] is absent because it is armed
    /// from the pointer control's options rather than from a button of its own.
    ///
    /// Every mark [`AnnotationTool::DOCUMENT`] offers is here too, in the same
    /// order, plus the pointer. Every mark a presenter makes is an annotation
    /// in the open document (A1), so a tool that document mode has and
    /// presentation does not is not a restraint on what goes into the file —
    /// it is only a mark the presenter has to stop and change mode to make.
    /// What presentation has and document mode does not is the pointer, which
    /// makes no mark at all: it is a thing you do to a slide in front of an
    /// audience, and there is nothing to keep afterwards.
    ///
    /// The two document mode has and this does not are the stamp and the
    /// shape tool, which are placed and drawn by pointing at a page rather
    /// than at a slide, and which the presenter's own transient painting path
    /// (§5.3) has no gesture for. That is a gap in this palette rather than a
    /// rule about it, and closing it is its own piece of work.
    pub const ALL: [AnnotationTool; 8] = [
        AnnotationTool::Pointer,
        AnnotationTool::Select,
        AnnotationTool::Ink,
        AnnotationTool::Highlighter,
        AnnotationTool::Text,
        AnnotationTool::Note,
        AnnotationTool::Eraser,
        AnnotationTool::SelectText,
    ];

    /// The tools a document layout's `AnnotationTools` widget offers, in the
    /// order it draws them.
    ///
    /// [`AnnotationTool::ALL`] without the pointer — which makes no mark and
    /// has nothing to leave behind — and with the two marks a page is pointed
    /// at to make: the stamp and the shape tool. See `ALL` for why that is a
    /// gap in the palette there rather than a rule here.
    pub const DOCUMENT: [AnnotationTool; 9] = [
        AnnotationTool::Select,
        AnnotationTool::Ink,
        AnnotationTool::Highlighter,
        // Beside the pen: the shape tool is the pen for the marks a hand
        // cannot draw straight.
        AnnotationTool::Shape,
        AnnotationTool::Text,
        AnnotationTool::Note,
        // Beside the note: both are placed by a click rather than drawn, and
        // both say something about the page rather than marking it.
        AnnotationTool::Stamp,
        AnnotationTool::Eraser,
        AnnotationTool::SelectText,
    ];

    /// Has this tool anything to configure — a colour, a size, a mode?
    ///
    /// The band has no colour and no size — a rubber band is a shape the hand
    /// makes — but it does have a [`SelectKind`], which is the same sort of
    /// choice as the highlighter's colour and belongs in the same place. The
    /// text selection has none: a selection looks the way selections look,
    /// and a colour chooser for one would be a control that changes nothing
    /// anyone keeps. Everything else has at least a mark to choose or a
    /// colour to choose it in — the stamp has both, now that which mark it
    /// puts down is a mode of the tool rather than a palette of its own and
    /// it puts that mark down in the pen's ink.
    pub fn has_options(self) -> bool {
        !matches!(self, AnnotationTool::SelectText)
    }

    pub fn label(self) -> &'static str {
        match self {
            AnnotationTool::Pointer => "Pointer",
            AnnotationTool::Spotlight => "Spotlight",
            AnnotationTool::Ink => "Ink",
            AnnotationTool::Highlighter => "Highlighter",
            AnnotationTool::Eraser => "Eraser",
            AnnotationTool::Text => "Text",
            AnnotationTool::Note => "Note",
            AnnotationTool::Stamp => "Stamp",
            AnnotationTool::Shape => "Shape",
            AnnotationTool::Select => "Select",
            AnnotationTool::SelectText => "Select text",
        }
    }
}

/// How a freehand stroke is composited over the page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StrokeKind {
    #[default]
    Ink,
    Highlight,
}

impl StrokeKind {
    pub fn opacity(self) -> f32 {
        match self {
            StrokeKind::Ink => 1.0,
            StrokeKind::Highlight => 0.38,
        }
    }
}

/// The ink colours on offer.
///
/// Six named ones and a mixed one, in that order of prominence. The colours are
/// chosen to stay legible over both a white slide and a dark one, which an
/// arbitrary colour is not — so they are what the palette shows, and mixing
/// is a deliberate step past them rather than the first thing on offer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InkColor {
    #[default]
    Black,
    /// Legible on a white slide, which most slides are.
    Red,
    Yellow,
    Cyan,
    Green,
    White,
    /// A colour the presenter mixed, in sRGB bytes.
    ///
    /// The named colours above are the ones that survive being projected onto somebody
    /// else's slide, and they stay the palette. This is the escape hatch for
    /// the deck that is entirely one of them already — a red diagram wants
    /// anything but red ink — and for matching a house colour.
    Custom {
        red: u8,
        green: u8,
        blue: u8,
    },
}

impl InkColor {
    pub const ALL: [InkColor; 6] = [
        InkColor::Black,
        InkColor::Red,
        InkColor::Yellow,
        InkColor::Cyan,
        InkColor::Green,
        InkColor::White,
    ];

    /// Straight sRGB components, so no UI toolkit type appears in the model.
    pub fn rgb(self) -> (f32, f32, f32) {
        match self {
            InkColor::Black => (0.0, 0.0, 0.0),
            InkColor::Red => (0.90, 0.16, 0.22),
            InkColor::Yellow => (0.98, 0.80, 0.16),
            InkColor::Cyan => (0.16, 0.78, 0.94),
            InkColor::Green => (0.22, 0.78, 0.36),
            InkColor::White => (1.0, 1.0, 1.0),
            InkColor::Custom { red, green, blue } => (
                f32::from(red) / 255.0,
                f32::from(green) / 255.0,
                f32::from(blue) / 255.0,
            ),
        }
    }

    /// A colour from sRGB components in `0.0..=1.0`.
    ///
    /// Stored as bytes rather than floats so two colours that look the same
    /// *are* the same: a palette that compares equal is what lets the swatch
    /// show which one is armed.
    ///
    /// A colour that *is* one of the named ones comes back as that name rather
    /// than as an anonymous triple. This matters more than it looks. A PDF has
    /// no field for "the presenter picked the swatch called Cyan" — `/C` is
    /// three numbers — so a mark read back out of a file would otherwise
    /// always be a mixed colour, the palette would never show which swatch it
    /// was made with, and every named colour would quietly become custom the
    /// first time it went through the document (A1). Recognising the exact
    /// bytes costs a comparison and keeps the name.
    pub fn from_rgb(red: f32, green: f32, blue: f32) -> Self {
        let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        let mixed = (byte(red), byte(green), byte(blue));
        for named in InkColor::ALL {
            let (red, green, blue) = named.rgb();
            if (byte(red), byte(green), byte(blue)) == mixed {
                return named;
            }
        }
        InkColor::Custom {
            red: mixed.0,
            green: mixed.1,
            blue: mixed.2,
        }
    }

    /// Is this one the presenter mixed rather than one of the five?
    pub fn is_custom(self) -> bool {
        matches!(self, InkColor::Custom { .. })
    }

    pub fn label(self) -> &'static str {
        match self {
            InkColor::Black => "Black",
            InkColor::Red => "Red",
            InkColor::Yellow => "Yellow",
            InkColor::Cyan => "Cyan",
            InkColor::Green => "Green",
            InkColor::White => "White",
            InkColor::Custom { .. } => "Custom",
        }
    }
}

/// How big the marks are, as fractions of the page's width.
///
/// Fractions rather than pixels for the same reason the coordinates are: the
/// presenter panel and the projector are different sizes, and a five-pixel
/// stroke would be a scratch on one and a smear on the other.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnotationStyle {
    /// Radius of the pointer dot.
    pub pointer_radius: f32,
    /// The colour of the pointer dot. A choice rather than the interface's
    /// accent: the accent is picked to sit against the application's own
    /// chrome, and the dot has to be found on somebody else's slide.
    pub pointer_color: InkColor,
    /// Radius of the lit circle.
    pub spotlight_radius: f32,
    /// Width of an ink stroke.
    pub ink_width: f32,
    /// How dark the page goes outside the spotlight, `0.0`–`1.0`.
    pub dim: f32,
}

impl Default for AnnotationStyle {
    fn default() -> Self {
        Self {
            pointer_radius: 0.012,
            pointer_color: InkColor::Red,
            spotlight_radius: 0.08,
            ink_width: 0.004,
            dim: 0.6,
        }
    }
}

/// The widest and narrowest each measure may be. Outside these a pointer is
/// invisible or a spotlight is the whole page, and neither is an annotation.
pub const POINTER_RADIUS_RANGE: (f32, f32) = (0.003, 0.06);
/// A *radius*, as a fraction of the page width. A quarter of the width is
/// already a lit circle half the page across — wider than that is not a
/// spotlight, it is the lights back on, which the tool being armed at all is
/// the way to say.
pub const SPOTLIGHT_RADIUS_RANGE: (f32, f32) = (0.02, 0.25);
pub const INK_WIDTH_RANGE: (f32, f32) = (0.001, 0.03);
pub const HIGHLIGHT_WIDTH_RANGE: (f32, f32) = (0.006, 0.08);
pub const ERASER_RADIUS_RANGE: (f32, f32) = (0.004, 0.10);

impl AnnotationStyle {
    /// Bring every measure back inside its range, replacing anything that is
    /// not a number with the default.
    pub fn sanitise(&mut self) {
        let default = AnnotationStyle::default();
        let bound = |value: f32, fallback: f32, range: (f32, f32)| {
            if value.is_finite() {
                value.clamp(range.0, range.1)
            } else {
                fallback
            }
        };
        self.pointer_radius = bound(
            self.pointer_radius,
            default.pointer_radius,
            POINTER_RADIUS_RANGE,
        );
        self.spotlight_radius = bound(
            self.spotlight_radius,
            default.spotlight_radius,
            SPOTLIGHT_RADIUS_RANGE,
        );
        self.ink_width = bound(self.ink_width, default.ink_width, INK_WIDTH_RANGE);
        self.dim = if self.dim.is_finite() {
            self.dim.clamp(0.0, 0.95)
        } else {
            default.dim
        };
    }
}

/// One freehand mark, in normalised page coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InkStroke {
    pub points: Vec<(f32, f32)>,
    /// Stroke width as a fraction of the page width.
    pub width: f32,
    pub color: InkColor,
    #[serde(default)]
    pub kind: StrokeKind,
    /// The annotation in the open document this stroke *is*, once the engine
    /// has confirmed it (A1).
    ///
    /// `None` while the stroke is still being drawn, and for the moment
    /// between the pen coming up and the engine answering. A stroke with no
    /// identity is a stroke the document does not know about yet: it is drawn,
    /// because the presenter drew it and must see it immediately, but it
    /// cannot be erased through the engine and does not survive a slide
    /// change, because there is nothing there to survive.
    ///
    /// This field is what makes the overlay a *view* of the document rather
    /// than a second store of marks. Every completed stroke on screen names
    /// the annotation it shows.
    #[serde(default)]
    pub id: Option<crate::annotate::AnnotationId>,
}

/// A temporary text label anchored in normalised page coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextMark {
    /// Stable within this process, so an asynchronous renderer can reject a
    /// result for a label that has since been edited or erased.
    #[serde(default)]
    pub id: u64,
    pub position: (f32, f32),
    pub text: String,
    /// Cap height as a fraction of the page width.
    pub size: f32,
    pub color: InkColor,
    /// The annotation in the open document this label is, once the engine has
    /// confirmed it. `None` while it is still being typed (A1).
    #[serde(default)]
    pub annotation: Option<crate::annotate::AnnotationId>,
    /// Whether this label becomes a sticky note rather than a mark on the
    /// page.
    ///
    /// The two are typed the same way — a spot is chosen and the keyboard goes
    /// into it — and differ only in what they become when the typing ends, so
    /// they share the gesture and part company at the commit.
    #[serde(default)]
    pub note: bool,
    /// The box the committed annotation occupies, in slide fractions, for a
    /// label read back out of the document.
    ///
    /// `None` for one being written here and now, which is drawn at the size
    /// it was set at. A PDF records the box a mark fills, not the type size
    /// the markup was set in, so a mark that came back from the file is drawn
    /// into the box it claims rather than at a size nothing recorded.
    #[serde(default)]
    pub fit: Option<(f32, f32)>,
}

/// A committed text markup — a highlight, an underline or a strikeout — as the
/// slide draws it.
///
/// The runs are the engine's answer about where the marked *text* is, mapped
/// into slide fractions. It is a view of the annotation (A1): the marks live
/// in the document, and this is what the overlay puts on the screen for them,
/// because a slide's pixels are rendered without annotations so that an
/// unfinished gesture and a committed mark are never drawn twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HighlightMark {
    /// The four corners of each run, clockwise from upper-left.
    pub runs: Vec<[(f32, f32); 4]>,
    pub color: InkColor,
    pub opacity: f32,
    /// Which of the three marks this is, so the overlay knows whether to wash
    /// the run or rule across it. Read back off the annotation's subtype
    /// rather than remembered here (A1).
    #[serde(default)]
    pub kind: MarkupKind,
    /// The annotation this shows. Always named: nothing puts an uncommitted
    /// highlight here, because the sweep in progress is `selection`.
    pub id: crate::annotate::AnnotationId,
}

/// A committed sticky note, as the slide draws it: an icon and nothing else.
///
/// The text behind a note is deliberately not drawn. A note is a mark that has
/// to be opened to be read, which is why the presenter palette does not make
/// one; what the slide owes it is an honest indication that it is there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteMark {
    /// The icon's top-left corner, in slide fractions.
    pub position: (f32, f32),
    /// The icon's size, as a fraction of the slide's width and height.
    pub size: (f32, f32),
    pub color: InkColor,
    pub id: crate::annotate::AnnotationId,
}

impl InkStroke {
    /// A stroke of one point is a dot, which is a mark a presenter means to
    /// make; a stroke of none is the residue of a press that went nowhere.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// The live text selection the highlighter is sweeping, in slide fractions.
///
/// One entry per contiguous run of text the engine resolved, each a rectangle
/// on the slide. Runs rather than one box because selected text is not a
/// rectangle: three lines of a paragraph are three runs, and the last of them
/// stops where the sentence does.
#[derive(Debug, Clone, PartialEq)]
pub struct SlideSelection {
    /// The four corners of each run, clockwise from upper-left, in fractions
    /// of the slide.
    pub runs: Vec<[(f32, f32); 4]>,
    /// The colour the mark will be laid down in.
    pub color: InkColor,
    /// The opacity it will be laid down at, so the live sweep and the
    /// committed mark look the same.
    pub opacity: f32,
    /// Which mark the release will leave behind, for the same reason: a sweep
    /// that washes the words and then commits an underline has shown the
    /// presenter the wrong thing for the length of the gesture.
    pub kind: MarkupKind,
}

/// Every annotation on the current slide, plus what the pointer is armed to
/// do and who can see the result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Annotations {
    pub strokes: Vec<InkStroke>,
    #[serde(default)]
    pub texts: Vec<TextMark>,
    /// The committed highlights on this slide, read back out of the document.
    ///
    /// Separate from `strokes` because a highlight is not a stroke: it is the
    /// runs of text it covers, and the document is what knows where those are.
    #[serde(default)]
    pub highlights: Vec<HighlightMark>,
    /// The committed sticky notes on this slide, likewise.
    #[serde(default)]
    pub notes: Vec<NoteMark>,
    /// Where the pointer dot is, when the pointer tool is armed and the
    /// pointer is over the page.
    pub pointer: Option<(f32, f32)>,
    /// Where the spotlight is centred.
    pub spotlight: Option<(f32, f32)>,
    /// The text the highlighter has swept so far, in slide fractions.
    ///
    /// Transient in the same sense as `pointer` and `spotlight`: it is what
    /// the presenter is doing, never what they have done, so it is not
    /// serialised and it never survives a release. The quads themselves are
    /// the engine's answer about where the *text* is — this is only the slide
    /// -space view of them, converted through `SlidePlacement` so the overlay
    /// can draw them without knowing about pages (A1).
    #[serde(skip)]
    pub selection: Option<SlideSelection>,
    /// The rubber band being dragged, as the two corners the drag has reached,
    /// in slide fractions.
    ///
    /// Transient like `selection`: it is what the hand is doing, and it never
    /// survives the release that turns it into a held set of marks.
    #[serde(skip)]
    pub band: Option<((f32, f32), (f32, f32))>,
    /// The marks the band is holding, by the annotation each one is.
    ///
    /// Held rather than copied: what a selection *is* is a list of names, and
    /// the marks themselves stay where they are — in the document (A1).
    #[serde(skip)]
    pub selected: Vec<crate::annotate::AnnotationId>,
    /// The armed tool, or `None` when the pointer belongs to links and media
    /// overlays as it normally does.
    pub tool: Option<AnnotationTool>,
    /// Whether the audience screen shows these too. On by default — a mark is
    /// drawn to be seen. Off means the presenter is marking up their own copy.
    pub audience_visible: bool,
    /// What the hand or the keyboard is in the middle of doing. Not
    /// serialised state anybody depends on, but it is what makes a stray
    /// move event after a release harmless, and what the eraser and the
    /// text tool use to tell their own open gesture from each other's
    /// (§81: was a `drawing`/`erasing` bool pair plus a `typing` index for
    /// what is, at any moment, one gesture).
    #[serde(skip)]
    gesture: Gesture,
    #[serde(skip)]
    next_text_id: u64,
    /// Annotations the eraser has taken and the document has not been told
    /// about yet. Not serialised: it is a message in transit, not state.
    #[serde(skip)]
    erased: Vec<crate::annotate::AnnotationId>,
    /// One entry per stroke commit still awaiting its answer, in the order
    /// the commits were sent, so [`Self::name_stroke`] can tell whether the
    /// next id due back names a stroke still on screen (`false`) or one the
    /// eraser already took before it had a name (`true`).
    ///
    /// Without this a stroke erased before it was named was simply dropped,
    /// and its arriving id was handed to whatever unnamed stroke happened to
    /// be oldest by then — misnaming a later stroke and leaving the erased
    /// one stuck in the document forever (§76.14).
    #[serde(skip)]
    pending_stroke_names: std::collections::VecDeque<PendingName>,
    /// Bumped by every visible change, so a consumer holding a rendered copy
    /// can tell "changed" from "same" without a deep comparison.
    #[serde(skip)]
    revision: Revision,
}

impl Default for Annotations {
    /// Everything empty and unarmed, and the marks shown to the audience:
    /// hiding them is the deliberate choice, not showing them.
    fn default() -> Self {
        Self {
            strokes: Vec::new(),
            texts: Vec::new(),
            highlights: Vec::new(),
            notes: Vec::new(),
            band: None,
            selected: Vec::new(),
            pointer: None,
            spotlight: None,
            tool: None,
            selection: None,
            audience_visible: true,
            gesture: Gesture::None,
            next_text_id: 1,
            erased: Vec::new(),
            pending_stroke_names: std::collections::VecDeque::new(),
            revision: Revision::default(),
        }
    }
}

/// What the pointer or the keyboard is in the middle of doing to the slide.
///
/// A stroke and an eraser sweep both hold the pointer down, which is why the
/// two used to share one `drawing` bool; they differ in what letting go
/// means, which is why a second `erasing` bool rode along beside it. Typing
/// held a third, `Option<usize>`, that never overlapped the other two in
/// practice, only in representation — so this is the one gesture the three
/// fields described, made into the type that can only hold one at a time
/// (§81).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Gesture {
    #[default]
    None,
    /// An ink or highlighter stroke is open.
    Stroke,
    /// An eraser sweep is open.
    Erase,
    /// A text or note label at this index in [`Annotations::texts`] is
    /// receiving keyboard input.
    Typing(usize),
}

/// A monotonic change counter that deliberately compares equal to any other,
/// so deriving `PartialEq` on the model keeps meaning "same marks", not
/// "same edit history".
#[derive(Debug, Clone, Copy, Default)]
pub struct Revision(u64);

impl PartialEq for Revision {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Annotations {
    /// Ink is a gesture about *this* slide, so it goes when the slide does.
    ///
    /// Keeping marks across navigation was considered and rejected: strokes
    /// are positioned in page coordinates with no idea what is underneath
    /// them, so a mark that survives a page turn lands on unrelated content
    /// and — worse, when the audience can see it — does so on the projector.
    /// The constant exists so the decision is a value the tests can assert
    /// rather than a comment.
    pub const RETAIN_ACROSS_NAVIGATION: bool = false;

    /// How many strokes one slide may hold. Beyond this the oldest is
    /// dropped: the alternative is a pen that silently stops writing in the
    /// middle of a talk, which is worse than losing a mark made minutes ago.
    pub const MAX_STROKES: usize = 64;

    /// How many points one stroke may hold. A pointer device reports far
    /// faster than a hand moves, and an unbounded stroke is an unbounded
    /// path to tessellate every frame.
    pub const MAX_POINTS_PER_STROKE: usize = 1024;

    /// Text is live UI state, so bound it before it reaches shaping/layout.
    pub const MAX_TEXT_BYTES: usize = 4096;

    /// How far the pointer must travel before a point is worth keeping, as a
    /// fraction of the page. Below this the points describe the tremor in a
    /// hand rather than the shape of the mark.
    pub const MIN_POINT_DISTANCE: f32 = 0.002;

    /// The current change count. Two reads returning the same number mean
    /// nothing visible changed between them.
    pub fn revision(&self) -> u64 {
        self.revision.0
    }

    fn bump(&mut self) {
        self.revision.0 = self.revision.0.wrapping_add(1);
    }

    /// Whether there is a gesture in progress that undo would settle first.
    ///
    /// Whether there is anything to *undo* is a question about the document,
    /// and the document answers it. This is only about the pen still being
    /// down, which the caller needs to know because settling the gesture is
    /// what makes the undo mean what the presenter expects.
    pub fn has_open_gesture(&self) -> bool {
        self.gesture != Gesture::None
    }

    /// Is this a point on the page at all?
    ///
    /// Non-finite coordinates come from degenerate panel geometry, and
    /// out-of-range ones from the letterbox bars — neither is somewhere the
    /// author drew anything, so neither is somewhere a mark can go.
    pub fn is_on_page(point: (f32, f32)) -> bool {
        point.0.is_finite()
            && point.1.is_finite()
            && (0.0..=1.0).contains(&point.0)
            && (0.0..=1.0).contains(&point.1)
    }

    /// Start a stroke at this point. Refuses points that are not on the page.
    pub fn begin_stroke(&mut self, point: (f32, f32), width: f32, color: InkColor) -> bool {
        self.begin_mark(point, width, color, StrokeKind::Ink)
    }

    /// Start an ink or highlighter stroke.
    pub fn begin_mark(
        &mut self,
        point: (f32, f32),
        width: f32,
        color: InkColor,
        kind: StrokeKind,
    ) -> bool {
        if !Self::is_on_page(point) {
            return false;
        }
        let range = match kind {
            StrokeKind::Ink => INK_WIDTH_RANGE,
            StrokeKind::Highlight => HIGHLIGHT_WIDTH_RANGE,
        };
        let fallback = match kind {
            StrokeKind::Ink => AnnotationStyle::default().ink_width,
            StrokeKind::Highlight => 0.025,
        };
        let width = if width.is_finite() {
            width.clamp(range.0, range.1)
        } else {
            fallback
        };
        if self.strokes.len() >= Self::MAX_STROKES {
            // The oldest mark goes off the screen. It stays in the document:
            // this cap is about how much the presenter can usefully see at
            // once, not about what the file holds.
            let dropped = self.strokes.remove(0);
            if dropped.id.is_none() {
                mark_pending_stroke(&mut self.pending_stroke_names, 0, PendingName::Dropped);
            }
        }
        self.strokes.push(InkStroke {
            points: vec![point],
            width,
            color,
            kind,
            // Nothing yet: the engine names it when it confirms the
            // commit, which cannot have happened before the pen is up.
            id: None,
        });
        self.pending_stroke_names.push_back(PendingName::Live);
        self.gesture = Gesture::Stroke;
        self.bump();
        true
    }

    /// Begin an eraser gesture and erase at its first point.
    pub fn begin_erase(&mut self, point: (f32, f32), radius: f32) -> bool {
        if !Self::is_on_page(point) {
            return false;
        }
        self.gesture = Gesture::Erase;
        self.erase_at(point, radius)
    }

    /// Continue an eraser gesture while the pointer button is held.
    pub fn extend_erase(&mut self, point: (f32, f32), radius: f32) -> bool {
        if !self.is_drawing() || !Self::is_on_page(point) {
            return false;
        }
        self.erase_at(point, radius)
    }

    /// Remove ink or text touched by a circular eraser.
    ///
    /// Stroke erasing is deliberate here: it keeps undo meaningful and
    /// avoids leaving tiny disconnected fragments that are hard to clear in
    /// front of an audience.
    fn erase_at(&mut self, point: (f32, f32), radius: f32) -> bool {
        let radius = if radius.is_finite() {
            radius.clamp(ERASER_RADIUS_RANGE.0, ERASER_RADIUS_RANGE.1)
        } else {
            0.02
        };
        let mut took = false;
        // Erasing takes the whole mark, deliberately: it keeps one sweep one
        // undo, and it avoids leaving tiny disconnected fragments that are
        // hard to clear in front of an audience. What is recorded is the
        // *identity* of what was taken, not a copy of it — putting it back is
        // the document's business now, and the document restores the
        // annotation rather than drawing a new one that looks the same (§9.4).
        let mut unnamed_seen = 0usize;
        self.strokes.retain(|stroke| {
            let unnamed = stroke.id.is_none();
            let hit_radius = radius + stroke.width / 2.0;
            let hit_radius_squared = hit_radius * hit_radius;
            // One pass: every interior point is an endpoint of some segment,
            // and the point-in-circle test is the degenerate case of the
            // segment test, so scanning the points *and* the segments tested
            // everything twice. Only a single-point stroke has no segment.
            let hit = match stroke.points.as_slice() {
                [only] => distance_squared(*only, point) <= hit_radius_squared,
                points => points.windows(2).any(|segment| {
                    crate::page::PagePoint::new(point.0, point.1).distance_to_segment_squared(
                        crate::page::PagePoint::new(segment[0].0, segment[0].1),
                        crate::page::PagePoint::new(segment[1].0, segment[1].1),
                    ) <= hit_radius_squared
                }),
            };
            if hit {
                took = true;
                if let Some(id) = &stroke.id {
                    self.erased.push(id.clone());
                } else {
                    // A stroke the document has not named yet is one whose
                    // commit is still in flight. Erasing it here takes it off
                    // the screen and marks its pending commit so that the id
                    // which eventually arrives for it becomes an immediate
                    // delete instead of being handed to a different stroke
                    // (§76.14).
                    mark_pending_stroke(
                        &mut self.pending_stroke_names,
                        unnamed_seen,
                        PendingName::Erased,
                    );
                }
            }
            if unnamed {
                unnamed_seen += 1;
            }
            !hit
        });
        self.texts.retain(|mark| {
            let hit = text_mark_hit(mark, point, radius);
            if hit {
                took = true;
                if let Some(id) = &mark.annotation {
                    self.erased.push(id.clone());
                }
            }
            !hit
        });
        // A highlight and a note are marks on this slide like any other, and
        // an eraser that reached only the ones made in presentation would be
        // an eraser that could not take back the mark the presenter just made
        // with the highlighter.
        self.highlights.retain(|highlight| {
            let hit = highlight
                .runs
                .iter()
                .any(|run| quad_hit(point, run, radius));
            if hit {
                took = true;
                self.erased.push(highlight.id.clone());
            }
            !hit
        });
        self.notes.retain(|note| {
            let hit = point.0 >= note.position.0 - radius
                && point.0 <= note.position.0 + note.size.0 + radius
                && point.1 >= note.position.1 - radius
                && point.1 <= note.position.1 + note.size.1 + radius;
            if hit {
                took = true;
                self.erased.push(note.id.clone());
            }
            !hit
        });
        if !took {
            return false;
        }
        self.bump();
        true
    }

    /// Continue the open stroke, if the point says something new.
    ///
    /// Returns whether the point was kept, which is what a caller needs to
    /// know to decide whether anything has to be redrawn.
    pub fn extend_stroke(&mut self, point: (f32, f32)) -> bool {
        if !self.is_drawing() || !Self::is_on_page(point) {
            return false;
        }
        let Some(stroke) = self.strokes.last_mut() else {
            return false;
        };
        if stroke.points.len() >= Self::MAX_POINTS_PER_STROKE {
            return false;
        }
        if let Some(last) = stroke.points.last() {
            let (dx, dy) = (point.0 - last.0, point.1 - last.1);
            if dx.hypot(dy) < Self::MIN_POINT_DISTANCE {
                return false;
            }
        }
        stroke.points.push(point);
        self.bump();
        true
    }

    /// Finish the open stroke, discarding one that carries no points.
    ///
    /// Returns the completed stroke, which is the caller's cue to commit it to
    /// the document: the pen coming up is what turns a gesture into an
    /// annotation (A2 → A1). `None` for a gesture that made no mark — an
    /// eraser sweep, a tap that drew nothing, or a release with no pen down.
    ///
    /// The stroke stays in [`Self::strokes`] either way, because the presenter
    /// must see it now rather than after a round trip to the worker. It is
    /// named once the engine answers, and it is the engine's from then on.
    #[must_use = "a completed stroke that is not committed is a mark that vanishes"]
    pub fn end_stroke(&mut self) -> Option<InkStroke> {
        let gesture = self.gesture;
        // Only a stroke or an erase sweep is this gesture's to end; typing
        // is a different one and is left exactly where it was.
        if matches!(gesture, Gesture::Stroke | Gesture::Erase) {
            self.gesture = Gesture::None;
        }
        if self.strokes.last().is_some_and(InkStroke::is_empty) {
            self.strokes.pop();
            self.bump();
            return None;
        }
        if gesture == Gesture::Stroke {
            return self.strokes.last().cloned();
        }
        None
    }

    /// Give the stroke the engine just confirmed its name.
    ///
    /// Applied to the most recent unnamed stroke rather than to a position,
    /// because between the commit and the answer the presenter may have drawn
    /// another one — and commits are answered in order, so the oldest unnamed
    /// stroke is the one this answer is about.
    ///
    /// When the commit this id answers was for a stroke the eraser already
    /// took before it had a name, `pending_stroke_names` says so, and the
    /// arriving id is turned into an immediate delete for the caller to send
    /// instead of being handed to whichever stroke happens to be oldest
    /// (§76.14).
    pub fn name_stroke(&mut self, id: crate::annotate::AnnotationId) {
        match self.pending_stroke_names.pop_front() {
            Some(PendingName::Erased) => self.erased.push(id),
            // A stroke the view cap dropped stays in the file; its name is
            // consumed so it cannot be handed to a younger stroke.
            Some(PendingName::Dropped) => {}
            Some(PendingName::Live) | None => {
                if let Some(stroke) = self.strokes.iter_mut().find(|stroke| stroke.id.is_none()) {
                    stroke.id = Some(id);
                }
            }
        }
    }

    /// Replace every completed mark with what the document says is on this
    /// slide.
    ///
    /// This is how a page turn works now: the marks are not stashed and
    /// restored from a cache beside the file, they are read back out of the
    /// file, because the file is where they are (A1). A mark made in document
    /// mode arrives here too, and so does one that was in the PDF before
    /// pulpit ever opened it.
    ///
    /// An unfinished gesture is left alone: it belongs to the hand that is
    /// still moving, not to the document. So is a stroke the document has not
    /// named yet — one whose commit is still in flight. The list the engine
    /// answered with was made before that commit landed, so a stroke missing
    /// from it is not a stroke that was deleted; adopting over it would take
    /// the pen's last line off the screen for the length of a round trip.
    pub fn adopt(&mut self, strokes: Vec<InkStroke>) {
        let in_flight: Vec<InkStroke> = self
            .strokes
            .iter()
            .filter(|stroke| stroke.id.is_none())
            .cloned()
            .collect();
        self.strokes = strokes;
        self.strokes.extend(in_flight);
        self.bump();
    }

    /// Replace the committed labels with what the document says is on this
    /// slide, the way [`Self::adopt`] does for ink.
    ///
    /// The label being typed is left alone and keeps its place in the list: it
    /// is the open gesture, and it belongs to the keyboard rather than to the
    /// document (A2). So does one that has been finished but not yet named,
    /// which is a label the engine has not answered about — dropping it would
    /// take a mark off the screen between the commit and its confirmation.
    ///
    /// Local drawing identities are derived here rather than carried in from
    /// the document, because they exist to key an in-process compile cache and
    /// an annotation's name is not a `u64`.
    ///
    /// Derived *from* the annotation's name rather than freshly minted,
    /// though: a name is stable across a round trip, so a re-adopted label
    /// keeps the id it had, and the Typst cache keyed by that id does not
    /// have to recompile every label on the slide just because one `Applied`
    /// answer came back (§82.6). A label with no name yet — unreachable in
    /// practice, since only the document's own committed labels arrive here
    /// — still gets a fresh one so the id remains a total function of the
    /// input.
    pub fn adopt_texts(&mut self, texts: impl IntoIterator<Item = TextMark>) {
        let typing = self
            .typing_index()
            .and_then(|index| self.texts.get(index))
            .cloned();
        let unnamed: Vec<TextMark> = self
            .texts
            .iter()
            .filter(|mark| mark.annotation.is_none())
            .filter(|mark| Some(mark.id) != typing.as_ref().map(|mark| mark.id))
            .cloned()
            .collect();
        let mut adopted: Vec<TextMark> = texts.into_iter().collect();
        for mark in &mut adopted {
            mark.id = match &mark.annotation {
                Some(name) => stable_text_id(name),
                None => {
                    let id = self.next_text_id;
                    self.next_text_id = self.next_text_id.wrapping_add(1).max(1);
                    id
                }
            };
        }
        self.texts = adopted;
        self.texts.extend(unnamed);
        // `typing` is `Some` only when `self.gesture` was already
        // `Typing(_)`; any other gesture (including none at all) is left
        // exactly where it was, the way the separate `typing` field used to
        // be untouched by this method.
        if let Some(mark) = typing {
            self.texts.push(mark);
            self.gesture = Gesture::Typing(self.texts.len() - 1);
        }
        self.bump();
    }

    /// Replace the committed highlights with the document's.
    pub fn adopt_highlights(&mut self, highlights: Vec<HighlightMark>) {
        self.highlights = highlights;
        self.bump();
    }

    /// Replace the committed notes with the document's.
    pub fn adopt_notes(&mut self, notes: Vec<NoteMark>) {
        self.notes = notes;
        self.bump();
    }

    /// The annotations the eraser has taken since this was last asked, which
    /// the caller deletes from the document.
    ///
    /// Drained rather than read, because an eraser sweep reported twice is a
    /// delete sent twice, and the second one names an annotation that is no
    /// longer there.
    pub fn take_erased(&mut self) -> Vec<crate::annotate::AnnotationId> {
        std::mem::take(&mut self.erased)
    }

    /// Start a label at `point`. An unfinished empty label is harmless and is
    /// discarded when typing ends.
    /// Returns whether the press started a label, and the label it closed to
    /// do so — which the caller commits, exactly as it commits the one
    /// [`Self::finish_text`] hands back. Starting a second label is how a
    /// presenter most often finishes the first, and a label that ended by
    /// being replaced is not a label they meant to throw away.
    #[must_use = "a label closed by starting another is a mark that vanishes"]
    ///
    /// `note` says which mark the typing becomes: a label on the page, or a
    /// sticky note whose text is behind an icon.
    pub fn begin_text(
        &mut self,
        point: (f32, f32),
        size: f32,
        color: InkColor,
        note: bool,
    ) -> (bool, Option<TextMark>) {
        if !Self::is_on_page(point) {
            return (false, None);
        }
        let finished = self.finish_text();
        // `next_text_id` is already the one counter every local id is minted
        // from — here and in `adopt_texts` — so it alone is the next value;
        // no need to also scan `self.texts` for its current maximum (§81).
        let id = self.next_text_id.max(1);
        self.next_text_id = id.wrapping_add(1).max(1);
        self.texts.push(TextMark {
            id,
            position: point,
            text: String::new(),
            size: if size.is_finite() {
                size.clamp(0.008, 0.12)
            } else {
                0.025
            },
            color,
            note,
            // Nothing yet: a label with no text in it is not an annotation.
            annotation: None,
            // Written here, so it is drawn at the size it was set at.
            fit: None,
        });
        self.gesture = Gesture::Typing(self.texts.len() - 1);
        self.bump();
        (true, finished)
    }

    /// Append composed keyboard text to the active label.
    pub fn type_text(&mut self, value: &str) -> bool {
        let Some(mark) = self
            .typing_index()
            .and_then(|index| self.texts.get_mut(index))
        else {
            return false;
        };
        let remaining = Self::MAX_TEXT_BYTES.saturating_sub(mark.text.len());
        if remaining == 0 {
            return false;
        }
        let end = value
            .char_indices()
            .map(|(index, ch)| index + ch.len_utf8())
            .take_while(|end| *end <= remaining)
            .last()
            .unwrap_or(0);
        if end == 0 {
            return false;
        }
        mark.text.push_str(&value[..end]);
        self.bump();
        true
    }

    pub fn backspace_text(&mut self) -> bool {
        let Some(mark) = self
            .typing_index()
            .and_then(|index| self.texts.get_mut(index))
        else {
            return false;
        };
        if mark.text.pop().is_some() {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Stop typing, and return the label that was finished.
    ///
    /// The returned mark is the caller's cue to commit it to the document, the
    /// same way a completed stroke is. A label with nothing typed into it is
    /// removed and returns `None`: an empty annotation is a thing other
    /// viewers draw as an empty box, and it is never what someone meant.
    #[must_use = "a finished label that is not committed is a mark that vanishes"]
    pub fn finish_text(&mut self) -> Option<TextMark> {
        let index = self.take_typing()?;
        let finished = match self.texts.get(index) {
            Some(mark) if mark.text.is_empty() => {
                self.texts.remove(index);
                None
            }
            Some(mark) => Some(mark.clone()),
            None => None,
        };
        self.bump();
        finished
    }

    /// Give the label the engine just confirmed its name, as `name_stroke`
    /// does for ink.
    pub fn name_text(&mut self, id: crate::annotate::AnnotationId) {
        if let Some(mark) = self
            .texts
            .iter_mut()
            .find(|mark| mark.annotation.is_none() && !mark.text.is_empty())
        {
            mark.annotation = Some(id);
        }
    }

    pub fn is_typing(&self) -> bool {
        self.typing_index().is_some()
    }

    /// The mark receiving input, for presenter-only editing affordances.
    pub fn typing_index(&self) -> Option<usize> {
        match self.gesture {
            Gesture::Typing(index) => Some(index),
            _ => None,
        }
    }

    /// Take the index of the label being typed, if any, the way
    /// `Option::take` would — leaving any other open gesture untouched.
    fn take_typing(&mut self) -> Option<usize> {
        match self.gesture {
            Gesture::Typing(index) => {
                self.gesture = Gesture::None;
                Some(index)
            }
            _ => None,
        }
    }

    /// Is a stroke or an eraser sweep currently open?
    pub fn is_drawing(&self) -> bool {
        matches!(self.gesture, Gesture::Stroke | Gesture::Erase)
    }

    /// Start a rubber band at `point`, dropping whatever was held.
    ///
    /// A new band is a new question, so the previous answer goes with it —
    /// the same thing document mode's band does (§8.4).
    pub fn begin_band(&mut self, point: (f32, f32)) -> bool {
        if !Self::is_on_page(point) {
            return false;
        }
        self.selected.clear();
        self.band = Some((point, point));
        self.bump();
        true
    }

    /// Drag the band's far corner.
    pub fn extend_band(&mut self, point: (f32, f32)) -> bool {
        let Some((from, _)) = self.band else {
            return false;
        };
        let point = (point.0.clamp(0.0, 1.0), point.1.clamp(0.0, 1.0));
        self.band = Some((from, point));
        self.bump();
        true
    }

    /// Close the band and hold everything it encloses.
    ///
    /// Returns the marks now held, which is what the caller deletes when the
    /// delete key follows. A band that enclosed nothing holds nothing and is
    /// not an error: it is how a selection is dismissed.
    ///
    /// Only marks the document has named can be held. One whose commit is
    /// still in flight has no name to hold it by, and a moment later it will
    /// have one — so it is left out rather than held by a name it does not
    /// have yet.
    pub fn finish_band(&mut self) -> &[crate::annotate::AnnotationId] {
        let Some((from, to)) = self.band.take() else {
            return &self.selected;
        };
        let rect = crate::notes::Region::new(
            from.0.min(to.0),
            from.1.min(to.1),
            (from.0 - to.0).abs(),
            (from.1 - to.1).abs(),
        );
        let within = |point: (f32, f32)| rect.contains(point.0, point.1);
        let mut held = Vec::new();
        for stroke in &self.strokes {
            if let Some(id) = &stroke.id {
                if stroke.points.iter().any(|point| within(*point)) {
                    held.push(id.clone());
                }
            }
        }
        for mark in &self.texts {
            if let Some(id) = &mark.annotation {
                if within(mark.position) {
                    held.push(id.clone());
                }
            }
        }
        for highlight in &self.highlights {
            if highlight
                .runs
                .iter()
                .any(|run| run.iter().any(|corner| within(*corner)))
            {
                held.push(highlight.id.clone());
            }
        }
        for note in &self.notes {
            if within(note.position) {
                held.push(note.id.clone());
            }
        }
        self.selected = held;
        self.bump();
        &self.selected
    }

    /// Put down whatever is held, without touching the document.
    pub fn clear_selection(&mut self) {
        if self.band.is_none() && self.selected.is_empty() {
            return;
        }
        self.band = None;
        self.selected.clear();
        self.bump();
    }

    /// Take the held marks off the slide and hand back their names, for the
    /// caller to delete from the document in one transaction — one press, one
    /// undo (§8.4).
    #[must_use = "held marks that are not deleted are marks the slide has lost"]
    pub fn take_selection(&mut self) -> Vec<crate::annotate::AnnotationId> {
        let held: Vec<_> = std::mem::take(&mut self.selected);
        if held.is_empty() {
            return held;
        }
        self.strokes
            .retain(|stroke| !stroke.id.as_ref().is_some_and(|id| held.contains(id)));
        self.texts
            .retain(|mark| !mark.annotation.as_ref().is_some_and(|id| held.contains(id)));
        self.highlights
            .retain(|highlight| !held.contains(&highlight.id));
        self.notes.retain(|note| !held.contains(&note.id));
        self.band = None;
        self.bump();
        held
    }

    /// Abandon the label being typed, taking it off the slide.
    ///
    /// The way out that makes nothing, and the same one document mode offers:
    /// escape from a mark being written cancels it, and cancelling is not a
    /// mutation (§8.5). Distinct from [`Self::finish_text`], which hands the
    /// label over to be committed.
    pub fn cancel_text(&mut self) {
        let Some(index) = self.take_typing() else {
            return;
        };
        if index < self.texts.len() {
            self.texts.remove(index);
        }
        self.bump();
    }

    /// Finish whatever gesture is open, so that "undo" means the same thing
    /// whether or not the button happens to be down.
    ///
    /// Undo itself is not here any more. A completed mark is an annotation in
    /// the open document (A1) and the document's history is the only one there
    /// is; a second stack in this type would be a second answer to "what did I
    /// just do", and the two would disagree the first time an edit was made in
    /// document mode. What is left is this: settle the gesture, and let the
    /// caller ask the engine.
    ///
    /// Returns the label the settling finished, for the caller to commit. The
    /// finished *stroke* is the last one in [`Self::strokes`], where it was
    /// already; a label is returned because an empty one is removed rather
    /// than kept, so there would otherwise be no way to tell which it was.
    #[must_use = "a settled label that is not committed is a mark that vanishes"]
    pub fn settle(&mut self) -> Option<TextMark> {
        if self.is_drawing() {
            let _ = self.end_stroke();
        }
        self.finish_text()
    }

    /// Remove every mark, leaving the armed tool and the audience choice
    /// alone: clearing the page is not putting the pen down.
    pub fn clear(&mut self) {
        self.strokes.clear();
        self.texts.clear();
        self.highlights.clear();
        self.notes.clear();
        self.pointer = None;
        self.spotlight = None;
        self.selection = None;
        self.band = None;
        self.selected.clear();
        self.gesture = Gesture::None;
        self.pending_stroke_names.clear();
        self.bump();
    }

    /// What navigation does to the annotations on the slide being left.
    ///
    /// Only the marks go. The armed tool and whether the audience can see
    /// annotations are settings the presenter chose, not properties of the
    /// slide, so they survive navigation.
    pub fn clear_on_slide_change(&mut self) {
        if !Self::RETAIN_ACROSS_NAVIGATION {
            self.clear();
        }
    }

    /// Move the pointer dot, or take it away. A point off the page takes it
    /// away rather than pinning it to the nearest edge, which would leave a
    /// dot sitting in the letterbox.
    pub fn set_pointer(&mut self, point: Option<(f32, f32)>) {
        let point = point.filter(|point| Self::is_on_page(*point));
        if self.pointer != point {
            self.pointer = point;
            self.bump();
        }
    }

    pub fn set_spotlight(&mut self, point: Option<(f32, f32)>) {
        let point = point.filter(|point| Self::is_on_page(*point));
        if self.spotlight != point {
            self.spotlight = point;
            self.bump();
        }
    }

    /// Choose whether the audience window shows these annotations.
    pub fn set_audience_visible(&mut self, visible: bool) {
        if self.audience_visible != visible {
            self.audience_visible = visible;
            self.bump();
        }
    }

    /// Is there nothing at all to draw?
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
            && self.texts.is_empty()
            && self.highlights.is_empty()
            && self.notes.is_empty()
            && self.pointer.is_none()
            && self.spotlight.is_none()
            && self.selection.is_none()
            && self.band.is_none()
            && self.selected.is_empty()
    }

    /// Arm a tool, or put the pointer back to its ordinary duties. Changing
    /// tool takes away the marks that belonged to the old one, but never the
    /// ink: ink is the only annotation the presenter deliberately committed.
    ///
    /// Returns the label that putting the text tool down finished, which the
    /// caller commits. Reaching for another tool is a way of saying a label is
    /// done, not a way of throwing it away.
    #[must_use = "a label closed by changing tool is a mark that vanishes"]
    pub fn arm(&mut self, tool: Option<AnnotationTool>) -> Option<TextMark> {
        self.tool = tool;
        // Only a stroke or an erase sweep is put down here; typing is left
        // for `finish_text` below to settle on its own terms.
        if matches!(self.gesture, Gesture::Stroke | Gesture::Erase) {
            self.gesture = Gesture::None;
        }
        // A label and a note are both typed, so neither one ends the other's
        // typing; every other tool does.
        let finished = if matches!(tool, Some(AnnotationTool::Text | AnnotationTool::Note)) {
            None
        } else {
            self.finish_text()
        };
        if tool != Some(AnnotationTool::Pointer) {
            self.pointer = None;
        }
        if tool != Some(AnnotationTool::Spotlight) {
            self.spotlight = None;
        }
        if !matches!(
            tool,
            Some(AnnotationTool::Highlighter | AnnotationTool::SelectText)
        ) {
            self.selection = None;
        }
        // Putting the band down puts down what it was holding: a selection is
        // a thing that tool is doing, and the delete key belongs to the deck
        // again the moment another tool is armed.
        if tool != Some(AnnotationTool::Select) {
            self.band = None;
            self.selected.clear();
        }
        self.bump();
        finished
    }

    /// Show the text the highlighter has swept so far, or take it away.
    ///
    /// Called as the engine answers, which is several times during one drag:
    /// the last answer is what is drawn, and none of them is a mark yet.
    pub fn set_selection(&mut self, selection: Option<SlideSelection>) {
        if self.selection != selection {
            self.selection = selection;
            self.bump();
        }
    }

    /// Does the pointer belong to the annotations rather than to links and
    /// media overlays? The one question the input router has to ask.
    pub fn is_armed(&self) -> bool {
        self.tool.is_some()
    }
}

/// A local `u64` id derived from an annotation's name, so the same
/// annotation always adopts to the same id.
///
/// The same FNV-1a construction as `overlay.rs::stable_overlay_id`, for the
/// same reason: a name is stable across a round trip and an incrementing
/// counter is not (§82.6).
fn stable_text_id(name: &crate::annotate::AnnotationId) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in name.as_str().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    // Zero is reserved so a derived id is never confused with an unset one.
    hash | 1
}

fn distance_squared(left: (f32, f32), right: (f32, f32)) -> f32 {
    let x = left.0 - right.0;
    let y = left.1 - right.1;
    x * x + y * y
}

/// What a stroke whose commit is still in flight is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingName {
    /// Still on screen; the arriving id names it.
    Live,
    /// Erased before it was named; the arriving id is deleted at once.
    Erased,
    /// Dropped from the view by the stroke cap; the arriving id is consumed
    /// and the mark stays in the file.
    Dropped,
}

/// Mark the `index`-th still-live entry of a stroke-naming queue.
///
/// `index` counts only over `Live` entries, because the others have no
/// counterpart left in `Annotations::strokes` to be confused with (§76.14).
/// A free function, not a method, so it can be called from inside a `retain`
/// closure that already borrows `self.strokes` and `self.erased`.
fn mark_pending_stroke(
    queue: &mut std::collections::VecDeque<PendingName>,
    index: usize,
    state: PendingName,
) {
    if let Some(entry) = queue
        .iter_mut()
        .filter(|entry| **entry == PendingName::Live)
        .nth(index)
    {
        *entry = state;
    }
}

/// Hit-test the same approximate text block the canvas lays out. Exact glyph
/// outlines would couple the pure model to a renderer; a rectangular label
/// target is also much easier to erase deliberately at presentation speed.
fn text_mark_hit(mark: &TextMark, point: (f32, f32), radius: f32) -> bool {
    // A label read back out of the document knows the box it fills, so it is
    // erased by that box rather than by an estimate of one.
    let (width, height) = match mark.fit {
        Some(fit) => fit,
        None => {
            let mut lines = 0_usize;
            let mut longest = 0_usize;
            for line in mark.text.split('\n') {
                lines += 1;
                longest = longest.max(line.chars().count());
            }
            (
                longest as f32 * mark.size * 0.6,
                lines.max(1) as f32 * mark.size * 1.2,
            )
        }
    };
    let nearest = (
        point.0.clamp(mark.position.0, mark.position.0 + width),
        point.1.clamp(mark.position.1, mark.position.1 + height),
    );
    distance_squared(point, nearest) <= radius * radius
}

/// Whether an eraser of `radius` centred on `point` touches a text run.
///
/// A run is a quadrilateral rather than a rectangle — a line of rotated text
/// resolves to one that is not axis-aligned — so the test is against its four
/// edges, plus containment for a press in the middle of a word.
fn quad_hit(point: (f32, f32), quad: &[(f32, f32); 4], radius: f32) -> bool {
    let radius_squared = radius * radius;
    let touches_edge = (0..4).any(|corner| {
        let (start, end) = (quad[corner], quad[(corner + 1) % 4]);
        crate::page::PagePoint::new(point.0, point.1).distance_to_segment_squared(
            crate::page::PagePoint::new(start.0, start.1),
            crate::page::PagePoint::new(end.0, end.1),
        ) <= radius_squared
    });
    touches_edge || point_in_quad(point, quad)
}

/// Whether a point is inside a quadrilateral, by the sign of the cross product
/// against each edge. Convex by construction: it is a run of text.
fn point_in_quad(point: (f32, f32), quad: &[(f32, f32); 4]) -> bool {
    let mut positive = false;
    let mut negative = false;
    for corner in 0..4 {
        let (start, end) = (quad[corner], quad[(corner + 1) % 4]);
        let cross =
            (end.0 - start.0) * (point.1 - start.1) - (end.1 - start.1) * (point.0 - start.0);
        positive |= cross > 0.0;
        negative |= cross < 0.0;
    }
    !(positive && negative)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: InkColor = InkColor::Red;
    const WIDTH: f32 = 0.004;

    fn drawn(points: &[(f32, f32)]) -> Annotations {
        let mut annotations = Annotations::default();
        let mut points = points.iter();
        if let Some(first) = points.next() {
            assert!(annotations.begin_stroke(*first, WIDTH, RED));
        }
        for point in points {
            annotations.extend_stroke(*point);
        }
        let _ = annotations.end_stroke();
        annotations
    }

    #[test]
    fn only_the_highlighters_default_nib_washes_the_words() {
        assert!(MarkupKind::Highlight.is_wash());
        assert!(MarkupKind::Highlight.rule_at().is_none());
        assert_eq!(
            MarkupKind::default(),
            MarkupKind::Highlight,
            "the tool must keep making the mark it is named for until it is told otherwise"
        );
        for kind in [MarkupKind::Underline, MarkupKind::StrikeOut] {
            assert!(!kind.is_wash(), "{kind:?}");
            // A rule sits inside the run rather than at its edge: an
            // underline at 1.0 is the next line's overline, and one at 0.0 is
            // the previous line's.
            let at = kind.rule_at().unwrap_or_else(|| panic!("{kind:?} rules"));
            assert!((0.0..=1.0).contains(&at), "{kind:?} at {at}");
            assert!(
                at - MarkupKind::RULE_THICKNESS / 2.0 > 0.0
                    && at + MarkupKind::RULE_THICKNESS / 2.0 < 1.0,
                "{kind:?} would spill out of the run it belongs to"
            );
            assert_eq!(kind.opacity(), 1.0, "a rule is not laid down faded");
        }
        assert!(
            MarkupKind::StrikeOut.rule_at() < MarkupKind::Underline.rule_at(),
            "a strikeout goes through the words and an underline beneath them"
        );
    }

    #[test]
    fn only_the_bands_default_kind_touches_the_document() {
        // What the gesture code branches on. The mark-gathering kind is the
        // one that edits; the other two put something outside the process.
        assert!(!SelectKind::Marks.copies());
        assert!(SelectKind::Image.copies());
        assert!(SelectKind::Text.copies());
        assert_eq!(
            SelectKind::default(),
            SelectKind::Marks,
            "the band must keep doing what it has always done until it is told otherwise"
        );
    }

    #[test]
    fn every_tool_with_a_panel_has_something_in_it() {
        // A control that opens an options panel with nothing in it teaches
        // people not to open options panels, so this is not cosmetic: it is
        // what decides whether the arrow is drawn at all. The text selection
        // has nothing to configure at all; every other tool has a colour, a
        // size or a mode — the stamp included, now that which mark it puts
        // down is a mode of the tool.
        for tool in AnnotationTool::ALL {
            let expected = !matches!(tool, AnnotationTool::SelectText);
            assert_eq!(
                tool.has_options(),
                expected,
                "{tool:?} draws an options arrow with nothing behind it, or hides one"
            );
        }
        assert!(AnnotationTool::Stamp.has_options());
        assert!(AnnotationTool::Shape.has_options());
    }

    #[test]
    fn a_stroke_records_the_points_the_hand_actually_moved_through() {
        let annotations = drawn(&[(0.1, 0.1), (0.3, 0.1), (0.5, 0.1)]);
        assert_eq!(annotations.strokes.len(), 1);
        assert_eq!(annotations.strokes[0].points.len(), 3);
        assert!(!annotations.is_empty());
    }

    #[test]
    fn points_closer_than_the_minimum_distance_are_dropped() {
        // A hand resting on a trackpad reports constantly; none of it is a
        // mark, and all of it would be geometry to tessellate every frame.
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.5, 0.5), WIDTH, RED);
        for step in 0..100 {
            let jitter = step as f32 * 1e-5;
            assert!(
                !annotations.extend_stroke((0.5 + jitter, 0.5)),
                "a tremor is not a stroke"
            );
        }
        assert_eq!(annotations.strokes[0].points.len(), 1);

        assert!(
            annotations.extend_stroke((0.6, 0.5)),
            "real movement is kept"
        );
        assert_eq!(annotations.strokes[0].points.len(), 2);
    }

    #[test]
    fn a_stroke_stops_growing_at_its_point_limit() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.0, 0.0), WIDTH, RED);
        // Every point is far enough apart to be kept, so only the cap stops
        // this.
        for step in 1..(Annotations::MAX_POINTS_PER_STROKE * 2) {
            let x = (step as f32 * Annotations::MIN_POINT_DISTANCE * 2.0) % 1.0;
            annotations.extend_stroke((x, 0.5));
        }
        assert_eq!(
            annotations.strokes[0].points.len(),
            Annotations::MAX_POINTS_PER_STROKE
        );
    }

    #[test]
    fn the_oldest_stroke_is_dropped_rather_than_the_pen_going_dry() {
        let mut annotations = Annotations::default();
        for step in 0..(Annotations::MAX_STROKES + 5) {
            let y = (step as f32) / 1000.0;
            assert!(
                annotations.begin_stroke((0.5, y), WIDTH, RED),
                "the pen always writes"
            );
            let _ = annotations.end_stroke();
        }
        assert_eq!(annotations.strokes.len(), Annotations::MAX_STROKES);
        assert_eq!(
            annotations.strokes[0].points[0].1,
            5.0 / 1000.0,
            "the five oldest strokes went, not the five newest"
        );
    }

    #[test]
    fn coordinates_off_the_page_are_refused_rather_than_clamped() {
        let mut annotations = Annotations::default();
        for point in [
            (-0.01, 0.5),
            (1.5, 0.5),
            (0.5, -2.0),
            (f32::NAN, 0.5),
            (0.5, f32::INFINITY),
        ] {
            assert!(
                !annotations.begin_stroke(point, WIDTH, RED),
                "{point:?} is not on the page"
            );
        }
        assert!(annotations.strokes.is_empty());

        annotations.begin_stroke((0.5, 0.5), WIDTH, RED);
        assert!(!annotations.extend_stroke((1.4, 0.5)));
        assert_eq!(annotations.strokes[0].points.len(), 1);

        annotations.set_pointer(Some((2.0, 0.5)));
        assert_eq!(annotations.pointer, None, "the letterbox holds no pointer");
        annotations.set_spotlight(Some((f32::NAN, 0.1)));
        assert_eq!(annotations.spotlight, None);
    }

    #[test]
    fn a_move_after_the_button_came_up_does_not_extend_anything() {
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.1)]);
        assert!(!annotations.is_drawing());
        assert!(!annotations.extend_stroke((0.9, 0.9)));
        assert_eq!(annotations.strokes[0].points.len(), 2);
    }

    #[test]
    fn clearing_takes_the_marks_but_not_the_pen() {
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.4)]);
        let _ = annotations.arm(Some(AnnotationTool::Ink));
        annotations.audience_visible = true;
        annotations.set_pointer(Some((0.2, 0.2)));

        annotations.clear();

        assert!(annotations.is_empty());
        assert_eq!(
            annotations.tool,
            Some(AnnotationTool::Ink),
            "clearing the page does not put the pen down"
        );
        assert!(annotations.audience_visible);
    }

    #[test]
    fn navigation_discards_everything_because_ink_is_temporary() {
        let retained: bool = Annotations::RETAIN_ACROSS_NAVIGATION;
        assert!(!retained, "ink belongs to the slide it was drawn on");
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.4)]);
        annotations.set_spotlight(Some((0.5, 0.5)));
        let _ = annotations.arm(Some(AnnotationTool::Spotlight));

        annotations.clear_on_slide_change();

        assert!(annotations.is_empty(), "no mark survives a page turn");
        assert_eq!(annotations.tool, Some(AnnotationTool::Spotlight));
    }

    #[test]
    fn arming_a_tool_takes_away_only_the_other_tools_marks() {
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.4)]);
        let _ = annotations.arm(Some(AnnotationTool::Pointer));
        annotations.set_pointer(Some((0.3, 0.3)));

        let _ = annotations.arm(Some(AnnotationTool::Spotlight));
        assert_eq!(annotations.pointer, None, "the dot belonged to the pointer");
        assert_eq!(annotations.strokes.len(), 1, "the ink was committed");

        annotations.set_spotlight(Some((0.6, 0.6)));
        let _ = annotations.arm(None);
        assert_eq!(annotations.spotlight, None);
        assert!(
            !annotations.is_armed(),
            "the pointer is the document's again"
        );
    }

    #[test]
    fn audience_visibility_is_a_deliberate_choice_that_survives_marks_coming_and_going() {
        let mut annotations = Annotations::default();
        assert!(
            annotations.audience_visible,
            "annotations start shown to the audience"
        );
        annotations.audience_visible = false;
        annotations.begin_stroke((0.2, 0.2), WIDTH, RED);
        let _ = annotations.end_stroke();
        annotations.clear_on_slide_change();
        assert!(
            !annotations.audience_visible,
            "turning the page is not a decision about who can see"
        );
    }

    #[test]
    fn a_press_that_went_nowhere_leaves_a_dot_but_never_an_empty_stroke() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.5, 0.5), WIDTH, RED);
        let _ = annotations.end_stroke();
        assert_eq!(annotations.strokes.len(), 1, "a dot is a mark");
        assert!(!annotations.strokes[0].is_empty());

        annotations.strokes[0].points.clear();
        annotations.begin_stroke((0.1, 0.1), WIDTH, RED);
        annotations.strokes.last_mut().unwrap().points.clear();
        let _ = annotations.end_stroke();
        assert_eq!(annotations.strokes.len(), 1, "the empty one went");
    }

    #[test]
    fn absurd_measures_are_brought_back_into_range() {
        let mut style = AnnotationStyle {
            pointer_radius: 40.0,
            pointer_color: InkColor::Cyan,
            spotlight_radius: -1.0,
            ink_width: f32::NAN,
            dim: 5.0,
        };
        style.sanitise();
        assert_eq!(style.pointer_radius, POINTER_RADIUS_RANGE.1);
        assert_eq!(style.spotlight_radius, SPOTLIGHT_RADIUS_RANGE.0);
        assert_eq!(style.ink_width, AnnotationStyle::default().ink_width);
        assert_eq!(style.dim, 0.95);
    }

    #[test]
    fn a_stroke_width_outside_the_range_is_bounded_when_the_stroke_starts() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.5, 0.5), 9.0, RED);
        assert_eq!(annotations.strokes[0].width, INK_WIDTH_RANGE.1);
        let _ = annotations.end_stroke();
        annotations.begin_stroke((0.4, 0.5), f32::NAN, RED);
        assert_eq!(
            annotations.strokes[1].width,
            AnnotationStyle::default().ink_width
        );
    }

    #[test]
    fn a_highlighter_stroke_keeps_its_translucent_kind_and_own_width_range() {
        let mut annotations = Annotations::default();
        assert!(annotations.begin_mark((0.5, 0.5), 9.0, InkColor::Yellow, StrokeKind::Highlight,));
        let _ = annotations.end_stroke();
        let stroke = &annotations.strokes[0];
        assert_eq!(stroke.kind, StrokeKind::Highlight);
        assert_eq!(stroke.width, HIGHLIGHT_WIDTH_RANGE.1);
        assert!(stroke.kind.opacity() < 1.0);
    }

    #[test]
    fn the_eraser_removes_only_a_stroke_it_touches() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.2, 0.2), WIDTH, RED);
        let _ = annotations.end_stroke();
        annotations.begin_stroke((0.8, 0.8), WIDTH, RED);
        let _ = annotations.end_stroke();

        annotations.begin_erase((0.21, 0.2), 0.02);
        let _ = annotations.end_stroke();

        assert_eq!(annotations.strokes.len(), 1);
        assert_eq!(annotations.strokes[0].points[0], (0.8, 0.8));
    }

    #[test]
    fn the_eraser_detects_a_crossing_between_sampled_points() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.2, 0.5), WIDTH, RED);
        annotations.extend_stroke((0.8, 0.5));
        let _ = annotations.end_stroke();

        annotations.begin_erase((0.5, 0.5), ERASER_RADIUS_RANGE.0);
        let _ = annotations.end_stroke();

        assert!(annotations.strokes.is_empty());
    }

    #[test]
    fn a_mixed_colour_survives_the_round_trip_and_compares_equal() {
        let mixed = InkColor::from_rgb(0.2, 0.4, 0.6);
        assert!(mixed.is_custom());
        assert_eq!(mixed, InkColor::from_rgb(0.2, 0.4, 0.6), "same colour");
        assert_ne!(mixed, InkColor::from_rgb(0.2, 0.4, 0.61));

        let (red, green, blue) = mixed.rgb();
        for (component, wanted) in [(red, 0.2), (green, 0.4), (blue, 0.6)] {
            assert!((component - wanted).abs() < 1.0 / 255.0, "{component}");
        }

        // Out-of-range components are brought back rather than wrapped: a
        // colour is a colour, not an arithmetic accident.
        assert_eq!(
            InkColor::from_rgb(-1.0, 2.0, f32::NAN),
            InkColor::Custom {
                red: 0,
                green: 255,
                blue: 0
            }
        );
        assert!(!InkColor::Red.is_custom());
        assert_eq!(mixed.label(), "Custom");
    }

    #[test]
    fn every_ink_colour_is_a_real_colour_with_a_name() {
        for color in InkColor::ALL {
            assert!(!color.label().is_empty());
            let (r, g, b) = color.rgb();
            for component in [r, g, b] {
                assert!((0.0..=1.0).contains(&component), "{color:?}");
            }
        }
        for tool in AnnotationTool::ALL {
            assert!(!tool.label().is_empty());
        }
    }

    /// The seam between the pen and the document.
    ///
    /// These are the four things this type does now that it no longer keeps
    /// the marks itself: it hands a finished stroke over, it accepts the name
    /// the document gave it, it takes what the document says is on the slide,
    /// and it reports what the eraser took so the document can be told.
    mod the_document_owns_the_marks {
        use super::*;
        use crate::annotate::AnnotationId;

        fn named(name: &str) -> AnnotationId {
            AnnotationId::imported(name).expect("a name")
        }

        #[test]
        fn the_pen_coming_up_hands_the_stroke_over() {
            let mut annotations = Annotations::default();
            assert!(annotations.begin_stroke((0.2, 0.2), WIDTH, RED));
            annotations.extend_stroke((0.6, 0.6));
            let finished = annotations
                .end_stroke()
                .expect("a drawn stroke is handed over");
            assert_eq!(finished.points.len(), 2);
            assert!(
                finished.id.is_none(),
                "the document has not named it yet, and this type must not invent one"
            );
            // …and it is still on screen, because the presenter drew it and
            // must see it now rather than after a round trip.
            assert_eq!(annotations.strokes.len(), 1);
        }

        #[test]
        fn a_gesture_that_made_no_mark_hands_nothing_over() {
            // An eraser sweep changes the document but creates nothing, and a
            // release with no pen down is not a stroke at all.
            let mut annotations = Annotations::default();
            // `begin_erase` reports whether it took anything, and over an
            // empty slide it takes nothing — but the sweep is still open, and
            // ending it must still hand nothing over.
            let _ = annotations.begin_erase((0.5, 0.5), 0.03);
            assert!(annotations.end_stroke().is_none());
            assert!(annotations.end_stroke().is_none());

            // A press that went nowhere leaves a dot, which is a mark.
            assert!(annotations.begin_stroke((0.3, 0.3), WIDTH, RED));
            assert!(annotations.end_stroke().is_some());
        }

        #[test]
        fn the_answer_names_the_stroke_it_was_about() {
            // Commits are answered in order, so the oldest unnamed stroke is
            // the one an answer belongs to — which matters, because a
            // presenter draws faster than a worker answers.
            let mut annotations = Annotations::default();
            for at in [0.2, 0.4, 0.6] {
                assert!(annotations.begin_stroke((at, at), WIDTH, RED));
                let _ = annotations.end_stroke();
            }
            annotations.name_stroke(named("first"));
            annotations.name_stroke(named("second"));
            assert_eq!(annotations.strokes[0].id, Some(named("first")));
            assert_eq!(annotations.strokes[1].id, Some(named("second")));
            assert_eq!(annotations.strokes[2].id, None, "not answered yet");
        }

        #[test]
        fn a_page_turn_takes_what_the_document_says_is_there() {
            let mut annotations = Annotations::default();
            assert!(annotations.begin_stroke((0.2, 0.2), WIDTH, RED));
            let _ = annotations.end_stroke();
            assert_eq!(annotations.strokes.len(), 1);

            // What the engine reported for the page just turned to: two marks
            // that were in the file, one of which nobody in this process ever
            // drew.
            let from_the_document = vec![
                InkStroke {
                    points: vec![(0.1, 0.1), (0.2, 0.2)],
                    width: WIDTH,
                    color: RED,
                    kind: StrokeKind::Ink,
                    id: Some(named("was-in-the-file")),
                },
                InkStroke {
                    points: vec![(0.5, 0.5), (0.6, 0.6)],
                    width: WIDTH,
                    color: RED,
                    kind: StrokeKind::Highlight,
                    id: Some(named("made-in-document-mode")),
                },
            ];
            annotations.adopt(from_the_document.clone());
            assert_eq!(annotations.strokes[..2], from_the_document[..]);
            // The stroke drawn a moment ago is still there: it has no name, so
            // its commit has not been answered, so the list the engine sent
            // was made before it existed. A page turn is the case where it
            // *does* go, and `clear_on_slide_change` is what takes it.
            assert_eq!(annotations.strokes.len(), 3);
            assert!(annotations.strokes[2].id.is_none());
            annotations.clear_on_slide_change();
            annotations.adopt(from_the_document.clone());
            assert_eq!(annotations.strokes, from_the_document);
        }

        #[test]
        fn a_page_turn_does_not_snatch_the_pen_out_of_a_moving_hand() {
            // The one case `adopt` has to be careful about: an answer can
            // arrive mid-stroke, and the stroke belongs to the hand, not to
            // the document (A2).
            let mut annotations = Annotations::default();
            assert!(annotations.begin_stroke((0.2, 0.2), WIDTH, RED));
            annotations.extend_stroke((0.3, 0.3));
            annotations.adopt(vec![InkStroke {
                points: vec![(0.9, 0.9)],
                width: WIDTH,
                color: RED,
                kind: StrokeKind::Ink,
                id: Some(named("arrived-mid-stroke")),
            }]);
            assert_eq!(annotations.strokes.len(), 2);
            assert!(annotations.is_drawing());
            // Still the same open stroke, still extensible.
            assert!(annotations.extend_stroke((0.4, 0.4)));
            assert_eq!(annotations.strokes.last().unwrap().points.len(), 3);
            assert_eq!(annotations.strokes.last().unwrap().id, None);
        }

        #[test]
        fn a_re_adopted_label_keeps_the_same_local_id() {
            // §82.6: the Typst compile cache is keyed by `TextMark::id`, so
            // an id that changed on every `Applied` answer would recompile
            // every label on the slide for nothing. A label's PDF name is
            // stable across the round trip, so the id derived from it must
            // be too.
            let mark = |text: &str| TextMark {
                id: 0,
                position: (0.3, 0.3),
                text: text.to_string(),
                size: 0.025,
                color: RED,
                annotation: Some(named("label")),
                note: false,
                fit: None,
            };
            let mut annotations = Annotations::default();
            annotations.adopt_texts(vec![mark("first draft")]);
            let first_id = annotations.texts[0].id;
            assert_ne!(first_id, 0);

            annotations.adopt_texts(vec![mark("first draft")]);
            assert_eq!(
                annotations.texts[0].id, first_id,
                "the same name must adopt to the same id"
            );

            annotations.adopt_texts(vec![TextMark {
                annotation: Some(named("a different label")),
                ..mark("first draft")
            }]);
            assert_ne!(
                annotations.texts[0].id, first_id,
                "a different name must not collide"
            );
        }

        #[test]
        fn the_eraser_reports_what_it_took_so_the_document_can_be_told() {
            let mut annotations = Annotations::default();
            annotations.adopt(vec![
                InkStroke {
                    points: vec![(0.2, 0.5), (0.4, 0.5)],
                    width: WIDTH,
                    color: RED,
                    kind: StrokeKind::Ink,
                    id: Some(named("crossed")),
                },
                InkStroke {
                    points: vec![(0.8, 0.9), (0.9, 0.9)],
                    width: WIDTH,
                    color: RED,
                    kind: StrokeKind::Ink,
                    id: Some(named("missed")),
                },
            ]);
            assert!(annotations.begin_erase((0.3, 0.5), 0.03));
            assert_eq!(annotations.take_erased(), vec![named("crossed")]);
            assert_eq!(annotations.strokes.len(), 1);
            assert!(
                annotations.take_erased().is_empty(),
                "a sweep reported twice is a delete sent twice"
            );
        }

        #[test]
        fn erasing_a_stroke_the_document_has_not_named_reports_nothing() {
            // The stroke's commit is still in flight. Taking it off the screen
            // is right; naming an annotation that does not exist yet is not,
            // and would be refused by the engine anyway.
            let mut annotations = Annotations::default();
            assert!(annotations.begin_stroke((0.3, 0.5), WIDTH, RED));
            annotations.extend_stroke((0.5, 0.5));
            let _ = annotations.end_stroke();
            assert!(annotations.begin_erase((0.4, 0.5), 0.03));
            assert!(annotations.strokes.is_empty());
            assert!(annotations.take_erased().is_empty());
        }

        #[test]
        fn erasing_before_the_answer_does_not_misname_the_next_stroke() {
            // §76.14: draw A, erase A before its commit is answered, draw B,
            // then deliver both answers in the order they were sent. The
            // document must end up with exactly B under B's id — not A's id
            // stuck on B while A is orphaned in the file.
            let mut annotations = Annotations::default();
            assert!(annotations.begin_stroke((0.2, 0.2), WIDTH, RED));
            let _ = annotations.end_stroke();
            assert!(annotations.begin_erase((0.2, 0.2), 0.03));
            assert!(
                annotations.strokes.is_empty(),
                "A is off the screen before it ever had a name"
            );
            assert!(
                annotations.take_erased().is_empty(),
                "A has no name yet, so nothing can be sent to delete it"
            );
            assert!(annotations.begin_stroke((0.6, 0.6), WIDTH, RED));
            let _ = annotations.end_stroke();

            // Answers arrive in commit order: A's, then B's.
            annotations.name_stroke(named("a"));
            annotations.name_stroke(named("b"));

            assert_eq!(
                annotations.take_erased(),
                vec![named("a")],
                "A's arriving name became an immediate delete"
            );
            assert_eq!(annotations.strokes.len(), 1);
            assert_eq!(
                annotations.strokes[0].id,
                Some(named("b")),
                "B kept its own name"
            );
            assert_eq!(annotations.strokes[0].points, vec![(0.6, 0.6)]);
        }

        #[test]
        fn a_finished_label_is_handed_over_and_an_empty_one_is_not() {
            let mut annotations = Annotations::default();
            assert!(annotations.begin_text((0.3, 0.3), 0.025, RED, false).0);
            assert!(annotations.type_text("Hello"));
            let finished = annotations
                .finish_text()
                .expect("a typed label is handed over");
            assert_eq!(finished.text, "Hello");
            assert_eq!(finished.annotation, None);
            annotations.name_text(named("the-label"));
            assert_eq!(annotations.texts[0].annotation, Some(named("the-label")));

            // A label with nothing typed into it is not an annotation.
            let (started, closed) = annotations.begin_text((0.7, 0.7), 0.025, RED, false);
            assert!(started);
            assert!(
                closed.is_none(),
                "the first label was already finished by hand"
            );
            assert!(annotations.finish_text().is_none());
            assert_eq!(annotations.texts.len(), 1);
        }

        #[test]
        fn settling_a_gesture_is_not_the_same_as_having_something_to_undo() {
            // `has_open_gesture` answers only "is the pen down". What there is
            // to undo is the document's answer, and this type must not pretend
            // to know it.
            let mut annotations = Annotations::default();
            assert!(!annotations.has_open_gesture());
            assert!(annotations.begin_stroke((0.2, 0.2), WIDTH, RED));
            assert!(annotations.has_open_gesture());
            assert!(annotations.settle().is_none());
            assert!(!annotations.has_open_gesture());
            assert_eq!(annotations.strokes.len(), 1, "settling keeps the mark");
        }

        #[test]
        fn a_label_and_a_note_are_the_same_gesture_and_differ_at_the_commit() {
            let mut annotations = Annotations::default();
            assert!(annotations.begin_text((0.3, 0.3), 0.025, RED, true).0);
            assert!(annotations.type_text("ask about the third column"));
            let finished = annotations.finish_text().expect("a typed note");
            assert!(finished.note, "the commit is what tells the two apart");

            // Reaching from one to the other does not end the typing: they are
            // one gesture wearing two names.
            assert!(annotations.begin_text((0.6, 0.6), 0.025, RED, false).0);
            assert!(annotations.arm(Some(AnnotationTool::Note)).is_none());
            assert!(annotations.is_typing());
            // Any other tool does end it, and hands the label over.
            assert!(annotations.type_text("a label"));
            let closed = annotations
                .arm(Some(AnnotationTool::Ink))
                .expect("the label is handed over rather than dropped");
            assert_eq!(closed.text, "a label");
        }

        #[test]
        fn a_band_holds_what_it_encloses_and_the_delete_takes_exactly_those() {
            let mut annotations = Annotations::default();
            annotations.adopt(vec![
                InkStroke {
                    points: vec![(0.2, 0.2), (0.3, 0.3)],
                    width: WIDTH,
                    color: RED,
                    kind: StrokeKind::Ink,
                    id: Some(named("inside")),
                },
                InkStroke {
                    points: vec![(0.8, 0.8)],
                    width: WIDTH,
                    color: RED,
                    kind: StrokeKind::Ink,
                    id: Some(named("outside")),
                },
            ]);
            annotations.adopt_notes(vec![NoteMark {
                position: (0.25, 0.25),
                size: (0.02, 0.02),
                color: RED,
                id: named("a-note"),
            }]);

            assert!(annotations.begin_band((0.1, 0.1)));
            assert!(annotations.extend_band((0.5, 0.5)));
            let held = annotations.finish_band().to_vec();
            assert!(held.contains(&named("inside")), "{held:?}");
            assert!(held.contains(&named("a-note")), "{held:?}");
            assert!(!held.contains(&named("outside")), "{held:?}");
            assert!(annotations.band.is_none(), "the band ends at the release");

            let taken = annotations.take_selection();
            assert_eq!(taken.len(), 2);
            assert_eq!(annotations.strokes.len(), 1, "only the held one went");
            assert!(annotations.notes.is_empty());
            assert!(annotations.selected.is_empty());
        }

        #[test]
        fn a_mark_the_document_has_not_named_cannot_be_held() {
            // Its commit is still in flight, so there is no name to hold it by
            // — and a moment later there will be.
            let mut annotations = Annotations::default();
            assert!(annotations.begin_stroke((0.2, 0.2), WIDTH, RED));
            let _ = annotations.end_stroke();
            assert!(annotations.begin_band((0.0, 0.0)));
            assert!(annotations.extend_band((1.0, 1.0)));
            assert!(annotations.finish_band().is_empty());
        }
    }
}
