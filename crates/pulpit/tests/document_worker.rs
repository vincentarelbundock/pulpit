//! The document worker, end to end, as a real child process.
//!
//! Everything else about document mode is tested in-process: the engine
//! against a memory backend, the PDFium engine against a real PDF, the worker
//! loop over a pipe made of byte vectors. This is the one test that starts an
//! actual `--document-worker=FILE` process, talks to it over its stdin and
//! stdout, and checks that an annotation committed on the far side of the
//! boundary is in the file afterwards.
//!
//! It lives in `pulpit` rather than `pulpit-render` because the worker is a
//! role of *this* binary (§5.1), and `std::env::current_exe()` from a test in
//! this crate is the test binary rather than pulpit — so the test names the
//! built executable explicitly, and skips when there is not one to name.

use std::path::PathBuf;

use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft, InkDraft, InkPoint, MarkStyle};
use pulpit_core::page::PageIndex;
use pulpit_render::document::protocol::{
    DocumentRenderRequest, DocumentRequest, DocumentResponse, SaveRequest,
};
use pulpit_render::document::session::{DocumentSession, DocumentWorkerCommand, SessionError};
use pulpit_render::document::{DocumentRevision, DocumentTransaction, SaveOptions};

/// The pulpit executable this test run built, beside the test binary itself.
///
/// `cargo test` puts integration-test binaries in `target/<profile>/deps` and
/// the executable in `target/<profile>`, so the parent of the test binary's
/// directory is where to look. Absent under `cargo build --tests`, which is
/// why this skips rather than fails.
fn executable() -> Option<PathBuf> {
    let test_binary = std::env::current_exe().ok()?;
    let candidate = test_binary.parent()?.parent()?.join(if cfg!(windows) {
        "pulpit.exe"
    } else {
        "pulpit"
    });
    candidate.is_file().then_some(candidate)
}

fn command(source: &std::path::Path) -> Option<DocumentWorkerCommand> {
    let program = executable()?;
    // The child searches for PDFium relative to its own executable and the
    // working directory, and a test's working directory is the crate rather
    // than the workspace. Pointing it at the checked-out library is what the
    // dev shell does for a real run; the child inherits it.
    if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
        let lib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib");
        std::env::set_var("PULPIT_PDFIUM_PATH", lib);
    }
    Some(DocumentWorkerCommand::Explicit {
        program,
        args: vec![format!("--document-worker={}", source.display())],
    })
}

fn stroke() -> DocumentTransaction {
    DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
        InkDraft {
            page: PageIndex(0),
            points: vec![
                InkPoint::new(72.0, 72.0),
                InkPoint::new(180.0, 140.0),
                InkPoint::new(300.0, 96.0),
            ],
            style: MarkStyle::default(),
        },
    ))])
}

#[test]
fn a_mark_committed_across_the_process_boundary_is_in_the_saved_file() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("source.pdf");
    if pulpit_render::pdf::synth::write_pdf(&source, 2, None).is_err() {
        eprintln!("skipping: cannot write a synthetic PDF");
        return;
    }
    let Some(command) = command(&source) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };

    let mut session = match DocumentSession::start(&command, &source) {
        Ok(session) => session,
        Err(error) => {
            // The worker exits with guidance when it cannot bind PDFium,
            // which on a machine without one is the honest outcome and not a
            // failure of this test.
            eprintln!("skipping the document worker test: {error}");
            return;
        }
    };
    assert_eq!(session.source(), source);

    // What the worker holds, which is what a reader needs before it can lay
    // anything out: how many pages, and how big each of them is.
    let DocumentResponse::Opened(info) = session
        .request(DocumentRequest::Info)
        .expect("the worker describes its document")
    else {
        panic!("expected document info")
    };
    assert_eq!(info.page_count, 2);
    assert!(info.first_page.is_valid());

    let DocumentResponse::PageGeometries(pages) = session
        .request(DocumentRequest::PageGeometries {
            from: PageIndex(0),
            // More than the document has: a run past the end is the tail of
            // the document, not an error.
            count: 64,
        })
        .expect("the worker measures its pages")
    else {
        panic!("expected page geometries")
    };
    assert_eq!(pages.len(), 2);
    assert!(pages.iter().all(|page| page.is_valid()));

    // Nothing on the page to begin with.
    let response = session
        .request(DocumentRequest::ListAnnotations { page: PageIndex(0) })
        .expect("the worker answers");
    let DocumentResponse::Annotations(annotations) = response else {
        panic!("expected an annotation list, got {response:?}")
    };
    assert!(annotations.is_empty());

    // A mark, committed on the far side of the boundary.
    let response = session
        .request(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(),
        })
        .expect("the stroke commits");
    let DocumentResponse::Applied(applied) = response else {
        panic!("expected an applied transaction, got {response:?}")
    };
    assert_eq!(applied.document_revision, DocumentRevision(1));
    assert_eq!(applied.dirty_pages, vec![PageIndex(0)]);

    // …and it is there when the worker is asked again.
    let response = session
        .request(DocumentRequest::ListAnnotations { page: PageIndex(0) })
        .unwrap();
    let DocumentResponse::Annotations(annotations) = response else {
        panic!("expected an annotation list")
    };
    assert_eq!(annotations.len(), 1);
    let id = annotations[0].id.clone();

    // A frame, from the process that holds the mutated document — which is
    // the only one that can promise it contains the commit (A7).
    let DocumentResponse::Frame(frame) = session
        .request(DocumentRequest::Render(DocumentRenderRequest {
            page: PageIndex(0),
            width: 200,
            height: 260,
            region: pulpit_core::notes::Region::FULL,
            full_width: 0,
            full_height: 0,
        }))
        .expect("the page renders")
    else {
        panic!("expected a frame")
    };
    assert!(frame.is_consistent());
    assert_eq!(frame.revision, DocumentRevision(1));
    assert!(
        frame.pixels.iter().any(|byte| *byte != 0),
        "the frame is entirely blank"
    );

    // A stale revision is refused across the wire exactly as it is in
    // process: a delayed message must not overwrite a later change (A7).
    let stale = session.request(DocumentRequest::Apply {
        expected_revision: DocumentRevision::INITIAL,
        transaction: stroke(),
    });
    match stale {
        Err(SessionError::Refused(failure)) => assert!(!failure.is_retryable()),
        other => panic!("expected a revision conflict, got {other:?}"),
    }

    // Save As, and the file it wrote has the mark in it.
    let destination = directory.path().join("annotated.pdf");
    let response = session
        .request(DocumentRequest::SaveAs(SaveRequest {
            destination: destination.clone(),
            options: SaveOptions::verified(),
        }))
        .expect("the save succeeds");
    let DocumentResponse::Saved(saved) = response else {
        panic!("expected a save, got {response:?}")
    };
    assert_eq!(saved.revision, DocumentRevision(1));
    assert!(destination.is_file());
    assert!(saved.bytes > 0);

    session.close();

    // A6, checked from outside every process that touched it: the source is
    // byte-identical to what was written before any of this happened.
    let reopened = std::fs::read(&destination).unwrap();
    assert!(reopened.starts_with(b"%PDF"), "the output is not a PDF");
    assert!(
        reopened.len() > std::fs::read(&source).unwrap().len(),
        "the annotated copy should carry more than the source"
    );

    // The identity the worker minted is pulpit's own, which is what lets a
    // later session find the annotation again (A3).
    assert!(id.looks_generated(), "{id}");
}

#[test]
fn a_worker_that_cannot_open_its_document_reports_it_rather_than_hanging() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let missing = directory.path().join("not-a-pdf.pdf");
    std::fs::write(&missing, b"this is not a PDF at all").unwrap();
    let Some(command) = command(&missing) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };

    // The worker exits before the handshake, so starting the session fails —
    // which is the point: the supervisor learns immediately instead of
    // waiting on a pipe that will never carry an answer.
    match DocumentSession::start(&command, &missing) {
        Err(error) => assert!(error.is_worker_loss(), "{error}"),
        Ok(_) => panic!("a worker opened a file that is not a PDF"),
    }
}

/// A one-page AcroForm with a single text field named `name`.
///
/// Written by hand for the same reason the JavaScript fixtures in
/// `pulpit-render` are: the interesting part is four lines of PDF, and a
/// binary blob in the tree would hide them.
fn form_pdf() -> Vec<u8> {
    let objects: [&str; 5] = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] \
         /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V () /Ff 0 \
         /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R >>",
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

/// Typing into a field, across the process boundary, ends up in the file.
///
/// The companion to the ink test above, and the one that covers what the
/// application actually does when someone clicks a form field: the events it
/// forwards are raw input, the value is composed by PDFium on the far side,
/// and the proof is that reopening the saved file finds the characters.
#[test]
fn a_field_typed_across_the_process_boundary_is_in_the_saved_file() {
    use pulpit_core::page::PagePoint;
    use pulpit_render::document::protocol::{FormInputEvent, FormKey, KeyModifiers};

    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("form.pdf");
    std::fs::write(&source, form_pdf()).expect("the fixture is written");
    let Some(command) = command(&source) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };
    let mut session = match DocumentSession::start(&command, &source) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("skipping the document worker test: {error}");
            return;
        }
    };

    let DocumentResponse::Opened(info) = session
        .request(DocumentRequest::Info)
        .expect("the worker describes its document")
    else {
        panic!("expected document info")
    };
    assert!(
        info.has_form,
        "the fixture is an AcroForm; if this is false the form-fill environment \
         did not start and every field would silently be read-only"
    );

    // The centre of the widget, in page space — which measures down from the
    // top, where the `/Rect` measures up from the bottom.
    let at = PagePoint {
        x: 200.0,
        y: 792.0 - 715.0,
    };
    let page = PageIndex(0);
    let mut focused = false;
    for event in [
        FormInputEvent::PointerDown { at },
        FormInputEvent::PointerUp { at },
    ] {
        let DocumentResponse::Form(result) = session
            .request(DocumentRequest::FormEvent { page, event })
            .expect("the worker takes a pointer event")
        else {
            panic!("expected a form event result")
        };
        focused |= result.text_focus;
    }
    assert!(
        focused,
        "a click in the field must report the caret; without it the application \
         cannot tell a letter from a shortcut"
    );

    for character in "Ada".chars() {
        session
            .request(DocumentRequest::FormEvent {
                page,
                event: FormInputEvent::Char { character },
            })
            .expect("the worker takes a character");
    }
    // A typo, taken back the way a person would take it back: backspace is a
    // *character* to PDFium's environment, not a key event, and sending it as
    // the latter deletes nothing at all.
    session
        .request(DocumentRequest::FormEvent {
            page,
            event: FormInputEvent::KeyDown {
                key: FormKey::Backspace,
                modifiers: KeyModifiers::NONE,
            },
        })
        .expect("the worker takes a backspace");

    // Losing focus commits the value, and the answer says so.
    let DocumentResponse::Form(result) = session
        .request(DocumentRequest::FormEvent {
            page,
            event: FormInputEvent::Focus { gained: false },
        })
        .expect("the worker takes a focus change")
    else {
        panic!("expected a form event result")
    };
    let committed = result
        .committed
        .expect("losing focus commits the field that was being typed into");
    assert_eq!(committed.name, "name");
    assert_eq!(committed.value, "Ad", "the backspace took the 'a' back");
    assert!(!result.text_focus, "the caret left the field");

    // …and the file that comes out of it holds what was typed.
    let destination = directory.path().join("filled.pdf");
    session
        .request(DocumentRequest::SaveAs(SaveRequest {
            destination: destination.clone(),
            options: SaveOptions {
                incremental: false,
                verify: true,
            },
        }))
        .expect("the worker saves the filled form");
    let bytes = std::fs::read(&destination).expect("the saved form is readable");
    assert!(
        bytes.windows(2).any(|pair| pair == b"Ad"),
        "the typed value is not in the saved file"
    );
    // A6: the source is never written.
    assert_eq!(
        std::fs::read(&source).expect("the source is readable"),
        form_pdf(),
        "the source file must be untouched"
    );
}

/// Undoing a filled field puts the old value back, across the process
/// boundary and through PDFium's own editor.
///
/// The half of §8.6 that was missing until an inverse existed: a field edit
/// used to be the one mutation with no undo, so pressing undo after typing a
/// value reached straight past it to the last annotation edit.
#[test]
fn a_filled_field_can_be_undone_and_redone_across_the_boundary() {
    use pulpit_core::page::PagePoint;
    use pulpit_render::document::protocol::FormInputEvent;
    use pulpit_render::document::{DocumentUndo, UndoOperation};

    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("form.pdf");
    std::fs::write(&source, form_pdf()).expect("the fixture is written");
    let Some(command) = command(&source) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };
    let mut session = match DocumentSession::start(&command, &source) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("skipping the document worker test: {error}");
            return;
        }
    };

    let value_now = |session: &mut DocumentSession| -> String {
        let DocumentResponse::Fields(fields) = session
            .request(DocumentRequest::ListFields)
            .expect("the worker lists its fields")
        else {
            panic!("expected a field list")
        };
        fields
            .into_iter()
            .find(|field| field.name == "name")
            .expect("the fixture has a field called name")
            .value
    };

    let page = PageIndex(0);
    let at = PagePoint {
        x: 200.0,
        y: 792.0 - 715.0,
    };
    for event in [
        FormInputEvent::PointerDown { at },
        FormInputEvent::PointerUp { at },
    ] {
        session
            .request(DocumentRequest::FormEvent { page, event })
            .expect("the worker takes a pointer event");
    }
    for character in "Ada".chars() {
        session
            .request(DocumentRequest::FormEvent {
                page,
                event: FormInputEvent::Char { character },
            })
            .expect("the worker takes a character");
    }
    let DocumentResponse::Form(result) = session
        .request(DocumentRequest::FormEvent {
            page,
            event: FormInputEvent::Focus { gained: false },
        })
        .expect("the worker takes a focus change")
    else {
        panic!("expected a form event result")
    };
    let committed = result.committed.expect("the field committed");
    assert_eq!(committed.value, "Ada");
    assert_eq!(
        committed.previous, "",
        "the before-image is what makes the edit reversible"
    );
    assert_eq!(value_now(&mut session), "Ada");

    // The inverse the application would have built from that commit.
    let undo = DocumentUndo {
        operations: vec![UndoOperation::SetField {
            name: committed.name.clone(),
            value: committed.previous.clone(),
            selected: committed.previous_selected.clone(),
        }],
        restores: DocumentRevision::INITIAL,
        label: format!("Fill {}", committed.name),
    };
    let DocumentResponse::Applied(applied) = session
        .request(DocumentRequest::Undo {
            expected_revision: committed.revision,
            operation: undo,
        })
        .expect("the worker undoes the fill")
    else {
        panic!("expected an applied undo")
    };
    assert_eq!(
        value_now(&mut session),
        "",
        "undo did not put the field back the way it was"
    );

    // …and the answer to an undo redoes it, which is what makes redo need no
    // request of its own.
    session
        .request(DocumentRequest::Undo {
            expected_revision: applied.document_revision,
            operation: applied.undo.clone(),
        })
        .expect("the worker redoes the fill");
    assert_eq!(
        value_now(&mut session),
        "Ada",
        "redo did not put the typed value back"
    );
}

/// A date chosen from the calendar reaches the file, written the way the
/// field's own pattern asks for.
///
/// The picker is pulpit's — a PDF names a date field and its pattern and
/// offers no calendar — but what it produces is text, and it goes into the
/// field through PDFium's own editor as a `SetField`, which is the same path
/// an undo takes. So a picked date is an ordinary edit: one revision, one undo
/// entry, and the field's own format script run over it.
#[test]
fn a_date_picked_from_the_calendar_lands_in_the_field_in_its_own_pattern() {
    use pulpit_render::document::{DocumentCommand, DocumentTransaction};

    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("dates.pdf");
    std::fs::write(&source, dated_pdf()).expect("the fixture is written");
    let Some(command) = command(&source) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };
    let mut session = match DocumentSession::start(&command, &source) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("skipping the document worker test: {error}");
            return;
        }
    };

    // What the picker would produce for the 16th of August 2026 in a field
    // whose pattern is `dd mmmm yyyy`, in French.
    let value = "16 août 2026";
    let DocumentResponse::Applied(applied) = session
        .request(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: DocumentTransaction::one(DocumentCommand::SetField {
                name: "when".into(),
                value: value.into(),
                selected: Vec::new(),
            }),
        })
        .expect("the worker takes the picked date")
    else {
        panic!("expected an applied transaction")
    };
    assert!(applied.document_revision > DocumentRevision::INITIAL);

    let DocumentResponse::Fields(fields) = session
        .request(DocumentRequest::ListFields)
        .expect("the worker lists its fields")
    else {
        panic!("expected a field list")
    };
    let field = fields
        .into_iter()
        .find(|field| field.name == "when")
        .expect("the date field");
    assert_eq!(
        field.value, value,
        "the picked date is not what the field holds; the accented month name \
         is the part most likely to have been mangled on the way through"
    );

    // …and it survives the file.
    let destination = directory.path().join("filled.pdf");
    session
        .request(DocumentRequest::SaveAs(SaveRequest {
            destination: destination.clone(),
            options: SaveOptions {
                incremental: false,
                verify: true,
            },
        }))
        .expect("the worker saves the filled form");
    assert!(destination.is_file());
}

/// One page, one text field whose format script makes it a date.
fn dated_pdf() -> Vec<u8> {
    let objects: [&str; 5] = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] \
         /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (when) /V () /Ff 0 \
         /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R \
         /AA << /F << /S /JavaScript /JS (AFDate_FormatEx(\"dd mmmm yyyy\");) >> \
         /K << /S /JavaScript /JS (AFDate_KeystrokeEx(\"dd mmmm yyyy\");) >> >> >>",
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

/// `SPEC-images.md` §45.3 and §48, end to end as a real child process: a
/// folder of pictures opens in a document worker **without a PDF library**,
/// its pages render, and every PDF semantic reports `Unsupported` rather than
/// pretending to have an answer.
#[test]
fn a_folder_of_images_opens_in_a_document_worker_and_refuses_pdf_semantics() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    for (name, size) in [("img2.png", (40u32, 20u32)), ("img10.png", (10, 30))] {
        image::RgbaImage::from_pixel(size.0, size.1, image::Rgba([9, 9, 9, 255]))
            .save(directory.path().join(name))
            .expect("write a fixture image");
    }
    let source = directory.path().to_path_buf();
    let Some(command) = command(&source) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };

    let mut session = DocumentSession::start(&command, &source)
        .expect("a folder needs no PDF library to open (§45.3)");

    let DocumentResponse::Opened(info) = session
        .request(DocumentRequest::Info)
        .expect("the worker describes its folder")
    else {
        panic!("expected document info")
    };
    assert_eq!(info.page_count, 2);
    assert!(!info.has_form);
    assert!(
        info.level.is_view_only(),
        "§48.3: the UI reads this rather than offering controls that refuse"
    );
    assert!(!info.level.allows_annotation());
    assert!(!info.level.allows_form_filling());
    // Natural order, so page 0 is img2.png at 40×20.
    assert_eq!(info.first_page.width, 40.0);

    let DocumentResponse::Frame(frame) = session
        .request(DocumentRequest::Render(DocumentRenderRequest {
            page: PageIndex(1),
            width: 20,
            height: 60,
            region: pulpit_core::notes::Region::FULL,
            full_width: 0,
            full_height: 0,
        }))
        .expect("the picture renders")
    else {
        panic!("expected a frame")
    };
    assert!(frame.is_consistent());
    assert_eq!(&frame.pixels[..4], &[9, 9, 9, 255]);

    // §48.1 and §48.2, over the wire.
    for request in [
        DocumentRequest::ListAnnotations { page: PageIndex(0) },
        DocumentRequest::ListFields,
        DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(),
        },
        DocumentRequest::SaveAs(SaveRequest {
            destination: directory.path().join("saved.pdf"),
            options: SaveOptions::verified(),
        }),
    ] {
        match session.request(request) {
            Err(SessionError::Refused(_)) => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    session.close();
}

/// `SPEC-reader-formats.md` §54 and §56.1, end to end as a real child
/// process: a comic archive opens in a document worker **without a PDF
/// library**, its pages render in sorted-full-path order, and nothing is
/// unpacked to disk.
#[test]
fn a_comic_archive_opens_in_a_document_worker_without_a_pdf_library() {
    use std::io::Write;

    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("comic.cbz");
    {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for (name, width, height) in [
            ("ch-1/page-10.png", 30u32, 20u32),
            ("ch-1/page-02.png", 20, 30),
            ("ComicInfo.xml", 0, 0),
        ] {
            writer.start_file(name, options).unwrap();
            if name.ends_with(".png") {
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::RgbaImage::from_pixel(width, height, image::Rgba([2, 3, 4, 255]))
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                writer.write_all(bytes.get_ref()).unwrap();
            } else {
                writer.write_all(b"<ComicInfo/>").unwrap();
            }
        }
        writer.finish().unwrap();
    }

    let Some(command) = command(&source) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };

    let mut session = DocumentSession::start(&command, &source)
        .expect("a comic archive needs no PDF library to open (§56.1)");

    let DocumentResponse::Opened(info) = session
        .request(DocumentRequest::Info)
        .expect("the worker describes its archive")
    else {
        panic!("expected document info")
    };
    assert_eq!(info.page_count, 2, "the XML is not a page");
    assert!(info.level.is_view_only(), "§60.1");
    // page-02 before page-10: natural sort over the full entry path (§54.3).
    assert_eq!(info.first_page.width, 20.0);

    let DocumentResponse::Frame(frame) = session
        .request(DocumentRequest::Render(DocumentRenderRequest {
            page: PageIndex(1),
            width: 15,
            height: 10,
            region: pulpit_core::notes::Region::FULL,
            full_width: 0,
            full_height: 0,
        }))
        .expect("the page renders")
    else {
        panic!("expected a frame")
    };
    assert!(frame.is_consistent());
    assert_eq!(&frame.pixels[..4], &[2, 3, 4, 255]);

    // §54.2 and §54.5: the archive is still one file, and nothing was
    // unpacked beside it.
    let beside: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect();
    assert_eq!(beside, [std::ffi::OsString::from("comic.cbz")]);

    // §60.1, over the wire.
    match session.request(DocumentRequest::ListFields) {
        Err(SessionError::Refused(_)) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }

    session.close();
}
