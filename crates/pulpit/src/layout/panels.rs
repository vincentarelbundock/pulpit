//! How wide a layout's slide panels actually draw.
//!
//! The render plan needs one number per slide panel: the width, in physical
//! pixels, at which that panel's picture is drawn. It used to guess — 45% of
//! the window, plus a quarter for luck — and a guess cannot be right for two
//! panels of different sizes. In the shipped presenter layout the current
//! slide takes 72% of the width and the next slide 28%, so the single guessed
//! width was 21% too small for the panel the presenter reads and twice too
//! large for the one beside it. Custom layouts can put a slide panel at any
//! size at all, and a display can be any resolution at any scale factor, so
//! there is no constant that would work.
//!
//! The layout already knows. It is a tree of splits with fractions summing to
//! one, so a walk from the root gives every panel its share of the window,
//! and the drawn picture follows from that share, the page's aspect ratio and
//! the display's scale factor.
//!
//! Two deliberate roundings, both upward, because the failure modes are not
//! symmetric: a picture rendered a little too large is invisible, and one
//! rendered a little too small is soft text in front of a room. Split gutters
//! and cell padding are ignored rather than subtracted, and the result is
//! rounded up to a bucket.

use crate::layout::model::Layout;
use crate::layout::tree::{Direction, Node};
use crate::widgets::plan::PageRef;
use crate::widgets::registry;

/// Widths are quantised to this many pixels.
///
/// A panel's share of the window changes with every divider drag and every
/// window resize, and a picture that changes size changes texture. Buckets
/// mean an ordinary drag re-uses the frames already rendered instead of
/// minting a new size — and a new size, mid-talk, is the flicker this whole
/// design exists to prevent.
const BUCKET: u32 = 128;

/// No panel is rendered narrower than this, however small the layout makes
/// it: below it a slide is unreadable anyway, and the floor keeps a
/// pathological layout from asking for a thumbnail-sized audience frame.
const FLOOR: u32 = 512;

/// What a slide panel shows: the committed page, or one of its neighbours.
///
/// The distinction is the whole point of computing widths at all. The
/// committed page's panel is usually the large one and the neighbours' are
/// usually small, and rendering both at one width wastes pixels on one and
/// starves the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Current,
    Neighbour,
}

/// The physical width each role's panels draw at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlideWidths {
    pub current: u32,
    pub neighbour: u32,
}

impl SlideWidths {
    pub fn for_role(&self, role: Role) -> u32 {
        match role {
            Role::Current => self.current,
            Role::Neighbour => self.neighbour,
        }
    }

    /// Every distinct width, so the render plan can ask for each once.
    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn distinct(&self) -> Vec<u32> {
        if self.current == self.neighbour {
            vec![self.current]
        } else {
            vec![self.current, self.neighbour]
        }
    }
}

/// The width each slide panel of `layout` draws at, in physical pixels.
///
/// `window` is the presenter window in logical points, `scale` its device
/// pixel ratio, and `aspect` the page's width over its height.
///
/// A layout with no panel of a role gets the other role's width rather than a
/// guess: the number is then unused, and an unused number that matches
/// something real cannot mint a texture of its own.
pub fn slide_widths(layout: &Layout, window: (f32, f32), scale: f32, aspect: f32) -> SlideWidths {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let mut found: Vec<(Role, f32)> = Vec::new();
    walk(&layout.root, 1.0, 1.0, &mut found);

    let widest = |role: Role| -> Option<u32> {
        found
            .iter()
            .filter(|(panel_role, _)| *panel_role == role)
            .map(|(_, fraction)| drawn_width(*fraction, window, scale, aspect))
            .max()
    };
    let current = widest(Role::Current);
    let neighbour = widest(Role::Neighbour);
    // A layout with no slide panel at all still needs an answer, because the
    // projector's mirror and the stand-in machinery ask for one.
    let fallback = current.or(neighbour).unwrap_or(FLOOR);
    SlideWidths {
        current: current.unwrap_or(fallback),
        neighbour: neighbour.unwrap_or(fallback),
    }
}

/// Accumulate each slide panel's fraction of the window.
///
/// Only the width fraction is carried: a panel's height never limits the
/// drawn picture in any shipped layout, and treating a tall narrow cell as
/// though it did would render *narrower* than the panel — the one direction
/// that shows.
///
/// This used to match `WidgetKind` directly — `CurrentSlide` gets
/// `Role::Current`, `NextSlide`/`PreviousSlide` get `Role::Neighbour`, and
/// `PreviousCurrentNext` was special-cased with its own strip fractions
/// baked in here. That match is gone: every placed widget now declares its
/// frame needs through [`crate::widgets::registry::registration`]'s `plan`
/// hook (see [`crate::widgets::plan`]), and this walk only asks for the
/// plan and translates its [`PageRef`]s to the [`Role`] the rest of this
/// module already understood. A widget that shows the current slide as part
/// of some future compound panel needs only to declare it in its own
/// registration — nothing here has to learn a new kind.
fn walk(node: &Node, width: f32, height: f32, found: &mut Vec<(Role, f32)>) {
    match node {
        Node::Split(split) => {
            for (child, size) in split.children.iter().zip(split.sizes.iter()) {
                let (width, height) = match split.direction {
                    Direction::Horizontal => (width * size, height),
                    Direction::Vertical => (width, height * size),
                };
                walk(child, width, height, found);
            }
        }
        Node::Leaf(cell) => {
            let Some(widget) = cell.widget.as_ref() else {
                return;
            };
            let plan = registry::registration(widget.kind()).plan(widget, width);
            for need in plan.frames {
                let role = match need.page {
                    PageRef::Current => Role::Current,
                    PageRef::Previous | PageRef::Next => Role::Neighbour,
                };
                found.push((role, need.width_fraction));
            }
        }
    }
}

/// Whether any placed panel in `layout` actually declares a need for the
/// committed page's own role, or for a neighbour's.
///
/// Drawn at the same granularity as [`SlideWidths`] — `Role::Current` versus
/// `Role::Neighbour`, not Previous versus Next — because that is the
/// granularity the render plan's prefetch step has always worked at: a
/// neighbour page is asked for at `widths.neighbour` regardless of which
/// side it is on, and `SlideWidths` itself cannot tell Previous and Next
/// panels apart (a layout with only a Next panel still reports a `neighbour`
/// width). Answering "is there a neighbour panel at all" at the same
/// granularity keeps this exactly the flag that reproduces today's
/// behaviour: the shipped presenter layout has a Next panel and no Previous
/// one, and today's `live_slide_plan` prefetches both directions anyway,
/// because the two sides were never actually distinguished — only "is there
/// a neighbour panel, or not".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanelDemand {
    pub current: bool,
    pub neighbour: bool,
}

/// Collect which roles `layout`'s panels declare, reusing [`walk`] rather
/// than a second traversal.
pub fn demand(layout: &Layout) -> PanelDemand {
    let mut found: Vec<(Role, f32)> = Vec::new();
    walk(&layout.root, 1.0, 1.0, &mut found);
    PanelDemand {
        current: found.iter().any(|(role, _)| *role == Role::Current),
        neighbour: found.iter().any(|(role, _)| *role == Role::Neighbour),
    }
}

/// The physical width of the picture a panel of this fraction draws.
fn drawn_width(fraction: f32, window: (f32, f32), scale: f32, aspect: f32) -> u32 {
    let _ = aspect;
    let logical = (window.0.max(1.0) * fraction).max(1.0);
    let physical = (logical * scale).ceil() as u32;
    physical.max(FLOOR).div_ceil(BUCKET) * BUCKET
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::builtin;
    use crate::widgets::WidgetKind;

    /// The window the whole investigation was measured on.
    const WINDOW: (f32, f32) = (1800.0, 1125.0);
    const SCALE: f32 = 2.0;
    const WIDE: f32 = 16.0 / 9.0;

    #[test]
    fn the_shipped_layout_gets_its_two_real_widths() {
        let widths = slide_widths(&builtin::presenter_default(), WINDOW, SCALE, WIDE);
        // Current slide is 0.72 of 1800 = 1296 logical, doubled by the scale
        // factor: 2592, bucketed up to 2688. The guessed width was 2048, so
        // the panel a presenter reads was being upscaled by a quarter.
        assert_eq!(widths.current, 2688);
        // Next slide is 0.28 of 1800 = 504 logical → 1008 → 1024. The guess
        // rendered it at 2048: four times the pixels, for a panel a fifth of
        // the size.
        assert_eq!(widths.neighbour, 1024);
        assert_eq!(widths.distinct(), vec![2688, 1024]);
    }

    /// The point of computing rather than guessing: the same layout on a
    /// different display asks for different pictures.
    #[test]
    fn the_same_layout_scales_with_the_display() {
        let layout = builtin::presenter_default();
        let hidpi = slide_widths(&layout, WINDOW, 2.0, WIDE);
        let plain = slide_widths(&layout, WINDOW, 1.0, WIDE);
        assert!(hidpi.current > plain.current);
        // A 4K presenter at scale 1 needs what a half-size one at scale 2
        // needs: the pixels are what matter, not either number alone.
        let big = slide_widths(&layout, (3600.0, 2250.0), 1.0, WIDE);
        assert_eq!(big.current, hidpi.current);
    }

    /// A layout nobody shipped, which is the case a constant cannot serve.
    #[test]
    fn a_custom_layout_is_measured_not_assumed() {
        let mut layout = builtin::presenter_default();
        // Give the whole window to one slide panel.
        layout.root = Node::Leaf(crate::layout::tree::Cell {
            widget: Some(crate::widgets::Widget::new(WidgetKind::CurrentSlide)),
            ..crate::layout::tree::Cell::new(crate::layout::tree::NodeId(1))
        });
        let widths = slide_widths(&layout, WINDOW, SCALE, WIDE);
        // 1800 × 2 = 3600, rounded up to the next bucket.
        assert_eq!(widths.current, 3712);
        // No neighbour panel exists, so its width is the one that does — an
        // unused number that cannot mint a texture of its own.
        assert_eq!(widths.neighbour, widths.current);
        assert_eq!(widths.distinct(), vec![3712]);
    }

    #[test]
    fn the_three_up_strip_divides_its_own_cell() {
        let mut layout = builtin::presenter_default();
        layout.root = Node::Leaf(crate::layout::tree::Cell {
            widget: Some(crate::widgets::Widget::new(WidgetKind::PreviousCurrentNext)),
            ..crate::layout::tree::Cell::new(crate::layout::tree::NodeId(1))
        });
        let widths = slide_widths(&layout, WINDOW, SCALE, WIDE);
        // 46% and 27% of the strip, not the whole of it.
        assert_eq!(widths.current, 1664);
        assert_eq!(widths.neighbour, 1024);
    }

    /// Ordinary resize noise must not mint a new texture: a divider dragged a
    /// few points is the same picture.
    #[test]
    fn nearby_window_sizes_share_a_width() {
        let layout = builtin::presenter_default();
        let a = slide_widths(&layout, (1800.0, 1125.0), SCALE, WIDE);
        let b = slide_widths(&layout, (1810.0, 1131.0), SCALE, WIDE);
        assert_eq!(a, b, "a small drag re-uses the frames already rendered");
    }

    /// The old, kind-matching walk, kept only in this test as the value the
    /// plan-aggregating walk above must keep reproducing.
    fn old_walk(node: &Node, width: f32, height: f32, found: &mut Vec<(Role, f32)>) {
        use crate::widgets::WidgetKind;
        const STRIP_CURRENT: f32 = 46.0 / 100.0;
        const STRIP_SIDE: f32 = 27.0 / 100.0;
        match node {
            Node::Split(split) => {
                for (child, size) in split.children.iter().zip(split.sizes.iter()) {
                    let (width, height) = match split.direction {
                        Direction::Horizontal => (width * size, height),
                        Direction::Vertical => (width, height * size),
                    };
                    old_walk(child, width, height, found);
                }
            }
            Node::Leaf(cell) => {
                let Some(widget) = cell.widget.as_ref() else {
                    return;
                };
                match widget.kind() {
                    WidgetKind::CurrentSlide => found.push((Role::Current, width)),
                    WidgetKind::NextSlide | WidgetKind::PreviousSlide => {
                        found.push((Role::Neighbour, width))
                    }
                    WidgetKind::PreviousCurrentNext => {
                        found.push((Role::Current, width * STRIP_CURRENT));
                        found.push((Role::Neighbour, width * STRIP_SIDE));
                    }
                    _ => {}
                }
            }
        }
    }

    fn old_slide_widths(
        layout: &Layout,
        window: (f32, f32),
        scale: f32,
        aspect: f32,
    ) -> SlideWidths {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            16.0 / 9.0
        };
        let mut found: Vec<(Role, f32)> = Vec::new();
        old_walk(&layout.root, 1.0, 1.0, &mut found);
        let widest = |role: Role| -> Option<u32> {
            found
                .iter()
                .filter(|(panel_role, _)| *panel_role == role)
                .map(|(_, fraction)| drawn_width(*fraction, window, scale, aspect))
                .max()
        };
        let current = widest(Role::Current);
        let neighbour = widest(Role::Neighbour);
        let fallback = current.or(neighbour).unwrap_or(FLOOR);
        SlideWidths {
            current: current.unwrap_or(fallback),
            neighbour: neighbour.unwrap_or(fallback),
        }
    }

    /// The acceptance bar for the plan-aggregation refactor: for every
    /// built-in layout, the widths the new registry-driven walk produces are
    /// identical to what the old `WidgetKind`-matching walk produced, at
    /// several windows and scales.
    #[test]
    fn plan_aggregation_matches_the_old_kind_matching_walk() {
        let layouts = [
            builtin::presenter_default(),
            builtin::reader_default(crate::layout::model::AspectRatio::SixteenNine),
        ];
        let probes = [
            (WINDOW, SCALE, WIDE),
            ((1280.0, 720.0), 1.0, WIDE),
            ((3840.0, 2160.0), 2.0, 4.0 / 3.0),
        ];
        for layout in &layouts {
            for (window, scale, aspect) in probes {
                assert_eq!(
                    slide_widths(layout, window, scale, aspect),
                    old_slide_widths(layout, window, scale, aspect),
                );
            }
        }
    }

    #[test]
    fn the_shipped_layout_wants_a_current_and_a_neighbour_panel() {
        // The shipped presenter layout has a Current panel and a Next panel
        // (no Previous one) — both register as `Role::Neighbour`.
        let demand = demand(&builtin::presenter_default());
        assert_eq!(
            demand,
            PanelDemand {
                current: true,
                neighbour: true,
            }
        );
    }

    #[test]
    fn a_layout_with_only_a_current_slide_wants_no_neighbour() {
        let mut layout = builtin::presenter_default();
        layout.root = Node::Leaf(crate::layout::tree::Cell {
            widget: Some(crate::widgets::Widget::new(WidgetKind::CurrentSlide)),
            ..crate::layout::tree::Cell::new(crate::layout::tree::NodeId(1))
        });
        let demand = demand(&layout);
        assert_eq!(
            demand,
            PanelDemand {
                current: true,
                neighbour: false,
            }
        );
    }

    #[test]
    fn the_strip_wants_both_roles_too() {
        let mut layout = builtin::presenter_default();
        layout.root = Node::Leaf(crate::layout::tree::Cell {
            widget: Some(crate::widgets::Widget::new(WidgetKind::PreviousCurrentNext)),
            ..crate::layout::tree::Cell::new(crate::layout::tree::NodeId(1))
        });
        assert_eq!(
            demand(&layout),
            PanelDemand {
                current: true,
                neighbour: true,
            }
        );
    }

    #[test]
    fn nonsense_from_the_runtime_never_becomes_a_nonsense_render() {
        let layout = builtin::presenter_default();
        let sane = slide_widths(&layout, WINDOW, 1.0, WIDE);
        assert_eq!(slide_widths(&layout, WINDOW, 0.0, WIDE), sane);
        assert_eq!(slide_widths(&layout, WINDOW, f32::NAN, WIDE), sane);
        // A window with no width still yields something drawable.
        let tiny = slide_widths(&layout, (0.0, 0.0), SCALE, WIDE);
        assert!(tiny.current >= FLOOR);
    }
}
