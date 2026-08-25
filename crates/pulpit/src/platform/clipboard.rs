//! Putting an image on the system clipboard.
//!
//! Text is the toolkit's job: Iced has a clipboard and pulpit uses it. An
//! image is not — Iced's clipboard carries strings and nothing else — so this
//! is the one place the application reaches past the toolkit for a thing the
//! toolkit does not offer.
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
}
