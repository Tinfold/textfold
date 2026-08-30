//! Framed JSON over a pipe or a socket.
//!
//! The one piece of machinery shared by everything textfold talks to that is
//! not a person: language servers, plugin hosts, and debug adapters. All three
//! speak the same wire format — a `Content-Length` header, a blank line, then
//! that many bytes of JSON — and all three want the same three properties
//! from it.
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
//! **Where the bytes come from is not the conversation.** Nearly every peer is
//! a program we start, talking on its own standard input and output. One is
//! not: Java's debug adapter lives inside the Java *language server*, which
//! starts it on request and hands back a port. So a peer is a pair of streams
//! and, where there is one, a child process to stop afterwards — and nothing
//! above this line has to know which it got.
//!
//! What is deliberately *not* here is any vocabulary: no `initialize`, no
//! capabilities, no notion of what a method means. This module moves JSON
//! between a process and the event loop, and the modules above it decide what
//! the JSON says.
//!
//! The one thing it does know is that there are two envelopes. JSON-RPC 2.0 is
//! what a language server and a plugin speak; the Debug Adapter Protocol
//! wraps the same three kinds of message in fields of its own — `seq` for
//! `id`, `command` for `method`, `arguments` for `params`. That is a
//! [`Dialect`], chosen when a peer is started and never asked about again:
//! everything above this module says "ask", "notify", "answer", and the
//! difference in what goes down the pipe is four lines here rather than a
//! second copy of the framing, the threads and the process-group handling.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

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

/// How many lines of a peer's complaints are worth keeping to show somebody.
/// Enough for a Python traceback, which is the shape most of them take.
const COMPLAINTS_KEPT: usize = 24;

/// Which envelope a peer's messages come in.
///
/// Not two transports: one transport, and a choice of four field names. The
/// framing, the reader thread, the process group and the "a reply carries its
/// own meaning" bargain are the same either way, and this is the whole of the
/// difference.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    /// `{"jsonrpc":"2.0","id":1,"method":"…","params":{…}}` — a language
    /// server, a plugin host.
    JsonRpc,
    /// `{"seq":1,"type":"request","command":"…","arguments":{…}}` — a debug
    /// adapter.
    ///
    /// The other direction differs too: an answer is `type: "response"` with
    /// a `request_seq`, and something the adapter volunteers is
    /// `type: "event"` rather than a method with no id.
    Dap,
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
    /// Which envelope it speaks. Decided once, here, because a peer that was
    /// asked in one dialect and answered in another is a peer that has hung.
    pub dialect: Dialect,
}

/// A peer that is already running and listening on a port.
///
/// The one that is not a program of ours: `jdtls` is asked for a debug session
/// and answers with the port it has started one on. Nothing is spawned, so
/// there is nothing to kill afterwards — closing the socket is the whole of
/// stopping it.
pub struct Connect<'a> {
    /// Always the loopback address so far, and passed rather than assumed
    /// because a debug adapter on another machine is a thing that exists.
    pub host: &'a str,
    pub port: u16,
    pub label: &'a str,
    pub dialect: Dialect,
}

/// One peer we talk framed JSON to.
///
/// `A` is whatever the owner wants to remember about a question while it waits
/// for the answer — [`crate::lsp::Ask`] for a language server. This module
/// never looks inside one; it only hands it back.
pub struct Peer<A> {
    /// The process, where the peer is one we started. `None` for a peer we
    /// merely connected to, which is not ours to kill.
    child: Option<Child>,
    /// Where messages go. Boxed because it is a pipe for a peer we started and
    /// a socket for one we did not, and the difference ends here.
    out: Option<Box<dyn Write + Send>>,
    pending: HashMap<i64, A>,
    next_id: i64,
    dialect: Dialect,
    /// The last few lines it wrote to standard error.
    ///
    /// These go to the log as well, which is where they used to only go — and
    /// that was wrong for the one case that matters most: a peer that dies
    /// before it says anything at all. `python3 -m debugpy.adapter` with no
    /// `debugpy` installed exits immediately having printed exactly what is
    /// wrong, and an editor that reports "it stopped" while the answer sits in
    /// a file nobody has been told about is an editor that has hidden it.
    ///
    /// Shared because the thread reading them outlives nothing else here.
    complaints: Arc<Mutex<Vec<String>>>,
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

        // Each peer gets a **session** of its own: its own process group, and
        // no controlling terminal.
        //
        // The group half is so that stopping it can stop *everything it
        // started*. This is not a detail. A peer is very often a program that
        // runs another program — the Copilot plugin is Python that runs node,
        // and jdtls is a script that runs a JVM — and killing the one we
        // spawned leaves the other one running. It is reparented to init, it
        // keeps whatever it had (a quarter of a gigabyte, for Copilot's
        // server), and nothing will ever collect it. Seven of those is a
        // laptop that stops responding, which is exactly how this was found.
        //
        // The terminal half is why this is `setsid` rather than `setpgid`,
        // and a debug adapter is what found it. A debugger runs *your*
        // program, and a program run in a terminal expects to be able to read
        // from it — so `debugpy`'s launcher does what a shell does and calls
        // `tcsetpgrp` to put the program it started in the foreground. Our
        // terminal. From that moment every key you press goes to the program
        // being debugged instead of to the editor, and textfold is a text
        // editor that has stopped responding to the keyboard. A peer in a
        // session of its own has no controlling terminal to hand over, so it
        // cannot do this, and neither can anything it starts.
        //
        // Nothing textfold spawns wants a terminal — every one of them talks
        // down a pipe — so there is nothing given up by not having one.
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                // Safety: this runs in the forked child before `exec`, where
                // only async-signal-safe calls are allowed. Both of these are.
                //
                // `setsid` cannot fail here: it is refused only for a process
                // that already leads a group, and a freshly forked child never
                // does. It leaves the child leading both its new session and
                // its new group, with the group id equal to its pid — which is
                // what `signal_group` relies on.
                libc::setsid();

                // And the other half of the bargain. A session of its own
                // means the peer no longer dies with the terminal, so
                // something has to make it die with the editor even where the
                // editor was given no chance to say so — a `kill -9`, an
                // out-of-memory kill, a crash in a thread.
                //
                // `PDEATHSIG` is the kernel doing it: when the thread that
                // spawned this child goes, the child is signalled. It reaches
                // the peer itself rather than its whole tree, so a plugin that
                // starts programs of its own should still tidy up after
                // itself — but it is the difference between a language server
                // outliving the editor and not.
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }

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

        // A peer's complaints go somewhere a person can find them rather than
        // into the terminal underneath the editor, which would scribble over
        // the screen. Into the log, and — the last few of them — into the peer
        // itself, so that whoever owns it can say what it said rather than
        // only that it went.
        //
        // Started before the reader below, and handed a channel it never
        // sends on. Dropping that when the thread ends is how the reader
        // knows there is nothing more to come — see [`read_messages`].
        let complaints: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut said: Option<Receiver<()>> = None;
        if let Some(stderr) = stderr {
            let label = spawn.label.to_string();
            let kept = Arc::clone(&complaints);
            let (finished, waiting) = std::sync::mpsc::channel::<()>();
            let started = std::thread::Builder::new()
                .name(format!("rpc-err-{label}"))
                .spawn(move || {
                    let _finished = finished;
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        log(&label, &line);
                        let Ok(mut kept) = kept.lock() else { return };
                        // Only ever the last few. A language server that logs
                        // a line per keystroke would otherwise be a leak, and
                        // what anybody wants when a peer dies is the end of
                        // what it said, not the beginning.
                        if kept.len() == COMPLAINTS_KEPT {
                            kept.remove(0);
                        }
                        kept.push(line);
                    }
                });
            said = started.is_ok().then_some(waiting);
        }

        let dialect = spawn.dialect;
        if std::thread::Builder::new()
            .name(format!("rpc-{}", spawn.label))
            .spawn(move || read_messages(stdout, dialect, said, tx, wrap))
            .is_err()
        {
            child.kill().ok();
            return Err(NotStarted::Failed(format!(
                "could not listen to {}",
                spawn.command
            )));
        }

        Ok(Peer {
            child: Some(child),
            out: Some(Box::new(stdin)),
            pending: HashMap::new(),
            next_id: 0,
            dialect: spawn.dialect,
            complaints,
            failed: None,
        })
    }

    /// Talk to a peer that is already running, on a port somebody told us
    /// about.
    ///
    /// Nothing is spawned and nothing is killed. The far end belongs to
    /// whoever started it — for Java that is the language server, which will
    /// tidy up its own debug session when the socket closes — so this peer
    /// has no process group, no parent-death signal and no standard error to
    /// read. What it has is the same two streams as any other, which is all
    /// anything above this module ever wanted.
    pub fn connect(
        connect: Connect<'_>,
        tx: Sender<crate::app::Event>,
        wrap: impl Fn(Incoming) -> crate::app::Event + Send + 'static,
    ) -> Result<Self, NotStarted> {
        let at = format!("{}:{}", connect.host, connect.port);
        let stream = TcpStream::connect(&at)
            .map_err(|e| NotStarted::Failed(format!("could not reach {at}: {e}")))?;
        // Nagle's algorithm holds a small write back waiting for a bigger one
        // to join it. Every message here is small and every one of them is
        // somebody waiting, so it is exactly the wrong trade.
        stream.set_nodelay(true).ok();
        let reading = stream
            .try_clone()
            .map_err(|e| NotStarted::Failed(format!("could not listen to {at}: {e}")))?;

        let dialect = connect.dialect;
        if std::thread::Builder::new()
            .name(format!("rpc-{}", connect.label))
            // A socket has no standard error to wait on: everything the
            // other end has to say comes down the same pipe as the rest.
            .spawn(move || read_messages(reading, dialect, None, tx, wrap))
            .is_err()
        {
            return Err(NotStarted::Failed(format!("could not listen to {at}")));
        }
        Ok(Peer {
            child: None,
            out: Some(Box::new(stream)),
            pending: HashMap::new(),
            next_id: 0,
            dialect,
            complaints: Arc::new(Mutex::new(Vec::new())),
            failed: None,
        })
    }

    fn send(&mut self, message: &Value) {
        let Some(out) = &mut self.out else {
            return;
        };
        let body = message.to_string();
        // The header is the whole framing: a byte count, a blank line, then
        // that many bytes. Getting it wrong desynchronises the stream for
        // good, which is why it is written in exactly one place.
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        if out.write_all(framed.as_bytes()).is_err() || out.flush().is_err() {
            self.failed = Some("stopped listening".into());
            self.out = None;
        }
    }

    /// Say something nobody will answer.
    ///
    /// The Debug Adapter Protocol has no such message — everything the client
    /// sends is a request — so there it is a request whose answer nothing is
    /// waiting for. Which is the same thing said in the adapter's grammar:
    /// the reply arrives, [`Peer::claim`] finds no note against it, and it is
    /// dropped exactly as a duplicate would be.
    pub fn notify(&mut self, method: &str, params: Value) {
        match self.dialect {
            Dialect::JsonRpc => {
                self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
            }
            Dialect::Dap => {
                let seq = self.next_seq();
                self.send(&dap_request(seq, method, params));
            }
        }
    }

    /// Ask something, and write down what the answer will mean.
    pub fn request(&mut self, method: &str, params: Value, ask: A) -> i64 {
        let id = self.next_seq();
        self.pending.insert(id, ask);
        match self.dialect {
            Dialect::JsonRpc => self.send(&json!({
                "jsonrpc": "2.0", "id": id, "method": method, "params": params
            })),
            Dialect::Dap => self.send(&dap_request(id, method, params)),
        }
        id
    }

    /// Answer something the peer asked us.
    ///
    /// `id` is whatever came back on the [`Incoming::Request`], and it is a
    /// token rather than a number: JSON-RPC hands back the id it sent, and a
    /// debug adapter needs the command name repeated to it as well as the
    /// sequence number, so that is what its token carries. Nothing outside
    /// this module should look inside one — the only thing to do with it is
    /// hand it back.
    pub fn answer(&mut self, id: Value, result: Value) {
        match self.dialect {
            Dialect::JsonRpc => {
                self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
            Dialect::Dap => {
                let seq = self.next_seq();
                self.send(&json!({
                    "seq": seq, "type": "response",
                    "request_seq": id.get("seq").cloned().unwrap_or(Value::Null),
                    "command": id.get("command").cloned().unwrap_or(Value::Null),
                    "success": true, "body": result,
                }));
            }
        }
    }

    /// Refuse something the peer asked us. Still an answer: a peer waiting on
    /// a reply it will never get is a peer that has hung, and a plugin author
    /// staring at a hung plugin has no way to see why.
    pub fn refuse(&mut self, id: Value, code: i64, message: &str) {
        match self.dialect {
            Dialect::JsonRpc => self.send(&json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": code, "message": message }
            })),
            Dialect::Dap => {
                let seq = self.next_seq();
                self.send(&json!({
                    "seq": seq, "type": "response",
                    "request_seq": id.get("seq").cloned().unwrap_or(Value::Null),
                    "command": id.get("command").cloned().unwrap_or(Value::Null),
                    "success": false, "message": message,
                }));
            }
        }
    }

    /// The next number to put on a message.
    ///
    /// One counter for both directions, which is what the Debug Adapter
    /// Protocol asks for: every message a client sends carries a `seq`,
    /// answers to the adapter's own questions included.
    fn next_seq(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
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
        self.out = None;
        self.pending.clear();
    }

    /// Why the last write failed, if one did, taken so it is reported once.
    pub fn take_failure(&mut self) -> Option<String> {
        self.failed.take()
    }

    /// The last few lines it wrote to standard error.
    ///
    /// What to say when a peer dies without ever having said anything on the
    /// protocol — which is what "it is installed but it cannot run" looks
    /// like from here, and is by far the commonest way a peer fails.
    pub fn complaints(&self) -> Vec<String> {
        self.complaints
            .lock()
            .map(|kept| kept.clone())
            .unwrap_or_default()
    }

    /// Whether there is still a pipe to write down.
    pub fn is_writable(&self) -> bool {
        self.out.is_some()
    }

    /// Stop it, politely and then not.
    ///
    /// `shutdown` then `exit` is what LSP asks for, and what the plugin
    /// protocol borrows, so one implementation serves both. A debug adapter
    /// is asked to `disconnect` instead, and told to take the program it
    /// started with it — a debugger that leaves the thing being debugged
    /// suspended forever is worse than one that never ran.
    pub fn stop(&mut self) {
        match self.dialect {
            Dialect::JsonRpc => {
                self.send(&json!({"jsonrpc": "2.0", "id": 0, "method": "shutdown"}));
                self.notify("exit", json!(null));
            }
            Dialect::Dap => return self.disconnect(true),
        }
        self.end();
    }

    /// Stop a debug adapter, saying whether the program goes with it.
    ///
    /// The one place the two kinds of debugging part company at the end, and
    /// it is not a detail. A program textfold *launched* is textfold's: it
    /// exists because somebody pressed a key here, and leaving it running when
    /// the debugger goes would leave a process behind after every session,
    /// invisible and holding its port. A program textfold *attached to* was
    /// somebody else's before we arrived and is somebody else's after we
    /// leave. Killing that is not stopping debugging; it is stopping their
    /// program — a server, a game, a long simulation somebody attached to in
    /// order to look at it — and there is no undo for it.
    pub fn disconnect(&mut self, terminate: bool) {
        self.notify("disconnect", json!({ "terminateDebuggee": terminate }));
        self.end();
    }

    /// Make sure it is gone, along with anything it started.
    ///
    /// Called by [`Peer::stop`] after asking nicely, and by `Drop` where
    /// nobody asked at all — a panic on the way out must not leave a language
    /// server behind.
    fn end(&mut self) {
        // Dropping the stream closes it, which for a peer on a socket is the
        // whole of stopping it: the far end sees the connection go.
        self.out = None;
        self.pending.clear();
        let Some(child) = &mut self.child else { return };
        // A peer that will not go is a peer that gets killed. Waiting on a
        // wedged process is how editors hang on quit.
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Ok(Some(_)) = child.try_wait() {
            // It went on its own — but something it started may not have, so
            // the group still gets a signal.
            signal_group(child.id(), false);
            return;
        }
        signal_group(child.id(), false);
        std::thread::sleep(std::time::Duration::from_millis(20));
        signal_group(child.id(), true);
        child.kill().ok();
        child.wait().ok();
    }
}

impl<A> Drop for Peer<A> {
    fn drop(&mut self) {
        self.end();
    }
}

/// Signal a peer's whole process group: everything it started, and everything
/// those started, however deep.
///
/// `hard` is `SIGKILL` rather than `SIGTERM`. A language server is given the
/// chance to go quietly first, because several of them write an index to disk
/// on the way out and half a written index is worse than none.
///
/// The group id is the peer's own pid, because that is what `process_group(0)`
/// arranges when it is spawned. Negating it is what turns "this process" into
/// "this group" — the same thing `kill -TERM -1234` does from a shell.
#[cfg(unix)]
fn signal_group(pid: u32, hard: bool) {
    let signal = if hard { libc::SIGKILL } else { libc::SIGTERM };
    // Safety: `kill` with a negative pid is a signal to a process group. The
    // group is one we made when the child was spawned, so it holds that child
    // and its descendants and nothing else — in particular it is never 0,
    // which would mean *our own* group and would take the editor with it.
    if pid > 1 {
        unsafe {
            libc::kill(-(pid as libc::pid_t), signal);
        }
    }
}

#[cfg(not(unix))]
fn signal_group(_pid: u32, _hard: bool) {}

/// One request in a debug adapter's envelope.
///
/// Written once because `arguments` is left out entirely when there are none:
/// several adapters treat `"arguments": null` as an argument object that is
/// not an object and refuse the request over it.
fn dap_request(seq: i64, command: &str, arguments: Value) -> Value {
    let mut message = json!({ "seq": seq, "type": "request", "command": command });
    if !arguments.is_null()
        && let Some(object) = message.as_object_mut()
    {
        object.insert("arguments".into(), arguments);
    }
    message
}

/// How long the news that a peer has gone waits for the last of what it said.
///
/// The two are read by two threads, and a program that fails on its way up
/// writes its reason and dies in the same breath: the reason and the death
/// reach the editor in whichever order the scheduler felt like. Losing that
/// race means "would not start" with nothing after it — which is the one
/// message in this whole module that exists to carry a reason.
///
/// So the news waits. Bounded, because it must never be the reason the editor
/// is not told at all: a peer that handed its standard error to a child of its
/// own leaves that pipe open for as long as the child lives, and no amount of
/// waiting would produce an end that is not coming.
const LAST_WORDS: std::time::Duration = std::time::Duration::from_millis(250);

/// Read framed messages off a peer's output until it stops.
///
/// The only thing this thread does. It never touches editor state and never
/// waits for it, which is what keeps a wedged peer from being a wedged editor.
///
/// `said` is the thread reading the peer's standard error, which has nothing
/// to send and is waited on only for its end — see [`LAST_WORDS`].
fn read_messages(
    stdout: impl Read,
    dialect: Dialect,
    said: Option<Receiver<()>>,
    tx: Sender<crate::app::Event>,
    wrap: impl Fn(Incoming) -> crate::app::Event,
) {
    let mut reader = BufReader::new(stdout);
    let stopped = |why: &str, tx: &Sender<crate::app::Event>| {
        if let Some(said) = &said {
            said.recv_timeout(LAST_WORDS).ok();
        }
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

        let read = match dialect {
            Dialect::JsonRpc => read_one(message),
            Dialect::Dap => read_dap(message),
        };
        let Some(incoming) = read else {
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

/// One parsed message in a debug adapter's envelope.
///
/// The same three kinds under different names, and one thing genuinely
/// different: an adapter reports failure with `success: false` and a
/// `message`, so there is no `error` object to read a reason out of. A refusal
/// with nothing said about it still has to be a refusal rather than an empty
/// success, or a `launch` that failed looks from the editor like a program
/// that started and had nothing to say.
fn read_dap(message: Value) -> Option<Incoming> {
    match message.get("type").and_then(Value::as_str)? {
        // `body` rather than `result`, and absent where the adapter did what
        // it was asked and has nothing to report.
        "response" => {
            let id = message.get("request_seq").and_then(Value::as_i64)?;
            let ok = message
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let result = match ok {
                true => Ok(message.get("body").cloned().unwrap_or(Value::Null)),
                false => Err(message
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the adapter would not say why")
                    .to_string()),
            };
            Some(Incoming::Response { id, result })
        }
        // Something the adapter volunteered: it stopped, the program printed
        // a line, a thread started.
        "event" => Some(Incoming::Notification {
            method: message.get("event").and_then(Value::as_str)?.to_string(),
            params: message.get("body").cloned().unwrap_or(Value::Null),
        }),
        // Something the adapter wants from us — `runInTerminal` is the one
        // every adapter has. The token carries the command as well as the
        // sequence number, because a response has to name the command it is
        // answering and by then the request itself is gone.
        "request" => {
            let command = message.get("command").and_then(Value::as_str)?;
            Some(Incoming::Request {
                id: json!({ "seq": message.get("seq").cloned()?, "command": command }),
                method: command.to_string(),
                params: message.get("arguments").cloned().unwrap_or(Value::Null),
            })
        }
        _ => None,
    }
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

    /// Whether a process is still there, by asking the kernel rather than by
    /// running `ps`.
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        // Safety: signal 0 sends nothing and only reports whether the process
        // exists and could be signalled, which is what it is for.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[test]
    #[cfg(unix)]
    fn stopping_a_peer_stops_everything_it_started() {
        // The bug this is about: a peer is very often a program that runs
        // another program — the Copilot plugin is Python that runs node — and
        // killing the one we spawned leaves the other one running, reparented
        // to init, holding a quarter of a gigabyte, forever. Seven of those is
        // a laptop that stops responding.
        if !crate::pack::on_path("python3") {
            return;
        }
        let (tx, _rx) = std::sync::mpsc::channel();
        // A peer that starts a grandchild, says what its pid is, and then sits
        // there — which is what a plugin with a language server behind it is.
        let script = "import subprocess,sys;\
                      c=subprocess.Popen(['sleep','120']);\
                      sys.stdout.write('PID %d\\n' % c.pid);sys.stdout.flush();\
                      sys.stdin.read()";
        let args = vec!["-c".to_string(), script.to_string()];
        let env = BTreeMap::new();
        let mut peer: Peer<()> = Peer::start(
            Spawn {
                command: "python3",
                args: &args,
                root: Path::new("."),
                env: &env,
                label: "test",
                dialect: Dialect::JsonRpc,
            },
            tx,
            |_| crate::app::Event::Files(Vec::new()),
        )
        .expect("it started");

        // The grandchild's pid, read off the peer's own output. The listening
        // thread has the pipe, so this is read from the log line it prints —
        // simpler: give it a moment and find it by what it is.
        std::thread::sleep(std::time::Duration::from_millis(600));
        let child = peer.child.as_ref().expect("a child").id();
        let grandchildren = descendants_of(child);
        assert!(
            !grandchildren.is_empty(),
            "the test peer never started anything, so this proves nothing"
        );
        for pid in &grandchildren {
            assert!(alive(*pid), "{pid} should be running");
        }

        peer.stop();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!alive(child), "the peer itself is still running");
        for pid in &grandchildren {
            assert!(
                !alive(*pid),
                "{pid} outlived the peer that started it — this is the leak"
            );
        }
    }

    /// Every process under `pid`, out of `/proc`. Linux only, which is where
    /// this is checked; the behaviour it checks is every unix's.
    #[cfg(target_os = "linux")]
    fn descendants_of(pid: u32) -> Vec<u32> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return found;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(other) = name.parse::<u32>() else {
                continue;
            };
            let Ok(status) = std::fs::read_to_string(format!("/proc/{other}/status")) else {
                continue;
            };
            let parent = status
                .lines()
                .find_map(|line| line.strip_prefix("PPid:"))
                .and_then(|said| said.trim().parse::<u32>().ok());
            if parent == Some(pid) {
                found.push(other);
            }
        }
        found
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    fn descendants_of(_pid: u32) -> Vec<u32> {
        Vec::new()
    }

    #[test]
    fn a_peer_on_a_socket_talks_the_same_as_one_on_a_pipe() {
        // Java's debug adapter is not a program: it lives inside the Java
        // language server, which starts one on request and answers with a
        // port. So a peer is a pair of streams and, where there is one, a
        // process to stop afterwards — and this checks that the half without
        // a process works, because everything above this module is written as
        // though it could not tell the difference.
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();

        // The far end: read one framed request, answer it, and stay put.
        let far = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("a caller");
            let mut reader = BufReader::new(stream.try_clone().expect("a copy"));
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return String::new();
                }
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    break;
                }
                if let Some(rest) = line.strip_prefix("Content-Length:") {
                    length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).expect("a body");
            let asked: Value = serde_json::from_slice(&body).expect("json");

            let reply = json!({
                "seq": 1, "type": "response",
                "request_seq": asked["seq"], "command": asked["command"],
                "success": true, "body": { "supportsConfigurationDoneRequest": true },
            })
            .to_string();
            let mut stream = stream;
            write!(stream, "Content-Length: {}\r\n\r\n{reply}", reply.len()).ok();
            stream.flush().ok();
            asked["command"].as_str().unwrap_or_default().to_string()
        });

        let (tx, rx) = std::sync::mpsc::channel();
        let mut peer: Peer<&str> = Peer::connect(
            Connect {
                host: "127.0.0.1",
                port,
                label: "test",
                dialect: Dialect::Dap,
            },
            tx,
            |incoming| crate::app::Event::Dap(crate::dap::SessionId(1), incoming),
        )
        .expect("it connected");
        let id = peer.request("initialize", json!({ "adapterID": "test" }), "hello");

        let event = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("an answer");
        match event {
            crate::app::Event::Dap(_, Incoming::Response { id: got, result }) => {
                assert_eq!(got, id, "the answer should carry the sequence we sent");
                assert_eq!(result.expect("it worked")["supportsConfigurationDoneRequest"], true);
            }
            other => panic!("{other:?}", other = std::mem::discriminant(&other)),
        }
        assert_eq!(peer.claim(id), Some("hello"), "and what it was for");
        assert_eq!(far.join().expect("the far end"), "initialize");

        // Nothing was spawned, so there is nothing to kill: closing the
        // socket is the whole of stopping it.
        peer.stop();
        assert!(!peer.is_writable());
    }

    #[test]
    #[cfg(unix)]
    fn the_news_that_a_peer_has_gone_carries_what_it_said_on_the_way_out() {
        // The bug: a program that fails on its way up writes its reason and
        // dies in the same breath, and the two reach the editor down different
        // pipes read by different threads. Losing that race gave "would not
        // start" with nothing after it — the one message in this module whose
        // entire purpose is to carry a reason. Under load it lost the race
        // about one time in twenty, which is the worst rate for a bug to have:
        // too rare to catch, too common to never happen to anybody.
        let (tx, rx) = std::sync::mpsc::channel();
        let peer: Peer<()> = Peer::start(
            Spawn {
                command: "sh",
                args: &["-c".into(), "echo 'no module named nope' >&2; exit 1".into()],
                root: std::path::Path::new("."),
                env: &Default::default(),
                label: "test",
                dialect: Dialect::Dap,
            },
            tx,
            |incoming| crate::app::Event::Dap(crate::dap::SessionId(1), incoming),
        )
        .expect("sh started");

        let gone = loop {
            match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(crate::app::Event::Dap(_, Incoming::Exited(why))) => break Some(why),
                Ok(_) => continue,
                Err(_) => break None,
            }
        };
        assert!(gone.is_some(), "it never said the peer had gone");
        // And by the time it says so, what the peer said is there to be read.
        assert!(
            peer.complaints().iter().any(|line| line.contains("no module named nope")),
            "the reason had not arrived yet: {:?}",
            peer.complaints()
        );
    }

    #[test]
    fn a_socket_nobody_is_listening_on_says_so_rather_than_hanging() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        drop(listener);
        let (tx, _rx) = std::sync::mpsc::channel();
        let peer: Result<Peer<()>, _> = Peer::connect(
            Connect {
                host: "127.0.0.1",
                port,
                label: "test",
                dialect: Dialect::Dap,
            },
            tx,
            |_| crate::app::Event::Files(Vec::new()),
        );
        assert!(matches!(peer, Err(NotStarted::Failed(_))));
    }

    #[test]
    #[cfg(unix)]
    fn a_peer_cannot_take_the_terminal_away_from_the_editor() {
        // The bug: a debug adapter runs *your* program, and a program run in
        // a terminal expects to read from it — so `debugpy`'s launcher does
        // what a shell does and calls `tcsetpgrp` to put the program it
        // started in the foreground. Of our terminal. From that moment every
        // key goes to the program being debugged, and textfold is a text
        // editor that no longer answers the keyboard.
        //
        // A peer in a session of its own has no controlling terminal to hand
        // to anybody, which is what makes that impossible rather than merely
        // unlikely. Checked as "a session of its own", because that is the
        // property; whether any particular adapter tries it is not something
        // a test can promise.
        if !crate::pack::on_path("python3") {
            return;
        }
        let (tx, _rx) = std::sync::mpsc::channel();
        let args = vec!["-c".to_string(), "import sys;sys.stdin.read()".to_string()];
        let env = BTreeMap::new();
        let peer: Peer<()> = Peer::start(
            Spawn {
                command: "python3",
                args: &args,
                root: Path::new("."),
                env: &env,
                label: "test",
                dialect: Dialect::JsonRpc,
            },
            tx,
            |_| crate::app::Event::Files(Vec::new()),
        )
        .expect("it started");

        let child = peer.child.as_ref().expect("a child").id() as libc::pid_t;
        // Safety: `getsid` reads a property of a process and changes nothing.
        // A pid of 0 means our own.
        let (theirs, ours) = unsafe { (libc::getsid(child), libc::getsid(0)) };
        assert!(theirs > 0, "the child should have a session");
        assert_ne!(
            theirs, ours,
            "a peer sharing our session can hand our terminal to whatever it starts"
        );
        // And it leads that session, which is what makes signalling its whole
        // group by its own pid the right thing to do — see `signal_group`.
        assert_eq!(theirs, child, "the peer should lead its own session");
    }

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
    fn a_debug_adapters_answer_is_read_as_an_answer() {
        // `request_seq` where JSON-RPC has `id`, and `body` where it has
        // `result`. Same three kinds of message, different field names.
        let message = json!({
            "seq": 9, "type": "response", "request_seq": 4,
            "success": true, "command": "stackTrace",
            "body": { "stackFrames": [] }
        });
        match read_dap(message) {
            Some(Incoming::Response { id, result }) => {
                assert_eq!(id, 4);
                assert!(result.expect("it worked").get("stackFrames").is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_debug_adapter_that_says_no_says_why() {
        // There is no `error` object in this protocol: a refusal is
        // `success: false` and a `message`. Read as a success, a `launch`
        // that failed would look like a program that started and had nothing
        // to say — which is the difference between "you have a typo in your
        // path" and a debugger that appears to hang.
        let message = json!({
            "seq": 9, "type": "response", "request_seq": 2,
            "success": false, "command": "launch",
            "message": "there is no such file"
        });
        match read_dap(message) {
            Some(Incoming::Response { result, .. }) => {
                assert_eq!(result, Err("there is no such file".into()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_refusal_with_nothing_said_about_it_is_still_a_refusal() {
        let message = json!({
            "seq": 9, "type": "response", "request_seq": 2, "success": false,
            "command": "launch"
        });
        match read_dap(message) {
            Some(Incoming::Response { result, .. }) => assert!(result.is_err()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_event_is_what_a_notification_is_over_there() {
        let message = json!({
            "seq": 14, "type": "event", "event": "stopped",
            "body": { "reason": "breakpoint", "threadId": 1 }
        });
        match read_dap(message) {
            Some(Incoming::Notification { method, params }) => {
                assert_eq!(method, "stopped");
                assert_eq!(params["reason"], "breakpoint");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn something_an_adapter_asks_of_us_carries_what_it_takes_to_answer() {
        // A response in this protocol has to name the command it is
        // answering, and by the time the answer is written the request is
        // gone. So the token handed back with the question carries both — and
        // it is a token, not a number: the only thing anyone should do with
        // one is give it back.
        let message = json!({
            "seq": 21, "type": "request", "command": "runInTerminal",
            "arguments": { "args": ["python", "x.py"] }
        });
        match read_dap(message) {
            Some(Incoming::Request { id, method, params }) => {
                assert_eq!(method, "runInTerminal");
                assert_eq!(id["seq"], 21);
                assert_eq!(id["command"], "runInTerminal");
                assert_eq!(params["args"][0], "python");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_message_that_is_not_one_of_the_three_is_passed_over() {
        assert!(read_dap(json!({ "type": "whatever" })).is_none());
        assert!(read_dap(json!({ "seq": 1 })).is_none());
        // And a JSON-RPC message read as DAP is not a DAP message, which is
        // what keeps a peer started in the wrong dialect from half-working.
        assert!(read_dap(json!({ "jsonrpc": "2.0", "id": 1, "result": {} })).is_none());
    }

    #[test]
    fn a_request_with_no_arguments_leaves_the_field_out() {
        // Not a nicety. Several adapters take `"arguments": null` for an
        // argument object that is not an object, and refuse the request over
        // it — `configurationDone`, which is the one with no arguments, is
        // exactly the request that must not be refused.
        let message = dap_request(3, "configurationDone", Value::Null);
        assert_eq!(message["command"], "configurationDone");
        assert!(message.get("arguments").is_none());
        let message = dap_request(4, "continue", json!({ "threadId": 1 }));
        assert_eq!(message["arguments"]["threadId"], 1);
    }

    #[test]
    fn an_answer_is_claimed_once() {
        let mut peer: Peer<&str> = Peer {
            child: None,
            out: None,
            pending: HashMap::new(),
            next_id: 0,
            dialect: Dialect::JsonRpc,
            complaints: Arc::new(Mutex::new(Vec::new())),
            failed: None,
        };
        peer.pending.insert(1, "the thing we asked");
        assert_eq!(peer.claim(1), Some("the thing we asked"));
        // A confused peer answering twice is ignored the second time rather
        // than acted on twice.
        assert_eq!(peer.claim(1), None);
    }
}
