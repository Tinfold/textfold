//! JSON-RPC 2.0 over a child process's standard input and output.
//!
//! The one piece of machinery shared by everything textfold talks to that is
//! not a person: language servers today, plugin hosts next door, and a debug
//! adapter when there is one. All three speak the same wire format — a
//! `Content-Length` header, a blank line, then that many bytes of JSON — and
//! all three want the same three properties from it.
//!
//! **Nothing blocks.** Each peer gets a thread that does nothing but read its
//! output, frame it, and post it to the channel the keyboard posts to. A peer
//! that is slow, wedged, or busy indexing half a million lines cannot make the
//! cursor stutter, because the cursor is not waiting on it.
//!
//! **A reply carries its own meaning.** What to do with an answer is written
//! down when the question goes out and handed back when it arrives, so there
//! is no state machine to fall out of step with. What that note *is* differs
//! per peer, which is what the type parameter on [`Peer`] is for.
//!
//! **Framing is written once.** Getting a byte count wrong desynchronises a
//! stream for good, and a bug in it would be a bug in every peer at once.
//!
//! What is deliberately *not* here is any vocabulary: no `initialize`, no
//! capabilities, no notion of what a method means. This module moves JSON
//! between a process and the event loop, and the modules above it decide what
//! the JSON says.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;

use serde_json::{Value, json};

/// What came back from a peer, on its way to the editor's event loop.
#[derive(Debug)]
pub enum Incoming {
    /// An answer to something we asked.
    Response {
        id: i64,
        result: Result<Value, String>,
    },
    /// Something the peer volunteered: diagnostics, progress, a log line.
    Notification { method: String, params: Value },
    /// Something the peer wants from us. Every one of these must be answered,
    /// including the ones we do not understand, or a peer that waits for the
    /// reply will sit there forever.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// It stopped. The words are for the status line.
    Exited(String),
}

/// Why a peer never got going.
///
/// Split rather than stringified because the two halves are worth saying
/// differently: a program that is not installed is a thing the person can go
/// and install, and each caller knows what to suggest they do about it.
#[derive(Debug)]
pub enum NotStarted {
    /// The program is not on the `PATH`. By far the commonest, and an errno
    /// is no use to anybody.
    Missing,
    /// It started, and then would not talk.
    Failed(String),
}

/// What to run, and what to call it.
pub struct Spawn<'a> {
    pub command: &'a str,
    pub args: &'a [String],
    /// The directory it runs in, which for every peer so far is the root of
    /// the project it is being asked about.
    pub root: &'a Path,
    pub env: &'a BTreeMap<String, String>,
    /// What its threads and its log lines are named after: `rust-analyzer`,
    /// `stm32`. Short, and recognisable in a log a person is reading.
    pub label: &'a str,
}

/// One child process we talk JSON-RPC to.
///
/// `A` is whatever the owner wants to remember about a question while it waits
/// for the answer — [`crate::lsp::Ask`] for a language server. This module
/// never looks inside one; it only hands it back.
pub struct Peer<A> {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    pending: HashMap<i64, A>,
    next_id: i64,
    /// Set when a write failed, and taken by the owner so it can record the
    /// death in whatever way it says such things. Kept here rather than
    /// reported directly because this module has no opinion about what a dead
    /// peer means to the editor.
    failed: Option<String>,
}

impl<A> Peer<A> {
    /// Start the program and the thread that listens to it.
    ///
    /// `wrap` is how an [`Incoming`] becomes an event the editor's loop
    /// understands — the one thing that differs between a language server and
    /// a plugin, once the framing is shared.
    pub fn start(
        spawn: Spawn<'_>,
        tx: Sender<crate::app::Event>,
        wrap: impl Fn(Incoming) -> crate::app::Event + Send + 'static,
    ) -> Result<Self, NotStarted> {
        let mut command = Command::new(spawn.command);
        command
            .args(spawn.args)
            .current_dir(spawn.root)
            .envs(spawn.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => NotStarted::Missing,
            _ => NotStarted::Failed(format!("{}: {e}", spawn.command)),
        })?;

        let taken = child.stdin.take().zip(child.stdout.take());
        let Some((stdin, stdout)) = taken else {
            child.kill().ok();
            return Err(NotStarted::Failed(format!(
                "{} would not talk",
                spawn.command
            )));
        };
        let stderr = child.stderr.take();

        if std::thread::Builder::new()
            .name(format!("rpc-{}", spawn.label))
            .spawn(move || read_messages(stdout, tx, wrap))
            .is_err()
        {
            child.kill().ok();
            return Err(NotStarted::Failed(format!(
                "could not listen to {}",
                spawn.command
            )));
        }

        // A peer's complaints go somewhere a person can find them rather than
        // into the terminal underneath the editor, which would scribble over
        // the screen.
        if let Some(stderr) = stderr {
            let label = spawn.label.to_string();
            std::thread::Builder::new()
                .name(format!("rpc-err-{label}"))
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        log(&label, &line);
                    }
                })
                .ok();
        }

        Ok(Peer {
            child: Some(child),
            stdin: Some(stdin),
            pending: HashMap::new(),
            next_id: 0,
            failed: None,
        })
    }

    fn send(&mut self, message: &Value) {
        let Some(stdin) = &mut self.stdin else {
            return;
        };
        let body = message.to_string();
        // The header is the whole framing: a byte count, a blank line, then
        // that many bytes. Getting it wrong desynchronises the stream for
        // good, which is why it is written in exactly one place.
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        if stdin.write_all(framed.as_bytes()).is_err() || stdin.flush().is_err() {
            self.failed = Some("stopped listening".into());
            self.stdin = None;
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Ask something, and write down what the answer will mean.
    pub fn request(&mut self, method: &str, params: Value, ask: A) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        self.pending.insert(id, ask);
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        id
    }

    /// Answer something the peer asked us.
    pub fn answer(&mut self, id: Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    /// Refuse something the peer asked us. Still an answer: a peer waiting on
    /// a reply it will never get is a peer that has hung, and a plugin author
    /// staring at a hung plugin has no way to see why.
    pub fn refuse(&mut self, id: Value, code: i64, message: &str) {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": code, "message": message }
        }));
    }

    /// Take back what an answer was for. Called once per reply, so a duplicate
    /// answer from a confused peer is ignored rather than acted on twice.
    pub fn claim(&mut self, id: i64) -> Option<A> {
        self.pending.remove(&id)
    }

    /// Give up on it without saying anything, for a peer that has already
    /// gone. The questions it never answered go with it: nothing will arrive
    /// for them now, and holding the notes would only leak.
    pub fn close(&mut self) {
        self.stdin = None;
        self.pending.clear();
    }

    /// Why the last write failed, if one did, taken so it is reported once.
    pub fn take_failure(&mut self) -> Option<String> {
        self.failed.take()
    }

    /// Whether there is still a pipe to write down.
    pub fn is_writable(&self) -> bool {
        self.stdin.is_some()
    }

    /// Stop it, politely and then not.
    ///
    /// `shutdown` then `exit` is what LSP asks for, and what the plugin
    /// protocol borrows, so one implementation serves both.
    pub fn stop(&mut self) {
        self.send(&json!({"jsonrpc": "2.0", "id": 0, "method": "shutdown"}));
        self.notify("exit", json!(null));
        self.stdin = None;
        if let Some(child) = &mut self.child {
            // A peer that will not go is a peer that gets killed. Waiting on a
            // wedged process is how editors hang on quit.
            std::thread::sleep(std::time::Duration::from_millis(50));
            match child.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    child.kill().ok();
                    child.wait().ok();
                }
            }
        }
    }
}

/// Read framed messages off a peer's output until it stops.
///
/// The only thing this thread does. It never touches editor state and never
/// waits for it, which is what keeps a wedged peer from being a wedged editor.
fn read_messages(
    stdout: std::process::ChildStdout,
    tx: Sender<crate::app::Event>,
    wrap: impl Fn(Incoming) -> crate::app::Event,
) {
    let mut reader = BufReader::new(stdout);
    let stopped = |why: &str, tx: &Sender<crate::app::Event>| {
        tx.send(wrap(Incoming::Exited(why.to_string()))).ok();
    };
    loop {
        // Headers, until a blank line. `Content-Length` is the only one that
        // matters; the rest are read and dropped.
        let mut length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return stopped("stopped", &tx),
                Ok(_) => {}
                Err(e) => return stopped(&format!("stopped: {e}"), &tx),
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line
                .strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
            {
                length = rest.trim().parse().ok();
            }
        }
        let Some(length) = length else {
            // No length is not a message we can find the end of, and guessing
            // would corrupt everything after it.
            return stopped("sent something that was not a message", &tx);
        };

        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return stopped("stopped", &tx);
        }
        let Ok(message) = serde_json::from_slice::<Value>(&body) else {
            // One unreadable message is not a reason to stop listening.
            continue;
        };

        let Some(incoming) = read_one(message) else {
            continue;
        };
        if tx.send(wrap(incoming)).is_err() {
            // The editor has gone. So should we.
            return;
        }
    }
}

/// One parsed message as whichever of the three kinds it is, or `None` for a
/// well-formed JSON value that is not a JSON-RPC message at all.
fn read_one(message: Value) -> Option<Incoming> {
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        return Some(match message.get("id") {
            Some(request_id) => Incoming::Request {
                id: request_id.clone(),
                method: method.to_string(),
                params,
            },
            None => Incoming::Notification {
                method: method.to_string(),
                params,
            },
        });
    }
    let id = message.get("id").and_then(Value::as_i64)?;
    let result = match message.get("error") {
        Some(error) => Err(error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("something went wrong")
            .to_string()),
        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
    };
    Some(Incoming::Response { id, result })
}

/// Where a peer's complaints go. A file, because the screen belongs to the
/// editor and the terminal underneath it belongs to whoever started us.
pub fn log(name: &str, line: &str) {
    use std::sync::Mutex;
    static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
    let Ok(mut file) = FILE.lock() else { return };
    if file.is_none() {
        let Some(path) = log_path() else { return };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        *file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
    }
    if let Some(file) = file.as_mut() {
        writeln!(file, "[{name}] {line}").ok();
    }
}

/// Where the log is, so that the status line can tell you.
///
/// One file for every peer rather than one each: what a person is doing when
/// they go looking is working out why something did not happen, and the
/// answer is often in the other program's half of the conversation.
pub fn log_path() -> Option<PathBuf> {
    Some(
        dirs::state_dir()
            .or_else(dirs::cache_dir)?
            .join("textfold")
            .join("textfold.log"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notification_is_a_method_with_no_id() {
        let message = json!({"jsonrpc": "2.0", "method": "hello", "params": {"a": 1}});
        match read_one(message) {
            Some(Incoming::Notification { method, params }) => {
                assert_eq!(method, "hello");
                assert_eq!(params["a"], 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_method_with_an_id_is_a_question_that_wants_answering() {
        let message = json!({"jsonrpc": "2.0", "id": 7, "method": "ask", "params": null});
        match read_one(message) {
            Some(Incoming::Request { id, method, .. }) => {
                assert_eq!(id, json!(7));
                assert_eq!(method, "ask");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_error_reply_comes_back_as_the_words_it_carried() {
        let message = json!({
            "jsonrpc": "2.0", "id": 3,
            "error": { "code": -32601, "message": "no such method" }
        });
        match read_one(message) {
            Some(Incoming::Response { id, result }) => {
                assert_eq!(id, 3);
                assert_eq!(result, Err("no such method".into()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_reply_with_no_result_is_still_a_reply() {
        // A peer that did what it was asked and has nothing to say about it
        // answers with `result: null`, or leaves the field out entirely.
        match read_one(json!({"jsonrpc": "2.0", "id": 3})) {
            Some(Incoming::Response { result, .. }) => assert_eq!(result, Ok(Value::Null)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn json_that_is_not_a_message_is_passed_over_rather_than_guessed_at() {
        assert!(read_one(json!({"hello": "there"})).is_none());
        assert!(read_one(json!([1, 2, 3])).is_none());
    }

    #[test]
    fn an_answer_is_claimed_once() {
        let mut peer: Peer<&str> = Peer {
            child: None,
            stdin: None,
            pending: HashMap::new(),
            next_id: 0,
            failed: None,
        };
        peer.pending.insert(1, "the thing we asked");
        assert_eq!(peer.claim(1), Some("the thing we asked"));
        // A confused peer answering twice is ignored the second time rather
        // than acted on twice.
        assert_eq!(peer.claim(1), None);
    }
}
