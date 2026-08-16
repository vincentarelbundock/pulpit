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
