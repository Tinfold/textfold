//! The editor's own tests.
//!
//! Beside the module they test rather than at the bottom of it, because there
//! are a great many of them and they are the last thing anybody scrolling
//! through `app` is looking for — and split the way `app` is split, so that a
//! failure says which part of the editor it is about before you have read the
//! name of it. Children all the same, so they reach the same private things
//! they always did.
//!
//! What is here is what they all need: an editor with nothing of yours in it,
//! a place to put a file, and the handful of ways to press a key.

use super::*;
use serde_json::json;
use std::sync::mpsc;

mod buffers;
mod debug;
mod editing;
mod find;
mod mouse;
mod plugins;
mod servers;

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

fn typed_into_prompt(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
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

/// The parts of a rendered line a pointer would offer to follow.
fn followable(line: &DocLine) -> Vec<String> {
    line.links
        .iter()
        .map(|range| line.text.chars().skip(range.start).take(range.len()).collect())
        .collect()
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

/// A formatter a plugin brought, as a `&'static Tool` the step queue holds.
/// Leaked, the same way [`docked_panel`] is and for the same reason: that is
/// what the command table hands out.
fn a_formatter(id: &str) -> &'static crate::plugin::Tool {
    Box::leak(Box::new(crate::plugin::Tool {
        id: id.to_string(),
        name: id.split('/').next_back().unwrap_or(id).to_string(),
        about: "lays the file out".into(),
        command: id.split('/').next_back().unwrap_or(id).to_string(),
        args: Vec::new(),
        languages: Vec::new(),
        roots: Vec::new(),
        stdin: true,
        output: crate::plugin::Output::Replace,
        pattern: None,
        on_save: true,
        builds: false,
    }))
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
