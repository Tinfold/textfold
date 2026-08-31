//! Putting it on the screen.
//!
//! The shape is deliberately plain: a row of tabs at the top, a row of status
//! at the bottom, and everything in between is the file. No borders around the
//! text, no boxes inside boxes, no chrome that is not carrying information —
//! the point of a text editor is the text, and every column spent on
//! decoration is a column of code somebody cannot see.
//!
//! What floats over it — the fuzzy list, a suggestion, a hover — is drawn last
//! and drawn small, over the middle or beside the cursor, and gets out of the
//! way the moment it is dismissed.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use crate::app::{App, Overlay, PromptKind, Tone};
use crate::cmd::Cmd;
use crate::config::LineNumbers;
use crate::doc::{Diagnostic, DocId, Document, Severity};
use crate::text::{self, Range};
use crate::theme::Theme;
use crate::view::{self, Edge, Layout, View};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.screen = area;
    app.tab_hits.clear();
    app.tab_nudges.clear();
    app.status_hits.clear();
    if area.width < 4 || area.height < 3 {
        return;
    }

    let theme = app.theme;
    let paint = app.config.background();
    let ground = if paint {
        theme.background
    } else {
        Color::Reset
    };
    frame
        .buffer_mut()
        .set_style(area, Style::new().bg(ground).fg(theme.foreground));

    let tabs = Rect::new(area.x, area.y, area.width, 1);
    let status = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);

    place_panes(app, body);
    draw_tabs(frame, app, tabs, ground);
    draw_status(frame, app, status, ground);

    let mut cursor = None;
    for index in 0..app.panes.len() {
        if let Some(at) = draw_pane(frame, app, index, ground) {
            cursor = Some(at);
        }
    }

    if let Some(at) = draw_floating(frame, app, ground) {
        cursor = Some(at);
    }
    if let Some(at) = cursor {
        frame.set_cursor_position(at);
    }
    // Where the caret ended up, written down for whatever needs to open
    // beside it — filled in by the drawing every frame, the way a menu's own
    // area and the tab positions are, so that what opens next to the cursor
    // opens next to where the cursor actually is.
    app.caret = cursor.map(|at| (at.x, at.y));
}

/// Work out where each pane goes, and how wide its line numbers are.
///
/// Done before anything is drawn, because the mouse reads these back and a
/// click has to land on what is actually on the screen.
fn place_panes(app: &mut App, body: Rect) {
    // The docked panes come off the edges first, and what is left is the
    // middle for everything else to share. Done in this order because that is
    // what docking means: a sidebar is not competing with the code for room,
    // it is taking its share off the top and handing the rest over.
    let (middle, docked) = carve_docks(app, body);
    let ordinary: Vec<usize> = (0..app.panes.len())
        .filter(|at| app.panes[*at].dock.is_none())
        .collect();
    // What each pane in the middle gets, by the share it has been dragged to.
    let along = share_out(
        &ordinary.iter().map(|at| app.panes[*at].share).collect::<Vec<f32>>(),
        match app.side_by_side {
            true => middle.width,
            false => middle.height,
        },
        least_pane(app.side_by_side),
    );

    for index in 0..app.panes.len() {
        let frame = match docked.iter().find(|(at, _)| *at == index) {
            Some((_, frame)) => *frame,
            None => {
                let at = ordinary.iter().position(|p| *p == index).unwrap_or(0);
                let before: u16 = along[..at].iter().sum();
                let size = along.get(at).copied().unwrap_or(0);
                match app.side_by_side {
                    true => Rect::new(middle.x + before, middle.y, size, middle.height),
                    false => Rect::new(middle.x, middle.y + before, middle.width, size),
                }
            }
        };

        // A docked pane keeps one cell along the edge facing the middle for
        // the divider you drag it by. Taken out of the room the text gets,
        // not added to the room the dock takes, so what a plugin asked for is
        // what the dock occupies.
        // A pane in the middle that is not the first has a divider on its
        // leading edge, and it is the same thing as its focus rule: a line
        // already drawn between two panes, which is exactly where somebody
        // reaches to pull them. Side by side that costs nothing — the rule was
        // already there — and stacked it takes the row the rule cannot use.
        let after_another = app.panes[index].dock.is_none()
            && ordinary.first().is_some_and(|first| *first != index);
        let (grip, inner) = match app.panes[index].dock.map(|d| d.edge) {
            _ if frame.width == 0 || frame.height == 0 => (None, frame),
            None if after_another && app.side_by_side => (
                Some(Rect::new(frame.x, frame.y, 1, frame.height)),
                frame,
            ),
            None if after_another => (
                Some(Rect::new(frame.x, frame.y, frame.width, 1)),
                Rect::new(frame.x, frame.y + 1, frame.width, frame.height - 1),
            ),
            Some(Edge::Left) => (
                Some(Rect::new(frame.right() - 1, frame.y, 1, frame.height)),
                Rect::new(frame.x, frame.y, frame.width - 1, frame.height),
            ),
            Some(Edge::Right) => (
                Some(Rect::new(frame.x, frame.y, 1, frame.height)),
                Rect::new(frame.x + 1, frame.y, frame.width - 1, frame.height),
            ),
            Some(Edge::Bottom) => (
                Some(Rect::new(frame.x, frame.y, frame.width, 1)),
                Rect::new(frame.x, frame.y + 1, frame.width, frame.height - 1),
            ),
            None => (None, frame),
        };

        let id = app.panes[index].doc;
        let lines = app.doc(id).map(Document::len_lines).unwrap_or(1);
        let numbers = match app.config.line_numbers() {
            LineNumbers::Off => 1,
            // Room for the number, a space either side, and the mark that says
            // there is something wrong on this line.
            _ => (digits(lines) + 3) as u16,
        };
        // And one more for the bar that says this line is not what git has —
        // or, while two panes are being compared, not what the other pane has.
        // Only where there is something to say: a column of nothing down every
        // buffer would be a column wasted.
        let comparing = app
            .diff
            .as_ref()
            .is_some_and(|d| d.side_of(index).is_some());
        let numbers = numbers + u16::from(comparing || app.git.tracking(id));
        // A column of its own for the rule that says which pane has the
        // focus, rather than borrowing one from the line numbers — which for
        // a file long enough would have taken a digit with it.
        // A docked pane is a plugin's own surface. Line numbers down a tree of
        // file names are noise, and the room they take is room the names
        // needed — and the divider is already the line that says where the
        // pane ends, so it needs no focus rule either.
        let docked = app.panes[index].dock.is_some();
        let gutter = match docked {
            true => 0,
            false => numbers + rule_width(app.panes.len() as u16),
        };
        // A scroll bar down the right, except in a pane too narrow to spare it.
        let bar = if inner.width > 20 { 1 } else { 0 };
        let text_width = inner.width.saturating_sub(gutter + bar).max(1);

        let pane = &mut app.panes[index];
        // The whole rectangle, divider included, because that is what decides
        // which pane a click was in — and a click on the divider is a click on
        // the dock it belongs to.
        pane.frame = frame;
        pane.grip = grip;
        pane.gutter = gutter + (inner.x - frame.x);
        pane.area = Rect::new(inner.x + gutter, inner.y, text_width, inner.height);
    }
}

/// Take the docked panes off the edges of `body`, and say what is left for
/// everything else.
///
/// Left, then right, then bottom, and each one is clamped so that the middle
/// never disappears: a plugin asking for eighty columns on a narrow terminal
/// gets what there is to give rather than an editor with no room in it. Two
/// docks on the same edge sit next to each other and share what that edge was
/// given, which is the least surprising answer and needs no rule of its own.
fn carve_docks(app: &App, body: Rect) -> (Rect, Vec<(usize, Rect)>) {
    let mut middle = body;
    let mut placed: Vec<(usize, Rect)> = Vec::new();

    for edge in [Edge::Left, Edge::Right, Edge::Bottom] {
        let here: Vec<(usize, u16)> = app
            .panes
            .iter()
            .enumerate()
            .filter_map(|(at, pane)| {
                pane.dock
                    .filter(|dock| dock.edge == edge)
                    .map(|dock| (at, dock.size.max(1)))
            })
            .collect();
        if here.is_empty() {
            continue;
        }
        // Never more than half of what is left, so the code stays the thing
        // the editor is mostly showing.
        let room = match edge.is_side() {
            true => middle.width,
            false => middle.height,
        };
        let wanted: u16 = here.iter().map(|(_, size)| *size).sum();
        let given = wanted.min(room.saturating_sub(MIN_MIDDLE));
        if given == 0 {
            // Nothing to give. The dock is placed empty rather than left
            // unplaced, so a click can never land on a stale rectangle.
            for (at, _) in here {
                placed.push((at, Rect::new(middle.x, middle.y, 0, 0)));
            }
            continue;
        }
        // Each dock keeps its share of what was actually given, so shrinking
        // the terminal shrinks them in proportion rather than starving the
        // last one.
        let mut used = 0;
        for (which, (at, size)) in here.iter().enumerate() {
            let last = which + 1 == here.len();
            let share = match last {
                true => given - used,
                false => ((*size as u32 * given as u32) / wanted.max(1) as u32) as u16,
            };
            let frame = match edge {
                Edge::Left => Rect::new(middle.x + used, middle.y, share, middle.height),
                Edge::Right => Rect::new(
                    middle.x + middle.width - given + used,
                    middle.y,
                    share,
                    middle.height,
                ),
                Edge::Bottom => Rect::new(
                    middle.x,
                    middle.y + middle.height - given + used,
                    middle.width,
                    share,
                ),
            };
            placed.push((*at, frame));
            used += share;
        }
        match edge {
            Edge::Left => {
                middle.x += given;
                middle.width -= given;
            }
            Edge::Right => middle.width -= given,
            Edge::Bottom => middle.height -= given,
        }
    }
    (middle, placed)
}

/// Divide `room` between panes in the proportions they have been dragged to.
///
/// Proportions rather than columns, so that resizing the terminal keeps the
/// layout you chose rather than the numbers it happened to work out to. The
/// last pane takes the remainder, so a share that does not divide evenly does
/// not leave a stripe of nothing down the middle of the screen.
pub fn share_out(shares: &[f32], room: u16, least: u16) -> Vec<u16> {
    if shares.is_empty() {
        return Vec::new();
    }
    // What is left may be less than the least a pane is allowed, on a terminal
    // too small to hold them all. There is no answer that respects both, and
    // the one that must not happen is a panic: `clamp` with a floor above its
    // ceiling is a crash, and "the window got short" is not a crashing matter.
    let least = least.min(room);
    let total: f32 = shares.iter().map(|s| s.max(0.0)).sum();
    if total <= 0.0 {
        // Nothing to go on. Equal, which is what they all were to begin with.
        let each = room / shares.len() as u16;
        let mut out = vec![each; shares.len()];
        *out.last_mut().expect("not empty") = room - each * (shares.len() as u16 - 1);
        return out;
    }
    let mut out: Vec<u16> = Vec::with_capacity(shares.len());
    let mut used = 0;
    for (at, share) in shares.iter().enumerate() {
        if at + 1 == shares.len() {
            out.push(room.saturating_sub(used));
            break;
        }
        // Never nothing: a pane dragged shut would be a pane with no edge left
        // to drag it back by.
        let size = ((share.max(0.0) / total) * room as f32).round() as u16;
        let left = room.saturating_sub(used);
        let size = size.clamp(least.min(left), left);
        out.push(size);
        used += size;
    }
    out
}

/// The least a pane in the middle may be dragged down to: columns when they
/// are side by side, rows when they are stacked. A pane narrower than a line
/// number and a word is not a pane; a pane shorter than three rows has no room
/// to show you where you are in it.
pub const MIN_PANE: u16 = 8;
pub const MIN_PANE_ROWS: u16 = 3;

/// Which of those two applies, given how the panes are arranged.
pub fn least_pane(side_by_side: bool) -> u16 {
    match side_by_side {
        true => MIN_PANE,
        false => MIN_PANE_ROWS,
    }
}

/// The least the middle may be squeezed to. Below this the editor stops being
/// an editor with a sidebar and becomes a sidebar with a rumour of an editor.
const MIN_MIDDLE: u16 = 20;

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// How wide the pane's left edge is: one column for the focus rule when there
/// is more than one pane, and nothing at all when there is not.
pub fn rule_width(panes: u16) -> u16 {
    u16::from(panes > 1)
}

// ---------------------------------------------------------------------------
// The text.
// ---------------------------------------------------------------------------

/// Draw one pane, and say where the terminal's own cursor should go if this is
/// the pane with the focus.
/// The colours tree-sitter worked out for the lines on screen.
fn syntax_spans(
    doc: &crate::doc::Document,
    from_char: usize,
    to_char: usize,
) -> Vec<(Range, crate::theme::Role)> {
    let from_grammar = grammar_spans(doc, from_char, to_char);
    // What the server worked out, over the top of what the grammar guessed.
    //
    // The grammar knows the shape of the code without knowing anything about
    // it: `Foo(x)` is a call, and whether it is a function or a constructor is
    // a lookup rather than a parse. Where the server has an opinion it is the
    // better one, because it had to look something up to have it — so the
    // server's spans are laid down whole and the grammar's are cut around
    // them, which also keeps the list in order and free of overlaps, the two
    // things [`colour_of`] is built on.
    let from_server: Vec<(Range, crate::theme::Role)> = doc
        .semantic
        .iter()
        .filter(|(range, _)| range.end() > from_char && range.start() < to_char)
        .cloned()
        .collect();
    if from_server.is_empty() {
        return from_grammar;
    }
    let mut spans: Vec<(Range, crate::theme::Role)> = from_server.clone();
    for (range, role) in from_grammar {
        for piece in outside_of(range, &from_server) {
            spans.push((piece, role));
        }
    }
    spans.sort_by_key(|(range, _)| range.start());
    spans
}

/// The parts of a range that no span in `covered` covers.
///
/// `covered` is in order and its ranges do not overlap, which is what lets
/// this be one walk.
fn outside_of(range: Range, covered: &[(Range, crate::theme::Role)]) -> Vec<Range> {
    let mut pieces = Vec::new();
    let mut at = range.start();
    for (taken, _) in covered {
        if taken.end() <= at {
            continue;
        }
        if taken.start() >= range.end() {
            break;
        }
        if taken.start() > at {
            pieces.push(Range::new(at, taken.start()));
        }
        at = at.max(taken.end());
    }
    if at < range.end() {
        pieces.push(Range::new(at, range.end()));
    }
    pieces
}

/// What the grammar makes of this stretch of the file.
fn grammar_spans(
    doc: &crate::doc::Document,
    from_char: usize,
    to_char: usize,
) -> Vec<(Range, crate::theme::Role)> {
    doc.syntax
        .as_ref()
        .map(|syntax| {
            syntax
                .highlights(
                    &doc.rope,
                    doc.rope.char_to_byte(from_char)..doc.rope.char_to_byte(to_char),
                )
                .into_iter()
                .map(|(bytes, role)| {
                    (
                        Range::new(
                            doc.rope.byte_to_char(bytes.start),
                            doc.rope.byte_to_char(bytes.end),
                        ),
                        role,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn draw_pane(frame: &mut Frame, app: &App, index: usize, ground: Color) -> Option<Position> {
    let view = &app.panes[index];
    let doc = app.doc(view.doc)?;
    let theme = &app.theme;
    let tab_width = app.config.tab_width();
    let focused = index == app.focus.min(app.panes.len() - 1);

    // The divider, and the fact that you can pull it. Drawn brighter where
    // the pane it belongs to has the focus, for the same reason the focus
    // rule is: so that "which of these am I typing into" has an answer on the
    // screen.
    // The rule down a pane's own left edge is already drawn by the pane, and
    // for a pane in the middle that rule *is* the divider — drawing over it
    // would only replace one vertical line with another.
    let draw_grip = view.dock.is_some() || !app.side_by_side;
    if let Some(grip) = view.grip.filter(|_| draw_grip) {
        let colour = match focused {
            true => theme.accent,
            false => theme.chrome(),
        };
        let across = match view.dock.map(|d| d.edge) {
            Some(view::Edge::Bottom) => true,
            Some(_) => false,
            // A pane in the middle, stacked: the divider lies across.
            None => true,
        };
        let line = if across { "\u{2500}" } else { "\u{2502}" };
        let buf = frame.buffer_mut();
        for step in 0..if across { grip.width } else { grip.height } {
            let at = match across {
                true => Position::new(grip.x + step, grip.y),
                false => Position::new(grip.x, grip.y + step),
            };
            if let Some(cell) = buf.cell_mut(at) {
                cell.set_symbol(line);
                cell.set_style(Style::new().bg(ground).fg(colour));
            }
        }
    }

    let area = view.area;
    if area.width == 0 || area.height == 0 {
        return None;
    }

    let folds = view.folded(&doc.rope);
    let hints = doc.inlay_columns();
    let layout = Layout {
        rope: &doc.rope,
        hints: &hints,
        width: area.width as usize,
        tab_width,
        wrap: view.wrap,
        folds: &folds,
    };
    let (first, last) = view::visible_lines(view, doc, tab_width);

    // Colours for everything on screen, worked out once for the whole visible
    // stretch rather than per line: one query, and the answer walked through
    // in order as the rows are drawn.
    let from_char = text::line_start(&doc.rope, first);
    let to_char = if last < doc.len_lines() {
        text::line_start(&doc.rope, last)
    } else {
        doc.len_chars()
    };
    // A panel's colours come from the plugin that filled it, and stand in for
    // the tree-sitter highlights a panel has none of. Same shape, so nothing
    // below this line knows the difference.
    let spans: Vec<(Range, crate::theme::Role)> = match &doc.panel {
        Some(panel) => panel
            .spans
            .iter()
            .filter(|(range, _)| range.end() > from_char && range.start() < to_char)
            .cloned()
            .collect(),
        None => syntax_spans(doc, from_char, to_char),
    };

    // What the pointer is resting on, worked out once for the pane. Nothing
    // for a pane with no panel in it, which is nearly all of them.
    let hover = app
        .pointer
        .and_then(|(column, row)| app.panel_action_under(index, column, row));

    let selections = view.sel.ranges();
    let cursors: Vec<usize> = selections.iter().map(|r| r.head).collect();
    let cursor_lines: Vec<usize> = cursors
        .iter()
        .map(|&at| text::line_of(&doc.rope, at))
        .collect();
    let primary = view.sel.primary().head;
    // The bracket under the cursor, and its partner, so that both light up.
    let partner = crate::edit::match_bracket(doc, primary);

    let mut cursor_at = None;
    let mut screen = area.y;
    let mut line = first;
    let mut sub = view.top_row;

    while screen < area.y + area.height && line < doc.len_lines() {
        let rows = layout.rows_of(line);
        let mut row = sub.min(rows.len().saturating_sub(1));
        while row < rows.len() && screen < area.y + area.height {
            draw_gutter(
                frame.buffer_mut(),
                app,
                view,
                doc,
                Gutter {
                    line,
                    numbered: row == 0,
                    screen,
                    cursor_lines: &cursor_lines,
                    pane: index,
                    ground,
                },
            );
            let placed = draw_row(
                frame.buffer_mut(),
                app,
                view,
                doc,
                &layout,
                DrawRow {
                    line,
                    row,
                    screen,
                    rows: &rows,
                    spans: &spans,
                    hover,
                    cursors: &cursors,
                    cursor_lines: &cursor_lines,
                    partner,
                    focused,
                    ground,
                },
            );
            if let Some(at) = placed
                && focused
            {
                cursor_at = Some(at);
            }
            row += 1;
            screen += 1;
        }
        sub = 0;
        line += 1;
    }

    // The rule down the left edge that says which pane has the focus. Not on
    // a docked one: a dock has no line-number margin to borrow the column
    // from, so the rule would be drawn straight over the first character of
    // every row — the `d` of `debugpy`, the arrow beside the frame you are
    // standing on. Its divider already says where it ends, which is what that
    // rule is for.
    if app.panes.len() > 1 && view.dock.is_none() {
        mark_focus(frame.buffer_mut(), view, theme, focused);
    }
    draw_scrollbar(frame.buffer_mut(), view, doc, theme);
    cursor_at
}

/// What one row of text needs to know about itself.
struct DrawRow<'a> {
    line: usize,
    row: usize,
    screen: u16,
    rows: &'a [usize],
    spans: &'a [(Range, crate::theme::Role)],
    /// The stretch of a panel the pointer is on, where it is on one.
    hover: Option<Range>,
    cursors: &'a [usize],
    cursor_lines: &'a [usize],
    partner: Option<usize>,
    focused: bool,
    ground: Color,
}

fn draw_row(
    buf: &mut Buffer,
    app: &App,
    view: &View,
    doc: &Document,
    layout: &Layout,
    it: DrawRow,
) -> Option<Position> {
    let theme = &app.theme;
    let area = view.area;
    let tab_width = app.config.tab_width();
    let show_space = app.config.show_whitespace();

    let start = it.rows[it.row];
    let end = it
        .rows
        .get(it.row + 1)
        .copied()
        .unwrap_or_else(|| text::line_end(&doc.rope, it.line));

    let on_cursor_line = it.cursor_lines.contains(&it.line);
    let line_bg = if on_cursor_line && it.focused && theme.current_line != theme.background {
        theme.current_line
    } else {
        it.ground
    };
    // The whole row first, so that the part past the end of the line is
    // shaded too — a cursor line that stops where the text stops looks broken.
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell_mut(Position::new(x, it.screen)) {
            cell.set_char(' ').set_style(Style::new().bg(line_bg));
        }
    }
    for &column in app.config.rulers() {
        let x = area.x as usize + column.saturating_sub(view.left);
        if column >= view.left
            && x < (area.x + area.width) as usize
            && let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen))
        {
            cell.set_style(Style::new().bg(line_bg).fg(theme.ruler));
            cell.set_char('\u{2502}');
        }
    }

    let diagnostics = diagnostics_on(doc, start, end);
    let mut cursor_at = None;
    let mut at = start;
    let mut column = 0usize;

    // The left edge, for a pane that is not folding lines.
    let skip = if view.wrap { 0 } else { view.left };

    // The block an extra cursor is drawn as. The terminal has one cursor of
    // its own and multi-cursor editing needs all of them, so every cursor but
    // the primary is painted rather than placed.
    let block = Style::new()
        .bg(theme.cursor)
        .fg(theme.on_accent)
        .add_modifier(Modifier::BOLD);

    while at <= end {
        // What the server says is here that the code does not say: the type of
        // a variable, the name of an argument. Drawn before the character it
        // belongs to, and counted in the width of the line by
        // [`Layout::hints_between`], so that everything from a click to a
        // selection still lands where it looks like it should.
        for hint in doc.inlays.iter().filter(|hint| hint.at == at) {
            let style = Style::new()
                .bg(line_bg)
                .fg(theme.faint)
                .add_modifier(Modifier::ITALIC);
            for c in hint.text.chars() {
                let width = text::char_width(c, column, tab_width);
                if column >= skip {
                    let x = area.x as usize + column - skip;
                    if x >= (area.x + area.width) as usize {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen)) {
                        cell.set_style(style).set_char(c);
                    }
                }
                column += width;
            }
        }
        let is_cursor = it.cursors.contains(&at);
        let extra = is_cursor && it.focused && at != view.sel.primary().head;
        if is_cursor && column >= skip {
            let x = area.x as usize + column - skip;
            if x < (area.x + area.width) as usize {
                if at == view.sel.primary().head {
                    cursor_at = Some(Position::new(x as u16, it.screen));
                } else if extra && let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen))
                {
                    // A cursor sitting past the last character of the line has
                    // no character coming to draw it, so it is painted here.
                    // One with a character under it is painted below, with the
                    // character — painting it here as well would only be
                    // overwritten by it.
                    cell.set_style(block);
                }
            }
        }
        if at >= end {
            break;
        }

        let c = doc.rope.char(at);
        let width = text::char_width(c, column, tab_width);
        let selected = view
            .sel
            .ranges()
            .iter()
            .any(|range| range.contains(at) && !range.is_empty());
        // The other places in this file that are the same thing as the one
        // under the cursor, in the colour that means "these go together" — the
        // same one a selection is drawn in, because it is the same statement.
        // A selection you made has edges you watched appear; this does not
        // need a colour of its own to be told apart from it.
        let same = doc.highlights.iter().any(|range| range.contains(at));

        // Lit under the pointer, in the colour every other list in textfold
        // uses for the row you are pointing at. The span keeps its own
        // foreground: a button's colour is what says whether it is a frame, a
        // file or a heading, and a highlight that repainted the text would
        // throw that away to say something the background already says.
        let hovered = it.hover.is_some_and(|range| range.contains(at));
        let mut style = Style::new().bg(if selected || hovered || same {
            theme.selection
        } else {
            line_bg
        });
        style = style.fg(colour_of(it.spans, at, theme));

        if let Some(severity) = diagnostics
            .iter()
            .filter(|d| d.range.contains(at) || (d.range.is_empty() && d.range.start() == at))
            .map(|d| d.severity)
            .min()
        {
            // Underlined in the colour of how bad it is, rather than
            // recoloured: the code should still look like code.
            //
            // The colour only where the terminal has the sequence for it.
            // Where it has not, asking anyway does not cost a colour, it
            // costs the screen — see [`crate::term::underline_colour`] — and
            // a plain underline beside the mark already in the gutter says
            // the same thing without the wreckage.
            style = style.add_modifier(Modifier::UNDERLINED);
            if crate::term::underline_colour() {
                style = style.underline_color(severity_colour(severity, theme));
            }
        }
        if Some(at) == it.partner || (it.partner.is_some() && at == view.sel.primary().head) {
            style = style.add_modifier(Modifier::BOLD).fg(theme.bracket_match);
        }

        // What to actually draw: a tab is spaces, and whitespace shows itself
        // only when asked.
        let (shown, filler) = match c {
            '\t' if show_space => ('\u{2192}', ' '),
            '\t' => (' ', ' '),
            ' ' if show_space => ('\u{00b7}', ' '),
            c if c.is_control() => ('\u{fffd}', ' '),
            c => (c, ' '),
        };
        let faded = matches!(c, ' ' | '\t') && show_space;

        for step in 0..width {
            if column + step < skip {
                continue;
            }
            let x = area.x as usize + column + step - skip;
            if x >= (area.x + area.width) as usize {
                break;
            }
            let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen)) else {
                break;
            };
            let mut style = style;
            if faded {
                style = style.fg(theme.whitespace);
            }
            // An extra cursor is a block *over* the character it is on, so it
            // goes on last: anything before it here would be painted over.
            if extra && step == 0 {
                style = block;
            }
            cell.set_style(style);
            cell.set_char(if step == 0 { shown } else { filler });
        }
        column += width;
        at += 1;
        if column >= skip + area.width as usize {
            // The rest of the line is off the side. Say so rather than
            // stopping silently.
            if !view.wrap && at < end {
                let x = area.x + area.width - 1;
                if let Some(cell) = buf.cell_mut(Position::new(x, it.screen)) {
                    cell.set_char('\u{203a}')
                        .set_style(Style::new().bg(line_bg).fg(theme.faint));
                }
            }
            break;
        }
    }

    // What a server has to say about this line, after the end of it: "3
    // implementations", "Run test". After the text rather than on a line of
    // its own, because a line of its own would mean row twelve is not line
    // twelve, and every click, drag and scroll in the editor would have to
    // know about it.
    if it.row + 1 == it.rows.len() {
        let note: Vec<&str> = doc
            .lenses
            .iter()
            .filter(|lens| text::line_of(&doc.rope, lens.at) == it.line)
            .map(|lens| lens.label.as_str())
            .collect();
        if !note.is_empty() {
            let text = format!("  {}", note.join(" · "));
            let style = Style::new()
                .bg(line_bg)
                .fg(theme.faint)
                .add_modifier(Modifier::ITALIC);
            let mut x = area.x as usize + column.saturating_sub(skip);
            for c in text.chars() {
                if x >= (area.x + area.width) as usize {
                    break;
                }
                if let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen)) {
                    cell.set_style(style).set_char(c);
                }
                x += 1;
            }
            column += text.chars().count();
        }
    }

    // A line with something folded onto it says so, and says how much, right
    // after its text. Not in the margin: the one column there is already the
    // breakpoint's, and the end of the line is where the eye is anyway when it
    // wonders where the rest of the function went.
    if it.row + 1 == it.rows.len()
        && let Some((_, last)) = layout.folds.iter().find(|(first, _)| *first == it.line)
    {
        let hidden = last - it.line;
        let note = fold_mark(hidden);
        let style = Style::new()
            .bg(theme.selection)
            .fg(theme.faint)
            .add_modifier(Modifier::ITALIC);
        let mut x = area.x as usize + column.saturating_sub(skip);
        for c in note.chars() {
            if x >= (area.x + area.width) as usize {
                break;
            }
            if let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen)) {
                cell.set_style(style).set_char(c);
            }
            x += 1;
        }
    }

    // What a plugin is offering, drawn where it would go and in the colour of
    // something that is not there yet. Only the primary cursor's row, only
    // where the cursor actually is, and only to the right of it — so nothing
    // that is really in the file is covered up, and the map from screen to
    // text is untouched. Taking it is what puts it in the file; until then
    // this is the only place it exists.
    if let Some(hint) = &doc.hint
        && hint.at == view.sel.primary().head
        && hint.at >= start
        && hint.at <= end
    {
        let mut x = area.x as usize + column - skip;
        let stop = (area.x + area.width) as usize;
        let ghost = Style::new().bg(line_bg).fg(theme.faint).add_modifier(Modifier::ITALIC);
        // The first line of it. A suggestion is often several, and the rest
        // are counted rather than drawn: rows below this one belong to the
        // file, and borrowing them would move the text under somebody's mouse.
        let mut first = hint.text.lines();
        for c in first.next().unwrap_or_default().chars() {
            if x >= stop {
                break;
            }
            if let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen)) {
                cell.set_style(ghost).set_char(c);
            }
            x += 1;
        }
        let more = hint.text.lines().count().saturating_sub(1);
        if more > 0 {
            let note = format!("  +{more} line{}", if more == 1 { "" } else { "s" });
            for c in note.chars() {
                if x >= stop {
                    break;
                }
                if let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen)) {
                    cell.set_style(ghost).set_char(c);
                }
                x += 1;
            }
        }
    }
    cursor_at
}

/// The colour of the character at `at`, from the spans the highlighter gave.
fn colour_of(spans: &[(Range, crate::theme::Role)], at: usize, theme: &Theme) -> Color {
    // Spans are in order and do not overlap, so this could be a walk with a
    // pointer; a binary search is the same thing without having to thread the
    // pointer through every row.
    match spans.binary_search_by(|(range, _)| {
        if range.end() <= at {
            std::cmp::Ordering::Less
        } else if range.start() > at {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(at) => theme.role(spans[at].1),
        Err(_) => theme.foreground,
    }
}

fn severity_colour(severity: Severity, theme: &Theme) -> Color {
    match severity {
        Severity::Error => theme.error,
        Severity::Warning => theme.warning,
        Severity::Info => theme.info,
        Severity::Hint => theme.faint,
    }
}

/// The diagnostics that touch a stretch of a line.
fn diagnostics_on(doc: &Document, from: usize, to: usize) -> Vec<&Diagnostic> {
    doc.diagnostics
        .iter()
        .filter(|d| d.range.start() < to.max(from + 1) && d.range.end() >= from)
        .collect()
}

/// One line's worth of margin: which line, whether this is its first folded
/// row (only the first gets a number), and where on the screen it goes.
struct Gutter<'a> {
    line: usize,
    numbered: bool,
    screen: u16,
    cursor_lines: &'a [usize],
    /// Which pane this is, so a comparison of two panes knows which side of it
    /// this gutter is drawing.
    pane: usize,
    /// What the screen behind everything is: the theme's background, or the
    /// terminal's own where the settings say to leave it showing. The same
    /// colour the text beside it is drawn on — the margin is part of the page
    /// rather than a strip of terminal beside it.
    ground: Color,
}

fn draw_gutter(buf: &mut Buffer, app: &App, view: &View, doc: &Document, it: Gutter) {
    let Gutter {
        line,
        numbered,
        screen,
        cursor_lines,
        pane,
        ground,
    } = it;
    let theme = &app.theme;
    let frame = view.frame;
    let rule = rule_width(app.panes.len() as u16);
    let x = frame.x + rule;
    let width = view.gutter.saturating_sub(rule) as usize;
    if width == 0 {
        return;
    }
    let here = cursor_lines.contains(&line);

    // The worst thing wrong on this line, as a mark in the margin.
    let worst = doc
        .diagnostics
        .iter()
        .filter(|d| text::line_of(&doc.rope, d.range.start()) == line)
        .map(|d| d.severity)
        .min();

    // And what the debugger has to say about it: a dot where you asked it to
    // stop, an arrow where it actually has. The arrow wins, because a line
    // that is both is a line where the interesting fact is that the program
    // is standing on it.
    let stopped = app
        .stopped_at()
        .is_some_and(|(path, at)| at == line && doc.path.as_deref() == Some(path));
    // A hollow dot for one the adapter would not take. An adapter refuses a
    // breakpoint on a blank line or a comment, and moves one to the next line
    // that has code on it — and a breakpoint that looks exactly like a working
    // one while quietly being nothing is the most confusing thing a debugger
    // does. Hollow only once the adapter has actually been asked: before that
    // it is neither confirmed nor refused.
    let taken = doc
        .path
        .as_deref()
        .and_then(|path| app.debug.is_verified(path, line))
        .unwrap_or(true);
    let breakpoint = match (doc.has_breakpoint(line), taken) {
        (false, _) => None,
        (true, true) => Some((BREAKPOINT_MARK, theme.error)),
        (true, false) => Some((UNSET_BREAKPOINT_MARK, theme.muted)),
    };
    // A place you marked to come back to. Last of the three, because the
    // margin has one column for all of them and a bookmark is the only one of
    // the three that is not about a program that is running.
    let bookmark = doc
        .has_bookmark(line)
        .then_some((BOOKMARK_MARK, theme.info));
    let mark = match stopped {
        true => Some((STOPPED_MARK, theme.warning)),
        false => breakpoint.or(bookmark),
    };

    let numbers = app.config.line_numbers();
    let cursor_line = text::line_of(&doc.rope, view.sel.primary().head);
    let label = match (numbers, numbered) {
        (LineNumbers::Off, _) | (_, false) => String::new(),
        (LineNumbers::Absolute, _) => (line + 1).to_string(),
        (LineNumbers::Relative, _) => line.abs_diff(cursor_line).to_string(),
        (LineNumbers::Both, _) if line == cursor_line => (line + 1).to_string(),
        (LineNumbers::Both, _) => line.abs_diff(cursor_line).to_string(),
    };

    let style = Style::new()
        .bg(if here && theme.current_line != theme.background {
            theme.current_line
        } else {
            ground
        })
        .fg(if here {
            theme.gutter_current
        } else {
            theme.gutter
        });

    // ` 42 ` and then the marks, hard against the text: the bar, and then
    // whatever is wrong on this line. The bar says one of two things — how
    // this line differs from the last commit, or how it differs from the pane
    // beside it — and while two panes are being compared it is the second,
    // because that is what you asked to be shown.
    let comparing = app
        .diff
        .as_ref()
        .filter(|d| d.side_of(pane).is_some())
        .map(|d| d.mark(pane, line));
    let tracked = comparing.is_some() || app.git.tracking(doc.id);
    let reserved = 1 + usize::from(tracked);
    // The whole margin first, in the one style, so that the stripe on the
    // cursor's line reaches all of it — the columns the marks live in are part
    // of the line too, and a highlight with two gaps in it at the right-hand
    // end reads as a drawing bug rather than as a margin.
    for column in 0..width as u16 {
        if let Some(cell) = buf.cell_mut(Position::new(x + column, screen)) {
            cell.set_char(' ').set_style(style);
        }
    }
    let text = if width > reserved {
        format!("{label:>room$} ", room = width - reserved - 1)
    } else {
        String::new()
    };
    buf.set_stringn(x, screen, &text, width, style);
    let bar = match comparing {
        Some(mark) => mark,
        None => app.git.mark(doc.id, line),
    };
    if tracked
        && let Some(mark) = bar
        && let Some(cell) = buf.cell_mut(Position::new(frame.x + view.gutter - 2, screen))
    {
        cell.set_style(style.fg(git_colour(mark, theme)));
        cell.set_char(mark.glyph());
    }
    if let Some(severity) = worst
        && let Some(cell) = buf.cell_mut(Position::new(frame.x + view.gutter - 1, screen))
    {
        cell.set_style(style.fg(severity_colour(severity, theme)));
        cell.set_char(severity.mark().chars().next().unwrap_or('*'));
    }
    // Hard against the left edge of the margin, which is the blank column the
    // line number is padded with — so a breakpoint costs no room, and lands
    // where every other editor puts one and where the mouse expects to click.
    //
    // With the line numbers off there is only the one column and the
    // diagnostic already has it. The debugger's mark takes it: a red dot you
    // put there deliberately outranks a warning that arrived on its own, and
    // the alternative is a breakpoint you cannot see.
    if let Some((glyph, colour)) = mark
        && let Some(cell) = buf.cell_mut(Position::new(x, screen))
    {
        cell.set_style(style.fg(colour));
        cell.set_char(glyph);
    }
}

/// What a line with something folded onto it says at the end of it.
///
/// Written here and read by the click that lands on it, so that the two cannot
/// disagree about how wide it is — a button and the thing that answers it
/// being two different opinions is how a mark becomes unclickable.
pub fn fold_mark(hidden: usize) -> String {
    format!(" \u{22ef} {hidden} {} ", plural_lines(hidden))
}

/// `line` or `lines`, for the note on a folded row.
pub fn plural_lines(n: usize) -> &'static str {
    match n {
        1 => "line",
        _ => "lines",
    }
}

/// Where you asked the debugger to stop. A filled dot, because that is what
/// one is in every debugger anybody has used.
const BREAKPOINT_MARK: char = '\u{25cf}';
/// One the adapter would not take: a blank line, a comment, a file it is not
/// running. Hollow, and in the quiet colour, because it is a breakpoint that
/// is not going to happen.
const UNSET_BREAKPOINT_MARK: char = '\u{25cb}';
/// Where the program actually is.
const STOPPED_MARK: char = '\u{25b6}';
/// Somewhere you said you were coming back to. A diamond, so that it is not
/// mistaken for a breakpoint at a glance — the two share the column.
const BOOKMARK_MARK: char = '\u{25c6}';

/// What colour a line's history is drawn in. Green for new, blue for changed,
/// red for gone — the three every diff has used since diffs were in colour.
fn git_colour(mark: crate::git::Mark, theme: &Theme) -> Color {
    match mark {
        crate::git::Mark::Added => theme.added,
        crate::git::Mark::Changed => theme.changed,
        crate::git::Mark::Removed => theme.removed,
    }
}

fn draw_scrollbar(buf: &mut Buffer, view: &View, doc: &Document, theme: &Theme) {
    let frame = view.frame;
    if frame.width <= view.gutter + view.area.width || frame.height < 3 {
        return;
    }
    let lines = doc.len_lines().max(1);
    let height = frame.height as usize;
    // A file that fits has nothing to scroll, and a bar that fills its own
    // track is a wall rather than a bar.
    if lines <= height {
        return;
    }
    let x = frame.x + frame.width - 1;
    // How much of the file is showing, as a bar you can grab.
    let size = ((height * height) / lines.max(height)).clamp(1, height);
    let top = (view.top * height) / lines;
    let top = top.min(height - size);

    for row in 0..height {
        let inside = row >= top && row < top + size;
        if let Some(cell) = buf.cell_mut(Position::new(x, frame.y + row as u16)) {
            cell.set_char(if inside { '\u{2588}' } else { '\u{2502}' })
                .set_style(Style::new().fg(if inside { theme.faint } else { theme.chrome() }));
        }
    }
}

/// A thin line down the left of the pane with the focus, so that a screen with
/// four panes says which one the keyboard is talking to.
fn mark_focus(buf: &mut Buffer, view: &View, theme: &Theme, focused: bool) {
    let frame = view.frame;
    if frame.width == 0 {
        return;
    }
    for row in 0..frame.height {
        if let Some(cell) = buf.cell_mut(Position::new(frame.x, frame.y + row)) {
            cell.set_char('\u{2503}')
                .set_style(Style::new().fg(if focused {
                    theme.accent
                } else {
                    theme.chrome()
                }));
        }
    }
}

// ---------------------------------------------------------------------------
// The row at the top and the row at the bottom.
// ---------------------------------------------------------------------------

/// The row of tabs at the top.
///
/// Twenty files open is more than fits across a terminal, so the row is a
/// window onto a strip that is as wide as it needs to be: the whole strip is
/// drawn to one side and the part you can see is copied across. Switching file
/// scrolls it to wherever that file is, the wheel scrolls it by hand, and the
/// ‹ › at the ends say there is more and take you there.
fn draw_tabs(frame: &mut Frame, app: &mut App, area: Rect, ground: Color) {
    let theme = app.theme;
    let chrome = theme.chrome();
    let plain = Style::new().bg(chrome).fg(theme.muted);
    frame.buffer_mut().set_style(area, plain);

    // What is going on at the far right: the language servers, so that "why
    // are there no completions" has an answer on the screen. It takes its room
    // off the end of the strip rather than being drawn over it, or a tab and a
    // server would fight for the same columns.
    let busy = app.lsp.all().iter().find_map(|server| {
        server
            .busy_with()
            .map(|what| format!(" {} {what} ", server.name))
    });
    let busy = busy.filter(|said| area.width > text::str_width(said) as u16 + 12);
    let aside = busy.as_ref().map_or(0, |said| text::str_width(said) as u16);
    let window = area.width - aside;
    if window < 4 {
        app.tab_hits.clear();
        app.tab_nudges.clear();
        return;
    }

    // Every tab, at its place along a strip as wide as all of them together.
    // ` name • ` — the dot is the close cross, and the mark that says what
    // state the file is in, because they are never both wanted at once and one
    // column is one column.
    let here = app.view().doc;
    // What is docked is not a tab. A sidebar is part of the editor's shape,
    // and a row across the top offering to switch to the thing already down
    // the left — and to close it with a cross that is not how you close it —
    // is a row saying something untrue.
    let docked: Vec<DocId> = app
        .panes
        .iter()
        .filter(|pane| pane.dock.is_some())
        .map(|pane| pane.doc)
        .collect();
    let mut tabs = Vec::new();
    let mut total = 0u16;
    for doc in app.docs() {
        if docked.contains(&doc.id) {
            continue;
        }
        let label = format!(" {} ", doc.name);
        let width = (text::str_width(&label) + 2) as u16;
        tabs.push((doc.id, label, tab_state(doc), total, width));
        total = total.saturating_add(width);
    }

    // Where along the strip to look. Whatever it was, brought back inside what
    // there is to see, and then moved far enough that the file you are looking
    // at is on the screen — switching to a file has to show you its tab.
    let furthest = total.saturating_sub(window);
    let mut scroll = app.tab_scroll.min(furthest);
    if let Some((_, _, _, at, width)) = tabs.iter().find(|(id, ..)| *id == here) {
        if *at < scroll {
            scroll = *at;
        } else if at + width > scroll + window {
            scroll = (at + width) - window;
        }
    }
    let scroll = scroll.min(furthest);
    app.tab_scroll = scroll;

    // Drawn whole, then the part that fits is copied across. Clipping each tab
    // as it is drawn would mean every string, every style and every hit box
    // knowing about the edges; this way only the copy does.
    let mut strip = Buffer::empty(Rect::new(0, 0, total.max(1), 1));
    strip.set_style(strip.area, plain);
    let carried = app.dragging_tab();
    for (id, label, state, at, width) in &tabs {
        // A tab being carried is drawn as picked up, in the accent colour: the
        // row is reordering itself under the pointer, and which one is doing
        // the moving should not be something you have to work out.
        let mut style = if carried == Some(*id) {
            Style::new()
                .bg(theme.accent)
                .fg(theme.on_accent)
                .add_modifier(Modifier::BOLD)
        } else if *id == here {
            Style::new()
                .bg(theme.background)
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD)
        } else {
            plain
        };
        // A file with an error in it says so in the name, so that a mistake in
        // a file you are not looking at is still on the screen. Not while it
        // is being carried, where the colour is saying something else.
        if let Some(worst) = state.worst
            && carried != Some(*id)
        {
            style = style.fg(severity_colour(worst, &theme));
        }
        strip.set_stringn(*at, 0, label, (width - 2) as usize, style);
        let (mark, colour) = state.mark(&theme);
        strip.set_stringn(at + width - 2, 0, format!("{mark} "), 2, style.fg(colour));
    }

    let buf = frame.buffer_mut();
    for x in 0..window {
        let Some(from) = strip.cell(Position::new(scroll + x, 0)) else {
            break;
        };
        let from = from.clone();
        if let Some(cell) = buf.cell_mut(Position::new(area.x + x, area.y)) {
            *cell = from;
        }
    }
    if let Some(said) = &busy {
        buf.set_stringn(
            area.x + window,
            area.y,
            said,
            aside as usize,
            plain.fg(theme.info),
        );
    }

    // What is under each spot, in the screen's own columns rather than the
    // strip's, so a click is answered by what is actually on the screen.
    let seen = |at: u16, width: u16| -> Option<Rect> {
        let from = at.max(scroll);
        let to = (at + width).min(scroll + window);
        (to > from).then(|| Rect::new(area.x + from - scroll, area.y, to - from, 1))
    };
    app.tab_hits = tabs
        .iter()
        .flat_map(|(id, _, _, at, width)| {
            [
                seen(*at, width - 2).map(|rect| (rect, *id, false)),
                seen(at + width - 2, 1).map(|rect| (rect, *id, true)),
            ]
        })
        .flatten()
        .collect();

    // And the ends, where there is more row than there is room. Each takes one
    // column back off the tab under it, and answers a click first.
    app.tab_nudges.clear();
    let starts = || tabs.iter().map(|(_, _, _, at, _)| *at);
    if scroll > 0 {
        let back = starts().filter(|at| *at < scroll).next_back().unwrap_or(0);
        arrow(buf, area.x, area.y, '\u{2039}', theme);
        app.tab_nudges.push((Rect::new(area.x, area.y, 1, 1), back));
    }
    if scroll < furthest {
        let on = starts()
            .find(|at| *at > scroll)
            .unwrap_or(furthest)
            .min(furthest);
        let x = area.x + window - 1;
        arrow(buf, x, area.y, '\u{203a}', theme);
        app.tab_nudges.push((Rect::new(x, area.y, 1, 1), on));
    }
    let _ = ground;
}

/// What a tab has to say about its file beyond its name: whether it is worth
/// looking at, and what the one column at its right edge should be.
struct TabState {
    modified: bool,
    on_disk: crate::doc::OnDisk,
    /// The worst thing a language server has said about it, if anything.
    worst: Option<Severity>,
}

fn tab_state(doc: &Document) -> TabState {
    TabState {
        modified: doc.is_modified(),
        on_disk: doc.on_disk,
        // Only the two that mean something is wrong. A hint colouring a tab
        // would leave half the row lit up and say nothing.
        worst: doc
            .diagnostics
            .iter()
            .map(|d| d.severity)
            .filter(|s| matches!(s, Severity::Error | Severity::Warning))
            .min(),
    }
}

impl TabState {
    /// The one column at the right of a tab, which is also its close cross.
    ///
    /// Most urgent first: a file that is not there any more, then one that has
    /// been written behind your back, then one with changes of yours in it,
    /// and otherwise the cross.
    fn mark(&self, theme: &Theme) -> (char, Color) {
        match (self.on_disk, self.modified) {
            (crate::doc::OnDisk::Gone, _) => ('\u{0021}', theme.error),
            (crate::doc::OnDisk::Changed, _) => ('\u{2260}', theme.error),
            (_, true) => ('\u{25cf}', theme.warning),
            _ => ('\u{00d7}', theme.faint),
        }
    }
}

/// One of the ‹ › at the ends of the tab row: there is more this way.
fn arrow(buf: &mut Buffer, x: u16, y: u16, mark: char, theme: Theme) {
    if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
        cell.set_char(mark).set_style(
            Style::new()
                .bg(theme.chrome())
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    }
}

fn draw_status(frame: &mut Frame, app: &mut App, area: Rect, ground: Color) {
    let theme = app.theme;
    let doc = app.here();
    let view = app.view();

    let at = view.sel.primary().head;
    let line = text::line_of(&doc.rope, at) + 1;
    let column = text::visual_column(&doc.rope, at, app.config.tab_width()) + 1;
    let cursors = view.sel.len();
    let selected: usize = view.sel.ranges().iter().map(Range::len).sum();

    let language = crate::lang::get(doc.language).name.clone();
    let errors = doc
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = doc
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();

    // What the cursor is standing on, if anybody has said anything about it.
    // This is the line that turns a red squiggle into something you can act on
    // without reaching for a key.
    let under = doc
        .diagnostics
        .iter()
        .filter(|d| d.range.contains(at) || d.range.start() == at)
        .min_by_key(|d| d.severity)
        .map(|d| {
            (
                d.severity,
                d.message.lines().next().unwrap_or("").to_string(),
            )
        });

    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().bg(theme.chrome()).fg(theme.muted));

    // Left: what is being said, or what is wrong under the cursor.
    let (left, left_style) = if app.status.showing() {
        (
            app.status.text.clone(),
            Style::new().fg(match app.status.tone {
                Tone::Plain => theme.foreground,
                Tone::Good => theme.success,
                Tone::Bad => theme.error,
            }),
        )
    } else if let Some((severity, message)) = under {
        (message, Style::new().fg(severity_colour(severity, &theme)))
    } else {
        let name = doc
            .path
            .as_ref()
            .map(|p| crate::app::short(p, &app.project))
            .unwrap_or_else(|| doc.name.clone());
        (
            format!("{name}{}", if doc.is_modified() { " •" } else { "" }),
            Style::new().fg(theme.muted),
        )
    };

    // Right: the facts, each one a button.
    let mut chips: Vec<(String, Color, Cmd)> = Vec::new();
    if errors + warnings > 0 {
        let mut said = String::new();
        if errors > 0 {
            said.push_str(&format!("{errors} error{}", plural(errors)));
        }
        if warnings > 0 {
            if !said.is_empty() {
                said.push_str(", ");
            }
            said.push_str(&format!("{warnings} warning{}", plural(warnings)));
        }
        chips.push((
            said,
            if errors > 0 {
                theme.error
            } else {
                theme.warning
            },
            Cmd::DIAGNOSTICS,
        ));
    }
    if cursors > 1 {
        chips.push((
            format!("{cursors} cursors"),
            theme.accent,
            Cmd::COLLAPSE_CURSORS,
        ));
    } else if selected > 0 {
        chips.push((format!("{selected} selected"), theme.info, Cmd::SELECT_LINE));
    }
    // That the recorder is running. Near the front, because it is a mode —
    // the one thing textfold has that is — and a mode you cannot see is a
    // mode you are in by accident.
    if app.is_recording() {
        chips.push(("recording".into(), theme.error, Cmd::RECORD_MACRO));
    }
    // Why it cannot be written, where there is a reason worth giving. A file
    // that is not text is read-only for a different reason than a file whose
    // permissions say so, and "read-only" on its own would send somebody to
    // `chmod` to fix something `chmod` has nothing to do with.
    if let Some(why) = doc.bytes.label() {
        chips.push((format!("{why}, read-only"), theme.warning, Cmd::ABOUT));
    } else if doc.read_only {
        chips.push(("read-only".into(), theme.warning, Cmd::ABOUT));
    }
    // What the debugger is doing, and a click that shows the panel where the
    // rest of it is. Near the left of the chips because while you are
    // debugging it is the most important thing on the bar.
    if let Some(session) = app.debug.session() {
        let colour = match &session.state {
            crate::dap::State::Stopped(_) => theme.warning,
            crate::dap::State::Ended(_) => theme.muted,
            _ => theme.success,
        };
        let said = match &session.state {
            crate::dap::State::Stopped(why) => format!("{} {why}", session.what),
            state => format!("{} {}", session.what, state.label()),
        };
        chips.push((said, colour, Cmd::DEBUG_PANEL));
    }
    // What can be done about the problem under the cursor, in the words the
    // server used for it: `Import 'List' (java.util)`, and a key to press.
    // This is the difference between a red squiggle and a red squiggle you can
    // do something about without knowing there was anything to try.
    if let Some(fixes) = app.fixes.as_ref().filter(|f| f.doc == doc.id)
        && let Some(title) = fixes.headline()
    {
        let key = app
            .keys
            .shortcut(Cmd::FIX_IT)
            .unwrap_or_else(|| "Alt-i".into());
        // Long enough to recognise the fix, short enough to leave the line
        // and column at the end of the bar where they always are.
        let title = text::truncate(title, 42);
        let said = match fixes.len() {
            1 => format!("{key}: {title}"),
            n => format!("{key}: {title} (+{})", n - 1),
        };
        chips.push((said, theme.success, Cmd::FIX_IT));
    }

    // That two panes are being compared, and how far apart they are. Clicking
    // it steps to the next difference, which while a comparison is on is what
    // the next-change key does too.
    if let Some(diff) = &app.diff {
        let said = match diff.differing() {
            0 => "same".to_string(),
            1 => "1 line differs".to_string(),
            n => format!("{n} lines differ"),
        };
        chips.push((
            said,
            if diff.same() {
                theme.muted
            } else {
                theme.changed
            },
            Cmd::NEXT_CHANGE,
        ));
    }

    // Which branch, and how much of this file is not in it. Clicking it steps
    // to the next of your own changes, which is the thing you want from a
    // branch name often enough to be worth the click.
    if let Some(head) = app.git.head() {
        let changed = app.git.changed_lines(doc.id);
        let said = match changed {
            0 => head.to_string(),
            n => format!("{head} +{n}"),
        };
        chips.push((
            said,
            if changed > 0 {
                theme.changed
            } else {
                theme.muted
            },
            Cmd::NEXT_CHANGE,
        ));
    }
    if let Some(why) = doc.colours_off {
        chips.push((format!("no colours: {why}"), theme.faint, Cmd::ABOUT));
    }
    chips.push((language, theme.muted, Cmd::SET_LANGUAGE));
    chips.push((format!("{line}:{column}"), theme.muted, Cmd::GOTO_LINE));
    chips.push((
        app.config.theme_name().to_string(),
        theme.muted,
        Cmd::THEME_PICKER,
    ));

    let mut hits = Vec::new();
    let mut right = area.x + area.width;
    for (text, colour, cmd) in chips.iter().rev() {
        let shown = format!(" {text} ");
        let width = text::str_width(&shown) as u16;
        if right < area.x + width + 8 {
            break;
        }
        right -= width;
        buf.set_stringn(
            right,
            area.y,
            &shown,
            width as usize,
            Style::new().bg(theme.chrome()).fg(*colour),
        );
        hits.push((Rect::new(right, area.y, width, 1), *cmd));
    }

    let room = right.saturating_sub(area.x + 1) as usize;
    buf.set_stringn(
        area.x + 1,
        area.y,
        text::truncate(&left, room),
        room,
        left_style.bg(theme.chrome()),
    );
    let _ = ground;
    app.status_hits = hits;
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ---------------------------------------------------------------------------
// What floats over the top.
// ---------------------------------------------------------------------------

use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear};

fn draw_floating(frame: &mut Frame, app: &mut App, ground: Color) -> Option<Position> {
    // Beside the cursor first, then over the middle: a suggestion belongs
    // where you are typing, and a list belongs where you can read it.
    let at = cursor_screen(app);
    if let Some(at) = at {
        draw_signature(frame, app, at, ground);
        draw_completion(frame, app, at, ground);
        draw_hover(frame, app, at, ground);
    }

    match &app.overlay {
        Overlay::Picker(_) => draw_picker(frame, app, ground),
        Overlay::Prompt(_) => draw_prompt(frame, app),
        Overlay::Confirm(_) => {
            draw_confirm(frame, app, ground);
            None
        }
        Overlay::Help(_) => {
            draw_help(frame, app, ground);
            None
        }
        Overlay::Menu(_) => {
            draw_menu(frame, app, ground);
            None
        }
        Overlay::None => None,
    }
}

/// Where the cursor is on the screen, if it is on the screen at all.
fn cursor_screen(app: &App) -> Option<Position> {
    screen_position_of(app, app.view().sel.primary().head)
}

/// The same, as a plain pair, for anything that wants a place to hang a box
/// off rather than a place to put the caret.
pub fn cursor_cell(app: &App) -> Option<(u16, u16)> {
    cursor_screen(app).map(|at| (at.x, at.y))
}

/// Where a place in the file is on the screen. `None` for one scrolled out of
/// sight, which is what tells a popup about that place not to be drawn.
fn screen_position_of(app: &App, at: usize) -> Option<Position> {
    let view = app.view();
    let doc = app.doc(view.doc)?;
    let folds = view.folded(&doc.rope);
    let hints = doc.inlay_columns();
    let layout = Layout {
        rope: &doc.rope,
        hints: &hints,
        width: view.area.width.max(1) as usize,
        tab_width: app.config.tab_width(),
        wrap: view.wrap,
        folds: &folds,
    };
    let at = at.min(doc.len_chars());
    let row = view::screen_row(view, &layout, at)?;
    let (_, column) = layout.place(at);
    let x = view.area.x as usize + column.saturating_sub(if view.wrap { 0 } else { view.left });
    (x < (view.area.x + view.area.width) as usize)
        .then(|| Position::new(x as u16, view.area.y + row as u16))
}

/// A box of `width` by `height` that sits beside `at` without falling off any
/// edge: below where there is room, above where there is not.
fn beside(screen: Rect, at: Position, width: u16, height: u16) -> Rect {
    let width = width.min(screen.width);
    let height = height.min(screen.height.saturating_sub(2));
    let below = at.y + 1;
    let y = if below + height <= screen.y + screen.height - 1 {
        below
    } else {
        at.y.saturating_sub(height).max(screen.y)
    };
    let x = at.x.min(screen.x + screen.width.saturating_sub(width));
    Rect::new(x, y, width, height)
}

fn box_style(theme: &Theme, ground: Color) -> Style {
    Style::new().bg(if theme.background == Color::Reset {
        ground
    } else {
        theme.background
    })
}

/// What goes at the right-hand end of a row in the completion list: the type
/// of the thing, or where the name comes from, with a plus in front of it when
/// taking this one writes a line at the top of the file as well. An import
/// that appears out of nowhere is a surprise; one the list said was coming is
/// a feature.
fn right_of(item: &crate::app::Suggestion) -> Option<String> {
    match (item.also.is_empty(), &item.detail) {
        (true, detail) => detail.clone(),
        (false, Some(detail)) => Some(format!("+ {detail}")),
        (false, None) => Some("+ import".to_string()),
    }
}

/// One line of the completion list, as it will be drawn.
struct Row {
    label: String,
    suffix: Option<String>,
    detail: Option<String>,
    kind: &'static str,
    /// The colour the kind is drawn in: the one that kind of thing has in the
    /// file. See [`crate::app::Suggestion::role`].
    role: crate::theme::Role,
}

fn draw_completion(frame: &mut Frame, app: &mut App, at: Position, ground: Color) {
    let Some(completion) = &app.completion else {
        return;
    };
    if completion.is_empty() {
        return;
    }
    let theme = app.theme;
    let screen = app.screen;

    let rows = completion.len().min(10) as u16;
    let widest = completion
        .visible()
        .take(40)
        .map(|item| {
            text::str_width(&item.label)
                + item.suffix.as_deref().map(|s| text::str_width(s) + 1).unwrap_or(0)
                + right_of(item).as_deref().map(text::str_width).unwrap_or(0)
                // The kind gets a column of its own up to ten wide, whatever
                // this particular word is; asking for only as much as the
                // word needs is how the note at the right edge ends up with
                // nowhere to go and is dropped.
                + text::str_width(item.kind).max(10)
                + 6
        })
        .max()
        .unwrap_or(20);
    let width = (widest as u16)
        .clamp(20, screen.width.saturating_sub(4))
        .min(72);
    // The list lines up under the word being completed rather than under the
    // cursor, so the text you are matching against sits above its matches.
    let start_column = at.x.saturating_sub(
        app.doc(app.view().doc)
            .map(|doc| {
                doc.slice(Range::new(
                    completion.start.min(doc.len_chars()),
                    app.view().sel.primary().head.min(doc.len_chars()),
                ))
                .chars()
                .count() as u16
            })
            .unwrap_or(0),
    );
    let area = beside(screen, Position::new(start_column, at.y), width, rows);

    let (top, cursor) = (completion.top, completion.cursor);
    let items: Vec<Row> = completion
        .visible()
        .skip(top)
        .take(rows as usize)
        .map(|item| Row {
            label: item.label.clone(),
            suffix: item.suffix.clone(),
            detail: right_of(item),
            kind: item.kind,
            role: item.role,
        })
        .collect();
    let about = completion.selected().and_then(|item| item.about.clone());

    frame.render_widget(Clear, area);
    let buf = frame.buffer_mut();
    buf.set_style(area, box_style(&theme, ground).fg(theme.foreground));

    for (row, Row {
        label,
        suffix,
        detail,
        kind,
        role,
    }) in items.iter().enumerate()
    {
        let y = area.y + row as u16;
        let chosen = top + row == cursor;
        let style = if chosen {
            Style::new().bg(theme.selection).fg(theme.foreground)
        } else {
            box_style(&theme, ground).fg(theme.foreground)
        };
        buf.set_style(Rect::new(area.x, y, area.width, 1), style);

        // A space either side of the kind, always — `keyword` is seven
        // characters and would otherwise run straight into the label.
        let kind_width = 10.min((area.width as usize / 3).max(4));
        let kind = text::truncate(kind, kind_width.saturating_sub(2));
        // In the colour the thing itself has in the file, so that the shape
        // of the list — three methods, then the fields — is legible before
        // any of it has been read. See `completion_role`.
        buf.set_stringn(
            area.x,
            y,
            format!(" {kind:<w$}", w = kind_width.saturating_sub(1)),
            kind_width,
            style.fg(theme.role(*role)),
        );
        let room = area.width as usize - kind_width - 1;
        let mut label_width = text::str_width(label).min(room);
        buf.set_stringn(
            area.x + kind_width as u16,
            y,
            text::truncate(label, room),
            room,
            style.fg(if chosen {
                theme.accent
            } else {
                theme.foreground
            }),
        );
        // The rest of the name, against the name and dimmer than it: the
        // arguments of a function, or the `(use std::collections::HashMap)`
        // that says where this one comes from. It is not part of what you are
        // matching against, and it does not read as though it were.
        if let Some(suffix) = suffix {
            let left = room.saturating_sub(label_width);
            if left > 3 {
                // A space between, which LSP says not to put there. Servers
                // send `(use std::collections::HashMap)` and `(x: i32)`
                // through the same field, and the first of those run against
                // the name is a word nobody can read.
                let shown = format!(" {}", text::truncate(suffix, left - 1));
                buf.set_stringn(
                    area.x + kind_width as u16 + label_width as u16,
                    y,
                    &shown,
                    left,
                    style.fg(theme.muted),
                );
                label_width += text::str_width(&shown);
            }
        }
        // The type, right up against the right edge, dimmed — it is there to
        // be glanced at, not read.
        if let Some(detail) = detail {
            let left = room.saturating_sub(label_width + 2);
            if left > 4 {
                let shown = text::truncate(detail, left);
                let width = text::str_width(&shown) as u16;
                buf.set_stringn(
                    area.x + area.width - width - 1,
                    y,
                    &shown,
                    width as usize,
                    style.fg(theme.faint),
                );
            }
        }
    }

    // One line about the chosen one, on the far side of the list from the
    // cursor: under the list where the list is under the cursor, and over it
    // where the list had to go over. Always under it would lay a line of
    // documentation straight across the line you are typing, which is the one
    // line on the screen that has to stay readable while you type it.
    if let Some(about) = about.filter(|a| !a.is_empty()) {
        let over = area.y + area.height <= at.y;
        let y = if over {
            area.y.checked_sub(1)
        } else {
            Some(area.y + area.height)
        };
        if let Some(y) = y.filter(|y| *y >= screen.y && *y < screen.y + screen.height - 1) {
            let shown = format!(" {} ", text::truncate(&about, area.width as usize - 2));
            frame.render_widget(Clear, Rect::new(area.x, y, area.width, 1));
            frame.buffer_mut().set_stringn(
                area.x,
                y,
                &shown,
                area.width as usize,
                box_style(&theme, ground).fg(theme.muted),
            );
        }
    }

    if let Some(completion) = &mut app.completion {
        completion.area = area;
    }
}

fn draw_hover(frame: &mut Frame, app: &mut App, at: Position, ground: Color) {
    let Some(hover) = &app.hover else { return };
    if app.completion.is_some() {
        // One box at a time. The suggestion is the one you are working in.
        return;
    }
    // Beside what it is about, which for a hover started with the mouse is
    // not where the cursor is.
    let at = screen_position_of(app, hover.at).unwrap_or(at);
    let theme = app.theme;
    let screen = app.screen;
    // Wide enough to read, no wider than the screen, and no wider than a
    // comfortable line of prose. Worked out before the text is looked at,
    // because it is what the text is folded to: a documentation box that ran
    // off the side and could only be scrolled downwards would be showing you
    // the first half of every sentence in it.
    let room = screen.width.saturating_sub(4).min(84);
    let Some(hover) = &mut app.hover else { return };
    hover.fold_to((room.saturating_sub(2)).max(8) as usize);
    let hover = &*hover;
    let widest = hover
        .lines
        .iter()
        .map(|line| text::str_width(&line.text))
        .max()
        .unwrap_or(20);
    let width = ((widest + 2) as u16).min(room).max(24.min(room));
    // A glance gets fourteen rows; something you have asked to read gets as
    // much of the screen as there is, because scrolling a paragraph at a time
    // through a fourteen-row window is the thing that makes documentation in
    // a terminal not worth reading.
    let most = if hover.focused {
        screen.height.saturating_sub(6).max(6) as usize
    } else {
        14
    };
    // How much there is to show, not how much is left below where you have
    // scrolled to. A box that shrinks as you read down it is a box that pulls
    // itself out from under you, and one that ends up a single row is no
    // longer showing you anything.
    let rows = hover.lines.len().clamp(1, most) as u16;
    let area = beside(screen, at, width, rows + 2);
    // Never scrolled further than the last line on the bottom row. The
    // keyboard and the wheel both clamp as they go, but the box can also grow
    // — the screen is resized, or a glance becomes something you asked to read
    // — and then a scroll that was at the end is past it.
    let shown = area.height.saturating_sub(2) as usize;
    let scroll = hover.scroll.min(hover.lines.len().saturating_sub(shown));

    frame.render_widget(Clear, area);
    let border = if hover.focused {
        theme.accent
    } else {
        theme.faint
    };
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .style(box_style(&theme, ground));
    // How far down a long one you are, and what the keys do, along the bottom
    // edge — where a border is already being drawn and no row of text is lost
    // to saying it.
    let more = hover.lines.len().saturating_sub(scroll + shown);
    // A name under the pointer is the thing you are about to do, so it says
    // what would happen and everything else waits.
    let under = hover
        .pointer
        .and_then(|(column, row)| hover.link_at(column, row))
        .map(|link| link.word);
    let dragged = hover
        .select
        .is_some_and(|(anchor, head)| anchor != head);
    let hint = match (&under, dragged, hover.focused, more) {
        (Some(word), ..) => Some(format!(" Ctrl-click to go to {word} ")),
        (None, true, ..) => Some(" Ctrl-C copies what you dragged over ".to_string()),
        (None, false, true, 0) => Some(" drag to select · Enter opens a tab ".to_string()),
        (None, false, true, n) => Some(format!(" {n} more · Enter opens a tab ")),
        (None, false, false, 0) => None,
        (None, false, false, n) => Some(format!(" {n} more ")),
    };
    if let Some(hint) = hint.filter(|h| area.width as usize > text::str_width(h) + 2) {
        let colour = if under.is_some() || dragged {
            theme.accent
        } else {
            theme.faint
        };
        block = block.title_bottom(Line::from(hint).style(Style::new().fg(colour)));
    }
    let inside = block.inner(area);
    frame.render_widget(block, area);

    let link = hover
        .pointer
        .and_then(|(column, row)| hover.link_at(column, row));

    let buf = frame.buffer_mut();
    for (row, line) in hover
        .lines
        .iter()
        .skip(scroll)
        .take(inside.height as usize)
        .enumerate()
    {
        let y = inside.y + row as u16;
        if line.text == crate::app::RULE {
            buf.set_stringn(
                inside.x,
                y,
                crate::app::RULE.repeat(inside.width as usize),
                inside.width as usize,
                box_style(&theme, ground).fg(theme.chrome()),
            );
            continue;
        }
        if line.spans.is_empty() {
            buf.set_stringn(
                inside.x,
                y,
                text::truncate(&line.text, inside.width as usize),
                inside.width as usize,
                box_style(&theme, ground).fg(theme.foreground),
            );
            continue;
        }
        // Code out of a fence, in the colours it would have in the editor.
        // Written a piece at a time because each piece has its own colour, and
        // the pieces between the coloured ones are ordinary text.
        let mut x = inside.x;
        let mut left = inside.width as usize;
        let mut at = 0;
        for (range, role) in line.spans.iter().chain(std::iter::once(&TAIL)) {
            let plain = line.text.get(at..range.start.min(line.text.len()));
            let coloured = line.text.get(range.clone());
            for (piece, colour) in [(plain, theme.foreground), (coloured, theme.role(*role))] {
                let Some(piece) = piece.filter(|p| !p.is_empty()) else {
                    continue;
                };
                if left == 0 {
                    break;
                }
                let shown = text::truncate(piece, left);
                buf.set_stringn(x, y, &shown, left, box_style(&theme, ground).fg(colour));
                let used = text::str_width(&shown).min(left);
                x += used as u16;
                left -= used;
            }
            at = range.end;
        }
    }

    // What has been dragged over, behind the words, in the colour selected
    // text has everywhere else in the editor.
    for row in 0..inside.height {
        let Some((from, to)) = hover.selected_on(scroll + row as usize) else {
            continue;
        };
        let y = inside.y + row;
        let from = inside.x + (from as u16).min(inside.width);
        let to = inside.x + (to as u16).min(inside.width);
        for x in from..to {
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                let style = cell.style();
                cell.set_style(style.bg(theme.selection));
            }
        }
    }

    // The name under the pointer, drawn the way a link is drawn everywhere:
    // underlined and in the colour that means "this does something". Done last
    // so that it sits over whatever colour the code underneath was given.
    if let Some(link) = &link {
        for x in link.from..link.to.min(inside.x + inside.width) {
            if let Some(cell) = buf.cell_mut(Position::new(x, link.row)) {
                let style = cell.style();
                cell.set_style(style.fg(theme.accent).add_modifier(Modifier::UNDERLINED));
            }
        }
    }

    if let Some(hover) = &mut app.hover {
        hover.area = inside;
        hover.outer = area;
        hover.scroll = scroll;
    }
}

/// Stands in for the span after the last one, so that the text past the end of
/// the colouring is written by the same loop rather than after it. Its range
/// is empty, so it draws nothing of its own.
static TAIL: (std::ops::Range<usize>, crate::theme::Role) =
    (usize::MAX..usize::MAX, crate::theme::Role::Variable);

fn draw_signature(frame: &mut Frame, app: &mut App, at: Position, ground: Color) {
    let Some(signature) = &app.signature else {
        return;
    };
    let theme = app.theme;
    let screen = app.screen;
    let Some(label) = signature.lines.first() else {
        return;
    };
    let shown = format!(
        " {} ",
        text::truncate(&label.text, screen.width as usize - 4)
    );
    let width = text::str_width(&shown) as u16;
    // Always above: the thing being typed is below, and covering it would
    // defeat the purpose.
    let y = at.y.saturating_sub(1);
    let x = at.x.min(screen.x + screen.width.saturating_sub(width));
    let area = Rect::new(x, y, width, 1);
    frame.render_widget(Clear, area);
    frame.buffer_mut().set_stringn(
        x,
        y,
        &shown,
        width as usize,
        box_style(&theme, ground).fg(theme.info),
    );
}

/// One row of a list, taken out of the picker so the drawing can hold it while
/// it writes into the screen buffer.
struct Shown {
    label: String,
    detail: Option<String>,
    tag: Option<String>,
    key: Option<String>,
    severity: Option<Severity>,
    matched: Vec<u32>,
}

/// The context menu: a short list where the pointer was.
fn draw_menu(frame: &mut Frame, app: &mut App, ground: Color) {
    let Overlay::Menu(menu) = &app.overlay else {
        return;
    };
    let theme = app.theme;
    let screen = app.screen;
    let width = menu.width();
    let height = (menu.len() as u16 + 2).min(screen.height.saturating_sub(1));
    let (x, y) = menu.anchor;
    let area = beside(screen, Position::new(x, y), width, height);

    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.faint))
        .style(box_style(&theme, ground));
    let inside = block.inner(area);
    frame.render_widget(block, area);

    // More rows than there is room for is rare, and silently dropping the ones
    // past the bottom edge is worse than rare: those rows cannot be pointed
    // at, so a command simply is not there on a short terminal. Scroll instead,
    // keeping the highlight in view, the way the fuzzy list already does.
    let shown = (inside.height as usize).max(1);
    let scroll = menu
        .scroll
        .min(menu.len().saturating_sub(shown))
        .min(menu.cursor)
        .max((menu.cursor + 1).saturating_sub(shown));

    let buf = frame.buffer_mut();
    let room = inside.width as usize;
    for (row, item) in menu.items.iter().enumerate().skip(scroll).take(shown).map(|(n, item)| (n - scroll, item)) {
        let y = inside.y + row as u16;
        if matches!(item.action, crate::menu::Action::Divide) {
            buf.set_stringn(
                inside.x,
                y,
                "\u{2500}".repeat(room),
                room,
                box_style(&theme, ground).fg(theme.faint),
            );
            continue;
        }
        let here = row + scroll == menu.cursor;
        let chosen = here && item.enabled;
        // The highlighted row is the accent colour behind it rather than a
        // marker beside it, which is what a menu looks like everywhere and
        // leaves the whole width for the words. A row you are pointing at but
        // cannot choose is lit more quietly, so that the pointer is never
        // somewhere the highlight is not, and never promises a click will do
        // something when it will not.
        let base = if chosen {
            Style::new().bg(theme.accent).fg(theme.on_accent)
        } else if here {
            Style::new().bg(theme.selection).fg(theme.faint)
        } else if item.enabled {
            box_style(&theme, ground).fg(theme.foreground)
        } else {
            box_style(&theme, ground).fg(theme.faint)
        };
        for x in inside.x..inside.x + inside.width {
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_char(' ').set_style(base);
            }
        }
        // The key on the right, where the eye can run down the column, and
        // only where the label leaves room for it.
        let mut left = room.saturating_sub(1);
        if let Some(key) = item.key.as_deref() {
            let width = text::str_width(key);
            if left > width + text::str_width(&item.label) + 3 {
                let at = inside.x + inside.width - width as u16 - 1;
                buf.set_stringn(
                    at,
                    y,
                    key,
                    width,
                    if chosen { base } else { base.fg(theme.faint) },
                );
                left -= width + 2;
            }
        }
        buf.set_stringn(
            inside.x + 1,
            y,
            text::truncate(&item.label, left.saturating_sub(1)),
            left.saturating_sub(1),
            base,
        );
    }

    if let Overlay::Menu(menu) = &mut app.overlay {
        menu.area = inside;
        menu.scroll = scroll;
    }
}

fn draw_picker(frame: &mut Frame, app: &mut App, ground: Color) -> Option<Position> {
    let Overlay::Picker(picker) = &app.overlay else {
        return None;
    };
    let theme = app.theme;
    let screen = app.screen;

    let width = (screen.width * 4 / 5).clamp(30, 110).min(screen.width - 2);
    let height = (screen.height * 3 / 4).clamp(6, 30).min(screen.height - 2);
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 3,
        width,
        height,
    );

    let title = format!(" {} ", picker.title());
    let count = if picker.total() > 0 {
        format!(" {} of {} ", picker.len(), picker.total())
    } else {
        String::new()
    };
    let hint = picker.kind.hint();

    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title(title)
        .title_style(Style::new().fg(theme.accent).add_modifier(Modifier::BOLD))
        .style(box_style(&theme, ground));
    let inside = block.inner(area);
    frame.render_widget(block, area);
    if inside.height < 3 {
        return None;
    }

    let buf = frame.buffer_mut();
    // The count, and the hint, on the frame itself.
    if !count.is_empty() && area.width > count.len() as u16 + 8 {
        buf.set_stringn(
            area.x + area.width - count.len() as u16 - 2,
            area.y,
            &count,
            count.len(),
            Style::new().bg(theme.background).fg(theme.faint),
        );
    }
    if !hint.is_empty() && area.width as usize > hint.len() + 6 {
        buf.set_stringn(
            area.x + 2,
            area.y + area.height - 1,
            format!(" {hint} "),
            hint.len() + 2,
            Style::new().bg(theme.background).fg(theme.faint),
        );
    }

    // The box you type in.
    let query_row = inside.y;
    buf.set_string(
        inside.x,
        query_row,
        "\u{203a} ",
        Style::new().fg(theme.accent),
    );
    buf.set_stringn(
        inside.x + 2,
        query_row,
        &picker.query,
        inside.width as usize - 2,
        Style::new().fg(theme.foreground),
    );
    let rule = inside.y + 1;
    for x in inside.x..inside.x + inside.width {
        buf.set_string(x, rule, "\u{2500}", Style::new().fg(theme.chrome()));
    }

    let list = Rect::new(inside.x, inside.y + 2, inside.width, inside.height - 2);
    // How tall the box is, is only known here. Tell the list, so that a
    // scroll made before this point against a guessed height is put right.
    if let Overlay::Picker(picker) = &mut app.overlay {
        picker.fit(list.height as usize);
    }
    let Overlay::Picker(picker) = &app.overlay else {
        return None;
    };
    // Copied out of the picker so that the drawing below can borrow the
    // buffer mutably without the list still being borrowed from `app`.
    let rows: Vec<Shown> = picker
        .visible()
        .skip(picker.top)
        .take(list.height as usize)
        .map(|(row, matched)| Shown {
            label: row.label.clone(),
            detail: row.detail.clone(),
            tag: row.tag.clone(),
            key: row.key.clone(),
            severity: row.severity,
            matched: matched.to_vec(),
        })
        .collect();
    let (top, cursor) = (picker.top, picker.cursor);

    if picker.is_empty() {
        let said = if picker.total() == 0 && picker.query.is_empty() {
            "looking\u{2026}"
        } else {
            "nothing matches"
        };
        buf.set_string(list.x + 1, list.y, said, Style::new().fg(theme.faint));
    }

    for (
        index,
        Shown {
            label,
            detail,
            tag,
            key,
            severity,
            matched,
        },
    ) in rows.iter().enumerate()
    {
        let y = list.y + index as u16;
        let chosen = top + index == cursor;
        let style = if chosen {
            Style::new().bg(theme.selection)
        } else {
            box_style(&theme, ground)
        };
        buf.set_style(Rect::new(list.x, y, list.width, 1), style);

        let mut x = list.x + 1;
        // The tag on the left, in the colour of what it means.
        if let Some(tag) = tag {
            let colour = severity
                .map(|s| severity_colour(s, &theme))
                .unwrap_or(theme.info);
            let shown = format!("{tag} ");
            let width = text::str_width(&shown).min(12) as u16;
            buf.set_stringn(x, y, &shown, width as usize, style.fg(colour));
            x += width;
        }

        // The key, on the right, so the palette teaches the keyboard.
        let mut right = list.x + list.width;
        if let Some(key) = key {
            let shown = format!(" {key} ");
            let width = text::str_width(&shown) as u16;
            if right > x + width + 8 {
                right -= width;
                buf.set_stringn(right, y, &shown, width as usize, style.fg(theme.warning));
            }
        }
        if let Some(detail) = detail {
            let room =
                (right.saturating_sub(x) as usize).saturating_sub(text::str_width(label) + 3);
            if room > 6 {
                let shown = text::truncate(detail, room);
                let width = text::str_width(&shown) as u16;
                right -= width + 1;
                buf.set_stringn(right, y, &shown, width as usize, style.fg(theme.faint));
            }
        }

        // The label, with the letters that matched lit up — the reason this
        // row is on the list, shown rather than left to be guessed at.
        let room = right.saturating_sub(x) as usize;
        let shown = text::truncate(label, room);
        let base = style.fg(theme.foreground);
        buf.set_stringn(x, y, &shown, room, base);
        for &position in matched {
            let column = x as usize + position as usize;
            if column < right as usize
                && let Some(cell) = buf.cell_mut(Position::new(column as u16, y))
            {
                cell.set_style(base.fg(theme.accent).add_modifier(Modifier::BOLD));
            }
        }
    }

    let caret = Position::new(
        inside.x + 2 + picker.caret.min(inside.width as usize) as u16,
        query_row,
    );
    if let Overlay::Picker(picker) = &mut app.overlay {
        picker.area = list;
    }
    Some(caret)
}

fn draw_prompt(frame: &mut Frame, app: &mut App) -> Option<Position> {
    let Overlay::Prompt(prompt) = &app.overlay else {
        return None;
    };
    let theme = app.theme;
    let screen = app.screen;
    let area = Rect::new(screen.x, screen.y + screen.height - 1, screen.width, 1);

    let label = format!(" {} ", prompt.label());
    // The prompt takes the whole row. Setting a style over what the status bar
    // drew would leave its words showing through underneath.
    frame.render_widget(Clear, area);
    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().bg(theme.chrome()).fg(theme.foreground));
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell_mut(Position::new(x, area.y)) {
            cell.set_char(' ');
        }
    }
    buf.set_string(
        area.x,
        area.y,
        &label,
        Style::new()
            .bg(theme.accent)
            .fg(theme.on_accent)
            .add_modifier(Modifier::BOLD),
    );
    let x = area.x + text::str_width(&label) as u16 + 1;
    let room = (area.width.saturating_sub(x - area.x)) as usize;
    buf.set_stringn(
        x,
        area.y,
        &prompt.input,
        room,
        Style::new().bg(theme.chrome()).fg(theme.foreground),
    );

    // For a search, how many there are — the thing you actually want to know
    // while typing one.
    if matches!(prompt.kind, PromptKind::Find | PromptKind::ReplaceFind) && !prompt.input.is_empty()
    {
        let (place, count) = app.match_place_of(&prompt.input);
        let said = format!(
            " {} ",
            match (place, count) {
                (_, 0) => "none".to_string(),
                (Some(at), n) => format!("{at} of {n}"),
                (None, 1) => "1 match".to_string(),
                (None, n) => format!("{n} matches"),
            }
        );
        let width = text::str_width(&said) as u16;
        if area.width > width + 20 {
            frame.buffer_mut().set_stringn(
                area.x + area.width - width,
                area.y,
                &said,
                width as usize,
                Style::new().bg(theme.chrome()).fg(if count == 0 {
                    theme.error
                } else {
                    theme.muted
                }),
            );
        }
    }

    Some(Position::new(x + prompt.caret.min(room) as u16, area.y))
}

fn draw_confirm(frame: &mut Frame, app: &mut App, ground: Color) {
    let Overlay::Confirm(confirm) = &app.overlay else {
        return;
    };
    let theme = app.theme;
    let screen = app.screen;

    let mut lines = vec![confirm.message.clone(), String::new()];
    for (key, what) in &confirm.choices {
        lines.push(format!("  {}  {what}", key.to_uppercase()));
    }
    let width = lines
        .iter()
        .map(|l| text::str_width(l))
        .max()
        .unwrap_or(30)
        .max(30) as u16
        + 4;
    let width = width.min(screen.width - 4);
    let height = lines.len() as u16 + 2;
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 3,
        width,
        height,
    );

    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.warning))
        .style(box_style(&theme, ground));
    let inside = block.inner(area);
    frame.render_widget(block, area);

    let buf = frame.buffer_mut();
    for (row, line) in lines.iter().enumerate() {
        let style = match row {
            0 => box_style(&theme, ground)
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
            _ => box_style(&theme, ground).fg(theme.muted),
        };
        buf.set_stringn(
            inside.x + 1,
            inside.y + row as u16,
            line,
            inside.width as usize,
            style,
        );
        // The letter to press, in colour.
        if row >= 2
            && let Some(cell) = buf.cell_mut(Position::new(inside.x + 3, inside.y + row as u16))
        {
            cell.set_style(
                box_style(&theme, ground)
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }
}

fn draw_help(frame: &mut Frame, app: &mut App, ground: Color) {
    let Overlay::Help(scroll) = &app.overlay else {
        return;
    };
    let theme = app.theme;
    let screen = app.screen;
    let area = Rect::new(
        screen.x + 1,
        screen.y + 1,
        screen.width.saturating_sub(2),
        screen.height.saturating_sub(2),
    );

    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title(format!(" textfold {} ", env!("CARGO_PKG_VERSION")))
        .title_style(Style::new().fg(theme.accent).add_modifier(Modifier::BOLD))
        .style(box_style(&theme, ground));
    let inside = block.inner(area);
    frame.render_widget(block, area);

    let lines = help_lines(app);
    let buf = frame.buffer_mut();
    // As many columns as there is room to read, because the list is long and
    // a terminal is wide. Room to read is the whole of it: a column narrower
    // than this cuts nearly every line in it short, and two columns of stubs
    // — `Swap this line with the on…` above `Swap this line with the on…` —
    // are not two bindings a person can tell apart. They read as the same
    // line printed twice, which is worse than scrolling.
    const NARROWEST_COLUMN: u16 = 60;
    let columns = (inside.width / NARROWEST_COLUMN).clamp(1, 3);
    let column_width = inside.width / columns;
    let rows = inside.height.saturating_sub(1) as usize;
    let shown = &lines[(*scroll).min(lines.len())..];

    for (index, line) in shown.iter().take(rows * columns as usize).enumerate() {
        let column = index / rows;
        let row = index % rows;
        let x = inside.x + column as u16 * column_width;
        let y = inside.y + row as u16;
        match line {
            HelpLine::Heading(text) => {
                buf.set_stringn(
                    x,
                    y,
                    text,
                    column_width as usize,
                    box_style(&theme, ground)
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                );
            }
            HelpLine::Item(key, what) => {
                let key_width = 16.min(column_width as usize / 2);
                buf.set_stringn(
                    x + 1,
                    y,
                    text::truncate(key, key_width),
                    key_width,
                    box_style(&theme, ground).fg(theme.warning),
                );
                buf.set_stringn(
                    x + 1 + key_width as u16,
                    y,
                    text::truncate(what, (column_width as usize).saturating_sub(key_width + 2)),
                    (column_width as usize).saturating_sub(key_width + 2),
                    box_style(&theme, ground).fg(theme.muted),
                );
            }
            HelpLine::Blank => {}
        }
    }

    let feet = " Esc closes this   ↑↓ scrolls   everything else is in the command palette (Alt-X) ";
    buf.set_stringn(
        inside.x,
        inside.y + inside.height - 1,
        feet,
        inside.width as usize,
        box_style(&theme, ground).fg(theme.faint),
    );
}

enum HelpLine {
    Heading(String),
    Item(String, String),
    Blank,
}

/// The help, built from the bindings actually in force — so a rebound key
/// shows up here rather than a lie about what textfold shipped with.
fn help_lines(app: &App) -> Vec<HelpLine> {
    use crate::cmd::Group;
    let mut lines = Vec::new();
    let groups = [
        (Group::File, "Files"),
        (Group::Edit, "Editing"),
        (Group::Move, "Moving about"),
        (Group::Select, "Selecting"),
        (Group::Search, "Finding"),
        (Group::Code, "Code"),
        (Group::View, "The view"),
        (Group::Tool, "Tools"),
        (Group::Help, "Help"),
    ];
    for (group, title) in groups {
        let mut items: Vec<HelpLine> = crate::cmd::all()
            .iter()
            .filter(|cmd| cmd.group() == group)
            .filter_map(|cmd| {
                let keys = app.keys.keys_for(*cmd);
                (!keys.is_empty()).then(|| {
                    HelpLine::Item(
                        keys.iter()
                            .take(2)
                            .map(crate::keys::Key::show)
                            .collect::<Vec<_>>()
                            .join(" / "),
                        cmd.about().to_string(),
                    )
                })
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        lines.push(HelpLine::Heading(title.to_string()));
        lines.append(&mut items);
        lines.push(HelpLine::Blank);
    }
    lines.push(HelpLine::Heading("The mouse".into()));
    for (what, does) in [
        ("Click", "put the cursor there"),
        ("Drag", "select"),
        ("Double click", "select the word, drag for more words"),
        ("Triple click", "select the line"),
        ("Click a line number", "select that line"),
        ("Ctrl-click", "go to the definition"),
        ("Alt-click", "another cursor there"),
        ("Right click", "what can be done here"),
        ("Wheel", "scroll the pane under the pointer"),
        ("Click a tab", "switch to it; the × closes it"),
        (
            "Wheel over the tabs",
            "walk along them when there are more than fit",
        ),
        ("Click a ‹ or ›", "the next tab that way"),
        (
            "Click the status bar",
            "the language, the position and the problems are buttons",
        ),
    ] {
        lines.push(HelpLine::Item(what.into(), does.into()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    use super::*;
    use crate::app::App;
    use crate::cmd::Cmd;
    use crate::config::Config;
    use crate::text::{Range, Selections};
    use ratatui::crossterm::event::{
        Event as TermEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    /// An editor with one document in it, drawn onto a buffer we can read
    /// back. The point of these tests is what actually reaches the screen:
    /// everything below here is a fact about pixels, not about state.
    fn screen(text: &str, then: impl FnOnce(&mut App)) -> (Buffer, App) {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.here_mut().rope = ropey::Rope::from_str(text);
        then(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("a terminal to draw on");
        terminal
            .draw(|frame| super::draw(frame, &mut app))
            .expect("drawn");
        (terminal.backend().buffer().clone(), app)
    }

    /// Every cell drawn in the given background, as (x, y).
    fn cells_on(buffer: &Buffer, bg: Color) -> Vec<(u16, u16)> {
        let area = buffer.area;
        let mut found = Vec::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if buffer[(x, y)].style().bg == Some(bg) {
                    found.push((x, y));
                }
            }
        }
        found
    }

    /// An editor with `count` scratch buffers open, drawn narrow enough that
    /// they cannot all be on the screen at once.
    fn many_tabs(count: usize, width: u16) -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        for _ in 1..count {
            app.run(Cmd::NEW);
        }
        app.screen = Rect::new(0, 0, width, 12);
        app
    }

    /// Put it on a screen `width` columns across, and hand back what is on the
    /// top row. The drawing is what settles where the tabs went, so a test
    /// about tabs has to draw.
    fn tab_row(app: &mut App, width: u16) -> String {
        let mut terminal =
            Terminal::new(TestBackend::new(width, 12)).expect("a terminal to draw on");
        terminal
            .draw(|frame| super::draw(frame, app))
            .expect("drawn");
        let buffer = terminal.backend().buffer();
        (0..width).map(|x| buffer[(x, 0)].symbol()).collect()
    }

    /// One press of the left button, the way it arrives from the terminal.
    fn click(app: &mut App, column: u16, row: u16) {
        app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })));
    }

    /// The left button held and moved, the way it arrives from the terminal.
    fn drag(app: &mut App, column: u16, row: u16) {
        app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })));
    }

    /// An editor with tabs of the names given, in that order.
    ///
    /// Names of different lengths on purpose: tabs are as wide as what is
    /// written on them, and dragging a narrow one onto a wide one is the case
    /// that a naive reordering gets wrong.
    fn tabs_named(names: &[&str], width: u16) -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        for (n, name) in names.iter().enumerate() {
            if n > 0 {
                app.run(Cmd::NEW);
            }
            app.here_mut().name = (*name).to_string();
        }
        app.screen = Rect::new(0, 0, width, 12);
        app
    }

    fn order(app: &App) -> Vec<String> {
        app.docs().iter().map(|d| d.name.clone()).collect()
    }

    /// Where a tab is on the screen, from what the drawing last worked out.
    fn tab_at(app: &App, name: &str) -> (u16, u16) {
        let id = app
            .docs()
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.id)
            .expect("no such tab");
        let mut span: Option<(u16, u16)> = None;
        for (area, seen, _) in &app.tab_hits {
            if *seen != id {
                continue;
            }
            span = Some(match span {
                Some((from, to)) => (from.min(area.x), to.max(area.x + area.width)),
                None => (area.x, area.x + area.width),
            });
        }
        span.expect("that tab is not on the screen")
    }

    /// Everything on one row, as text, for a test that cares what a row says.
    fn row_text(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn the_margin_is_drawn_on_the_same_background_as_the_text() {
        // The bug: the line numbers were painted with the *terminal's* default
        // background rather than the theme's, so the left of every pane was a
        // strip of whatever colour the terminal happened to be — visible in
        // any theme whose background is not the terminal's own, which is
        // all but one of the ones textfold ships.
        // A theme with a background of its own. The default is `terminal`,
        // whose whole point is that it names no background — testing against
        // that one would be testing that `Reset` is `Reset`.
        let (buffer, app) = screen("one\ntwo\nthree\n", |app| {
            app.theme = app.themes.by_name("dracula").expect("a shipped theme");
        });
        let ground = app.theme.background;
        assert_ne!(ground, Color::Reset, "this theme paints a background");
        let view = &app.panes[0];
        let gutter = view.gutter;
        assert!(gutter > 0, "there are line numbers to test");

        // Past the cursor's own line, which is striped on purpose — that one
        // is what the test below is about.
        for y in view.area.y + 1..view.area.y + 3 {
            for x in view.frame.x..view.frame.x + gutter {
                assert_eq!(
                    buffer[(x, y)].style().bg,
                    Some(ground),
                    "column {x} of row {y} is not the theme's background"
                );
            }
        }
    }

    #[test]
    fn the_stripe_on_the_cursor_line_reaches_across_the_margin_too() {
        // Half a highlight reads as a drawing bug rather than as a margin:
        // the columns the git bar and the problem marks live in are part of
        // the line the cursor is on, whether or not there is a mark in them.
        let (buffer, app) = screen("one\ntwo\nthree\n", |app| {
            app.theme = app.themes.by_name("dracula").expect("a shipped theme");
            app.view_mut().sel = Selections::single(Range::point(0));
        });
        let theme = &app.theme;
        if theme.current_line == theme.background {
            return; // A theme that does not stripe the cursor's line.
        }
        let view = &app.panes[0];
        for x in view.frame.x + rule_width(1)..view.frame.x + view.gutter {
            assert_eq!(
                buffer[(x, view.area.y)].style().bg,
                Some(theme.current_line),
                "column {x} of the cursor's line is not part of the stripe"
            );
        }
    }

    #[test]
    fn a_suggestion_says_which_module_it_would_import_from() {
        // The whole reason to look at the list is often that you know the
        // name and not the path to it, and a server that offers a name your
        // file has not imported sends the path along beside it.
        let (buffer, _app) = screen("HashMa", |app| {
            app.view_mut().sel = Selections::single(Range::point(6));
            app.suggest_for_test(
                6,
                true,
                serde_json::json!([{
                    "label": "HashMap",
                    "labelDetails": {
                        "detail": "(use std::collections::HashMap)",
                        "description": "HashMap<K, V>",
                    },
                }]),
            );
        });

        let rows: Vec<String> = (0..buffer.area.height).map(|y| row_text(&buffer, y)).collect();
        assert!(
            rows.iter()
                .any(|row| row.contains("HashMap (use std::collections::HashMap)")),
            "the list should say where the name comes from, got:\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn what_a_suggestion_is_keeps_off_the_line_you_are_typing() {
        // Near the bottom of the screen the list has to go above the cursor.
        // The line of documentation under it then has nowhere to be but on
        // the cursor's own row, which is the one row it must not cover.
        let text = "\n".repeat(9) + "HashMa";
        let (buffer, app) = screen(&text, |app| {
            let at = app.here().len_chars();
            app.view_mut().sel = Selections::single(Range::point(at));
            app.suggest_for_test(
                at,
                true,
                serde_json::json!([{
                    "label": "HashMap",
                    "documentation": "quadratic probing",
                }]),
            );
        });

        let at = super::cursor_screen(&app).expect("a cursor on the screen");
        let row = row_text(&buffer, at.y);
        assert!(
            row.contains("HashMa"),
            "the line being typed is still readable, got: {row:?}",
        );
        assert!(
            !row.contains("quadratic probing"),
            "and what the suggestion is has not been laid over it, got: {row:?}",
        );
        assert!(
            (0..buffer.area.height).any(|y| row_text(&buffer, y).contains("quadratic probing")),
            "but it is on the screen somewhere",
        );
    }

    /// The help at a given size, as the rows a person would read.
    fn help_rows(width: u16, height: u16) -> Vec<String> {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.run(Cmd::HELP);
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("a terminal to draw on");
        terminal
            .draw(|frame| super::draw(frame, &mut app))
            .expect("drawn");
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect()
    }


    #[test]
    fn the_help_never_shows_the_same_line_twice() {
        // Two columns in a terminal too narrow for two columns cuts every
        // description down to a stub, and the stubs collide: `Swap this line
        // with the one above` and `Swap this line with the one below` come out
        // as the same row, which reads as the help repeating itself.
        for width in [80u16, 90, 95, 100, 110, 120, 140, 180, 200, 240] {
            let rows = help_rows(width, 40);
            let mut said: Vec<&str> = rows
                .iter()
                .flat_map(|row| row.split("  "))
                .map(str::trim)
                .filter(|part| part.len() > 8 && part.chars().any(char::is_alphabetic))
                .collect();
            said.sort_unstable();
            let before = said.len();
            said.dedup();
            assert_eq!(
                before,
                said.len(),
                "the help says something twice at {width} columns wide",
            );
        }
    }

    #[test]
    fn the_help_takes_the_columns_a_wide_terminal_gives_it() {
        // The list is long, and reading it a screenful at a time on a screen
        // with room for all of it is the other way to get this wrong.
        let narrow = help_rows(80, 40);
        let wide = help_rows(200, 40);
        // How much of the help is on the screen at once, counted in the
        // things it has to say rather than in rows.
        let entries = |rows: &[String]| {
            rows.iter()
                .flat_map(|row| row.split("  "))
                .map(str::trim)
                .filter(|part| part.len() > 8 && part.chars().any(char::is_alphabetic))
                .count()
        };
        assert!(
            entries(&wide) > entries(&narrow) * 2,
            "three columns should show more than twice what one column does: \
             {} against {}",
            entries(&wide),
            entries(&narrow),
        );
    }

    #[test]
    fn a_suggestion_that_brings_an_import_says_so() {
        let (buffer, _app) = screen("HashMa", |app| {
            app.view_mut().sel = Selections::single(Range::point(6));
            app.suggest_for_test(
                6,
                true,
                serde_json::json!([{
                    "label": "HashMap",
                    "additionalTextEdits": [{
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 0 },
                        },
                        "newText": "use std::collections::HashMap;\n",
                    }],
                }]),
            );
        });

        let rows: Vec<String> = (0..buffer.area.height).map(|y| row_text(&buffer, y)).collect();
        assert!(
            rows.iter().any(|row| row.contains("+ import")),
            "taking it writes a line at the top of the file, and the list should say so, got:\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn a_tab_whose_file_has_changed_on_disk_says_so_rather_than_looking_saved() {
        let (buffer, _app) = screen("text\n", |app| {
            app.here_mut().on_disk = crate::doc::OnDisk::Changed;
        });
        assert!(
            row_text(&buffer, 0).contains('\u{2260}'),
            "{:?}",
            row_text(&buffer, 0)
        );
    }

    #[test]
    fn a_tab_whose_file_is_gone_says_something_louder() {
        let (buffer, _app) = screen("text\n", |app| {
            app.here_mut().on_disk = crate::doc::OnDisk::Gone;
        });
        let row = row_text(&buffer, 0);
        assert!(row.contains('!'), "{row:?}");
    }

    #[test]
    fn a_tab_with_an_error_in_it_is_drawn_in_the_error_colour() {
        let (buffer, app) = screen("text\n", |app| {
            let range = Range::new(0, 4);
            app.here_mut().diagnostics = vec![crate::doc::Diagnostic {
                range,
                severity: crate::doc::Severity::Error,
                message: "no".into(),
                source: None,
                code: None,
                data: None,
                told: crate::doc::Told::Server(0),
            }];
        });
        let coloured = (0..buffer.area.width)
            .filter(|x| buffer[(*x, 0)].style().fg == Some(app.theme.error))
            .count();
        assert!(coloured > 1, "only the mark was coloured, not the name");
    }

    #[test]
    fn the_underline_under_a_problem_is_only_coloured_where_the_terminal_has_it() {
        // Because asking a terminal that does not for `CSI 58 … m` does not
        // cost a colour, it costs the screen: the parameters get read one at a
        // time and the file goes dim, italic, and in places invisible.
        let with_a_problem = |app: &mut App| {
            app.here_mut().diagnostics = vec![crate::doc::Diagnostic {
                range: Range::new(0, 4),
                severity: crate::doc::Severity::Warning,
                message: "hm".into(),
                source: None,
                code: None,
                data: None,
                told: crate::doc::Told::Server(0),
            }];
        };
        let underlines = |buffer: &Buffer| {
            (0..buffer.area.width)
                .filter(|x| {
                    buffer[(*x, 1)]
                        .style()
                        .add_modifier
                        .contains(Modifier::UNDERLINED)
                })
                .count()
        };

        let coloured = |buffer: &Buffer, want: Color| {
            (0..buffer.area.width)
                .filter(|x| buffer[(*x, 1)].style().underline_color == Some(want))
                .count()
        };

        crate::term::set_underline_colour(true);
        let (buffer, app) = screen("text\n", with_a_problem);
        assert_eq!(underlines(&buffer), 4, "the problem was not underlined");
        assert_eq!(coloured(&buffer, app.theme.warning), 4);

        crate::term::set_underline_colour(false);
        let (buffer, app) = screen("text\n", with_a_problem);
        assert_eq!(
            underlines(&buffer),
            4,
            "the underline itself should still be there"
        );
        assert_eq!(
            coloured(&buffer, app.theme.warning),
            0,
            "a colour was asked for that this terminal would have mangled"
        );
    }

    #[test]
    fn a_context_menu_is_drawn_where_you_right_clicked() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.here_mut().rope = ropey::Rope::from_str("one two three\n");
        app.screen = Rect::new(0, 0, 60, 12);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("a terminal to draw on");
        terminal
            .draw(|frame| super::draw(frame, &mut app))
            .expect("drawn");

        app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 8,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })));
        terminal
            .draw(|frame| super::draw(frame, &mut app))
            .expect("drawn");

        let buffer = terminal.backend().buffer();
        let all: String = (0..buffer.area.height)
            .map(|y| row_text(buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("Copy"), "no menu on the screen:\n{all}");
        assert!(all.contains("Go to definition"), "{all}");
    }

    /// The rows of a drawn menu that are painted in the accent colour: the
    /// one the highlight is on.
    fn highlighted(buffer: &Buffer, accent: Color) -> Vec<u16> {
        (0..buffer.area.height)
            .filter(|y| (0..buffer.area.width).any(|x| buffer[(x, *y)].style().bg == Some(accent)))
            .collect()
    }

    /// An editor with a context menu open at (8, 6), and a terminal to draw it
    /// on again after moving the pointer about.
    fn menu_on_screen() -> (App, Terminal<TestBackend>) {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.here_mut().rope = ropey::Rope::from_str("one two three\n".repeat(20).as_str());
        app.screen = Rect::new(0, 0, 60, 24);
        let mut terminal =
            Terminal::new(TestBackend::new(60, 24)).expect("a terminal to draw on");
        terminal.draw(|frame| super::draw(frame, &mut app)).expect("drawn");
        app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 8,
            row: 6,
            modifiers: KeyModifiers::NONE,
        })));
        terminal.draw(|frame| super::draw(frame, &mut app)).expect("drawn");
        (app, terminal)
    }

    fn move_pointer(app: &mut App, column: u16, row: u16) {
        app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })));
    }

    #[test]
    fn a_menu_opens_with_its_first_row_highlighted() {
        let (app, terminal) = menu_on_screen();
        let lit = highlighted(terminal.backend().buffer(), app.theme.accent);
        assert_eq!(lit.len(), 1, "one row highlighted, got {lit:?}");
    }

    #[test]
    fn the_menu_highlight_follows_the_pointer() {
        let (mut app, mut terminal) = menu_on_screen();
        let accent = app.theme.accent;
        let first = highlighted(terminal.backend().buffer(), accent);
        let row = first[0];

        // Cut, then Copy: the row below, and one anything can do.
        move_pointer(&mut app, 12, row + 1);
        terminal.draw(|frame| super::draw(frame, &mut app)).expect("drawn");
        let now = highlighted(terminal.backend().buffer(), accent);
        assert_eq!(now, vec![row + 1], "was {first:?}");
    }

    #[test]
    fn pointing_at_a_row_that_can_do_nothing_does_not_light_it_up_as_though_it_could() {
        let (mut app, mut terminal) = menu_on_screen();
        let accent = app.theme.accent;
        let row = highlighted(terminal.backend().buffer(), accent)[0];

        // Cut, Copy, Paste, a divider, then Undo — which a buffer nobody has
        // typed in cannot do.
        move_pointer(&mut app, 12, row + 4);
        terminal.draw(|frame| super::draw(frame, &mut app)).expect("drawn");
        assert!(
            highlighted(terminal.backend().buffer(), accent).is_empty(),
            "an unavailable row was lit as though it were available"
        );
        // The highlight has still moved there, so a click knows where it is.
        let Overlay::Menu(menu) = &app.overlay else {
            panic!("the menu closed")
        };
        assert_eq!(menu.cursor, 4);
        assert!(menu.chosen().is_none(), "and choosing it does nothing");
    }

    #[test]
    fn the_menu_highlight_stays_put_over_a_divider() {
        let (mut app, mut terminal) = menu_on_screen();
        let accent = app.theme.accent;
        let row = highlighted(terminal.backend().buffer(), accent)[0];
        // Cut, Copy, Paste, then a divider.
        move_pointer(&mut app, 12, row + 3);
        terminal.draw(|frame| super::draw(frame, &mut app)).expect("drawn");
        assert_eq!(highlighted(terminal.backend().buffer(), accent), vec![row]);
    }

    /// An editor with a long hover open, and a terminal to draw it on.
    fn hover_on_screen(lines: usize) -> (App, Terminal<TestBackend>) {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.here_mut().rope = ropey::Rope::from_str("fn main() {}\n");
        app.screen = Rect::new(0, 0, 60, 24);
        // Backticks, because that is how a language server says "this word is
        // a name" and the only thing a hover offers to follow.
        let text: Vec<crate::app::DocLine> = (0..lines)
            .map(|n| crate::app::DocLine::prose(format!("line {n} about `Selections` here")))
            .collect();
        let mut popup = crate::app::Popup::new(text, 0);
        popup.focused = true;
        app.hover = Some(popup);
        let terminal = Terminal::new(TestBackend::new(60, 24)).expect("a terminal");
        (app, terminal)
    }

    #[test]
    fn a_dock_takes_its_room_off_the_edge_and_the_rest_share_what_is_left() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        let body = Rect::new(0, 1, 100, 28);

        // One ordinary pane: it has the lot.
        place_panes(&mut app, body);
        assert_eq!(app.panes[0].frame, Rect::new(0, 1, 100, 28));

        // A dock on the left takes thirty columns and the pane takes seventy.
        let doc = app.panes[0].doc;
        let mut sidebar = crate::view::View::new(doc, false);
        sidebar.dock = Some(crate::view::Dock::new(crate::view::Edge::Left, Some(30)));
        app.panes.insert(0, sidebar);
        place_panes(&mut app, body);
        assert_eq!(app.panes[0].frame, Rect::new(0, 1, 30, 28));
        assert_eq!(app.panes[1].frame, Rect::new(30, 1, 70, 28));

        // A second sidebar on the right, and the middle is what is left.
        let mut right = crate::view::View::new(doc, false);
        right.dock = Some(crate::view::Dock::new(crate::view::Edge::Right, Some(20)));
        app.panes.push(right);
        place_panes(&mut app, body);
        assert_eq!(app.panes[0].frame, Rect::new(0, 1, 30, 28));
        assert_eq!(app.panes[1].frame, Rect::new(30, 1, 50, 28));
        assert_eq!(app.panes[2].frame, Rect::new(80, 1, 20, 28));

        // And along the bottom, which is a height rather than a width.
        let mut bottom = crate::view::View::new(doc, false);
        bottom.dock = Some(crate::view::Dock::new(crate::view::Edge::Bottom, Some(8)));
        app.panes.push(bottom);
        place_panes(&mut app, body);
        assert_eq!(app.panes[3].frame, Rect::new(30, 21, 50, 8));
        assert_eq!(app.panes[1].frame, Rect::new(30, 1, 50, 20));
    }

    #[test]
    fn panes_in_the_middle_share_it_in_the_proportions_they_were_dragged_to() {
        // Equal until somebody pulls a divider, and proportional afterwards —
        // so resizing the terminal keeps the layout you chose rather than the
        // number of columns it happened to work out to.
        assert_eq!(share_out(&[1.0, 1.0], 100, MIN_PANE), vec![50, 50]);
        assert_eq!(share_out(&[1.0, 1.0, 1.0], 100, MIN_PANE), vec![33, 33, 34]);
        assert_eq!(share_out(&[30.0, 70.0], 100, MIN_PANE), vec![30, 70]);
        // The same proportions in half the room.
        assert_eq!(share_out(&[30.0, 70.0], 50, MIN_PANE), vec![15, 35]);
        // Every column is given out: no stripe of nothing down the middle.
        for room in [40u16, 61, 80, 137] {
            for shares in [vec![1.0, 1.0], vec![1.0, 2.0, 7.0], vec![5.0]] {
                let given = share_out(&shares, room, MIN_PANE);
                assert_eq!(given.iter().sum::<u16>(), room, "{shares:?} in {room}");
            }
        }
        // And nothing is dragged shut, because a pane with no width has no
        // edge left to drag it back by.
        assert!(share_out(&[1.0, 400.0], 100, MIN_PANE)[0] >= MIN_PANE);

        // A window too small to hold what a pane is owed has no answer that
        // respects both, and the one it must not give is a crash — `clamp`
        // with a floor above its ceiling panics, and "the window got short" is
        // not a crashing matter.
        for room in 0u16..12 {
            for count in 1usize..5 {
                let shares = vec![1.0; count];
                let given = share_out(&shares, room, MIN_PANE);
                assert_eq!(given.len(), count, "{count} panes in {room}");
                assert_eq!(given.iter().sum::<u16>(), room, "{count} panes in {room}");
            }
        }
    }

    #[test]
    fn a_dock_never_squeezes_the_code_out_of_the_editor() {
        // A plugin asking for eighty columns on a narrow terminal gets what
        // there is to give, not an editor with no room left in it.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        let doc = app.panes[0].doc;
        let mut sidebar = crate::view::View::new(doc, false);
        sidebar.dock = Some(crate::view::Dock::new(crate::view::Edge::Left, Some(80)));
        app.panes.insert(0, sidebar);

        let body = Rect::new(0, 1, 60, 20);
        place_panes(&mut app, body);
        assert!(
            app.panes[1].frame.width >= MIN_MIDDLE,
            "the middle was squeezed to {}",
            app.panes[1].frame.width
        );
        // The two together are still exactly the body: no stripe of nothing.
        assert_eq!(app.panes[0].frame.width + app.panes[1].frame.width, 60);
    }

    #[test]
    fn what_is_docked_is_not_offered_as_a_tab() {
        // A row across the top offering to switch to the thing already down
        // the left — and to close it with a cross that is not how you close
        // it — is a row saying something untrue.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.screen = Rect::new(0, 0, 90, 12);
        // A second buffer standing in for the plugin's own.
        app.run(crate::cmd::Cmd::NEW);
        let sidebar = app.view().doc;
        if let Some(doc) = app.doc_mut(sidebar) {
            doc.name = "tree".into();
        }
        let mut pane = crate::view::View::new(sidebar, false);
        pane.dock = Some(crate::view::Dock::new(crate::view::Edge::Left, Some(20)));
        app.panes.insert(0, pane);
        app.focus = 1;

        let mut terminal = Terminal::new(TestBackend::new(90, 12)).expect("a terminal");
        terminal.draw(|frame| super::draw(frame, &mut app)).expect("drawn");
        let buffer = terminal.backend().buffer();
        let row: String = (0..buffer.area.width).map(|x| buffer[(x, 0)].symbol()).collect();
        assert!(!row.contains("tree"), "the sidebar was offered as a tab: {row:?}");
        assert!(row.contains("untitled"), "the real tab went missing: {row:?}");
    }

    #[test]
    fn a_dock_has_no_line_numbers_down_it() {
        // A tree of file names does not have lines you refer to by number,
        // and the room they take is room the names needed.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        let doc = app.panes[0].doc;
        let mut sidebar = crate::view::View::new(doc, false);
        sidebar.dock = Some(crate::view::Dock::new(crate::view::Edge::Left, Some(30)));
        app.panes.insert(0, sidebar);
        place_panes(&mut app, Rect::new(0, 1, 100, 28));
        // Nothing between the edge and the names. The divider is on the other
        // side and stands in for the focus rule.
        assert_eq!(app.panes[0].gutter, 0);
        assert_eq!(
            app.panes[0].grip,
            Some(Rect::new(29, 1, 1, 28)),
            "the divider should be the dock's inner edge"
        );
        // And it comes out of the room the text gets, not out of the room the
        // dock takes: the dock still occupies the thirty columns asked for.
        assert_eq!(app.panes[0].frame.width, 30);
        assert_eq!(app.panes[0].area.x, 0);
        assert!(app.panes[0].area.right() <= 29);
        assert!(app.panes[1].gutter > 1, "the code still has its numbers");
    }

    #[test]
    fn a_hover_line_wider_than_the_screen_is_folded_rather_than_elided() {
        // What the box does about a long line, drawn: no ellipsis, nothing
        // running off the side, and every word still there to read.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.here_mut().rope = ropey::Rope::from_str("fn main() {}\n");
        app.screen = Rect::new(0, 0, 60, 24);
        let long = "pub fn something(first: &BTreeMap<String, usize>, second: usize) \
                    -> Result<Vec<String>, Error>";
        let mut popup = crate::app::Popup::new(vec![crate::app::DocLine::prose(long)], 0);
        popup.focused = true;
        app.hover = Some(popup);
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).expect("a terminal");
        terminal.draw(|frame| super::draw(frame, &mut app)).expect("drawn");

        let buffer = terminal.backend().buffer();
        let drawn: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!drawn.contains('\u{2026}'), "it was cut off:\n{drawn}");
        // Every word of it is on the screen somewhere, which is the whole
        // point — the half that says what the arguments are is the half an
        // ellipsis used to take.
        for word in ["BTreeMap<String,", "second:", "Result<Vec<String>,", "Error>"] {
            assert!(drawn.contains(word), "{word:?} was lost:\n{drawn}");
        }
        // And the box is more than one row of text now.
        assert!(hover_height(terminal.backend().buffer()) > 3);
    }

    /// How tall the drawn hover is, measured by its rounded corners.
    fn hover_height(buffer: &Buffer) -> usize {
        let corners: Vec<u16> = (0..buffer.area.height)
            .filter(|y| {
                (0..buffer.area.width)
                    .any(|x| matches!(buffer[(x, *y)].symbol(), "\u{256d}" | "\u{2570}"))
            })
            .collect();
        match corners.as_slice() {
            [top, bottom] => (bottom - top + 1) as usize,
            _ => 0,
        }
    }

    #[test]
    fn a_hover_stays_the_same_size_however_far_down_it_you_read() {
        let (mut app, mut terminal) = hover_on_screen(60);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let was = hover_height(terminal.backend().buffer());
        assert!(was > 4, "nothing was drawn");

        for _ in 0..80 {
            app.hover.as_mut().expect("a hover").scroll_by(1);
            terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
            assert_eq!(
                hover_height(terminal.backend().buffer()),
                was,
                "the box changed size while being read"
            );
        }
    }

    #[test]
    fn a_hover_cannot_be_scrolled_past_its_last_line() {
        let (mut app, mut terminal) = hover_on_screen(60);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        for _ in 0..200 {
            app.hover.as_mut().expect("a hover").scroll_by(1);
        }
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");

        let hover = app.hover.as_ref().expect("a hover");
        let rows = hover.area.height as usize;
        assert_eq!(
            hover.scroll,
            60 - rows,
            "the last line should sit on the bottom row and no further"
        );
        // And the box is still full of text rather than empty from the top.
        let text: String = (hover.area.y..hover.area.y + hover.area.height)
            .map(|y| row_text(terminal.backend().buffer(), y))
            .collect();
        assert!(text.contains("line 59"), "the end is not on the screen");
        assert!(!text.trim().is_empty());
    }

    #[test]
    fn the_name_under_the_pointer_in_a_hover_is_drawn_as_something_you_can_follow() {
        let (mut app, mut terminal) = hover_on_screen(20);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let area = app.hover.as_ref().expect("a hover").area;

        // "line 0 about `Selections` here" — point at the S.
        let column = area.x + "line 0 about `".len() as u16;
        move_pointer(&mut app, column, area.y);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");

        let buffer = terminal.backend().buffer();
        let underlined: String = (0..buffer.area.width)
            .filter(|x| {
                buffer[(*x, area.y)]
                    .style()
                    .add_modifier
                    .contains(Modifier::UNDERLINED)
            })
            .map(|x| buffer[(x, area.y)].symbol())
            .collect();
        assert_eq!(underlined, "Selections");
    }

    fn nothing_underlined(buffer: &Buffer, y: u16) -> bool {
        !(0..buffer.area.width).any(|x| {
            buffer[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::UNDERLINED)
        })
    }

    #[test]
    fn pointing_at_prose_rather_than_a_name_underlines_nothing() {
        let (mut app, mut terminal) = hover_on_screen(20);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let area = app.hover.as_ref().expect("a hover").area;

        // "about" is an ordinary word in a sentence, not a name in backticks,
        // and a box where every word lights up says nothing by lighting up.
        move_pointer(&mut app, area.x + "line 0 a".len() as u16, area.y);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        assert!(nothing_underlined(terminal.backend().buffer(), area.y));

        // Nor does the space between words.
        move_pointer(&mut app, area.x + 6, area.y);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        assert!(nothing_underlined(terminal.backend().buffer(), area.y));
    }

    /// Press, move, release — a drag across a hover, the way the terminal
    /// sends one.
    fn drag_over(app: &mut App, from: (u16, u16), to: (u16, u16)) {
        for (kind, at) in [
            (MouseEventKind::Down(MouseButton::Left), from),
            (MouseEventKind::Drag(MouseButton::Left), to),
            (MouseEventKind::Up(MouseButton::Left), to),
        ] {
            app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
                kind,
                column: at.0,
                row: at.1,
                modifiers: KeyModifiers::NONE,
            })));
        }
    }

    #[test]
    fn dragging_over_a_hover_selects_what_you_dragged_over() {
        let (mut app, mut terminal) = hover_on_screen(20);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let area = app.hover.as_ref().expect("a hover").area;

        // "line 0 about `Selections` here" — take "0 about".
        drag_over(&mut app, (area.x + 5, area.y), (area.x + 12, area.y));
        assert_eq!(
            app.hover.as_ref().expect("a hover").selected_text(),
            Some("0 about".into())
        );

        // And it is painted, so you can see what you took.
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let buffer = terminal.backend().buffer();
        let lit: String = (0..buffer.area.width)
            .filter(|x| buffer[(*x, area.y)].style().bg == Some(app.theme.selection))
            .map(|x| buffer[(x, area.y)].symbol())
            .collect();
        assert_eq!(lit, "0 about");
    }

    #[test]
    fn a_selection_across_lines_takes_the_line_breaks_with_it() {
        let (mut app, mut terminal) = hover_on_screen(20);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let area = app.hover.as_ref().expect("a hover").area;

        drag_over(&mut app, (area.x, area.y), (area.x + 6, area.y + 2));
        let took = app
            .hover
            .as_ref()
            .expect("a hover")
            .selected_text()
            .expect("something selected");
        assert_eq!(took.lines().count(), 3, "{took:?}");
        assert!(took.starts_with("line 0 about"), "{took:?}");
        assert!(took.ends_with("line 2"), "{took:?}");
    }

    #[test]
    fn ctrl_c_in_a_hover_copies_what_was_dragged_over_and_nothing_else() {
        let (mut app, mut terminal) = hover_on_screen(20);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let area = app.hover.as_ref().expect("a hover").area;
        drag_over(&mut app, (area.x, area.y), (area.x + 6, area.y));

        let key = crate::keys::Key::parse("ctrl-c").expect("a key");
        app.handle(crate::app::Event::Term(TermEvent::Key(
            ratatui::crossterm::event::KeyEvent::new(key.code, key.mods),
        )));
        assert_eq!(app.clipboard, "line 0");
    }

    #[test]
    fn double_clicking_a_hover_takes_the_word() {
        let (mut app, mut terminal) = hover_on_screen(20);
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let area = app.hover.as_ref().expect("a hover").area;
        let column = area.x + "line 0 ab".len() as u16;
        for _ in 0..2 {
            app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            })));
        }
        assert_eq!(
            app.hover.as_ref().expect("a hover").selected_text(),
            Some("about".into())
        );
    }

    #[test]
    fn moving_the_pointer_into_a_hover_does_not_dismiss_it() {
        let (mut app, mut terminal) = hover_on_screen(20);
        app.hover.as_mut().expect("a hover").focused = false;
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");
        let area = app.hover.as_ref().expect("a hover").outer;

        move_pointer(&mut app, area.x + 2, area.y + 1);
        assert!(app.hover.is_some(), "it vanished as the pointer reached it");

        // Away from it, and an unasked-for hover goes as it always did.
        move_pointer(&mut app, 0, 23);
        assert!(app.hover.is_none());
    }

    #[test]
    fn dragging_a_tab_along_the_row_reorders_it() {
        let mut app = tabs_named(&["aaa", "bbbbbbbbbbbb", "ccc"], 80);
        tab_row(&mut app, 80);
        let (from, to) = tab_at(&app, "aaa");

        click(&mut app, from + 1, 0);
        assert_eq!(app.here().name, "aaa", "the press did not switch to it");

        // Past the middle of the tab beside it.
        let (their_from, their_to) = tab_at(&app, "bbbbbbbbbbbb");
        let _ = to;
        drag(&mut app, their_from + (their_to - their_from) / 2 + 1, 0);
        assert_eq!(order(&app), vec!["bbbbbbbbbbbb", "aaa", "ccc"]);
        assert_eq!(app.here().name, "aaa", "it stopped being the current tab");
    }

    #[test]
    fn a_tab_does_not_move_until_the_pointer_is_past_the_middle_of_the_next_one() {
        let mut app = tabs_named(&["aaa", "bbbbbbbbbbbb", "ccc"], 80);
        tab_row(&mut app, 80);
        let (from, _) = tab_at(&app, "aaa");
        click(&mut app, from + 1, 0);

        // Just inside its neighbour, but not yet halfway across it.
        let (their_from, _) = tab_at(&app, "bbbbbbbbbbbb");
        drag(&mut app, their_from + 1, 0);
        assert_eq!(
            order(&app),
            vec!["aaa", "bbbbbbbbbbbb", "ccc"],
            "it jumped as soon as the pointer touched the next tab"
        );
    }

    #[test]
    fn a_narrow_tab_dragged_onto_a_wide_one_settles_instead_of_trading_places() {
        // The bug this rule exists for. Put a narrow tab where a wide one was
        // and the pointer is left sitting over the wide one again — which, on
        // the obvious rule of "move it to whatever is under the pointer", asks
        // for the swap back, and the two flicker for as long as you hold still.
        let mut app = tabs_named(&["aaa", "bbbbbbbbbbbbbbbbbbbb"], 80);
        tab_row(&mut app, 80);
        let (from, _) = tab_at(&app, "aaa");
        click(&mut app, from + 1, 0);

        let (their_from, their_to) = tab_at(&app, "bbbbbbbbbbbbbbbbbbbb");
        let past = their_from + (their_to - their_from) / 2 + 1;
        drag(&mut app, past, 0);
        assert_eq!(order(&app), vec!["bbbbbbbbbbbbbbbbbbbb", "aaa"]);

        // Now hold it there. Every further report of the same position must
        // leave the row exactly as it is.
        for _ in 0..8 {
            tab_row(&mut app, 80);
            drag(&mut app, past, 0);
            assert_eq!(
                order(&app),
                vec!["bbbbbbbbbbbbbbbbbbbb", "aaa"],
                "the two tabs traded places again while the pointer held still"
            );
        }
    }

    #[test]
    fn a_tab_dragged_off_the_row_is_left_where_it_was() {
        let mut app = tabs_named(&["aaa", "bbb"], 80);
        tab_row(&mut app, 80);
        let (from, _) = tab_at(&app, "aaa");
        click(&mut app, from + 1, 0);
        // Down into the text, which is not the row of tabs.
        drag(&mut app, 40, 6);
        assert_eq!(order(&app), vec!["aaa", "bbb"]);
    }

    #[test]
    fn moving_a_tab_by_key_walks_it_and_stops_at_the_ends() {
        let mut app = tabs_named(&["aaa", "bbb", "ccc"], 80);
        // The current tab is the last one made.
        assert_eq!(app.here().name, "ccc");
        app.run(Cmd::MOVE_TAB_LEFT);
        assert_eq!(order(&app), vec!["aaa", "ccc", "bbb"]);
        app.run(Cmd::MOVE_TAB_LEFT);
        assert_eq!(order(&app), vec!["ccc", "aaa", "bbb"]);
        assert_eq!(app.here().name, "ccc", "it stopped being the current tab");

        // And no further: moving a tab does not wrap it round to the far end,
        // which is never what nudging one along meant.
        app.run(Cmd::MOVE_TAB_LEFT);
        assert_eq!(order(&app), vec!["ccc", "aaa", "bbb"]);
        app.run(Cmd::MOVE_TAB_RIGHT);
        app.run(Cmd::MOVE_TAB_RIGHT);
        assert_eq!(order(&app), vec!["aaa", "bbb", "ccc"]);
        app.run(Cmd::MOVE_TAB_RIGHT);
        assert_eq!(order(&app), vec!["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn the_tab_of_the_file_you_are_looking_at_is_always_on_the_screen() {
        // Twelve files across forty columns: most of them are off the side, and
        // the one being edited is the twelfth, which is not one of the ones
        // that happen to fit.
        let mut app = many_tabs(12, 40);
        let here = app.here().name.clone();
        let row = tab_row(&mut app, 40);
        assert!(row.contains(&here), "{here} is not in {row:?}");
        assert!(app.tab_scroll > 0, "the row scrolled to reach it");

        // And walking back to the first one scrolls the other way.
        for _ in 0..11 {
            app.run(Cmd::PREV_BUFFER);
        }
        let here = app.here().name.clone();
        let row = tab_row(&mut app, 40);
        assert_eq!(app.tab_scroll, 0);
        assert!(row.contains(&here), "{here} is not in {row:?}");
    }

    #[test]
    fn a_row_of_tabs_too_wide_to_fit_says_which_way_the_rest_are() {
        let mut app = many_tabs(12, 40);
        // Somewhere in the middle, so there is more of the row both ways.
        for _ in 0..5 {
            app.run(Cmd::PREV_BUFFER);
        }
        let row = tab_row(&mut app, 40);
        assert!(row.starts_with('\u{2039}'), "{row:?}");
        assert!(row.ends_with('\u{203a}'), "{row:?}");
        assert_eq!(app.tab_nudges.len(), 2, "one arrow at each end");

        // Clicking the left one moves back by a whole tab, not by a column.
        let was = app.tab_scroll;
        click(&mut app, 0, 0);
        assert!(
            app.tab_scroll < was,
            "{} is not before {was}",
            app.tab_scroll
        );
        // And it did not switch file, which is what the tab under it would do.
        assert_eq!(app.here().name, app.docs()[6].name);
    }

    #[test]
    fn tabs_that_all_fit_do_not_scroll_and_have_no_arrows() {
        let mut app = many_tabs(3, 100);
        let row = tab_row(&mut app, 100);
        assert_eq!(app.tab_scroll, 0);
        assert!(app.tab_nudges.is_empty());
        assert!(
            !row.contains('\u{2039}') && !row.contains('\u{203a}'),
            "{row:?}"
        );
    }

    #[test]
    fn a_click_lands_on_the_tab_that_is_drawn_there() {
        // The hit boxes are in the screen's columns and the tabs are drawn from
        // a strip that has been slid sideways, so the two have to agree: a
        // click must switch to the file whose name is under the pointer.
        let mut app = many_tabs(12, 40);
        let row: Vec<char> = tab_row(&mut app, 40).chars().collect();
        // A tab that is on the screen whole, found by its name rather than by
        // counting columns — where it lands is the thing under test.
        let wanted = format!(" {} ", app.docs()[10].name);
        let at = row
            .windows(wanted.chars().count())
            .position(|w| w.iter().collect::<String>() == wanted)
            .expect("a whole tab on the screen") as u16;

        click(&mut app, at + 1, 0);
        assert_eq!(
            app.here().name,
            app.docs()[10].name,
            "clicking column {at} of {:?}",
            row.iter().collect::<String>()
        );
    }

    #[test]
    fn the_wheel_over_the_tabs_walks_along_them() {
        let mut app = many_tabs(12, 40);
        for _ in 0..11 {
            app.run(Cmd::PREV_BUFFER);
        }
        tab_row(&mut app, 40);
        assert_eq!(app.tab_scroll, 0);
        app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })));
        assert!(app.tab_scroll > 0, "the row moved along");
    }

    #[test]
    fn every_extra_cursor_is_drawn_where_it_is() {
        // The terminal has one cursor and this has three, so two of them are
        // blocks painted onto the text. They are painted over the character
        // they are on, which is the part that is easy to get backwards: draw
        // the block first and the character lands on top of it, and a
        // multi-cursor edit becomes one you cannot see.
        let (buffer, app) = screen("alpha\nbravo\ncharlie\n", |app| {
            app.view_mut().sel =
                Selections::many(vec![Range::point(2), Range::point(8), Range::point(14)], 0);
        });
        let blocks = cells_on(&buffer, app.theme.cursor);
        // The primary is the terminal's own cursor and is not painted, so two.
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        // And each one still shows the character it is sitting on.
        for (x, y) in blocks {
            let symbol = buffer[(x, y)].symbol();
            assert!(
                matches!(symbol, "p" | "a"),
                "the block at {x},{y} is {symbol:?}, not the character under it"
            );
        }
    }

    #[test]
    fn a_cursor_past_the_end_of_a_line_is_drawn_too() {
        // Alt-Shift-I puts a cursor at the end of every selected line, which
        // is a cursor with no character under it to carry the block.
        let (buffer, app) = screen("aa\nbb\ncc\n", |app| {
            app.run(Cmd::SELECT_ALL);
            app.run(Cmd::CURSORS_TO_LINE_ENDS);
        });
        assert!(app.view().sel.len() > 1, "several cursors");
        let blocks = cells_on(&buffer, app.theme.cursor);
        assert_eq!(blocks.len(), app.view().sel.len() - 1, "{blocks:?}");
    }
    /// Click the left button somewhere, the way the terminal reports it.
    fn click_at(app: &mut App, column: u16, row: u16) {
        app.handle(crate::app::Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })));
    }

    #[test]
    fn clicking_a_divider_does_nothing_rather_than_cutting_your_text() {
        let (mut app, _t) = menu_on_screen();
        let (area, divider) = {
            let Overlay::Menu(menu) = &app.overlay else { panic!("no menu") };
            let divider = menu
                .items
                .iter()
                .position(|i| matches!(i.action, crate::menu::Action::Divide))
                .expect("the text menu has dividers");
            (menu.area, divider)
        };
        // The menu opens with "Cut" highlighted, and the first divider is three
        // rows below it. Clicking the line used to run the highlight.
        click_at(&mut app, area.x + 2, area.y + divider as u16);
        assert!(matches!(app.overlay, Overlay::None), "the menu stayed open");
        assert!(
            !app.here().is_modified(),
            "clicking a divider changed the text"
        );
    }

    #[test]
    fn clicking_a_row_runs_that_row() {
        let (mut app, _t) = menu_on_screen();
        let (area, select_all) = {
            let Overlay::Menu(menu) = &app.overlay else { panic!("no menu") };
            let at = menu
                .items
                .iter()
                .position(|i| i.action == crate::menu::Action::Run(Cmd::SELECT_ALL))
                .expect("select all is on the menu");
            (menu.area, at)
        };
        click_at(&mut app, area.x + 2, area.y + select_all as u16);
        assert_eq!(
            app.view().sel.primary().len(),
            app.here().len_chars(),
            "clicking \"Select all\" did not select all"
        );
    }

    #[test]
    fn a_menu_taller_than_the_screen_can_still_reach_its_last_row() {
        let (mut app, mut terminal) = menu_on_screen();
        let last = {
            let Overlay::Menu(menu) = &app.overlay else { panic!("no menu") };
            assert!(
                menu.len() > menu.area.height as usize,
                "this test needs a menu that does not fit; it does"
            );
            menu.len() - 1
        };
        // Walk the highlight to the end, which is what the arrows and the
        // wheel both do.
        for _ in 0..menu_rows(&app) {
            let Overlay::Menu(menu) = &mut app.overlay else { panic!() };
            menu.step(1);
        }
        {
            let Overlay::Menu(menu) = &mut app.overlay else { panic!() };
            menu.cursor = last;
        }
        terminal.draw(|f| super::draw(f, &mut app)).expect("drawn");

        let (area, scroll) = {
            let Overlay::Menu(menu) = &app.overlay else { panic!() };
            (menu.area, menu.scroll)
        };
        assert!(scroll > 0, "the menu did not scroll to show its last row");
        let row = area.y + (last - scroll) as u16;
        assert!(
            row < area.y + area.height,
            "the last row is still off the bottom"
        );
        click_at(&mut app, area.x + 2, row);
        assert!(
            matches!(app.overlay, Overlay::Picker(_)),
            "clicking the last row (\"Find it in every file\") did nothing"
        );
    }

    fn menu_rows(app: &App) -> usize {
        let Overlay::Menu(menu) = &app.overlay else { panic!() };
        menu.len()
    }

}


