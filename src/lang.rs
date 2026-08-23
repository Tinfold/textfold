//! What textfold knows about a language: how to colour it, how to comment it
//! out, and what to run to get intelligence about it.
//!
//! A language is a table of facts, not code, which is the point. The thirteen
//! textfold ships are written in `languages.json` beside this file and built
//! into the binary; a file of the same name in `~/.config/textfold/` is read
//! on top of it, and a table there naming a language textfold already has
//! *merges into* it rather than replacing it. So switching rust-analyzer for
//! something else is three lines, and so is teaching textfold a language it
//! has never heard of:
//!
//! ```json
//! { "languages": { "zig": {
//!     "extensions": ["zig", "zon"],
//!     "line_comment": "//",
//!     "servers": [{ "command": "zls", "roots": ["build.zig"] }],
//!     "grammar": { "library": "~/.config/textfold/grammars/zig.so",
//!                  "highlights": "~/.config/textfold/grammars/zig.scm" }
//! } } }
//! ```
//!
//! The grammars that ship are compiled in. One named by file is opened with
//! `dlopen` at the moment a file of that language is first shown, which is the
//! same thing every other tree-sitter editor does and means a grammar built by
//! `tree-sitter build` works here without textfold being rebuilt.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

static REGISTRY: OnceLock<Languages> = OnceLock::new();

/// Build the registry. Called once, before anything asks for a language.
pub fn init() {
    REGISTRY.get_or_init(Languages::load);
}

/// Every language there is. Cheap, and the same table every time.
pub fn all() -> &'static Languages {
    REGISTRY.get_or_init(Languages::load)
}

/// One language by id.
pub fn get(id: LangId) -> &'static Lang {
    let langs = all();
    langs
        .langs
        .get(id.0 as usize)
        .unwrap_or(&langs.langs[0])
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
            l.name.to_lowercase() == wanted
                || l.lsp_id.to_lowercase() == wanted
                || l.extensions.iter().any(|e| e.to_lowercase() == wanted)
        })
        .map(|l| l.id)
        .filter(|id| *id != LangId::PLAIN)
}

/// A language by name. Nothing in the editor asks by name — a picker holds
/// the id it is offering — but the tests do, constantly, and a lookup that
/// only the tests use is still a lookup worth having in one place.
#[cfg(test)]
pub fn by_name(name: &str) -> Option<LangId> {
    let wanted = name.trim().to_lowercase();
    all()
        .langs
        .iter()
        .find(|l| l.name.to_lowercase() == wanted)
        .map(|l| l.id)
}

/// Every language's name, for a picker.
pub fn names() -> Vec<(LangId, &'static str)> {
    all()
        .langs
        .iter()
        .map(|l| (l.id, l.name.as_str()))
        .collect()
}

/// The directory a language server should be told is the top of the project:
/// the nearest ancestor of `from` holding one of `markers`.
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
        if markers.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
    }
    start.to_path_buf()
}

impl Languages {
    fn load() -> Self {
        let mut it = Self {
            langs: Vec::new(),
            problems: Vec::new(),
        };
        // Plain text, first, so that `LangId::PLAIN` is 0 and every lookup
        // that falls through has somewhere to land.
        it.langs.push(Lang {
            id: LangId::PLAIN,
            name: "text".into(),
            lsp_id: "plaintext".into(),
            extensions: vec!["txt".into(), "text".into()],
            filenames: Vec::new(),
            shebangs: Vec::new(),
            line_comment: None,
            block_comment: None,
            // Plain text still has brackets: a file textfold knows nothing
            // else about should still close a parenthesis you open.
            brackets: default_brackets(),
            servers: Vec::new(),
            grammar: None,
            compiled: OnceLock::new(),
        });

        let built_in: FileLanguages = serde_json::from_str(include_str!("languages.json"))
            .expect("the languages textfold ships are checked by a test");
        it.merge(built_in);

        // No file of your own is the ordinary case, not a problem — but one
        // that will not parse is worth a complaint, or a typo in it silently
        // costs you a language.
        if let Some(path) = crate::config::config_dir().map(|d| d.join("languages.json"))
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            match serde_json::from_str::<FileLanguages>(&text) {
                Ok(file) => it.merge(file),
                Err(e) => {
                    let said = e.to_string();
                    let said = said.split(" at line ").next().unwrap_or(&said).to_string();
                    it.problems.push(format!("languages.json: {said}"));
                }
            }
        }
        it
    }

    /// Fold a file's worth of definitions in. A language already here is
    /// added to, field by field: a user file saying only `servers` for Rust
    /// keeps the grammar and the comment syntax.
    fn merge(&mut self, file: FileLanguages) {
        for (name, def) in file.languages {
            let name = name.trim().to_lowercase();
            if name.is_empty() {
                continue;
            }
            match self.langs.iter().position(|l| l.name == name) {
                Some(at) => {
                    let id = self.langs[at].id;
                    let existing = &mut self.langs[at];
                    apply(existing, def, id, &mut self.problems);
                }
                None => {
                    let id = LangId(self.langs.len() as u16);
                    let mut lang = Lang {
                        id,
                        name: name.clone(),
                        lsp_id: name,
                        extensions: Vec::new(),
                        filenames: Vec::new(),
                        shebangs: Vec::new(),
                        line_comment: None,
                        block_comment: None,
                        brackets: default_brackets(),
                        servers: Vec::new(),
                        grammar: None,
                        compiled: OnceLock::new(),
                    };
                    apply(&mut lang, def, id, &mut self.problems);
                    self.langs.push(lang);
                }
            }
        }
    }
}

/// The brackets nearly every language has. A language saying nothing about
/// brackets means these, not none.
fn default_brackets() -> Vec<(char, char)> {
    vec![('(', ')'), ('[', ']'), ('{', '}')]
}

/// Write one definition over a language.
fn apply(lang: &mut Lang, def: FileLang, id: LangId, problems: &mut Vec<String>) {
    if let Some(v) = def.lsp_id {
        lang.lsp_id = v;
    }
    if let Some(v) = def.extensions {
        lang.extensions = v.into_iter().map(|e| e.trim_start_matches('.').to_lowercase()).collect();
    }
    if let Some(v) = def.filenames {
        lang.filenames = v;
    }
    if let Some(v) = def.shebangs {
        lang.shebangs = v;
    }
    if let Some(v) = def.line_comment {
        lang.line_comment = Some(v).filter(|s| !s.is_empty());
    }
    if let Some(v) = def.block_comment {
        lang.block_comment = (v.len() == 2).then(|| (v[0].clone(), v[1].clone()));
    }
    if let Some(v) = def.brackets {
        lang.brackets = v
            .iter()
            .filter_map(|pair| {
                let mut chars = pair.chars();
                Some((chars.next()?, chars.next()?))
            })
            .collect();
    }
    if let Some(v) = def.servers {
        lang.servers = v
            .into_iter()
            .filter(|s| !s.command.trim().is_empty())
            .map(|s| Server {
                command: s.command,
                args: s.args,
                roots: if s.roots.is_empty() {
                    // Every project has one of these somewhere above it, and
                    // stopping at a repository root is nearly always right.
                    vec![".git".into()]
                } else {
                    s.roots
                },
                init_options: s.init_options,
                settings: s.settings,
                env: s.env,
            })
            .collect();
    }
    if let Some(g) = def.grammar {
        match g {
            FileGrammar::BuiltIn { built_in } => match built_in_grammar(&built_in) {
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
                let symbol = symbol.unwrap_or_else(|| {
                    format!("tree_sitter_{}", lang.name.replace(['-', '.'], "_"))
                });
                lang.grammar = Some(GrammarSource::Library {
                    path: expand(&library),
                    symbol,
                    highlights: expand(&highlights),
                });
            }
            FileGrammar::None {} => lang.grammar = None,
        }
    }
    let _ = id;
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
        "css" => tree_sitter_css::LANGUAGE, [tree_sitter_css::HIGHLIGHTS_QUERY],
        "go" => tree_sitter_go::LANGUAGE, [tree_sitter_go::HIGHLIGHTS_QUERY],
        "html" => tree_sitter_html::LANGUAGE, [tree_sitter_html::HIGHLIGHTS_QUERY],
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

/// `languages.json`, as its file writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLanguages {
    /// Notes to whoever opens the file. JSON has nowhere to put a comment, so
    /// it gets a key and is read into nothing.
    #[serde(default, rename = "_about")]
    _about: Option<serde_json::Value>,
    #[serde(default)]
    languages: BTreeMap<String, FileLang>,
}

/// One language. Every field optional, because a file that only wants to
/// change the server should only have to say the server.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLang {
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
    servers: Option<Vec<FileServer>>,
    #[serde(default)]
    grammar: Option<FileGrammar>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServer {
    command: String,
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

#[derive(Deserialize)]
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
    fn the_languages_textfold_ships_all_read() {
        let file: FileLanguages =
            serde_json::from_str(include_str!("languages.json")).expect("valid JSON");
        assert!(file.languages.contains_key("rust"));
        for (name, def) in &file.languages {
            if let Some(FileGrammar::BuiltIn { built_in }) = &def.grammar {
                assert!(
                    built_in_grammar(built_in).is_some(),
                    "{name} wants a grammar called {built_in:?} that is not compiled in"
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
    fn a_user_file_adds_to_a_language_rather_than_replacing_it() {
        let mut langs = Languages {
            langs: Vec::new(),
            problems: Vec::new(),
        };
        langs.merge(
            serde_json::from_str(
                r#"{"languages":{"rust":{"extensions":["rs"],"line_comment":"//",
                   "servers":[{"command":"rust-analyzer"}]}}}"#,
            )
            .unwrap(),
        );
        langs.merge(
            serde_json::from_str(r#"{"languages":{"rust":{"servers":[{"command":"ra-multiplex"}]}}}"#)
                .unwrap(),
        );
        let rust = &langs.langs[0];
        assert_eq!(rust.servers[0].command, "ra-multiplex");
        // The parts the second file said nothing about survived.
        assert_eq!(rust.line_comment.as_deref(), Some("//"));
        assert_eq!(rust.extensions, ["rs"]);
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
