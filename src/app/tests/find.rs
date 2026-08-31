//! Searching and replacing, in this file and in every file — and the two
//! things a merge leaves behind.

use super::*;

#[test]
fn the_key_that_opens_a_path_works_with_something_else_already_open() {
    // The program sending it cannot see the screen, so a list, a prompt or
    // a question in the way has to give up the key rather than eat it.
    let (mut app, _rx) = editor();
    for opened in [Cmd::OPEN, Cmd::FIND, Cmd::COMMAND_PALETTE] {
        app.run(opened);
        keyed(&mut app, "alt-e");
        assert!(
            matches!(&app.overlay, Overlay::Prompt(p) if p.kind == PromptKind::OpenPath),
            "{opened:?} swallowed it"
        );
        app.overlay = Overlay::None;
    }
}

#[test]
fn a_key_bound_to_opening_a_path_still_types_where_typing_is_meant() {
    // Bound to a plain letter, it is a letter first: a global key that
    // stole `e` from every search box would be worse than no global key.
    let mut config = Config::default();
    config.keys.insert("open-path".into(), vec!["e".into()]);
    let (tx, _rx) = mpsc::channel();
    let mut app = App::new(config, tx);
    app.screen = Rect::new(0, 0, 100, 30);
    app.run(Cmd::FIND);
    keyed(&mut app, "e");
    match &app.overlay {
        Overlay::Prompt(prompt) => assert_eq!(prompt.input, "e"),
        _ => panic!("the search box closed"),
    }
}

#[test]
fn the_search_box_opens_empty_but_the_last_search_is_still_there() {
    let (mut app, _rx) = editor();
    typed(&mut app, "alpha beta alpha");
    app.last_search = "alpha".into();
    app.run(Cmd::FIND);
    match &app.overlay {
        Overlay::Prompt(p) => assert_eq!(p.input, "", "the box kept the last search"),
        other => panic!("no search box: {:?}", matches!(other, Overlay::None)),
    }
    // Which is not the same as forgetting it.
    assert_eq!(app.last_search, "alpha");
}

#[test]
fn enter_in_the_search_box_walks_the_matches_without_closing_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "alpha beta alpha gamma alpha");
    app.run(Cmd::MOVE_DOC_START);
    app.run(Cmd::FIND);
    typed_into_prompt(&mut app, "alpha");
    let first = app.view().sel.primary().start();

    keyed(&mut app, "enter");
    assert!(
        matches!(&app.overlay, Overlay::Prompt(p) if p.kind == PromptKind::Find),
        "Enter closed the box"
    );
    let second = app.view().sel.primary().start();
    assert!(
        second > first,
        "Enter did not move on: {first} then {second}"
    );

    keyed(&mut app, "enter");
    let third = app.view().sel.primary().start();
    assert!(third > second);

    // And back the way it came.
    keyed(&mut app, "shift-enter");
    assert_eq!(app.view().sel.primary().start(), second);
}

#[test]
fn leaving_the_search_box_keeps_where_enter_took_you() {
    let (mut app, _rx) = editor();
    typed(&mut app, "alpha beta alpha");
    app.run(Cmd::MOVE_DOC_START);
    app.run(Cmd::FIND);
    typed_into_prompt(&mut app, "alpha");
    keyed(&mut app, "enter");
    let landed = app.view().sel.primary().start();
    keyed(&mut app, "esc");
    assert_eq!(app.view().sel.primary().start(), landed);
}

#[test]
fn changing_your_mind_about_a_search_puts_the_cursor_back() {
    let (mut app, _rx) = editor();
    typed(&mut app, "alpha beta alpha");
    app.run(Cmd::MOVE_DOC_START);
    let was = app.view().sel.primary().start();
    app.run(Cmd::FIND);
    typed_into_prompt(&mut app, "beta");
    assert_ne!(
        app.view().sel.primary().start(),
        was,
        "typing did not search"
    );
    keyed(&mut app, "esc");
    assert_eq!(app.view().sel.primary().start(), was);
}

#[test]
fn find_walks_the_matches_and_wraps_round() {
    let (mut app, _rx) = editor();
    typed(&mut app, "alpha beta alpha gamma alpha");
    app.run(Cmd::MOVE_DOC_START);
    app.last_search = "alpha".into();

    app.run(Cmd::FIND_NEXT);
    let first = app.view().sel.primary().start();
    app.run(Cmd::FIND_NEXT);
    let second = app.view().sel.primary().start();
    assert!(second > first);
    app.run(Cmd::FIND_NEXT);
    app.run(Cmd::FIND_NEXT);
    // Four steps through three matches comes back to the first.
    assert_eq!(app.view().sel.primary().start(), first);
}

#[test]
fn replacing_changes_every_match_as_one_undo() {
    let (mut app, _rx) = editor();
    typed(&mut app, "red green red blue red");
    app.run(Cmd::MOVE_DOC_START);
    app.replace_all("red", "amber");
    assert_eq!(app.here().rope.to_string(), "amber green amber blue amber");
    app.run(Cmd::UNDO);
    assert_eq!(app.here().rope.to_string(), "red green red blue red");
}

#[test]
fn replacing_inside_a_selection_leaves_the_rest_alone() {
    let (mut app, _rx) = editor();
    typed(&mut app, "red red red");
    app.view_mut().sel = Selections::single(Range::new(0, 7));
    app.replace_all("red", "blue");
    assert_eq!(app.here().rope.to_string(), "blue blue red");
}

#[test]
fn a_buffer_is_not_rewritten_under_you_with_something_that_is_not_text() {
    // Half a file very often ends in the middle of a character, and a
    // lossy conversion turns the remains into replacement characters. Done
    // on a timer, to a buffer somebody is looking at, that is rubbish
    // appearing in their file from nowhere.
    let (mut app, _rx) = editor();
    let path = scratch("torn.txt");
    std::fs::write(&path, "hello\n").expect("written");
    app.open_path(&path);
    let id = app.view().doc;

    std::fs::write(&path, b"good \xff\xfe bad").expect("written");
    assert!(
        app.take_from_disk(id, Reread::OnATimer).is_err(),
        "it should refuse"
    );
    assert_eq!(app.here().rope.to_string(), "hello\n", "and leave the buffer alone");

    // Asked for outright, it still does its best — you can see the result
    // and undo it.
    assert!(app.take_from_disk(id, Reread::Asked).is_ok());
    assert!(app.here().rope.to_string().starts_with("good "));
    std::fs::remove_file(&path).ok();
}

#[test]
fn replacing_across_the_project_asks_first_and_then_changes_buffers() {
    // The whole shape of it: the walk finds the files, a question goes up
    // with a count in it, and agreeing to it changes buffers rather than
    // files — so it can be undone, looked at, and saved when you mean it.
    let (mut app, rx) = editor();
    let dir = scratch("replace-project").parent().expect("a dir").to_path_buf();
    std::fs::create_dir_all(&dir).expect("a place to work");
    let one = dir.join("one.txt");
    let two = dir.join("two.txt");
    std::fs::write(&one, "alpha beta alpha\n").expect("written");
    std::fs::write(&two, "beta only\n").expect("written");
    app.project = dir.clone();

    app.find_what_to_replace("alpha".into(), "omega".into());
    // The walk answers on the same channel as everything else.
    let what = loop {
        match rx.recv().expect("the walk answers") {
            Event::ToReplace(what) => break *what,
            _ => continue,
        }
    };
    assert_eq!(what.files.len(), 1, "only the file that has it in it");
    assert_eq!(what.files[0].1, 2, "twice in that file");

    app.ask_before_replacing(what);
    let Overlay::Confirm(confirm) = &app.overlay else {
        panic!("it must ask before rewriting anybody's project");
    };
    assert!(confirm.message.contains("2 places in 1 file"), "{}", confirm.message);

    pressed(&mut app, "r");
    let doc = app
        .docs
        .iter()
        .find(|d| d.path.as_deref() == Some(one.as_path()))
        .expect("the file was opened");
    assert_eq!(doc.rope.to_string(), "omega beta omega\n");
    assert!(doc.is_modified(), "and left unsaved, to be looked at");
    assert_eq!(
        std::fs::read_to_string(&one).expect("read"),
        "alpha beta alpha\n",
        "nothing has been written to the disk"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_project_replace_that_matches_nothing_says_so() {
    let (mut app, _rx) = editor();
    app.ask_before_replacing(Replace {
        needle: "nowhere".into(),
        with: "x".into(),
        files: Vec::new(),
        over: 0,
    });
    assert_eq!(app.status.text, "no nowhere in any file");
    assert!(matches!(app.overlay, Overlay::None), "and asks nothing");
}

#[test]
fn where_a_search_matches_is_asked_in_one_place() {
    assert_eq!(occurrences("Cat cat CAT", "cat").len(), 3);
    assert_eq!(occurrences("Cat cat CAT", "Cat"), vec![0]);
    assert!(occurrences("anything", "").is_empty());
}

#[test]
fn taking_one_side_of_a_conflict_leaves_that_side_and_no_markers() {
    let (mut app, _rx) = editor();
    typed(
        &mut app,
        "before\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> other\nafter\n",
    );
    app.go_to_line(2);
    app.run(Cmd::TAKE_OURS);
    assert_eq!(app.here().rope.to_string(), "before\nmine\nafter\n");

    // And undo puts the whole conflict back, because it was one edit.
    app.run(Cmd::UNDO);
    assert!(app.here().rope.to_string().contains("<<<<<<<"));

    app.go_to_line(2);
    app.run(Cmd::TAKE_THEIRS);
    assert_eq!(app.here().rope.to_string(), "before\ntheirs\nafter\n");
}

#[test]
fn stepping_through_conflicts_says_how_big_each_one_is() {
    let (mut app, _rx) = editor();
    typed(
        &mut app,
        "a\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> other\nb\n",
    );
    app.go_to_line(0);
    app.run(Cmd::NEXT_CONFLICT);
    assert_eq!(line_now(&app), 1);
    assert_eq!(app.status.text, "conflict, 1 line yours and 1 line theirs");
}

#[test]
fn a_cursor_outside_a_conflict_is_told_so_rather_than_changing_something() {
    let (mut app, _rx) = editor();
    typed(&mut app, "nothing to merge here\n");
    app.run(Cmd::TAKE_OURS);
    assert_eq!(app.status.text, "the cursor is not in a conflict");
    assert_eq!(app.here().rope.to_string(), "nothing to merge here\n");
}

#[test]
fn reverting_a_hunk_puts_back_what_was_committed_and_can_be_undone() {
    let (mut app, _rx) = editor();
    let path = scratch("reverting.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").expect("written");
    app.open_path(&path);
    let id = app.view().doc;

    // Stand in for git having seen it: the tracker's baseline is what a
    // revert puts back, and where it came from is git's business.
    app.git.remember_baseline(id, "one\ntwo\nthree\n".into());
    app.go_to_line(1);
    app.run(Cmd::SELECT_LINE);
    typed(&mut app, "TWO\n");
    assert_eq!(app.here().rope.to_string(), "one\nTWO\nthree\n");

    app.go_to_line(1);
    app.run(Cmd::REVERT_HUNK);
    assert_eq!(app.here().rope.to_string(), "one\ntwo\nthree\n");
    app.run(Cmd::UNDO);
    assert_eq!(
        app.here().rope.to_string(),
        "one\nTWO\nthree\n",
        "one edit, so one undo"
    );
    std::fs::remove_file(&path).ok();
}
