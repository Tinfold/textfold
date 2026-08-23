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
use crate::doc::{Diagnostic, Document, Severity};
use crate::text::{self, Range};
use crate::theme::Theme;
use crate::view::{self, Layout, View};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.screen = area;
    app.tab_hits.clear();
    app.status_hits.clear();
    if area.width < 4 || area.height < 3 {
        return;
    }

    let theme = app.theme;
    let paint = app.config.background();
    let ground = if paint { theme.bg } else { Color::Reset };
    frame
        .buffer_mut()
        .set_style(area, Style::new().bg(ground).fg(theme.text));

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
}

/// Work out where each pane goes, and how wide its line numbers are.
///
/// Done before anything is drawn, because the mouse reads these back and a
/// click has to land on what is actually on the screen.
fn place_panes(app: &mut App, body: Rect) {
    let count = app.panes.len().max(1) as u16;
    for index in 0..app.panes.len() {
        let at = index as u16;
        let frame = if app.side_by_side {
            let width = body.width / count;
            let x = body.x + width * at;
            // The last pane takes whatever is left over, so a width that does
            // not divide evenly does not leave a stripe of nothing.
            let width = if at + 1 == count {
                body.width - width * at
            } else {
                width
            };
            Rect::new(x, body.y, width, body.height)
        } else {
            let height = body.height / count;
            let y = body.y + height * at;
            let height = if at + 1 == count {
                body.height - height * at
            } else {
                height
            };
            Rect::new(body.x, y, body.width, height)
        };

        let id = app.panes[index].doc;
        let lines = app.doc(id).map(Document::len_lines).unwrap_or(1);
        let numbers = match app.config.line_numbers() {
            LineNumbers::Off => 1,
            // Room for the number, a space either side, and the mark that says
            // there is something wrong on this line.
            _ => (digits(lines) + 3) as u16,
        };
        // A column of its own for the rule that says which pane has the
        // focus, rather than borrowing one from the line numbers — which for
        // a file long enough would have taken a digit with it.
        let gutter = numbers + rule_width(count);
        // A scroll bar down the right, except in a pane too narrow to spare it.
        let bar = if frame.width > 20 { 1 } else { 0 };
        let text_width = frame.width.saturating_sub(gutter + bar).max(1);

        let pane = &mut app.panes[index];
        pane.frame = frame;
        pane.gutter = gutter;
        pane.area = Rect::new(frame.x + gutter, frame.y, text_width, frame.height);
    }
}

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// How wide the pane's left edge is: one column for the focus rule when there
/// is more than one pane, and nothing at all when there is not.
fn rule_width(panes: u16) -> u16 {
    u16::from(panes > 1)
}

// ---------------------------------------------------------------------------
// The text.
// ---------------------------------------------------------------------------

/// Draw one pane, and say where the terminal's own cursor should go if this is
/// the pane with the focus.
fn draw_pane(frame: &mut Frame, app: &App, index: usize, ground: Color) -> Option<Position> {
    let view = &app.panes[index];
    let doc = app.doc(view.doc)?;
    let theme = &app.theme;
    let tab_width = app.config.tab_width();
    let focused = index == app.focus.min(app.panes.len() - 1);
    let area = view.area;
    if area.width == 0 || area.height == 0 {
        return None;
    }

    let layout = Layout {
        rope: &doc.rope,
        width: area.width as usize,
        tab_width,
        wrap: view.wrap,
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
    let spans: Vec<(Range, crate::theme::Role)> = doc
        .syntax
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
        .unwrap_or_default();

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

    if app.panes.len() > 1 {
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
    let line_bg = if on_cursor_line && it.focused && theme.cursorline != theme.bg {
        theme.cursorline
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
            cell.set_style(Style::new().bg(line_bg).fg(theme.dim));
            cell.set_char('\u{2502}');
        }
    }

    let diagnostics = diagnostics_on(doc, start, end);
    let mut cursor_at = None;
    let mut at = start;
    let mut column = 0usize;

    // The left edge, for a pane that is not folding lines.
    let skip = if view.wrap { 0 } else { view.left };

    while at <= end {
        let is_cursor = it.cursors.contains(&at);
        if is_cursor && column >= skip {
            let x = area.x as usize + column - skip;
            if x < (area.x + area.width) as usize {
                if at == view.sel.primary().head {
                    cursor_at = Some(Position::new(x as u16, it.screen));
                } else if it.focused {
                    // The other cursors have no terminal cursor of their own,
                    // so they are drawn: a block where the character is.
                    if let Some(cell) = buf.cell_mut(Position::new(x as u16, it.screen)) {
                        cell.set_style(
                            Style::new()
                                .bg(theme.accent)
                                .fg(theme.on_accent)
                                .add_modifier(Modifier::BOLD),
                        );
                    }
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

        let mut style = Style::new().bg(if selected { theme.selection } else { line_bg });
        style = style.fg(colour_of(it.spans, at, theme));

        if let Some(severity) = diagnostics
            .iter()
            .filter(|d| d.range.contains(at) || (d.range.is_empty() && d.range.start() == at))
            .map(|d| d.severity)
            .min()
        {
            // Underlined in the colour of how bad it is, rather than
            // recoloured: the code should still look like code.
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(severity_colour(severity, theme));
        }
        if Some(at) == it.partner || (it.partner.is_some() && at == view.sel.primary().head) {
            style = style
                .add_modifier(Modifier::BOLD)
                .fg(theme.accent);
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
                style = style.fg(theme.dim);
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
                        .set_style(Style::new().bg(line_bg).fg(theme.dim));
                }
            }
            break;
        }
    }
    let _ = layout;
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
        Err(_) => theme.text,
    }
}

fn severity_colour(severity: Severity, theme: &Theme) -> Color {
    match severity {
        Severity::Error => theme.bad,
        Severity::Warning => theme.warn,
        Severity::Info => theme.info,
        Severity::Hint => theme.dim,
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
}

fn draw_gutter(buf: &mut Buffer, app: &App, view: &View, doc: &Document, it: Gutter) {
    let Gutter {
        line,
        numbered,
        screen,
        cursor_lines,
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
        .bg(if here && theme.cursorline != theme.bg {
            theme.cursorline
        } else {
            Color::Reset
        })
        .fg(if here { theme.gutter_active } else { theme.gutter });

    // ` 42 ` and then the mark, hard against the text.
    let text = if width >= 2 {
        format!("{label:>width$} ", width = width - 2)
    } else {
        String::new()
    };
    buf.set_stringn(x, screen, &text, width, style);
    if let Some(severity) = worst
        && let Some(cell) = buf.cell_mut(Position::new(frame.x + view.gutter - 1, screen))
    {
        cell.set_style(style.fg(severity_colour(severity, theme)));
        cell.set_char(severity.mark().chars().next().unwrap_or('*'));
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
                .set_style(Style::new().fg(if inside { theme.dim } else { theme.faint() }));
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
            cell.set_char('\u{2503}').set_style(
                Style::new().fg(if focused { theme.accent } else { theme.faint() }),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The row at the top and the row at the bottom.
// ---------------------------------------------------------------------------

fn draw_tabs(frame: &mut Frame, app: &mut App, area: Rect, ground: Color) {
    let theme = app.theme;
    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().bg(theme.faint()).fg(theme.muted));

    let here = app.view().doc;
    let mut x = area.x;
    let mut hits = Vec::new();

    for doc in app.docs() {
        if x >= area.x + area.width {
            break;
        }
        let current = doc.id == here;
        // ` name • ` — the dot is the close cross, and the mark that says
        // there are unsaved changes, because they are never both wanted at
        // once and one column is one column.
        let mark = if doc.is_modified() { '\u{25cf}' } else { '\u{00d7}' };
        let label = format!(" {} ", doc.name);
        let width = (text::str_width(&label) + 2) as u16;
        let width = width.min(area.x + area.width - x);
        if width < 4 {
            break;
        }

        let style = if current {
            Style::new()
                .bg(theme.bg)
                .fg(theme.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(theme.faint()).fg(theme.muted)
        };
        buf.set_stringn(x, area.y, &label, (width - 2) as usize, style);
        let cross = x + width - 2;
        buf.set_stringn(
            cross,
            area.y,
            format!("{mark} "),
            2,
            style.fg(if doc.is_modified() {
                theme.warn
            } else {
                theme.dim
            }),
        );

        hits.push((Rect::new(x, area.y, width - 2, 1), doc.id, false));
        hits.push((Rect::new(cross, area.y, 1, 1), doc.id, true));
        x += width;
    }

    // What is going on at the far right: the language servers, so that "why
    // are there no completions" has an answer on the screen.
    let busy: Vec<String> = app
        .lsp
        .all()
        .iter()
        .filter_map(|server| {
            server
                .busy_with()
                .map(|what| format!("{} {what}", server.name))
        })
        .collect();
    if let Some(said) = busy.first() {
        let said = format!(" {said} ");
        let width = text::str_width(&said) as u16;
        if area.width > width + 4 {
            buf.set_stringn(
                area.x + area.width - width,
                area.y,
                &said,
                width as usize,
                Style::new().bg(theme.faint()).fg(theme.info),
            );
        }
    }
    let _ = ground;
    app.tab_hits = hits;
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
        .map(|d| (d.severity, d.message.lines().next().unwrap_or("").to_string()));

    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().bg(theme.faint()).fg(theme.muted));

    // Left: what is being said, or what is wrong under the cursor.
    let (left, left_style) = if app.status.showing() {
        (
            app.status.text.clone(),
            Style::new().fg(match app.status.tone {
                Tone::Plain => theme.text,
                Tone::Good => theme.good,
                Tone::Bad => theme.bad,
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
            if errors > 0 { theme.bad } else { theme.warn },
            Cmd::Diagnostics,
        ));
    }
    if cursors > 1 {
        chips.push((
            format!("{cursors} cursors"),
            theme.accent,
            Cmd::CollapseCursors,
        ));
    } else if selected > 0 {
        chips.push((format!("{selected} selected"), theme.info, Cmd::SelectLine));
    }
    if doc.read_only {
        chips.push(("read-only".into(), theme.warn, Cmd::About));
    }
    if let Some(why) = doc.colours_off {
        chips.push((format!("no colours: {why}"), theme.dim, Cmd::About));
    }
    chips.push((language, theme.muted, Cmd::SetLanguage));
    chips.push((format!("{line}:{column}"), theme.muted, Cmd::GotoLine));
    chips.push((
        app.config.theme_name().to_string(),
        theme.muted,
        Cmd::ThemePicker,
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
            Style::new().bg(theme.faint()).fg(*colour),
        );
        hits.push((Rect::new(right, area.y, width, 1), *cmd));
    }

    let room = right.saturating_sub(area.x + 1) as usize;
    buf.set_stringn(
        area.x + 1,
        area.y,
        text::truncate(&left, room),
        room,
        left_style.bg(theme.faint()),
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
        Overlay::None => None,
    }
}

/// Where the cursor is on the screen, if it is on the screen at all.
fn cursor_screen(app: &App) -> Option<Position> {
    screen_position_of(app, app.view().sel.primary().head)
}

/// Where a place in the file is on the screen. `None` for one scrolled out of
/// sight, which is what tells a popup about that place not to be drawn.
fn screen_position_of(app: &App, at: usize) -> Option<Position> {
    let view = app.view();
    let doc = app.doc(view.doc)?;
    let layout = Layout {
        rope: &doc.rope,
        width: view.area.width.max(1) as usize,
        tab_width: app.config.tab_width(),
        wrap: view.wrap,
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
    Style::new().bg(if theme.bg == Color::Reset {
        ground
    } else {
        theme.bg
    })
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
                + item.detail.as_deref().map(text::str_width).unwrap_or(0)
                + text::str_width(item.kind)
                + 6
        })
        .max()
        .unwrap_or(20);
    let width = (widest as u16).clamp(20, screen.width.saturating_sub(4)).min(72);
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
    let items: Vec<(String, Option<String>, &'static str)> = completion
        .visible()
        .skip(top)
        .take(rows as usize)
        .map(|item| (item.label.clone(), item.detail.clone(), item.kind))
        .collect();
    let about = completion.selected().and_then(|item| item.about.clone());

    frame.render_widget(Clear, area);
    let buf = frame.buffer_mut();
    buf.set_style(area, box_style(&theme, ground).fg(theme.text));

    for (row, (label, detail, kind)) in items.iter().enumerate() {
        let y = area.y + row as u16;
        let chosen = top + row == cursor;
        let style = if chosen {
            Style::new().bg(theme.selection).fg(theme.text)
        } else {
            box_style(&theme, ground).fg(theme.text)
        };
        buf.set_style(Rect::new(area.x, y, area.width, 1), style);

        // A space either side of the kind, always — `keyword` is seven
        // characters and would otherwise run straight into the label.
        let kind_width = 10.min((area.width as usize / 3).max(4));
        let kind = text::truncate(kind, kind_width.saturating_sub(2));
        buf.set_stringn(
            area.x,
            y,
            format!(" {kind:<w$}", w = kind_width.saturating_sub(1)),
            kind_width,
            style.fg(theme.dim),
        );
        let room = area.width as usize - kind_width - 1;
        let label_width = text::str_width(label).min(room);
        buf.set_stringn(
            area.x + kind_width as u16,
            y,
            text::truncate(label, room),
            room,
            style.fg(if chosen { theme.accent } else { theme.text }),
        );
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
                    style.fg(theme.dim),
                );
            }
        }
    }

    // One line about the chosen one, under the list.
    if let Some(about) = about.filter(|a| !a.is_empty()) {
        let y = area.y + area.height;
        if y < screen.y + screen.height - 1 {
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
    let widest = hover
        .lines
        .iter()
        .map(|line| text::str_width(line))
        .max()
        .unwrap_or(20);
    // Wide enough to read, no wider than the screen, and no wider than a
    // comfortable line of prose.
    let room = screen.width.saturating_sub(4).min(84);
    let width = ((widest + 2) as u16).min(room).max(24.min(room));
    let rows = hover.lines.len().saturating_sub(hover.scroll).clamp(1, 14) as u16;
    let area = beside(screen, at, width, rows + 2);

    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.dim))
        .style(box_style(&theme, ground));
    let inside = block.inner(area);
    frame.render_widget(block, area);

    let buf = frame.buffer_mut();
    for (row, line) in hover
        .lines
        .iter()
        .skip(hover.scroll)
        .take(inside.height as usize)
        .enumerate()
    {
        let (shown, style) = if line == crate::app::RULE {
            (
                crate::app::RULE.repeat(inside.width as usize),
                box_style(&theme, ground).fg(theme.faint()),
            )
        } else {
            (
                text::truncate(line, inside.width as usize),
                box_style(&theme, ground).fg(theme.text),
            )
        };
        buf.set_stringn(
            inside.x,
            inside.y + row as u16,
            &shown,
            inside.width as usize,
            style,
        );
    }
}

fn draw_signature(frame: &mut Frame, app: &mut App, at: Position, ground: Color) {
    let Some(signature) = &app.signature else {
        return;
    };
    let theme = app.theme;
    let screen = app.screen;
    let Some(label) = signature.lines.first() else {
        return;
    };
    let shown = format!(" {} ", text::truncate(label, screen.width as usize - 4));
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

    let title = format!(" {} ", picker.kind.title());
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
            Style::new().bg(theme.bg).fg(theme.dim),
        );
    }
    if !hint.is_empty() && area.width as usize > hint.len() + 6 {
        buf.set_stringn(
            area.x + 2,
            area.y + area.height - 1,
            format!(" {hint} "),
            hint.len() + 2,
            Style::new().bg(theme.bg).fg(theme.dim),
        );
    }

    // The box you type in.
    let query_row = inside.y;
    buf.set_string(inside.x, query_row, "\u{203a} ", Style::new().fg(theme.accent));
    buf.set_stringn(
        inside.x + 2,
        query_row,
        &picker.query,
        inside.width as usize - 2,
        Style::new().fg(theme.text),
    );
    let rule = inside.y + 1;
    for x in inside.x..inside.x + inside.width {
        buf.set_string(x, rule, "\u{2500}", Style::new().fg(theme.faint()));
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
        buf.set_string(list.x + 1, list.y, said, Style::new().fg(theme.dim));
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
                buf.set_stringn(right, y, &shown, width as usize, style.fg(theme.warn));
            }
        }
        if let Some(detail) = detail {
            let room = (right.saturating_sub(x) as usize).saturating_sub(text::str_width(label) + 3);
            if room > 6 {
                let shown = text::truncate(detail, room);
                let width = text::str_width(&shown) as u16;
                right -= width + 1;
                buf.set_stringn(right, y, &shown, width as usize, style.fg(theme.dim));
            }
        }

        // The label, with the letters that matched lit up — the reason this
        // row is on the list, shown rather than left to be guessed at.
        let room = right.saturating_sub(x) as usize;
        let shown = text::truncate(label, room);
        let base = style.fg(theme.text);
        buf.set_stringn(x, y, &shown, room, base);
        for &position in matched {
            let column = x as usize + position as usize;
            if column < right as usize
                && let Some(cell) = buf.cell_mut(Position::new(column as u16, y))
            {
                cell.set_style(
                    base.fg(theme.accent).add_modifier(Modifier::BOLD),
                );
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

    let label = format!(" {} ", prompt.kind.label());
    // The prompt takes the whole row. Setting a style over what the status bar
    // drew would leave its words showing through underneath.
    frame.render_widget(Clear, area);
    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().bg(theme.faint()).fg(theme.text));
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
        Style::new().bg(theme.faint()).fg(theme.text),
    );

    // For a search, how many there are — the thing you actually want to know
    // while typing one.
    if matches!(prompt.kind, PromptKind::Find | PromptKind::ReplaceFind)
        && !prompt.input.is_empty()
    {
        let count = app.count_matches_of(&prompt.input);
        let said = format!(
            " {} ",
            match count {
                0 => "none".to_string(),
                1 => "1 match".to_string(),
                n => format!("{n} matches"),
            }
        );
        let width = text::str_width(&said) as u16;
        if area.width > width + 20 {
            frame.buffer_mut().set_stringn(
                area.x + area.width - width,
                area.y,
                &said,
                width as usize,
                Style::new().bg(theme.faint()).fg(if count == 0 {
                    theme.bad
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
        .border_style(Style::new().fg(theme.warn))
        .style(box_style(&theme, ground));
    let inside = block.inner(area);
    frame.render_widget(block, area);

    let buf = frame.buffer_mut();
    for (row, line) in lines.iter().enumerate() {
        let style = match row {
            0 => box_style(&theme, ground)
                .fg(theme.text)
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
    // Two columns, because the list is long and a terminal is wide.
    let columns = if inside.width >= 90 { 2 } else { 1 };
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
                    box_style(&theme, ground).fg(theme.warn),
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
        box_style(&theme, ground).fg(theme.dim),
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
    use crate::cmd::{ALL, Group};
    let mut lines = Vec::new();
    let groups = [
        (Group::File, "Files"),
        (Group::Edit, "Editing"),
        (Group::Move, "Moving about"),
        (Group::Select, "Selecting"),
        (Group::Search, "Finding"),
        (Group::Code, "Code"),
        (Group::View, "The view"),
        (Group::Help, "Help"),
    ];
    for (group, title) in groups {
        let mut items: Vec<HelpLine> = ALL
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
        ("Click the status bar", "the language, the position and the problems are buttons"),
    ] {
        lines.push(HelpLine::Item(what.into(), does.into()));
    }
    lines
}
