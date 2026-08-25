//! Out-of-process PDF rendering: protocol, workers, supervisor and the
//! bounded frame cache.
//!
//! PDFium never runs in the main process. This crate is the boundary that
//! provides crash containment for malformed or adversarial PDFs and parallel
//! progress when one page is expensive.

pub mod cache;
/// DjVu, bound to an installed djvulibre at run time
/// (`SPEC-reader-formats.md` §55.3). The backend is behind the `djvu` feature
/// and on by default: the binding is `dlopen`, so it adds no build-time
/// dependency, and §55.4 requires DjVu to be a capability of the machine
/// rather than of the build. The module itself is unconditional, because a
/// build without the backend must still name the format when it refuses one.
pub mod djvu;
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
pub use djvu::{is_djvu, missing_djvu_message, DJVU_EXTENSIONS};
#[cfg(feature = "djvu")]
pub use djvu::{DjvuBackend, DjvuDocument};
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
