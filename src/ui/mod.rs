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
    app.hits.forget();
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

mod pane;
pub(crate) use pane::*;
mod bars;
use bars::{draw_status, draw_tabs};
mod floating;
pub(crate) use floating::*;
#[cfg(test)]
mod tests;