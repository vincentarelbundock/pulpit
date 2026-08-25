//! Out-of-process PDF rendering: protocol, workers, supervisor and the
//! bounded frame cache.
//!
//! PDFium never runs in the main process. This crate is the boundary that
//! provides crash containment for malformed or adversarial PDFs and parallel
//! progress when one page is expensive.

pub mod cache;
pub mod document;
pub mod images;
pub mod pdf;
pub mod pdftext;
pub mod pdfwrite;
pub mod protocol;
pub mod shm;
pub mod sign;
pub mod supervisor;
pub mod verify;
pub mod worker;

pub use cache::{CacheStats, Frame, FrameCache, FrameKey, FrameKind, DEFAULT_BUDGET_BYTES};
pub use images::{ImageBackend, ImageDocument, IMAGE_EXTENSIONS};
pub use pdf::capabilities::{CapabilityFinding, DocumentCapabilities, FindingKind};
pub use protocol::{Priority, Quality, RenderJob, Request, RequestId, Response, PROTOCOL_VERSION};
pub use sign::{
    build_cms, estimate_cms_size, load_pkcs12, CredentialSummary, DigestAlgorithm, SigningError,
    SigningProfile,
};
pub use supervisor::{
    RenderDiagnostics, RenderEvent, RendererSupervisor, SupervisorConfig, WorkerCommand,
};
