//! The slice of djvulibre's `ddjvuapi` that pulpit uses, bound at run time.
//!
//! `SPEC-reader-formats.md` Â§55.3 is the rule this file exists to obey:
//! **no Class B library is ever bundled.** djvulibre is discovered through the
//! system loader, never shipped beside the binary and never linked in, so a
//! build carrying this module still runs on a machine with no DjVu library at
//! all â it reports the format unsupported and names what is missing (Â§61.1).
//!
//! That is also why this is hand-written `dlopen` FFI rather than a published
//! `-sys` crate: those link at build time, which would make DjVu a capability
//! of the *build*. Â§55.4 requires it to be a capability of the *machine*.
//!
//! Several `ddjvuapi` entry points are C macros over other functions rather
//! than symbols of their own. Those are reproduced here as Rust methods â
//! [`Api::document_release`] and friends â because `dlsym` cannot find a
//! macro, and a binding that looked for one would fail at load with a
//! confusing "symbol not found" for a call that was never a symbol.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CStr, CString};
use std::path::{Path, PathBuf};

/// Opaque `ddjvu_context_t`.
#[repr(C)]
pub struct Context {
    _opaque: [u8; 0],
}

/// Opaque `ddjvu_document_t`.
#[repr(C)]
pub struct Document {
    _opaque: [u8; 0],
}

/// Opaque `ddjvu_page_t`.
#[repr(C)]
pub struct Page {
    _opaque: [u8; 0],
}

/// Opaque `ddjvu_job_t`. Documents and pages *are* jobs; the C header casts
/// between them through `ddjvu_document_job` and `ddjvu_page_job`, which are
/// real exported functions rather than casts, so they are bound below.
#[repr(C)]
pub struct Job {
    _opaque: [u8; 0],
}

/// Opaque `ddjvu_format_t`.
#[repr(C)]
pub struct Format {
    _opaque: [u8; 0],
}

/// `ddjvu_status_t`. Ordered: anything `>= OK` is finished, anything
/// `>= FAILED` finished badly. The C header's `ddjvu_job_done` and
/// `ddjvu_job_error` macros are exactly those two comparisons.
pub const STATUS_OK: c_int = 2;
pub const STATUS_FAILED: c_int = 3;

/// `ddjvu_message_tag_t::DDJVU_ERROR`, the first variant.
pub const MESSAGE_ERROR: c_int = 0;

/// `ddjvu_render_mode_t::DDJVU_RENDER_COLOR`.
///
/// "Colour page or stencil": a photographic page renders in colour and a
/// bitonal scan renders as its stencil, which is the whole point â a scanned
/// book and a colour comic are the same call.
pub const RENDER_COLOR: c_int = 0;

/// `ddjvu_format_style_t::DDJVU_FORMAT_RGB24`.
///
/// The alternative, `RGBMASK32`, can write RGBA straight into the caller's
/// buffer and would save the expansion pass in
/// [`super::backend::DjvuBackend::render_into`]. It is not used because its
/// three masks address a native `unsigned int`, so the byte order they
/// produce follows the host's endianness, and the alpha channel would have to
/// come from the optional fourth "xor" argument. Trading a byte-order
/// argument that cannot be exercised in CI against one linear pass over a
/// frame is not a trade worth making; Â§62 says measure first.
pub const FORMAT_RGB24: c_int = 1;

/// `ddjvu_rect_t`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: c_int,
    pub y: c_int,
    pub w: c_uint,
    pub h: c_uint,
}

/// `ddjvu_pageinfo_t`, as of `DDJVUAPI_VERSION` 18 (djvulibre 3.5.22).
///
/// The size is passed explicitly to `ddjvu_document_get_pageinfo_imp`, which
/// is what the C macro does, so a library older than the struct fills only
/// the prefix it knows and leaves the rest at the zero this is created with.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PageInfo {
    pub width: c_int,
    pub height: c_int,
    pub dpi: c_int,
    /// `ddjvu_page_rotation_t`: 0, 1, 2, 3 for 0Â°, 90Â°, 180Â°, 270Â°
    /// counter-clockwise.
    pub rotation: c_int,
    pub version: c_int,
}

/// `ddjvu_message_any_t`, the common prefix of every message in the union.
#[repr(C)]
pub struct MessageAny {
    pub tag: c_int,
    pub context: *mut Context,
    pub document: *mut Document,
    pub page: *mut Page,
    pub job: *mut Job,
}

/// `ddjvu_message_error_t`.
#[repr(C)]
pub struct MessageError {
    pub any: MessageAny,
    pub message: *const c_char,
    pub function: *const c_char,
    pub filename: *const c_char,
    pub lineno: c_int,
}

/// `miniexp_t`, the tagged pointer djvulibre's s-expressions are made of.
///
/// It is a pointer *or* an immediate value depending on its low two bits,
/// which is why it is never dereferenced except through [`car`] and [`cdr`]
/// below.
pub type MiniExp = *const c_void;

/// `miniexp_nil`, the empty list — and the answer for a page with no text.
pub const MINIEXP_NIL: MiniExp = std::ptr::null();

/// `miniexp_dummy`, "not available yet": `ddjvu_document_get_pagetext`
/// returns it while it fetches the page, and the caller pumps and asks again.
pub const MINIEXP_DUMMY: MiniExp = 2 as MiniExp;

// The accessors below are `static inline` in `miniexp.h` rather than exported
// symbols, so `dlsym` cannot find them and a binding that looked for one
// would fail at load with a "symbol not found" for something that was never a
// symbol. They are transcribed from the header, and they are the reason this
// file reproduces a tag scheme instead of calling into the library: the two
// low bits of a `miniexp_t` say what it is.
//
//   `& 3 == 3` a number, whose value is the pointer shifted right by two
//   `& 3 == 2` a symbol
//   `& 3 == 0` a list: null for nil, otherwise a two-word cons cell
//
// Everything else — strings and other objects — is reached through the real
// exported functions above.

/// `miniexp_numberp`.
pub fn is_number(p: MiniExp) -> bool {
    p as usize & 3 == 3
}

/// `miniexp_to_int`. Meaningful only for a number.
///
/// The header casts to `int` before shifting, so the arithmetic is 32-bit and
/// signed on every platform: a negative coordinate must stay negative.
pub fn to_int(p: MiniExp) -> i32 {
    (p as usize as u32 as i32) >> 2
}

/// `miniexp_symbolp`.
pub fn is_symbol(p: MiniExp) -> bool {
    p as usize & 3 == 2
}

/// `miniexp_consp`: a pair, which is to say a non-empty list.
pub fn is_cons(p: MiniExp) -> bool {
    !p.is_null() && p as usize & 3 == 0
}

/// `miniexp_car`, the head of a list.
///
/// # Safety
///
/// `p` must be an s-expression owned by a document that has not yet been
/// released — the cons cell it may point at belongs to djvulibre's heap.
pub unsafe fn car(p: MiniExp) -> MiniExp {
    if is_cons(p) {
        *p.cast::<MiniExp>()
    } else {
        MINIEXP_NIL
    }
}

/// `miniexp_cdr`, the rest of a list.
///
/// # Safety
///
/// As [`car`].
pub unsafe fn cdr(p: MiniExp) -> MiniExp {
    if is_cons(p) {
        *p.cast::<MiniExp>().add(1)
    } else {
        MINIEXP_NIL
    }
}

/// The file names a system-installed djvulibre goes by.
///
/// Bare names on purpose: they are handed to the platform loader, which
/// resolves them against the machine's own library path. No directory beside
/// the executable is searched, unlike the PDFium binding â a copy found there
/// would be a bundled one, and Â§65.1 forbids bundling a Class B library.
fn library_names() -> &'static [&'static str] {
    // Naming a shared library is the platform boundary's business, not a
    // capability question, so this is the one place a target check belongs.
    if cfg!(target_os = "windows") {
        &["libdjvulibre.dll", "djvulibre.dll"]
    } else if cfg!(target_os = "macos") {
        &["libdjvulibre.21.dylib", "libdjvulibre.dylib"]
    } else {
        &["libdjvulibre.so.21", "libdjvulibre.so"]
    }
}

/// Every place a djvulibre might be, most specific first.
///
/// `PULPIT_DJVU_PATH` names either the library file itself or a directory
/// holding it, which is what lets somebody test against a build the loader
/// would not otherwise find.
fn candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("PULPIT_DJVU_PATH") {
        let configured = PathBuf::from(configured);
        if configured.is_dir() {
            candidates.extend(library_names().iter().map(|name| configured.join(name)));
        } else {
            candidates.push(configured);
        }
    }
    candidates.extend(library_names().iter().map(PathBuf::from));
    candidates
}

macro_rules! api {
    ($( $field:ident : $symbol:literal : unsafe extern "C" fn($($arg:ty),* $(,)?) $(-> $ret:ty)? ),* $(,)?) => {
        /// djvulibre's entry points, and the loaded library they came from.
        pub struct Api {
            /// Every function pointer below borrows from this. Function
            /// pointers have no destructor, so field order does not matter
            /// for soundness â but the library must not be unloaded while an
            /// [`Api`] lives, which is why it is owned here rather than
            /// leaked.
            _library: libloading::Library,
            /// Where the library was found, for diagnostics.
            path: PathBuf,
            $( pub $field: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
        }

        impl Api {
            /// Resolve every symbol from an already-open library.
            ///
            /// # Safety
            ///
            /// The caller warrants that `library` really is djvulibre: the
            /// signatures below are asserted, not checked, and a library that
            /// exports these names with other shapes would be called wrongly.
            unsafe fn bind(library: libloading::Library, path: PathBuf) -> std::result::Result<Api, String> {
                $(
                    let $field = *library
                        .get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>($symbol)
                        .map_err(|e| format!("{}: {e}", String::from_utf8_lossy(&$symbol[..$symbol.len() - 1])))?;
                )*
                Ok(Api { _library: library, path, $( $field, )* })
            }
        }
    };
}

api! {
    context_create: b"ddjvu_context_create\0": unsafe extern "C" fn(*const c_char) -> *mut Context,
    context_release: b"ddjvu_context_release\0": unsafe extern "C" fn(*mut Context),
    cache_set_size: b"ddjvu_cache_set_size\0": unsafe extern "C" fn(*mut Context, c_ulong),

    message_wait: b"ddjvu_message_wait\0": unsafe extern "C" fn(*mut Context) -> *const MessageAny,
    message_peek: b"ddjvu_message_peek\0": unsafe extern "C" fn(*mut Context) -> *const MessageAny,
    message_pop: b"ddjvu_message_pop\0": unsafe extern "C" fn(*mut Context),

    job_status: b"ddjvu_job_status\0": unsafe extern "C" fn(*mut Job) -> c_int,
    job_release: b"ddjvu_job_release\0": unsafe extern "C" fn(*mut Job),

    document_create_by_filename_utf8: b"ddjvu_document_create_by_filename_utf8\0":
        unsafe extern "C" fn(*mut Context, *const c_char, c_int) -> *mut Document,
    document_job: b"ddjvu_document_job\0": unsafe extern "C" fn(*mut Document) -> *mut Job,
    document_get_pagenum: b"ddjvu_document_get_pagenum\0": unsafe extern "C" fn(*mut Document) -> c_int,
    // The C header spells this `ddjvu_document_get_pageinfo`, a macro that
    // appends `sizeof(ddjvu_pageinfo_t)`. Binding the underlying `_imp` and
    // passing the size explicitly is what makes the struct's growth in
    // DDJVUAPI 18 safe in both directions.
    document_get_pageinfo_imp: b"ddjvu_document_get_pageinfo_imp\0":
        unsafe extern "C" fn(*mut Document, c_int, *mut PageInfo, c_uint) -> c_int,

    page_create_by_pageno: b"ddjvu_page_create_by_pageno\0":
        unsafe extern "C" fn(*mut Document, c_int) -> *mut Page,
    page_job: b"ddjvu_page_job\0": unsafe extern "C" fn(*mut Page) -> *mut Job,
    page_render: b"ddjvu_page_render\0": unsafe extern "C" fn(
        *mut Page, c_int, *const Rect, *const Rect, *const Format, c_ulong, *mut c_char,
    ) -> c_int,

    format_create: b"ddjvu_format_create\0": unsafe extern "C" fn(c_int, c_int, *const c_uint) -> *mut Format,
    format_set_row_order: b"ddjvu_format_set_row_order\0": unsafe extern "C" fn(*mut Format, c_int),
    format_set_y_direction: b"ddjvu_format_set_y_direction\0": unsafe extern "C" fn(*mut Format, c_int),
    format_release: b"ddjvu_format_release\0": unsafe extern "C" fn(*mut Format),

    // The hidden text layer (§59.2), as the same s-expression `djvused
    // print-txt` prints. `maxdetail` names the finest granularity wanted.
    document_get_pagetext: b"ddjvu_document_get_pagetext\0":
        unsafe extern "C" fn(*mut Document, c_int, *const c_char) -> MiniExp,
    // Without this the s-expression stays allocated for as long as the
    // document does, which for a book searched page by page is the whole
    // text layer held twice — once by djvulibre and once in pulpit's cache.
    miniexp_release: b"ddjvu_miniexp_release\0": unsafe extern "C" fn(*mut Document, MiniExp),

    // The three miniexp accessors that are real exported symbols. Every
    // other one this module needs is `static inline` in `miniexp.h`, which
    // `dlsym` cannot find, and is reproduced in Rust below.
    miniexp_stringp: b"miniexp_stringp\0": unsafe extern "C" fn(MiniExp) -> c_int,
    miniexp_to_str: b"miniexp_to_str\0": unsafe extern "C" fn(MiniExp) -> *const c_char,
    miniexp_to_name: b"miniexp_to_name\0": unsafe extern "C" fn(MiniExp) -> *const c_char,
}

impl Api {
    /// Find and bind an installed djvulibre.
    ///
    /// Returns the accumulated per-candidate failure on the way out, because
    /// the list of names already tried is the useful half of the diagnostic
    /// (Â§61.1).
    pub fn load() -> std::result::Result<Api, String> {
        let mut failures = Vec::new();
        for candidate in candidates() {
            // SAFETY: loading a shared library runs its initialisers, which
            // is why this is unsafe at all. The name is either one the
            // platform loader resolves or one the operator supplied.
            match unsafe { libloading::Library::new(&candidate) } {
                Ok(library) => {
                    // SAFETY: the symbols resolved below are djvulibre's, and
                    // their signatures are transcribed from `ddjvuapi.h`. A
                    // library exporting `ddjvu_context_create` that is not
                    // djvulibre is not a case worth defending against.
                    match unsafe { Api::bind(library, candidate.clone()) } {
                        Ok(api) => {
                            tracing::info!(path = %candidate.display(), "bound to djvulibre");
                            return Ok(api);
                        }
                        Err(missing) => failures.push(format!(
                            "{}: loaded, but {missing} is missing (djvulibre 3.5.22 or newer is \
                             needed)",
                            candidate.display()
                        )),
                    }
                }
                Err(e) => failures.push(format!("{}: {e}", candidate.display())),
            }
        }
        Err(failures.join("; "))
    }

    /// Where this library was found.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `ddjvu_document_release`, a macro in the C header.
    ///
    /// # Safety
    ///
    /// `document` must be a live document from this `Api`, and must not be
    /// used again.
    pub unsafe fn document_release(&self, document: *mut Document) {
        (self.job_release)((self.document_job)(document));
    }

    /// `ddjvu_document_decoding_status`, a macro in the C header.
    ///
    /// # Safety
    ///
    /// `document` must be a live document from this `Api`.
    pub unsafe fn document_status(&self, document: *mut Document) -> c_int {
        (self.job_status)((self.document_job)(document))
    }

    /// `ddjvu_page_release`, a macro in the C header.
    ///
    /// # Safety
    ///
    /// `page` must be a live page from this `Api`, and must not be used again.
    pub unsafe fn page_release(&self, page: *mut Page) {
        (self.job_release)((self.page_job)(page));
    }

    /// `ddjvu_document_get_pageinfo`, a macro in the C header.
    ///
    /// # Safety
    ///
    /// `document` must be a live document from this `Api`.
    pub unsafe fn page_info(&self, document: *mut Document, page: c_int) -> (c_int, PageInfo) {
        let mut info = PageInfo::default();
        let status = (self.document_get_pageinfo_imp)(
            document,
            page,
            &mut info,
            std::mem::size_of::<PageInfo>() as c_uint,
        );
        (status, info)
    }
}

/// A C string for a path, for the one call that takes one.
///
/// djvulibre's `_utf8` entry point wants UTF-8 bytes; a path that is not
/// UTF-8 is refused here rather than silently transliterated, which would
/// open a *different* file.
pub fn path_argument(path: &Path) -> std::result::Result<CString, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?;
    CString::new(text).map_err(|_| format!("{} contains a NUL byte", path.display()))
}

/// A borrowed C string as a Rust `String`, for message text owned by the
/// library.
///
/// # Safety
///
/// `text` must be NUL-terminated and valid for the duration of the call.
pub unsafe fn borrowed_text(text: *const c_char) -> Option<String> {
    if text.is_null() {
        return None;
    }
    Some(CStr::from_ptr(text).to_string_lossy().into_owned())
}
