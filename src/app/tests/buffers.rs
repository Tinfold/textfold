//! Files, tabs, what is on the disk, and what was open last time.

use super::*;

#[test]
fn what_is_open_and_where_you_are_in_it_is_what_gets_written_down() {
    let (mut app, _rx) = editor();
    let one = scratch("session-one.rs");
    let two = scratch("session-two.rs");
    std::fs::write(&one, "fn a() {}\nfn b() {}\n").unwrap();
    std::fs::write(&two, "// notes\n").unwrap();
    app.open_path(&one);
    app.open_path(&two);
    // Back to the first, and down a line in it.
    let first = app.docs[0].id;
    app.show(first);
    app.go_to(1, 3);

    let session = app.session();
    let paths: Vec<&std::path::Path> =
        session.tabs.iter().map(|t| t.path.as_path()).collect();
    assert_eq!(paths, [one.as_path(), two.as_path()], "tab order");
    assert_eq!((session.tabs[0].line, session.tabs[0].column), (1, 3));
    // One pane, showing the file it is showing.
    assert_eq!(session.panes.len(), 1);
    assert_eq!(session.panes[0].tab, 0);
    std::fs::remove_file(&one).ok();
    std::fs::remove_file(&two).ok();
}

#[test]
fn a_session_opens_the_tabs_again_where_they_were() {
    let one = scratch("restore-one.rs");
    let two = scratch("restore-two.rs");
    std::fs::write(&one, "a\nb\nc\nd\n").unwrap();
    std::fs::write(&two, "x\ny\n").unwrap();

    let (mut app, _rx) = editor();
    let session = crate::session::Session {
        tabs: vec![
            crate::session::Tab {
                path: one.clone(),
                line: 2,
                column: 0,
            },
            crate::session::Tab {
                path: two.clone(),
                line: 1,
                column: 1,
            },
        ],
        panes: Vec::new(),
        focus: 0,
        side_by_side: true,
        at: 0,
        docks: Vec::new(),
    };
    assert_eq!(app.apply_session(&session, false), 2);

    let names: Vec<&str> = app.docs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["restore-one.rs", "restore-two.rs"]);
    // The last one opened is the one you are looking at, and it is where
    // it was left.
    assert_eq!(app.here().name, "restore-two.rs");
    assert_eq!(app.here().point_at_char(app.view().cursor()), (1, 1));
    // And the one behind it kept its own place, which is the whole point
    // of writing a line down per tab rather than one for the session.
    let first = app.docs[0].id;
    app.show(first);
    assert_eq!(app.here().point_at_char(app.view().cursor()), (2, 0));

    std::fs::remove_file(&one).ok();
    std::fs::remove_file(&two).ok();
}

#[test]
fn a_file_that_has_gone_since_is_skipped_rather_than_made_again() {
    let here = scratch("restore-here.rs");
    std::fs::write(&here, "still here\n").unwrap();
    let gone = scratch("restore-gone.rs");
    std::fs::remove_file(&gone).ok();

    let (mut app, _rx) = editor();
    let session = crate::session::Session {
        tabs: vec![
            crate::session::Tab {
                path: gone.clone(),
                line: 0,
                column: 0,
            },
            crate::session::Tab {
                path: here.clone(),
                line: 0,
                column: 0,
            },
        ],
        ..crate::session::Session::default()
    };
    assert_eq!(app.apply_session(&session, false), 1);
    let names: Vec<&str> = app.docs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["restore-here.rs"]);
    std::fs::remove_file(&here).ok();
}

#[test]
fn coming_back_to_a_tab_comes_back_to_where_you_were_in_it() {
    let (mut app, _rx) = editor();
    let one = scratch("place-one.txt");
    let two = scratch("place-two.txt");
    std::fs::write(&one, "line\n".repeat(400)).expect("written");
    std::fs::write(&two, "other\n".repeat(400)).expect("written");

    app.open_path(&one);
    app.go_to_line(300);
    let (top, at) = (app.view().top, app.view().cursor());
    assert!(top > 0, "line 300 of 400 is not at the top of the screen");

    app.open_path(&two);
    assert_eq!(
        app.view().top,
        0,
        "a file never seen before opens at the top"
    );

    app.open_path(&one);
    assert_eq!(app.view().top, top, "the view came back somewhere else");
    assert_eq!(
        app.view().cursor(),
        at,
        "the cursor came back somewhere else"
    );
    std::fs::remove_dir_all(one.parent().unwrap()).ok();
}

#[test]
fn a_file_written_by_something_else_is_noticed_and_read_again() {
    let (mut app, _rx) = editor();
    let path = scratch("changed-underneath.txt");
    std::fs::write(&path, "before\n").expect("written");
    app.open_path(&path);
    assert_eq!(app.here().on_disk, OnDisk::Same);

    // A formatter, a `git checkout`, the same file open next door.
    std::fs::write(&path, "after\n").expect("written");
    // Twice: one look cannot tell a file that has just changed from one
    // that is halfway through being written, so nothing is read until it
    // has looked the same twice. The second look comes a quarter of a
    // second later rather than a whole cycle — see `SETTLE_CHECK_EVERY`.
    app.check_disk();
    assert!(app.unsettled, "the first sighting is not enough");
    assert_eq!(app.here().rope.to_string(), "before\n");
    app.check_disk();

    assert!(!app.unsettled);
    assert_eq!(app.here().rope.to_string(), "after\n");
    assert!(
        !app.here().is_modified(),
        "reading a file is not editing it"
    );
    assert_eq!(app.here().on_disk, OnDisk::Same);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn a_file_with_unsaved_changes_is_marked_rather_than_overwritten() {
    let (mut app, _rx) = editor();
    let path = scratch("clash.txt");
    std::fs::write(&path, "before\n").expect("written");
    app.open_path(&path);
    typed(&mut app, "mine ");

    std::fs::write(&path, "theirs\n").expect("written");
    app.check_disk();

    assert!(
        app.here().rope.to_string().starts_with("mine "),
        "unsaved work was thrown away"
    );
    assert_eq!(app.here().on_disk, OnDisk::Changed);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn a_file_that_has_gone_is_not_read_as_an_empty_one() {
    let (mut app, _rx) = editor();
    let path = scratch("vanishing.txt");
    std::fs::write(&path, "here for now\n").expect("written");
    app.open_path(&path);
    std::fs::remove_file(&path).expect("removed");
    app.check_disk();

    assert_eq!(app.here().on_disk, OnDisk::Gone);
    assert_eq!(app.here().rope.to_string(), "here for now\n");
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn re_reading_a_file_can_be_undone() {
    let (mut app, _rx) = editor();
    let path = scratch("undo-reload.txt");
    std::fs::write(&path, "one\n").expect("written");
    app.open_path(&path);
    std::fs::write(&path, "two\n").expect("written");
    app.do_reload(app.view().doc);
    assert_eq!(app.here().rope.to_string(), "two\n");

    app.run(Cmd::UNDO);
    assert_eq!(app.here().rope.to_string(), "one\n");
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn closing_the_other_tabs_keeps_the_one_you_are_in() {
    let (mut app, _rx) = editor();
    for name in ["a.txt", "b.txt", "c.txt"] {
        let path = scratch(name);
        std::fs::write(&path, "text\n").expect("written");
        app.open_path(&path);
    }
    let here = app.view().doc;
    assert_eq!(app.docs().len(), 3);
    app.run(Cmd::CLOSE_OTHERS);
    assert_eq!(app.docs().len(), 1);
    assert_eq!(app.view().doc, here);
    std::fs::remove_dir_all(scratch("a.txt").parent().unwrap()).ok();
}

#[test]
fn closing_everything_leaves_unsaved_work_open() {
    let (mut app, _rx) = editor();
    let saved = scratch("saved.txt");
    std::fs::write(&saved, "on disk\n").expect("written");
    app.open_path(&saved);
    app.run(Cmd::NEW);
    typed(&mut app, "not saved anywhere");

    app.run(Cmd::CLOSE_ALL);
    let left: Vec<String> = app.docs().iter().map(|d| d.name.clone()).collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(app.here().is_modified());
    std::fs::remove_dir_all(saved.parent().unwrap()).ok();
}

#[test]
fn comparing_two_panes_marks_what_differs_on_both_sides() {
    let (mut app, _rx, dir) = two_panes("dcmp", "one\ntwo\nthree\n", "one\nextra\ntwo\nthree\n");
    app.run(Cmd::DIFF_PANES);
    let diff = app.diff.as_ref().expect("nothing was compared");
    assert!(!diff.same());
    let (left, right) = diff.panes();
    assert_eq!(
        diff.mark(right, 1),
        Some(crate::git::Mark::Added),
        "the line only the right has was not marked"
    );
    assert!(
        (0..3).any(|line| diff.mark(left, line).is_some()),
        "the left said nothing about a line it is missing"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn closing_a_pane_ends_the_comparison() {
    let (mut app, _rx, dir) = two_panes("dclose", "one\n", "two\n");
    app.run(Cmd::DIFF_PANES);
    assert!(app.diff.is_some());
    app.run(Cmd::CLOSE_PANE);
    app.tick();
    assert!(app.diff.is_none(), "a comparison of one pane");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_tab_menu_offers_moving_it_and_says_where_it_cannot_go() {
    let (mut app, _rx) = editor();
    app.run(Cmd::NEW);
    app.run(Cmd::NEW);
    let first = app.docs[0].id;
    let menu = app.tab_menu(first, (0, 0));
    let row = |cmd: Cmd| {
        menu.items
            .iter()
            .find(|i| i.action == crate::menu::Action::RunOn(first, cmd))
            .unwrap_or_else(|| panic!("no row for {cmd:?}"))
    };
    assert!(
        !row(Cmd::MOVE_TAB_LEFT).enabled,
        "the first tab was offered a move left"
    );
    assert!(row(Cmd::MOVE_TAB_RIGHT).enabled);
}

#[test]
fn typing_and_saving_puts_the_text_on_disk() {
    let (mut app, _rx) = editor();
    let path = scratch("typed.txt");
    std::fs::remove_file(&path).ok();
    app.open_path(&path);
    typed(&mut app, "hello\nworld");
    app.save(None);
    assert_eq!(
        std::fs::read_to_string(&path).expect("written"),
        "hello\nworld\n"
    );
    assert!(!app.here().is_modified());
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn opening_a_file_twice_shows_the_one_already_open() {
    let (mut app, _rx) = editor();
    let path = scratch("once.txt");
    std::fs::write(&path, "content\n").expect("written");
    app.open_path(&path);
    let first = app.view().doc;
    app.run(Cmd::NEW);
    app.open_path(&path);
    assert_eq!(app.view().doc, first);
    assert_eq!(
        app.docs()
            .iter()
            .filter(|d| d.path.as_deref() == Some(path.as_path()))
            .count(),
        1
    );
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn closing_the_last_buffer_leaves_one_to_type_in() {
    let (mut app, _rx) = editor();
    app.run(Cmd::CLOSE_FORCE);
    assert_eq!(app.docs().len(), 1);
    assert!(!app.quit);
}

#[test]
fn a_file_being_written_to_is_left_alone_until_it_stops() {
    let (mut app, _rx) = editor();
    let path = scratch("busy.txt");
    std::fs::write(&path, "one\n").expect("written");
    app.open_path(&path);
    let id = app.view().doc;

    // Changing every time we look. Nothing is taken, and the editor says
    // it wants to look again sooner.
    for n in 1..5 {
        std::fs::write(&path, format!("{}\n", "line\n".repeat(n))).expect("written");
        app.check_disk();
        assert!(app.unsettled, "it was still moving");
        assert_eq!(app.here().rope.to_string(), "one\n", "and was not taken");
    }

    // It stops. The next look sees it twice the same and takes it.
    app.check_disk();
    assert!(!app.unsettled);
    assert_eq!(app.doc(id).map(|d| d.rope.to_string()).as_deref(), Some("line\nline\nline\nline\n\n"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn reverting_where_nothing_changed_says_so() {
    let (mut app, _rx) = editor();
    let path = scratch("reverting-nothing.txt");
    std::fs::write(&path, "one\ntwo\n").expect("written");
    app.open_path(&path);
    let id = app.view().doc;
    app.git.remember_baseline(id, "one\ntwo\n".into());
    app.run(Cmd::REVERT_HUNK);
    assert_eq!(app.status.text, "nothing has changed on this line");
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_log_being_appended_to_leaves_the_cursor_where_it_was() {
    // The bug this is about: a re-read replaced the whole buffer, and every
    // position inside an edit is carried to the end of what replaced it — so
    // each time a log grew, every cursor, bookmark and breakpoint in it
    // landed on the last character and the view followed. Reading somewhere
    // in the middle of a log was impossible while it was being written.
    let (mut app, _rx) = editor();
    let path = scratch("appended.log");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("written");
    app.open_path(&path);
    let id = app.view().doc;

    // Somebody reading the middle of it, with a bookmark on that line.
    app.go_to(2, 0);
    let was = app.view().cursor();
    if let Some(doc) = app.doc_mut(id) {
        doc.bookmarks = vec![was];
    }

    // The service writes another line. Twice round, because one look cannot
    // tell a file that has just changed from one that is still changing.
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\nsix\n").expect("written");
    app.check_disk();
    app.check_disk();

    assert_eq!(
        app.here().rope.to_string(),
        "one\ntwo\nthree\nfour\nfive\nsix\n",
        "the new line was not taken"
    );
    assert_eq!(app.view().cursor(), was, "the cursor was dragged to the end");
    assert_eq!(app.doc(id).map(|d| d.bookmarks.clone()), Some(vec![was]));
    assert_eq!(line_now(&app), 2, "and is still on the line it was reading");
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_file_read_again_and_again_does_not_grow_a_history_of_it() {
    // A buffer on a file something else keeps writing takes an edit every
    // time it is read, on a timer, for as long as it is open. Nothing about
    // that is a thing anybody will undo, and without a bound a log tailed for
    // an afternoon holds an afternoon of it.
    let (mut app, _rx) = editor();
    let path = scratch("growing.log");
    std::fs::write(&path, "start\n").expect("written");
    app.open_path(&path);
    let id = app.view().doc;

    let mut text = String::from("start\n");
    for n in 0..40 {
        text.push_str(&format!("line {n}\n"));
        std::fs::write(&path, &text).expect("written");
        app.check_disk();
        app.check_disk();
    }
    assert_eq!(app.here().rope.to_string(), text, "it kept up");
    assert!(
        !app.here().is_modified(),
        "reading a file is not editing it"
    );

    // And each round was one small edit rather than a replacement of the
    // whole buffer, so undoing the lot walks back up the file a line at a
    // time instead of restoring forty whole copies of it. What bounds the
    // number kept is `MAX_REVISIONS`, tested where the history lives.
    let _ = id;
    std::fs::remove_file(&path).ok();
}
