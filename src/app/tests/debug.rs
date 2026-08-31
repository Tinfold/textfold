//! The debugger, the panel, and the breakpoints it stops on.

use super::*;

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
