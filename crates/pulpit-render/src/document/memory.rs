//! A document that lives in memory.
//!
//! Two jobs, and the second is the reason it is not `#[cfg(test)]`:
//!
//! 1. It is what the revision, atomicity and undo tests in [`super`] run
//!    against, so those semantics are checked on every machine rather than
//!    only on one with PDFium installed.
//! 2. It is the document engine's counterpart to
//!    [`crate::pdf::fixture::FixtureBackend`] — the thing a developer without
//!    a PDF library still gets a working reader out of.
//!
//! It is *not* a second persistence format (A1). It has no file behind it
//! until it is asked to write one, and what it writes is a stub: a document
//! opened from a real PDF is always backed by the real engine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pulpit_core::annotate::{AnnotationDraft, AnnotationId, MarkStyle};
use pulpit_core::page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect};

use super::model::{
    AnnotationBeforeImage, AnnotationContents, AnnotationSummary, AnnotationSupport,
    CompatibilityLevel, DocumentWarning, FieldKind, FieldWidget, FormField, OpenDocumentInfo,
    SaveOptions, TextSelection, TextSelectionResult,
};
use super::{DocumentBackend, DocumentError, DocumentRevision, Result};

/// One annotation as this backend holds it.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    id: AnnotationId,
    draft: AnnotationDraft,
    support: AnnotationSupport,
    /// The bytes a real engine would have kept for entries it does not model
    /// (A5). Carried here so the undo path is exercised end to end rather than
    /// only where PDFium is installed.
    preserved: Vec<u8>,
}

pub struct MemoryDocument {
    info: OpenDocumentInfo,
    geometry: Vec<PageGeometry>,
    /// In `/Annots` order per page, which is paint order.
    annotations: Vec<Vec<Entry>>,
    fields: BTreeMap<String, FormField>,
    /// Field order as the document declares it, which is not alphabetical.
    field_order: Vec<String>,
    source: Option<PathBuf>,
}

impl MemoryDocument {
    /// `pages` US Letter pages, no form, no warnings.
    pub fn letter(pages: usize) -> MemoryDocument {
        let geometry = vec![PageGeometry::upright(612.0, 792.0); pages.max(1)];
        MemoryDocument {
            info: OpenDocumentInfo {
                page_count: geometry.len(),
                level: CompatibilityLevel::AnnotateOnly,
                warnings: Vec::new(),
                first_page: geometry[0],
                has_form: false,
            },
            annotations: vec![Vec::new(); geometry.len()],
            geometry,
            fields: BTreeMap::new(),
            field_order: Vec::new(),
            source: None,
        }
    }

    /// One page with a small AcroForm: a text field, a checkbox, a radio group
    /// and one field the document marks read-only.
    pub fn with_form() -> MemoryDocument {
        let mut document = MemoryDocument::letter(1);
        document.info.level = CompatibilityLevel::Native;
        document.info.has_form = true;
        document.add_field(FormField {
            name: "name".into(),
            kind: FieldKind::Text,
            value: String::new(),
            read_only: false,
            format: crate::document::model::FieldFormat::Plain,
            options: Vec::new(),
            allows_custom_value: true,
            multiple_selection: false,
            selected: Vec::new(),
            required: false,
            password: false,
            file_select: false,
            rich_text: false,
            truncated: false,
            hidden: false,
            widgets: vec![FieldWidget {
                page: PageIndex(0),
                bounds: PageRect::new(100.0, 100.0, 400.0, 124.0),
                option: None,
            }],
        });
        document.add_field(FormField {
            name: "agreed".into(),
            kind: FieldKind::Checkbox,
            value: "Off".into(),
            read_only: false,
            format: crate::document::model::FieldFormat::Plain,
            options: vec!["Yes".into(), "Off".into()],
            allows_custom_value: false,
            multiple_selection: false,
            selected: Vec::new(),
            required: false,
            password: false,
            file_select: false,
            rich_text: false,
            truncated: false,
            hidden: false,
            widgets: vec![FieldWidget {
                page: PageIndex(0),
                bounds: PageRect::new(100.0, 160.0, 116.0, 176.0),
                option: None,
            }],
        });
        document.add_field(FormField {
            name: "colour".into(),
            kind: FieldKind::RadioGroup,
            value: "red".into(),
            read_only: false,
            format: crate::document::model::FieldFormat::Plain,
            options: vec!["red".into(), "green".into()],
            allows_custom_value: false,
            multiple_selection: false,
            selected: Vec::new(),
            required: false,
            password: false,
            file_select: false,
            rich_text: false,
            truncated: false,
            hidden: false,
            widgets: vec![
                FieldWidget {
                    page: PageIndex(0),
                    bounds: PageRect::new(100.0, 200.0, 116.0, 216.0),
                    option: Some("red".into()),
                },
                FieldWidget {
                    page: PageIndex(0),
                    bounds: PageRect::new(160.0, 200.0, 176.0, 216.0),
                    option: Some("green".into()),
                },
            ],
        });
        document.add_field(FormField {
            name: "locked".into(),
            kind: FieldKind::Text,
            value: "computed".into(),
            read_only: true,
            format: crate::document::model::FieldFormat::Plain,
            options: Vec::new(),
            allows_custom_value: false,
            multiple_selection: false,
            selected: Vec::new(),
            required: false,
            password: false,
            file_select: false,
            rich_text: false,
            truncated: false,
            hidden: false,
            widgets: Vec::new(),
        });
        document
    }

    /// A document whose own permissions forbid changing it.
    pub fn locked() -> MemoryDocument {
        let mut document = MemoryDocument::letter(1);
        document.info.warnings.push(DocumentWarning::Encrypted);
        document
            .info
            .warnings
            .push(DocumentWarning::MutationForbidden);
        document
    }

    /// Say where this document came from, so Save As can refuse it (A6).
    pub fn with_source(mut self, source: PathBuf) -> MemoryDocument {
        self.source = Some(source);
        self
    }

    pub fn with_pages(geometry: Vec<PageGeometry>) -> MemoryDocument {
        let mut document = MemoryDocument::letter(geometry.len());
        document.info.first_page = geometry[0];
        document.geometry = geometry;
        document
    }

    pub fn add_field(&mut self, field: FormField) {
        self.field_order.push(field.name.clone());
        self.fields.insert(field.name.clone(), field);
    }

    /// Put an annotation in that pulpit did not write and cannot edit, as an
    /// imported one would be (§10.1). The A5 tests need one.
    pub fn add_imported(
        &mut self,
        page: PageIndex,
        id: AnnotationId,
        support: AnnotationSupport,
        bounds: PageRect,
        preserved: Vec<u8>,
    ) {
        let draft = AnnotationDraft::Stamp(pulpit_core::annotate::StampDraft {
            page,
            rect: bounds,
            mark: pulpit_core::annotate::StampMark::Check,
            style: MarkStyle::default(),
            source: None,
        });
        self.annotations[page.get()].push(Entry {
            id,
            draft,
            support,
            preserved,
        });
    }

    fn locate(&self, id: &AnnotationId) -> Option<(usize, usize)> {
        for (page, entries) in self.annotations.iter().enumerate() {
            if let Some(index) = entries.iter().position(|entry| entry.id == *id) {
                return Some((page, index));
            }
        }
        None
    }

    fn summarise(&self, entry: &Entry) -> AnnotationSummary {
        let draft = &entry.draft;
        let (path, quads) = match draft {
            AnnotationDraft::Ink(ink) => (
                ink.points.iter().map(|point| point.at).collect(),
                Vec::new(),
            ),
            AnnotationDraft::Highlight(highlight) => (Vec::new(), highlight.quads.clone()),
            _ => (Vec::<PagePoint>::new(), Vec::<PageQuad>::new()),
        };
        let text = match draft {
            AnnotationDraft::Highlight(highlight) => highlight.text.clone(),
            AnnotationDraft::FreeText(free) => free.text.clone(),
            AnnotationDraft::Note(note) => note.text.clone(),
            _ => String::new(),
        };
        AnnotationSummary {
            id: entry.id.clone(),
            page: draft.page(),
            kind: draft.kind(),
            bounds: draft.bounds().unwrap_or_default(),
            style: draft.style(),
            contents: AnnotationContents {
                text,
                truncated: false,
                pulpit_source: None,
            },
            support: entry.support,
            // A memory document has no notion of when a mark was made; the
            // engine's revision is stamped by the caller that reads it.
            revision: DocumentRevision::INITIAL,
            path,
            quads,
            geometry_elided: false,
        }
    }

    fn total(&self) -> usize {
        self.annotations.iter().map(Vec::len).sum()
    }
}

impl DocumentBackend for MemoryDocument {
    fn info(&self) -> &OpenDocumentInfo {
        &self.info
    }

    fn page_geometry(&self, page: PageIndex) -> Result<PageGeometry> {
        self.geometry
            .get(page.get())
            .copied()
            .ok_or(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.geometry.len(),
            })
    }

    fn annotations(&self, page: PageIndex) -> Result<Vec<AnnotationSummary>> {
        let entries = self
            .annotations
            .get(page.get())
            .ok_or(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.annotations.len(),
            })?;
        Ok(entries.iter().map(|entry| self.summarise(entry)).collect())
    }

    fn annotation(&self, id: &AnnotationId) -> Result<AnnotationSummary> {
        let (page, index) = self
            .locate(id)
            .ok_or_else(|| DocumentError::NoSuchAnnotation(id.clone()))?;
        Ok(self.summarise(&self.annotations[page][index]))
    }

    fn create(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary> {
        let page = draft.page();
        if page.get() >= self.annotations.len() {
            return Err(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.annotations.len(),
            });
        }
        super::limits::within(
            "annotations in a document",
            self.total() + 1,
            super::limits::MAX_ANNOTATIONS_PER_DOCUMENT,
        )?;
        let mut draft = draft.clone();
        draft.sanitise();
        let entry = Entry {
            id: id.clone(),
            draft,
            support: AnnotationSupport::Editable,
            preserved: Vec::new(),
        };
        // Appended, because a new mark is painted over what is already there.
        self.annotations[page.get()].push(entry);
        let entry = self.annotations[page.get()].last().expect("just pushed");
        Ok(self.summarise(entry))
    }

    fn replace(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary> {
        let (page, index) = self
            .locate(id)
            .ok_or_else(|| DocumentError::NoSuchAnnotation(id.clone()))?;
        if !self.annotations[page][index].support.is_editable() {
            return Err(DocumentError::NotEditable(id.clone()));
        }
        let mut draft = draft.clone();
        draft.sanitise();
        if draft.page() != PageIndex(page) {
            // Moving an annotation between pages is a delete and a create,
            // not a replace: the identity would have to be reissued.
            return Err(DocumentError::Backend(
                "an annotation cannot be replaced onto another page".into(),
            ));
        }
        self.annotations[page][index].draft = draft;
        Ok(self.summarise(&self.annotations[page][index]))
    }

    fn delete(&mut self, id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        let (page, index) = self
            .locate(id)
            .ok_or_else(|| DocumentError::NoSuchAnnotation(id.clone()))?;
        if !self.annotations[page][index].support.is_editable() {
            // A5: an unsupported annotation is not the eraser's to take.
            return Err(DocumentError::NotEditable(id.clone()));
        }
        let entry = self.annotations[page].remove(index);
        Ok(AnnotationBeforeImage {
            page: PageIndex(page),
            draft: Some(entry.draft),
            preserved: entry.preserved,
        })
    }

    fn restore(
        &mut self,
        id: &AnnotationId,
        before: &AnnotationBeforeImage,
    ) -> Result<AnnotationSummary> {
        let draft = before
            .draft
            .clone()
            .ok_or_else(|| DocumentError::NotEditable(id.clone()))?;
        let entry = Entry {
            id: id.clone(),
            draft,
            support: AnnotationSupport::Editable,
            preserved: before.preserved.clone(),
        };
        match self.locate(id) {
            // Restoring over an annotation that is still there is an undo of a
            // *replace*: the entry keeps its place in paint order.
            Some((page, index)) => {
                self.annotations[page][index] = entry;
                Ok(self.summarise(&self.annotations[page][index]))
            }
            None => {
                let page = before.page.get();
                if page >= self.annotations.len() {
                    return Err(DocumentError::NoSuchPage {
                        page,
                        count: self.annotations.len(),
                    });
                }
                self.annotations[page].push(entry);
                let entry = self.annotations[page].last().expect("just pushed");
                Ok(self.summarise(entry))
            }
        }
    }

    fn before_image(&self, id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        let (page, index) = self
            .locate(id)
            .ok_or_else(|| DocumentError::NoSuchAnnotation(id.clone()))?;
        let entry = &self.annotations[page][index];
        Ok(AnnotationBeforeImage {
            page: PageIndex(page),
            draft: Some(entry.draft.clone()),
            preserved: entry.preserved.clone(),
        })
    }

    fn fields(&self) -> Result<Vec<FormField>> {
        Ok(self
            .field_order
            .iter()
            .filter_map(|name| self.fields.get(name).cloned())
            .collect())
    }

    fn set_field(&mut self, name: &str, value: &str, selected: &[u32]) -> Result<String> {
        let field = self
            .fields
            .get_mut(name)
            .ok_or_else(|| DocumentError::NoSuchField(name.to_string()))?;
        if field.read_only {
            // The same refusal PDFium's engine gives, in the same words: a
            // fixture that answered differently would let a test pass here and
            // the application behave differently in front of a real document.
            return Err(DocumentError::FieldReadOnly(name.to_string()));
        }
        // A selection named by index restores exactly the options it names —
        // the case one string cannot carry, and the reason `selected` exists.
        if !selected.is_empty() {
            if selected
                .iter()
                .any(|index| *index as usize >= field.options.len())
            {
                return Err(DocumentError::Backend(format!(
                    "{name} has no such option to select"
                )));
            }
            field.selected = selected.to_vec();
            field.value = field.options[selected[0] as usize].clone();
            return Ok(field.value.clone());
        }
        // A choice field takes one of its options and nothing else: a document
        // that offers "Yes"/"Off" does not gain a third state because a caller
        // sent one.
        let offered = field.options.iter().position(|option| option == value);
        if field.options.is_empty() || field.allows_custom_value || offered.is_some() {
            let taken = value.to_string();
            field.value = taken.clone();
            field.selected = offered.into_iter().map(|index| index as u32).collect();
            return Ok(taken);
        }
        Err(DocumentError::Backend(format!(
            "{name} does not offer {value}"
        )))
    }

    fn field_value(&self, name: &str) -> Result<String> {
        self.fields
            .get(name)
            .map(|field| field.value.clone())
            .ok_or_else(|| DocumentError::NoSuchField(name.to_string()))
    }

    fn select_text(
        &self,
        page: PageIndex,
        _selection: TextSelection,
    ) -> Result<TextSelectionResult> {
        if page.get() >= self.geometry.len() {
            return Err(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.geometry.len(),
            });
        }
        // No text layer. An empty result, not an error (§6.3): the UI's answer
        // is "the highlighter is unavailable here", which is exactly true.
        Ok(TextSelectionResult::default())
    }

    fn area_text(&self, page: PageIndex, _rect: pulpit_core::page::PageRect) -> Result<String> {
        if page.get() >= self.geometry.len() {
            return Err(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.geometry.len(),
            });
        }
        // A memory document has no text layer, and here that has to be said
        // rather than answered with an empty string: the band that asked is
        // about to put the answer on the clipboard.
        Err(DocumentError::Unsupported(
            "have its text read: an in-memory document has no text layer".into(),
        ))
    }

    fn write_to(&mut self, destination: &Path, _options: SaveOptions) -> Result<u64> {
        // Enough of a PDF to be recognisably one, and no more: a memory
        // document is not a document anybody should be saving for real.
        let mut bytes = Vec::from(&b"%PDF-1.7\n% pulpit in-memory document\n"[..]);
        bytes.extend_from_slice(
            format!(
                "% {} pages, {} annotations\n",
                self.geometry.len(),
                self.total()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"%%EOF\n");
        std::fs::write(destination, &bytes)?;
        Ok(bytes.len() as u64)
    }

    fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::annotate::{IdGenerator, InkDraft, InkPoint};

    fn ink(page: usize) -> AnnotationDraft {
        AnnotationDraft::Ink(InkDraft {
            page: PageIndex(page),
            points: vec![InkPoint::new(10.0, 10.0), InkPoint::new(60.0, 40.0)],
            style: MarkStyle::default(),
        })
    }

    #[test]
    fn annotations_come_back_in_paint_order() {
        let mut document = MemoryDocument::letter(1);
        let mut generator = IdGenerator::new(0);
        let first = generator.next_id();
        let second = generator.next_id();
        document.create(&first, &ink(0)).unwrap();
        document.create(&second, &ink(0)).unwrap();
        let ids: Vec<AnnotationId> = document
            .annotations(PageIndex(0))
            .unwrap()
            .into_iter()
            .map(|summary| summary.id)
            .collect();
        assert_eq!(ids, vec![first, second], "the newest mark is painted last");
    }

    #[test]
    fn an_unsupported_annotation_is_neither_edited_nor_erased() {
        // A5, from the backend's own side.
        let mut document = MemoryDocument::letter(1);
        let id = AnnotationId::imported("acrobat-7").unwrap();
        document.add_imported(
            PageIndex(0),
            id.clone(),
            AnnotationSupport::Unsupported,
            PageRect::new(10.0, 10.0, 40.0, 40.0),
            b"<< /Vendor (private) >>".to_vec(),
        );
        assert!(matches!(
            document.delete(&id),
            Err(DocumentError::NotEditable(_))
        ));
        assert!(matches!(
            document.replace(&id, &ink(0)),
            Err(DocumentError::NotEditable(_))
        ));
        assert_eq!(document.annotations(PageIndex(0)).unwrap().len(), 1);
    }

    #[test]
    fn a_restore_keeps_an_annotations_place_in_paint_order() {
        let mut document = MemoryDocument::letter(1);
        let mut generator = IdGenerator::new(1);
        let (a, b, c) = (
            generator.next_id(),
            generator.next_id(),
            generator.next_id(),
        );
        for id in [&a, &b, &c] {
            document.create(id, &ink(0)).unwrap();
        }
        let before = document.before_image(&b).unwrap();
        document.replace(&b, &ink(0)).unwrap();
        document.restore(&b, &before).unwrap();
        let ids: Vec<AnnotationId> = document
            .annotations(PageIndex(0))
            .unwrap()
            .into_iter()
            .map(|summary| summary.id)
            .collect();
        assert_eq!(ids, vec![a, b, c]);
    }

    #[test]
    fn a_restore_carries_the_entries_pulpit_never_modelled() {
        let mut document = MemoryDocument::letter(1);
        let id = AnnotationId::imported("other-producer").unwrap();
        document.add_imported(
            PageIndex(0),
            id.clone(),
            AnnotationSupport::Editable,
            PageRect::new(10.0, 10.0, 40.0, 40.0),
            b"<< /Vendor (private) >>".to_vec(),
        );
        let before = document.before_image(&id).unwrap();
        assert_eq!(before.preserved, b"<< /Vendor (private) >>".to_vec());
        document.delete(&id).unwrap();
        document.restore(&id, &before).unwrap();
        assert_eq!(
            document.before_image(&id).unwrap().preserved,
            b"<< /Vendor (private) >>".to_vec()
        );
    }

    #[test]
    fn a_choice_field_takes_only_what_it_offers() {
        let mut document = MemoryDocument::with_form();
        assert_eq!(document.set_field("agreed", "Yes", &[]).unwrap(), "Yes");
        assert!(document.set_field("agreed", "maybe", &[]).is_err());
        assert_eq!(document.field_value("agreed").unwrap(), "Yes");
        // A text field takes whatever it is given.
        assert_eq!(document.set_field("name", "Ada", &[]).unwrap(), "Ada");
        // `FieldReadOnly`, not `MutationForbidden`: the document allows being
        // changed, this one field does not. The engines used to disagree about
        // which of the two this is, so the variant is the assertion.
        assert!(matches!(
            document.set_field("locked", "x", &[]),
            Err(DocumentError::FieldReadOnly(name)) if name == "locked"
        ));
    }

    #[test]
    fn fields_come_back_in_the_documents_own_order() {
        let document = MemoryDocument::with_form();
        let names: Vec<String> = document
            .fields()
            .unwrap()
            .into_iter()
            .map(|field| field.name)
            .collect();
        assert_eq!(names, vec!["name", "agreed", "colour", "locked"]);
    }

    #[test]
    fn a_radio_groups_widgets_each_stand_for_one_of_its_options() {
        let document = MemoryDocument::with_form();
        let field = document
            .fields()
            .unwrap()
            .into_iter()
            .find(|field| field.name == "colour")
            .unwrap();
        assert_eq!(field.widgets.len(), 2);
        assert_eq!(field.widgets[0].option.as_deref(), Some("red"));
        // One editor per page, at the first widget, however many there are.
        assert_eq!(
            field.anchor_on(PageIndex(0)),
            Some(PageRect::new(100.0, 200.0, 116.0, 216.0))
        );
    }

    #[test]
    fn a_mark_cannot_be_replaced_onto_another_page() {
        let mut document = MemoryDocument::letter(2);
        let id = IdGenerator::new(2).next_id();
        document.create(&id, &ink(0)).unwrap();
        assert!(document.replace(&id, &ink(1)).is_err());
    }

    #[test]
    fn a_page_that_is_not_there_is_refused_by_every_reader() {
        let document = MemoryDocument::letter(1);
        assert!(document.annotations(PageIndex(4)).is_err());
        assert!(document.page_geometry(PageIndex(4)).is_err());
        assert!(document
            .select_text(
                PageIndex(4),
                TextSelection::Word {
                    at: PagePoint::new(1.0, 1.0)
                }
            )
            .is_err());
    }
}
