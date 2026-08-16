//! Assembling small PDFs byte by byte.
//!
//! The corpus needs documents that are wrong in one specific, chosen way. No
//! PDF producer will emit those on request, so the tests write the file
//! format directly: an object per entry, a cross-reference table counted from
//! the actual byte offsets, and a trailer pointing at object 1 as the catalog.

/// A PDF built from numbered objects, where object `n` is the `n`-th body
/// added and object 1 is the catalog.
#[derive(Default, Clone)]
pub struct Pdf {
    objects: Vec<Vec<u8>>,
}

impl Pdf {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an object body, returning the object number it was given.
    pub fn add(&mut self, body: impl AsRef<[u8]>) -> u32 {
        self.objects.push(body.as_ref().to_vec());
        self.objects.len() as u32
    }

    /// Add a stream object, computing `/Length` from the content so the
    /// document is only as malformed as a case means it to be.
    pub fn add_stream(&mut self, dictionary: &str, content: &[u8]) -> u32 {
        self.add(stream_body(dictionary, content))
    }

    /// Reserve an object number before its body is known, so objects can refer
    /// to each other in a cycle. The placeholder must be filled with [`set`].
    pub fn reserve(&mut self) -> u32 {
        self.add(b"null")
    }

    pub fn set(&mut self, number: u32, body: impl AsRef<[u8]>) {
        self.objects[number as usize - 1] = body.as_ref().to_vec();
    }

    pub fn len(&self) -> u32 {
        self.objects.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Serialize with a correct cross-reference table.
    pub fn build(&self) -> Vec<u8> {
        self.build_with_trailer("/Size {size} /Root 1 0 R")
    }

    /// Serialize with a trailer of the caller's choosing. `{size}` expands to
    /// the object count plus one, so a case can corrupt the trailer on purpose
    /// without also having to count objects.
    pub fn build_with_trailer(&self, trailer: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(object);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", self.objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        let trailer = trailer.replace("{size}", &(self.objects.len() + 1).to_string());
        out.extend_from_slice(
            format!("trailer\n<< {trailer} >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        out
    }
}

/// A stream object body with `/Length` computed from the content.
pub fn stream_body(dictionary: &str, content: &[u8]) -> Vec<u8> {
    let mut body = format!("<< {dictionary} /Length {} >>\nstream\n", content.len()).into_bytes();
    body.extend_from_slice(content);
    body.extend_from_slice(b"\nendstream");
    body
}

/// A PDF text string holding `text` as UTF-16BE with a byte-order mark, the
/// only way the format carries anything outside PDFDoc encoding.
pub fn utf16_string(text: &str) -> String {
    let mut hex = String::from("<FEFF");
    for unit in text.encode_utf16() {
        hex.push_str(&format!("{unit:04X}"));
    }
    hex.push('>');
    hex
}

/// A minimal one-page document carrying `fields`, for cases whose point is the
/// field dictionaries rather than the page.
///
/// `extra_acroform` is spliced into the `/AcroForm` dictionary; `annots` is
/// the `/Annots` array body of the single page.
pub struct Page {
    pub rotate: Option<i64>,
    pub media_box: [f64; 4],
}

impl Default for Page {
    fn default() -> Self {
        Self {
            rotate: None,
            media_box: [0.0, 0.0, 612.0, 792.0],
        }
    }
}

impl Page {
    pub fn dictionary(&self, annots: &str, contents: u32) -> String {
        let rotate = self
            .rotate
            .map(|degrees| format!("/Rotate {degrees} "))
            .unwrap_or_default();
        let [x0, y0, x1, y1] = self.media_box;
        format!(
            "<< /Type /Page /Parent 2 0 R {rotate}/MediaBox [{x0} {y0} {x1} {y1}] \
             /Resources << /Font << /Helv 3 0 R >> >> /Contents {contents} 0 R /Annots [{annots}] >>"
        )
    }
}
