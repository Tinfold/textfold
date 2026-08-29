//! Packages: getting a plugin onto this machine, and off it again.
//!
//! A plugin is two things, and installing one means dealing with both. There
//! is the plugin itself — a manifest and whatever files sit beside it — and
//! there is what it needs in order to do anything, which is nearly always a
//! program somebody else wrote. A `pyright` plugin that is on your disk and
//! has no `pyright-langserver` to run is a switch wired to nothing.
//!
//! So an install is: put the files where textfold looks, then run the steps
//! the manifest gives for fetching what it needs, then check that they worked.
//! An uninstall is the same in reverse. Both are the same machinery whether
//! the plugin ships in the binary or came out of a directory you pointed at,
//! because the only difference between those is whether there are files to
//! copy.
//!
//! ```text
//! textfold --list-packages
//! textfold --install ./my-plugin          a directory with a plugin.json in it
//! textfold --install pyright              something a repository is offering
//! textfold --refresh                      ask the repositories what they have
//! textfold --update                       fetch a newer version of anything
//! textfold --uninstall cargo
//! ```
//!
//! A package comes from one of three places, and they are one list from where
//! you are sitting. It is sitting in `~/.config/textfold/packages`, or in any
//! other directory named in `package_paths` — which is how a checkout of
//! somebody's plugins becomes rows to choose from. Or it is in a package
//! repository, which is a URL with an `index.json` under it: see
//! [`crate::repo`], and [`Origin::Remote`] for what installing one does.
//!
//! ## Versions and updates
//!
//! A manifest may say what version it is. What is installed is remembered in
//! the receipt beside it, and a repository offering a higher number is an
//! update — one row in the plugins list with `update` in the margin, and one
//! `--update` to take it. Nothing is ever fetched and run on its own: the
//! refresh at startup changes what the lists say and nothing else.
//!
//! A plugin that declines to number itself is one nothing is ever an update
//! to, which is better than reinstalling somebody's plugin forever for a
//! version it never claimed.
//!
//! ## Steps
//!
//! An installer is a list of programs to run, not a script, and the rules for
//! reading one are three sentences:
//!
//! - A step whose `unless` program is already on the `PATH` is skipped. There
//!   is nothing to do.
//! - A step whose *own* program is not installed is skipped too, and this is
//!   the load-bearing one: it is what lets a plugin offer `uv`, then `pipx`,
//!   then `pip` as three ways to get the same thing and have the first one
//!   that exists be the one that runs. A step you cannot run is not a step
//!   that failed.
//! - A step that runs and comes back unhappy stops the install there. That is
//!   a real failure and carrying on past it would only make a worse mess.
//!
//! Which leaves the question of whether it worked, and that is not answered by
//! the exit codes. It is answered by looking: `needs` names the programs the
//! plugin has to have, and if one of them is still not there when the last
//! step has run, the install failed however cheerful the steps were about it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};

use crate::plugin::{Plugin, Step};

/// The file a package leaves behind saying that textfold put it there.
///
/// The whole of uninstall's safety. Removing a directory is not a thing to do
/// on a guess, and this is the difference between a directory textfold copied
/// in — which it may take away again — and one you wrote by hand in the same
/// place, which it may not.
const RECEIPT: &str = ".installed.json";

/// What a package directory has to have in it.
const MANIFEST: &str = "plugin.json";

/// Where a package comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// It ships in the binary. There is nothing to copy, so installing it
    /// means only getting the programs it runs.
    BuiltIn,
    /// A directory on this machine with a `plugin.json` in it, or a single
    /// manifest file. Installing it copies it in.
    Path(PathBuf),
    /// Already installed here, by us. The path is what uninstall may remove.
    Here(PathBuf),
    /// Here, but not by us: something you wrote or linked into the plugins
    /// directory yourself. Its steps can be run; its files are yours.
    Yours,
    /// A package repository has it. Installing it fetches the tarball, checks
    /// it against what the index said, and unpacks it where a package from a
    /// directory would have been copied.
    Remote(Box<Remote>),
}

/// A package a repository is offering, and which repository is offering it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub repository: crate::repo::Repository,
    pub entry: crate::repo::Entry,
}

impl PartialEq for crate::repo::Entry {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.version == other.version && self.url == other.url
    }
}
impl Eq for crate::repo::Entry {}

impl Origin {
    pub fn label(&self) -> String {
        match self {
            Origin::BuiltIn => "built in".into(),
            Origin::Path(path) => path.display().to_string(),
            Origin::Here(path) => path.display().to_string(),
            Origin::Yours => "yours".into(),
            Origin::Remote(remote) => remote.repository.name.clone(),
        }
    }
}

/// One thing that could be installed or removed.
#[derive(Clone, Debug)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub about: String,
    pub origin: Origin,
    /// What it needs that is not on this machine.
    pub missing: Vec<String>,
    /// Whether the plugin itself is already here — as against its files still
    /// sitting in the directory you would install them from.
    pub here: bool,
    /// What version is here, where one is installed and says.
    pub installed: Option<String>,
    /// What version could be here, where a repository is offering one.
    pub offered: Option<String>,
}

impl Package {
    /// Whether there is a newer version to be had than the one installed.
    pub fn has_update(&self) -> bool {
        match (&self.installed, &self.offered) {
            (Some(installed), Some(offered)) => crate::repo::is_newer(offered, installed),
            // Nothing said about what is here. An update is a thing you can
            // only offer against a version, and guessing would mean
            // reinstalling somebody's plugin because it declined to number
            // itself.
            _ => false,
        }
    }
}

impl Package {
    /// The line under the name in a list, which is the one place most people
    /// will read what a package is and what state it is in.
    pub fn detail(&self) -> String {
        let mut said = self.about.clone();
        if self.has_update()
            && let (Some(installed), Some(offered)) = (&self.installed, &self.offered)
        {
            said = format!("{said} — {installed} → {offered}");
        } else if let Some(version) = self.offered.as_ref().or(self.installed.as_ref()) {
            said = format!("{said} — v{version}");
        }
        if !self.missing.is_empty() {
            said = format!("{said} — needs {}", self.missing.join(", "));
        }
        said
    }

    /// The word in the margin.
    pub fn tag(&self) -> &'static str {
        if self.has_update() {
            return "update";
        }
        match (self.here, self.missing.is_empty()) {
            (false, _) => "new",
            (true, false) => "needs",
            (true, true) => "ready",
        }
    }
}

/// Where packages may come from: the directories to look in, and the
/// repositories to fetch from.
///
/// One argument rather than two, because every question about packages needs
/// both and threading them separately through the editor would mean finding
/// every call site again the next time there is a third kind of source.
#[derive(Clone, Copy)]
pub struct Sources<'a> {
    pub paths: &'a [String],
    pub repositories: &'a [crate::repo::Repository],
}

impl<'a> Sources<'a> {
    /// From the settings file, which is where both of them live.
    pub fn of(config: &'a crate::config::Config) -> Self {
        Self {
            paths: config.package_paths(),
            repositories: config.package_repositories(),
        }
    }
}

// ---------------------------------------------------------------------------
// Finding things
// ---------------------------------------------------------------------------

/// Whether a program can be run by that name.
///
/// A name with a separator in it is a path and is checked as one; anything
/// else is looked for along the `PATH`, which is what the shell would do and
/// so is what a person means when they write `ruff` in a manifest.
pub fn on_path(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    let named = Path::new(command);
    if named.components().count() > 1 {
        // A path is a file somebody named exactly, and whether it carries an
        // executable bit is their business — a plugin may well be pointing at
        // something it hands to an interpreter, as a `.js` file handed to
        // node. Being there is the question that was asked.
        return named.exists();
    }
    // A bare name is looked up the way a shell would, and there it does have
    // to be runnable: a file on the `PATH` with no executable bit is not a
    // program, and treating it as one is how a download gets mistaken for a
    // finished install.
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let full = dir.join(command);
        runnable(&full)
            // Windows does not put the extension in the name you type.
            || ["exe", "cmd", "bat"]
                .iter()
                .any(|e| runnable(&full.with_extension(e)))
    })
}

/// Whether this is a file that could actually be run.
///
/// The executable bit is the difference, and it matters more than it sounds:
/// a step that downloads a program has not finished until the program can be
/// run, and a check that said yes to the downloaded-but-not-yet-executable
/// file would skip the step that makes it runnable and then report success.
fn runnable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// Where plugins are installed to.
pub fn plugins_dir() -> Option<PathBuf> {
    Some(crate::config::config_dir()?.join("plugins"))
}

// ---------------------------------------------------------------------------
// Textfold's own corner of the machine
// ---------------------------------------------------------------------------

/// Where the programs textfold installs go.
///
/// **Nothing textfold fetches is installed system-wide.** An editor that runs
/// `npm install -g` on your behalf and drops a package into the same place
/// your projects' toolchains live has done something you did not ask for and
/// cannot easily see, and the first sign of it is usually a version conflict
/// in something unrelated. So there is one directory, it belongs to textfold,
/// and removing it undoes everything textfold ever installed.
///
/// `$XDG_DATA_HOME/textfold/tools`, or `~/.local/share/textfold/tools`.
/// Deliberately not beside the settings, which on macOS is `~/Library/
/// Application Support/…`: this directory is full of executables, and a great
/// many of them are scripts whose first line names an interpreter by path. A
/// space in that path is a well-known way to break them.
pub fn tools_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("textfold").join("tools"));
    }
    #[cfg(windows)]
    {
        return Some(dirs::data_dir()?.join("textfold").join("tools"));
    }
    #[cfg(not(windows))]
    Some(
        dirs::home_dir()?
            .join(".local")
            .join("share")
            .join("textfold")
            .join("tools"),
    )
}

/// Where the programs themselves end up, which is what goes on the `PATH`.
pub fn bin_dir() -> Option<PathBuf> {
    Some(tools_dir()?.join("bin"))
}

/// Put textfold's own programs on the `PATH` of this process, so that every
/// child inherits them: language servers, tools, plugins' own programs, and
/// the install steps themselves.
///
/// **Last, not first.** What you have installed yourself is your choice and
/// goes on winning; textfold's copy is what there is when you have not got
/// one. An editor that quietly shadowed the `ruff` in your virtual environment
/// with a copy of its own would be a very difficult afternoon.
///
/// Doing it to the process rather than to each spawn is what makes it hold
/// everywhere without a list to keep: `${env:PATH}` in a server's manifest
/// picks it up, [`on_path`] picks it up, and there is no fourth place that
/// starts a program and was forgotten.
pub fn put_tools_on_path() {
    let Some(bin) = bin_dir() else { return };
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if dirs.contains(&bin) {
        return;
    }
    dirs.push(bin);
    if let Ok(joined) = std::env::join_paths(dirs) {
        // Safety: called once, at startup, before any thread is spawned.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

/// What an install step is run with, so that what it fetches lands in
/// textfold's own directory rather than in yours.
///
/// Every one of these is the variable that package manager already documents
/// for the purpose, which is why a manifest can go on saying `npm install
/// --global` — the obvious thing to write — and have it mean "global to
/// textfold". A plugin author does not have to know any of this, and cannot
/// get it wrong.
///
/// The exceptions are named rather than hidden: `brew` and `rustup` install
/// into the system and the Rust toolchain respectively and have no equivalent
/// knob, so a step that uses one says `"system": true` and is called out
/// before it runs.
fn install_env() -> Vec<(String, String)> {
    let Some(tools) = tools_dir() else {
        return Vec::new();
    };
    let bin = tools.join("bin");
    let at = |p: &PathBuf| p.display().to_string();
    vec![
        // npm: `--global` installs into $prefix/lib/node_modules and links the
        // programs into $prefix/bin.
        ("npm_config_prefix".into(), at(&tools)),
        // pip: `--user` installs into $PYTHONUSERBASE/{lib,bin}.
        ("PYTHONUSERBASE".into(), at(&tools)),
        ("PIPX_HOME".into(), at(&tools.join("pipx"))),
        ("PIPX_BIN_DIR".into(), at(&bin)),
        ("UV_TOOL_DIR".into(), at(&tools.join("uv"))),
        ("UV_TOOL_BIN_DIR".into(), at(&bin)),
        // cargo install puts the binary in $CARGO_INSTALL_ROOT/bin.
        ("CARGO_INSTALL_ROOT".into(), at(&tools)),
        ("GOBIN".into(), at(&bin)),
    ]
}

/// Where packages are looked for: the one beside the settings, and anywhere
/// else you have said.
///
/// The second half is what makes this a package manager rather than a copy
/// command. Point `package_paths` at a checkout of somebody's plugins and
/// every one of them is a row in a list with `install` beside it.
pub fn package_dirs(also: &[String]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(config) = crate::config::config_dir() {
        dirs.push(config.join("packages"));
    }
    for path in also {
        let path = expand(path);
        if !dirs.contains(&path) {
            dirs.push(path);
        }
    }
    dirs
}

/// `${bin}` and `${tools}`, for a step that fetches a program itself rather
/// than asking a package manager to. What lets a plugin whose language server
/// is only published as a tarball put it somewhere textfold will find it, and
/// somewhere removing textfold's tools directory takes it away again.
fn fill(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(tools) = tools_dir() {
        out = out.replace("${tools}", &tools.display().to_string());
        out = out.replace("${bin}", &tools.join("bin").display().to_string());
    }
    out
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

/// The manifest inside a package: the file itself, or the `plugin.json` in it.
fn manifest_in(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        let inside = path.join(MANIFEST);
        return inside.is_file().then_some(inside);
    }
    (path.is_file() && path.extension().is_some_and(|e| e == "json")).then(|| path.to_path_buf())
}

/// The id a package at this path would install as: what its manifest says, or
/// failing that what the directory is called.
fn id_at(path: &Path) -> Option<String> {
    let manifest = manifest_in(path)?;
    let named = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| serde_json::from_str::<Named>(&text).ok())
        .and_then(|n| n.id);
    let fallback = match path.is_dir() {
        true => path.file_name(),
        false => path.file_stem(),
    };
    named
        .map(|id| id.trim().to_lowercase())
        .filter(|id| !id.is_empty())
        .or_else(|| fallback.map(|n| n.to_string_lossy().to_lowercase()))
}

/// Just enough of a manifest to know what it calls itself, without caring
/// whether the rest of it is anything we understand. A package written for a
/// later textfold should still be nameable in a list.
#[derive(Deserialize)]
struct Named {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    about: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

/// A receipt: what textfold put here, and where it came from.
#[derive(Serialize, Deserialize, Default)]
struct Receipt {
    /// The path or URL it came from, for the list and for reinstalling.
    from: String,
    /// Which repository, where it came from one. What makes an update a
    /// question about the place it was got from rather than about whichever
    /// repository happens to be offering the id today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    /// What version was installed, which is the whole of what an update is
    /// decided by. Absent for a package installed before versions existed, or
    /// one whose manifest declines to number itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

/// The receipt beside an installed plugin, if textfold left one.
fn receipt_of(id: &str) -> Option<Receipt> {
    let path = plugins_dir()?.join(id).join(RECEIPT);
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// What version of a plugin is installed: what its own manifest says, and
/// failing that what the receipt remembers being fetched.
///
/// The manifest first, because that is the copy actually on the disk — a
/// receipt can be left behind by an install that was later edited by hand.
fn installed_version(id: &str) -> Option<String> {
    crate::plugin::find(id)
        .and_then(|p| p.version.clone())
        .or_else(|| receipt_of(id)?.version)
}

/// Whether there is anything at all at this path, a link that points nowhere
/// included.
///
/// [`Path::exists`] follows links, so it says no to a link whose target has
/// gone — and then everything that writes there fails with "file exists",
/// which is the least helpful way to be told. Linking a plugin in and later
/// installing the published copy over it is a thing the documentation
/// suggests doing, so this is a path that gets walked.
fn is_there(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Take away whatever is at this path: a link as a link, a directory as a
/// directory.
///
/// The distinction is the one uninstall already makes. Removing *the link*
/// is safe either way — what it points at is somebody's working copy and is
/// none of our business — and following it to delete the target would be the
/// worst thing this code could do.
fn remove_whatever_is_at(path: &Path) -> Result<(), String> {
    let Ok(what) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    match what.file_type().is_symlink() || what.is_file() {
        true => std::fs::remove_file(path),
        false => std::fs::remove_dir_all(path),
    }
    .map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether a plugin's directory is one textfold may take away again.
///
/// Three answers, and they are all different. A directory with our receipt in
/// it is ours to remove. A symbolic link is a link we made or you made, and
/// removing *the link* is safe either way — what it points at is untouched.
/// Anything else is yours, and the answer is no.
fn removable(id: &str) -> Option<PathBuf> {
    let dir = plugins_dir()?.join(id);
    let linked = std::fs::symlink_metadata(&dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if linked {
        return Some(dir);
    }
    dir.join(RECEIPT).is_file().then_some(dir)
}

/// Everything textfold could install: what it ships that is not working yet,
/// and every package sitting in a directory it has been told to look in.
///
/// The two are one list on purpose. "Install pyright" and "install this plugin
/// I was given" are the same sentence from where you are sitting, and having
/// to know which kind of thing you are asking for before you can ask is a
/// distinction the editor should be keeping to itself.
pub fn available(from: Sources) -> Vec<Package> {
    let mut out: Vec<Package> = Vec::new();
    let offered = crate::repo::offered(from.repositories);

    // Plugins that are here and are not going to work until something is
    // fetched, and plugins that are here with a newer version to be had.
    for plugin in crate::plugin::all() {
        let missing = plugin.missing();
        let installed = installed_version(&plugin.id);
        let newer = offered
            .iter()
            .find(|(_, entry)| entry.id == plugin.id)
            .filter(|(_, entry)| {
                installed
                    .as_deref()
                    .is_some_and(|had| crate::repo::is_newer(&entry.version, had))
            });
        // Nothing to fetch and nothing newer is nothing to offer.
        if newer.is_none() && (missing.is_empty() || !plugin.can_install()) {
            continue;
        }
        out.push(Package {
            id: plugin.id.clone(),
            name: plugin.name.clone(),
            about: plugin.detail(),
            // An update is a fetch from where it came from, so where there is
            // one the origin is the repository rather than the disk.
            origin: match newer {
                Some((repository, entry)) => Origin::Remote(Box::new(Remote {
                    repository: repository.clone(),
                    entry: entry.clone(),
                })),
                None => origin_of(plugin),
            },
            missing: missing.into_iter().map(str::to_string).collect(),
            here: true,
            offered: newer.map(|(_, entry)| entry.version.clone()),
            installed,
        });
    }

    // And packages nobody has installed yet.
    for dir in package_dirs(from.paths) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        // A settled order, so a list of packages is the same list twice
        // running rather than whatever the directory said first today.
        found.sort();
        for path in found {
            let Some(id) = id_at(&path) else { continue };
            if out.iter().any(|p| p.id == id) || crate::plugin::find(&id).is_some() {
                continue;
            }
            let named = manifest_in(&path)
                .and_then(|m| std::fs::read_to_string(m).ok())
                .and_then(|text| serde_json::from_str::<Named>(&text).ok());
            out.push(Package {
                name: named
                    .as_ref()
                    .and_then(|n| n.name.clone())
                    .unwrap_or_else(|| id.clone()),
                about: named
                    .as_ref()
                    .and_then(|n| n.about.clone())
                    .unwrap_or_else(|| path.display().to_string()),
                origin: Origin::Path(path),
                // What it needs cannot be known until it is read properly, and
                // reading it properly is what installing it does.
                missing: Vec::new(),
                here: false,
                installed: None,
                offered: None,
                id,
            });
        }
    }

    // And everything the repositories have that is not here at all. Last, so
    // that a package sitting in a directory you named beats a fetch — what is
    // on the machine already is what somebody meant.
    for (repository, entry) in offered {
        if out.iter().any(|p| p.id == entry.id) || crate::plugin::find(&entry.id).is_some() {
            continue;
        }
        out.push(Package {
            id: entry.id.clone(),
            name: entry.name.clone().unwrap_or_else(|| entry.id.clone()),
            about: entry
                .about
                .clone()
                .unwrap_or_else(|| format!("from {}", repository.name)),
            // What it will want, said before anything has been downloaded —
            // which is the point of the index carrying it.
            //
            // Except what it names inside itself. A plugin whose `needs` is
            // `${plugin}/node_modules/…` is naming a file that will exist once
            // it is installed, and there is nothing to fill `${plugin}` in
            // with until it is: checking would report every such plugin as
            // missing a program with a `${` in its name.
            missing: entry
                .needs
                .iter()
                .filter(|command| !command.contains("${") && !on_path(command))
                .cloned()
                .collect(),
            here: false,
            installed: None,
            offered: Some(entry.version.clone()),
            origin: Origin::Remote(Box::new(Remote { repository, entry })),
        });
    }
    out
}

/// Everything installed that a repository has a newer version of.
pub fn updates(from: Sources) -> Vec<Package> {
    available(from)
        .into_iter()
        .filter(Package::has_update)
        .collect()
}

/// Ask every repository what it has now.
///
/// Returns what went wrong with each one that did, rather than stopping: one
/// repository being unreachable is not a reason to have nothing from the
/// others.
pub fn refresh(from: &[crate::repo::Repository]) -> Vec<String> {
    crate::repo::repositories(from)
        .iter()
        .filter_map(|repository| crate::repo::refresh(repository).err())
        .collect()
}

/// Where a plugin that is already here came from.
fn origin_of(plugin: &Plugin) -> Origin {
    match &plugin.source {
        crate::plugin::Source::BuiltIn => Origin::BuiltIn,
        crate::plugin::Source::File(_) => match removable(&plugin.id) {
            Some(dir) => Origin::Here(dir),
            None => Origin::Yours,
        },
    }
}

/// Everything installed that could be removed, which is every plugin that
/// either has files textfold put here or knows how to undo what it fetched.
///
/// A plugin that is neither — one of the language definitions built into the
/// binary — is not on this list, because there is nothing removing it could
/// mean. Switching it off is what you want, and that is a different list.
pub fn removable_plugins() -> Vec<Package> {
    crate::plugin::all()
        .iter()
        .filter_map(|plugin| {
            let origin = origin_of(plugin);
            let has_files = matches!(origin, Origin::Here(_));
            if !has_files && plugin.uninstall.is_empty() {
                return None;
            }
            Some(Package {
                about: plugin.detail(),
                missing: plugin.missing().into_iter().map(str::to_string).collect(),
                here: true,
                installed: installed_version(&plugin.id),
                offered: None,
                origin,
                id: plugin.id.clone(),
                name: plugin.name.clone(),
            })
        })
        .collect()
}

/// The package something on the command line names: a path if it is one, and
/// otherwise the id of a plugin or a package.
pub fn find(what: &str, from: Sources) -> Result<Package, String> {
    let named = expand(what);
    if manifest_in(&named).is_some() {
        let id = id_at(&named).ok_or_else(|| format!("{what}: cannot tell what this is called"))?;
        return Ok(Package {
            name: id.clone(),
            about: named.display().to_string(),
            origin: Origin::Path(named),
            missing: Vec::new(),
            here: false,
            installed: None,
            offered: None,
            id,
        });
    }
    if named.exists() {
        return Err(format!("{what} has no {MANIFEST} in it"));
    }

    let id = what.trim().to_lowercase();
    if let Some(found) = available(from).into_iter().find(|p| p.id == id) {
        return Ok(found);
    }
    // Already here and already working. Worth saying so rather than saying
    // there is no such thing.
    if let Some(plugin) = crate::plugin::find(&id) {
        return Ok(Package {
            about: plugin.detail(),
            origin: origin_of(plugin),
            missing: Vec::new(),
            here: true,
            installed: installed_version(&plugin.id),
            offered: None,
            id: plugin.id.clone(),
            name: plugin.name.clone(),
        });
    }
    Err(format!("there is no package called {what}"))
}

// ---------------------------------------------------------------------------
// Doing it
// ---------------------------------------------------------------------------

/// What has to happen to the files, as against to the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Files {
    /// Nothing. A plugin that is already here, or ships in the binary.
    Leave,
    /// Copy a package in, and leave a receipt saying we did.
    Copy { from: PathBuf, to: PathBuf },
    /// Fetch a package from a repository, check it against what the index
    /// said, and unpack it where a copy would have gone.
    Fetch { remote: Box<Remote>, to: PathBuf },
    /// Take away what we put here.
    Remove(PathBuf),
}

/// Everything an install or an uninstall is going to do, worked out before any
/// of it happens.
///
/// Separate from carrying it out so that the same plan can be run on a thread
/// with the answers arriving as events, or run straight through by a
/// `--install` on the command line with the answers printed. It is also the
/// thing to show somebody before asking whether they meant it.
#[derive(Clone, Debug)]
pub struct Plan {
    pub id: String,
    pub name: String,
    /// Whether this is putting something here or taking it away, for the
    /// words used about it.
    pub removing: bool,
    pub files: Files,
    pub steps: Vec<Step>,
    /// What has to be on the `PATH` when this has finished. Empty when
    /// removing: nothing is being checked for then.
    pub needs: Vec<String>,
    /// Where to go when none of the steps could manage it.
    pub see: Option<String>,
    /// Where the manifest that says what to run will be, for a package that
    /// is being fetched.
    ///
    /// A package still in a repository has no steps to show, because its
    /// steps are in its manifest and its manifest is inside the tarball. So a
    /// fetch is planned on its own, and what it turns out to want is read off
    /// the disk once it is on the disk. Nothing else works: an index that
    /// listed the steps would be a second copy of every manifest, going stale
    /// on its own schedule.
    pub steps_from: Option<PathBuf>,
}

impl Plan {
    /// Whether there is anything in it at all. An install of a plugin that is
    /// already here and already working is worth saying so about rather than
    /// pretending to do.
    pub fn is_empty(&self) -> bool {
        self.files == Files::Leave && self.steps.is_empty() && self.steps_from.is_none()
    }

    /// The lines to show somebody who wants to know what this will do.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.files {
            Files::Leave => {}
            Files::Copy { from, to } => {
                out.push(format!("copy {} to {}", from.display(), to.display()))
            }
            Files::Fetch { remote, to } => out.push(format!(
                "fetch {} {} from {} into {}",
                remote.entry.id,
                remote.entry.version,
                remote.repository.name,
                to.display()
            )),
            Files::Remove(dir) => out.push(format!("remove {}", dir.display())),
        }
        if self.steps_from.is_some() {
            // Honest about what is not yet known. The alternative is a list
            // that looks complete and then runs three programs nobody was
            // shown, which is the one thing showing the list is for.
            out.push("then whatever its manifest says it needs, once it is here".into());
        }
        for step in &self.steps {
            // Filled in, because the point of showing somebody what is about
            // to run is that it is what will actually run — a line with
            // `${bin}` still in it is a line they cannot check.
            let line = fill(&step.line());
            out.push(match step.system {
                // Named rather than discovered afterwards: this one is not
                // going into textfold's own directory.
                true => format!("{line} (installs system-wide)"),
                false => line,
            });
        }
        out
    }

    /// Whether any of it reaches outside textfold's own directory.
    pub fn touches_system(&self) -> bool {
        self.steps.iter().any(|s| s.system)
    }
}

/// What an install is doing, on its way back to whoever asked for it.
#[derive(Debug)]
pub enum Note {
    /// About to do something: which of how many, and what it is.
    Doing { at: usize, of: usize, about: String },
    /// It has been done, and this is what it printed. `ok` false is a step
    /// that ran and was unhappy, which stops everything after it.
    Did {
        about: String,
        ok: bool,
        output: String,
    },
    /// Nothing to do, and why not — the program it fetches is already here,
    /// or the program that fetches it is not.
    Skipped { about: String, why: String },
    /// The last word.
    Done { ok: bool, why: String },
}

/// One thing an install said, and which install said it.
#[derive(Debug)]
pub struct Progress {
    pub id: String,
    pub note: Note,
}

/// Work out what installing this would do.
pub fn install(package: &Package) -> Result<Plan, String> {
    // A package from a path has not been read as a plugin yet, so its steps
    // are read out of its manifest here. One already here has been read, and
    // asking the registry gets the version with `${plugin}` filled in.
    //
    // One that is still in a repository has not been downloaded, so there is
    // nothing to read at all: its steps are whatever its manifest turns out to
    // say once it is here, which is why the fetch runs before them.
    let read;
    let plugin: Option<&Plugin> = match &package.origin {
        Origin::Path(path) => {
            read = from_manifest(path)?;
            Some(&read)
        }
        Origin::Remote(_) => None,
        _ => Some(
            crate::plugin::find(&package.id)
                .ok_or_else(|| format!("{} is not here to install", package.id))?,
        ),
    };
    let (steps, needs, see, name) = match plugin {
        Some(plugin) => (
            plugin.install.clone(),
            plugin.needs.clone(),
            plugin.see.clone(),
            plugin.name.clone(),
        ),
        // What the index said, which is enough to say what will be checked
        // for afterwards and where to go if it is not there.
        None => (
            Vec::new(),
            package.missing.clone(),
            None,
            package.name.clone(),
        ),
    };

    let mut fetched_to: Option<PathBuf> = None;
    let files = match &package.origin {
        Origin::Path(from) => Files::Copy {
            from: from.clone(),
            to: install_to(&package.id)?,
        },
        Origin::Remote(remote) => {
            fetched_to = Some(install_to(&package.id)?);
            Files::Fetch {
                remote: remote.clone(),
                to: fetched_to.clone().expect("just set"),
            }
        }
        _ => Files::Leave,
    };

    Ok(Plan {
        id: package.id.clone(),
        removing: false,
        name,
        files,
        // A step for another operating system is not a step that gets skipped
        // at the last moment — it is not part of the plan at all, so what
        // textfold says it is about to do is what it is about to do.
        steps: steps.into_iter().filter(Step::here).collect(),
        steps_from: fetched_to,
        needs,
        see,
    })
}

/// Work out what removing this would do.
pub fn uninstall(id: &str) -> Result<Plan, String> {
    let plugin = crate::plugin::find(id).ok_or_else(|| format!("there is no plugin called {id}"))?;
    let files = match removable(id) {
        Some(dir) => Files::Remove(dir),
        None => Files::Leave,
    };
    if files == Files::Leave && plugin.uninstall.is_empty() {
        return Err(match plugin.source {
            crate::plugin::Source::BuiltIn => format!(
                "{id} is built in and says nothing about undoing an install — switch it off instead"
            ),
            crate::plugin::Source::File(_) => {
                format!("{id} was not installed by textfold, so it is not textfold's to remove")
            }
        });
    }
    Ok(Plan {
        id: plugin.id.clone(),
        name: plugin.name.clone(),
        removing: true,
        steps: plugin.uninstall.iter().filter(|s| s.here()).cloned().collect(),
        steps_from: None,
        needs: Vec::new(),
        see: None,
        files,
    })
}

/// Where a package's files go, refusing to write over a directory somebody
/// made by hand.
fn install_to(id: &str) -> Result<PathBuf, String> {
    let to = plugins_dir()
        .ok_or("there is nowhere to install plugins on this machine")?
        .join(id);
    // Installing over a directory somebody wrote by hand would throw away
    // work nobody asked us to touch.
    if is_there(&to) && removable(id).is_none() {
        return Err(format!(
            "{} is already there and textfold did not put it there",
            to.display()
        ));
    }
    Ok(to)
}

/// Read the steps out of a manifest that has not been loaded as a plugin.
fn from_manifest(path: &Path) -> Result<Plugin, String> {
    let manifest =
        manifest_in(path).ok_or_else(|| format!("{} has no {MANIFEST}", path.display()))?;
    let id = id_at(path).unwrap_or_default();
    // Read against where it is *going*, not where it is, so that a `${plugin}`
    // in an install step names the installed copy.
    let installed = plugins_dir()
        .map(|d| d.join(&id).join(MANIFEST))
        .unwrap_or_else(|| manifest.clone());
    let (plugin, problems) = crate::plugin::read(&manifest, &id, installed)?;
    match problems.first() {
        Some(first) => Err(first.clone()),
        None => Ok(plugin),
    }
}

impl Plan {
    /// Carry it out, saying what is happening as it goes.
    ///
    /// Returns whether it worked. Everything that talks to the world outside
    /// happens here, and nothing here talks to the editor — which is what
    /// makes it the same code on a thread and on the command line.
    pub fn run(&self, say: &mut dyn FnMut(Note)) -> bool {
        // The files first when installing, because a step may want to run
        // something the package brought with it; last when removing, because
        // a step may want the same.
        if !self.removing && let Err(why) = self.do_files(say) {
            say(Note::Done { ok: false, why });
            return false;
        }

        // A package that was just fetched brought its own instructions with
        // it, and they are the ones to follow — the index says what a plugin
        // is, not what it does to your machine.
        let fetched = match &self.steps_from {
            Some(dir) => match from_manifest(dir) {
                Ok(plugin) => Some(plugin),
                Err(why) => {
                    say(Note::Done { ok: false, why });
                    return false;
                }
            },
            None => None,
        };
        let (steps, needs, see) = match &fetched {
            Some(plugin) => (
                plugin
                    .install
                    .iter()
                    .filter(|s| s.here())
                    .cloned()
                    .collect::<Vec<Step>>(),
                plugin.needs.clone(),
                plugin.see.clone(),
            ),
            None => (self.steps.clone(), self.needs.clone(), self.see.clone()),
        };

        let of = steps.len();
        for (at, step) in steps.iter().enumerate() {
            if let Some(already) = &step.unless
                && on_path(already)
            {
                say(Note::Skipped {
                    about: step.about.clone(),
                    why: format!("{already} is already here"),
                });
                continue;
            }
            // Something an earlier step was supposed to have produced is not
            // there, so this one has nothing to work on. The usual reason is
            // that the download before it was skipped for want of `curl`, and
            // the right answer is to fall through to the next way of getting
            // it rather than to fail on a missing file.
            if let Some(needed) = &step.when {
                let needed = fill(needed);
                if !Path::new(&needed).exists() {
                    say(Note::Skipped {
                        about: step.about.clone(),
                        why: format!("there is no {needed}"),
                    });
                    continue;
                }
            }
            // A step whose own program is missing is a way of getting
            // something that this machine does not have, not a failure. This
            // is what lets a plugin list uv, pipx and pip and have the first
            // one that exists be the one that runs.
            let program = fill(&step.run[0]);
            if !on_path(&program) {
                say(Note::Skipped {
                    about: step.about.clone(),
                    why: format!("no {program}"),
                });
                continue;
            }
            say(Note::Doing {
                at: at + 1,
                of,
                about: fill(&step.about),
            });
            let (ok, output) = run_step(step);
            say(Note::Did {
                about: fill(&step.line()),
                ok,
                output,
            });
            if !ok {
                say(Note::Done {
                    ok: false,
                    why: format!("{} failed", step.line()),
                });
                return false;
            }
        }

        if self.removing && let Err(why) = self.do_files(say) {
            say(Note::Done { ok: false, why });
            return false;
        }

        // And the only question that actually matters: is the thing here now?
        // Exit codes are what a step claims; this is what is true.
        let missing: Vec<&str> = needs
            .iter()
            .map(String::as_str)
            .filter(|c| !on_path(c))
            .collect();
        if !missing.is_empty() {
            let mut why = format!("still no {}", missing.join(", "));
            if let Some(see) = &see {
                why.push_str(&format!(" — see {see}"));
            }
            say(Note::Done { ok: false, why });
            return false;
        }
        say(Note::Done {
            ok: true,
            why: match self.removing {
                true => format!("{} removed", self.name),
                false => format!("{} installed", self.name),
            },
        });
        true
    }

    fn do_files(&self, say: &mut dyn FnMut(Note)) -> Result<(), String> {
        match &self.files {
            Files::Leave => Ok(()),
            Files::Copy { from, to } => {
                say(Note::Doing {
                    at: 0,
                    of: self.steps.len(),
                    about: format!("copying it to {}", to.display()),
                });
                // A reinstall replaces rather than merges: files left over
                // from a version that had more in it are worse than a clean
                // copy of the version you asked for.
                remove_whatever_is_at(to)?;
                copy_in(from, to)?;
                write_receipt(
                    to,
                    Receipt {
                        from: from.display().to_string(),
                        version: id_version_at(from),
                        repository: None,
                    },
                )
            }
            Files::Fetch { remote, to } => {
                say(Note::Doing {
                    at: 0,
                    of: self.steps.len(),
                    about: format!(
                        "fetching {} {} from {}",
                        remote.entry.id, remote.entry.version, remote.repository.name
                    ),
                });
                fetch_in(remote, to)?;
                write_receipt(
                    to,
                    Receipt {
                        from: remote.entry.url.clone(),
                        version: Some(remote.entry.version.clone()),
                        repository: Some(remote.repository.name.clone()),
                    },
                )
            }
            Files::Remove(dir) => {
                say(Note::Doing {
                    at: self.steps.len(),
                    of: self.steps.len(),
                    about: format!("removing {}", dir.display()),
                });
                // A link is removed as a link. What it points at is somebody's
                // working copy and is none of our business.
                remove_whatever_is_at(dir)
            }
        }
    }

    /// The same, on a thread, with what it says arriving as events.
    ///
    /// Nothing waits. `npm install -g` is thirty seconds on a good day and
    /// that is not a length of time the cursor should stop for.
    pub fn spawn(self, tx: Sender<crate::app::Event>) -> Result<(), String> {
        let name = self.id.clone();
        std::thread::Builder::new()
            .name(format!("install-{name}"))
            .spawn(move || {
                let id = self.id.clone();
                self.run(&mut |note| {
                    tx.send(crate::app::Event::Package(Box::new(Progress {
                        id: id.clone(),
                        note,
                    })))
                    .ok();
                });
            })
            .map_err(|_| format!("could not start installing {name}"))?;
        Ok(())
    }
}

/// Run one step and gather up everything it had to say.
///
/// With the environment that keeps what it fetches inside textfold's own
/// directory — see [`install_env`] — and with `${bin}` and `${tools}` in its
/// arguments filled in, for a step that fetches something itself rather than
/// asking a package manager to.
fn run_step(step: &Step) -> (bool, String) {
    // The program as well as its arguments, so that a step can run something
    // out of textfold's own directory — the `pip` inside a virtual environment
    // an earlier step made, say.
    let program = fill(&step.run[0]);
    let args: Vec<String> = step.run[1..].iter().map(|a| fill(a)).collect();
    // The programs that install here expect somewhere to install to, and the
    // first one to run would otherwise fail on a machine where nothing has
    // been installed yet.
    if let Some(bin) = bin_dir()
        && let Err(e) = std::fs::create_dir_all(&bin)
    {
        return (false, format!("{}: {e}", bin.display()));
    }
    let done = std::process::Command::new(&program)
        .args(&args)
        .envs(install_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match done {
        Ok(out) => {
            let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
            said.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), said)
        }
        Err(e) => (false, format!("{program}: {e}")),
    }
}

/// Leave the note saying textfold put this here, which is the whole of
/// uninstall's safety and now of update's too.
fn write_receipt(to: &Path, receipt: Receipt) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?;
    std::fs::write(to.join(RECEIPT), text).map_err(|e| format!("{}: {e}", to.display()))
}

/// What version a package at a path says it is, for the receipt.
fn id_version_at(path: &Path) -> Option<String> {
    let manifest = manifest_in(path)?;
    let text = std::fs::read_to_string(manifest).ok()?;
    serde_json::from_str::<Named>(&text).ok()?.version
}

/// Fetch a package from a repository and put it where it goes.
///
/// Into a directory of its own first, and moved into place only once the
/// download has been checked and unpacked. A tarball that arrived truncated,
/// or that does not match the digest the index gave for it, must never have
/// been anywhere near the plugins directory: what is unpacked from it is a
/// manifest whose install steps run programs.
fn fetch_in(remote: &Remote, to: &Path) -> Result<(), String> {
    let holding = to.with_extension("fetching");
    std::fs::remove_dir_all(&holding).ok();
    std::fs::create_dir_all(&holding).map_err(|e| format!("{}: {e}", holding.display()))?;

    let tidy = |why: String| {
        std::fs::remove_dir_all(&holding).ok();
        why
    };
    let tarball = holding.join("package.tar.gz");
    crate::repo::fetch(&remote.repository, &remote.entry, &tarball).map_err(tidy)?;

    let unpacked = holding.join("unpacked");
    std::fs::create_dir_all(&unpacked)
        .map_err(|e| tidy(format!("{}: {e}", unpacked.display())))?;
    let done = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&unpacked)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match done {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(tidy(format!("could not unpack {}: {said}", remote.entry.id)));
        }
        Err(e) => return Err(tidy(format!("tar: {e}"))),
    }

    // A tarball made of the plugin's directory has the manifest at the top; a
    // tarball made of the directory *itself* has it one level down. Both are
    // things people produce, and the difference is not worth an error message.
    let root = match unpacked.join(MANIFEST).is_file() {
        true => unpacked.clone(),
        false => one_directory_in(&unpacked)
            .filter(|dir| dir.join(MANIFEST).is_file())
            .ok_or_else(|| tidy(format!("{} has no {MANIFEST} in it", remote.entry.id)))?,
    };

    // The replacement, as late as possible: everything that could fail has
    // failed by now, so the window in which the plugin is neither the old one
    // nor the new one is two file system calls wide.
    remove_whatever_is_at(to).map_err(tidy)?;
    let moved = std::fs::rename(&root, to).is_ok();
    if !moved {
        // A rename across file systems does not work, and a cache and a
        // config directory are on different ones often enough to matter.
        copy_in(&root, to).map_err(tidy)?;
    }
    std::fs::remove_dir_all(&holding).ok();
    Ok(())
}

/// The single directory inside `dir`, if that is all there is in it.
fn one_directory_in(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    found.retain(|p| p.is_dir());
    (found.len() == 1).then(|| found.remove(0))
}

/// Copy a package in.
///
/// A single manifest becomes a directory with a `plugin.json` in it, so that
/// every installed plugin has somewhere to keep a receipt and there is one
/// shape to reason about rather than two.
fn copy_in(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    if from.is_file() {
        return std::fs::copy(from, to.join(MANIFEST))
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", from.display()));
    }
    copy_tree(from, to)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        // Somebody's history of the package is not part of the package, and a
        // repository copied into a plugins directory is a surprise nobody
        // needs. Everything else goes, including whatever a build left — a
        // plugin's `node_modules` is the plugin.
        if name == ".git" || name == "__pycache__" {
            continue;
        }
        let source = entry.path();
        let target = to.join(&name);
        match source.is_dir() {
            true => {
                std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
                copy_tree(&source, &target)?;
            }
            false => {
                std::fs::copy(&source, &target)
                    .map_err(|e| format!("{}: {e}", source.display()))?;
            }
        }
    }
    Ok(())
}

/// Which plugins have files textfold put here, and where they came from. For
/// `--list-packages`, so that "installed" is something you can look at.
pub fn receipts() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(dir) = plugins_dir() else {
        return out;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(path.join(RECEIPT))
            && let Ok(receipt) = serde_json::from_str::<Receipt>(&text)
        {
            out.insert(name, receipt.from);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("textfold-pack-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("a place to work");
        dir
    }

    fn step(run: &[&str], unless: Option<&str>) -> Step {
        Step {
            about: run.join(" "),
            run: run.iter().map(|s| s.to_string()).collect(),
            unless: unless.map(str::to_string),
            when: None,
            os: Vec::new(),
            arch: Vec::new(),
            system: false,
        }
    }

    fn plan(steps: Vec<Step>, needs: &[&str]) -> Plan {
        Plan {
            id: "p".into(),
            name: "P".into(),
            removing: false,
            files: Files::Leave,
            steps,
            steps_from: None,
            needs: needs.iter().map(|s| s.to_string()).collect(),
            see: None,
        }
    }

    fn notes(plan: &Plan) -> (bool, Vec<String>) {
        let mut said = Vec::new();
        let ok = plan.run(&mut |note| {
            said.push(match note {
                Note::Doing { about, .. } => format!("doing {about}"),
                Note::Did { ok, .. } => format!("did {ok}"),
                Note::Skipped { why, .. } => format!("skipped: {why}"),
                Note::Done { ok, why } => format!("done {ok}: {why}"),
            })
        });
        (ok, said)
    }

    #[test]
    fn what_an_install_runs_with_points_every_package_manager_at_our_own_corner() {
        // The answer to "is this going to put things on my system". Every one
        // of these is that package manager's own documented variable, which is
        // why a manifest can go on saying `npm install --global` — the obvious
        // thing to write — and have it mean global to textfold.
        let env = install_env();
        let Some(tools) = tools_dir() else { return };
        let bin = tools.join("bin").display().to_string();
        let tools = tools.display().to_string();
        let get = |name: &str| {
            env.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{name} was not set"))
        };
        assert_eq!(get("npm_config_prefix"), tools);
        assert_eq!(get("PYTHONUSERBASE"), tools);
        assert_eq!(get("CARGO_INSTALL_ROOT"), tools);
        assert_eq!(get("PIPX_BIN_DIR"), bin);
        assert_eq!(get("UV_TOOL_BIN_DIR"), bin);
        assert_eq!(get("GOBIN"), bin);
        // And nothing points anywhere near the places a system install goes.
        for (name, value) in &env {
            assert!(
                value.starts_with(&tools),
                "{name} is set to {value}, which is outside textfold's own directory"
            );
        }
    }

    #[test]
    fn our_own_programs_go_on_the_path_last() {
        // What you installed yourself goes on winning. An editor that shadowed
        // the ruff in your virtual environment with a copy of its own would be
        // a very difficult afternoon.
        let Some(bin) = bin_dir() else { return };
        put_tools_on_path();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs.last(), Some(&bin));
        assert_eq!(
            dirs.iter().filter(|d| *d == &bin).count(),
            1,
            "twice on the PATH is once too many"
        );
        // And saying it again changes nothing, which is what makes it safe to
        // call from more than one entry point.
        put_tools_on_path();
        let path = std::env::var_os("PATH").unwrap_or_default();
        assert_eq!(std::env::split_paths(&path).count(), dirs.len());
    }

    #[test]
    fn a_step_for_another_machine_is_not_part_of_the_plan_at_all() {
        // Dropped when the plan is made rather than skipped at the last
        // moment, so that what textfold says it is about to do is what it is
        // about to do.
        let here = std::env::consts::OS;
        let mine = Step {
            os: vec![here.to_string()],
            ..step(&["true"], None)
        };
        let theirs = Step {
            os: vec!["plan9".into()],
            ..step(&["true"], None)
        };
        assert!(mine.here());
        assert!(!theirs.here());

        let wrong_chip = Step {
            arch: vec!["s390x".into()],
            ..step(&["true"], None)
        };
        assert!(!wrong_chip.here(), "the processor has to match too");
    }

    #[test]
    fn a_downloaded_program_is_not_finished_until_it_can_be_run() {
        // A file with no executable bit is not a program, and a check that
        // said otherwise would skip the `chmod` step that follows the download
        // and then report the install a success.
        let dir = scratch("runnable");
        let path = dir.join("thing");
        std::fs::write(&path, "#!/bin/sh\ntrue\n").expect("written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(!runnable(&path), "it has no executable bit yet");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        assert!(runnable(&path));
        assert!(!runnable(&dir.join("not-there")));
        assert!(!runnable(&dir), "a directory is not a program");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_step_can_run_a_program_an_earlier_step_put_there() {
        // The `pip` inside a virtual environment a previous step made. Without
        // filling the program as well as its arguments, a plugin could fetch
        // something into textfold's own directory and then have no way to run
        // it — which is the whole of how a Python tool gets its own
        // environment rather than being dropped into yours.
        let dir = scratch("own-program");
        let bin = dir.join("prog");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").expect("written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        // `fill` only knows `${tools}` and `${bin}`, so this is checked by
        // running an absolute path through the same door a filled one uses.
        let step = Step {
            about: "run it".into(),
            run: vec![bin.display().to_string()],
            unless: None,
            when: None,
            os: Vec::new(),
            arch: Vec::new(),
            system: false,
        };
        let (ok, _) = run_step(&step);
        assert!(ok, "a program named by its full path should run");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_step_waits_for_what_an_earlier_step_was_meant_to_make() {
        // Three steps that are one operation: make an environment, install
        // into it, put the result on the path. If the first cannot run, the
        // other two have nothing to work on and must stand aside rather than
        // fail — that is what lets the next way of getting it be tried.
        let dir = scratch("waiting");
        let missing = dir.join("not-made-yet").display().to_string();
        let plan = Plan {
            id: "p".into(),
            name: "P".into(),
            removing: false,
            files: Files::Leave,
            steps: vec![
                Step {
                    when: Some(missing.clone()),
                    ..step(&["true"], None)
                },
                step(&["true"], None),
            ],
            steps_from: None,
            needs: Vec::new(),
            see: None,
        };
        let (ok, said) = notes(&plan);
        assert!(ok, "{said:?}");
        assert_eq!(said[0], format!("skipped: there is no {missing}"));
        assert_eq!(said[1], "doing true", "and the one after it still ran");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_step_can_name_the_directory_it_is_installing_into() {
        // For a program published as a build per platform rather than through
        // a package manager. Without this a manifest could not say where to
        // put what it downloads, and everything would have to go system-wide.
        let Some(tools) = tools_dir() else { return };
        let filled = fill("curl -o ${bin}/marksman && ls ${tools}");
        assert!(filled.contains(&tools.join("bin").display().to_string()));
        assert!(filled.contains(&tools.display().to_string()));
        assert!(!filled.contains("${"), "{filled}");
    }

    #[test]
    fn a_program_is_found_the_way_a_shell_would_find_it() {
        assert!(on_path("sh"), "there is always a shell");
        assert!(on_path("/bin/sh"), "a path is checked as a path");
        assert!(!on_path("a-program-nobody-has-written"));
        assert!(!on_path(""));
    }

    #[test]
    fn a_path_asks_whether_the_file_is_there_and_a_bare_name_whether_it_can_be_run() {
        // The two questions are different and a plugin asks both. `needs:
        // ["ruff"]` means a program you could run; `needs:
        // ["${plugin}/node_modules/…/language-server.js"]` means a file that
        // has to have been fetched, which nothing will ever run directly —
        // node runs it. Demanding an executable bit of the second would say a
        // perfectly good install had failed.
        let dir = scratch("named");
        let plain = dir.join("data.js");
        std::fs::write(&plain, "// not executable\n").expect("written");
        assert!(
            on_path(plain.to_str().unwrap()),
            "a file named by its path is there, whatever its mode"
        );
        assert!(!runnable(&plain), "and it is still not a program");
        assert!(!on_path(&dir.join("absent.js").display().to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_step_whose_work_is_already_done_is_not_done_again() {
        let (ok, said) = notes(&plan(vec![step(&["false"], Some("sh"))], &[]));
        assert!(ok, "{said:?}");
        assert_eq!(said[0], "skipped: sh is already here");
    }

    #[test]
    fn a_step_you_cannot_run_is_not_a_step_that_failed() {
        // The rule the whole three-ways-to-get-ruff arrangement rests on: no
        // `uv` on this machine means try the next one, not give up.
        let plan = plan(
            vec![
                step(&["a-package-manager-nobody-has", "install", "x"], None),
                step(&["true"], None),
            ],
            &[],
        );
        let (ok, said) = notes(&plan);
        assert!(ok, "{said:?}");
        assert_eq!(said[0], "skipped: no a-package-manager-nobody-has");
        assert_eq!(said[1], "doing true");
    }

    #[test]
    fn a_step_that_runs_and_fails_stops_the_rest() {
        let plan = plan(vec![step(&["false"], None), step(&["true"], None)], &[]);
        let (ok, said) = notes(&plan);
        assert!(!ok);
        assert!(said.iter().any(|s| s == "did false"), "{said:?}");
        assert!(
            !said.iter().any(|s| s == "doing true"),
            "the step after a failure ran: {said:?}"
        );
    }

    #[test]
    fn an_install_that_did_not_install_anything_has_not_worked() {
        // Every step was cheerful and the program is still not there, which is
        // the failure that exit codes do not catch.
        let (ok, said) = notes(&plan(vec![step(&["true"], None)], &["still-not-here"]));
        assert!(!ok);
        assert_eq!(said.last().unwrap(), "done false: still no still-not-here");
    }

    #[test]
    fn what_it_needs_being_there_already_is_a_finished_install() {
        let (ok, _) = notes(&plan(vec![], &["sh"]));
        assert!(ok);
    }

    #[test]
    fn a_package_is_copied_in_and_leaves_a_receipt() {
        let dir = scratch("copy");
        let from = dir.join("mine");
        std::fs::create_dir_all(from.join("guts")).expect("made");
        std::fs::write(from.join(MANIFEST), r#"{"id":"mine","name":"Mine"}"#).expect("written");
        std::fs::write(from.join("guts").join("run.py"), "print(1)").expect("written");

        let to = dir.join("plugins").join("mine");
        let plan = Plan {
            id: "mine".into(),
            name: "Mine".into(),
            removing: false,
            files: Files::Copy {
                from: from.clone(),
                to: to.clone(),
            },
            steps: Vec::new(),
            steps_from: None,
            needs: Vec::new(),
            see: None,
        };
        assert!(notes(&plan).0);
        assert!(to.join(MANIFEST).is_file());
        assert!(to.join("guts").join("run.py").is_file(), "and what is beside it");
        assert!(to.join(RECEIPT).is_file(), "so that it can be removed again");

        // And removing it takes the lot.
        let away = Plan {
            removing: true,
            files: Files::Remove(to.clone()),
            ..plan
        };
        assert!(notes(&away).0);
        assert!(!to.exists());
        assert!(from.is_dir(), "and left where it was copied from alone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn installing_over_a_link_replaces_the_link_and_not_what_it_points_at() {
        // Linking a plugin in while you work on it is what the documentation
        // suggests, so installing the published copy over one is a path that
        // gets walked. `Path::exists` follows a link, so a link whose target
        // has gone reads as nothing being there — and then every write to it
        // fails with "file exists", which is the least helpful way to be told.
        let dir = scratch("over-a-link");
        let working = dir.join("my-copy");
        std::fs::create_dir_all(&working).expect("made");
        std::fs::write(working.join(MANIFEST), r#"{"id":"zls"}"#).expect("written");

        let installed = dir.join("plugins").join("zls");
        std::fs::create_dir_all(installed.parent().expect("a parent")).expect("made");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&working, &installed).expect("linked");
        #[cfg(not(unix))]
        return;

        // Nothing is there as far as `exists` is concerned once the target
        // goes, and something very much is there as far as the disk is.
        assert!(is_there(&installed));

        remove_whatever_is_at(&installed).expect("the link went");
        assert!(!is_there(&installed), "the link is gone");
        assert!(working.is_dir(), "and what it pointed at is untouched");
        assert!(
            working.join(MANIFEST).is_file(),
            "following the link to delete the target is the worst thing this could do"
        );

        // And a link that points nowhere at all, which is what a working copy
        // that has been moved away leaves behind.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("gone"), &installed).expect("linked");
            assert!(is_there(&installed), "a dangling link is still something");
            assert!(!installed.exists(), "though `exists` says otherwise");
            remove_whatever_is_at(&installed).expect("it went anyway");
            assert!(!is_there(&installed));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_single_manifest_becomes_a_directory_with_a_receipt_in_it() {
        let dir = scratch("one-file");
        let from = dir.join("zig.json");
        std::fs::write(&from, r#"{"id":"zig"}"#).expect("written");
        let to = dir.join("plugins").join("zig");
        copy_in(&from, &to).expect("copied");
        assert!(to.join(MANIFEST).is_file());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn what_a_package_names_inside_itself_is_not_reported_as_missing() {
        // A plugin whose `needs` is `${plugin}/node_modules/…` names a file
        // that exists once it is installed, and there is nothing to fill
        // `${plugin}` in with until it is. Reported as missing, every such
        // plugin sits in the list saying it needs a program with a `${` in
        // its name.
        let entry = crate::repo::Entry {
            id: "copilot".into(),
            name: None,
            about: None,
            version: "1.0.0".into(),
            url: "dist/copilot-1.0.0.tar.gz".into(),
            sha256: None,
            size: None,
            needs: vec![
                "python3".into(),
                "${plugin}/node_modules/x/language-server.js".into(),
                "a-program-nobody-wrote".into(),
            ],
            see: None,
        };
        let missing: Vec<&String> = entry
            .needs
            .iter()
            .filter(|command| !command.contains("${") && !on_path(command))
            .collect();
        assert_eq!(missing, ["a-program-nobody-wrote"]);
    }

    #[test]
    fn an_update_is_offered_only_where_both_sides_say_what_version_they_are() {
        let package = |installed: Option<&str>, offered: Option<&str>| Package {
            id: "zls".into(),
            name: "zls".into(),
            about: "Zig".into(),
            origin: Origin::Yours,
            missing: Vec::new(),
            here: true,
            installed: installed.map(str::to_string),
            offered: offered.map(str::to_string),
        };
        assert!(package(Some("1.0.0"), Some("1.1.0")).has_update());
        assert!(!package(Some("1.1.0"), Some("1.1.0")).has_update());
        assert!(!package(Some("1.2.0"), Some("1.1.0")).has_update());
        // A plugin that declines to number itself is one nothing is ever an
        // update to. Guessing would mean reinstalling somebody's plugin over
        // and over for a version it never claimed.
        assert!(!package(None, Some("1.1.0")).has_update());
        assert!(!package(Some("1.0.0"), None).has_update());

        // And what the row says, which is where anybody actually reads this.
        assert_eq!(package(Some("1.0.0"), Some("1.1.0")).tag(), "update");
        assert!(
            package(Some("1.0.0"), Some("1.1.0"))
                .detail()
                .contains("1.0.0 → 1.1.0")
        );
        assert_eq!(package(Some("1.1.0"), Some("1.1.0")).tag(), "ready");
    }

    #[test]
    fn a_fetch_says_that_more_will_follow_once_it_knows_what() {
        // A package still in a repository has no steps to show: they are in
        // its manifest, and its manifest is inside the tarball. A list that
        // looked complete and then ran three programs nobody was shown would
        // defeat the point of showing the list.
        let plan = Plan {
            id: "zls".into(),
            name: "zls".into(),
            removing: false,
            files: Files::Fetch {
                remote: Box::new(Remote {
                    repository: crate::repo::Repository {
                        name: "r".into(),
                        url: "https://example.invalid/p".into(),
                    },
                    entry: crate::repo::Entry {
                        id: "zls".into(),
                        name: None,
                        about: None,
                        version: "1.0.0".into(),
                        url: "dist/zls-1.0.0.tar.gz".into(),
                        sha256: None,
                        size: None,
                        needs: vec!["zls".into()],
                        see: None,
                    },
                }),
                to: PathBuf::from("/home/me/.config/textfold/plugins/zls"),
            },
            steps: Vec::new(),
            steps_from: Some(PathBuf::from("/home/me/.config/textfold/plugins/zls")),
            needs: vec!["zls".into()],
            see: None,
        };
        assert!(!plan.is_empty(), "a fetch is something to do");
        let lines = plan.lines();
        assert_eq!(
            lines[0],
            "fetch zls 1.0.0 from r into /home/me/.config/textfold/plugins/zls"
        );
        assert!(lines[1].contains("once it is here"), "{lines:?}");
    }

    #[test]
    fn a_package_says_what_it_is_called_and_falls_back_to_its_directory() {
        let dir = scratch("named");
        let one = dir.join("whatever");
        std::fs::create_dir_all(&one).expect("made");
        std::fs::write(one.join(MANIFEST), r#"{"id":"Cargo"}"#).expect("written");
        assert_eq!(id_at(&one).as_deref(), Some("cargo"));

        let two = dir.join("Silent");
        std::fs::create_dir_all(&two).expect("made");
        std::fs::write(two.join(MANIFEST), "{}").expect("written");
        assert_eq!(id_at(&two).as_deref(), Some("silent"));

        // And a directory with nothing in it is not a package.
        let three = dir.join("empty");
        std::fs::create_dir_all(&three).expect("made");
        assert_eq!(id_at(&three), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn what_a_plan_will_do_can_be_read_before_it_does_it() {
        let plan = Plan {
            id: "zls".into(),
            name: "zls".into(),
            removing: false,
            files: Files::Copy {
                from: PathBuf::from("/pkg/zls"),
                to: PathBuf::from("/home/me/.config/textfold/plugins/zls"),
            },
            steps: vec![step(&["brew", "install", "zls"], Some("zls"))],
            steps_from: None,
            needs: vec!["zls".into()],
            see: None,
        };
        assert_eq!(
            plan.lines(),
            [
                "copy /pkg/zls to /home/me/.config/textfold/plugins/zls",
                "brew install zls",
            ]
        );
        assert!(!plan.is_empty());
    }
}
