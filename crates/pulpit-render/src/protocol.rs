//! The versioned renderer IPC protocol.
//!
//! Every message is length-prefixed and every field that will later size an
//! allocation or a memory mapping is validated *before* anything is mapped.
//! Both sides reject stale generations, so a late reply from a document that
//! no longer exists can never replace a current frame.

use std::io::{Read, Write};

use pulpit_core::notes::Region;
use pulpit_core::{PageSize, RenderGeneration};
use serde::{Deserialize, Serialize};

/// Bumped whenever the wire format changes. A worker that does not answer
/// with the same version is shut down rather than trusted.
pub const PROTOCOL_VERSION: u32 = 8;

/// Hard ceiling on one encoded message.
///
/// Frames are metadata only — pixels travel through shared memory — so this
/// would be a kilobyte-scale limit were it not for attachments, which are the
/// one payload that genuinely travels inline. Shared memory was considered
/// and rejected for them: a region is supervisor-owned and sized before the
/// request is sent, so an attachment of unknown length would need a
/// size-then-fetch round trip and a second region lifetime to manage, all to
/// avoid one bounded copy of a file that is already bounded by
/// [`MAX_ATTACHMENT_BYTES`]. The cost of the simpler choice is this ceiling:
/// a hostile stream can make the reader allocate it once per message.
pub const MAX_MESSAGE_BYTES: u32 = 80 << 20;

/// Hard ceiling on one attachment's bytes. Web bundles and short clips live
/// well inside this; anything larger is refused rather than staged.
pub const MAX_ATTACHMENT_BYTES: u64 = 64 << 20;

/// Hard ceiling on one shared-memory region: a 16k × 16k RGBA frame.
pub const MAX_REGION_BYTES: u64 = 16_384 * 16_384 * 4;

/// Frames at or below this many bytes travel inline in the response instead
/// of through the shared region.
///
/// The region is the reason a worker can hold only one large render at a
/// time: its pixels sit there until the supervisor's next pump copies them
/// out, so a second render would overwrite the first. Frames that travel in
/// the pipe need no such exclusivity, so a worker holds several and starts
/// the next the instant one is sent, instead of idling a tick between each.
///
/// The value places a *cliff*, not a trade-off. `warm-bench sweep` measured
/// throughput at the application's own settings: pipe copy cost never
/// appeared at any size up to 8.3 MB, so an inline frame costs nothing but
/// its share of [`MAX_INLINE_IN_FLIGHT_BYTES`] — 570 pages/s flat through
/// 2 MB, easing to 160 pages/s at 8.3 MB as the byte budget admits fewer
/// per tick — while the first frame past the threshold drops to one per
/// worker per tick (40 pages/s, 14× below the plateau), however narrowly it
/// crosses. The threshold therefore decides only which consumers stand at
/// that cliff. Eight mebibytes puts every consumer that exists in any
/// window configuration — thumbnails (≤0.5 MB), a reader page up to a 4K
/// presenter window (6.7 MB), even an HD audience frame (8.29 MB) — on the
/// fast side, and leaves only
/// frames from the 33 MB class of a 4K projector in the region, where a
/// second copy of that many bytes is the one cost that would actually
/// register.
pub const INLINE_FRAME_BYTES: u64 = 8 << 20;

/// Ceiling on the summed bytes of inline jobs one worker may hold
/// undispatched-or-unanswered at once.
///
/// The depth cap bounds how many jobs die with a worker; this bounds how
/// much frame data can sit buffered in the event channel between pumps. Two
/// constants multiply into the worst case otherwise — sixteen jobs at eight
/// mebibytes each is a number nobody chose. Four full-sized inline frames is
/// enough for the audience frame and a prefetch either side, while a warming
/// pass of small thumbnails never comes near it.
pub const MAX_INLINE_IN_FLIGHT_BYTES: u64 = 32 << 20;

/// Why a message could not be moved across the pipe.
///
/// One definition for both workers, in `pulpit-core`: the envelope is the same
/// problem whatever is inside it, and the half where a mistake is a security
/// bug rather than a wrong picture.
pub use pulpit_core::ipc::ProtocolError;

/// Identifies one render request for its whole lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RequestId(pub u64);

/// Priority classes, exactly the order the specification prescribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Priority {
    /// 1. The committed audience page.
    Audience,
    /// 2. The current presenter page.
    Presenter,
    /// 3. The next page.
    Next,
    /// 4. Adjacent pages.
    Adjacent,
    /// 5. Notes and thumbnails.
    Ancillary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderJob {
    pub id: RequestId,
    pub generation: RenderGeneration,
    pub document: u64,
    pub page: usize,
    pub region: Region,
    pub width: u32,
    pub height: u32,
    pub priority: Priority,
    /// Draw the document's own annotations into the frame.
    ///
    /// Off for every presentation frame — the presenter's marks are a vector
    /// overlay, and the projector must not show the document's own ink twice.
    /// On for a reader page, where the annotations are the point: the frame
    /// is the one representation of the committed marks (A1).
    pub with_annotations: bool,
    /// Name of the shared-memory region the worker must write into.
    pub region_name: String,
}

impl RenderJob {
    pub fn byte_len(&self) -> u64 {
        self.width as u64 * self.height as u64 * 4
    }

    /// Whether this frame travels inline in the response rather than through
    /// the shared region. Both sides derive this from the job so they cannot
    /// disagree.
    pub fn is_inline(&self) -> bool {
        self.byte_len() <= INLINE_FRAME_BYTES
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.width == 0 || self.height == 0 {
            return Err(ProtocolError::Malformed("zero-sized render".into()));
        }
        if self.byte_len() > MAX_REGION_BYTES {
            return Err(ProtocolError::Malformed(format!(
                "render {}×{} exceeds the shared-memory limit",
                self.width, self.height
            )));
        }
        if !self.region.is_valid() {
            return Err(ProtocolError::Malformed("region outside the page".into()));
        }
        // Inline frames never touch shared memory: any region name they
        // carry is ignored, so the checks below do not apply.
        if self.is_inline() {
            return Ok(());
        }
        if self.region_name.is_empty() || self.region_name.len() > 256 {
            return Err(ProtocolError::Malformed("bad shared-memory name".into()));
        }
        if self.region_name.contains(['/', '\\', '\0']) {
            return Err(ProtocolError::Malformed(
                "shared-memory name must not contain path separators".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Hello {
        version: u32,
    },
    Open {
        document: u64,
        path: String,
    },
    Close {
        document: u64,
    },
    Render(RenderJob),
    /// Ask for the link annotations on one page.
    Links {
        document: u64,
        page: usize,
    },
    /// Ask which overlays one page declares.
    Overlays {
        document: u64,
        page: usize,
    },
    /// Ask for the page labels and outline of one document.
    Navigation {
        document: u64,
    },
    /// Find a string in the text layer of a run of pages.
    ///
    /// A run rather than the whole deck: the answer travels over the pipe
    /// and a presenter watching a search crawl through five hundred slides
    /// needs the first hits before the last page is read.
    FindText {
        document: u64,
        generation: pulpit_core::search::SearchGeneration,
        query: pulpit_core::search::Query,
        from_page: usize,
        to_page: usize,
    },
    /// Ask what the document declares that pulpit will not honour.
    Capabilities {
        document: u64,
    },
    /// Ask for the bytes of one embedded file.
    Attachment {
        document: u64,
        name: String,
    },
    /// Cancel one in-flight or queued request.
    Cancel {
        id: RequestId,
    },
    /// Cancel everything older than `generation`.
    CancelGeneration {
        generation: RenderGeneration,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenedDocument {
    pub document: u64,
    pub page_count: usize,
    pub first_page_size: PageSize,
    /// Size of pages `0..page_sizes.len()`. Bounded by
    /// [`pulpit_core::document::MAX_TRACKED_PAGE_SIZES`] so a
    /// fifty-thousand-page scan cannot put half a megabyte of geometry into
    /// this message; anything beyond the bound falls back to the first page.
    pub page_sizes: Vec<PageSize>,
    pub page_sizes_sampled: bool,
    pub metadata_text: String,
    /// The document's embedded `*.pdfpc` speaker notes, verbatim.
    ///
    /// Carried as text rather than parsed because the worker's job is to get
    /// bytes out of the PDF; what a pdfpc payload means is
    /// [`pulpit_core::pdfpc`]'s business, and the application is where a
    /// malformed one has to be reported. Absent when the document has no such
    /// attachment, or when it is not UTF-8.
    pub notes_pdfpc: Option<String>,
    /// The worker's own digest of a directory source, so the application can
    /// tell whether the two listings agree (`SPEC-images.md` §42.3). `None`
    /// for a PDF, whose source is one file and needs no such check.
    #[serde(default)]
    pub source_digest: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Hello {
        version: u32,
        backend: String,
        /// What the backend is, for diagnostics: a library path or a build.
        backend_version: String,
    },
    /// What the worker is really rendering with, once it knows.
    ///
    /// `Hello` is sent before any document exists, and the PDF and DjVu
    /// backends are bound lazily on the first `Open` that needs them — so the
    /// version in `Hello` says "not bound yet" and, reported alone, went on
    /// saying it for the life of the process. Every rendering question starts
    /// by asking which backend is in use, and the diagnostics answered it
    /// with the state at spawn.
    BackendBound {
        backend: String,
        backend_version: String,
    },
    Opened(OpenedDocument),
    OpenFailed {
        document: u64,
        reason: String,
    },
    Rendered {
        id: RequestId,
        generation: RenderGeneration,
        width: u32,
        height: u32,
        /// Bytes of the frame: written into the shared-memory region, or the
        /// length of `pixels` when the frame travels inline.
        bytes: u64,
        /// The frame itself, present exactly when the job `is_inline()`.
        /// Inline frames never occupy the region, which is what lets a worker
        /// hold several small jobs and render them back to back.
        pixels: Option<Vec<u8>>,
        /// How long the rasteriser itself took, in microseconds.
        ///
        /// The supervisor can time from handing a job over to getting a frame
        /// back, but a worker holds several jobs at once and renders them one
        /// at a time, so that measurement is the rasteriser *and* the wait in
        /// the worker's own inbox. Only the worker can tell them apart, and
        /// the difference decides whether the answer is a faster render or
        /// less work in flight.
        #[serde(default)]
        render_micros: u64,
    },
    RenderFailed {
        id: RequestId,
        generation: RenderGeneration,
        reason: String,
    },
    Cancelled {
        id: RequestId,
    },
    /// The link annotations on one page. A page without links (or a backend
    /// without link support) answers with an empty list.
    Links {
        document: u64,
        page: usize,
        links: Vec<pulpit_core::PageLink>,
    },
    /// What one page's link annotations declared, with a diagnostic for every
    /// URI that announced itself as media and then failed to parse.
    Overlays {
        document: u64,
        page: usize,
        declarations: Vec<pulpit_core::overlay::OverlayDeclaration>,
        diagnostics: Vec<String>,
    },
    /// The page labels and outline of one document. A deck without bookmarks
    /// answers with an empty model rather than an error.
    Navigation {
        document: u64,
        navigation: pulpit_core::navigation::DocumentNavigation,
    },
    /// The hits in one run of pages, under the generation that asked for
    /// them, so an answer to a superseded query is recognisable as stale.
    ///
    /// `searchable` is false when the backend cannot read a text layer at
    /// all: an unsearchable deck and a deck with no matches must not look the
    /// same to the person who typed the query.
    Found {
        document: u64,
        generation: pulpit_core::search::SearchGeneration,
        chunk: pulpit_core::search::HitChunk,
        searchable: bool,
    },
    /// What the document declares that pulpit will flatten or ignore.
    Capabilities {
        document: u64,
        capabilities: crate::pdf::capabilities::DocumentCapabilities,
    },
    Attachment {
        document: u64,
        name: String,
        bytes: Vec<u8>,
    },
    /// A missing, oversized or unreadable attachment. Never fatal: the static
    /// PDF page is still what the audience sees.
    AttachmentFailed {
        document: u64,
        name: String,
        reason: String,
    },
}

/// Write one length-prefixed message, at this protocol's ceiling.
pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
) -> Result<(), ProtocolError> {
    pulpit_core::ipc::write_message(writer, message, MAX_MESSAGE_BYTES)
}

/// Read one length-prefixed message, refusing implausible lengths before
/// allocating anything.
pub fn read_message<T: serde::de::DeserializeOwned>(
    reader: &mut impl Read,
) -> Result<T, ProtocolError> {
    pulpit_core::ipc::read_message(reader, MAX_MESSAGE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> RenderJob {
        RenderJob {
            id: RequestId(7),
            generation: RenderGeneration(3),
            document: 1,
            page: 4,
            region: Region::FULL,
            // 4K: decisively above `INLINE_FRAME_BYTES`, so this job maps
            // shared memory and every name check below applies to it.
            width: 3840,
            height: 2160,
            priority: Priority::Audience,
            with_annotations: false,
            region_name: "pulpit-shm-1".into(),
        }
    }

    #[test]
    fn messages_round_trip() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &Request::Render(job())).unwrap();
        write_message(&mut buffer, &Request::Cancel { id: RequestId(7) }).unwrap();

        let mut cursor = std::io::Cursor::new(buffer);
        assert_eq!(
            read_message::<Request>(&mut cursor).unwrap(),
            Request::Render(job())
        );
        assert_eq!(
            read_message::<Request>(&mut cursor).unwrap(),
            Request::Cancel { id: RequestId(7) }
        );
        assert!(matches!(
            read_message::<Request>(&mut cursor),
            Err(ProtocolError::Closed)
        ));
    }

    #[test]
    fn an_implausible_length_is_refused_before_allocating() {
        let mut framed = Vec::new();
        framed.extend_from_slice(&u32::MAX.to_le_bytes());
        let mut cursor = std::io::Cursor::new(framed);
        assert!(matches!(
            read_message::<Request>(&mut cursor),
            Err(ProtocolError::TooLarge {
                bytes: u32::MAX,
                ..
            })
        ));
    }

    #[test]
    fn garbage_bytes_do_not_panic() {
        for seed in 0..64u32 {
            let mut framed = Vec::new();
            let payload: Vec<u8> = (0..32).map(|i| (i * 31 + seed) as u8).collect();
            framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            framed.extend_from_slice(&payload);
            let mut cursor = std::io::Cursor::new(framed);
            let _ = read_message::<Request>(&mut cursor);
        }
    }

    #[test]
    fn truncated_payloads_report_closed() {
        let mut framed = Vec::new();
        framed.extend_from_slice(&64u32.to_le_bytes());
        framed.extend_from_slice(&[0u8; 10]);
        let mut cursor = std::io::Cursor::new(framed);
        assert!(matches!(
            read_message::<Request>(&mut cursor),
            Err(ProtocolError::Closed)
        ));
    }

    #[test]
    fn jobs_are_validated_before_memory_is_mapped() {
        assert!(job().validate().is_ok());
        assert!(RenderJob { width: 0, ..job() }.validate().is_err());
        assert!(RenderJob {
            width: 20_000,
            height: 20_000,
            ..job()
        }
        .validate()
        .is_err());
        assert!(RenderJob {
            region_name: String::new(),
            ..job()
        }
        .validate()
        .is_err());
        assert!(
            RenderJob {
                region_name: "../../etc/passwd".into(),
                ..job()
            }
            .validate()
            .is_err(),
            "a worker must not be talked into mapping an arbitrary path"
        );
        assert!(RenderJob {
            region: Region::new(0.0, 0.0, 2.0, 1.0),
            ..job()
        }
        .validate()
        .is_err());

        // An inline job never maps memory, so the name checks do not apply
        // to it: the empty name is its normal state, and a hostile one is
        // inert because nothing ever opens it.
        let inline = RenderJob {
            width: 240,
            height: 135,
            region_name: String::new(),
            ..job()
        };
        assert!(inline.is_inline());
        assert!(inline.validate().is_ok());
    }

    #[test]
    fn priorities_order_the_way_the_spec_says() {
        let mut order = vec![
            Priority::Ancillary,
            Priority::Next,
            Priority::Audience,
            Priority::Presenter,
        ];
        order.sort();
        assert_eq!(
            order,
            vec![
                Priority::Audience,
                Priority::Presenter,
                Priority::Next,
                Priority::Ancillary
            ]
        );
    }
}
