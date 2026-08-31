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
    /// How much of the middle this pane gets, against its neighbours.
    ///
    /// Relative rather than absolute: they are added up and each pane gets its
    /// share of whatever there is, so a terminal that is resized keeps the
    /// proportions you dragged rather than the columns. Equal until somebody
    /// pulls a divider, which is what every pane used to get always.
    pub share: f32,
    /// Whether this pane shows one buffer and only that one.
    ///
    /// For a pane that is half of a pair — the plugin's own settings beside
    /// yours — where opening a file into it would leave you comparing your
    /// settings against something that is not what they are settings for.
    pub pinned: bool,
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
    /// The divider along a docked pane's inner edge: one column down the side
    /// facing the middle, or one row along the top of a bottom dock.
    ///
    /// It is drawn, and it is what you drag to resize. A sidebar whose width
    /// could only be set in a settings file would be a sidebar of the wrong
    /// width, because the right width depends on the project and the terminal
    /// and neither of those is knowable in advance.
    pub grip: Option<Rect>,
    /// Whether this pane folds long lines. Taken from the settings when the
    /// pane is made, and changed per pane after that.
    pub wrap: bool,

    /// Stretches of the file this pane has folded away, as pairs of character
    /// positions: the first line of each stays on screen and everything after
    /// it, up to and including the line the second position is on, is not
    /// drawn at all.
    ///
    /// Positions rather than line numbers, so that a fold stays around the
    /// thing it was put around while the text above it is edited — the same
    /// bargain a breakpoint and a bookmark make.
    ///
    /// On the pane rather than on the document, because folding is a way of
    /// looking at a file and not a fact about one: the same file open twice
    /// is two views, and folding away the imports in one of them should not
    /// take them out of the other.
    pub folds: Vec<(usize, usize)>,

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
            share: 1.0,
            pinned: false,
            grip: None,
            sel: Selections::default(),
            top: 0,
            top_row: 0,
            left: 0,
            goal: None,
            area: Rect::new(0, 0, 80, 24),
            frame: Rect::new(0, 0, 80, 24),
            gutter: 0,
            wrap,
            folds: Vec::new(),
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
        // And the folds. A fold whose two ends have arrived on the same line —
        // because everything it was hiding has been deleted — is not a fold
        // any more, and goes rather than sitting there hiding nothing.
        for (from, to) in &mut self.folds {
            for edit in edits {
                *from = edit.map(*from);
                *to = edit.map(*to);
            }
            *from = (*from).min(len);
            *to = (*to).min(len);
        }
    }

    /// The stretches of lines this pane has hidden, as `(first, last)` pairs.
    ///
    /// The first line of each is still drawn — it is the line with the `{` on
    /// it, and the one you click to bring the rest back. Everything after it
    /// up to and including the last is gone.
    ///
    /// Worked out from the positions rather than stored, for the reason every
    /// other mark in this editor is: the positions are what the text carries
    /// along, and a line number written down is a line number that is wrong as
    /// soon as anybody types above it.
    pub fn folded(&self, rope: &Rope) -> Vec<(usize, usize)> {
        self.folds
            .iter()
            .filter_map(|(from, to)| {
                let len = rope.len_chars();
                let first = text::line_of(rope, (*from).min(len));
                let last = text::line_of(rope, (*to).min(len));
                (last > first).then_some((first, last))
            })
            .collect()
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
pub struct Layout<'a> {
    pub rope: &'a Rope,
    pub width: usize,
    pub tab_width: usize,
    pub wrap: bool,
    /// The notes drawn into the text that are not in it, as `(position,
    /// width)` pairs — [`crate::doc::Document::inlay_columns`].
    ///
    /// Held rather than borrowed, and filled in by [`Layout::of`], because a
    /// layout built without them is a layout that maps the screen to the text
    /// wrongly on every line that has one — and does it silently.
    ///
    /// They are here rather than only in the drawing because a note takes room
    /// on a line, and every question this module answers — which row is that
    /// position on, what is under the pointer, where does this line wrap — is
    /// about room. A note drawn but not counted is a line whose characters are
    /// no longer where the editor thinks they are, and a click that lands two
    /// words to the left of where it was aimed.
    pub hints: Vec<(usize, usize)>,
    /// The stretches of lines the pane has folded away, as `(first, last)`
    /// line pairs — [`View::folded`].
    ///
    /// A hidden line is one that takes **no rows on the screen**, and that one
    /// sentence is the whole of how folding works here. Every piece of
    /// arithmetic in this module already counts in rows rather than lines,
    /// because a wrapped line has always been several rows; a line worth
    /// nothing is a line that scrolling, cursor movement and drawing all step
    /// over without any of them being told about folding.
    pub folds: Vec<(usize, usize)>,
}

impl<'a> Layout<'a> {
    /// The layout a pane is looking at a document through.
    ///
    /// The one way to build one. Everything about how the screen maps to the
    /// text — how wide the pane is, whether it wraps, what is folded away,
    /// what notes are drawn into it — is gathered here rather than at the
    /// dozen places that ask a question of it, because every one of those
    /// places got it right only by remembering to.
    pub fn of(view: &View, doc: &'a Document, tab_width: usize) -> Self {
        Self {
            rope: &doc.rope,
            width: view.width(),
            tab_width,
            wrap: view.wrap,
            hints: doc.inlay_columns(),
            folds: view.folded(&doc.rope),
        }
    }

    /// The same, laid out on a pane of a different width — the drawing knows
    /// the exact area it has, which is not always what the pane says.
    pub fn across(self, width: usize) -> Self {
        Self { width, ..self }
    }

    /// Where each folded row of a line begins, as character indices. Always at
    /// least one, since even an empty line is one row on screen.
    ///
    /// Breaks at the last space that fits, so that words stay whole; a word
    /// too long to fit anywhere is broken at the edge, because the alternative
    /// is a row with nothing on it and a word still not fitting.
    pub fn rows_of(&self, line: usize) -> Vec<usize> {
        if self.hidden(line) {
            return Vec::new();
        }
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
            col += self
                .hints
                .iter()
                .filter(|(where_, _)| *where_ == at)
                .map(|(_, w)| w)
                .sum::<usize>();
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

    /// How many columns the notes between two positions take up, counting one
    /// that sits at either end.
    ///
    /// Both ends, because a note is drawn immediately before the character it
    /// is attached to: one at the start of a row is drawn at the start of that
    /// row, and one at the position being measured to has already been passed
    /// by the time you are standing there.
    fn hints_between(&self, from: usize, to: usize) -> usize {
        self.hints
            .iter()
            .filter(|(at, _)| *at >= from && *at <= to)
            .map(|(_, width)| width)
            .sum()
    }

    /// Whether this line has been folded away behind the one above it.
    pub fn hidden(&self, line: usize) -> bool {
        self.folds
            .iter()
            .any(|(first, last)| line > *first && line <= *last)
    }

    /// How many rows a line takes on screen. None at all, for one inside a
    /// fold.
    pub fn rows_in(&self, line: usize) -> usize {
        if self.hidden(line) {
            return 0;
        }
        if !self.wrap {
            return 1;
        }
        self.rows_of(line).len()
    }

    /// Which folded row of its line a position sits on, and how far across.
    pub fn place(&self, at: usize) -> (usize, usize) {
        // Clamped, because this is the drawing's question and a position past
        // the end of the rope is a panic rather than an answer. Whoever asked
        // may be holding a position from a longer version of the text — a
        // panel refilled while the pointer was in it, an edit made to a
        // document rather than through a pane.
        let at = at.min(self.rope.len_chars());
        let line = text::line_of(self.rope, at);
        let plain = |from: usize| {
            let mut col = 0;
            for c in self.rope.slice(from..at).chars() {
                col += text::char_width(c, col, self.tab_width);
            }
            col + self.hints_between(from, at)
        };
        if !self.wrap {
            return (0, plain(text::line_start(self.rope, line)));
        }
        let rows = self.rows_of(line);
        if rows.is_empty() {
            // A position inside a fold. It is nowhere on the screen, and the
            // column is still the honest answer to half the question.
            return (0, plain(text::line_start(self.rope, line)));
        }
        let row = rows.iter().rposition(|&start| start <= at).unwrap_or(0);
        (row, plain(rows[row]))
    }

    /// The character at a folded row and column of a line — the way back from
    /// a mouse click.
    pub fn position(&self, line: usize, row: usize, col: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        // Which stretch of the line this row is. A pane that is not folding
        // long lines has one row per line, and the walk below is the same walk
        // either way — it has to be, because the notes drawn into the text
        // take room on the screen and `text::char_at_column` counts only what
        // is in the file.
        let (start, end) = if self.wrap {
            let rows = self.rows_of(line);
            if rows.is_empty() {
                return text::line_start(self.rope, line);
            }
            let row = row.min(rows.len() - 1);
            (
                rows[row],
                rows.get(row + 1)
                    .copied()
                    .unwrap_or_else(|| text::line_end(self.rope, line)),
            )
        } else {
            (
                text::line_start(self.rope, line),
                text::line_end(self.rope, line),
            )
        };
        let mut at = start;
        let mut width = 0;
        while at < end {
            // A note sits before the character it belongs to, and it is not
            // text: a click anywhere in one means the character behind it.
            let note: usize = self
                .hints
                .iter()
                .filter(|(where_, _)| *where_ == at)
                .map(|(_, w)| w)
                .sum();
            if width + note > col {
                return at;
            }
            width += note;
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
    if line < view.top || layout.hidden(line) {
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
    let layout = Layout::of(view, doc, tab_width);
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
            match layout.rows_in(line) {
                // A folded-away line is not a row to step over: it is not
                // there. Keep going up without spending anything on it.
                0 => continue,
                rows => row = rows - 1,
            }
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
        return true;
    }
    // The next line that is drawn at all, stepping over anything folded away.
    let mut line = view.top + 1;
    while line < doc.len_lines() && layout.rows_in(line) == 0 {
        line += 1;
    }
    if line >= doc.len_lines() {
        return false;
    }
    view.top = line;
    view.top_row = 0;
    true
}

/// Move the view by `rows`, without touching the cursors. The mouse wheel, and
/// the commands that scroll.
pub fn scroll_by(view: &mut View, doc: &Document, tab_width: usize, rows: isize) {
    let layout = Layout::of(view, doc, tab_width);
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
    let layout = Layout::of(view, doc, tab_width);
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
            // A folded-away line is not under the pointer, because it is not
            // on the screen: step past it without counting a row for it.
            while line < doc.len_lines() && layout.rows_in(line) == 0 {
                line += 1;
            }
            if line >= doc.len_lines() {
                line = doc.len_lines() - 1;
                sub = layout.rows_in(line).saturating_sub(1);
                break;
            }
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
    let layout = Layout::of(view, doc, tab_width);
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
    fn a_folded_line_takes_no_rows_at_all() {
        // The whole of how folding works: a hidden line is worth no rows, and
        // every piece of arithmetic in this module already counts rows.
        let doc = doc_of("one\ntwo\nthree\nfour\n");
        let layout = Layout {
            rope: &doc.rope,
            hints: Vec::new(),
            width: 40,
            tab_width: 4,
            wrap: false,
            // Lines 1 and 2 are folded onto line 0.
            folds: Vec::from([(0, 2)]),
        };
        assert_eq!(layout.rows_in(0), 1, "the line it is folded onto stays");
        assert_eq!(layout.rows_in(1), 0);
        assert_eq!(layout.rows_in(2), 0);
        assert_eq!(layout.rows_in(3), 1, "and the file goes on after it");
        assert!(layout.rows_of(1).is_empty());
    }

    #[test]
    fn a_click_below_a_fold_lands_after_it_rather_than_in_it() {
        let doc = doc_of("one\ntwo\nthree\nfour\n");
        let mut view = view_of(&doc, 40, 10, false);
        view.folds = vec![(3, 13)];
        assert_eq!(view.folded(&doc.rope), vec![(0, 2)]);
        // The second row on the screen is line three, the first one that is
        // not folded away.
        let at = position_at_screen(&view, &doc, 4, 1, 0);
        assert_eq!(text::line_of(&doc.rope, at), 3);
    }

    #[test]
    fn a_fold_stays_around_what_it_was_put_around_when_the_text_above_moves() {
        let mut doc = doc_of("one\ntwo\nthree\nfour\n");
        let mut view = view_of(&doc, 40, 10, false);
        view.folds = vec![(3, 13)];
        // Four characters go in above it, the way typing a word would.
        let sel = Selections::single(Range::point(0));
        let edits = doc.apply_atomic(vec![crate::doc::Change::insert(0, "abcd".to_string())], &sel);
        let len = doc.rope.len_chars();
        view.absorb(&edits, len);
        assert_eq!(
            view.folds,
            vec![(7, 17)],
            "both ends moved with the text they were on"
        );
    }

    #[test]
    fn scrolling_steps_over_what_is_folded_away() {
        let doc = doc_of("one\ntwo\nthree\nfour\nfive\n");
        let mut view = view_of(&doc, 40, 10, false);
        // Lines 1 and 2 folded onto line 0.
        view.folds = vec![(3, 13)];
        scroll_by(&mut view, &doc, 4, 1);
        assert_eq!(view.top, 3, "one row down is the next line that is drawn");
    }

    #[test]
    fn a_line_that_fits_is_one_row() {
        let doc = doc_of("short\n");
        let layout = Layout {
            rope: &doc.rope,
            hints: Vec::new(),
            width: 40,
            tab_width: 4,
            wrap: true,
            folds: Vec::new(),
        };
        assert_eq!(layout.rows_of(0), vec![0]);
    }

    #[test]
    fn folding_breaks_between_words() {
        let doc = doc_of("the quick brown fox jumps\n");
        let layout = Layout {
            rope: &doc.rope,
            hints: Vec::new(),
            width: 10,
            tab_width: 4,
            wrap: true,
            folds: Vec::new(),
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
            hints: Vec::new(),
            width: 8,
            tab_width: 4,
            wrap: true,
            folds: Vec::new(),
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
            hints: Vec::new(),
            width: 40,
            tab_width: 4,
            wrap: false,
            folds: Vec::new(),
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
