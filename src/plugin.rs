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

/// One server a plugin contributes, as the list of things you can switch off
/// needs to know about it.
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
    pub source: Source,
    /// Whether it is on when nobody has said. A plugin can ship turned off.
    pub on_by_default: bool,
    /// What it says about languages, in the shape `languages.json` uses.
    pub languages: BTreeMap<String, FileLang>,
    pub servers: Vec<ServerEntry>,
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

/// The language servers, one plugin each, every one of them with an id, a
/// switch, and instructions for getting it.
///
/// This is what "the language servers are plugins" finally means. `pyright` is
/// not a line in the Python plugin that happens to name a program; it is a
/// plugin, with its own row in the list, its own entry in the settings file,
/// and its own answer to *and how do I get it*. Removing it is removing a
/// plugin, not editing the definition of Python.
///
/// One plugin can serve several languages, which is the other thing the split
/// buys: `clangd` was written out twice, once in C and once in C++, and
/// `tsserver` three times. Now each is written once and says which languages
/// it is for.
const SERVERS: &[(&str, &str)] = built_in![
    "rust-analyzer",
    "pyright",
    "ruff",
    "tsserver",
    "gopls",
    "clangd",
    "omnisharp",
    "jdtls",
    "bash-language-server",
    "vscode-langservers",
    "taplo",
    "yaml-language-server",
    "marksman",
    "docker-langserver",
];

fn load() -> Registry {
    let mut it = Registry {
        plugins: Vec::new(),
        problems: Vec::new(),
    };

    for (id, text) in LANGUAGES.iter().chain(SERVERS) {
        let file: FilePlugin = serde_json::from_str(text)
            .expect("the plugins textfold ships are checked by a test");
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
                Ok(file) => {
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
struct FilePlugin {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    about: Option<String>,
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
}

impl FilePlugin {
    /// Turn a manifest into a plugin, along with anything wrong with it worth
    /// telling somebody about. A manifest is written by hand, so a mistake in
    /// one is a thing to say out loud rather than a thing to swallow.
    fn into_plugin(self, fallback_id: &str, source: Source) -> (Plugin, Vec<String>) {
        let mut problems = Vec::new();
        let id = self
            .id
            .map(|id| id.trim().to_lowercase())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| fallback_id.to_lowercase());
        let mut servers = Vec::new();
        for (language, def) in &self.languages {
            for server in def.servers.iter().flatten() {
                let name = server.plugin_name();
                // The same server written out for three languages is one
                // switch, not three: `tsserver` is on or off, and which of
                // JavaScript, TypeScript and TSX you happen to be looking at
                // is not a thing anybody wants to decide separately.
                if let Some(seen) = servers.iter_mut().find(|s: &&mut ServerEntry| s.name == name) {
                    seen.languages.push(language.clone());
                    continue;
                }
                servers.push(ServerEntry {
                    id: server_id(&id, &name),
                    name,
                    command: server.command.clone(),
                    languages: vec![language.clone()],
                });
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
            commands.push(Command {
                opens_panel,
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

        let plugin = Plugin {
            name: self.name.unwrap_or_else(|| id.clone()),
            about: self.about,
            on_by_default: self.enabled.unwrap_or(true),
            languages: self.languages,
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

    #[test]
    fn the_plugins_textfold_ships_all_read() {
        let registry = load();
        for (id, _) in LANGUAGES.iter().chain(SERVERS) {
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
    fn every_language_server_that_ships_says_how_to_get_it() {
        // A row in a list that says "you have not got this" and does not say
        // what to do about it is a row that wastes an afternoon.
        let registry = load();
        for (id, _) in SERVERS {
            let plugin = registry.plugins.iter().find(|p| &p.id == id).unwrap();
            assert!(!plugin.servers.is_empty(), "{id} runs no server");
            assert!(!plugin.needs.is_empty(), "{id} does not say what it needs");
            assert!(plugin.can_install(), "{id} does not say how to get it");
            assert!(
                plugin.see.is_some(),
                "{id} does not say where to get it by hand"
            );
            // What it needs is what it runs. A `needs` naming something other
            // than the command would report a server as ready that cannot
            // start, which is worse than saying nothing.
            for server in &plugin.servers {
                assert!(
                    plugin.needs.contains(&server.command),
                    "{id} runs {} and does not say it needs it",
                    server.command
                );
            }
        }
    }

    #[test]
    fn a_plugin_that_is_one_server_is_named_once() {
        // `pyright/pyright` is a name nobody would write, and it would be the
        // name in everybody's settings file.
        let registry = load();
        let pyright = registry.plugins.iter().find(|p| p.id == "pyright").unwrap();
        let ids: Vec<&str> = pyright.servers.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["pyright"]);

        // And one server for three languages is one switch, not three.
        let ts = registry.plugins.iter().find(|p| p.id == "tsserver").unwrap();
        assert_eq!(ts.servers.len(), 1);
        assert_eq!(ts.servers[0].id, "tsserver");
        assert_eq!(ts.servers[0].for_what(), "javascript, tsx, typescript");
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
