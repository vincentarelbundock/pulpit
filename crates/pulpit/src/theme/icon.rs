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
    /// The audience can see the marks: `eye`.
    Eye,
    /// The marks are the presenter's alone: `eye-off`.
    EyeOff,
    /// Opens a panel below its trigger: `chevron-down`.
    ChevronDown,
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
    /// The previous page: `chevron-left`.
    ChevronLeft,
    /// The next page: `chevron-right`.
    ChevronRight,
    /// The bookmark tree: `list-tree`.
    Outline,
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
}

impl Icon {
    /// The vendored file, compiled in so the binary carries its own icons.
    fn source(self) -> &'static [u8] {
        match self {
            Icon::Pen => include_bytes!("../../assets/icons/pen.svg"),
            Icon::Highlighter => include_bytes!("../../assets/icons/highlighter.svg"),
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
            Icon::Close => include_bytes!("../../assets/icons/x.svg"),
            Icon::Pencil => include_bytes!("../../assets/icons/pencil.svg"),
            Icon::Type => include_bytes!("../../assets/icons/type.svg"),
            Icon::ArrowLeft => include_bytes!("../../assets/icons/arrow-left.svg"),
            Icon::Menu => include_bytes!("../../assets/icons/menu.svg"),
            Icon::Ellipsis => include_bytes!("../../assets/icons/ellipsis.svg"),
            Icon::Gear => include_bytes!("../../assets/icons/settings.svg"),
            Icon::Save => include_bytes!("../../assets/icons/save.svg"),
            Icon::StickyNote => include_bytes!("../../assets/icons/sticky-note.svg"),
            Icon::Stamp => include_bytes!("../../assets/icons/stamp.svg"),
            Icon::Select => include_bytes!("../../assets/icons/mouse-pointer.svg"),
            Icon::ZoomIn => include_bytes!("../../assets/icons/zoom-in.svg"),
            Icon::ZoomOut => include_bytes!("../../assets/icons/zoom-out.svg"),
            Icon::FitWidth => include_bytes!("../../assets/icons/move-horizontal.svg"),
            Icon::FitPage => include_bytes!("../../assets/icons/maximize.svg"),
            Icon::FitHeight => include_bytes!("../../assets/icons/move-vertical.svg"),
            Icon::Crop => include_bytes!("../../assets/icons/crop.svg"),
            Icon::ChevronLeft => include_bytes!("../../assets/icons/chevron-left.svg"),
            Icon::ChevronRight => include_bytes!("../../assets/icons/chevron-right.svg"),
            Icon::Outline => include_bytes!("../../assets/icons/list-tree.svg"),
            Icon::Document => include_bytes!("../../assets/icons/file-text.svg"),
            Icon::Check => include_bytes!("../../assets/icons/check.svg"),
            Icon::SinglePage => include_bytes!("../../assets/icons/file.svg"),
            Icon::TwoPages => include_bytes!("../../assets/icons/book-open.svg"),
            Icon::RotatePage => include_bytes!("../../assets/icons/rotate-cw-square.svg"),
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

    const ALL: [Icon; 37] = [
        Icon::Pen,
        Icon::Highlighter,
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
        Icon::Close,
        Icon::Pencil,
        Icon::Type,
        Icon::ArrowLeft,
        Icon::Menu,
        Icon::Ellipsis,
        Icon::Gear,
        Icon::Save,
        Icon::StickyNote,
        Icon::Stamp,
        Icon::Select,
        Icon::ZoomIn,
        Icon::ZoomOut,
        Icon::FitWidth,
        Icon::FitPage,
        Icon::FitHeight,
        Icon::Crop,
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::Outline,
        Icon::Document,
        Icon::Check,
        Icon::SinglePage,
        Icon::TwoPages,
        Icon::RotatePage,
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
