//! Talking to debug adapters.
//!
//! The Debug Adapter Protocol is to running a program what the Language Server
//! Protocol is to reading one: a program somebody else wrote, speaking JSON
//! down a pipe, that knows how to stop `python` at line four and say what `n`
//! is. textfold does not know how to debug anything, in the same way it does
//! not know how to type-check anything, and that is the point — `debugpy`,
//! `lldb-dap` and `js-debug` are the ones who do.
//!
//! **One session at a time.** Not a limitation that was hard to lift and left:
//! a person debugging is debugging *a thing*, and the whole of the interface —
//! F5, the arrow in the margin, the panel along the bottom — is about that
//! thing. Two at once would need a way to say which one you meant at every one
//! of those, in exchange for a case that comes up about once a year.
//!
//! **The editor is told, not asked.** Every message from an adapter arrives on
//! the same channel as a keystroke, is folded into [`Session`], and comes back
//! out as a [`Change`] saying whether anything happened that the person should
//! see. Nothing here draws, opens a file, or moves a cursor: this module knows
//! the protocol and the editor knows the editor.
//!
//! **What to debug is a table, not code.** An adapter's `launch` arguments are
//! whatever that adapter's own documentation says they are, filled in from the
//! manifest with the same `${…}` a language server's settings use. There is no
//! list here of the fields textfold understands, because such a list would be
//! wrong for the next adapter somebody installs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use crate::lang;
use crate::rpc::{self, Peer};

/// What came back from an adapter. The same three kinds of message everything
/// else speaks — see [`crate::rpc`], which does the framing for all of them.
pub use crate::rpc::Incoming;

/// Which session a message belongs to.
///
/// There is only ever one session, which is exactly why this is needed rather
/// than why it is not. Stopping a session kills its adapter, the thread
/// reading that adapter notices the pipe close and posts one last message, and
/// that message arrives *after* the next session has been started — on the
/// same channel, into the same slot. Without a name on it, the new session is
/// told its adapter has gone before it has finished starting, and everything
/// after is ignored because the session already ended.
///
/// From the outside that is: press the key twice and the debugger stops
/// working, for every file, with the program running perfectly well and
/// nothing on the screen to say so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionId(pub usize);

/// What we asked for, so the answer knows what it is an answer to.
///
/// The same bargain [`crate::lsp::Ask`] makes, for the same reason: a reply
/// that arrives after you have stepped twice more should be dropped by
/// whoever asked, rather than by a state machine here guessing whether it is
/// still wanted.
#[derive(Clone, Debug)]
pub enum Ask {
    Initialize,
    /// `launch` or `attach` — the request that starts the program.
    Start,
    /// Breakpoints for one file, so that what the adapter says it actually
    /// managed to set can be put back against that file.
    Breakpoints {
        path: PathBuf,
    },
    Threads,
    Stack,
    Scopes,
    /// The contents of one scope or one structured value.
    Variables {
        reference: i64,
    },
    /// Something typed into the panel.
    Evaluate {
        what: String,
    },
    /// Continue, step, pause, terminate. There is nothing to do with the
    /// answer beyond saying so if it failed — which is worth doing, because a
    /// step that was refused otherwise looks exactly like a step that worked
    /// and landed where you already were.
    Control {
        what: &'static str,
    },
}

/// Where a session has got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// The adapter is up and has been asked to `initialize`.
    Starting,
    /// The program is running and nothing is stopped.
    Running,
    /// Stopped, and why: `breakpoint`, `step`, `exception`.
    Stopped(String),
    /// Over, and why. Kept rather than cleared so that the panel can say how
    /// it ended instead of emptying itself the moment it does.
    Ended(String),
}

impl State {
    /// The word for the status line.
    pub fn label(&self) -> &str {
        match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Stopped(why) => why,
            State::Ended(why) => why,
        }
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self, State::Stopped(_))
    }

    pub fn is_over(&self) -> bool {
        matches!(self, State::Ended(_))
    }
}

/// One line of a call stack.
#[derive(Clone, Debug)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    /// Where its code is, where the adapter named a file we could open. A
    /// frame inside the interpreter's own C has none, and is still worth
    /// showing — it is often the reason you are looking.
    pub path: Option<PathBuf>,
    /// Counted from zero, like everywhere else in textfold. The protocol
    /// counts from one and the conversion happens at the edge, once.
    pub line: usize,
    pub column: usize,
}

/// One thread of the program being debugged.
#[derive(Clone, Debug)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

/// One name and value in view at a frame.
#[derive(Clone, Debug)]
pub struct Variable {
    pub name: String,
    pub value: String,
    /// What it holds, where the adapter can say. Shown after the value when
    /// the value alone does not say — `[]` is a list of what?
    pub kind: Option<String>,
    /// Non-zero for something that can be opened up. This is the handle the
    /// adapter wants back to say what is inside, and it goes stale the moment
    /// the program moves — see [`Session::forget_values`].
    pub reference: i64,
}

/// A group of variables at a frame: `Locals`, `Globals`.
#[derive(Clone, Debug)]
pub struct Scope {
    pub name: String,
    pub reference: i64,
    /// Whether the adapter says fetching it is slow. Not opened on its own,
    /// which is what keeps stopping at a breakpoint from fetching every
    /// module in the interpreter.
    pub expensive: bool,
    /// What the adapter calls this kind of scope: `arguments`, `locals`,
    /// `registers`. The protocol's own words, and what decides which of them
    /// open without being asked.
    pub hint: Option<String>,
}

/// What a message from the adapter means to the editor, beyond a redraw.
///
/// Deliberately small. The editor decides what to *do* about stopping —
/// which file to open, whether to move the cursor, what to say in the status
/// line — and this only says that it happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    /// Nothing the editor has to do beyond drawing what is already here.
    Nothing,
    /// It has stopped somewhere, and [`Session::here`] says where.
    Stopped,
    /// The program is going again, so whatever was showing the stopped line
    /// should stop showing it.
    Resumed,
    /// It is over.
    Ended,
}

/// How many lines of a program's output are kept.
///
/// A program under a debugger is very often one printing in a loop, and the
/// panel is a panel rather than a terminal: everything it has ever said is
/// both unbounded memory and unreadable. The last few hundred lines is what
/// somebody actually reads.
const OUTPUT_KEPT: usize = 400;

/// One line that was printed, and who printed it.
///
/// Two different things arrive on the same list and they answer different
/// questions. What the *program* printed is the thing the `print` was put
/// there for, and is very often the whole reason a debugger was started at
/// all. What the *editor* has to say — "would not start", the command line it
/// actually ran, the way to point it somewhere else — is about the run rather
/// than in it.
///
/// Run together with no mark on them, the second kind is what makes people
/// stop reading the panel: fifteen rows about interpreters above the one line
/// their program printed. Kept apart, each can be shown in its own words and
/// its own colour, and "show me what my program printed" is a question that
/// can be answered exactly. See [`Session::program_printed`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Printed {
    pub text: String,
    pub from: Printer,
}

/// Whose line it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Printer {
    /// The program, on its standard output.
    Out,
    /// The program, on its standard error. Worth telling apart from `Out`
    /// only because it is worth *colouring* apart: a traceback among six
    /// hundred lines of ordinary output should be findable at a glance.
    Err,
    /// The editor or the adapter, about the run rather than in it.
    Note,
}

impl Printer {
    /// Whether this is the program's own output, as against the editor
    /// talking about the program.
    pub fn is_the_program(&self) -> bool {
        matches!(self, Printer::Out | Printer::Err)
    }
}

/// One debugging session: the adapter, the program, and where it has got to.
pub struct Session {
    /// Which session this is, so a message from the one before it can be told
    /// apart from a message for this one.
    id: SessionId,
    /// The adapter's short name, for the status line: `debugpy`.
    pub name: String,
    /// What is being debugged, in the words to show: `main.py`.
    pub what: String,
    peer: Peer<Ask>,
    pub state: State,
    /// What the adapter said it can do. Consulted rather than assumed: an
    /// adapter that cannot step out should not be offered a key that does.
    caps: Value,
    /// The `launch` or `attach` request, held until `initialize` comes back.
    start: (&'static str, Value),
    /// Whether the adapter has said it is ready for breakpoints. Until it
    /// has, sending them is a protocol error rather than an early start.
    configured: bool,
    /// Whether it ever answered `initialize`.
    ///
    /// The line between "your program ended" and "the debugger never ran".
    /// They are the same event on the wire — a process that is no longer
    /// there — and telling somebody their program finished when what actually
    /// happened is that nothing ever started it is the worst answer an editor
    /// can give: it is wrong, and it sounds like it worked.
    answered: bool,
    /// What the manifest says to do about it not being there.
    ///
    /// Data rather than code, because "how do I get `debugpy`" has a different
    /// answer for every adapter and the one place that knows it is the
    /// manifest that named the adapter in the first place.
    see: Option<String>,
    /// The adapter's full id — `python/debugpy` — so that the way to point it
    /// at something else can be spelled out rather than looked up.
    adapter: String,
    /// The command line, exactly as it was run, with a bare command name
    /// resolved to the file a shell would have found.
    ///
    /// Shown when the adapter would not start, because at that point the
    /// question is always "what did you actually try to run" — and `${python}`
    /// having quietly resolved to an interpreter with no `debugpy` in it is
    /// invisible from anywhere else. `python3` and `python3` are the same five
    /// characters and, often enough, two different programs.
    ran: String,
    /// Where that interpreter came from, in the words the environment picker
    /// uses: the shell it was started from, one in the project, conda, or the
    /// `PATH`.
    ///
    /// This is the other half of the same question, and the half nobody can
    /// work out for themselves: a virtual environment activated in the shell
    /// an hour ago for a *different* project is still `VIRTUAL_ENV`, textfold
    /// takes that as the answer for this one, and from the outside it looks
    /// like an editor that cannot find a package you can plainly import.
    ///
    /// `None` for an adapter that never asked for one. `gdb` is not run by a
    /// Python and does not care what Python this project uses, and a failure
    /// report that mentions virtual environments to somebody debugging a C
    /// program is an answer to a question they did not ask — noise in exactly
    /// the place where every line has to count. See [`uses_an_environment`].
    from: Option<String>,

    pub threads: Vec<Thread>,
    /// The thread that stopped, which is the one everything else is about.
    pub thread: Option<i64>,
    pub frames: Vec<Frame>,
    /// The frame being looked at, which is the top one unless somebody has
    /// clicked another. Its variables are the ones in the panel.
    pub frame: Option<i64>,
    pub scopes: Vec<Scope>,
    /// What is inside each reference the adapter has been asked about.
    pub values: BTreeMap<i64, Vec<Variable>>,
    /// Which structured values have been opened up in the panel.
    pub open: HashSet<i64>,
    /// What the program and the editor have printed, in the order it arrived
    /// and marked with which of them said it.
    pub output: Vec<Printed>,
    /// Which lines the adapter actually managed to set a breakpoint on, by
    /// file. An adapter moves a breakpoint to the next line that has code on
    /// it, and refuses one it cannot set at all; both are worth showing,
    /// because a breakpoint that is not where you put it is the single most
    /// confusing thing a debugger does. See [`Debugger::is_verified`], which
    /// is what the margin draws from.
    ///
    /// It is also what makes taking a breakpoint away possible: the protocol
    /// has no "remove one", only "here is every breakpoint in this file", so a
    /// file that had some and now has none still has to be named — and this is
    /// the record of which files those are.
    pub verified: BTreeMap<PathBuf, Vec<usize>>,
}

impl Session {
    /// Where the program is stopped, as a file and a line counted from zero.
    ///
    /// The frame being looked at rather than the top one: clicking your way up
    /// the stack should move the editor with it, which is most of what a stack
    /// is for.
    pub fn here(&self) -> Option<(PathBuf, usize, usize)> {
        let frame = self.selected()?;
        Some((frame.path.clone()?, frame.line, frame.column))
    }

    /// The frame being looked at.
    pub fn selected(&self) -> Option<&Frame> {
        let wanted = self.frame?;
        self.frames.iter().find(|f| f.id == wanted)
    }

    /// Whether the adapter says it can do something. The names are the
    /// protocol's own, so this reads as its documentation does.
    pub fn can(&self, what: &str) -> bool {
        self.caps.get(what).and_then(Value::as_bool).unwrap_or(false)
    }

    fn notify(&mut self, command: &str, arguments: Value) {
        self.peer.notify(command, arguments);
    }

    fn request(&mut self, command: &str, arguments: Value, ask: Ask) {
        self.peer.request(command, arguments, ask);
    }

    /// Everything the adapter told us about values is about the program as it
    /// was standing a moment ago. The moment it moves, every reference in
    /// here names something that may no longer exist — so they go, rather
    /// than being shown as though they were still true.
    fn forget_values(&mut self) {
        self.frames.clear();
        self.frame = None;
        self.scopes.clear();
        self.values.clear();
        // What was opened up is *not* forgotten, because it is a fact about
        // what you were looking at rather than about the program: stepping
        // once and having every tree you opened close itself is the thing
        // that makes a variables panel exhausting to use. The references
        // change, so this is re-applied by name when the new ones arrive.
        self.thread = None;
    }

    /// Put what the adapter printed, and what it was, where somebody will
    /// read it — the panel, rather than a log file they have not been told
    /// about.
    ///
    /// This is the whole of the difference between "the program stopped" and
    /// "no module named debugpy, and here is the interpreter that said so".
    fn blame(&mut self) {
        let complaints = self.peer.complaints();
        if complaints.is_empty() {
            self.say("it stopped without saying anything".to_string());
        }
        for line in complaints {
            self.say(line);
        }
        // What was run, for an adapter that is a program. One that lives
        // inside a language server was not run by us at all, and "textfold
        // ran:" with nothing after it is a line that raises a question rather
        // than answering one.
        let ran = self.ran.clone();
        if !ran.trim().is_empty() {
            self.say(format!("textfold ran: {ran}"));
        }
        // Only where the adapter asked for an interpreter in the first place.
        // See [`Session::from`].
        if let Some(from) = self.from.clone() {
            self.say(format!("from {from}"));
        }
        if let Some(see) = self.see.clone() {
            self.say(see);
        }
        // And the way out, spelled out. Somebody whose interpreter is the
        // wrong one can say so in a settings file, and telling them where is
        // the difference between a fixable problem and a wall. An adapter a
        // language server starts is not one you can point elsewhere, so it is
        // offered `launch` and `attach` instead of `command` — the two things
        // about it that *are* yours to change.
        let (plugin, name) = match self.adapter.split_once('/') {
            Some((plugin, name)) => (plugin.to_string(), name.to_string()),
            None => (self.adapter.clone(), self.adapter.clone()),
        };
        self.say(match self.ran.trim().is_empty() {
            true => format!(
                "to change what it is asked for: {{\"debuggers\": {{\"{name}\": \
                 {{\"launch\": {{…}}}}}}}} in your settings for the {plugin} plugin"
            ),
            false => format!(
                "to run something else: {{\"debuggers\": {{\"{name}\": \
                 {{\"command\": \"…\"}}}}}} in your settings for the {plugin} plugin"
            ),
        });
    }

    /// The one line worth putting in the status bar when it would not start.
    ///
    /// The last thing it said, because a program that fails on its way up
    /// prints the useful part last: a Python traceback ends with the error.
    pub fn why_not(&self) -> Option<String> {
        self.peer
            .complaints()
            .into_iter()
            .rev()
            .find(|line| !line.trim().is_empty())
    }

    /// Whether it ever got as far as being a debugger.
    pub fn ever_started(&self) -> bool {
        self.answered
    }

    /// The editor's own word about the run, which is not the program's output
    /// and is not shown as though it were.
    fn say(&mut self, line: impl Into<String>) {
        self.printed(line, Printer::Note);
    }

    fn printed(&mut self, line: impl Into<String>, from: Printer) {
        self.output.push(Printed {
            text: line.into(),
            from,
        });
        if self.output.len() > OUTPUT_KEPT {
            let over = self.output.len() - OUTPUT_KEPT;
            self.output.drain(..over);
        }
    }

    /// Whether the program itself has printed anything yet.
    pub fn has_printed(&self) -> bool {
        self.output.iter().any(|line| line.from.is_the_program())
    }

    /// Everything on the list, in one piece. What a test reads, and what the
    /// panel's rows are made of one at a time.
    #[cfg(test)]
    fn all_printed(&self) -> String {
        self.output
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Everything the program printed, in one piece, for reading somewhere
    /// bigger than a panel fourteen rows tall.
    ///
    /// The program's own lines and nothing else: somebody who asked to see
    /// what their program printed did not ask for the editor's account of how
    /// it was started, however useful that is two rows further up.
    pub fn program_printed(&self) -> String {
        self.output
            .iter()
            .filter(|line| line.from.is_the_program())
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The debugger: at most one session, and the machinery to start it.
pub struct Debugger {
    session: Option<Session>,
    /// How many sessions there have been, which is where the next one's name
    /// comes from. Only ever counted up.
    started: usize,
    tx: Sender<crate::app::Event>,
    /// Things worth telling somebody: an adapter that is not installed, or one
    /// that refused what it was asked. Drained by the editor, which has the
    /// status line.
    pub problems: Vec<String>,
}

impl Debugger {
    pub fn new(tx: Sender<crate::app::Event>) -> Self {
        Self {
            session: None,
            started: 0,
            tx,
            problems: Vec::new(),
        }
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// Whether there is a session that has not finished.
    pub fn is_running(&self) -> bool {
        self.session.as_ref().is_some_and(|s| !s.state.is_over())
    }

    /// Whether the adapter managed to put a breakpoint on this line.
    ///
    /// `None` means nobody has been asked yet — there is no session, or this
    /// file has not been sent — and a breakpoint then is neither confirmed nor
    /// refused. `Some(false)` is the interesting one: the adapter was told
    /// about this file and did *not* take this line, which is the difference
    /// between a breakpoint that will stop your program and one that will sit
    /// there looking exactly like it should have.
    pub fn is_verified(&self, path: &Path, line: usize) -> Option<bool> {
        let session = self.session.as_ref().filter(|s| !s.state.is_over())?;
        let lines = session.verified.get(path)?;
        Some(lines.contains(&line))
    }

    /// Start an adapter and ask it to run something.
    ///
    /// `file` is what is being debugged — the file you were looking at when
    /// you pressed the key — and is what `${file}` in the manifest's launch
    /// arguments stands for. `root` is the project, worked out from the
    /// adapter's own markers exactly as a language server's is.
    ///
    /// `environment` is the one the project is using, where somebody has
    /// chosen one. Handed in rather than worked out here, because it has to be
    /// the *same* answer the language servers got: a debugger running under a
    /// different interpreter from the one your type checker is reading is a
    /// debugger that will disagree with your editor about what is installed.
    pub fn start(
        &mut self,
        config: &lang::Debugger,
        root: &Path,
        file: &Path,
        environment: Option<&Path>,
    ) -> Result<(), String> {
        // A session that is over is a session that is in the way. One that is
        // not over is somebody's actual debugging, and is stopped out loud
        // rather than replaced under them — see [`crate::app::App::debug`],
        // which is where that question is asked.
        self.stop();

        self.started += 1;
        let id = SessionId(self.started);
        let filled = filled(config, root, file, environment);
        let peer = Peer::start(
            rpc::Spawn {
                command: &filled.command,
                args: &filled.args,
                root,
                env: &filled.env,
                label: &filled.name,
                dialect: rpc::Dialect::Dap,
            },
            self.tx.clone(),
            move |incoming| crate::app::Event::Dap(id, incoming),
        )
        .map_err(|e| match e {
            rpc::NotStarted::Missing => format!(
                "{} is not installed, so there is nothing here that can debug this",
                filled.command
            ),
            rpc::NotStarted::Failed(why) => why,
        })?;
        self.begin(id, filled, config, root, file, environment, peer);
        Ok(())
    }

    /// Talk to an adapter a language server has already started for us.
    ///
    /// The other half of [`crate::lang::FromServer`]. Everything after the
    /// connection is identical — the same `initialize`, the same breakpoints,
    /// the same panel — which is the point: Java is not a special kind of
    /// debugging, it is a special way of getting hold of a debugger.
    pub fn connect(
        &mut self,
        config: &lang::Debugger,
        root: &Path,
        file: &Path,
        environment: Option<&Path>,
        port: u16,
    ) -> Result<(), String> {
        self.stop();

        self.started += 1;
        let id = SessionId(self.started);
        let filled = filled(config, root, file, environment);
        let peer = Peer::connect(
            rpc::Connect {
                host: "127.0.0.1",
                port,
                label: &filled.name,
                dialect: rpc::Dialect::Dap,
            },
            self.tx.clone(),
            move |incoming| crate::app::Event::Dap(id, incoming),
        )
        .map_err(|e| match e {
            rpc::NotStarted::Missing => format!("nothing is listening on port {port}"),
            rpc::NotStarted::Failed(why) => why,
        })?;
        self.begin(id, filled, config, root, file, environment, peer);
        Ok(())
    }

    /// Make the session and say hello. Shared by both ways of getting a peer,
    /// because only the getting differs.
    #[allow(clippy::too_many_arguments)]
    fn begin(
        &mut self,
        id: SessionId,
        filled: lang::Debugger,
        config: &lang::Debugger,
        root: &Path,
        file: &Path,
        environment: Option<&Path>,
        peer: Peer<Ask>,
    ) {

        let what = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| filled.name.clone());
        let mut session = Session {
            id,
            name: filled.name.clone(),
            what,
            peer,
            state: State::Starting,
            caps: Value::Null,
            start: (config.request(), filled.launch.clone()),
            configured: false,
            answered: false,
            see: config.see.clone(),
            adapter: config.id.clone(),
            ran: std::iter::once(
                crate::pack::which(&filled.command)
                    .map(|at| at.display().to_string())
                    .unwrap_or_else(|| filled.command.clone()),
            )
            .chain(filled.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" "),
            from: where_from(config, root, environment),
            threads: Vec::new(),
            thread: None,
            frames: Vec::new(),
            frame: None,
            scopes: Vec::new(),
            values: BTreeMap::new(),
            open: HashSet::new(),
            output: Vec::new(),
            verified: BTreeMap::new(),
        };
        session.request(
            "initialize",
            json!({
                "clientID": "textfold",
                "clientName": "textfold",
                "adapterID": filled.name,
                "locale": "en",
                // Both true, so that the numbers on the wire are the ones a
                // person reads. Everything inside textfold counts from zero,
                // and the conversion happens where the message is read rather
                // than being remembered at forty call sites.
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
                "supportsVariableType": true,
                "supportsVariablePaging": false,
                // Not supported, and said so. An adapter that believes we can
                // open a terminal for it will ask us to, and then wait.
                "supportsRunInTerminalRequest": false,
                "supportsStartDebuggingRequest": false,
                "supportsMemoryReferences": false,
                "supportsProgressReporting": false,
            }),
            Ask::Initialize,
        );
        self.session = Some(session);
    }

    /// Everything an adapter says.
    ///
    /// `breakpoints` is where they are *now*, handed in rather than held here,
    /// because a breakpoint is a fact about a line of text and lives with the
    /// text — see [`crate::doc::Document::breakpoints`]. The adapter asks for
    /// them exactly once, when it says it is ready, and this is where they go.
    pub fn on(
        &mut self,
        id: SessionId,
        message: Incoming,
        breakpoints: &[(PathBuf, Vec<usize>)],
    ) -> Change {
        // Anything from a session that is not the one running now. There is
        // always at least one of these per restart — killing an adapter is
        // what makes its reader thread post that it has gone — and acting on
        // it would end the session that has just begun.
        if self.session.as_ref().is_none_or(|s| s.id != id) {
            return Change::Nothing;
        }
        match message {
            Incoming::Notification { method, params } => {
                self.on_event(&method, &params, breakpoints)
            }
            Incoming::Response { id, result } => self.on_response(id, result),
            Incoming::Request { id, method, .. } => {
                // Answered before anything else, because an adapter waiting on
                // a reply is an adapter that has stopped — and refused rather
                // than ignored, because a refusal it can read is a message in
                // its own log rather than a hang in ours. `runInTerminal` is
                // the one that turns up, and we said at `initialize` that we
                // could not do it.
                if let Some(session) = &mut self.session {
                    session
                        .peer
                        .refuse(id, -32601, &format!("textfold cannot {method}"));
                }
                Change::Nothing
            }
            Incoming::Exited(why) => {
                let Some(session) = &mut self.session else {
                    return Change::Nothing;
                };
                if session.state.is_over() {
                    return Change::Nothing;
                }
                // An adapter that never answered `initialize` did not run
                // anybody's program; it failed to be a debugger. Whatever it
                // printed on its way out is the only account of why there is,
                // so it goes where somebody will read it.
                if !session.answered {
                    session.state = State::Ended("would not start".into());
                    session.blame();
                } else {
                    // One that goes while the program is still running has
                    // taken the program with it, whatever it thought it was
                    // doing.
                    session.state = State::Ended(why);
                }
                session.forget_values();
                Change::Ended
            }
        }
    }

    /// Something the adapter volunteered.
    fn on_event(&mut self, event: &str, body: &Value, breakpoints: &[(PathBuf, Vec<usize>)]) -> Change {
        // "I am ready to be configured." Breakpoints go now and not before:
        // this is the one ordering rule in the protocol that every adapter
        // enforces, and answering it here rather than handing it back to the
        // editor is what keeps "when may breakpoints be sent" a fact about
        // the protocol instead of a fact about the editor's event loop.
        if event == "initialized" {
            if let Some(session) = &mut self.session {
                session.configured = true;
            }
            self.send_breakpoints(breakpoints);
            self.configuration_done();
            return Change::Nothing;
        }
        let Some(session) = &mut self.session else {
            return Change::Nothing;
        };
        match event {
            "stopped" => {
                let why = body
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("stopped")
                    .to_string();
                // The description is where an exception's message is, which
                // is the whole story when the reason is `exception`.
                if let Some(said) = body.get("text").and_then(Value::as_str) {
                    session.say(said.to_string());
                }
                session.forget_values();
                session.state = State::Stopped(why);
                session.thread = body.get("threadId").and_then(Value::as_i64);
                let thread = session.thread;
                session.request("threads", json!({}), Ask::Threads);
                if let Some(thread) = thread {
                    session.request(
                        "stackTrace",
                        json!({ "threadId": thread, "startFrame": 0, "levels": 40 }),
                        Ask::Stack,
                    );
                }
                // Not `Stopped` yet: there is nowhere to go until the stack
                // arrives. The editor is told when it does.
                Change::Nothing
            }
            "continued" => {
                session.forget_values();
                session.state = State::Running;
                Change::Resumed
            }
            "output" => {
                // Telemetry is the adapter talking to its own authors about
                // version numbers, and is not something anybody debugging
                // wants in the middle of their program's output.
                let category = body.get("category").and_then(Value::as_str).unwrap_or("");
                if category == "telemetry" {
                    return Change::Nothing;
                }
                // Nor is an adapter clearing its throat. `gdb` greets its
                // client with fifteen lines of copyright notice, under the
                // category the protocol reserves for *the debuggee's* standard
                // output — so it cannot be told apart by what it claims to be.
                // A panel headed "what it printed" whose first fifteen lines
                // are the GNU licence is a panel nobody reads twice.
                //
                // What it can be told apart by is *when*: a greeting is said
                // before the adapter has answered a single question, and by
                // definition there is no program yet to have printed
                // anything. Which is a fact about the conversation rather
                // than about the clock, so it does not depend on what came
                // back first.
                //
                // Nothing is lost by it. An adapter that fails before it
                // starts is reported from its standard error instead, in full
                // — see [`Session::blame`].
                if !session.answered {
                    return Change::Nothing;
                }
                // `stderr` is the program's, and is worth telling apart so
                // that a traceback can be found at a glance in six hundred
                // lines of ordinary output. Everything else the adapter sends
                // — `stdout`, `console`, `important`, or no category at all —
                // is the run talking, and the protocol's own default for a
                // missing category is `console`.
                let from = match category {
                    "stderr" => Printer::Err,
                    _ => Printer::Out,
                };
                if let Some(text) = body.get("output").and_then(Value::as_str) {
                    for line in text.trim_end_matches('\n').split('\n') {
                        session.printed(line.to_string(), from);
                    }
                }
                Change::Nothing
            }
            // A session that has already ended keeps the reason it ended
            // for. `terminated` follows `exited 3`, and "it finished" is a
            // worse answer than "it exited 3" — the first thing that went
            // wrong is the thing worth saying.
            "terminated" if session.state.is_over() => Change::Nothing,
            "terminated" => {
                session.state = State::Ended("finished".into());
                session.forget_values();
                Change::Ended
            }
            "exited" if session.state.is_over() => Change::Nothing,
            "exited" => {
                let code = body.get("exitCode").and_then(Value::as_i64).unwrap_or(0);
                session.state = State::Ended(match code {
                    0 => "finished".into(),
                    code => format!("exited {code}"),
                });
                session.forget_values();
                Change::Ended
            }
            "thread" => {
                // Worth asking again rather than patching the list by hand:
                // the answer is one small message and the list is what the
                // panel shows.
                session.request("threads", json!({}), Ask::Threads);
                Change::Nothing
            }
            // An adapter saying it has changed its mind about a breakpoint.
            //
            // `debugpy` answers `setBreakpoints` with what it managed and is
            // done. `gdb` cannot answer at all until the program is loaded —
            // before that every breakpoint is "pending" — and says so later,
            // in one of these. Without it the margin would show a hollow dot
            // for the rest of the session beside a breakpoint that works
            // perfectly, which is a lie about the one thing that mark is for.
            "breakpoint" => {
                let Some(about) = body.get("breakpoint") else {
                    return Change::Nothing;
                };
                let path = about
                    .get("source")
                    .and_then(|source| source.get("path"))
                    .and_then(Value::as_str)
                    .map(PathBuf::from);
                let line = about
                    .get("line")
                    .and_then(Value::as_i64)
                    .map(|line| line.max(1) as usize - 1);
                let ok = about
                    .get("verified")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let (Some(path), Some(line)) = (path, line) else {
                    // Nothing that says which line in which file it is about
                    // is nothing this can act on. An adapter is allowed to
                    // send one; guessing which breakpoint it meant is not.
                    return Change::Nothing;
                };
                let taken = session.verified.entry(path).or_default();
                match (ok, taken.contains(&line)) {
                    (true, false) => taken.push(line),
                    (false, true) => taken.retain(|had| *had != line),
                    _ => {}
                }
                Change::Nothing
            }
            "capabilities" => {
                if let Some(more) = body.get("capabilities") {
                    merge(&mut session.caps, more);
                }
                Change::Nothing
            }
            _ => Change::Nothing,
        }
    }

    /// An answer to something we asked.
    fn on_response(&mut self, id: i64, result: Result<Value, String>) -> Change {
        let Some(session) = &mut self.session else {
            return Change::Nothing;
        };
        // Claimed once, so an adapter that answers twice is ignored the second
        // time rather than acted on twice.
        let Some(ask) = session.peer.claim(id) else {
            return Change::Nothing;
        };
        let value = match result {
            Ok(value) => value,
            Err(why) => return self.refused(ask, why),
        };
        match ask {
            Ask::Initialize => {
                session.answered = true;
                session.caps = value;
                // The program is asked for now. What comes back first is the
                // `initialized` event, not this reply — which is why the
                // breakpoints are sent from there and not from here.
                let (request, arguments) = session.start.clone();
                session.request(request, arguments, Ask::Start);
                Change::Nothing
            }
            Ask::Start => {
                // A `launch` that succeeded says nothing. The program is
                // running unless something has already stopped it, which for a
                // breakpoint on the first line it may well have.
                if matches!(session.state, State::Starting) {
                    session.state = State::Running;
                }
                Change::Nothing
            }
            Ask::Breakpoints { path } => {
                // Where the adapter actually put them, which is not always
                // where they were asked for.
                let lines = value
                    .get("breakpoints")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter(|b| {
                                b.get("verified").and_then(Value::as_bool).unwrap_or(false)
                            })
                            .filter_map(|b| Some(b.get("line")?.as_i64()? as usize))
                            .map(|line| line.saturating_sub(1))
                            .collect()
                    })
                    .unwrap_or_default();
                session.verified.insert(path, lines);
                Change::Nothing
            }
            Ask::Threads => {
                session.threads = value
                    .get("threads")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|t| {
                                Some(Thread {
                                    id: t.get("id")?.as_i64()?,
                                    name: t
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("thread")
                                        .to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Change::Nothing
            }
            Ask::Stack => {
                session.frames = read_frames(&value);
                session.frame = session.frames.first().map(|f| f.id);
                let scopes = session.frame;
                if let Some(frame) = scopes {
                    session.request("scopes", json!({ "frameId": frame }), Ask::Scopes);
                }
                // Now there is somewhere to go, so now the editor is told.
                Change::Stopped
            }
            Ask::Scopes => {
                session.scopes = value
                    .get("scopes")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|s| {
                                Some(Scope {
                                    name: s
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("scope")
                                        .to_string(),
                                    reference: s.get("variablesReference")?.as_i64()?,
                                    expensive: s
                                        .get("expensive")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false),
                                    hint: s
                                        .get("presentationHint")
                                        .and_then(Value::as_str)
                                        .map(str::to_string),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // The ones you stopped to look at open themselves; the rest
                // wait to be asked.
                //
                // Which is which is the adapter's own word for it: the
                // protocol has a `presentationHint` of `arguments`, `locals`
                // or `registers`, and those are exactly the three answers
                // needed. `gdb` gives all three and only the first two are
                // ever what somebody wanted — sixteen registers is not a
                // thing you stopped to read. `debugpy` marks `Locals` and
                // says nothing about `Globals`, which is right: opening
                // globals means opening `__builtins__` on every frame.
                //
                // An adapter that hints at nothing gets the old rule, which
                // is the first cheap scope — because the protocol puts them
                // in order of relevance, so the first one is the answer when
                // there is nothing better to go on.
                let hinted: Vec<i64> = session
                    .scopes
                    .iter()
                    .filter(|s| matches!(s.hint.as_deref(), Some("arguments" | "locals")))
                    .map(|s| s.reference)
                    .collect();
                let wanted: Vec<i64> = match hinted.is_empty() {
                    false => hinted,
                    true => session
                        .scopes
                        .iter()
                        .filter(|s| !s.expensive)
                        .map(|s| s.reference)
                        .take(1)
                        .collect(),
                };
                for reference in wanted {
                    session.open.insert(reference);
                    session.request(
                        "variables",
                        json!({ "variablesReference": reference }),
                        Ask::Variables { reference },
                    );
                }
                Change::Nothing
            }
            Ask::Variables { reference } => {
                session.values.insert(reference, read_variables(&value));
                Change::Nothing
            }
            Ask::Evaluate { what } => {
                let said = value
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("(nothing)");
                session.say(format!("{what} = {said}"));
                Change::Nothing
            }
            // A `continue` that worked means the program is going again. Some
            // adapters send a `continued` event as well and some do not, so
            // this is where it is recorded rather than there.
            Ask::Control { what } => {
                if matches!(what, "continue" | "next" | "stepIn" | "stepOut") {
                    session.forget_values();
                    session.state = State::Running;
                    return Change::Resumed;
                }
                Change::Nothing
            }
        }
    }

    /// The adapter said no. What that means depends on what was asked, and
    /// two of them are worth more than a line in the status bar.
    fn refused(&mut self, ask: Ask, why: String) -> Change {
        let Some(session) = &mut self.session else {
            return Change::Nothing;
        };
        match ask {
            // The one failure that ends the session. If the program could not
            // be started there is nothing to debug, and an adapter sitting
            // there with no program is worse than no adapter — it looks like
            // a debugger that is about to do something.
            Ask::Initialize | Ask::Start => {
                session.say(why.clone());
                session.state = State::Ended(why);
                session.blame();
                Change::Ended
            }
            // A step refused after the program has already gone is ordinary
            // and not worth a word. One refused while it is stopped is worth
            // saying, because otherwise it looks like a key that did nothing.
            Ask::Control { .. } if session.state.is_over() => Change::Nothing,
            // A question about where the program was standing, answered after
            // it has moved on. Every adapter refuses these — "unable to find
            // thread to evaluate variable reference" — and every one of them
            // is a race we started and no longer care about: the reply is
            // about a moment that has passed, and putting it in front of
            // somebody as though their program had said it is worse than
            // silence.
            Ask::Stack | Ask::Scopes | Ask::Variables { .. } | Ask::Threads
                if !session.state.is_stopped() =>
            {
                Change::Nothing
            }
            _ => {
                session.say(why);
                Change::Nothing
            }
        }
    }

    /// Tell the adapter where the breakpoints are.
    ///
    /// Sent per file and always in full: the protocol has no "add one", only
    /// "here is every breakpoint in this file", which is the right shape for
    /// an editor anyway — what is in the buffer is the truth and the adapter
    /// is brought up to it.
    ///
    /// A file with none is still sent, once, because that is how a breakpoint
    /// is taken *away*.
    pub fn send_breakpoints(&mut self, breakpoints: &[(PathBuf, Vec<usize>)]) {
        let Some(session) = &mut self.session else {
            return;
        };
        if !session.configured || session.state.is_over() {
            return;
        }
        // Files that had some and now have none still need saying so, which
        // is what makes this the union rather than just what was handed in.
        let gone: Vec<PathBuf> = session
            .verified
            .keys()
            .filter(|path| !breakpoints.iter().any(|(had, _)| had == *path))
            .cloned()
            .collect();
        let all = breakpoints
            .iter()
            .map(|(path, lines)| (path.clone(), lines.clone()))
            .chain(gone.into_iter().map(|path| (path, Vec::new())));
        for (path, lines) in all {
            let wanted: Vec<Value> = lines
                .iter()
                .map(|line| json!({ "line": line + 1 }))
                .collect();
            session.request(
                "setBreakpoints",
                json!({
                    "source": { "path": path.to_string_lossy(), "name": path
                        .file_name().map(|n| n.to_string_lossy().into_owned()) },
                    "breakpoints": wanted,
                    "sourceModified": false,
                }),
                Ask::Breakpoints { path },
            );
        }
    }

    /// And then say the configuration is finished, which is what lets the
    /// program start. Sent once, after the breakpoints, and only where the
    /// adapter said it wanted it.
    pub fn configuration_done(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.can("supportsConfigurationDoneRequest") {
            session.notify("configurationDone", json!({}));
        }
    }

    /// Let it go again.
    pub fn resume(&mut self) {
        self.control("continue", json!({ "threadId": self.thread() }));
    }

    /// Over the next line, into the call on it, or out of the function.
    pub fn step(&mut self, what: Step) {
        let thread = self.thread();
        self.control(what.command(), json!({ "threadId": thread }));
    }

    /// Stop the program where it is.
    pub fn pause(&mut self) {
        self.control("pause", json!({ "threadId": self.thread() }));
    }

    /// Ask what an expression comes to, at the frame being looked at.
    pub fn evaluate(&mut self, what: &str) {
        let Some(session) = &mut self.session else {
            return;
        };
        let frame = session.frame;
        session.request(
            "evaluate",
            json!({ "expression": what, "frameId": frame, "context": "repl" }),
            Ask::Evaluate {
                what: what.to_string(),
            },
        );
    }

    /// Look at a different frame: its variables become the ones shown.
    pub fn select_frame(&mut self, id: i64) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.frame == Some(id) || !session.frames.iter().any(|f| f.id == id) {
            return;
        }
        session.frame = Some(id);
        session.scopes.clear();
        session.values.clear();
        session.request("scopes", json!({ "frameId": id }), Ask::Scopes);
    }

    /// Open a structured value up, or close it again.
    pub fn toggle_value(&mut self, reference: i64) {
        let Some(session) = &mut self.session else {
            return;
        };
        if reference == 0 {
            return;
        }
        if !session.open.insert(reference) {
            session.open.remove(&reference);
            return;
        }
        if !session.values.contains_key(&reference) {
            session.request(
                "variables",
                json!({ "variablesReference": reference }),
                Ask::Variables { reference },
            );
        }
    }

    /// The thread everything is asked about: the one that stopped, or the
    /// first there is for an adapter that never said.
    fn thread(&self) -> i64 {
        self.session
            .as_ref()
            .and_then(|s| s.thread.or_else(|| s.threads.first().map(|t| t.id)))
            .unwrap_or(1)
    }

    fn control(&mut self, what: &'static str, arguments: Value) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.state.is_over() {
            return;
        }
        session.request(what, arguments, Ask::Control { what });
    }

    /// End it, and take the program with it.
    ///
    /// Called when somebody asks, when a new session starts, and on the way
    /// out of the editor. A program left suspended under a debugger nobody is
    /// talking to any more is a process that never finishes.
    pub fn stop(&mut self) {
        if let Some(mut session) = self.session.take() {
            // A program we started goes with the debugger; one we attached to
            // does not. The session has known which it was since it was made
            // — see [`crate::rpc::Peer::disconnect`], which is where the
            // reason is written down.
            // A program we started goes with the debugger; one we attached to
            // does not. The session has known which it was since it was made
            // — see [`crate::rpc::Peer::disconnect`], which is where the
            // reason is written down.
            let ours = session.start.0 == "launch";
            session.peer.disconnect(ours);
        }
    }
}

/// Which way to step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// Over the next line, calls and all.
    Over,
    /// Into the call on it.
    Into,
    /// Out of this function, back to whoever called it.
    Out,
}

impl Step {
    fn command(&self) -> &'static str {
        match self {
            Step::Over => "next",
            Step::Into => "stepIn",
            Step::Out => "stepOut",
        }
    }
}

/// A stack as the protocol writes it, in the editor's own counting.
fn read_frames(value: &Value) -> Vec<Frame> {
    value
        .get("stackFrames")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|f| {
                    Some(Frame {
                        id: f.get("id")?.as_i64()?,
                        name: f
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("frame")
                            .to_string(),
                        // A frame with no file is one inside a library the
                        // adapter has no source for. Worth showing, and not
                        // worth trying to open.
                        path: f
                            .get("source")
                            .and_then(|s| s.get("path"))
                            .and_then(Value::as_str)
                            .filter(|p| !p.is_empty())
                            .map(PathBuf::from),
                        line: f
                            .get("line")
                            .and_then(Value::as_i64)
                            .unwrap_or(1)
                            .max(1) as usize
                            - 1,
                        column: f
                            .get("column")
                            .and_then(Value::as_i64)
                            .unwrap_or(1)
                            .max(1) as usize
                            - 1,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The variables in one scope or one value.
fn read_variables(value: &Value) -> Vec<Variable> {
    value
        .get("variables")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| {
                    Some(Variable {
                        name: v.get("name")?.as_str()?.to_string(),
                        value: shorten(
                            v.get("value").and_then(Value::as_str).unwrap_or(""),
                        ),
                        kind: v
                            .get("type")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                            .map(str::to_string),
                        reference: v
                            .get("variablesReference")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Where the interpreter textfold used came from, in words.
///
/// The environment picker's own words, so that "the environment you are in"
/// here means the same thing it means there. `PATH` for a project with no
/// environment at all, which is the ordinary case for a script and the case
/// where somebody is most likely to be surprised by which `python3` ran.
///
/// `None` for an adapter that never asked for an interpreter — see
/// [`uses_an_environment`], which is what decides whether this is a fact
/// about the run or a fact about somebody else's language.
fn where_from(config: &lang::Debugger, root: &Path, environment: Option<&Path>) -> Option<String> {
    if !uses_an_environment(config) {
        return None;
    }
    Some(match crate::venv::chosen(root, environment) {
        Some(env) => format!("{} — {}", env.about, env.root.display()),
        None => "the PATH — this project has no Python environment".to_string(),
    })
}

/// Whether an adapter is run out of a Python environment at all.
///
/// Asked of the manifest as it was written, before the `${…}` were filled in,
/// because that is the only place the question is still visible: once
/// `${python}` has become `/usr/bin/python3` it looks exactly like an adapter
/// that was always going to be `/usr/bin/python3`.
///
/// This is not a list of which languages are Python. It is the same test the
/// substitution itself uses — a manifest that asks for an environment gets one
/// and is told where it came from, and one that does not is never told about
/// environments at all. A plugin for some language nobody here has thought of
/// that runs its adapter out of a virtual environment gets the same answer for
/// free, and `gdb` gets silence.
fn uses_an_environment(config: &lang::Debugger) -> bool {
    const ASKED: [&str; 3] = ["${python}", "${venv}", "${venv_bin}"];
    let mentions = |text: &str| ASKED.iter().any(|name| text.contains(name));
    mentions(&config.command)
        || config.args.iter().any(|arg| mentions(arg))
        || config.env.values().any(|value| mentions(value))
        || mentions(&config.launch.to_string())
}

/// How much of a value is worth showing on its own row.
///
/// Python's `__builtins__` is two and a half thousand characters of `repr`,
/// and it is *always there*, on every frame, at the top of the globals. Drawn
/// in full in a panel fourteen rows tall it is the panel — twenty rows of
/// dictionary for one variable nobody asked about, with whatever you were
/// actually looking at pushed off the bottom.
const VALUE_SHOWN: usize = 160;

/// One value, on one row.
///
/// Two things happen to it, and neither loses anything you cannot get back:
/// newlines become spaces, because a panel is a list and a value with a
/// newline in it would put the next variable's name in the middle of this
/// one's; and it is cut to a length somebody can read. What was cut is a
/// keystroke away — anything with a `▸` opens up, and `debug-evaluate` will
/// print the whole of it.
fn shorten(value: &str) -> String {
    let flat = value.replace('\n', " ");
    crate::text::truncate(&flat, VALUE_SHOWN)
}

/// The package name in the `Cargo.toml` at the root of the project, which is
/// what the binary it builds is called.
///
/// A hand-rolled read of two lines rather than a TOML parser, and deliberately
/// a shy one: it looks only for a `name` under `[package]`, stops at the next
/// section, and answers `None` for anything it is not sure about. A wrong name
/// here is a debugger opening a file that is not there, which says so; a TOML
/// dependency for one field would be a dependency for one field.
///
/// A virtual workspace has no `[package]` at all, and gets `None` — the
/// members are separate binaries and there is no one answer.
fn package_name(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "name" {
            continue;
        }
        // `name = "thing"  # a comment`, and the quotes are the only thing
        // that says where the value stops.
        let value = value.trim();
        let quoted = value.strip_prefix(['"', '\''])?;
        let end = quoted.find(['"', '\''])?;
        return Some(quoted[..end].to_string()).filter(|name| !name.is_empty());
    }
    None
}

/// The class `file` declares, qualified by the package it is in.
///
/// Java requires a public class to be named after its file, so the name is the
/// file's, and the only question is what comes before it. A file with no
/// `package` line is in the default package and the answer is the bare name —
/// the same string `${file_base}` gives.
///
/// `None` only where the file cannot be read or has no name at all, which
/// leaves the placeholder unfilled rather than filled in wrongly.
fn declared_class(file: &Path) -> Option<String> {
    let base = file.file_stem()?.to_string_lossy().into_owned();
    // Reading the file rather than the buffer means an unsaved edit to the
    // `package` line is not seen. That line is written once when the file is
    // made and then left alone, and the alternative is threading the open
    // document all the way down here for it.
    let text = std::fs::read_to_string(file).ok()?;
    Some(match package_of(&text) {
        Some(package) => format!("{package}.{base}"),
        None => base,
    })
}

/// The name on the `package` line, if there is one.
fn package_of(text: &str) -> Option<String> {
    let code = without_comments(text);
    // `package` is the first statement in a file that has one, and it comes
    // before any type — so the first statement of what is left before the
    // first `{` either is the package declaration or there is not one.
    let first = code.split('{').next()?.split(';').next()?.trim();
    let rest = first.strip_prefix("package")?;
    // `packagefoo` is an identifier, not a declaration.
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let name = rest.trim();
    let named = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '$');
    (!name.is_empty() && name.chars().all(named)).then(|| name.to_string())
}

/// `text` with its comments taken out, so that a `package` line inside one is
/// not mistaken for the real thing. Newlines are kept so nothing runs together.
fn without_comments(text: &str) -> String {
    let mut code = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let (mut in_block, mut in_line) = (false, false);
    while let Some(c) = chars.next() {
        if in_block {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block = false;
            } else if c == '\n' {
                code.push('\n');
            }
            continue;
        }
        if in_line {
            if c == '\n' {
                in_line = false;
                code.push('\n');
            }
            continue;
        }
        if c == '/' {
            match chars.peek() {
                Some('*') => {
                    chars.next();
                    in_block = true;
                    continue;
                }
                Some('/') => {
                    chars.next();
                    in_line = true;
                    continue;
                }
                _ => {}
            }
        }
        code.push(c);
    }
    code
}

/// An adapter's configuration with its `${…}` filled in.
///
/// The same substitution a language server's settings get — see
/// [`crate::venv::Vars`] — plus `${file}`, which is the one thing a debugger
/// needs that a server does not: *which program*. A server is told about a
/// project and works out the rest; a debugger is told to run one thing.
pub fn filled(
    config: &lang::Debugger,
    root: &Path,
    file: &Path,
    environment: Option<&Path>,
) -> lang::Debugger {
    let env = crate::venv::chosen(root, environment);
    let mut vars = crate::venv::Vars::new(root, env.as_ref());
    // `${python}` with no virtual environment anywhere would otherwise be a
    // hole, and a hole in the *command* is a program that cannot be run at
    // all. A project with no environment is the ordinary case for a script,
    // and the ordinary answer is the interpreter on the `PATH`.
    if env.is_none() {
        vars.set("python", "python3".into());
    }
    vars.set("file", file.display().to_string());
    if let Some(dir) = file.parent() {
        vars.set("file_dir", dir.display().to_string());
    }
    // The file with its extension taken off, which for a compiled language is
    // the whole question. You cannot debug `main.c`; you debug what came out
    // of compiling it, and `cc -g -o main main.c` is what everybody's first
    // one is called.
    vars.set(
        "file_stem",
        file.with_extension("").display().to_string(),
    );
    // And its own name with the extension off, which is what a Java class is
    // called: `src/Main.java` is the class `Main`.
    if let Some(base) = file.file_stem() {
        vars.set("file_base", base.to_string_lossy().into_owned());
    }
    // The same name with its package in front of it. A JVM is told which class
    // to run by its *qualified* name, and `Main` and `com.example.Main` are
    // different answers: the short one only matches a class in the default
    // package, which almost nothing outside a tutorial is in.
    if let Some(class) = declared_class(file) {
        vars.set("file_class", class);
    }
    // What Cargo will have called the binary. `${file_stem}` is the answer for
    // a language whose compiler writes its output beside the source; for one
    // with a build system it is not even close — you do not debug
    // `src/main.rs`, you debug `target/debug/whatever-the-package-is-called`,
    // and the only place that name is written down is `Cargo.toml`.
    if let Some(name) = package_name(root) {
        vars.set("crate", name);
    }
    // And as a language server writes a path, for a question that is going to
    // one — see [`crate::lang::Resolve`].
    vars.set("file_uri", crate::lsp::uri_of(file));
    lang::Debugger {
        id: config.id.clone(),
        name: config.name.clone(),
        // A command that cannot be filled in is left as it was written rather
        // than dropped. `${python}` with no environment anywhere should fall
        // back to running `python3` off the `PATH`, which is what most people
        // have and what the manifest says to do about it.
        command: vars.fill(&config.command).unwrap_or_else(|| config.command.clone()),
        args: config
            .args
            .iter()
            .map(|a| vars.fill(a).unwrap_or_else(|| a.clone()))
            .collect(),
        roots: config.roots.clone(),
        launch: vars.fill_value(&config.launch).unwrap_or_else(|| config.launch.clone()),
        // Carried through as written. What reaches the adapter is a copy of
        // this with a picked process in it, put where `launch` goes — see
        // [`about_process`], which is the whole of the difference between the
        // two ways of starting one.
        attach: config
            .attach
            .as_ref()
            .map(|attach| vars.fill_value(attach).unwrap_or_else(|| attach.clone())),
        from_server: config.from_server.as_ref().map(|from| lang::FromServer {
            server: from.server.clone(),
            start: from.start.clone(),
            resolve: from.resolve.as_ref().map(|resolve| lang::Resolve {
                command: resolve.command.clone(),
                arguments: resolve
                    .arguments
                    .iter()
                    .map(|a| vars.fill_value(a).unwrap_or_else(|| a.clone()))
                    .collect(),
                into: resolve.into.clone(),
            }),
        }),
        see: config.see.clone(),
        env: config
            .env
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), vars.fill(v)?)))
            .collect(),
    }
}

/// The attach request for one running process: the manifest's `attach` object
/// with `${pid}` and `${program}` filled in.
///
/// A substitution of its own rather than another entry in [`crate::venv::Vars`]
/// because of what a `pid` *is*. Everything else a manifest asks for is a path
/// or a name, and a string is the right shape for all of them; a process id is
/// a number, and every adapter there is refuses `"pid": "4127"` on the grounds
/// that a string is not a number. So a value that is nothing but `${pid}`
/// becomes a JSON number, and a `${pid}` inside a longer string —
/// `"--pid=${pid}"`, which is how some adapters want it — becomes the digits.
///
/// `${program}` is the file the process is running. Where the machine will not
/// say what that is, the field asking for it is dropped rather than left with
/// a hole in it: an adapter handed `"program": ""` goes looking for a file
/// called nothing, where one handed no `program` at all works it out from the
/// pid it was given.
/// Whether an attach request needs a process picked for it.
///
/// The two shapes of attaching, and the difference is what the person has to
/// do. An adapter that attaches to a *process* — `gdb`, `lldb` — needs one
/// chosen, because a pid means nothing until somebody points at it. An adapter
/// that attaches to a *port* — `debugpy`, `dlv`, the Java one — needs nothing
/// chosen at all: the port is written down in the settings, the program is
/// waiting on it, and there is exactly one thing to connect to.
///
/// Asking is what keeps the second kind from being made to answer a question
/// with one right answer. A list of a hundred and fifty processes, none of
/// which matters, is not a choice; it is a form to get past.
/// Whether an attach request wants an address filled in — a host and a port
/// somebody has to say.
///
/// The manifest decides whether you are asked, by writing a placeholder or a
/// number. `"port": 5005` is a project that always debugs on 5005 and should
/// never be questioned about it; `"port": "${port}"` is one where the answer
/// changes, and the question is a prompt with the last answer already in it.
pub fn needs_an_address(attach: &Value) -> bool {
    let text = attach.to_string();
    text.contains("${host") || text.contains("${port")
}

/// The address a manifest suggests, for the box to open with.
///
/// `${port:5678}` — a placeholder with a default after a colon, the way a
/// shell writes one. It is worth the two characters of syntax because the
/// conventional port is a fact about the *adapter*: JDWP is 5005 and debugpy
/// is 5678, and a first guess that is wrong for one of them is an edit every
/// person using it makes once, forever.
///
/// Only a first guess. What is actually attached to is what comes back from
/// the box, and after the first time that is whatever was said last.
pub fn suggested_address(attach: &Value) -> Option<(String, u16)> {
    let text = attach.to_string();
    let default_of = |name: &str| -> Option<&str> {
        let at = text.find(&format!("${{{name}:"))?;
        let rest = &text[at + name.len() + 3..];
        Some(&rest[..rest.find('}')?])
    };
    let port = default_of("port")?.parse().ok()?;
    Some((default_of("host").unwrap_or("127.0.0.1").to_string(), port))
}

/// The attach request with an address in it.
///
/// `${port}` becomes a number for the same reason `${pid}` does: every adapter
/// refuses a port that arrives as a string. See [`about_process`].
pub fn at_address(attach: &Value, host: &str, port: u16) -> Value {
    let mut out = attach.clone();
    fill_address(&mut out, host, port);
    out
}

fn fill_address(value: &mut Value, host: &str, port: u16) {
    match value {
        Value::Object(fields) => {
            for value in fields.values_mut() {
                fill_address(value, host, port);
            }
        }
        Value::Array(items) => {
            for value in items.iter_mut() {
                fill_address(value, host, port);
            }
        }
        Value::String(text) => {
            // Whatever default the manifest wrote goes with the placeholder:
            // it was the suggestion for the box, and the box has answered.
            let filled = without_defaults(text)
                .replace("${host}", host)
                .replace("${port}", &port.to_string());
            match text.trim().starts_with("${port") && filled.trim() == port.to_string() {
                true => *value = json!(port),
                false => *text = filled,
            }
        }
        _ => {}
    }
}

/// `${port:5678}` written back as `${port}`, so that one substitution handles
/// a placeholder whether or not it came with a suggestion.
fn without_defaults(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at..].find('}').map(|n| at + n) else {
            break;
        };
        let name = &rest[at + 2..end];
        let name = name.split_once(':').map_or(name, |(name, _)| name);
        out.push_str(&format!("${{{name}}}"));
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// An address as somebody writes one, as a host and a port.
///
/// `5005` on its own is a port on this machine, because that is what everybody
/// means by it and typing `127.0.0.1:` first is a toll. A bare host is not
/// accepted: a debugger with no port is not a debugger that will connect, and
/// guessing one would be guessing which of two programs to talk to.
pub fn read_address(said: &str) -> Option<(String, u16)> {
    let said = said.trim();
    if said.is_empty() {
        return None;
    }
    let (host, port) = match said.rsplit_once(':') {
        // `[::1]:5005`, whose colons are its own.
        Some((host, port)) => (host.trim().trim_matches(['[', ']']), port),
        None => ("127.0.0.1", said),
    };
    let port: u16 = port.trim().parse().ok()?;
    let host = match host.is_empty() {
        true => "127.0.0.1",
        false => host,
    };
    Some((host.to_string(), port))
}

pub fn needs_a_process(attach: &Value) -> bool {
    let text = attach.to_string();
    text.contains("${pid}") || text.contains("${program}")
}

pub fn about_process(attach: &Value, pid: u32, program: Option<&Path>) -> Value {
    let program = program.map(|path| path.display().to_string());
    let mut out = attach.clone();
    fill_process(&mut out, pid, program.as_deref());
    out
}

fn fill_process(value: &mut Value, pid: u32, program: Option<&str>) {
    match value {
        Value::Object(fields) => {
            fields.retain(|_, value| match value.as_str() {
                Some(text) => text.trim() != "${program}" || program.is_some(),
                None => true,
            });
            for value in fields.values_mut() {
                fill_process(value, pid, program);
            }
        }
        Value::Array(items) => {
            for value in items.iter_mut() {
                fill_process(value, pid, program);
            }
        }
        Value::String(text) => {
            if text.trim() == "${pid}" {
                *value = json!(pid);
                return;
            }
            *text = text
                .replace("${pid}", &pid.to_string())
                .replace("${program}", program.unwrap_or_default());
        }
        _ => {}
    }
}

/// Put a language server's answer into the launch arguments, by the names the
/// manifest gave — see [`crate::lang::Resolve`].
///
/// Here rather than in the editor because it is about what an adapter is
/// launched with, and because it is the sort of small mapping that is easy to
/// get subtly wrong and worth a test of its own.
pub fn fold_into_launch(launch: &mut Value, resolve: &lang::Resolve, answer: &Value) {
    let Some(launch) = launch.as_object_mut() else {
        return;
    };
    for (into, from) in &resolve.into {
        // A field the server did not answer with is left alone rather than
        // set to null: an adapter handed `"classPaths": null` refuses the
        // launch over it, where one handed nothing may well work it out.
        if let Some(value) = answer.get(from) {
            launch.insert(into.clone(), value.clone());
        }
    }
}

/// Fold one capabilities object into another, key by key. An adapter may send
/// a `capabilities` event later saying it can do more than it first said.
fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                base.insert(key.clone(), value.clone());
            }
        }
        (base, over) => *base = over.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `${file_class}` is what a JVM is told to run, and a project with
    /// packages in it is the ordinary case rather than the exception.
    #[test]
    fn a_class_is_qualified_by_the_package_it_declares() {
        assert_eq!(package_of("package com.example.calc;\n"), Some("com.example.calc".into()));
        assert_eq!(
            package_of("package com.example;\n\nimport java.util.List;\n\nclass Main {}\n"),
            Some("com.example".into()),
        );
    }

    #[test]
    fn a_file_with_no_package_line_is_in_the_default_package() {
        // The bare name is right here, and is what `${file_base}` also gives.
        assert_eq!(package_of("class Main {}\n"), None);
        assert_eq!(package_of("import java.util.List;\n\nclass Main {}\n"), None);
        assert_eq!(package_of(""), None);
    }

    /// A licence header with the word `package` in it is the reason comments
    /// are taken out before the first statement is looked for.
    #[test]
    fn the_word_package_in_a_comment_is_not_a_package_declaration() {
        assert_eq!(package_of("// package com.wrong;\nclass Main {}\n"), None);
        assert_eq!(package_of("/* package com.wrong; */\nclass Main {}\n"), None);
        assert_eq!(
            package_of("/*\n * package com.wrong;\n */\npackage com.right;\nclass Main {}\n"),
            Some("com.right".into()),
        );
    }

    #[test]
    fn an_identifier_beginning_with_package_is_not_a_declaration() {
        assert_eq!(package_of("packagefoo.Bar x;\n"), None);
    }

    #[test]
    fn a_class_is_the_file_name_with_the_package_in_front_of_it() {
        let root = std::env::temp_dir().join(format!("textfold-class-{}", std::process::id()));
        let dir = root.join("com").join("example");
        std::fs::create_dir_all(&dir).expect("made");

        let packaged = dir.join("Main.java");
        std::fs::write(&packaged, "package com.example;\n\nclass Main {}\n").expect("written");
        assert_eq!(declared_class(&packaged), Some("com.example.Main".into()));

        let bare = root.join("Scratch.java");
        std::fs::write(&bare, "class Scratch {}\n").expect("written");
        assert_eq!(declared_class(&bare), Some("Scratch".into()));

        // A file that is not there fills nothing in, rather than filling in a
        // name that would not be found.
        assert_eq!(declared_class(&root.join("Missing.java")), None);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A session with nothing behind it but a process that will sit there.
    ///
    /// `Session` holds a live peer, so there is no making one out of thin air
    /// — and there should not be: a session with no process is a state this
    /// module never has. `cat` reading a pipe is the cheapest real one there
    /// is, and it goes when the peer is dropped.
    #[cfg(unix)]
    fn a_session() -> Session {
        let (tx, _rx) = std::sync::mpsc::channel();
        let env = BTreeMap::new();
        let peer = Peer::start(
            rpc::Spawn {
                command: "cat",
                args: &[],
                root: Path::new("."),
                env: &env,
                label: "test",
                dialect: rpc::Dialect::Dap,
            },
            tx,
            |incoming| crate::app::Event::Dap(SessionId(1), incoming),
        )
        .expect("cat is on every unix there is");
        Session {
            id: SessionId(1),
            name: "test".into(),
            what: "main.py".into(),
            peer,
            state: State::Stopped("breakpoint".into()),
            caps: Value::Null,
            start: ("launch", json!({})),
            configured: true,
            answered: true,
            see: None,
            adapter: "test/quiet".into(),
            from: Some("a test".into()),
            ran: "cat".into(),
            threads: Vec::new(),
            thread: None,
            frames: Vec::new(),
            frame: None,
            scopes: Vec::new(),
            values: BTreeMap::new(),
            open: HashSet::new(),
            output: Vec::new(),
            verified: BTreeMap::new(),
        }
    }

    fn a_stack() -> Value {
        json!({ "stackFrames": [
            { "id": 2, "name": "fizz", "line": 4, "column": 1,
              "source": { "path": "/p/main.py" } },
            { "id": 3, "name": "main", "line": 9, "column": 1,
              "source": { "path": "/p/main.py" } },
            { "id": 4, "name": "<module>", "line": 13, "column": 1 },
        ]})
    }

    #[test]
    fn a_stack_is_read_in_the_editors_own_counting() {
        // The protocol counts lines from one and textfold counts from zero.
        // Getting this wrong is an arrow one line below where the program
        // actually is, which is the kind of bug that costs somebody an hour
        // of believing their program.
        let frames = read_frames(&a_stack());
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].name, "fizz");
        assert_eq!(frames[0].line, 3);
        assert_eq!(frames[0].column, 0);
        assert_eq!(frames[0].path.as_deref(), Some(Path::new("/p/main.py")));
    }

    #[test]
    fn a_frame_with_no_file_is_still_a_frame() {
        // Inside the interpreter, or inside a library with no source. Worth
        // showing — it is often why you are looking — and not worth trying to
        // open.
        let frames = read_frames(&a_stack());
        assert_eq!(frames[2].name, "<module>");
        assert_eq!(frames[2].path, None);
    }

    #[test]
    fn a_frame_with_a_line_of_zero_does_not_go_round_the_houses() {
        // An adapter that says line 0 in a protocol that counts from 1 is an
        // adapter with a bug, and the answer is line 0 rather than a line
        // number of eighteen quintillion.
        let frames = read_frames(&json!({ "stackFrames": [
            { "id": 1, "name": "x", "line": 0, "column": 0 }
        ]}));
        assert_eq!(frames[0].line, 0);
        assert_eq!(frames[0].column, 0);
    }

    #[test]
    fn a_value_that_can_be_opened_says_so() {
        let read = read_variables(&json!({ "variables": [
            { "name": "n", "value": "5", "type": "int", "variablesReference": 0 },
            { "name": "xs", "value": "[1, 2]", "type": "list", "variablesReference": 7 },
        ]}));
        assert_eq!(read[0].reference, 0, "a number has nothing inside it");
        assert_eq!(read[1].reference, 7);
        assert_eq!(read[1].kind.as_deref(), Some("list"));
    }

    #[test]
    fn a_value_too_long_to_read_is_cut_to_a_length_somebody_can() {
        // `__builtins__` is two and a half thousand characters of `repr` and
        // is on every frame there is. In full it is not a row of a panel, it
        // is the panel.
        let huge = "x".repeat(4000);
        let read = read_variables(&json!({ "variables": [
            { "name": "__builtins__", "value": huge, "variablesReference": 7 }
        ]}));
        assert!(
            read[0].value.chars().count() <= VALUE_SHOWN,
            "{} characters",
            read[0].value.chars().count()
        );
        // And it says it was cut rather than pretending that is the value.
        assert!(read[0].value.ends_with('…'), "{:?}", read[0].value);
        // What was cut is still reachable: it has something inside it, so the
        // panel offers to open it.
        assert_eq!(read[0].reference, 7);
    }

    #[test]
    fn a_value_short_enough_to_read_is_left_exactly_as_it_is() {
        let read = read_variables(&json!({ "variables": [
            { "name": "n", "value": "5", "variablesReference": 0 }
        ]}));
        assert_eq!(read[0].value, "5");
    }

    #[test]
    fn a_value_with_newlines_in_it_stays_one_line() {
        // A panel is a list of things, one per line. A repr with a newline in
        // it would put the next variable's name in the middle of this one's
        // value and every colour after it one row out.
        let read = read_variables(&json!({ "variables": [
            { "name": "s", "value": "one\ntwo", "variablesReference": 0 }
        ]}));
        assert_eq!(read[0].value, "one two");
    }

    #[test]
    #[cfg(unix)]
    fn a_program_we_only_attached_to_is_not_killed_when_we_leave() {
        // The bug this is about: pressing Shift-F5 on an attached session
        // killed the program. A program textfold launched is textfold's and
        // goes with the debugger. A program textfold attached to was somebody
        // else's before we arrived — a server, a game, a simulation four hours
        // in — and stopping the debugger is not a request to stop *that*.
        // There is no undo for it, and no warning either: it read as "the
        // program finished".
        if !crate::pack::on_path("gdb") || !crate::pack::on_path("cc") {
            return;
        }
        let root = a_project("attached");
        let source = root.join("loop.c");
        std::fs::write(
            &source,
            "#include <unistd.h>\nint main(void){for(;;)sleep(1);return 0;}\n",
        )
        .expect("written");
        let program = root.join("loop");
        let built = std::process::Command::new("cc")
            .args(["-g", "-O0", "-o"])
            .arg(&program)
            .arg(&source)
            .output();
        if !built.is_ok_and(|out| out.status.success()) {
            std::fs::remove_dir_all(&root).ok();
            return;
        }
        let mut running = std::process::Command::new(&program)
            .spawn()
            .expect("the program to attach to");

        let config = lang::Debugger {
            id: "c/gdb".into(),
            name: "gdb".into(),
            command: "gdb".into(),
            args: vec!["--interpreter=dap".into()],
            roots: vec![".git".into()],
            launch: json!({
                "request": "attach",
                "program": program.display().to_string(),
                "pid": running.id(),
            }),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        debug.start(&config, &root, &source, None).expect("gdb started");
        let attached = run_until(&mut debug, &rx, &[], 40, |d| {
            d.session().is_some_and(|s| s.state.is_stopped())
        });
        assert!(attached, "it never attached: {:?}", state_of(&debug));

        debug.stop();
        // Given a moment to be killed, if it were going to be. `try_wait`
        // answering `None` is the process still being there, which is the
        // whole of what this test is about.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let gone = running.try_wait().expect("asking after it");
        let alive = gone.is_none();
        running.kill().ok();
        running.wait().ok();
        std::fs::remove_dir_all(&root).ok();
        assert!(alive, "stopping the debugger killed a program it did not start");
    }

    #[test]
    fn what_cargo_calls_the_binary_is_read_out_of_the_manifest() {
        // `${file_stem}` is the answer for a compiler that writes its output
        // beside the source. For one with a build system it is not close: you
        // do not debug `src/main.rs`, you debug `target/debug/wordcount`, and
        // the only place that name is written down is `Cargo.toml`.
        let root = a_project("cargo");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"word-count\"  # what it is called\nversion = \"0.1.0\"\n\n\
             [dependencies]\nname = \"not this one\"\n",
        )
        .expect("written");
        assert_eq!(package_name(&root).as_deref(), Some("word-count"));

        // A virtual workspace has no `[package]`, and there is no one answer
        // — its members are separate binaries.
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        )
        .expect("written");
        assert_eq!(package_name(&root), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_address_is_read_the_way_somebody_would_write_one() {
        // A bare number is a port on this machine, because that is what
        // everybody means by it and typing `127.0.0.1:` first is a toll.
        assert_eq!(read_address("5005"), Some(("127.0.0.1".into(), 5005)));
        assert_eq!(read_address("  5005 "), Some(("127.0.0.1".into(), 5005)));
        assert_eq!(read_address("10.0.0.2:5005"), Some(("10.0.0.2".into(), 5005)));
        assert_eq!(read_address("box.local:9"), Some(("box.local".into(), 9)));
        // An address whose own colons are its own.
        assert_eq!(read_address("[::1]:5005"), Some(("::1".into(), 5005)));
        // And a host with no port is refused rather than guessed at: a
        // debugger with no port will not connect, and picking one would be
        // picking which of two programs to talk to.
        assert_eq!(read_address("localhost"), None);
        assert_eq!(read_address(""), None);
        assert_eq!(read_address("5005 or so"), None);
        assert_eq!(read_address("70000"), None, "not a port at all");
    }

    #[test]
    fn an_address_reaches_an_adapter_as_a_host_and_a_number() {
        let attach = json!({
            "request": "attach",
            "hostName": "${host}",
            "port": "${port}",
            "note": "waiting on ${host}:${port}",
        });
        let filled = at_address(&attach, "10.0.0.2", 5099);
        assert_eq!(filled["hostName"], json!("10.0.0.2"));
        // A port is a number for the same reason a pid is.
        assert_eq!(filled["port"], json!(5099));
        assert!(filled["port"].is_number(), "{filled}");
        assert_eq!(filled["note"], json!("waiting on 10.0.0.2:5099"));
    }

    #[test]
    fn a_manifest_decides_whether_it_is_asked_where_to_attach() {
        // A project that always debugs on one port should never be questioned
        // about it, and one where the answer changes should be asked. The
        // difference is a number or a placeholder, which is the manifest's to
        // write.
        assert!(needs_an_address(&json!({ "port": "${port}" })));
        assert!(needs_an_address(&json!({ "listen": { "host": "${host}" } })));
        assert!(!needs_an_address(&json!({ "port": 5005 })));
        // And neither kind of address is a process to pick out of a list.
        assert!(!needs_a_process(&json!({ "port": "${port}" })));
        assert!(needs_a_process(&json!({ "pid": "${pid}" })));
    }

    #[test]
    fn a_process_id_reaches_an_adapter_as_a_number() {
        // Every other placeholder in a manifest stands for a path or a name,
        // and a string is the right shape for all of them. A process id is a
        // number, and an adapter handed `"pid": "4127"` refuses it on the
        // grounds that a string is not a number — which arrives as a launch
        // that failed for no visible reason.
        let attach = json!({ "request": "attach", "program": "${program}", "pid": "${pid}" });
        let filled = about_process(&attach, 4127, Some(Path::new("/usr/bin/thing")));
        assert_eq!(filled["pid"], json!(4127));
        assert!(filled["pid"].is_number(), "{filled}");
        assert_eq!(filled["program"], json!("/usr/bin/thing"));
        assert_eq!(filled["request"], json!("attach"));
    }

    #[test]
    fn a_process_id_inside_a_longer_argument_is_still_the_digits() {
        // Some adapters want it as a flag rather than a field, and a whole
        // string that is not a placeholder cannot become a number.
        let attach = json!({ "args": ["--pid=${pid}", "--exe", "${program}"] });
        let filled = about_process(&attach, 22, Some(Path::new("/bin/ls")));
        assert_eq!(filled["args"], json!(["--pid=22", "--exe", "/bin/ls"]));
    }

    #[test]
    fn a_field_wanting_a_program_we_cannot_name_is_dropped_rather_than_emptied() {
        // The machine does not always say what a process is running. An
        // adapter handed `"program": ""` goes looking for a file called
        // nothing and fails; one handed no `program` at all works it out from
        // the pid it was given, which is the answer we want.
        let attach = json!({ "request": "attach", "program": "${program}", "pid": "${pid}" });
        let filled = about_process(&attach, 7, None);
        assert_eq!(filled.get("program"), None, "{filled}");
        assert_eq!(filled["pid"], json!(7));
    }

    #[test]
    fn a_launch_object_says_which_of_the_two_requests_it_is() {
        let mut config = lang::Debugger {
            id: "python/debugpy".into(),
            name: "debugpy".into(),
            command: "python3".into(),
            args: Vec::new(),
            roots: Vec::new(),
            launch: json!({ "program": "${file}" }),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        assert_eq!(config.request(), "launch", "saying nothing means launch");
        config.launch = json!({ "request": "attach", "port": 5678 });
        assert_eq!(config.request(), "attach");
    }

    #[test]
    fn the_file_being_debugged_is_what_dollar_file_means() {
        let config = lang::Debugger {
            id: "python/debugpy".into(),
            name: "debugpy".into(),
            command: "python3".into(),
            args: vec!["-m".into(), "debugpy.adapter".into()],
            roots: Vec::new(),
            launch: json!({ "program": "${file}", "cwd": "${root}" }),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let filled = filled(&config, Path::new("/p"), Path::new("/p/src/main.py"), None);
        assert_eq!(filled.launch["program"], "/p/src/main.py");
        assert_eq!(filled.launch["cwd"], "/p");
    }

    /// The whole point of `${file_class}`: what jdtls is told to run comes out
    /// qualified, from the file alone, with nothing said in settings.
    #[test]
    fn what_a_jvm_is_told_to_run_is_the_qualified_class() {
        let root = std::env::temp_dir().join(format!("textfold-launch-{}", std::process::id()));
        let dir = root.join("src/main/java/com/example/calc");
        std::fs::create_dir_all(&dir).expect("made");
        let file = dir.join("Main.java");
        std::fs::write(&file, "package com.example.calc;\n\nclass Main {}\n").expect("written");

        let config = lang::Debugger {
            id: "java/java-debug".into(),
            name: "java-debug".into(),
            command: String::new(),
            args: Vec::new(),
            roots: Vec::new(),
            launch: json!({ "mainClass": "${file_class}", "cwd": "${root}" }),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let packaged = filled(&config, &root, &file, None);
        assert_eq!(packaged.launch["mainClass"], "com.example.calc.Main");

        // And a file in the default package is still its own bare name.
        let bare = root.join("Scratch.java");
        std::fs::write(&bare, "class Scratch {}\n").expect("written");
        let default_package = filled(&config, &root, &bare, None);
        assert_eq!(default_package.launch["mainClass"], "Scratch");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nothing_a_command_is_run_with_is_left_as_a_placeholder() {
        // `${python}` in a directory with no virtual environment under it. A
        // language server that cannot be pointed at one is a server that
        // should not start; a debugger is different, because the ordinary
        // Python script in an ordinary directory has no environment and is
        // exactly the thing somebody wants to debug.
        //
        // What it falls back *to* depends on the machine — an activated
        // environment in the shell counts, and this test may well be run
        // inside one — so what is asserted is the thing that is always true:
        // whatever comes out is a program, not a placeholder.
        let config = lang::Debugger {
            id: "python/debugpy".into(),
            name: "debugpy".into(),
            command: "${python}".into(),
            args: Vec::new(),
            roots: Vec::new(),
            launch: json!({}),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let filled = filled(
            &config,
            Path::new("/nowhere-at-all"),
            Path::new("/nowhere-at-all/a.py"),
            None,
        );
        assert!(
            !filled.command.contains("${"),
            "a command with a placeholder still in it is a program that cannot \
             be run at all: {:?}",
            filled.command
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_adapter_that_will_not_start_says_what_it_printed() {
        // The bug this is about: `python3 -m debugpy.adapter` with no
        // `debugpy` installed exits at once having printed exactly what is
        // wrong. That reached the editor as a process that is no longer
        // there, which is the same event as a program that ran to the end —
        // so textfold said "the program stopped" while the answer sat in a
        // log file nobody had been told about. It is the worst kind of wrong
        // answer: it sounds like it worked.
        let root = a_project("wontstart");
        let file = root.join("main.py");
        std::fs::write(&file, "print(1)\n").expect("written");
        let config = lang::Debugger {
            id: "test/nope".into(),
            name: "nope".into(),
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "echo 'No module named nope' >&2; exit 1".into(),
            ],
            roots: vec![".git".into()],
            launch: json!({}),
            env: BTreeMap::new(),
            see: Some("get it from somewhere".into()),
            from_server: None,
            attach: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        debug.start(&config, &root, &file, None).expect("sh started");

        let over = run_until(&mut debug, &rx, &[], 20, |d| {
            d.session().is_some_and(|s| s.state.is_over())
        });
        assert!(over, "it should have noticed the adapter go");
        let session = debug.session().expect("a session");
        assert!(
            !session.ever_started(),
            "it never answered initialize, so it never was a debugger"
        );
        assert_eq!(session.state, State::Ended("would not start".into()));
        assert!(
            session.why_not().is_some_and(|why| why.contains("No module named nope")),
            "the one line worth showing should be what it actually said"
        );
        let said = session.all_printed();
        assert!(said.contains("No module named nope"), "{said:?}");
        // What was actually run — resolved, because `${python}` quietly
        // becoming an interpreter with nothing in it is invisible from
        // anywhere else, and two different `python3`s look identical.
        assert!(said.contains("textfold ran: /"), "not a full path: {said:?}");
        assert!(said.contains("sh -c"), "{said:?}");
        // And *not* where a Python came from. This adapter is `sh`; it never
        // asked for an interpreter, and telling somebody debugging something
        // that is not Python which Python environment they are in is an
        // answer to a question nobody asked.
        assert!(!said.contains("environment"), "{said:?}");
        assert!(!said.contains("from the PATH"), "{said:?}");
        // What the manifest says to do about it.
        assert!(said.contains("get it from somewhere"), "{said:?}");
        // And the way to point it somewhere else.
        assert!(said.contains("\"debuggers\": {\"nope\""), "{said:?}");
        assert!(said.contains("the test plugin"), "{said:?}");

        debug.stop();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn an_adapter_run_out_of_an_environment_says_which_one() {
        // The other half of the rule above. `${python}` in the manifest is
        // the adapter saying it is run by the project's interpreter, and
        // *which* interpreter that turned out to be is the single most useful
        // line in the report — `python3 -m debugpy.adapter` against the wrong
        // environment is most of how this fails.
        let root = a_project("askedforone");
        let file = root.join("main.py");
        std::fs::write(&file, "print(1)\n").expect("written");
        let config = lang::Debugger {
            id: "test/nope".into(),
            name: "nope".into(),
            // Filled in before the adapter is started; what matters here is
            // that the manifest asked, which is what survives into the report.
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "echo 'no debugpy here' >&2; exit 1".into(),
                "${python}".into(),
            ],
            roots: vec![".git".into()],
            launch: json!({}),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        debug.start(&config, &root, &file, None).expect("sh started");
        let over = run_until(&mut debug, &rx, &[], 20, |d| {
            d.session().is_some_and(|s| s.state.is_over())
        });
        assert!(over);
        let said = debug.session().expect("a session").all_printed();
        assert!(said.contains("from the PATH"), "{said:?}");
        debug.stop();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn an_adapter_that_dies_saying_nothing_still_says_that_much() {
        let root = a_project("silent");
        let file = root.join("main.py");
        std::fs::write(&file, "print(1)\n").expect("written");
        let config = lang::Debugger {
            id: "test/quiet".into(),
            name: "quiet".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "exit 3".into()],
            roots: vec![".git".into()],
            launch: json!({}),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        debug.start(&config, &root, &file, None).expect("sh started");
        let over = run_until(&mut debug, &rx, &[], 20, |d| {
            d.session().is_some_and(|s| s.state.is_over())
        });
        assert!(over);
        let said = debug.session().expect("a session").all_printed();
        assert!(said.contains("without saying anything"), "{said:?}");
        assert!(said.contains("sh -c"), "{said:?}");
        debug.stop();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_real_adapter_that_is_not_python_works_the_same_way() {
        // The point of the module, checked rather than asserted: nothing above
        // `rpc` knows what language is being debugged. `gdb` is a good test of
        // that because it does three things differently from `debugpy` and
        // every one of them used to be wrong here — it answers `initialized`
        // before the program is launched rather than after, it cannot verify
        // a breakpoint until the program is loaded and says so in an event
        // later, and it greets its client with fifteen lines of GNU copyright
        // notice under the category the protocol reserves for the debuggee's
        // own output.
        if !crate::pack::on_path("gdb") || !crate::pack::on_path("cc") {
            return;
        }
        let root = a_project("c");
        let source = root.join("main.c");
        let program = [
            "#include <stdio.h>",
            "",
            "int fizz(int n) {",
            "    int total = 0;",
            "    for (int i = 0; i < n; i++) {",
            "        total += i;",
            "    }",
            "    return total;",
            "}",
            "",
            "int main(void) {",
            "    int answer = fizz(5);",
            "    printf(\"answer %d\\n\", answer);",
            "    return 0;",
            "}",
            "",
        ];
        std::fs::write(&source, program.join("\n")).expect("written");
        let built = std::process::Command::new("cc")
            .args(["-g", "-O0", "-o"])
            .arg(root.join("main"))
            .arg(&source)
            .status();
        if !built.is_ok_and(|it| it.success()) {
            // No working compiler is nothing to test with and nothing to fail
            // over.
            std::fs::remove_dir_all(&root).ok();
            return;
        }

        let config = lang::Debugger {
            id: "c/gdb".into(),
            name: "gdb".into(),
            command: "gdb".into(),
            args: vec!["--interpreter=dap".into()],
            roots: vec![".git".into()],
            launch: json!({
                "request": "launch",
                "program": "${file_stem}",
                "cwd": "${root}",
            }),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        // Line 6 counted from zero — `total += i;`, inside the loop.
        let breakpoints = vec![(source.clone(), vec![5])];
        debug
            .start(&config, &root, &source, None)
            .expect("gdb started");

        let stopped = run_until(&mut debug, &rx, &breakpoints, 60, |d| {
            d.session()
                .is_some_and(|s| s.state.is_stopped() && !s.frames.is_empty())
        });
        assert!(stopped, "it never stopped: {:?}", state_of(&debug));

        let session = debug.session().expect("a session");
        let (path, line, _) = session.here().expect("somewhere to be");
        assert_eq!(path, source);
        assert_eq!(line, 5, "the breakpoint was on line 6, counted from zero");
        assert_eq!(
            session.frames.first().map(|f| f.name.as_str()),
            Some("fizz")
        );
        // And the breakpoint gdb could only confirm once the program was
        // loaded is confirmed — the `breakpoint` event, without which the
        // margin shows a hollow dot beside a breakpoint that works.
        assert_eq!(
            debug.is_verified(&source, 5),
            Some(true),
            "gdb verifies late, in an event of its own"
        );

        // Both scopes, not just the first: `n` is an argument and `total` is
        // a local, and gdb keeps them apart. Waiting for the second is what
        // makes this a test of "the scopes worth opening opened" rather than
        // of "one of them did".
        let got_values = run_until(&mut debug, &rx, &breakpoints, 20, |d| {
            d.session().is_some_and(|s| {
                let mut names = s.values.values().flatten().map(|v| v.name.as_str());
                let seen: Vec<&str> = names.by_ref().collect();
                seen.contains(&"n") && seen.contains(&"total")
            })
        });
        assert!(
            got_values,
            "the arguments and the locals should both have arrived: {:?}",
            debug.session().map(|s| s
                .scopes
                .iter()
                .map(|sc| format!("{} {:?}", sc.name, sc.hint))
                .collect::<Vec<_>>())
        );
        let session = debug.session().expect("a session");
        let named = |want: &str| {
            session
                .values
                .values()
                .flatten()
                .find(|v| v.name == want)
                .map(|v| v.value.clone())
        };
        assert_eq!(named("n").as_deref(), Some("5"), "the argument");
        assert_eq!(named("total").as_deref(), Some("0"), "and a local");
        // The registers are a scope too, and not one anybody stopped to read.
        assert!(
            session
                .scopes
                .iter()
                .any(|s| s.hint.as_deref() == Some("registers")),
            "gdb should have offered registers: {:?}",
            session.scopes.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        assert!(
            session
                .scopes
                .iter()
                .filter(|s| s.hint.as_deref() == Some("registers"))
                .all(|s| !session.open.contains(&s.reference)),
            "and they should not have opened themselves"
        );

        // The breakpoint is inside a loop that goes round five times, so
        // letting it go would only stop again. Taking it away first is both
        // how you get to the end and a test of the one thing the protocol has
        // no request for: there is no "remove a breakpoint", only "here is
        // every breakpoint in this file", so removing the last one in a file
        // means naming a file that now has none.
        debug.send_breakpoints(&[]);
        debug.resume();
        let ended = run_until(&mut debug, &rx, &[], 30, |d| {
            d.session().is_some_and(|s| s.state.is_over())
        });
        assert!(ended, "it never finished: {:?}", state_of(&debug));
        let printed = debug
            .session()
            .map(|s| s.all_printed())
            .unwrap_or_default();
        assert!(printed.contains("answer 10"), "{printed:?}");
        // And not gdb clearing its throat before there was a program at all.
        assert!(
            !printed.contains("GNU gdb") && !printed.contains("NO WARRANTY"),
            "the GNU licence is not something your program printed: {printed:?}"
        );

        debug.stop();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn the_session_before_this_one_cannot_end_it() {
        // The bug, and it is the whole of "press F5 twice and the debugger
        // stops working, for every file":
        //
        // starting a session stops the one before it, stopping an adapter
        // kills it, and killing it is what makes the thread reading it notice
        // the pipe close and post one last message. That message lands after
        // the new session has been made. With nothing to say which session it
        // was about, the new one was told its adapter had gone — before it had
        // finished starting — and everything after that was ignored, because a
        // session that has ended stays ended. The adapter was running fine the
        // whole time and there was nothing on the screen to say otherwise.
        let root = a_project("restart");
        let file = root.join("main.py");
        std::fs::write(&file, "print(1)\n").expect("written");
        let sit_there = lang::Debugger {
            id: "test/waits".into(),
            name: "waits".into(),
            command: "cat".into(),
            args: Vec::new(),
            roots: vec![".git".into()],
            launch: json!({}),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);

        // One session, and then another — which is what pressing the key
        // again does once the first attempt has been given up on.
        debug.start(&sit_there, &root, &file, None).expect("started");
        let first = debug.session().expect("a session").id;
        debug.start(&sit_there, &root, &file, None).expect("started again");
        let second = debug.session().expect("a session").id;
        assert_ne!(first, second, "a restart is a new session");

        // The dying gasp of the one before, arriving late. It must not be
        // taken for news about this one.
        assert_eq!(
            debug.on(first, Incoming::Exited("stopped".into()), &[]),
            Change::Nothing
        );
        assert!(
            !debug.session().expect("a session").state.is_over(),
            "the session that is actually running should still be running"
        );

        // And the one that really is about this session still lands.
        assert_eq!(
            debug.on(second, Incoming::Exited("stopped".into()), &[]),
            Change::Ended
        );
        assert!(debug.session().expect("a session").state.is_over());

        // Nothing was ever read off the channel; draining it here only keeps
        // the reader threads from writing into a receiver that has gone.
        drop(rx);
        debug.stop();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn a_reason_a_session_ended_for_is_not_replaced_by_a_vaguer_one() {
        // `terminated` follows `exited 3`. "It finished" is a worse answer
        // than "it exited 3", and the first thing that went wrong is the
        // thing worth saying.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        let mut session = a_session();
        let id = session.id;
        session.state = State::Running;
        debug.session = Some(session);

        debug.on(id, Incoming::Notification {
            method: "exited".into(),
            params: json!({ "exitCode": 3 }),
        }, &[]);
        assert_eq!(
            debug.session().expect("a session").state,
            State::Ended("exited 3".into())
        );
        debug.on(id, Incoming::Notification {
            method: "terminated".into(),
            params: Value::Null,
        }, &[]);
        assert_eq!(
            debug.session().expect("a session").state,
            State::Ended("exited 3".into()),
            "the specific reason should survive the vague one that follows it"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_breakpoint_the_adapter_would_not_take_is_known_to_be_unset() {
        // What the margin draws a hollow dot from. A breakpoint on a blank
        // line looks exactly like a working one until the program runs past
        // it, and "exactly like a working one" is the whole problem.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        // With no session nothing has been asked, and a breakpoint is neither
        // confirmed nor refused.
        assert_eq!(debug.is_verified(Path::new("/p/main.py"), 3), None);

        debug.session = Some(a_session());
        // Nor before the file has been sent.
        assert_eq!(debug.is_verified(Path::new("/p/main.py"), 3), None);

        let session = debug.session.as_mut().expect("a session");
        session
            .verified
            .insert(PathBuf::from("/p/main.py"), vec![3]);
        assert_eq!(debug.is_verified(Path::new("/p/main.py"), 3), Some(true));
        assert_eq!(debug.is_verified(Path::new("/p/main.py"), 5), Some(false));
        // And a session that is over says nothing about anything: the marks
        // go back to being what you asked for rather than what some program
        // that is no longer running once thought of them.
        debug.session.as_mut().expect("a session").state = State::Ended("finished".into());
        assert_eq!(debug.is_verified(Path::new("/p/main.py"), 5), None);
    }

    #[test]
    fn what_a_state_is_called_is_what_it_says() {
        assert_eq!(State::Stopped("breakpoint".into()).label(), "breakpoint");
        assert!(State::Stopped("step".into()).is_stopped());
        assert!(State::Ended("finished".into()).is_over());
        assert!(!State::Running.is_stopped());
    }

    // ---- Against a real adapter ----
    //
    // Everything above this line reads messages that were written by hand,
    // which proves the parsing and nothing else. What it cannot prove is the
    // thing most likely to be wrong: whether the *order* textfold sends
    // things in is an order a real adapter accepts. `initialize`, then the
    // program, then breakpoints only once the adapter says it is ready, then
    // `configurationDone` — get any of that wrong and the adapter either
    // refuses the request or sits there, and no amount of unit testing the
    // envelope will tell you.
    //
    // So this one runs `debugpy` for real, against a real Python file, and
    // checks that the program stops where it was told to and that `n` is 5
    // when it does.

    /// A Python that has `debugpy` in it, or `None` — in which case there is
    /// nothing here to test with and saying so beats failing.
    ///
    /// Looked for exactly the way the editor looks for one: the environment
    /// this project would use, and then the interpreter on the `PATH`.
    fn a_python_with_debugpy(root: &Path) -> Option<String> {
        let from_env = crate::venv::chosen(root, None).map(|e| e.python.display().to_string());
        for python in from_env.into_iter().chain(Some("python3".to_string())) {
            let ran = std::process::Command::new(&python)
                .args(["-c", "import debugpy"])
                .output();
            if ran.is_ok_and(|out| out.status.success()) {
                return Some(python);
            }
        }
        None
    }

    fn a_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("textfold-dap-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("a place to work");
        dir
    }

    /// Pump the channel into the debugger until something is true, or give up.
    ///
    /// The deadline is generous because what is being waited for is a Python
    /// interpreter starting under a debugger on a machine that may be busy
    /// building this very program.
    fn run_until(
        debug: &mut Debugger,
        rx: &std::sync::mpsc::Receiver<crate::app::Event>,
        breakpoints: &[(PathBuf, Vec<usize>)],
        seconds: u64,
        done: impl Fn(&Debugger) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        // A session that is over will not start stopping at breakpoints. Once
        // it is over there is a moment's grace for the last few messages, and
        // then no sense waiting out the whole minute for something that can no
        // longer happen — a test that fails in a second says exactly what a
        // test that fails in sixty says.
        let mut over_since: Option<std::time::Instant> = None;
        while std::time::Instant::now() < deadline {
            if done(debug) {
                return true;
            }
            if debug.session().is_some_and(|s| s.state.is_over()) {
                let since = *over_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() > std::time::Duration::from_millis(500) {
                    return false;
                }
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left.min(std::time::Duration::from_millis(100))) {
                Ok(crate::app::Event::Dap(id, message)) => {
                    debug.on(id, message, breakpoints);
                }
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        done(debug)
    }

    #[test]
    fn a_real_adapter_stops_where_it_was_told_to_and_says_what_the_values_are() {
        let root = a_project("stops");
        let Some(python) = a_python_with_debugpy(&root) else {
            // No adapter installed. Nothing to prove and nothing to fail.
            return;
        };
        let file = root.join("main.py");
        // A line at a time rather than one escaped literal: what this test is
        // about is which *line* the program stops on, and a stray space in a
        // continued string would be a Python file that does not run at all.
        let program = [
            "def fizz(n):",
            "    total = 0",
            "    for i in range(n):",
            "        total += i",
            "    return total",
            "",
            "",
            "def main():",
            "    answer = fizz(5)",
            "    print(\"answer\", answer)",
            "",
            "",
            "main()",
            "",
        ];
        std::fs::write(&file, program.join("\n")).expect("written");

        let config = lang::Debugger {
            id: "python/debugpy".into(),
            name: "debugpy".into(),
            command: python,
            args: vec!["-m".into(), "debugpy.adapter".into()],
            roots: vec![".git".into()],
            launch: json!({
                "request": "launch",
                "type": "python",
                "program": "${file}",
                "cwd": "${root}",
                "console": "internalConsole",
                "justMyCode": true,
            }),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        // Line 2, counted from zero: `total = 0`, the first line of the
        // function. Sent as line 3 on the wire, which is the conversion this
        // test exists to catch.
        let breakpoints = vec![(file.clone(), vec![1])];
        debug.start(&config, &root, &file, None).expect("debugpy started");

        let stopped = run_until(&mut debug, &rx, &breakpoints, 60, |d| {
            d.session()
                .is_some_and(|s| s.state.is_stopped() && !s.frames.is_empty())
        });
        assert!(stopped, "it never stopped: {:?}", state_of(&debug));

        let session = debug.session().expect("a session");
        assert_eq!(session.state, State::Stopped("breakpoint".into()));
        let (path, line, _) = session.here().expect("somewhere to be");
        assert_eq!(path, file, "it stopped in a file nobody asked about");
        assert_eq!(line, 1, "the breakpoint was on line 2, counted from zero");
        assert_eq!(
            session.frames.first().map(|f| f.name.as_str()),
            Some("fizz"),
            "the innermost frame is the function the breakpoint is in"
        );
        assert!(
            session.frames.iter().any(|f| f.name == "main"),
            "and its caller is below it: {:?}",
            session.frames.iter().map(|f| &f.name).collect::<Vec<_>>()
        );

        // The locals arrive a moment after the stack does — scopes, then the
        // variables in them — so this waits rather than asserting at once.
        let got_values = run_until(&mut debug, &rx, &breakpoints, 20, |d| {
            d.session().is_some_and(|s| {
                s.values.values().any(|list| list.iter().any(|v| v.name == "n"))
            })
        });
        assert!(got_values, "no locals ever arrived");
        let n = debug
            .session()
            .and_then(|s| {
                s.values
                    .values()
                    .flatten()
                    .find(|v| v.name == "n")
                    .cloned()
            })
            .expect("n is a local of fizz");
        assert_eq!(n.value, "5", "fizz was called with 5");

        // Stepping over one line should leave it in the same function, one
        // line further on.
        debug.step(Step::Over);
        let stepped = run_until(&mut debug, &rx, &breakpoints, 20, |d| {
            d.session()
                .is_some_and(|s| s.state.is_stopped() && s.here().is_some_and(|(_, l, _)| l > 1))
        });
        assert!(stepped, "it never stepped: {:?}", state_of(&debug));
        assert_eq!(
            debug.session().and_then(|s| s.here()).map(|(_, l, _)| l),
            Some(2),
            "one line on from where it was"
        );

        // And letting it go should run the program to the end, printing what
        // it prints on the way.
        debug.resume();
        let ended = run_until(&mut debug, &rx, &breakpoints, 30, |d| {
            d.session().is_some_and(|s| s.state.is_over())
        });
        assert!(ended, "it never finished: {:?}", state_of(&debug));
        // The program's own lines, and not the editor's account of the run:
        // "show me what my program printed" has to be answerable exactly, or
        // the panel is a place where two different things are jumbled.
        let printed = debug
            .session()
            .map(|s| s.program_printed())
            .unwrap_or_default();
        // `print("answer", answer)` reaches the adapter as more than one
        // `output` event — the word, the space, the number — so what is
        // checked is that both halves got here rather than the exact shape
        // of the line, which is Python's business and not textfold's.
        assert!(
            printed.contains("answer") && printed.contains("10"),
            "the program's own output should be here: {printed:?}"
        );
        assert!(
            !printed.contains("Unable to find thread"),
            "an adapter refusing a question we asked about a moment that has \
             passed is not something the program said: {printed:?}"
        );

        debug.stop();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_real_adapter_that_is_told_about_no_breakpoints_runs_straight_through() {
        // The other half of the same bargain, and the one that catches a
        // `setBreakpoints` sent before the adapter said it was ready: that is
        // refused, and a refused configuration is a program that never starts
        // — which looks exactly like a program with no breakpoints in it
        // unless you check that it actually ran.
        let root = a_project("straight");
        let Some(python) = a_python_with_debugpy(&root) else {
            return;
        };
        let file = root.join("main.py");
        std::fs::write(&file, "print(\"hello\")\n").expect("written");

        let config = lang::Debugger {
            id: "python/debugpy".into(),
            name: "debugpy".into(),
            command: python,
            args: vec!["-m".into(), "debugpy.adapter".into()],
            roots: vec![".git".into()],
            launch: json!({
                "request": "launch",
                "type": "python",
                "program": "${file}",
                "cwd": "${root}",
                "console": "internalConsole",
            }),
            env: BTreeMap::new(),
            see: None,
            from_server: None,
            attach: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debug = Debugger::new(tx);
        debug.start(&config, &root, &file, None).expect("debugpy started");
        let ended = run_until(&mut debug, &rx, &[], 60, |d| {
            d.session().is_some_and(|s| s.state.is_over())
        });
        assert!(ended, "it never finished: {:?}", state_of(&debug));
        let printed = debug
            .session()
            .map(|s| s.all_printed())
            .unwrap_or_default();
        assert!(printed.contains("hello"), "{printed:?}");
        debug.stop();
        std::fs::remove_dir_all(&root).ok();
    }

    /// Where a session got to, for a failure message that says something.
    fn state_of(debug: &Debugger) -> String {
        match debug.session() {
            None => "there is no session".into(),
            Some(session) => format!(
                "{:?}, output: {:?}",
                session.state,
                session.all_printed().replace('\n', " | ")
            ),
        }
    }

    #[test]
    fn what_a_server_answers_goes_where_the_manifest_said() {
        // Java will not launch without a classpath, and the only thing that
        // knows what the classpath is — after Maven, Gradle, and jdtls
        // compiling into a workspace of its own — is the language server.
        // Nothing here knows what a classpath is; it knows that a field of an
        // answer was to be put in a field of a launch.
        let resolve = lang::Resolve {
            command: "java.project.getClasspaths".into(),
            arguments: Vec::new(),
            into: BTreeMap::from([
                ("classPaths".to_string(), "classpaths".to_string()),
                ("modulePaths".to_string(), "modulepaths".to_string()),
            ]),
        };
        let mut launch = json!({ "request": "launch", "mainClass": "Main" });
        fold_into_launch(
            &mut launch,
            &resolve,
            &json!({ "classpaths": ["/w/bin"], "modulepaths": [], "projectRoot": "file:/w" }),
        );
        assert_eq!(launch["classPaths"], json!(["/w/bin"]));
        assert_eq!(launch["modulePaths"], json!([]));
        assert_eq!(launch["mainClass"], "Main", "and it left the rest alone");
        // What the answer did not mention is not set to null: an adapter
        // handed `"classPaths": null` refuses the launch over it.
        assert!(launch.get("projectRoot").is_none());
    }

    #[test]
    fn a_field_a_server_said_nothing_about_is_left_alone() {
        let resolve = lang::Resolve {
            command: "x".into(),
            arguments: Vec::new(),
            into: BTreeMap::from([("classPaths".to_string(), "classpaths".to_string())]),
        };
        let mut launch = json!({ "classPaths": ["already"] });
        fold_into_launch(&mut launch, &resolve, &json!({ "something": "else" }));
        assert_eq!(launch["classPaths"], json!(["already"]));
    }

    #[test]
    fn capabilities_arriving_later_are_added_to_rather_than_swapped_in() {
        let mut caps = json!({ "supportsConfigurationDoneRequest": true });
        merge(&mut caps, &json!({ "supportsStepBack": true }));
        assert_eq!(caps["supportsConfigurationDoneRequest"], true);
        assert_eq!(caps["supportsStepBack"], true);
    }
}
