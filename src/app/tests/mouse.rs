//! The mouse: clicks, drags, and what is under the pointer.

use super::*;

#[test]
fn clicking_the_left_of_the_margin_puts_a_breakpoint_there() {
    // The gesture every editor with a debugger in it has, and the reason
    // the mark sits in the blank column the line number is padded with:
    // it costs the numbers no room and it is where the pointer goes.
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree\n");
    app.panes[0].gutter = 6;
    let id = app.view().doc;

    // Row 1 is the first line of the text — the pane starts at y = 1.
    app.click(0, 2, KeyModifiers::NONE);
    assert_eq!(app.doc(id).expect("a buffer").breakpoint_lines(), vec![1]);
    // And clicking it again takes it off.
    app.click(0, 2, KeyModifiers::NONE);
    assert!(app.doc(id).expect("a buffer").breakpoint_lines().is_empty());
}

#[test]
fn clicking_the_line_numbers_still_takes_the_line() {
    // The breakpoint column is one column wide. The rest of the margin
    // goes on meaning what it always meant, or this would be a feature
    // that broke selecting a line by clicking its number.
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree\n");
    app.panes[0].gutter = 6;
    let id = app.view().doc;

    app.click(3, 2, KeyModifiers::NONE);
    assert!(
        app.doc(id).expect("a buffer").breakpoint_lines().is_empty(),
        "clicking a line number is not putting a breakpoint on it"
    );
    assert!(
        !app.view().sel.primary().is_empty(),
        "it should have taken the line"
    );
}

#[test]
fn clicking_away_closes_the_list_of_suggestions() {
    let (mut app, _rx) = editor();
    typed(&mut app, "Ha");
    suggested(
        &mut app,
        2,
        false,
        json!([{ "label": "Handle" }, { "label": "Hasty" }]),
    );
    assert!(app.completion.is_some(), "nothing was suggested to close");

    // Somewhere in the text, well away from where the list was drawn.
    app.click(20, 12, KeyModifiers::NONE);
    assert!(
        app.completion.is_none(),
        "the list is still there over a word nobody is typing any more"
    );
}

#[test]
fn clicking_a_suggestion_still_takes_it() {
    // The other half: closing on a click away must not close it on the
    // click that was choosing something from it.
    let (mut app, _rx) = editor();
    typed(&mut app, "Ha");
    suggested(
        &mut app,
        2,
        false,
        json!([{ "label": "Handle" }, { "label": "Hasty" }]),
    );
    // Where the drawing would have put it.
    let list = app.completion.as_mut().expect("a list");
    list.area = Rect::new(4, 2, 24, 2);

    app.click(6, 3, KeyModifiers::NONE);
    assert!(app.completion.is_none(), "the list stayed open");
    assert_eq!(app.here().rope.to_string().trim_end(), "Hasty");
}

#[test]
fn right_clicking_closes_the_list_of_suggestions() {
    let (mut app, _rx) = editor();
    typed(&mut app, "Ha");
    suggested(&mut app, 2, false, json!([{ "label": "Handle" }]));
    app.right_click(1, 1);
    assert!(app.completion.is_none());
    assert!(matches!(app.overlay, Overlay::Menu(_)), "no menu opened");
}

#[test]
fn right_clicking_inside_a_selection_keeps_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "one two three");
    app.run(Cmd::SELECT_ALL);
    let was = app.view().sel.primary();

    // Somewhere in the middle of the line, which is inside the selection.
    app.right_click(10, 1);
    assert!(matches!(app.overlay, Overlay::Menu(_)), "no menu opened");
    assert_eq!(
        app.view().sel.primary(),
        was,
        "the selection was thrown away"
    );
}

#[test]
fn a_tab_held_against_the_end_of_a_full_row_keeps_moving() {
    let (mut app, _rx) = editor();
    for _ in 0..4 {
        app.run(Cmd::NEW);
    }
    let id = app.view().doc;
    let was = app.docs.iter().position(|d| d.id == id).expect("open");
    assert!(was > 0, "there is nowhere to move it from");

    // The row as the drawing would have left it with more tabs than fit:
    // an arrow at the left end, whose scroll target is behind where the
    // row is now, and the pointer holding the tab over it.
    app.tab_scroll = 8;
    app.hits.nudges = vec![(Rect::new(0, 0, 1, 1), 0)];
    app.drag = Some(Drag::Tab {
        id,
        at: (0, 0),
        stepped: Instant::now() - TAB_STEP_EVERY,
    });

    app.tick();
    assert_eq!(
        app.docs.iter().position(|d| d.id == id),
        Some(was - 1),
        "holding a tab against the arrow did not walk it along"
    );

    // And not again straight away: it walks at a pace you can stop at.
    app.tick();
    assert_eq!(app.docs.iter().position(|d| d.id == id), Some(was - 1));
}

#[test]
fn a_tab_held_against_the_end_does_not_walk_off_it() {
    let (mut app, _rx) = editor();
    app.run(Cmd::NEW);
    let id = app.docs[0].id;
    app.show(id);
    app.tab_scroll = 4;
    app.hits.nudges = vec![(Rect::new(0, 0, 1, 1), 0)];
    app.drag = Some(Drag::Tab {
        id,
        at: (0, 0),
        stepped: Instant::now() - TAB_STEP_EVERY,
    });
    app.tick();
    assert_eq!(
        app.docs.iter().position(|d| d.id == id),
        Some(0),
        "the first tab was moved before the first tab"
    );
}

#[test]
fn clicking_what_a_panel_offers_does_not_leave_a_caret_in_it() {
    // Clicking a button opened a file, stepped a program, folded a tree —
    // and then dropped a text caret in the middle of its label, in a
    // buffer that cannot be typed into. It also moved the one thing the
    // keyboard uses to pick a row, so a click quietly changed what Enter
    // would do next.
    let (mut app, _rx) = editor();
    a_panel(&mut app);
    let area = app.panes[app.focus].area;
    let click = |app: &mut App, column| {
        app.handle(Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        })));
    };

    click(&mut app, area.x + 8);
    assert_eq!(
        app.view().sel.primary().head,
        0,
        "clicking a button moved the caret onto its label"
    );
    // But the panel is still a buffer you can put the caret in on purpose,
    // which is how the keyboard picks a row at all.
    click(&mut app, area.x + 2);
    assert_eq!(app.view().sel.primary().head, 2);
}

#[test]
fn a_drag_out_of_one_pane_stays_in_the_pane_it_began_in() {
    // The crash this is about, exactly as it was reported: press in the
    // debugger's panel, drag up into the source file, and textfold is
    // gone. `position_at` answered with whichever pane the pointer was
    // over, so an offset four thousand characters into `main.c` became
    // the selection head of a panel holding four hundred — and the next
    // frame asked the rope for a slice that is not there.
    //
    // Nothing about it was specific to the debugger. Any two panes
    // whose buffers are different lengths would do it, and the panel is
    // merely where somebody is most likely to drag out of, being short
    // and full of things that look clickable.
    let (mut app, _rx) = editor();
    let long = scratch("dragged-into.txt");
    std::fs::write(&long, "a line of text\n".repeat(400)).expect("written");
    app.open_path(&long);
    app.split();
    // The pane the drag starts in holds far less text than the one the
    // pointer ends over, which is the whole of the setup.
    let short = app.new_scratch();
    if let Some(doc) = app.doc_mut(short) {
        doc.name = "Debug".into();
    }
    app.show(short);
    let (from, onto) = (app.focus, 1 - app.focus);
    let small = app.doc(short).expect("a buffer").len_chars();

    let mouse = |app: &mut App, kind, column, row| {
        app.handle(Event::Term(TermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })));
    };
    let area = app.panes[from].area;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x,
        area.y,
    );
    assert_eq!(app.focus, from, "the press did not land where it was aimed");
    let over = app.panes[onto].area;
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        over.x + over.width / 2,
        over.y + over.height - 1,
    );

    // Both ends of what is selected are inside the buffer the drag began
    // in. Anything else is a panic on the next frame rather than a
    // selection that looks slightly wrong.
    let range = app.panes[from].sel.primary();
    assert!(
        range.anchor <= small && range.head <= small,
        "the drag took a position from the other pane: {range:?} in {small} characters"
    );
    // And laying it out is the moment the old bug went off, so it is laid
    // out: `Layout::place` is what the drawing asks where the cursor is,
    // and what used to take the rope past its end.
    let doc = app.doc(short).expect("a buffer");
    let layout = crate::view::Layout {
        wrap: true,
        ..crate::view::Layout::of(&app.panes[from], doc, 4)
    };
    layout.place(range.head);
    layout.place(range.anchor);

    std::fs::remove_file(&long).ok();
}

#[test]
fn letting_go_of_a_tab_ends_the_drag() {
    let (mut app, _rx) = editor();
    app.run(Cmd::NEW);
    let id = app.view().doc;
    app.drag = Some(Drag::Tab {
        id,
        at: (0, 0),
        stepped: Instant::now(),
    });
    assert_eq!(app.dragging_tab(), Some(id));
    app.handle(Event::Term(TermEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })));
    assert!(app.drag.is_none(), "the tab is still being carried");
    assert_eq!(app.dragging_tab(), None);
}

#[test]
fn right_clicking_a_tab_offers_things_about_that_tab() {
    let (mut app, _rx) = editor();
    let path = scratch("tab-menu.txt");
    std::fs::write(&path, "text\n").expect("written");
    app.open_path(&path);
    let id = app.view().doc;
    app.hits.tabs = vec![(Rect::new(0, 0, 10, 1), id, false)];

    app.right_click(3, 0);
    let Overlay::Menu(menu) = &app.overlay else {
        panic!("no menu");
    };
    assert!(
        menu.items
            .iter()
            .any(|i| matches!(i.action, crate::menu::Action::RunOn(_, Cmd::CLOSE_OTHERS))),
        "a tab menu with nothing about tabs in it"
    );
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn the_divider_between_two_panes_can_be_pulled_either_way() {
    let (mut app, _rx) = editor();
    app.screen = Rect::new(0, 0, 100, 30);
    app.run(Cmd::SPLIT);
    assert_eq!(app.panes.len(), 2);
    // What the drawing would have worked out: two equal halves.
    app.panes[0].frame = Rect::new(0, 1, 50, 28);
    app.panes[1].frame = Rect::new(50, 1, 50, 28);

    // Pull the divider right: the first pane grows, the second gives.
    app.pull_divider(1, 70, 10);
    assert_eq!(app.panes[0].share, 70.0);
    assert_eq!(app.panes[1].share, 30.0);
    // Which is a proportion, not a column count — the same drag in a
    // narrower terminal keeps the same split.
    assert_eq!(
        crate::ui::share_out(
            &[app.panes[0].share, app.panes[1].share],
            50,
            crate::ui::MIN_PANE
        ),
        vec![35, 15]
    );

    // And neither side can be pulled shut.
    app.panes[0].frame = Rect::new(0, 1, 70, 28);
    app.panes[1].frame = Rect::new(70, 1, 30, 28);
    app.pull_divider(1, 0, 10);
    assert_eq!(app.panes[0].share, crate::ui::MIN_PANE as f32);
    assert_eq!(app.panes[1].share, (100 - crate::ui::MIN_PANE) as f32);

    // Dragging the leading edge of the *first* pane is not a divider at
    // all — there is nothing on the other side of it.
    let was = app.panes[0].share;
    app.pull_divider(0, 40, 10);
    assert_eq!(app.panes[0].share, was);
}

#[test]
fn a_sidebar_can_be_pulled_wider_and_narrower() {
    let (mut app, _rx) = editor();
    app.screen = Rect::new(0, 0, 100, 30);
    app.open_panel(docked_panel("files/tree", Some("left"), Some(30)));
    // What the drawing would have worked out, since a drag measures
    // against where the pane actually is.
    app.panes[0].frame = Rect::new(0, 1, 30, 28);

    app.resize_dock(0, 44, 10);
    assert_eq!(app.panes[0].dock.map(|d| d.size), Some(45));

    app.panes[0].frame = Rect::new(0, 1, 45, 28);
    app.resize_dock(0, 14, 10);
    assert_eq!(app.panes[0].dock.map(|d| d.size), Some(15));

    // Never down to nothing, and never so wide the middle is squeezed out
    // — a width that only looked right because the layout clamped it is a
    // width that springs back the moment the terminal is resized.
    app.resize_dock(0, 0, 10);
    assert_eq!(app.panes[0].dock.map(|d| d.size), Some(MIN_DOCK));
    app.panes[0].frame = Rect::new(0, 1, MIN_DOCK, 28);
    app.resize_dock(0, 99, 10);
    assert_eq!(
        app.panes[0].dock.map(|d| d.size),
        Some(100 - MIN_MIDDLE_ROOM)
    );
}

#[test]
fn alt_dragging_selects_the_same_columns_on_every_line() {
    let (mut app, _rx) = editor();
    typed(&mut app, "abcdef\nabcdef\nabcdef\n");
    let area = app.panes[app.focus].area;
    let mouse = |app: &mut App, kind, column, row| {
        app.handle(Event::Term(TermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::ALT,
        })));
    };
    // From column 1 of the first line to column 4 of the third.
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 1,
        area.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        area.x + 4,
        area.y + 2,
    );

    let sel = &app.panes[app.focus].sel;
    assert_eq!(sel.len(), 3, "a cursor on each line");
    let rope = &app.here().rope;
    for range in sel.ranges() {
        assert_eq!(
            rope.slice(range.start()..range.end()).to_string(),
            "bcd",
            "the same columns on every line"
        );
    }
}

#[test]
fn a_block_over_a_short_line_puts_a_cursor_at_the_end_of_it() {
    // What makes "type at the end of all of these" work on ragged text.
    let (mut app, _rx) = editor();
    typed(&mut app, "long line here\nshort\nlong line here\n");
    let area = app.panes[app.focus].area;
    let mouse = |app: &mut App, kind, column, row| {
        app.handle(Event::Term(TermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::ALT,
        })));
    };
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 10,
        area.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        area.x + 12,
        area.y + 2,
    );
    let sel = &app.panes[app.focus].sel;
    assert_eq!(sel.len(), 3);
    let rope = &app.here().rope;
    let middle = sel.ranges()[1];
    assert!(middle.is_empty(), "the short line gets a bare cursor");
    assert_eq!(
        text::line_of(rope, middle.head),
        1,
        "and it is on that line, not on the one after"
    );
    assert_eq!(middle.head, text::line_end(rope, 1), "at the end of it");
}
