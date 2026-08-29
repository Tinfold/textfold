//! A pane: which file it shows, where its cursors are, and what part of the
//! file is on screen.
//!
//! Cursors live here rather than in the document because the same file can be
//! open in two panes, and a cursor is a fact about looking at a file rather
//! than about the file. An edit made in one pane is told to every pane showing
//! that document, which is how the other one's cursor stays where it was
//! pointing instead of where it used to be.
//!
//! This is also where screen coordinates and text positions meet, in both
//! directions: drawing needs to know what to put on row twelve, and a mouse
//! click needs to know what row twelve was. Both walk the same wrapping, so
//! they cannot disagree about where a folded line broke.

use std::collections::HashMap;

use ratatui::layout::Rect;
use ropey::Rope;

use crate::doc::{AppliedEdit, DocId, Document};
use crate::text::{self, Range, Selections};

/// Where a pane was in a file it is no longer showing.
///
/// Coming back to a tab and finding it at line one is the sort of thing that
/// makes a person stop using tabs. A pane remembers a spot per file, so
/// switching away and back is switching away and back rather than reopening.
#[derive(Clone, Default)]
pub struct Spot {
    pub sel: Selections,
    pub top: usize,
    pub top_row: usize,
    pub left: usize,
}

/// One place you jumped from, so you can get back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Jump {
    pub doc: DocId,
    pub at: usize,
}

/// A pane.
/// Which edge of the editor a docked pane is pinned to.
///
/// Not a direction so much as a shape: the two sides are a column of a fixed
/// width and the bottom is a row of a fixed height, because that is what the
/// things people dock are — a tree of files down one side, a list of problems
/// along the bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Bottom,
}

impl Edge {
    /// What a manifest or a plugin calls it.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "left" => Some(Edge::Left),
            "right" => Some(Edge::Right),
            "bottom" => Some(Edge::Bottom),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Bottom => "bottom",
        }
    }

    /// Whether it takes a width out of the body rather than a height.
    pub fn is_side(&self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// A pane pinned to an edge at a size of its own, rather than taking an equal
/// share of the middle.
///
/// This is what lets a plugin add to the editor's shape rather than only to
/// its contents. A dock is an ordinary pane in every other respect — it has a
/// buffer, a cursor, the focus rule, and the keys — which is deliberate: a
/// sidebar that was a special kind of surface would need its own answer to
/// every question a pane has already answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dock {
    pub edge: Edge,
    /// Columns for a side, rows for the bottom. Clamped when it is laid out,
    /// so a plugin asking for eighty columns on a forty-column terminal gets
    /// something usable rather than an editor with no room left in it.
    pub size: u16,
}

/// What a dock is when nobody says: wide enough for a tree of file names,
/// narrow enough to leave the code readable.
pub const DEFAULT_DOCK_WIDTH: u16 = 30;
/// And the same along the bottom, where the useful unit is rows.
pub const DEFAULT_DOCK_HEIGHT: u16 = 10;

impl Dock {
    pub fn new(edge: Edge, size: Option<u16>) -> Self {
        let size = size.unwrap_or(match edge.is_side() {
            true => DEFAULT_DOCK_WIDTH,
            false => DEFAULT_DOCK_HEIGHT,
        });
        Self { edge, size }
    }
}

pub struct View {
    pub doc: DocId,
    /// Where this pane is pinned, if it is pinned at all. `None` is an
    /// ordinary pane, which is what all of them used to be.
    pub dock: Option<Dock>,
    pub sel: Selections,
    /// The first line on screen, and how far into it when a folded line is cut
    /// across the top of the pane.
    pub top: usize,
    pub top_row: usize,
    /// The first column on screen, for a pane that is not folding lines.
    pub left: usize,
    /// The column vertical movement is aiming for. Moving down a short line
    /// and then down again should get back to the column you started in, which
    /// only works if something remembers it.
    pub goal: Option<usize>,
    /// Where this pane was last drawn: the text part, without the line numbers
    /// or the scroll bar. What a mouse click is measured against.
    pub area: Rect,
    /// The whole pane, including the line numbers and the scroll bar. What
    /// decides which pane a click was in.
    pub frame: Rect,
    /// How wide the line numbers were, so a click on them is not read as a
    /// click on the text.
    pub gutter: u16,
    /// Whether this pane folds long lines. Taken from the settings when the
    /// pane is made, and changed per pane after that.
    pub wrap: bool,

    /// Where you have been, and how far back through it you are. Jumping
    /// somewhere new from the middle throws away the part you had gone back
    /// past, which is what every browser does and what everyone expects.
    pub jumps: Vec<Jump>,
    pub jump_at: usize,

    /// Where this pane was in each file it has shown. Kept per pane rather
    /// than per document because two panes on the same file are two places in
    /// it, and each one should come back to its own.
    spots: HashMap<DocId, Spot>,
}

impl View {
    pub fn new(doc: DocId, wrap: bool) -> Self {
        Self {
            doc,
            dock: None,
            sel: Selections::default(),
            top: 0,
            top_row: 0,
            left: 0,
            goal: None,
            area: Rect::new(0, 0, 80, 24),
            frame: Rect::new(0, 0, 80, 24),
            gutter: 0,
            wrap,
            jumps: Vec::new(),
            jump_at: 0,
            spots: HashMap::new(),
        }
    }

    /// Where the pane is right now, in the file it is showing.
    pub fn spot(&self) -> Spot {
        Spot {
            sel: self.sel.clone(),
            top: self.top,
            top_row: self.top_row,
            left: self.left,
        }
    }

    /// Put away where the pane is, so that coming back to this file comes back
    /// here. Called before the pane is pointed at something else.
    pub fn remember(&mut self) {
        let spot = self.spot();
        self.spots.insert(self.doc, spot);
    }

    /// Where this pane was in a file, as a character index — now, if it is the
    /// one showing, and otherwise wherever it last was. `None` for a file this
    /// pane has never shown.
    pub fn place_in(&self, doc: DocId) -> Option<usize> {
        if self.doc == doc {
            return Some(self.cursor());
        }
        self.spots.get(&doc).map(|spot| spot.sel.primary().head)
    }

    /// Forget a file, because it has been closed. Otherwise a document id that
    /// came round again would be met with somebody else's scroll position.
    pub fn forget(&mut self, doc: DocId) {
        self.spots.remove(&doc);
    }

    /// Where the cursor is, as a character index.
    pub fn cursor(&self) -> usize {
        self.sel.primary().head
    }

    /// How many lines of text the pane shows.
    pub fn height(&self) -> usize {
        self.area.height.max(1) as usize
    }

    pub fn width(&self) -> usize {
        self.area.width.max(1) as usize
    }

    /// Show a different file, keeping the pane's own settings, and landing
    /// where this pane last was in that file.
    pub fn show(&mut self, doc: DocId, sel: Selections) {
        if self.doc != doc {
            self.remember();
        }
        self.doc = doc;
        self.sel = sel;
        self.top = 0;
        self.top_row = 0;
        self.left = 0;
        self.goal = None;
    }

    /// Show a file and put the pane back exactly where it was in it.
    ///
    /// A file this pane has not shown before starts at the top with whatever
    /// selection it was handed, which is what `show` alone does.
    pub fn revisit(&mut self, doc: DocId, sel: Selections, len: usize) {
        let spot = self.spots.get(&doc).cloned();
        self.show(doc, sel);
        if let Some(spot) = spot {
            self.sel = spot.sel;
            self.sel.clamp(len);
            self.top = spot.top;
            self.top_row = spot.top_row;
            self.left = spot.left;
        }
    }

    /// Take in edits made to the document this pane shows.
    ///
    /// Every edit in the order they were made, so that a set of changes from
    /// several cursors lands the same way here as it did in the rope.
    pub fn absorb(&mut self, edits: &[AppliedEdit], len: usize) {
        if edits.is_empty() {
            return;
        }
        self.sel.map(|range| {
            let mut anchor = range.anchor;
            let mut head = range.head;
            for edit in edits {
                anchor = edit.map(anchor);
                head = edit.map(head);
            }
            Range::new(anchor, head)
        });
        self.sel.clamp(len);
    }

    /// Remember where you are, before going somewhere else.
    pub fn mark_jump(&mut self) {
        let here = Jump {
            doc: self.doc,
            at: self.cursor(),
        };
        // Coming back to somewhere you already are is not a jump.
        if self.jumps.get(self.jump_at.wrapping_sub(1)) == Some(&here) {
            return;
        }
        self.jumps.truncate(self.jump_at);
        self.jumps.push(here);
        // A history of everywhere you have ever been is not a history, it is a
        // leak with a search function.
        if self.jumps.len() > 128 {
            self.jumps.remove(0);
        }
        self.jump_at = self.jumps.len();
    }

    /// The place before this one, if there is one.
    pub fn jump_back(&mut self) -> Option<Jump> {
        if self.jump_at == 0 {
            return None;
        }
        // Stepping back for the first time has to record where you are, or
        // there would be nothing to step forward to.
        if self.jump_at == self.jumps.len() {
            let here = Jump {
                doc: self.doc,
                at: self.cursor(),
            };
            if self.jumps.last() != Some(&here) {
                self.jumps.push(here);
            }
        }
        self.jump_at -= 1;
        self.jumps.get(self.jump_at).copied()
    }

    pub fn jump_forward(&mut self) -> Option<Jump> {
        if self.jump_at + 1 >= self.jumps.len() {
            return None;
        }
        self.jump_at += 1;
        self.jumps.get(self.jump_at).copied()
    }
}

/// Everything about how a document is laid out on a pane. Passed around rather
/// than stored, because it is a fact about the pane's current size and the
/// current settings, and both change.
#[derive(Clone, Copy)]
pub struct Layout<'a> {
    pub rope: &'a Rope,
    pub width: usize,
    pub tab_width: usize,
    pub wrap: bool,
}

impl<'a> Layout<'a> {
    /// Where each folded row of a line begins, as character indices. Always at
    /// least one, since even an empty line is one row on screen.
    ///
    /// Breaks at the last space that fits, so that words stay whole; a word
    /// too long to fit anywhere is broken at the edge, because the alternative
    /// is a row with nothing on it and a word still not fitting.
    pub fn rows_of(&self, line: usize) -> Vec<usize> {
        let start = text::line_start(self.rope, line);
        if !self.wrap {
            return vec![start];
        }
        let end = text::line_end(self.rope, line);
        let mut rows = vec![start];
        let mut at = start;
        let mut col = 0;
        let mut last_break: Option<usize> = None;

        while at < end {
            let c = self.rope.char(at);
            let w = text::char_width(c, col, self.tab_width);
            if col + w > self.width && at > *rows.last().expect("seeded") {
                let cut = match last_break {
                    // Break after the space, so the space stays on the line it
                    // ended rather than starting the next one.
                    Some(space) if space > *rows.last().expect("seeded") => space + 1,
                    _ => at,
                };
                rows.push(cut);
                at = cut;
                col = 0;
                last_break = None;
                continue;
            }
            if c == ' ' || c == '\t' {
                last_break = Some(at);
            }
            col += w;
            at += 1;
        }
        rows
    }

    /// How many rows a line takes on screen.
    pub fn rows_in(&self, line: usize) -> usize {
        if !self.wrap {
            return 1;
        }
        self.rows_of(line).len()
    }

    /// Which folded row of its line a position sits on, and how far across.
    pub fn place(&self, at: usize) -> (usize, usize) {
        let line = text::line_of(self.rope, at);
        if !self.wrap {
            return (0, text::visual_column(self.rope, at, self.tab_width));
        }
        let rows = self.rows_of(line);
        let row = rows.iter().rposition(|&start| start <= at).unwrap_or(0);
        let mut col = 0;
        for c in self.rope.slice(rows[row]..at).chars() {
            col += text::char_width(c, col, self.tab_width);
        }
        (row, col)
    }

    /// The character at a folded row and column of a line — the way back from
    /// a mouse click.
    pub fn position(&self, line: usize, row: usize, col: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        if !self.wrap {
            return text::char_at_column(self.rope, line, col, self.tab_width);
        }
        let rows = self.rows_of(line);
        let row = row.min(rows.len() - 1);
        let start = rows[row];
        let end = rows
            .get(row + 1)
            .copied()
            .unwrap_or_else(|| text::line_end(self.rope, line));
        let mut at = start;
        let mut width = 0;
        while at < end {
            let step = text::char_width(self.rope.char(at), width, self.tab_width);
            if width + step > col {
                return if col >= width + step.div_ceil(2) {
                    (at + 1).min(end)
                } else {
                    at
                };
            }
            width += step;
            at += 1;
        }
        end
    }
}

/// Where a position is on screen, relative to the top of the pane. `None` for
/// a position scrolled out of sight.
pub fn screen_row(view: &View, layout: &Layout, at: usize) -> Option<usize> {
    let line = text::line_of(layout.rope, at);
    if line < view.top {
        return None;
    }
    let (row, _) = layout.place(at);
    let mut screen = 0usize;
    for l in view.top..line {
        screen += layout.rows_in(l);
        if l == view.top {
            screen = screen.saturating_sub(view.top_row);
        }
        if screen > view.height() {
            return None;
        }
    }
    let screen = if line == view.top {
        row.checked_sub(view.top_row)?
    } else {
        screen + row
    };
    (screen < view.height()).then_some(screen)
}

/// Move the view so the primary cursor is on it, with `pad` rows to spare.
///
/// Called after everything that moves a cursor, rather than by everything that
/// moves a cursor, so there is one rule about where the view goes and no
/// command can forget it.
///
/// The work here is bounded by the height of the pane, never by the size of
/// the file. Jumping from the first line of a ten-thousand-line file to the
/// last is the same amount of work as pressing Down.
pub fn scroll_to_cursor(view: &mut View, doc: &Document, tab_width: usize, pad: usize) {
    let layout = Layout {
        rope: &doc.rope,
        width: view.width(),
        tab_width,
        wrap: view.wrap,
    };
    let at = view.cursor();
    let line = text::line_of(&doc.rope, at);
    let (row, col) = layout.place(at);
    let height = view.height();
    // Padding cannot take up more than half the pane, or the two rules below
    // would contradict each other.
    let pad = pad.min(height.saturating_sub(1) / 2);

    // The top can be no further down than `pad` rows above the cursor, and no
    // further up than a screenful less the padding. Between those two it is
    // left exactly where it was, which is what stops the view drifting on
    // every keystroke.
    let highest = rows_above(&layout, line, row, pad);
    let lowest = rows_above(&layout, line, row, height.saturating_sub(1 + pad));
    let top = (view.top, view.top_row);
    if top > highest {
        (view.top, view.top_row) = highest;
    } else if top < lowest {
        (view.top, view.top_row) = lowest;
    }

    // Sideways, for a pane that is not folding lines.
    if view.wrap {
        view.left = 0;
    } else {
        let width = view.width();
        if col < view.left {
            view.left = col;
        } else if col >= view.left + width {
            view.left = col + 1 - width;
        }
    }
}

/// The row `rows` rows above the one at `(line, row)`, stopping at the top of
/// the file.
fn rows_above(layout: &Layout, mut line: usize, mut row: usize, mut rows: usize) -> (usize, usize) {
    while rows > 0 {
        if row > 0 {
            row -= 1;
        } else if line > 0 {
            line -= 1;
            row = layout.rows_in(line) - 1;
        } else {
            return (0, 0);
        }
        rows -= 1;
    }
    (line, row)
}

/// One row further down. `false` when the view is already showing the last
/// row of the file, so a caller looping on this has something to stop for.
fn step_down(view: &mut View, layout: &Layout, doc: &Document) -> bool {
    let rows = layout.rows_in(view.top);
    if view.top_row + 1 < rows {
        view.top_row += 1;
        true
    } else if view.top + 1 < doc.len_lines() {
        view.top += 1;
        view.top_row = 0;
        true
    } else {
        false
    }
}

/// Move the view by `rows`, without touching the cursors. The mouse wheel, and
/// the commands that scroll.
pub fn scroll_by(view: &mut View, doc: &Document, tab_width: usize, rows: isize) {
    let layout = Layout {
        rope: &doc.rope,
        width: view.width(),
        tab_width,
        wrap: view.wrap,
    };
    let mut left = rows.unsigned_abs();
    if rows > 0 {
        while left > 0 {
            // Stop with the last line still showing rather than scrolling the
            // file off the top of the pane entirely.
            if !step_down(view, &layout, doc) {
                break;
            }
            left -= 1;
        }
    } else {
        while left > 0 {
            if view.top_row > 0 {
                view.top_row -= 1;
            } else if view.top > 0 {
                view.top -= 1;
                view.top_row = layout.rows_in(view.top) - 1;
            } else {
                break;
            }
            left -= 1;
        }
    }
}

/// The character a click at a screen row and column means.
///
/// `row` and `col` are already relative to the text area, so the gutter and
/// the border have been taken off by the caller.
pub fn position_at_screen(
    view: &View,
    doc: &Document,
    tab_width: usize,
    row: usize,
    col: usize,
) -> usize {
    let layout = Layout {
        rope: &doc.rope,
        width: view.width(),
        tab_width,
        wrap: view.wrap,
    };
    let mut line = view.top;
    let mut sub = view.top_row;
    let mut left = row;
    while left > 0 {
        let rows = layout.rows_in(line);
        if sub + 1 < rows {
            sub += 1;
        } else if line + 1 < doc.len_lines() {
            line += 1;
            sub = 0;
        } else {
            // Clicking below the end of the file means the end of the file.
            break;
        }
        left -= 1;
    }
    let col = if view.wrap { col } else { col + view.left };
    layout.position(line, sub, col)
}

/// The visible stretch of the file, as lines. What the drawing walks, and what
/// the highlighter is asked about.
pub fn visible_lines(view: &View, doc: &Document, tab_width: usize) -> (usize, usize) {
    let layout = Layout {
        rope: &doc.rope,
        width: view.width(),
        tab_width,
        wrap: view.wrap,
    };
    let mut line = view.top;
    let mut rows = layout.rows_in(line).saturating_sub(view.top_row);
    while rows < view.height() && line + 1 < doc.len_lines() {
        line += 1;
        rows += layout.rows_in(line);
    }
    (view.top, (line + 1).min(doc.len_lines()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocId, Indent};

    fn doc_of(text: &str) -> Document {
        let mut d = Document::scratch(DocId(0), "t".into(), Indent::Spaces(4));
        d.rope = Rope::from_str(text);
        d
    }

    fn view_of(doc: &Document, w: u16, h: u16, wrap: bool) -> View {
        let mut v = View::new(doc.id, wrap);
        v.area = Rect::new(0, 0, w, h);
        v
    }

    #[test]
    fn a_line_that_fits_is_one_row() {
        let doc = doc_of("short\n");
        let layout = Layout {
            rope: &doc.rope,
            width: 40,
            tab_width: 4,
            wrap: true,
        };
        assert_eq!(layout.rows_of(0), vec![0]);
    }

    #[test]
    fn folding_breaks_between_words() {
        let doc = doc_of("the quick brown fox jumps\n");
        let layout = Layout {
            rope: &doc.rope,
            width: 10,
            tab_width: 4,
            wrap: true,
        };
        let rows = layout.rows_of(0);
        let text = doc.rope.to_string();
        let pieces: Vec<&str> = rows
            .iter()
            .enumerate()
            .map(|(i, &start)| {
                let end = rows.get(i + 1).copied().unwrap_or(text.len() - 1);
                &text[start..end]
            })
            .collect();
        assert_eq!(pieces, ["the quick ", "brown fox ", "jumps"]);
    }

    #[test]
    fn a_word_too_long_to_fit_is_broken_rather_than_lost() {
        let doc = doc_of("supercalifragilistic\n");
        let layout = Layout {
            rope: &doc.rope,
            width: 8,
            tab_width: 4,
            wrap: true,
        };
        let rows = layout.rows_of(0);
        assert!(rows.len() >= 3, "{rows:?}");
        assert_eq!(rows[0], 0);
        assert_eq!(rows[1], 8);
    }

    #[test]
    fn a_click_lands_on_what_was_drawn_there() {
        let doc = doc_of("one\ntwo\nthree\nfour\nfive\n");
        let mut view = view_of(&doc, 20, 5, false);
        view.top = 1;
        // Second row of the pane, third column: line 2 (`three`), char 2.
        let at = position_at_screen(&view, &doc, 4, 1, 2);
        assert_eq!(text::line_of(&doc.rope, at), 2);
        assert_eq!(at, doc.rope.line_to_char(2) + 2);
    }

    #[test]
    fn clicking_below_the_last_line_lands_in_the_file_rather_than_past_it() {
        let doc = doc_of("one\ntwo\n");
        let view = view_of(&doc, 20, 20, false);
        let at = position_at_screen(&view, &doc, 4, 18, 0);
        assert!(at <= doc.len_chars());
    }

    #[test]
    fn the_view_follows_the_cursor_and_keeps_a_margin() {
        let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let doc = doc_of(&text);
        let mut view = view_of(&doc, 40, 10, false);
        view.sel = Selections::single(Range::point(doc.rope.line_to_char(150)));
        scroll_to_cursor(&mut view, &doc, 4, 3);
        let layout = Layout {
            rope: &doc.rope,
            width: 40,
            tab_width: 4,
            wrap: false,
        };
        let row = screen_row(&view, &layout, view.cursor()).expect("on screen");
        assert!((3..10 - 3).contains(&row), "row {row}");

        // And back up again.
        view.sel = Selections::single(Range::point(doc.rope.line_to_char(2)));
        scroll_to_cursor(&mut view, &doc, 4, 3);
        assert_eq!(view.top, 0);
    }

    #[test]
    fn a_jump_across_a_large_file_costs_no_more_than_a_jump_across_a_small_one() {
        // The view used to walk down a row at a time to catch up with the
        // cursor, which made Ctrl-End on a long file take visible seconds.
        let text: String = (0..40_000).map(|i| format!("line {i}\n")).collect();
        let doc = doc_of(&text);
        let mut view = view_of(&doc, 60, 20, true);
        view.sel = Selections::single(Range::point(doc.len_chars()));
        let started = std::time::Instant::now();
        scroll_to_cursor(&mut view, &doc, 4, 3);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "took {:?}",
            started.elapsed()
        );
        assert!(view.top > 39_000, "top {}", view.top);
    }

    #[test]
    fn the_view_stays_put_when_the_cursor_is_already_comfortably_on_it() {
        let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let doc = doc_of(&text);
        let mut view = view_of(&doc, 40, 20, false);
        view.top = 50;
        view.sel = Selections::single(Range::point(doc.rope.line_to_char(58)));
        scroll_to_cursor(&mut view, &doc, 4, 3);
        assert_eq!(view.top, 50);
        // And moving one line further does not move it either.
        view.sel = Selections::single(Range::point(doc.rope.line_to_char(59)));
        scroll_to_cursor(&mut view, &doc, 4, 3);
        assert_eq!(view.top, 50);
    }

    #[test]
    fn the_view_does_not_run_off_the_end_of_the_file() {
        let doc = doc_of("one\ntwo\nthree\n");
        let mut view = view_of(&doc, 20, 10, false);
        scroll_by(&mut view, &doc, 4, 100);
        assert!(view.top < doc.len_lines());
        scroll_by(&mut view, &doc, 4, -100);
        assert_eq!(view.top, 0);
    }

    #[test]
    fn a_long_line_scrolls_sideways_when_it_is_not_folded() {
        let doc = doc_of(&format!("{}\n", "x".repeat(300)));
        let mut view = view_of(&doc, 40, 10, false);
        view.sel = Selections::single(Range::point(250));
        scroll_to_cursor(&mut view, &doc, 4, 3);
        assert!(view.left > 0);
        assert!(view.left <= 250);
    }

    #[test]
    fn going_back_and_forward_lands_where_you_were() {
        let mut view = View::new(DocId(0), false);
        view.sel = Selections::single(Range::point(10));
        view.mark_jump();
        view.sel = Selections::single(Range::point(500));
        let back = view.jump_back().expect("somewhere to go");
        assert_eq!(back.at, 10);
        let forward = view.jump_forward().expect("somewhere to come back to");
        assert_eq!(forward.at, 500);
    }
}
