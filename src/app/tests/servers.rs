//! What a language server says, and what the editor makes of it.

use super::*;

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

    let spans = &app.here().said.semantic;
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
        doc.said.inlays = vec![crate::doc::Inlay {
            at: 5,
            text: ": usize".into(),
        }];
    }
    let doc = app.doc(id).expect("open");
    let layout = crate::view::Layout {
        wrap: false,
        ..crate::view::Layout::of(&app.panes[0], doc, 4)
    }
    .across(80);
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
    assert_eq!(app.here().said.highlights.len(), 2);

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
    assert!(app.here().said.highlights.is_empty());
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
        app.here().said.inlays.is_empty(),
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
    let hints = &app.here().said.inlays;
    assert_eq!(hints.len(), 2);
    assert_eq!(hints[0].text, "count:");
    assert_eq!(hints[1].text, ": i32", "the pieces are one label");
}
