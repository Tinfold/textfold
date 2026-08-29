//! Which Java to run jdtls with, and which Javas a project can be built
//! against.
//!
//! These are two questions and not one, and the first thing anybody learns
//! about jdtls is that they are not the same answer. jdtls is itself a Java
//! program and wants a recent JDK to run at all; the project it is being
//! asked about wants whatever that project targets, which on a great many
//! real code bases is 8 or 11. A server started under 8 does not start, and a
//! server that only knows about 21 reports every line of an 8 project as an
//! error. Both halves have to be said, and the machine already knows the
//! answer to both.
//!
//! So a manifest asks by name, and this is what answers:
//!
//! - `${java_home}` — a JDK new enough to run the server itself.
//! - `${java}` — the `java` inside it, for anything that wants the program
//!   rather than the directory.
//! - `${java_runtimes}` — every JDK here, in the shape jdtls'
//!   `java.configuration.runtimes` is written in. A list, not a string, which
//!   is why [`crate::venv::Vars`] knows how to substitute a whole value and
//!   not only a stretch of text.
//!
//! Where they are looked for, in the order that decides:
//!
//! 1. `java_home` in your settings. Saying it outright is the point of having
//!    a settings file, and it wins over anything found by looking.
//! 2. `JAVA_HOME`, which is what the rest of your toolchain is already using.
//! 3. The directories JDKs are installed into on this kind of machine, and
//!    the ones the version managers use — sdkman, asdf, jenv, jabba.
//! 4. The `java` on the `PATH`, followed through its symbolic links to the
//!    directory it lives in. This is the one that works on a machine where
//!    somebody unpacked a JDK wherever they felt like it.
//!
//! Nothing here runs `java -version`. A JDK says what it is in the `release`
//! file at the top of it, reading a file is a great deal cheaper than starting
//! a JVM, and this happens while somebody is waiting for a Java file to open.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde_json::{Value, json};

/// The oldest JDK that can run jdtls. Recent releases want 21; 17 is what the
/// ones still in wide use want, and offering the newest thing here regardless
/// means the choice only goes wrong on a machine that has nothing new enough
/// for either.
const RUNS_THE_SERVER: u32 = 17;

/// One JDK on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Jdk {
    /// The top of it: the directory with `bin/java` in it.
    pub home: PathBuf,
    /// 8, 11, 17, 21 — the number people say out loud, so `1.8.0_392` is 8.
    pub version: u32,
    /// Whether it can compile as well as run. jdtls needs a JDK rather than a
    /// JRE for a project's runtime, because a runtime it cannot compile
    /// against is a runtime that reports the standard library as missing.
    pub compiles: bool,
}

impl Jdk {
    /// What Eclipse calls this version: `JavaSE-1.8` for 8, and `JavaSE-21`
    /// for everything since 9. Not a shape anybody would invent, and jdtls
    /// silently ignores a runtime whose name is not one of them.
    pub fn execution_environment(&self) -> String {
        match self.version {
            0..=8 => format!("JavaSE-1.{}", self.version.max(1)),
            v => format!("JavaSE-{v}"),
        }
    }

    /// The program itself.
    pub fn java(&self) -> PathBuf {
        self.home.join("bin").join(exe("java"))
    }
}

/// What settings said, if anything. Set once at startup, before any Java file
/// can have been opened.
static SAID: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn said() -> &'static RwLock<Option<PathBuf>> {
    SAID.get_or_init(|| RwLock::new(None))
}

/// Take note of the `java_home` in the settings file.
///
/// Called again when the setting is changed from inside the editor, which is
/// why what has been found is thrown away rather than kept: the whole point of
/// changing it is that the answer should be different afterwards.
pub fn configure(home: Option<&str>) {
    let home = home
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(expand);
    let mut said = said().write().unwrap_or_else(|e| e.into_inner());
    if *said == home {
        return;
    }
    *said = home;
    forget();
}

/// Every JDK found, newest first, worked out once.
static FOUND: OnceLock<RwLock<Option<&'static [Jdk]>>> = OnceLock::new();

fn cell() -> &'static RwLock<Option<&'static [Jdk]>> {
    FOUND.get_or_init(|| RwLock::new(None))
}

/// Throw away what was found, so the next question looks again.
fn forget() {
    *cell().write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Every JDK on this machine, newest first.
///
/// Worked out on the first question and remembered, because the answer is a
/// walk of several directories and most projects are not Java. Leaked rather
/// than dropped for the same reason the language registry is: it is asked for
/// as long as the editor runs, and there is one of it.
pub fn all() -> &'static [Jdk] {
    if let Some(found) = *cell().read().unwrap_or_else(|e| e.into_inner()) {
        return found;
    }
    let found: &'static [Jdk] = Box::leak(look().into_boxed_slice());
    *cell().write().unwrap_or_else(|e| e.into_inner()) = Some(found);
    found
}

/// The JDK to run a program that needs at least `least`, or the newest there
/// is if nothing is new enough — which at least produces an error from Java
/// saying what it wanted rather than nothing happening at all.
pub fn newest(least: u32) -> Option<&'static Jdk> {
    let all = all();
    all.iter().find(|j| j.version >= least).or_else(|| all.first())
}

/// What a `${…}` in a manifest means, where it is one of ours. `None` for a
/// name we know nothing about, and for one we do know and cannot answer —
/// which is what makes a setting naming a JDK disappear on a machine with no
/// Java rather than being filled in with an empty string.
pub fn var(name: &str) -> Option<String> {
    match name {
        "java_home" => Some(newest(RUNS_THE_SERVER)?.home.display().to_string()),
        "java" => Some(newest(RUNS_THE_SERVER)?.java().display().to_string()),
        _ => None,
    }
}

/// The same for a placeholder whose answer is not a string.
pub fn value(name: &str) -> Option<Value> {
    match name {
        "java_runtimes" => runtimes(all()),
        _ => None,
    }
}

/// The JDKs, in the shape jdtls' `java.configuration.runtimes` is written in.
///
/// Only the ones that can compile: a JRE offered as a project runtime is a
/// project whose standard library jdtls reports as missing. `None` where
/// there are none, so that the setting disappears rather than arriving empty
/// — an empty list is jdtls being told there is nothing to build against,
/// which is worse than not being told.
fn runtimes(jdks: &[Jdk]) -> Option<Value> {
    let mut out: Vec<Value> = Vec::new();
    for jdk in jdks.iter().filter(|j| j.compiles) {
        let name = jdk.execution_environment();
        // Two installs of the same version are one runtime; jdtls takes the
        // first and complains about the rest.
        if out.iter().any(|r| r["name"] == json!(name)) {
            continue;
        }
        out.push(json!({
            "name": name,
            "path": jdk.home.display().to_string(),
            // The newest is what a project that says nothing about what it
            // targets gets, which is the one you would have used by hand.
            "default": out.is_empty(),
        }));
    }
    (!out.is_empty()).then_some(Value::Array(out))
}

// ---------------------------------------------------------------------------
// Looking
// ---------------------------------------------------------------------------

fn look() -> Vec<Jdk> {
    let mut found: BTreeMap<PathBuf, Jdk> = BTreeMap::new();
    let mut add = |dir: PathBuf| {
        if let Some(jdk) = read(&dir) {
            found.entry(jdk.home.clone()).or_insert(jdk);
        }
    };

    // What you said, then what the rest of your toolchain is using.
    if let Some(home) = said().read().unwrap_or_else(|e| e.into_inner()).clone() {
        add(home);
    }
    if let Some(home) = std::env::var_os("JAVA_HOME").filter(|h| !h.is_empty()) {
        add(PathBuf::from(home));
    }

    // The directories JDKs land in, and the ones the version managers use.
    for dir in searched() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            add(path.clone());
            // macOS buries it, and always in the same place.
            add(path.join("Contents").join("Home"));
        }
    }

    // And the one on the `PATH`, followed to where it actually lives. This is
    // the answer on a machine where somebody unpacked a JDK wherever they
    // felt like it.
    if let Some(java) = which("java") {
        let real = std::fs::canonicalize(&java).unwrap_or(java);
        if let Some(home) = real.parent().and_then(Path::parent) {
            add(home.to_path_buf());
        }
    }

    let mut all: Vec<Jdk> = found.into_values().collect();
    // Newest first, and a JDK before a JRE of the same version, because the
    // first of these is what runs the server and what a project that says
    // nothing gets.
    all.sort_by(|a, b| {
        b.version
            .cmp(&a.version)
            .then(b.compiles.cmp(&a.compiles))
            .then(a.home.cmp(&b.home))
    });
    all
}

/// Where to look, on this kind of machine.
fn searched() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.push("/Library/Java/JavaVirtualMachines".into());
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join("Library/Java/JavaVirtualMachines"));
        }
    }
    if cfg!(windows) {
        for base in ["C:\\Program Files\\Java", "C:\\Program Files\\Eclipse Adoptium"] {
            dirs.push(base.into());
        }
    }
    if cfg!(unix) && !cfg!(target_os = "macos") {
        for base in ["/usr/lib/jvm", "/usr/java", "/opt/java", "/opt/jdk"] {
            dirs.push(base.into());
        }
    }
    // The version managers, which is where a JDK is on the machine of anybody
    // who deals with more than one of them.
    if let Some(home) = dirs::home_dir() {
        for under in [
            ".sdkman/candidates/java",
            ".asdf/installs/java",
            ".jenv/versions",
            ".jabba/jdk",
            ".jdks",
            "Library/Java/JavaVirtualMachines",
        ] {
            dirs.push(home.join(under));
        }
    }
    dirs
}

/// Read one directory as a JDK, or `None` if it is not one.
fn read(dir: &Path) -> Option<Jdk> {
    let bin = dir.join("bin");
    if !bin.join(exe("java")).is_file() {
        return None;
    }
    // Followed to where it really is, so that `/usr/lib/jvm/default` and the
    // directory it points at are one JDK rather than two.
    let home = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    Some(Jdk {
        version: version_of(&home)?,
        compiles: bin.join(exe("javac")).is_file(),
        home,
    })
}

/// Which Java this is, out of the `release` file at the top of it — and out
/// of the directory's name where there is no such file, which is how the
/// oldest installs and a handful of repackaged ones look.
fn version_of(home: &Path) -> Option<u32> {
    if let Ok(text) = std::fs::read_to_string(home.join("release"))
        && let Some(said) = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("JAVA_VERSION="))
        && let Some(version) = number_in(said.trim().trim_matches('"'))
    {
        return Some(version);
    }
    number_in(&home.file_name()?.to_string_lossy())
}

/// The version a string like `21.0.5`, `1.8.0_392`, `java-17-openjdk` or
/// `temurin-21.jdk` is talking about.
///
/// `1.8` is 8. Java called itself `1.x` until 9 and every tool that reads a
/// version has had to know it ever since.
fn number_in(text: &str) -> Option<u32> {
    let digits: Vec<u32> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    match digits.first()? {
        1 => digits.get(1).copied(),
        first => Some(*first),
    }
}

/// `java.exe` where that is what it is called.
fn exe(name: &str) -> String {
    match cfg!(windows) {
        true => format!("{name}.exe"),
        false => name.to_string(),
    }
}

/// Where a program on the `PATH` is, as a path.
fn which(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let full = dir.join(exe(command));
        full.is_file().then_some(full)
    })
}

/// `~/…` the way a person writes it in a settings file.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_the_number_a_person_says_out_loud() {
        // Java called itself `1.x` until 9, and every tool that reads a
        // version has had to know it ever since.
        assert_eq!(number_in("21.0.5"), Some(21));
        assert_eq!(number_in("1.8.0_392"), Some(8));
        assert_eq!(number_in("java-17-openjdk"), Some(17));
        assert_eq!(number_in("temurin-21.jdk"), Some(21));
        assert_eq!(number_in("jdk-11.0.22+7"), Some(11));
        assert_eq!(number_in("graalvm-community-openjdk-21.0.2"), Some(21));
        assert_eq!(number_in("no digits here"), None);
    }

    #[test]
    fn eclipse_is_told_the_name_it_uses_for_a_version() {
        // jdtls silently ignores a runtime whose name is not one of these,
        // which is a failure with nothing at all to see.
        let at = |version| Jdk {
            home: PathBuf::from("/x"),
            version,
            compiles: true,
        }
        .execution_environment();
        assert_eq!(at(8), "JavaSE-1.8");
        assert_eq!(at(11), "JavaSE-11");
        assert_eq!(at(21), "JavaSE-21");
    }

    #[test]
    fn a_directory_with_no_java_in_it_is_not_a_jdk() {
        let dir = std::env::temp_dir().join(format!("textfold-jdk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a place to work");
        assert_eq!(read(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_jdk_is_read_out_of_its_release_file() {
        let dir = std::env::temp_dir().join(format!("textfold-jdk-read-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("bin")).expect("a place to work");
        std::fs::write(dir.join("bin/java"), "").expect("written");
        std::fs::write(dir.join("bin/javac"), "").expect("written");
        std::fs::write(dir.join("release"), "JAVA_VERSION=\"17.0.9\"\nOS_ARCH=\"x86_64\"\n")
            .expect("written");
        let jdk = read(&dir).expect("a JDK");
        assert_eq!(jdk.version, 17);
        // It can compile, so a project may be built against it.
        assert!(jdk.compiles);

        // Without `javac` it can run the server and nothing else.
        std::fs::remove_file(dir.join("bin/javac")).expect("removed");
        assert!(!read(&dir).expect("still a JDK").compiles);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_placeholder_nobody_here_knows_is_not_answered() {
        // The whole point of answering `None`: a setting naming a JDK
        // disappears on a machine with no Java rather than being filled in
        // with an empty string that points at the root of the disk.
        assert_eq!(var("venv"), None);
        assert_eq!(value("java_home"), None);
    }
}
