//! What a layout actually does with the space it is given.
//!
//! A layout is designed at one aspect ratio and shown on whatever screen the
//! venue provides. The consequences are usually invisible until the talk
//! starts: a slide pane that letterboxes away a third of its cell looks fine
//! in the editor, because the editor is not the projector.
//!
//! Everything here is pure arithmetic over the computed placements, so the
//! consequences can be measured and shown *without* the application ever
//! choosing a layout on the presenter's behalf. Recommending is allowed;
//! selecting is not.

use crate::layout::model::{AspectRatio, LayoutId};
use crate::layout::tree::Frame;
#[cfg(test)]
use crate::layout::tree::Placement;

/// Where the slide actually lands inside a pane, and what that costs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FittedSlide {
    /// The pane the slide was given.
    pub cell: Frame,
    /// The slide as drawn, aspect-fit and centred inside the cell.
    pub drawn: Frame,
}

impl FittedSlide {
    /// Aspect-fit `ratio` inside `cell`, exactly as `ContentFit::Contain`
    /// does when the slide is drawn.
    pub fn fit(cell: Frame, ratio: f32) -> Option<FittedSlide> {
        if cell.width <= 0.0 || cell.height <= 0.0 || !ratio.is_finite() || ratio <= 0.0 {
            return None;
        }
        let cell_ratio = cell.width / cell.height;
        let (width, height) = if cell_ratio > ratio {
            // The cell is wider than the slide: bars left and right.
            (cell.height * ratio, cell.height)
        } else {
            (cell.width, cell.width / ratio)
        };
        Some(FittedSlide {
            cell,
            drawn: Frame::new(
                cell.x + (cell.width - width) / 2.0,
                cell.y + (cell.height - height) / 2.0,
                width,
                height,
            ),
        })
    }

    /// Area of the pane the slide cannot use, in square points.
    pub fn padding_area(&self) -> f32 {
        (self.cell.width * self.cell.height - self.drawn.width * self.drawn.height).max(0.0)
    }

    /// The share of the pane lost to letterboxing, in `0.0..=1.0`.
    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn padding_fraction(&self) -> f32 {
        let cell = self.cell.width * self.cell.height;
        if cell <= 0.0 {
            return 0.0;
        }
        (self.padding_area() / cell).clamp(0.0, 1.0)
    }

    /// Which way the bars run. `None` when the fit is exact.
    pub fn bars(&self) -> Option<Bars> {
        const EPSILON: f32 = 0.5;
        let horizontal = self.cell.width - self.drawn.width > EPSILON;
        let vertical = self.cell.height - self.drawn.height > EPSILON;
        match (horizontal, vertical) {
            (true, false) => Some(Bars::LeftAndRight),
            (false, true) => Some(Bars::TopAndBottom),
            // Both at once means the cell and the slide differ in scale but
            // not in shape, which rounding can produce; report the larger.
            (true, true) => Some(
                if self.cell.width - self.drawn.width > self.cell.height - self.drawn.height {
                    Bars::LeftAndRight
                } else {
                    Bars::TopAndBottom
                },
            ),
            (false, false) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bars {
    LeftAndRight,
    TopAndBottom,
}

impl Bars {
    pub fn label(self) -> &'static str {
        match self {
            Bars::LeftAndRight => "bars left and right",
            Bars::TopAndBottom => "bars top and bottom",
        }
    }
}

/// How much of a layout's space the slide panes actually use.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceReport {
    /// The screen the layout was measured against.
    pub screen: Frame,
    /// The slide aspect ratio the measurement assumed.
    pub slide_ratio: f32,
    /// One entry per slide pane, in the order the placements gave them.
    pub slides: Vec<FittedSlide>,
}

/// A layout wasting more than this share of its slide panes is worth saying
/// something about. Below it, the bars are the document's own shape and not
/// a choice the presenter can do anything about.
pub const SUBSTANTIAL_WASTE: f32 = 0.20;

impl SpaceReport {
    /// Measure a layout's slide panes at a given screen size and slide ratio.
    ///
    /// `slide_cells` supplies the placements that hold slide content; which
    /// widgets those are is the caller's business, so this stays independent
    /// of the widget catalogue.
    pub fn measure(screen: Frame, slide_ratio: f32, slide_cells: &[Frame]) -> SpaceReport {
        SpaceReport {
            screen,
            slide_ratio,
            slides: slide_cells
                .iter()
                .filter_map(|cell| FittedSlide::fit(*cell, slide_ratio))
                .collect(),
        }
    }

    /// Total pane area lost to letterboxing across every slide pane.
    pub fn wasted_area(&self) -> f32 {
        self.slides.iter().map(FittedSlide::padding_area).sum()
    }

    /// The share of all slide-pane area lost to letterboxing.
    pub fn wasted_fraction(&self) -> f32 {
        let panes: f32 = self
            .slides
            .iter()
            .map(|slide| slide.cell.width * slide.cell.height)
            .sum();
        if panes <= 0.0 {
            return 0.0;
        }
        (self.wasted_area() / panes).clamp(0.0, 1.0)
    }

    /// Is this worth telling the presenter about?
    pub fn wastes_substantial_space(&self) -> bool {
        self.wasted_fraction() > SUBSTANTIAL_WASTE
    }

    /// A sentence describing the cost, or `None` when there is nothing to say.
    ///
    /// Deliberately factual: it reports what the space is doing and leaves the
    /// decision to the presenter.
    pub fn warning(&self) -> Option<String> {
        if !self.wastes_substantial_space() {
            return None;
        }
        let percent = (self.wasted_fraction() * 100.0).round() as u32;
        let bars = self
            .slides
            .iter()
            .find_map(FittedSlide::bars)
            .map(Bars::label)
            .unwrap_or("padding");
        Some(format!(
            "This layout leaves {percent}% of its slide panes empty at this screen shape ({bars})."
        ))
    }
}

/// A layout worth suggesting, and why.
///
/// Selection stays entirely explicit: this ranks, it never applies.
#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    /// Carried alongside the name (§82.4) so a caller that only needs to
    /// show and select a recommendation never has to rebuild
    /// `built_in_layouts()` and search it by name to get back to the
    /// layout it is about.
    pub id: LayoutId,
    pub layout: String,
    pub wasted_fraction: f32,
    pub reason: String,
}

/// Rank candidate layouts by how little slide-pane space they waste.
///
/// `candidates` pairs a layout id and name with the slide cells it produces
/// at the screen being measured. Ties keep their input order, so the list is
/// stable and a presenter's own layout is never reshuffled underneath them.
pub fn recommend(
    screen: Frame,
    slide_ratio: f32,
    candidates: &[(LayoutId, String, Vec<Frame>)],
) -> Vec<Recommendation> {
    let mut ranked: Vec<Recommendation> = candidates
        .iter()
        .map(|(id, name, cells)| {
            let report = SpaceReport::measure(screen, slide_ratio, cells);
            let wasted = report.wasted_fraction();
            Recommendation {
                id: id.clone(),
                layout: name.clone(),
                wasted_fraction: wasted,
                reason: if wasted <= 0.01 {
                    "fills its slide panes at this screen shape".to_string()
                } else {
                    format!(
                        "leaves {}% of its slide panes empty",
                        (wasted * 100.0).round() as u32
                    )
                },
            }
        })
        .collect();
    ranked.sort_by(|left, right| {
        left.wasted_fraction
            .partial_cmp(&right.wasted_fraction)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
}

/// Preview a slide of one ratio inside a presenter screen of another.
///
/// The two ratios are independent: a 4:3 deck on a 16:9 presenter display is
/// an ordinary situation, and the editor must be able to show it without the
/// presenter first having to connect that projector.
#[allow(dead_code)] // reached by its tests, not by the application
pub fn preview_frame(screen: Frame, slide_ratio: AspectRatio) -> Option<FittedSlide> {
    FittedSlide::fit(screen, slide_ratio.ratio())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(width: f32, height: f32) -> Frame {
        Frame::new(0.0, 0.0, width, height)
    }

    const WIDE: f32 = 16.0 / 9.0;
    const CLASSIC: f32 = 4.0 / 3.0;

    #[test]
    fn a_slide_matching_its_pane_wastes_nothing() {
        let fitted = FittedSlide::fit(cell(1600.0, 900.0), WIDE).unwrap();
        assert_eq!(fitted.drawn, cell(1600.0, 900.0));
        assert!(fitted.padding_area() < 1.0);
        assert_eq!(fitted.bars(), None);
    }

    #[test]
    fn a_wide_slide_in_a_square_pane_gets_bars_top_and_bottom() {
        let fitted = FittedSlide::fit(cell(1000.0, 1000.0), WIDE).unwrap();
        assert!((fitted.drawn.width - 1000.0).abs() < 0.1);
        assert!((fitted.drawn.height - 562.5).abs() < 0.1);
        assert_eq!(fitted.bars(), Some(Bars::TopAndBottom));
        // Centred, not pinned to the top.
        assert!((fitted.drawn.y - 218.75).abs() < 0.1);
    }

    #[test]
    fn a_classic_slide_in_a_wide_pane_gets_bars_left_and_right() {
        let fitted = FittedSlide::fit(cell(1600.0, 900.0), CLASSIC).unwrap();
        assert!((fitted.drawn.height - 900.0).abs() < 0.1);
        assert!((fitted.drawn.width - 1200.0).abs() < 0.1);
        assert_eq!(fitted.bars(), Some(Bars::LeftAndRight));
        assert!((fitted.drawn.x - 200.0).abs() < 0.1);
    }

    #[test]
    fn the_padding_fraction_is_the_share_of_the_pane_the_slide_cannot_use() {
        // A 4:3 slide in a 16:9 pane uses 1200/1600 of the width.
        let fitted = FittedSlide::fit(cell(1600.0, 900.0), CLASSIC).unwrap();
        assert!((fitted.padding_fraction() - 0.25).abs() < 1e-3);
    }

    #[test]
    fn a_pane_with_no_area_yields_no_fit_rather_than_a_division_by_zero() {
        assert!(FittedSlide::fit(cell(0.0, 900.0), WIDE).is_none());
        assert!(FittedSlide::fit(cell(1600.0, 0.0), WIDE).is_none());
        assert!(FittedSlide::fit(cell(1600.0, 900.0), 0.0).is_none());
        assert!(FittedSlide::fit(cell(1600.0, 900.0), f32::NAN).is_none());
    }

    #[test]
    fn a_report_over_several_panes_sums_what_each_one_loses() {
        let report = SpaceReport::measure(
            cell(1600.0, 900.0),
            CLASSIC,
            &[cell(800.0, 900.0), cell(800.0, 900.0)],
        );
        assert_eq!(report.slides.len(), 2);
        // Each 800×900 pane holds a 4:3 slide at 800×600.
        assert!((report.wasted_fraction() - (1.0 - 600.0 / 900.0)).abs() < 1e-3);
    }

    #[test]
    fn a_layout_that_fits_its_slides_produces_no_warning() {
        let report = SpaceReport::measure(cell(1600.0, 900.0), WIDE, &[cell(1600.0, 900.0)]);
        assert!(!report.wastes_substantial_space());
        assert_eq!(report.warning(), None);
    }

    #[test]
    fn a_layout_that_wastes_substantial_space_says_so_factually() {
        let report = SpaceReport::measure(cell(1000.0, 1000.0), WIDE, &[cell(1000.0, 1000.0)]);
        assert!(report.wastes_substantial_space());
        let warning = report.warning().expect("should warn");
        assert!(warning.contains('%'), "{warning}");
        assert!(
            warning.contains("bars top and bottom"),
            "the warning says which way the bars run: {warning}"
        );
        // Factual, not directive: it must not tell the presenter what to pick.
        assert!(!warning.to_lowercase().contains("should"), "{warning}");
    }

    #[test]
    fn a_report_with_no_slide_panes_is_quiet_rather_than_dividing_by_zero() {
        let report = SpaceReport::measure(cell(1600.0, 900.0), WIDE, &[]);
        assert_eq!(report.wasted_fraction(), 0.0);
        assert!(!report.wastes_substantial_space());
        assert_eq!(report.warning(), None);
    }

    #[test]
    fn recommendations_are_ranked_by_wasted_space_and_never_applied() {
        let candidates = vec![
            (
                LayoutId("square".into()),
                "square panes".to_string(),
                vec![cell(900.0, 900.0)],
            ),
            (
                LayoutId("wide".into()),
                "wide pane".to_string(),
                vec![cell(1600.0, 900.0)],
            ),
        ];
        let ranked = recommend(cell(1600.0, 900.0), WIDE, &candidates);
        assert_eq!(ranked[0].layout, "wide pane");
        assert_eq!(ranked[0].id, LayoutId("wide".into()));
        assert!(ranked[0].wasted_fraction < ranked[1].wasted_fraction);
        assert!(ranked[0].reason.contains("fills"));
        assert!(ranked[1].reason.contains('%'));
    }

    #[test]
    fn equally_good_candidates_keep_their_input_order() {
        let candidates = vec![
            (
                LayoutId("first".into()),
                "first".to_string(),
                vec![cell(1600.0, 900.0)],
            ),
            (
                LayoutId("second".into()),
                "second".to_string(),
                vec![cell(800.0, 450.0)],
            ),
        ];
        let ranked = recommend(cell(1600.0, 900.0), WIDE, &candidates);
        assert_eq!(ranked[0].layout, "first");
        assert_eq!(ranked[1].layout, "second");
    }

    #[test]
    fn slide_ratio_and_screen_ratio_are_independent() {
        // A 4:3 deck previewed on a 16:9 presenter screen, which is what the
        // editor must be able to show without that projector attached.
        let fitted = preview_frame(cell(1600.0, 900.0), AspectRatio::FourThree).unwrap();
        assert_eq!(fitted.bars(), Some(Bars::LeftAndRight));

        let fitted = preview_frame(cell(1600.0, 900.0), AspectRatio::SixteenNine).unwrap();
        assert_eq!(fitted.bars(), None);
    }

    #[test]
    fn the_fitted_slide_stays_inside_its_pane() {
        for ratio in [WIDE, CLASSIC, 1.0, 0.5, 3.0] {
            let pane = Frame::new(100.0, 50.0, 640.0, 480.0);
            let fitted = FittedSlide::fit(pane, ratio).unwrap();
            assert!(fitted.drawn.x >= pane.x - 0.01);
            assert!(fitted.drawn.y >= pane.y - 0.01);
            assert!(fitted.drawn.x + fitted.drawn.width <= pane.x + pane.width + 0.01);
            assert!(fitted.drawn.y + fitted.drawn.height <= pane.y + pane.height + 0.01);
        }
    }

    #[test]
    fn placements_can_be_measured_directly_from_the_layout_tree() {
        // The report takes plain frames precisely so it does not need to know
        // which widget kinds hold slides.
        let placements = [
            Placement {
                id: crate::layout::NodeId(1),
                frame: cell(800.0, 900.0),
            },
            Placement {
                id: crate::layout::NodeId(2),
                frame: cell(800.0, 900.0),
            },
        ];
        let cells: Vec<Frame> = placements.iter().map(|placement| placement.frame).collect();
        let report = SpaceReport::measure(cell(1600.0, 900.0), WIDE, &cells);
        assert_eq!(report.slides.len(), 2);
    }
}
