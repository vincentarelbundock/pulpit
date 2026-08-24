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
use std::time::{Duration, Instant};

use crate::testkit::corpus;
use pulpit_core::page::{PageIndex, PagePoint};
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::protocol::{
    DocumentRequest, DocumentResponse, FormInputEvent, FormKey, KeyModifiers,
};
use pulpit_render::document::worker::DocumentWorker;
use pulpit_render::document::{FieldKind, PdfDocument, SaveOptions};
use pulpit_render::pdf::pdfium::PdfiumBackend;

mod common;
mod testkit;

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
    crate::testkit::on_the_pdfium_thread(|| {
        // The first thing the environment buys: the fields exist and are
        // describable. `fields()` returned an empty list before it was wired.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
    });
}

#[test]
fn typing_into_a_field_puts_the_characters_in_it() {
    crate::testkit::on_the_pdfium_thread(|| {
        // The gate itself. Raw events in; the field holds what was typed.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
    });
}

#[test]
fn backspace_takes_a_character_back_out() {
    crate::testkit::on_the_pdfium_thread(|| {
        // Not a separate feature: the point is that *editing* is PDFium's too, so
        // a key that is not a character still does what it does in a form.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
                    modifiers: KeyModifiers::NONE,
                },
            )
            .expect("backspace is accepted");
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap();

        assert_eq!(document.field_value("name").unwrap(), "Ada");
    });
}

#[test]
fn a_keystroke_reports_the_rectangle_it_dirtied() {
    crate::testkit::on_the_pdfium_thread(|| {
        // §9.4: the engine answers with invalidated page rectangles, which is what
        // makes a re-composite cost a field rather than a page. A keystroke that
        // reported nothing would leave the caret and the new glyph undrawn.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
    });
}

#[test]
fn a_committed_value_is_one_revision_and_marks_the_document_unsaved() {
    crate::testkit::on_the_pdfium_thread(|| {
        // §8.6: a committed change is a document change like any other, in the
        // same history as the annotations. Typing is not — only the commit is.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
        // *Which* field, and what it now holds. This used to go unasserted,
        // and both were empty: a focus loss is the usual way a text field
        // commits, and by the time the commit is reported PDFium has no
        // focused annotation left to name it. A caller that wanted to say
        // which field it had just filled could not.
        assert_eq!(committed.name, "name");
        assert_eq!(committed.value, "Ada");
        assert!(document.is_dirty(), "a filled form is unsaved work");
    });
}

#[test]
fn a_filled_form_saves_and_reopens_with_the_value_in_it() {
    crate::testkit::on_the_pdfium_thread(|| {
        // Acceptance criterion 5, end to end: filled, saved, reopened, still
        // filled. The reopen goes through a fresh engine, so nothing in memory is
        // being read back to itself.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
    });
}

#[test]
fn a_save_asked_for_mid_edit_commits_the_caret_first() {
    crate::testkit::on_the_pdfium_thread(|| {
        // The application's Save As, in the order the application performs it.
        //
        // Nothing here defocuses the field because the test knows to: it
        // defocuses because the last answer said a widget holds the caret,
        // which is the rule `App::ask_form_commit_before_save` follows.
        // Losing focus is what commits an in-progress edit, and `write_to`
        // serialises what the document holds — so a save sent straight out
        // while someone is still typing would write a file without the
        // characters they can see on screen.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = plain_form(directory.path()) else {
            return;
        };
        let saved = directory.path().join("saved-mid-edit.pdf");

        {
            let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
            let mut document = PdfDocument::new(Box::new(engine), 91);
            click_into(&mut document, "name");
            let mut last = None;
            for character in "Grace".chars() {
                last = Some(
                    document
                        .form_event(PageIndex(0), FormInputEvent::Char { character })
                        .unwrap(),
                );
            }

            // The state the application reads when the save arrives.
            let focused = last
                .and_then(|result| result.focused_widget)
                .expect("the caret is in a field");
            let committed = document
                .form_event(focused.page, FormInputEvent::Focus { gained: false })
                .expect("the defocus is taken")
                .committed
                .expect("leaving the field commits what was typed");
            assert_eq!(committed.value, "Grace");

            document
                .save_as(&saved, SaveOptions::verified())
                .expect("the copy is written");
        }

        let engine = PdfiumDocument::open(&mut guard, &saved).expect("the copy reopens");
        let document = PdfDocument::new(Box::new(engine), 92);
        assert_eq!(
            document.field_value("name").expect("the field is there"),
            "Grace",
            "a save asked for mid-edit keeps what was being typed"
        );
    });
}

#[test]
fn a_typed_value_is_in_the_picture_before_it_is_in_the_file() {
    crate::testkit::on_the_pdfium_thread(|| {
        // The `FPDF_FFLDraw` half, and the reason it is not optional.
        //
        // `FPDF_RenderPageBitmap` draws the appearance stream the file was saved
        // with. A value typed a moment ago lives in PDFium's form-fill environment
        // and is not in any appearance yet — so without the compositing pass the
        // person filling the form watches an empty box while they type.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
                None,
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
                None,
            )
            .expect("the form renders while it is being typed into");

        let changed = empty
            .as_chunks::<4>()
            .0
            .iter()
            .zip(typed.as_chunks::<4>().0)
            .filter(|(before, after)| before != after)
            .count();
        assert!(
            changed > 20,
            "only {changed} pixels changed — the typed value is not in the picture, \
         which means FPDF_FFLDraw is not running"
        );
    });
}

#[test]
fn the_form_events_survive_the_worker_boundary() {
    crate::testkit::on_the_pdfium_thread(|| {
        // §8.6 requires that this stay in the supervised worker: form filling
        // exercises PDFium's most complex code paths on hostile input, and a crash
        // mid-fill must lose at most uncommitted in-field state. So the events go
        // through the worker's own dispatch rather than straight to the engine.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
    });
}

#[test]
fn a_field_set_for_undo_goes_through_the_same_editor_as_typing() {
    crate::testkit::on_the_pdfium_thread(|| {
        // §8.6's "exactly one editing surface", kept while still being able to
        // put a value back. `set_field` used to refuse outright, which made a
        // field edit the one mutation with no inverse — and so the one that
        // could not join the undo history the annotations share (§9.1).
        //
        // It no longer refuses, and the way it works is what preserves the
        // rule: it focuses the widget, selects what is in it and replaces the
        // selection, so PDFium does the editing exactly as it does for a
        // person who selected all and typed. There is still one implementation
        // of what a value looks like in a field, and it is still PDFium's.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = plain_form(directory.path()) else {
            return;
        };
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 61);
        assert_eq!(document.field_value("name").unwrap(), "");

        // Type into it the ordinary way…
        click_into(&mut document, "name");
        for character in "Ada".chars() {
            document
                .form_event(PageIndex(0), FormInputEvent::Char { character })
                .unwrap();
        }
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap();
        assert_eq!(document.field_value("name").unwrap(), "Ada");

        // …and put it back the way an undo would.
        assert_eq!(document.set_field("name", "", &[]).unwrap(), "");
        assert_eq!(
            document.field_value("name").unwrap(),
            "",
            "undoing the first fill of an empty field must clear it, not leave it"
        );

        // And forward again, which is what a redo is.
        assert_eq!(document.set_field("name", "Grace", &[]).unwrap(), "Grace");
        assert_eq!(document.field_value("name").unwrap(), "Grace");

        // A field that is not there is still refused rather than invented.
        assert!(document.set_field("nobody", "x", &[]).is_err());
    });
}

#[test]
fn type_to_glyph_latency_is_measured_rather_than_assumed() {
    crate::testkit::on_the_pdfium_thread(|| {
        // The number §14.3 step 6 asks for. This is the *engine* half of the
        // round trip — the event in, the invalidation out — which is what the
        // spike had to decide on: if a keystroke cost tens of milliseconds here,
        // no amount of care in the IPC or the UI would make a form feel typed
        // into.
        //
        // The specification is explicit that the IPC hop that follows MUST NOT be
        // optimised away by moving PDFium in-process, so what matters is that this
        // leaves room for it. A local pipe round trip is tens of microseconds.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
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
                None,
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
                    None,
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
    });
}

/// A choice field: what the arrow keys can do on their own, and what they
/// cannot.
///
/// The asymmetry here is measured rather than assumed, and it is the reason
/// the application treats the two kinds of choice field differently. A list
/// box moves its own selection on `FORM_OnKeyDown`. A closed combo box
/// ignores the same key entirely — in a real viewer it would be travelling to
/// a dropdown that is not open — so a combo box needs `FORM_SetIndexSelected`,
/// which is what `SelectOption` carries.
#[test]
fn a_list_box_answers_the_arrow_keys_and_a_combo_box_needs_the_index() {
    crate::testkit::on_the_pdfium_thread(|| {
        use pulpit_render::document::protocol::{FormKey, KeyModifiers};

        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };

        fn open_case<'a>(
            guard: &'a mut PdfiumBackend,
            name: &str,
            directory: &std::path::Path,
        ) -> Option<PdfDocument<'a>> {
            let case = corpus().into_iter().find(|case| case.name == name)?;
            let path = directory.join(format!("{name}.pdf"));
            std::fs::write(&path, &case.bytes).ok()?;
            let engine = PdfiumDocument::open(guard, &path).ok()?;
            Some(PdfDocument::new(Box::new(engine), 71))
        }
        let directory = tempfile::tempdir().expect("a temporary directory");

        // A list box, driven by the arrow key alone.
        if let Some(mut document) = open_case(&mut guard, "list-box-multi-select", directory.path())
        {
            assert_eq!(document.field_value("colour").unwrap(), "Red");
            // The press is answered with focus alone now: a non-editable
            // choice field's list is the application's to draw, so the click
            // never reaches `FORM_OnLButtonDown` (§8.6). What matters here is
            // that the key still moves the selection — the value after the
            // press is the baseline whatever the press did to it.
            let pressed = click_into(&mut document, "colour");
            assert!(pressed.is_some());
            let after_click = document.field_value("colour").unwrap();
            let arrowed = document
                .form_event(
                    PageIndex(0),
                    FormInputEvent::KeyDown {
                        key: FormKey::Down,
                        modifiers: KeyModifiers::NONE,
                    },
                )
                .unwrap();
            assert!(
                !arrowed.invalidated.is_empty(),
                "a list box must repaint when its selection moves"
            );
            // A list box needs no translation of the arrow key, but it is
            // still reported: its rows are drawn by the application, which
            // needs the labels and the widget's rectangle to draw them.
            let choice = arrowed
                .focused_choice
                .expect("a focused list box must be reported with its options");
            assert!(choice.list_box);
            assert!(!choice.editable);
            assert_eq!(choice.labels.len(), choice.options as usize);
            assert!(!choice.labels.is_empty());
            document
                .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
                .unwrap();
            assert_ne!(
                document.field_value("colour").unwrap(),
                after_click,
                "a list box must move its own selection on an arrow key"
            );
        }

        // A combo box, which does not.
        let Some(mut document) = open_case(&mut guard, "combo-box-plain-options", directory.path())
        else {
            return;
        };
        assert_eq!(document.field_value("country").unwrap(), "Canada");
        click_into(&mut document, "country");

        let arrowed = document
            .form_event(
                PageIndex(0),
                FormInputEvent::KeyDown {
                    key: FormKey::Down,
                    modifiers: KeyModifiers::NONE,
                },
            )
            .unwrap();
        // The focused combo is reported, so the application knows to translate.
        let choice = arrowed
            .focused_choice
            .expect("a focused combo box must be reported as one");
        assert_eq!(choice.options, 2);
        assert_eq!(choice.selected, Some(0));

        // It may repaint — a focus ring, a caret — but the *value* does not
        // move, which is the whole reason the arrow has to be translated. If
        // this ever starts changing, the translation in the application is
        // redundant and would double-step the selection.
        assert_eq!(
            document.field_value("country").unwrap(),
            "Canada",
            "a closed combo box was expected to ignore the arrow key"
        );

        let selected = document
            .form_event(
                PageIndex(0),
                FormInputEvent::SelectOption {
                    index: 1,
                    selected: true,
                },
            )
            .unwrap();
        assert!(
            !selected.invalidated.is_empty(),
            "choosing an option must repaint the field"
        );
        let committed = document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap()
            .committed
            .expect("choosing an option is a committed change");
        assert_eq!(committed.name, "country");
        assert_eq!(committed.value, "France");
        assert_eq!(
            committed.previous, "Canada",
            "a choice is undoable like any other field edit"
        );
    });
}

/// A press on a non-editable choice field focuses it and opens nothing.
///
/// PDFium would draw its own list into the page bitmap. That list is viewer
/// chrome — no saved file has one in it — and compositing it costs a guess at
/// where the engine put it plus a round trip per hovered row, so the press is
/// answered with focus alone and the application draws the list from what
/// comes back (§8.6). The value is still PDFium's: the option chosen goes back
/// as `SelectOption`.
#[test]
fn a_press_on_a_plain_combo_box_focuses_it_without_opening_a_list() {
    crate::testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(case) = corpus()
            .into_iter()
            .find(|case| case.name == "combo-box-plain-options")
        else {
            return;
        };
        let path = directory.path().join("combo.pdf");
        std::fs::write(&path, &case.bytes).expect("the fixture is written");
        let Ok(engine) = PdfiumDocument::open(&mut guard, &path) else {
            return;
        };
        let mut document = PdfDocument::new(Box::new(engine), 71);

        let bounds = document
            .fields()
            .expect("the form lists its fields")
            .into_iter()
            .find(|field| field.name == "country")
            .and_then(|field| field.anchor_on(PageIndex(0)))
            .expect("the combo box has a widget");
        let at = PagePoint {
            x: (bounds.left + bounds.right) / 2.0,
            y: (bounds.top + bounds.bottom) / 2.0,
        };

        let pressed = document
            .form_event(PageIndex(0), FormInputEvent::PointerDown { at })
            .expect("the press is answered");
        assert!(
            pressed.opened_choice,
            "a press on a plain combo box must be answered with focus and \
             leave the list to the application"
        );
        let choice = pressed
            .focused_choice
            .expect("the focused combo box is reported with its options");
        assert!(!choice.editable);
        assert!(!choice.list_box);
        assert_eq!(choice.field, "country");
        assert_eq!(choice.labels.len(), choice.options as usize);
        assert!(choice.labels.contains(&"France".to_string()));
        assert_eq!(choice.page, PageIndex(0));
        assert!(choice.bounds.right > choice.bounds.left);

        // The release of a press the engine never saw is not the engine's
        // either, and neither of them changed the value.
        let released = document
            .form_event(PageIndex(0), FormInputEvent::PointerUp { at })
            .expect("the release is answered");
        assert!(!released.opened_choice);
        assert_eq!(document.field_value("country").unwrap(), "Canada");

        // …and what the drawn list chooses is committed by PDFium, exactly as
        // a click on its own list would have been.
        document
            .form_event(
                PageIndex(0),
                FormInputEvent::SelectOption {
                    index: 1,
                    selected: true,
                },
            )
            .expect("the choice is answered");
        let committed = document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .expect("the focus loss is answered")
            .committed
            .expect("choosing an option is a committed change");
        assert_eq!(committed.name, "country");
        assert_eq!(committed.value, "France");
    });
}

/// A form's values are in the picture the *render pool* draws, not only in the
/// one the document worker draws.
///
/// This is the bug that made a filled form look empty. PDFium splits a form's
/// pixels in two: `FPDF_RenderPageBitmap` draws page content, and a widget's
/// value is drawn from the form-fill environment by `FPDF_FFLDraw`. The reader
/// gets its full pages from the render pool, which had no environment, so
/// every field came out blank — and the only field that ever appeared was the
/// one under a §9.4 partial repaint, which *is* drawn by the document worker.
/// The symptom was "the entries only show up when I click on a field"; the
/// cause had nothing to do with clicking, and the values had been in the file
/// all along.
#[test]
fn field_values_are_drawn_by_the_render_pool_and_not_only_by_the_editor() {
    crate::testkit::on_the_pdfium_thread(|| {
        use pulpit_render::pdf::{NeverCancel, PdfBackend, RenderRequest};

        /// Pixels inside a fraction-of-the-bitmap rectangle that are not white.
        fn ink(rgba: &[u8], width: u32, height: u32, rect: (f32, f32, f32, f32)) -> usize {
            let (l, t, r, b) = rect;
            let (x0, x1) = ((l * width as f32) as u32, (r * width as f32) as u32);
            let (y0, y1) = ((t * height as f32) as u32, (b * height as f32) as u32);
            let mut count = 0;
            for y in y0..y1.min(height) {
                for x in x0..x1.min(width) {
                    let index = ((y * width + x) * 4) as usize;
                    if rgba[index] < 200 || rgba[index + 1] < 200 || rgba[index + 2] < 200 {
                        count += 1;
                    }
                }
            }
            count
        }

        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = plain_form(directory.path()) else {
            return;
        };

        // Fill the field and save, so the file on disk holds a value — which
        // is the state the pool renders from, and the state a form that
        // arrives already filled is in from the start.
        let filled = directory.path().join("filled.pdf");
        let field = {
            let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
            let mut document = PdfDocument::new(Box::new(engine), 81);
            let field = document.fields().unwrap().remove(0);
            click_into(&mut document, &field.name);
            for character in "WWWW".chars() {
                document
                    .form_event(PageIndex(0), FormInputEvent::Char { character })
                    .unwrap();
            }
            document
                .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
                .unwrap();
            document
                .save_as(
                    &filled,
                    SaveOptions {
                        incremental: false,
                        verify: true,
                    },
                )
                .expect("the filled form saves");
            field
        };
        let bounds = field.anchor_on(PageIndex(0)).expect("a widget to look at");

        // Where that widget is, as a fraction of the page.
        let geometry = {
            let engine = PdfiumDocument::open(&mut guard, &filled).expect("the copy opens");
            let document = PdfDocument::new(Box::new(engine), 82);
            document
                .page_geometry(PageIndex(0))
                .expect("a measured page")
        };
        let rect = (
            bounds.left / geometry.width,
            bounds.top / geometry.height,
            bounds.right / geometry.width,
            bounds.bottom / geometry.height,
        );

        // Now draw it the way the reader does: through the pool backend, with
        // no document engine and no editing anywhere in sight.
        let backend: &mut PdfiumBackend = &mut guard;
        let document = PdfBackend::open(backend, &filled).expect("the copy opens for rendering");
        let (width, height) = (900, 1165);
        let mut rgba = vec![0u8; width * height * 4];
        PdfBackend::render_into(
            backend,
            &RenderRequest {
                document,
                page: 0,
                region: pulpit_core::notes::Region::FULL,
                width: width as u32,
                height: height as u32,
                full_size: None,
                with_annotations: true,
            },
            &mut rgba,
            &NeverCancel,
        )
        .expect("the pool renders the page");

        assert!(
            ink(&rgba, width as u32, height as u32, rect) > 0,
            "the field is blank in a pool render: its value is in the file and \
             in the document worker's picture, but not in the one the reader \
             actually shows"
        );
    });
}

/// A date field is recognised as one, and says what it wants typed into it.
///
/// PDF has no date field type: a date is a text field whose `/AA /F` script
/// calls `AFDate_FormatEx("dd mmmm yyyy")`, and Acrobat shows a calendar for
/// it. pulpit has no calendar, so the pattern the script names is the only
/// thing that tells anyone what to type — and reporting the field as plain
/// text threw it away.
#[test]
fn a_date_field_is_recognised_and_says_what_it_expects() {
    crate::testkit::on_the_pdfium_thread(|| {
        use pulpit_render::document::FieldFormat;

        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("dates.pdf");
        std::fs::write(&path, dated_form()).expect("the fixture is written");

        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 91);
        let fields = document.fields().expect("the fields are readable");

        let date = fields
            .iter()
            .find(|f| f.name == "when")
            .expect("a date field");
        assert_eq!(date.kind, FieldKind::Text, "a date really is a text field");
        assert_eq!(
            date.format,
            FieldFormat::Date {
                pattern: "dd mmmm yyyy".into()
            }
        );
        assert_eq!(date.format.hint().as_deref(), Some("date, as dd mmmm yyyy"));

        // A number field is told apart from a date, and a plain one from both.
        let count = fields.iter().find(|f| f.name == "count").expect("a number");
        // …and the decimals its own script asks for come with it: the
        // fixture's `AFNumber_Format(1, …)` is a number to one place, and a
        // hint that says so teaches the shape before it is typed wrong.
        assert_eq!(
            count.format,
            FieldFormat::Number {
                decimals: 1,
                currency: String::new(),
            }
        );
        assert_eq!(count.format.hint().as_deref(), Some("number, 1 decimal"));
        let plain = fields
            .iter()
            .find(|f| f.name == "who")
            .expect("a plain one");
        assert_eq!(plain.format, FieldFormat::Plain);
        assert!(plain.format.hint().is_none());

        // The hint travels with the focus, so it can be said as the caret
        // arrives rather than looked up separately.
        let bounds = date.anchor_on(PageIndex(0)).expect("a widget");
        let at = PagePoint {
            x: (bounds.left + bounds.right) / 2.0,
            y: (bounds.top + bounds.bottom) / 2.0,
        };
        document
            .form_event(PageIndex(0), FormInputEvent::PointerDown { at })
            .unwrap();
        let focused = document
            .form_event(PageIndex(0), FormInputEvent::PointerUp { at })
            .unwrap();
        assert_eq!(
            focused.focused_hint.as_deref(),
            Some("date, as dd mmmm yyyy"),
            "the field with the caret in it must say what it wants"
        );
    });
}

/// Three text fields: a date, a number, and one with no format script.
fn dated_form() -> Vec<u8> {
    let objects: [&str; 7] = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R 7 0 R] \
         /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R 6 0 R 7 0 R] >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (when) /V () /Ff 0 \
         /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R \
         /AA << /F << /S /JavaScript /JS (AFDate_FormatEx(\"dd mmmm yyyy\");) >> \
         /K << /S /JavaScript /JS (AFDate_KeystrokeEx(\"dd mmmm yyyy\");) >> >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (count) /V () /Ff 0 \
         /Rect [100 650 300 680] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R \
         /AA << /F << /S /JavaScript /JS (AFNumber_Format(1, 3, 0, 0, \"\", true);) >> >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (who) /V () /Ff 0 \
         /Rect [100 600 300 630] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R >>",
    ];
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
    }
    let start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// One named corpus case, written where the engine can open it.
fn corpus_form(directory: &std::path::Path, name: &str) -> Option<PathBuf> {
    let case = corpus().into_iter().find(|case| case.name == name)?;
    let path = directory.join(format!("{name}.pdf"));
    std::fs::write(&path, &case.bytes).ok()?;
    Some(path)
}

#[test]
fn undoing_a_checkbox_toggle_presses_the_box_again() {
    crate::testkit::on_the_pdfium_thread(|| {
        // `set_field` used to reach every kind through text replacement, which
        // edits a button not at all — silently, because the read-back then
        // reported the unchanged value as a success. The inverse of a toggle
        // is a press, and only when the state differs from what is asked for:
        // pressing a box that is already right would toggle it wrong.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = corpus_form(directory.path(), "checkbox-standard") else {
            return;
        };
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 71);
        assert_eq!(document.field_value("agree").unwrap(), "Off");

        // Tick it the way a person does…
        click_into(&mut document, "agree");
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap();
        assert_eq!(document.field_value("agree").unwrap(), "Yes");

        // …and put it back the way an undo would.
        assert_eq!(document.set_field("agree", "Off", &[]).unwrap(), "Off");
        // Redo, and then redo again: the second application must not toggle.
        assert_eq!(document.set_field("agree", "Yes", &[]).unwrap(), "Yes");
        assert_eq!(
            document.set_field("agree", "Yes", &[]).unwrap(),
            "Yes",
            "setting a checkbox to the state it already holds must not press it"
        );
    });
}

#[test]
fn undoing_a_radio_choice_presses_the_previous_option() {
    crate::testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = corpus_form(directory.path(), "radio-group") else {
            return;
        };
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 72);
        assert_eq!(document.field_value("contact").unwrap(), "Email");

        // Forward — what a redo of "choose Phone" is…
        assert_eq!(
            document.set_field("contact", "Phone", &[]).unwrap(),
            "Phone"
        );
        // …and back, which is the undo pressing the other option.
        assert_eq!(
            document.set_field("contact", "Email", &[]).unwrap(),
            "Email"
        );
        // A state no press can produce is refused rather than faked: nothing a
        // person can click chooses *nothing* in a chosen group.
        assert!(
            document.set_field("contact", "Off", &[]).is_err(),
            "clearing a chosen radio group has no press to do it with"
        );
        // An option the group does not offer is refused by name.
        assert!(document.set_field("contact", "Fax", &[]).is_err());
    });
}

/// A multi-select list box is toggled one index at a time, and the others stay.
///
/// This is what lets the application draw a list of tick boxes rather than a
/// list of one choice (§8.6). `FORM_SetIndexSelected` is per-index on a
/// multi-select field — it does *not* clear the rest, the way it does on a
/// combo box — so one `SelectOption` per press is enough, and the selection
/// the drawn rows are ticked from comes back on every answer.
#[test]
fn a_multi_select_list_box_toggles_one_index_without_clearing_the_others() {
    crate::testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = corpus_form(directory.path(), "list-box-multi-select") else {
            return;
        };
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 79);

        let pressed = click_into(&mut document, "colour");
        assert!(pressed.is_some(), "the list box takes the press");

        let toggle = |document: &mut PdfDocument<'_>, index: u32, selected: bool| {
            document
                .form_event(
                    PageIndex(0),
                    FormInputEvent::SelectOption { index, selected },
                )
                .expect("the selection is answered")
        };
        let chosen = |document: &PdfDocument<'_>| {
            document
                .fields()
                .unwrap()
                .into_iter()
                .find(|field| field.name == "colour")
                .expect("the list box is listed")
                .selected
        };

        // The file starts on Red alone.
        assert_eq!(chosen(&document), vec![0]);

        // Add Green. Red stays — this is the whole point: a single-select
        // field would have answered with `[2]`.
        let answer = toggle(&mut document, 2, true);
        assert!(
            !answer.invalidated.is_empty(),
            "ticking a row must repaint the field"
        );
        assert_eq!(chosen(&document), vec![0, 2]);
        let choice = answer
            .focused_choice
            .expect("the focused list box is reported with its selection");
        assert!(choice.multiple_selection, "the fixture sets /Ff bit 22");
        assert!(choice.list_box);
        assert_eq!(
            choice.selections,
            vec![0, 2],
            "every chosen row is reported, not only the first"
        );
        assert_eq!(
            choice.selected,
            Some(0),
            "and the single-index field stays the first of them"
        );

        // Add Blue: three at once, which no single string could name.
        toggle(&mut document, 1, true);
        assert_eq!(chosen(&document), vec![0, 1, 2]);

        // And take Red away again, leaving the two that were added.
        let answer = toggle(&mut document, 0, false);
        assert_eq!(chosen(&document), vec![1, 2]);
        assert_eq!(
            answer.focused_choice.map(|choice| choice.selections),
            Some(vec![1, 2]),
            "unticking is reported the same way as ticking"
        );

        // Each tick is committed as it is made, not held back until the field
        // loses focus — that is what lets the drawn rows be ticked from the
        // engine's own answer rather than from a guess. So it is a change with
        // a faithful before-image of its own: one string cannot name three
        // selections, so the indices carry it, and the undo history gets one
        // entry per tick (§8.6).
        let committed = answer
            .committed
            .expect("each tick is a committed change of its own");
        assert_eq!(committed.name, "colour");
        assert_eq!(committed.selected, vec![1, 2]);
        assert_eq!(
            committed.previous_selected,
            vec![0, 1, 2],
            "and its before-image is the selection it undoes to"
        );

        // …and the focus loss that follows has nothing left to commit.
        assert!(
            document
                .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
                .expect("the focus loss is answered")
                .committed
                .is_none(),
            "nothing is held back for the blur to commit"
        );
    });
}

#[test]
fn a_multi_select_list_box_round_trips_through_its_selection_indices() {
    crate::testkit::on_the_pdfium_thread(|| {
        // One string cannot name three selections, which is why the undo
        // record carries the selected indices — and why `set_field` takes
        // them: restoring "the first of what was chosen" is not restoring
        // what was chosen.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = corpus_form(directory.path(), "list-box-multi-select") else {
            return;
        };
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 73);
        let selected = |document: &PdfDocument<'_>| {
            document
                .fields()
                .unwrap()
                .into_iter()
                .find(|field| field.name == "colour")
                .expect("the list box is listed")
                .selected
        };
        assert_eq!(selected(&document), vec![0], "the file starts on Red");

        // Choose Blue and Green together, as a redo of that selection would.
        document.set_field("colour", "Blue", &[1, 2]).unwrap();
        assert_eq!(selected(&document), vec![1, 2]);

        // And back to Red alone, as the undo would.
        document.set_field("colour", "Red", &[0]).unwrap();
        assert_eq!(selected(&document), vec![0]);

        // An index past the options is refused before anything is selected.
        assert!(document.set_field("colour", "", &[9]).is_err());
    });
}

#[test]
fn the_text_field_flag_variants_are_told_apart() {
    crate::testkit::on_the_pdfium_thread(|| {
        // `/FT Tx` hides its variants in `/Ff` bits, and collapsing them into
        // plain text is how a password ends up echoed and a file-select field
        // ends up looking editable when no fill of it can ever succeed.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = corpus_form(directory.path(), "password-field") else {
            return;
        };
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let document = PdfDocument::new(Box::new(engine), 74);
        let field = document
            .fields()
            .unwrap()
            .into_iter()
            .find(|field| field.name == "secret")
            .expect("the password field is listed");
        assert_eq!(field.kind, FieldKind::Text);
        assert!(field.password, "the password flag must be surfaced");
        assert!(!field.file_select);
        assert!(!field.rich_text);
        assert!(
            field.is_editable(),
            "a password field still fills; only the echo is masked"
        );
    });
}

#[test]
fn the_clipboard_reads_and_replaces_what_a_field_has_selected() {
    crate::testkit::on_the_pdfium_thread(|| {
        // Copy and paste are PDFium's too, for the same reason typing is. The
        // selection exists only inside the engine — this layer forwarded the
        // clicks that made it and never modelled it — so the text comes out
        // through `FORM_GetSelectedText` and goes back in through
        // `FORM_ReplaceSelection`, in one edit rather than as a run of
        // synthesised keystrokes that would type *over* the selection.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = plain_form(directory.path()) else {
            return;
        };

        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 75);
        click_into(&mut document, "name");
        for character in "Ada".chars() {
            document
                .form_event(PageIndex(0), FormInputEvent::Char { character })
                .expect("a character is accepted");
        }

        // Nothing is selected yet, so a copy comes back with nothing — and
        // that is an answer rather than a failure.
        let nothing = document
            .form_event(PageIndex(0), FormInputEvent::CopySelection)
            .expect("a copy with no selection is answered");
        assert!(
            nothing
                .selected_text
                .as_ref()
                .is_none_or(|text| text.is_empty()),
            "an empty selection must not report text"
        );

        document
            .form_event(PageIndex(0), FormInputEvent::SelectAll)
            .expect("select-all is accepted");
        let copied = document
            .form_event(PageIndex(0), FormInputEvent::CopySelection)
            .expect("a copy is answered");
        assert_eq!(copied.selected_text.as_deref(), Some("Ada"));

        // …and the paste replaces exactly what was selected.
        document
            .form_event(
                PageIndex(0),
                FormInputEvent::ReplaceSelection {
                    text: "Grace".into(),
                },
            )
            .expect("a replacement is accepted");
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .expect("focus can be dropped");

        assert_eq!(document.field_value("name").unwrap(), "Grace");
    });
}

#[test]
fn a_cut_takes_the_text_out_of_the_field_it_copied_it_from() {
    crate::testkit::on_the_pdfium_thread(|| {
        // A cut is a copy that remembers to remove what it took, and the
        // removal is an empty replacement rather than a run of backspaces: one
        // edit, one keystroke script, one commit.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = plain_form(directory.path()) else {
            return;
        };

        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 76);
        click_into(&mut document, "name");
        for character in "Ada".chars() {
            document
                .form_event(PageIndex(0), FormInputEvent::Char { character })
                .unwrap();
        }
        document
            .form_event(PageIndex(0), FormInputEvent::SelectAll)
            .unwrap();
        let cut = document
            .form_event(PageIndex(0), FormInputEvent::CopySelection)
            .unwrap();
        assert_eq!(cut.selected_text.as_deref(), Some("Ada"));
        document
            .form_event(
                PageIndex(0),
                FormInputEvent::ReplaceSelection {
                    text: String::new(),
                },
            )
            .unwrap();
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap();

        assert_eq!(document.field_value("name").unwrap(), "");
    });
}

#[test]
fn a_space_toggles_the_box_that_holds_the_focus() {
    crate::testkit::on_the_pdfium_thread(|| {
        // What Tab-then-Space has to do, and the reason the application
        // forwards a character rather than synthesising a click: PDFium's own
        // button handler acts on `FORM_OnChar(' ')`, so the toggle, the
        // appearance and the commit are all still the engine's. A click would
        // have to be aimed, and a keyboard has no pointer to aim it with.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = corpus_form(directory.path(), "checkbox-standard") else {
            return;
        };
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 77);
        assert_eq!(document.field_value("agree").unwrap(), "Off");

        // Focused by name, which is what the traversal does — not clicked,
        // because a click is itself a toggle and would prove nothing.
        document
            .form_event(
                PageIndex(0),
                FormInputEvent::FocusField {
                    name: "agree".into(),
                },
            )
            .expect("the box takes the focus");
        document
            .form_event(PageIndex(0), FormInputEvent::Char { character: ' ' })
            .expect("a space is accepted");
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap();

        assert_eq!(document.field_value("agree").unwrap(), "Yes");
    });
}

/// A two-page form, one text field on each page, so a keystroke can be
/// addressed to the wrong one.
fn two_page_form(directory: &std::path::Path) -> Option<PathBuf> {
    use crate::testkit::{stream_body, Page, Pdf};

    let widget = |name: &str, page: u32| {
        format!(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T ({name}) /V () \
             /Rect [100 300 400 330] /P {page} 0 R /F 4 /DA (/Helv 12 Tf 0 g) >>"
        )
    };
    let mut pdf = Pdf::new();
    // 1 catalog, 2 page tree, 3 font, 4 first page, 5 contents.
    for _ in 0..5 {
        pdf.reserve();
    }
    let first = pdf.add(widget("first", 4));
    let second = pdf.reserve();
    let layout = Page::default();
    let back = pdf.add(layout.dictionary(&format!("{second} 0 R"), 5));
    pdf.set(second, widget("second", back));
    pdf.set(
        1,
        format!(
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [{first} 0 R {second} 0 R] \
             /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 3 0 R >> >> >> >>"
        ),
    );
    pdf.set(
        2,
        format!("<< /Type /Pages /Count 2 /Kids [4 0 R {back} 0 R] >>"),
    );
    pdf.set(3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    pdf.set(4, layout.dictionary(&format!("{first} 0 R"), 5));
    pdf.set(
        5,
        stream_body("", b"BT /Helv 12 Tf 72 720 Td (pulpit) Tj ET"),
    );

    let path = directory.join("two-page-form.pdf");
    std::fs::write(&path, pdf.build()).ok()?;
    Some(path)
}

/// A keystroke belongs to the page the *focus* is on, and addressing it to any
/// other page loses it (§8.6).
///
/// The hazard the application's routing exists for: a caret in a field on one
/// page with the pointer resting over the next. Opening another page's form
/// handle runs `FORM_OnBeforeClosePage` on the one being left, which kills the
/// focus and commits the field — so the character arrives at a page where
/// nothing is focused and is simply dropped, and the commit it caused happens
/// underneath the revision and undo bookkeeping.
#[test]
fn a_keystroke_addressed_to_the_wrong_page_is_lost() {
    crate::testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = two_page_form(directory.path()) else {
            return;
        };

        // The focused page takes both characters, which is what the
        // application now does whatever the pointer is over.
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 81);
        document
            .form_event(
                PageIndex(0),
                FormInputEvent::FocusField {
                    name: "first".into(),
                },
            )
            .expect("the field takes the focus");
        for character in "Ab".chars() {
            document
                .form_event(PageIndex(0), FormInputEvent::Char { character })
                .expect("a character is accepted");
        }
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap();
        assert_eq!(document.field_value("first").unwrap(), "Ab");
        drop(document);

        // The same two characters, the second addressed to the page a pointer
        // happened to be over. It never reaches the field.
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 82);
        document
            .form_event(
                PageIndex(0),
                FormInputEvent::FocusField {
                    name: "first".into(),
                },
            )
            .expect("the field takes the focus");
        document
            .form_event(PageIndex(0), FormInputEvent::Char { character: 'A' })
            .expect("a character is accepted");
        document
            .form_event(PageIndex(1), FormInputEvent::Char { character: 'b' })
            .expect("the misaddressed character is accepted and discarded");
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .unwrap();
        assert_ne!(
            document.field_value("first").unwrap(),
            "Ab",
            "a keystroke sent to another page must not be assumed to reach the field"
        );
        assert_eq!(document.field_value("second").unwrap(), "");
    });
}

/// Shift-arrow extends the field's selection, which is what a copy reads.
///
/// The modifier is the whole point: the same arrow without it moves the caret
/// and selects nothing, so a protocol that could not carry shift could not
/// select from the keyboard at all.
#[test]
fn shift_and_an_arrow_extend_the_selection_a_copy_reads() {
    crate::testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = plain_form(directory.path()) else {
            return;
        };

        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 83);
        click_into(&mut document, "name");
        for character in "Ada".chars() {
            document
                .form_event(PageIndex(0), FormInputEvent::Char { character })
                .expect("a character is accepted");
        }

        fn selected(document: &mut PdfDocument<'_>) -> String {
            document
                .form_event(PageIndex(0), FormInputEvent::CopySelection)
                .expect("a copy is answered")
                .selected_text
                .expect("a copy always answers with what was selected")
        }

        // A bare arrow moves the caret and selects nothing.
        document
            .form_event(
                PageIndex(0),
                FormInputEvent::KeyDown {
                    key: FormKey::Left,
                    modifiers: KeyModifiers::NONE,
                },
            )
            .unwrap();
        assert_eq!(selected(&mut document), "");

        // Held with shift, it takes the character it passes over.
        document
            .form_event(
                PageIndex(0),
                FormInputEvent::KeyDown {
                    key: FormKey::Left,
                    modifiers: KeyModifiers::SHIFT,
                },
            )
            .unwrap();
        assert_eq!(
            selected(&mut document),
            "d",
            "shift-arrow must extend the field's selection"
        );
    });
}

/// A one-page form built from field dictionaries, for the cases whose point is
/// what a field *declares* rather than how it is laid out.
///
/// `fields` is one entry per widget, spliced whole so a case can say exactly
/// what it means — a `/V` longer than the read bound, an `/F` that hides the
/// widget, an `/FT` that holds no value at all.
fn form_of(fields: &[String]) -> Vec<u8> {
    use crate::testkit::builder::{Page, Pdf};

    let mut pdf = Pdf::new();
    let catalog = pdf.reserve();
    let pages = pdf.reserve();
    let font = pdf.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let contents = pdf.add_stream("", b"");
    let page = pdf.reserve();
    let numbers: Vec<u32> = fields
        .iter()
        .map(|field| pdf.add(field.replace("{page}", &page.to_string())))
        .collect();
    let refs = numbers
        .iter()
        .map(|number| format!("{number} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    pdf.set(page, Page::default().dictionary(&refs, contents));
    pdf.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    pdf.set(
        catalog,
        format!(
            "<< /Type /Catalog /Pages {pages} 0 R /AcroForm << /Fields [{refs}] \
             /DA (/Helv 10 Tf 0 g) /DR << /Font << /Helv {font} 0 R >> >> >> >>"
        ),
    );
    pdf.build()
}

/// One text field, with whatever `/V` and `/F` the case wants.
fn text_field(name: &str, value: &str, flags: u32) -> String {
    format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T ({name}) /V ({value}) \
         /Rect [50 600 500 700] /F {flags} /DA (/Helv 10 Tf 0 g) /P {{page}} 0 R >>"
    )
}

/// Put one of these in-memory forms where the engine can open it.
fn write_form(directory: &std::path::Path, name: &str, bytes: Vec<u8>) -> PathBuf {
    let path = directory.join(format!("{name}.pdf"));
    std::fs::write(&path, bytes).expect("the form is written");
    path
}

#[test]
fn a_value_longer_than_pulpit_carries_is_cut_and_says_so_rather_than_vanishing() {
    crate::testkit::on_the_pdfium_thread(|| {
        // The bug this holds shut: PDFium's string getters write *nothing*
        // into a buffer smaller than the value they were asked for — they
        // report the length and leave the bytes alone. Reading with a buffer
        // capped at the carrying limit therefore did not truncate a longer
        // value, it erased it: a filled-in comment box came back as the empty
        // string, indistinguishable from a field nobody had touched. Which
        // then told the reader, at Save As, that a required field they had
        // filled in was still empty.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let long = "a".repeat(40_000);
        let path = write_form(
            directory.path(),
            "long",
            form_of(&[text_field("big", &long, 4), text_field("small", "here", 4)]),
        );

        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let document = PdfDocument::new(Box::new(engine), 71);
        let fields = document.fields().expect("the fields are readable");
        let big = fields.iter().find(|f| f.name == "big").expect("the field");

        assert!(
            !big.value.is_empty(),
            "a long value came back empty, which reads as a field nobody filled in"
        );
        assert!(
            big.truncated,
            "the cut has to be reported, not made silently"
        );
        assert!(
            big.value.len() <= 16 * 1024 && big.value.starts_with("aaa"),
            "the value is a prefix of the document's, bounded by the carrying limit"
        );
        // …and it must not be offered as something to edit, because writing
        // the prefix back would throw the rest of it away.
        assert!(
            !big.is_editable(),
            "a value pulpit only half read must not be writable"
        );
        assert!(
            document.field_value("big").unwrap().len() > 1_000,
            "the single-field read has to agree with the listing"
        );

        // The field beside it is untouched by any of this.
        let small = fields
            .iter()
            .find(|f| f.name == "small")
            .expect("the field");
        assert_eq!(small.value, "here");
        assert!(!small.truncated);
        assert!(small.is_editable());
    });
}

#[test]
fn a_hidden_widget_is_listed_and_is_not_somewhere_to_put_the_caret() {
    crate::testkit::on_the_pdfium_thread(|| {
        // `/F` bit 2 is Hidden and bit 6 is NoView; a widget with either set is
        // one no viewer paints. Listing it is right — a field that exists is a
        // fact an inspector may want — and offering it as an editing target is
        // not: tabbing to it scrolls the page to a rectangle with nothing in
        // it and types into something the reader cannot see.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = write_form(
            directory.path(),
            "hidden",
            form_of(&[
                text_field("shown", "", 4),
                text_field("concealed", "", 2),
                text_field("noview", "", 32),
            ]),
        );

        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let document = PdfDocument::new(Box::new(engine), 72);
        let fields = document.fields().expect("the fields are readable");
        let of = |name: &str| {
            fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} was not listed at all"))
        };

        assert!(!of("shown").hidden);
        assert!(of("shown").is_reachable());
        for name in ["concealed", "noview"] {
            let field = of(name);
            assert!(
                field.hidden,
                "{name} is drawn by nothing and was not marked"
            );
            assert!(
                !field.is_reachable(),
                "{name} was offered as somewhere to put the caret"
            );
            // Still *editable* in the engine's sense: the document may mean it
            // to be filled, and a `SetField` for it is not refused. What it is
            // not is reachable on the page.
            assert!(field.is_editable());
        }
    });
}

#[test]
fn a_field_that_holds_no_typed_value_refuses_one_instead_of_swallowing_it() {
    crate::testkit::on_the_pdfium_thread(|| {
        // A push button and a signature field have no value to type into.
        // `FORM_ReplaceSelection` on one of them edits nothing and reports
        // nothing, so the old catch-all path answered "written" and read back
        // the value that was already there — a success for an edit that never
        // happened, and a revision and an undo entry for it.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = write_form(
            directory.path(),
            "buttons",
            form_of(&[
                "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T (press) \
                 /Rect [50 600 200 640] /F 4 /P {page} 0 R >>"
                    .to_string(),
                "<< /Type /Annot /Subtype /Widget /FT /Sig /T (sign) \
                 /Rect [50 500 200 540] /F 4 /P {page} 0 R >>"
                    .to_string(),
                text_field("typed", "", 4),
            ]),
        );

        let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
        let mut document = PdfDocument::new(Box::new(engine), 73);
        for name in ["press", "sign"] {
            let refusal = document.set_field(name, "anything", &[]);
            assert!(
                matches!(
                    refusal,
                    Err(pulpit_render::document::DocumentError::Unsupported(_))
                ),
                "{name} answered {refusal:?} rather than refusing a value it cannot hold"
            );
        }
        // The ordinary field beside them still takes one.
        assert_eq!(document.set_field("typed", "Ada", &[]).unwrap(), "Ada");
    });
}

#[test]
fn one_field_read_by_name_says_what_the_whole_listing_says() {
    crate::testkit::on_the_pdfium_thread(|| {
        // There are two paths to a field now — the listing and the by-name
        // lookup — and the lookup exists because the listing is a walk of the
        // whole document. Two paths to one answer is two answers waiting to
        // happen, and the case that would drift first is a radio group, whose
        // widgets are gathered across pages and whose chosen option is stated
        // on the selected kid rather than on the group.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        // The cases where the two could differ: a group whose widgets are the
        // options, one field drawn on two pages, a multi-select whose chosen
        // rows are not in its value, and two fields sharing a name.
        let interesting = [
            "radio-group",
            "same-widget-on-two-pages",
            "list-box-multi-select",
            "duplicate-field-names",
            "combo-box-export-value-pairs",
        ];
        let mut compared = 0usize;
        for name in interesting {
            let path = corpus_form(directory.path(), name)
                .unwrap_or_else(|| panic!("the corpus no longer carries {name}"));
            let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
            let document = PdfDocument::new(Box::new(engine), 74);
            let listed = document.fields().expect("the fields are readable");
            assert!(!listed.is_empty(), "{name} carries no fields to compare");
            for field in &listed {
                let looked_up = document
                    .field(&field.name)
                    .expect("the lookup answers")
                    .unwrap_or_else(|| panic!("{}: {} was listed and not found", name, field.name));
                assert_eq!(
                    &looked_up, field,
                    "{name}: the by-name lookup disagrees with the listing about {}",
                    field.name
                );
                compared += 1;
            }
            assert!(
                document.field("no-such-field").unwrap().is_none(),
                "a field that is not there is None, not an error"
            );
        }
        assert!(
            compared >= interesting.len(),
            "only {compared} fields were compared, so this proved less than it looks"
        );
    });
}

#[test]
fn a_save_made_around_an_open_caret_still_carries_what_was_typed() {
    crate::testkit::on_the_pdfium_thread(|| {
        // The application drops the focus before Save As and waits for the
        // commit, which is what keeps the *session* consistent — the field
        // list and the undo history know about the value before the file is
        // written. This is the other half: a save reached any other way must
        // still write the right bytes.
        //
        // Uncommitted characters live in the page view PDFium built when the
        // interaction opened, not in `/V`, so a serialisation made around that
        // view writes the value the field had before they were typed. The
        // engine now closes the form page first, which is what commits them.
        // Deliberately *not* sending the focus-loss event that the application
        // sends, because that is the path being checked around.
        let Some(mut guard) = common::pdfium("the form-fill spike") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let Some(path) = plain_form(directory.path()) else {
            panic!("the corpus no longer carries its control case")
        };

        let destination = directory.path().join("saved-mid-caret.pdf");
        {
            let engine = PdfiumDocument::open(&mut guard, &path).expect("the form opens");
            let mut document = PdfDocument::new(Box::new(engine), 75);
            click_into(&mut document, "name").expect("the field takes the caret");
            for character in "Ada".chars() {
                document
                    .form_event(PageIndex(0), FormInputEvent::Char { character })
                    .expect("the keystroke goes in");
            }
            document
                .save_as(&destination, SaveOptions::verified())
                .expect("the copy is written");
        }

        let engine = PdfiumDocument::open(&mut guard, &destination).expect("the copy opens");
        let reopened = PdfDocument::new(Box::new(engine), 76);
        assert_eq!(
            reopened.field_value("name").unwrap(),
            "Ada",
            "the save was made around an open caret and lost what was in it"
        );
    });
}
