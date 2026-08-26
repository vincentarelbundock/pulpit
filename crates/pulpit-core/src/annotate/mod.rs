//! Document annotation: what the user is *doing*, and the commands that come
//! out of it.
//!
//! This is the other half of the split `SPEC-document.md` §5.3 asks for. The
//! presenter's [`crate::annotation::Annotations`] holds transient marks that
//! vanish with the slide; everything here is about marks that become native
//! PDF annotations the moment a gesture ends, and about the gesture state that
//! never does (§3.2).
//!
//! The rule that shapes the module: **a gesture is not an annotation**. A
//! [`Gesture`] is bounded, ephemeral, and never snapshotted mid-flight; on
//! release it produces exactly one [`AnnotationCommand`] and is discarded.
//! Nothing here knows about PDFium, Iced, Typst or SVG, which is what keeps
//! stroke simplification, hit-testing and eraser selection ordinary unit tests.

pub mod draft;
pub mod gesture;
pub mod hit;
pub mod id;
pub mod presenter;
pub mod stroke;
pub mod text_box;

pub use draft::{
    AnnotationCommand, AnnotationDraft, AnnotationKind, DraftError, FreeTextDraft, HighlightDraft,
    InkDraft, MarkStyle, NoteDraft, StampDraft, StampMark, TextSource, MAX_ANNOTATION_TEXT,
    MAX_QUADS, NOTE_ICON_POINTS,
};
pub use gesture::{
    AnnotationInteraction, AnnotationTool, Corner, Gesture, GestureOutcome, PlacedMark,
    SelectedText, TransformHandle, MIN_MARK_SIZE,
};
pub use hit::{AnnotationHit, HitTarget};
pub use id::{AnnotationId, IdGenerator};
pub use stroke::{simplify, InkPoint, MAX_INK_POINTS, MIN_SAMPLE_DISTANCE};
