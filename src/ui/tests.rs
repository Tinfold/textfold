//! What actually reaches the screen.
//!
//! These draw onto a buffer and read it back, so they are facts about pixels
//! rather than about state — which is the only way to test a thing whose whole
//! job is what it looks like.

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
    for (area, seen, _) in &app.hits.tabs {
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
    assert_eq!(app.hits.nudges.len(), 2, "one arrow at each end");

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
    assert!(app.hits.nudges.is_empty());
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
