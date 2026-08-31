//! The application's icons, drawn from one set at one weight.
//!
//! Every glyph the interface draws comes from [Lucide], vendored under
//! `assets/icons/` and rendered through iced's SVG widget. Two things follow
//! from that choice and are the reason it was made:
//!
//! * The icons are *drawings*, not characters. Reaching for `✕` or `▾` in a
//!   `text` widget hands the shape to whatever font the system happened to
//!   pick, which is why those glyphs used to sit at a different weight than
//!   the hand-drawn canvas icons beside them.
//! * Lucide draws in `currentColor`, so a single [`Icon`] is tinted at the
//!   call site — the same glyph is text-coloured in the chrome and ink-coloured
//!   in the annotation palette without a second asset.
//!
//! Adding an icon is dropping its `.svg` into `assets/icons/` and adding a
//! variant here. Keep the file as Lucide ships it: the 24-unit box and the
//! 2-unit stroke are what make the set look like a set.
//!
//! [Lucide]: https://lucide.dev — ISC licensed; see `LICENSES/LUCIDE-LICENSE`.

use std::sync::OnceLock;

use iced::widget::svg;
use iced::{Color, Element, Length};

/// One glyph from the vendored Lucide set.
///
/// The names are the interface's words rather than Lucide's file names where
/// the two differ, so a reader here does not have to know that "clear the
/// annotations" is drawn with a wastebasket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Freehand ink: `pen`.
    Pen,
    /// The broad translucent stroke: `highlighter`.
    Highlighter,
    /// The highlighter's second mark, a rule under the words: `underline`.
    Underline,
    /// Its third, a rule through them: `strikethrough`.
    StrikeOut,
    /// `eraser`.
    Eraser,
    /// Nothing armed: a press reaches the slide's own links, media and
    /// navigation instead of laying down a mark: `hand`.
    Hand,
    /// The dot that follows the presenter's hand: `mouse-pointer-2`.
    Pointer,
    /// The lit circle, drawn as what it does to attention: `focus`.
    Spotlight,
    /// Step back one stroke: `undo-2`.
    Undo,
    /// `redo-2`.
    Redo,
    /// Discard everything: `trash-2`.
    Trash,
    /// Write the marks out as a file: `save`.
    Save,
    /// Send the document to a printer: `printer`.
    Printer,
    /// The audience can see the marks: `eye`.
    Eye,
    /// The marks are the presenter's alone: `eye-off`.
    EyeOff,
    /// Opens a panel below its trigger: `chevron-down`.
    ChevronDown,
    /// Increase a stepped value: `chevron-up`.
    ChevronUp,
    /// Dismiss: `x`.
    Close,
    /// Edit in place: `pencil`.
    Pencil,
    /// Text annotation: `type`.
    Type,
    /// Back to where this came from: `arrow-left`.
    ArrowLeft,
    /// Opens the presenter menu: `menu`.
    Menu,
    /// What did not fit, one press away: `ellipsis`.
    Ellipsis,
    /// This reading is also a control: `settings`.
    Gear,

    // Document mode. A reader has controls a presenter has no use for — you
    // do not zoom a slide or pick a mark up off it — so these are their own
    // run rather than being folded into the palette's set above.
    /// A sticky note left on a page: `sticky-note`.
    StickyNote,
    /// A check, a cross or a visible mark placed on a page: `stamp`.
    Stamp,
    /// The shape tool's box, and the mark it draws: `square`.
    Rectangle,
    /// Its ellipse: `circle`.
    Ellipse,
    /// Its line: `minus`, which is a stroke drawn between two points.
    Line,
    /// Its arrow: `move-up-right`.
    Arrow,
    /// Pick an existing annotation up: `mouse-pointer`.
    ///
    /// Distinct from [`Icon::Pointer`], which is the dot that follows a
    /// presenter's hand; this one is an arrow that picks things up.
    Select,
    /// `zoom-in`.
    ZoomIn,
    /// `zoom-out`.
    ZoomOut,
    /// Fit the page's width to the cell: `move-horizontal`.
    FitWidth,
    /// Fit the whole page in the cell: `maximize`.
    FitPage,
    /// Fit the page.s height to the cell: `move-vertical`.
    FitHeight,
    /// Draw a rectangle on the page and read through it: `crop`.
    Crop,
    /// The band gathers the annotations it encloses: `lasso-select`.
    Lasso,
    /// The band copies its region as a picture: `image`.
    Picture,
    /// The band copies the text under it: `text-select`.
    TextRegion,
    /// The select-text tool, which sweeps characters rather than bounding an
    /// area: `text-cursor`, the I-beam it also wears as a cursor.
    TextCursor,
    /// The previous page: `chevron-left`.
    ChevronLeft,
    /// The next page: `chevron-right`.
    ChevronRight,
    /// The bookmark tree: `list-tree`.
    Outline,
    /// One bookmark, and the sidebar tab holding the tree of them:
    /// `bookmark`.
    Bookmark,
    /// Bookmark the page being shown: `bookmark-plus`.
    BookmarkPlus,
    /// Find text in the document: `search`.
    Search,
    /// A document, for a page thumbnail with nothing in it yet: `file-text`.
    Document,
    /// A filled or satisfied thing: `check`.
    Check,
    /// One page across the window: `file`.
    SinglePage,
    /// Two facing pages: `book-open`.
    TwoPages,
    /// Turn every page a quarter turn clockwise: `rotate-cw-square`.
    RotatePage,
    /// Set a stopped timer running: `play`.
    Play,
    /// Hold a running timer where it is: `pause`.
    Pause,
    /// Put a timer back to the beginning: `rotate-ccw`.
    Reset,
    /// Measure where the content sits and act on it — the automatic margin
    /// crop: `scan-text`, corner brackets reading lines of text.
    ScanText,

    // The shortcut reference. Its sheet names itself with a keyboard and
    // marks each category with one line icon, so a reader can find a group
    // by shape before reading a single heading.
    /// The keys themselves: `keyboard`.
    Keyboard,
    /// Moving through the deck: `compass`.
    Compass,
    /// The room's screen, and everything done to it live: `monitor`.
    Monitor,
    /// Reading aloud: `volume-2`.
    Volume,
}

impl Icon {
    /// The vendored file, compiled in so the binary carries its own icons.
    fn source(self) -> &'static [u8] {
        match self {
            Icon::Pen => include_bytes!("../../assets/icons/pen.svg"),
            Icon::Highlighter => include_bytes!("../../assets/icons/highlighter.svg"),
            Icon::Underline => include_bytes!("../../assets/icons/underline.svg"),
            Icon::StrikeOut => include_bytes!("../../assets/icons/strikethrough.svg"),
            Icon::Eraser => include_bytes!("../../assets/icons/eraser.svg"),
            Icon::Hand => include_bytes!("../../assets/icons/hand.svg"),
            Icon::Pointer => include_bytes!("../../assets/icons/mouse-pointer-2.svg"),
            Icon::Spotlight => include_bytes!("../../assets/icons/focus.svg"),
            Icon::Undo => include_bytes!("../../assets/icons/undo-2.svg"),
            Icon::Redo => include_bytes!("../../assets/icons/redo-2.svg"),
            Icon::Trash => include_bytes!("../../assets/icons/trash-2.svg"),
            Icon::Eye => include_bytes!("../../assets/icons/eye.svg"),
            Icon::EyeOff => include_bytes!("../../assets/icons/eye-off.svg"),
            Icon::ChevronDown => include_bytes!("../../assets/icons/chevron-down.svg"),
            Icon::ChevronUp => include_bytes!("../../assets/icons/chevron-up.svg"),
            Icon::Close => include_bytes!("../../assets/icons/x.svg"),
            Icon::Pencil => include_bytes!("../../assets/icons/pencil.svg"),
            Icon::Type => include_bytes!("../../assets/icons/type.svg"),
            Icon::ArrowLeft => include_bytes!("../../assets/icons/arrow-left.svg"),
            Icon::Menu => include_bytes!("../../assets/icons/menu.svg"),
            Icon::Ellipsis => include_bytes!("../../assets/icons/ellipsis.svg"),
            Icon::Gear => include_bytes!("../../assets/icons/settings.svg"),
            Icon::Save => include_bytes!("../../assets/icons/save.svg"),
            Icon::Printer => include_bytes!("../../assets/icons/printer.svg"),
            Icon::StickyNote => include_bytes!("../../assets/icons/sticky-note.svg"),
            Icon::Stamp => include_bytes!("../../assets/icons/stamp.svg"),
            Icon::Rectangle => include_bytes!("../../assets/icons/square.svg"),
            Icon::Ellipse => include_bytes!("../../assets/icons/circle.svg"),
            Icon::Line => include_bytes!("../../assets/icons/minus.svg"),
            Icon::Arrow => include_bytes!("../../assets/icons/move-up-right.svg"),
            Icon::Select => include_bytes!("../../assets/icons/mouse-pointer.svg"),
            Icon::ZoomIn => include_bytes!("../../assets/icons/zoom-in.svg"),
            Icon::ZoomOut => include_bytes!("../../assets/icons/zoom-out.svg"),
            Icon::FitWidth => include_bytes!("../../assets/icons/move-horizontal.svg"),
            Icon::FitPage => include_bytes!("../../assets/icons/expand.svg"),
            Icon::FitHeight => include_bytes!("../../assets/icons/move-vertical.svg"),
            Icon::Crop => include_bytes!("../../assets/icons/crop.svg"),
            Icon::Lasso => include_bytes!("../../assets/icons/lasso-select.svg"),
            Icon::Picture => include_bytes!("../../assets/icons/image.svg"),
            Icon::TextRegion => include_bytes!("../../assets/icons/text-select.svg"),
            Icon::TextCursor => include_bytes!("../../assets/icons/text-cursor.svg"),
            Icon::ChevronLeft => include_bytes!("../../assets/icons/chevron-left.svg"),
            Icon::ChevronRight => include_bytes!("../../assets/icons/chevron-right.svg"),
            Icon::Outline => include_bytes!("../../assets/icons/list-tree.svg"),
            Icon::Bookmark => include_bytes!("../../assets/icons/bookmark.svg"),
            Icon::BookmarkPlus => include_bytes!("../../assets/icons/bookmark-plus.svg"),
            Icon::Search => include_bytes!("../../assets/icons/search.svg"),
            Icon::Document => include_bytes!("../../assets/icons/file-text.svg"),
            Icon::Check => include_bytes!("../../assets/icons/check.svg"),
            Icon::SinglePage => include_bytes!("../../assets/icons/file.svg"),
            Icon::TwoPages => include_bytes!("../../assets/icons/book-open.svg"),
            Icon::RotatePage => include_bytes!("../../assets/icons/rotate-cw-square.svg"),
            Icon::Play => include_bytes!("../../assets/icons/play.svg"),
            Icon::Pause => include_bytes!("../../assets/icons/pause.svg"),
            Icon::Reset => include_bytes!("../../assets/icons/rotate-ccw.svg"),
            Icon::ScanText => include_bytes!("../../assets/icons/scan-text.svg"),
            Icon::Keyboard => include_bytes!("../../assets/icons/keyboard.svg"),
            Icon::Compass => include_bytes!("../../assets/icons/compass.svg"),
            Icon::Monitor => include_bytes!("../../assets/icons/monitor.svg"),
            Icon::Volume => include_bytes!("../../assets/icons/volume-2.svg"),
        }
    }

    /// Position in [`Icon::ALL`], which is also the slot this icon's cached
    /// handle occupies.
    fn index(self) -> usize {
        Icon::ALL
            .iter()
            .position(|icon| *icon == self)
            .expect("every Icon is listed in ALL")
    }

    const ALL: [Icon; 60] = [
        Icon::Pen,
        Icon::Highlighter,
        Icon::Underline,
        Icon::StrikeOut,
        Icon::Eraser,
        Icon::Hand,
        Icon::Pointer,
        Icon::Spotlight,
        Icon::Undo,
        Icon::Redo,
        Icon::Trash,
        Icon::Eye,
        Icon::EyeOff,
        Icon::ChevronDown,
        Icon::ChevronUp,
        Icon::Close,
        Icon::Pencil,
        Icon::Type,
        Icon::ArrowLeft,
        Icon::Menu,
        Icon::Ellipsis,
        Icon::Gear,
        Icon::Save,
        Icon::Printer,
        Icon::StickyNote,
        Icon::Stamp,
        Icon::Rectangle,
        Icon::Ellipse,
        Icon::Line,
        Icon::Arrow,
        Icon::Select,
        Icon::ZoomIn,
        Icon::ZoomOut,
        Icon::FitWidth,
        Icon::FitPage,
        Icon::FitHeight,
        Icon::Crop,
        Icon::Lasso,
        Icon::Picture,
        Icon::TextRegion,
        Icon::TextCursor,
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::Outline,
        Icon::Bookmark,
        Icon::BookmarkPlus,
        Icon::Search,
        Icon::Document,
        Icon::Check,
        Icon::SinglePage,
        Icon::TwoPages,
        Icon::RotatePage,
        Icon::Play,
        Icon::Pause,
        Icon::Reset,
        Icon::ScanText,
        Icon::Keyboard,
        Icon::Compass,
        Icon::Monitor,
        Icon::Volume,
    ];

    /// The handle for this icon, built once per process.
    ///
    /// `Handle::from_memory` hashes the bytes it is given to derive an id, and
    /// the views here rebuild every icon on every view pass — so the handles
    /// are cached rather than re-hashed sixty times a second.
    fn handle(self) -> svg::Handle {
        static HANDLES: OnceLock<Vec<svg::Handle>> = OnceLock::new();
        HANDLES.get_or_init(|| {
            Icon::ALL
                .iter()
                .map(|icon| svg::Handle::from_memory(icon.source()))
                .collect()
        })[self.index()]
        .clone()
    }
}

/// Every variant is in `ALL`, at the slot its own discriminant names.
///
/// `ALL` is what the handle cache is indexed by, so a variant added to the
/// enum and not to the list is a panic the first time something draws it —
/// which is how the shape tools shipped once. A free const rather than an
/// associated one: the compiler only evaluates an associated const somebody
/// reads, which is how the old anchor sat stale on `Reset` while `ScanText`
/// shipped past it without a murmur. `Volume` is the last variant; move this
/// with it.
const _COMPLETE: () = assert!(Icon::ALL.len() == Icon::Volume as usize + 1);

/// An icon at `size` points square, in the palette's text colour.
pub fn icon<'a, Message: 'a>(icon: Icon, size: f32) -> Element<'a, Message> {
    tinted(icon, size, super::ambient::text())
}

/// An icon at `size` points square, in the muted colour — for glyphs that
/// annotate rather than act.
pub fn muted<'a, Message: 'a>(icon: Icon, size: f32) -> Element<'a, Message> {
    tinted(icon, size, super::ambient::muted())
}

/// An icon at `size` points square, in a colour the caller chose.
///
/// The annotation palette uses this to draw the pen and the highlighter in the
/// ink they are about to lay down, which is the whole of what tells the two
/// tools' current colours apart at a glance.
pub fn tinted<'a, Message: 'a>(icon: Icon, size: f32, color: Color) -> Element<'a, Message> {
    svg(icon.handle())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_theme, _status| svg::Style { color: Some(color) })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_carries_a_lucide_drawing() {
        for icon in Icon::ALL {
            let source = std::str::from_utf8(icon.source()).expect("icons are text");
            assert!(
                source.contains("lucide"),
                "{icon:?} is not a Lucide icon — the set has to stay one set"
            );
            // The whole reason a tint works is that Lucide draws in
            // `currentColor`; a file that hard-codes a fill would ignore the
            // palette and stay one colour in every theme.
            assert!(
                source.contains("currentColor"),
                "{icon:?} does not draw in currentColor and cannot be tinted"
            );
            assert!(
                source.contains("viewBox=\"0 0 24 24\""),
                "{icon:?} is not in the 24-unit box the rest of the set uses"
            );
        }
    }

    #[test]
    fn each_icon_has_its_own_drawing_and_its_own_cached_handle() {
        // A copy-pasted `include_bytes!` arm would silently give two icons the
        // same picture, and the index-into-ALL cache would then be wrong in a
        // way nothing else notices.
        for (position, icon) in Icon::ALL.iter().enumerate() {
            assert_eq!(icon.index(), position);
            for other in &Icon::ALL[position + 1..] {
                assert_ne!(
                    icon.source(),
                    other.source(),
                    "{icon:?} and {other:?} draw the same picture"
                );
            }
        }
    }

    #[test]
    fn handles_are_built_once_and_handed_out_repeatedly() {
        assert_eq!(Icon::Pen.handle(), Icon::Pen.handle());
        assert_ne!(Icon::Pen.handle(), Icon::Pencil.handle());
    }
}
