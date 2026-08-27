//! Closed-world Typst compilation for text annotations.
//!
//! Annotation source is intentionally compiled without files, packages,
//! plugins, network access, or a clock. The only fonts are Typst's pinned
//! built-ins, which makes the SVG identical on presenter and audience.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

use iced::widget::svg;
use serde::{Deserialize, Serialize};

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration as TypstDuration};
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

struct Assets {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let fonts: Vec<_> = typst_assets::fonts()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect();
        let book = FontBook::from_fonts(fonts.iter());
        Assets {
            library: LazyHash::new(Library::default()),
            book: LazyHash::new(book),
            fonts,
        }
    })
}

struct ClosedWorld {
    source: Source,
}

impl ClosedWorld {
    fn new(source: String) -> Self {
        Self {
            source: Source::detached(source),
        }
    }
}

impl World for ClosedWorld {
    fn library(&self) -> &LazyHash<Library> {
        &assets().library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &assets().book
    }

    fn main(&self) -> FileId {
        self.source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        (id == self.source.id())
            .then(|| self.source.clone())
            .ok_or(FileError::AccessDenied)
    }

    fn file(&self, _id: FileId) -> FileResult<Bytes> {
        Err(FileError::AccessDenied)
    }

    fn font(&self, index: usize) -> Option<Font> {
        assets().fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<TypstDuration>) -> Option<Datetime> {
        None
    }
}

/// Wrap one annotation in the page and text setup both outputs share, and
/// compile it.
///
/// The SVG and the raster appearance must come from *the same* markup: a mark
/// the reader sees on screen and the one written into the PDF cannot be laid
/// out differently.
fn compile(
    source: &str,
    width_pt: f32,
    size_pt: f32,
    rgb: (u8, u8, u8),
) -> Result<PagedDocument, String> {
    // Typst's auto-height page can otherwise end exactly on the math frame's
    // bounds. Give accents, superscripts, and descenders a font-relative
    // viewport gutter so SVG consumers never clip their antialiasing fringe.
    let vertical_gutter = size_pt * 0.2;
    let wrapped = format!(
        "#set page(width: {width_pt}pt, height: auto, margin: (x: 0pt, y: {vertical_gutter}pt), fill: none)\n\
         #set text(size: {size_pt}pt, fill: rgb(\"#{:02x}{:02x}{:02x}\"))\n{}",
        rgb.0, rgb.1, rgb.2, source
    );
    let warned = typst::compile::<PagedDocument>(&ClosedWorld::new(wrapped));
    warned.output.map_err(|diagnostics| {
        diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Compile one annotation to a transparent, single-page SVG.
pub fn render(
    source: &str,
    width_pt: f32,
    size_pt: f32,
    rgb: (u8, u8, u8),
) -> Result<String, String> {
    let document = compile(source, width_pt, size_pt, rgb)?;
    let page = document.pages().first().ok_or("Typst produced no page")?;
    Ok(typst_svg::svg(page, &typst_svg::SvgOptions::default()))
}

/// One Typst annotation, rasterised for a PDF appearance (§7.4).
#[derive(Debug, Clone, PartialEq)]
pub struct RasterisedText {
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Tightly packed RGBA8.
    pub rgba: Vec<u8>,
    /// The size the mark should occupy on the page, in PDF points.
    pub width_pt: f32,
    pub height_pt: f32,
}

/// Compile one annotation to pixels, for embedding as a `/Stamp` appearance.
///
/// Typst markup has no lossless standard `/FreeText` encoding, so §7.4 has
/// pulpit generate the appearance and keep the *source* in its own namespaced
/// entry: other viewers show the picture, and pulpit reopens the markup.
///
/// Raster rather than vector because the workspace already rasterises Typst
/// nowhere and vectorises it into SVG, which is not a PDF content stream; a
/// bounded raster is what §7.4 explicitly permits. `scale` is pixels per
/// point, so a mark stays sharp at the zoom it is read at.
pub fn rasterise(
    source: &str,
    width_pt: f32,
    size_pt: f32,
    rgb: (u8, u8, u8),
    scale: f32,
) -> Result<RasterisedText, String> {
    // Bounded before anything is allocated (A8): a document that asked for a
    // hundred-megapixel appearance would be asking for the memory, not for a
    // mark anyone can read.
    const MAX_PIXELS: u64 = 2048 * 2048;
    let scale = if scale.is_finite() && scale > 0.0 {
        scale.clamp(0.5, 8.0)
    } else {
        2.0
    };

    let document = compile(source, width_pt, size_pt, rgb)?;
    let page = document.pages().first().ok_or("Typst produced no page")?;

    let size = page.frame.size();
    let (width_pt, height_pt) = (size.x.to_pt() as f32, size.y.to_pt() as f32);
    if width_pt <= 0.0 || height_pt <= 0.0 || !width_pt.is_finite() || !height_pt.is_finite() {
        return Err("Typst produced a page with no area".to_string());
    }
    let pixels = (f64::from(width_pt * scale) * f64::from(height_pt * scale)) as u64;
    if pixels > MAX_PIXELS {
        return Err(format!(
            "that mark would need {pixels} pixels, past the {MAX_PIXELS} limit"
        ));
    }

    let pixmap = typst_render::render(
        page,
        &typst_render::RenderOptions {
            pixel_per_pt: f64::from(scale).into(),
            render_bleed: false,
        },
    );
    Ok(RasterisedText {
        pixel_width: pixmap.width(),
        pixel_height: pixmap.height(),
        // `tiny_skia` hands back premultiplied RGBA; `take` gives the bytes in
        // the order a PDF image wants them.
        rgba: pixmap.take(),
        width_pt,
        height_pt,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    id: u64,
    revision: u64,
    source: String,
    width_pt: f32,
    size_pt: f32,
    rgb: (u8, u8, u8),
}

#[derive(Debug, Serialize, Deserialize)]
struct Response {
    id: u64,
    revision: u64,
    result: Result<String, String>,
}

/// Run the newline-framed compiler role of the Pulpit executable.
pub fn run_worker() {
    let input = std::io::stdin();
    let mut output = std::io::BufWriter::new(std::io::stdout().lock());
    for line in input.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Request>(&line) else {
            continue;
        };
        let response = Response {
            id: request.id,
            revision: request.revision,
            result: render(
                &request.source,
                request.width_pt,
                request.size_pt,
                request.rgb,
            ),
        };
        if serde_json::to_writer(&mut output, &response).is_err()
            || output.write_all(b"\n").is_err()
            || output.flush().is_err()
        {
            break;
        }
    }
}

struct Worker {
    child: Child,
    input: ChildStdin,
    replies: mpsc::Receiver<Response>,
}

impl Worker {
    fn spawn() -> std::io::Result<Self> {
        // The same fork-bomb bound the render and media supervisors keep: a
        // worker that spawns workers grows exponentially and takes the machine
        // down before any deadline or restart budget can notice. This site
        // used to set `PULPIT_WORKER_PROCESS`, which nothing ever read, and
        // never touched the marker that is actually checked — so the one bound
        // that holds had been forgotten here.
        pulpit_core::ipc::worker::spawn_guard("typst worker")?;
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--typst-worker")
            .env(pulpit_core::ipc::WORKER_MARKER, "1")
            // stderr is dropped rather than inherited: typst writes diagnostics
            // for markup the reader is still editing, and they are reported
            // through the compile result instead of the terminal.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("missing stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("missing stdout"))?;
        let (send, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if line.len() > 8 * 1024 * 1024 {
                    break;
                }
                if let Ok(response) = serde_json::from_str(&line) {
                    if send.send(response).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            input,
            replies,
        })
    }

    fn send(&mut self, request: &Request) -> std::io::Result<()> {
        serde_json::to_writer(&mut self.input, request)?;
        self.input.write_all(b"\n")?;
        self.input.flush()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone)]
pub struct RenderedText {
    pub handle: Option<svg::Handle>,
    /// Width divided by height from the SVG viewBox.
    pub aspect: f32,
    pub error: Option<String>,
}

struct Entry {
    source: String,
    revision: u64,
    due: Instant,
    in_flight: Option<Instant>,
    rendered: Option<RenderedText>,
}

/// Debounces edits and retains the last complete SVG while a replacement is
/// compiling or has failed.
pub struct Coordinator {
    worker: Option<Worker>,
    entries: HashMap<u64, Entry>,
    next_revision: u64,
    snapshot: Arc<HashMap<u64, RenderedText>>,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self {
            worker: None,
            entries: HashMap::new(),
            next_revision: 1,
            snapshot: Arc::new(HashMap::new()),
        }
    }
}

impl Coordinator {
    const DEBOUNCE: Duration = Duration::from_millis(80);
    const DEADLINE: Duration = Duration::from_millis(750);

    pub fn sync(&mut self, annotations: &pulpit_core::annotation::Annotations, now: Instant) {
        let present: HashSet<_> = annotations.texts.iter().map(|mark| mark.id).collect();
        self.entries.retain(|id, _| present.contains(id));
        for mark in &annotations.texts {
            let changed = self
                .entries
                .get(&mark.id)
                .is_none_or(|entry| entry.source != mark.text);
            if changed {
                let rendered = self
                    .entries
                    .remove(&mark.id)
                    .and_then(|entry| entry.rendered);
                let revision = self.next_revision;
                self.next_revision = self.next_revision.wrapping_add(1).max(1);
                self.entries.insert(
                    mark.id,
                    Entry {
                        source: mark.text.clone(),
                        revision,
                        due: now + Self::DEBOUNCE,
                        in_flight: None,
                        rendered,
                    },
                );
            }
        }
        self.refresh_snapshot();
    }

    pub fn service(&mut self, annotations: &pulpit_core::annotation::Annotations, now: Instant) {
        if self.entries.values().any(|entry| {
            entry
                .in_flight
                .is_some_and(|at| now.duration_since(at) > Self::DEADLINE)
        }) {
            self.worker = None;
            for entry in self.entries.values_mut() {
                if entry.in_flight.take().is_some() {
                    entry.due = now;
                }
            }
        }
        if let Some(worker) = &mut self.worker {
            while let Ok(response) = worker.replies.try_recv() {
                let Some(entry) = self.entries.get_mut(&response.id) else {
                    continue;
                };
                if entry.revision != response.revision {
                    continue;
                }
                entry.in_flight = None;
                match response.result {
                    Ok(svg) => {
                        let aspect = svg_aspect(&svg).unwrap_or(1.0);
                        entry.rendered = Some(RenderedText {
                            handle: Some(svg::Handle::from_memory(svg.into_bytes())),
                            aspect,
                            error: None,
                        })
                    }
                    Err(error) => {
                        if let Some(rendered) = &mut entry.rendered {
                            rendered.error = Some(error);
                        } else {
                            entry.rendered = Some(RenderedText {
                                handle: None,
                                aspect: 1.0,
                                error: Some(error),
                            });
                        }
                    }
                }
            }
        }
        let marks: HashMap<_, _> = annotations
            .texts
            .iter()
            .map(|mark| (mark.id, mark))
            .collect();
        let due: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(&id, entry)| {
                (entry.in_flight.is_none() && entry.due <= now).then_some((id, entry.revision))
            })
            .collect();
        for (id, revision) in due {
            let Some(mark) = marks.get(&id) else { continue };
            if self.worker.is_none() {
                self.worker = Worker::spawn().ok();
            }
            let Some(worker) = &mut self.worker else {
                continue;
            };
            let (r, g, b) = mark.color.rgb();
            let request = Request {
                id,
                revision,
                source: mark.text.clone(),
                // A label being written is set to the room left between it and
                // the edge of the page; one read back out of the document is
                // set to the box the annotation occupies, so that its lines
                // break where they broke when it was made.
                width_pt: match mark.fit {
                    Some((width, _)) => (width * 1000.0).max(20.0),
                    None => ((1.0 - mark.position.0) * 1000.0).max(20.0),
                },
                size_pt: mark.size * 1000.0,
                rgb: ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8),
            };
            if worker.send(&request).is_ok() {
                if let Some(entry) = self.entries.get_mut(&id) {
                    entry.in_flight = Some(now);
                }
            } else {
                self.worker = None;
            }
        }
        self.refresh_snapshot();
    }

    pub fn snapshot(&self) -> &Arc<HashMap<u64, RenderedText>> {
        &self.snapshot
    }

    fn refresh_snapshot(&mut self) {
        self.snapshot = Arc::new(
            self.entries
                .iter()
                .filter_map(|(&id, entry)| entry.rendered.clone().map(|rendered| (id, rendered)))
                .collect(),
        );
    }
}

fn svg_aspect(svg: &str) -> Option<f32> {
    let view_box = svg.split("viewBox=\"").nth(1)?.split('"').next()?;
    let mut values = view_box
        .split_whitespace()
        .filter_map(|value| value.parse::<f32>().ok());
    let _x = values.next()?;
    let _y = values.next()?;
    let width = values.next()?;
    let height = values.next()?;
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some(width / height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_markup_and_math_to_transparent_svg() {
        let svg = render("*Pulpit* $x^2$", 360.0, 24.0, (0, 0, 0)).unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(!svg.contains("fill=\"#ffffff\""));
    }

    #[test]
    fn math_uses_typsts_explicit_variable_separation() {
        assert!(render("$e=m c^2$", 360.0, 24.0, (0, 0, 0)).is_ok());
        let error = render("$e=mc^2$", 360.0, 24.0, (0, 0, 0)).unwrap_err();
        assert!(error.contains("unknown variable: mc"));
    }

    #[test]
    fn math_svg_viewport_has_room_above_and_below_the_frame() {
        let svg = render("$e=m c^2$", 360.0, 24.0, (0, 0, 0)).unwrap();
        // The 4.8pt top and bottom gutters alone account for 9.6pt. A
        // viewport shorter than that cannot contain the requested padding.
        let view_box = svg
            .split("viewBox=\"")
            .nth(1)
            .and_then(|tail| tail.split('"').next())
            .expect("SVG has a viewBox");
        let height: f32 = view_box
            .split_whitespace()
            .nth(3)
            .expect("viewBox has a height")
            .parse()
            .expect("viewBox height is numeric");
        assert!(height > 9.6);
    }

    #[test]
    fn svg_aspect_comes_from_its_complete_viewport() {
        assert_eq!(svg_aspect(r#"<svg viewBox="0 0 300 75">"#), Some(4.0));
    }

    #[test]
    fn rejects_files_from_annotation_source() {
        let error = render("#read(\"/etc/passwd\")", 360.0, 24.0, (0, 0, 0)).unwrap_err();
        assert!(!error.is_empty());
    }
}
