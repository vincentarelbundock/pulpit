//! The presenter's grid of thumbnails (§79.4): the overview toggle, its
//! scroll position and keyboard navigation, and the pure grid-geometry
//! helpers ([`grid_target`], [`settled_selection`], [`visible_centre`]) that
//! decide where a key or a settling scroll lands.
//!
//! [`OverviewGrid`] is the shape the view last laid the grid out in —
//! columns, row height, viewport height — recorded by the view pass and
//! read back here (and by `app::thumbnails`, for warming) to answer "where
//! is the presenter looking" without either module reaching into the
//! other's internals.

use std::time::Instant;

use iced::Task;

use pulpit_core::Command as Nav;

use super::{App, Message, OVERVIEW_SCROLL_CLAIM};

/// Where an arrow key lands in the overview grid.
///
/// `None` means the key is not one the grid owns and should go on to the
/// keymap; `Some(None)` means it is, but there is nowhere to go — the edge of
/// the grid absorbs the press rather than letting it move the audience.
fn grid_target(
    key: &str,
    current: usize,
    count: usize,
    columns: usize,
    page_rows: usize,
) -> Option<Option<usize>> {
    let columns = columns.max(1);
    let last = count.saturating_sub(1);
    // A page is a screenful of rows, so the selection moves by exactly what
    // the eye just read; a viewport too short to hold a whole row still
    // moves one.
    let page = columns * page_rows.max(1);
    // The grid answers to the vim keys as well as the arrows, in the vim
    // sense rather than the navigation one: here `j` and `k` move between
    // rows and `h` and `l` along one, because this is a grid being looked
    // over and not a deck being advanced through.
    Some(match key {
        "Left" | "h" => current.checked_sub(1),
        "Right" | "l" => (current < last).then(|| current + 1),
        "Up" | "k" => current.checked_sub(columns),
        // The last row is usually short. Dropping to its final page is what
        // the eye expects of a grid; refusing to move is not.
        "Down" | "j" if current + columns <= last => Some(current + columns),
        "Down" | "j" => (current / columns < last / columns).then_some(last),
        // A page step that would fall off the end lands on the first or the
        // last page rather than nowhere — the same reasoning as a short last
        // row, over a whole screenful.
        "PageUp" => (current > 0).then(|| current.saturating_sub(page)),
        "PageDown" => (current < last).then(|| (current + page).min(last)),
        // The ends of the grid, which the grid owns for the same reason it
        // owns the arrows: while the menu is open these are the presenter
        // looking over the deck, so they move the selection rather than
        // falling through to First and Last and moving the audience behind
        // it. Already at the end the press is absorbed.
        "Home" => (current > 0).then_some(0),
        "End" => (current < last).then_some(last),
        _ => return None,
    })
}

/// Where the selection belongs once the grid has stopped scrolling.
///
/// `None` means it is already on screen and should stay exactly where it is:
/// scrolling a little should not shuffle the selection about. Otherwise it
/// moves to the nearest edge of what is on screen, keeping its column, so the
/// selection arrives from the direction the scroll came from.
///
/// A row counts as on screen when at least half of it is, which is what the
/// eye means by seeing a thumbnail rather than a sliver of one.
fn settled_selection(
    selected: usize,
    count: usize,
    scroll: f32,
    grid: OverviewGrid,
) -> Option<usize> {
    if count == 0 || grid.row_height <= 0.0 || grid.viewport_height <= 0.0 {
        return None;
    }
    let columns = grid.columns.max(1);
    let last_row = (count - 1) / columns;
    let half = grid.row_height / 2.0;
    let first = ((scroll + half) / grid.row_height).floor().max(0.0) as usize;
    let last =
        (((scroll + grid.viewport_height - half) / grid.row_height).floor()).max(0.0) as usize;
    let (first, last) = (first.min(last_row), last.min(last_row));
    let row = selected / columns;
    if (first..=last).contains(&row) {
        return None;
    }
    let target_row = row.clamp(first, last);
    let slide = (target_row * columns + selected % columns).min(count - 1);
    (slide != selected).then_some(slide)
}

/// The shape of the overview grid as it was last drawn.
///
/// A default of one column is the honest answer before the grid has ever
/// been laid out: up and down then behave exactly like back and forward,
/// which is the arrangement a single column actually has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverviewGrid {
    pub columns: usize,
    /// One row plus the gap beneath it, in pixels.
    pub row_height: f32,
    /// How much of the grid is on screen, in pixels.
    pub viewport_height: f32,
}

impl Default for OverviewGrid {
    fn default() -> Self {
        Self {
            columns: 1,
            row_height: 1.0,
            viewport_height: 0.0,
        }
    }
}

/// The order pages are warmed in: nearest the presenter first.
///
/// Pages already held, and pages a shorter document no longer has, drop out.
/// Ordering is the whole of the strategy — a deck warms front to back from
/// wherever the presenter is standing, so the pictures they are most likely
/// to want are the ones that exist first, and a five-hundred-page deck is
/// useful long before it is finished.
/// The page in the middle of what the overview grid is showing.
///
/// `None` before the grid has been laid out — there is no honest answer then,
/// and the caller falls back to the presenter's own position.
pub(super) fn visible_centre(scroll: f32, grid: OverviewGrid, count: usize) -> Option<usize> {
    if count == 0 || grid.row_height <= 0.0 || grid.viewport_height <= 0.0 {
        return None;
    }
    let columns = grid.columns.max(1);
    let middle = scroll + grid.viewport_height / 2.0;
    let row = (middle / grid.row_height).floor().max(0.0) as usize;
    Some((row * columns + columns / 2).min(count - 1))
}

impl App {
    /// `Message::OverviewThumbDragged`: the presenter dragged the overview's
    /// own scroll thumb.
    pub(super) fn handle_overview_thumb_dragged(&mut self, offset: f32) -> Task<Message> {
        // A drag is the presenter's own choice, exactly as a keyboard
        // reveal is: it outranks a glide still in flight and claims
        // the offset until the scrollable reports it back.
        self.overview_settling = None;
        self.overview_scroll = offset;
        self.overview_scroll_claim = Some((offset, Instant::now() + OVERVIEW_SCROLL_CLAIM));
        iced::widget::operation::scroll_to(
            crate::view::overview_scrollable(),
            iced::widget::operation::AbsoluteOffset { x: 0.0, y: offset },
        )
    }
    /// `Message::OverviewScrolled`: the overview scrollable reported its
    /// offset, from a wheel, a drag it settled from, or the scroll-to this
    /// module itself issued.
    pub(super) fn handle_overview_scrolled(&mut self, offset: f32) -> Task<Message> {
        let now = Instant::now();
        // A scroll the keyboard asked for wins over one the hand did
        // not: while the claim stands, only the offset it asked for
        // is believed, and the claim ends the moment that offset
        // arrives.
        if let Some((target, deadline)) = self.overview_scroll_claim {
            if now < deadline {
                if (offset - target).abs() <= 0.5 {
                    self.overview_scroll_claim = None;
                    self.overview_scroll = offset;
                }
                return Task::none();
            }
            self.overview_scroll_claim = None;
        }
        self.overview_scroll = offset;
        self.overview_settling = Some(now);
        // Scrolling moves what warming should be working outwards
        // from. Re-planning here rather than on the next tick is what
        // makes a fast scroll into an unwarmed part of a long deck
        // fill under the eye instead of behind it; the plan is
        // memoised on its inputs, so a scroll that stays within one
        // row of the grid costs a comparison.
        self.plan_thumbnails();
        self.pump_thumbnails();
        Task::none()
    }
    /// `Message::ToggleOverview`: open or close the grid.
    pub(super) fn handle_toggle_overview(&mut self) -> Task<Message> {
        self.overview = !self.overview;
        if self.overview {
            self.thumbnails_demanded = true;
        }
        // It can be reached from the menu, which must not stay open
        // over the grid it just opened.
        self.menu_open = false;
        // Opening it is what asks for the thumbnails: rendering the
        // whole deck on the off-chance would be work nobody asked
        // for, on every document.
        self.request_renders();
        // Opening or closing the grid changes both the priority the
        // remaining thumbnails go out at and where warming works
        // outwards from, so neither waits for the next tick.
        self.plan_thumbnails();
        self.pump_thumbnails();
        if self.overview {
            // Open on what the active layout is showing. Ask the
            // layout what it contains rather than choosing a mode.
            let shows_document = self
                .active_layout
                .widgets()
                .iter()
                .any(|widget| widget.kind() == crate::widgets::WidgetKind::DocumentPage);
            let slide = if shows_document {
                let slide = self.slide_showing(self.reader.controls().page.get());
                // The grid's cursor and accent read the preview slide,
                // so it is seeded here rather than left where the last
                // presentation left it.
                let _ = self.state.apply(Nav::PreviewGoTo(slide), self.now);
                slide
            } else {
                self.state.preview()
            };
            // The overlay is mounted by the view pass after this
            // update. Even when measurements survive an earlier
            // opening, scrolling now targets an absent widget and is
            // discarded. Defer every initial reveal until the tick
            // after the overview is in the tree.
            self.overview_reveal = Some(slide);
            return Task::none();
        }
        // A grid that is closed has nowhere to scroll to.
        self.overview_reveal = None;
        Task::none()
    }
    /// `Message::GoToFromOverview`: the presenter picked a slide from the grid.
    pub(super) fn handle_goto_from_overview(&mut self, slide: usize) -> Task<Message> {
        self.overview = false;
        // The overview is a jump, never a continuation of a slider
        // drag. In particular the reader path below does not pass
        // through `Message::Nav`, which normally clears this flag.
        self.scrubbing = false;
        // In a document layout the grid is a way of moving the reader,
        // not of showing a slide to a room: the session index would
        // change with nothing on screen following it.
        if crate::layout::PrimaryViewer::of(&self.active_layout)
            == crate::layout::PrimaryViewer::Document
        {
            let page = self.page_of_slide(slide);
            return self.on_read_command(crate::widgets::event::ReadCommand::GoToPage(page));
        }
        self.dispatch(Message::Nav(Nav::GoTo(slide)))
    }

    /// Move about the overview grid with the arrow keys.
    ///
    /// `None` means the key was not one the grid owns, and should go on to
    /// the keymap as usual. Vertical movement is a whole row — the grid's own
    /// columns, not a guess — and a step that would fall off the end of a
    /// short last row lands on the last page rather than nowhere. Page up and
    /// page down move by a screenful of those rows, on the same reasoning.
    pub(super) fn overview_key(&mut self, key: Option<&str>) -> Option<Task<Message>> {
        let count = self.state.slide_count();
        if count == 0 {
            return None;
        }
        let grid = self.overview_grid.get();
        let columns = grid.columns.max(1);
        // The selection is the preview, so moving about the grid is looking,
        // not presenting: the audience stays on the slide it is on until the
        // presenter says Return.
        let current = self.state.preview().min(count - 1);
        // Return picks the slide the grid has landed on, which is the whole
        // point of the menu, and closes it — the same thing a click on that
        // thumbnail does.
        if matches!(key?, "Enter" | "Return") {
            return Some(self.dispatch(Message::GoToFromOverview(current)));
        }
        // How many whole rows are on screen at once, which is what a page
        // key moves by. Zero before the grid has ever been laid out; the
        // step then falls back to a single row.
        let page_rows = if grid.row_height > 0.0 {
            (grid.viewport_height / grid.row_height).floor().max(1.0) as usize
        } else {
            1
        };
        // An arrow at the edge of the grid is still an arrow the grid owns:
        // it stays put rather than falling through to the binding that would
        // move the audience behind the open menu.
        let Some(target) = grid_target(key?, current, count, columns, page_rows)? else {
            return Some(Task::none());
        };
        Some(Task::batch([
            self.dispatch(Message::Nav(Nav::PreviewGoTo(target))),
            self.reveal_in_overview(target),
        ]))
    }

    /// Bring the selection back onto the screen the scroll has arrived at.
    ///
    /// Only the preview moves, so the audience stays where it is: this is
    /// looking around the deck, not presenting it. No scroll is issued —
    /// the selection comes to the screen, never the screen to the selection.
    pub(super) fn settle_overview_selection(&mut self) -> Option<Task<Message>> {
        if !self.overview {
            return None;
        }
        let target = settled_selection(
            self.state.preview(),
            self.state.slide_count(),
            self.overview_scroll,
            self.overview_grid.get(),
        )?;
        Some(self.dispatch(Message::Nav(Nav::PreviewGoTo(target))))
    }

    /// Scroll the overview just far enough that `slide` is on screen.
    ///
    /// Only when it is not: a selection already in view should stay where it
    /// is on screen rather than being dragged to an edge under the presenter.
    pub(super) fn reveal_in_overview(&mut self, slide: usize) -> Task<Message> {
        // The presenter has just said where the selection goes, so a glide
        // that has not settled yet has nothing left to say about it.
        self.overview_settling = None;
        let grid = self.overview_grid.get();
        if grid.row_height <= 0.0 || grid.viewport_height <= 0.0 {
            // The grid has not been laid out yet, which is the ordinary case
            // the first time it is opened. Remembered rather than dropped.
            self.overview_reveal = Some(slide);
            return Task::none();
        }
        self.overview_reveal = None;
        let row = (slide / grid.columns.max(1)) as f32;
        let rows = self.state.slide_count().div_ceil(grid.columns.max(1));
        let furthest = (rows as f32 * grid.row_height - grid.viewport_height).max(0.0);
        let offset = (row * grid.row_height + grid.row_height / 2.0 - grid.viewport_height / 2.0)
            .clamp(0.0, furthest);
        self.overview_scroll = offset;
        // This is the presenter's own choice, so it outranks any glide still
        // in flight, and there is nothing left to settle: the selection is
        // already where it asked to be.
        self.overview_scroll_claim = Some((offset, Instant::now() + OVERVIEW_SCROLL_CLAIM));
        self.overview_settling = None;
        iced::widget::operation::scroll_to(
            crate::view::overview_scrollable(),
            iced::widget::operation::AbsoluteOffset { x: 0.0, y: offset },
        )
    }

    /// Whether Overview is waiting for its mounted grid before revealing its
    /// selected page. Views use this to avoid flashing an incorrect first row.
    pub(crate) fn overview_is_positioning(&self) -> bool {
        self.overview_reveal.is_some()
    }
}

#[cfg(test)]
mod grid_navigation_tests {
    use super::{grid_target, settled_selection, OverviewGrid};

    /// Two rows of five on screen, each row a hundred pixels tall.
    fn grid() -> OverviewGrid {
        OverviewGrid {
            columns: COLUMNS,
            row_height: 100.0,
            viewport_height: 200.0,
        }
    }

    /// How many whole rows a screenful of the grid holds in these tests.
    const PAGE_ROWS: usize = 2;

    // A five-column grid over eleven pages: two full rows and a row of one.
    const COLUMNS: usize = 5;
    const COUNT: usize = 11;

    #[test]
    fn the_arrows_move_in_all_four_directions() {
        assert_eq!(
            grid_target("Right", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(7))
        );
        assert_eq!(
            grid_target("Left", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(5))
        );
        assert_eq!(
            grid_target("Down", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(11 - 1))
        );
        assert_eq!(
            grid_target("Up", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(1))
        );
    }

    #[test]
    fn down_from_a_full_row_moves_a_whole_row() {
        assert_eq!(
            grid_target("Down", 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(6))
        );
    }

    #[test]
    fn down_into_a_short_last_row_lands_on_its_last_page() {
        // Column 3 of the middle row has no page beneath it; the eye still
        // expects to arrive somewhere on the row below.
        assert_eq!(
            grid_target("Down", 8, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(10))
        );
    }

    #[test]
    fn the_edges_absorb_the_press() {
        assert_eq!(grid_target("Up", 2, COUNT, COLUMNS, PAGE_ROWS), Some(None));
        assert_eq!(
            grid_target("Left", 0, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
        assert_eq!(
            grid_target("Right", COUNT - 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
        assert_eq!(
            grid_target("Down", COUNT - 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
    }

    #[test]
    fn the_grid_answers_to_the_vim_keys_as_well_as_the_arrows() {
        // In the vim sense: `j` and `k` between rows, `h` and `l` along one.
        // The overview is a grid being looked over, not a deck being advanced
        // through, so `j` here means what it means in vim rather than what it
        // means on the slide.
        for (vim, arrow) in [("h", "Left"), ("l", "Right"), ("k", "Up"), ("j", "Down")] {
            assert_eq!(
                grid_target(vim, 7, COUNT, COLUMNS, PAGE_ROWS),
                grid_target(arrow, 7, COUNT, COLUMNS, PAGE_ROWS),
                "{vim} should move like {arrow}"
            );
        }
    }

    #[test]
    fn a_key_the_grid_does_not_own_falls_through() {
        assert_eq!(grid_target("b", 3, COUNT, COLUMNS, PAGE_ROWS), None);
        assert_eq!(grid_target("Escape", 3, COUNT, COLUMNS, PAGE_ROWS), None);
    }

    #[test]
    fn the_end_keys_move_the_selection_to_the_ends_of_the_grid() {
        assert_eq!(
            grid_target("Home", 7, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(0))
        );
        assert_eq!(
            grid_target("End", 7, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(COUNT - 1))
        );
    }

    #[test]
    fn an_end_key_at_the_end_it_names_stays_put() {
        // Absorbed rather than passed on: `Some(None)` is what keeps the
        // audience from moving behind the open menu.
        assert_eq!(
            grid_target("Home", 0, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
        assert_eq!(
            grid_target("End", COUNT - 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
    }

    #[test]
    fn a_page_key_moves_a_screenful_of_rows() {
        // Two rows of five on screen, so a page is ten pages away.
        assert_eq!(
            grid_target("PageDown", 0, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(10))
        );
        assert_eq!(
            grid_target("PageUp", 10, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(0))
        );
    }

    #[test]
    fn a_page_key_past_the_end_lands_on_the_end() {
        assert_eq!(
            grid_target("PageDown", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(COUNT - 1))
        );
        assert_eq!(
            grid_target("PageUp", 6, COUNT, COLUMNS, PAGE_ROWS),
            Some(Some(0))
        );
        assert_eq!(
            grid_target("PageDown", COUNT - 1, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
        assert_eq!(
            grid_target("PageUp", 0, COUNT, COLUMNS, PAGE_ROWS),
            Some(None)
        );
    }

    #[test]
    fn a_selection_still_on_screen_is_left_alone() {
        // Rows 0 and 1 are on screen and the selection is in row 1.
        assert_eq!(settled_selection(6, COUNT, 0.0, grid()), None);
    }

    #[test]
    fn a_selection_scrolled_off_the_top_follows_the_screen_down() {
        // Scrolled to row 1: the selection in row 0 comes down a row and
        // keeps its column.
        assert_eq!(settled_selection(2, COUNT, 100.0, grid()), Some(7));
    }

    #[test]
    fn a_selection_scrolled_off_the_bottom_follows_the_screen_up() {
        // Back at the top, with the selection down in the short last row.
        assert_eq!(settled_selection(10, COUNT, 0.0, grid()), Some(5));
    }

    #[test]
    fn the_short_last_row_never_selects_past_the_deck() {
        // Row 2 holds a single page; a column-3 selection arriving there
        // lands on the last page rather than past it.
        assert_eq!(settled_selection(3, COUNT, 200.0, grid()), Some(COUNT - 1));
    }

    #[test]
    fn a_grid_that_has_never_been_laid_out_moves_nothing() {
        assert_eq!(
            settled_selection(3, COUNT, 0.0, OverviewGrid::default()),
            None
        );
    }

    #[test]
    fn one_column_behaves_like_a_list() {
        assert_eq!(grid_target("Down", 3, COUNT, 1, PAGE_ROWS), Some(Some(4)));
        assert_eq!(grid_target("Up", 3, COUNT, 1, PAGE_ROWS), Some(Some(2)));
    }
}
