//! The form-fill spike (§14.3 step 6), and what it decided.
//!
//! The gate this had to pass, in the specification's words: *raw `FORM_*`
//! bindings through pdfium-render, event forwarding over the existing IPC,
//! dirty-rect `FPDF_FFLDraw` compositing; measure type-to-glyph latency. This
//! gate decides §8.6's viability before any form UI is built.*
//!
//! Each of those is a test below, and the last one prints its number.
//!
//! # Why it matters that this works
//!
//! The alternative — the one pdfform used, and the one this replaces — is for
//! the application to draw its own text box over the field, take the typing
//! itself, and write the value back into the PDF afterwards. That is a second
//! implementation of what a form field looks like when it has a value in it:
//! comb spacing, auto-sizing, quadding, multiline wrapping, the checkbox glyph
//! from `/ZapfDingbats`. It will disagree with PDFium's somewhere, and where it
//! disagrees is between what the person filling the form sees and what the
//! file will show everybody else.
//!
//! Forwarding raw events removes the second implementation. The code that
//! edits the field is the code that draws it.
//!
//! Skipped with a message when no `libpdfium` is installed.

#![cfg(feature = "pdfium")]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use pulpit_core::page::{PageIndex, PagePoint};
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::protocol::{
    DocumentRequest, DocumentResponse, FormInputEvent, FormKey,
};
use pulpit_render::document::worker::DocumentWorker;
use pulpit_render::document::{FieldKind, PdfDocument, SaveOptions};
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_testkit::corpus;

fn binding() -> Option<MutexGuard<'static, PdfiumBackend>> {
    static BACKEND: OnceLock<Option<Mutex<PdfiumBackend>>> = OnceLock::new();
    let backend = BACKEND
        .get_or_init(|| {
            if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
                std::env::set_var(
                    "PULPIT_PDFIUM_PATH",
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib"),
                );
            }
            match PdfiumBackend::bind() {
                Ok(backend) => Some(Mutex::new(backend)),
                Err(error) => {
                    eprintln!("skipping the form-fill spike: {error}");
                    None
                }
            }
        })
        .as_ref()?;
    Some(
        backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// The corpus's control case: one plain text field named `name`, nothing wrong
/// with it. If the spike cannot fill this, it cannot fill anything.
fn plain_form(directory: &std::path::Path) -> Option<PathBuf> {
    let case = corpus()
        .into_iter()
        .find(|case| case.name == "plain-text-field")?;
    let path = directory.join("form.pdf");
    std::fs::write(&path, &case.bytes).ok()?;
    Some(path)
}

/// Click into the middle of a field's first widget, so PDFium gives it focus.
fn click_into(document: &mut PdfDocument<'_>, field: &str) -> Option<PagePoint> {
    let target = document
        .fields()
        .ok()?
        .into_iter()
        .find(|candidate| candidate.name == field)?;
    let bounds = target.anchor_on(PageIndex(0))?;
    let at = PagePoint {
        x: (bounds.left + bounds.right) / 2.0,
        y: (bounds.top + bounds.bottom) / 2.0,
    };
    document
        .form_event(PageIndex(0), FormInputEvent::PointerDown { at })
        .ok()?;
    document
        .form_event(PageIndex(0), FormInputEvent::PointerUp { at })
        .ok()?;
    Some(at)
}

#[test]
fn a_documents_fields_are_found_through_the_form_fill_environment() {
    // The first thing the environment buys: the fields exist and are
    // describable. `fields()` returned an empty list before it was wired.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        panic!("the corpus no longer carries its control case")
    };

    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let document = PdfDocument::new(Box::new(engine), 51);
    let fields = document.fields().expect("the fields are readable");

    assert_eq!(fields.len(), 1, "{fields:?}");
    assert_eq!(fields[0].name, "name");
    assert_eq!(fields[0].kind, FieldKind::Text);
    assert!(!fields[0].read_only);
    assert!(
        fields[0].anchor_on(PageIndex(0)).is_some(),
        "a field with no rectangle cannot be navigated to"
    );
}

#[test]
fn typing_into_a_field_puts_the_characters_in_it() {
    // The gate itself. Raw events in; the field holds what was typed.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };

    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let mut document = PdfDocument::new(Box::new(engine), 52);
    assert!(
        click_into(&mut document, "name").is_some(),
        "no field to click"
    );

    for character in "Ada".chars() {
        document
            .form_event(PageIndex(0), FormInputEvent::Char { character })
            .expect("a character is accepted");
    }
    // Focus loss is what commits it (§8.6).
    document
        .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
        .expect("focus can be dropped");

    assert_eq!(
        document.field_value("name").expect("the field is readable"),
        "Ada",
        "the characters did not reach the field"
    );
}

#[test]
fn backspace_takes_a_character_back_out() {
    // Not a separate feature: the point is that *editing* is PDFium's too, so
    // a key that is not a character still does what it does in a form.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };

    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let mut document = PdfDocument::new(Box::new(engine), 53);
    click_into(&mut document, "name");
    for character in "Adax".chars() {
        document
            .form_event(PageIndex(0), FormInputEvent::Char { character })
            .expect("a character is accepted");
    }
    document
        .form_event(
            PageIndex(0),
            FormInputEvent::KeyDown {
                key: FormKey::Backspace,
            },
        )
        .expect("backspace is accepted");
    document
        .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
        .unwrap();

    assert_eq!(document.field_value("name").unwrap(), "Ada");
}

#[test]
fn a_keystroke_reports_the_rectangle_it_dirtied() {
    // §9.4: the engine answers with invalidated page rectangles, which is what
    // makes a re-composite cost a field rather than a page. A keystroke that
    // reported nothing would leave the caret and the new glyph undrawn.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };

    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let mut document = PdfDocument::new(Box::new(engine), 54);
    click_into(&mut document, "name");

    let result = document
        .form_event(PageIndex(0), FormInputEvent::Char { character: 'A' })
        .expect("a character is accepted");
    assert!(
        !result.invalidated.is_empty(),
        "a keystroke that changed the field invalidated nothing"
    );
    let field = document.fields().unwrap().remove(0);
    let bounds = field
        .anchor_on(PageIndex(0))
        .expect("the field has a place");
    for dirty in &result.invalidated {
        // The invalidation is about the field, not about the page. A little
        // slack, because PDFium includes the widget's border.
        assert!(
            dirty.left >= bounds.left - 8.0
                && dirty.right <= bounds.right + 8.0
                && dirty.top >= bounds.top - 8.0
                && dirty.bottom <= bounds.bottom + 8.0,
            "{dirty:?} is not inside the field at {bounds:?}"
        );
    }
}

#[test]
fn a_committed_value_is_one_revision_and_marks_the_document_unsaved() {
    // §8.6: a committed change is a document change like any other, in the
    // same history as the annotations. Typing is not — only the commit is.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };

    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let mut document = PdfDocument::new(Box::new(engine), 55);
    let before = document.revision();
    click_into(&mut document, "name");
    for character in "Ada".chars() {
        document
            .form_event(PageIndex(0), FormInputEvent::Char { character })
            .unwrap();
    }

    let committed = document
        .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
        .unwrap()
        .committed;
    assert!(committed.is_some(), "the commit was not reported");
    let committed = committed.unwrap();
    assert!(
        committed.revision > before,
        "a committed value did not move the revision"
    );
    assert!(document.is_dirty(), "a filled form is unsaved work");
}

#[test]
fn a_filled_form_saves_and_reopens_with_the_value_in_it() {
    // Acceptance criterion 5, end to end: filled, saved, reopened, still
    // filled. The reopen goes through a fresh engine, so nothing in memory is
    // being read back to itself.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };
    let saved = directory.path().join("filled.pdf");

    {
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 56);
        click_into(&mut document, "name");
        for character in "Ada Lovelace".chars() {
            document
                .form_event(PageIndex(0), FormInputEvent::Char { character })
                .unwrap();
        }
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap();
        document
            .save_as(&saved, SaveOptions::verified())
            .expect("a filled form saves");
    }

    {
        let engine = PdfiumDocument::open(&mut guard, &saved).expect("the filled form reopens");
        let document = PdfDocument::new(Box::new(engine), 57);
        assert_eq!(
            document.field_value("name").expect("the field survived"),
            "Ada Lovelace"
        );
    }

    // …and the source is untouched (A6, criterion 11).
    let engine = PdfiumDocument::open(&mut guard, &path).expect("the source still opens");
    let source = PdfDocument::new(Box::new(engine), 58);
    assert_eq!(source.field_value("name").unwrap(), "");
}

#[test]
fn a_typed_value_is_in_the_picture_before_it_is_in_the_file() {
    // The `FPDF_FFLDraw` half, and the reason it is not optional.
    //
    // `FPDF_RenderPageBitmap` draws the appearance stream the file was saved
    // with. A value typed a moment ago lives in PDFium's form-fill environment
    // and is not in any appearance yet — so without the compositing pass the
    // person filling the form watches an empty box while they type.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };

    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let mut document = PdfDocument::new(Box::new(engine), 59);
    let (width, height) = (400u32, 500u32);
    let empty = document
        .render_page(
            PageIndex(0),
            pulpit_core::notes::Region::FULL,
            width,
            height,
        )
        .expect("the empty form renders");

    click_into(&mut document, "name");
    for character in "MMMMMMMM".chars() {
        document
            .form_event(PageIndex(0), FormInputEvent::Char { character })
            .unwrap();
    }

    let typed = document
        .render_page(
            PageIndex(0),
            pulpit_core::notes::Region::FULL,
            width,
            height,
        )
        .expect("the form renders while it is being typed into");

    let changed = empty
        .chunks_exact(4)
        .zip(typed.chunks_exact(4))
        .filter(|(before, after)| before != after)
        .count();
    assert!(
        changed > 20,
        "only {changed} pixels changed — the typed value is not in the picture, \
         which means FPDF_FFLDraw is not running"
    );
}

#[test]
fn the_form_events_survive_the_worker_boundary() {
    // §8.6 requires that this stay in the supervised worker: form filling
    // exercises PDFium's most complex code paths on hostile input, and a crash
    // mid-fill must lose at most uncommitted in-field state. So the events go
    // through the worker's own dispatch rather than straight to the engine.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };

    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let mut worker = DocumentWorker::new();
    worker.adopt(PdfDocument::new(Box::new(engine), 60));

    let DocumentResponse::Fields(fields) = worker.handle(DocumentRequest::ListFields) else {
        panic!("the worker could not list the fields")
    };
    let bounds = fields[0].anchor_on(PageIndex(0)).expect("a place to click");
    let at = PagePoint {
        x: (bounds.left + bounds.right) / 2.0,
        y: (bounds.top + bounds.bottom) / 2.0,
    };

    for event in [
        FormInputEvent::PointerDown { at },
        FormInputEvent::PointerUp { at },
        FormInputEvent::Char { character: 'A' },
        FormInputEvent::Char { character: 'd' },
        FormInputEvent::Char { character: 'a' },
        FormInputEvent::Focus { gained: false },
    ] {
        let response = worker.handle(DocumentRequest::FormEvent {
            page: PageIndex(0),
            event: event.clone(),
        });
        assert!(
            matches!(response, DocumentResponse::Form(_)),
            "the worker refused {event:?}: {response:?}"
        );
    }

    let DocumentResponse::Fields(fields) = worker.handle(DocumentRequest::ListFields) else {
        panic!("the worker could not list the fields again")
    };
    assert_eq!(
        fields[0].value, "Ada",
        "the value did not cross the boundary"
    );
}

#[test]
fn a_field_cannot_be_set_from_outside_the_page() {
    // §8.6's "exactly one editing surface", as a refusal rather than a
    // comment. A second way to write a value is the thing that would let an
    // inspector and the page disagree about what a field holds.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };
    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let mut document = PdfDocument::new(Box::new(engine), 61);
    assert!(
        document.set_field("name", "Ada").is_err(),
        "a field was written from outside the page"
    );
    assert_eq!(document.field_value("name").unwrap(), "");
}

#[test]
fn type_to_glyph_latency_is_measured_rather_than_assumed() {
    // The number §14.3 step 6 asks for. This is the *engine* half of the
    // round trip — the event in, the invalidation out — which is what the
    // spike had to decide on: if a keystroke cost tens of milliseconds here,
    // no amount of care in the IPC or the UI would make a form feel typed
    // into.
    //
    // The specification is explicit that the IPC hop that follows MUST NOT be
    // optimised away by moving PDFium in-process, so what matters is that this
    // leaves room for it. A local pipe round trip is tens of microseconds.
    let Some(mut guard) = binding() else { return };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let Some(path) = plain_form(directory.path()) else {
        return;
    };

    let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
    let mut document = PdfDocument::new(Box::new(engine), 62);
    click_into(&mut document, "name");

    // Warm: the first keystroke into a field also builds its editing state.
    document
        .form_event(PageIndex(0), FormInputEvent::Char { character: 'x' })
        .unwrap();

    let keystrokes = 200;
    let start = Instant::now();
    for step in 0..keystrokes {
        // Varied, so this is not one character's fast path repeated.
        let character = char::from(b'a' + (step % 26) as u8);
        document
            .form_event(PageIndex(0), FormInputEvent::Char { character })
            .expect("a character is accepted");
    }
    let per_keystroke = start.elapsed() / keystrokes;

    // …and the redraw that follows one, at a size a reader actually looks at.
    let (width, height) = (900u32, 1200u32);
    let _ = document
        .render_page(
            PageIndex(0),
            pulpit_core::notes::Region::FULL,
            width,
            height,
        )
        .unwrap();
    let start = Instant::now();
    let rounds = 10;
    for _ in 0..rounds {
        let _ = document
            .render_page(
                PageIndex(0),
                pulpit_core::notes::Region::FULL,
                width,
                height,
            )
            .unwrap();
    }
    let per_redraw = start.elapsed() / rounds;

    println!("  one keystroke through the form-fill environment: {per_keystroke:?}");
    println!("  one full-page redraw with the FFLDraw pass: {per_redraw:?}");

    // The budget is not "fast", it is "leaves room". A keystroke has a frame —
    // 16 ms — to become a glyph, and the engine's share of that has to leave
    // most of it for the pipe, the supervisor, the compositing and the draw.
    assert!(
        per_keystroke <= Duration::from_millis(2),
        "a keystroke cost {per_keystroke:?} in the engine alone, which does not \
         leave a frame's room for the IPC and the redraw"
    );
}
