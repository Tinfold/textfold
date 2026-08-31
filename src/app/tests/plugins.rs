//! Plugins: what they may ask the editor for, what they may put on the
//! screen, and what they are told no about.

use super::*;

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

#[test]
fn a_suggestion_and_a_list_have_a_key_each() {
    // The bug: the suggestion list took Tab, a plugin's inline offer had
    // nothing but Tab, and with both on the screen — which is the ordinary
    // case, since textfold asks for completions as you type — there was no
    // way at all to take the offer. Now Enter takes the row that is lit and
    // Tab takes the text sitting at the cursor.
    let (mut app, _rx) = editor();
    typed(&mut app, "HashMa");
    let at = app.view().cursor();
    suggested(&mut app, at, false, json!([{ "label": "HashMap" }]));
    suggesting(&mut app, "p::new()");
    assert!(app.completion.is_some(), "the list is up");
    assert!(app.hint_showing(), "and so is the offer");

    pressed(&mut app, "tab");
    assert_eq!(
        app.here().text(),
        "HashMap::new()",
        "Tab took what was offered in the text"
    );
}

#[test]
fn the_list_still_has_enter_while_something_is_offered() {
    let (mut app, _rx) = editor();
    typed(&mut app, "HashMa");
    let at = app.view().cursor();
    suggested(&mut app, at, false, json!([{ "label": "HashMap" }]));
    suggesting(&mut app, "p::new()");

    pressed(&mut app, "enter");
    assert_eq!(app.here().text(), "HashMap", "Enter took the row that was lit");
}

#[test]
fn what_a_plugin_offers_has_a_key_of_its_own() {
    // Tab is indent, and the list's, and this — so there is one key that is
    // only ever this, for when the other two are in the way.
    let (mut app, _rx) = editor();
    typed(&mut app, "x");
    suggesting(&mut app, " = 1");
    pressed(&mut app, "alt-a");
    assert_eq!(app.here().text(), "x = 1");
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
