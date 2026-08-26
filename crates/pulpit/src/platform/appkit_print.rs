//! Printing through `NSPrintOperation`, which is the macOS print panel.
//!
//! ## Why PDFKit and not a printer context
//!
//! macOS has no "show the print panel for this file" call. What it has is
//! `NSPrintOperation`, which drives a panel and then asks *something* to draw
//! the pages — and pulpit drawing pages is the job
//! [`crate::printing`] says it does not take on.
//!
//! PDFKit is the something. `-[PDFDocument
//! printOperationForPrintInfo:scalingMode:autoRotate:]` hands back an
//! operation that already knows how to draw the file, so the panel's paper
//! size, duplex, tray, page range, copies and scaling are applied by Apple's
//! code to Apple's rendering of the PDF. pulpit contributes the file and the
//! job's name. This is the same bargain the Linux portal offers, reached a
//! longer way round.
//!
//! It is worth being plain about the alternative that was not taken: reading
//! the panel's settings back out of an `NSPrintInfo` and spooling the file
//! with `lp` would have been a third of the code, and every setting nobody
//! remembered to translate — duplex, page order, pages-per-sheet — would have
//! been a control the reader set and the printer ignored. A dialog whose
//! choices silently do nothing is worse than no dialog.
//!
//! ## Why this one blocks the event loop
//!
//! `-[NSPrintOperation runOperation]` runs the panel modally and AppKit
//! requires it on the main thread. The Linux portal is handed to a thread and
//! answered by a message; this cannot be, so
//! [`crate::platform::services::PlatformServices::print_dialog_wants_main_thread`]
//! is true here and the application calls it in place. AppKit services its
//! own panel from that modal run loop, so the panel is live and responsive;
//! what stops for the duration is pulpit's own drawing. The audience window
//! keeps its last complete frame throughout, which is all the third standing
//! rule asks of it.

use std::ffi::c_void;

use crate::platform::services::PrintJob;
use crate::platform::Outcome;

type Id = *mut c_void;
type Sel = *mut c_void;
/// Objective-C `BOOL`. A signed char on x86_64 and a C99 `_Bool` on
/// aarch64; one byte on both, and 0/1 is a valid value for either, which is
/// why one spelling serves both rather than a `cfg` on the architecture.
type ObjcBool = i8;
const NO: ObjcBool = 0;
const YES: ObjcBool = 1;

/// `kPDFPrintPageScaleDownToFit`.
///
/// The one scaling decision pulpit makes, and it makes it because
/// `NSPrintOperation` takes it at construction, before the reader has seen a
/// panel to say otherwise. Down-to-fit rather than none: a page larger than
/// the paper otherwise prints with its edges cut off, silently, and a reader
/// printing a document wants the document.
const SCALE_DOWN_TO_FIT: isize = 2;

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "Quartz", kind = "framework")]
extern "C" {}

#[link(name = "Foundation", kind = "framework")]
extern "C" {}

extern "C" {
    fn objc_getClass(name: *const u8) -> Id;
    fn sel_registerName(name: *const u8) -> Sel;
    fn objc_msgSend();
}

/// `objc_msgSend` is variadic and must be called through a pointer typed for
/// the message being sent; there is no one signature that is correct for all
/// of them. Each of these names one shape.
mod send {
    use super::{Id, ObjcBool, Sel};

    pub unsafe fn id(receiver: Id, selector: Sel) -> Id {
        let f: extern "C" fn(Id, Sel) -> Id = std::mem::transmute(super::objc_msgSend as *const ());
        f(receiver, selector)
    }

    pub unsafe fn id_with_id(receiver: Id, selector: Sel, argument: Id) -> Id {
        let f: extern "C" fn(Id, Sel, Id) -> Id =
            std::mem::transmute(super::objc_msgSend as *const ());
        f(receiver, selector, argument)
    }

    pub unsafe fn void_with_bool(receiver: Id, selector: Sel, argument: ObjcBool) {
        let f: extern "C" fn(Id, Sel, ObjcBool) =
            std::mem::transmute(super::objc_msgSend as *const ());
        f(receiver, selector, argument)
    }

    pub unsafe fn bool_(receiver: Id, selector: Sel) -> ObjcBool {
        let f: extern "C" fn(Id, Sel) -> ObjcBool =
            std::mem::transmute(super::objc_msgSend as *const ());
        f(receiver, selector)
    }

    /// `-[PDFDocument printOperationForPrintInfo:scalingMode:autoRotate:]`.
    pub unsafe fn print_operation(
        receiver: Id,
        selector: Sel,
        info: Id,
        scaling: isize,
        auto_rotate: ObjcBool,
    ) -> Id {
        let f: extern "C" fn(Id, Sel, Id, isize, ObjcBool) -> Id =
            std::mem::transmute(super::objc_msgSend as *const ());
        f(receiver, selector, info, scaling, auto_rotate)
    }
}

unsafe fn class(name: &[u8]) -> Option<Id> {
    let class = objc_getClass(name.as_ptr());
    (!class.is_null()).then_some(class)
}

unsafe fn selector(name: &[u8]) -> Sel {
    sel_registerName(name.as_ptr())
}

/// An `NSString` from a Rust string, autoreleased into the caller's pool.
unsafe fn ns_string(value: &str) -> Option<Id> {
    let class = class(b"NSString\0")?;
    // Interior NULs would truncate the string silently. A document whose name
    // contains one is not worth refusing a print over, so they are dropped.
    let bytes: Vec<u8> = value
        .bytes()
        .filter(|byte| *byte != 0)
        .chain(std::iter::once(0))
        .collect();
    let string = send::id_with_id(
        class,
        selector(b"stringWithUTF8String:\0"),
        bytes.as_ptr() as Id,
    );
    (!string.is_null()).then_some(string)
}

/// Whether this session can put the macOS print panel up.
///
/// A question about the frameworks that answered, not about the operating
/// system: a build that did not link Quartz has no `PDFDocument`, and
/// reporting a print panel it cannot open would put a dead command in a menu.
pub fn available() -> bool {
    unsafe { class(b"PDFDocument\0").is_some() && class(b"NSPrintOperation\0").is_some() }
}

/// Put the macOS print panel up and print what it says.
///
/// **Main thread only**, and blocking until the reader is done with the
/// panel. See the module note.
pub fn print_with_dialog(job: &PrintJob) -> Outcome {
    if !job.file.is_file() {
        return Outcome::failed("there is nothing at that path to print");
    }
    unsafe { run(job) }
}

unsafe fn run(job: &PrintJob) -> Outcome {
    let Some(pool_class) = class(b"NSAutoreleasePool\0") else {
        return Outcome::failed("AppKit did not answer");
    };
    // Every object below is autoreleased; without a pool of our own they
    // would accumulate on whatever pool the event loop happens to be holding,
    // which for a document-sized `PDFDocument` is a lot to leave lying about.
    let pool = send::id(
        send::id(pool_class, selector(b"alloc\0")),
        selector(b"init\0"),
    );
    let outcome = print_inside_pool(job);
    send::id(pool, selector(b"drain\0"));
    outcome
}

unsafe fn print_inside_pool(job: &PrintJob) -> Outcome {
    let (Some(document_class), Some(url_class), Some(info_class)) = (
        class(b"PDFDocument\0"),
        class(b"NSURL\0"),
        class(b"NSPrintInfo\0"),
    ) else {
        return Outcome::Unsupported {
            what: "the macOS print panel",
        };
    };

    let Some(path) = ns_string(&job.file.to_string_lossy()) else {
        return Outcome::failed("the path to print could not be read");
    };
    let url = send::id_with_id(url_class, selector(b"fileURLWithPath:\0"), path);
    if url.is_null() {
        return Outcome::failed("the path to print could not be read");
    }

    let document = send::id_with_id(
        send::id(document_class, selector(b"alloc\0")),
        selector(b"initWithURL:\0"),
        url,
    );
    if document.is_null() {
        // PDFKit read the file and would not have it. Said as a failure
        // rather than a refusal: nobody chose this.
        return Outcome::failed("this file could not be opened for printing");
    }

    let info = send::id(info_class, selector(b"sharedPrintInfo\0"));
    if info.is_null() {
        send::id(document, selector(b"release\0"));
        return Outcome::failed("macOS offered no printer settings");
    }

    let operation = send::print_operation(
        document,
        selector(b"printOperationForPrintInfo:scalingMode:autoRotate:\0"),
        info,
        SCALE_DOWN_TO_FIT,
        YES,
    );
    if operation.is_null() {
        send::id(document, selector(b"release\0"));
        return Outcome::failed("macOS would not start the print");
    }

    // The queue shows the document's name, never the scratch copy's — which
    // is the whole reason the job carries a title separately from its file.
    if let Some(title) = ns_string(&job.title) {
        send::id_with_id(operation, selector(b"setJobTitle:\0"), title);
    }
    // The panel is the point. The progress panel goes with it: a print that
    // takes a while with nothing on screen reads as a print that did not
    // happen.
    send::void_with_bool(operation, selector(b"setShowsPrintPanel:\0"), YES);
    send::void_with_bool(operation, selector(b"setShowsProgressPanel:\0"), YES);

    let ran = send::bool_(operation, selector(b"runOperation\0"));
    send::id(document, selector(b"release\0"));

    if ran != NO {
        return Outcome::Done;
    }
    // `runOperation` answers NO for a cancelled panel and for a failed job
    // alike, and does not say which. Refused rather than Failed, because
    // cancelling is overwhelmingly the reason a person sees this and telling
    // them their own decision went wrong is the worse mistake of the two.
    Outcome::refused("The print was cancelled.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(path: &str) -> PrintJob {
        PrintJob {
            file: std::path::PathBuf::from(path),
            title: "Lease agreement".into(),
            pages: Vec::new(),
            copies: 1,
            destination: None,
        }
    }

    #[test]
    fn a_file_that_is_not_there_fails_before_the_panel_opens() {
        // Checked before any Objective-C is sent, so this test says something
        // on a machine with no window server as well as on one with.
        assert!(matches!(
            print_with_dialog(&job("/nonexistent/pulpit-test.pdf")),
            Outcome::Failed { .. }
        ));
    }

    #[test]
    fn the_frameworks_are_asked_for_rather_than_assumed() {
        // Reaching the classes is what `available` means; that it answers at
        // all is what this pins.
        let _ = available();
    }
}
