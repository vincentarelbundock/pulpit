//! What a thumbnail actually costs, at each width, on a real deck.
//!
//! The warming design turns on one number: how much of a small render is
//! resolution-dependent rasterising and how much is resolution-independent
//! page parsing. If it is mostly the former, a coarser first pass covers a
//! deck proportionally sooner; if it is mostly the latter, halving the width
//! buys almost nothing and the only way to go faster is to render fewer pages.
//!
//! `cargo run -p pulpit-render --features pdfium --example thumb-bench -- deck.pdf`

#[cfg(not(feature = "pdfium"))]
fn main() {
    eprintln!("build with --features pdfium");
}

#[cfg(feature = "pdfium")]
fn main() {
    use pulpit_core::notes::Region;
    use pulpit_render::pdf::pdfium::PdfiumBackend;
    use pulpit_render::pdf::{NeverCancel, PdfBackend, RenderRequest};
    use std::path::PathBuf;
    use std::time::Instant;

    let path = std::env::args()
        .nth(1)
        .expect("usage: thumb-bench deck.pdf");
    if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
        std::env::set_var(
            "PULPIT_PDFIUM_PATH",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib"),
        );
    }
    let mut backend = PdfiumBackend::bind().expect("no libpdfium");
    let document = backend.open(std::path::Path::new(&path)).unwrap();
    let metadata = backend.metadata(document).unwrap();
    let pages = metadata.page_count;
    let aspect = metadata.first_page_size.aspect_ratio();
    println!("{path}: {pages} pages, aspect {aspect:.3}");

    for width in [120u32, 240, 480, 960] {
        let height = (width as f32 / aspect).max(1.0) as u32;
        // One warm pass first: the first render of a document pays for work
        // no later one repeats, and that is not what is being measured.
        for page in 0..pages.min(3) {
            let _ = backend.render(
                &RenderRequest {
                    document,
                    page,
                    region: Region::FULL,
                    width,
                    height,
                    full_size: None,
                    with_annotations: false,
                },
                &NeverCancel,
            );
        }
        let start = Instant::now();
        for page in 0..pages {
            backend
                .render(
                    &RenderRequest {
                        document,
                        page,
                        region: Region::FULL,
                        width,
                        height,
                        full_size: None,
                        with_annotations: false,
                    },
                    &NeverCancel,
                )
                .unwrap();
        }
        let elapsed = start.elapsed();
        println!(
            "{width:>4}x{height:<4} {:>8.1} ms total  {:>6.2} ms/page",
            elapsed.as_secs_f64() * 1000.0,
            elapsed.as_secs_f64() * 1000.0 / pages as f64
        );
    }
}
