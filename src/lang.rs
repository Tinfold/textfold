//! What textfold knows about a language: how to colour it, how to comment it
//! out, and what to run to get intelligence about it.
//!
//! A language is a table of facts, not code, which is the point. The facts
//! arrive from [plugins](crate::plugin) — the JSON files textfold ships, plus
//! whatever is in `~/.config/textfold/plugins/` and `languages.json` — and a
//! table that names a language another plugin already defined *merges into*
//! it rather than replacing it. So switching rust-analyzer for something else
//! is three lines, and so is teaching textfold a language it has never heard
//! of:
//!
//! ```json
//! { "id": "zig", "languages": { "zig": {
//!     "extensions": ["zig", "zon"],
//!     "line_comment": "//",
//!     "servers": [{ "name": "zls", "command": "zls", "roots": ["build.zig"] }],
//!     "grammar": { "library": "~/.config/textfold/grammars/zig.so",
//!                  "highlights": "~/.config/textfold/grammars/zig.scm" }
//! } } }
//! ```
//!
//! The grammars that ship are compiled in. One named by file is opened with
//! `dlopen` at the moment a file of that language is first shown, which is the
//! same thing every other tree-sitter editor does and means a grammar built by
//! `tree-sitter build` works here without textfold being rebuilt.
//!
//! The registry is built once at startup and again whenever a plugin is turned
//! on or off. A language id survives that: ids are handed out per name and
//! kept, so a buffer that was Python is still pointing at Python after the
//! Python plugin has been switched off and on again. While it is off, the
//! language is still there by name but has nothing in it — no extensions, no
//! grammar, no servers — which is exactly what "off" should look like from
//! the outside.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};

use ropey::Rope;
use serde::Deserialize;
use tree_sitter::Language;

/// Which language, as a document holds onto one. An index into the registry,
/// which is built once at startup and never changes afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct LangId(pub u16);

impl LangId {
    /// A file textfold has nothing to say about. Always the first entry, so
    /// that "no language" needs no special case anywhere else.
    pub const PLAIN: LangId = LangId(0);
}

/// Everything known about one language.
pub struct Lang {
    pub id: LangId,
    /// What it is called, in a status line and in `languages.json`.
    pub name: String,
    /// What a language server calls it, which is not always what we do.
    pub lsp_id: String,
    pub extensions: Vec<String>,
    /// Whole file names, for the many files with no extension at all.
    pub filenames: Vec<String>,
    /// Words that, in a `#!` line, mean this language.
    pub shebangs: Vec<String>,

    pub line_comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    /// Characters that open and close a nesting level, for auto-pairs and for
    /// working out the indentation of a new line.
    pub brackets: Vec<(char, char)>,

    /// The language servers to try, in order. More than one is normal: Python
    /// gets a type checker and a linter, and both are wanted.
    pub servers: Vec<Server>,

    /// Whether a plugin that is on defined this. A language whose plugin has
    /// been switched off keeps its id and its name and loses everything else,
    /// so that nothing holding the id is left pointing at nowhere.
    provided: bool,

    grammar: Option<GrammarSource>,
    /// Compiled on first use. Building a highlight query takes a few
    /// milliseconds, which is nothing once and everything on every keystroke.
    compiled: OnceLock<Option<&'static crate::syntax::Grammar>>,
}

/// Where a grammar comes from.
enum GrammarSource {
    /// Compiled into the binary. More than one query is normal: TypeScript's
    /// own says only what it adds to JavaScript's.
    BuiltIn {
        language: fn() -> Language,
        highlights: &'static [&'static str],
    },
    /// A shared library on disk, opened when first needed.
    Library {
        path: PathBuf,
        symbol: String,
        highlights: PathBuf,
    },
}

/// A language server, as a table of what to run.
#[derive(Clone, Debug)]
pub struct Server {
    /// What the settings file and the plugin list call it: `python/ruff`.
    /// This is what a switch is thrown against.
    pub id: String,
    /// The short half of that: `ruff`.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Files that mark the top of a project. The first one found walking up
    /// from the file being edited is the root the server is told about; a
    /// server given the wrong root indexes either far too much or nothing.
    pub roots: Vec<String>,
    /// Handed over in `initializationOptions`.
    pub init_options: Option<serde_json::Value>,
    /// Handed over in `workspace/didChangeConfiguration`, and in answer to
    /// `workspace/configuration`. This is where rust-analyzer's settings go.
    pub settings: Option<serde_json::Value>,
    pub env: BTreeMap<String, String>,
}

impl Lang {
    /// The grammar, compiling it the first time it is asked for. `None` for a
    /// language with none, or one whose library would not open — a grammar
    /// that fails to load means no colours, not no editor.
    pub fn grammar(&self) -> Option<&'static crate::syntax::Grammar> {
        *self.compiled.get_or_init(|| {
            let source = self.grammar.as_ref()?;
            let (language, highlights) = match source {
                GrammarSource::BuiltIn {
                    language,
                    highlights,
                } => (language(), highlights.join("\n")),
                GrammarSource::Library {
                    path,
                    symbol,
                    highlights,
                } => {
                    let language = load_library_grammar(path, symbol)?;
                    let query = std::fs::read_to_string(highlights).ok()?;
                    (language, query)
                }
            };
            let grammar = crate::syntax::Grammar::new(language, &highlights)?;
            // The registry outlives the process, and a grammar is wanted for
            // as long as a file of its language is open, which is the same
            // thing. Leaking it is what makes it `'static`, and there is
            // exactly one per language.
            Some(Box::leak(Box::new(grammar)))
        })
    }

    pub fn has_grammar(&self) -> bool {
        self.grammar.is_some()
    }

    /// Whether anything is switched on that knows about this language.
    pub fn is_available(&self) -> bool {
        self.provided
    }
}

/// Open a `.so` and pull a `tree_sitter_<name>` out of it.
///
/// The library is deliberately never closed: the `Language` it hands back
/// points into it, and dropping it while a document still holds a parse tree
/// would be a crash rather than a missing colour.
fn load_library_grammar(path: &Path, symbol: &str) -> Option<Language> {
    unsafe {
        let library = libloading::Library::new(path).ok()?;
        let entry: libloading::Symbol<unsafe extern "C" fn() -> *const ()> =
            library.get(symbol.as_bytes()).ok()?;
        let language = Language::from_raw(entry() as *const _);
        std::mem::forget(library);
        Some(language)
    }
}

/// Every language there is.
pub struct Languages {
    langs: Vec<Lang>,
    /// Complaints about the user's `languages.json`, to show once at startup.
    /// A typo in it is worth hearing about; silently having no Zig is not.
    pub problems: Vec<String>,
}

/// The registry, swapped whole when a plugin is turned on or off.
///
/// The old one is leaked rather than dropped, because a `&'static Lang` handed
/// out before the swap may still be sitting in a parse tree or a status line.
/// Toggling a plugin is something a person does a handful of times, so a few
/// stale registries is a price worth not thinking about; the alternative is a
/// lifetime on every language lookup in the editor.
static REGISTRY: OnceLock<RwLock<&'static Languages>> = OnceLock::new();

/// Which id each language name has, in order, growing only. This is what makes
/// a `LangId` mean the same thing before and after a rebuild — a buffer that
/// was Python does not become YAML because a plugin above Python in the list
/// was switched off.
static NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn registry() -> &'static RwLock<&'static Languages> {
    REGISTRY.get_or_init(|| RwLock::new(Box::leak(Box::new(Languages::load()))))
}

/// Build the registry. Called once, before anything asks for a language.
pub fn init() {
    registry();
}

/// Read the plugins again and build the registry afresh, for after one has
/// been turned on or off.
pub fn rebuild() {
    let fresh: &'static Languages = Box::leak(Box::new(Languages::load()));
    *registry().write().unwrap_or_else(|e| e.into_inner()) = fresh;
}

/// Every language there is. Cheap, and the same table every time.
pub fn all() -> &'static Languages {
    *registry().read().unwrap_or_else(|e| e.into_inner())
}

/// One language by id.
pub fn get(id: LangId) -> &'static Lang {
    let langs = all();
    langs.langs.get(id.0 as usize).unwrap_or(&langs.langs[0])
}

/// What language a file is, from its name and, failing that, its first line.
pub fn detect(path: &Path, rope: &Rope) -> LangId {
    let langs = all();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    // A whole file name beats an extension: `Makefile` has no extension, and
    // `CMakeLists.txt` is not text.
    for lang in &langs.langs {
        if lang.filenames.iter().any(|f| f.to_lowercase() == name) {
            return lang.id;
        }
    }
    // The longest matching extension wins, so `.d.ts` beats `.ts` for anyone
    // who adds it.
    let mut best: Option<(usize, LangId)> = None;
    for lang in &langs.langs {
        for ext in &lang.extensions {
            if name.len() > ext.len()
                && name.ends_with(ext.as_str())
                && name.as_bytes()[name.len() - ext.len() - 1] == b'.'
                && best.is_none_or(|(len, _)| ext.len() > len)
            {
                best = Some((ext.len(), lang.id));
            }
        }
    }
    if let Some((_, id)) = best {
        return id;
    }

    // Nothing in the name: ask the file. A script called `deploy` is still a
    // shell script, and it says so on its first line.
    if rope.len_lines() > 0 {
        let first = rope.line(0).to_string();
        if let Some(rest) = first.trim_end().strip_prefix("#!") {
            let word = rest
                .rsplit(['/', ' '])
                .find(|w| !w.is_empty() && *w != "env")
                .unwrap_or("");
            for lang in &langs.langs {
                if lang.shebangs.iter().any(|s| s == word) {
                    return lang.id;
                }
            }
        }
    }
    LangId::PLAIN
}

/// The language a markdown fence means: ```` ```rust ````, ```` ```py ````,
/// ```` ```sh ````.
///
/// Whoever wrote the fence was writing for a reader, not for us, so the tag is
/// matched against everything a language answers to — its name, what a
/// language server calls it, and its extensions. `None` where nothing matches,
/// which is not a problem: an unrecognised fence is code drawn in one colour,
/// the way it was before anything was coloured at all.
pub fn by_tag(tag: &str) -> Option<LangId> {
    let wanted = tag.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    let langs = all();
    langs
        .langs
        .iter()
        .find(|l| {
            l.is_available()
                && (l.name.to_lowercase() == wanted
                    || l.lsp_id.to_lowercase() == wanted
                    || l.extensions.iter().any(|e| e.to_lowercase() == wanted))
        })
        .map(|l| l.id)
        .filter(|id| *id != LangId::PLAIN)
}

/// A language by name. A picker holds the id it is offering, so nothing that
/// a person drives asks this — but a buffer the editor makes for itself knows
/// what it is putting in and has only the name to say so with, and the tests
/// ask constantly.
pub fn by_name(name: &str) -> Option<LangId> {
    let wanted = name.trim().to_lowercase();
    all()
        .langs
        .iter()
        .find(|l| l.is_available() && l.name.to_lowercase() == wanted)
        .map(|l| l.id)
}

/// Every language a plugin that is on knows about, for a picker.
///
/// Plain text is always there: it is what a file with nothing said about it
/// is, and "this file is text" has to stay something you can choose.
pub fn names() -> Vec<(LangId, &'static str)> {
    all()
        .langs
        .iter()
        .filter(|l| l.is_available() || l.id == LangId::PLAIN)
        .map(|l| (l.id, l.name.as_str()))
        .collect()
}

/// The directory a language server should be told is the top of the project:
/// the nearest ancestor of `from` holding one of `markers`.
///
/// A marker is usually a file name, but `"*.sln"` is allowed and means any
/// file with that extension — which is the only way to say what marks the top
/// of a C# project, where the file is named after the solution rather than
/// after the language.
///
/// Falling back to the file's own directory is deliberate. A server pointed at
/// your home directory will try to index all of it.
pub fn project_root(from: &Path, markers: &[String]) -> PathBuf {
    let start = if from.is_dir() {
        from
    } else {
        from.parent().unwrap_or(Path::new("."))
    };
    for dir in start.ancestors() {
        if markers.iter().any(|m| marker_is_in(dir, m)) {
            return dir.to_path_buf();
        }
    }
    start.to_path_buf()
}

/// Whether `dir` holds `marker`, which is a file name or a `*.ext` pattern.
fn marker_is_in(dir: &Path, marker: &str) -> bool {
    let Some(ext) = marker.strip_prefix("*.") else {
        return dir.join(marker).exists();
    };
    // Reading a directory is more work than asking after one file, so it is
    // only done for the markers that need it, and only on the way up.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    })
}

impl Languages {
    /// Build the table out of every plugin that is switched on.
    ///
    /// The ids are handed out first, from the names *every* plugin mentions —
    /// on or off — so that switching one off does not renumber the languages
    /// underneath it. A language nobody provides is still in the table, by
    /// name, with nothing in it.
    fn load() -> Self {
        let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
        if names.is_empty() {
            // Plain text, first, so that `LangId::PLAIN` is 0 and every lookup
            // that falls through has somewhere to land.
            names.push("text".into());
        }
        for plugin in crate::plugin::all() {
            for name in plugin.languages.keys() {
                let name = name.trim().to_lowercase();
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
            }
        }

        let mut it = Self {
            langs: names
                .iter()
                .enumerate()
                .map(|(at, name)| blank(LangId(at as u16), name))
                .collect(),
            problems: crate::plugin::problems().to_vec(),
        };
        drop(names);

        // Plain text still has brackets: a file textfold knows nothing else
        // about should still close a parenthesis you open. And it is text
        // whether or not anybody shipped a plugin saying so.
        it.langs[0].lsp_id = "plaintext".into();
        it.langs[0].extensions = vec!["txt".into(), "text".into()];
        it.langs[0].brackets = default_brackets();
        it.langs[0].provided = true;

        for plugin in crate::plugin::active() {
            it.merge(&plugin.id, &plugin.languages);
        }
        it
    }

    /// Fold a plugin's worth of definitions in. A language already here is
    /// added to, field by field: a plugin saying only `servers` for Rust keeps
    /// the grammar and the comment syntax the one before it gave.
    fn merge(&mut self, plugin: &str, languages: &BTreeMap<String, FileLang>) {
        let Self { langs, problems } = self;
        for (name, def) in languages {
            let name = name.trim().to_lowercase();
            // Every name any plugin mentions was given an id before this ran,
            // so there is always somewhere to put it.
            let Some(lang) = langs.iter_mut().find(|l| l.name == name) else {
                continue;
            };
            apply(lang, plugin, def, problems);
        }
    }
}

/// A language with its name and nothing else: what an id points at before any
/// plugin has said anything, and what it goes back to when the plugin that
/// said it is switched off.
fn blank(id: LangId, name: &str) -> Lang {
    Lang {
        id,
        name: name.to_string(),
        lsp_id: name.to_string(),
        extensions: Vec::new(),
        filenames: Vec::new(),
        shebangs: Vec::new(),
        line_comment: None,
        block_comment: None,
        brackets: default_brackets(),
        servers: Vec::new(),
        provided: false,
        grammar: None,
        compiled: OnceLock::new(),
    }
}

/// The brackets nearly every language has. A language saying nothing about
/// brackets means these, not none.
fn default_brackets() -> Vec<(char, char)> {
    vec![('(', ')'), ('[', ']'), ('{', '}')]
}

/// Write one plugin's definition over a language.
fn apply(lang: &mut Lang, plugin: &str, def: &FileLang, problems: &mut Vec<String>) {
    lang.provided = true;
    if let Some(v) = &def.lsp_id {
        lang.lsp_id = v.clone();
    }
    if let Some(v) = &def.extensions {
        lang.extensions = v
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect();
    }
    if let Some(v) = &def.filenames {
        lang.filenames = v.clone();
    }
    if let Some(v) = &def.shebangs {
        lang.shebangs = v.clone();
    }
    if let Some(v) = &def.line_comment {
        lang.line_comment = Some(v.clone()).filter(|s| !s.is_empty());
    }
    if let Some(v) = &def.block_comment {
        lang.block_comment = (v.len() == 2).then(|| (v[0].clone(), v[1].clone()));
    }
    if let Some(v) = &def.brackets {
        lang.brackets = v
            .iter()
            .filter_map(|pair| {
                let mut chars = pair.chars();
                Some((chars.next()?, chars.next()?))
            })
            .collect();
    }
    if let Some(v) = &def.servers {
        lang.servers = v
            .iter()
            .filter(|s| !s.command.trim().is_empty())
            // A server switched off is a server that is not in the table, so
            // nothing downstream has to ask again whether it is wanted.
            .filter(|s| crate::plugin::is_on(&format!("{plugin}/{}", s.plugin_name())))
            .map(|s| Server {
                id: format!("{plugin}/{}", s.plugin_name()),
                name: s.plugin_name(),
                command: s.command.clone(),
                args: s.args.clone(),
                roots: if s.roots.is_empty() {
                    // Every project has one of these somewhere above it, and
                    // stopping at a repository root is nearly always right.
                    vec![".git".into()]
                } else {
                    s.roots.clone()
                },
                init_options: s.init_options.clone(),
                settings: s.settings.clone(),
                env: s.env.clone(),
            })
            .collect();
    }
    if let Some(g) = &def.grammar {
        match g {
            FileGrammar::BuiltIn { built_in } => match built_in_grammar(built_in) {
                Some(source) => lang.grammar = Some(source),
                None => problems.push(format!(
                    "{}: there is no grammar built in called {built_in:?}",
                    lang.name
                )),
            },
            FileGrammar::Library {
                library,
                symbol,
                highlights,
            } => {
                let symbol = symbol.clone().unwrap_or_else(|| {
                    format!("tree_sitter_{}", lang.name.replace(['-', '.'], "_"))
                });
                lang.grammar = Some(GrammarSource::Library {
                    path: expand(library),
                    symbol,
                    highlights: expand(highlights),
                });
            }
            FileGrammar::None {} => lang.grammar = None,
        }
    }
}

/// `~/…` the way a person writes it in a config file.
fn expand(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// The grammars compiled into the binary, by the name `languages.json` calls
/// them. Adding one here and naming it there is the whole of adding a
/// built-in language.
fn built_in_grammar(name: &str) -> Option<GrammarSource> {
    macro_rules! grammars {
        ($($key:literal => $language:expr, [$($query:expr),* $(,)?]),* $(,)?) => {
            match name {
                $($key => Some(GrammarSource::BuiltIn {
                    language: || $language.into(),
                    highlights: &[$($query),*],
                }),)*
                _ => None,
            }
        };
    }
    grammars! {
        "bash" => tree_sitter_bash::LANGUAGE, [tree_sitter_bash::HIGHLIGHT_QUERY],
        "c" => tree_sitter_c::LANGUAGE, [tree_sitter_c::HIGHLIGHT_QUERY],
        // The grammar's own query opens with a catch-all that would swallow
        // every identifier; ours goes first and says what they are.
        "c-sharp" => tree_sitter_c_sharp::LANGUAGE, [
            include_str!("queries/csharp.scm"),
            tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
        ],
        "css" => tree_sitter_css::LANGUAGE, [tree_sitter_css::HIGHLIGHTS_QUERY],
        "go" => tree_sitter_go::LANGUAGE, [tree_sitter_go::HIGHLIGHTS_QUERY],
        "html" => tree_sitter_html::LANGUAGE, [tree_sitter_html::HIGHLIGHTS_QUERY],
        // Same as C#: the grammar's file opens with a catch-all that takes
        // every identifier before its own rules get a look in.
        "java" => tree_sitter_java::LANGUAGE, [
            include_str!("queries/java.scm"),
            tree_sitter_java::HIGHLIGHTS_QUERY,
        ],
        "javascript" => tree_sitter_javascript::LANGUAGE, [
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        ],
        "json" => tree_sitter_json::LANGUAGE, [tree_sitter_json::HIGHLIGHTS_QUERY],
        "markdown" => tree_sitter_md::LANGUAGE, [tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
        "python" => tree_sitter_python::LANGUAGE, [tree_sitter_python::HIGHLIGHTS_QUERY],
        "rust" => tree_sitter_rust::LANGUAGE, [
            include_str!("queries/rust.scm"),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ],
        "toml" => tree_sitter_toml_ng::LANGUAGE, [tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
        // TypeScript's own query says only what it adds to JavaScript's, so
        // it is read on top of it rather than instead of it.
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT, [
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX, [
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        "yaml" => tree_sitter_yaml::LANGUAGE, [tree_sitter_yaml::HIGHLIGHTS_QUERY],
    }
}

/// One language, as a plugin's file writes it. Every field optional, because
/// a plugin that only wants to change the server should only have to say the
/// server.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileLang {
    #[serde(default)]
    lsp_id: Option<String>,
    #[serde(default)]
    extensions: Option<Vec<String>>,
    #[serde(default)]
    filenames: Option<Vec<String>>,
    #[serde(default)]
    shebangs: Option<Vec<String>>,
    #[serde(default)]
    line_comment: Option<String>,
    /// The two halves, as a pair: `["/*", "*/"]`.
    #[serde(default)]
    block_comment: Option<Vec<String>>,
    /// Pairs written as the two characters together: `["()", "[]", "{}"]`.
    #[serde(default)]
    brackets: Option<Vec<String>>,
    #[serde(default)]
    pub servers: Option<Vec<FileServer>>,
    #[serde(default)]
    grammar: Option<FileGrammar>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileServer {
    /// What to call it in the plugin list and in the settings file. Absent
    /// means the command itself, which is a fine name for the many servers
    /// whose command is already the name everyone uses.
    #[serde(default)]
    pub name: Option<String>,
    pub command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    init_options: Option<serde_json::Value>,
    #[serde(default)]
    settings: Option<serde_json::Value>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

impl FileServer {
    /// The half of its id after the slash.
    pub fn plugin_name(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.command)
            .to_lowercase()
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged, deny_unknown_fields)]
enum FileGrammar {
    BuiltIn {
        built_in: String,
    },
    Library {
        library: String,
        #[serde(default)]
        symbol: Option<String>,
        highlights: String,
    },
    /// `{}` — a language deliberately without one.
    None {},
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grammars_the_plugins_ask_for_are_all_here_and_all_compile() {
        for plugin in crate::plugin::all() {
            for (name, def) in &plugin.languages {
                let Some(FileGrammar::BuiltIn { built_in }) = &def.grammar else {
                    continue;
                };
                let Some(GrammarSource::BuiltIn {
                    language,
                    highlights,
                }) = built_in_grammar(built_in)
                else {
                    panic!("{name} wants a grammar called {built_in:?} that is not compiled in");
                };
                // And that it is a grammar the queries beside it actually
                // parse against — a highlight query written for last year's
                // grammar compiles to nothing and shows up as no colours.
                assert!(
                    crate::syntax::Grammar::new(language(), &highlights.join("\n")).is_some(),
                    "{name}: the {built_in:?} highlight query would not compile"
                );
            }
        }
    }

    #[test]
    fn a_file_is_recognised_by_name_then_by_shebang() {
        init();
        let rust = detect(Path::new("/x/src/main.rs"), &Rope::new());
        assert_eq!(get(rust).name, "rust");
        // No extension at all, but it says what it is.
        let script = detect(
            Path::new("/x/deploy"),
            &Rope::from_str("#!/usr/bin/env bash\necho hi\n"),
        );
        assert_eq!(get(script).name, "bash");
        // Nothing to go on.
        assert_eq!(detect(Path::new("/x/notes"), &Rope::new()), LangId::PLAIN);
    }

    #[test]
    fn a_whole_file_name_beats_an_extension() {
        init();
        let makefile = detect(Path::new("/x/Dockerfile"), &Rope::new());
        assert_eq!(get(makefile).name, "dockerfile");
    }

    #[test]
    fn a_later_plugin_adds_to_a_language_rather_than_replacing_it() {
        let mut langs = Languages {
            langs: vec![blank(LangId(0), "rust")],
            problems: Vec::new(),
        };
        let first: BTreeMap<String, FileLang> = serde_json::from_str(
            r#"{"rust":{"extensions":["rs"],"line_comment":"//",
               "servers":[{"command":"rust-analyzer"}]}}"#,
        )
        .unwrap();
        langs.merge("rust", &first);
        let second: BTreeMap<String, FileLang> =
            serde_json::from_str(r#"{"rust":{"servers":[{"command":"ra-multiplex"}]}}"#).unwrap();
        langs.merge("mine", &second);

        let rust = &langs.langs[0];
        assert_eq!(rust.servers[0].command, "ra-multiplex");
        // A server with no name of its own is called after its command, so
        // that it still has an id to be switched off by.
        assert_eq!(rust.servers[0].id, "mine/ra-multiplex");
        // The parts the second plugin said nothing about survived.
        assert_eq!(rust.line_comment.as_deref(), Some("//"));
        assert_eq!(rust.extensions, ["rs"]);
    }

    #[test]
    fn a_language_nothing_provides_keeps_its_name_and_loses_the_rest() {
        // Which is what a plugin being switched off looks like from here: the
        // id a buffer is holding still means the same language, and there is
        // simply nothing behind it.
        let lang = blank(LangId(7), "zig");
        assert!(!lang.is_available());
        assert!(lang.extensions.is_empty());
        assert!(lang.servers.is_empty());
        assert!(!lang.has_grammar());
    }

    #[test]
    fn a_marker_can_name_an_extension_rather_than_a_file() {
        // Which is what C# needs: the file that marks the top of a project is
        // named after the solution, not after the language.
        let dir = std::env::temp_dir().join(format!("textfold-sln-{}", std::process::id()));
        let deep = dir.join("src/Widgets");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.join("Thing.sln"), "").unwrap();
        let root = project_root(&deep.join("Widget.cs"), &["*.sln".into(), ".git".into()]);
        assert_eq!(root, dir);
        // And an extension nothing has still falls back rather than matching.
        let root = project_root(&deep.join("Widget.cs"), &["*.nope".into()]);
        assert_eq!(root, deep);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_project_root_is_the_nearest_marker_above_the_file() {
        let dir = std::env::temp_dir().join(format!("textfold-root-{}", std::process::id()));
        let deep = dir.join("crate/src/inner");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.join("crate/Cargo.toml"), "").unwrap();
        let root = project_root(&deep.join("thing.rs"), &["Cargo.toml".into()]);
        assert_eq!(root, dir.join("crate"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

