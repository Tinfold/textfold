//! Talking to language servers.
//!
//! One process per server per project root, shared by every file that belongs
//! to it — opening forty Rust files starts one rust-analyzer, not forty.
//!
//! Nothing here blocks. Each server gets a thread that does nothing but read
//! its output, frame it, and post it to the same channel the keyboard posts
//! to; the editor picks messages off that channel between keystrokes. A server
//! that is slow, wedged, or busy indexing half a million lines cannot make the
//! cursor stutter, because the cursor is not waiting on it.
//!
//! Requests are asked and answered later. What to do with an answer is written
//! down as an [`Ask`] when the question goes out, so the reply carries its own
//! meaning back — there is no state machine to fall out of step, and a reply
//! that arrives after you have moved on is discarded by whoever asked rather
//! than by guessing here.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use crate::doc::{AppliedEdit, Diagnostic, DocId, Document, Severity};
use crate::lang;
use crate::text::Range;

/// Which server, as everything outside holds onto one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ServerId(pub usize);

/// What came back from a server, on its way to the editor's event loop.
#[derive(Debug)]
pub enum Incoming {
    /// An answer to something we asked.
    Response {
        id: i64,
        result: Result<Value, String>,
    },
    /// Something the server volunteered: diagnostics, progress, a log line.
    Notification { method: String, params: Value },
    /// Something the server wants from us. Every one of these must be
    /// answered, including the ones we do not understand, or a server that
    /// waits for the reply will sit there forever.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// It stopped. The words are for the status line.
    Exited(String),
}

/// What we asked for, so the answer knows what it is an answer to.
#[derive(Clone, Debug)]
pub enum Ask {
    Initialize,
    Completion {
        doc: DocId,
        at: usize,
        version: i32,
    },
    Hover {
        doc: DocId,
        at: usize,
    },
    Goto {
        doc: DocId,
        what: Goto,
    },
    References,
    Symbols {
        doc: DocId,
    },
    WorkspaceSymbols,
    Rename {
        /// The new name, to say so afterwards.
        to: String,
    },
    Format {
        doc: DocId,
        version: i32,
    },
    CodeActions,
    Signature {
        doc: DocId,
        at: usize,
    },
    /// A code action we asked the server to work out the details of before
    /// applying it.
    ResolveAction,
    /// A command we asked the server to run. The answer is usually nothing;
    /// what matters arrives separately as `workspace/applyEdit`.
    Command,
}

/// Which kind of "where is this" was asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Goto {
    Definition,
    Type,
    Implementation,
}

impl Goto {
    fn method(&self) -> &'static str {
        match self {
            Goto::Definition => "textDocument/definition",
            Goto::Type => "textDocument/typeDefinition",
            Goto::Implementation => "textDocument/implementation",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Goto::Definition => "definition",
            Goto::Type => "type definition",
            Goto::Implementation => "implementation",
        }
    }
}

/// How far along a server is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
    /// Asked to initialize, waiting for the answer. Files opened in the
    /// meantime are remembered and sent the moment it is ready.
    Starting,
    Ready,
    /// It died, or would not start. The words say which.
    Dead(String),
}

/// One running server.
pub struct Server {
    pub id: ServerId,
    /// What was run, for the status line: `rust-analyzer`, not the path.
    pub name: String,
    pub root: PathBuf,
    pub state: State,
    /// What the server said it can do. Asked before offering a feature, so
    /// that a server without rename does not get a rename request and an
    /// error message nobody can act on.
    capabilities: Value,
    settings: Option<Value>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    /// Files this server has been told about.
    open: HashSet<PathBuf>,
    /// Files opened before it was ready, waiting to be sent.
    queued: Vec<DocId>,
    /// What it is busy with, by progress token. rust-analyzer's indexing shows
    /// up here, which is the difference between "no completions" and "no
    /// completions yet".
    pub progress: BTreeMap<String, String>,
    /// The last thing it said about itself, worth a line in the status bar.
    pub message: Option<String>,
    pending: HashMap<i64, Ask>,
    next_id: i64,
}

impl Server {
    pub fn is_ready(&self) -> bool {
        self.state == State::Ready
    }

    /// What it is doing, if anything, in a few words.
    pub fn busy_with(&self) -> Option<&str> {
        self.progress.values().next().map(String::as_str)
    }

    /// Whether the server said it can do this. The path through the
    /// capabilities is dotted: `textDocument/rename` lives at `renameProvider`.
    fn can(&self, capability: &str) -> bool {
        // A capability is present, `null`, `false`, or an object saying how it
        // works. Only the last of those is a yes.
        !matches!(
            self.capabilities.get(capability),
            None | Some(Value::Null) | Some(Value::Bool(false))
        )
    }

    /// The characters that should make completions appear on their own.
    pub fn completion_triggers(&self) -> Vec<char> {
        self.capabilities
            .get("completionProvider")
            .and_then(|c| c.get("triggerCharacters"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|s| s.chars().next())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The characters that should make a signature hint appear.
    pub fn signature_triggers(&self) -> Vec<char> {
        self.capabilities
            .get("signatureHelpProvider")
            .and_then(|c| c.get("triggerCharacters"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|s| s.chars().next())
                    .collect()
            })
            .unwrap_or_default()
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
            self.state = State::Dead("stopped listening".into());
            self.stdin = None;
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Ask something, and write down what the answer will mean.
    fn request(&mut self, method: &str, params: Value, ask: Ask) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        self.pending.insert(id, ask);
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        id
    }

    fn answer(&mut self, id: Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    /// Take back what an answer was for. Called once per reply, so a
    /// duplicate answer from a confused server is ignored rather than acted
    /// on twice.
    pub fn claim(&mut self, id: i64) -> Option<Ask> {
        self.pending.remove(&id)
    }

    /// Stop it, politely and then not.
    fn shutdown(&mut self) {
        self.send(&json!({"jsonrpc": "2.0", "id": 0, "method": "shutdown"}));
        self.notify("exit", json!(null));
        self.stdin = None;
        if let Some(child) = &mut self.child {
            // A server that will not go is a server that gets killed. Waiting
            // on a wedged process is how editors hang on quit.
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

/// Every server there is, and the machinery to start more.
pub struct Servers {
    servers: Vec<Server>,
    tx: Sender<crate::app::Event>,
    /// Servers we tried and could not start, so we do not try again on every
    /// keystroke. Cleared by restarting them by hand.
    failed: HashSet<(String, PathBuf)>,
    /// Things worth telling somebody: a server that is not installed, or one
    /// that died. Drained by the editor, which has the status line.
    pub problems: Vec<String>,
}

impl Servers {
    pub fn new(tx: Sender<crate::app::Event>) -> Self {
        Self {
            servers: Vec::new(),
            tx,
            failed: HashSet::new(),
            problems: Vec::new(),
        }
    }

    pub fn all(&self) -> &[Server] {
        &self.servers
    }

    pub fn get(&self, id: ServerId) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: ServerId) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.id == id)
    }

    /// The servers that should be handling a document.
    pub fn for_doc(&self, doc: &Document) -> Vec<ServerId> {
        let Some(path) = &doc.path else {
            return Vec::new();
        };
        self.servers
            .iter()
            .filter(|s| s.is_ready() && s.open.contains(path))
            .map(|s| s.id)
            .collect()
    }

    /// The one server to ask a question of. Where a language has several — a
    /// type checker and a linter — the first is the one that answers
    /// questions; the others are there for their diagnostics.
    pub fn primary_for(&self, doc: &Document) -> Option<ServerId> {
        self.for_doc(doc).first().copied()
    }

    /// Start whatever this document needs, and tell them about it.
    ///
    /// Safe to call again for a document already open: it is what happens
    /// every time you switch to a tab, and doing nothing is the common case.
    pub fn open(&mut self, doc: &Document) {
        let Some(path) = doc.path.clone() else {
            // A buffer with no file has no project and no server. There is
            // nothing to be sorry about.
            return;
        };
        for config in &lang::get(doc.language).servers {
            let root = lang::project_root(&path, &config.roots);
            let key = (config.command.clone(), root.clone());
            if self.failed.contains(&key) {
                continue;
            }
            let existing = self
                .servers
                .iter()
                .position(|s| s.name == config.command && s.root == root)
                .filter(|&at| !matches!(self.servers[at].state, State::Dead(_)));

            let at = match existing {
                Some(at) => at,
                None => match self.start(config, &root) {
                    Ok(server) => {
                        self.servers.push(server);
                        self.servers.len() - 1
                    }
                    Err(why) => {
                        // Once. Trying again on every keystroke would put the
                        // same complaint on the screen forever.
                        self.failed.insert(key);
                        self.problems.push(why);
                        continue;
                    }
                },
            };
            if self.servers[at].open.contains(&path) {
                continue;
            }
            if self.servers[at].is_ready() {
                self.did_open(at, doc);
            } else {
                self.servers[at].queued.push(doc.id);
            }
        }
    }

    /// Run a server and set a thread to listen to it.
    ///
    /// The id is the position it will take in the list, which is only valid
    /// because a failure returns words rather than a half-made server — an id
    /// handed out for a server that never joined the list would name whichever
    /// server joined it next.
    fn start(&mut self, config: &lang::Server, root: &Path) -> Result<Server, String> {
        let id = ServerId(self.servers.len());
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(root)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|e| {
            // The common case by far is that it is not installed, and saying
            // so is more use than an errno.
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{} is not installed — code intelligence is off for now", config.command)
            } else {
                format!("{}: {e}", config.command)
            }
        })?;

        let taken = child.stdin.take().zip(child.stdout.take());
        let Some((stdin, stdout)) = taken else {
            child.kill().ok();
            return Err(format!("{} would not talk", config.command));
        };
        let stderr = child.stderr.take();

        let tx = self.tx.clone();
        if std::thread::Builder::new()
            .name(format!("lsp-{}", config.command))
            .spawn(move || read_messages(id, stdout, tx))
            .is_err()
        {
            child.kill().ok();
            return Err(format!("could not listen to {}", config.command));
        }

        // A server's complaints go somewhere a person can find them rather
        // than into the terminal underneath the editor, which would scribble
        // over the screen.
        if let Some(stderr) = stderr {
            let name = config.command.clone();
            std::thread::Builder::new()
                .name(format!("lsp-err-{name}"))
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        log(&name, &line);
                    }
                })
                .ok();
        }

        let mut server = Server {
            id,
            name: config.command.clone(),
            root: root.to_path_buf(),
            state: State::Starting,
            capabilities: Value::Null,
            settings: config.settings.clone(),
            child: Some(child),
            stdin: Some(stdin),
            open: HashSet::new(),
            queued: Vec::new(),
            progress: BTreeMap::new(),
            message: None,
            pending: HashMap::new(),
            next_id: 0,
        };

        let uri = uri_of(root);
        server.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": { "name": "textfold", "version": env!("CARGO_PKG_VERSION") },
                "rootUri": uri,
                "workspaceFolders": [{ "uri": uri, "name": root.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "workspace".into()) }],
                "initializationOptions": config.init_options.clone().unwrap_or(Value::Null),
                "capabilities": capabilities(),
            }),
            Ask::Initialize,
        );
        Ok(server)
    }

    /// The server answered `initialize`. Everything that was waiting on it
    /// goes now.
    pub fn ready(&mut self, id: ServerId, result: Value, docs: &[&Document]) {
        let Some(at) = self.servers.iter().position(|s| s.id == id) else {
            return;
        };
        self.servers[at].capabilities = result
            .get("capabilities")
            .cloned()
            .unwrap_or(Value::Null);
        self.servers[at].state = State::Ready;
        self.servers[at].notify("initialized", json!({}));

        if let Some(settings) = self.servers[at].settings.clone() {
            self.servers[at].notify(
                "workspace/didChangeConfiguration",
                json!({ "settings": settings }),
            );
        }

        let queued = std::mem::take(&mut self.servers[at].queued);
        for doc in docs {
            if queued.contains(&doc.id) {
                self.did_open(at, doc);
            }
        }
    }

    fn did_open(&mut self, at: usize, doc: &Document) {
        let Some(path) = &doc.path else { return };
        let server = &mut self.servers[at];
        server.open.insert(path.clone());
        let params = json!({
            "textDocument": {
                "uri": uri_of(path),
                "languageId": lang::get(doc.language).lsp_id,
                "version": doc.version,
                "text": doc.text(),
            }
        });
        server.notify("textDocument/didOpen", params);
    }

    /// Tell every server about an edit.
    ///
    /// The edits go over as they happened, in order, because that is the only
    /// order in which each one's positions describe the document the one
    /// before it left behind.
    pub fn did_change(&mut self, doc: &Document, edits: &[AppliedEdit]) {
        if edits.is_empty() {
            return;
        }
        let Some(path) = doc.path.clone() else { return };
        let changes: Vec<Value> = edits
            .iter()
            .map(|edit| {
                json!({
                    "range": {
                        "start": { "line": edit.lsp_start.0, "character": edit.lsp_start.1 },
                        "end": { "line": edit.lsp_old_end.0, "character": edit.lsp_old_end.1 },
                    },
                    "text": edit.text,
                })
            })
            .collect();
        let params = json!({
            "textDocument": { "uri": uri_of(&path), "version": doc.version },
            "contentChanges": changes,
        });
        for server in self.servers.iter_mut().filter(|s| s.open.contains(&path)) {
            server.notify("textDocument/didChange", params.clone());
        }
    }

    pub fn did_save(&mut self, doc: &Document) {
        let Some(path) = doc.path.clone() else { return };
        let params = json!({
            "textDocument": { "uri": uri_of(&path) },
            "text": doc.text(),
        });
        for server in self.servers.iter_mut().filter(|s| s.open.contains(&path)) {
            server.notify("textDocument/didSave", params.clone());
        }
    }

    pub fn did_close(&mut self, path: &Path) {
        let params = json!({ "textDocument": { "uri": uri_of(path) } });
        for server in self.servers.iter_mut() {
            if server.open.remove(path) {
                server.notify("textDocument/didClose", params.clone());
            }
        }
    }

    /// Ask about a position in a file. The workhorse: hover, goto, references
    /// and signature help all have the same shape.
    pub fn ask_at(
        &mut self,
        doc: &Document,
        at: usize,
        method: &str,
        capability: &str,
        ask: Ask,
        extra: Value,
    ) -> Option<ServerId> {
        let path = doc.path.clone()?;
        let id = self.primary_for(doc)?;
        let server = self.get_mut(id)?;
        if !server.can(capability) {
            return None;
        }
        let (line, character) = doc.lsp_point_at(at);
        let mut params = json!({
            "textDocument": { "uri": uri_of(&path) },
            "position": { "line": line, "character": character },
        });
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                params.insert(key.clone(), value.clone());
            }
        }
        server.request(method, params, ask);
        Some(id)
    }

    pub fn completion(&mut self, doc: &Document, at: usize, triggered: Option<char>) -> Option<ServerId> {
        let context = match triggered {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
            None => json!({ "triggerKind": 1 }),
        };
        self.ask_at(
            doc,
            at,
            "textDocument/completion",
            "completionProvider",
            Ask::Completion {
                doc: doc.id,
                at,
                version: doc.version,
            },
            json!({ "context": context }),
        )
    }

    pub fn hover(&mut self, doc: &Document, at: usize) -> Option<ServerId> {
        self.ask_at(
            doc,
            at,
            "textDocument/hover",
            "hoverProvider",
            Ask::Hover { doc: doc.id, at },
            json!({}),
        )
    }

    pub fn goto(&mut self, doc: &Document, at: usize, what: Goto) -> Option<ServerId> {
        let capability = match what {
            Goto::Definition => "definitionProvider",
            Goto::Type => "typeDefinitionProvider",
            Goto::Implementation => "implementationProvider",
        };
        self.ask_at(
            doc,
            at,
            what.method(),
            capability,
            Ask::Goto { doc: doc.id, what },
            json!({}),
        )
    }

    pub fn references(&mut self, doc: &Document, at: usize) -> Option<ServerId> {
        self.ask_at(
            doc,
            at,
            "textDocument/references",
            "referencesProvider",
            Ask::References,
            json!({ "context": { "includeDeclaration": false } }),
        )
    }

    pub fn signature(&mut self, doc: &Document, at: usize) -> Option<ServerId> {
        self.ask_at(
            doc,
            at,
            "textDocument/signatureHelp",
            "signatureHelpProvider",
            Ask::Signature { doc: doc.id, at },
            json!({}),
        )
    }

    pub fn rename(&mut self, doc: &Document, at: usize, to: &str) -> Option<ServerId> {
        self.ask_at(
            doc,
            at,
            "textDocument/rename",
            "renameProvider",
            Ask::Rename { to: to.to_string() },
            json!({ "newName": to }),
        )
    }

    /// What the server offers to do about the selection: fix an error, import
    /// a name, fill in a match.
    pub fn code_actions(&mut self, doc: &Document, range: Range) -> Option<ServerId> {
        let path = doc.path.clone()?;
        let id = self.primary_for(doc)?;
        let here: Vec<Value> = doc
            .diagnostics
            .iter()
            .filter(|d| d.range.overlaps(&range))
            .map(diagnostic_to_lsp)
            .collect();
        let (from_line, from_char) = doc.lsp_point_at(range.start());
        let (to_line, to_char) = doc.lsp_point_at(range.end());
        let server = self.get_mut(id)?;
        if !server.can("codeActionProvider") {
            return None;
        }
        server.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri_of(&path) },
                "range": {
                    "start": { "line": from_line, "character": from_char },
                    "end": { "line": to_line, "character": to_char },
                },
                "context": { "diagnostics": here },
            }),
            Ask::CodeActions,
        );
        Some(id)
    }

    pub fn format(&mut self, doc: &Document, tab_width: usize, spaces: bool) -> Option<ServerId> {
        let path = doc.path.clone()?;
        let id = self.primary_for(doc)?;
        let server = self.get_mut(id)?;
        if !server.can("documentFormattingProvider") {
            return None;
        }
        server.request(
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": uri_of(&path) },
                "options": {
                    "tabSize": tab_width,
                    "insertSpaces": spaces,
                    "trimTrailingWhitespace": true,
                    "insertFinalNewline": true,
                },
            }),
            Ask::Format {
                doc: doc.id,
                version: doc.version,
            },
        );
        Some(id)
    }

    pub fn symbols(&mut self, doc: &Document) -> Option<ServerId> {
        let path = doc.path.clone()?;
        let id = self.primary_for(doc)?;
        let server = self.get_mut(id)?;
        if !server.can("documentSymbolProvider") {
            return None;
        }
        server.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri_of(&path) } }),
            Ask::Symbols { doc: doc.id },
        );
        Some(id)
    }

    pub fn workspace_symbols(&mut self, doc: &Document, query: &str) -> Option<ServerId> {
        let id = self.primary_for(doc)?;
        let server = self.get_mut(id)?;
        if !server.can("workspaceSymbolProvider") {
            return None;
        }
        server.request(
            "workspace/symbol",
            json!({ "query": query }),
            Ask::WorkspaceSymbols,
        );
        Some(id)
    }

    /// Run a command the server offered, usually as part of a code action.
    pub fn execute(&mut self, id: ServerId, command: &Value) {
        let Some(server) = self.get_mut(id) else {
            return;
        };
        server.request(
            "workspace/executeCommand",
            json!({
                "command": command.get("command").cloned().unwrap_or(Value::Null),
                "arguments": command.get("arguments").cloned().unwrap_or(json!([])),
            }),
            Ask::Command,
        );
    }

    /// Ask the server to fill in a code action it sent in outline.
    pub fn resolve_action(&mut self, id: ServerId, action: &Value) -> bool {
        let Some(server) = self.get_mut(id) else {
            return false;
        };
        if server
            .capabilities
            .get("codeActionProvider")
            .and_then(|c| c.get("resolveProvider"))
            != Some(&Value::Bool(true))
        {
            return false;
        }
        server.request("codeAction/resolve", action.clone(), Ask::ResolveAction);
        true
    }

    /// Answer something the server asked us.
    ///
    /// Every request gets an answer, including the ones we have nothing useful
    /// to say about — a server left waiting on a reply stops working, and
    /// "null" is a complete answer to a question we do not understand.
    pub fn respond(&mut self, id: ServerId, request_id: Value, method: &str, params: &Value) {
        let Some(server) = self.get_mut(id) else {
            return;
        };
        let result = match method {
            // rust-analyzer asks for its own settings back, by section.
            "workspace/configuration" => {
                let settings = server.settings.clone().unwrap_or(Value::Null);
                let items = params.get("items").and_then(Value::as_array);
                let answers: Vec<Value> = items
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| {
                                let section = item.get("section").and_then(Value::as_str);
                                section
                                    .map(|s| dig(&settings, s))
                                    .unwrap_or_else(|| settings.clone())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!(answers)
            }
            // Dynamic registration: we do not track it, but saying yes is
            // both true enough and required.
            "client/registerCapability" | "client/unregisterCapability" => Value::Null,
            "window/workDoneProgress/create" => Value::Null,
            _ => Value::Null,
        };
        server.answer(request_id, result);
    }

    /// Take in `$/progress`, so the status line can say what a server is busy
    /// with rather than looking broken.
    pub fn progress(&mut self, id: ServerId, params: &Value) {
        let Some(server) = self.get_mut(id) else {
            return;
        };
        let token = match params.get("token") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return,
        };
        let value = params.get("value");
        let kind = value.and_then(|v| v.get("kind")).and_then(Value::as_str);
        match kind {
            Some("begin") | Some("report") => {
                let title = value
                    .and_then(|v| v.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or("working");
                let detail = value
                    .and_then(|v| v.get("message"))
                    .and_then(Value::as_str);
                let percent = value
                    .and_then(|v| v.get("percentage"))
                    .and_then(Value::as_u64);
                let mut said = match detail {
                    Some(detail) if !detail.is_empty() => format!("{title}: {detail}"),
                    _ => title.to_string(),
                };
                if let Some(percent) = percent {
                    said.push_str(&format!(" {percent}%"));
                }
                // A progress report that started as "begin" keeps its token,
                // so a later "report" replaces it instead of stacking up.
                server.progress.insert(token, said);
            }
            Some("end") => {
                server.progress.remove(&token);
            }
            _ => {}
        }
    }

    /// Take a set of diagnostics for a file.
    pub fn diagnostics_for(
        &self,
        id: ServerId,
        params: &Value,
        doc: &Document,
    ) -> Option<Vec<Diagnostic>> {
        let uri = params.get("uri").and_then(Value::as_str)?;
        let path = path_of(uri)?;
        if doc.path.as_deref() != Some(path.as_path()) {
            return None;
        }
        let list = params.get("diagnostics").and_then(Value::as_array)?;
        Some(
            list.iter()
                .filter_map(|d| diagnostic_from_lsp(d, doc, id))
                .collect(),
        )
    }

    /// Stop everything, on the way out.
    pub fn shutdown_all(&mut self) {
        for server in &mut self.servers {
            if server.stdin.is_some() {
                server.shutdown();
            }
        }
    }

    /// Start them all again. For after installing a server, or after one has
    /// wedged itself.
    pub fn restart(&mut self) {
        self.shutdown_all();
        self.servers.clear();
        self.failed.clear();
    }

    /// Mark a server as gone.
    ///
    /// It goes on the list of ones not to try again, because a server that
    /// falls over as it starts would otherwise be started again by the next
    /// keystroke, and again, and again. `restart-servers` clears that list,
    /// which is what to do once whatever was wrong is fixed.
    pub fn died(&mut self, id: ServerId, why: String) {
        if let Some(server) = self.get_mut(id) {
            server.state = State::Dead(why);
            server.stdin = None;
            server.open.clear();
            server.progress.clear();
            let key = (server.name.clone(), server.root.clone());
            self.failed.insert(key);
        }
    }
}

/// Read framed messages off a server's output until it stops.
///
/// The only thing this thread does. It never touches editor state and never
/// waits for it, which is what keeps a wedged server from being a wedged
/// editor.
fn read_messages(id: ServerId, stdout: std::process::ChildStdout, tx: Sender<crate::app::Event>) {
    let mut reader = BufReader::new(stdout);
    loop {
        // Headers, until a blank line. `Content-Length` is the only one that
        // matters; the rest are read and dropped.
        let mut length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    tx.send(crate::app::Event::Lsp(id, Incoming::Exited("stopped".into())))
                        .ok();
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    tx.send(crate::app::Event::Lsp(
                        id,
                        Incoming::Exited(format!("stopped: {e}")),
                    ))
                    .ok();
                    return;
                }
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
            tx.send(crate::app::Event::Lsp(
                id,
                Incoming::Exited("sent something that was not a message".into()),
            ))
            .ok();
            return;
        };

        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            tx.send(crate::app::Event::Lsp(id, Incoming::Exited("stopped".into())))
                .ok();
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&body) else {
            // One unreadable message is not a reason to stop listening.
            continue;
        };

        let incoming = if let Some(method) = message.get("method").and_then(Value::as_str) {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            match message.get("id") {
                Some(request_id) => Incoming::Request {
                    id: request_id.clone(),
                    method: method.to_string(),
                    params,
                },
                None => Incoming::Notification {
                    method: method.to_string(),
                    params,
                },
            }
        } else if let Some(response_id) = message.get("id").and_then(Value::as_i64) {
            let result = match message.get("error") {
                Some(error) => Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("something went wrong")
                    .to_string()),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            Incoming::Response {
                id: response_id,
                result,
            }
        } else {
            continue;
        };

        if tx.send(crate::app::Event::Lsp(id, incoming)).is_err() {
            // The editor has gone. So should we.
            return;
        }
    }
}

/// What textfold tells a server it can do.
///
/// Claiming a capability we do not implement is worse than not claiming it: a
/// server takes us at our word and sends things nobody handles.
fn capabilities() -> Value {
    json!({
        "workspace": {
            "applyEdit": true,
            "configuration": true,
            "workspaceFolders": true,
            "didChangeConfiguration": { "dynamicRegistration": false },
            "symbol": { "dynamicRegistration": false },
            "executeCommand": { "dynamicRegistration": false },
        },
        "textDocument": {
            "synchronization": {
                "dynamicRegistration": false,
                "willSave": false,
                "didSave": true,
            },
            "completion": {
                "dynamicRegistration": false,
                "completionItem": {
                    "snippetSupport": false,
                    "documentationFormat": ["markdown", "plaintext"],
                    "insertReplaceSupport": true,
                    "resolveSupport": { "properties": ["documentation", "detail"] },
                },
                "contextSupport": true,
            },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "signatureHelp": {
                "signatureInformation": {
                    "documentationFormat": ["markdown", "plaintext"],
                    "parameterInformation": { "labelOffsetSupport": true },
                },
            },
            "definition": { "linkSupport": true },
            "typeDefinition": { "linkSupport": true },
            "implementation": { "linkSupport": true },
            "references": {},
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "formatting": {},
            "rename": { "prepareSupport": false },
            "publishDiagnostics": { "relatedInformation": true },
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": {
                        "valueSet": [
                            "", "quickfix", "refactor", "refactor.extract",
                            "refactor.inline", "refactor.rewrite", "source",
                            "source.organizeImports",
                        ],
                    },
                },
                "resolveSupport": { "properties": ["edit"] },
            },
        },
        "window": {
            "workDoneProgress": true,
            "showMessage": {},
        },
        "general": {
            "positionEncodings": ["utf-16"],
        },
    })
}

/// A path as a `file://` URI, which is the only way LSP names a file.
pub fn uri_of(path: &Path) -> String {
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            // Unreserved, plus the separators a path is made of.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// And back again. `None` for a URI naming something that is not a file, which
/// some servers do send.
pub fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).ok()?;
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// Reach into a settings object by dotted section name, which is how a server
/// asks for the part it cares about.
fn dig(value: &Value, section: &str) -> Value {
    let mut at = value;
    for part in section.split('.') {
        match at.get(part) {
            Some(next) => at = next,
            None => return Value::Null,
        }
    }
    at.clone()
}

fn diagnostic_from_lsp(value: &Value, doc: &Document, server: ServerId) -> Option<Diagnostic> {
    let range = value.get("range")?;
    let start = point_of(range.get("start")?)?;
    let end = point_of(range.get("end")?)?;
    let message = value.get("message").and_then(Value::as_str)?.to_string();
    Some(Diagnostic {
        range: Range::new(
            doc.char_at_lsp_point(start.0, start.1),
            doc.char_at_lsp_point(end.0, end.1),
        ),
        severity: value
            .get("severity")
            .and_then(Value::as_u64)
            .map(Severity::from_lsp)
            .unwrap_or(Severity::Warning),
        message,
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string),
        code: value.get("code").map(|c| match c {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        server: server.0,
    })
}

/// A diagnostic on its way back out, for a code action request that has to say
/// which problem it is about.
fn diagnostic_to_lsp(d: &Diagnostic) -> Value {
    json!({
        "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0} },
        "severity": match d.severity {
            Severity::Error => 1,
            Severity::Warning => 2,
            Severity::Info => 3,
            Severity::Hint => 4,
        },
        "message": d.message,
        "source": d.source,
        "code": d.code,
    })
}

pub fn point_of(value: &Value) -> Option<(usize, usize)> {
    Some((
        value.get("line")?.as_u64()? as usize,
        value.get("character")?.as_u64()? as usize,
    ))
}

/// Where a server's complaints go. A file, because the screen belongs to the
/// editor and the terminal underneath it belongs to whoever started us.
fn log(name: &str, line: &str) {
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
pub fn log_path() -> Option<PathBuf> {
    Some(
        dirs::state_dir()
            .or_else(dirs::cache_dir)?
            .join("textfold")
            .join("lsp.log"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_survives_the_round_trip_through_a_uri() {
        for path in [
            "/home/someone/project/src/main.rs",
            "/tmp/a file with spaces.rs",
            "/tmp/ünïcödé/x.rs",
            "/tmp/100% sure.txt",
        ] {
            let uri = uri_of(Path::new(path));
            assert!(uri.starts_with("file:///"), "{uri}");
            assert_eq!(path_of(&uri).as_deref(), Some(Path::new(path)), "{uri}");
        }
    }

    #[test]
    fn a_uri_that_is_not_a_file_is_not_a_path() {
        assert_eq!(path_of("untitled:Untitled-1"), None);
        assert_eq!(path_of("jdt://contents/rt.jar"), None);
    }

    #[test]
    fn settings_are_handed_back_by_the_section_asked_for() {
        let settings = json!({ "rust-analyzer": { "check": { "command": "clippy" } } });
        assert_eq!(
            dig(&settings, "rust-analyzer.check.command"),
            json!("clippy")
        );
        assert_eq!(dig(&settings, "rust-analyzer.nothing"), Value::Null);
        assert_eq!(dig(&settings, "elsewhere"), Value::Null);
    }
}
