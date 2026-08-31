//! Plugins: the parts of textfold that are data rather than code, and that you
//! can turn off.
//!
//! Everything textfold knows about a language — how to colour it, how to
//! comment it out, and which servers to run for it — arrives as a plugin. The
//! ones that ship are the JSON files in `src/plugins/`, built into the binary;
//! yours go in `~/.config/textfold/plugins/`, either as `name.json` or as
//! `name/plugin.json`. There is no difference between the two kinds once they
//! are loaded, which is the point: nothing textfold ships is reachable by a
//! route your own plugin cannot take.
//!
//! ```json
//! { "id": "zig", "name": "Zig", "about": "Colours and zls",
//!   "languages": { "zig": {
//!       "extensions": ["zig", "zon"],
//!       "line_comment": "//",
//!       "servers": [{ "name": "zls", "command": "zls", "roots": ["build.zig"] }]
//!   } } }
//! ```
//!
//! A plugin has an id, and so does each server inside it: `pytools` is the
//! plugin, and `pytools/ruff` is the one server. Either can be turned off,
//! from the settings file or from `plugins` in the command palette, and
//! turning off a plugin turns off the servers inside it. A plugin that *is*
//! one server — which is what every language server textfold ships now is —
//! is named once rather than twice: `pyright`, not `pyright/pyright`.
//!
//! What is on is remembered in `config.json` under `plugins`, and only what
//! you have changed is written there — a plugin nobody has said anything about
//! is on.
//!
//! A plugin can also say what it needs on the machine and how to get it, which
//! is what [`crate::pack`] carries out:
//!
//! ```json
//! { "id": "zls", "needs": ["zls"],
//!   "install": [{ "about": "zls, from brew", "run": ["brew", "install", "zls"] }],
//!   "uninstall": [{ "run": ["brew", "uninstall", "zls"] }],
//!   "see": "https://github.com/zigtools/zls" }
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use serde::Deserialize;

use crate::lang::FileLang;

/// Where a plugin came from, for the list you look at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Built into the binary.
    BuiltIn,
    /// A file of your own.
    File(PathBuf),
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::BuiltIn => "built in".to_string(),
            Source::File(path) => path.display().to_string(),
        }
    }
}

/// What a server inside a plugin is called, in the settings file and in the
/// list of things with a switch beside them.
///
/// `pytools/ruff` where the plugin has several things in it — and just
/// `pyright` where the plugin *is* the one server, because `pyright/pyright`
/// is a name nobody would choose to write. Every language server textfold
/// ships is now a plugin of its own, so the short form is the common one.
pub fn server_id(plugin: &str, name: &str) -> String {
    match plugin == name {
        true => plugin.to_string(),
        false => format!("{plugin}/{name}"),
    }
}

/// One step of getting a plugin working: a program to run, and when running it
/// would be a waste of everybody's time.
///
/// The declarative half of installing. A great deal of what an installer does
/// is *run this, unless the thing it fetches is already here*, and that is a
/// table rather than a script — with the same benefit the rest of textfold's
/// plugin manifests have, which is that you can read what a plugin is about to
/// do to your machine before you let it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The line the status bar shows while it runs.
    pub about: String,
    /// The program and its arguments. There is no shell here, so there is
    /// nothing to quote wrongly and nothing a `$` can do to you.
    pub run: Vec<String>,
    /// A program that, if it is already on the `PATH`, means this step has
    /// nothing left to do. How a plugin offers three ways to get the same
    /// thing without installing it three times.
    pub unless: Option<String>,
    /// A file that has to be there for this step to be worth running. The
    /// mirror of `unless`, and what makes a download safe to write as more
    /// than one step.
    ///
    /// Fetching a program is three steps — download, unpack, make it runnable
    /// — and the second two are only meaningful if the first one happened.
    /// Without this, a machine with no `curl` would skip the download (a step
    /// whose program is missing is skipped) and then *fail* on the unpack,
    /// stopping the install before it reached the ways of getting it that do
    /// work here.
    pub when: Option<String>,
    /// Which systems it is for: `"macos"`, `"linux"`, `"windows"`. Empty means
    /// any of them.
    ///
    /// Most steps need none of this — `npm` and `pip` are the same everywhere.
    /// It is for the ones where the answer really does differ, which is
    /// usually a program that is only distributed as a build per platform.
    pub os: Vec<String>,
    /// Which processors it is for: `"x86_64"`, `"aarch64"`. Empty means any.
    /// Needed alongside `os` because a program distributed as a build per
    /// platform is distributed as a build per *processor* too.
    pub arch: Vec<String>,
    /// Whether it installs outside textfold's own directory.
    ///
    /// Said out loud rather than discovered afterwards. Almost everything
    /// textfold fetches goes into a directory of its own — see
    /// [`crate::pack::tools_dir`] — and the handful of things that cannot,
    /// because the program that fetches them has no notion of installing
    /// anywhere but the system, are named before they run.
    pub system: bool,
}

impl Step {
    /// What is actually run, for the log and for the line that says what
    /// failed.
    pub fn line(&self) -> String {
        self.run.join(" ")
    }

    /// Whether this step is for the machine textfold is on.
    pub fn here(&self) -> bool {
        let ok = |said: &[String], is: &str| said.is_empty() || said.iter().any(|w| w == is);
        ok(&self.os, std::env::consts::OS) && ok(&self.arch, std::env::consts::ARCH)
    }
}

/// One server or debug adapter a plugin contributes, as the list of things you
/// can switch off needs to know about it.
///
/// One type for both because from the switch list's point of view they are the
/// same thing: an id, a name, a program, and the languages it is for. The two
/// are kept in separate lists on the [`Plugin`] so that the list can still say
/// which is which.
#[derive(Clone, Debug)]
pub struct ServerEntry {
    /// `pyright`, or `pytools/ruff`.
    pub id: String,
    /// `ruff`.
    pub name: String,
    /// What is actually run.
    pub command: String,
    /// Which of the plugin's languages it is for. More than one is normal:
    /// `clangd` is C and C++, and `tsserver` is three.
    pub languages: Vec<String>,
}

impl ServerEntry {
    /// The languages it is for, for the line under its name in a list.
    pub fn for_what(&self) -> String {
        self.languages.join(", ")
    }
}

/// A program a plugin can run for you: a formatter, a linter, a test run.
///
/// The half of "an editor with plugins" that needs no plugin runtime at all.
/// A great deal of what people write plugins for in other editors is running
/// something on the buffer and doing one of four things with what it printed,
/// and that is a table, not code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    /// `python/black`, which is also the command name a key binds to.
    pub id: String,
    /// `black`.
    pub name: String,
    pub about: String,
    pub command: String,
    pub args: Vec<String>,
    /// Which languages it is for. Empty means any file.
    pub languages: Vec<String>,
    /// Files that mark the top of the project it should be run in.
    pub roots: Vec<String>,
    /// Whether the buffer goes in on standard input. Otherwise the tool is
    /// expected to read the file itself, and `${file}` in the arguments is
    /// where its path goes.
    pub stdin: bool,
    pub output: Output,
    /// How to read a line of output as a problem. See [`Output::Problems`].
    pub pattern: Option<String>,
    /// Whether to run it every time the file is saved.
    pub on_save: bool,
    /// Whether this is what turns a file of its language into something that
    /// can be run.
    ///
    /// A build is an ordinary tool in every respect — a program, run on your
    /// file, whose complaints go in the margin — and this one flag is the
    /// whole of what makes it special: it is the one the editor runs *for*
    /// you, before a debugger, without being asked. You cannot debug `main.c`;
    /// you debug what `cc -g` made out of it, and an editor that knows how to
    /// start a debugger but not how to produce the thing it debugs has left
    /// the interesting half to a terminal in another window.
    ///
    /// One per language, first found. A second is a manifest saying two
    /// different things are *the* build, and there is no answer to which.
    pub builds: bool,
}

/// What to do with what a tool printed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Output {
    /// It replaces the buffer. Formatters: `black -`, `gofmt`, `prettier`.
    Replace,
    /// It opens in a buffer of its own. Test runs, `git log`, anything you
    /// want to read rather than apply.
    Show,
    /// It is a list of problems, read with `pattern` and shown in the margin
    /// beside the language server's own.
    Problems,
    /// Nothing to read. Say that it ran and what it said if it failed.
    Ignore,
}

impl Tool {
    /// What running it does to the text, which is what decides whether it may
    /// run on a read-only file.
    pub fn behaviour(&self) -> crate::cmd::Behaviour {
        match self.output {
            Output::Replace => crate::cmd::Behaviour::Edits,
            _ => crate::cmd::Behaviour::Passive,
        }
    }

    /// Whether it is for this language.
    pub fn wants(&self, language: &str) -> bool {
        self.languages.is_empty() || self.languages.iter().any(|l| l == language)
    }
}

/// A long-running program a plugin brings with it.
///
/// The difference between this and a [`Tool`] is memory. A tool is started,
/// prints, and dies, which covers a formatter and a linter and nothing that
/// has to hold anything between one keystroke and the next. A host stays up:
/// it can keep a parsed board description, a connection to a debug probe, or
/// a build that is still running, and it can volunteer something without
/// being asked first.
///
/// It talks JSON-RPC over its own standard input and output, which is why a
/// plugin can be written in any language — see [`crate::host`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub command: String,
    pub args: Vec<String>,
    /// Files that mark the top of the project it should be started in. One
    /// process per root, as with a language server.
    pub roots: Vec<String>,
    /// What has to happen before it is worth starting. Empty means "when one
    /// of its commands is run", which is the least surprising default: a
    /// plugin nobody has asked anything of is a plugin that need not run.
    pub activate: Vec<Activate>,
    /// Which languages it wants to be told about the text of. Empty means
    /// none at all — a plugin that never asked receives no buffer traffic.
    pub wants_buffers: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Whatever the plugin wants to be told about itself, handed over at
    /// `initialize` and never looked inside. A plugin's own settings are the
    /// plugin's business; the editor's job is to carry them.
    pub settings: Option<serde_json::Value>,
}

/// What makes a host worth starting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activate {
    /// A file of this language was opened.
    Language(String),
    /// A file whose name matches this was opened. `*` stands for anything
    /// within a path segment and `**` for anything at all, so `**/*.ioc`
    /// finds one anywhere in the tree and `Cargo.toml` only at the root.
    File(String),
    /// One of the plugin's own commands was run. Always allowed, whether or
    /// not it is written down: a command in the palette that quietly does
    /// nothing would be a bug, not a configuration.
    Command,
}

impl Activate {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(name) = text.strip_prefix("language:") {
            return Some(Activate::Language(name.trim().to_lowercase()));
        }
        if let Some(glob) = text.strip_prefix("file:") {
            return Some(Activate::File(glob.trim().to_string()));
        }
        match text {
            "command" => Some(Activate::Command),
            _ => None,
        }
    }
}

/// One command a plugin contributes.
///
/// Declared in the manifest rather than announced by the running program, so
/// that it is in the palette and bindable before the program has ever been
/// started. That is what makes starting on demand possible at all: running a
/// command is one of the things that starts a host, and you cannot run a
/// command nobody can see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// `cargo/test`, which is also the command name a key binds to.
    pub id: String,
    /// `test`.
    pub name: String,
    pub about: String,
    /// Which plugin to send it to.
    pub plugin: String,
    pub behaviour: crate::cmd::Behaviour,
    /// Which languages it is offered for. Empty means any file.
    pub languages: Vec<String>,
    /// Whether running it opens a panel rather than telling the plugin to do
    /// something. A panel is declared in the manifest like a command because
    /// that is what it is from the outside: a row in the palette, a key you
    /// can bind, a switch in the plugins list.
    pub opens_panel: bool,
    /// Where the panel sits, for one that is part of the editor's shape
    /// rather than a buffer you switch to.
    ///
    /// `None` is a tab, which is what a panel used to always be and is still
    /// the right answer for something you read and then leave — a build
    /// report, a list of test failures. An edge is for something you keep
    /// beside the code: a tree of files, a list of problems along the bottom.
    ///
    /// Declared in the manifest rather than announced by the running program,
    /// for the same reason the command is: the editor should be able to lay
    /// the thing out before the plugin has ever been started. A plugin that
    /// wants to move it afterwards can, with `panel/dock`.
    pub dock: Option<crate::view::Dock>,
}

impl Command {
    /// Whether it is for this language.
    pub fn wants(&self, language: &str) -> bool {
        self.languages.is_empty() || self.languages.iter().any(|l| l == language)
    }
}

/// Whether a path matches one of the patterns a host activates on.
///
/// Small on purpose: `*` within a segment, `**` across them, and everything
/// else literal. A pattern with no `/` in it is matched against the file name
/// alone, because `"Cargo.toml"` plainly means that file and not only one in
/// the directory the editor happens to have been started in.
pub fn matches_glob(pattern: &str, path: &std::path::Path) -> bool {
    let text = match pattern.contains('/') {
        true => path.to_string_lossy().into_owned(),
        false => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    glob_here(pattern.as_bytes(), text.as_bytes())
}

fn glob_here(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'*') => {
            // `**` crosses directory separators; a single `*` stops at one.
            // The trailing `/` of `**/` is optional so that `**/*.rs` also
            // finds a file sitting at the top.
            let (rest, crosses) = match pattern.get(1) {
                Some(b'*') => {
                    let after = &pattern[2..];
                    // The `/` of `**/` is eaten with it, so that `**/*.rs`
                    // finds a file at the top of the tree as well as one
                    // further down.
                    match after.first() {
                        Some(b'/') => (&after[1..], true),
                        _ => (after, true),
                    }
                }
                _ => (&pattern[1..], false),
            };
            if glob_here(rest, text) {
                return true;
            }
            for at in 0..text.len() {
                if !crosses && text[at] == b'/' {
                    return false;
                }
                if glob_here(rest, &text[at + 1..]) {
                    return true;
                }
            }
            false
        }
        Some(&c) => match text.first() {
            Some(&t) if t == c => glob_here(&pattern[1..], &text[1..]),
            _ => false,
        },
    }
}

/// One plugin.
pub struct Plugin {
    /// What the settings file and the list call it. Unique: a plugin of yours
    /// with the id of one that ships replaces it, which is how you swap out
    /// what textfold does for a language without editing textfold.
    pub id: String,
    /// What to call it on the screen.
    pub name: String,
    pub about: Option<String>,
    /// What version of it this is, where it says. Compared against what a
    /// package repository is offering to decide whether there is an update —
    /// see [`crate::repo::is_newer`]. A plugin that says nothing has no
    /// version, and nothing is ever an update to it.
    pub version: Option<String>,
    pub source: Source,
    /// Whether it is on when nobody has said. A plugin can ship turned off.
    pub on_by_default: bool,
    /// What it says about languages, in the shape `languages.json` uses.
    pub languages: BTreeMap<String, FileLang>,
    pub servers: Vec<ServerEntry>,
    /// The debug adapters it contributes, in the same shape and switched off
    /// the same way.
    pub debuggers: Vec<ServerEntry>,
    /// Programs it can run for you, each a command of its own.
    pub tools: Vec<Tool>,
    /// The program it brings with it, if it brings one.
    pub host: Option<Host>,
    /// Commands that program answers to. Only meaningful with a `host`: a
    /// command with nothing to send it to is dropped when the manifest is
    /// read, with a complaint, rather than sitting in the palette doing
    /// nothing.
    pub commands: Vec<Command>,
    /// Sets of colours it brings, by name, in the shape a theme file is in.
    pub themes: BTreeMap<String, serde_json::Value>,
    /// Keys it would like bound, by command name. What you have set in your
    /// own settings wins; this is the plugin saying what it would suggest.
    pub keys: BTreeMap<String, Vec<String>>,
    /// The programs it needs on the `PATH` before it can do anything. A
    /// plugin with none of these needs nothing fetched and is ready the
    /// moment it is read.
    pub needs: Vec<String>,
    /// How to get them. Run in order by [`crate::pack`].
    pub install: Vec<Step>,
    /// How to put them back, for when the plugin is removed. Absent means
    /// removing the plugin leaves what it installed alone, which is the safe
    /// answer for anything a plugin did not fetch itself.
    pub uninstall: Vec<Step>,
    /// Where to get it by hand, for when none of the steps could. Not every
    /// program on earth has an installer that works on every machine, and
    /// saying where to go is better than failing without a suggestion.
    pub see: Option<String>,
}

impl Plugin {
    /// A line for the list: what it does, or failing that where it came from.
    pub fn detail(&self) -> String {
        match &self.about {
            Some(about) => about.clone(),
            None => self.source.label(),
        }
    }

    /// The version, for a list that has room to say it.
    pub fn version_label(&self) -> Option<String> {
        self.version.as_ref().map(|v| format!("v{v}"))
    }

    /// The programs it needs that are not on this machine.
    ///
    /// What the plugins list shows instead of "on" for a plugin that is
    /// switched on and cannot work: a row that says `on` beside a language
    /// server nobody has installed is a row that lies.
    pub fn missing(&self) -> Vec<&str> {
        self.needs
            .iter()
            .map(String::as_str)
            .filter(|command| !crate::pack::on_path(command))
            .collect()
    }

    /// Whether everything it needs is here.
    pub fn is_ready(&self) -> bool {
        self.missing().is_empty()
    }

    /// Whether there is anything to fetch for it at all.
    pub fn can_install(&self) -> bool {
        !self.install.is_empty()
    }
}

/// Every plugin found, on or off, in the order they are merged.
struct Registry {
    plugins: Vec<Plugin>,
    problems: Vec<String>,
}

/// Swapped whole when a plugin is installed or removed. The old one is leaked
/// rather than dropped, because a `&'static Plugin` handed out before the swap
/// may still be sitting in a command table or a picker that is on the screen.
/// This happens a handful of times in a session at most.
static REGISTRY: OnceLock<RwLock<&'static Registry>> = OnceLock::new();

/// What is switched off, and what has been switched back on. Separate from the
/// registry because this changes while the editor runs and the registry does
/// not.
static CHOSEN: OnceLock<RwLock<BTreeMap<String, bool>>> = OnceLock::new();

fn chosen() -> &'static RwLock<BTreeMap<String, bool>> {
    CHOSEN.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Ids that used to mean something and now mean something else.
///
/// The language servers moved out into plugins of their own, so a settings
/// file that says `"python/ruff": false` is talking about a name that is no
/// longer anybody's. Renaming what it said is the difference between a
/// setting that survives an upgrade and a linter that quietly comes back on.
const RENAMED: &[(&str, &str)] = &[
    ("bash/bash-language-server", "bash-language-server"),
    ("c/clangd", "clangd"),
    ("cpp/clangd", "clangd"),
    ("csharp/omnisharp", "omnisharp"),
    ("css/css-language-server", "vscode-langservers/css-language-server"),
    ("dockerfile/docker-langserver", "docker-langserver"),
    ("go/gopls", "gopls"),
    ("html/html-language-server", "vscode-langservers/html-language-server"),
    ("java/jdtls", "jdtls"),
    ("javascript/tsserver", "tsserver"),
    ("json/json-language-server", "vscode-langservers/json-language-server"),
    ("markdown/marksman", "marksman"),
    ("python/pyright", "pyright"),
    ("python/ruff", "ruff"),
    ("rust/rust-analyzer", "rust-analyzer"),
    ("toml/taplo", "taplo"),
    ("tsx/tsserver", "tsserver"),
    ("typescript/tsserver", "tsserver"),
    ("yaml/yaml-language-server", "yaml-language-server"),
];

/// Read the plugins, and take note of what the settings say is off.
///
/// Called before anything asks for a language. Calling it twice is harmless
/// and the second call only updates what is on — the files are read once.
///
/// The settings go in by reference and come out renamed, so that a file
/// written by an older textfold is brought up to date the first time it is
/// read rather than being half-obeyed forever.
pub fn init(settings: &mut BTreeMap<String, bool>) {
    cell();
    rename(settings);
    *chosen().write().unwrap_or_else(|e| e.into_inner()) = settings.clone();
}

/// Bring the ids in a settings file up to date. An id that has been renamed
/// keeps what you said about it; one that was already the new name is left
/// alone, so this is safe to run over the same file forever.
fn rename(settings: &mut BTreeMap<String, bool>) {
    for (was, now) in RENAMED {
        if let Some(said) = settings.remove(*was) {
            settings.entry(now.to_string()).or_insert(said);
        }
    }
}

fn cell() -> &'static RwLock<&'static Registry> {
    REGISTRY.get_or_init(|| RwLock::new(Box::leak(Box::new(load()))))
}

fn registry() -> &'static Registry {
    *cell().read().unwrap_or_else(|e| e.into_inner())
}

/// Read the plugin files again, for after one has been installed or removed.
///
/// What is switched off is left alone: it is about ids, not about files, and a
/// plugin you switched off before installing another one should stay off.
pub fn reload() {
    let fresh: &'static Registry = Box::leak(Box::new(load()));
    *cell().write().unwrap_or_else(|e| e.into_inner()) = fresh;
}

/// Every plugin there is, on or off.
pub fn all() -> &'static [Plugin] {
    &registry().plugins
}

/// Complaints about plugin files, to show once at startup.
pub fn problems() -> &'static [String] {
    &registry().problems
}

pub fn find(id: &str) -> Option<&'static Plugin> {
    registry().plugins.iter().find(|p| p.id == id)
}

/// Whether `id` is on. Takes `python` and `python/ruff` both; a server inside a
/// plugin that is off is off, whatever the settings say about it on its own.
pub fn is_on(id: &str) -> bool {
    let chosen = chosen().read().unwrap_or_else(|e| e.into_inner());
    decides(id, &chosen, |id| find(id).is_none_or(|p| p.on_by_default))
}

/// The rule, with the two things it consults handed in so a test can try one
/// without touching what the rest of the editor is looking at.
fn decides(
    id: &str,
    chosen: &BTreeMap<String, bool>,
    by_default: impl Fn(&str) -> bool + Copy,
) -> bool {
    if let Some((plugin, _)) = id.split_once('/')
        && !decides(plugin, chosen, by_default)
    {
        return false;
    }
    match chosen.get(id) {
        Some(said) => *said,
        // Nothing said about it. A plugin decides for itself; a server inside
        // one is on with it.
        None => by_default(id),
    }
}

/// Turn one on or off, and say what the settings file should now hold.
///
/// Only what differs from the default is written back, so a settings file says
/// what you decided rather than listing every plugin that ships.
pub fn set(id: &str, on: bool, settings: &mut BTreeMap<String, bool>) {
    let default = find(id).is_none_or(|p| p.on_by_default);
    let mut map = chosen().write().unwrap_or_else(|e| e.into_inner());
    if on == default {
        map.remove(id);
        settings.remove(id);
    } else {
        map.insert(id.to_string(), on);
        settings.insert(id.to_string(), on);
    }
}

/// The plugins that are on, in merge order — which is what the language
/// registry is built out of.
pub fn active() -> impl Iterator<Item = &'static Plugin> {
    all().iter().filter(|p| is_on(&p.id))
}

// ---- Reading them ----

/// The plugins that ship, in the order they are merged. A language is defined
/// once, so the order matters only for a plugin that adds to one — which is
/// exactly what the language servers below do, and why they come second.
macro_rules! built_in {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("plugins/", $name, ".json")))),*]
    };
}

/// What a language *is*: how to colour it, how to comment it out, what its
/// files are called. Nothing here runs a program.
const LANGUAGES: &[(&str, &str)] = built_in![
    "rust",
    "python",
    "javascript",
    "typescript",
    "tsx",
    "go",
    "c",
    "cpp",
    "csharp",
    "java",
    "bash",
    "json",
    "toml",
    "yaml",
    "markdown",
    "html",
    "css",
    "dockerfile",
    "make",
    "git",
];

/// The language servers do not ship in the binary. They live in a package
/// repository — see [`crate::repo`] — and are fetched, which is what makes
/// `pyright` a plugin in the ordinary sense rather than a special case with a
/// switch: one directory, one manifest, one version, fetched and updated by
/// the same machinery as anything anybody else publishes.
///
/// What is left built in is what a language *is*: how to colour it, how to
/// comment it out, and which files are one. Those need nothing fetched and
/// nothing running, so a textfold that has never seen a network still opens a
/// Rust file and colours it, which is the promise worth keeping offline.
///
/// The two halves were already separate before this — a language plugin and a
/// server plugin are different files with different ids — so nothing about
/// how they are read changed. Only where they are read from.
fn load() -> Registry {
    let mut it = Registry {
        plugins: Vec::new(),
        problems: Vec::new(),
    };

    for (id, text) in LANGUAGES.iter() {
        let mut file: FilePlugin = serde_json::from_str(text)
            .expect("the plugins textfold ships are checked by a test");
        // What ships is settable too. There is no reason a language built into
        // the binary should be the one thing you cannot have an opinion about.
        let (said, problem) = read_override(id);
        it.problems.extend(problem);
        if let Some(said) = said {
            file.apply_override(said);
        }
        let (plugin, problems) = file.into_plugin(id, Source::BuiltIn);
        debug_assert!(problems.is_empty(), "{id}: {problems:?}");
        it.add(plugin);
    }

    let Some(dir) = crate::config::config_dir() else {
        return it;
    };
    for (id, path) in manifests(&dir.join("plugins")) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<FilePlugin>(&text) {
                Ok(mut file) => {
                    // Your settings, over the top of what it shipped — see
                    // [`settings_dir`]. Applied to the manifest as read rather
                    // than to the plugin afterwards, so that everything
                    // downstream sees one plugin and none of it has to know
                    // there were two files.
                    let id = file
                        .id
                        .as_deref()
                        .map(|said| said.trim().to_lowercase())
                        .filter(|said| !said.is_empty())
                        .unwrap_or_else(|| id.clone());
                    let (said, problem) = read_override(&id);
                    it.problems.extend(problem);
                    if let Some(said) = said {
                        file.apply_override(said);
                    }
                    let (plugin, problems) = file.into_plugin(&id, Source::File(path));
                    it.problems.extend(problems);
                    it.add(plugin);
                }
                Err(e) => it.problems.push(format!("{id}: {}", said(&e))),
            },
            Err(e) => it.problems.push(format!("{}: {e}", path.display())),
        }
    }

    // The old file, which is still the shortest way to change one thing about
    // one language. It is read last so that it wins, and it is a plugin like
    // any other so that it shows up in the list rather than being invisible
    // magic.
    let path = dir.join("languages.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        match serde_json::from_str::<FilePlugin>(&text) {
            Ok(file) => {
                let (mut plugin, problems) = file.into_plugin("local", Source::File(path));
                it.problems.extend(problems);
                plugin.name = "Your languages.json".into();
                plugin.about.get_or_insert_with(|| {
                    "What your own languages.json says, on top of the rest".into()
                });
                it.add(plugin);
            }
            Err(e) => it.problems.push(format!("languages.json: {}", said(&e))),
        }
    }
    it
}

// ---------------------------------------------------------------------------
// What you have said about a plugin
// ---------------------------------------------------------------------------

/// Where your own settings for a plugin go: one file per plugin, by id.
///
/// **Not inside the plugin.** A plugin's directory is replaced whole when it
/// is updated — that is what updating is — so anything written in there is
/// gone the next time a newer version arrives. Settings that a package
/// manager destroys are settings nobody can afford to write, and an editor
/// that punished you for configuring it would not deserve to be configured.
///
/// So yours are a layer *over* the manifest rather than an edit *to* it,
/// which is how Sublime Text has always done this and is the part of it worth
/// copying: the plugin ships its defaults, you keep a file of what you
/// disagree with, and an update changes the first without touching the second.
pub fn settings_dir() -> Option<PathBuf> {
    Some(crate::config::config_dir()?.join("plugin-settings"))
}

/// Your file for one plugin, whether or not it exists yet.
pub fn settings_path(id: &str) -> Option<PathBuf> {
    // An id is a lowercased word from a manifest, but a manifest is a thing
    // people write, and a `../` in one must not name a file elsewhere.
    let safe = id.replace(['/', '\\'], "-");
    let safe = safe.trim_matches('.');
    (!safe.is_empty()).then(|| settings_dir().map(|d| d.join(format!("{safe}.json"))))?
}

/// What you have said about one plugin, if anything.
///
/// The shape mirrors the parts of a manifest that are *configuration* rather
/// than *identity*: what a plugin's own program is told about itself, and what
/// each of its servers is told. Nothing here can change what a plugin is —
/// its id, its commands, what it says it needs — because those are the
/// plugin, not your opinion of it.
///
/// Nor whether it is *on*. That is asked and answered in `config.json` under
/// `plugins`, which is where the list with the switches in it writes what you
/// chose. One question with two places to answer it is worse than one place
/// that is slightly further away.
#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileOverride {
    /// Notes to yourself. JSON has nowhere to put a comment.
    #[serde(default, rename = "_about")]
    _about: Option<serde_json::Value>,
    /// Merged over what the plugin's own program is told at `initialize`.
    #[serde(default)]
    settings: Option<serde_json::Value>,
    /// Merged over what one of its language servers is told, by server name.
    #[serde(default)]
    servers: BTreeMap<String, FileServerOverride>,
    /// The same for its debug adapters, by adapter name. This is where the
    /// arguments a program is debugged with go, and where you point an
    /// adapter at a different interpreter from the one it shipped with.
    #[serde(default)]
    debuggers: BTreeMap<String, FileDebuggerOverride>,
    /// The same for the programs it runs on your files, by tool name.
    ///
    /// Which matters most for a build. What textfold ships for C is `cc -g -o
    /// main main.c`, because one file is what somebody has when they first
    /// press the key and a `Makefile` is not something an editor may assume.
    /// A project with more than one file in it has a build of its own, and
    /// this is where you say so: `{"tools": {"cc": {"command": "make",
    /// "args": []}}}`.
    #[serde(default)]
    tools: BTreeMap<String, FileToolOverride>,
}

/// What you have said about one language server inside a plugin.
#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileServerOverride {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    roots: Option<Vec<String>>,
    #[serde(default)]
    settings: Option<serde_json::Value>,
    #[serde(default)]
    init_options: Option<serde_json::Value>,
    /// Merged key by key, so naming one variable does not drop the others.
    #[serde(default)]
    env: BTreeMap<String, String>,
}

impl FileServerOverride {
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }
    pub fn args(&self) -> Option<&[String]> {
        self.args.as_deref()
    }
    pub fn roots(&self) -> Option<&[String]> {
        self.roots.as_deref()
    }
    pub fn settings(&self) -> Option<&serde_json::Value> {
        self.settings.as_ref()
    }
    pub fn init_options(&self) -> Option<&serde_json::Value> {
        self.init_options.as_ref()
    }
    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }
}

/// What you have said about one debug adapter inside a plugin.
///
/// `launch` is the field this exists for: it is where you say which arguments
/// your program takes, or which port to attach to, without copying the rest of
/// what the plugin shipped.
#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileDebuggerOverride {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    roots: Option<Vec<String>>,
    /// Merged key by key, so naming one field leaves the rest as it shipped.
    #[serde(default)]
    launch: Option<serde_json::Value>,
    /// The same, for attaching to something already running.
    #[serde(default)]
    attach: Option<serde_json::Value>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

impl FileDebuggerOverride {
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }
    pub fn args(&self) -> Option<&[String]> {
        self.args.as_deref()
    }
    pub fn roots(&self) -> Option<&[String]> {
        self.roots.as_deref()
    }
    pub fn launch(&self) -> Option<&serde_json::Value> {
        self.launch.as_ref()
    }
    pub fn attach(&self) -> Option<&serde_json::Value> {
        self.attach.as_ref()
    }
    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }
}

/// What you have said about one tool inside a plugin.
///
/// `command` and `args` are the pair this exists for: a tool is a program and
/// a list of arguments, and the two are very often changed together — the
/// project that builds with `make` wants no `-g -O0 -o` either. So `args`
/// replaces rather than merging, exactly as a server's does.
#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileToolOverride {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    roots: Option<Vec<String>>,
    /// How to read a line it printed as a problem. Worth having here because
    /// a different program prints its complaints differently, and a pattern
    /// left behind from the one it replaced finds nothing at all.
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    on_save: Option<bool>,
}

impl FileToolOverride {
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }
    pub fn args(&self) -> Option<&[String]> {
        self.args.as_deref()
    }
    pub fn roots(&self) -> Option<&[String]> {
        self.roots.as_deref()
    }
    pub fn pattern(&self) -> Option<&str> {
        self.pattern.as_deref()
    }
    pub fn on_save(&self) -> Option<bool> {
        self.on_save
    }
}

/// The manifest a plugin ships, as text.
///
/// Read afresh rather than printed back from the registry, because the
/// registry has already had your own settings merged into it — and the whole
/// point of showing this is that it is the half you did *not* write.
pub fn shipped_manifest(plugin: &Plugin) -> String {
    match &plugin.source {
        Source::File(path) => std::fs::read_to_string(path)
            .unwrap_or_else(|e| format!("{}: {e}\n", path.display())),
        Source::BuiltIn => LANGUAGES
            .iter()
            .find(|(id, _)| *id == plugin.id)
            .map(|(_, text)| (*text).to_string())
            .unwrap_or_else(|| format!("{} is built in and has no file.\n", plugin.id)),
    }
}

/// A first draft of your own settings file for a plugin: the shape of the
/// thing, with the names it actually has in it.
///
/// A file made empty is a blank page and a guess. This is the difference
/// between "there is somewhere to write this" and "and here is what goes in
/// it" — and the manifest opened beside it answers the rest.
pub fn settings_stub(plugin: &Plugin) -> String {
    let mut about = vec![
        format!("Your settings for {}, laid over what it ships.", plugin.id),
        String::new(),
        "The manifest beside this is what you are overriding. That file is replaced".into(),
        "whole every time the plugin updates; this one is never touched, which is".into(),
        "the point of it being a separate file.".into(),
        String::new(),
        "Objects merge key by key, so saying something about one setting leaves the".into(),
        "rest exactly as it shipped. Lists and everything else replace.".into(),
    ];
    let mut body = serde_json::Map::new();

    if plugin.host.is_some() {
        about.push(String::new());
        about.push("`settings` is what this plugin's own program is told at startup.".into());
        body.insert("settings".into(), serde_json::json!({}));
    }
    if !plugin.servers.is_empty() {
        about.push(String::new());
        about.push("`servers` is by server name. Each may say `settings`,".into());
        about.push("`init_options`, `env`, `args`, `roots` or `command`.".into());
        let mut servers = serde_json::Map::new();
        for server in &plugin.servers {
            servers.insert(server.name.clone(), serde_json::json!({ "settings": {} }));
        }
        body.insert("servers".into(), serde_json::Value::Object(servers));
    }
    if !plugin.debuggers.is_empty() {
        about.push(String::new());
        about.push("`debuggers` is by adapter name. Each may say `launch`, `env`,".into());
        about.push("`attach`, `args`, `roots` or `command` — `launch` is where the".into());
        about.push("arguments your program is debugged with go.".into());
        let mut debuggers = serde_json::Map::new();
        for debugger in &plugin.debuggers {
            debuggers.insert(debugger.name.clone(), serde_json::json!({ "launch": {} }));
        }
        body.insert("debuggers".into(), serde_json::Value::Object(debuggers));
    }
    if !plugin.tools.is_empty() {
        about.push(String::new());
        about.push("`tools` is by tool name. Each may say `command`, `args`,".into());
        about.push("`roots`, `pattern` or `on_save` — this is where a project with".into());
        about.push("a build of its own says so.".into());
        let mut tools = serde_json::Map::new();
        for tool in &plugin.tools {
            tools.insert(tool.name.clone(), serde_json::json!({ "args": tool.args }));
        }
        body.insert("tools".into(), serde_json::Value::Object(tools));
    }
    if body.is_empty() {
        about.push(String::new());
        about.push("This plugin runs no program of its own and no language server,".into());
        about.push("so there is nothing here to tell it. Whether it is on at all is".into());
        about.push("a different question, asked in `plugins` — the palette, or".into());
        about.push("`plugins` in config.json.".into());
    }

    let mut out = serde_json::Map::new();
    out.insert("_about".into(), serde_json::json!(about));
    out.extend(body);
    serde_json::to_string_pretty(&serde_json::Value::Object(out))
        .unwrap_or_else(|_| "{}".into())
        + "\n"
}

/// Read what you have said about a plugin.
fn read_override(id: &str) -> (Option<FileOverride>, Option<String>) {
    let Some(path) = settings_path(id) else {
        return (None, None);
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (None, None);
    };
    // An empty file is somebody who opened it and has not written anything
    // yet, which is not a mistake worth complaining about.
    if text.trim().is_empty() {
        return (None, None);
    }
    match serde_json::from_str::<FileOverride>(&text) {
        Ok(said) => (Some(said), None),
        Err(e) => (None, Some(format!("{}: {}", path.display(), said(&e)))),
    }
}

/// Put one JSON value on top of another.
///
/// Objects merge key by key, and everything else is replaced whole. That is
/// the rule Sublime uses and the one people expect: saying something about
/// `java.format` should not throw away `java.completion`, and giving a list
/// should give *that* list rather than appending to one you cannot see.
fn merge(base: &mut serde_json::Value, over: &serde_json::Value) {
    match (base, over) {
        (serde_json::Value::Object(base), serde_json::Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, over) => *base = over.clone(),
    }
}

/// Merge one value into an optional one, making it if there was none.
pub fn merge_into(base: &mut Option<serde_json::Value>, over: Option<&serde_json::Value>) {
    let Some(over) = over else { return };
    match base {
        Some(base) => merge(base, over),
        None => *base = Some(over.clone()),
    }
}

impl FilePlugin {
    /// Lay your settings over what the plugin shipped.
    fn apply_override(&mut self, said: FileOverride) {
        if let Some(host) = &mut self.host {
            merge_into(&mut host.settings, said.settings.as_ref());
        }
        for tool in &mut self.tools {
            // By the name the command palette shows, which is the name
            // lowercased — so somebody who writes `CC` in their settings gets
            // the tool they plainly meant.
            if let Some(over) = said.tools.get(&tool.name.trim().to_lowercase()) {
                tool.apply_override(over);
            }
        }
        for language in self.languages.values_mut() {
            for server in language.servers.iter_mut().flatten() {
                if let Some(over) = said.servers.get(&server.plugin_name()) {
                    server.apply_override(over);
                }
            }
            for debugger in language.debuggers.iter_mut().flatten() {
                if let Some(over) = said.debuggers.get(&debugger.plugin_name()) {
                    debugger.apply_override(over);
                }
            }
        }
    }
}

/// Read a manifest that has not been installed yet, as the plugin it would be.
///
/// `at` is where its manifest is going to live rather than where it is being
/// read from, because a `${plugin}` in it names the installed copy — an
/// install step pointing at the directory somebody happened to be installing
/// out of would work once and never again.
pub fn read(
    manifest: &std::path::Path,
    id: &str,
    at: PathBuf,
) -> Result<(Plugin, Vec<String>), String> {
    let text =
        std::fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let file: FilePlugin = serde_json::from_str(&text)
        .map_err(|e| format!("{}: {}", manifest.display(), said(&e)))?;
    Ok(file.into_plugin(id, Source::File(at)))
}

/// Just the message, without the "at line 4 column 9" that a person reading a
/// status line cannot do anything with.
fn said(e: &serde_json::Error) -> String {
    let text = e.to_string();
    text.split(" at line ").next().unwrap_or(&text).to_string()
}

/// The manifests in a plugins directory: `name.json`, and `name/plugin.json`
/// for a plugin that has more than one file to its name.
fn manifests(dir: &std::path::Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            let manifest = path.join("plugin.json");
            if manifest.exists() {
                found.push((name, manifest));
            }
        } else if path.extension().is_some_and(|e| e == "json") {
            found.push((name, path));
        }
    }
    // Read in a settled order, so that two plugins touching the same language
    // do the same thing every time rather than whatever the directory listed
    // first today.
    found.sort();
    found
}

impl Registry {
    /// Add one, replacing any plugin already here with the same id. Yours
    /// beating one that ships is the point: `plugins/rust.json` of your own is
    /// how you say what Rust means here.
    fn add(&mut self, plugin: Plugin) {
        match self.plugins.iter().position(|p| p.id == plugin.id) {
            Some(at) => self.plugins[at] = plugin,
            None => self.plugins.push(plugin),
        }
    }
}

/// A manifest, as its file writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilePlugin {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    about: Option<String>,
    /// `"1.2.0"`. What an update is decided by.
    #[serde(default)]
    version: Option<String>,
    /// Whether it is on when nobody has said. Absent means on.
    #[serde(default)]
    enabled: Option<bool>,
    /// Notes to whoever opens the file. JSON has nowhere to put a comment, so
    /// it gets a key and is read into nothing.
    #[serde(default, rename = "_about")]
    _about: Option<serde_json::Value>,
    #[serde(default)]
    languages: BTreeMap<String, FileLang>,
    #[serde(default)]
    tools: Vec<FileTool>,
    #[serde(default)]
    host: Option<FileHost>,
    #[serde(default)]
    commands: Vec<FileCommand>,
    #[serde(default)]
    panels: Vec<FileCommand>,
    #[serde(default)]
    themes: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    keys: BTreeMap<String, Vec<String>>,
    /// Programs it needs on the `PATH`.
    #[serde(default)]
    needs: Vec<String>,
    #[serde(default)]
    install: Vec<FileStep>,
    #[serde(default)]
    uninstall: Vec<FileStep>,
    /// Where to get it by hand, when nothing here can.
    #[serde(default)]
    see: Option<String>,
}

/// A step of an installer, as its file writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStep {
    #[serde(default)]
    about: Option<String>,
    /// The program and its arguments, as a list. A string would need a shell
    /// to take it apart again, and a shell is a thing that can be surprised.
    run: Vec<String>,
    #[serde(default)]
    unless: Option<String>,
    /// A file that has to exist for this step to be worth running.
    #[serde(default)]
    when: Option<String>,
    /// `"macos"`, `"linux"`, `"windows"`, or a list of them. Absent means any.
    #[serde(default)]
    os: Option<OneOrMore>,
    /// `"x86_64"`, `"aarch64"`, or a list. Absent means any.
    #[serde(default)]
    arch: Option<OneOrMore>,
    /// Whether it installs outside textfold's own directory.
    #[serde(default)]
    system: Option<bool>,
}

/// A field that is usually one thing and occasionally several. `"linux"` and
/// `["linux", "macos"]` both mean what they look like they mean.
#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMore {
    One(String),
    Several(Vec<String>),
}

impl OneOrMore {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMore::One(one) => vec![one],
            OneOrMore::Several(several) => several,
        }
    }
}

/// A host, as its file writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileHost {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    activate: Vec<String>,
    #[serde(default)]
    wants_buffers: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

/// A contributed command, as its file writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCommand {
    name: String,
    #[serde(default)]
    about: Option<String>,
    /// `"left"`, `"right"` or `"bottom"` — a panel that is part of the
    /// editor's shape. Absent means a tab. Meaningless on a command.
    #[serde(default)]
    dock: Option<String>,
    /// How wide a docked panel is in columns, or how tall in rows. Absent
    /// means a sensible one for the edge.
    #[serde(default)]
    size: Option<u16>,
    /// `"passive"`, `"edits"` or `"types"`. Absent means passive, since a
    /// command that changes the text is the rarer kind and saying so should
    /// be deliberate — the answer decides whether it may run on a read-only
    /// file, and guessing wrong the other way would let one through.
    #[serde(default)]
    behaviour: Option<String>,
    #[serde(default)]
    languages: Vec<String>,
}

/// A tool, as its file writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTool {
    name: String,
    #[serde(default)]
    about: Option<String>,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    stdin: Option<bool>,
    /// `"replace"`, `"show"`, `"problems"`, or `"ignore"`. Absent means
    /// `"replace"`, since a formatter is what most tools are.
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    on_save: Option<bool>,
    /// Whether this is the language's build — the one F5 runs before it
    /// debugs. See [`Tool::builds`].
    #[serde(default)]
    builds: Option<bool>,
}

impl FileTool {
    /// Lay somebody's own settings over what the plugin shipped, by the same
    /// rules a server's get: what is named replaces, what is not is left
    /// exactly as it was.
    fn apply_override(&mut self, said: &FileToolOverride) {
        if let Some(command) = said.command() {
            self.command = command.to_string();
        }
        if let Some(args) = said.args() {
            self.args = args.to_vec();
        }
        if let Some(roots) = said.roots() {
            self.roots = roots.to_vec();
        }
        if let Some(pattern) = said.pattern() {
            self.pattern = Some(pattern.to_string());
        }
        if let Some(on_save) = said.on_save() {
            self.on_save = Some(on_save);
        }
    }
}

impl FilePlugin {
    /// Turn a manifest into a plugin, along with anything wrong with it worth
    /// telling somebody about. A manifest is written by hand, so a mistake in
    /// one is a thing to say out loud rather than a thing to swallow.
    pub fn into_plugin(self, fallback_id: &str, source: Source) -> (Plugin, Vec<String>) {
        let mut problems = Vec::new();
        let id = self
            .id
            .map(|id| id.trim().to_lowercase())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| fallback_id.to_lowercase());
        let mut servers = Vec::new();
        let mut debuggers = Vec::new();
        // The same server written out for three languages is one switch, not
        // three: `tsserver` is on or off, and which of JavaScript, TypeScript
        // and TSX you happen to be looking at is not a thing anybody wants to
        // decide separately. The same goes for an adapter — `lldb-dap` is C,
        // C++ and Rust — so both lists are gathered the same way.
        let gather = |into: &mut Vec<ServerEntry>, name: String, command: &str, language: &str| {
            if let Some(seen) = into.iter_mut().find(|s: &&mut ServerEntry| s.name == name) {
                seen.languages.push(language.to_string());
                return;
            }
            into.push(ServerEntry {
                id: server_id(&id, &name),
                name,
                command: command.to_string(),
                languages: vec![language.to_string()],
            });
        };
        for (language, def) in &self.languages {
            for server in def.servers.iter().flatten() {
                gather(&mut servers, server.plugin_name(), &server.command, language);
            }
            for debugger in def.debuggers.iter().flatten() {
                gather(&mut debuggers, debugger.plugin_name(), &debugger.runs(), language);
            }
        }
        let tools = self
            .tools
            .into_iter()
            .filter(|t| !t.command.trim().is_empty() && !t.name.trim().is_empty())
            .map(|t| {
                let name = t.name.trim().to_lowercase();
                let output = match t.output.as_deref().map(str::trim).unwrap_or("replace") {
                    "show" => Output::Show,
                    "problems" | "diagnostics" => Output::Problems,
                    "ignore" | "none" => Output::Ignore,
                    _ => Output::Replace,
                };
                Tool {
                    id: format!("{id}/{name}"),
                    about: t
                        .about
                        .unwrap_or_else(|| format!("Run {} on this file", t.command)),
                    command: t.command,
                    args: t.args,
                    languages: t.languages.iter().map(|l| l.to_lowercase()).collect(),
                    roots: if t.roots.is_empty() {
                        vec![".git".into()]
                    } else {
                        t.roots
                    },
                    // A formatter reads the buffer; anything else is usually
                    // about the file as it is on disk.
                    stdin: t.stdin.unwrap_or(output == Output::Replace),
                    on_save: t.on_save.unwrap_or(false),
                    builds: t.builds.unwrap_or(false),
                    pattern: t.pattern,
                    output,
                    name,
                }
            })
            .collect();
        // A plugin's program lives beside its manifest, but it is *run* in
        // the root of the project it is being asked about — so a plugin has no
        // way to name its own script without this. `${plugin}` is that: the
        // directory the manifest was read from.
        let beside = match &source {
            Source::File(path) => path.parent().map(|d| d.display().to_string()),
            Source::BuiltIn => None,
        };
        let fill = |text: &str| match &beside {
            Some(dir) => text.replace("${plugin}", dir),
            None => text.to_string(),
        };

        let host = self.host.map(|h| Host {
            activate: h
                .activate
                .iter()
                .filter_map(|text| match Activate::parse(text) {
                    Some(one) => Some(one),
                    None => {
                        problems.push(format!("{id}: {text:?} is not something to start on"));
                        None
                    }
                })
                .collect(),
            roots: if h.roots.is_empty() {
                vec![".git".into()]
            } else {
                h.roots
            },
            wants_buffers: h.wants_buffers.iter().map(|l| l.to_lowercase()).collect(),
            command: fill(&h.command),
            args: h.args.iter().map(|a| fill(a)).collect(),
            env: h.env.iter().map(|(k, v)| (k.clone(), fill(v))).collect(),
            settings: h.settings,
        });

        let mut commands = Vec::new();
        // Panels first, so that a plugin declaring both keeps them apart in
        // the palette by name rather than by which list it wrote them in.
        let declared = self
            .commands
            .into_iter()
            .map(|c| (c, false))
            .chain(self.panels.into_iter().map(|c| (c, true)));
        for (c, opens_panel) in declared {
            let name = c.name.trim().to_lowercase();
            if name.is_empty() {
                continue;
            }
            // A command with nothing to send it to would sit in the palette
            // and do nothing, which is worse than not being there.
            if host.is_none() {
                problems.push(format!("{id}: {name} has no host to run it"));
                continue;
            }
            // A `dock` on something that is not a panel has nothing to
            // place, and silently ignoring it would leave somebody looking
            // for a sidebar that was never going to appear.
            let dock = match (&c.dock, opens_panel) {
                (None, _) => None,
                (Some(_), false) => {
                    problems.push(format!("{id}: {name} is a command, so it cannot be docked"));
                    None
                }
                (Some(where_), true) => match crate::view::Edge::parse(where_) {
                    Some(edge) => Some(crate::view::Dock::new(edge, c.size)),
                    None => {
                        problems.push(format!(
                            "{id}: {name} cannot be docked {where_:?} — left, right or bottom"
                        ));
                        None
                    }
                },
            };
            commands.push(Command {
                opens_panel,
                dock,
                behaviour: match c.behaviour.as_deref().map(str::trim) {
                    Some("edits") => crate::cmd::Behaviour::Edits,
                    Some("types") => crate::cmd::Behaviour::Types,
                    _ => crate::cmd::Behaviour::Passive,
                },
                about: c.about.unwrap_or_else(|| format!("Run {name}")),
                languages: c.languages.iter().map(|l| l.to_lowercase()).collect(),
                id: format!("{id}/{name}"),
                plugin: id.clone(),
                name,
            });
        }

        // A step that names no program is a step that cannot be run, and one
        // left in the list would stop an install halfway through for no
        // reason anybody could see.
        let mut steps = |from: Vec<FileStep>, what: &str| -> Vec<Step> {
            from.into_iter()
                .filter_map(|s| {
                    let run: Vec<String> = s
                        .run
                        .into_iter()
                        .filter(|word| !word.trim().is_empty())
                        .map(|word| fill(&word))
                        .collect();
                    if run.is_empty() {
                        problems.push(format!("{id}: a {what} step says nothing to run"));
                        return None;
                    }
                    Some(Step {
                        about: s.about.unwrap_or_else(|| run.join(" ")),
                        unless: s.unless.filter(|u| !u.trim().is_empty()).map(|u| fill(&u)),
                        when: s.when.filter(|w| !w.trim().is_empty()).map(|w| fill(&w)),
                        os: s
                            .os
                            .map(OneOrMore::into_vec)
                            .unwrap_or_default()
                            .iter()
                            .map(|o| o.trim().to_lowercase())
                            .collect(),
                        arch: s
                            .arch
                            .map(OneOrMore::into_vec)
                            .unwrap_or_default()
                            .iter()
                            .map(|a| a.trim().to_lowercase())
                            .collect(),
                        system: s.system.unwrap_or(false),
                        run,
                    })
                })
                .collect()
        };
        let install = steps(self.install, "install");
        let uninstall = steps(self.uninstall, "uninstall");

        // A grammar a plugin brings with it lives beside its manifest, and the
        // manifest cannot know where that is — so it says `${plugin}`, as a
        // plugin's own program does, and this is where that becomes a path.
        // Without it a plugin can only name a grammar somebody installed by
        // hand, which is the difference between "plugins can add a language"
        // and "plugins can add a language you have already set up yourself".
        let mut languages = self.languages;
        for def in languages.values_mut() {
            def.fill_paths(fill);
        }

        let plugin = Plugin {
            name: self.name.unwrap_or_else(|| id.clone()),
            about: self.about,
            version: self.version.filter(|v| !v.trim().is_empty()),
            on_by_default: self.enabled.unwrap_or(true),
            languages,
            themes: self.themes,
            keys: self.keys,
            needs: self
                .needs
                .into_iter()
                .filter(|n| !n.trim().is_empty())
                .map(|n| fill(&n))
                .collect(),
            see: self.see.filter(|s| !s.trim().is_empty()),
            install,
            uninstall,
            servers,
            debuggers,
            tools,
            host,
            commands,
            source,
            id,
        };
        (plugin, problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_plugins_textfold_ships_all_read() {
        let registry = load();
        for (id, _) in LANGUAGES.iter() {
            let plugin = registry
                .plugins
                .iter()
                .find(|p| &p.id == id)
                .unwrap_or_else(|| panic!("{id} did not load"));
            assert!(!plugin.name.is_empty(), "{id} has no name");
            assert!(plugin.about.is_some(), "{id} says nothing about itself");
            assert!(
                !plugin.languages.is_empty(),
                "{id} contributes no languages"
            );
        }
    }

    #[test]
    fn a_language_that_ships_says_what_the_language_is_and_runs_nothing() {
        // The split the language servers moved out under. A language plugin
        // that named a program again would be the thing this went to the
        // trouble of undoing.
        let registry = load();
        for (id, _) in LANGUAGES {
            let plugin = registry.plugins.iter().find(|p| &p.id == id).unwrap();
            assert!(
                plugin.servers.is_empty(),
                "{id} still has a language server written into it"
            );
            assert!(plugin.needs.is_empty(), "{id} needs a program to be a language");
        }
    }

    #[test]
    fn nothing_that_ships_in_the_binary_needs_fetching_to_work() {
        // The line the split is drawn along, and the promise worth keeping
        // offline: a textfold that has never seen a network opens a Rust file
        // and colours it. Everything that has to be downloaded lives in a
        // package repository, which is where the language servers went.
        let registry = load();
        for (id, _) in LANGUAGES {
            let plugin = registry.plugins.iter().find(|p| &p.id == id).unwrap();
            assert!(plugin.needs.is_empty(), "{id} needs a program fetching");
            assert!(plugin.install.is_empty(), "{id} has something to install");
            assert!(plugin.host.is_none(), "{id} brings a program with it");
        }
    }

    #[test]
    fn a_plugin_that_is_one_server_is_named_once() {
        // `pyright/pyright` is a name nobody would write, and it would be the
        // name in everybody's settings file. Read here rather than out of the
        // registry, because the servers are fetched now rather than built in.
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"pyright","languages":{"python":{"servers":[
                 {"name":"pyright","command":"pyright-langserver"}]}}}"#,
        )
        .unwrap();
        let (plugin, _) = file.into_plugin("pyright", Source::BuiltIn);
        let ids: Vec<&str> = plugin.servers.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["pyright"]);

        // And one server for three languages is one switch, not three.
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"tsserver","languages":{
                 "javascript":{"servers":[{"name":"tsserver","command":"typescript-language-server"}]},
                 "typescript":{"servers":[{"name":"tsserver","command":"typescript-language-server"}]},
                 "tsx":{"servers":[{"name":"tsserver","command":"typescript-language-server"}]}}}"#,
        )
        .unwrap();
        let (plugin, _) = file.into_plugin("tsserver", Source::BuiltIn);
        assert_eq!(plugin.servers.len(), 1);
        assert_eq!(plugin.servers[0].id, "tsserver");
        assert_eq!(plugin.servers[0].for_what(), "javascript, tsx, typescript");
    }

    #[test]
    fn a_version_is_read_and_a_plugin_without_one_has_none() {
        // What an update is decided by. A plugin that declines to number
        // itself is one nothing is ever an update to, which is the safe
        // answer rather than reinstalling it forever.
        let file: FilePlugin =
            serde_json::from_str(r#"{"id":"zls","version":"1.2.0"}"#).unwrap();
        let (plugin, _) = file.into_plugin("zls", Source::BuiltIn);
        assert_eq!(plugin.version.as_deref(), Some("1.2.0"));
        assert_eq!(plugin.version_label().as_deref(), Some("v1.2.0"));

        let file: FilePlugin = serde_json::from_str(r#"{"id":"zls","version":"  "}"#).unwrap();
        assert_eq!(file.into_plugin("zls", Source::BuiltIn).0.version, None);
    }

    #[test]
    fn a_settings_file_from_before_the_servers_moved_still_says_what_it_said() {
        let mut settings = BTreeMap::from([
            ("python/ruff".to_string(), false),
            ("rust".to_string(), false),
        ]);
        rename(&mut settings);
        assert_eq!(settings.get("ruff"), Some(&false));
        assert_eq!(settings.get("python/ruff"), None);
        // And an id that was never renamed is left exactly as it was.
        assert_eq!(settings.get("rust"), Some(&false));

        // Running it over a file that has already been brought up to date
        // changes nothing, which is what makes it safe to do every time.
        let again = settings.clone();
        rename(&mut settings);
        assert_eq!(settings, again);
    }

    #[test]
    fn what_a_new_id_already_says_beats_what_an_old_one_said() {
        // Three old ids point at `tsserver`. Whatever the file says about the
        // new name is what it meant most recently, so that wins.
        let mut settings = BTreeMap::from([
            ("typescript/tsserver".to_string(), false),
            ("tsserver".to_string(), true),
        ]);
        rename(&mut settings);
        assert_eq!(settings.get("tsserver"), Some(&true));
    }

    #[test]
    fn an_installer_is_read_as_a_list_of_things_to_run() {
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"zls","needs":["zls"],"see":"https://example.invalid",
                "install":[{"about":"zls, with brew","run":["brew","install","zls"],
                            "unless":"zls"},
                           {"run":["cargo","install","zls"]}],
                "uninstall":[{"run":["brew","uninstall","zls"]}]}"#,
        )
        .unwrap();
        let (plugin, problems) = file.into_plugin("zls", Source::BuiltIn);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plugin.needs, ["zls"]);
        assert_eq!(plugin.install.len(), 2);
        assert_eq!(plugin.install[0].about, "zls, with brew");
        assert_eq!(plugin.install[0].unless.as_deref(), Some("zls"));
        // A step that did not say what it is for is described by what it runs,
        // which is the honest answer and the one a person can act on.
        assert_eq!(plugin.install[1].about, "cargo install zls");
        assert_eq!(plugin.install[1].unless, None);
        assert_eq!(plugin.uninstall.len(), 1);
        assert_eq!(plugin.see.as_deref(), Some("https://example.invalid"));
    }

    #[test]
    fn a_step_with_nothing_to_run_is_dropped_and_said_so() {
        // It would otherwise stop an install halfway through for a reason
        // nobody watching could see.
        let file: FilePlugin =
            serde_json::from_str(r#"{"id":"p","install":[{"run":["  "]}]}"#).unwrap();
        let (plugin, problems) = file.into_plugin("p", Source::BuiltIn);
        assert!(plugin.install.is_empty());
        assert_eq!(problems, ["p: a install step says nothing to run"]);
    }

    #[test]
    fn a_plugin_that_needs_nothing_is_ready_and_one_that_needs_the_impossible_is_not() {
        let file: FilePlugin = serde_json::from_str(r#"{"id":"p"}"#).unwrap();
        assert!(file.into_plugin("p", Source::BuiltIn).0.is_ready());

        let file: FilePlugin =
            serde_json::from_str(r#"{"id":"p","needs":["a-program-nobody-wrote"]}"#).unwrap();
        let (plugin, _) = file.into_plugin("p", Source::BuiltIn);
        assert!(!plugin.is_ready());
        assert_eq!(plugin.missing(), ["a-program-nobody-wrote"]);
    }

    #[test]
    fn a_plugin_of_your_own_replaces_one_that_ships() {
        let mut registry = Registry {
            plugins: Vec::new(),
            problems: Vec::new(),
        };
        let ship: FilePlugin =
            serde_json::from_str(r#"{"id":"zig","name":"Zig","languages":{}}"#).unwrap();
        registry.add(ship.into_plugin("zig", Source::BuiltIn).0);
        let mine: FilePlugin =
            serde_json::from_str(r#"{"id":"zig","name":"My Zig","languages":{}}"#).unwrap();
        registry.add(mine.into_plugin("zig", Source::File(PathBuf::from("/tmp/zig.json"))).0);
        assert_eq!(registry.plugins.len(), 1);
        assert_eq!(registry.plugins[0].name, "My Zig");
    }

    #[test]
    fn a_server_inside_a_plugin_that_is_off_is_off() {
        let mut chosen = BTreeMap::new();
        chosen.insert("python".to_string(), false);
        assert!(!decides("python", &chosen, |_| true));
        // Even though nobody said anything about the server itself.
        assert!(!decides("python/ruff", &chosen, |_| true));
        // And even where somebody went out of their way to say it was on: a
        // language nothing is reading is a language nothing lints.
        chosen.insert("python/ruff".to_string(), true);
        assert!(!decides("python/ruff", &chosen, |_| true));
    }

    #[test]
    fn one_server_can_be_switched_off_without_the_language_going_with_it() {
        let mut chosen = BTreeMap::new();
        chosen.insert("python/ruff".to_string(), false);
        assert!(decides("python", &chosen, |_| true));
        assert!(decides("python/pyright", &chosen, |_| true));
        assert!(!decides("python/ruff", &chosen, |_| true));
    }

    #[test]
    fn a_plugin_that_ships_switched_off_stays_off_until_asked_for() {
        let chosen = BTreeMap::new();
        assert!(!decides("sleeping", &chosen, |id| id != "sleeping"));
        let mut chosen = BTreeMap::new();
        chosen.insert("sleeping".to_string(), true);
        assert!(decides("sleeping", &chosen, |id| id != "sleeping"));
    }

    #[test]
    fn a_tool_is_read_with_the_defaults_that_suit_what_it_is() {
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"pytools","tools":[
                 {"name":"fmt","command":"ruff","args":["format","-"]},
                 {"name":"lint","command":"ruff","output":"problems",
                  "pattern":"%f:%l:%c: %m"},
                 {"name":"tests","command":"pytest","output":"show"}]}"#,
        )
        .unwrap();
        let (plugin, problems) = file.into_plugin("pytools", Source::BuiltIn);
        assert!(problems.is_empty(), "{problems:?}");

        let ids: Vec<&str> = plugin.tools.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["pytools/fmt", "pytools/lint", "pytools/tests"]);

        // A tool that says nothing is a formatter, because most of them are —
        // and a formatter reads the buffer rather than the file on disk, so it
        // gets standard input without having to ask.
        assert_eq!(plugin.tools[0].output, Output::Replace);
        assert!(plugin.tools[0].stdin);
        assert_eq!(plugin.tools[0].behaviour(), crate::cmd::Behaviour::Edits);

        // Anything else is about the file as it was saved, so it is not.
        assert_eq!(plugin.tools[1].output, Output::Problems);
        assert!(!plugin.tools[1].stdin);
        assert_eq!(plugin.tools[1].behaviour(), crate::cmd::Behaviour::Passive);
        assert_eq!(plugin.tools[2].output, Output::Show);

        // And with nothing said about languages, it is for any file.
        assert!(plugin.tools[0].wants("python"));
        assert!(plugin.tools[0].wants("rust"));
    }

    #[test]
    fn a_tool_says_which_languages_it_is_for() {
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"p","tools":[{"name":"t","command":"c","languages":["Python"]}]}"#,
        )
        .unwrap();
        let (plugin, _) = file.into_plugin("p", Source::BuiltIn);
        assert!(plugin.tools[0].wants("python"), "the case should not matter");
        assert!(!plugin.tools[0].wants("rust"));
    }

    #[test]
    fn a_plugin_with_a_program_of_its_own_says_what_starts_it() {
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"stm32",
                "host":{"command":"python3","args":["-m","stm"],
                        "activate":["file:**/*.ioc","language:C","command"],
                        "wants_buffers":["C"]},
                "commands":[{"name":"Build","about":"Build it","languages":["C"]}]}"#,
        )
        .unwrap();
        let (plugin, problems) = file.into_plugin("stm32", Source::BuiltIn);
        assert!(problems.is_empty(), "{problems:?}");

        let host = plugin.host.expect("it brought a program");
        assert_eq!(
            host.activate,
            [
                Activate::File("**/*.ioc".into()),
                // Written however you like and read the one way, as
                // everywhere else a language is named.
                Activate::Language("c".into()),
                Activate::Command,
            ]
        );
        assert_eq!(host.wants_buffers, ["c"]);
        // Nothing said about roots means the top of the repository, which is
        // the answer for a plugin that has not thought about it.
        assert_eq!(host.roots, [".git"]);

        assert_eq!(plugin.commands.len(), 1);
        assert_eq!(plugin.commands[0].id, "stm32/build");
        assert_eq!(plugin.commands[0].plugin, "stm32");
        // Passive unless it says otherwise: the answer decides whether it may
        // run on a read-only file, and guessing the other way lets one
        // through.
        assert_eq!(plugin.commands[0].behaviour, crate::cmd::Behaviour::Passive);
        assert!(plugin.commands[0].wants("c"));
        assert!(!plugin.commands[0].wants("rust"));
    }

    #[test]
    fn a_plugin_can_name_its_own_script_without_knowing_where_it_is_installed() {
        // It runs in the project root, not beside its manifest, so without
        // this a plugin could not point at the program it ships with.
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"p","host":{"command":"python3","args":["${plugin}/run.py"]}}"#,
        )
        .unwrap();
        let (plugin, _) = file.into_plugin(
            "p",
            Source::File(PathBuf::from("/home/me/.config/textfold/plugins/p/plugin.json")),
        );
        let host = plugin.host.expect("it brought a program");
        assert_eq!(
            host.args,
            ["/home/me/.config/textfold/plugins/p/run.py".to_string()]
        );
    }

    #[test]
    fn a_panel_says_in_its_manifest_where_it_sits() {
        // Declared rather than announced, so the editor can lay the thing out
        // before the plugin has ever been started.
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"files","host":{"command":"python3"},
                "panels":[{"name":"tree","dock":"left","size":32},
                          {"name":"log","dock":"bottom"},
                          {"name":"report"}]}"#,
        )
        .unwrap();
        let (plugin, problems) = file.into_plugin("files", Source::BuiltIn);
        assert!(problems.is_empty(), "{problems:?}");
        let dock = |name: &str| {
            plugin
                .commands
                .iter()
                .find(|c| c.name == name)
                .and_then(|c| c.dock)
        };
        assert_eq!(
            dock("tree"),
            Some(crate::view::Dock {
                edge: crate::view::Edge::Left,
                size: 32
            })
        );
        // No size means one that suits the edge — columns for a side, rows
        // for the bottom.
        assert_eq!(
            dock("log"),
            Some(crate::view::Dock {
                edge: crate::view::Edge::Bottom,
                size: crate::view::DEFAULT_DOCK_HEIGHT
            })
        );
        // And a panel that says nothing is a tab, which is what they all were.
        assert_eq!(dock("report"), None);
    }

    #[test]
    fn a_panel_docked_somewhere_that_is_not_an_edge_is_a_complaint() {
        // Silently ignoring it would leave somebody looking for a sidebar
        // that was never going to appear.
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"p","host":{"command":"x"},"panels":[{"name":"t","dock":"middle"}]}"#,
        )
        .unwrap();
        let (plugin, problems) = file.into_plugin("p", Source::BuiltIn);
        assert_eq!(plugin.commands[0].dock, None);
        assert_eq!(
            problems,
            [r#"p: t cannot be docked "middle" — left, right or bottom"#]
        );

        // And a command is not a panel, so there is nothing of it to dock.
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"p","host":{"command":"x"},"commands":[{"name":"go","dock":"left"}]}"#,
        )
        .unwrap();
        let (_, problems) = file.into_plugin("p", Source::BuiltIn);
        assert_eq!(problems, ["p: go is a command, so it cannot be docked"]);
    }

    #[test]
    fn your_settings_go_over_the_manifest_rather_than_into_it() {
        // The whole reason this exists: a plugin's directory is replaced whole
        // when it updates, so anything written inside it is destroyed. Yours
        // are a layer over the top, in a file an update never touches.
        let mut file: FilePlugin = serde_json::from_str(
            r#"{"id":"jdtls",
                "languages":{"java":{"servers":[{
                  "name":"jdtls","command":"jdtls","args":["-data","x"],
                  "env":{"JAVA_HOME":"/usr/lib/jvm/21"},
                  "settings":{"java":{
                    "format":{"enabled":true},
                    "maven":{"downloadSources":true}}}}]}}}"#,
        )
        .unwrap();
        let said: FileOverride = serde_json::from_str(
            r#"{"servers":{"jdtls":{
                 "settings":{"java":{"format":{"enabled":false}}},
                 "env":{"JDTLS_JVM_ARGS":"-Xmx2G"}}}}"#,
        )
        .unwrap();
        file.apply_override(said);
        let (plugin, problems) = file.into_plugin("jdtls", Source::BuiltIn);
        assert!(problems.is_empty(), "{problems:?}");

        let server = plugin.languages["java"].servers.as_ref().unwrap()[0].clone();
        let server = server.into_server("jdtls");
        let settings = server.settings.expect("settings");
        // What you said won.
        assert_eq!(settings.pointer("/java/format/enabled"), Some(&json!(false)));
        // And what you did not say is still there — the point of merging key
        // by key rather than replacing the block.
        assert_eq!(
            settings.pointer("/java/maven/downloadSources"),
            Some(&json!(true)),
            "saying one thing threw away the rest: {settings}"
        );
        // The same for the environment, and for everything not mentioned.
        assert_eq!(server.env.get("JDTLS_JVM_ARGS").map(String::as_str), Some("-Xmx2G"));
        assert_eq!(
            server.env.get("JAVA_HOME").map(String::as_str),
            Some("/usr/lib/jvm/21"),
            "naming one variable dropped the others"
        );
        assert_eq!(server.args, ["-data", "x"]);
        assert_eq!(server.command, "jdtls");
    }

    #[test]
    fn a_project_with_a_build_of_its_own_can_say_so() {
        // The one override this had no way of expressing, and the one people
        // will reach for first. What textfold ships for C is a compile of the
        // one file in front of you, because one file is what somebody has when
        // they first press the key. A project of nine files and a `Makefile`
        // has a build of its own, and there has to be somewhere to say that
        // without editing a directory an update replaces whole.
        let mut file: FilePlugin = serde_json::from_str(
            r#"{"id":"c","tools":[{
                 "name":"cc","command":"cc","args":["-g","-o","${file_stem}","${file}"],
                 "languages":["c"],"output":"problems",
                 "pattern":"%f:%l:%c: %t: %m","builds":true}]}"#,
        )
        .unwrap();
        let said: FileOverride =
            serde_json::from_str(r#"{"tools":{"cc":{"command":"make","args":[]}}}"#).unwrap();
        file.apply_override(said);
        let (plugin, problems) = file.into_plugin("c", Source::BuiltIn);
        assert!(problems.is_empty(), "{problems:?}");

        let tool = &plugin.tools[0];
        assert_eq!(tool.command, "make");
        // A list replaces rather than being appended to: the flags that made
        // sense for `cc` make none for `make`, and there would otherwise be no
        // way to be rid of them.
        assert!(tool.args.is_empty(), "{:?}", tool.args);
        // And what was not mentioned is exactly as it shipped — including the
        // flag that makes this the build rather than merely a tool.
        assert!(tool.builds);
        assert_eq!(tool.output, Output::Problems);
        assert_eq!(tool.pattern.as_deref(), Some("%f:%l:%c: %t: %m"));
        assert!(tool.wants("c"));

        // And the first draft of a settings file names it, so nobody has to
        // find this out from the source.
        let stub = settings_stub(&plugin);
        let read: FileOverride = serde_json::from_str(&stub).expect("{stub}");
        assert_eq!(read.tools.len(), 1, "{stub}");
        assert!(stub.contains("\"cc\""), "{stub}");
    }

    #[test]
    fn every_language_that_can_be_debugged_can_also_be_built() {
        // The pair that has to hold, whatever a language calls the two halves.
        // `gdb` cannot open a program nobody has compiled, and shipping the
        // debugger without the build leaves the interesting half in another
        // window — which is exactly how this started.
        let registry = load();
        let has_build = |language: &str| {
            registry
                .plugins
                .iter()
                .flat_map(|plugin| plugin.tools.iter())
                .any(|tool| tool.builds && tool.wants(language))
        };
        // The languages textfold ships a compiler story for. A script run by
        // an interpreter has nothing to compile first and is not in it.
        for name in ["c", "cpp", "rust"] {
            let id = crate::lang::by_name(name)
                .unwrap_or_else(|| panic!("{name} is not a language here"));
            let lang = crate::lang::get(id);
            assert!(
                !lang.debuggers.is_empty(),
                "{name} can be built and not debugged"
            );
            assert!(has_build(name), "{name} can be debugged and not built");
            // And every one of them says what file to debug, since for a
            // compiled language that can never be the file you are looking at.
            for debugger in &lang.debuggers {
                let program = debugger.launch.get("program").and_then(|p| p.as_str());
                assert!(
                    program.is_some_and(|p| !p.is_empty()),
                    "{name}/{} debugs nothing in particular",
                    debugger.name
                );
            }
        }
    }

    #[test]
    fn what_ships_for_a_compiled_language_knows_how_to_attach_to_one() {
        // A debugger that can only run programs it started is half a debugger.
        // What ships has to say *how* it attaches, because where the process
        // id goes is the adapter's own business — `pid` for gdb, `processId`
        // for others — and a list of the fields textfold understands would be
        // wrong for the next adapter somebody installs.
        let shipped: Vec<Plugin> = LANGUAGES
            .iter()
            .map(|(id, text)| {
                let file: FilePlugin = serde_json::from_str(text)
                    .unwrap_or_else(|e| panic!("{id}: {}", said(&e)));
                file.into_plugin(id, Source::BuiltIn).0
            })
            .collect();
        for (plugin, language) in [("c", "c"), ("cpp", "cpp"), ("python", "python")] {
            let file = shipped
                .iter()
                .find(|p| p.id == plugin)
                .and_then(|p| p.languages.get(language).cloned())
                .unwrap_or_else(|| panic!("{plugin} defines no {language}"));
            let debugger = file
                .debuggers
                .as_ref()
                .and_then(|list| list.first())
                .cloned()
                .unwrap_or_else(|| panic!("{language} ships no debugger"))
                .into_debugger(plugin);
            assert!(
                debugger.can_attach(),
                "{language} can only debug programs it started"
            );
            let attach = debugger.attach.expect("just checked");
            assert_eq!(attach["request"], serde_json::json!("attach"));
            // And it says how to reach the program: a process to point at,
            // or a port to meet it on. An `attach` that says neither is a
            // request to attach to nothing in particular.
            let by_process = crate::dap::needs_a_process(&attach);
            let by_port = attach.get("listen").is_some() || attach.get("connect").is_some();
            assert!(by_process || by_port, "{language}: {attach}");
        }
    }

    #[test]
    fn what_ships_for_a_compiled_language_knows_how_to_compile_it() {
        // The bug this is about: pressing F5 on a `main.c` nobody had compiled
        // yet started `gdb`, which reported that `main` does not exist. True,
        // and no help at all — the editor knew what was missing and had no way
        // to make it.
        // Read from the manifests themselves rather than through `load`,
        // which merges in whatever the person running the tests has said in
        // their own settings. What ships is the question, and a developer who
        // has pointed their C build at `make` should not fail this.
        let shipped: Vec<Plugin> = LANGUAGES
            .iter()
            .map(|(id, text)| {
                let file: FilePlugin = serde_json::from_str(text)
                    .unwrap_or_else(|e| panic!("{id}: {}", said(&e)));
                file.into_plugin(id, Source::BuiltIn).0
            })
            .collect();
        for language in ["c", "cpp"] {
            let build = shipped
                .iter()
                .flat_map(|plugin| plugin.tools.iter())
                .find(|tool| tool.builds && tool.wants(language))
                .unwrap_or_else(|| panic!("{language} ships a debugger and no way to build"));
            // With debugging in it, or there is nothing for the adapter to
            // read when it gets there.
            assert!(
                build.args.iter().any(|arg| arg == "-g"),
                "{language} builds without debugging in it: {:?}",
                build.args
            );
            // And into the file the debugger is going to open. See the `gdb`
            // entry in the same manifest, whose `program` is `${file_stem}`.
            assert!(
                build.args.iter().any(|arg| arg == "${file_stem}"),
                "{language} builds something other than what it debugs: {:?}",
                build.args
            );
            assert_eq!(build.output, Output::Problems);
            assert!(build.pattern.is_some(), "a build with nothing to read it");
            // And run where the project is built rather than beside the file.
            // `.git` alone is not enough: a checkout is not the only thing
            // that makes a directory a project, and a build run in `src/`
            // because there was no repository is a `make` with no `Makefile`
            // in front of it.
            assert!(
                build.roots.iter().any(|root| root.eq_ignore_ascii_case("makefile")),
                "{language} builds wherever the file happens to be: {:?}",
                build.roots
            );
        }
    }

    #[test]
    fn a_list_you_give_is_the_list_rather_than_one_appended_to() {
        // Objects merge; everything else replaces. Anything else and there
        // would be no way to *shorten* a list you cannot see.
        let mut base = json!({"a": {"b": [1, 2, 3], "c": 1}, "d": "keep"});
        merge(&mut base, &json!({"a": {"b": [9]}}));
        assert_eq!(base, json!({"a": {"b": [9], "c": 1}, "d": "keep"}));

        // And a value that was not an object before is simply replaced.
        let mut base = json!({"a": 1});
        merge(&mut base, &json!({"a": {"b": 2}}));
        assert_eq!(base, json!({"a": {"b": 2}}));
    }

    #[test]
    fn a_settings_file_says_what_a_plugin_is_told_and_nothing_else() {
        // What a plugin *is* belongs to the plugin, not to your opinion of it,
        // and whether it is *on* is asked in `config.json` under `plugins` —
        // one question with two places to answer it is worse than one place.
        // Each of these is refused rather than ignored quietly, because a
        // setting that does nothing and says nothing is an afternoon.
        for wrong in [
            r#"{"enabled":false}"#,
            r#"{"id":"mine"}"#,
            r#"{"needs":["x"]}"#,
            r#"{"install":[]}"#,
            r#"{"languages":{}}"#,
        ] {
            assert!(
                serde_json::from_str::<FileOverride>(wrong).is_err(),
                "{wrong} should not be something a settings file can say"
            );
        }
        // And what it can say, it says.
        let said: FileOverride =
            serde_json::from_str(r#"{"_about":"why","settings":{"a":1},"servers":{}}"#)
                .expect("this is the whole of the shape");
        assert_eq!(said.settings, Some(json!({"a": 1})));
    }

    #[test]
    fn a_first_draft_names_the_servers_the_plugin_actually_has() {
        // A file made empty is a blank page and a guess.
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"vscode-langservers","languages":{
                 "css":{"servers":[{"name":"css-language-server","command":"a"}]},
                 "html":{"servers":[{"name":"html-language-server","command":"b"}]}}}"#,
        )
        .unwrap();
        let (plugin, _) = file.into_plugin("vscode-langservers", Source::BuiltIn);
        let stub = settings_stub(&plugin);
        // It parses as one of these, which is the least a first draft can do.
        let read: FileOverride = serde_json::from_str(&stub).expect("{stub}");
        assert_eq!(read.servers.len(), 2);
        assert!(stub.contains("css-language-server"), "{stub}");
        assert!(stub.contains("html-language-server"), "{stub}");
        // And no `settings` of its own, because this plugin brings no program
        // of its own to tell anything to.
        assert_eq!(read.settings, None, "{stub}");

        // And a plugin with nothing to be told says so rather than offering
        // an empty shape to fill in.
        let file: FilePlugin =
            serde_json::from_str(r#"{"id":"rust","languages":{"rust":{"extensions":["rs"]}}}"#)
                .unwrap();
        let (plugin, _) = file.into_plugin("rust", Source::BuiltIn);
        let stub = settings_stub(&plugin);
        serde_json::from_str::<FileOverride>(&stub).expect("{stub}");
        assert!(stub.contains("nothing here to tell it"), "{stub}");
    }

    #[test]
    fn a_settings_file_is_never_written_outside_where_settings_go() {
        // The id comes out of a manifest, and a manifest is a thing people
        // paste.
        let Some(dir) = settings_dir() else { return };
        for id in ["../../etc/passwd", "a/b", "..", "."] {
            if let Some(path) = settings_path(id) {
                assert_eq!(path.parent(), Some(dir.as_path()), "{id}");
            }
        }
        assert_eq!(settings_path("pyright"), Some(dir.join("pyright.json")));
    }

    #[test]
    fn a_plugin_can_bring_a_grammar_of_its_own_without_knowing_where_it_lands() {
        // The other half of `${plugin}`, and the difference between "plugins
        // can add a language" and "plugins can add a language you have
        // already built and installed yourself". A grammar lives beside the
        // manifest, and the manifest cannot know where textfold put it.
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"zig","languages":{"zig":{"extensions":["zig"],
                "grammar":{"library":"${plugin}/zig.so","symbol":"tree_sitter_zig",
                           "highlights":"${plugin}/highlights.scm"}}}}"#,
        )
        .unwrap();
        let (plugin, problems) = file.into_plugin(
            "zig",
            Source::File(PathBuf::from("/home/me/.config/textfold/plugins/zig/plugin.json")),
        );
        assert!(problems.is_empty(), "{problems:?}");
        let said = format!("{:?}", plugin.languages.get("zig").expect("the language"));
        assert!(
            said.contains("/home/me/.config/textfold/plugins/zig/zig.so"),
            "the library was not found: {said}"
        );
        assert!(
            said.contains("/home/me/.config/textfold/plugins/zig/highlights.scm"),
            "the highlights were not found: {said}"
        );
        assert!(!said.contains("${plugin}"), "something was left unfilled: {said}");
    }

    #[test]
    fn a_command_with_nothing_to_run_it_is_dropped_and_said_so() {
        // It would otherwise sit in the palette looking like a command and do
        // nothing at all, which is worse than not being there.
        let file: FilePlugin =
            serde_json::from_str(r#"{"id":"p","commands":[{"name":"go"}]}"#).unwrap();
        let (plugin, problems) = file.into_plugin("p", Source::BuiltIn);
        assert!(plugin.commands.is_empty());
        assert_eq!(problems, ["p: go has no host to run it"]);
    }

    #[test]
    fn something_to_start_on_that_nobody_understands_is_a_complaint() {
        let file: FilePlugin = serde_json::from_str(
            r#"{"id":"p","host":{"command":"x","activate":["whenever"]}}"#,
        )
        .unwrap();
        let (plugin, problems) = file.into_plugin("p", Source::BuiltIn);
        assert!(plugin.host.expect("still a host").activate.is_empty());
        assert_eq!(problems, [r#"p: "whenever" is not something to start on"#]);
    }

    #[test]
    fn a_glob_matches_the_way_a_person_writing_one_would_expect() {
        let path = std::path::Path::new("/home/me/board/Core/Src/main.c");
        // A pattern with no slash is about the file's name, wherever it is.
        assert!(matches_glob("*.c", path));
        assert!(matches_glob("main.c", path));
        assert!(!matches_glob("*.h", path));
        // `**` crosses directories; a single `*` does not.
        assert!(matches_glob("**/Src/*.c", path));
        assert!(!matches_glob("/home/*/main.c", path));
        assert!(matches_glob("/home/**/main.c", path));
        // And `**/` finds one sitting at the top as well as one further down.
        assert!(matches_glob("**/*.c", std::path::Path::new("main.c")));
        assert!(matches_glob("Cargo.toml", std::path::Path::new("/p/Cargo.toml")));
        assert!(!matches_glob("/p/Cargo.toml", std::path::Path::new("/q/Cargo.toml")));
    }

    #[test]
    fn an_id_that_is_not_written_down_is_the_file_it_came_from() {
        let file: FilePlugin = serde_json::from_str(r#"{"languages":{}}"#).unwrap();
        let (plugin, _) = file.into_plugin("Zig", Source::BuiltIn);
        assert_eq!(plugin.id, "zig");
    }
}
