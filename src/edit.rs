//! Everything that moves a cursor or changes text.
//!
//! Every operation here works on all the cursors at once, because there is no
//! such thing as one cursor — a plain cursor is a set of one, and the code
//! that handles a set of one is the code that handles a set of forty. That is
//! the only way multiple cursors stay honest: there is no second path through
//! typing a character that could behave differently.
//!
//! The rule for building a set of changes is that they arrive sorted and
//! disjoint, which they are: the cursors are, and each change is derived from
//! its own cursor. [`plan`] holds that true even where two cursors are close
//! enough that their changes would have reached for the same characters.

use ropey::Rope;

use crate::doc::{AppliedEdit, Change, Document};
use crate::lang;
use crate::text::{self, Class, Range, Selections, class_of};
use crate::view::{Layout, View};

/// What one keystroke of movement means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    /// The first thing on the line that is not a space — or, if you are
    /// already there, column one. Two presses of Home get you both, and the
    /// first press gets you the one you usually want.
    LineStart,
    LineEnd,
    PageUp,
    PageDown,
    DocStart,
    DocEnd,
    ParaUp,
    ParaDown,
}

/// Move every cursor.
///
/// `extend` is shift: the anchors stay and the heads move. Otherwise the
/// selections collapse — and collapse to the *edge* they are moving towards,
/// which is why pressing Left with something selected puts you at its start
/// rather than one character to the left of where the cursor happened to be.
pub fn move_cursors(doc: &Document, view: &mut View, motion: Motion, extend: bool, tab_width: usize) {
    let rope = &doc.rope;
    let folds = view.folded(rope);
    let hints = doc.inlay_columns();
    let layout = Layout {
        rope,
        hints: &hints,
        width: view.width(),
        tab_width,
        wrap: view.wrap,
        folds: &folds,
    };
    let height = view.height();

    // Vertical movement aims for a column, and keeps aiming for it across
    // short lines. Anything else forgets.
    let vertical = matches!(
        motion,
        Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
    );
    let goal = if vertical {
        view.goal.unwrap_or_else(|| layout.place(view.sel.primary().head).1)
    } else {
        0
    };

    view.sel.map(|range| {
        let from = if extend || range.is_empty() {
            range.head
        } else {
            // A collapse towards where you are going.
            match motion {
                Motion::Left | Motion::WordLeft | Motion::LineStart => range.start(),
                Motion::Right | Motion::WordRight | Motion::LineEnd => range.end(),
                _ => range.head,
            }
        };
        let to = match motion {
            Motion::Left => {
                if !extend && !range.is_empty() {
                    range.start()
                } else {
                    from.saturating_sub(1)
                }
            }
            Motion::Right => {
                if !extend && !range.is_empty() {
                    range.end()
                } else {
                    (from + 1).min(rope.len_chars())
                }
            }
            Motion::WordLeft => text::word_start(rope, from),
            Motion::WordRight => text::word_end(rope, from),
            Motion::LineStart => {
                let line = text::line_of(rope, from);
                let first = text::first_non_blank(rope, line);
                if from == first {
                    text::line_start(rope, line)
                } else {
                    first
                }
            }
            Motion::LineEnd => text::line_end(rope, text::line_of(rope, from)),
            Motion::Up => vertical_step(&layout, rope, from, -1, goal),
            Motion::Down => vertical_step(&layout, rope, from, 1, goal),
            Motion::PageUp => vertical_step(&layout, rope, from, -(height as isize - 2).max(1), goal),
            Motion::PageDown => vertical_step(&layout, rope, from, (height as isize - 2).max(1), goal),
            Motion::DocStart => 0,
            Motion::DocEnd => rope.len_chars(),
            Motion::ParaUp => paragraph(rope, from, -1),
            Motion::ParaDown => paragraph(rope, from, 1),
        };
        if extend {
            Range::new(range.anchor, to)
        } else {
            Range::point(to)
        }
    });

    view.goal = vertical.then_some(goal);
}

/// A step of `by` rows up or down, landing as near `goal` columns across as
/// the line allows.
fn vertical_step(layout: &Layout, rope: &Rope, at: usize, by: isize, goal: usize) -> usize {
    let line = text::line_of(rope, at);
    let (row, _) = layout.place(at);

    // Rows, not lines: a folded line is several rows, and pressing Down inside
    // one should move within it.
    let mut line = line;
    let mut row = row as isize + by;
    while row < 0 {
        if line == 0 {
            return 0;
        }
        line -= 1;
        row += layout.rows_in(line) as isize;
    }
    loop {
        let rows = layout.rows_in(line) as isize;
        if row < rows {
            break;
        }
        if line + 1 >= rope.len_lines() {
            return rope.len_chars();
        }
        row -= rows;
        line += 1;
    }
    layout.position(line, row as usize, goal)
}

/// The next blank line in a direction, which is what a paragraph boundary is
/// in prose and what a gap between functions is in code.
fn paragraph(rope: &Rope, at: usize, by: isize) -> usize {
    let mut line = text::line_of(rope, at) as isize;
    let last = rope.len_lines() as isize - 1;
    let blank = |l: isize| {
        l < 0 || l > last || {
            let start = text::line_start(rope, l as usize);
            let end = text::line_end(rope, l as usize);
            rope.slice(start..end).chars().all(char::is_whitespace)
        }
    };
    // Step off whatever we are standing on first, so holding the key moves.
    line += by;
    while line > 0 && line < last && blank(line) {
        line += by;
    }
    while line > 0 && line < last && !blank(line) {
        line += by;
    }
    text::line_start(rope, line.clamp(0, last) as usize)
}

/// Turn one change per cursor into a transaction.
///
/// Cursors are in order and apart, so the changes made from them are too —
/// except where two cursors are close enough that reaching backwards or
/// forwards makes them collide. Rather than corrupt the document, a change
/// that would reach into the one before it is trimmed back to where that one
/// ended, and one left with nothing to do is dropped.
fn plan(mut changes: Vec<Change>) -> Vec<Change> {
    let mut last_end = 0usize;
    let mut first = true;
    changes.retain_mut(|change| {
        if !first && change.from < last_end {
            change.from = last_end;
        }
        first = false;
        if change.to < change.from {
            return false;
        }
        if change.from == change.to && change.text.is_empty() {
            return false;
        }
        last_end = change.to;
        true
    });
    changes
}

/// One cursor's worth of change: what to replace, with what, and where to
/// leave the cursor afterwards.
struct Made {
    from: usize,
    to: usize,
    text: String,
    /// How far into the new text the cursor goes. Almost always the end of it
    /// — you type and the cursor is after what you typed — but typing `(`
    /// puts in `()` and belongs between them.
    cursor: usize,
}

/// The ordinary case: replace this, with that, cursor after it.
fn made(from: usize, to: usize, text: impl Into<String>) -> Option<Made> {
    let text = text.into();
    Some(Made {
        cursor: text.chars().count(),
        from,
        to,
        text,
    })
}

impl Made {
    /// Leave the cursor `n` characters into the new text instead of at its
    /// end.
    fn at(mut self, n: usize) -> Option<Self> {
        self.cursor = n.min(self.text.chars().count());
        Some(self)
    }
}

/// Make one change per cursor and put the cursors where the new text leaves
/// them.
///
/// `make` is given each selection and answers with what to replace and with
/// what, or `None` to leave that cursor alone. The new positions are worked
/// out here rather than by mapping afterwards, so that a cursor lands where
/// the operation meant it to rather than wherever a general rule would.
fn each(
    doc: &mut Document,
    view: &mut View,
    atomic: bool,
    mut make: impl FnMut(&Rope, Range) -> Option<Made>,
) -> Vec<AppliedEdit> {
    let before = view.sel.clone();
    let mut planned: Vec<Option<Made>> = Vec::with_capacity(before.len());
    for range in before.ranges() {
        planned.push(make(&doc.rope, *range));
    }

    let changes = plan(
        planned
            .iter()
            .flatten()
            .map(|m| Change::replace(m.from, m.to, m.text.clone()))
            .collect(),
    );
    if changes.is_empty() {
        return Vec::new();
    }

    let edits = if atomic {
        doc.apply_atomic(changes, &before)
    } else {
        doc.apply(changes, &before)
    };

    // Everything shifts by what the changes before it did. The changes are in
    // order, so one sweep with a running total is enough.
    let mut delta: isize = 0;
    let mut after = Vec::with_capacity(before.len());
    for (range, plan) in before.ranges().iter().zip(&planned) {
        match plan {
            Some(m) => {
                let start = (m.from as isize + delta).max(0) as usize;
                let inserted = m.text.chars().count();
                after.push(Range::point(start + m.cursor));
                delta += inserted as isize - (m.to as isize - m.from as isize);
            }
            None => after.push(Range::new(
                (range.anchor as isize + delta).max(0) as usize,
                (range.head as isize + delta).max(0) as usize,
            )),
        }
    }
    view.sel = Selections::many(after, before.primary_index());
    view.sel.clamp(doc.len_chars());
    doc.record_selections(&view.sel);
    view.goal = None;
    edits
}

/// Type text at every cursor, replacing what is selected.
pub fn insert(doc: &mut Document, view: &mut View, text: &str) -> Vec<AppliedEdit> {
    let text = text.to_string();
    each(doc, view, false, move |_, range| {
        made(range.start(), range.end(), text.clone())
    })
}

/// Put text in as one undoable action of its own. For pasting, and for
/// anything a language server sent.
pub fn insert_atomic(doc: &mut Document, view: &mut View, text: &str) -> Vec<AppliedEdit> {
    let text = text.to_string();
    each(doc, view, true, move |_, range| {
        made(range.start(), range.end(), text.clone())
    })
}

/// Type one character, with the small courtesies that make typing code
/// bearable: closing what you open, stepping over what is already closed, and
/// wrapping a selection rather than replacing it.
pub fn insert_char(
    doc: &mut Document,
    view: &mut View,
    c: char,
    auto_pairs: bool,
) -> Vec<AppliedEdit> {
    let language = lang::get(doc.language);
    let pairs: Vec<(char, char)> = if auto_pairs {
        language
            .brackets
            .iter()
            .copied()
            .chain(QUOTES.iter().copied())
            .collect()
    } else {
        Vec::new()
    };

    // Typing a bracket or a quote with something selected wraps it. This is
    // the one case where a character does not replace the selection, and it is
    // worth the exception: replacing a selection with `(` is almost never what
    // anybody meant.
    if let Some((open, close)) = pairs.iter().find(|(o, _)| *o == c)
        && view.sel.ranges().iter().any(|r| !r.is_empty())
    {
        let (open, close) = (*open, *close);
        return surround(doc, view, open, close);
    }

    // Typing the closing half of a pair that is already sitting there steps
    // over it instead of adding a second one. Only when every cursor is in
    // that position: a set of cursors where half would step over and half
    // would type is a set with no single sensible answer, and typing the
    // character is the answer that at least does what was asked.
    if auto_pairs
        && pairs.iter().any(|(_, close)| *close == c)
        && view.sel.ranges().iter().all(|range| {
            range.is_empty() && text::char_at(&doc.rope, range.head) == Some(c)
        })
    {
        view.sel.map(|range| Range::point(range.head + 1));
        doc.close_revision();
        return Vec::new();
    }

    each(doc, view, false, move |rope, range| {
        let at = range.head;
        if range.is_empty() && auto_pairs {
            let next = text::char_at(rope, at);
            if let Some((_, close)) = pairs.iter().find(|(o, _)| *o == c) {
                // Only close where a closing character would not be in the
                // way: at the end of a line, before whitespace, or before
                // something that is itself a closing bracket.
                let welcome = match next {
                    None => true,
                    Some(n) => {
                        n.is_whitespace() || pairs.iter().any(|(_, cl)| *cl == n) || n == ','
                    }
                };
                // A quote inside a word is an apostrophe, not the start of a
                // string, and nobody wants `don''t`.
                let inside_word = QUOTES.iter().any(|(o, _)| *o == c)
                    && at > 0
                    && class_of(rope.char(at - 1)) == Class::Word;
                if welcome && !inside_word {
                    // Between the two, which is the entire point of putting
                    // the second one in.
                    return made(at, at, format!("{c}{close}"))?.at(1);
                }
            }
        }
        made(range.start(), range.end(), c.to_string())
    })
}

/// Quote characters, where the two halves are the same character and so
/// cannot be told apart by looking. Kept separate from a language's brackets
/// for that reason.
const QUOTES: &[(char, char)] = &[('"', '"'), ('\'', '\''), ('`', '`')];

/// Put `open` and `close` around every selection, keeping it selected.
fn surround(doc: &mut Document, view: &mut View, open: char, close: char) -> Vec<AppliedEdit> {
    let before = view.sel.clone();
    let mut changes = Vec::new();
    for range in before.ranges() {
        changes.push(Change::insert(range.start(), open.to_string()));
        changes.push(Change::insert(range.end(), close.to_string()));
    }
    let edits = doc.apply_atomic(plan(changes), &before);

    // Each selection has gained one character in front of it and one behind,
    // and every selection before it has done the same.
    let mut after = Vec::with_capacity(before.len());
    for (i, range) in before.ranges().iter().enumerate() {
        let shift = (i * 2 + 1) as isize;
        after.push(Range::new(
            (range.anchor as isize + shift) as usize,
            (range.head as isize + shift) as usize,
        ));
    }
    view.sel = Selections::many(after, before.primary_index());
    doc.record_selections(&view.sel);
    edits
}

/// Break the line, carrying the indentation with it.
///
/// Inside a pair of brackets the new line goes in one level further, and the
/// closing bracket is pushed onto a line of its own — the shape you were going
/// to type anyway.
pub fn newline(doc: &mut Document, view: &mut View, tab_width: usize) -> Vec<AppliedEdit> {
    let unit = doc.indent.unit();
    let indent_width = doc.indent.width(tab_width);
    let brackets: Vec<(char, char)> = lang::get(doc.language).brackets.clone();

    each(doc, view, false, move |rope, range| {
        let at = range.start();
        let line = text::line_of(rope, at);
        let indent = text::indent_of(rope, line);
        let before = at.checked_sub(1).and_then(|i| text::char_at(rope, i));
        let after = text::char_at(rope, range.end());

        let opens = before.is_some_and(|b| brackets.iter().any(|(o, _)| *o == b));
        let closes_here = match (before, after) {
            (Some(b), Some(a)) => brackets.iter().any(|(o, c)| *o == b && *c == a),
            _ => false,
        };

        let text = if closes_here {
            // Between `{` and `}`: a line for the cursor and a line for the
            // brace. The cursor is left on the first of them by the caller
            // trimming what follows, so it is spelled out here instead.
            format!("\n{indent}{unit}")
        } else if opens {
            format!("\n{indent}{unit}")
        } else {
            format!("\n{indent}")
        };
        let _ = indent_width;
        made(range.start(), range.end(), text)
    })
}

/// The second half of [`newline`] where it split a bracket pair: put the
/// closing bracket on its own line, below the cursor.
///
/// Done as a separate change so that the cursor, which [`each`] leaves at the
/// end of what it typed, stays on the line between the two.
pub fn newline_closing(doc: &mut Document, view: &mut View, tab_width: usize) -> Vec<AppliedEdit> {
    let brackets: Vec<(char, char)> = lang::get(doc.language).brackets.clone();
    let _ = tab_width;
    let before = view.sel.clone();
    let mut changes = Vec::new();
    for range in before.ranges() {
        let at = range.head;
        let Some(next) = text::char_at(&doc.rope, at) else {
            continue;
        };
        if !brackets.iter().any(|(_, c)| *c == next) {
            continue;
        }
        let line = text::line_of(&doc.rope, at);
        // The indentation of the line the opening bracket was on, which is one
        // level less than the line the cursor is now on.
        let indent = text::indent_of(&doc.rope, line);
        let outer = indent
            .strip_suffix(&doc.indent.unit())
            .unwrap_or(&indent)
            .to_string();
        changes.push(Change::insert(at, format!("\n{outer}")));
    }
    if changes.is_empty() {
        return Vec::new();
    }
    let edits = doc.apply(plan(changes), &before);
    // The cursors did not move: everything went in after them.
    doc.record_selections(&view.sel);
    edits
}

/// Rub out backwards.
///
/// With something selected, that is the selection. In indentation, it is a
/// whole level, because deleting four spaces one at a time is nobody's idea of
/// a feature. Between a pair of brackets it is both of them.
pub fn delete_backward(doc: &mut Document, view: &mut View, tab_width: usize) -> Vec<AppliedEdit> {
    let indent = doc.indent;
    let brackets: Vec<(char, char)> = lang::get(doc.language)
        .brackets
        .iter()
        .copied()
        .chain(QUOTES.iter().copied())
        .collect();

    each(doc, view, false, move |rope, range| {
        if !range.is_empty() {
            return made(range.start(), range.end(), "");
        }
        let at = range.head;
        if at == 0 {
            return None;
        }
        let before = rope.char(at - 1);
        let after = text::char_at(rope, at);
        // An empty pair goes as a pair.
        if let Some(after) = after
            && brackets.iter().any(|(o, c)| *o == before && *c == after)
        {
            return made(at - 1, at + 1, "");
        }

        let line = text::line_of(rope, at);
        let start = text::line_start(rope, line);
        let all_blank = rope.slice(start..at).chars().all(|c| c == ' ');
        if all_blank && at > start && before == ' ' {
            // Back to the previous multiple of one indentation level.
            let column = at - start;
            let width = indent.width(tab_width).max(1);
            let target = start + (column - 1) / width * width;
            return made(target, at, "");
        }
        made(at - 1, at, "")
    })
}

pub fn delete_forward(doc: &mut Document, view: &mut View) -> Vec<AppliedEdit> {
    each(doc, view, false, |rope, range| {
        if !range.is_empty() {
            return made(range.start(), range.end(), "");
        }
        (range.head < rope.len_chars())
            .then(|| made(range.head, range.head + 1, ""))
            .flatten()
    })
}

pub fn delete_word_backward(doc: &mut Document, view: &mut View) -> Vec<AppliedEdit> {
    each(doc, view, false, |rope, range| {
        if !range.is_empty() {
            return made(range.start(), range.end(), "");
        }
        let to = range.head;
        let from = text::word_start(rope, to);
        (from < to).then(|| made(from, to, "")).flatten()
    })
}

pub fn delete_word_forward(doc: &mut Document, view: &mut View) -> Vec<AppliedEdit> {
    each(doc, view, false, |rope, range| {
        if !range.is_empty() {
            return made(range.start(), range.end(), "");
        }
        let from = range.head;
        let to = text::word_end(rope, from);
        (to > from).then(|| made(from, to, "")).flatten()
    })
}

pub fn delete_to_line_start(doc: &mut Document, view: &mut View) -> Vec<AppliedEdit> {
    each(doc, view, false, |rope, range| {
        let to = range.head;
        let line = text::line_of(rope, to);
        // To the first thing on the line, or the whole way if you are already
        // there — the same two-step Home does.
        let first = text::first_non_blank(rope, line);
        let from = if to > first {
            first
        } else {
            text::line_start(rope, line)
        };
        (from < to).then(|| made(from, to, "")).flatten()
    })
}

pub fn delete_to_line_end(doc: &mut Document, view: &mut View) -> Vec<AppliedEdit> {
    each(doc, view, false, |rope, range| {
        let from = range.head;
        let line = text::line_of(rope, from);
        let end = text::line_end(rope, line);
        // At the end of a line already, take the line ending itself, so
        // repeating it eats the file rather than stalling.
        let to = if from >= end {
            (end + 1).min(rope.len_chars())
        } else {
            end
        };
        (to > from).then(|| made(from, to, "")).flatten()
    })
}

/// The lines any cursor is on or touches, in order and without repeats. What
/// every line-shaped command works from.
fn touched_lines(rope: &Rope, sel: &Selections) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for range in sel.ranges() {
        let first = text::line_of(rope, range.start());
        let mut last = text::line_of(rope, range.end());
        // A selection ending exactly at the start of a line has not reached
        // into that line, whatever the arithmetic says.
        if last > first && range.end() == text::line_start(rope, last) {
            last -= 1;
        }
        match spans.last_mut() {
            Some(prev) if prev.1 + 1 >= first => prev.1 = prev.1.max(last),
            _ => spans.push((first, last)),
        }
    }
    spans
}

pub fn delete_line(doc: &mut Document, view: &mut View) -> Vec<AppliedEdit> {
    let before = view.sel.clone();
    let spans = touched_lines(&doc.rope, &before);
    let mut changes = Vec::new();
    for (first, last) in &spans {
        let from = text::line_start(&doc.rope, *first);
        // Take the line ending with the line, or the last line of a file would
        // leave a blank one behind.
        let to = if last + 1 < doc.len_lines() {
            text::line_start(&doc.rope, last + 1)
        } else {
            doc.len_chars()
        };
        changes.push(Change::delete(from, to));
    }
    let changes = plan(changes);
    if changes.is_empty() {
        return Vec::new();
    }
    let edits = doc.apply_atomic(changes, &before);
    view.absorb(&edits, doc.len_chars());
    view.sel.collapse_selections();
    doc.record_selections(&view.sel);
    edits
}

pub fn duplicate_line(doc: &mut Document, view: &mut View) -> Vec<AppliedEdit> {
    let before = view.sel.clone();
    let spans = touched_lines(&doc.rope, &before);
    let mut changes = Vec::new();
    for (first, last) in &spans {
        let from = text::line_start(&doc.rope, *first);
        let to = if last + 1 < doc.len_lines() {
            text::line_start(&doc.rope, last + 1)
        } else {
            doc.len_chars()
        };
        let mut copy = doc.rope.slice(from..to).to_string();
        if !copy.ends_with('\n') {
            copy.push('\n');
        }
        changes.push(Change::insert(from, copy));
    }
    let edits = doc.apply_atomic(plan(changes), &before);
    view.absorb(&edits, doc.len_chars());
    doc.record_selections(&view.sel);
    edits
}

/// Swap the lines the cursors are on with the ones above or below, taking the
/// cursors along.
pub fn move_lines(doc: &mut Document, view: &mut View, down: bool) -> Vec<AppliedEdit> {
    let before = view.sel.clone();
    let spans = touched_lines(&doc.rope, &before);
    let last_line = doc.len_lines().saturating_sub(1);

    let mut changes = Vec::new();
    for (first, last) in &spans {
        if down && *last >= last_line {
            return Vec::new();
        }
        if !down && *first == 0 {
            return Vec::new();
        }
        // Swapping a block with the line beyond it is one replacement: take
        // both, and put them back the other way round.
        let (block_first, block_last, other) = if down {
            (*first, *last, last + 1)
        } else {
            (*first, *last, first - 1)
        };
        let (from_line, to_line) = if down {
            (block_first, other)
        } else {
            (other, block_last)
        };
        let from = text::line_start(&doc.rope, from_line);
        let to = if to_line < last_line {
            text::line_start(&doc.rope, to_line + 1)
        } else {
            doc.len_chars()
        };

        let block_start = text::line_start(&doc.rope, block_first);
        let block_end = if block_last < last_line {
            text::line_start(&doc.rope, block_last + 1)
        } else {
            doc.len_chars()
        };
        let other_start = text::line_start(&doc.rope, other);
        let other_end = if other < last_line {
            text::line_start(&doc.rope, other + 1)
        } else {
            doc.len_chars()
        };

        let mut block = doc.rope.slice(block_start..block_end).to_string();
        let mut neighbour = doc.rope.slice(other_start..other_end).to_string();
        // The last line of a file has no newline of its own. Moving it up has
        // to give it one and take the other's away, or the two would fuse.
        if !block.ends_with('\n') {
            block.push('\n');
            neighbour = neighbour.trim_end_matches('\n').to_string();
        } else if !neighbour.ends_with('\n') {
            neighbour.push('\n');
            block = block.trim_end_matches('\n').to_string();
        }
        let text = if down {
            format!("{neighbour}{block}")
        } else {
            format!("{block}{neighbour}")
        };
        changes.push(Change::replace(from, to, text));
    }

    let changes = plan(changes);
    if changes.is_empty() {
        return Vec::new();
    }
    let edits = doc.apply_atomic(changes, &before);

    // The cursors move a line, which is not something mapping through the
    // edits can work out — the text they were in was replaced wholesale.
    let shift = |at: usize, rope: &Rope| -> usize {
        let line = text::line_of(rope, at);
        let col = at - text::line_start(rope, line);
        let target = if down {
            (line + 1).min(rope.len_lines().saturating_sub(1))
        } else {
            line.saturating_sub(1)
        };
        let start = text::line_start(rope, target);
        (start + col).min(text::line_end(rope, target))
    };
    let rope = doc.rope.clone();
    view.sel.map(|range| {
        Range::new(shift(range.anchor, &rope), shift(range.head, &rope))
    });
    doc.record_selections(&view.sel);
    edits
}

/// Pull the following line onto this one, with a single space between.
pub fn join_lines(doc: &mut Document, view: &mut View) -> Vec<AppliedEdit> {
    let before = view.sel.clone();
    let mut changes = Vec::new();
    for (first, last) in touched_lines(&doc.rope, &before) {
        // A selection spanning lines joins all of them; a bare cursor joins
        // the one below.
        let last = if last == first { first } else { last - 1 };
        for line in first..=last {
            if line + 1 >= doc.len_lines() {
                break;
            }
            let end = text::line_end(&doc.rope, line);
            let next = text::first_non_blank(&doc.rope, line + 1);
            // Nothing on the next line means nothing to separate from.
            let joiner = if next >= text::line_end(&doc.rope, line + 1) || end == text::line_start(&doc.rope, line) {
                ""
            } else {
                " "
            };
            changes.push(Change::replace(end, next, joiner));
        }
    }
    let changes = plan(changes);
    if changes.is_empty() {
        return Vec::new();
    }
    let edits = doc.apply_atomic(changes, &before);
    view.absorb(&edits, doc.len_chars());
    doc.record_selections(&view.sel);
    edits
}

/// Push lines right, or pull them left.
///
/// With nothing selected, indenting types an indent where you are, which is
/// what Tab does everywhere. With something selected it moves whole lines,
/// which is what Tab does in every editor that has ever had a selection.
pub fn indent(doc: &mut Document, view: &mut View, tab_width: usize, out: bool) -> Vec<AppliedEdit> {
    let unit = doc.indent.unit();
    let width = doc.indent.width(tab_width).max(1);
    let selecting = out || view.sel.ranges().iter().any(|r| !r.is_empty());

    if !selecting {
        // A tab from the middle of a line goes to the next stop rather than
        // adding a whole level, so that lining things up works.
        return each(doc, view, false, move |rope, range| {
            let col = text::visual_column(rope, range.head, tab_width);
            let text = match doc_indent_is_tabs(&unit) {
                true => "\t".to_string(),
                false => " ".repeat(width - col % width),
            };
            made(range.start(), range.end(), text)
        });
    }

    let before = view.sel.clone();
    let mut changes = Vec::new();
    for (first, last) in touched_lines(&doc.rope, &before) {
        for line in first..=last {
            let start = text::line_start(&doc.rope, line);
            let end = text::line_end(&doc.rope, line);
            if out {
                // Take one level off, however it happens to be spelled.
                let mut taken = 0;
                let mut at = start;
                while at < end && taken < width {
                    match doc.rope.char(at) {
                        ' ' => taken += 1,
                        '\t' => taken = width,
                        _ => break,
                    }
                    at += 1;
                }
                if at > start {
                    changes.push(Change::delete(start, at));
                }
            } else if start < end || first == last {
                // A blank line inside a block gets indented with the rest; a
                // blank line on its own does not get trailing whitespace.
                changes.push(Change::insert(start, unit.clone()));
            }
        }
    }
    let changes = plan(changes);
    if changes.is_empty() {
        return Vec::new();
    }
    let edits = doc.apply_atomic(changes, &before);
    view.absorb(&edits, doc.len_chars());
    doc.record_selections(&view.sel);
    edits
}

fn doc_indent_is_tabs(unit: &str) -> bool {
    unit.starts_with('\t')
}

/// Comment the selected lines out, or take the comments off if they are
/// already commented.
///
/// The marker goes at the shallowest indentation of the block, so a commented
/// block keeps its shape instead of collapsing to the left margin.
pub fn toggle_comment(doc: &mut Document, view: &mut View, tab_width: usize) -> Option<Vec<AppliedEdit>> {
    let language = lang::get(doc.language);
    let marker = language.line_comment.clone()?;
    let before = view.sel.clone();

    let mut lines: Vec<usize> = Vec::new();
    for (first, last) in touched_lines(&doc.rope, &before) {
        lines.extend(first..=last);
    }
    // A blank line is neither commented nor not; it should not decide which
    // way the whole block goes, and it should not gain a marker of its own.
    let interesting: Vec<usize> = lines
        .iter()
        .copied()
        .filter(|&line| {
            let start = text::line_start(&doc.rope, line);
            let end = text::line_end(&doc.rope, line);
            !doc.rope.slice(start..end).chars().all(char::is_whitespace)
        })
        .collect();
    if interesting.is_empty() {
        return Some(Vec::new());
    }

    let commented = |line: usize| {
        let at = text::first_non_blank(&doc.rope, line);
        let end = text::line_end(&doc.rope, line);
        let width = marker.chars().count();
        at + width <= end && doc.rope.slice(at..at + width) == marker
    };
    let all_commented = interesting.iter().all(|&line| commented(line));

    let mut changes = Vec::new();
    if all_commented {
        for &line in &interesting {
            let at = text::first_non_blank(&doc.rope, line);
            let width = marker.chars().count();
            let mut to = at + width;
            // Take the space we added with the marker, if it is still there.
            if text::char_at(&doc.rope, to) == Some(' ') {
                to += 1;
            }
            changes.push(Change::delete(at, to));
        }
    } else {
        let column = interesting
            .iter()
            .map(|&line| {
                text::visual_column(&doc.rope, text::first_non_blank(&doc.rope, line), tab_width)
            })
            .min()
            .unwrap_or(0);
        for &line in &interesting {
            let start = text::line_start(&doc.rope, line);
            let at = text::char_at_column(&doc.rope, line, column, tab_width);
            let _ = start;
            changes.push(Change::insert(at, format!("{marker} ")));
        }
    }

    let changes = plan(changes);
    if changes.is_empty() {
        return Some(Vec::new());
    }
    let edits = doc.apply_atomic(changes, &before);
    view.absorb(&edits, doc.len_chars());
    doc.record_selections(&view.sel);
    Some(edits)
}

/// What to do to a block of lines, for the three commands that are the same
/// command with a different verb in the middle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shuffle {
    /// Alphabetically, by what the line says.
    Sort,
    /// Alphabetically, the other way up.
    SortBackwards,
    /// The order they are in now, backwards.
    Reverse,
    /// Every line that has been seen before goes; the first of each stays.
    Unique,
}

/// Reorder the lines the selection covers — or the whole file, if nothing is
/// selected.
///
/// The whole file is the right answer for a bare cursor. Sorting one line is
/// not a thing anybody means, and a file of names or a list of imports with
/// nothing selected in it is exactly what somebody sorting means, which is
/// also what every other editor does with the same keystroke.
///
/// A block keeps whether it ended with a newline: sorting the last few lines
/// of a file must not put a newline on the end of one that never had one, and
/// must not take it off one that did.
pub fn shuffle_lines(doc: &mut Document, view: &mut View, how: Shuffle) -> Vec<AppliedEdit> {
    let before = view.sel.clone();
    let everything = before.ranges().iter().all(|r| r.is_empty());
    let spans: Vec<(usize, usize)> = if everything {
        vec![(0, doc.len_lines().saturating_sub(1))]
    } else {
        touched_lines(&doc.rope, &before)
    };

    let mut changes = Vec::new();
    for (first, last) in &spans {
        if last == first {
            continue;
        }
        let from = text::line_start(&doc.rope, *first);
        let to = if last + 1 < doc.len_lines() {
            text::line_start(&doc.rope, last + 1)
        } else {
            doc.len_chars()
        };
        let block = doc.rope.slice(from..to).to_string();
        let ended = block.ends_with('\n');
        let mut lines: Vec<&str> = block.split('\n').collect();
        if ended {
            // `split` leaves an empty piece after the last newline, which is
            // not a line and must not be sorted to the top of the file.
            lines.pop();
        }
        let mut lines: Vec<String> = lines.into_iter().map(str::to_string).collect();
        match how {
            // By what is on the line rather than by its leading whitespace,
            // which is how a list of indented things sorts the way it looks.
            Shuffle::Sort => lines.sort_by(|a, b| a.trim_start().cmp(b.trim_start())),
            Shuffle::SortBackwards => lines.sort_by(|a, b| b.trim_start().cmp(a.trim_start())),
            Shuffle::Reverse => lines.reverse(),
            Shuffle::Unique => {
                let mut seen = std::collections::HashSet::new();
                lines.retain(|line| seen.insert(line.clone()));
            }
        }
        let mut text = lines.join("\n");
        if ended {
            text.push('\n');
        }
        if text != block {
            changes.push(Change::replace(from, to, text));
        }
    }

    let changes = plan(changes);
    if changes.is_empty() {
        return Vec::new();
    }
    let edits = doc.apply_atomic(changes, &before);
    view.absorb(&edits, doc.len_chars());
    doc.record_selections(&view.sel);
    edits
}

/// Which way to change the case of some text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    Upper,
    Lower,
    /// The first letter of every word up and the rest down.
    Title,
}

impl Case {
    /// This text, in this case.
    fn of(&self, text: &str) -> String {
        match self {
            Case::Upper => text.to_uppercase(),
            Case::Lower => text.to_lowercase(),
            Case::Title => title_case(text),
        }
    }
}

/// The first letter of every word up, the rest of it down.
///
/// A word starts after anything that is not a letter or a digit, so
/// `it's a well-known fact` becomes `It's A Well-Known Fact` — which is what
/// a hyphen means and what an apostrophe does not.
fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut starting = true;
    for c in text.chars() {
        if starting {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        starting = !c.is_alphanumeric() && c != '\'';
    }
    out
}

/// Change the case of what is selected, leaving it selected.
pub fn change_case(doc: &mut Document, view: &mut View, case: Case) -> Vec<AppliedEdit> {
    let before = view.sel.clone();
    let mut changes = Vec::new();
    for range in before.ranges() {
        if range.is_empty() {
            continue;
        }
        let text = doc.slice(*range);
        let changed = case.of(&text);
        if changed != text {
            changes.push(Change::replace(range.start(), range.end(), changed));
        }
    }
    let changes = plan(changes);
    if changes.is_empty() {
        return Vec::new();
    }
    let edits = doc.apply_atomic(changes, &before);
    view.absorb(&edits, doc.len_chars());
    doc.record_selections(&view.sel);
    edits
}

// ---- Selecting ----

pub fn select_all(doc: &Document, view: &mut View) {
    view.sel = Selections::single(Range::new(0, doc.len_chars()));
}

/// Select the line the cursor is on; again, the one below as well.
pub fn select_line(doc: &Document, view: &mut View) {
    view.sel.map(|range| {
        let first = text::line_of(&doc.rope, range.start());
        let last = text::line_of(&doc.rope, range.end());
        let start = text::line_start(&doc.rope, first);
        // Already covering these lines exactly: reach for one more.
        let want = if range.start() == start
            && range.end() == line_after(&doc.rope, last)
            && last + 1 < doc.len_lines()
        {
            last + 1
        } else {
            last
        };
        Range::new(start, line_after(&doc.rope, want))
    });
}

fn line_after(rope: &Rope, line: usize) -> usize {
    if line + 1 < rope.len_lines() {
        text::line_start(rope, line + 1)
    } else {
        rope.len_chars()
    }
}

/// Select the word under each cursor.
pub fn select_word(doc: &Document, view: &mut View) {
    view.sel.map(|range| {
        if range.is_empty() {
            text::word_around(&doc.rope, range.head)
        } else {
            range
        }
    });
}

/// Another cursor a line above or below the primary, in the same column.
pub fn add_cursor_vertically(doc: &Document, view: &mut View, tab_width: usize, down: bool) {
    let folds = view.folded(&doc.rope);
    let hints = doc.inlay_columns();
    let layout = Layout {
        rope: &doc.rope,
        hints: &hints,
        width: view.width(),
        tab_width,
        wrap: false,
        folds: &folds,
    };
    // Grow from the edge of the block of cursors, so holding the key keeps
    // adding rather than fighting over one line.
    let edge = if down {
        view.sel.ranges().iter().map(|r| r.head).max()
    } else {
        view.sel.ranges().iter().map(|r| r.head).min()
    };
    let Some(from) = edge else { return };
    let line = text::line_of(&doc.rope, from);
    if down && line + 1 >= doc.len_lines() {
        return;
    }
    if !down && line == 0 {
        return;
    }
    let goal = view.goal.unwrap_or_else(|| layout.place(from).1);
    let target = if down { line + 1 } else { line - 1 };
    let at = text::char_at_column(&doc.rope, target, goal, tab_width);
    view.sel.push(Range::point(at));
    view.goal = Some(goal);
}

/// Another cursor at the next copy of what is selected — or, with nothing
/// selected, select the word first, which is what makes this one key rather
/// than two.
pub fn add_cursor_next_match(doc: &Document, view: &mut View) -> bool {
    if view.sel.ranges().iter().all(Range::is_empty) {
        select_word(doc, view);
        return true;
    }
    let primary = view.sel.primary();
    let needle = doc.slice(primary);
    if needle.is_empty() {
        return false;
    }
    let taken: Vec<usize> = view.sel.ranges().iter().map(|r| r.start()).collect();
    let Some(at) = find_from(&doc.rope, &needle, primary.end(), &taken, true) else {
        return false;
    };
    view.sel.push(Range::new(at, at + needle.chars().count()));
    true
}

/// A cursor at every copy of what is selected.
pub fn select_all_matches(doc: &Document, view: &mut View) -> usize {
    if view.sel.ranges().iter().all(Range::is_empty) {
        select_word(doc, view);
    }
    let needle = doc.slice(view.sel.primary());
    if needle.is_empty() {
        return 0;
    }
    let width = needle.chars().count();
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some(found) = find_from(&doc.rope, &needle, at, &[], false) {
        ranges.push(Range::new(found, found + width));
        at = found + width.max(1);
    }
    if ranges.is_empty() {
        return 0;
    }
    let count = ranges.len();
    view.sel = Selections::many(ranges, 0);
    count
}

/// The next occurrence of `needle` at or after `from`, skipping any position
/// already taken.
///
/// `wrap` decides what happens at the end of the file: searching for the next
/// match wants to come round the front again, and collecting every match must
/// not, or it would collect them forever.
fn find_from(rope: &Rope, needle: &str, from: usize, taken: &[usize], wrap: bool) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let text = rope.to_string();
    let start = rope.char_to_byte(from.min(rope.len_chars()));
    let taken: Vec<usize> = taken
        .iter()
        .map(|&at| rope.char_to_byte(at.min(rope.len_chars())))
        .collect();

    let mut at = start;
    while at <= text.len() {
        let Some(offset) = text[at..].find(needle) else {
            break;
        };
        let found = at + offset;
        if !taken.contains(&found) {
            return Some(rope.byte_to_char(found));
        }
        at = found + needle.len();
    }
    if !wrap {
        return None;
    }
    let mut at = 0;
    while at < start {
        let Some(offset) = text[at..].find(needle) else {
            break;
        };
        let found = at + offset;
        // Past where we began is ground the first pass already covered.
        if found >= start {
            break;
        }
        if !taken.contains(&found) {
            return Some(rope.byte_to_char(found));
        }
        at = found + needle.len();
    }
    None
}

/// A cursor at the end of every line the selections touch. Turns "select these
/// twenty lines" into "type at the end of all twenty".
pub fn cursors_to_line_ends(doc: &Document, view: &mut View) {
    let mut ranges = Vec::new();
    for (first, last) in touched_lines(&doc.rope, &view.sel) {
        for line in first..=last {
            ranges.push(Range::point(text::line_end(&doc.rope, line)));
        }
    }
    if !ranges.is_empty() {
        view.sel = Selections::many(ranges, 0);
    }
}

/// Where the bracket matching the one at the cursor is.
pub fn match_bracket(doc: &Document, at: usize) -> Option<usize> {
    let syntax = doc_syntax(doc)?;
    let byte = doc.rope.char_to_byte(at.min(doc.len_chars()));
    let found = syntax.matching_bracket(byte)?;
    Some(doc.rope.byte_to_char(found))
}

fn doc_syntax(doc: &Document) -> Option<&crate::syntax::Syntax> {
    doc.syntax.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocId, Indent};
    use ratatui::layout::Rect;

    fn setup(text: &str, at: usize) -> (Document, View) {
        lang::init();
        let mut doc = Document::scratch(DocId(0), "t".into(), Indent::Spaces(4));
        doc.rope = Rope::from_str(text);
        let mut view = View::new(doc.id, false);
        view.area = Rect::new(0, 0, 80, 24);
        view.sel = Selections::single(Range::point(at));
        (doc, view)
    }

    fn cursors(text: &str, at: &[usize]) -> (Document, View) {
        let (doc, mut view) = setup(text, at[0]);
        view.sel = Selections::many(at.iter().map(|&a| Range::point(a)).collect(), 0);
        (doc, view)
    }

    #[test]
    fn sorting_with_nothing_selected_sorts_the_whole_file() {
        let (mut doc, mut view) = setup("pear\napple\ncherry\n", 0);
        shuffle_lines(&mut doc, &mut view, Shuffle::Sort);
        assert_eq!(doc.rope.to_string(), "apple\ncherry\npear\n");
    }

    #[test]
    fn sorting_a_selection_leaves_the_rest_of_the_file_where_it_was() {
        let (mut doc, mut view) = setup("head\npear\napple\ntail\n", 0);
        // The two middle lines.
        view.sel = Selections::single(Range::new(5, 15));
        shuffle_lines(&mut doc, &mut view, Shuffle::Sort);
        assert_eq!(doc.rope.to_string(), "head\napple\npear\ntail\n");
    }

    #[test]
    fn sorting_the_end_of_a_file_does_not_invent_a_last_newline() {
        // The block being sorted is the end of the file, and the file does not
        // end in a newline. Sorting must not give it one, and must not sort the
        // nothing after the last line to the top.
        let (mut doc, mut view) = setup("pear\napple", 0);
        shuffle_lines(&mut doc, &mut view, Shuffle::Sort);
        assert_eq!(doc.rope.to_string(), "apple\npear");
    }

    #[test]
    fn sorting_is_by_what_the_line_says_not_by_its_indentation() {
        let (mut doc, mut view) = setup("    pear\napple\n", 0);
        shuffle_lines(&mut doc, &mut view, Shuffle::Sort);
        assert_eq!(doc.rope.to_string(), "apple\n    pear\n");
    }

    #[test]
    fn the_other_way_up_is_the_other_way_up() {
        let (mut doc, mut view) = setup("apple\npear\ncherry\n", 0);
        shuffle_lines(&mut doc, &mut view, Shuffle::SortBackwards);
        assert_eq!(doc.rope.to_string(), "pear\ncherry\napple\n");
    }

    #[test]
    fn reversing_turns_the_lines_back_to_front() {
        let (mut doc, mut view) = setup("one\ntwo\nthree\n", 0);
        shuffle_lines(&mut doc, &mut view, Shuffle::Reverse);
        assert_eq!(doc.rope.to_string(), "three\ntwo\none\n");
    }

    #[test]
    fn unique_keeps_the_first_of_each_and_the_order_they_were_in() {
        let (mut doc, mut view) = setup("b\na\nb\nc\na\n", 0);
        shuffle_lines(&mut doc, &mut view, Shuffle::Unique);
        assert_eq!(doc.rope.to_string(), "b\na\nc\n");
    }

    #[test]
    fn a_file_already_in_order_is_not_an_edit() {
        let (mut doc, mut view) = setup("a\nb\nc\n", 0);
        assert!(
            shuffle_lines(&mut doc, &mut view, Shuffle::Sort).is_empty(),
            "nothing to do is nothing to undo"
        );
    }

    #[test]
    fn one_line_is_never_reordered() {
        let (mut doc, mut view) = setup("only\n", 0);
        assert!(shuffle_lines(&mut doc, &mut view, Shuffle::Sort).is_empty());
    }

    #[test]
    fn title_case_capitalises_words_and_not_apostrophes() {
        let (mut doc, mut view) = setup("it's a WELL-known fact\n", 0);
        view.sel = Selections::single(Range::new(0, 21));
        change_case(&mut doc, &mut view, Case::Title);
        assert_eq!(doc.rope.to_string(), "It's A Well-Known Fact\n");
    }

    #[test]
    fn typing_at_several_cursors_types_at_all_of_them() {
        let (mut doc, mut view) = cursors("aa\nbb\ncc\n", &[0, 3, 6]);
        insert(&mut doc, &mut view, "x");
        assert_eq!(doc.rope.to_string(), "xaa\nxbb\nxcc\n");
        // And each cursor is after what it typed, not somewhere general.
        assert_eq!(
            view.sel.ranges().iter().map(|r| r.head).collect::<Vec<_>>(),
            vec![1, 5, 9]
        );
    }

    #[test]
    fn backspace_in_indentation_takes_a_whole_level() {
        let (mut doc, mut view) = setup("        x\n", 8);
        delete_backward(&mut doc, &mut view, 4);
        assert_eq!(doc.rope.to_string(), "    x\n");
        delete_backward(&mut doc, &mut view, 4);
        assert_eq!(doc.rope.to_string(), "x\n");
    }

    #[test]
    fn backspace_in_text_takes_one_character() {
        let (mut doc, mut view) = setup("    hello\n", 9);
        delete_backward(&mut doc, &mut view, 4);
        assert_eq!(doc.rope.to_string(), "    hell\n");
    }

    #[test]
    fn a_bracket_closes_itself_and_backspace_takes_both() {
        let (mut doc, mut view) = setup("", 0);
        insert_char(&mut doc, &mut view, '(', true);
        assert_eq!(doc.rope.to_string(), "()");
        assert_eq!(view.cursor(), 1);
        delete_backward(&mut doc, &mut view, 4);
        assert_eq!(doc.rope.to_string(), "");
    }

    #[test]
    fn typing_the_closing_bracket_steps_over_it() {
        let (mut doc, mut view) = setup("", 0);
        insert_char(&mut doc, &mut view, '(', true);
        insert_char(&mut doc, &mut view, 'x', true);
        insert_char(&mut doc, &mut view, ')', true);
        assert_eq!(doc.rope.to_string(), "(x)");
        assert_eq!(view.cursor(), 3);
    }

    #[test]
    fn an_apostrophe_in_a_word_is_not_the_start_of_a_string() {
        let (mut doc, mut view) = setup("don", 3);
        insert_char(&mut doc, &mut view, '\'', true);
        assert_eq!(doc.rope.to_string(), "don'");
    }

    #[test]
    fn a_bracket_typed_over_a_selection_wraps_it() {
        let (mut doc, mut view) = setup("hello world", 0);
        view.sel = Selections::single(Range::new(0, 5));
        insert_char(&mut doc, &mut view, '(', true);
        assert_eq!(doc.rope.to_string(), "(hello) world");
        // And it is still selected, so you can wrap it again.
        assert_eq!(doc.slice(view.sel.primary()), "hello");
    }

    #[test]
    fn enter_keeps_the_indentation_and_deepens_it_inside_a_bracket() {
        let (mut doc, mut view) = setup("    foo\n", 7);
        newline(&mut doc, &mut view, 4);
        assert_eq!(doc.rope.to_string(), "    foo\n    \n");

        let (mut doc, mut view) = setup("fn a() {\n", 8);
        newline(&mut doc, &mut view, 4);
        assert_eq!(doc.rope.to_string(), "fn a() {\n    \n");
    }

    #[test]
    fn enter_between_braces_puts_the_closing_one_on_its_own_line() {
        let (mut doc, mut view) = setup("fn a() {}\n", 8);
        newline(&mut doc, &mut view, 4);
        newline_closing(&mut doc, &mut view, 4);
        assert_eq!(doc.rope.to_string(), "fn a() {\n    \n}\n");
        // The cursor is on the middle line, indented.
        assert_eq!(text::line_of(&doc.rope, view.cursor()), 1);
    }

    #[test]
    fn indenting_a_selection_moves_whole_lines() {
        let (mut doc, mut view) = setup("one\ntwo\nthree\n", 0);
        // Up to but not into the third line: a selection ending exactly where
        // a line starts has not reached that line.
        view.sel = Selections::single(Range::new(0, 8));
        indent(&mut doc, &mut view, 4, false);
        assert_eq!(doc.rope.to_string(), "    one\n    two\nthree\n");
        indent(&mut doc, &mut view, 4, true);
        assert_eq!(doc.rope.to_string(), "one\ntwo\nthree\n");
    }

    #[test]
    fn tab_with_no_selection_goes_to_the_next_stop() {
        let (mut doc, mut view) = setup("ab", 2);
        indent(&mut doc, &mut view, 4, false);
        assert_eq!(doc.rope.to_string(), "ab  ");
    }

    #[test]
    fn commenting_keeps_the_shape_of_the_block() {
        lang::init();
        let mut doc = Document::scratch(DocId(0), "t.rs".into(), Indent::Spaces(4));
        doc.language = lang::by_name("rust").unwrap();
        doc.rope = Rope::from_str("    if x {\n        y();\n    }\n");
        let mut view = View::new(doc.id, false);
        view.area = Rect::new(0, 0, 80, 24);
        view.sel = Selections::single(Range::new(0, doc.len_chars()));

        toggle_comment(&mut doc, &mut view, 4);
        assert_eq!(
            doc.rope.to_string(),
            "    // if x {\n    //     y();\n    // }\n"
        );
        toggle_comment(&mut doc, &mut view, 4);
        assert_eq!(doc.rope.to_string(), "    if x {\n        y();\n    }\n");
    }

    #[test]
    fn moving_a_line_takes_the_cursor_with_it() {
        let (mut doc, mut view) = setup("one\ntwo\nthree\n", 4);
        move_lines(&mut doc, &mut view, false);
        assert_eq!(doc.rope.to_string(), "two\none\nthree\n");
        assert_eq!(text::line_of(&doc.rope, view.cursor()), 0);
        move_lines(&mut doc, &mut view, true);
        assert_eq!(doc.rope.to_string(), "one\ntwo\nthree\n");
        assert_eq!(text::line_of(&doc.rope, view.cursor()), 1);
    }

    #[test]
    fn moving_the_last_line_up_does_not_fuse_it_with_the_one_above() {
        let (mut doc, mut view) = setup("one\ntwo", 4);
        move_lines(&mut doc, &mut view, false);
        assert_eq!(doc.rope.to_string(), "two\none");
    }

    #[test]
    fn deleting_a_line_takes_its_ending_with_it() {
        let (mut doc, mut view) = setup("one\ntwo\nthree\n", 4);
        delete_line(&mut doc, &mut view);
        assert_eq!(doc.rope.to_string(), "one\nthree\n");
    }

    #[test]
    fn joining_puts_one_space_between() {
        let (mut doc, mut view) = setup("one\n    two\n", 0);
        join_lines(&mut doc, &mut view);
        assert_eq!(doc.rope.to_string(), "one two\n");
    }

    #[test]
    fn select_line_grows_a_line_at_a_time() {
        let (doc, mut view) = setup("one\ntwo\nthree\n", 1);
        select_line(&doc, &mut view);
        assert_eq!(doc.slice(view.sel.primary()), "one\n");
        select_line(&doc, &mut view);
        assert_eq!(doc.slice(view.sel.primary()), "one\ntwo\n");
    }

    #[test]
    fn another_cursor_lands_on_the_next_copy_of_the_word() {
        let (doc, mut view) = setup("foo bar foo baz foo\n", 1);
        add_cursor_next_match(&doc, &mut view);
        assert_eq!(view.sel.len(), 1);
        assert_eq!(doc.slice(view.sel.primary()), "foo");
        add_cursor_next_match(&doc, &mut view);
        assert_eq!(view.sel.len(), 2);
        add_cursor_next_match(&doc, &mut view);
        assert_eq!(view.sel.len(), 3);
        for range in view.sel.ranges() {
            assert_eq!(doc.slice(*range), "foo");
        }
    }

    #[test]
    fn every_copy_at_once() {
        let (doc, mut view) = setup("x = x + x\n", 0);
        view.sel = Selections::single(Range::new(0, 1));
        let found = select_all_matches(&doc, &mut view);
        assert_eq!(found, 3);
        assert_eq!(view.sel.len(), 3);
    }

    #[test]
    fn moving_with_a_selection_collapses_towards_where_you_are_going() {
        let (doc, mut view) = setup("hello world", 0);
        view.sel = Selections::single(Range::new(2, 7));
        move_cursors(&doc, &mut view, Motion::Left, false, 4);
        assert_eq!(view.cursor(), 2);

        view.sel = Selections::single(Range::new(2, 7));
        move_cursors(&doc, &mut view, Motion::Right, false, 4);
        assert_eq!(view.cursor(), 7);
    }

    #[test]
    fn home_goes_to_the_text_first_and_the_margin_second() {
        let (doc, mut view) = setup("    hello\n", 7);
        move_cursors(&doc, &mut view, Motion::LineStart, false, 4);
        assert_eq!(view.cursor(), 4);
        move_cursors(&doc, &mut view, Motion::LineStart, false, 4);
        assert_eq!(view.cursor(), 0);
    }

    #[test]
    fn going_down_a_short_line_and_on_remembers_the_column() {
        let (doc, mut view) = setup("aaaaaaaaaa\nbb\ncccccccccc\n", 8);
        move_cursors(&doc, &mut view, Motion::Down, false, 4);
        assert_eq!(view.cursor(), 13); // the end of `bb`
        move_cursors(&doc, &mut view, Motion::Down, false, 4);
        // Back out to column eight on the long line below.
        assert_eq!(view.cursor() - text::line_start(&doc.rope, 2), 8);
    }

    #[test]
    fn cursors_that_would_collide_do_not_corrupt_the_document() {
        // Two cursors one character apart, both deleting a word backwards.
        let (mut doc, mut view) = cursors("hello world", &[5, 6]);
        delete_word_backward(&mut doc, &mut view);
        // Whatever it does, it must still be text.
        assert!(doc.rope.len_chars() <= 11);
        doc.undo();
        assert_eq!(doc.rope.to_string(), "hello world");
    }
}
