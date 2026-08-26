//! The one place the keyboard's priority order is written down.
//!
//! Every key the presenter window hears descends this ladder, top rung
//! first. A rung that is *active* gets the press; if its handler consumes
//! the key the descent stops, and if not — an Escape rung offered a letter,
//! the overview offered a key that is not a grid motion — the press falls
//! through to the next rung. The bottom rung is the keymap, so a shortcut
//! only ever means its bound action when nothing above wanted the key more.
//!
//! Escape needs no ordering of its own: it closes the topmost open thing
//! because the innermost contexts sit on the highest rungs, which is the
//! ordinary reading of Escape everywhere. The order below is behaviour, not
//! bookkeeping — moving a rung moves who wins a contested key, which is why
//! the order lives here as data, with the reasons alongside, rather than as
//! the sequence of early returns it used to be. The handlers themselves stay
//! on `App` (`rung_active`, `on_rung`): a rung is a claim about *when* a
//! surface owns the keyboard; what it does with a key is that surface's own
//! business.

use crate::settings::Mods;

/// One key press, as every rung sees it.
///
/// `captured` is the toolkit's report that some widget already handled the
/// event — a text box taking a character, a scrollable taking an arrow. It
/// rides along rather than ending the descent, because a rung above
/// [`Rung::CapturedWidget`] outranks the widget and one action class reaches
/// below it (see [`crate::settings::keys::Action::reaches_captured`]).
pub struct KeyPress<'a> {
    pub key: Option<&'a str>,
    /// The text the press would insert, for the one rung that is typing.
    pub text: Option<&'a str>,
    /// The raw scancode, for remote keys the toolkit cannot name.
    pub scancode: Option<u32>,
    pub mods: Mods,
    pub captured: bool,
}

/// A context that can own the keyboard, named. §see `docs-src/internals.typ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// A label being typed on a slide owns everything: the characters are
    /// the point, and any key that reached a binding from here would be a
    /// shortcut fired by someone writing a word.
    AnnotationTyping,
    /// A document-mode mark being written. Escape is the way out; nothing
    /// falls through.
    ComposingMark,
    /// Marks held by the presenter's rubber band answer Delete, Backspace
    /// and Escape — the same keys document mode's selection answers — and
    /// only those, so every other key keeps its meaning while marks are
    /// held.
    HeldMarks,
    /// The open overview grid owns navigation: arrows, page keys, Home/End,
    /// Enter. An arrow at the grid's edge is still the grid's. Keys it does
    /// not recognise — Escape among them — continue down.
    OverviewGrid,
    /// A widget the toolkit says captured the press — a text box, a slider.
    /// It owns the key, bar the few commanding shortcuts that declare
    /// otherwise (`Action::reaches_captured`); everything else stops here.
    CapturedWidget,
    /// The settings page's reset-colours confirmation. Escape declines.
    ConfirmResetColors,
    /// A document-requested jump awaiting consent (§8.6). Escape declines,
    /// which safely leaves the reader where they already are.
    PendingFormGoto,
    /// A save under review. Escape declines: writing no file leaves
    /// everything as it was.
    PendingSaveReview,
    /// A cue going off. Escape acknowledges it — hands are not always on
    /// the mouse — and the ringing outranks the popup that configured it.
    AlarmRinging,
    /// The timer's overrun, pulsing for the same reason and answered the
    /// same way.
    TimerOvertime,
    /// The open alarm popup. Escape closes it.
    AlarmPopup,
    /// The open timer popup. Escape closes it.
    TimerPopup,
    /// A crop rectangle mid-draw is the innermost thing on the page. Escape
    /// takes it back and leaves the tool armed: the reader who mis-drew one
    /// meant to draw another.
    CropMarquee,
    /// The search workspace. Escape closes it and restores the rail.
    SearchWorkspace,
    /// An open annotation panel or its overflow. Escape closes it without
    /// also cancelling the preview behind it.
    AnnotationPanel,
    /// The presenter's own popups — shortcuts, about, properties, menus,
    /// and the overview itself. Escape closes them all; backing out of the
    /// overview also abandons the preview it was moving.
    PresenterPopups,
    /// Any page that is not the presenter: settings, library, the layout
    /// editor. It owns every key — presenter shortcuts must not blank the
    /// audience while someone types a layout name — and Escape on the pages
    /// with a Back button means that button.
    EditorPages,
    /// A focused media overlay owns every key; Escape is interpreted by the
    /// overlay router as releasing focus. No press falls through.
    MediaOverlay,
    /// The document viewer, when a Reader layout is on screen: form fields,
    /// Tab traversal, the toolbar digits, Page Up/Down as *scrolling*. Keys
    /// it does not claim continue to the keymap, where the presenter's
    /// bindings mean what they always did.
    DocumentViewer,
    /// Reader fullscreen, dead last before the keymap: Escape reveals the
    /// band and the rail, but only after everything above declined the key.
    ReaderFullscreen,
    /// The fixed keymap, then the presenter-remote hardware aliases. The
    /// floor of the ladder: a key that means nothing here means nothing.
    Keymap,
}

#[cfg(test)]
impl Rung {
    /// Every rung, for the exhaustiveness check. Order is meaningless here;
    /// [`LADDER`] is the order.
    pub const ALL: [Rung; 21] = [
        Rung::AnnotationTyping,
        Rung::ComposingMark,
        Rung::HeldMarks,
        Rung::OverviewGrid,
        Rung::CapturedWidget,
        Rung::ConfirmResetColors,
        Rung::PendingFormGoto,
        Rung::PendingSaveReview,
        Rung::AlarmRinging,
        Rung::TimerOvertime,
        Rung::AlarmPopup,
        Rung::TimerPopup,
        Rung::CropMarquee,
        Rung::SearchWorkspace,
        Rung::AnnotationPanel,
        Rung::PresenterPopups,
        Rung::EditorPages,
        Rung::MediaOverlay,
        Rung::DocumentViewer,
        Rung::ReaderFullscreen,
        Rung::Keymap,
    ];
}

/// The order itself. Top to bottom; first active rung to consume the key
/// wins.
pub const LADDER: [Rung; 21] = [
    // Typing surfaces first: while characters are being written, letters
    // are letters.
    Rung::AnnotationTyping,
    Rung::ComposingMark,
    // A held selection outranks the grid and the widgets: Delete must mean
    // the marks wherever the press lands.
    Rung::HeldMarks,
    // The grid outranks capture because its own scrollable would otherwise
    // swallow the vertical arrows the grid is for.
    Rung::OverviewGrid,
    Rung::CapturedWidget,
    // The Escape ladder proper, innermost surface to outermost: a
    // confirmation over a page, a demand for attention over the popup that
    // made it, a marquee over the search pane it is drawn beside.
    Rung::ConfirmResetColors,
    Rung::PendingFormGoto,
    Rung::PendingSaveReview,
    Rung::AlarmRinging,
    Rung::TimerOvertime,
    Rung::AlarmPopup,
    Rung::TimerPopup,
    Rung::CropMarquee,
    Rung::SearchWorkspace,
    Rung::AnnotationPanel,
    Rung::PresenterPopups,
    // Whole-page owners, after the popups so a dialog over the settings
    // page closes before the page itself is left.
    Rung::EditorPages,
    Rung::MediaOverlay,
    Rung::DocumentViewer,
    // Fullscreen wants Escape *least*: a marquee, a panel, an overlay and a
    // form all answer it first.
    Rung::ReaderFullscreen,
    Rung::Keymap,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn position(rung: Rung) -> usize {
        LADDER
            .iter()
            .position(|step| *step == rung)
            .expect("every rung is on the ladder")
    }

    #[test]
    fn the_ladder_holds_every_rung_exactly_once() {
        // A rung missing from LADDER is a context whose keys silently mean
        // nothing; a duplicate is an order contradiction.
        for rung in Rung::ALL {
            assert_eq!(
                LADDER.iter().filter(|step| **step == rung).count(),
                1,
                "{rung:?} must appear on the ladder exactly once"
            );
        }
    }

    #[test]
    fn typing_outranks_everything() {
        // While characters are being written, "n" is a letter — never the
        // next page, whatever else is open.
        assert_eq!(position(Rung::AnnotationTyping), 0);
        assert_eq!(position(Rung::ComposingMark), 1);
    }

    #[test]
    fn the_grid_outranks_the_widget_that_scrolls_it() {
        // The overview's own scrollable captures the vertical arrows; the
        // grid must hear them first or up and down would move the scrollbar
        // and not the selection.
        assert!(position(Rung::OverviewGrid) < position(Rung::CapturedWidget));
    }

    #[test]
    fn a_captured_widget_outranks_every_escape_rung() {
        // A text box with the caret keeps Escape-adjacent keys to itself;
        // only the declared punch-through actions reach past it, and those
        // are handled inside the CapturedWidget rung, not below it.
        for rung in [
            Rung::ConfirmResetColors,
            Rung::PendingFormGoto,
            Rung::PendingSaveReview,
            Rung::AlarmRinging,
            Rung::TimerOvertime,
            Rung::AlarmPopup,
            Rung::TimerPopup,
            Rung::CropMarquee,
            Rung::SearchWorkspace,
            Rung::AnnotationPanel,
            Rung::PresenterPopups,
        ] {
            assert!(
                position(Rung::CapturedWidget) < position(rung),
                "{rung:?} must not take keys from a widget that owns them"
            );
        }
    }

    #[test]
    fn attention_outranks_the_popup_that_configured_it() {
        // A ringing cue is dismissed before the alarm popup closes, and an
        // overrun before the timer popup: the thing demanding attention
        // takes the first Escape.
        assert!(position(Rung::AlarmRinging) < position(Rung::AlarmPopup));
        assert!(position(Rung::TimerOvertime) < position(Rung::TimerPopup));
    }

    #[test]
    fn the_marquee_is_taken_back_before_the_search_pane_closes() {
        assert!(position(Rung::CropMarquee) < position(Rung::SearchWorkspace));
    }

    #[test]
    fn fullscreen_wants_escape_least() {
        // Everything above it answers Escape first; only the keymap sits
        // below.
        assert_eq!(position(Rung::ReaderFullscreen), LADDER.len() - 2);
    }

    #[test]
    fn the_keymap_is_the_floor() {
        // Bindings fire only when no surface wanted the key: the whole
        // reason a digit can arm a slide tool in one mode and a document
        // tool in the other without being a conflict.
        assert_eq!(position(Rung::Keymap), LADDER.len() - 1);
    }
}
