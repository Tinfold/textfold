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

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use crate::rpc::{self, Peer};

use crate::doc::{AppliedEdit, Diagnostic, DocId, Document, Severity, Told};
use crate::venv;
use crate::lang;
use crate::text::Range;

/// Which server, as everything outside holds onto one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ServerId(pub usize);

/// What came back from a server. The framing and the three kinds of message
/// are the same for everything textfold talks to, so they live in `rpc`; this
/// is here so that the rest of the editor can go on saying `lsp::Incoming`.
pub use crate::rpc::Incoming;

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
        /// A name to search the project for if the server has no definition
        /// at that position.
        fallback: Option<String>,
    },
    References,
    Symbols {
        doc: DocId,
    },
    WorkspaceSymbols {
        /// Set when one hit should be gone to rather than listed.
        going: Option<String>,
    },
    Rename {
        /// The new name, to say so afterwards.
        to: String,
    },
    Format {
        doc: DocId,
        version: i32,
    },
    CodeActions {
        doc: DocId,
        /// Where the question was about, so an answer that arrives after the
        /// cursor has moved on can be dropped.
        at: usize,
    },
    /// The fixes a server would make to a whole file on its own — a linter's
    /// autofixes, an import list put in order. Asked of every server at once
    /// before a save, and applied without anybody choosing from a list.
    SourceActions {
        doc: DocId,
        version: i32,
    },
    /// Fixes for whatever is wrong under the cursor, asked for by the editor
    /// rather than by a person, so that they can be offered before anyone
    /// thinks to go looking for them.
    QuickFixes {
        doc: DocId,
        at: usize,
    },
    Signature {
        doc: DocId,
        at: usize,
    },
    /// A code action we asked the server to work out the details of before
    /// applying it.
    ResolveAction,
    /// A suggestion we asked the server to fill in while it sits under the
    /// cursor in the list. Where the import that comes with it lives, for the
    /// servers that do not work one out until asked.
    ResolveCompletion {
        doc: DocId,
        /// Which suggestion in the open list, since the answer says nothing
        /// about which question it belongs to.
        index: usize,
    },
    /// A command we asked the server to run. The answer is usually nothing;
    /// what matters arrives separately as `workspace/applyEdit`.
    Command,
    /// The text of a class that lives inside a jar, and where in it we were
    /// going when we asked.
    ClassFile {
        uri: String,
        line: usize,
        column: usize,
    },
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

/// How a server wants to hear about an edit, as it said at startup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sync {
    /// Not at all.
    None,
    /// The whole document, every time. What the simpler servers ask for, and
    /// what taplo asks for.
    Full,
    /// Just what changed, as ranges. What the big ones ask for, because
    /// re-reading a hundred thousand lines on every keystroke is not free.
    Incremental,
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
    /// The process, the pipe to it, and the questions it has not answered yet.
    rpc: Peer<Ask>,
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

    /// How this server wants to be told about an edit.
    ///
    /// Not a detail to skip. A server that asked for the whole document and is
    /// handed a range is being handed something it has no way to read: by the
    /// letter of the protocol a full-sync change has a `text` and no `range`,
    /// so what arrives is either an error or — worse — a document replaced by
    /// the four characters you just typed. Either way the server's copy stops
    /// being your file, and everything it says afterwards is about something
    /// that does not exist. It looks exactly like features working until the
    /// first keystroke and never again.
    fn sync(&self) -> Sync {
        sync_of(&self.capabilities)
    }

    /// The characters that should make completions appear on their own.
    pub fn completion_triggers(&self) -> Vec<char> {
        triggers_of(&self.capabilities, "completionProvider")
    }

    /// The characters that should make a signature hint appear.
    pub fn signature_triggers(&self) -> Vec<char> {
        triggers_of(&self.capabilities, "signatureHelpProvider")
    }
}

/// What a server's announced capabilities say about being told of an edit.
///
/// A free function rather than a method because it is a fact about what the
/// server said, and reading it out of a blob of JSON is a thing worth testing
/// without a process on the other end of a pipe.
fn sync_of(capabilities: &Value) -> Sync {
    // Two shapes: a bare number, or an object with `change` in it. Both are in
    // the specification and servers use both.
    let said = match capabilities.get("textDocumentSync") {
        Some(Value::Object(it)) => it.get("change").and_then(Value::as_i64),
        Some(other) => other.as_i64(),
        None => None,
    };
    match said {
        Some(0) => Sync::None,
        Some(1) => Sync::Full,
        Some(2) => Sync::Incremental,
        // Nothing said, or something nobody understands. The specification's
        // default is none at all, but a server that says nothing and means it
        // is rarer by far than one that forgot — and of the two ways to be
        // wrong, sending the whole document is merely wasteful.
        _ => Sync::Full,
    }
}

/// The trigger characters under one of the capabilities that has them.
fn triggers_of(capabilities: &Value, capability: &str) -> Vec<char> {
    capabilities
        .get(capability)
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

impl Server {
    /// Everything that writes goes through here, so that a pipe that has
    /// closed under us is noticed in one place rather than four.
    fn note_failure(&mut self) {
        if let Some(why) = self.rpc.take_failure() {
            self.state = State::Dead(why);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.rpc.notify(method, params);
        self.note_failure();
    }

    /// Ask something, and write down what the answer will mean.
    fn request(&mut self, method: &str, params: Value, ask: Ask) -> i64 {
        let id = self.rpc.request(method, params, ask);
        self.note_failure();
        id
    }

    fn answer(&mut self, id: Value, result: Value) {
        self.rpc.answer(id, result);
        self.note_failure();
    }

    /// Take back what an answer was for. Called once per reply, so a
    /// duplicate answer from a confused server is ignored rather than acted
    /// on twice.
    pub fn claim(&mut self, id: i64) -> Option<Ask> {
        self.rpc.claim(id)
    }

    /// Stop it, politely and then not.
    fn shutdown(&mut self) {
        self.rpc.stop();
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
    /// Which Python environment to use for a project, where somebody has said
    /// so, by project root. Empty means "whichever one is found", which is
    /// right nearly always and wrong exactly when a project has two.
    pub environments: BTreeMap<PathBuf, PathBuf>,
}

impl Servers {
    pub fn new(tx: Sender<crate::app::Event>) -> Self {
        Self {
            servers: Vec::new(),
            tx,
            failed: HashSet::new(),
            problems: Vec::new(),
            environments: BTreeMap::new(),
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

    /// The one server to ask a question of, where the question does not name a
    /// capability. Where a language has several — a type checker and a linter
    /// — the first is the one that answers questions; the others are there for
    /// their diagnostics.
    pub fn primary_for(&self, doc: &Document) -> Option<ServerId> {
        self.for_doc(doc).first().copied()
    }

    /// The server to ask a *particular* question of: the first one attached to
    /// this file that says it can answer that one.
    ///
    /// Not simply the first server. A language with two of them has two for a
    /// reason, and the reason is that they do different things: Python gets a
    /// type checker and a linter, and only the type checker knows where a name
    /// is defined. Worse, they do not arrive together — `ruff` is answering
    /// inside a few milliseconds and `pyright-langserver` takes seconds to read
    /// a project — so "the first one that is ready" is, for the whole of that
    /// time, the one that cannot answer anything. Asking it and stopping there
    /// is how "find references" comes to quietly do nothing while the menu row
    /// offering it stays lit.
    pub fn who_can(&self, doc: &Document, capability: &str) -> Option<ServerId> {
        self.for_doc(doc)
            .into_iter()
            .find(|id| self.get(*id).is_some_and(|s| s.can(capability)))
    }

    /// *Every* server attached to this file that can answer that one.
    ///
    /// Which is the right question for anything where two answers are better
    /// than one. The fixes for a Python file come from `ruff` and the imports
    /// from `pyright`, and asking only the first server that says it does code
    /// actions gets you one of those two and no way of telling which. Nothing
    /// is lost by asking both: an answer that is empty costs a round trip a
    /// language server was going to be idle for anyway.
    pub fn who_all_can(&self, doc: &Document, capability: &str) -> Vec<ServerId> {
        self.for_doc(doc)
            .into_iter()
            .filter(|id| self.get(*id).is_some_and(|s| s.can(capability)))
            .collect()
    }

    /// Whether anything attached to this file can do this at all. What a menu
    /// row asks before offering itself, so that what is lit and what works are
    /// the same set of things.
    pub fn can(&self, doc: &Document, capability: &str) -> bool {
        self.who_can(doc, capability).is_some()
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
        let filled = self.fill(config, root);
        let rpc = Peer::start(
            rpc::Spawn {
                command: &config.command,
                args: &filled.args,
                root,
                env: &filled.env,
                label: &config.command,
            },
            self.tx.clone(),
            move |incoming| crate::app::Event::Lsp(id, incoming),
        )
        .map_err(|e| match e {
            // The common case by far is that it is not installed, and saying
            // so is more use than an errno.
            rpc::NotStarted::Missing => format!(
                "{} is not installed — code intelligence is off for now",
                config.command
            ),
            rpc::NotStarted::Failed(why) => why,
        })?;

        let mut server = Server {
            id,
            name: config.command.clone(),
            root: root.to_path_buf(),
            state: State::Starting,
            capabilities: Value::Null,
            settings: filled.settings.clone(),
            rpc,
            open: HashSet::new(),
            queued: Vec::new(),
            progress: BTreeMap::new(),
            message: None,
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
                "initializationOptions": filled.init_options.clone().unwrap_or(Value::Null),
                "capabilities": capabilities(),
            }),
            Ask::Initialize,
        );
        Ok(server)
    }

    /// A server's configuration with its `${…}` placeholders filled in.
    ///
    /// This is what points a Python type checker at the project's virtual
    /// environment. It is not written into the code as a Python special case:
    /// `languages.json` says `"pythonPath": "${python}"`, and a server that
    /// never mentions a placeholder is handed back untouched — including
    /// without so much as a look at the disk, since working out which
    /// environment a project means costs a directory read and most languages
    /// have no such question.
    fn fill(&self, config: &lang::Server, root: &Path) -> lang::Server {
        if !wants_filling(config) {
            return config.clone();
        }
        let picked = self.environments.get(root).map(PathBuf::as_path);
        let env = venv::chosen(root, picked);
        let vars = venv::Vars::new(root, env.as_ref());
        lang::Server {
            id: config.id.clone(),
            name: config.name.clone(),
            command: config.command.clone(),
            args: config.args.iter().filter_map(|a| vars.fill(a)).collect(),
            roots: config.roots.clone(),
            init_options: config
                .init_options
                .as_ref()
                .and_then(|v| vars.fill_value(v)),
            settings: config.settings.as_ref().and_then(|v| vars.fill_value(v)),
            env: config
                .env
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), vars.fill(v)?)))
                .collect(),
        }
    }

    /// Which environment a project is using, for the status line and for the
    /// list you pick from.
    pub fn environment_for(&self, root: &Path) -> Option<venv::Env> {
        venv::chosen(root, self.environments.get(root).map(PathBuf::as_path))
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
    /// Tell the servers about an edit, each in the form it asked for.
    ///
    /// Not one message sent to everybody. Which form a server wants is a thing
    /// it told us at startup and is not ours to choose — see
    /// [`Server::sync`] for what goes wrong when it is chosen for them.
    pub fn did_change(&mut self, doc: &Document, edits: &[AppliedEdit]) {
        if edits.is_empty() {
            return;
        }
        let Some(path) = doc.path.clone() else { return };
        let uri = uri_of(&path);

        // Worked out once each, and only if somebody wants that form: the
        // whole text of a large file is not a thing to build for a server
        // that did not ask for it.
        let ranged: Vec<Value> = edits
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
        let mut whole: Option<Vec<Value>> = None;

        for server in self.servers.iter_mut().filter(|s| s.open.contains(&path)) {
            let changes = match server.sync() {
                // It asked not to be told. Telling it anyway is a protocol
                // error, not a kindness.
                Sync::None => continue,
                Sync::Incremental => ranged.clone(),
                Sync::Full => whole
                    .get_or_insert_with(|| vec![json!({ "text": doc.text() })])
                    .clone(),
            };
            server.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": doc.version },
                    "contentChanges": changes,
                }),
            );
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
        let id = self.who_can(doc, capability)?;
        let server = self.get_mut(id)?;
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
        self.goto_or(doc, at, what, None)
    }

    /// Go to a definition, with something to try if there is not one there.
    ///
    /// `fallback` is a name to search the project for when the server has
    /// nothing at that position — how following a name out of a docstring
    /// keeps working for a name the file itself never mentions.
    pub fn goto_or(
        &mut self,
        doc: &Document,
        at: usize,
        what: Goto,
        fallback: Option<String>,
    ) -> Option<ServerId> {
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
            Ask::Goto {
                doc: doc.id,
                what,
                fallback,
            },
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

    /// What the servers offer to do about the selection: fix an error, import
    /// a name, fill in a match.
    ///
    /// All of them, not the first one that says it does code actions. See
    /// [`Servers::who_all_can`] — this is the question that gets it wrong most
    /// visibly, because the menu it fills is called "what can be done here"
    /// and quietly leaving out half the answer is a menu that lies.
    pub fn code_actions(&mut self, doc: &Document, range: Range) -> Vec<ServerId> {
        self.ask_actions(
            doc,
            range,
            None,
            Ask::CodeActions {
                doc: doc.id,
                at: range.start(),
            },
        )
    }

    /// Only the fixes: what a server would do about a diagnostic, and nothing
    /// about the code that is already fine.
    ///
    /// Asked for on its own because it is asked for constantly — every time
    /// the cursor lands on a red squiggle — and `only` is what keeps that from
    /// meaning "work out every refactoring available at this position", which
    /// for a large project is not a question you want asked on a timer.
    pub fn quick_fixes(&mut self, doc: &Document, range: Range) -> Vec<ServerId> {
        self.ask_actions(
            doc,
            range,
            Some(json!(["quickfix"])),
            Ask::QuickFixes {
                doc: doc.id,
                at: range.start(),
            },
        )
    }

    /// Ask *one* server what it would fix in the whole file under one kind:
    /// `source.fixAll`, `source.organizeImports`.
    ///
    /// This is the half of "format the file" that a formatter is not: `ruff
    /// format` lays the code out, and `ruff check --fix` is what takes the
    /// unused import away. They are different requests, and an editor that
    /// only makes the first one leaves you with a file that is beautifully
    /// laid out and still has forty warnings in it.
    ///
    /// One server and one kind at a time, deliberately. Two of these worked
    /// out against the same text cannot both be applied to it: each is a set
    /// of edits at positions in the file as it was, and the first one to go in
    /// moves everything the second one was pointing at. Asking them one after
    /// another means every answer is about the file as it actually is.
    pub fn source_action(&mut self, doc: &Document, kind: &str, id: ServerId) -> bool {
        let Some(path) = doc.path.clone() else {
            return false;
        };
        if !self.get(id).is_some_and(|s| s.can("codeActionProvider")) {
            return false;
        }
        let whole = Range::new(0, doc.rope.len_chars());
        let here: Vec<Value> = doc
            .diagnostics
            .iter()
            .map(|d| diagnostic_to_lsp(d, doc))
            .collect();
        let (to_line, to_char) = doc.lsp_point_at(whole.end());
        let ask = Ask::SourceActions {
            doc: doc.id,
            version: doc.version,
        };
        let Some(server) = self.get_mut(id) else {
            return false;
        };
        server.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri_of(&path) },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": to_line, "character": to_char },
                },
                "context": {
                    "diagnostics": here,
                    "only": [kind],
                    "triggerKind": 2,
                },
            }),
            ask,
        );
        true
    }

    /// Put the same code action question to every server that can answer it.
    ///
    /// Each gets its own request and answers in its own time; whoever asked
    /// gathers the answers up. Nothing here waits.
    fn ask_actions(
        &mut self,
        doc: &Document,
        range: Range,
        only: Option<Value>,
        ask: Ask,
    ) -> Vec<ServerId> {
        let Some(path) = doc.path.clone() else {
            return Vec::new();
        };
        let ids = self.who_all_can(doc, "codeActionProvider");
        let here: Vec<Value> = doc
            .diagnostics
            .iter()
            .filter(|d| d.range.overlaps(&range))
            .map(|d| diagnostic_to_lsp(d, doc))
            .collect();
        let (from_line, from_char) = doc.lsp_point_at(range.start());
        let (to_line, to_char) = doc.lsp_point_at(range.end());
        let mut context = json!({ "diagnostics": here });
        if let Some(only) = only {
            context["only"] = only;
            // "The editor asked rather than the person" — servers that care
            // use it to leave out the expensive answers.
            context["triggerKind"] = json!(2);
        }
        let params = json!({
            "textDocument": { "uri": uri_of(&path) },
            "range": {
                "start": { "line": from_line, "character": from_char },
                "end": { "line": to_line, "character": to_char },
            },
            "context": context,
        });

        let mut asked = Vec::new();
        for id in ids {
            let Some(server) = self.get_mut(id) else {
                continue;
            };
            server.request("textDocument/codeAction", params.clone(), ask.clone());
            asked.push(id);
        }
        asked
    }

    /// Ask for the text of something that is not a file.
    ///
    /// `jdtls` only. Java's answer to "where is `List` defined" is inside
    /// `rt.jar`, which is not a path and cannot be opened; the server offers
    /// this instead, and hands back the source or a decompilation of it. It is
    /// not in the protocol — it is an extension jdtls invented and every Java
    /// editor implements, because without it going to a definition in a
    /// library simply does not work.
    pub fn class_file(
        &mut self,
        doc: &Document,
        uri: &str,
        line: usize,
        column: usize,
    ) -> Option<ServerId> {
        if !uri.starts_with("jdt://") {
            return None;
        }
        let id = self.primary_for(doc)?;
        let server = self.get_mut(id)?;
        server.request(
            "java/classFileContents",
            json!({ "uri": uri }),
            Ask::ClassFile {
                uri: uri.to_string(),
                line,
                column,
            },
        );
        Some(id)
    }

    pub fn format(&mut self, doc: &Document, tab_width: usize, spaces: bool) -> Option<ServerId> {
        let path = doc.path.clone()?;
        let id = self.who_can(doc, "documentFormattingProvider")?;
        let server = self.get_mut(id)?;
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
        let id = self.who_can(doc, "documentSymbolProvider")?;
        let server = self.get_mut(id)?;
        server.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri_of(&path) } }),
            Ask::Symbols { doc: doc.id },
        );
        Some(id)
    }

    /// Search the project for a name.
    ///
    /// `going` is set when the answer is meant to be gone to rather than
    /// browsed — a name followed out of a docstring — and carries the name so
    /// that "there is nothing called that" can say what "that" was.
    pub fn workspace_symbols(
        &mut self,
        doc: &Document,
        query: &str,
        going: Option<String>,
    ) -> Option<ServerId> {
        let id = self.who_can(doc, "workspaceSymbolProvider")?;
        let server = self.get_mut(id)?;
        server.request(
            "workspace/symbol",
            json!({ "query": query }),
            Ask::WorkspaceSymbols { going },
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

    /// Ask the server to fill in a suggestion it sent in outline.
    ///
    /// Several servers — TypeScript's, and the Java and C# ones — never work
    /// out the import a suggestion needs until somebody asks about that one
    /// suggestion. Taking such an item as it arrived puts the name in and
    /// leaves the file not compiling, which is the opposite of the point.
    pub fn resolve_completion(
        &mut self,
        id: ServerId,
        doc: DocId,
        index: usize,
        item: &Value,
    ) -> bool {
        let Some(server) = self.get_mut(id) else {
            return false;
        };
        if server
            .capabilities
            .get("completionProvider")
            .and_then(|c| c.get("resolveProvider"))
            != Some(&Value::Bool(true))
        {
            return false;
        }
        server.request(
            "completionItem/resolve",
            item.clone(),
            Ask::ResolveCompletion { doc, index },
        );
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
            if server.rpc.is_writable() {
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
            server.rpc.close();
            server.open.clear();
            server.progress.clear();
            let key = (server.name.clone(), server.root.clone());
            self.failed.insert(key);
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
                    // `additionalTextEdits` is the one that matters. A
                    // server will not offer a name your file has not imported
                    // unless the client says it can take the import
                    // separately and later — rust-analyzer leaves every such
                    // name out of the list entirely otherwise, which is the
                    // difference between typing `HashMa` and being offered
                    // `HashMap` and typing it out in full and importing it by
                    // hand afterwards. It is a promise to ask, and
                    // `resolve_completion` is where we keep it.
                    "resolveSupport": {
                        "properties": ["documentation", "detail", "additionalTextEdits"],
                    },
                    // Where a server puts the part of a suggestion that is
                    // not its name — `(use std::collections::HashMap)`. Say
                    // nothing and rust-analyzer glues it onto the label,
                    // where it gets in the way of matching what you typed.
                    "labelDetailsSupport": true,
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

/// Whether a server's configuration mentions a placeholder at all.
///
/// Asked before anything is looked up, because the looking up is a directory
/// read and the answer is no for every language but one.
fn wants_filling(config: &lang::Server) -> bool {
    fn mentions(text: &str) -> bool {
        text.contains("${")
    }
    config.args.iter().any(|a| mentions(a))
        || config.env.values().any(|v| mentions(v))
        || [&config.settings, &config.init_options]
            .into_iter()
            .flatten()
            .any(|v| mentions(&v.to_string()))
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
        data: value.get("data").cloned(),
        told: Told::Server(server.0),
    })
}

/// A diagnostic on its way back out, for a code action request that has to say
/// which problem it is about.
///
/// Faithfully, which matters more than it sounds. A server is not told "there
/// is a problem near here" — it is handed back the problem it sent, and it
/// looks it up. `ruff` matches on the range and on `data`, in which it put the
/// fix; a diagnostic that comes back with neither is one it cannot recognise,
/// and it answers with the actions it has for the *file* and none of the ones
/// it has for that line. Which is exactly what "the linter's warnings cannot
/// be fixed from inside the editor" looks like.
fn diagnostic_to_lsp(d: &Diagnostic, doc: &Document) -> Value {
    let (from_line, from_char) = doc.lsp_point_at(d.range.start());
    let (to_line, to_char) = doc.lsp_point_at(d.range.end());
    let mut out = json!({
        "range": {
            "start": { "line": from_line, "character": from_char },
            "end": { "line": to_line, "character": to_char },
        },
        "severity": match d.severity {
            Severity::Error => 1,
            Severity::Warning => 2,
            Severity::Info => 3,
            Severity::Hint => 4,
        },
        "message": d.message,
        "source": d.source,
        "code": d.code,
    });
    if let Some(data) = &d.data {
        out["data"] = data.clone();
    }
    out
}

pub fn point_of(value: &Value) -> Option<(usize, usize)> {
    Some((
        value.get("line")?.as_u64()? as usize,
        value.get("character")?.as_u64()? as usize,
    ))
}

/// Where a server's complaints go, so that the status line can tell you.
/// Shared with everything else textfold starts — see [`crate::rpc::log_path`].
pub use crate::rpc::log_path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_told_about_an_edit_the_way_it_asked_to_be() {
        // The difference between a server that works and one that works until
        // the first keystroke. A full-sync server handed a range gets a
        // document replaced by the few characters you just typed, and
        // everything it says afterwards is about a file that does not exist.
        assert_eq!(sync_of(&json!({ "textDocumentSync": 1 })), Sync::Full);
        assert_eq!(sync_of(&json!({ "textDocumentSync": 2 })), Sync::Incremental);
        assert_eq!(sync_of(&json!({ "textDocumentSync": 0 })), Sync::None);

        // The other shape it comes in. Both are in the specification and
        // servers use both — taplo says `1`, and plenty say the object.
        assert_eq!(
            sync_of(&json!({ "textDocumentSync": { "openClose": true, "change": 2 } })),
            Sync::Incremental
        );
        assert_eq!(
            sync_of(&json!({ "textDocumentSync": { "openClose": true, "change": 1 } })),
            Sync::Full
        );

        // Said nothing, or said something nobody understands. Of the two ways
        // to be wrong, the whole document is merely wasteful.
        assert_eq!(sync_of(&json!({})), Sync::Full);
        assert_eq!(
            sync_of(&json!({ "textDocumentSync": { "openClose": true } })),
            Sync::Full
        );
    }

    #[test]
    fn the_server_that_taplo_is_asks_for_the_whole_document() {
        // Taken from what taplo 0.10 actually answers `initialize` with, so
        // that this stays a fact about taplo rather than about our reading of
        // the specification.
        let taplo = json!({
            "textDocumentSync": 1,
            "completionProvider": {
                "resolveProvider": false,
                "triggerCharacters": [".", "=", "[", "{", ",", "\""]
            },
            "hoverProvider": true,
            "documentFormattingProvider": true
        });
        assert_eq!(sync_of(&taplo), Sync::Full);
        // And the characters that make its completions appear as you type,
        // which for TOML is where a key or a value begins.
        let triggers = triggers_of(&taplo, "completionProvider");
        for c in ['.', '=', '[', '{', ',', '"'] {
            assert!(triggers.contains(&c), "{c} should open the list");
        }
    }

    /// The Python textfold ships with reaches the server as a real path.
    ///
    /// End to end from `languages.json`, because the value of this is entirely
    /// in the two halves agreeing: a placeholder nobody fills in and a filler
    /// nobody wrote a placeholder for both look fine on their own.
    #[test]
    fn a_python_server_is_pointed_at_the_environment_beside_the_project() {
        let dir = std::env::temp_dir().join(format!("textfold-lsp-venv-{}", std::process::id()));
        let project = dir.join("project");
        let bin = project.join(".venv").join("bin");
        std::fs::create_dir_all(&bin).expect("made");
        std::fs::write(bin.join("python3"), "").expect("written");
        std::fs::write(project.join("pyproject.toml"), "").expect("written");

        lang::init();
        let python = lang::by_name("python").expect("shipped");
        let config = lang::get(python).servers[0].clone();
        assert_eq!(config.command, "pyright-langserver");

        let (tx, _rx) = std::sync::mpsc::channel();
        let servers = Servers::new(tx);
        let filled = servers.fill(&config, &project);

        let said = filled
            .settings
            .as_ref()
            .and_then(|s| s.pointer("/python/pythonPath"))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let want = bin.join("python3").canonicalize().ok();
        assert_eq!(
            said.and_then(|p| p.canonicalize().ok()),
            want,
            "pyright was not told where Python is: {:?}",
            filled.settings
        );
        assert!(
            filled.env.get("PATH").is_some_and(|p| p.contains(".venv")),
            "the environment's bin was not put on PATH: {:?}",
            filled.env
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// And a project with no environment is not told a lie about one.
    #[test]
    fn a_python_project_with_no_environment_is_told_nothing_about_one() {
        let dir = std::env::temp_dir().join(format!("textfold-lsp-bare-{}", std::process::id()));
        let project = dir.join("project");
        std::fs::create_dir_all(&project).expect("made");
        std::fs::write(project.join("pyproject.toml"), "").expect("written");

        lang::init();
        let python = lang::by_name("python").expect("shipped");
        let config = lang::get(python).servers[0].clone();
        let (tx, _rx) = std::sync::mpsc::channel();
        let servers = Servers::new(tx);
        // `VIRTUAL_ENV` from whatever ran the tests would be a real answer, and
        // this is about there being none.
        if std::env::var_os("VIRTUAL_ENV").is_some() || std::env::var_os("CONDA_PREFIX").is_some() {
            return;
        }
        let filled = servers.fill(&config, &project);
        assert_eq!(
            filled
                .settings
                .as_ref()
                .and_then(|s| s.pointer("/python/pythonPath")),
            None,
            "a pythonPath was invented: {:?}",
            filled.settings
        );
        assert!(
            !filled.env.contains_key("PATH"),
            "PATH was rewritten around an environment that is not there: {:?}",
            filled.env
        );
        // What did not depend on an environment is still there.
        assert!(
            filled
                .settings
                .as_ref()
                .and_then(|s| s.pointer("/python/analysis/autoSearchPaths"))
                .is_some(),
            "the rest of the settings went with it: {:?}",
            filled.settings
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_server_with_no_placeholders_is_left_exactly_as_it_was() {
        lang::init();
        let rust = lang::by_name("rust").expect("shipped");
        let config = lang::get(rust).servers[0].clone();
        assert!(!wants_filling(&config));
    }

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
