//! What pulpit will *not* do with a document, decided before the talk.
//!
//! A PDF can ask for far more than a presentation tool should grant: forms to
//! fill, scripts to run, files to launch, slide transitions to animate. None
//! of that is honoured, and the failure mode of not honouring it silently is
//! a presenter discovering on stage that a button does nothing. So the
//! renderer collects evidence of these features and this module turns the
//! evidence into findings phrased as what will actually happen.
//!
//! Detection is deliberately pure. [`analyse`] takes a [`DocumentEvidence`]
//! that a backend filled in and returns findings; nothing here opens a file,
//! so every rule is unit-testable without PDFium.
//!
//! What is *not* reported matters as much as what is: the `pulpit://` and
//! `run:` overlay conventions (see [`crate::pdf::overlays`]) and the
//! Screen/Movie annotations that carry them are played, not flattened, and
//! reporting them would train presenters to ignore this list.

use serde::{Deserialize, Serialize};

/// Findings kept for one kind before the rest are summarised. A deck that
/// declares a transition on all 300 pages needs one sentence, not 300.
const MAX_FINDINGS_PER_KIND: usize = 8;

/// What kind of thing pulpit will ignore or flatten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FindingKind {
    /// An embedded media annotation whose content pulpit has no player
    /// for: sound, 3-D artwork, Flash-era rich media.
    UnplayableMedia,
    /// AcroForm fields. Rendered as they were last saved and not editable.
    FormFields,
    /// Document-level JavaScript, which runs on open in a reader.
    DocumentJavaScript,
    /// JavaScript attached to an annotation, run on focus, click or change.
    AnnotationJavaScript,
    /// A page transition effect declared by the producer.
    PageTransition,
    /// An annotation action pulpit refuses to perform: launching files,
    /// jumping into another document, or anything it does not recognise.
    UnsupportedAction,
    /// The document is encrypted or carries permission flags.
    Restricted,
    /// An XFA form, which is a different document format wearing a PDF.
    Xfa,
}

impl FindingKind {
    /// A short noun phrase for the feature itself.
    pub fn label(self) -> &'static str {
        match self {
            FindingKind::UnplayableMedia => "embedded media",
            FindingKind::FormFields => "form fields",
            FindingKind::DocumentJavaScript => "document JavaScript",
            FindingKind::AnnotationJavaScript => "annotation JavaScript",
            FindingKind::PageTransition => "page transitions",
            FindingKind::UnsupportedAction => "unsupported link actions",
            FindingKind::Restricted => "document restrictions",
            FindingKind::Xfa => "XFA form",
        }
    }
}

/// One thing the presenter should know before going on stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityFinding {
    pub kind: FindingKind,
    /// The physical page, when the feature belongs to one.
    pub page: Option<usize>,
    /// What will happen, in the presenter's terms.
    pub detail: String,
}

impl CapabilityFinding {
    fn new(kind: FindingKind, page: Option<usize>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            page,
            detail: detail.into(),
        }
    }

    /// The finding as one line, page first when it has one.
    pub fn describe(&self) -> String {
        match self.page {
            Some(page) => format!("page {}: {}", page + 1, self.detail),
            None => self.detail.clone(),
        }
    }
}

/// Everything pulpit will flatten or ignore in one document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DocumentCapabilities {
    pub findings: Vec<CapabilityFinding>,
}

impl DocumentCapabilities {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn len(&self) -> usize {
        self.findings.len()
    }

    pub fn has(&self, kind: FindingKind) -> bool {
        self.findings.iter().any(|finding| finding.kind == kind)
    }

    pub fn of_kind(&self, kind: FindingKind) -> Vec<&CapabilityFinding> {
        self.findings
            .iter()
            .filter(|finding| finding.kind == kind)
            .collect()
    }

    /// The whole report, one finding per line.
    pub fn to_report(&self) -> String {
        if self.is_empty() {
            return "nothing in this document will be ignored or flattened".to_string();
        }
        let mut lines = vec![format!(
            "{} thing(s) in this document will be ignored or flattened:",
            self.findings.len()
        )];
        lines.extend(
            self.findings
                .iter()
                .map(|finding| format!("  [{}] {}", finding.kind.label(), finding.describe())),
        );
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// The kind of interactive form the document declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FormType {
    #[default]
    None,
    AcroForm,
    /// XFA, in either of its two flavours. Both are refused the same way.
    Xfa,
}

/// Annotation subtypes pulpit makes a decision about. Everything else is
/// ordinary page furniture and is drawn, not reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AnnotationSubtype {
    Link,
    /// A form field.
    Widget,
    /// Screen and Movie annotations are how the `pulpit://` and `run:`
    /// conventions travel; pulpit plays these itself.
    Screen,
    Movie,
    Sound,
    ThreeD,
    RichMedia,
    #[default]
    Other,
}

/// The action an annotation carries, reduced to what pulpit decides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ActionKind {
    #[default]
    None,
    /// A destination in this document: honoured.
    GoTo,
    /// A URI: honoured, either as an overlay or through the platform opener.
    Uri,
    /// Run a program or open a file. Never performed.
    Launch,
    /// Jump into another document. Not performed: the deck on screen is the
    /// deck that was opened.
    RemoteGoTo,
    EmbeddedGoTo,
    /// Play an embedded rendition — how a Screen annotation declares media.
    Rendition,
    /// An action type PDFium does not classify, which in practice is almost
    /// always JavaScript.
    Unrecognised,
}

/// One annotation, reduced to the parts that decide whether pulpit can
/// honour it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AnnotationEvidence {
    pub subtype: AnnotationSubtype,
    pub action: ActionKind,
    /// The URI of a URI action, so an overlay convention can be recognised.
    pub uri: Option<String>,
    /// The annotation carries an additional-actions (`/AA`) dictionary, which
    /// is where annotation JavaScript lives.
    pub has_additional_actions: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PageEvidence {
    pub page: usize,
    pub annotations: Vec<AnnotationEvidence>,
    /// The transition style the page declares (`/Trans /S`), if any.
    pub transition: Option<String>,
}

/// Why the document is restricted, as the producer stated it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestrictionEvidence {
    /// The security handler revision; zero means unencrypted.
    pub security_revision: i32,
    /// The `/P` permission bits, as PDFium reports them.
    pub permissions: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DocumentEvidence {
    pub form_type: FormType,
    /// Names of document-level JavaScript actions.
    pub document_javascript: Vec<String>,
    pub restriction: Option<RestrictionEvidence>,
    pub pages: Vec<PageEvidence>,
    /// Transition styles found in the document without being attributable to
    /// a page. PDFium exposes no accessor for `/Trans`, so a backend that
    /// finds them by scanning the file bytes knows *that* they exist and not
    /// *where*; saying so is better than staying silent about an effect the
    /// producer will expect to see.
    pub transition_styles: Vec<String>,
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

/// Turn collected evidence into presenter-actionable findings.
pub fn analyse(evidence: &DocumentEvidence) -> DocumentCapabilities {
    let mut findings = Vec::new();

    if let Some(restriction) = &evidence.restriction {
        if restriction.security_revision > 0 {
            findings.push(CapabilityFinding::new(
                FindingKind::Restricted,
                None,
                format!(
                    "this document is encrypted (security handler revision {}, permission bits \
                     {:#x}); it opened, so it renders normally, but a document that needs a \
                     password to render will fail on stage rather than here",
                    restriction.security_revision, restriction.permissions
                ),
            ));
        }
    }

    match evidence.form_type {
        FormType::Xfa => findings.push(CapabilityFinding::new(
            FindingKind::Xfa,
            None,
            "this is an XFA form, not a page-based document; pulpit shows the static \
             fallback pages the producer embedded, which may say only \"please open in \
             Adobe Reader\"",
        )),
        FormType::AcroForm => findings.push(CapabilityFinding::new(
            FindingKind::FormFields,
            None,
            "this document declares form fields; they are drawn exactly as they were last \
             saved and cannot be typed into, ticked or reset during the talk",
        )),
        FormType::None => {}
    }

    if !evidence.document_javascript.is_empty() {
        findings.push(CapabilityFinding::new(
            FindingKind::DocumentJavaScript,
            None,
            format!(
                "{} document-level script(s) ({}) would run on open in a PDF reader; \
                 pulpit never executes them, so anything they were meant to set up — \
                 field values, a starting page, an auto-advance — will not happen",
                evidence.document_javascript.len(),
                summarise_names(&evidence.document_javascript)
            ),
        ));
    }

    if !evidence.transition_styles.is_empty() {
        findings.push(CapabilityFinding::new(
            FindingKind::PageTransition,
            None,
            format!(
                "this document declares page transitions ({}); pulpit cuts between slides \
                 and will play none of them",
                summarise_names(&evidence.transition_styles)
            ),
        ));
    }

    let mut widget_pages = Vec::new();
    for page in &evidence.pages {
        if let Some(style) = &page.transition {
            findings.push(CapabilityFinding::new(
                FindingKind::PageTransition,
                Some(page.page),
                format!(
                    "this page declares a {} transition; pulpit will cut to it instead",
                    style.to_lowercase()
                ),
            ));
        }
        for annotation in &page.annotations {
            if annotation.has_additional_actions {
                findings.push(CapabilityFinding::new(
                    FindingKind::AnnotationJavaScript,
                    Some(page.page),
                    "an annotation on this page carries scripted actions (run on click, focus \
                     or change in a reader); pulpit ignores them and the annotation stays \
                     as drawn",
                ));
            }
            match annotation.subtype {
                AnnotationSubtype::Widget => {
                    widget_pages.push(page.page);
                    continue;
                }
                AnnotationSubtype::Sound => findings.push(CapabilityFinding::new(
                    FindingKind::UnplayableMedia,
                    Some(page.page),
                    "this page embeds a sound annotation; pulpit has no audio player for \
                     it and the page will be silent",
                )),
                AnnotationSubtype::ThreeD => findings.push(CapabilityFinding::new(
                    FindingKind::UnplayableMedia,
                    Some(page.page),
                    "this page embeds 3-D artwork; pulpit shows its flat poster image \
                     and it cannot be rotated",
                )),
                AnnotationSubtype::RichMedia => findings.push(CapabilityFinding::new(
                    FindingKind::UnplayableMedia,
                    Some(page.page),
                    "this page embeds rich media (Flash-era content); pulpit shows its \
                     poster image and nothing will play",
                )),
                // Screen and Movie annotations are pulpit's own overlay
                // conventions arriving; they play, so they are not findings.
                AnnotationSubtype::Screen
                | AnnotationSubtype::Movie
                | AnnotationSubtype::Link
                | AnnotationSubtype::Other => {}
            }
            if let Some(detail) = unsupported_action_detail(annotation) {
                findings.push(CapabilityFinding::new(
                    FindingKind::UnsupportedAction,
                    Some(page.page),
                    detail,
                ));
            }
        }
    }

    // Widgets are counted once for the whole deck: a form is a property of the
    // document, and one finding per field would bury everything else.
    if !widget_pages.is_empty() && evidence.form_type == FormType::None {
        findings.push(CapabilityFinding::new(
            FindingKind::FormFields,
            None,
            format!(
                "{} form field(s) on {} page(s) are drawn as last saved and cannot be \
                 interacted with",
                widget_pages.len(),
                distinct(&widget_pages)
            ),
        ));
    }

    DocumentCapabilities {
        findings: compress(findings),
    }
}

/// What pulpit will refuse to do about one annotation's action, when it
/// will refuse anything at all.
fn unsupported_action_detail(annotation: &AnnotationEvidence) -> Option<String> {
    match annotation.action {
        // A URI is honoured: either the platform opens it, or it is one of the
        // overlay conventions and pulpit plays it itself.
        ActionKind::None | ActionKind::GoTo | ActionKind::Uri | ActionKind::Rendition => None,
        ActionKind::Launch => Some(
            "a link on this page asks to launch a file or program; pulpit never runs it \
             and the click will do nothing"
                .to_string(),
        ),
        ActionKind::RemoteGoTo | ActionKind::EmbeddedGoTo => Some(
            "a link on this page jumps into another document; pulpit navigates only the \
             deck that is open and the click will do nothing"
                .to_string(),
        ),
        ActionKind::Unrecognised => Some(
            "a link on this page carries an action pulpit does not perform, which is \
             almost always JavaScript; the click will do nothing"
                .to_string(),
        ),
    }
}

/// Keep the first few findings of each kind and replace the tail with one
/// sentence saying how many more there were.
fn compress(findings: Vec<CapabilityFinding>) -> Vec<CapabilityFinding> {
    let mut kept: Vec<CapabilityFinding> = Vec::new();
    let mut overflow: Vec<(FindingKind, usize)> = Vec::new();
    for finding in findings {
        let seen = kept.iter().filter(|f| f.kind == finding.kind).count();
        if seen < MAX_FINDINGS_PER_KIND {
            kept.push(finding);
            continue;
        }
        match overflow.iter_mut().find(|(kind, _)| *kind == finding.kind) {
            Some((_, count)) => *count += 1,
            None => overflow.push((finding.kind, 1)),
        }
    }
    for (kind, count) in overflow {
        kept.push(CapabilityFinding::new(
            kind,
            None,
            format!(
                "{count} further page(s) are affected by {} in the same way",
                kind.label()
            ),
        ));
    }
    kept
}

/// Transition styles named anywhere in a PDF's bytes.
///
/// PDFium exposes no accessor for a page's `/Trans` dictionary, and writing a
/// PDF parser to reach one would be a far larger risk than the finding is
/// worth. So this reads what the producer wrote: every `/Trans` marker, and
/// the `/S /Style` name that follows it inside a short window. Compressed
/// object streams hide their entries from this scan, which only ever means a
/// missed finding — it can never invent one, because the byte sequence it
/// looks for is the one a transition is written with.
pub fn scan_transition_styles(bytes: &[u8]) -> Vec<String> {
    /// How far past a `/Trans` marker the style name can be. A transition
    /// dictionary is a handful of short entries.
    const WINDOW: usize = 160;
    /// The styles ISO 32000-1 table 164 defines. Anything else is not a
    /// transition, whatever it is next to.
    const STYLES: [&str; 11] = [
        "Split", "Blinds", "Box", "Wipe", "Dissolve", "Glitter", "R", "Fly", "Push", "Cover",
        "Uncover",
    ];

    let mut found: Vec<String> = Vec::new();
    let marker = b"/Trans";
    let mut index = 0;
    while index + marker.len() <= bytes.len() {
        if &bytes[index..index + marker.len()] != marker {
            index += 1;
            continue;
        }
        let end = (index + WINDOW).min(bytes.len());
        let window = &bytes[index..end];
        for style in STYLES {
            // `/S` then the style name; `/R` alone is the "replace" style,
            // which is a cut and therefore nothing to report.
            let needle = format!("/S/{style}");
            let spaced = format!("/S /{style}");
            if style != "R"
                && (contains(window, needle.as_bytes()) || contains(window, spaced.as_bytes()))
                && !found.iter().any(|seen| seen == style)
            {
                found.push(style.to_string());
            }
        }
        index += marker.len();
    }
    found
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn distinct(pages: &[usize]) -> usize {
    let mut pages = pages.to_vec();
    pages.sort_unstable();
    pages.dedup();
    pages.len()
}

fn summarise_names(names: &[String]) -> String {
    const SHOWN: usize = 3;
    if names.len() <= SHOWN {
        return names.join(", ");
    }
    format!("{}, …", names[..SHOWN].join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(page: usize, annotations: Vec<AnnotationEvidence>) -> PageEvidence {
        PageEvidence {
            page,
            annotations,
            transition: None,
        }
    }

    fn annotation(subtype: AnnotationSubtype, action: ActionKind) -> AnnotationEvidence {
        AnnotationEvidence {
            subtype,
            action,
            uri: None,
            has_additional_actions: false,
        }
    }

    fn analyse_pages(pages: Vec<PageEvidence>) -> DocumentCapabilities {
        analyse(&DocumentEvidence {
            pages,
            ..Default::default()
        })
    }

    #[test]
    fn a_plain_deck_has_nothing_to_report() {
        let capabilities = analyse_pages(vec![page(
            0,
            vec![annotation(AnnotationSubtype::Link, ActionKind::GoTo)],
        )]);
        assert!(capabilities.is_empty());
        assert_eq!(
            capabilities.to_report(),
            "nothing in this document will be ignored or flattened"
        );
    }

    #[test]
    fn the_overlays_pulpit_plays_are_never_reported_as_unsupported() {
        let capabilities = analyse_pages(vec![page(
            0,
            vec![
                AnnotationEvidence {
                    subtype: AnnotationSubtype::Screen,
                    action: ActionKind::Rendition,
                    uri: None,
                    has_additional_actions: false,
                },
                AnnotationEvidence {
                    subtype: AnnotationSubtype::Movie,
                    action: ActionKind::None,
                    uri: None,
                    has_additional_actions: false,
                },
                AnnotationEvidence {
                    subtype: AnnotationSubtype::Link,
                    action: ActionKind::Uri,
                    uri: Some("pulpit://video/clip?autoplay".into()),
                    has_additional_actions: false,
                },
                AnnotationEvidence {
                    subtype: AnnotationSubtype::Link,
                    action: ActionKind::Uri,
                    uri: Some("run:media/clip.mp4?autostart".into()),
                    has_additional_actions: false,
                },
            ],
        )]);
        assert!(
            capabilities.is_empty(),
            "pulpit's own overlay conventions must not be reported: {capabilities:?}"
        );
    }

    #[test]
    fn unplayable_media_is_reported_per_page_with_what_will_be_seen() {
        let capabilities = analyse_pages(vec![
            page(
                2,
                vec![annotation(AnnotationSubtype::Sound, ActionKind::None)],
            ),
            page(
                5,
                vec![annotation(AnnotationSubtype::ThreeD, ActionKind::None)],
            ),
            page(
                6,
                vec![annotation(AnnotationSubtype::RichMedia, ActionKind::None)],
            ),
        ]);
        let media = capabilities.of_kind(FindingKind::UnplayableMedia);
        assert_eq!(media.len(), 3);
        assert_eq!(media[0].page, Some(2));
        assert!(media[0].detail.contains("silent"));
        assert!(media[1].detail.contains("poster"));
        assert!(
            capabilities.to_report().contains("page 3:"),
            "pages are one-based for the presenter"
        );
    }

    #[test]
    fn form_fields_are_reported_once_for_the_whole_deck() {
        let capabilities = analyse_pages(vec![
            page(
                0,
                vec![
                    annotation(AnnotationSubtype::Widget, ActionKind::None),
                    annotation(AnnotationSubtype::Widget, ActionKind::None),
                ],
            ),
            page(
                1,
                vec![annotation(AnnotationSubtype::Widget, ActionKind::None)],
            ),
        ]);
        let forms = capabilities.of_kind(FindingKind::FormFields);
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].page, None);
        assert!(forms[0].detail.contains("3 form field(s) on 2 page(s)"));
    }

    #[test]
    fn a_declared_acroform_is_reported_without_counting_widgets_twice() {
        let capabilities = analyse(&DocumentEvidence {
            form_type: FormType::AcroForm,
            pages: vec![page(
                0,
                vec![annotation(AnnotationSubtype::Widget, ActionKind::None)],
            )],
            ..Default::default()
        });
        assert_eq!(capabilities.of_kind(FindingKind::FormFields).len(), 1);
    }

    #[test]
    fn an_xfa_document_says_what_the_audience_will_actually_see() {
        let capabilities = analyse(&DocumentEvidence {
            form_type: FormType::Xfa,
            ..Default::default()
        });
        assert!(capabilities.has(FindingKind::Xfa));
        assert!(!capabilities.has(FindingKind::FormFields));
        assert!(capabilities.of_kind(FindingKind::Xfa)[0]
            .detail
            .contains("static fallback"));
    }

    #[test]
    fn document_javascript_names_the_scripts_that_will_not_run() {
        let capabilities = analyse(&DocumentEvidence {
            document_javascript: vec!["setUp".into(), "autoAdvance".into()],
            ..Default::default()
        });
        let findings = capabilities.of_kind(FindingKind::DocumentJavaScript);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].detail.contains("setUp, autoAdvance"));
    }

    #[test]
    fn many_script_names_are_summarised_rather_than_listed() {
        let capabilities = analyse(&DocumentEvidence {
            document_javascript: (0..20).map(|index| format!("script{index}")).collect(),
            ..Default::default()
        });
        assert!(capabilities.of_kind(FindingKind::DocumentJavaScript)[0]
            .detail
            .contains('…'));
    }

    #[test]
    fn annotation_scripts_are_reported_against_their_page() {
        let capabilities = analyse_pages(vec![page(
            4,
            vec![AnnotationEvidence {
                subtype: AnnotationSubtype::Link,
                action: ActionKind::GoTo,
                uri: None,
                has_additional_actions: true,
            }],
        )]);
        let findings = capabilities.of_kind(FindingKind::AnnotationJavaScript);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].page, Some(4));
    }

    #[test]
    fn a_page_transition_says_pulpit_will_cut_instead() {
        let capabilities = analyse_pages(vec![PageEvidence {
            page: 7,
            annotations: Vec::new(),
            transition: Some("Dissolve".into()),
        }]);
        let findings = capabilities.of_kind(FindingKind::PageTransition);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].detail,
            "this page declares a dissolve transition; pulpit will cut to it instead"
        );
    }

    #[test]
    fn every_refused_action_type_explains_the_dead_click() {
        for (action, expected) in [
            (ActionKind::Launch, "never runs it"),
            (ActionKind::RemoteGoTo, "another document"),
            (ActionKind::EmbeddedGoTo, "another document"),
            (ActionKind::Unrecognised, "JavaScript"),
        ] {
            let capabilities = analyse_pages(vec![page(
                1,
                vec![annotation(AnnotationSubtype::Link, action)],
            )]);
            let findings = capabilities.of_kind(FindingKind::UnsupportedAction);
            assert_eq!(findings.len(), 1, "{action:?}");
            assert!(findings[0].detail.contains(expected), "{action:?}");
            assert!(findings[0].detail.contains("do nothing"), "{action:?}");
        }
    }

    #[test]
    fn an_encrypted_document_is_reported_as_a_stage_risk() {
        let capabilities = analyse(&DocumentEvidence {
            restriction: Some(RestrictionEvidence {
                security_revision: 3,
                permissions: 0xffff_fffc,
            }),
            ..Default::default()
        });
        assert!(capabilities.has(FindingKind::Restricted));

        let unencrypted = analyse(&DocumentEvidence {
            restriction: Some(RestrictionEvidence {
                security_revision: 0,
                permissions: 0xffff_ffff,
            }),
            ..Default::default()
        });
        assert!(
            unencrypted.is_empty(),
            "an unencrypted document has no restriction to report"
        );
    }

    #[test]
    fn transitions_found_only_in_the_bytes_are_reported_for_the_whole_document() {
        let capabilities = analyse(&DocumentEvidence {
            transition_styles: vec!["Dissolve".into(), "Wipe".into()],
            ..Default::default()
        });
        let findings = capabilities.of_kind(FindingKind::PageTransition);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].page, None);
        assert!(findings[0].detail.contains("Dissolve, Wipe"));
        assert!(findings[0].detail.contains("cuts between slides"));
    }

    #[test]
    fn the_byte_scan_finds_declared_transition_styles_and_nothing_else() {
        let page = b"<< /Type /Page /Trans << /Type /Trans /S /Dissolve /D 1 >> >>";
        assert_eq!(scan_transition_styles(page), vec!["Dissolve".to_string()]);

        let tight = b"<</Trans<</S/Wipe/Di 90>>>><</Trans<</S/Wipe>>>>";
        assert_eq!(
            scan_transition_styles(tight),
            vec!["Wipe".to_string()],
            "a style declared twice is reported once"
        );

        assert!(
            scan_transition_styles(b"<< /Trans << /S /R >> >>").is_empty(),
            "the replace style is a cut, which is what pulpit does anyway"
        );
        assert!(scan_transition_styles(
            b"a plain deck with /S /Dissolve nowhere near a trans dict"
        )
        .is_empty());
        assert!(scan_transition_styles(b"").is_empty());
        assert!(scan_transition_styles(b"/Tran").is_empty());
    }

    #[test]
    fn a_repeated_finding_is_summarised_instead_of_repeated_hundreds_of_times() {
        let pages = (0..200)
            .map(|index| PageEvidence {
                page: index,
                annotations: Vec::new(),
                transition: Some("Wipe".into()),
            })
            .collect();
        let capabilities = analyse_pages(pages);
        let findings = capabilities.of_kind(FindingKind::PageTransition);
        assert_eq!(findings.len(), MAX_FINDINGS_PER_KIND + 1);
        let summary = findings.last().unwrap();
        assert_eq!(summary.page, None);
        assert!(summary
            .detail
            .contains(&format!("{} further page(s)", 200 - MAX_FINDINGS_PER_KIND)));
    }
}
