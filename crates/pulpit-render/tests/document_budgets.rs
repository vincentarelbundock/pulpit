//! The §13.6 budgets that belong to the engine, measured rather than inferred.
//!
//! Two of that section's six bullets are claims about this crate: that a
//! committed annotation reaches a revised frame promptly, and that annotation
//! enumeration is bounded on large documents. The other four are the session's
//! and are measured beside it in `pulpit`.
//!
//! Enumeration is checked as a *ratio*, because "is it bounded" is a question
//! about how cost grows, and a ratio reads the same on a fast machine and a
//! slow one. The commit turnaround is absolute, with a threshold far above the
//! baseline on the development machines. Both print what they measured, so
//! `cargo test -p pulpit-render --test document_budgets -- --nocapture` is how
//! the baseline gets re-read.

use std::time::{Duration, Instant};

use pulpit_core::annotate::{
    AnnotationCommand, AnnotationDraft, AnnotationId, InkDraft, InkPoint, MarkStyle,
};
use pulpit_core::page::{PageIndex, PageRect};
use pulpit_render::document::memory::MemoryDocument;
use pulpit_render::document::{
    AnnotationSupport, DocumentRevision, DocumentTransaction, PdfDocument,
};

#[cfg(feature = "pdfium")]
mod common;

#[test]
fn a_stroke_commits_promptly_enough_to_be_in_the_next_frame() {
    // The engine's own turnaround, without a process boundary or a PDF parser
    // in it, because that is the part this codebase controls. The frame that
    // follows carries the revision the commit produced (A7), which the session
    // tests check; what is timed here is how long the caller waits for it.
    let mut document = PdfDocument::new(Box::new(MemoryDocument::letter(4)), 5);
    let transaction = DocumentTransaction::from_annotations([AnnotationCommand::Create(
        AnnotationDraft::Ink(InkDraft {
            page: PageIndex(0),
            points: (0..200)
                .map(|step| InkPoint::new(100.0 + step as f32, 120.0))
                .collect(),
            style: MarkStyle::default(),
        }),
    )]);

    let start = Instant::now();
    let applied = document
        .apply(DocumentRevision::INITIAL, transaction)
        .expect("the stroke commits");
    let elapsed = start.elapsed();

    assert_eq!(applied.document_revision, DocumentRevision(1));
    assert!(
        applied.dirty_pages.contains(&PageIndex(0)),
        "the drawn page was not marked for redrawing"
    );
    println!("  committing a 200-point stroke: {elapsed:?} (budget 20ms)");
    assert!(
        elapsed <= Duration::from_millis(20),
        "committing a stroke took {elapsed:?}"
    );
}

#[test]
fn enumerating_one_page_costs_the_same_however_large_the_document_is() {
    // A page's annotations are read from that page. If enumeration ever walked
    // the document, the large one would cost proportionally more — and a
    // hundred-page review document is exactly where that would first hurt.
    // Both documents carry the same 32 annotations on page zero, so the only
    // difference between the measurements is everything else in the file.
    fn document(pages: usize) -> PdfDocument<'static> {
        let mut engine = MemoryDocument::letter(pages);
        for page in 0..pages {
            for mark in 0..32 {
                let at = mark as f32 * 10.0;
                engine.add_imported(
                    PageIndex(page),
                    AnnotationId::imported(&format!("p{page}-m{mark}")).expect("a name"),
                    AnnotationSupport::Editable,
                    PageRect::new(at, at, at + 8.0, at + 8.0),
                    Vec::new(),
                );
            }
        }
        PdfDocument::new(Box::new(engine), 7)
    }

    fn cost(document: &mut PdfDocument<'_>) -> Duration {
        // Warm first: the first call through is not what is being compared.
        let _ = document.annotations(PageIndex(0));
        let rounds = 200;
        let start = Instant::now();
        for _ in 0..rounds {
            let listed = document.annotations(PageIndex(0)).expect("a page");
            assert_eq!(listed.len(), 32);
        }
        start.elapsed() / rounds
    }

    let small = cost(&mut document(4));
    let large = cost(&mut document(400));
    println!("  one page of a 4-page document: {small:?}");
    println!("  one page of a 400-page document: {large:?}");

    // A hundredfold more document. Timing noise on a small absolute number is
    // real, so the threshold is loose; what it will not tolerate is the cost
    // tracking the document's size.
    assert!(
        large < small.max(Duration::from_micros(1)) * 8,
        "listing one page of a 400-page document cost {large:?} against {small:?} \
         for a 4-page one — enumeration is walking the document"
    );
}

/// What a form commit costs the reader, component by component (§13.6).
///
/// The commit path is the one place where the engine deliberately throws work
/// away: a committed field is snapshotted with an incremental `SaveAsCopy`,
/// the snapshot is opened under a new supervisor document and a new render
/// generation, and every visible page is redrawn from it. Whether that is
/// affordable is a question about three numbers — the snapshot write, the
/// reopen, and one page render at the size the reader draws at — and this
/// measures all three rather than reasoning about them.
///
/// Printed rather than tightly bounded: the point is the baseline, and the
/// assertions only catch an order of magnitude.
#[cfg(feature = "pdfium")]
mod commit_path {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use pulpit_core::page::{PageIndex, PagePoint};
    use pulpit_render::document::pdfium::PdfiumDocument;
    use pulpit_render::document::protocol::FormInputEvent;
    use pulpit_render::document::{PdfDocument, SaveOptions};
    use pulpit_render::pdf::pdfium::PdfiumBackend;

    use crate::common;

    fn corpus_form() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/pdf-corpus/acroform/multiple_form_types.pdf")
    }

    /// A many-paged deck with real page content, for the render half: what a
    /// re-render costs depends on what is on the page, and a form fixture's
    /// blank sheet would flatter it.
    fn deck() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/mosaic.pdf")
    }

    fn median(mut samples: Vec<Duration>) -> Duration {
        samples.sort();
        samples[samples.len() / 2]
    }

    /// Type a character into the first writable text field and commit it.
    fn commit_a_field(document: &mut PdfDocument<'_>) -> bool {
        let Ok(fields) = document.fields() else {
            return false;
        };
        let Some(target) = fields
            .into_iter()
            .find(|field| !field.read_only && field.anchor_on(PageIndex(0)).is_some())
        else {
            return false;
        };
        let bounds = target.anchor_on(PageIndex(0)).expect("checked above");
        let at = PagePoint {
            x: (bounds.left + bounds.right) / 2.0,
            y: (bounds.top + bounds.bottom) / 2.0,
        };
        let _ = document.form_event(PageIndex(0), FormInputEvent::PointerDown { at });
        let _ = document.form_event(PageIndex(0), FormInputEvent::PointerUp { at });
        let _ = document.form_event(PageIndex(0), FormInputEvent::Char { character: 'A' });
        document
            .form_event(PageIndex(0), FormInputEvent::Focus { gained: false })
            .is_ok()
    }

    fn render_cost(document: &PdfDocument<'_>, page: usize, size: (u32, u32)) -> Duration {
        let start = Instant::now();
        document
            .render_page(
                PageIndex(page),
                pulpit_core::notes::Region::FULL,
                size.0,
                size.1,
                None,
            )
            .expect("the page renders");
        start.elapsed()
    }

    fn open<'a>(backend: &'a mut PdfiumBackend, path: &Path, id: u64) -> PdfDocument<'a> {
        let engine = PdfiumDocument::open(backend, path).expect("the document opens");
        PdfDocument::new(Box::new(engine), id)
    }

    #[test]
    fn a_form_commit_pays_a_snapshot_a_reopen_and_a_cold_render_of_every_visible_page() {
        pulpit_testkit::on_the_pdfium_thread(|| {
            let Some(mut guard) = common::pdfium("the commit-path budget") else {
                return;
            };
            let backend = &mut *guard;
            let directory = tempfile::tempdir().expect("a temporary directory");

            // --- the snapshot write, on a real AcroForm --------------------
            let mut writes = Vec::new();
            {
                let mut form = open(backend, &corpus_form(), 900);
                assert!(commit_a_field(&mut form), "the corpus form did not commit");
                for round in 0..7 {
                    let destination = directory.path().join(format!("snapshot-{round}.pdf"));
                    let start = Instant::now();
                    form.save_as(
                        &destination,
                        SaveOptions {
                            incremental: true,
                            verify: false,
                        },
                    )
                    .expect("the snapshot is written");
                    writes.push(start.elapsed());
                }
            }

            // --- reopening it under the new document id --------------------
            let mut reopens = Vec::new();
            for round in 0..7 {
                let destination = directory.path().join(format!("snapshot-{round}.pdf"));
                let start = Instant::now();
                let reopened = open(backend, &destination, 901 + round);
                reopens.push(start.elapsed());
                drop(reopened);
            }
            let write = median(writes);
            let reopen = median(reopens);
            println!("  incremental snapshot write: {write:?}");
            println!("  reopening the snapshot:     {reopen:?}");

            // --- what a cold visible page costs to redraw -------------------
            // The plan is a coarse entry (capped at 480px wide) plus a refined
            // one at the cell's own size, for every page in a window three
            // viewports tall — typically three or four pages. The sizes below
            // bracket a reader cell on a 1080p display and on a HiDPI one.
            {
                let pages = open(backend, &deck(), 950);
                let coarse: Vec<Duration> = (0..5)
                    .map(|round| render_cost(&pages, round % 2, (480, 622)))
                    .collect();
                let coarse = median(coarse);
                println!("  cold render 480x622 (the coarse pass): {coarse:?}");
                for size in [(816u32, 1056u32), (1224, 1584), (1632, 2112)] {
                    let cold: Vec<Duration> = (0..5)
                        .map(|round| render_cost(&pages, round % 2, size))
                        .collect();
                    let each = median(cold);
                    println!(
                        "  cold render {}x{}: {each:?} each; a four-page window \
                         (coarse + refined) {:?}",
                        size.0,
                        size.1,
                        (each + coarse) * 4
                    );
                }
            }

            // The whole point of the measurement: the fixed part of the commit
            // path, which the per-page optimisation could not remove anyway.
            let fixed = write + reopen;
            println!("  fixed cost of a commit (write + reopen): {fixed:?}");
            assert!(
                fixed <= Duration::from_millis(250),
                "the fixed part of a form commit took {fixed:?}"
            );
        });
    }
}
