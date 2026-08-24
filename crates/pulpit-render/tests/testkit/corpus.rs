//! A corpus of AcroForms that are each wrong, or unusual, in one named way.
//!
//! The public corpora (veraPDF, PDF.js, PDFium) exercise parsers and
//! renderers, which this project delegates to PDFium. What they do not cover
//! is the thing this project actually does: find fields, fill them, and write
//! them back. These cases do, one hazard at a time, so a failure names its
//! own cause.
//!
//! Every case must survive: opening it, filling what it offers, and exporting
//! must leave the process alive, the source file untouched, and either a
//! readable PDF or a clean error. Cases that also have a defensible correct
//! answer say so with [`Expect::Roundtrips`].

use super::builder::{stream_body, utf16_string, Page, Pdf};

/// What a case promises beyond mere survival.
#[derive(Debug, Clone, PartialEq)]
pub enum Expect {
    /// The document is malformed enough that any self-consistent reading is
    /// acceptable. Only the survival invariants apply.
    Survives,
    /// Filling `field` with `value` must produce an export that reports the
    /// same value when reopened.
    Roundtrips {
        field: &'static str,
        value: &'static str,
    },
    /// The field must be discovered but must not be writable.
    ReadOnly { field: &'static str },
}

pub struct Case {
    pub name: &'static str,
    /// What the case is testing, and why it is a hazard.
    pub note: &'static str,
    pub bytes: Vec<u8>,
    pub expect: Expect,
}

/// A one-page document with a form, built around a fixed object layout:
/// 1 catalog, 2 page tree, 3 font, 4 page, 5 contents, 6 and up for fields.
struct Doc {
    pdf: Pdf,
    page: Page,
}

impl Doc {
    fn new() -> Self {
        let mut pdf = Pdf::new();
        for _ in 0..5 {
            pdf.reserve();
        }
        Self {
            pdf,
            page: Page::default(),
        }
    }

    fn rotated(degrees: i64) -> Self {
        let mut doc = Self::new();
        doc.page.rotate = Some(degrees);
        doc
    }

    fn add(&mut self, body: impl AsRef<[u8]>) -> u32 {
        self.pdf.add(body)
    }

    fn finish(self, acroform: &str, annots: &str) -> Vec<u8> {
        self.finish_with_pages(acroform, &[annots])
    }

    /// Finish with a trailer of the caller's choosing, for cases whose damage
    /// is in the trailer rather than in an object.
    fn finish_with_trailer(mut self, acroform: &str, annots: &str, trailer: &str) -> Vec<u8> {
        self.fill_skeleton(acroform, &[annots], vec![4]);
        self.pdf.build_with_trailer(trailer)
    }

    /// Finish with one page per entry in `annots`, all sharing the contents
    /// stream. The extra pages are appended after the field objects.
    fn finish_with_pages(mut self, acroform: &str, annots: &[&str]) -> Vec<u8> {
        let mut kids = vec![4u32];
        for annot in &annots[1..] {
            let page = self.page.dictionary(annot, 5);
            kids.push(self.pdf.add(page));
        }
        self.fill_skeleton(acroform, annots, kids);
        self.pdf.build()
    }

    /// Fill in the five fixed objects now that the field objects exist.
    fn fill_skeleton(&mut self, acroform: &str, annots: &[&str], kids: Vec<u32>) {
        let kid_refs = kids
            .iter()
            .map(|number| format!("{number} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        self.pdf.set(
            1,
            format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << {acroform} >> >>"),
        );
        self.pdf.set(
            2,
            format!(
                "<< /Type /Pages /Count {} /Kids [{kid_refs}] >>",
                kids.len()
            ),
        );
        self.pdf
            .set(3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let page = self.page.dictionary(annots[0], 5);
        self.pdf.set(4, page);
        self.pdf.set(
            5,
            stream_body("", b"BT /Helv 12 Tf 72 720 Td (pulpit corpus) Tj ET"),
        );
    }
}

/// A plain text widget, as its own field.
fn text_widget(name: &str, rect: &str, extra: &str) -> String {
    format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T ({name}) /V () /Rect [{rect}] \
         /P 4 0 R /F 4 /DA (/Helv 12 Tf 0 g) {extra} >>"
    )
}

fn case(name: &'static str, note: &'static str, bytes: Vec<u8>, expect: Expect) -> Case {
    Case {
        name,
        note,
        bytes,
        expect,
    }
}

/// Every case in the corpus.
pub fn corpus() -> Vec<Case> {
    let mut cases = Vec::new();

    // --- Baseline -------------------------------------------------------
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("name", "100 300 400 330", ""));
        cases.push(case(
            "plain-text-field",
            "The control: nothing wrong. If this fails, nothing below means anything.",
            doc.finish(
                &format!(
                    "/Fields [{field} 0 R] /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 3 0 R >> >>"
                ),
                &format!("{field} 0 R"),
            ),
            Expect::Roundtrips {
                field: "name",
                value: "Vincent",
            },
        ));
    }

    // --- Naming ---------------------------------------------------------
    {
        let mut doc = Doc::new();
        let first = doc.add(text_widget("name", "100 300 300 330", ""));
        let second = doc.add(text_widget("name", "100 200 300 230", ""));
        cases.push(case(
            "duplicate-field-names",
            "Two independent fields share /T. Per spec they are one field with \
             two widgets; readers disagree. Filling must not write one and \
             silently drop the other, nor loop.",
            doc.finish(
                &format!("/Fields [{first} 0 R {second} 0 R]"),
                &format!("{first} 0 R {second} 0 R"),
            ),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let parent = doc.pdf.reserve();
        let kid = doc.add(format!(
            "<< /Type /Annot /Subtype /Widget /Parent {parent} 0 R /T (child) \
             /Rect [100 300 300 330] /P 4 0 R /F 4 >>"
        ));
        doc.pdf.set(
            parent,
            format!("<< /FT /Tx /T (parent) /Kids [{kid} 0 R] /V () /DA (/Helv 12 Tf 0 g) >>"),
        );
        cases.push(case(
            "nested-field-tree",
            "A partial name under a parent. The field's real name is \
             'parent.child'; a reader that reports 'child' will fail to match \
             it on write.",
            doc.finish(&format!("/Fields [{parent} 0 R]"), &format!("{kid} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let grandparent = doc.pdf.reserve();
        let parent = doc.pdf.reserve();
        let kid = doc.add(format!(
            "<< /Type /Annot /Subtype /Widget /Parent {parent} 0 R /T (c) \
             /Rect [100 300 300 330] /P 4 0 R /F 4 >>"
        ));
        doc.pdf.set(
            parent,
            format!("<< /Parent {grandparent} 0 R /T (b) /Kids [{kid} 0 R] >>"),
        );
        doc.pdf.set(
            grandparent,
            format!("<< /FT /Tx /T (a) /Kids [{parent} 0 R] /V () /DA (/Helv 12 Tf 0 g) >>"),
        );
        cases.push(case(
            "three-level-field-tree",
            "'a.b.c'. Inherited /FT and /DA come from two levels up.",
            doc.finish(
                &format!("/Fields [{grandparent} 0 R]"),
                &format!("{kid} 0 R"),
            ),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let parent = doc.pdf.reserve();
        let kid = doc.pdf.reserve();
        doc.pdf.set(
            kid,
            format!(
                "<< /Type /Annot /Subtype /Widget /Parent {parent} 0 R /T (loop) \
                 /Kids [{parent} 0 R] /Rect [100 300 300 330] /P 4 0 R /F 4 >>"
            ),
        );
        doc.pdf.set(
            parent,
            format!("<< /FT /Tx /T (loop) /Kids [{kid} 0 R] /V () >>"),
        );
        cases.push(case(
            "cyclic-field-tree",
            "Parent and kid list each other. A name-building walk that does not \
             bound its depth hangs here — the one case in this corpus that can \
             cost a timeout rather than an error.",
            doc.finish(&format!("/Fields [{parent} 0 R]"), &format!("{kid} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(format!(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T {} /V () \
             /Rect [100 300 400 330] /P 4 0 R /F 4 /DA (/Helv 12 Tf 0 g) >>",
            utf16_string("naïve—名前")
        ));
        cases.push(case(
            "unicode-field-name",
            "A UTF-16BE field name. Matching on write compares whatever the \
             reader decoded against whatever the writer encodes.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /V () /Rect [100 300 400 330] \
             /P 4 0 R /F 4 /DA (/Helv 12 Tf 0 g) >>",
        );
        cases.push(case(
            "field-without-name",
            "No /T at all. The inspector synthesizes a placeholder name; that \
             name must not then be written back into the document as if real.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T 42 /V () /Rect [100 300 400 330] \
             /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "field-name-not-a-string",
            "/T is a number. Every accessor that assumes a string must decline \
             rather than unwrap.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }

    // --- Values and encodings -------------------------------------------
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("note", "100 300 400 330", ""));
        cases.push(case(
            "unicode-value",
            "Non-Latin text written into a field whose /DA names Helvetica, \
             which cannot encode it. The value must survive even if the \
             appearance cannot.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "note",
                value: "naïve — 名前 — 🙂",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("paren", "100 300 400 330", ""));
        cases.push(case(
            "value-with-pdf-syntax",
            "A value full of the delimiters that end a PDF string. If it is \
             written unescaped it corrupts the object graph, and the export \
             either fails to reopen or silently truncates.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "paren",
                value: r"a(b)c\d>>e<</f%g#h",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("limited", "100 300 400 330", "/MaxLen 5"));
        cases.push(case(
            "value-exceeding-maxlen",
            "/MaxLen 5 with a longer value. Truncating and not truncating are \
             both defensible; corrupting is not.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("empty", "100 300 400 330", ""));
        cases.push(case(
            "value-cleared-to-empty",
            "Writing the empty string is a real edit, not a no-op: it must not \
             leave the old value in place.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "empty",
                value: "",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (named) /V /SomeName \
             /Rect [100 300 400 330] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "text-value-is-a-name",
            "/V on a text field holds a name object rather than a string.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("multi", "100 200 400 330", "/Ff 4096"));
        cases.push(case(
            "multiline-text-field",
            "/Ff bit 13. Newlines in the value must not break the appearance.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "multi",
                value: "line one\nline two",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("secret", "100 300 400 330", "/Ff 8192"));
        cases.push(case(
            "password-field",
            "/Ff bit 14. A password field's value must not be written into a \
             visible appearance stream in cleartext.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }

    // --- Flags ----------------------------------------------------------
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget(
            "locked",
            "100 300 400 330",
            "/Ff 1 /V (original)",
        ));
        cases.push(case(
            "read-only-field",
            "/Ff bit 1. The export path checks this; the check is the test.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::ReadOnly { field: "locked" },
        ));
    }
    {
        let mut doc = Doc::new();
        let parent = doc.pdf.reserve();
        let kid = doc.add(format!(
            "<< /Type /Annot /Subtype /Widget /Parent {parent} 0 R \
             /Rect [100 300 300 330] /P 4 0 R /F 4 >>"
        ));
        doc.pdf.set(
            parent,
            format!("<< /FT /Tx /T (inherited) /Ff 1 /Kids [{kid} 0 R] /V (x) >>"),
        );
        cases.push(case(
            "read-only-inherited",
            "Read-only set on the parent, not the widget. A check that only \
             looks at the widget dictionary writes to a locked field.",
            doc.finish(&format!("/Fields [{parent} 0 R]"), &format!("{kid} 0 R")),
            Expect::ReadOnly { field: "inherited" },
        ));
    }

    // --- Appearances ----------------------------------------------------
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("bare", "100 300 400 330", ""));
        cases.push(case(
            "no-appearance-no-needappearances",
            "No /AP and no /NeedAppearances. A viewer that does not synthesize \
             appearances shows nothing — this is the case that catches an \
             export which only PDFium can read.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "bare",
                value: "Visible",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("bare", "100 300 400 330", ""));
        cases.push(case(
            "needappearances-true",
            "/NeedAppearances true asks the viewer to build appearances. Many \
             do not. The exported file should not depend on it.",
            doc.finish(
                &format!("/Fields [{field} 0 R] /NeedAppearances true /DA (/Helv 12 Tf 0 g) /DR << /Font << /Helv 3 0 R >> >>"),
                &format!("{field} 0 R"),
            ),
            Expect::Roundtrips { field: "bare", value: "Visible" },
        ));
    }
    {
        let mut doc = Doc::new();
        let appearance = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 300 30]",
            b"BT /Helv 12 Tf 2 8 Td (stale) Tj ET",
        ));
        let field = doc.add(text_widget(
            "stale",
            "100 300 400 330",
            &format!("/AP << /N {appearance} 0 R >>"),
        ));
        cases.push(case(
            "stale-appearance-stream",
            "An /AP that says 'stale' while /V will say something else. If the \
             appearance is not regenerated, the file prints the old value.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "stale",
                value: "fresh",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let appearance = doc.pdf.add(
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 300 30] /Length 9999 >>\nstream\nBT ET\nendstream",
        );
        let field = doc.add(text_widget(
            "broken",
            "100 300 400 330",
            &format!("/AP << /N {appearance} 0 R >>"),
        ));
        cases.push(case(
            "appearance-stream-bad-length",
            "/Length runs past the end of the file. The stream reader must \
             stop at the file's end rather than read past it.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget(
            "dangling",
            "100 300 400 330",
            "/AP << /N 999 0 R >>",
        ));
        cases.push(case(
            "appearance-reference-dangling",
            "/AP points at an object that does not exist.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget(
            "nodr",
            "100 300 400 330",
            "/DA (/Missing 12 Tf 0 g)",
        ));
        cases.push(case(
            "da-names-missing-font",
            "The /DA names a font absent from /DR. Building an appearance \
             requires a font that is not there.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("inheritda", "100 300 400 330", ""));
        cases.push(case(
            "da-inherited-from-acroform",
            "The widget has no /DA; the form-level default applies.",
            doc.finish(
                &format!(
                    "/Fields [{field} 0 R] /DA (/Helv 14 Tf 0 g) /DR << /Font << /Helv 3 0 R >> >>"
                ),
                &format!("{field} 0 R"),
            ),
            Expect::Roundtrips {
                field: "inheritda",
                value: "inherited",
            },
        ));
    }

    // --- Buttons --------------------------------------------------------
    {
        let mut doc = Doc::new();
        let on = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let off = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let field = doc.add(format!(
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /V /Off /AS /Off \
             /Rect [50 650 70 670] /P 4 0 R /F 4 \
             /AP << /N << /Yes {on} 0 R /Off {off} 0 R >> >> >>"
        ));
        cases.push(case(
            "checkbox-standard",
            "A checkbox whose on-state is /Yes.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "agree",
                value: "true",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let on = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let field = doc.add(format!(
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (odd) /V /Off /AS /Off \
             /Rect [50 650 70 670] /P 4 0 R /F 4 \
             /AP << /N << /Confirmé {on} 0 R /Off {on} 0 R >> >> >>"
        ));
        cases.push(case(
            "checkbox-nonstandard-on-state",
            "The on-state is not /Yes. Writing a hard-coded /Yes leaves the \
             box unchecked in every viewer.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (noap) /V /Off \
             /Rect [50 650 70 670] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "checkbox-without-appearance",
            "No /AP, so the on-state name cannot be discovered at all.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let on = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let off = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let field = doc.add(format!(
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (mismatch) /V /Yes /AS /Off \
             /Rect [50 650 70 670] /P 4 0 R /F 4 \
             /AP << /N << /Yes {on} 0 R /Off {off} 0 R >> >> >>"
        ));
        cases.push(case(
            "checkbox-value-appearance-mismatch",
            "/V says on, /AS says off. The value and what the reader sees \
             disagree before any edit; an export must resolve it, not keep it.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let parent = doc.pdf.reserve();
        let email_on = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let phone_on = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let off = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let first = doc.pdf.add(format!(
            "<< /Type /Annot /Subtype /Widget /Parent {parent} 0 R /Rect [50 650 70 670] \
             /P 4 0 R /F 4 /AP << /N << /Email {email_on} 0 R /Off {off} 0 R >> >> /AS /Email >>"
        ));
        let second = doc.pdf.add(format!(
            "<< /Type /Annot /Subtype /Widget /Parent {parent} 0 R /Rect [50 610 70 630] \
             /P 4 0 R /F 4 /AP << /N << /Phone {phone_on} 0 R /Off {off} 0 R >> >> /AS /Off >>"
        ));
        doc.pdf.set(
            parent,
            format!(
                "<< /FT /Btn /Ff 32768 /T (contact) /Kids [{first} 0 R {second} 0 R] /V /Email >>"
            ),
        );
        cases.push(case(
            "radio-group",
            "Two options under one group. Selecting the second must set the \
             group's /V and both kids' /AS.",
            doc.finish(
                &format!("/Fields [{parent} 0 R]"),
                &format!("{first} 0 R {second} 0 R"),
            ),
            Expect::Roundtrips {
                field: "contact",
                value: "Phone",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let parent = doc.pdf.reserve();
        let on = doc.pdf.add(stream_body(
            "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
            b"",
        ));
        let kid = doc.pdf.add(format!(
            "<< /Type /Annot /Subtype /Widget /Parent {parent} 0 R /Rect [50 650 70 670] \
             /P 4 0 R /F 4 /AP << /N << /Email {on} 0 R >> >> /AS /Email >>"
        ));
        doc.pdf.set(
            parent,
            format!("<< /FT /Btn /Ff 32768 /T (ghost) /Kids [{kid} 0 R] /V /Fax >>"),
        );
        cases.push(case(
            "radio-value-not-an-option",
            "/V names a state no kid offers. Every widget should end up /Off \
             rather than one being forced on.",
            doc.finish(&format!("/Fields [{parent} 0 R]"), &format!("{kid} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let parent = doc.pdf.reserve();
        doc.pdf.set(
            parent,
            "<< /FT /Btn /Ff 32768 /T (empty) /Kids [] /V /Off >>",
        );
        cases.push(case(
            "radio-group-without-kids",
            "A radio group with no options at all.",
            doc.finish(&format!("/Fields [{parent} 0 R]"), ""),
            Expect::Survives,
        ));
    }

    // --- Choice fields --------------------------------------------------
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (country) \
             /Opt [(Canada) (France)] /V (Canada) /I [0] /Rect [100 650 250 680] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "combo-box-plain-options",
            "Options are plain strings, so label and export value coincide.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "country",
                value: "France",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (country) \
             /Opt [[(CA) (Canada)] [(FR) (France)]] /V (CA) /I [0] \
             /Rect [100 650 250 680] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "combo-box-export-value-pairs",
            "Options are [export, label] pairs. The user picks the label; the \
             document must record the export value.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "country",
                value: "France",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 393216 /T (city) \
             /Opt [(Montreal) (Quebec)] /V (Montreal) /Rect [100 650 250 680] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "combo-box-editable",
            "/Ff bit 19 makes the combo editable, so a value outside /Opt is \
             legitimate and must not be coerced back into the list.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "city",
                value: "Trois-Rivières",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (locked) \
             /Opt [(A) (B)] /V (A) /Rect [100 650 250 680] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "combo-box-value-outside-options",
            "A non-editable combo told to hold something not in /Opt.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 2097152 /T (colour) \
             /Opt [(Red) (Blue) (Green)] /V [(Red)] /I [0] \
             /Rect [100 550 250 620] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "list-box-multi-select",
            "/Ff bit 22. /V is an array and /I must list the chosen indices, \
             in order, or viewers highlight the wrong rows.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "colour",
                value: r#"["Blue","Green"]"#,
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (empty) /Opt [] /V () \
             /Rect [100 650 250 680] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "choice-with-empty-options",
            "An empty /Opt. Index lookups have nothing to find.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (junk) \
             /Opt [(A) 42 null [(B)] << /C 1 >>] /V (A) /I [99] \
             /Rect [100 650 250 680] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "choice-options-of-wrong-types",
            "/Opt holds numbers, nulls, a one-element array and a dictionary, \
             and /I points past the end.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 2097152 /T (multi) \
             /Opt [(A) (B)] /V (A) /Rect [100 650 250 680] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "multi-select-with-scalar-value",
            "A multi-select whose /V is a bare string rather than an array — \
             the shape the writer's JSON decoding falls back from.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }

    // --- Structure ------------------------------------------------------
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("orphan", "100 300 400 330", ""));
        cases.push(case(
            "widget-not-in-acroform-fields",
            "The widget is on the page but absent from /Fields. It is still \
             drawn and still editable in most viewers.",
            doc.finish("/Fields []", &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("hidden", "100 300 400 330", ""));
        cases.push(case(
            "field-not-on-any-page",
            "Listed in /Fields but in no /Annots array. Filling it writes to \
             something the user never saw.",
            doc.finish(&format!("/Fields [{field} 0 R]"), ""),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (elsewhere) /V () \
             /Rect [100 300 400 330] /P 4 0 R /F 4 >>",
        );
        cases.push(case(
            "widget-page-pointer-wrong",
            "/P names page 1 while the widget is listed in page 2's /Annots. \
             A reader that trusts /P places the editor on the wrong page.",
            doc.finish_with_pages(
                &format!("/Fields [{field} 0 R]"),
                &["", &format!("{field} 0 R")],
            ),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (shared) /V () \
             /Rect [100 300 400 330] /F 4 >>",
        );
        cases.push(case(
            "same-widget-on-two-pages",
            "One widget object in two pages' /Annots, with no /P to break the \
             tie. It must be reported once per page or once overall — never \
             twice for the same page.",
            doc.finish_with_pages(
                &format!("/Fields [{field} 0 R]"),
                &[&format!("{field} 0 R"), &format!("{field} 0 R")],
            ),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("gone", "100 300 400 330", ""));
        cases.push(case(
            "fields-array-dangling-reference",
            "/Fields lists an object number that was never written.",
            doc.finish(
                &format!("/Fields [{field} 0 R 998 0 R 999 0 R]"),
                &format!("{field} 0 R"),
            ),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("noacro", "100 300 400 330", ""));
        let mut bytes = doc.finish("/Fields []", &format!("{field} 0 R"));
        // Strip the AcroForm entirely: a page of widgets and no form dictionary.
        let needle = b"/AcroForm << /Fields [] >>";
        if let Some(at) = bytes
            .windows(needle.len())
            .position(|window| window == needle)
        {
            bytes.splice(
                at..at + needle.len(),
                std::iter::repeat_n(b' ', needle.len()),
            );
        }
        cases.push(case(
            "no-acroform-dictionary",
            "Widget annotations with no /AcroForm at all. The catalog offset \
             stays valid because the entry is blanked, not removed.",
            bytes,
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("xfa", "100 300 400 330", ""));
        let xfa = doc.pdf.add(stream_body(
            "",
            br#"<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template/></xdp:xdp>"#,
        ));
        cases.push(case(
            "xfa-alongside-acroform",
            "A hybrid form. The AcroForm values are authoritative for readers \
             that ignore XFA, and writing them must not be skipped because an \
             /XFA key is present.",
            doc.finish(
                &format!("/Fields [{field} 0 R] /XFA {xfa} 0 R"),
                &format!("{field} 0 R"),
            ),
            Expect::Survives,
        ));
    }

    // --- Geometry -------------------------------------------------------
    for degrees in [90, 180, 270, -90] {
        let mut doc = Doc::rotated(degrees);
        let field = doc.add(text_widget("turned", "100 300 400 330", ""));
        let name: &'static str = Box::leak(format!("rotated-page-{degrees}").into_boxed_str());
        cases.push(case(
            name,
            "A field on a rotated page. Its reported rectangle must be where \
             the reader sees it, and a mark placed there must land there.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "turned",
                value: "turned",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        doc.page.media_box = [100.0, 200.0, 712.0, 992.0];
        let field = doc.add(text_widget("offset", "200 500 500 530", ""));
        cases.push(case(
            "mediabox-with-offset-origin",
            "The page does not start at (0,0). Normalizing against width and \
             height alone, without subtracting the origin, misplaces the mark.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Roundtrips {
                field: "offset",
                value: "offset",
            },
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("inverted", "400 330 100 300", ""));
        cases.push(case(
            "widget-rect-inverted",
            "/Rect given right-to-left and top-to-bottom. Per spec it must be \
             normalized before use; a negative width otherwise propagates.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("flat", "100 300 100 300", ""));
        cases.push(case(
            "widget-rect-zero-area",
            "A degenerate rectangle. Any division by its width or height is a \
             NaN waiting to reach the layout.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("huge", "-1e9 -1e9 1e9 1e9", ""));
        cases.push(case(
            "widget-rect-absurdly-large",
            "A rectangle far outside the page, in scientific notation.",
            doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R")),
            Expect::Survives,
        ));
    }

    // --- File structure -------------------------------------------------
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("updated", "100 300 400 330", "/V (first)"));
        let base = doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R"));
        cases.push(case(
            "incremental-update",
            "A second revision appended after the first, overriding the field's \
             value. A reader that takes the first definition it finds reports \
             the superseded value.",
            append_incremental_update(&base, field, "second"),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("badxref", "100 300 400 330", ""));
        let mut bytes = doc.finish(&format!("/Fields [{field} 0 R]"), &format!("{field} 0 R"));
        // Point startxref at nothing, forcing reconstruction from the objects.
        if let Some(at) = find_last(&bytes, b"startxref\n") {
            let digits = at + b"startxref\n".len();
            for byte in bytes[digits..]
                .iter_mut()
                .take_while(|b| b.is_ascii_digit())
            {
                *byte = b'9';
            }
        }
        cases.push(case(
            "startxref-points-nowhere",
            "The cross-reference offset is wrong, so the file is only readable \
             by scanning for objects. Recovery must still find the form.",
            bytes,
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let field = doc.add(text_widget("sized", "100 300 400 330", ""));
        cases.push(case(
            "trailer-size-too-small",
            "/Size understates the object count, so a reader that trusts it \
             stops before reaching the field.",
            doc.finish_with_trailer(
                &format!("/Fields [{field} 0 R]"),
                &format!("{field} 0 R"),
                "/Size 2 /Root 1 0 R",
            ),
            Expect::Survives,
        ));
    }
    {
        let mut doc = Doc::new();
        let mut annots = Vec::new();
        let mut fields = Vec::new();
        for index in 0..250 {
            let number = doc.add(text_widget(
                &format!("field{index}"),
                &format!("50 {} 250 {}", 700 - index % 60 * 10, 720 - index % 60 * 10),
                "",
            ));
            annots.push(format!("{number} 0 R"));
            fields.push(format!("{number} 0 R"));
        }
        cases.push(case(
            "many-fields",
            "250 fields. Anything quadratic in the field count — a linear scan \
             per field, say — starts to show here.",
            doc.finish(
                &format!("/Fields [{}]", fields.join(" ")),
                &annots.join(" "),
            ),
            Expect::Roundtrips {
                field: "field7",
                value: "seven",
            },
        ));
    }

    cases
}

/// Append a revision that changes one field's `/V`, as an incremental update:
/// the original bytes untouched, new objects after them, and a cross-reference
/// section that chains back with `/Prev`.
fn append_incremental_update(base: &[u8], field: u32, value: &str) -> Vec<u8> {
    let previous = find_last(base, b"startxref\n")
        .and_then(|at| {
            let digits = &base[at + b"startxref\n".len()..];
            let end = digits
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .unwrap_or(digits.len());
            std::str::from_utf8(&digits[..end])
                .ok()?
                .parse::<usize>()
                .ok()
        })
        .unwrap_or(0);

    let mut out = base.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let offset = out.len();
    out.extend_from_slice(
        format!(
            "{field} 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (updated) /V ({value}) \
             /Rect [100 300 400 330] /P 4 0 R /F 4 /DA (/Helv 12 Tf 0 g) >>\nendobj\n"
        )
        .as_bytes(),
    );
    let xref = out.len();
    out.extend_from_slice(
        format!(
            "xref\n{field} 1\n{offset:010} 00000 n \n\
             trailer\n<< /Size {} /Root 1 0 R /Prev {previous} >>\nstartxref\n{xref}\n%%EOF\n",
            field + 1
        )
        .as_bytes(),
    );
    out
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}
