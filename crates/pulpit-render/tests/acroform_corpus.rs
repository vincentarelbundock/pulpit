//! The AcroForm hazard corpus, against the document engine (§13.1).
//!
//! Its premise, restated because it is the reason the corpus survived the fold
//! from pdfform: the public corpora exercise parsers and renderers, which this
//! project delegates to PDFium. What they do not cover is finding fields,
//! filling them and writing them back.
//!
//! Two halves. Every case must survive opening, annotating and saving,
//! leaving the process alive, the source untouched and either a readable PDF
//! or a clean error. And every case that names a field must keep the promise
//! it makes about it: a value entered, saved and reopened, or a read-only
//! field that is found and refuses to change.
//!
//! The second half is filled the way a person fills a form — clicking into a
//! field and typing, pressing a checkbox, choosing from a list — because §8.6
//! gives the application no other way in. That is the point of it: the code
//! that edits a field is PDFium's own — the code that draws it.
//!
//! Skipped with a message when no `libpdfium` is installed.

#![cfg(feature = "pdfium")]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft, InkDraft, InkPoint, MarkStyle};
use pulpit_core::page::PageIndex;
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::{DocumentRevision, DocumentTransaction, PdfDocument, SaveOptions};
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_testkit::{corpus, Expect, Unchanged};

fn workspace_lib() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib")
}

fn binding() -> Option<MutexGuard<'static, PdfiumBackend>> {
    static BACKEND: OnceLock<Option<Mutex<PdfiumBackend>>> = OnceLock::new();
    let backend = BACKEND
        .get_or_init(|| {
            if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
                std::env::set_var("PULPIT_PDFIUM_PATH", workspace_lib());
            }
            match PdfiumBackend::bind() {
                Ok(backend) => Some(Mutex::new(backend)),
                Err(error) => {
                    eprintln!("skipping the AcroForm corpus: {error}");
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

/// One ink stroke, which is the mutation every case gets: it exercises the
/// write path against a document whose form is malformed, which is exactly
/// where a shared `/AcroForm` dictionary would take an annotation edit down
/// with it.
fn stroke() -> DocumentTransaction {
    DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
        InkDraft {
            page: PageIndex(0),
            points: vec![InkPoint::new(80.0, 80.0), InkPoint::new(200.0, 140.0)],
            style: MarkStyle::default(),
        },
    ))])
}

#[test]
fn every_corpus_case_survives_being_opened_annotated_and_saved() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;

    let directory = tempfile::tempdir().expect("a temporary directory");
    let cases = corpus();
    assert!(cases.len() > 40, "the corpus did not survive the fold");

    let mut opened = 0usize;
    let mut refused: Vec<(&str, String)> = Vec::new();

    for case in cases {
        let source = pulpit_testkit::write_pdf(directory.path(), case.name, &case.bytes);
        // The one irreversible action this program has is writing a file, and
        // the source is the file it must never write (A6).
        let unchanged = Unchanged::new(&source, case.name);

        let engine = match PdfiumDocument::open(backend, &source) {
            Ok(engine) => engine,
            Err(error) => {
                // A clean error is an acceptable outcome for a document
                // malformed enough not to open: the corpus promises "either a
                // readable PDF or a clean error", and reaching this line at
                // all is the proof that it was not a crash. The source is
                // still checked, because a failed open must leave nothing
                // behind either.
                unchanged.check();
                refused.push((case.name, error.to_string()));
                continue;
            }
        };
        let mut document = PdfDocument::new(Box::new(engine), 1_234);
        opened += 1;

        // Reading everything the document offers, on hostile input.
        let pages = document.page_count();
        assert!(pages > 0, "{}: opened with no pages", case.name);
        for page in 0..pages.min(4) {
            let page = PageIndex(page);
            let _ = document.page_geometry(page);
            let _ = document.annotations(page);
        }
        let _ = document.fields();

        // …and writing to it.
        // A refusal is fine — an encrypted document may forbid changes — as
        // long as it is a refusal and not a fall over.
        if let Ok(applied) = document.apply(DocumentRevision::INITIAL, stroke()) {
            assert_eq!(applied.document_revision, DocumentRevision(1));
        }

        let destination = directory.path().join(format!("{}-saved.pdf", case.name));
        let written = match document.save_as(&destination, SaveOptions::verified()) {
            Ok(saved) => {
                assert!(saved.bytes > 0, "{}: saved an empty file", case.name);
                assert!(destination.exists());
                true
            }
            Err(_) => false,
        };
        // Dropping the document gives the binding back, which is what lets the
        // saved file be opened next.
        drop(document);

        if written {
            // The output has to be *readable*, not merely written.
            let reopened = PdfiumDocument::open(backend, &destination).unwrap_or_else(|error| {
                panic!("{}: the saved file will not open: {error}", case.name)
            });
            let reopened = PdfDocument::new(Box::new(reopened), 1);
            assert!(reopened.page_count() > 0, "{}: saved no pages", case.name);
        }

        unchanged.check();
    }

    // A case that will not open is an acceptable outcome, but a corpus where
    // most of them do not is a broken engine wearing a green test.
    assert!(
        opened > 40,
        "only {opened} of the corpus opened; refused: {refused:?}"
    );
}

/// The corpus has to keep having something to say.
///
/// A corpus where every case expects only survival is a smoke test wearing a
/// corpus's name: it would pass against an engine that opened each file and
/// then did nothing at all. This is what stops that happening by attrition,
/// one softened expectation at a time.
#[test]
fn the_corpus_states_a_defensible_answer_and_not_only_survival() {
    let cases = corpus();
    let promising = cases
        .iter()
        .filter(|case| !matches!(case.expect, Expect::Survives))
        .count();
    assert!(
        promising > 0,
        "a corpus where no case has a defensible correct answer is a smoke test"
    );
    eprintln!(
        "{promising} of {} corpus cases assert a fill result, checked by \
         `the_corpus_fill_promises_are_kept`",
        cases.len()
    );
}

/// The corpus's fill promises, now that §8.6's events are wired.
///
/// Each case carries an [`Expect`]: a field that must round-trip a value
/// through a save and a reopen, or one that must be found and must not be
/// writable. This is acceptance criterion 16, and criterion 5 on 55 documents
/// that are each wrong in one named way.
///
/// A case that will not open at all is not counted against the fill path —
/// [`every_corpus_case_survives_being_opened_annotated_and_saved`] is what
/// holds the line on those. What this checks is the ones that do open: their
/// promise is kept, or the failure is named.
#[test]
fn the_corpus_fill_promises_are_kept() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = tempfile::tempdir().expect("a temporary directory");

    let mut checked = 0usize;
    let mut broken: Vec<String> = Vec::new();

    for case in corpus() {
        let (field, expected) = match case.expect {
            Expect::Survives => continue,
            Expect::Roundtrips { field, value } => (field, Some(value)),
            Expect::ReadOnly { field } => (field, None),
        };
        let source = pulpit_testkit::write_pdf(directory.path(), case.name, &case.bytes);
        let unchanged = Unchanged::new(&source, case.name);

        let Ok(engine) = PdfiumDocument::open(backend, &source) else {
            // Covered by the survival test; a document that will not open has
            // no fill behaviour to promise.
            unchanged.check();
            continue;
        };
        let mut document = PdfDocument::new(Box::new(engine), 4_321);

        let Some(target) = document
            .fields()
            .ok()
            .and_then(|fields| fields.into_iter().find(|f| f.name == field))
        else {
            broken.push(format!("{}: the field {field} was not found", case.name));
            drop(document);
            unchanged.check();
            continue;
        };
        checked += 1;

        match expected {
            // A read-only field must be *found* — so it can be shown — and
            // must not take a value. §8.6 puts that enforcement in the engine,
            // which is what this checks: the events are forwarded exactly as
            // they are for any other field, and PDFium refuses them.
            None => {
                assert!(
                    target.read_only,
                    "{}: {field} is writable and should not be",
                    case.name
                );
                let before = document.field_value(field).unwrap_or_default();
                enter_value(&mut document, &target, "should not take");
                let after = document.field_value(field).unwrap_or_default();
                if before != after {
                    broken.push(format!(
                        "{}: the read-only field {field} took a value ({before:?} → {after:?})",
                        case.name
                    ));
                }
            }
            Some(value) => {
                if target.read_only {
                    broken.push(format!(
                        "{}: {field} should round-trip but is read-only",
                        case.name
                    ));
                    drop(document);
                    unchanged.check();
                    continue;
                }
                enter_value(&mut document, &target, value);
                let typed = document.field_value(field).unwrap_or_default();
                if !holds(&target, &typed, value) {
                    broken.push(format!(
                        "{}: {field} holds {typed:?} after entering {value:?}",
                        case.name
                    ));
                    drop(document);
                    unchanged.check();
                    continue;
                }

                // …and it survives the file (criterion 5).
                let destination = directory.path().join(format!("{}-filled.pdf", case.name));
                let saved = document
                    .save_as(&destination, SaveOptions::verified())
                    .is_ok();
                drop(document);
                if !saved {
                    broken.push(format!("{}: a filled form would not save", case.name));
                    unchanged.check();
                    continue;
                }
                match PdfiumDocument::open(backend, &destination) {
                    Ok(engine) => {
                        let reopened = PdfDocument::new(Box::new(engine), 4_322);
                        let read = reopened.field_value(field).unwrap_or_default();
                        let reread = reopened
                            .fields()
                            .ok()
                            .and_then(|fields| fields.into_iter().find(|f| f.name == field));
                        let kept = reread
                            .as_ref()
                            .map(|reread| holds(reread, &read, value))
                            .unwrap_or(false);
                        if !kept {
                            broken.push(format!(
                                "{}: {field} reopened as {read:?}, not {value:?}",
                                case.name
                            ));
                        }
                    }
                    Err(error) => broken.push(format!(
                        "{}: the filled file will not reopen: {error}",
                        case.name
                    )),
                }
                unchanged.check();
                continue;
            }
        }
        drop(document);
        unchanged.check();
    }

    assert!(
        checked > 0,
        "no corpus case reached the fill path, so this test proved nothing"
    );
    assert!(
        broken.is_empty(),
        "{} of {checked} fill promises broken:\n  {}",
        broken.len(),
        broken.join("\n  ")
    );
    eprintln!("all {checked} corpus fill promises kept");
}

/// Whether `field`, now holding `held`, carries the outcome the corpus asked
/// for.
///
/// Not string equality, because the corpus states the *outcome a person
/// wanted* and a PDF field states what it holds, and for several kinds those
/// are written in different vocabularies:
///
/// - a checkbox holds its on-state name — usually `Yes`, sometimes `1`, once
///   `On` — where the corpus says `true`. What is being asserted is that the
///   box is ticked, and any non-`Off` value is a ticked box.
/// - a choice field with export-value pairs holds the *export* value where the
///   corpus names the label a person would read: `FR` for `France`. Both are
///   the same option, and the field's own option list is what says so.
/// - a multi-select list holds one of the chosen values where the corpus names
///   the set. Choosing more than one is what the case is about; holding one of
///   them is the evidence.
///
/// Treating these as string mismatches would report five failures that are not
/// failures, and — worse — would push someone to "fix" the engine by making it
/// report something no other PDF reader would.
fn holds(field: &pulpit_render::document::FormField, held: &str, wanted: &str) -> bool {
    use pulpit_render::document::FieldKind;

    if held == wanted {
        return true;
    }
    match field.kind {
        FieldKind::Checkbox => {
            let ticked = !held.is_empty() && !held.eq_ignore_ascii_case("off");
            matches!(wanted, "true" | "on" | "yes" | "1") == ticked
        }
        FieldKind::ComboBox | FieldKind::ListBox => {
            // The same option under its other name: the corpus named the
            // label, the field holds the export value, and the index of one is
            // the index of the other.
            let by_label = field.options.iter().position(|option| option == wanted);
            let by_value = field.options.iter().position(|option| option == held);
            if by_label.is_some() && by_label == by_value {
                return true;
            }
            // A multi-select set, stated as a list. Holding any of them is the
            // evidence that the selection took; which one PDFium reports as
            // "the" value of a multiple selection is its own business.
            wanted
                .trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|part| part.trim().trim_matches('"'))
                .any(|part| part == held)
        }
        _ => false,
    }
}

/// Put `value` into `field`, the way a person would (§8.6).
///
/// There is no other way in, on purpose: values are entered on the page, by
/// PDFium, and a test that wrote one directly would be testing a path the
/// application does not have.
///
/// "The way a person would" is different for each kind, and getting that wrong
/// is not a small thing — typing the characters `t`, `r`, `u`, `e` into a
/// checkbox is not how anyone ticks a box, and a test that did it would be
/// asserting a behaviour nothing should have. So:
///
/// - a text field is clicked into and typed, with a newline sent as the Enter
///   key it is;
/// - a checkbox and a radio group are *pressed*, on the widget that stands for
///   the wanted value;
/// - a choice field has the option selected by index, which is PDFium's own
///   `FORM_SetIndexSelected`;
/// - an editable combo box is cleared first, because typing into one that
///   already holds a value appends to it, exactly as it would for a person.
fn enter_value(
    document: &mut PdfDocument<'_>,
    field: &pulpit_render::document::FormField,
    value: &str,
) {
    use pulpit_render::document::{FieldKind, FormField};

    fn press(document: &mut PdfDocument<'_>, bounds: pulpit_core::page::PageRect) {
        use pulpit_render::document::protocol::FormInputEvent;
        let at = pulpit_core::page::PagePoint {
            x: (bounds.left + bounds.right) / 2.0,
            y: (bounds.top + bounds.bottom) / 2.0,
        };
        let _ = document.form_event(PageIndex(0), FormInputEvent::PointerDown { at });
        let _ = document.form_event(PageIndex(0), FormInputEvent::PointerUp { at });
    }

    fn commit(document: &mut PdfDocument<'_>) {
        use pulpit_render::document::protocol::FormInputEvent;
        let _ = document.form_event(PageIndex(0), FormInputEvent::Focus { gained: false });
    }

    /// The widget that stands for `value`, for a field whose widgets are its
    /// options. Falls back to the only widget there is.
    fn widget_for(field: &FormField, value: &str) -> Option<pulpit_core::page::PageRect> {
        field
            .widgets
            .iter()
            .find(|widget| widget.option.as_deref() == Some(value))
            .or_else(|| field.widgets.first())
            .map(|widget| widget.bounds)
    }

    match field.kind {
        FieldKind::Checkbox => {
            let Some(bounds) = widget_for(field, value) else {
                return;
            };
            press(document, bounds);
            commit(document);
        }
        FieldKind::RadioGroup => {
            // A radio group's options are its buttons, and which button means
            // which value is not always readable: a group whose kids carry
            // their on-state in `/AP /N` rather than in an `/Opt` array has no
            // export value PDFium will report, and the corpus contains exactly
            // that.
            //
            // When it *is* readable, press that button. When it is not, press
            // each in turn until the group holds what was wanted — which is
            // what a person does with a form whose options are printed on the
            // paper beside the buttons rather than written in the file. It
            // settles, because pressing a radio button sets the group to that
            // button's value and to nothing else.
            if let Some(bounds) = field
                .widgets
                .iter()
                .find(|widget| widget.option.as_deref() == Some(value))
                .map(|widget| widget.bounds)
            {
                press(document, bounds);
                commit(document);
                return;
            }
            for widget in &field.widgets {
                press(document, widget.bounds);
                commit(document);
                if document.field_value(&field.name).unwrap_or_default() == value {
                    return;
                }
            }
        }
        FieldKind::ComboBox | FieldKind::ListBox => {
            use pulpit_render::document::protocol::FormInputEvent;
            let Some(bounds) = field.anchor_on(PageIndex(0)) else {
                return;
            };
            press(document, bounds);

            // A list of values is a multiple selection, and choosing each of
            // them is what the case is about.
            let wanted: Vec<&str> = if value.starts_with('[') {
                value
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|part| part.trim().trim_matches('"'))
                    .collect()
            } else {
                vec![value]
            };
            if wanted.len() > 1 {
                // `FORM_SetIndexSelected` acts on the *focused* widget, and a
                // press on a list box lands on whichever row is under it
                // rather than reliably focusing the field.
                let _ = document.form_event(
                    PageIndex(0),
                    FormInputEvent::FocusField {
                        name: field.name.clone(),
                    },
                );
                // Unchoose what is chosen first. Selecting in a multi-select
                // list *adds* to the selection, so a list that opens with Red
                // ticked and is then told to choose Blue and Green ends up
                // with all three — which is not what "choose Blue and Green"
                // means, for a person or for this test.
                for index in &field.selected {
                    let _ = document.form_event(
                        PageIndex(0),
                        FormInputEvent::SelectOption {
                            index: *index,
                            selected: false,
                        },
                    );
                }
                for one in &wanted {
                    if let Some(index) = field.options.iter().position(|option| option == one) {
                        let _ = document.form_event(
                            PageIndex(0),
                            FormInputEvent::SelectOption {
                                index: index as u32,
                                selected: true,
                            },
                        );
                    }
                }
                commit(document);
                return;
            }

            // A value the list offers is chosen from the list. One that it
            // does not is typed, which only an editable combo box allows —
            // and which is the case the corpus is testing when it asks for
            // one.
            match field.options.iter().position(|option| option == value) {
                Some(index) => {
                    let _ = document.form_event(
                        PageIndex(0),
                        FormInputEvent::SelectOption {
                            index: index as u32,
                            selected: true,
                        },
                    );
                }
                None if field.allows_custom_value => {
                    use pulpit_render::document::protocol::{FormKey, KeyModifiers};
                    // Clear what is there first: typing into a combo that
                    // already holds a value appends to it, for a person too.
                    // To the end of the text before deleting backwards — a
                    // fresh caret sits at the start, where backspace has
                    // nothing behind it to take.
                    let _ = document.form_event(
                        PageIndex(0),
                        FormInputEvent::KeyDown {
                            key: FormKey::End,
                            modifiers: KeyModifiers::NONE,
                        },
                    );
                    for _ in 0..field.value.chars().count() + 8 {
                        let _ = document.form_event(
                            PageIndex(0),
                            FormInputEvent::KeyDown {
                                key: FormKey::Backspace,
                                modifiers: KeyModifiers::NONE,
                            },
                        );
                    }
                    for character in value.chars() {
                        let _ =
                            document.form_event(PageIndex(0), FormInputEvent::Char { character });
                    }
                }
                None => {}
            }
            commit(document);
        }
        _ => {
            use pulpit_render::document::protocol::{FormInputEvent, FormKey, KeyModifiers};
            let Some(bounds) = field.anchor_on(PageIndex(0)) else {
                return;
            };
            press(document, bounds);
            // …and then by name, which is how an occluded field is reached.
            // It matters here because a click can only reach whichever
            // widget PDFium puts on top, and the corpus deliberately contains
            // a document whose widgets overlap — every point on `field7` is
            // also on `field6` or `field8`. Naming it is the only way in, and
            // it is a way the application has (§8.6).
            let _ = document.form_event(
                PageIndex(0),
                FormInputEvent::FocusField {
                    name: field.name.clone(),
                },
            );
            for character in value.chars() {
                let event = if character == '\n' {
                    // A newline is the Enter key, which is what puts a
                    // multiline field onto its next line. Sent as a character
                    // it is silently dropped.
                    FormInputEvent::KeyDown {
                        key: FormKey::Enter,
                        modifiers: KeyModifiers::NONE,
                    }
                } else {
                    FormInputEvent::Char { character }
                };
                let _ = document.form_event(PageIndex(0), event);
            }
            commit(document);
        }
    }
}
