//! PDFium's interactive form-fill environment (§8.6).
//!
//! This is the spike §14.3 step 6 calls for, and then the thing it decided in
//! favour of. The question it had to answer was whether a PDF form can be
//! filled *in place* — by PDFium, drawing into the page bitmap, under the
//! field's own `/DA` — rather than by pulpit drawing its own text boxes over
//! the page and writing values back.
//!
//! The difference is not cosmetic. A comb field, an auto-sizing field, a
//! right-quadded field, a multiline field, a checkbox with a `/ZapfDingbats`
//! appearance: each is a small pile of rules about how a value is drawn, and
//! every one of them is already implemented, in PDFium, by the code that
//! generates the appearance stream. An application-drawn editor has to imitate
//! all of it and will disagree somewhere — and the place it disagrees is
//! between what the person typing sees and what the file will show anyone
//! else. Handing the editing to PDFium removes that entire class of bug by
//! removing the second implementation.
//!
//! # What the environment is
//!
//! `FPDFDOC_InitFormFillEnvironment` takes a `FPDF_FORMFILLINFO`: a C struct of
//! callbacks through which PDFium asks its host to do the things a viewer does
//! — invalidate a rectangle, set a cursor, run a timer, open a URL. pulpit
//! implements the ones that are required and refuses the ones that would let a
//! document reach outside itself.
//!
//! That refusal is the interesting half. A PDF can carry JavaScript, can ask to
//! navigate to a URL, email itself, upload itself, or download from a URL. This
//! environment answers *no* to all of it: no JS platform, no URI actions, no
//! file access, no network. A form is a thing you type values into, and none of
//! those capabilities is needed to type a value into one (A8).
//!
//! # How the callbacks find their state
//!
//! Every callback receives `pThis`, the pointer PDFium was given. The struct
//! below is `#[repr(C)]` with `FPDF_FORMFILLINFO` as its *first* field, so that
//! pointer is also a pointer to the whole thing — the standard C way of
//! attaching host state to a callback interface, and the reason the field order
//! here is load-bearing rather than stylistic.
//!
//! The environment is therefore pinned: it is boxed, and the box is never moved
//! after PDFium has been handed its address. It also must outlive the form
//! handle, because PDFium keeps the pointer.

use std::os::raw::{c_char, c_int, c_ulong, c_ushort};

use crate::document::limits;
use pdfium_render::prelude::{
    PdfiumLibraryBindings, FPDF_BOOL, FPDF_DOCUMENT, FPDF_FORMFILLINFO, FPDF_FORMHANDLE, FPDF_PAGE,
    FPDF_WIDESTRING,
};

/// PDFium's `FPDF_SYSTEMTIME`, mirrored.
///
/// The generated binding for it sits behind a private module in
/// `pdfium-render`, so the one type in this file that is a `struct` rather than
/// a transparent alias has to be written out. Eight `unsigned short`s in this
/// order, per `fpdf_formfill.h`; the layout is asserted below rather than
/// trusted, because a mismatch here would be a callback writing the wrong bytes
/// into C's stack frame.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct SystemTime {
    year: c_ushort,
    month: c_ushort,
    day_of_week: c_ushort,
    day: c_ushort,
    hour: c_ushort,
    minute: c_ushort,
    second: c_ushort,
    milliseconds: c_ushort,
}

/// A rectangle of a page that must be drawn again, in PDF user space.
///
/// PDFium reports invalidations in the page's own coordinates rather than in
/// the bitmap's, because it does not know what size the host is drawing at.
/// Turning one into pixels is the caller's job and depends on the render it is
/// composited into (§9.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirtyRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl DirtyRect {
    /// The smallest rectangle covering both.
    ///
    /// Used to coalesce a burst of invalidations into one: a single keystroke
    /// in a text field can invalidate the caret, the text run and the field
    /// background separately, and three redraws of overlapping rectangles is
    /// three times the work for one picture.
    pub fn union(self, other: DirtyRect) -> DirtyRect {
        DirtyRect {
            left: self.left.min(other.left),
            top: self.top.max(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }

    /// Whether this rectangle covers no area at all.
    ///
    /// Deliberately orientation-agnostic. `FFI_Invalidate`'s parameters are
    /// named `top` and `bottom`, but PDFium hands them over in whichever order
    /// the widget's rectangle happened to be written in the file — and a
    /// rectangle rejected for being "upside down" is a redraw that does not
    /// happen, which shows up as a field that does not repaint while it is
    /// being typed into. Area is the question; winding is not.
    pub fn is_empty(&self) -> bool {
        (self.right - self.left).abs() <= f64::EPSILON
            || (self.top - self.bottom).abs() <= f64::EPSILON
    }
}

/// The host side of PDFium's form-fill environment.
///
/// `info` MUST stay the first field: PDFium hands each callback the address it
/// was given, and every callback here casts it straight back to this type.
#[repr(C)]
pub struct FormEnvironment {
    info: FPDF_FORMFILLINFO,
    /// What PDFium has asked to have redrawn since the last look.
    dirty: Vec<DirtyRect>,
    /// Whether a field's value changed since the last look.
    ///
    /// PDFium calls `FFI_OnChange` when the *document* becomes dirty, which for
    /// a form is a committed field edit. That is the signal §8.6 turns into one
    /// revision and one undo entry.
    changed: bool,
    /// Whether a text field currently has the caret.
    ///
    /// The application needs this to decide where a keystroke goes: into a
    /// field, or to the shortcut that letter is bound to. Getting it wrong
    /// means a talk where typing a name into a form turns the page.
    text_focus: bool,
}

impl FormEnvironment {
    /// Version 1 of the interface: the stable one. Version 2 additionally
    /// enables the experimental XFA interfaces, which pulpit does not
    /// implement and does not want called.
    const INTERFACE_VERSION: c_int = 1;

    /// Build the environment. It is boxed because PDFium keeps the address.
    pub fn new() -> Box<FormEnvironment> {
        // Zeroed, then filled: the struct has a long tail of optional
        // callbacks, and a null there is exactly "not implemented" — which is
        // the answer pulpit wants for every one of them. Writing them out as
        // explicit `None`s would be the same value and one more place to
        // forget something when PDFium adds a field.
        //
        // Safety: `FPDF_FORMFILLINFO` is a C struct of integers, pointers and
        // nullable function pointers. All-zero is a valid value for every one
        // of them, and is what PDFium's own samples start from.
        let mut environment = Box::new(FormEnvironment {
            info: unsafe { std::mem::zeroed() },
            dirty: Vec::new(),
            changed: false,
            text_focus: false,
        });

        environment.info.version = Self::INTERFACE_VERSION;

        // The ones PDFium documents as required. A null here is not "use a
        // default": it is a call through a null pointer the first time a form
        // needs one.
        environment.info.FFI_Invalidate = Some(invalidate);
        environment.info.FFI_SetCursor = Some(set_cursor);
        environment.info.FFI_SetTimer = Some(set_timer);
        environment.info.FFI_KillTimer = Some(kill_timer);
        // The one callback that cannot be assigned directly. Its return type is
        // a `struct` whose generated binding `pdfium-render` keeps private, so
        // the function is written against the mirror above and reinterpreted
        // here. The layout assertion below is what makes that safe rather than
        // hopeful.
        //
        // Safety: `SystemTime` and `FPDF_SYSTEMTIME` are both `#[repr(C)]`
        // structs of eight `unsigned short`s in the same order, so the two
        // function types differ only in the name of a type with identical size,
        // alignment and field layout — and therefore identical ABI.
        // The destination type cannot be written down here — it names a struct
        // in a private module of `pdfium-render` — so the target is inferred
        // from the field it is being assigned to, which is the one place that
        // knows it. That is precisely what the lint objects to and precisely
        // what is wanted.
        #[allow(
            clippy::missing_transmute_annotations,
            reason = "the target type is PDFium's own and is not nameable from here; \
                      the layout assertion below is what makes the cast sound"
        )]
        let get_local_time = unsafe { std::mem::transmute(get_local_time as LocalTimeCallback) };
        environment.info.FFI_GetLocalTime = Some(get_local_time);
        environment.info.FFI_GetPage = Some(get_page);
        environment.info.FFI_GetCurrentPage = Some(get_current_page);
        environment.info.FFI_GetRotation = Some(get_rotation);
        environment.info.FFI_ExecuteNamedAction = Some(execute_named_action);

        // The ones pulpit wants told about.
        environment.info.FFI_OnChange = Some(on_change);
        environment.info.FFI_SetTextFieldFocus = Some(set_text_field_focus);

        // Everything else stays null, and that is the security posture rather
        // than an omission (§8.6, A8):
        //
        // - `m_pJsPlatform` null means a document's JavaScript has no platform
        //   to run against — no alerts, no `app.launchURL`, no field scripts
        //   reaching the host.
        // - `FFI_DoURIAction`, `FFI_GotoURL`, `FFI_DoURIActionWithKeyboardModifier`
        //   null means a form cannot navigate anywhere.
        // - `FFI_EmailTo`, `FFI_UploadTo`, `FFI_PostRequestURL`,
        //   `FFI_PutRequestURL`, `FFI_DownloadFromURL` null means a form cannot
        //   send itself, or anything else, over a network.
        // - `FFI_OpenFile` null means a form cannot read from the filesystem.
        // - `FFI_PopupMenu` null means a document cannot raise UI of its own.
        //
        // A form is a thing you type values into. None of the above is needed
        // to type a value into one.
        environment.info.xfa_disabled = 1;

        environment
    }

    /// Hand this environment to PDFium and get the form handle back.
    ///
    /// # Safety
    ///
    /// The returned handle borrows this environment: PDFium keeps the pointer
    /// and calls through it for as long as the handle lives. The environment
    /// must not be moved or dropped until `FPDFDOC_ExitFormFillEnvironment` has
    /// been called on the handle.
    pub unsafe fn attach(
        self: &mut Box<FormEnvironment>,
        bindings: &dyn PdfiumLibraryBindings,
        document: FPDF_DOCUMENT,
    ) -> Option<FPDF_FORMHANDLE> {
        let info = &mut self.as_mut().info as *mut _;
        let handle = unsafe { bindings.FPDFDOC_InitFormFillEnvironment(document, info) };
        (!handle.is_null()).then_some(handle)
    }

    /// Take everything PDFium has asked to have redrawn, coalesced.
    ///
    /// Drained rather than read: a rectangle reported twice is a redraw done
    /// twice, and the second one draws what the first already drew.
    ///
    /// The result is at most [`limits::MAX_DIRTY_RECTS`] rectangles. Past that
    /// they collapse into one covering rectangle — a form that invalidates a
    /// thousand small rectangles in one keystroke is better served by one large
    /// redraw than by a thousand small ones, and the bound is what stops a
    /// hostile document turning a keystroke into unbounded work (A8).
    pub fn take_dirty(&mut self) -> Vec<DirtyRect> {
        let dirty = std::mem::take(&mut self.dirty);
        if dirty.len() <= limits::MAX_DIRTY_RECTS {
            return dirty;
        }
        match dirty.split_first() {
            Some((first, rest)) => vec![rest.iter().fold(*first, |all, one| all.union(*one))],
            None => Vec::new(),
        }
    }

    /// Whether a field's value changed since this was last asked, and reset.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Whether a text field currently holds the caret.
    pub fn has_text_focus(&self) -> bool {
        self.text_focus
    }
}

/// Recover the host state from the pointer PDFium hands back.
///
/// # Safety
///
/// `this` must be the pointer PDFium was given in
/// `FPDFDOC_InitFormFillEnvironment`, which is the address of a
/// `FormEnvironment`'s first field and therefore of the `FormEnvironment`.
unsafe fn environment<'a>(this: *mut FPDF_FORMFILLINFO) -> Option<&'a mut FormEnvironment> {
    if this.is_null() {
        return None;
    }
    Some(unsafe { &mut *(this as *mut FormEnvironment) })
}

unsafe extern "C" fn invalidate(
    this: *mut FPDF_FORMFILLINFO,
    _page: FPDF_PAGE,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
) {
    let Some(environment) = (unsafe { environment(this) }) else {
        return;
    };
    let rect = DirtyRect {
        left,
        top,
        right,
        bottom,
    };
    if rect.is_empty() {
        return;
    }
    // Bounded here as well as in `take_dirty`, so a form that invalidates in a
    // loop cannot grow this vector without limit before anyone looks at it.
    if environment.dirty.len() < limits::MAX_DIRTY_RECTS * 4 {
        environment.dirty.push(rect);
    }
}

unsafe extern "C" fn set_cursor(_this: *mut FPDF_FORMFILLINFO, _kind: c_int) {
    // The cursor over the page surface is the application's business and
    // follows the armed tool, not the field under the pointer. Accepting the
    // call and ignoring it is what the interface requires; a null here would
    // be a crash.
}

unsafe extern "C" fn set_timer(
    _this: *mut FPDF_FORMFILLINFO,
    _elapse: c_int,
    _callback: Option<unsafe extern "C" fn(c_int)>,
) -> c_int {
    // No timers. PDFium uses one to blink a caret, and a blinking caret is not
    // worth a wakeup on a worker process that would otherwise sleep — the
    // caret is drawn, it simply does not blink. Returning zero says the timer
    // was not installed, which is a case PDFium handles.
    0
}

unsafe extern "C" fn kill_timer(_this: *mut FPDF_FORMFILLINFO, _id: c_int) {
    // Nothing was installed, so there is nothing to kill.
}

unsafe extern "C" fn get_local_time(_this: *mut FPDF_FORMFILLINFO) -> SystemTime {
    // A fixed, obviously-not-a-real-time value rather than the wall clock.
    //
    // This is what a document's JavaScript would read to learn when the form
    // was filled, and a form does not need to know. It is also the closed-world
    // discipline the Typst compiler follows for the same reason: a build, or a
    // fill, that depends on the clock is one that cannot be reproduced.
    //
    // Safety: `FPDF_SYSTEMTIME` is a struct of integers; all-zero is valid.
    unsafe { std::mem::zeroed() }
}

unsafe extern "C" fn on_change(this: *mut FPDF_FORMFILLINFO) {
    if let Some(environment) = unsafe { environment(this) } {
        environment.changed = true;
    }
}

unsafe extern "C" fn get_page(
    _this: *mut FPDF_FORMFILLINFO,
    _document: FPDF_DOCUMENT,
    _index: c_int,
) -> FPDF_PAGE {
    // PDFium asks for a page it does not itself hold, which happens for
    // document-level actions and cross-page focus moves. pulpit does not keep
    // pages loaded across calls — a page handle that outlived the call that
    // made it is the second of this codebase's three rules — so the honest
    // answer is that there is no page here.
    std::ptr::null_mut()
}

unsafe extern "C" fn get_current_page(
    _this: *mut FPDF_FORMFILLINFO,
    _document: FPDF_DOCUMENT,
) -> FPDF_PAGE {
    std::ptr::null_mut()
}

unsafe extern "C" fn get_rotation(_this: *mut FPDF_FORMFILLINFO, _page: FPDF_PAGE) -> c_int {
    // Quarter turns clockwise. Zero, because every event pulpit forwards is
    // already in the page's own space: the canonical-space conversion happened
    // before the event left the application (A4), so telling PDFium the view
    // is also rotated would rotate it twice.
    0
}

unsafe extern "C" fn execute_named_action(_this: *mut FPDF_FORMFILLINFO, _name: *const c_char) {
    // `NextPage`, `Print`, `SaveAs` and friends, from a document that wants to
    // drive the viewer. Accepted and ignored: a document does not get to turn
    // pulpit's pages, open its printer or save its file (A8).
}

unsafe extern "C" fn set_text_field_focus(
    this: *mut FPDF_FORMFILLINFO,
    _value: FPDF_WIDESTRING,
    _value_length: c_ulong,
    is_focus: FPDF_BOOL,
) {
    if let Some(environment) = unsafe { environment(this) } {
        environment.text_focus = is_focus != 0;
    }
}

/// The type PDFium declares for `FFI_GetLocalTime`, with the return struct
/// replaced by the mirror above.
type LocalTimeCallback = unsafe extern "C" fn(*mut FPDF_FORMFILLINFO) -> SystemTime;

// Eight `unsigned short`s and nothing else. If PDFium ever changed the shape of
// `FPDF_SYSTEMTIME`, this is what would catch it — at compile time, rather than
// as a callback writing the wrong bytes into a C stack frame.
const _: () = {
    assert!(std::mem::size_of::<SystemTime>() == 8 * std::mem::size_of::<c_ushort>());
    assert!(std::mem::align_of::<SystemTime>() == std::mem::align_of::<c_ushort>());
};

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f64, top: f64, right: f64, bottom: f64) -> DirtyRect {
        DirtyRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn the_environment_puts_its_callback_struct_first() {
        // The whole design rests on this: PDFium hands back a pointer to the
        // `FPDF_FORMFILLINFO`, and every callback casts it to a
        // `FormEnvironment`. If a field were ever added above it, every
        // callback would read the wrong memory — silently, and only when a
        // form was touched.
        let environment = FormEnvironment::new();
        let whole = &*environment as *const FormEnvironment as usize;
        let info = &environment.info as *const _ as usize;
        assert_eq!(whole, info, "the callback struct is no longer first");
    }

    #[test]
    fn the_environment_refuses_everything_a_document_could_reach_out_with() {
        // A8, as an assertion rather than a comment. Each of these being null
        // is what stops a form from running scripts, opening URLs, reading
        // files or talking to a network.
        let environment = FormEnvironment::new();
        let info = &environment.info;
        assert!(info.m_pJsPlatform.is_null(), "a document has a JS platform");
        assert!(info.FFI_DoURIAction.is_none());
        assert!(info.FFI_DoURIActionWithKeyboardModifier.is_none());
        assert!(info.FFI_GotoURL.is_none());
        assert!(info.FFI_EmailTo.is_none());
        assert!(info.FFI_UploadTo.is_none());
        assert!(info.FFI_PostRequestURL.is_none());
        assert!(info.FFI_PutRequestURL.is_none());
        assert!(info.FFI_DownloadFromURL.is_none());
        assert!(info.FFI_OpenFile.is_none());
        assert!(info.FFI_PopupMenu.is_none());
        assert_eq!(info.xfa_disabled, 1);
    }

    #[test]
    fn the_environment_implements_everything_pdfium_requires() {
        // The other half: a null in one of these is a call through a null
        // pointer the first time a form needs it, which is a crash rather than
        // a missing feature.
        let environment = FormEnvironment::new();
        let info = &environment.info;
        assert_eq!(info.version, 1);
        assert!(info.FFI_Invalidate.is_some());
        assert!(info.FFI_SetCursor.is_some());
        assert!(info.FFI_SetTimer.is_some());
        assert!(info.FFI_KillTimer.is_some());
        assert!(info.FFI_GetLocalTime.is_some());
        assert!(info.FFI_GetPage.is_some());
        assert!(info.FFI_GetCurrentPage.is_some());
        assert!(info.FFI_GetRotation.is_some());
        assert!(info.FFI_ExecuteNamedAction.is_some());
    }

    /// Call a callback the way PDFium would, through the info pointer.
    fn as_pdfium_would(environment: &mut Box<FormEnvironment>) -> *mut FPDF_FORMFILLINFO {
        &mut environment.as_mut().info as *mut _
    }

    #[test]
    fn an_invalidation_arrives_where_the_host_can_read_it() {
        let mut environment = FormEnvironment::new();
        let this = as_pdfium_would(&mut environment);
        unsafe {
            invalidate(this, std::ptr::null_mut(), 10.0, 100.0, 60.0, 80.0);
            invalidate(this, std::ptr::null_mut(), 20.0, 90.0, 70.0, 70.0);
        }
        assert_eq!(
            environment.take_dirty(),
            vec![rect(10.0, 100.0, 60.0, 80.0), rect(20.0, 90.0, 70.0, 70.0)]
        );
        assert!(
            environment.take_dirty().is_empty(),
            "a rectangle reported twice is a redraw done twice"
        );
    }

    #[test]
    fn an_invalidation_with_no_area_is_not_a_redraw() {
        let mut environment = FormEnvironment::new();
        let this = as_pdfium_would(&mut environment);
        unsafe {
            // Degenerate in one axis, then the other. Neither is an area.
            invalidate(this, std::ptr::null_mut(), 10.0, 100.0, 10.0, 20.0);
            invalidate(this, std::ptr::null_mut(), 10.0, 10.0, 90.0, 10.0);
        }
        assert!(environment.take_dirty().is_empty());
    }

    #[test]
    fn an_upside_down_invalidation_is_still_a_redraw() {
        // PDFium hands `top` and `bottom` over in whichever order the widget's
        // rectangle was written in the file, and a rectangle rejected for its
        // winding is a field that does not repaint while it is typed into.
        // This is the test for a bug that had exactly that symptom.
        let mut environment = FormEnvironment::new();
        let this = as_pdfium_would(&mut environment);
        unsafe { invalidate(this, std::ptr::null_mut(), 60.0, 80.0, 10.0, 100.0) };
        assert_eq!(environment.take_dirty().len(), 1);
    }

    #[test]
    fn a_storm_of_invalidations_collapses_into_one_redraw() {
        // A8: a document that invalidates in a loop must not turn one
        // keystroke into unbounded drawing.
        let mut environment = FormEnvironment::new();
        let this = as_pdfium_would(&mut environment);
        for step in 0..(limits::MAX_DIRTY_RECTS * 8) {
            let at = step as f64;
            unsafe { invalidate(this, std::ptr::null_mut(), at, at + 2.0, at + 1.0, at) };
        }
        let dirty = environment.take_dirty();
        assert_eq!(dirty.len(), 1, "the storm did not collapse");
        assert!(!dirty[0].is_empty());
    }

    #[test]
    fn a_committed_change_is_reported_once() {
        // §8.6: one committed field change is one revision and one undo entry,
        // so the signal has to be edge-triggered rather than level.
        let mut environment = FormEnvironment::new();
        let this = as_pdfium_would(&mut environment);
        assert!(!environment.take_changed());
        unsafe { on_change(this) };
        assert!(environment.take_changed());
        assert!(!environment.take_changed(), "one change, one revision");
    }

    #[test]
    fn text_focus_follows_the_caret_in_and_out_of_a_field() {
        // What decides whether a keystroke is a character or a shortcut. A
        // presenter typing a name into a form must not turn the page.
        let mut environment = FormEnvironment::new();
        let this = as_pdfium_would(&mut environment);
        assert!(!environment.has_text_focus());
        unsafe { set_text_field_focus(this, std::ptr::null(), 0, 1) };
        assert!(environment.has_text_focus());
        unsafe { set_text_field_focus(this, std::ptr::null(), 0, 0) };
        assert!(!environment.has_text_focus());
    }

    #[test]
    fn a_callback_with_no_environment_behind_it_does_nothing() {
        // Defensive, because these are `extern "C"` and the caller is not Rust.
        unsafe {
            invalidate(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0.0,
                1.0,
                1.0,
                0.0,
            );
            on_change(std::ptr::null_mut());
            set_text_field_focus(std::ptr::null_mut(), std::ptr::null(), 0, 1);
        }
    }

    #[test]
    fn rectangles_union_into_one_that_covers_both() {
        let left = rect(10.0, 100.0, 40.0, 80.0);
        let right = rect(30.0, 120.0, 70.0, 60.0);
        assert_eq!(left.union(right), rect(10.0, 120.0, 70.0, 60.0));
        assert_eq!(
            left.union(left),
            left,
            "a union with itself changes nothing"
        );
    }

    #[test]
    fn the_clock_a_document_can_read_is_not_the_wall_clock() {
        // A form's JavaScript would read this to learn when it was filled.
        let time = unsafe { get_local_time(std::ptr::null_mut()) };
        assert_eq!(time, SystemTime::default());
    }

    #[test]
    fn no_timer_is_installed_and_killing_one_is_harmless() {
        assert_eq!(
            unsafe { set_timer(std::ptr::null_mut(), 500, None) },
            0,
            "a timer was installed on a worker that should be asleep"
        );
        unsafe { kill_timer(std::ptr::null_mut(), 7) };
    }
}
