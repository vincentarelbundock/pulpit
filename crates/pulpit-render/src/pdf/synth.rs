//! A tiny valid-PDF writer used to generate test fixtures.
//!
//! Not a PDF library: just enough to produce a real, openable document with a
//! known page count, a visible page number and optional metadata, so tests
//! and CI can exercise the actual PDFium path (and the reload path, which
//! needs a file that genuinely changes on disk).

use std::io::Write;
use std::path::Path;

/// Write a `pages`-page PDF at `path`. Each page shows its number.
///
/// `metadata` is placed in `/Keywords`, which is where the notes-mapping
/// contract is read from.
pub fn write_pdf(path: &Path, pages: usize, metadata: Option<&str>) -> std::io::Result<()> {
    write_pdf_with_attachment(path, pages, metadata, None)
}

/// Write a fixture that also carries one embedded file.
///
/// `attachment` is `(name, contents)`, and lands in the catalog's
/// `/Names /EmbeddedFiles` name tree — the same place beamer's `pdfpc` package
/// and Typst's `pdf.attach` put speaker notes, so a test can read one back
/// through the real PDFium name-tree walk rather than a stand-in.
pub fn write_pdf_with_attachment(
    path: &Path,
    pages: usize,
    metadata: Option<&str>,
    attachment: Option<(&str, &str)>,
) -> std::io::Result<()> {
    let pages = pages.max(1);
    let mut objects: Vec<Vec<u8>> = Vec::new();

    // 1: catalog, 2: page tree, 3: font, then per page: page object + content.
    let page_object_ids: Vec<usize> = (0..pages).map(|i| 4 + i * 2).collect();
    let kids: String = page_object_ids
        .iter()
        .map(|id| format!("{id} 0 R "))
        .collect::<String>();

    // The catalog is written first but has to name objects appended last, so
    // their ids are computed rather than observed: three fixed objects, two
    // per page, then the optional info dictionary.
    let after_pages = 3 + pages * 2 + usize::from(metadata.is_some());
    let (filespec_id, stream_id) = (after_pages + 1, after_pages + 2);

    let names = match attachment {
        Some((name, _)) => format!(
            " /Names << /EmbeddedFiles << /Names [ ({}) {filespec_id} 0 R ] >> >>",
            escape(name)
        ),
        None => String::new(),
    };
    objects.push(format!("<< /Type /Catalog /Pages 2 0 R{names} >>").into_bytes());
    objects.push(format!("<< /Type /Pages /Count {pages} /Kids [ {kids}] >>").into_bytes());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());

    for (index, page_id) in page_object_ids.iter().enumerate() {
        let content_id = page_id + 1;
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 720 405] \
                 /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
            )
            .into_bytes(),
        );
        let stream = format!(
            "BT /F1 96 Tf 300 180 Td ({}) Tj ET\n\
             1 0 0 RG 4 w 10 10 700 385 re S\n",
            index + 1
        );
        objects.push(
            format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()).into_bytes(),
        );
    }

    let info_id = objects.len() + 1;
    let has_info = metadata.is_some();
    if let Some(metadata) = metadata {
        objects.push(
            format!(
                "<< /Keywords ({}) /Producer (pulpit fixtures) >>",
                escape(metadata)
            )
            .into_bytes(),
        );
    }

    if let Some((name, contents)) = attachment {
        objects.push(
            format!(
                "<< /Type /Filespec /F ({0}) /UF ({0}) /EF << /F {stream_id} 0 R >> >>",
                escape(name)
            )
            .into_bytes(),
        );
        objects.push(
            format!(
                "<< /Type /EmbeddedFile /Subtype /application#2Fjson /Length {} >>\n\
                 stream\n{contents}\nendstream",
                contents.len() + 1
            )
            .into_bytes(),
        );
        debug_assert_eq!(objects.len(), stream_id, "computed object ids must hold");
    }

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let trailer = if has_info {
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info {info_id} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
    } else {
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
    };
    out.extend_from_slice(trailer.as_bytes());

    // Written whole: fixtures must never be observed half-complete by the
    // file watcher unless a test asks for exactly that.
    let mut file = std::fs::File::create(path)?;
    file.write_all(&out)?;
    file.sync_all()
}

fn escape(text: &str) -> String {
    text.replace('\\', r"\\")
        .replace('(', r"\(")
        .replace(')', r"\)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_a_plausible_pdf() {
        let dir = std::env::temp_dir().join("pulpit-synth-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deck.pdf");
        write_pdf(&path, 3, Some("pulpit:mapping=slides-only")).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.7"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        assert_eq!(
            bytes.windows(9).filter(|w| *w == b"/Type /Pa").count(),
            4,
            "one page tree plus three pages"
        );
        std::fs::remove_file(path).ok();
    }
}
