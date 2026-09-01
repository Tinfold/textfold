//! What floats over the top: the suggestion list, the hover, the signature,
//! the context menu, the fuzzy list, the prompt, the question and the help.
//!
//! All of them are drawn last and none of them is part of the text, so they
//! are the one place in the drawing where the map from the screen to the
//! characters does not have to hold.

use super::*;

use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear};
use unicode_segmentation::UnicodeSegmentation;

pub(super) fn draw_floating(frame: &mut Frame, app: &mut App, ground: Color) -> Option<Position> {
    // Beside the cursor first, then over the middle: a suggestion belongs
    // where you are typing, and a list belongs where you can read it.
    let at = cursor_screen(app);
    if let Some(at) = at {
        draw_signature(frame, app, at, ground);
        draw_completion(frame, app, at, ground);
        draw_hover(frame, app, at, ground);
    }

    // Under everything else that floats, and above the text: a label about a
    // tab is the smallest thing on the screen and the least entitled to be in
    // the way of a list somebody opened.
    draw_tip(frame, app, ground);

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
pub(super) fn cursor_screen(app: &App) -> Option<Position> {
    screen_position_of(app, app.view().sel.primary().head)
}

/// The same, as a plain pair, for anything that wants a place to hang a box
/// off rather than a place to put the caret.
pub fn cursor_cell(app: &App) -> Option<(u16, u16)> {
    cursor_screen(app).map(|at| (at.x, at.y))
}

/// Where a place in the file is on the screen. `None` for one scrolled out of
/// sight, which is what tells a popup about that place not to be drawn.
pub(super) fn screen_position_of(app: &App, at: usize) -> Option<Position> {
    let view = app.view();
    let doc = app.doc(view.doc)?;
    let layout = Layout::of(view, doc, app.config.tab_width());
    let at = at.min(doc.len_chars());
    let row = view::screen_row(view, &layout, at)?;
    let (_, column) = layout.place(at);
    let x = view.area.x as usize + column.saturating_sub(if view.wrap { 0 } else { view.left });
    (x < (view.area.x + view.area.width) as usize)
        .then(|| Position::new(x as u16, view.area.y + row as u16))
}

/// A box of `width` by `height` that sits beside `at` without falling off any
/// edge: below where there is room, above where there is not.
pub(super) fn beside(screen: Rect, at: Position, width: u16, height: u16) -> Rect {
    let width = width.min(screen.width);
    let height = height.min(screen.height.saturating_sub(2));
    let below = at.y + 1;
    let y = if below + height < screen.y + screen.height {
        below
    } else {
        at.y.saturating_sub(height).max(screen.y)
    };
    let x = at.x.min(screen.x + screen.width.saturating_sub(width));
    Rect::new(x, y, width, height)
}

pub(super) fn box_style(theme: &Theme, ground: Color) -> Style {
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
pub(super) fn right_of(item: &crate::app::Suggestion) -> Option<String> {
    match (item.also.is_empty(), &item.detail) {
        (true, detail) => detail.clone(),
        (false, Some(detail)) => Some(format!("+ {detail}")),
        (false, None) => Some("+ import".to_string()),
    }
}

/// One line of the completion list, as it will be drawn.
pub(super) struct Row {
    label: String,
    suffix: Option<String>,
    detail: Option<String>,
    kind: &'static str,
    /// The colour the kind is drawn in: the one that kind of thing has in the
    /// file. See [`crate::app::Suggestion::role`].
    role: crate::theme::Role,
}

pub(super) fn draw_completion(frame: &mut Frame, app: &mut App, at: Position, ground: Color) {
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

/// The one-line label for whatever chrome the pointer has come to rest on.
///
/// Drawn against the thing it is about rather than against the cursor, and
/// kept on the screen: a tab near the right-hand edge would otherwise hang its
/// label off the side, which is where the half of the path you wanted is.
pub(super) fn draw_tip(frame: &mut Frame, app: &mut App, ground: Color) {
    let Some(tip) = &app.tip else { return };
    // One box at a time, and a suggestion or a documentation box is the one
    // being read.
    if app.completion.is_some() || app.hover.is_some() {
        return;
    }
    let theme = app.theme;
    let screen = app.screen;
    let room = screen.width.saturating_sub(2) as usize;
    let text = elide_start(&tip.text, room);
    let width = (text::str_width(&text) + 2) as u16;
    let at = Position::new(tip.about.x, tip.about.y + tip.about.height - 1);
    let area = beside(screen, at, width, 3);

    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.faint))
        .style(box_style(&theme, ground));
    let inside = block.inner(area);
    frame.render_widget(block, area);
    frame.buffer_mut().set_stringn(
        inside.x,
        inside.y,
        &text,
        inside.width as usize,
        Style::new().fg(theme.foreground),
    );
}

/// Cut a string to fit by taking off the front, which is what a path wants:
/// the end of it is the file, and the file is the part you are asking about.
fn elide_start(text: &str, width: usize) -> String {
    if text::str_width(text) <= width || width < 2 {
        return text::truncate(text, width);
    }
    let mut out = String::new();
    let mut used = 0;
    for g in text.graphemes(true).rev() {
        let w = text::str_width(g).max(1);
        if used + w > width - 1 {
            break;
        }
        out.insert_str(0, g);
        used += w;
    }
    format!("…{out}")
}

pub(super) fn draw_hover(frame: &mut Frame, app: &mut App, at: Position, ground: Color) {
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
pub(super) static TAIL: (std::ops::Range<usize>, crate::theme::Role) =
    (usize::MAX..usize::MAX, crate::theme::Role::Variable);

pub(super) fn draw_signature(frame: &mut Frame, app: &mut App, at: Position, ground: Color) {
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
pub(super) struct Shown {
    label: String,
    detail: Option<String>,
    tag: Option<String>,
    key: Option<String>,
    severity: Option<Severity>,
    matched: Vec<u32>,
}

/// The context menu: a short list where the pointer was.
pub(super) fn draw_menu(frame: &mut Frame, app: &mut App, ground: Color) {
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

pub(super) fn draw_picker(frame: &mut Frame, app: &mut App, ground: Color) -> Option<Position> {
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

pub(super) fn draw_prompt(frame: &mut Frame, app: &mut App) -> Option<Position> {
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

pub(super) fn draw_confirm(frame: &mut Frame, app: &mut App, ground: Color) {
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

pub(super) fn draw_help(frame: &mut Frame, app: &mut App, ground: Color) {
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

pub(super) enum HelpLine {
    Heading(String),
    Item(String, String),
    Blank,
}

/// The help, built from the bindings actually in force — so a rebound key
/// shows up here rather than a lie about what textfold shipped with.
pub(super) fn help_lines(app: &App) -> Vec<HelpLine> {
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
