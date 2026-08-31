//! Changing the text: typing, the line verbs, folding, bookmarks and the
//! recorder.

use super::*;

#[test]
fn running_a_c_file_compiles_it_first() {
    // The bug: F5 on a `main.c` nobody had compiled started `gdb`, which
    // said `main`: no such file. The editor knew what was missing and had
    // no way to make it.
    if !crate::pack::on_path("cc") {
        return;
    }
    let (mut app, rx) = editor();
    // The rest of this runs a real compiler, so it is about the build
    // textfold ships. Somebody who has pointed their own C build at `make`
    // has a settings file, not a bug, and a test that failed for them
    // would be a test about their machine.
    let shipped = app.build_for("c").is_some_and(|build| build.command == "cc");
    let path = scratch("run-me.c");
    std::fs::write(&path, "int main(void) { return 0; }\n").expect("written");
    app.open_path(&path);
    app.run(Cmd::DEBUG);
    assert_eq!(
        app.after_build,
        Some(AfterBuild::Debug),
        "F5 went straight to the debugger with nothing built"
    );

    // And when the compiler comes back, the debugger it was for is
    // started. Which is the whole of the point: one key, and what it does
    // is what somebody meant by it.
    let built = loop {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Event::Tool(done)) => break Some(done),
            Ok(_) => continue,
            Err(_) => break None,
        }
    };
    let built = built.expect("the build never answered");
    if shipped {
        assert!(built.ok, "cc would not compile an empty main: {}", built.err);
        assert!(path.with_extension("").exists(), "nothing was built");
    }
    let ok = built.ok;
    app.handle(Event::Tool(built));
    assert_eq!(app.after_build, None, "the build is over and still being waited on");
    // `gdb` is not on every machine, and this test is about the editor
    // rather than about the adapter — so what is checked is that it got as
    // far as trying, which is a session either way.
    if ok && crate::pack::on_path("gdb") {
        assert!(
            app.debug.session().is_some(),
            "it compiled and then never started a debugger"
        );
        app.debug.stop();
    }
    // And a build that failed never gets that far, whichever build it is.
    if !ok {
        assert!(
            app.debug.session().is_none(),
            "it debugged something the build had not made"
        );
    }

    // And a language with no build goes straight through rather than
    // waiting for one that is never coming.
    let py = scratch("run-me.py");
    std::fs::write(&py, "print(1)\n").expect("written");
    app.after_build = None;
    app.open_path(&py);
    app.run(Cmd::DEBUG);
    assert_eq!(app.after_build, None);

    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&py).ok();
    std::fs::remove_file(path.with_extension("")).ok();
}

#[test]
fn a_build_that_fails_in_a_way_the_margin_cannot_hold_still_says_why() {
    // The complaint this is about: "cc just fails without telling me why".
    // The margin holds what a compiler said about a line of a file you
    // have open, which is most of what it says and never all of it — a
    // linker error names no line, `make` names no file, and a mistake in a
    // header you have not opened has nowhere to be drawn. Every one of
    // those reached the person as a count of nothing.
    let (mut app, rx) = editor();
    // About the build textfold ships, for the reason above.
    if app.build_for("c").is_none_or(|build| build.command != "cc") {
        return;
    }
    let path = scratch("wont-link.c");
    // Calls something that is never defined, so it compiles and does not
    // link — and a linker's complaint matches no `%f:%l:%c` pattern there
    // has ever been.
    std::fs::write(&path, "int fizz(int);\nint main(void) { return fizz(1); }\n")
        .expect("written");
    app.open_path(&path);
    app.run(Cmd::BUILD);

    let done = loop {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Event::Tool(done)) => break Some(done),
            Ok(_) => continue,
            Err(_) => break None,
        }
    };
    let done = done.expect("the compiler never answered");
    if done.ok {
        // A machine whose `cc` links this anyway has nothing to say about
        // the bug, and a test that asserts on somebody's toolchain is a
        // test that fails for the wrong reason.
        return;
    }
    app.handle(Event::Tool(done));

    // The whole of what it printed is kept…
    let kept = app.last_build.as_ref().expect("nothing was kept");
    assert!(!kept.ok);
    assert!(
        kept.text.to_lowercase().contains("fizz"),
        "what it actually said was thrown away: {:?}",
        kept.text
    );
    // …and put in front of somebody, because nothing went in the margin
    // and asking for it is the part nobody knows to do.
    let shown = app
        .docs
        .iter()
        .find(|doc| doc.name.ends_with("output"))
        .expect("it failed and showed nothing");
    assert!(shown.rope.to_string().to_lowercase().contains("fizz"));
    // And the status line carries what the compiler said rather than the
    // count of nothing it used to: "cc found nothing it could read" is a
    // sentence about the parser, not about the program.
    assert!(!app.status.text.contains("found nothing"), "{}", app.status.text);
    assert_eq!(app.status.tone, Tone::Bad, "{}", app.status.text);
    let first = kept
        .text
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("it printed something");
    assert_eq!(app.status.text, format!("cc: {first}"));

    std::fs::remove_file(&path).ok();
    std::fs::remove_file(path.with_extension("")).ok();
}

#[test]
fn a_cursor_is_never_put_past_the_end_of_the_buffer() {
    let (mut app, _rx) = editor();
    typed(&mut app, "short\n");
    app.place_cursor(10_000, false, false);
    assert_eq!(app.view().cursor(), app.here().len_chars());
}

#[test]
fn there_is_no_arrow_in_the_margin_when_nothing_is_stopped() {
    let (app, _rx) = editor();
    assert!(app.stopped_at().is_none());
}

#[test]
fn a_plugin_asking_for_something_textfold_does_not_do_is_told_so() {
    // Not a silence. A plugin author who has misspelt a method, or reached
    // for one that does not exist yet, should hear it from the editor
    // rather than watch nothing happen.
    let (mut app, _rx) = editor();
    assert_eq!(
        plugin_asks(&mut app, "buffer/incinerate", json!({})),
        Err("textfold has no buffer/incinerate".into())
    );
}

#[test]
fn the_panes_come_back_as_they_were() {
    let one = scratch("panes-one.rs");
    let two = scratch("panes-two.rs");
    std::fs::write(&one, "a\n").unwrap();
    std::fs::write(&two, "b\n").unwrap();

    let (mut app, _rx) = editor();
    let session = crate::session::Session {
        tabs: vec![
            crate::session::Tab {
                path: one.clone(),
                line: 0,
                column: 0,
            },
            crate::session::Tab {
                path: two.clone(),
                line: 0,
                column: 0,
            },
        ],
        panes: vec![
            crate::session::Pane { tab: 1, wrap: false },
            crate::session::Pane { tab: 0, wrap: true },
        ],
        focus: 1,
        side_by_side: false,
        at: 0,
        docks: Vec::new(),
    };
    app.apply_session(&session, false);
    assert_eq!(app.panes.len(), 2);
    assert_eq!(app.doc(app.panes[0].doc).map(|d| d.name.clone()).unwrap(), "panes-two.rs");
    assert_eq!(app.doc(app.panes[1].doc).map(|d| d.name.clone()).unwrap(), "panes-one.rs");
    assert!(app.panes[1].wrap, "the pane's own folding came back");
    assert_eq!(app.focus, 1);
    assert!(!app.side_by_side);
    std::fs::remove_file(&one).ok();
    std::fs::remove_file(&two).ok();
}

#[test]
fn a_path_can_be_opened_by_typing_it_rather_than_finding_it() {
    let (mut app, _rx) = editor();
    let path = scratch("typed-path.txt");
    std::fs::write(&path, "already here\n").expect("written");

    keyed(&mut app, "alt-e");
    typed_into_prompt(&mut app, &path.display().to_string());
    app.accept_prompt();

    assert_eq!(app.here().path.as_deref(), Some(path.as_path()));
    assert_eq!(app.here().rope.to_string(), "already here\n");
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn opening_the_file_list_goes_and_looks_again() {
    // A file written since textfold started is a file you can open. The
    // list from last time is shown at once, and a fresh walk is under way
    // behind it.
    let (mut app, rx) = editor();
    let dir = scratch("walk").parent().unwrap().to_path_buf();
    app.project = dir.clone();
    // What a walk found before the file existed, which is the state
    // textfold is in for the rest of an afternoon.
    app.files = Some(vec![dir.join("old.txt")]);
    std::fs::write(dir.join("new.txt"), "made just now\n").expect("written");

    app.run(Cmd::OPEN);
    assert!(matches!(&app.overlay, Overlay::Picker(p) if p.kind == Kind::Files));
    let shown = match &app.overlay {
        Overlay::Picker(picker) => picker.len(),
        _ => 0,
    };
    assert_eq!(shown, 1, "the list from last time shows first");

    // The walking thread reports what is there now, and the box follows.
    let found = loop {
        match rx.recv().expect("the walk answers") {
            Event::Files(found) => break found,
            _ => continue,
        }
    };
    assert!(
        found.iter().any(|p| p.ends_with("new.txt")),
        "the fresh walk missed a file written after startup: {found:?}"
    );
    app.handle(Event::Files(found));
    assert!(matches!(&app.overlay, Overlay::Picker(p)
        if p.visible().any(|(row, _)| row.label == "new.txt")));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_block_the_server_calls_code_is_not_read_as_markdown() {
    lang::init();
    // The old `MarkedString` form, naming a language nothing here parses.
    // It is still code: its hashes are not headings and its dashes are not
    // a rule.
    let hover = serde_json::json!({
        "language": "cmake",
        "value": "#include <stdio.h>\n---\n",
    });
    let lines = markup_lines(Some(&hover), LangId::PLAIN);
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["#include <stdio.h>", "---"]);
}

#[test]
fn colouring_a_block_does_not_colour_past_the_end_of_a_line() {
    lang::init();
    let python = lang::by_tag("py").expect("shipped");
    // A string over two lines: one span in the tree, two lines on screen,
    // and neither of them may reach outside itself.
    let hover = serde_json::json!({
        "value": "```python\nx = \"\"\"one\ntwo\"\"\"\n```",
    });
    let lines = markup_lines(Some(&hover), python);
    for line in &lines {
        for (range, _) in &line.spans {
            assert!(
                range.end <= line.text.len() && range.start < range.end,
                "{line:?}"
            );
        }
    }
    assert!(
        lines
            .iter()
            .any(|l| l.spans.iter().any(|(_, role)| *role == Role::String)),
        "{lines:?}"
    );
}

#[test]
fn a_line_you_have_changed_since_the_commit_is_marked_and_can_be_jumped_to() {
    use std::process::{Command, Stdio};
    let dir = std::env::temp_dir().join(format!("textfold-appgit-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("a place to work");
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()
            .filter(|s| s.success())
    };
    // No git on this machine is not a failing test.
    let Some(_) = run(&["init", "-q"])
        .and_then(|_| run(&["config", "user.email", "nobody@example.invalid"]))
        .and_then(|_| run(&["config", "user.name", "Nobody"]))
    else {
        std::fs::remove_dir_all(&dir).ok();
        return;
    };
    let path = dir.join("tracked.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").expect("written");
    if run(&["add", "tracked.txt"])
        .and_then(|_| run(&["commit", "-qm", "first"]))
        .is_none()
    {
        std::fs::remove_dir_all(&dir).ok();
        return;
    }

    let (mut app, _rx) = editor();
    app.project = dir.clone();
    app.git.open(&dir);
    app.open_path(&path);
    app.refresh_git();
    assert_eq!(
        app.git.changed_lines(app.view().doc),
        0,
        "nothing changed yet"
    );

    app.go_to_line(1);
    typed(&mut app, "changed ");
    app.refresh_git();

    let id = app.view().doc;
    assert_eq!(app.git.mark(id, 1), Some(crate::git::Mark::Changed));
    assert_eq!(app.git.mark(id, 0), None);

    app.run(Cmd::MOVE_DOC_START);
    app.run(Cmd::NEXT_CHANGE);
    assert_eq!(
        text::line_of(&app.here().rope, app.view().cursor()),
        1,
        "the jump did not land on the changed line"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn comparing_scrolls_the_other_pane_to_line_up() {
    let mut left = String::from("head\n");
    let mut right = String::from("head\n");
    // Ten lines only the right has, then a hundred the two share.
    for n in 0..10 {
        right.push_str(&format!("only-right-{n}\n"));
    }
    for n in 0..100 {
        left.push_str(&format!("shared-{n}\n"));
        right.push_str(&format!("shared-{n}\n"));
    }
    let (mut app, _rx, dir) = two_panes("dscroll", &left, &right);
    // The focus is the pane opened second, which is the right-hand file.
    app.run(Cmd::DIFF_PANES);
    let (left_pane, right_pane) = app.diff.as_ref().expect("compared").panes();
    let here = app.focus.min(app.panes.len() - 1);
    let there = if here == left_pane { right_pane } else { left_pane };

    app.panes[here].top = 40;
    app.tick();
    let want = app
        .diff
        .as_ref()
        .expect("compared")
        .beside(here, 40)
        .expect("a line beside it");
    assert_eq!(
        app.panes[there].top, want,
        "the other pane did not follow: {} vs {want}",
        app.panes[there].top
    );
    assert_ne!(
        app.panes[there].top, 40,
        "it followed by copying the number rather than by lining up"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_edit_is_taken_into_account_without_asking_again() {
    let (mut app, _rx, dir) = two_panes("dedit", "one\ntwo\n", "one\nTWO\n");
    app.run(Cmd::DIFF_PANES);
    assert!(!app.diff.as_ref().expect("compared").same());

    // Make the two agree. The comparison should notice on its own.
    app.run(Cmd::SELECT_ALL);
    typed(&mut app, "one\ntwo\n");
    app.tick();
    assert!(
        app.diff.as_ref().expect("compared").same(),
        "the comparison did not keep up with the text"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn comparing_needs_two_panes() {
    let (mut app, _rx) = editor();
    app.run(Cmd::DIFF_PANES);
    assert!(app.diff.is_none());
}

#[test]
fn a_project_with_no_marker_in_it_is_still_the_project_you_opened() {
    // `project_root` is handed a path and knows nothing else, so when it
    // finds no marker it answers with the directory the file sits in.
    // For a build that is `src/`, and `make` run in `src/` is `make` with
    // no `Makefile` in front of it. The editor knows the directory it was
    // opened on, which is what everything else here already means by "the
    // project".
    let (mut app, _rx) = editor();
    let root = scratch("no-marker").parent().expect("a directory").to_path_buf();
    let inside = root.join("src");
    std::fs::create_dir_all(&inside).expect("a place to work");
    let file = inside.join("main.c");
    std::fs::write(&file, "int main(void){return 0;}\n").expect("written");
    app.project = root.clone();

    let markers = vec![".git".to_string()];
    assert_eq!(
        lang::project_root(&file, &markers),
        inside,
        "the thing being corrected for"
    );
    assert_eq!(app.root_for(&file, &markers), root, "it settled for src/");

    // A marker that *is* there still wins: this only fills a hole, it does
    // not overrule an answer.
    std::fs::write(inside.join("Makefile"), "all:\n").expect("written");
    let markers = vec!["Makefile".to_string()];
    assert_eq!(
        app.root_for(&file, &markers),
        inside,
        "the nearest Makefile lost to the directory textfold was opened on"
    );

    // And a file from somewhere else entirely is not part of this project,
    // so guessing this project's directory for it would be worse than the
    // directory it lives in.
    let elsewhere = scratch("far-away.c");
    std::fs::write(&elsewhere, "int main(void){return 0;}\n").expect("written");
    let markers = vec![".git".to_string()];
    assert_eq!(
        app.root_for(&elsewhere, &markers),
        elsewhere.parent().expect("a directory"),
    );

    std::fs::remove_dir_all(&inside).ok();
    std::fs::remove_file(&elsewhere).ok();
}

#[test]
fn a_line_too_wide_for_the_box_folds_rather_than_being_cut_off() {
    // The whole complaint: a box that only scrolls downwards and elides
    // sideways is showing you the first half of every sentence in it.
    let line = DocLine::prose("the quick brown fox jumps over the lazy dog");
    let folded = line.wrap(20);
    let text: Vec<&str> = folded.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(text, ["the quick brown fox", "jumps over the lazy", "dog"]);
    // Nothing was lost and nothing was added.
    assert_eq!(text.join(" "), "the quick brown fox jumps over the lazy dog");
    for row in &folded {
        assert!(crate::text::str_width(&row.text) <= 20);
    }
}

#[test]
fn a_line_that_already_fits_is_left_exactly_as_it_was() {
    let line = DocLine::prose("short enough");
    assert_eq!(line.wrap(40), vec![line.clone()]);
    // And a rule stays one character, because the drawing is what
    // stretches it and only the drawing knows how wide the box turned out.
    let rule = DocLine::prose(RULE.to_string());
    assert_eq!(rule.wrap(4), vec![rule.clone()]);
}

#[test]
fn a_fold_keeps_the_indentation_of_the_line_it_came_from() {
    // Otherwise a bulleted list stops being a list at its first long item.
    let line = DocLine::prose("    an indented sentence that runs on and on");
    let text: Vec<String> = line.wrap(20).into_iter().map(|l| l.text).collect();
    assert_eq!(
        text,
        ["    an indented", "    sentence that", "    runs on and on"]
    );
}

#[test]
fn a_word_with_no_spaces_in_it_is_broken_rather_than_left_hanging() {
    // A Rust type is one long word constantly, and a row holding a single
    // character is not an improvement on eliding.
    let line = DocLine::prose("BTreeMap<String,Vec<Something::Awfully::Long>>");
    let text: Vec<String> = line.wrap(16).into_iter().map(|l| l.text).collect();
    assert_eq!(text.concat(), "BTreeMap<String,Vec<Something::Awfully::Long>>");
    for row in &text {
        assert!(crate::text::str_width(row) <= 16, "{row:?}");
    }
}

#[test]
fn folding_carries_the_colours_and_the_names_across_to_where_they_now_sit() {
    // A folded line whose spans still pointed at the unfolded offsets
    // would colour the wrong letters, and a name you could no longer
    // click is a name you can no longer go to the definition of.
    let mut lines = Vec::new();
    push_code(
        &mut lines,
        "let mapping: BTreeMap<String, u32> = BTreeMap::new();
",
        lang::by_name("rust"),
    );
    let line = lines.first().expect("one line of code");
    assert!(!line.spans.is_empty(), "the code was coloured to begin with");
    for row in line.wrap(24) {
        for (span, _) in &row.spans {
            assert!(row.text.get(span.clone()).is_some(), "{row:?} {span:?}");
        }
        for link in &row.links {
            assert!(link.end <= row.text.chars().count(), "{row:?} {link:?}");
        }
    }
}

#[test]
fn a_folded_hover_can_be_read_all_the_way_down_and_keeps_its_place() {
    let long = "a sentence long enough that it has to be folded more than once to fit";
    let mut popup = Popup::new(vec![DocLine::prose(long); 4], 0);
    popup.fold_to(20);
    assert!(popup.lines.len() > 4, "it folded");
    // Every row is inside the box, which is the point.
    for row in &popup.lines {
        assert!(crate::text::str_width(&row.text) <= 20);
    }
    // Scrolled to the third line's first row, then folded again at
    // another width: the same line of text is still on the top row.
    popup.scroll = popup.folded_at_line(2);
    popup.fold_to(30);
    assert_eq!(popup.scroll, popup.folded_at_line(2));
    assert_eq!(popup.unfolded_at(popup.scroll), 2);
}

#[test]
fn only_what_the_markup_called_code_is_worth_following() {
    let line = DocLine::prose("There is always at least one, and they are in order.");
    assert!(followable(&line).is_empty(), "prose is not a set of names");

    let line = DocLine::prose("assume, and [`Selections::normalise`](https://x/y) keeps it");
    assert_eq!(followable(&line), vec!["Selections::normalise"]);

    let line = DocLine::prose("A `HashMap` of `String` to `u32`, and a [bracket] in prose");
    assert_eq!(followable(&line), vec!["HashMap", "String", "u32"]);

    // The backtick that closed a link's name must not be read as one
    // opening another, or the rest of the line becomes a name.
    let line = DocLine::prose("[`Eq`](https://x/y) and then `Ord` and more prose");
    assert_eq!(followable(&line), vec!["Eq", "Ord"]);
}

#[test]
fn a_name_in_a_docstring_is_followed_and_the_prose_around_it_is_not() {
    let line = DocLine::prose("see `Selections` for that");
    let at = |column| {
        line.links
            .iter()
            .any(|r| r.contains(&column))
            .then(|| word_span(&line.text, column).map(|(w, ..)| w))
            .flatten()
    };
    assert_eq!(at(6), Some("Selections".into()));
    assert_eq!(at(1), None, "`see` is a word, not a name");
    assert_eq!(at(19), None, "`for` is a word, not a name");
}

#[test]
fn in_code_the_names_are_followed_and_the_keywords_are_not() {
    let rust = lang::by_name("rust").expect("rust");
    let mut lines = Vec::new();
    push_code(&mut lines, "let it: HashMap<String, u32> = HashMap::new();\n", Some(rust));
    let line = lines.first().expect("a line");
    let names = followable(line);
    assert!(names.contains(&"HashMap".to_string()), "{names:?}");
    assert!(names.contains(&"String".to_string()), "{names:?}");
    assert!(!names.contains(&"let".to_string()), "a keyword is not a name");
    assert!(!names.contains(&"it".to_string()), "a local is not a name");
}

#[test]
fn a_word_in_a_docstring_is_the_word_and_not_the_punctuation() {
    let word_in = |line: &str, column: usize| word_span(line, column).map(|(w, ..)| w);
    let line = "fn take(list: Vec<Widget>)";
    assert_eq!(word_in(line, 15), Some("Vec".into()));
    assert_eq!(word_in(line, 18), Some("Widget".into()));
    assert_eq!(word_in(line, 17), None, "`<` is not a word");
    assert_eq!(
        word_in(line, line.len()),
        None,
        "past the end is not a word"
    );
    // A single letter is `T` or `a`, which is everywhere in a paragraph
    // of prose, and a bare number is a length rather than a type.
    assert_eq!(word_in("a Vec of T items", 0), None);
    assert_eq!(word_in("a Vec of T items", 9), None);
    assert_eq!(word_in("at most 4096 bytes", 8), None);
    assert_eq!(word_in("a Vec of T items", 2), Some("Vec".into()));
    assert_eq!(
        word_in("see [`Selections::normalise`]", 7),
        Some("Selections".into()),
        "markdown punctuation is not part of the name"
    );
}

#[test]
fn escape_takes_off_one_layer_at_a_time() {
    let (mut app, _rx) = editor();
    typed(&mut app, "one two three");
    app.run(Cmd::SELECT_ALL);
    app.run(Cmd::ADD_CURSOR_BELOW);
    // Whatever the cursors ended up as, Escape works back to one bare one.
    for _ in 0..4 {
        app.run(Cmd::ESCAPE);
    }
    assert_eq!(app.view().sel.len(), 1);
    assert!(app.view().sel.primary().is_empty());
}

#[test]
fn a_lower_case_search_ignores_case_and_a_capital_means_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "Thing thing THING");
    assert_eq!(app.count_matches("thing"), 3);
    assert_eq!(app.count_matches("Thing"), 1);
}

#[test]
fn quitting_with_unsaved_work_asks_first() {
    let (mut app, _rx) = editor();
    typed(&mut app, "unsaved");
    app.run(Cmd::QUIT);
    assert!(!app.quit);
    assert!(matches!(app.overlay, Overlay::Confirm(_)));
    // Saying no keeps everything.
    app.confirm_key(Key::parse("c").unwrap());
    assert!(!app.quit);
    assert_eq!(app.here().rope.to_string(), "unsaved");
    // Saying discard leaves.
    app.run(Cmd::QUIT);
    app.confirm_key(Key::parse("d").unwrap());
    assert!(app.quit);
}

#[test]
fn the_palette_runs_what_you_choose() {
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree");
    app.run(Cmd::COMMAND_PALETTE);
    let Overlay::Picker(picker) = &mut app.overlay else {
        panic!("the palette did not open");
    };
    for c in "select-all".chars() {
        picker.type_char(c);
    }
    app.choose();
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.view().sel.primary().len(), app.here().len_chars());
}

#[test]
fn a_setting_changed_from_the_list_takes_effect_and_the_list_stays_open() {
    let (mut app, _rx) = editor();
    let before = app.config.show_whitespace();
    app.run(Cmd::SETTINGS);
    let Overlay::Picker(picker) = &mut app.overlay else {
        panic!("the settings did not open");
    };
    let at = picker
        .visible()
        .position(|(row, _)| matches!(row.choice, Choice::Setting("show_whitespace")))
        .expect("the setting is on the list");
    picker.select(at);
    app.choose();
    assert_ne!(app.config.show_whitespace(), before);
    assert!(matches!(app.overlay, Overlay::Picker(_)));
}

#[test]
fn a_pane_split_in_two_keeps_both_cursors_pointing_at_the_same_text() {
    let (mut app, _rx) = editor();
    typed(&mut app, "alpha\nbeta\ngamma");
    app.run(Cmd::SPLIT);
    assert_eq!(app.panes.len(), 2);
    // Put the other pane's cursor at the end, then type at the start.
    app.panes[0].sel = Selections::single(Range::point(app.here().len_chars()));
    app.focus = 1;
    app.view_mut().sel = Selections::single(Range::point(0));
    typed(&mut app, "XY");
    // The other pane's cursor moved along with the text it was pointing at.
    assert_eq!(app.panes[0].cursor(), app.here().len_chars());
    assert_eq!(app.here().rope.to_string(), "XYalpha\nbeta\ngamma");
}

#[test]
fn a_language_built_into_the_binary_cannot_be_uninstalled() {
    // There would be nothing for it to mean. Switching it off is the
    // thing you want, and the message says so rather than leaving you to
    // work out why nothing happened.
    let (mut app, _rx) = editor();
    app.start_uninstall("rust");
    assert_eq!(app.status.tone, Tone::Bad);
    assert!(app.status.text.contains("switch it off"), "{}", app.status.text);
}

#[test]
fn what_an_install_says_lands_in_a_buffer_you_can_read() {
    let (mut app, _rx) = editor();
    app.installing = Some(Installing {
        id: "zls".into(),
        removing: false,
        log: String::new(),
    });
    let note = |note| {
        Box::new(crate::pack::Progress {
            id: "zls".into(),
            note,
        })
    };
    app.on_package(*note(crate::pack::Note::Doing {
        at: 1,
        of: 1,
        about: "zls, with brew".into(),
    }));
    app.on_package(*note(crate::pack::Note::Did {
        about: "brew install zls".into(),
        ok: true,
        output: "poured from bottle\n".into(),
    }));
    app.on_package(*note(crate::pack::Note::Done {
        ok: true,
        why: "zls installed".into(),
    }));

    assert!(app.installing.is_none());
    assert_eq!(app.status.tone, Tone::Good);
    let log = app
        .docs
        .iter()
        .find(|d| d.name == "install zls")
        .expect("what it said is somewhere you can read it");
    assert!(log.rope.to_string().contains("poured from bottle"));
}

#[test]
fn a_read_only_file_refuses_to_be_changed() {
    let (mut app, _rx) = editor();
    typed(&mut app, "fixed");
    let id = app.view().doc;
    app.doc_mut(id).expect("open").read_only = true;
    app.run(Cmd::DELETE_LINE);
    typed(&mut app, "more");
    assert_eq!(app.here().rope.to_string(), "fixed");
    assert_eq!(app.status.tone, Tone::Bad);
}

#[test]
fn folding_hides_the_body_and_keeps_the_line_that_says_what_it_was() {
    let (mut app, _rx) = editor();
    let path = scratch("folding.rs");
    std::fs::write(
        &path,
        "fn one() {\n    let a = 1;\n    let b = 2;\n}\nfn two() {}\n",
    )
    .expect("written");
    app.open_path(&path);
    assert!(app.here().syntax.is_some(), "a rust file is parsed");

    app.go_to_line(0);
    app.run(Cmd::FOLD);
    let doc = app.here();
    let folded = app.view().folded(&doc.rope);
    assert_eq!(folded, vec![(0, 3)], "the body, with its first line left");

    // And the rows say so: the hidden lines are worth nothing on screen.
    let folds = app.view().folded(&doc.rope);
    let layout = crate::view::Layout {
        rope: &doc.rope,
        hints: Vec::new(),
        width: 80,
        tab_width: 4,
        wrap: false,
        folds,
    };
    assert_eq!(layout.rows_in(0), 1);
    assert_eq!(layout.rows_in(1), 0);
    assert_eq!(layout.rows_in(2), 0);
    assert_eq!(layout.rows_in(4), 1);
    std::fs::remove_file(&path).ok();
}

#[test]
fn folding_the_line_the_cursor_is_inside_moves_it_where_it_can_be_seen() {
    // A cursor nobody can see is a cursor that types where nobody is
    // looking, which is the one thing folding must never allow.
    let (mut app, _rx) = editor();
    let path = scratch("folding-cursor.rs");
    std::fs::write(&path, "fn one() {\n    let a = 1;\n    let b = 2;\n}\n").expect("written");
    app.open_path(&path);

    app.go_to_line(2);
    app.run(Cmd::FOLD);
    assert_eq!(
        line_now(&app),
        0,
        "the cursor came out onto the line the fold is folded onto"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn folding_and_unfolding_are_the_same_key() {
    let (mut app, _rx) = editor();
    let path = scratch("folding-toggle.rs");
    std::fs::write(&path, "fn one() {\n    let a = 1;\n}\n").expect("written");
    app.open_path(&path);
    app.go_to_line(0);

    app.run(Cmd::TOGGLE_FOLD);
    assert_eq!(app.view().folds.len(), 1);
    app.run(Cmd::TOGGLE_FOLD);
    assert!(app.view().folds.is_empty(), "and back again");
    std::fs::remove_file(&path).ok();
}

#[test]
fn folding_everything_leaves_the_file_as_a_list_of_what_is_in_it() {
    let (mut app, _rx) = editor();
    let path = scratch("folding-all.rs");
    std::fs::write(
        &path,
        "fn one() {\n    let a = 1;\n}\n\nfn two() {\n    let b = 2;\n}\n",
    )
    .expect("written");
    app.open_path(&path);

    app.run(Cmd::FOLD_ALL);
    assert_eq!(app.view().folds.len(), 2, "both functions, neither nested");
    app.run(Cmd::UNFOLD_ALL);
    assert!(app.view().folds.is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_file_with_no_parse_tree_says_why_it_will_not_fold() {
    let (mut app, _rx) = editor();
    typed(&mut app, "just some words\nand more of them\n");
    app.run(Cmd::FOLD);
    assert!(app.status.text.contains("no parse tree"), "{}", app.status.text);
}

#[test]
fn moving_down_from_a_folded_line_lands_after_what_is_folded() {
    let (mut app, _rx) = editor();
    let path = scratch("folding-down.rs");
    std::fs::write(
        &path,
        "fn one() {\n    let a = 1;\n    let b = 2;\n}\nfn two() {}\n",
    )
    .expect("written");
    app.open_path(&path);
    app.go_to_line(0);
    app.run(Cmd::FOLD);
    app.run(Cmd::MOVE_DOWN);
    assert_eq!(
        line_now(&app),
        4,
        "over the folded body rather than into it"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_bookmark_follows_the_line_it_was_put_on() {
    // The whole reason a bookmark is a position rather than a line number:
    // adding lines above it must carry it down with the code it marked.
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree\n");
    app.go_to_line(2);
    app.run(Cmd::TOGGLE_BOOKMARK);
    assert_eq!(app.here().bookmark_lines(), vec![2]);

    app.go_to_line(0);
    typed(&mut app, "new\n");
    assert_eq!(
        app.here().bookmark_lines(),
        vec![3],
        "it moved down with its line"
    );
}

#[test]
fn stepping_through_bookmarks_comes_round_the_end_of_the_file() {
    let (mut app, _rx) = editor();
    typed(&mut app, "a\nb\nc\nd\ne\n");
    for line in [1, 3] {
        app.go_to_line(line);
        app.run(Cmd::TOGGLE_BOOKMARK);
    }
    app.go_to_line(0);
    app.run(Cmd::NEXT_BOOKMARK);
    assert_eq!(line_now(&app), 1);
    app.run(Cmd::NEXT_BOOKMARK);
    assert_eq!(line_now(&app), 3);
    // Past the last one is round to the first, not "no more bookmarks".
    app.run(Cmd::NEXT_BOOKMARK);
    assert_eq!(line_now(&app), 1);
    app.run(Cmd::PREV_BOOKMARK);
    assert_eq!(line_now(&app), 3);
}

#[test]
fn a_bookmark_toggles_off_the_line_it_is_on() {
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\n");
    app.go_to_line(1);
    app.run(Cmd::TOGGLE_BOOKMARK);
    app.run(Cmd::TOGGLE_BOOKMARK);
    assert!(app.here().bookmark_lines().is_empty());
    app.run(Cmd::NEXT_BOOKMARK);
    assert_eq!(app.status.text, "no bookmarks in this file");
}

#[test]
fn a_macro_does_again_what_was_recorded() {
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree\n");
    app.go_to_line(0);

    app.run(Cmd::RECORD_MACRO);
    typed(&mut app, "- ");
    app.run(Cmd::MOVE_DOWN);
    app.run(Cmd::MOVE_LINE_START);
    app.run(Cmd::RECORD_MACRO);
    assert_eq!(app.here().rope.to_string(), "- one\ntwo\nthree\n");

    app.run(Cmd::PLAY_MACRO);
    app.run(Cmd::PLAY_MACRO);
    assert_eq!(app.here().rope.to_string(), "- one\n- two\n- three\n");
}

#[test]
fn the_recorder_does_not_record_itself() {
    // Both halves: stopping is not in the recording, and neither is
    // playing it — a macro with "play the macro" in it never comes back.
    let (mut app, _rx) = editor();
    typed(&mut app, "x");
    app.run(Cmd::RECORD_MACRO);
    assert!(app.is_recording());
    typed(&mut app, "y");
    app.run(Cmd::RECORD_MACRO);
    assert!(!app.is_recording());
    assert_eq!(app.recorder.kept().len(), 1, "only the typing was kept");
    app.run(Cmd::PLAY_MACRO);
    assert_eq!(app.here().rope.to_string(), "xyy");
}

#[test]
fn playing_nothing_says_so_rather_than_doing_nothing_quietly() {
    let (mut app, _rx) = editor();
    app.run(Cmd::PLAY_MACRO);
    assert_eq!(app.status.text, "nothing recorded to play");
}

#[test]
fn a_place_named_on_the_command_line_is_where_the_cursor_lands() {
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree\nfour");
    app.jump_to(2, 3);
    let at = app.view().cursor();
    assert_eq!(text::line_of(&app.here().rope, at), 2);
    assert_eq!(at - text::line_start(&app.here().rope, 2), 3);
}
