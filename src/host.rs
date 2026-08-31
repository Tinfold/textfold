//! Plugins that are programs.
//!
//! A [`crate::plugin::Tool`] is a program textfold runs on the file in front
//! of you: started, printed, dead. That covers a formatter and a linter and
//! nothing that has to remember anything. A *host* is the other kind — a
//! program that stays up, holds whatever state it likes, and talks to the
//! editor over JSON-RPC on its own standard input and output for as long as it
//! is wanted.
//!
//! Which means a plugin can be written in any language, because the only thing
//! it has to be able to do is read and write JSON on a pipe. The framing is
//! the one language servers use, so most languages already have a library that
//! speaks it and a plugin author writes handlers rather than a transport.
//!
//! Nothing here blocks. Starting a host is spawning a process and a thread to
//! listen to it; everything it says arrives on the channel the keyboard
//! arrives on, and is picked up between keystrokes. A plugin that wedges
//! itself is a queue that stops filling, not an editor that stops drawing.
//!
//! Hosts are started **when they are wanted** and not before: opening a Rust
//! file does not start the eleven other plugins you have installed. What counts
//! as wanted is [`crate::plugin::Activate`], written in the manifest.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::doc::{AppliedEdit, Document};
use crate::lang;
use crate::plugin::{self, Activate};
use crate::rpc::{self, Peer};

/// Which host, as everything outside holds onto one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct HostId(pub usize);

/// What we asked for, so the answer knows what it is an answer to.
#[derive(Clone, Debug)]
pub enum Ask {
    Initialize,
    /// A command we told it to run, named so that a refusal can say which.
    Command(String),
}

/// How far along a host is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
    /// Asked to initialize, waiting for the answer. Commands run in the
    /// meantime are remembered and sent the moment it is ready.
    Starting,
    Ready,
    /// It died, or would not start. The words say which.
    Dead(String),
}

/// How many times a host may fall over before it is left alone.
///
/// A plugin that crashes as it starts would otherwise be started again by the
/// next keystroke, and again, and again. Switching it off and on again in the
/// `plugins` list clears the count, which is what to do once whatever was
/// wrong is fixed.
const GIVE_UP_AFTER: u32 = 3;

/// The window those crashes have to happen in to count as "it is broken"
/// rather than "it has had a bad day at some point this session".
const CRASH_WINDOW: Duration = Duration::from_secs(60);

/// One running plugin.
pub struct Host {
    /// Which plugin it is running for: `cargo`, `stm32`. Its name as well as
    /// its identity — the id in the manifest is what the settings file, the
    /// plugins list and the status line all say.
    pub plugin: String,
    pub root: PathBuf,
    pub state: State,
    /// Which languages it asked to be told the text of. Copied from the
    /// manifest when it starts, because it is fixed for the life of the
    /// process and this is asked on every keystroke.
    wants: Vec<String>,
    /// Files it has been told about, so that a change is only sent for a
    /// buffer it has heard of and a close is only sent once.
    open: HashSet<PathBuf>,
    rpc: Peer<Ask>,
    /// What was asked for before it was ready, to send once it is. A person
    /// who pressed the key does not care that the program was still starting.
    queued: Vec<(String, Value, Option<Ask>)>,
}

impl Host {
    pub fn is_ready(&self) -> bool {
        self.state == State::Ready
    }

    /// Everything that writes goes through here, so that a pipe which has
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

    fn request(&mut self, method: &str, params: Value, ask: Ask) {
        self.rpc.request(method, params, ask);
        self.note_failure();
    }

    /// Tell it something, expecting no answer. The editor's half of a
    /// notification, for the things a plugin should know about but need not
    /// reply to.
    pub fn notify_out(&mut self, method: &str, params: Value) {
        self.notify(method, params);
    }

    pub fn answer(&mut self, id: Value, result: Value) {
        self.rpc.answer(id, result);
        self.note_failure();
    }

    /// Refuse something the plugin asked for. Still an answer: a plugin
    /// waiting on a reply it will never get is a plugin that has hung, and
    /// whoever wrote it has no way to see why.
    pub fn refuse(&mut self, id: Value, message: &str) {
        // -32601 is JSON-RPC's "no such method", which is what every one of
        // these is: the editor was asked for something it does not do.
        self.rpc.refuse(id, -32601, message);
        self.note_failure();
    }

    pub fn claim(&mut self, id: i64) -> Option<Ask> {
        self.rpc.claim(id)
    }

    /// Whether this plugin asked to be told the text of files like this one.
    ///
    /// The default is silence. A plugin that named no languages receives no
    /// buffer traffic at all, which is what stops "tell the plugins" from
    /// meaning "tell all eleven of them, on every keystroke".
    fn wants(&self, doc: &Document) -> bool {
        if self.wants.is_empty() {
            return false;
        }
        let language = lang::get(doc.language).name.to_lowercase();
        self.wants.contains(&language)
    }
}

/// A program the editor ran for a plugin, on its way back to it.
///
/// Boxed where it travels, because it carries everything the program printed
/// and an event that is occasionally a megabyte should not make every
/// keystroke a megabyte to move about.
#[derive(Debug)]
pub struct Ran {
    pub host: HostId,
    /// The plugin's own request id, so the answer finds its question.
    pub request: Value,
    pub ok: bool,
    pub code: Option<i32>,
    pub out: String,
    pub err: String,
}

/// Every host there is, and the machinery to start more.
pub struct Hosts {
    hosts: Vec<Host>,
    tx: Sender<crate::app::Event>,
    /// How often each host has fallen over, and when it last did.
    crashes: HashMap<(String, PathBuf), (u32, Instant)>,
    /// Things worth saying in the status line, taken by the editor next time
    /// it looks. Kept rather than printed because this module runs in the
    /// middle of other work and the screen belongs to the editor.
    pub problems: Vec<String>,
}

impl Hosts {
    pub fn new(tx: Sender<crate::app::Event>) -> Self {
        Hosts {
            hosts: Vec::new(),
            tx,
            crashes: HashMap::new(),
            problems: Vec::new(),
        }
    }

    pub fn all(&self) -> &[Host] {
        &self.hosts
    }

    pub fn get(&self, id: HostId) -> Option<&Host> {
        self.hosts.get(id.0)
    }

    pub fn get_mut(&mut self, id: HostId) -> Option<&mut Host> {
        self.hosts.get_mut(id.0)
    }

    /// A file was opened. Start whatever wanted to know about it.
    pub fn opened(&mut self, path: &Path, language: &str) {
        for plugin in plugin::active() {
            let Some(host) = &plugin.host else { continue };
            let wanted = host.activate.iter().any(|when| match when {
                Activate::Language(name) => name == language,
                Activate::File(glob) => plugin::matches_glob(glob, path),
                Activate::Command => false,
            });
            if wanted {
                self.wake(&plugin.id, host, path);
            }
        }
    }

    /// Run one of a plugin's commands, starting its host if it is not up yet.
    ///
    /// `from` is the file the command was run in, which is what decides the
    /// project root when the host is not already running for one.
    pub fn run(&mut self, command: &plugin::Command, from: Option<&Path>, context: Value) {
        // Opening a panel is not a command the plugin runs; it is the editor
        // saying "there is somewhere to draw now". The plugin answers by
        // filling it, and needs no reply from us.
        match command.opens_panel {
            true => self.send_for(
                command,
                from,
                "panel/opened",
                json!({ "panel": command.id, "context": context }),
                None,
            ),
            false => self.send_for(
                command,
                from,
                "command/run",
                json!({ "id": command.id, "context": context }),
                Some(Ask::Command(command.id.clone())),
            ),
        }
    }

    /// Say one thing to the host behind a command, starting it if need be.
    ///
    /// `ask` is `Some` where an answer is wanted and `None` where it is a
    /// statement. Either way nothing waits: a host that is still coming up
    /// keeps the message and sends it the moment it is ready, because the
    /// person pressed the key and should not have to press it again.
    fn send_for(
        &mut self,
        command: &plugin::Command,
        from: Option<&Path>,
        method: &str,
        params: Value,
        ask: Option<Ask>,
    ) {
        let Some(plugin) = plugin::find(&command.plugin) else {
            return;
        };
        let Some(host) = &plugin.host else { return };
        // Running a command is always reason enough to start, whatever the
        // manifest says it activates on. A command in the palette that
        // silently does nothing would be a bug, not a configuration.
        let root = self.wake(&plugin.id, host, from.unwrap_or(Path::new(".")));
        let Some(at) = self.find(&plugin.id, &root) else {
            return;
        };
        if !self.hosts[at].is_ready() {
            return self.hosts[at].queued.push((method.to_string(), params, ask));
        }
        match ask {
            Some(ask) => self.hosts[at].request(method, params, ask),
            None => self.hosts[at].notify(method, params),
        }
    }

    /// The host for this plugin and root, if there is a live one.
    fn find(&self, plugin: &str, root: &Path) -> Option<usize> {
        self.hosts.iter().position(|h| {
            h.plugin == plugin && h.root == root && !matches!(h.state, State::Dead(_))
        })
    }

    /// Make sure a host is running for the project `from` belongs to, and say
    /// which root that turned out to be.
    fn wake(&mut self, plugin: &str, config: &plugin::Host, from: &Path) -> PathBuf {
        // Settled to one absolute path before it is compared with anything.
        // A root reached from a relative path and the same root reached from
        // an absolute one are the same project, and a host is found by its
        // root — so without this, asking from a buffer with no file of its
        // own starts a *second* copy of a plugin that is already running, and
        // the second one knows nothing the first one found.
        let root = settle(lang::project_root(from, &config.roots));
        if self.find(plugin, &root).is_some() {
            return root;
        }
        let key = (plugin.to_string(), root.clone());
        if let Some((count, when)) = self.crashes.get(&key)
            && *count >= GIVE_UP_AFTER
            && when.elapsed() < CRASH_WINDOW
        {
            return root;
        }
        match self.start(plugin, config, &root) {
            Ok(host) => self.hosts.push(host),
            Err(why) => {
                // Counted as a crash so that a plugin whose program is not
                // installed complains a few times and then stops.
                self.note_crash(&key);
                self.problems.push(why);
            }
        }
        root
    }

    /// Run a plugin's program and set a thread to listen to it.
    ///
    /// The id is the position it will take in the list, which is only valid
    /// because a failure returns words rather than a half-made host — an id
    /// handed out for a host that never joined the list would name whichever
    /// host joined it next.
    fn start(
        &mut self,
        plugin: &str,
        config: &plugin::Host,
        root: &Path,
    ) -> Result<Host, String> {
        let id = HostId(self.hosts.len());
        let rpc = Peer::start(
            rpc::Spawn {
                command: &config.command,
                args: &config.args,
                root,
                env: &config.env,
                label: plugin,
                dialect: rpc::Dialect::JsonRpc,
            },
            self.tx.clone(),
            move |incoming| crate::app::Event::Plugin(id, incoming),
        )
        .map_err(|e| match e {
            rpc::NotStarted::Missing => format!(
                "{plugin}: {} is not installed — the plugin is off until it is",
                config.command
            ),
            rpc::NotStarted::Failed(why) => format!("{plugin}: {why}"),
        })?;

        let mut host = Host {
            plugin: plugin.to_string(),
            root: root.to_path_buf(),
            state: State::Starting,
            wants: config.wants_buffers.clone(),
            open: HashSet::new(),
            rpc,
            queued: Vec::new(),
        };
        host.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "client": { "name": "textfold", "version": env!("CARGO_PKG_VERSION") },
                "root": root,
                "capabilities": capabilities(),
                // What the manifest says about the plugin, to the plugin. The
                // editor carries it and does not read it.
                "settings": config.settings.clone().unwrap_or(Value::Null),
            }),
            Ask::Initialize,
        );
        Ok(host)
    }

    /// A host answered `initialize`. Anything held back while it started goes
    /// out now.
    /// What the plugin says it can do is read but not yet consulted: there is
    /// nothing to gate on until the message set grows past what every plugin
    /// answers. It is in the handshake so that plugins written now already say
    /// it when there is.
    pub fn ready(&mut self, id: HostId, _result: Value) {
        let Some(host) = self.get_mut(id) else { return };
        host.state = State::Ready;
        host.notify("initialized", json!({}));
        for (method, params, ask) in std::mem::take(&mut host.queued) {
            match ask {
                Some(ask) => host.request(&method, params, ask),
                None => host.notify(&method, params),
            }
        }
    }

    /// Mark a host as gone, and remember that it went.
    pub fn died(&mut self, id: HostId, why: String) {
        let Some(host) = self.hosts.get_mut(id.0) else {
            return;
        };
        host.state = State::Dead(why);
        host.rpc.close();
        host.queued.clear();
        let key = (host.plugin.clone(), host.root.clone());
        self.note_crash(&key);
    }

    fn note_crash(&mut self, key: &(String, PathBuf)) {
        let now = Instant::now();
        let entry = self.crashes.entry(key.clone()).or_insert((0, now));
        // A crash a long time after the last one starts the count again: a
        // plugin that fell over once this morning is not a broken plugin.
        if entry.1.elapsed() > CRASH_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
        entry.1 = now;
    }

    /// Whether this plugin has been given up on for now, so that the list can
    /// say so rather than showing a row that looks fine and does nothing.
    pub fn given_up_on(&self, plugin: &str) -> bool {
        self.crashes.iter().any(|((id, _), (count, when))| {
            id == plugin && *count >= GIVE_UP_AFTER && when.elapsed() < CRASH_WINDOW
        })
    }

    // ---- Telling plugins about the text ----

    /// Tell whoever asked that this buffer is open, and what is in it.
    ///
    /// Sent again for every open buffer when a host becomes ready, so that a
    /// plugin started by the eleventh file still knows about the first ten.
    pub fn opened_buffer(&mut self, doc: &Document) {
        let Some(path) = doc.path.clone() else { return };
        let language = lang::get(doc.language).name.clone();
        for host in &mut self.hosts {
            if !host.is_ready() || !host.wants(doc) || !host.open.insert(path.clone()) {
                continue;
            }
            host.notify(
                "buffer/opened",
                json!({
                    "path": path,
                    "language": language,
                    "version": doc.version,
                    "text": doc.text(),
                }),
            );
        }
    }

    /// What changed, in character indices into the document **as it was**.
    ///
    /// Not lines and columns, and not the UTF-16 that LSP counts in: a plugin
    /// keeping its own copy of the text wants the two numbers it can slice
    /// with, and those are the two the editor already has. Anything a plugin
    /// wants in lines and columns it can work out from the copy it is keeping.
    pub fn changed(&mut self, doc: &Document, edits: &[AppliedEdit]) {
        if edits.is_empty() {
            return;
        }
        let Some(path) = doc.path.clone() else { return };
        let changes: Vec<Value> = edits
            .iter()
            .map(|edit| json!({ "from": edit.from, "to": edit.to, "text": edit.text }))
            .collect();
        let params = json!({
            "path": path,
            "version": doc.version,
            "changes": changes,
        });
        for host in &mut self.hosts {
            if host.is_ready() && host.open.contains(&path) {
                host.notify("buffer/changed", params.clone());
            }
        }
    }

    /// Where the cursor is, for the plugins that asked about this buffer.
    pub fn selection_changed(&mut self, doc: &Document, params: Value) {
        let Some(path) = doc.path.clone() else { return };
        for host in &mut self.hosts {
            if host.is_ready() && host.wants(doc) && host.open.contains(&path) {
                host.notify("selection/changed", params.clone());
            }
        }
    }

    pub fn saved(&mut self, doc: &Document) {
        let Some(path) = doc.path.clone() else { return };
        let params = json!({ "path": path, "version": doc.version });
        for host in &mut self.hosts {
            if host.is_ready() && host.open.contains(&path) {
                host.notify("buffer/saved", params.clone());
            }
        }
    }

    pub fn closed(&mut self, path: &Path) {
        let params = json!({ "path": path });
        for host in &mut self.hosts {
            if host.open.remove(path) {
                host.notify("buffer/closed", params.clone());
            }
        }
    }

    // ---- Running a program for a plugin ----

    /// Run a program and answer the plugin when it has finished.
    ///
    /// A plugin can perfectly well spawn its own child, and one that wants to
    /// read the output as it arrives should. This is for the other case — a
    /// plugin that wants a program's output and nothing more — and it earns
    /// its place by being the one place that knows what the editor started.
    ///
    /// The reply is sent later, from the thread. Nothing waits here.
    pub fn run_program(
        &mut self,
        id: HostId,
        request: Value,
        command: &str,
        args: Vec<String>,
        cwd: Option<PathBuf>,
    ) -> Result<(), String> {
        let root = cwd
            .or_else(|| self.get(id).map(|h| h.root.clone()))
            .unwrap_or_else(|| PathBuf::from("."));
        let child = std::process::Command::new(command)
            .args(&args)
            .current_dir(&root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => format!("{command} is not installed"),
                _ => format!("{command}: {e}"),
            })?;

        let tx = self.tx.clone();
        let name = command.to_string();
        std::thread::Builder::new()
            .name(format!("plugin-run-{name}"))
            .spawn(move || {
                let done = child.wait_with_output();
                let ran = match done {
                    Ok(out) => Ran {
                        host: id,
                        request,
                        ok: out.status.success(),
                        code: out.status.code(),
                        out: String::from_utf8_lossy(&out.stdout).into_owned(),
                        err: String::from_utf8_lossy(&out.stderr).into_owned(),
                    },
                    Err(e) => Ran {
                        host: id,
                        request,
                        ok: false,
                        code: None,
                        out: String::new(),
                        err: e.to_string(),
                    },
                };
                tx.send(crate::app::Event::PluginRan(Box::new(ran))).ok();
            })
            .map_err(|_| format!("could not run {name}"))?;
        Ok(())
    }

    /// Stop everything one plugin is running, for when it is switched off.
    /// Its crash count goes too, so switching a plugin off and on again is the
    /// way to give a broken one another chance.
    pub fn stop_plugin(&mut self, plugin: &str) {
        for host in &mut self.hosts {
            if host.plugin == plugin && host.rpc.is_writable() {
                host.rpc.stop();
                host.state = State::Dead("switched off".into());
            }
        }
        self.hosts.retain(|h| h.plugin != plugin);
        self.crashes.retain(|(id, _), _| id != plugin);
    }

    /// Stop everything, on the way out.
    pub fn shutdown_all(&mut self) {
        for host in &mut self.hosts {
            if host.rpc.is_writable() {
                host.rpc.stop();
            }
        }
    }
}

/// The one absolute path a project root is known by.
///
/// A host is found by its root, so a root reached from a relative path has to
/// come out the same as the same root reached from an absolute one. Without
/// this, running a command from a buffer with no file of its own — a plugin's
/// own output, say — starts a *second* copy of a plugin that is already
/// running, and the second one knows nothing the first one found.
fn settle(root: PathBuf) -> PathBuf {
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// What textfold tells a plugin it can do.
///
/// Claiming something we do not implement is worse than not claiming it: a
/// plugin takes us at our word and sends things nobody handles.
fn capabilities() -> Value {
    json!({
        "status": true,
        "buffers": { "show": true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn hosts() -> Hosts {
        let (tx, _rx) = std::sync::mpsc::channel();
        Hosts::new(tx)
    }

    #[test]
    fn a_host_that_keeps_falling_over_is_left_alone() {
        let mut hosts = hosts();
        let key = ("stm32".to_string(), PathBuf::from("/tmp"));
        assert!(!hosts.given_up_on("stm32"));
        for _ in 0..GIVE_UP_AFTER {
            hosts.note_crash(&key);
        }
        assert!(hosts.given_up_on("stm32"));
        // And switching it off and on again is how you say "try once more".
        hosts.stop_plugin("stm32");
        assert!(!hosts.given_up_on("stm32"));
    }

    #[test]
    fn the_same_project_reached_two_ways_is_one_project() {
        // The bug this is here for: a command run from a buffer with no file
        // of its own settled on a relative root, which did not match the
        // absolute one the running host was found by — so a second copy
        // started, and it knew nothing the first had found.
        let here = std::env::current_dir().expect("somewhere to be");
        assert_eq!(settle(PathBuf::from(".")), settle(here.clone()));
        assert_eq!(settle(here.join("src").join("..")), settle(here));
    }

    #[test]
    fn a_root_that_is_not_there_is_left_as_it_was() {
        // Nothing to resolve against, and inventing one would be worse than
        // keeping what we were given.
        let made_up = PathBuf::from("/no/such/place/at/all");
        assert_eq!(settle(made_up.clone()), made_up);
    }

    #[test]
    fn one_bad_day_is_not_a_broken_plugin() {
        let mut hosts = hosts();
        let key = ("stm32".to_string(), PathBuf::from("/tmp"));
        hosts.note_crash(&key);
        // Long enough ago that it does not count towards giving up.
        if let Some(entry) = hosts.crashes.get_mut(&key) {
            entry.1 = Instant::now() - CRASH_WINDOW * 2;
        }
        for _ in 0..GIVE_UP_AFTER - 1 {
            hosts.note_crash(&key);
        }
        assert!(!hosts.given_up_on("stm32"), "the old crash was counted");
    }
}
