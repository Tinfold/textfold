//! The editor's own tests.
//!
//! In a file of their own rather than at the bottom of the module they test,
//! because there are a great many of them and they are the last thing anybody
//! scrolling through `app` is looking for. A child module all the same, so
//! they reach the same private things they always did.

use super::*;
use serde_json::json;
use std::sync::mpsc;

/// An editor with nothing of yours in it: the settings are the defaults
/// rather than whatever is in your home directory, so a test does not pass
/// or fail depending on whose machine it is on.
fn editor() -> (App, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(Config::default(), tx);
    app.screen = Rect::new(0, 0, 100, 30);
    for pane in &mut app.panes {
        pane.area = Rect::new(6, 1, 90, 28);
        pane.frame = Rect::new(0, 1, 100, 28);
    }
    (app, rx)
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("textfold-app-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).expect("a place to work");
    dir.join(name)
}

/// Which line the cursor is on now.
fn line_now(app: &App) -> usize {
    text::line_of(&app.here().rope, app.view().cursor())
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        if c == '\n' {
            app.run(Cmd::INSERT_NEWLINE);
        } else {
            app.type_char(c);
        }
    }
}

/// What a plugin asked for, answered the way the event loop answers it.
fn plugin_asks(app: &mut App, method: &str, params: serde_json::Value) -> Result<Value, String> {
    match app.plugin_asked(HostId(0), method, &params, Some(&json!(1))) {
        Answer::Now(value) => Ok(value),
        Answer::No(why) => Err(why),
        Answer::Later => Ok(json!("later")),
    }
}

/// A keystroke through the whole loop, so that everything `handle` does
/// afterwards — including noticing that a box has gone — happens too.
fn pressed(app: &mut App, key: &str) {
    let key = Key::parse(key).expect("a key");
    app.handle(Event::Term(TermEvent::Key(KeyEvent::new(key.code, key.mods))));
}

/// Everything one row of a panel says, run together, with the stretches
/// that do something marked. What a person sees, near enough.
fn row(line: &Value) -> String {
    line.get("spans")
        .and_then(Value::as_array)
        .map(|spans| {
            spans
                .iter()
                .map(|span| {
                    let text = span.get("text").and_then(Value::as_str).unwrap_or("");
                    match span.get("action").and_then(Value::as_str) {
                        Some(action) => format!("[{text}→{action}]"),
                        None => text.to_string(),
                    }
                })
                .collect::<String>()
        })
        .unwrap_or_else(|| line.as_str().unwrap_or("").to_string())
}

#[test]
fn the_debugger_says_what_can_be_done_and_not_only_what_happened() {
    // A debugger is very often the first thing in an editor somebody uses
    // before they have learned any of its keys, and the panel is already
    // on the screen saying where the program stopped. Making them find
    // `debug-step-over` in the command palette to act on that is asking
    // them to read the manual with the answer in front of them.
    let (app, _rx) = editor();
    let buttons = row(&app.debug_panel_lines()[0]);
    for label in ["Start", "Pause", "Over", "Into", "Out", "Stop"] {
        assert!(buttons.contains(label), "no {label} button: {buttons}");
    }
    // With nothing running, the only one that does anything is the one
    // that starts something. The rest are drawn and inert rather than
    // absent: a row whose buttons come and go is a row you have to read
    // every time.
    assert!(buttons.contains("[ ▶ Start →do:start]"), "{buttons}");
    assert!(!buttons.contains("→do:over"), "{buttons}");
    assert!(!buttons.contains("→do:stop"), "{buttons}");
    assert!(!buttons.contains("→do:pause"), "{buttons}");
}

#[test]
fn a_language_that_has_to_be_compiled_offers_that_where_you_are_looking() {
    // The other half of the same complaint. `gdb` cannot open a program
    // nobody has built, and an editor that knows that and offers no way to
    // build it has left the interesting half in another window.
    let (mut app, _rx) = editor();
    let c = scratch("buttons.c");
    std::fs::write(&c, "int main(void) { return 0; }\n").expect("written");
    app.open_path(&c);
    let buttons = row(&app.debug_panel_lines()[0]);
    assert!(buttons.contains("→do:build"), "no way to build a C file: {buttons}");

    // And a language with nothing to build says nothing about building.
    let py = scratch("buttons.py");
    std::fs::write(&py, "print(1)\n").expect("written");
    app.open_path(&py);
    let buttons = row(&app.debug_panel_lines()[0]);
    assert!(!buttons.contains("→do:build"), "{buttons}");
    std::fs::remove_file(&c).ok();
    std::fs::remove_file(&py).ok();
}

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
fn a_list_a_plugin_put_up_answers_with_the_row_that_was_picked() {
    let (mut app, _rx) = editor();
    let asked = app.plugin_asked(
        HostId(0),
        "pick",
        &json!({ "title": "Which board?", "items": [
            { "label": "Nucleo F401RE", "value": "f401re" },
            { "label": "Discovery F407", "value": "f407" }
        ]}),
        Some(&json!(1)),
    );
    assert!(matches!(asked, Answer::Later), "the person has not answered yet");
    assert!(app.plugin_waiting.is_some());
    match &app.overlay {
        Overlay::Picker(picker) => assert_eq!(picker.title(), "Which board?"),
        _ => panic!("no list went up"),
    }

    pressed(&mut app, "down");
    pressed(&mut app, "enter");
    assert!(
        app.plugin_waiting.is_none(),
        "the plugin should have been answered"
    );
    assert!(matches!(app.overlay, Overlay::None));
}

#[test]
fn changing_your_mind_about_a_plugins_question_is_still_an_answer() {
    // The property that matters most about all of these: Escape is the
    // commonest thing anybody does to a box, and a plugin that got nothing
    // back would wait for ever.
    for (method, params) in [
        ("pick", json!({ "items": ["one", "two"] })),
        ("prompt", json!({ "title": "Which port?" })),
        ("confirm", json!({ "text": "Erase the chip?" })),
        ("menu", json!({ "items": ["Input", "Output"] })),
    ] {
        let (mut app, _rx) = editor();
        app.plugin_asked(HostId(0), method, &params, Some(&json!(1)));
        assert!(app.plugin_waiting.is_some(), "{method} did not ask");
        pressed(&mut app, "esc");
        assert!(
            app.plugin_waiting.is_none(),
            "{method} left the plugin waiting on a box that had gone"
        );
    }
}

#[test]
fn a_plugins_second_question_does_not_leave_the_first_hanging() {
    // The second box replaces the first on the screen, so the first has to
    // be answered with nothing rather than quietly forgotten.
    let (mut app, _rx) = editor();
    app.plugin_asked(HostId(0), "prompt", &json!({}), Some(&json!(1)));
    let first = app.plugin_waiting.as_ref().map(|a| a.request.clone());
    assert_eq!(first, Some(json!(1)));

    app.plugin_asked(HostId(0), "confirm", &json!({ "text": "sure?" }), Some(&json!(2)));
    assert_eq!(
        app.plugin_waiting.as_ref().map(|a| a.request.clone()),
        Some(json!(2)),
        "the second question should be the one waiting now"
    );
}

#[test]
fn a_plugins_menu_opens_where_the_cursor_is_and_answers_what_was_picked() {
    let (mut app, _rx) = editor();
    app.caret = Some((40, 12));

    let asked = app.plugin_asked(
        HostId(0),
        "menu",
        &json!({ "items": [
            { "label": "Go to it", "value": "go" },
            null,
            { "label": "Input",  "value": "in" },
            { "label": "Analog", "value": "analog", "enabled": false }
        ]}),
        Some(&json!(1)),
    );
    assert!(matches!(asked, Answer::Later));

    match &app.overlay {
        Overlay::Menu(menu) => {
            // Where the pointer is, not the middle of the screen. That is
            // the whole difference between this and `pick`.
            assert_eq!(menu.anchor, (40, 12));
            assert_eq!(menu.len(), 4, "the divider is a row too");
        }
        _ => panic!("no menu opened"),
    }

    pressed(&mut app, "enter");
    assert!(app.plugin_waiting.is_none(), "the plugin should have its answer");
    assert!(matches!(app.overlay, Overlay::None));
}

#[test]
fn a_menu_with_nothing_to_choose_is_not_put_up_at_all() {
    // Dividers are rows but not choices. A menu of nothing but lines
    // would be a box you cannot get out of except by escaping it.
    let (mut app, _rx) = editor();
    match app.plugin_asked(HostId(0), "menu", &json!({ "items": [null, null] }), Some(&json!(1))) {
        Answer::No(why) => assert!(why.contains("nothing")),
        _ => panic!("it should have been turned down"),
    }
    assert!(matches!(app.overlay, Overlay::None));
}

#[test]
fn a_question_told_rather_than_asked_is_turned_down() {
    // A notification has no id, so there is nowhere to send the answer.
    // Better to say so than to put a box on the screen that answers into
    // the void.
    let (mut app, _rx) = editor();
    match app.plugin_asked(HostId(0), "pick", &json!({ "items": ["a"] }), None) {
        Answer::No(why) => assert!(why.contains("asked")),
        _ => panic!("it should have been turned down"),
    }
    assert!(matches!(app.overlay, Overlay::None));
}

// ---- Debugging ----

#[test]
fn a_breakpoint_follows_the_text_it_was_put_on() {
    // The bug this is about: put a breakpoint on line ten, add an import
    // at the top, and the debugger stops on line nine — which is a line
    // you did not choose, doing something you did not ask for, and looks
    // for all the world like a debugger that is broken.
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree\n");
    let id = app.view().doc;
    app.doc_mut(id).expect("a buffer").toggle_breakpoint(2);
    assert_eq!(app.doc(id).expect("a buffer").breakpoint_lines(), vec![2]);

    // A line put in above it.
    app.go_to(0, 0);
    typed(&mut app, "zero\n");
    assert_eq!(
        app.doc(id).expect("a buffer").breakpoint_lines(),
        vec![3],
        "the breakpoint should have moved down with `three`"
    );
}

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
fn a_breakpoint_in_a_buffer_with_no_file_is_not_offered_to_the_adapter() {
    // An adapter is told about breakpoints by path. A scratch buffer has
    // none, and an adapter told about a file that does not exist answers
    // with an error rather than ignoring it.
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\n");
    let id = app.view().doc;
    app.doc_mut(id).expect("a buffer").toggle_breakpoint(0);
    assert!(app.breakpoints_now().is_empty());
}

#[test]
fn a_cursor_is_never_put_past_the_end_of_the_buffer() {
    let (mut app, _rx) = editor();
    typed(&mut app, "short\n");
    app.place_cursor(10_000, false, false);
    assert_eq!(app.view().cursor(), app.here().len_chars());
}

#[test]
fn a_panel_can_never_hold_a_breakpoint() {
    // Not tidiness — this is what makes a feedback loop impossible. The
    // panel is filled by replacing its whole buffer, which reaches
    // `after_edit_to` as an edit like any other. If that edit could look
    // like one that moved a breakpoint, the adapter would be told, its
    // reply would refresh the panel, and the editor would never read
    // another keystroke. A buffer that cannot have one cannot start it.
    let (mut app, _rx) = editor();
    app.run(Cmd::DEBUG_PANEL);
    let id = app.debug_panel.expect("a panel");
    app.show(id);
    app.go_to(0, 0);
    app.run(Cmd::TOGGLE_BREAKPOINT);
    assert!(app.doc(id).expect("a buffer").breakpoints.is_empty());
    // And it is not a file, so it is never offered to the adapter either.
    assert!(app.breakpoints_now().iter().all(|(_, l)| !l.is_empty()));
}

#[test]
fn the_debug_panel_says_how_to_start_when_nothing_is_running() {
    // A panel that is empty until you have already worked out how to use
    // it is a panel that teaches nobody anything.
    let (mut app, _rx) = editor();
    app.run(Cmd::DEBUG_PANEL);
    let id = app.debug_panel.expect("a panel");
    let text = app.doc(id).expect("a buffer").rope.to_string();
    assert!(text.contains("Nothing is being debugged"), "{text}");
    assert!(text.contains("F5"), "it should say which key: {text}");
    assert!(text.contains("F9"), "and which one sets a breakpoint: {text}");
    // And it is docked along the bottom rather than opened as a tab.
    let at = app.pane_showing_docked(id).expect("docked");
    assert_eq!(
        app.panes[at].dock.map(|d| d.edge),
        Some(crate::view::Edge::Bottom)
    );

    // Running the command again puts it away, which is what a docked
    // panel's own command means everywhere else in the editor. The pane
    // goes and the buffer stays — a dock that gave its buffer back to the
    // pool would leave a sideways second copy of the file you are editing
    // where the panel was, which is exactly what it used to do.
    app.run(Cmd::DEBUG_PANEL);
    assert!(app.pane_showing_docked(id).is_none(), "the pane should go");
    assert!(
        app.panes.iter().all(|p| p.doc != id),
        "and nothing should be left showing the panel's buffer"
    );
    // The buffer goes too, or it turns into a tab called `Debug` in the
    // row at the top — a docked panel that has lost its dock is an
    // ordinary buffer, and nobody asked for one.
    assert!(app.doc(id).is_none(), "the buffer should have gone");
    assert!(app.debug_panel.is_none());

    // And showing it again builds a fresh one rather than a second dock.
    app.run(Cmd::DEBUG_PANEL);
    let again = app.debug_panel.expect("a panel");
    assert!(app.pane_showing_docked(again).is_some());
    assert_eq!(
        app.panes.iter().filter(|p| p.dock.is_some()).count(),
        1,
        "one panel, not two"
    );
}

#[test]
fn closing_a_panels_buffer_takes_its_sidebar_with_it() {
    // The bug: a docked pane whose buffer was closed was handed "whatever
    // was looked at most recently" like any other pane — which turned the
    // debugger's panel into a second, sideways copy of the file you were
    // editing, sitting along the bottom of the screen where the stack had
    // been.
    let (mut app, _rx) = editor();
    let file = app.view().doc;
    app.run(Cmd::DEBUG_PANEL);
    let panel = app.debug_panel.expect("a panel");
    assert!(app.pane_showing_docked(panel).is_some());

    app.close_doc(panel);
    assert!(
        app.panes.iter().all(|p| p.dock.is_none()),
        "the dock should have gone with the buffer it was showing"
    );
    assert!(app.panes.iter().all(|p| p.doc == file));
}

#[test]
fn stepping_with_nothing_running_says_so_rather_than_doing_nothing() {
    let (mut app, _rx) = editor();
    app.run(Cmd::DEBUG_STEP_OVER);
    assert!(app.status.text.contains("F5"), "{}", app.status.text);
    app.run(Cmd::DEBUG_STOP);
    assert!(
        app.status.text.contains("nothing is being debugged"),
        "{}",
        app.status.text
    );
}

#[test]
fn there_is_no_arrow_in_the_margin_when_nothing_is_stopped() {
    let (app, _rx) = editor();
    assert!(app.stopped_at().is_none());
}

#[test]
fn breakpoints_can_be_cleared_in_this_file_or_everywhere() {
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree\n");
    let first = app.view().doc;
    let doc = app.doc_mut(first).expect("a buffer");
    doc.toggle_breakpoint(0);
    doc.toggle_breakpoint(2);

    // A second file, with one of its own.
    app.run(Cmd::NEW);
    typed(&mut app, "four\nfive\n");
    let second = app.view().doc;
    assert_ne!(first, second);
    app.doc_mut(second).expect("a buffer").toggle_breakpoint(1);

    // Clearing this file leaves the other one alone, which is the whole
    // reason there are two commands.
    app.run(Cmd::CLEAR_BREAKPOINTS);
    assert!(app.doc(second).expect("a buffer").breakpoints.is_empty());
    assert_eq!(app.doc(first).expect("a buffer").breakpoint_lines(), vec![0, 2]);

    app.run(Cmd::CLEAR_ALL_BREAKPOINTS);
    assert!(app.doc(first).expect("a buffer").breakpoints.is_empty());
    assert!(app.status.text.contains("gone"), "{}", app.status.text);
}

#[test]
fn clearing_breakpoints_says_which_file_it_was_about() {
    // "All 14 breakpoints are gone" when you meant to clear one file is a
    // thing to find out now rather than the next time you run.
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\n");
    app.run(Cmd::CLEAR_BREAKPOINTS);
    assert!(app.status.text.contains("there were none in"), "{}", app.status.text);
}

#[test]
fn a_panel_gets_the_keys_that_would_have_changed_the_text() {
    let (mut app, _rx) = editor();
    let key = |text: &str| Key::parse(text).expect("a key");

    // In an ordinary buffer, nothing is a plugin's.
    assert!(!app.panel_wants(key("r")));

    let id = app.view().doc;
    if let Some(doc) = app.doc_mut(id) {
        doc.read_only = true;
        doc.panel = Some(crate::doc::Panel {
            owner: crate::doc::Owner::Plugin("cargo".into()),
            id: "cargo/report".into(),
            spans: Vec::new(),
            actions: Vec::new(),
        });
    }

    // A plain letter would have typed a character, and a panel is not
    // yours to type into — so it is the plugin's.
    assert!(app.panel_wants(key("r")));
    assert!(app.panel_wants(key("c")));
    // So is Enter, which would have made a newline.
    assert!(app.panel_wants(key("enter")));

    // But nothing anybody knows is taken. Every one of these still does
    // what it does everywhere else in the editor.
    for text in ["ctrl-p", "ctrl-w", "ctrl-q", "down", "ctrl-f", "alt-,", "f8"] {
        assert!(
            !app.panel_wants(key(text)),
            "{text} should still be the editor's"
        );
    }
}

/// An offer, as a plugin would have made it.
fn suggesting(app: &mut App, text: &str) {
    let at = app.view().cursor();
    let id = app.view().doc;
    if let Some(doc) = app.doc_mut(id) {
        doc.hint = Some(crate::doc::Hint {
            plugin: "copilot".into(),
            at,
            text: text.into(),
        });
    }
}

#[test]
fn taking_a_suggestion_puts_it_in_as_one_thing_to_undo() {
    let (mut app, _rx) = editor();
    typed(&mut app, "let x = ");
    suggesting(&mut app, "1 + 2;");

    assert!(app.hint_showing());
    pressed(&mut app, "tab");
    assert_eq!(app.here().text(), "let x = 1 + 2;");
    // The cursor ends where it would have if you had typed it.
    assert_eq!(app.view().cursor(), "let x = 1 + 2;".chars().count());
    // And it is your text now, in every way — including undoably.
    app.run(Cmd::UNDO);
    assert_eq!(app.here().text(), "let x = ");
}

#[test]
fn tab_is_still_tab_when_nothing_is_being_offered() {
    // The key is not conditional, the offer is. An editor where Tab
    // stopped indenting because a plugin was installed would be an editor
    // nobody would install the plugin into.
    let (mut app, _rx) = editor();
    typed(&mut app, "x");
    assert!(!app.hint_showing());
    pressed(&mut app, "tab");
    assert!(
        app.here().text().starts_with('x') && app.here().text().len() > 1,
        "tab should have indented, got {:?}",
        app.here().text()
    );
}

#[test]
fn an_offer_goes_when_you_walk_away_from_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "hello");
    suggesting(&mut app, " world");
    assert!(app.hint_showing());

    pressed(&mut app, "left");
    assert!(
        app.here().hint.is_none(),
        "moving off an offer is declining it"
    );
}

#[test]
fn an_offer_goes_when_the_text_it_was_about_changes() {
    // It was worked out against the text as it was. The same rule an edit
    // computed against an old version gets, arrived at from the other side.
    let (mut app, _rx) = editor();
    typed(&mut app, "hello");
    suggesting(&mut app, " world");
    typed(&mut app, "!");
    assert!(app.here().hint.is_none());
}

#[test]
fn escape_waves_an_offer_away_without_taking_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "hello");
    suggesting(&mut app, " world");
    pressed(&mut app, "esc");
    assert!(app.here().hint.is_none());
    assert_eq!(app.here().text(), "hello", "escape should not have taken it");
}

#[test]
fn a_panels_colours_line_up_with_its_text() {
    let (text, spans, actions) = panel_lines(&[
        json!({ "spans": [
            { "text": "USART2", "style": "keyword" },
            { "text": "  TX ", "style": "muted" },
            { "text": "PA2", "style": "string", "action": "pin:PA2" }
        ]}),
        json!(""),
        json!("plain line"),
    ]);
    assert_eq!(text, "USART2  TX PA2\n\nplain line\n");

    // Every span points at exactly the words it was given.
    let at = |r: Range| text.chars().skip(r.start()).take(r.len()).collect::<String>();
    assert_eq!(at(spans[0].0), "USART2");
    assert_eq!(at(spans[1].0), "  TX ");
    assert_eq!(at(spans[2].0), "PA2");
    assert_eq!(actions.len(), 1, "only one span said it does anything");
    assert_eq!(at(actions[0].0), "PA2");
    assert_eq!(actions[0].1, "pin:PA2");
}

#[test]
fn a_panel_is_counted_in_characters_and_not_in_bytes() {
    // A box-drawing character is three bytes and one column. Counting
    // bytes here would put every colour on a line after the first
    // non-ASCII character in the wrong place.
    let (text, spans, _) = panel_lines(&[json!({ "spans": [
        { "text": "▸ ", "style": "muted" },
        { "text": "ADC1", "style": "keyword" }
    ]})]);
    assert_eq!(text, "▸ ADC1\n");
    let second = spans[1].0;
    assert_eq!(
        text.chars().skip(second.start()).take(second.len()).collect::<String>(),
        "ADC1"
    );
}

#[test]
fn a_style_a_plugin_asks_for_is_the_themes_own() {
    // Names rather than colours, so a panel is themed with everything
    // else. Tree-sitter's names, which the editor already knows...
    assert_eq!(panel_role("keyword"), Some(crate::theme::Role::Keyword));
    assert_eq!(panel_role("string"), Some(crate::theme::Role::String));
    // ...as specific as the theme actually goes...
    assert_eq!(
        panel_role("keyword.control"),
        Some(crate::theme::Role::KeywordControl)
    );
    // ...and falling back along the dots when it goes further, the way a
    // grammar's capture does.
    assert_eq!(panel_role("keyword.made.up"), Some(crate::theme::Role::Keyword));
    // ...plus the couple a plugin author reaches for that no grammar has.
    assert_eq!(panel_role("muted"), Some(crate::theme::Role::Comment));
    // A name nobody knows is drawn as ordinary text rather than refused:
    // a panel with one style misspelt should still be a readable panel.
    assert_eq!(panel_role("fuchsia"), None);
}

#[test]
fn only_the_marked_parts_of_a_panel_do_anything() {
    let (mut app, _rx) = editor();
    let id = app.view().doc;
    if let Some(doc) = app.doc_mut(id) {
        doc.panel = Some(crate::doc::Panel {
            owner: crate::doc::Owner::Plugin("cargo".into()),
            id: "cargo/report".into(),
            spans: Vec::new(),
            actions: vec![(Range::new(4, 9), "go:somewhere".into())],
        });
    }
    // Inside the marked stretch, and at its first character.
    assert!(app.panel_action_at(4));
    assert!(app.panel_action_at(8));
    // Just past the end, and before the start. Enter there should go on to
    // mean what Enter usually means rather than being quietly eaten.
    assert!(!app.panel_action_at(9));
    assert!(!app.panel_action_at(3));
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
fn a_plugin_can_read_a_buffer_and_change_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "one\ntwo\nthree");

    let read = plugin_asks(&mut app, "buffer/read", json!({})).expect("it should read");
    assert_eq!(read["text"], "one\ntwo\nthree");
    let version = read["version"].clone();

    // Line and column both counted from zero, in characters.
    let done = plugin_asks(
        &mut app,
        "buffer/edit",
        json!({ "version": version, "edits": [
            { "line": 1, "column": 0, "end_line": 1, "end_column": 3, "text": "TWO" }
        ]}),
    )
    .expect("it should apply");
    assert_eq!(done["applied"], 1);
    assert_eq!(app.here().text(), "one\nTWO\nthree");

    // And it went through the same door a keystroke does, so it is one
    // thing to undo — which is the whole reason for insisting on that.
    app.run(Cmd::UNDO);
    assert_eq!(app.here().text(), "one\ntwo\nthree");
}

#[test]
fn an_edit_worked_out_against_older_text_is_refused_rather_than_applied() {
    let (mut app, _rx) = editor();
    typed(&mut app, "hello");
    let stale = app.here().version;
    typed(&mut app, " there");
    assert_ne!(app.here().version, stale, "typing should move the version on");

    // A plugin holding an edit for text that is no longer there would
    // corrupt the file rather than fix it, so it is turned down and told
    // why — not applied, and not silently dropped either.
    let refused = plugin_asks(
        &mut app,
        "buffer/edit",
        json!({ "version": stale, "edits": [
            { "line": 0, "column": 0, "end_line": 0, "end_column": 5, "text": "goodbye" }
        ]}),
    );
    assert!(
        refused.is_err_and(|why| why.contains(&stale.to_string())),
        "a stale edit should say which version it was for"
    );
    assert_eq!(app.here().text(), "hello there");
}

#[test]
fn a_plugin_that_says_nothing_is_not_given_the_status_line() {
    let (mut app, _rx) = editor();
    assert!(plugin_asks(&mut app, "status/say", json!({ "text": "  " })).is_err());
    assert!(plugin_asks(&mut app, "status/say", json!({ "text": "building" })).is_ok());
}

#[test]
fn problems_from_a_plugin_that_is_not_running_go_nowhere() {
    // The id names no host, which is what a message arriving after one has
    // died looks like.
    let (mut app, _rx) = editor();
    assert_eq!(
        plugin_asks(&mut app, "diagnostics/set", json!({ "items": [] })),
        Err("that plugin is not running".into())
    );
}

/// The keystroke another program sends to say "open this", as bytes on the
/// way in rather than as a call to `open_path`.
fn keyed(app: &mut App, key: &str) {
    let key = Key::parse(key).expect("a key");
    app.on_key(KeyEvent::new(key.code, key.mods));
}

/// A completion list as a server would have sent it, for a file with
/// `at` characters of a word typed so far.
fn suggested(app: &mut App, at: usize, incomplete: bool, items: Value) {
    app.suggest_for_test(at, incomplete, items);
}

fn offered(title: &str, kind: &str) -> Value {
    serde_json::json!({ "title": title, "kind": kind })
}

#[test]
fn what_two_servers_offer_ends_up_in_one_list() {
    // Which is the whole of the Python case: `ruff` knows how to take the
    // unused import out, `pyright` knows where the missing one lives, and
    // asking only whichever answers first gets you one of those.
    let (linter, checker) = (ServerId(0), ServerId(1));
    let mut gathered = Gathered::new(DocId(1), 12, vec![linter, checker]);
    assert!(!gathered.settled(), "nobody has answered yet");

    gathered.take(linter, serde_json::json!([offered("Remove unused import", "quickfix")]));
    assert!(!gathered.settled(), "the other one is still thinking");
    assert_eq!(gathered.len(), 1);

    gathered.take(checker, serde_json::json!([offered("Add import os", "quickfix")]));
    assert!(gathered.settled());
    let titles: Vec<&str> = gathered
        .actions()
        .iter()
        .filter_map(|(_, a)| a.get("title").and_then(Value::as_str))
        .collect();
    assert_eq!(titles, ["Remove unused import", "Add import os"]);
    // And each row still knows who to send the choice back to.
    assert_eq!(gathered.actions()[0].0, linter);
    assert_eq!(gathered.actions()[1].0, checker);
}

#[test]
fn a_server_answering_twice_replaces_its_own_and_leaves_the_rest() {
    let (linter, checker) = (ServerId(0), ServerId(1));
    let mut gathered = Gathered::new(DocId(1), 0, vec![linter, checker]);
    gathered.take(linter, serde_json::json!([offered("First go", "quickfix")]));
    gathered.take(checker, serde_json::json!([offered("From the checker", "quickfix")]));
    gathered.take(linter, serde_json::json!([offered("Second go", "quickfix")]));
    let titles: Vec<&str> = gathered
        .actions()
        .iter()
        .filter_map(|(_, a)| a.get("title").and_then(Value::as_str))
        .collect();
    assert_eq!(titles, ["Second go", "From the checker"]);
}

#[test]
fn a_server_with_nothing_to_say_does_not_hold_the_list_up() {
    let (quiet, useful) = (ServerId(0), ServerId(1));
    let mut gathered = Gathered::new(DocId(1), 0, vec![quiet, useful]);
    gathered.take(quiet, Value::Null);
    assert!(gathered.is_empty());
    assert!(!gathered.settled());
    gathered.take(useful, serde_json::json!([offered("Fix it", "quickfix")]));
    assert!(gathered.settled());
    assert_eq!(gathered.len(), 1);
    assert_eq!(gathered.headline(), Some("Fix it"));
}

#[test]
fn every_row_says_which_server_offered_it_when_two_did() {
    let both = vec![
        (ServerId(0), offered("From the linter", "quickfix")),
        (ServerId(1), offered("From the checker", "quickfix")),
    ];
    let rows = action_rows(&both);
    assert!(rows.iter().all(|r| r.detail.is_some()), "{rows:?}");
    // One server offering two things needs no such note: there is nothing
    // to tell apart.
    let one = vec![
        (ServerId(0), offered("A", "quickfix")),
        (ServerId(0), offered("B", "quickfix")),
    ];
    let rows = action_rows(&one);
    assert!(rows.iter().all(|r| r.detail.is_none()));
    assert_eq!(rows[0].tag.as_deref(), Some("quickfix"));
}

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
fn a_name_the_file_has_not_imported_shows_where_it_comes_from() {
    // What rust-analyzer sends for a name you have not imported: the
    // module in the label details rather than in the label, so that what
    // you typed still matches what you are being offered.
    let (mut app, _rx) = editor();
    typed(&mut app, "HashMa");
    suggested(
        &mut app,
        6,
        true,
        json!([{
            "label": "HashMap",
            "labelDetails": {
                "detail": "(use std::collections::HashMap)",
                "description": "HashMap<K, V>",
            },
        }]),
    );

    let item = app.completion.as_ref().expect("a list").selected().unwrap();
    assert_eq!(item.label, "HashMap");
    assert_eq!(item.suffix.as_deref(), Some("(use std::collections::HashMap)"));
    assert_eq!(item.detail.as_deref(), Some("HashMap<K, V>"));
}

#[test]
fn a_partial_list_is_asked_for_again_rather_than_narrowed_to_nothing() {
    // A server asked about two characters offers some of what it could
    // reach and says there is more. Narrowing that is how a name you are
    // typing towards disappears before you have finished typing it.
    let (mut app, _rx) = editor();
    typed(&mut app, "Ha");
    suggested(&mut app, 2, true, json!([{ "label": "Handle" }]));
    assert_eq!(app.completion.as_ref().map(Completion::len), Some(1));

    typed(&mut app, "s");
    assert!(app.completion.is_none(), "nothing left matching `Has`");
    assert!(
        app.completion_due.is_some(),
        "the server has more to say and has not been asked"
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
fn a_complete_list_narrows_where_it_stands() {
    // The other half of it: a server that said it gave a full answer is
    // taken at its word, and typing does not go back to it.
    let (mut app, _rx) = editor();
    typed(&mut app, "Ha");
    suggested(
        &mut app,
        2,
        false,
        json!([{ "label": "Handle" }, { "label": "Hasty" }]),
    );
    typed(&mut app, "s");

    assert_eq!(app.completion.as_ref().map(Completion::len), Some(1));
    assert!(app.completion_due.is_none());
}

#[test]
fn backspace_narrows_the_list_rather_than_closing_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "Has");
    suggested(
        &mut app,
        3,
        false,
        json!([{ "label": "Handle" }, { "label": "Hasty" }]),
    );
    assert_eq!(app.completion.as_ref().map(Completion::len), Some(1));

    app.run(Cmd::DELETE_BACKWARD);
    assert_eq!(
        app.completion.as_ref().map(Completion::len),
        Some(2),
        "backspacing to `Ha` matches both again"
    );
}

#[test]
fn the_import_arrives_with_the_name_even_when_it_is_worked_out_late() {
    // Servers send the name first and the import it needs only when asked
    // about that one suggestion. Taking it before the answer is back has
    // to wait for the answer, not go without it.
    let (mut app, _rx) = editor();
    app.here_mut().language = crate::lang::LangId::PLAIN;
    typed(&mut app, "HashMa");
    suggested(&mut app, 6, false, json!([{ "label": "HashMap" }]));

    // As though a server had been asked and had not answered yet.
    let index = app.completion.as_ref().unwrap().shown[0];
    app.suggestion_mut(index).unwrap().resolve = Resolve::Waiting;
    app.accept_completion();

    assert_eq!(app.here().rope.to_string(), "HashMa", "nothing put in yet");
    assert_eq!(app.accept_when_resolved, Some(index));

    let doc = app.here().id;
    app.take_resolved_completion(
        doc,
        index,
        json!({
            "label": "HashMap",
            "additionalTextEdits": [{
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 0 },
                },
                "newText": "use std::collections::HashMap;\n",
            }],
        }),
    );

    assert_eq!(
        app.here().rope.to_string(),
        "use std::collections::HashMap;\nHashMap",
    );
    // And the cursor is after the name, not still up where the import
    // pushed the line it was on out of the way.
    assert_eq!(app.view().cursor(), app.here().len_chars());
    assert!(app.completion.is_none());
    assert_eq!(app.accept_when_resolved, None);
}

#[test]
fn an_import_that_never_comes_does_not_eat_the_keystroke() {
    // A server that fails the question has still been answered: the name
    // goes in without the import rather than nothing going in at all.
    let (mut app, _rx) = editor();
    typed(&mut app, "HashMa");
    suggested(&mut app, 6, false, json!([{ "label": "HashMap" }]));
    let index = app.completion.as_ref().unwrap().shown[0];
    app.suggestion_mut(index).unwrap().resolve = Resolve::Waiting;
    app.accept_completion();

    app.on_response(
        crate::lsp::ServerId(0),
        0,
        Err("content modified".to_string()),
    );
    // Nothing claimed that request, so the editor is still waiting on it;
    // the resolve coming back empty is what actually unsticks it.
    let doc = app.here().id;
    app.take_resolved_completion(doc, index, json!({ "label": "HashMap" }));

    assert_eq!(app.here().rope.to_string(), "HashMap");
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

fn typed_into_prompt(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
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
fn a_docstring_is_coloured_the_way_the_code_in_it_would_be() {
    lang::init();
    let rust = lang::by_tag("rust").expect("shipped");
    let hover = serde_json::json!({
        "kind": "markdown",
        "value": "Adds two numbers.\n\n```rust\nfn add(a: u32) -> u32\n```\n",
    });
    let lines = markup_lines(Some(&hover), rust);

    let prose = lines.iter().find(|l| l.text == "Adds two numbers.");
    assert!(prose.is_some_and(|l| l.spans.is_empty()), "{lines:?}");

    let code = lines
        .iter()
        .find(|l| l.text == "fn add(a: u32) -> u32")
        .expect("the example survived the fence");
    let coloured = |want: &str| {
        code.spans
            .iter()
            .find(|(range, _)| &code.text[range.clone()] == want)
            .map(|(_, role)| *role)
    };
    assert_eq!(coloured("fn"), Some(Role::Keyword));
    assert_eq!(coloured("add"), Some(Role::Function));
    assert_eq!(coloured("u32"), Some(Role::TypeBuiltin));
}

#[test]
fn a_fence_that_says_nothing_is_the_language_you_are_looking_at() {
    lang::init();
    let rust = lang::by_tag("rust").expect("shipped");
    let hover = serde_json::json!({ "value": "```\nlet x = 1;\n```" });
    let lines = markup_lines(Some(&hover), rust);
    let code = lines.iter().find(|l| l.text == "let x = 1;").expect("kept");
    assert!(
        code.spans.iter().any(|(_, role)| *role == Role::Keyword),
        "{code:?}"
    );

    // And a fence naming a language nothing here can parse is left plain
    // rather than coloured as whatever file you happened to be in.
    let hover = serde_json::json!({ "value": "```brainfuck\nlet x = 1;\n```" });
    let lines = markup_lines(Some(&hover), rust);
    let code = lines.iter().find(|l| l.text == "let x = 1;").expect("kept");
    assert!(code.spans.is_empty(), "{code:?}");
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
fn pointing_at_a_problem_says_what_is_wrong_with_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "let x = 1");
    app.here_mut().diagnostics = vec![crate::doc::Diagnostic {
        range: Range::new(4, 5),
        severity: crate::doc::Severity::Error,
        message: "cannot find value `x` in this scope".into(),
        source: Some("rustc".into()),
        code: Some("E0425".into()),
        data: None,
        told: crate::doc::Told::Server(0),
    }];
    let said: Vec<String> = app
        .problem_lines(4)
        .into_iter()
        .map(|l| l.text)
        .collect();
    assert_eq!(
        said,
        vec![
            "error (rustc E0425)".to_string(),
            "cannot find value `x` in this scope".to_string(),
        ]
    );
    assert!(
        app.problem_lines(8).is_empty(),
        "somewhere with nothing wrong with it should say nothing"
    );
}

#[test]
fn the_worst_problem_at_a_spot_is_read_first() {
    let (mut app, _rx) = editor();
    typed(&mut app, "let x = 1");
    let at = |severity, message: &str| crate::doc::Diagnostic {
        range: Range::new(4, 5),
        severity,
        message: message.into(),
        source: None,
        code: None,
        data: None,
        told: crate::doc::Told::Server(0),
    };
    app.here_mut().diagnostics = vec![
        at(crate::doc::Severity::Hint, "unused"),
        at(crate::doc::Severity::Error, "undefined"),
    ];
    let said: Vec<String> = app.problem_lines(4).into_iter().map(|l| l.text).collect();
    assert_eq!(said.first().map(String::as_str), Some("error"));
    assert!(said.contains(&"undefined".to_string()));
    assert!(said.contains(&"unused".to_string()));
    assert!(
        said.iter().position(|l| l == "undefined")
            < said.iter().position(|l| l == "unused"),
        "the hint came before the error: {said:?}"
    );
}

#[test]
fn asking_about_a_problem_with_no_server_still_shows_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "let x = 1");
    app.here_mut().diagnostics = vec![crate::doc::Diagnostic {
        range: Range::new(4, 5),
        severity: crate::doc::Severity::Warning,
        message: "x is never read".into(),
        source: Some("clippy".into()),
        code: None,
        data: None,
        told: crate::doc::Told::Server(0),
    }];
    app.ask_hover(4);
    let hover = app.hover.as_ref().expect("no box appeared");
    assert!(
        hover.lines.iter().any(|l| l.text == "x is never read"),
        "{:?}",
        hover.lines.iter().map(|l| &l.text).collect::<Vec<_>>()
    );
}

/// Two panes, each showing a file of its own, ready to be compared.
///
/// `tag` names the pair, because these run beside each other and two tests
/// writing to one path is two tests failing at random.
fn two_panes(tag: &str, left: &str, right: &str) -> (App, mpsc::Receiver<Event>, PathBuf) {
    let (mut app, rx) = editor();
    let a = scratch(&format!("{tag}-a.txt"));
    let b = scratch(&format!("{tag}-b.txt"));
    std::fs::write(&a, left).expect("written");
    std::fs::write(&b, right).expect("written");
    app.open_path(&a);
    app.run(Cmd::SPLIT);
    app.open_path(&b);
    assert_eq!(app.panes.len(), 2, "the split did not happen");
    (app, rx, a.parent().expect("a directory").to_path_buf())
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
fn comparing_needs_two_panes() {
    let (mut app, _rx) = editor();
    app.run(Cmd::DIFF_PANES);
    assert!(app.diff.is_none());
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
    app.tab_nudges = vec![(Rect::new(0, 0, 1, 1), 0)];
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
    app.tab_nudges = vec![(Rect::new(0, 0, 1, 1), 0)];
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
fn attaching_over_a_port_asks_which_one_and_remembers_the_answer() {
    // A port is a number nobody holds in their head, and it is different
    // for the two JVMs somebody has up. Remembered per *project*, because
    // a port in a settings file is per-plugin: it would be the port every
    // Java project you ever open tried to attach to.
    let (mut app, _rx) = editor();
    let path = scratch("attach-port.py");
    std::fs::write(&path, "print(1)\n").expect("written");
    app.open_path(&path);
    app.project = path.parent().expect("a directory").to_path_buf();

    app.run(Cmd::DEBUG_ATTACH);
    // A question rather than a list: there is one program waiting on the
    // port and nothing to choose between.
    let asked = match &app.overlay {
        Overlay::Prompt(prompt) => prompt.input.clone(),
        _ => panic!("no question was asked: {}", app.status.text),
    };
    // And the first guess is the adapter's own conventional port rather
    // than one number for everything: debugpy's is 5678, JDWP's is 5005,
    // and a default that is wrong for one of them is an edit every person
    // using it makes once, forever.
    assert_eq!(asked, "127.0.0.1:5678", "not what debugpy's own examples use");

    // Answering it attaches, and is remembered against this project.
    let root = app.attach_root().display().to_string();
    app.remember_address("127.0.0.1:5099");
    assert_eq!(
        app.config.debug_addresses.get(&root).map(String::as_str),
        Some("127.0.0.1:5099")
    );
    assert_eq!(app.remembered_address(), "127.0.0.1:5099", "asked twice");

    // And something that is not an address is refused rather than
    // half-understood.
    assert_eq!(crate::dap::read_address("localhost"), None);
    std::fs::remove_file(&path).ok();
}

#[test]
fn attaching_offers_the_projects_own_programs_first() {
    // On a machine with two hundred processes on it, the one you want is
    // nearly always the thing you just built — and the editor knows which
    // those are, so making somebody scroll past `pipewire` to find it
    // would be withholding an answer it already has.
    // Offering them first means knowing what each process is *running*, and
    // that is a thing only some machines will say: it comes out of `/proc`,
    // and a machine without one is listed through `ps`, which gives a command
    // line and no executable path. Where nothing can be placed in a project,
    // there is no order here to test — so this says so rather than failing for
    // the operating system it is on.
    if !crate::proc::running().iter().any(|p| p.program.is_some()) {
        return;
    }
    let (mut app, _rx) = editor();
    let path = scratch("attach-me.c");
    std::fs::write(&path, "int main(void){return 0;}\n").expect("written");
    app.open_path(&path);
    app.project = path.parent().expect("a directory").to_path_buf();

    // Something of the project's, and something that is not.
    let mine = app.project.join("mine");
    std::fs::copy("/bin/sleep", &mine).expect("a program to run");
    let mut ours = std::process::Command::new(&mine)
        .arg("30")
        .spawn()
        .expect("started");
    let mut theirs = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("started");

    // A process is not in `/proc` the instant `spawn` returns, and on a
    // loaded machine it can be several moments behind. The list is asked
    // for again rather than once, or this is a test that fails for being
    // run beside the others — and the budget is seconds rather than
    // tenths, because the machine this has to pass on is a shared one
    // building a hundred crates at the time.
    let mut rows: Vec<Row> = Vec::new();
    for _ in 0..120 {
        app.run(Cmd::DEBUG_ATTACH);
        rows = match &app.overlay {
            Overlay::Picker(picker) => {
                (0..picker.len()).filter_map(|at| picker.row(at).cloned()).collect()
            }
            _ => panic!("no list went up: {}", app.status.text),
        };
        let both = [ours.id(), theirs.id()].iter().all(|pid| {
            rows.iter()
                .any(|row| matches!(row.choice, Choice::Process(it) if it == *pid))
        });
        if both {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let at = |pid: u32| {
        rows.iter()
            .position(|row| matches!(row.choice, Choice::Process(it) if it == pid))
            .unwrap_or_else(|| panic!("{pid} was not offered"))
    };
    assert!(
        at(ours.id()) < at(theirs.id()),
        "something that is not the project's came first"
    );
    // And it says which is which, rather than leaving the order to be
    // noticed.
    assert_eq!(rows[at(ours.id())].tag.as_deref(), Some("here"));
    assert_eq!(rows[at(theirs.id())].tag, None);

    ours.kill().ok();
    ours.wait().ok();
    theirs.kill().ok();
    theirs.wait().ok();
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&mine).ok();
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

/// A pane holding a panel with one plain stretch and one that does
/// something, laid out so a test can point at either.
fn a_panel(app: &mut App) -> DocId {
    let id = app.new_scratch();
    if let Some(doc) = app.doc_mut(id) {
        doc.read_only = true;
        doc.panel = Some(crate::doc::Panel {
            // A plugin that is not running, so acting on it does nothing
            // at all — this is about what a click *moves*, not about what
            // the action does.
            owner: crate::doc::Owner::Plugin("nobody".into()),
            id: "tree".into(),
            spans: Vec::new(),
            actions: Vec::new(),
        });
    }
    app.show(id);
    app.write_panel(
        id,
        &[json!({ "spans": [
            { "text": "plain " },
            { "text": "[button]", "action": "open:1" },
        ]})],
    );
    id
}

#[test]
fn what_a_panel_offers_lights_up_under_the_pointer() {
    // A panel's actionable text is the only thing inside a pane that
    // behaves like a button, and it used to look exactly like the text
    // beside it: the only way to find out whether something could be
    // clicked was to click it.
    let (mut app, _rx) = editor();
    a_panel(&mut app);
    let area = app.panes[app.focus].area;
    // "plain " is six characters, so the button starts six columns in.
    let on_button = area.x + 8;
    let on_plain = area.x + 2;

    let lit = app.panel_action_under(app.focus, on_button, area.y);
    assert!(lit.is_some_and(|range| range.start() == 6 && range.end() == 14), "{lit:?}");
    // And the words beside it are words, not a button.
    assert_eq!(app.panel_action_under(app.focus, on_plain, area.y), None);
    // Nor is the blank line under it, which has no spans at all.
    assert_eq!(app.panel_action_under(app.focus, on_button, area.y + 1), None);
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
        rope: &doc.rope,
        hints: &[],
        width: app.panes[from].area.width as usize,
        tab_width: 4,
        wrap: true,
        folds: &[],
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
fn right_clicking_a_tab_offers_things_about_that_tab() {
    let (mut app, _rx) = editor();
    let path = scratch("tab-menu.txt");
    std::fs::write(&path, "text\n").expect("written");
    app.open_path(&path);
    let id = app.view().doc;
    app.tab_hits = vec![(Rect::new(0, 0, 10, 1), id, false)];

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
fn a_suggestion_is_drawn_in_the_colour_that_kind_of_thing_has_in_the_file() {
    // A list of forty suggestions all in one colour is a list you have to
    // read a word at a time to find the method among the fields.
    let role = |n| completion_role(n);
    // A method is a function, a field is a property, a class is a type,
    // and a keyword is a keyword. Nothing here is a new vocabulary.
    assert_eq!(role(2), Role::Function);
    assert_eq!(role(3), Role::Function);
    assert_eq!(role(5), Role::Property);
    assert_eq!(role(7), Role::Type);
    assert_eq!(role(14), Role::Keyword);
    assert_eq!(role(21), Role::Constant);
    // The four kinds that get asked about most are four different
    // colours, which is the whole point of doing this at all.
    let four = [role(3), role(5), role(7), role(14)];
    for (at, one) in four.iter().enumerate() {
        assert!(
            !four[at + 1..].contains(one),
            "{four:?} has two of the same colour in it"
        );
    }
    // Something a later LSP invents is drawn as ordinary text rather than
    // as a guess.
    assert_eq!(role(99), Role::Variable);
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

/// The parts of a rendered line a pointer would offer to follow.
fn followable(line: &DocLine) -> Vec<String> {
    line.links
        .iter()
        .map(|range| line.text.chars().skip(range.start).take(range.len()).collect())
        .collect()
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
fn a_lower_case_search_ignores_case_and_a_capital_means_it() {
    let (mut app, _rx) = editor();
    typed(&mut app, "Thing thing THING");
    assert_eq!(app.count_matches("thing"), 3);
    assert_eq!(app.count_matches("Thing"), 1);
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

/// A panel a plugin declared, as a `&'static Command` the editor can be
/// handed. Leaked, because that is what the registry hands out and the
/// command tables hold.
fn docked_panel(id: &str, edge: Option<&str>, size: Option<u16>) -> &'static crate::plugin::Command {
    let dock = edge.map(|e| {
        crate::view::Dock::new(crate::view::Edge::parse(e).expect("an edge"), size)
    });
    Box::leak(Box::new(crate::plugin::Command {
        id: id.to_string(),
        name: id.split('/').next_back().unwrap_or(id).to_string(),
        about: "a panel".into(),
        plugin: id.split('/').next().unwrap_or(id).to_string(),
        behaviour: crate::cmd::Behaviour::Passive,
        languages: Vec::new(),
        opens_panel: true,
        dock,
    }))
}

#[test]
fn plugin_settings_open_beside_what_they_are_overriding() {
    // The question anybody has while writing this is *what could I say
    // here*, and the answer is the manifest — which is why it opens
    // beside, and why the manifest half is read-only: it is the file an
    // update throws away.
    let (mut app, _rx) = editor();
    if crate::plugin::settings_dir().is_none() {
        return;
    }
    // A plugin that is certainly there, and a settings file that is
    // certainly not, so this makes one and does not tread on a real one.
    let id = "rust";
    let path = crate::plugin::settings_path(id).expect("a path");
    let existed = path.exists();
    if existed {
        return;
    }
    app.edit_plugin_settings(id);

    assert_eq!(app.ordinary_panes(), 2, "{}", app.status.text);
    assert!(app.side_by_side, "stacked is two half-height windows");
    let left = app.doc(app.panes[0].doc).expect("the shipped half");
    assert!(left.read_only, "the manifest is not yours to edit");
    assert!(left.name.contains("shipped"), "{}", left.name);
    assert!(
        left.rope.to_string().contains("\"id\": \"rust\""),
        "the left pane is not the manifest"
    );
    let right = app.doc(app.panes[1].doc).expect("yours");
    assert_eq!(right.path.as_deref(), Some(path.as_path()));
    assert!(!right.read_only, "yours is the half you write in");
    // Made with the shape of the thing in it, rather than as a blank page.
    assert!(right.rope.to_string().contains("_about"), "no stub was written");

    std::fs::remove_file(&path).ok();
}

/// Plugin settings open on a plugin that certainly exists, with a real
/// file made and then taken away again. `None` where there is nowhere to
/// keep settings, or where a real one is already there and must not be
/// disturbed.
fn settings_open(app: &mut App, id: &str) -> Option<PathBuf> {
    let path = crate::plugin::settings_path(id)?;
    if path.exists() {
        return None;
    }
    app.edit_plugin_settings(id);
    Some(path)
}

#[test]
fn the_manifest_half_of_plugin_settings_shows_the_manifest_and_nothing_else() {
    // Opening a file into it would leave you comparing your settings
    // against something that is not what they are settings for.
    let (mut app, _rx) = editor();
    let Some(path) = settings_open(&mut app, "rust") else {
        return;
    };
    let shipped = app.panes[0].doc;
    assert!(app.panes[0].pinned, "the manifest half is not pinned");
    assert!(app.doc(shipped).expect("it").read_only, "and it is read-only");

    // Standing in it and opening something puts that something in the
    // other pane, and leaves the manifest where it was.
    app.focus = 0;
    app.run(Cmd::NEW);
    assert_eq!(app.panes[0].doc, shipped, "the manifest was replaced");
    assert_eq!(app.focus, 1, "the focus should have moved out of it");

    std::fs::remove_file(&path).ok();
}

#[test]
fn closing_either_half_of_plugin_settings_closes_both() {
    // One thing to look at is one thing to close. Being left with the
    // manifest is being left with half a comparison and a buffer there is
    // nothing to do with.
    let (mut app, _rx) = editor();
    let Some(path) = settings_open(&mut app, "rust") else {
        return;
    };
    let shipped = app.panes[0].doc;
    assert_eq!(app.ordinary_panes(), 2);

    // From the side you were most likely editing.
    app.focus = 1;
    app.run(Cmd::CLOSE_PANE);
    assert_eq!(app.ordinary_panes(), 1, "the other pane stayed");
    assert!(
        app.docs.iter().all(|d| d.id != shipped),
        "the manifest is still open with nothing to compare it to"
    );
    assert!(app.panes.iter().all(|p| !p.pinned), "a pane is still pinned");

    // And from the other side, which should behave the same.
    std::fs::remove_file(&path).ok();
    let Some(path) = settings_open(&mut app, "rust") else {
        return;
    };
    let shipped = app.panes[0].doc;
    app.focus = 0;
    app.run(Cmd::CLOSE_PANE);
    assert_eq!(app.ordinary_panes(), 1);
    assert!(app.docs.iter().all(|d| d.id != shipped));

    std::fs::remove_file(&path).ok();
}

#[test]
fn a_docked_panel_opens_beside_the_code_rather_than_over_it() {
    // The whole point of a dock: you asked for a tree of files, not for
    // the file you were reading to go away.
    let (mut app, _rx) = editor();
    let was = app.view().doc;
    app.open_panel(docked_panel("files/tree", Some("left"), Some(30)));

    assert_eq!(app.panes.len(), 2);
    // On the left, and it has the focus, because you just asked for it.
    assert_eq!(app.focus, 0);
    assert_eq!(
        app.panes[0].dock.map(|d| (d.edge, d.size)),
        Some((crate::view::Edge::Left, 30))
    );
    // And the code is still there, still showing what it was showing.
    assert!(app.panes[1].dock.is_none());
    assert_eq!(app.panes[1].doc, was);

    // Its buffer belongs to the plugin and nothing types into it.
    let panel = app.doc(app.panes[0].doc).expect("a buffer");
    assert!(panel.read_only);
    assert_eq!(panel.panel.as_ref().map(|p| p.id.as_str()), Some("files/tree"));
}

#[test]
fn opening_a_file_from_a_sidebar_puts_it_beside_the_sidebar() {
    // A file explorer that replaced itself with the file you clicked would
    // have thrown away the tree to show you one leaf of it.
    let (mut app, _rx) = editor();
    let code = app.view().doc;
    app.open_panel(docked_panel("files/tree", Some("left"), None));
    assert_eq!(app.focus, 0, "standing in the sidebar");
    let sidebar = app.panes[0].doc;

    // Whatever the plugin asked to open goes in the middle.
    app.run(Cmd::NEW);
    assert!(app.panes[0].dock.is_some(), "the sidebar is still a sidebar");
    assert_eq!(
        app.panes[0].doc, sidebar,
        "the sidebar was made to show something else"
    );
    assert_eq!(app.focus, 1, "and the focus moved out of it");
    assert_ne!(app.panes[1].doc, code, "the new buffer went in the middle");

    // The one thing a dock does show is the panel it was opened for, so
    // refreshing it must not be pushed out into the middle.
    app.focus = 0;
    app.show(sidebar);
    assert_eq!(app.focus, 0);
    assert_eq!(app.panes[0].doc, sidebar);
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
fn running_a_docked_panels_command_again_puts_it_away() {
    // That is what collapsible means from the keyboard. A sidebar you can
    // only open is a sidebar everybody closes by quitting.
    let (mut app, _rx) = editor();
    let panel = docked_panel("files/tree", Some("left"), None);
    app.open_panel(panel);
    assert_eq!(app.panes.len(), 2);
    app.open_panel(panel);
    assert_eq!(app.panes.len(), 1, "it should have gone away");
    assert!(app.panes[0].dock.is_none());
    // And opening it again gets the same buffer rather than a second one.
    app.open_panel(panel);
    assert_eq!(app.panes.len(), 2);
    assert_eq!(
        app.docs.iter().filter(|d| d.panel.is_some()).count(),
        1,
        "a second buffer was made for the same panel"
    );
}

#[test]
fn a_panel_with_no_edge_is_still_a_tab() {
    // Which is what a panel used to always be, and is still right for
    // something you read and then leave.
    let (mut app, _rx) = editor();
    app.open_panel(docked_panel("cargo/report", None, None));
    assert_eq!(app.panes.len(), 1, "a tab is not a pane");
    assert!(app.here().panel.is_some(), "and it is what the pane shows");
}

#[test]
fn the_last_pane_showing_a_file_cannot_be_closed_but_a_dock_always_can() {
    let (mut app, _rx) = editor();
    app.open_panel(docked_panel("files/tree", Some("left"), None));
    // Standing in the dock: closing it is fine, even though it is one of
    // only two panes.
    assert_eq!(app.focus, 0);
    app.run(Cmd::CLOSE_PANE);
    assert_eq!(app.panes.len(), 1);

    // Standing in the only pane showing a file, with a dock open: still
    // refused, because what has to survive is somewhere to read code.
    app.open_panel(docked_panel("files/tree", Some("left"), None));
    app.focus = 1;
    app.run(Cmd::CLOSE_PANE);
    assert_eq!(app.panes.len(), 2);
    assert_eq!(app.status.text, "that is the only pane");
}

#[test]
fn splitting_a_sidebar_does_not_give_you_two_sidebars() {
    let (mut app, _rx) = editor();
    app.open_panel(docked_panel("files/tree", Some("left"), None));
    assert_eq!(app.focus, 0);
    app.run(Cmd::SPLIT);
    assert_eq!(app.panes.len(), 3);
    assert_eq!(
        app.panes.iter().filter(|p| p.dock.is_some()).count(),
        1,
        "the copy was docked too"
    );
}

#[test]
fn comparing_two_panes_ignores_the_sidebar() {
    // Comparing the code against a tree of file names is not a thing
    // anybody means by "compare the two panes".
    let (mut app, _rx) = editor();
    app.open_panel(docked_panel("files/tree", Some("left"), None));
    app.focus = 1;
    // One dock and one file pane is not two panes to compare.
    app.run(Cmd::DIFF_PANES);
    assert!(app.diff.is_none(), "{}", app.status.text);
    assert!(app.status.text.contains("two panes"), "{}", app.status.text);

    // With a real second pane it compares those two and leaves the dock
    // out of it.
    app.run(Cmd::SPLIT);
    app.run(Cmd::DIFF_PANES);
    let (left, right) = app.diff.as_ref().expect("compared").panes();
    assert!(app.panes[left].dock.is_none());
    assert!(app.panes[right].dock.is_none());
}

#[test]
fn a_plugin_that_is_one_server_gets_one_row_in_the_list() {
    // It used to be a row for the plugin and an indented copy of itself
    // underneath with the same switch on it, which is one switch shown
    // twice and a list twice as long as it needs to be.
    //
    // Read against manifests rather than against the registry, because a
    // language server is fetched from a package repository now and a test
    // cannot assume one has been.
    let read = |manifest: &str, id: &str| {
        let file: crate::plugin::FilePlugin = serde_json::from_str(manifest).expect("read");
        file.into_plugin(id, crate::plugin::Source::BuiltIn).0
    };

    let pyright = read(
        r#"{"id":"pyright","name":"Pyright","languages":{"python":{"servers":[
             {"name":"pyright","command":"pyright-langserver"}]}}}"#,
        "pyright",
    );
    assert!(
        server_rows(&pyright, |_| true).is_empty(),
        "a plugin that is one server got a second row of itself"
    );

    // And one that has several things in it still shows them, indented,
    // and says which language each is for.
    let vscode = read(
        r#"{"id":"vscode-langservers","name":"VS Code's servers","languages":{
             "css":{"servers":[{"name":"css-language-server","command":"vscode-css-language-server"}]},
             "html":{"servers":[{"name":"html-language-server","command":"vscode-html-language-server"}]},
             "json":{"servers":[{"name":"json-language-server","command":"vscode-json-language-server"}]}}}"#,
        "vscode-langservers",
    );
    let rows = server_rows(&vscode, |_| true);
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "  css-language-server",
            "  html-language-server",
            "  json-language-server"
        ]
    );
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("vscode-langservers/css-language-server — runs vscode-css-language-server for css")
    );

    // A server switched off says so, rather than sitting there looking on
    // and quietly doing nothing.
    assert_eq!(server_rows(&vscode, |_| false)[0].tag.as_deref(), Some("off"));
}

#[test]
fn installing_something_nobody_has_heard_of_says_so() {
    // A command that quietly does nothing is the failure worth testing
    // for here: an install has no result you can see until it finishes,
    // so one that never started has to say it never started.
    let (mut app, _rx) = editor();
    app.start_install("a-plugin-nobody-wrote");
    assert_eq!(app.status.tone, Tone::Bad);
    assert!(
        app.status.text.contains("a-plugin-nobody-wrote"),
        "{}",
        app.status.text
    );
    assert!(app.installing.is_none(), "and nothing is left running");
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
fn one_install_at_a_time() {
    // Two `npm install`s at once is two of them fighting over the same
    // directory, and the second one is nearly always Enter pressed twice.
    let (mut app, _rx) = editor();
    app.installing = Some(Installing {
        id: "busy".into(),
        removing: false,
        log: String::new(),
    });
    app.start_plan(Ok(crate::pack::Plan {
        id: "other".into(),
        name: "Other".into(),
        removing: false,
        files: crate::pack::Files::Leave,
        steps: vec![crate::plugin::Step {
            about: "something".into(),
            run: vec!["true".into()],
            unless: None,
            when: None,
            os: Vec::new(),
            arch: Vec::new(),
            system: false,
        }],
        steps_from: None,
        needs: Vec::new(),
        see: None,
    }));
    assert!(app.status.text.contains("busy"), "{}", app.status.text);
    assert_eq!(
        app.installing.as_ref().map(|i| i.id.clone()),
        Some("busy".to_string()),
        "the one that was already going is the one still going"
    );
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
fn semantic_tokens_are_read_relative_to_the_one_before_them() {
    // The answer is a flat list of fives, each one counted from the token
    // before it, and one number read wrongly puts every colour after it in
    // the wrong place. This is that arithmetic, on an answer with a line
    // break in the middle of it.
    let (mut app, _rx) = editor();
    typed(&mut app, "let x = 1;\nlet yy = 2;\n");
    let id = app.view().doc;
    let version = app.here().version;
    let legend = vec!["keyword".to_string(), "variable".to_string()];
    // `let` on line 0 at column 0, three long, a keyword; then `x`, one
    // along from the end of it... counted from the *start* of the token
    // before, which is what the protocol says.
    let tokens = json!({ "data": [0, 0, 3, 0, 0, 0, 4, 1, 1, 0, 1, 0, 3, 0, 0, 0, 4, 2, 1, 0] });
    app.take_semantic_tokens(id, version, &legend, tokens);

    let spans = &app.here().semantic;
    assert_eq!(spans.len(), 4);
    let text = |range: Range| app.here().slice(range);
    assert_eq!(text(spans[0].0), "let");
    assert_eq!(text(spans[1].0), "x");
    assert_eq!(text(spans[2].0), "let", "a new line starts the column over");
    assert_eq!(text(spans[3].0), "yy");
    assert_eq!(spans[3].1, crate::theme::Role::Variable);
}

#[test]
fn a_note_drawn_into_a_line_is_counted_in_the_width_of_it() {
    // The bug this is here to prevent: an inlay hint is text on the screen
    // that is not in the file, so everything after it on that line is
    // drawn further right than the file says it is. Click on it, and a
    // cursor lands wherever the editor thought it was.
    let (mut app, _rx) = editor();
    typed(&mut app, "let x = compute();\n");
    let id = app.view().doc;
    // `: usize` after the `x`, the way rust-analyzer writes one.
    if let Some(doc) = app.doc_mut(id) {
        doc.inlays = vec![crate::doc::Inlay {
            at: 5,
            text: ": usize".into(),
        }];
    }
    let doc = app.doc(id).expect("open");
    let hints = doc.inlay_columns();
    let layout = crate::view::Layout {
        rope: &doc.rope,
        hints: &hints,
        width: 80,
        tab_width: 4,
        wrap: false,
        folds: &[],
    };
    // The `x` is at character 4 and still in column 4: a note goes in
    // *before* the character it belongs to, and this one belongs to the
    // space after the x.
    assert_eq!(layout.place(4).1, 4);
    // Everything after the note is seven columns further along than the
    // file alone would say.
    assert_eq!(layout.place(5).1, 5 + 7);
    assert_eq!(layout.place(6).1, 6 + 7);
    // And the way back agrees: clicking in the middle of the note means
    // the character it is attached to, not one seven places away.
    assert_eq!(layout.position(0, 0, 5 + 3), 5);
    assert_eq!(layout.position(0, 0, 6 + 7), 6);
}

#[test]
fn the_same_word_elsewhere_is_only_lit_when_there_is_an_elsewhere() {
    let (mut app, _rx) = editor();
    typed(&mut app, "alpha beta alpha\n");
    let id = app.view().doc;
    let version = app.here().version;
    app.go_to_line(0);
    let at = app.view().cursor();
    // Two mentions: both light up.
    app.take_highlights(
        id,
        at,
        version,
        json!([
            { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } } },
            { "range": { "start": { "line": 0, "character": 11 }, "end": { "line": 0, "character": 16 } } },
        ]),
    );
    assert_eq!(app.here().highlights.len(), 2);

    // One mention is the word the cursor is on, which is not worth
    // lighting anything up for.
    app.take_highlights(
        id,
        at,
        version,
        json!([
            { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } } },
        ]),
    );
    assert!(app.here().highlights.is_empty());
}

#[test]
fn an_answer_about_a_file_that_has_changed_since_is_dropped() {
    let (mut app, _rx) = editor();
    typed(&mut app, "let x = 1;\n");
    let id = app.view().doc;
    let stale = app.here().version - 1;
    app.take_inlay_hints(
        id,
        stale,
        json!([{ "position": { "line": 0, "character": 5 }, "label": ": usize" }]),
    );
    assert!(
        app.here().inlays.is_empty(),
        "positions worked out against a file that has been typed in since \
         are positions in the wrong places"
    );
}

#[test]
fn inlay_labels_come_as_a_string_or_as_pieces() {
    let (mut app, _rx) = editor();
    typed(&mut app, "f(1)\n");
    let id = app.view().doc;
    let version = app.here().version;
    app.take_inlay_hints(
        id,
        version,
        json!([
            { "position": { "line": 0, "character": 2 }, "label": "count:" },
            { "position": { "line": 0, "character": 3 },
              "label": [{ "value": ": " }, { "value": "i32" }] },
        ]),
    );
    let hints = &app.here().inlays;
    assert_eq!(hints.len(), 2);
    assert_eq!(hints[0].text, "count:");
    assert_eq!(hints[1].text, ": i32", "the pieces are one label");
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
        hints: &[],
        width: 80,
        tab_width: 4,
        wrap: false,
        folds: &folds,
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
    assert_eq!(app.macro_steps.len(), 1, "only the typing was kept");
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
