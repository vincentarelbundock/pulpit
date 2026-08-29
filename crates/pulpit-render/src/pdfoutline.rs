//! Serialising an edited bookmark tree back into a PDF's `/Outlines`.
//!
//! The one write path bookmarks have. Reading them is PDFium's job
//! (`FPDFBookmark_*`), but that family is read-only, so an edited
//! [`Outline`](pulpit_core::navigation::Outline) reaches the file the way the
//! PDF specification says a finished file is modified: an **incremental
//! update** (ISO 32000-2 §7.5.6) appended to the bytes the engine just saved —
//! a freshly numbered outline item per entry (§12.3.3), a new `/Outlines`
//! root, and the catalog re-emitted under its own object number to point at
//! it. The superseded tree becomes ordinary unreferenced garbage, which is
//! what an incremental update makes of everything it replaces.
//!
//! Like `sign::apply`, this module composes `verify` (to find the catalog and
//! walk the page tree) with `pdfwrite` (to append the update); `pdfwrite`
//! itself stays free of both.

use pulpit_core::document::LinkTarget;
use pulpit_core::navigation::{Outline, OutlineEntry};

use crate::pdfwrite::{IncrementalWriter, PdfObject};
use crate::sign::apply::{page_objects, parse_object_dictionary, set_entry};

/// Why an outline could not be appended. Nothing has been written when any of
/// these comes back: the caller still holds the bytes it started from.
#[derive(Debug, thiserror::Error)]
pub enum OutlineWriteError {
    /// Appending plaintext objects to an encrypted file produces a file no
    /// reader accepts, so it is refused before anything is built.
    #[error("the document is encrypted; its bookmarks cannot be rewritten")]
    Encrypted,
    #[error("the document could not be parsed: {0}")]
    Parse(String),
    #[error(transparent)]
    Write(#[from] crate::pdfwrite::PdfWriteError),
}

/// Append an incremental update that makes `outline` the document's bookmark
/// tree.
///
/// Pure on its inputs: the same bytes and the same outline produce the same
/// output, including the trailer's second `/ID` element, which is derived
/// from the content the way §14.4 suggests rather than drawn from a clock.
pub fn with_outline(bytes: &[u8], outline: &Outline) -> Result<Vec<u8>, OutlineWriteError> {
    if crate::verify::is_encrypted(bytes) {
        return Err(OutlineWriteError::Encrypted);
    }
    let writer = IncrementalWriter::open(bytes)?;

    let parse = |what: &dyn std::fmt::Display| OutlineWriteError::Parse(what.to_string());
    let catalog = crate::verify::find_catalog_ref(bytes).map_err(|e| parse(&e))?;
    let mut catalog_entries = parse_object_dictionary(bytes, catalog.0).map_err(|e| parse(&e))?;
    let pages = page_objects(bytes, &catalog_entries).map_err(|e| parse(&e))?;

    // Numbers first, dictionaries second: an item names its parent, both
    // neighbours and both ends of its child list, so every number must exist
    // before any dictionary is built.
    let mut next_number = writer.next_object_number();
    let mut allocate = || {
        let number = next_number;
        next_number += 1;
        number
    };
    let root = allocate();
    let numbered = number_level(&outline.entries, &mut allocate);

    let mut objects: Vec<(u32, u16, PdfObject)> = Vec::new();
    emit_level(&outline.entries, &numbered, root, &pages, &mut objects);

    // The outline root (§12.3.3): its `/Count` is the total number of visible
    // items, and every item this module writes is written open.
    let mut root_dictionary = vec![("Type".to_string(), PdfObject::Name("Outlines".to_string()))];
    if let (Some(first), Some(last)) = (numbered.first(), numbered.last()) {
        root_dictionary.push(("First".to_string(), reference(first.number)));
        root_dictionary.push(("Last".to_string(), reference(last.number)));
        root_dictionary.push((
            "Count".to_string(),
            PdfObject::Integer(outline.len() as i64),
        ));
    }
    objects.push((root, 0, PdfObject::Dictionary(root_dictionary)));

    // The catalog is re-emitted under its own object number — the newest
    // revision's cross-reference wins — so the trailer's `/Root` needs no
    // change at all.
    set_entry(&mut catalog_entries, "Outlines", reference(root));
    objects.push((catalog.0, 0, PdfObject::Dictionary(catalog_entries)));

    // The classic xref path requires its input sorted by object number.
    objects.sort_by_key(|(number, _, _)| *number);

    let id2 = content_id(bytes, outline);
    let mut cursor = std::io::Cursor::new(Vec::new());
    writer.append_objects(&mut cursor, &objects, &id2)?;
    Ok(cursor.into_inner())
}

/// One entry's allocated object number, and its children's.
struct Numbered {
    number: u32,
    children: Vec<Numbered>,
}

/// Give every entry of a level, and its subtree, an object number in
/// pre-order.
fn number_level(entries: &[OutlineEntry], allocate: &mut impl FnMut() -> u32) -> Vec<Numbered> {
    entries
        .iter()
        .map(|entry| Numbered {
            number: allocate(),
            children: number_level(&entry.children, allocate),
        })
        .collect()
}

/// Build the outline item dictionaries for one sibling level (§12.3.3,
/// Table 153).
fn emit_level(
    entries: &[OutlineEntry],
    numbered: &[Numbered],
    parent: u32,
    pages: &[u32],
    objects: &mut Vec<(u32, u16, PdfObject)>,
) {
    for (index, (entry, own)) in entries.iter().zip(numbered).enumerate() {
        let mut item = vec![
            (
                "Title".to_string(),
                PdfObject::String(entry.title.clone().into_bytes()),
            ),
            ("Parent".to_string(), reference(parent)),
        ];
        if index > 0 {
            item.push(("Prev".to_string(), reference(numbered[index - 1].number)));
        }
        if index + 1 < numbered.len() {
            item.push(("Next".to_string(), reference(numbered[index + 1].number)));
        }
        if let (Some(first), Some(last)) = (own.children.first(), own.children.last()) {
            item.push(("First".to_string(), reference(first.number)));
            item.push(("Last".to_string(), reference(last.number)));
            item.push((
                "Count".to_string(),
                PdfObject::Integer((subtree_len(entry) - 1) as i64),
            ));
        }
        match &entry.target {
            // An explicit destination; `/XYZ null null null` keeps the
            // viewer's position and zoom, which is all the model records.
            LinkTarget::Page { page, .. } => {
                // A page the document no longer has orders nothing; the title
                // survives, the way a dangling authored bookmark's would.
                if let Some(&page_object) = pages.get(*page) {
                    item.push((
                        "Dest".to_string(),
                        PdfObject::Array(vec![
                            reference(page_object),
                            PdfObject::Name("XYZ".to_string()),
                            PdfObject::Null,
                            PdfObject::Null,
                            PdfObject::Null,
                        ]),
                    ));
                }
            }
            LinkTarget::Uri(uri) => {
                item.push((
                    "A".to_string(),
                    PdfObject::Dictionary(vec![
                        ("S".to_string(), PdfObject::Name("URI".to_string())),
                        (
                            "URI".to_string(),
                            PdfObject::String(uri.clone().into_bytes()),
                        ),
                    ]),
                ));
            }
        }
        objects.push((own.number, 0, PdfObject::Dictionary(item)));
        emit_level(&entry.children, &own.children, own.number, pages, objects);
    }
}

fn reference(obj_num: u32) -> PdfObject {
    PdfObject::IndirectRef {
        obj_num,
        gen_num: 0,
    }
}

/// Total entries in one entry's subtree, itself included.
fn subtree_len(entry: &OutlineEntry) -> usize {
    1 + entry.children.iter().map(subtree_len).sum::<usize>()
}

/// The update's second `/ID` element, derived from what is being written.
///
/// §14.4 computes identifiers from the file's contents; deriving rather than
/// drawing randomness also keeps this module deterministic, which is what its
/// tests and the crate's conventions want of it. Two saves of identical
/// content produce identical identifiers, which is harmless — they are
/// identical files.
fn content_id(bytes: &[u8], outline: &Outline) -> [u8; 16] {
    use std::hash::{Hash, Hasher};
    let mut halves = [0u64; 2];
    for (salt, half) in halves.iter_mut().enumerate() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (salt as u64).hash(&mut hasher);
        bytes.hash(&mut hasher);
        for entry in outline.flattened() {
            entry.title.hash(&mut hasher);
            entry.depth.hash(&mut hasher);
            entry.page().unwrap_or(usize::MAX).hash(&mut hasher);
        }
        *half = hasher.finish();
    }
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&halves[0].to_be_bytes());
    id[8..].copy_from_slice(&halves[1].to_be_bytes());
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{ObjectResolver, PdfValue};

    // The shared fixture builder, the way `pdfwrite`'s tests take it: not
    // every helper is used here, so the module carries the allowance.
    #[allow(dead_code)]
    mod builder {
        include!("../tests/testkit/builder.rs");
    }
    use builder::Pdf;

    fn three_pages() -> Vec<u8> {
        let mut pdf = Pdf::new();
        pdf.add("<< /Type /Catalog /Pages 2 0 R >>");
        pdf.add("<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
        for _ in 0..3 {
            pdf.add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
        }
        pdf.build()
    }

    fn entry(title: &str, page: usize, depth: usize, children: Vec<OutlineEntry>) -> OutlineEntry {
        OutlineEntry {
            title: title.to_string(),
            target: LinkTarget::Page { page, zoom: None },
            depth,
            children,
        }
    }

    /// The dictionary of the object a reference names.
    fn dict_of(resolver: &ObjectResolver<'_>, value: &PdfValue) -> crate::verify::Dict {
        match resolver.deref(value).expect("deref") {
            PdfValue::Dict(dict) => dict,
            other => panic!("expected a dictionary, got {other:?}"),
        }
    }

    #[test]
    fn the_written_tree_walks_back_in_order_with_its_links_intact() {
        let bytes = three_pages();
        let outline = Outline {
            entries: vec![
                entry(
                    "Méthode",
                    0,
                    0,
                    vec![
                        entry("Setup", 1, 1, Vec::new()),
                        entry("Results", 2, 1, Vec::new()),
                    ],
                ),
                entry("Conclusion", 2, 0, Vec::new()),
            ],
        };
        let updated = with_outline(&bytes, &outline).expect("append");
        assert!(
            updated.starts_with(&bytes),
            "an incremental update leaves every original byte in place"
        );

        let resolver = ObjectResolver::new(&updated);
        let root_ref = resolver.root_ref().expect("root");
        let catalog = dict_of(&resolver, &PdfValue::Ref(root_ref.0, root_ref.1));
        let outlines = dict_of(&resolver, catalog.get("Outlines").expect("/Outlines"));
        assert_eq!(
            outlines.get("Type").and_then(PdfValue::as_name),
            Some("Outlines")
        );
        assert_eq!(outlines.get("Count").and_then(PdfValue::as_i64), Some(4));

        // Walk First/Next through the top level.
        let first = dict_of(&resolver, outlines.get("First").expect("/First"));
        let title = first.get("Title").expect("/Title");
        let PdfValue::Str(title_bytes) = title else {
            panic!("a /Title is a string");
        };
        assert_eq!(
            crate::pdftext::decode_text_string(title_bytes),
            "Méthode",
            "a non-ASCII title survives as UTF-16BE"
        );
        assert_eq!(first.get("Count").and_then(PdfValue::as_i64), Some(2));

        let second = dict_of(&resolver, first.get("Next").expect("/Next"));
        let PdfValue::Str(second_title) = second.get("Title").expect("/Title") else {
            panic!("a /Title is a string");
        };
        assert_eq!(
            crate::pdftext::decode_text_string(second_title),
            "Conclusion"
        );
        assert!(
            !second.contains_key("Next"),
            "the last sibling has no /Next"
        );

        // The child level: parents point up, destinations point at real
        // page objects.
        let first_ref = outlines
            .get("First")
            .and_then(PdfValue::as_ref_pair)
            .expect("/First is a reference");
        let child = dict_of(&resolver, first.get("First").expect("child /First"));
        assert_eq!(
            child.get("Parent").and_then(PdfValue::as_ref_pair),
            Some(first_ref),
            "a child's /Parent is its own parent item"
        );
        let PdfValue::Array(dest) = resolver.deref(child.get("Dest").expect("/Dest")).unwrap()
        else {
            panic!("a /Dest is an array");
        };
        assert_eq!(
            dest.first().and_then(PdfValue::as_ref_pair),
            Some((4, 0)),
            "Setup points at page two, which the fixture numbers 4"
        );
        assert_eq!(dest.get(1).and_then(PdfValue::as_name), Some("XYZ"));
    }

    #[test]
    fn an_empty_outline_writes_an_empty_root() {
        let bytes = three_pages();
        let updated = with_outline(&bytes, &Outline::default()).expect("append");
        let resolver = ObjectResolver::new(&updated);
        let root_ref = resolver.root_ref().expect("root");
        let catalog = dict_of(&resolver, &PdfValue::Ref(root_ref.0, root_ref.1));
        let outlines = dict_of(&resolver, catalog.get("Outlines").expect("/Outlines"));
        assert!(!outlines.contains_key("First"));
        assert!(!outlines.contains_key("Count"));
    }

    #[test]
    fn a_uri_bookmark_carries_an_action_and_a_dangling_page_no_destination() {
        let bytes = three_pages();
        let outline = Outline {
            entries: vec![
                OutlineEntry {
                    title: "Homepage".to_string(),
                    target: LinkTarget::Uri("https://example.org".to_string()),
                    depth: 0,
                    children: Vec::new(),
                },
                entry("Gone", 9, 0, Vec::new()),
            ],
        };
        let updated = with_outline(&bytes, &outline).expect("append");
        let resolver = ObjectResolver::new(&updated);
        let root_ref = resolver.root_ref().expect("root");
        let catalog = dict_of(&resolver, &PdfValue::Ref(root_ref.0, root_ref.1));
        let outlines = dict_of(&resolver, catalog.get("Outlines").expect("/Outlines"));
        let first = dict_of(&resolver, outlines.get("First").expect("/First"));
        let action = dict_of(&resolver, first.get("A").expect("/A"));
        assert_eq!(action.get("S").and_then(PdfValue::as_name), Some("URI"));
        let second = dict_of(&resolver, first.get("Next").expect("/Next"));
        assert!(!second.contains_key("Dest"), "page 9 does not exist");
        assert!(second.contains_key("Title"), "the title still survives");
    }

    #[test]
    fn an_encrypted_document_is_refused_whole() {
        let mut pdf = Pdf::new();
        pdf.add("<< /Type /Catalog /Pages 2 0 R >>");
        pdf.add("<< /Type /Pages /Kids [] /Count 0 >>");
        pdf.add("<< /Filter /Standard >>");
        let bytes = pdf.build_with_trailer("/Size {size} /Root 1 0 R /Encrypt 3 0 R");
        assert!(matches!(
            with_outline(&bytes, &Outline::default()),
            Err(OutlineWriteError::Encrypted)
        ));
    }

    #[test]
    fn the_same_inputs_write_the_same_bytes() {
        let bytes = three_pages();
        let outline = Outline {
            entries: vec![entry("Introduction", 0, 0, Vec::new())],
        };
        assert_eq!(
            with_outline(&bytes, &outline).expect("first"),
            with_outline(&bytes, &outline).expect("second")
        );
    }
}
