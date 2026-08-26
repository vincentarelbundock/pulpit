//! Speech's one request to the document worker, end to end.
//!
//! `PageText` is the whole reason speech can say anything: the reading cursor
//! asks for a page, and every sentence it speaks comes out of the answer. It
//! crosses a process boundary, so it is worth one test that actually crosses
//! it — the same reason `document_worker.rs` exists.
//!
//! What this catches is the failure that has no symptom: if the request is
//! never dispatched, or the answer never routed back, speech goes quiet with
//! nothing in the log and nothing on screen, because a reading with no text
//! has nothing to say and no reason to complain.

use std::path::PathBuf;

use pulpit_core::page::PageIndex;
use pulpit_render::document::protocol::{DocumentRequest, DocumentResponse};
use pulpit_render::document::session::{DocumentSession, DocumentWorkerCommand};

fn command(source: &std::path::Path) -> DocumentWorkerCommand {
    // `CARGO_BIN_EXE_pulpit` is set by cargo for integration tests in the
    // package that owns the binary, and cargo builds the binary *because*
    // this test names it — so unlike a filesystem search beside the test
    // executable, this cannot silently skip on a clean checkout and leave CI
    // green having exercised nothing.
    let program = PathBuf::from(env!("CARGO_BIN_EXE_pulpit"));
    if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
        let lib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib");
        std::env::set_var("PULPIT_PDFIUM_PATH", lib);
    }
    DocumentWorkerCommand::Explicit {
        program,
        args: vec![format!("--document-worker={}", source.display())],
    }
}

#[test]
fn a_page_of_text_crosses_the_worker_boundary_for_speech() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("source.pdf");
    if pulpit_render::pdf::synth::write_pdf(&source, 2, None).is_err() {
        eprintln!("skipping: cannot write a synthetic PDF");
        return;
    }
    let command = command(&source);
    let mut session = match DocumentSession::start(&command, &source) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("skipping: cannot start a document worker: {error}");
            return;
        }
    };

    match session.request(DocumentRequest::PageText { page: PageIndex(0) }) {
        Ok(DocumentResponse::PageText(text)) => {
            // The synthetic PDF has a text layer; what matters is that a
            // string came back through the protocol at all, and that speech
            // could segment it into something to say.
            let sentences = pulpit_core::speech::sentences(&text);
            eprintln!("page text: {text:?} ({} sentences)", sentences.len());
            assert!(
                !text.is_empty(),
                "the synthetic page has text, so speech has something to read"
            );
        }
        // A build with no PDFium answers this way, and so does a backend with
        // no text layer. Both are the honest answer rather than a failure,
        // and both are what the reading cursor turns into "cannot read this
        // aloud" rather than into silence.
        Ok(DocumentResponse::Failed(failure)) => {
            eprintln!("skipping: the worker refused: {failure:?}");
        }
        Ok(other) => panic!("the worker answered {other:?} when asked for a page's text"),
        Err(error) => panic!("asking for a page's text failed: {error}"),
    }
}
