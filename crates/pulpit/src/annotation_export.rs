//! Turning the presenter's marks into something a PDF page can carry.
//!
//! Ink and highlighter strokes stay vector: they are polylines already, and a
//! polyline is what a PDF content stream draws best. Text marks cannot follow
//! them, because a text mark is Typst — its glyphs, its maths, its line
//! breaking — and none of that is a thing PDFium can be asked to reproduce.
//! They are rasterised here instead, from exactly the compile the panels
//! showed, so the file says what the room saw.
//!
//! Nothing here talks to PDFium or touches a file; it produces the stamps and
//! the renderer worker does the writing.

use pulpit_core::annotation::{InkStroke, TextMark};
use pulpit_core::notes::NotesMapping;
use pulpit_render::pdf::{PageStamp, StampImage};

/// How many pixels a text annotation is rasterised at per point it occupies
/// on the page.
///
/// Four is 288 dpi against the PDF's 72: past what a projector resolves and
/// past what a printer does with a page-sized picture, which is what makes it
/// safe for the picture to be the only record of the glyphs.
const TEXT_SCALE: f32 = 4.0;

/// Rasterised text is bounded independently of the page it sits on, because
/// a mark placed near the left edge of a large page asks for a very wide
/// picture and the presenter is waiting on the save.
const MAX_TEXT_PIXELS: u64 = 4096 * 4096;

/// One page's worth of marks, plus whatever could not be drawn.
pub struct Export {
    pub pages: Vec<PageStamp>,
    /// Text marks that would not compile, named for the presenter. A deck
    /// still exports without them: a broken formula is not a reason to
    /// refuse to save the forty strokes around it.
    pub diagnostics: Vec<String>,
}

/// Build the stamps for every annotated slide.
///
/// `page_width` gives a physical page's width in points, which is what sets
/// the resolution a text mark is rasterised at.
pub fn build(
    marks: &[(usize, Vec<InkStroke>, Vec<TextMark>)],
    mapping: &NotesMapping,
    pdf_pages: usize,
    page_width: impl Fn(usize) -> f32,
) -> Export {
    let mut pages = Vec::new();
    let mut diagnostics = Vec::new();
    for (slide, strokes, texts) in marks {
        // The mark was made over whatever the panel was showing, which for a
        // split-page deck is half a physical page. `audience_source` is the
        // same answer the panel asked for, so the ink lands where it was put.
        let Some(source) = mapping.audience_source(*slide, pdf_pages) else {
            continue;
        };
        let mut images = Vec::new();
        for mark in texts {
            match rasterise(mark, source.region.width * page_width(source.pdf_page)) {
                Ok(Some(image)) => images.push(image),
                Ok(None) => {}
                Err(reason) => diagnostics.push(format!(
                    "slide {}: a text annotation could not be drawn ({reason})",
                    slide + 1
                )),
            }
        }
        if strokes.is_empty() && images.is_empty() {
            continue;
        }
        pages.push(PageStamp {
            page: source.pdf_page,
            region: source.region,
            strokes: strokes.clone(),
            images,
        });
    }
    Export { pages, diagnostics }
}

/// Compile one text mark and rasterise it at the size it occupies on the
/// page. `region_points` is the width of the area the mark is placed in.
fn rasterise(mark: &TextMark, region_points: f32) -> Result<Option<StampImage>, String> {
    // The panel lays the text out in the space to the right of where it was
    // placed, and the compile arguments below are byte-for-byte the ones the
    // panel used — so the exported picture breaks its lines in the same
    // places rather than merely saying the same words.
    let width = 1.0 - mark.position.0;
    if width <= 0.0 {
        return Ok(None);
    }
    let (red, green, blue) = mark.color.rgb();
    let svg = crate::typst_annotation::render(
        &mark.text,
        ((1.0 - mark.position.0) * 1000.0).max(20.0),
        mark.size * 1000.0,
        (
            (red * 255.0) as u8,
            (green * 255.0) as u8,
            (blue * 255.0) as u8,
        ),
    )?;

    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return Ok(None);
    }
    let points = width * region_points;
    let pixel_width = (points * TEXT_SCALE).round().clamp(1.0, 4096.0) as u32;
    let pixel_height = ((pixel_width as f32) * size.height() / size.width())
        .round()
        .clamp(1.0, 4096.0) as u32;
    if u64::from(pixel_width) * u64::from(pixel_height) > MAX_TEXT_PIXELS {
        return Err("the annotation is too large to rasterise".into());
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixel_width, pixel_height)
        .ok_or("cannot allocate the annotation's pixels")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(
            pixel_width as f32 / size.width(),
            pixel_height as f32 / size.height(),
        ),
        &mut pixmap.as_mut(),
    );

    Ok(Some(StampImage {
        x: mark.position.0,
        y: mark.position.1,
        width,
        pixel_width,
        pixel_height,
        rgba: demultiply(pixmap.take()),
    }))
}

/// tiny-skia composites in premultiplied alpha; PDF images do not. Dividing
/// the colour back out is what keeps a half-transparent glyph from arriving
/// in the file half black.
fn demultiply(mut pixels: Vec<u8>) -> Vec<u8> {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3];
        if alpha == 0 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else if alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                    .min(255) as u8;
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::annotation::{InkColor, StrokeKind};

    fn stroke() -> InkStroke {
        InkStroke {
            points: vec![(0.1, 0.1), (0.4, 0.4)],
            width: 0.004,
            color: InkColor::Red,
            kind: StrokeKind::Ink,
        }
    }

    #[test]
    fn a_slide_maps_to_the_physical_page_and_region_it_was_drawn_over() {
        // Alternating pages: slide 1 is physical page 2, whole page.
        let mapping = NotesMapping::PairedPages(pulpit_core::notes::PairedRule::Alternating {
            notes_first: false,
        });
        let export = build(&[(1, vec![stroke()], Vec::new())], &mapping, 6, |_| 720.0);
        assert_eq!(export.pages.len(), 1);
        assert_eq!(export.pages[0].page, 2);
        assert_eq!(export.pages[0].strokes.len(), 1);
        assert!(export.diagnostics.is_empty());
    }

    #[test]
    fn a_slide_the_mapping_does_not_reach_is_skipped_rather_than_misplaced() {
        let export = build(
            &[(99, vec![stroke()], Vec::new())],
            &NotesMapping::SlidesOnly,
            4,
            |_| 720.0,
        );
        assert!(export.pages.is_empty());
    }

    #[test]
    fn a_split_page_deck_carries_the_half_the_marks_were_made_on() {
        let mapping = NotesMapping::SplitPage {
            slide: pulpit_core::notes::Region::left_half(),
            notes: pulpit_core::notes::Region::right_half(),
        };
        let export = build(&[(0, vec![stroke()], Vec::new())], &mapping, 2, |_| 720.0);
        assert_eq!(export.pages.len(), 1);
        assert_eq!(
            export.pages[0].region,
            pulpit_core::notes::Region::left_half()
        );
    }

    #[test]
    fn a_slide_whose_marks_all_vanished_produces_no_stamp() {
        let export = build(
            &[(0, Vec::new(), Vec::new())],
            &NotesMapping::SlidesOnly,
            4,
            |_| 720.0,
        );
        assert!(export.pages.is_empty());
    }

    #[test]
    fn premultiplied_pixels_come_back_at_full_strength() {
        // Half-transparent red, premultiplied: 128 of 255 red at alpha 128.
        assert_eq!(demultiply(vec![128, 0, 0, 128]), vec![255, 0, 0, 128]);
        // A fully transparent pixel carries no colour at all.
        assert_eq!(demultiply(vec![9, 9, 9, 0]), vec![0, 0, 0, 0]);
        // An opaque pixel is left exactly alone.
        assert_eq!(demultiply(vec![10, 20, 30, 255]), vec![10, 20, 30, 255]);
    }
}
