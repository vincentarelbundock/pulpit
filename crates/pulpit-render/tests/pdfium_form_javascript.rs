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
//! Both tests run through `testkit::on_the_pdfium_thread`, which is not
//! optional: PDFium's V8 isolate belongs to the thread that created it, and
//! libtest gives every test its own. See that module for why this costs
//! nothing in production.
#![cfg(feature = "pdfium")]

use std::path::PathBuf;

use pulpit_core::page::{PageIndex, PagePoint};
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::protocol::FormInputEvent;
use pulpit_render::document::DocumentBackend;

mod common;
mod testkit;

/// A one-page PDF with two text fields, where `total` is calculated from
/// `count` by a script and has no value of its own.
///
/// Written out by hand: an AcroForm is a small enough object graph that
/// building it directly is clearer than generating it, and the cross-reference
/// table is the only fiddly part.
fn calculating_form() -> Vec<u8> {
    crate::testkit::builder::Pdf::from_objects([
        // 1: catalog, carrying the AcroForm and its calculation order.
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R] \
         /CO [6 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>",
        // 2: the page tree.
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        // 3: the page, whose annotations are the two widgets.
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R 6 0 R] >>",
        // 4: the font the fields' default appearance names.
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        // 5: the field that is typed into.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (count) /V () \
         /Ff 0 /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R >>",
        // 6: the calculated field. Its value is never set in the file; if it
        // ever reads back as anything, a script produced it.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (total) /V () \
         /Ff 0 /Rect [100 650 300 680] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R \
         /AA << /C << /S /JavaScript /JS (event.value = \
         this.getField(\"count\").value * 2;) >> >> >>",
    ])
    .build()
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
    crate::testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = common::pdfium("the PDFium form JavaScript tests") else {
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
    crate::testkit::builder::Pdf::from_objects([
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] \
         /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        // `/F` here is the format action, which PDFium runs when the field's
        // appearance is regenerated — that is, on every commit.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (count) /V () \
         /Ff 0 /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R \
         /AA << /F << /S /JavaScript /JS (app.alert(\"filled\", \"pulpit\"); \
         this.submitForm(\"https://example.invalid/collect\");) >> >> >>",
    ])
    .build()
}

#[test]
fn what_a_script_asks_the_host_for_is_reported_and_not_performed() {
    crate::testkit::on_the_pdfium_thread(|| {
        use pulpit_render::document::protocol::HostRequest;

        let Some(mut guard) = common::pdfium("the PDFium form JavaScript tests") else {
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
    crate::testkit::on_the_pdfium_thread(|| {
        use pulpit_render::document::DocumentWarning;

        let Some(mut guard) = common::pdfium("the PDFium form JavaScript tests") else {
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
    crate::testkit::builder::Pdf::from_objects([
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] \
         /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T (send) \
         /Rect [100 700 200 730] /F 4 /P 3 0 R \
         /A << /S /SubmitForm /F << /FS /URL /F (https://example.invalid/collect) >> \
         /Flags 4 >> >>",
    ])
    .build()
}

/// A submit button is named at open time, even though nothing can say it is a
/// submit button.
#[test]
fn a_form_button_that_carries_an_action_is_warned_about_when_it_opens() {
    crate::testkit::on_the_pdfium_thread(|| {
        use pulpit_render::document::{CompatibilityLevel, DocumentWarning};

        let Some(mut guard) = common::pdfium("the PDFium form JavaScript tests") else {
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

/// A *two-page* form: `count` is typed on page one, and the calculation script
/// on `total` — which lives on page **two** — rewrites itself from it.
///
/// The same arithmetic as `calculating_form`, moved across a page boundary,
/// because that is where the interesting asymmetry is.
fn cross_page_calculating_form() -> Vec<u8> {
    crate::testkit::builder::Pdf::from_objects([
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R 7 0 R] \
         /CO [7 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [6 0 R] >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [7 0 R] >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        // 6: the typed field, on page one.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (count) /V () \
         /Ff 0 /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R >>",
        // 7: the calculated field, on page two.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (total) /V () \
         /Ff 0 /Rect [100 650 300 680] /DA (/Helv 12 Tf 0 g) /F 4 /P 4 0 R \
         /AA << /C << /S /JavaScript /JS (event.value = \
         this.getField(\"count\").value * 2;) >> >> >>",
    ])
    .build()
}

/// Committing on one page can change a field on another, and the engine's
/// invalidation does not say so.
///
/// This is the hazard the reader's whole-document snapshot currently masks:
/// every commit reopens the document under a new render generation, so every
/// visible page is redrawn whether or not anything on it moved, and a
/// cross-page calculation is therefore *shown* correctly by accident. The
/// invalidation itself is page-local — `FFI_Invalidate` collects rectangles
/// for the page the event was delivered to, and `FormEventResult::invalidated`
/// carries no page at all — so anything that ever narrows the redraw to the
/// committed page will silently stop drawing the other one.
///
/// The test asserts both halves: the value crosses the page boundary, and the
/// invalidation does not mention it. If the second assertion ever fails,
/// PDFium has started reporting cross-page dirt and the note above is stale.
#[test]
fn a_calculation_can_rewrite_a_field_on_another_page_without_invalidating_it() {
    crate::testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = common::pdfium("the PDFium form JavaScript tests") else {
            eprintln!("no libpdfium; skipping");
            return;
        };
        let backend = &mut *guard;
        let path = std::env::temp_dir().join("pulpit-form-js-cross-page.pdf");
        std::fs::write(&path, cross_page_calculating_form()).expect("the fixture is written");
        let mut document = PdfiumDocument::open(backend, &path).expect("the fixture opens");
        assert_eq!(document.info().page_count, 2, "the fixture has two pages");

        let page = PageIndex(0);
        let at = inside_count_field();
        for event in [
            FormInputEvent::PointerDown { at },
            FormInputEvent::PointerUp { at },
            FormInputEvent::Char { character: '2' },
            FormInputEvent::Char { character: '1' },
        ] {
            document.form_event(page, event).expect("the event lands");
        }
        let committed = document
            .form_event(page, FormInputEvent::Focus { gained: false })
            .expect("focus is dropped");

        assert_eq!(
            document.field_value("total").expect("total is readable"),
            "42",
            "the calculation did not cross the page boundary; either PDFium was \
             built without V8 or no JS platform was installed"
        );

        // Everything the commit reported dirty is on page one — the widget on
        // page two is at `/Rect [100 650 300 680]`, i.e. 112..142 measured down
        // from the top of a 792pt page, and page one's widget is at 62..92.
        // Both are page-space rectangles with no page on them, so the only
        // thing that can be asserted is that the count is small and that they
        // sit where page one's field is: nothing here names page two.
        for dirty in &committed.invalidated {
            assert!(
                dirty.top >= 50.0 && dirty.bottom <= 100.0,
                "{dirty:?} is not the typed field's own rectangle — if the \
                 engine has started reporting the calculated field's, it is \
                 doing so without saying which page it is on"
            );
        }
    });
}

/// A form that stamps the date it was filled on, the way a real one does.
///
/// `when` carries an `/AA /C` calculation script that writes `new Date()` into
/// itself whenever anything commits, and `trigger` is the field that is typed
/// into to set that off.
fn date_stamping_form() -> Vec<u8> {
    crate::testkit::builder::Pdf::from_objects([
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R] \
         /CO [6 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 4 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R 6 0 R] >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (trigger) /V () /Ff 0 \
         /Rect [100 700 300 730] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R >>",
        // Single quotes in the script: a PDF literal string would otherwise
        // have to escape the double ones, and the point here is the `Date`.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (when) /V () /Ff 0 \
         /Rect [100 650 300 680] /DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R \
         /AA << /C << /S /JavaScript /JS (event.value = \
         util.printd('yyyy', new Date());) >> >> >>",
    ])
    .build()
}

/// A script's own `Date` is the real one, and `FFI_GetLocalTime` does not
/// change that.
///
/// The callback pulpit installs answers with a zeroed `FPDF_SYSTEMTIME`, and
/// that used to be described — and tested, under the name "the clock a document
/// can read is not the wall clock" — as though it closed the clock to a
/// document's JavaScript. It does not. Under the V8 build `new Date()` is
/// V8's, on V8's own clock, and a calculation script that stamps today's date
/// into a field gets today's date.
///
/// This test exists to hold that fact in the open rather than leave it to be
/// discovered. It asserts what is *true* — the year is a real, current one —
/// so that a build which ever did close V8's clock would fail here and send
/// somebody to the comment in `document::form` that says it is open.
///
/// Nothing is asserted about the exact day, because that would be a test that
/// fails at midnight in some timezone; a plausible year is enough to tell the
/// real clock from a zeroed one, which would produce `0000`.
#[test]
fn a_scripts_own_date_is_the_real_one_and_this_callback_does_not_change_that() {
    crate::testkit::on_the_pdfium_thread(|| {
        let Some(mut guard) = common::pdfium("the PDFium form JavaScript tests") else {
            eprintln!("no libpdfium; skipping");
            return;
        };
        let backend = &mut *guard;
        let path = std::env::temp_dir().join("pulpit-form-js-date.pdf");
        std::fs::write(&path, date_stamping_form()).expect("the fixture is written");
        let mut document = PdfiumDocument::open(backend, &path).expect("the fixture opens");

        let page = PageIndex(0);
        let at = PagePoint::new(200.0, 792.0 - 715.0);
        for event in [
            FormInputEvent::PointerDown { at },
            FormInputEvent::PointerUp { at },
            FormInputEvent::Char { character: 'x' },
            FormInputEvent::Focus { gained: false },
        ] {
            document.form_event(page, event).expect("the event lands");
        }

        let stamped = document.field_value("when").expect("the field is readable");
        assert!(
            !stamped.is_empty(),
            "the calculation did not run at all; either PDFium was built \
             without V8 or no JS platform was installed"
        );
        let year: i32 = stamped
            .parse()
            .unwrap_or_else(|_| panic!("the script wrote {stamped:?}, which is not a year"));
        assert!(
            (2024..2100).contains(&year),
            "a form's script stamped {year}. If this is 0, V8's clock has been \
             closed and `document::form`'s note about `FFI_GetLocalTime` — which \
             says it has not been — is now wrong and must be corrected."
        );
    });
}
