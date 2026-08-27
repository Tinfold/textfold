//! Which Python a project means.
//!
//! A Python project is very rarely the Python on your `PATH`. It is the one in
//! the virtual environment beside it, and every package it imports lives in
//! there — so a type checker pointed at the wrong interpreter does not merely
//! lose a few completions. It reads a different set of libraries, or none, and
//! then reports at length on code that is perfectly correct. `pydantic` is the
//! usual way people meet this: a `Settings()` that takes its values from the
//! environment is written with no arguments, and a checker that cannot see the
//! installed `pydantic-settings` has no way to know that and says the
//! arguments are missing.
//!
//! Finding the environment is guesswork, but it is well-worn guesswork: the
//! layout of a virtual environment is fixed, and the names people give them
//! are a short list. What is found is offered rather than assumed, because the
//! project with three of them is a real project and only the person sitting
//! there knows which one they meant.
//!
//! Nothing here is wired to Python by the code that uses it. A server in
//! `languages.json` asks for an environment by writing `${venv}` or
//! `${python}` in its `env`, `args` or `settings`, and a server that does not
//! mention them never sees one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The directory names people give a virtual environment, in the order they
/// should win. `.venv` first because it is what every tool that makes one for
/// you now uses.
const NAMES: &[&str] = &[".venv", "venv", ".virtualenv", "virtualenv", "env", ".env"];

/// One Python environment textfold could point a language server at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Env {
    /// The directory it lives in — what `VIRTUAL_ENV` is set to.
    pub root: PathBuf,
    /// The interpreter inside it.
    pub python: PathBuf,
    /// What to call it in a list: the directory's own name, or the path when
    /// that would be ambiguous.
    pub name: String,
    /// The line under the name: where it came from and which Python it is.
    pub about: String,
}

impl Env {
    /// The directory the interpreter is in — `bin` on everything except
    /// Windows, which puts it in `Scripts`.
    pub fn bin(&self) -> PathBuf {
        self.python
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.join("bin"))
    }
}

/// The interpreter inside a directory, if that directory is an environment.
///
/// Both layouts, because a project checked out on a Windows machine and edited
/// over `ssh` is a thing that happens, and because looking for the wrong one
/// costs a `stat`.
fn interpreter(dir: &Path) -> Option<PathBuf> {
    let tries = [
        dir.join("bin").join("python3"),
        dir.join("bin").join("python"),
        dir.join("Scripts").join("python.exe"),
        dir.join("Scripts").join("python3.exe"),
    ];
    tries.into_iter().find(|p| p.is_file())
}

/// What `pyvenv.cfg` says the version is, which is the one fact worth showing
/// beside the name: two environments in a project are usually two Pythons.
fn version(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("pyvenv.cfg")).ok()?;
    for line in text.lines() {
        let (key, value) = line.split_once('=')?;
        if key.trim() == "version" || key.trim() == "version_info" {
            return Some(value.trim().to_string());
        }
    }
    None
}

fn env_at(dir: &Path, about: &str) -> Option<Env> {
    let python = interpreter(dir)?;
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let about = match version(&root) {
        Some(version) => format!("{about} — Python {version}"),
        None => about.to_string(),
    };
    Some(Env {
        python,
        root,
        name,
        about,
    })
}

/// Every environment that looks as though it belongs to this project, best
/// first.
///
/// "Best" is: the one the shell is already in, then the ones inside the
/// project by the usual names, then anything else in the project with a
/// `pyvenv.cfg` in it, then conda. The order is the order a person would guess
/// in, and the first is what gets used when nobody has said otherwise.
pub fn found(project: &Path) -> Vec<Env> {
    let mut out: Vec<Env> = Vec::new();
    let mut push = |env: Option<Env>| {
        if let Some(env) = env
            && !out.iter().any(|seen| seen.root == env.root)
        {
            out.push(env);
        }
    };

    // The shell it was started from. Somebody who ran `source .venv/bin/activate`
    // and then opened an editor has already said which one they meant.
    if let Some(active) = std::env::var_os("VIRTUAL_ENV").filter(|v| !v.is_empty()) {
        push(env_at(Path::new(&active), "the environment you are in"));
    }
    for name in NAMES {
        push(env_at(&project.join(name), "in the project"));
    }
    // Anything else in the project that is one. A `pyvenv.cfg` is the only
    // thing that says so without ambiguity, and reading the directory once is
    // cheaper than guessing at more names.
    if let Ok(entries) = std::fs::read_dir(project) {
        let mut others: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("pyvenv.cfg").is_file())
            .collect();
        others.sort();
        for dir in others {
            push(env_at(&dir, "in the project"));
        }
    }
    if let Some(conda) = std::env::var_os("CONDA_PREFIX").filter(|v| !v.is_empty()) {
        push(env_at(Path::new(&conda), "conda"));
    }
    out
}

/// The environment to use for a project: the one chosen, if it is still there,
/// and otherwise the first one found.
pub fn chosen(project: &Path, picked: Option<&Path>) -> Option<Env> {
    let found = found(project);
    if let Some(picked) = picked
        && let Some(env) = found.iter().find(|e| e.root == picked)
    {
        return Some(env.clone());
    }
    // A choice that no longer exists is not silently swapped for another one
    // in the list — but it is not a reason to have no environment either, so
    // the environment is looked up afresh in case it moved rather than went.
    if let Some(picked) = picked
        && let Some(env) = env_at(picked, "chosen")
    {
        return Some(env);
    }
    found.into_iter().next()
}

// ---------------------------------------------------------------------------
// Putting it into a server's configuration.
// ---------------------------------------------------------------------------

/// The things `${…}` can stand for in a server's configuration.
pub struct Vars {
    values: BTreeMap<&'static str, String>,
}

impl Vars {
    /// `root` is the project, `env` the environment to point at — `None` where
    /// none was found, which is what makes a setting that needs one disappear
    /// rather than being filled in with nonsense.
    pub fn new(root: &Path, env: Option<&Env>) -> Self {
        let mut values = BTreeMap::new();
        values.insert("root", root.display().to_string());
        if let Some(env) = env {
            values.insert("venv", env.root.display().to_string());
            values.insert("python", env.python.display().to_string());
            values.insert("venv_bin", env.bin().display().to_string());
        }
        Self { values }
    }

    /// Another name a placeholder can use. What lets a tool ask for
    /// `${file}`, which is a fact about the buffer rather than about the
    /// project and so is not known when the table is built.
    pub fn set(&mut self, name: &'static str, value: String) {
        self.values.insert(name, value);
    }

    /// One string with its placeholders filled in. `None` where the string
    /// asks for something there is no answer to.
    ///
    /// All or nothing on purpose. A `PATH` built out of an environment that
    /// does not exist is worse than no `PATH` at all, and a `pythonPath`
    /// pointing at `/bin/python` because the substitution quietly left a hole
    /// is the exact failure this module is here to prevent.
    pub fn fill(&self, text: &str) -> Option<String> {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find("${") {
            out.push_str(&rest[..at]);
            let Some(end) = rest[at..].find('}').map(|n| at + n) else {
                // An unclosed `${` is text, not a placeholder.
                break;
            };
            let name = &rest[at + 2..end];
            match name.strip_prefix("env:") {
                // `${env:PATH}` — what the editor was started with, so a
                // server's `PATH` can be extended rather than replaced.
                Some(var) => out.push_str(&std::env::var(var).unwrap_or_default()),
                None => out.push_str(self.values.get(name)?),
            }
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Some(out)
    }

    /// The same through a whole settings object.
    ///
    /// A key whose value cannot be filled in is dropped, taking any object
    /// that is then empty with it — so a `settings` block that only ever said
    /// where Python is says nothing at all on a machine with no environment,
    /// rather than saying something wrong.
    pub fn fill_value(&self, value: &Value) -> Option<Value> {
        match value {
            Value::String(text) => self.fill(text).map(Value::String),
            Value::Array(items) => Some(Value::Array(
                items.iter().filter_map(|v| self.fill_value(v)).collect(),
            )),
            Value::Object(fields) => {
                let kept: serde_json::Map<String, Value> = fields
                    .iter()
                    .filter_map(|(k, v)| Some((k.clone(), self.fill_value(v)?)))
                    .collect();
                (!kept.is_empty() || fields.is_empty()).then_some(Value::Object(kept))
            }
            other => Some(other.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars() -> Vars {
        Vars {
            values: BTreeMap::from([
                ("root", "/work/app".to_string()),
                ("venv", "/work/app/.venv".to_string()),
                ("python", "/work/app/.venv/bin/python".to_string()),
                ("venv_bin", "/work/app/.venv/bin".to_string()),
            ]),
        }
    }

    fn nothing() -> Vars {
        Vars {
            values: BTreeMap::from([("root", "/work/app".to_string())]),
        }
    }

    #[test]
    fn a_placeholder_is_replaced_by_what_it_names() {
        assert_eq!(
            vars().fill("${venv}/bin:x").as_deref(),
            Some("/work/app/.venv/bin:x")
        );
    }

    #[test]
    fn a_string_asking_for_something_that_is_not_there_is_dropped_whole() {
        assert_eq!(nothing().fill("${venv}/bin"), None);
    }

    #[test]
    fn text_that_only_looks_like_a_placeholder_is_left_alone() {
        assert_eq!(nothing().fill("costs $100").as_deref(), Some("costs $100"));
        assert_eq!(nothing().fill("${unclosed").as_deref(), Some("${unclosed"));
    }

    #[test]
    fn settings_that_need_an_environment_vanish_without_one() {
        let settings = json!({ "python": { "pythonPath": "${python}" } });
        assert_eq!(
            vars().fill_value(&settings),
            Some(json!({ "python": { "pythonPath": "/work/app/.venv/bin/python" } }))
        );
        assert_eq!(
            nothing().fill_value(&settings),
            None,
            "an empty object was left where a setting used to be"
        );
    }

    #[test]
    fn only_the_part_that_needs_an_environment_goes() {
        let settings = json!({
            "python": { "pythonPath": "${python}", "analysis": { "typeCheckingMode": "basic" } },
        });
        assert_eq!(
            nothing().fill_value(&settings),
            Some(json!({ "python": { "analysis": { "typeCheckingMode": "basic" } } }))
        );
    }

    #[test]
    fn an_environment_is_found_by_the_shape_of_it() {
        let dir = std::env::temp_dir().join(format!("textfold-venv-{}", std::process::id()));
        let bin = dir.join("project").join(".venv").join("bin");
        std::fs::create_dir_all(&bin).expect("made");
        std::fs::write(bin.join("python3"), "").expect("written");
        std::fs::write(
            dir.join("project").join(".venv").join("pyvenv.cfg"),
            "home = /usr/bin\nversion = 3.12.1\n",
        )
        .expect("written");

        let found = found(&dir.join("project"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, ".venv");
        assert!(found[0].about.contains("3.12.1"), "{}", found[0].about);
        std::fs::remove_dir_all(&dir).ok();
    }
}
