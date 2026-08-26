//! Putting an image on the system clipboard.
//!
//! Text is the toolkit's job: Iced has a clipboard and pulpit uses it. An
//! image is not — Iced's clipboard carries strings and nothing else — so this
//! is the one place the application reaches past the toolkit for a thing the
//! toolkit does not offer.
//!
//! On Wayland the image goes out twice over: as `image/png` for anything that
//! pastes pixels, and as a freshly written PNG file under `text/uri-list` and
//! `x-special/gnome-copied-files` for anything that pastes *files*. A file
//! manager belongs to the second kind — Thunar asks the clipboard for a file
//! list and ignores raw image data — and a region that can only be pasted
//! into an image editor is half a copy. See [`copy_image_wayland`].
//!
//! Kept behind [`super::PlatformServices`] like every other desktop service,
//! for the usual reason: nothing above the boundary may ask which session it
//! is running in, and a session where this cannot work has to be able to say
//! so as an [`Outcome`] rather than by failing quietly.

use super::Outcome;

/// An image on its way to the clipboard: straight RGBA8, row-major, no
/// padding between rows.
///
/// Deliberately not a toolkit image type and not an encoded file. What the
/// renderer hands back is a buffer of exactly this shape, and every clipboard
/// implementation wants exactly this shape; encoding it to PNG in between
/// would be work done twice for the majority of pastes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes. Checked by [`ClipboardImage::new`], which
    /// is the only way to build one.
    pixels: Vec<u8>,
}

impl ClipboardImage {
    /// Build an image, or refuse the buffer.
    ///
    /// `None` for a zero dimension or a buffer that is not exactly the size
    /// the dimensions call for. A short buffer handed to a clipboard library
    /// is a read past the end of an allocation, so the check is here and not
    /// at the call site.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let wanted = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (pixels.len() == wanted).then_some(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// What the buffer weighs, for the diagnostics log.
    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }
}

/// Put `image` on the system clipboard.
///
/// The adapters for the three desktops all call this: one clipboard library
/// covers all of them, and three copies of the same four lines would be three
/// places for the error handling to drift apart. What differs between the
/// desktops is whether it *works*, which is what the [`Outcome`] reports.
///
/// A Wayland or X11 clipboard is owned by a live process rather than by the
/// system, so what this leaves behind is readable only while pulpit is
/// running. That is how every application on those sessions behaves and it is
/// not worth a warning; it is worth knowing when reading a bug report about a
/// paste that came back empty after a quit.
pub fn copy_image(image: &ClipboardImage) -> Outcome {
    let data = arboard::ImageData {
        width: image.width as usize,
        height: image.height as usize,
        bytes: std::borrow::Cow::Borrowed(image.pixels()),
    };
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => clipboard,
        // No clipboard at all — a headless session, a compositor that offers
        // no data-control protocol. Refused rather than failed: nothing broke,
        // this session simply has nowhere to put it.
        Err(error) => return Outcome::refused(format!("no clipboard in this session: {error}")),
    };
    match clipboard.set_image(data) {
        Ok(()) => Outcome::Done,
        Err(error) => Outcome::failed(format!("the clipboard refused the image: {error}")),
    }
}

/// Put `image` on a Wayland clipboard as pixels *and* as a file.
///
/// A file manager pastes files. Thunar and its GTK siblings ask the clipboard
/// for `x-special/gnome-copied-files` or `text/uri-list` and ignore raw image
/// data, so a region offered as `image/png` alone pastes into GIMP and into
/// no directory anywhere. arboard offers one format at a time; this goes to
/// `wl-clipboard-rs` — the same crate arboard's Wayland support is built on —
/// to make all three offers at once.
///
/// The PNG is written into `stash` first, named by its content so copying the
/// same region twice reuses one file and copying a different region cannot
/// overwrite a file whose paste is still pending. The stash belongs in the
/// cache directory: a file-manager paste *copies* the file out, so it only
/// has to survive until then, and anything older than a day is swept on the
/// way through.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn copy_image_wayland(image: &ClipboardImage, stash: &std::path::Path) -> Outcome {
    use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};

    let png = match encode_png(image) {
        Ok(png) => png,
        Err(reason) => return Outcome::failed(reason),
    };
    let name = format!("band-{}.png", &blake3::hash(&png).to_hex().as_str()[..16]);
    if let Err(error) = std::fs::create_dir_all(stash) {
        return Outcome::failed(format!(
            "no clipboard stash at {}: {error}",
            stash.display()
        ));
    }
    sweep_stash(stash, &name);
    let path = stash.join(&name);
    if let Err(error) = std::fs::write(&path, &png) {
        return Outcome::failed(format!("could not write {}: {error}", path.display()));
    }

    let uri = file_uri(&path);
    let offer = |bytes: Vec<u8>, mime: &str| MimeSource {
        source: Source::Bytes(bytes.into()),
        mime_type: MimeType::Specific(mime.into()),
    };
    let sources = vec![
        offer(png, "image/png"),
        // The uri-list grammar ends every line in CRLF, and more than one
        // paster rejects a list without it.
        offer(format!("{uri}\r\n").into_bytes(), "text/uri-list"),
        // The GTK file managers ask for this one first; the verb tells them
        // the paste is a copy rather than a move.
        offer(
            format!("copy\n{uri}").into_bytes(),
            "x-special/gnome-copied-files",
        ),
    ];
    match Options::new().copy_multi(sources) {
        Ok(()) => Outcome::Done,
        Err(error) => Outcome::failed(format!("the Wayland clipboard refused the offers: {error}")),
    }
}

/// The image as one PNG, encoded once for every offer that carries it.
#[cfg(all(unix, not(target_os = "macos")))]
fn encode_png(image: &ClipboardImage) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            image.pixels(),
            image.width,
            image.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| format!("could not encode the region as PNG: {error}"))?;
    Ok(png)
}

/// `path` as a `file://` URI, every byte outside the unreserved set escaped.
///
/// Built by hand rather than through a URL crate because the input is narrow:
/// an absolute path this process just wrote. Escaping bytes rather than
/// characters keeps a non-UTF-8 cache path from ever producing an invalid
/// URI.
#[cfg(all(unix, not(target_os = "macos")))]
fn file_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// Drop stash files a day past their copy, sparing the one being offered.
///
/// Best-effort on every count: a file that cannot be read or removed is left
/// for the next sweep, because failing a copy over housekeeping would be
/// backwards.
#[cfg(all(unix, not(target_os = "macos")))]
fn sweep_stash(stash: &std::path::Path, keep: &str) {
    const A_DAY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(stash) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        if entry.file_name().to_str() == Some(keep) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > A_DAY);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_must_match_its_dimensions() {
        assert!(ClipboardImage::new(2, 2, vec![0; 16]).is_some());
        assert!(
            ClipboardImage::new(2, 2, vec![0; 15]).is_none(),
            "a short buffer is a read past the end, not an image"
        );
        assert!(
            ClipboardImage::new(2, 2, vec![0; 17]).is_none(),
            "a long buffer means the caller and the callee disagree about the size"
        );
    }

    #[test]
    fn a_zero_dimension_is_not_an_image() {
        assert!(ClipboardImage::new(0, 4, Vec::new()).is_none());
        assert!(ClipboardImage::new(4, 0, Vec::new()).is_none());
    }

    /// The URI is what Thunar resolves; a path with a space in it is the
    /// classic way to hand a file manager a file that does not exist.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_file_uri_escapes_what_a_path_may_carry() {
        assert_eq!(
            file_uri(std::path::Path::new("/tmp/a b/band.png")),
            "file:///tmp/a%20b/band.png"
        );
        assert_eq!(
            file_uri(std::path::Path::new("/café/band.png")),
            "file:///caf%C3%A9/band.png"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn the_region_encodes_as_a_png_file() {
        let image = ClipboardImage::new(2, 2, vec![255; 16]).expect("a well-formed image");
        let png = encode_png(&image).expect("encoding cannot fail on a well-formed image");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG signature");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn the_sweep_takes_the_stale_and_spares_the_offered() {
        let stash = tempfile::tempdir().expect("a stash");
        let stale = stash.path().join("band-old.png");
        let kept = stash.path().join("band-new.png");
        std::fs::write(&stale, b"old").expect("write");
        std::fs::write(&kept, b"new").expect("write");
        let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        let stamp = std::fs::File::options()
            .append(true)
            .open(&stale)
            .expect("reopen");
        stamp
            .set_modified(two_days_ago)
            .expect("set a modification time");
        drop(stamp);

        sweep_stash(stash.path(), "band-new.png");
        assert!(!stale.exists(), "a two-day-old band should be swept");
        assert!(kept.exists(), "the file being offered must survive");

        // A second file from today survives a sweep for someone else: it may
        // still be the target of a paste nobody has finished.
        let fresh = stash.path().join("band-fresh.png");
        std::fs::write(&fresh, b"fresh").expect("write");
        sweep_stash(stash.path(), "band-new.png");
        assert!(fresh.exists());
    }
}
