//! The row at the top and the row at the bottom.
//!
//! The tabs, and the status bar — which is not a status *bar* so much as a row
//! of buttons that happen to be telling you things: the language opens the
//! language list, the position opens "go to line", the count of problems opens
//! the problem list.

use super::*;

pub(super) fn draw_tabs(frame: &mut Frame, app: &mut App, area: Rect, ground: Color) {
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
        app.hits.forget();
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
    app.hits.tabs = tabs
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
    app.hits.nudges.clear();
    let starts = || tabs.iter().map(|(_, _, _, at, _)| *at);
    if scroll > 0 {
        let back = starts().rfind(|at| *at < scroll).unwrap_or(0);
        arrow(buf, area.x, area.y, '\u{2039}', theme);
        app.hits.nudges.push((Rect::new(area.x, area.y, 1, 1), back));
    }
    if scroll < furthest {
        let on = starts()
            .find(|at| *at > scroll)
            .unwrap_or(furthest)
            .min(furthest);
        let x = area.x + window - 1;
        arrow(buf, x, area.y, '\u{203a}', theme);
        app.hits.nudges.push((Rect::new(x, area.y, 1, 1), on));
    }
    let _ = ground;
}

/// What a tab has to say about its file beyond its name: whether it is worth
/// looking at, and what the one column at its right edge should be.
pub(super) struct TabState {
    modified: bool,
    on_disk: crate::doc::OnDisk,
    /// The worst thing a language server has said about it, if anything.
    worst: Option<Severity>,
}

pub(super) fn tab_state(doc: &Document) -> TabState {
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
pub(super) fn arrow(buf: &mut Buffer, x: u16, y: u16, mark: char, theme: Theme) {
    if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
        cell.set_char(mark).set_style(
            Style::new()
                .bg(theme.chrome())
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    }
}

pub(super) fn draw_status(frame: &mut Frame, app: &mut App, area: Rect, ground: Color) {
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
            said.push_str(&crate::app::count("error", errors));
        }
        if warnings > 0 {
            if !said.is_empty() {
                said.push_str(", ");
            }
            said.push_str(&crate::app::count("warning", warnings));
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
    if let Some(fixes) = app.fixes.found.as_ref().filter(|f| f.doc == doc.id)
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
    app.hits.status = hits;
}
