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
//! # JavaScript, and where it stops
//!
//! A form's own scripts run. That is what the field formatting, keystroke
//! validation and calculated fields of a real-world form are made of, and
//! without them a document that says "24" in a total field is pulpit failing to
//! do arithmetic the file already knows how to do. Two things are needed and
//! neither works alone: the pinned library must be a `-v8` build (see
//! `scripts/fetch-pdfium.sh`), and `m_pJsPlatform` must be non-null, because
//! PDFium's own header says a null one means "JavaScript will be prevented from
//! executing".
//!
//! What does *not* run is anything that leaves the process. A PDF can ask to
//! navigate to a URL, email itself, upload itself, print itself or download
//! from a URL. Every one of those callbacks is either null — `FFI_DoURIAction`,
//! `FFI_EmailTo`, `FFI_UploadTo`, `FFI_PostRequestURL`, `FFI_PutRequestURL`,
//! `FFI_DownloadFromURL`, `FFI_OpenFile`, `FFI_PopupMenu` — or implemented here
//! to record a [`crate::document::protocol::HostRequest`] and return the answer
//! a dismissed dialog gives. The script runs to completion; the application,
//! which is the layer with a user in front of it, decides what to honour (A8).
//!
//! # Two constraints the V8 build imposes
//!
//! **The interface version is not a choice.** `FPDFDOC_InitFormFillEnvironment`
//! returns null on a version mismatch, and the published `-v8` builds are also
//! XFA builds, which demand exactly version 2. Setting 1 there makes every
//! document report no fillable form — silently, because a null handle is
//! indistinguishable from a document with no form. XFA itself stays off through
//! `xfa_disabled`.
//!
//! **The isolate is thread-bound.** PDFium creates its V8 isolate lazily, on
//! whichever thread first runs a script, and using it from a second thread is a
//! segfault inside V8 rather than an error. The document worker's `serve` loop
//! is a synchronous read-handle-write on one thread, which satisfies this; any
//! future attempt to hand PDFium work to a pool would not.
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

use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_ushort, c_void};

use crate::document::limits;
use crate::document::protocol::HostRequest;
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

/// PDFium's `IPDF_JSPLATFORM`, mirrored.
///
/// Mirrored for the same reason [`SystemTime`] is: `pdfium-render` keeps the
/// generated binding behind a private module, so the struct that has to be
/// filled in cannot be named from here. The field order is `fpdf_formfill.h`'s
/// and is load-bearing — this is a C vtable in all but name.
#[repr(C)]
struct JsPlatform {
    version: c_int,
    app_alert: Option<
        unsafe extern "C" fn(
            *mut JsPlatform,
            FPDF_WIDESTRING,
            FPDF_WIDESTRING,
            c_int,
            c_int,
        ) -> c_int,
    >,
    app_beep: Option<unsafe extern "C" fn(*mut JsPlatform, c_int)>,
    #[allow(clippy::type_complexity, reason = "the shape is PDFium's, not ours")]
    app_response: Option<
        unsafe extern "C" fn(
            *mut JsPlatform,
            FPDF_WIDESTRING,
            FPDF_WIDESTRING,
            FPDF_WIDESTRING,
            FPDF_WIDESTRING,
            FPDF_BOOL,
            *mut c_void,
            c_int,
        ) -> c_int,
    >,
    doc_get_file_path: Option<unsafe extern "C" fn(*mut JsPlatform, *mut c_void, c_int) -> c_int>,
    #[allow(clippy::type_complexity, reason = "the shape is PDFium's, not ours")]
    doc_mail: Option<
        unsafe extern "C" fn(
            *mut JsPlatform,
            *mut c_void,
            c_int,
            FPDF_BOOL,
            FPDF_WIDESTRING,
            FPDF_WIDESTRING,
            FPDF_WIDESTRING,
            FPDF_WIDESTRING,
            FPDF_WIDESTRING,
        ),
    >,
    #[allow(clippy::type_complexity, reason = "the shape is PDFium's, not ours")]
    doc_print: Option<
        unsafe extern "C" fn(
            *mut JsPlatform,
            FPDF_BOOL,
            c_int,
            c_int,
            FPDF_BOOL,
            FPDF_BOOL,
            FPDF_BOOL,
            FPDF_BOOL,
            FPDF_BOOL,
        ),
    >,
    doc_submit_form:
        Option<unsafe extern "C" fn(*mut JsPlatform, *mut c_void, c_int, FPDF_WIDESTRING)>,
    doc_goto_page: Option<unsafe extern "C" fn(*mut JsPlatform, c_int)>,
    field_browse: Option<unsafe extern "C" fn(*mut JsPlatform, *mut c_void, c_int) -> c_int>,
    /// PDFium never reads this; it is the embedder's to use, and pulpit uses it
    /// the way the header suggests — as the way back to the form-fill info, and
    /// therefore to the [`FormEnvironment`] it begins.
    form_fill_info: *mut c_void,
    /// Unused in v3, retained for layout.
    isolate: *mut c_void,
    /// Unused in v3, retained for layout.
    v8_embedder_slot: c_uint,
}

/// The host side of PDFium's form-fill environment.
///
/// `info` MUST stay the first field: PDFium hands each callback the address it
/// was given, and every callback here casts it straight back to this type.
#[repr(C)]
pub struct FormEnvironment {
    info: FPDF_FORMFILLINFO,
    /// The JS platform PDFium reaches the host through. Owned here so it lives
    /// exactly as long as the environment that points at it.
    js: JsPlatform,
    /// What a document's JavaScript has asked the host to do, bounded by
    /// [`limits::MAX_HOST_REQUESTS`].
    requests: Vec<HostRequest>,
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
    /// The page the form interaction is open on, for `FFI_GetCurrentPage` and
    /// `FFI_GetPage`.
    ///
    /// This is the one page handle the engine deliberately keeps loaded (see
    /// `FormBinding::open_page` for why), lent to the environment for exactly
    /// as long as it is open. A calculation script that reads `this.pageNum`,
    /// or an `AFSimple_Calculate` resolving its own field, asks through these
    /// callbacks — and a null answer while an interaction is live is what made
    /// cross-page arithmetic silently stop recalculating.
    current_page: FPDF_PAGE,
    /// Which page index [`Self::current_page`] is, so `FFI_GetPage` can answer
    /// for that page and refuse every other.
    current_page_index: c_int,
}

impl FormEnvironment {
    /// Version 2 of the interface, which the pinned library requires.
    ///
    /// This is not a free choice. `FPDFDOC_InitFormFillEnvironment` checks the
    /// version against how the library was built and returns null on a
    /// mismatch: a plain build demands exactly 1, and an XFA-enabled build
    /// demands exactly 2. The pinned artifact is bblanchon's `-v8` build, whose
    /// `args.gn` carries `pdf_enable_v8 = true` *and* `pdf_enable_xfa = true` —
    /// there is no V8-without-XFA build published — so 1 here means every
    /// document silently reports no fillable form at all.
    ///
    /// Version 2 does not turn XFA on. It makes the XFA fields of the struct
    /// *readable*, and the first of them is `xfa_disabled`, which is set below.
    /// The XFA-only callbacks stay null and are never reached.
    const INTERFACE_VERSION: c_int = 2;

    /// `IPDF_JSPLATFORM`'s own version, which `fpdf_formfill.h` documents as
    /// "currently must be 2". It is independent of [`Self::INTERFACE_VERSION`]:
    /// `m_pJsPlatform` is a version-1 field of `FPDF_FORMFILLINFO`, so running
    /// a form's JavaScript needs no XFA interfaces and does not enable any.
    const JS_PLATFORM_VERSION: c_int = 2;

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
            // Safety: `JsPlatform` is a struct of integers, pointers and
            // nullable function pointers; all-zero is valid for every field.
            js: unsafe { std::mem::zeroed() },
            requests: Vec::new(),
            dirty: Vec::new(),
            changed: false,
            text_focus: false,
            current_page: std::ptr::null_mut(),
            current_page_index: -1,
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

        // The JavaScript platform. Installing it is what lets a form's own
        // scripts run at all: PDFium documents `m_pJsPlatform` as "if NULL,
        // then JavaScript will be prevented from executing", and with it null
        // every field format, keystroke validation and calculated field in
        // every form silently does nothing (§8.6).
        //
        // Every callback is implemented, because a null one is a call through a
        // null pointer rather than a refusal. What each of them *does* is
        // record a [`HostRequest`] and return the answer a dismissed dialog
        // would have produced. The document's script therefore runs to
        // completion, and nothing it asked for happens without the application
        // — which is the layer with a user in front of it — deciding to do it.
        environment.js.version = Self::JS_PLATFORM_VERSION;
        environment.js.app_alert = Some(app_alert);
        environment.js.app_beep = Some(app_beep);
        environment.js.app_response = Some(app_response);
        environment.js.doc_get_file_path = Some(doc_get_file_path);
        environment.js.doc_mail = Some(doc_mail);
        environment.js.doc_print = Some(doc_print);
        environment.js.doc_submit_form = Some(doc_submit_form);
        environment.js.doc_goto_page = Some(doc_goto_page);
        environment.js.field_browse = Some(field_browse);
        // The way back from a JS callback to this environment. PDFium never
        // reads it; the header names `FPDF_FORMFILLINFO` as the traditional
        // value, and that is the address of this environment's first field and
        // therefore of the environment.
        environment.js.form_fill_info = (&mut environment.info as *mut FPDF_FORMFILLINFO).cast();
        // The target type names a struct in a private module of
        // `pdfium-render`, so it is inferred from the field being assigned —
        // the one place that knows it.
        environment.info.m_pJsPlatform = (&mut environment.js as *mut JsPlatform).cast();

        // Everything else stays null, and that is the security posture rather
        // than an omission (§8.6, A8). JavaScript now runs, but the paths that
        // leave the process do not exist for it to run down:
        //
        // - `FFI_DoURIAction`, `FFI_GotoURL`, `FFI_DoURIActionWithKeyboardModifier`
        //   null means a form cannot navigate anywhere.
        // - `FFI_EmailTo`, `FFI_UploadTo`, `FFI_PostRequestURL`,
        //   `FFI_PutRequestURL`, `FFI_DownloadFromURL` null means a form cannot
        //   send itself, or anything else, over a network.
        // - `FFI_OpenFile` null means a form cannot read from the filesystem.
        // - `FFI_PopupMenu` null means a document cannot raise UI of its own.
        //
        // A script that wants any of those gets a recorded request and a
        // refusal, not a socket.
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

    /// Take what a document's JavaScript asked the host to do, and reset.
    ///
    /// Drained rather than read, for the same reason [`Self::take_dirty`] is: a
    /// request answered twice is a dialog shown twice for one `app.alert`.
    pub fn take_requests(&mut self) -> Vec<HostRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Lend the environment the page a form interaction is open on, so
    /// `FFI_GetCurrentPage` and `FFI_GetPage` can answer while a script runs.
    ///
    /// The handle is borrowed, not owned: the caller that keeps the page
    /// loaded MUST call [`Self::clear_current_page`] before closing it, or the
    /// next script would be handed a pointer to a freed page.
    pub fn set_current_page(&mut self, index: usize, page: FPDF_PAGE) {
        self.current_page = page;
        self.current_page_index = index.min(c_int::MAX as usize) as c_int;
    }

    /// The interaction is over; scripts get the null answer again.
    pub fn clear_current_page(&mut self) {
        self.current_page = std::ptr::null_mut();
        self.current_page_index = -1;
    }

    /// Record one request, if there is room for it.
    fn request(&mut self, request: HostRequest) {
        if self.requests.len() < limits::MAX_HOST_REQUESTS {
            self.requests.push(request);
        } else {
            tracing::debug!(
                "a document's JavaScript made more than {} host requests in one event; \
                 the rest are dropped",
                limits::MAX_HOST_REQUESTS
            );
        }
    }
}

/// Recover the host state from the pointer PDFium hands back.
///
/// # Safety
///
/// `this` must be the pointer PDFium was given in
/// `FPDFDOC_InitFormFillEnvironment`, which is the address of a
/// `FormEnvironment`'s first field and therefore of the `FormEnvironment`.
///
/// The returned mutable reference must not be alive across a re-entry into
/// PDFium. No second call to this function may have a lifetime that overlaps
/// with the first: the returned reference assumes exclusive access to the
/// environment, and PDFium is allowed to re-enter through a callback, which
/// would create aliasing violations.
unsafe fn environment<'a>(this: *mut FPDF_FORMFILLINFO) -> Option<&'a mut FormEnvironment> {
    if this.is_null() {
        return None;
    }
    Some(unsafe { &mut *(this as *mut FormEnvironment) })
}

unsafe extern "C" fn invalidate(
    this: *mut FPDF_FORMFILLINFO,
    page: FPDF_PAGE,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
) {
    let Some(environment) = (unsafe { environment(this) }) else {
        return;
    };
    // Only the page the interaction is open on. The dirty list is read back
    // as rectangles *of that page*, so a rectangle a calculation script
    // invalidates on another page — reachable now that `FFI_GetCurrentPage`
    // answers — must not be patched onto this one. The other page's repaint
    // arrives with the snapshot the commit triggers, which re-renders it
    // whole.
    if !page.is_null() && !environment.current_page.is_null() && page != environment.current_page {
        return;
    }
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
    this: *mut FPDF_FORMFILLINFO,
    _document: FPDF_DOCUMENT,
    index: c_int,
) -> FPDF_PAGE {
    // PDFium asks for a page it does not itself hold, which happens for
    // document-level actions, cross-page focus moves and calculation scripts.
    // pulpit does not keep pages loaded across calls — a page handle that
    // outlived the call that made it is the second of this codebase's three
    // rules — with one stated exception: the page a form interaction is open
    // on (see `FormBinding::open_page`). That page is already loaded and
    // already lent to PDFium, so answering with it hands over nothing new;
    // every other page is honestly not here.
    let Some(environment) = (unsafe { environment(this) }) else {
        return std::ptr::null_mut();
    };
    if index >= 0 && index == environment.current_page_index {
        return environment.current_page;
    }
    std::ptr::null_mut()
}

unsafe extern "C" fn get_current_page(
    this: *mut FPDF_FORMFILLINFO,
    _document: FPDF_DOCUMENT,
) -> FPDF_PAGE {
    // The page the interaction is open on, while one is. A calculation or
    // format script that asks `this.pageNum`, or resolves a field through the
    // current page, gets the page the person is actually typing on — a null
    // here is why a subtotal driven from another page used to stop
    // recalculating until its own page was next opened.
    match unsafe { environment(this) } {
        Some(environment) => environment.current_page,
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn get_rotation(_this: *mut FPDF_FORMFILLINFO, _page: FPDF_PAGE) -> c_int {
    // Quarter turns clockwise. Zero, because every event pulpit forwards is
    // already in the page's own space: the canonical-space conversion happened
    // before the event left the application (A4), so telling PDFium the view
    // is also rotated would rotate it twice.
    0
}

unsafe extern "C" fn execute_named_action(this: *mut FPDF_FORMFILLINFO, name: *const c_char) {
    // `NextPage`, `Print`, `SaveAs` and friends, from a document that wants to
    // drive the viewer. Recorded, never performed here: a document does not get
    // to turn pulpit's pages, open its printer or save its file on its own say
    // so, but the application may choose to honour a navigation (A8).
    let Some(environment) = (unsafe { environment(this) }) else {
        return;
    };
    if name.is_null() {
        return;
    }
    // Bounded, and not assumed to be UTF-8: this is a byte string out of a
    // hostile document.
    let mut bytes = Vec::new();
    for offset in 0..limits::MAX_JS_STRING_UNITS {
        let byte = unsafe { *name.add(offset) }.to_ne_bytes()[0];
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    environment.request(HostRequest::NamedAction {
        name: String::from_utf8_lossy(&bytes).into_owned(),
    });
}

/// Read one of PDFium's null-terminated UTF-16 strings, bounded.
///
/// `FPDF_WIDESTRING` carries no length, so [`limits::MAX_JS_STRING_UNITS`] is
/// both a size bound and the thing that stops a missing terminator in a hostile
/// document from being read past the end of its allocation (A8).
unsafe fn widestring(text: FPDF_WIDESTRING) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut units = Vec::new();
    for offset in 0..limits::MAX_JS_STRING_UNITS {
        let unit = unsafe { *text.add(offset) };
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}

/// Recover the host state from the pointer PDFium hands a JS callback.
///
/// # Safety
///
/// `this` must be the `JsPlatform` pulpit installed, whose `form_fill_info` is
/// the address of the owning [`FormEnvironment`].
unsafe fn js_environment<'a>(this: *mut JsPlatform) -> Option<&'a mut FormEnvironment> {
    if this.is_null() {
        return None;
    }
    let info = unsafe { (*this).form_fill_info };
    unsafe { environment(info.cast()) }
}

unsafe extern "C" fn app_alert(
    this: *mut JsPlatform,
    message: FPDF_WIDESTRING,
    title: FPDF_WIDESTRING,
    _buttons: c_int,
    _icon: c_int,
) -> c_int {
    if let Some(environment) = unsafe { js_environment(this) } {
        let message = unsafe { widestring(message) };
        let title = unsafe { widestring(title) };
        environment.request(HostRequest::Alert { message, title });
    }
    // `JSPLATFORM_ALERT_RETURN_OK`. The script continues as though the dialog
    // it never saw was acknowledged, which is what a viewer whose user pressed
    // OK would report.
    0
}

unsafe extern "C" fn app_beep(this: *mut JsPlatform, _kind: c_int) {
    if let Some(environment) = unsafe { js_environment(this) } {
        environment.request(HostRequest::Beep);
    }
}

#[allow(clippy::too_many_arguments, reason = "the signature is PDFium's")]
unsafe extern "C" fn app_response(
    this: *mut JsPlatform,
    question: FPDF_WIDESTRING,
    title: FPDF_WIDESTRING,
    _default: FPDF_WIDESTRING,
    _label: FPDF_WIDESTRING,
    _password: FPDF_BOOL,
    _response: *mut c_void,
    _length: c_int,
) -> c_int {
    if let Some(environment) = unsafe { js_environment(this) } {
        let question = unsafe { widestring(question) };
        let title = unsafe { widestring(title) };
        environment.request(HostRequest::Response { question, title });
    }
    // No bytes written into the response buffer, and zero of them: an empty
    // answer, which is what a cancelled prompt gives. The buffer is left
    // untouched rather than zeroed, because PDFium owns it and the documented
    // contract is that a short answer writes only what it has.
    0
}

unsafe extern "C" fn doc_get_file_path(
    this: *mut JsPlatform,
    _path: *mut c_void,
    _length: c_int,
) -> c_int {
    if let Some(environment) = unsafe { js_environment(this) } {
        environment.request(HostRequest::FilePath);
    }
    // Zero bytes, which is a document with no path as far as its script is
    // concerned. Answering honestly would put the user's home directory inside
    // a string a hostile form could then submit somewhere.
    0
}

#[allow(clippy::too_many_arguments, reason = "the signature is PDFium's")]
unsafe extern "C" fn doc_mail(
    this: *mut JsPlatform,
    _data: *mut c_void,
    _length: c_int,
    _ui: FPDF_BOOL,
    to: FPDF_WIDESTRING,
    subject: FPDF_WIDESTRING,
    _cc: FPDF_WIDESTRING,
    _bcc: FPDF_WIDESTRING,
    _message: FPDF_WIDESTRING,
) {
    if let Some(environment) = unsafe { js_environment(this) } {
        let to = unsafe { widestring(to) };
        let subject = unsafe { widestring(subject) };
        environment.request(HostRequest::Mail { to, subject });
    }
}

#[allow(clippy::too_many_arguments, reason = "the signature is PDFium's")]
unsafe extern "C" fn doc_print(
    this: *mut JsPlatform,
    _ui: FPDF_BOOL,
    _start: c_int,
    _end: c_int,
    _silent: FPDF_BOOL,
    _shrink_to_fit: FPDF_BOOL,
    _print_as_image: FPDF_BOOL,
    _reverse: FPDF_BOOL,
    _annotations: FPDF_BOOL,
) {
    if let Some(environment) = unsafe { js_environment(this) } {
        environment.request(HostRequest::Print);
    }
}

unsafe extern "C" fn doc_submit_form(
    this: *mut JsPlatform,
    _data: *mut c_void,
    length: c_int,
    url: FPDF_WIDESTRING,
) {
    if let Some(environment) = unsafe { js_environment(this) } {
        let url = unsafe { widestring(url) };
        environment.request(HostRequest::SubmitForm {
            url,
            bytes: length.max(0) as usize,
        });
    }
    // The data is deliberately not retained. A submission is a decision the
    // application makes with a user present, and it can re-serialise the form
    // itself if the answer is yes; keeping a copy of every field value in this
    // queue would put it one logging mistake away from being written somewhere.
}

unsafe extern "C" fn doc_goto_page(this: *mut JsPlatform, page: c_int) {
    if let Some(environment) = unsafe { js_environment(this) } {
        environment.request(HostRequest::GotoPage {
            page: page.max(0) as usize,
        });
    }
}

unsafe extern "C" fn field_browse(
    this: *mut JsPlatform,
    _path: *mut c_void,
    _length: c_int,
) -> c_int {
    if let Some(environment) = unsafe { js_environment(this) } {
        environment.request(HostRequest::Browse);
    }
    // No path, which is a cancelled file picker. `FFI_OpenFile` is null for the
    // same reason: a form does not get to read the filesystem.
    0
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
        // is what stops a form from opening URLs, reading files or talking to
        // a network — the paths that leave the process, which stay shut even
        // though scripts now run.
        let environment = FormEnvironment::new();
        let info = &environment.info;
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
        // 2, not 1: the pinned `-v8` build is an XFA build, and
        // `FPDFDOC_InitFormFillEnvironment` answers a version-1 struct with a
        // null handle — which reads downstream as "this document has no form".
        assert_eq!(info.version, 2);
        // Installed, and required to be: PDFium's header says a null one
        // prevents JavaScript from executing at all, which would silently turn
        // off every field format and calculation in every form.
        assert!(
            !info.m_pJsPlatform.is_null(),
            "a form's own scripts have no platform to run against"
        );
        assert_eq!(environment.js.version, FormEnvironment::JS_PLATFORM_VERSION);
        assert!(environment.js.app_alert.is_some());
        assert!(environment.js.app_beep.is_some());
        assert!(environment.js.app_response.is_some());
        assert!(environment.js.doc_get_file_path.is_some());
        assert!(environment.js.doc_mail.is_some());
        assert!(environment.js.doc_print.is_some());
        assert!(environment.js.doc_submit_form.is_some());
        assert!(environment.js.doc_goto_page.is_some());
        assert!(environment.js.field_browse.is_some());
        assert_eq!(
            environment.js.form_fill_info as usize, info as *const _ as usize,
            "a JS callback finds its environment through this and nothing else"
        );
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
