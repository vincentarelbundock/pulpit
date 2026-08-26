//! Domain model for pulpit.
//!
//! This crate contains no UI, no window handles, no PDF library types and no
//! platform code. It is the authoritative presentation state, and is fully
//! unit testable without a graphical session.

pub mod annotate;
pub mod annotation;
pub mod autoadvance;
pub mod document;
pub mod generation;
pub mod history;
pub mod navigation;
pub mod notes;
pub mod overlay;
pub mod page;
pub mod pdfpc;
pub mod search;
pub mod speech;
pub mod state;
pub mod timer;

pub use annotate::{AnnotationCommand, AnnotationDraft, AnnotationId, AnnotationInteraction};
pub use annotation::{
    AnnotationStyle, AnnotationTool, Annotations, InkColor, InkStroke, MarkupKind, StrokeKind,
};
pub use autoadvance::Autoadvance;
pub use document::{DocumentId, DocumentInfo, LinkTarget, PageLink, PageSize};
pub use generation::RenderGeneration;
pub use history::{NavHistory, Place};
pub use navigation::{DocumentNavigation, Outline, OutlineEntry};
pub use notes::{NotesMapping, PageSource, PairedRule, Region};
pub use overlay::{
    ContentKind, OverlayContent, OverlayDeclaration, OverlayId, OverlayIndex, PageOverlay,
};
pub use page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect, PageRotation};
pub use pdfpc::TextNotes;
pub use search::{
    Hit, HitChunk, HitSource, PreparedQuery, Query, SearchGeneration, SearchProblem, SearchState,
    TextMatch,
};
pub use speech::{
    detect, LanguagePolicy, LanguageSetting, LanguageTag, Reading, Scope, SpeechRate, SpeechState,
};
pub use state::{Blank, Changed, Command, PresentationState, SlideIndex};
pub use timer::Timer;
