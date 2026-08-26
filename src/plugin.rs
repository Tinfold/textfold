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
//! A plugin has an id, and so does each server inside it: `python` is the
//! Python plugin, and `python/ruff` is the one server. Either can be turned
//! off, from the settings file or from `plugins` in the command palette, and
//! turning off a plugin turns off the servers inside it. That is the whole of
//! "the language servers are plugins": `ruff` is not a Python special case
//! written into the editor, it is a row in a list with a switch beside it.
//!
//! What is on is remembered in `config.json` under `plugins`, and only what
//! you have changed is written there — a plugin nobody has said anything about
//! is on.

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

/// One server a plugin contributes, as the list of things you can switch off
/// needs to know about it.
#[derive(Clone, Debug)]
pub struct ServerEntry {
    /// `python/ruff`.
    pub id: String,
    /// `ruff`.
    pub name: String,
    /// What is actually run.
    pub command: String,
    /// Which of the plugin's languages it is for.
    pub language: String,
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
}

impl Plugin {
    /// A line for the list: what it does, or failing that where it came from.
    pub fn detail(&self) -> String {
        match &self.about {
            Some(about) => about.clone(),
            None => self.source.label(),
        }
    }
}

/// Every plugin found, on or off, in the order they are merged.
struct Registry {
    plugins: Vec<Plugin>,
    problems: Vec<String>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// What is switched off, and what has been switched back on. Separate from the
/// registry because this changes while the editor runs and the registry does
/// not.
static CHOSEN: OnceLock<RwLock<BTreeMap<String, bool>>> = OnceLock::new();

fn chosen() -> &'static RwLock<BTreeMap<String, bool>> {
    CHOSEN.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Read the plugins, and take note of what the settings say is off.
///
/// Called before anything asks for a language. Calling it twice is harmless
/// and the second call only updates what is on — the files are read once.
pub fn init(settings: &BTreeMap<String, bool>) {
    REGISTRY.get_or_init(load);
    *chosen().write().unwrap_or_else(|e| e.into_inner()) = settings.clone();
}

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(load)
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
/// once, so the order matters only for a plugin of yours that adds to one.
macro_rules! built_in {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("plugins/", $name, ".json")))),*]
    };
}

const BUILT_IN: &[(&str, &str)] = built_in![
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

fn load() -> Registry {
    let mut it = Registry {
        plugins: Vec::new(),
        problems: Vec::new(),
    };

    for (id, text) in BUILT_IN {
        let file: FilePlugin = serde_json::from_str(text)
            .expect("the plugins textfold ships are checked by a test");
        it.add(file.into_plugin(id, Source::BuiltIn));
    }

    let Some(dir) = crate::config::config_dir() else {
        return it;
    };
    for (id, path) in manifests(&dir.join("plugins")) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<FilePlugin>(&text) {
                Ok(file) => it.add(file.into_plugin(&id, Source::File(path))),
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
                let mut plugin = file.into_plugin("local", Source::File(path));
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
}

impl FilePlugin {
    fn into_plugin(self, fallback_id: &str, source: Source) -> Plugin {
        let id = self
            .id
            .map(|id| id.trim().to_lowercase())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| fallback_id.to_lowercase());
        let mut servers = Vec::new();
        for (language, def) in &self.languages {
            for server in def.servers.iter().flatten() {
                let name = server.plugin_name();
                servers.push(ServerEntry {
                    id: format!("{id}/{name}"),
                    name,
                    command: server.command.clone(),
                    language: language.clone(),
                });
            }
        }
        Plugin {
            name: self.name.unwrap_or_else(|| id.clone()),
            about: self.about,
            on_by_default: self.enabled.unwrap_or(true),
            languages: self.languages,
            servers,
            source,
            id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugins_textfold_ships_all_read() {
        let registry = load();
        for (id, _) in BUILT_IN {
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
        // And the server ids are the ones the settings file will hold.
        let python = registry.plugins.iter().find(|p| p.id == "python").unwrap();
        let ids: Vec<&str> = python.servers.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["python/pyright", "python/ruff"]);
    }

    #[test]
    fn a_plugin_of_your_own_replaces_one_that_ships() {
        let mut registry = Registry {
            plugins: Vec::new(),
            problems: Vec::new(),
        };
        let ship: FilePlugin =
            serde_json::from_str(r#"{"id":"zig","name":"Zig","languages":{}}"#).unwrap();
        registry.add(ship.into_plugin("zig", Source::BuiltIn));
        let mine: FilePlugin =
            serde_json::from_str(r#"{"id":"zig","name":"My Zig","languages":{}}"#).unwrap();
        registry.add(mine.into_plugin("zig", Source::File(PathBuf::from("/tmp/zig.json"))));
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
    fn an_id_that_is_not_written_down_is_the_file_it_came_from() {
        let file: FilePlugin = serde_json::from_str(r#"{"languages":{}}"#).unwrap();
        let plugin = file.into_plugin("Zig", Source::BuiltIn);
        assert_eq!(plugin.id, "zig");
    }
}
