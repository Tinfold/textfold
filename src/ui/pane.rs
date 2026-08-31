//! The text: the panes, the rows in them, the margin down the side, and the
//! bar that says where in the file you are.
//!
//! Everything here is bounded by what is on the screen. Colours are worked out
//! for the visible lines and the rows are walked once, so a two-hundred
//! thousand line file costs what a two-hundred line one does.

use super::*;

pub(super) fn syntax_spans(
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
        .said
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
pub(super) fn outside_of(range: Range, covered: &[(Range, crate::theme::Role)]) -> Vec<Range> {
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
pub(super) fn grammar_spans(
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

pub(super) fn draw_pane(frame: &mut Frame, app: &App, index: usize, ground: Color) -> Option<Position> {
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

    // The area it is actually being drawn in, which is what the pane says
    // less whatever the margin took.
    let layout = Layout::of(view, doc, tab_width).across(area.width as usize);
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
pub(super) struct DrawRow<'a> {
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

/// Where a note drawn into a line goes: the pane, the row, how far along the
/// line the drawing has got, and how much of the left is scrolled off.
pub(super) struct Along {
    area: Rect,
    screen: u16,
    column: usize,
    skip: usize,
    tab_width: usize,
}

/// The notes a server would write into this line at this position — the type
/// of a variable, the name of an argument. Answers how many columns they took.
///
/// Drawn before the character each belongs to, and counted in the width of the
/// line by [`Layout::hints_between`], so that everything from a click to a
/// selection still lands where it looks like it should. Those two have to
/// agree; this is one half of that agreement and the reason it is written
/// where the other half can be read beside it.
pub(super) fn draw_inlays(buf: &mut Buffer, doc: &Document, at: usize, along: Along, style: Style) -> usize {
    let Along {
        area,
        screen,
        mut column,
        skip,
        tab_width,
    } = along;
    let was = column;
    for hint in doc.said.inlays.iter().filter(|hint| hint.at == at) {
        for c in hint.text.chars() {
            let width = text::char_width(c, column, tab_width);
            if column >= skip {
                let x = area.x as usize + column - skip;
                if x >= (area.x + area.width) as usize {
                    break;
                }
                if let Some(cell) = buf.cell_mut(Position::new(x as u16, screen)) {
                    cell.set_style(style).set_char(c);
                }
            }
            column += width;
        }
    }
    column - was
}

/// Write something after the end of a line's text, stopping at the edge of the
/// pane. Answers how many columns it took.
///
/// Three different things are drawn out here — what a server has to say about
/// the line, how much is folded onto it, and what a plugin is offering to
/// type — and none of them is *in* the file. That is what they have in common
/// and why they are drawn the same way: past the end of the text, nothing has
/// to agree with the map from the screen to the characters, because there are
/// no characters out here to point at.
pub(super) fn after_the_text(
    buf: &mut Buffer,
    area: Rect,
    screen: u16,
    at: usize,
    text: &str,
    style: Style,
) -> usize {
    let stop = (area.x + area.width) as usize;
    let mut wrote = 0;
    for (x, c) in (at..stop).zip(text.chars()) {
        if let Some(cell) = buf.cell_mut(Position::new(x as u16, screen)) {
            cell.set_style(style).set_char(c);
        }
        wrote += 1;
    }
    wrote
}

/// Everything the drawing has to say about a character before it paints one,
/// gathered once for a row.
///
/// Two jobs were interleaved here: deciding what a character *looks* like —
/// the colour the grammar or the server gave it, whether it is selected, the
/// same name as the one under the cursor, wrong, or the bracket matching the
/// one you are standing on — and putting that in cells. Only the second is
/// about the screen, and nearly every change to the drawing is a change to
/// one of them rather than both.
pub(super) struct Ink<'a> {
    doc: &'a Document,
    view: &'a View,
    spans: &'a [(Range, crate::theme::Role)],
    /// Everything wrong on this row, worked out once rather than per column.
    problems: Vec<&'a Diagnostic>,
    /// The stretch of a panel the pointer is on, where it is on one.
    hover: Option<Range>,
    /// The bracket matching the one under the cursor.
    partner: Option<usize>,
    /// What this row is painted on, which is the theme's background or the
    /// stripe on the cursor's line.
    ground: Color,
}

impl Ink<'_> {
    /// What this character looks like.
    fn style(&self, at: usize, theme: &Theme) -> Style {
        let selected = self
            .view
            .sel
            .ranges()
            .iter()
            .any(|range| range.contains(at) && !range.is_empty());
        // The other places in this file that are the same thing as the one
        // under the cursor, in the colour that means "these go together" — the
        // same one a selection is drawn in, because it is the same statement.
        // A selection you made has edges you watched appear; this does not
        // need a colour of its own to be told apart from it.
        let same = self.doc.said.highlights.iter().any(|range| range.contains(at));
        // Lit under the pointer, in the colour every other list in textfold
        // uses for the row you are pointing at. The span keeps its own
        // foreground: a button's colour is what says whether it is a frame, a
        // file or a heading, and a highlight that repainted the text would
        // throw that away to say something the background already says.
        let hovered = self.hover.is_some_and(|range| range.contains(at));

        let mut style = Style::new()
            .bg(match selected || hovered || same {
                true => theme.selection,
                false => self.ground,
            })
            .fg(colour_of(self.spans, at, theme));

        if let Some(severity) = self
            .problems
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
        // Both ends of a pair, so standing on one lights the other.
        let bracket = Some(at) == self.partner
            || (self.partner.is_some() && at == self.view.sel.primary().head);
        match bracket {
            true => style.add_modifier(Modifier::BOLD).fg(theme.bracket_match),
            false => style,
        }
    }
}

pub(super) fn draw_row(
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

    let ink = Ink {
        doc,
        view,
        spans: it.spans,
        problems: diagnostics_on(doc, start, end),
        hover: it.hover,
        partner: it.partner,
        ground: line_bg,
    };
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
        let style = Style::new()
            .bg(line_bg)
            .fg(theme.faint)
            .add_modifier(Modifier::ITALIC);
        column += draw_inlays(
            buf,
            doc,
            at,
            Along {
                area,
                screen: it.screen,
                column,
                skip,
                tab_width,
            },
            style,
        );
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
        let style = ink.style(at, theme);

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
            .said
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
            let from = area.x as usize + column.saturating_sub(skip);
            column += after_the_text(buf, area, it.screen, from, &text, style);
        }
    }

    // A line with something folded onto it says so, and says how much, right
    // after its text. Not in the margin: the one column there is already the
    // breakpoint's, and the end of the line is where the eye is anyway when it
    // wonders where the rest of the function went.
    if it.row + 1 == it.rows.len()
        && let Some((_, last)) = layout.folds.iter().find(|(first, _)| *first == it.line)
    {
        let note = fold_mark(last - it.line);
        let style = Style::new()
            .bg(theme.selection)
            .fg(theme.faint)
            .add_modifier(Modifier::ITALIC);
        let from = area.x as usize + column.saturating_sub(skip);
        after_the_text(buf, area, it.screen, from, &note, style);
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
        let ghost = Style::new().bg(line_bg).fg(theme.faint).add_modifier(Modifier::ITALIC);
        // The first line of it. A suggestion is often several, and the rest
        // are counted rather than drawn: rows below this one belong to the
        // file, and borrowing them would move the text under somebody's mouse.
        let first = hint.text.lines().next().unwrap_or_default();
        x += after_the_text(buf, area, it.screen, x, first, ghost);
        let more = hint.text.lines().count().saturating_sub(1);
        if more > 0 {
            let note = format!("  +{}", crate::app::count("line", more));
            after_the_text(buf, area, it.screen, x, &note, ghost);
        }
    }
    cursor_at
}

/// The colour of the character at `at`, from the spans the highlighter gave.
pub(super) fn colour_of(spans: &[(Range, crate::theme::Role)], at: usize, theme: &Theme) -> Color {
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

pub(super) fn severity_colour(severity: Severity, theme: &Theme) -> Color {
    match severity {
        Severity::Error => theme.error,
        Severity::Warning => theme.warning,
        Severity::Info => theme.info,
        Severity::Hint => theme.faint,
    }
}

/// The diagnostics that touch a stretch of a line.
pub(super) fn diagnostics_on(doc: &Document, from: usize, to: usize) -> Vec<&Diagnostic> {
    doc.diagnostics
        .iter()
        .filter(|d| d.range.start() < to.max(from + 1) && d.range.end() >= from)
        .collect()
}

/// One line's worth of margin: which line, whether this is its first folded
/// row (only the first gets a number), and where on the screen it goes.
pub(super) struct Gutter<'a> {
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

pub(super) fn draw_gutter(buf: &mut Buffer, app: &App, view: &View, doc: &Document, it: Gutter) {
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
    format!(" \u{22ef} {} ", crate::app::count("line", hidden))
}

/// Where you asked the debugger to stop. A filled dot, because that is what
/// one is in every debugger anybody has used.
pub(super) const BREAKPOINT_MARK: char = '\u{25cf}';
/// One the adapter would not take: a blank line, a comment, a file it is not
/// running. Hollow, and in the quiet colour, because it is a breakpoint that
/// is not going to happen.
pub(super) const UNSET_BREAKPOINT_MARK: char = '\u{25cb}';
/// Where the program actually is.
pub(super) const STOPPED_MARK: char = '\u{25b6}';
/// Somewhere you said you were coming back to. A diamond, so that it is not
/// mistaken for a breakpoint at a glance — the two share the column.
pub(super) const BOOKMARK_MARK: char = '\u{25c6}';

/// What colour a line's history is drawn in. Green for new, blue for changed,
/// red for gone — the three every diff has used since diffs were in colour.
pub(super) fn git_colour(mark: crate::git::Mark, theme: &Theme) -> Color {
    match mark {
        crate::git::Mark::Added => theme.added,
        crate::git::Mark::Changed => theme.changed,
        crate::git::Mark::Removed => theme.removed,
    }
}

pub(super) fn draw_scrollbar(buf: &mut Buffer, view: &View, doc: &Document, theme: &Theme) {
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
pub(super) fn mark_focus(buf: &mut Buffer, view: &View, theme: &Theme, focused: bool) {
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
