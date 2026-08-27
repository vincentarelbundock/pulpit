// Each integration test binary compiles this module separately and uses a
// different subset of it.
#![allow(dead_code)]

//! Shared fixture builder: produces a real signed PDF with the oracle's
//! self-signed test credential, following the §23.3 assembly order.
//!
//! Used by both `sign_spike.rs` (which also feeds the pyHanko oracle) and
//! `verify_cms.rs` (which checks it with pulpit's own §28.3 verifier).

use pulpit_render::sign::{self, SigningProfile};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// The `/M` value written into the signature dictionary, and its unix-seconds
/// equivalent — the two must agree, and `verify_cms.rs` asserts they do.
pub const MOD_DATE: &str = "D:20240820220000+00'00'";
pub const MOD_DATE_UNIX: i64 = 1_724_191_200;

/// The `signing-time` signed attribute value handed to the CMS builder.
pub const SIGNING_TIME_UNIX: i64 = 1_724_166_000;

/// A signed PDF, plus the offsets a tamper test needs.
pub struct SignedFixture {
    pub bytes: Vec<u8>,
    /// Offset of the `<` opening the `/Contents` reservation.
    pub sig_start: u64,
    /// Offset just past the `>` closing it.
    pub sig_end: u64,
    /// Length of the CMS DER actually written into the reservation.
    pub cms_len: usize,
}

/// Load the oracle's test credential, or `None` when it has not been generated.
pub fn load_test_credential() -> Option<sign::Credential> {
    let cred_path = PathBuf::from("../../tools/sign-oracle/credentials/test-self-signed.p12");
    if cred_path.exists() {
        if let Ok(p12_bytes) = std::fs::read(&cred_path) {
            if let Ok(cred) =
                sign::load_pkcs12(&p12_bytes, sign::Zeroizing::new("test".to_string()))
            {
                return Some(cred);
            }
        }
    }
    None
}

/// Skip-with-message helper for tests that need the oracle's credential.
pub fn skip_message() {
    eprintln!("SKIP: tools/sign-oracle/credentials/test-self-signed.p12 not found");
    eprintln!("      Run: make sign-oracle-setup");
    eprintln!("      Then: .venv-sign-oracle/bin/python tools/sign-oracle/gen-credentials.py");
}

/// A credential to sign with: the oracle's PKCS#12 when it has been generated,
/// otherwise a freshly generated self-signed ECDSA P-256 credential, so that
/// verification tests run on every machine.
pub fn any_test_credential() -> sign::Credential {
    if let Some(cred) = load_test_credential() {
        return cred;
    }
    generate_self_signed_credential()
}

/// Generate a self-signed ECDSA P-256 credential with rcgen.
pub fn generate_self_signed_credential() -> sign::Credential {
    let mut params = rcgen::CertificateParams::new(vec!["pulpit-verify-test".to_string()]);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "pulpit verify test");
    params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
    let cert = rcgen::Certificate::from_params(params).expect("rcgen certificate");
    let cert_der = cert.serialize_der().expect("serialize certificate");
    let key_der = cert.serialize_private_key_der();
    sign::credential_from_parts(&cert_der, &key_der, Vec::new()).expect("credential from parts")
}

/// Assemble a single-revision PDF from object bodies given in order, starting
/// at object 1, with a classic cross-reference table and a fixed `/ID`.
pub fn assemble_single_revision(objects: &[String]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(b"%PDF-1.7\n");

    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(output.len());
        output.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
    }

    let xref_start = output.len();
    output.extend_from_slice(b"xref\n");
    output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
    output.extend_from_slice(format!("1 {}\n", objects.len()).as_bytes());
    for offset in &offsets {
        output.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    output.extend_from_slice(b"trailer\n<<\n");
    output.extend_from_slice(format!("/Size {}\n", objects.len() + 1).as_bytes());
    output.extend_from_slice(b"/Root 1 0 R\n");
    output.extend_from_slice(
        b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
    );
    output.extend_from_slice(b">>\nstartxref\n");
    output.extend_from_slice(format!("{}\n", xref_start).as_bytes());
    output.extend_from_slice(b"%%EOF");
    output
}

/// How a fixture's cross-reference section is written.
///
/// A PDF 1.4 producer writes a classic `xref` table and keeps every object at
/// its own file offset. A PDF 1.5+ producer — LaTeX, Chrome's print-to-PDF,
/// Acrobat's "optimized" save — writes a cross-reference *stream* and packs
/// every non-stream object into an object stream. Both are ordinary files in
/// the wild, so a fixture builder that can only write the first shape cannot
/// exercise the reader on the majority case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XrefShape {
    /// `xref` … `trailer` … `startxref`: one offset per object.
    #[default]
    ClassicTable,
    /// A `/Type /XRef` stream, with every object still at its own offset.
    /// Isolates xref-stream parsing from object-stream extraction.
    XrefStream,
    /// A `/Type /XRef` stream plus a `/Type /ObjStm` holding every object,
    /// both `FlateDecode`d, the xref stream carrying a PNG `Up` predictor.
    /// This is what `mutool clean -Z` and Acrobat's optimizer produce.
    ObjectStreams,
}

/// Deflate `data` the way every real producer does.
fn deflate(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("deflate into a Vec");
    encoder.finish().expect("finish the deflate stream")
}

/// Apply the PNG `Up` predictor (filter type 2) to fixed-width `rows`, which
/// is the encoding side of what a reader has to undo. Every row is prefixed
/// with its filter tag, as §7.4.4.4 requires.
fn png_up_encode(rows: &[Vec<u8>], columns: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.len() * (columns + 1));
    let mut previous = vec![0u8; columns];
    for row in rows {
        assert_eq!(row.len(), columns, "every predictor row has /Columns bytes");
        out.push(2u8);
        for (index, byte) in row.iter().enumerate() {
            out.push(byte.wrapping_sub(previous[index]));
        }
        previous.clone_from(row);
    }
    out
}

/// One cross-reference entry, in the three-field form an xref stream stores.
#[derive(Clone, Copy)]
enum XrefRow {
    /// Type 0: free. Fields are the next free object and its generation.
    Free,
    /// Type 1: at a byte offset, with a generation.
    Offset(u64),
    /// Type 2: object `index` inside object stream `container`.
    InStream { container: u32, index: u32 },
}

/// Serialise `rows` (indexed by object number, starting at 0) as the body of a
/// `/W [1 4 2]` cross-reference stream.
///
/// The widths are the ones this fixture and pulpit's own writer both pick: one
/// byte is enough for a type in 0..=2, four bytes address a 4 GiB file, and two
/// bytes hold either a generation or an in-stream index.
fn xref_stream_rows(rows: &[XrefRow]) -> Vec<Vec<u8>> {
    rows.iter()
        .map(|row| {
            let (kind, second, third): (u8, u64, u16) = match *row {
                XrefRow::Free => (0, 0, 0xFFFF),
                XrefRow::Offset(offset) => (1, offset, 0),
                XrefRow::InStream { container, index } => (2, container as u64, index as u16),
            };
            let mut bytes = vec![kind];
            bytes.extend_from_slice(&(second as u32).to_be_bytes());
            bytes.extend_from_slice(&third.to_be_bytes());
            bytes
        })
        .collect()
}

/// Assemble a single-revision PDF from object bodies given in order, starting
/// at object 1, in the cross-reference shape `shape` asks for.
///
/// [`assemble_single_revision`] is `ClassicTable`; this is the same document,
/// byte-for-byte identical in its object bodies, written the way a PDF 1.5+
/// producer would. A test can therefore assert that a shape change alone does
/// not change what signing sees.
pub fn assemble_single_revision_shaped(objects: &[String], shape: XrefShape) -> Vec<u8> {
    match shape {
        XrefShape::ClassicTable => assemble_single_revision(objects),
        XrefShape::XrefStream => assemble_xref_stream(objects),
        XrefShape::ObjectStreams => assemble_object_streams(objects),
    }
}

/// Every object at its own offset, indexed by a `/Type /XRef` stream.
fn assemble_xref_stream(objects: &[String]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(b"%PDF-1.5\n");

    let mut rows = vec![XrefRow::Free];
    for (index, body) in objects.iter().enumerate() {
        rows.push(XrefRow::Offset(output.len() as u64));
        output.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
    }

    // The xref stream is itself an object and must appear in its own table,
    // so its number and offset are known before its body is serialised.
    let xref_number = objects.len() as u32 + 1;
    let xref_offset = output.len() as u64;
    rows.push(XrefRow::Offset(xref_offset));

    let body: Vec<u8> = xref_stream_rows(&rows).concat();
    output.extend_from_slice(
        format!(
            "{xref_number} 0 obj\n<</Type /XRef /Size {size} /W [1 4 2] /Root 1 0 R \
             /ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>] \
             /Length {length}>>\nstream\n",
            size = xref_number + 1,
            length = body.len()
        )
        .as_bytes(),
    );
    output.extend_from_slice(&body);
    output.extend_from_slice(b"\nendstream\nendobj\n");
    output.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF").as_bytes());
    output
}

/// Every object packed into one `/Type /ObjStm`, indexed by a deflated,
/// PNG-predicted `/Type /XRef` stream.
fn assemble_object_streams(objects: &[String]) -> Vec<u8> {
    // §7.5.7: the object stream's body is a header of `/N` number-offset pairs
    // followed by the object bodies, with `/First` pointing at the first body.
    let mut pairs = String::new();
    let mut bodies = String::new();
    for (index, body) in objects.iter().enumerate() {
        pairs.push_str(&format!("{} {} ", index + 1, bodies.len()));
        bodies.push_str(body);
        bodies.push(' ');
    }
    let first = pairs.len();
    let objstm_plain = format!("{pairs}{bodies}");
    let objstm_body = deflate(objstm_plain.as_bytes());

    let mut output = Vec::new();
    output.extend_from_slice(b"%PDF-1.5\n");

    let container = objects.len() as u32 + 1;
    let xref_number = container + 1;

    let mut rows = vec![XrefRow::Free];
    for index in 0..objects.len() {
        rows.push(XrefRow::InStream {
            container,
            index: index as u32,
        });
    }

    let container_offset = output.len() as u64;
    rows.push(XrefRow::Offset(container_offset));
    output.extend_from_slice(
        format!(
            "{container} 0 obj\n<</Type /ObjStm /N {n} /First {first} /Filter /FlateDecode \
             /Length {length}>>\nstream\n",
            n = objects.len(),
            length = objstm_body.len()
        )
        .as_bytes(),
    );
    output.extend_from_slice(&objstm_body);
    output.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = output.len() as u64;
    rows.push(XrefRow::Offset(xref_offset));

    let columns = 7; // /W [1 4 2]
    let predicted = png_up_encode(&xref_stream_rows(&rows), columns);
    let xref_body = deflate(&predicted);
    output.extend_from_slice(
        format!(
            "{xref_number} 0 obj\n<</Type /XRef /Size {size} /W [1 4 2] /Root 1 0 R \
             /Filter /FlateDecode /DecodeParms <</Predictor 12 /Columns {columns}>> \
             /ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>] \
             /Length {length}>>\nstream\n",
            size = xref_number + 1,
            length = xref_body.len()
        )
        .as_bytes(),
    );
    output.extend_from_slice(&xref_body);
    output.extend_from_slice(b"\nendstream\nendobj\n");
    output.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF").as_bytes());
    output
}

/// [`build_unsigned_pdf`] in a chosen cross-reference shape.
pub fn build_unsigned_pdf_shaped(fields: &[&str], shape: XrefShape) -> Vec<u8> {
    let named: Vec<(&str, NameSpelling)> = fields
        .iter()
        .map(|name| (*name, NameSpelling::PdfDoc))
        .collect();
    assemble_single_revision_shaped(&unsigned_pdf_objects(&named), shape)
}

/// How a fixture spells a field's `/T`.
///
/// A `/T` is a text string (§7.9.2.2), not UTF-8, and a producer picks the
/// spelling: Acrobat writes plain PDFDocEncoding for an ASCII name and
/// UTF-16BE behind a byte-order mark for anything else, in a literal string
/// whose unprintable bytes become octal escapes. A fixture that can only
/// write the first spelling cannot exercise a real form's field names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameSpelling {
    /// `(Name)` — PDFDocEncoding, which for an ASCII name is the name itself.
    #[default]
    PdfDoc,
    /// `(\376\377\000N\000a…)` — UTF-16BE in a literal string, the shape
    /// Acrobat produces.
    Utf16Literal,
    /// `<FEFF004E0061…>` — UTF-16BE in a hex string.
    Utf16Hex,
}

/// `name` written as a PDF string token, ready to follow `/T` in a dictionary.
pub fn pdf_text_string(name: &str, spelling: NameSpelling) -> String {
    match spelling {
        NameSpelling::PdfDoc => {
            let mut out = String::from("(");
            for byte in name.bytes() {
                if matches!(byte, b'(' | b')' | b'\\') {
                    out.push('\\');
                }
                out.push(byte as char);
            }
            assert!(
                name.is_ascii(),
                "a non-ASCII name has no single-byte spelling here; use a UTF-16 one"
            );
            out.push(')');
            out
        }
        NameSpelling::Utf16Literal => {
            let mut out = String::from("(");
            for byte in utf16be_with_bom(name) {
                match byte {
                    b'(' | b')' | b'\\' => {
                        out.push('\\');
                        out.push(byte as char);
                    }
                    0x20..=0x7E => out.push(byte as char),
                    other => out.push_str(&format!("\\{other:03o}")),
                }
            }
            out.push(')');
            out
        }
        NameSpelling::Utf16Hex => {
            let mut out = String::from("<");
            for byte in utf16be_with_bom(name) {
                out.push_str(&format!("{byte:02X}"));
            }
            out.push('>');
            out
        }
    }
}

fn utf16be_with_bom(name: &str) -> Vec<u8> {
    let mut out = vec![0xFEu8, 0xFF];
    for unit in name.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// An unsigned single-page PDF carrying `fields` empty `/Sig` fields.
///
/// With no field names it has no AcroForm at all, which is the shape the
/// "create a new invisible field" path has to cope with.
pub fn build_unsigned_pdf(fields: &[&str]) -> Vec<u8> {
    let named: Vec<(&str, NameSpelling)> = fields
        .iter()
        .map(|name| (*name, NameSpelling::PdfDoc))
        .collect();
    build_unsigned_pdf_named(&named)
}

/// [`build_unsigned_pdf`] with each field's `/T` spelling chosen, so that a
/// test can build the UTF-16BE names a real form carries.
pub fn build_unsigned_pdf_named(fields: &[(&str, NameSpelling)]) -> Vec<u8> {
    assemble_single_revision(&unsigned_pdf_objects(fields))
}

/// The object bodies of [`build_unsigned_pdf_named`], in order from object 1,
/// so that every cross-reference shape assembles the same document.
fn unsigned_pdf_objects(fields: &[(&str, NameSpelling)]) -> Vec<String> {
    let field_first = 5u32;
    let refs: Vec<String> = (0..fields.len())
        .map(|i| format!("{} 0 R", field_first + i as u32))
        .collect();

    let mut objects = Vec::new();
    if fields.is_empty() {
        objects.push("<</Type /Catalog /Pages 2 0 R>>".to_string());
    } else {
        objects.push("<</Type /Catalog /Pages 2 0 R /AcroForm 4 0 R>>".to_string());
    }
    objects.push("<</Type /Pages /Kids [3 0 R] /Count 1>>".to_string());
    if fields.is_empty() {
        objects.push("<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>".to_string());
    } else {
        objects.push(format!(
            "<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [{}]>>",
            refs.join(" ")
        ));
        // Object 4 is the AcroForm even when unused, so that field object
        // numbers do not shift between the two shapes.
        objects.push(format!("<</Fields [{}] /SigFlags 3>>", refs.join(" ")));
        // A merged field/widget dictionary, which is what Acrobat writes for
        // essentially every real form field.
        for (name, spelling) in fields {
            objects.push(format!(
                "<</FT /Sig /T {} /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R>>",
                pdf_text_string(name, *spelling)
            ));
        }
    }
    objects
}

/// One empty signature field in a multi-page fixture: which page carries its
/// widget, and the box the document's author drew for it.
pub struct FixtureField<'a> {
    pub name: &'a str,
    /// Zero-based page index.
    pub page: usize,
    /// `[x0, y0, x1, y1]`; `[0.0; 4]` is the degenerate placeholder shape a
    /// sender's tool writes for an invisible field.
    pub rect: [f64; 4],
}

/// One page of a fixture: its boxes and its rotation.
///
/// The crop box and the rotation are the two things that make PDF *user*
/// space differ from what a reader sees, which is exactly what a placement
/// bug hides behind — so a fixture that can express them is what a placement
/// test needs.
#[derive(Debug, Clone, Copy)]
pub struct FixturePage {
    pub media_box: [f64; 4],
    /// `None` writes no `/CropBox`, so the media box is the crop box.
    pub crop_box: Option<[f64; 4]>,
    /// `/Rotate`, in degrees clockwise: 0, 90, 180 or 270.
    pub rotate: i32,
}

impl Default for FixturePage {
    fn default() -> Self {
        FixturePage {
            media_box: [0.0, 0.0, 612.0, 792.0],
            crop_box: None,
            rotate: 0,
        }
    }
}

impl FixturePage {
    pub fn rotated(degrees: i32) -> FixturePage {
        FixturePage {
            rotate: degrees,
            ..FixturePage::default()
        }
    }

    /// A page whose crop box is inset from the media box, so that the crop
    /// origin is not the user-space origin.
    pub fn cropped(crop_box: [f64; 4]) -> FixturePage {
        FixturePage {
            crop_box: Some(crop_box),
            ..FixturePage::default()
        }
    }

    fn dictionary(&self, annots: &[String]) -> String {
        let [x0, y0, x1, y1] = self.media_box;
        let mut out = format!("<</Type /Page /Parent 2 0 R /MediaBox [{x0} {y0} {x1} {y1}]");
        if let Some([cx0, cy0, cx1, cy1]) = self.crop_box {
            out.push_str(&format!(" /CropBox [{cx0} {cy0} {cx1} {cy1}]"));
        }
        if self.rotate != 0 {
            out.push_str(&format!(" /Rotate {}", self.rotate));
        }
        if !annots.is_empty() {
            out.push_str(&format!(" /Annots [{}]", annots.join(" ")));
        }
        out.push_str(">>");
        out
    }
}

/// An unsigned PDF of `page_count` pages carrying `fields` empty `/Sig`
/// fields, each with its own `/Rect` on its own page.
///
/// This is the shape the "click the sign-here box on the last page" flow
/// needs: a field that already has a box, on a page that is not page 0.
pub fn build_unsigned_pdf_multipage(page_count: usize, fields: &[FixtureField]) -> Vec<u8> {
    let pages = vec![FixturePage::default(); page_count];
    build_unsigned_pdf_pages(&pages, fields)
}

/// [`build_unsigned_pdf_multipage`] with each page described in full, so a
/// test can give a page a crop box or a `/Rotate`.
pub fn build_unsigned_pdf_pages(pages: &[FixturePage], fields: &[FixtureField]) -> Vec<u8> {
    let page_count = pages.len();
    assert!(page_count >= 1, "a PDF has at least one page");
    // 1 catalog, 2 page tree, 3..=2+page_count pages, then the AcroForm and
    // the fields.
    let first_page = 3u32;
    let acroform = first_page + page_count as u32;
    let first_field = acroform + 1;
    let page_object = |page: usize| first_page + page as u32;

    let kids: Vec<String> = (0..page_count)
        .map(|p| format!("{} 0 R", page_object(p)))
        .collect();
    let field_refs: Vec<String> = (0..fields.len())
        .map(|i| format!("{} 0 R", first_field + i as u32))
        .collect();

    let mut objects = Vec::new();
    if fields.is_empty() {
        objects.push("<</Type /Catalog /Pages 2 0 R>>".to_string());
    } else {
        objects.push(format!(
            "<</Type /Catalog /Pages 2 0 R /AcroForm {acroform} 0 R>>"
        ));
    }
    objects.push(format!(
        "<</Type /Pages /Kids [{}] /Count {}>>",
        kids.join(" "),
        page_count
    ));
    for (page, description) in pages.iter().enumerate() {
        let annots: Vec<String> = fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.page == page)
            .map(|(i, _)| field_refs[i].clone())
            .collect();
        objects.push(description.dictionary(&annots));
    }
    // The AcroForm object is emitted even with no fields so that object
    // numbers do not shift between the two shapes.
    objects.push(format!(
        "<</Fields [{}] /SigFlags 3>>",
        field_refs.join(" ")
    ));
    for field in fields {
        objects.push(format!(
            "<</FT /Sig /T ({}) /Type /Annot /Subtype /Widget /Rect [{} {} {} {}] /F 132 /P {} 0 R>>",
            field.name,
            field.rect[0],
            field.rect[1],
            field.rect[2],
            field.rect[3],
            page_object(field.page)
        ));
    }
    assemble_single_revision(&objects)
}

/// An unsigned two-field PDF in which `Sig1` carries a signature dictionary
/// whose FieldMDP transform locks `Sig2`.
///
/// The signature dictionary is a stub: the pre-flight refusal this fixture
/// exists to trigger happens before any cryptography is consulted.
pub fn build_pdf_with_fieldmdp_lock() -> Vec<u8> {
    let objects = vec![
        "<</Type /Catalog /Pages 2 0 R /AcroForm 4 0 R>>".to_string(),
        "<</Type /Pages /Kids [3 0 R] /Count 1>>".to_string(),
        "<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R 6 0 R]>>".to_string(),
        "<</Fields [5 0 R 6 0 R] /SigFlags 3>>".to_string(),
        "<</FT /Sig /T (Sig1) /V 7 0 R /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R>>".to_string(),
        "<</FT /Sig /T (Sig2) /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R>>".to_string(),
        "<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /Contents <00> \
          /ByteRange [0 0 0 0] /Reference [<< /Type /SigRef /TransformMethod /FieldMDP \
          /TransformParams << /Type /TransformParams /Action /Include /Fields [(Sig2)] /V /1.2 >> >>]>>".to_string(),
    ];
    assemble_single_revision(&objects)
}

/// Build a two-revision PDF whose second revision carries a signature over the
/// whole file.
pub fn build_signed_pdf(cred: &sign::Credential) -> SignedFixture {
    // Estimate CMS size per §23.5
    let test_digest = vec![0u8; 32];
    let bytes_reserved = sign::estimate_cms_size(
        cred,
        &test_digest,
        SigningProfile::AdbePkcs7Detached,
        false,
        false,
        Some(SIGNING_TIME_UNIX),
    )
    .expect("failed to estimate CMS size");

    let mut output = Vec::new();

    output.extend_from_slice(b"%PDF-1.4\n");

    // Object 1: Catalog (initial, no AcroForm yet)
    let obj1_pos = output.len();
    output.extend_from_slice(b"1 0 obj\n");
    output.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R>>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 2: Pages
    let obj2_pos = output.len();
    output.extend_from_slice(b"2 0 obj\n");
    output.extend_from_slice(b"<</Type /Pages /Kids [3 0 R] /Count 1>>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 3: Page
    let obj3_pos = output.len();
    output.extend_from_slice(b"3 0 obj\n");
    output.extend_from_slice(b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\n");
    output.extend_from_slice(b"endobj\n");

    // xref and trailer for initial revision
    let xref1_start = output.len();
    output.extend_from_slice(b"xref\n");
    output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
    output.extend_from_slice(format!("1 1\n{:010} 00000 n \n", obj1_pos).as_bytes());
    output.extend_from_slice(format!("2 1\n{:010} 00000 n \n", obj2_pos).as_bytes());
    output.extend_from_slice(format!("3 1\n{:010} 00000 n \n", obj3_pos).as_bytes());
    output.extend_from_slice(b"trailer\n");
    output.extend_from_slice(b"<<\n");
    output.extend_from_slice(b"/Size 4\n");
    output.extend_from_slice(b"/Root 1 0 R\n");
    output.extend_from_slice(
        b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
    );
    output.extend_from_slice(b">>\n");
    output.extend_from_slice(b"startxref\n");
    output.extend_from_slice(format!("{}\n", xref1_start).as_bytes());
    output.extend_from_slice(b"%%EOF");

    // Now start the signing revision

    // Object 4: Updated Catalog with /AcroForm reference (not inline)
    let obj4_start = output.len();
    output.extend_from_slice(b"4 0 obj\n");
    output.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 5: AcroForm dictionary with /Fields array
    let obj5_start = output.len();
    output.extend_from_slice(b"5 0 obj\n");
    output.extend_from_slice(b"<</Fields [6 0 R] /SigFlags 3 >>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 6: Signature field
    let obj6_start = output.len();
    output.extend_from_slice(b"6 0 obj\n");
    output.extend_from_slice(b"<</FT /Sig /T (Sig1) /V 7 0 R /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R >>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 7: Signature dictionary with placeholders per §23.2
    let obj7_start = output.len();
    output.extend_from_slice(b"7 0 obj\n");
    output
        .extend_from_slice(b"<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached ");

    // /ByteRange placeholder ([] + 60 spaces = 62 bytes)
    output.extend_from_slice(b"/ByteRange ");
    let byterange_start = output.len() as u64;
    output.extend_from_slice(b"[]");
    output.resize(output.len() + 60, b' ');

    // /Contents placeholder (< + bytes_reserved 0s + >)
    output.extend_from_slice(b"/Contents ");
    let sig_start = output.len() as u64;
    output.extend_from_slice(b"<");
    output.resize(output.len() + bytes_reserved, b'0');
    output.extend_from_slice(b">");
    let sig_end = output.len() as u64;

    // /M entry
    output.extend_from_slice(format!("/M ({}) ", MOD_DATE).as_bytes());

    output.extend_from_slice(b">>\n");
    output.extend_from_slice(b"endobj\n");

    // xref for signing revision - all new/modified objects with correct offsets
    let xref2_start = output.len();
    output.extend_from_slice(b"xref\n");
    output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
    output.extend_from_slice(format!("4 4\n{:010} 00000 n \n", obj4_start).as_bytes());
    output.extend_from_slice(format!("{:010} 00000 n \n", obj5_start).as_bytes());
    output.extend_from_slice(format!("{:010} 00000 n \n", obj6_start).as_bytes());
    output.extend_from_slice(format!("{:010} 00000 n \n", obj7_start).as_bytes());

    output.extend_from_slice(b"trailer\n");
    output.extend_from_slice(b"<<\n");
    output.extend_from_slice(b"/Size 8\n");
    output.extend_from_slice(b"/Root 4 0 R\n");
    output.extend_from_slice(format!("/Prev {}\n", xref1_start).as_bytes());
    output.extend_from_slice(
        b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
    );
    output.extend_from_slice(b">>\n");
    output.extend_from_slice(b"startxref\n");
    output.extend_from_slice(format!("{}\n", xref2_start).as_bytes());
    output.extend_from_slice(b"%%EOF");

    // Signing per §23.3: assembly order matters.
    let final_eof = output.len() as u64;

    // Step 1: back-patch /ByteRange BEFORE the digest.
    let byterange_str = format!("[0 {} {} {}]", sig_start, sig_end, final_eof - sig_end);
    for (i, byte) in byterange_str.as_bytes().iter().enumerate() {
        output[byterange_start as usize + i] = *byte;
    }

    // Step 2: compute the document digest.
    let mut hasher = Sha256::new();
    hasher.update(&output[0..sig_start as usize]);
    hasher.update(&output[sig_end as usize..final_eof as usize]);
    let document_digest = hasher.finalize().to_vec();

    // Step 3: build the CMS over the digest.
    let cms_bytes = sign::build_cms(
        cred,
        &document_digest,
        SigningProfile::AdbePkcs7Detached,
        false,
        None,
        Some(SIGNING_TIME_UNIX),
    )
    .expect("failed to build CMS");

    // Step 4: fill the reservation; the remaining hex digits stay '0' (§23.4).
    for (i, byte) in cms_bytes.iter().enumerate() {
        let pos = sig_start as usize + 1 + i * 2;
        let hex = format!("{:02X}", byte);
        output[pos] = hex.as_bytes()[0];
        output[pos + 1] = hex.as_bytes()[1];
    }

    SignedFixture {
        bytes: output,
        sig_start,
        sig_end,
        cms_len: cms_bytes.len(),
    }
}

/// A request with the fixed inputs the app layer would otherwise supply: the
/// signing time and the trailer's new `/ID` randomness. `pulpit-render` reads
/// neither a clock nor an entropy source, so tests pin both.
///
/// `reason` is the one thing each test binary wants to say for itself.
pub fn request_because(field: sign::SignTarget, reason: &str) -> sign::SignRequest {
    sign::SignRequest {
        signing_time: SIGNING_TIME_UNIX,
        field,
        reason: Some(reason.to_string()),
        location: Some("Montréal".to_string()),
        id2: [
            0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18, 0x29, 0x3A, 0x4B, 0x5C, 0x6D, 0x7E,
            0x8F, 0x90,
        ],
        ..sign::SignRequest::default()
    }
}

/// Verify every signature in `bytes`, insisting each one is at least readable.
///
/// A broken signature is a failure of the fixture rather than a result to
/// assert on: the tests that use this are checking *what* a verified signature
/// says, and one that cannot be parsed means the test never got that far.
pub fn statuses(bytes: &[u8]) -> Vec<pulpit_render::verify::SignatureStatus> {
    pulpit_render::verify::verify_signatures(bytes)
        .expect("verification runs")
        .into_iter()
        .map(|v| match v {
            pulpit_render::verify::SignatureVerification::Checked(status) => *status,
            pulpit_render::verify::SignatureVerification::Broken { field_name, reason } => {
                panic!("signature '{field_name}' is broken: {reason}")
            }
        })
        .collect()
}

/// Where `make sign-oracle` looks for PDFs to hand to pyHanko.
pub fn oracle_fixture_path(name: &str) -> PathBuf {
    let directory = std::path::Path::new("../../tools/sign-oracle/fixtures");
    std::fs::create_dir_all(directory).expect("create the oracle fixtures directory");
    directory.join(name)
}
