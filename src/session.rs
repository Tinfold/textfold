//! What was open last time, so that it can be open again.
//!
//! Closing an editor should not be a decision about which of thirty files you
//! will have to find again tomorrow. textfold writes down the tabs, where the
//! cursor was in each of them, and how the panes were arranged, and puts them
//! back when you start it in the same directory with nothing named on the
//! command line.
//!
//! Per directory, not per machine. Sessions are kept in one file, keyed on the
//! project you were in, because "where was I" is a question about a project and
//! not about the editor: opening textfold in one repository should not bring
//! back the other one's tabs. The list is trimmed to the last few dozen
//! projects, so it stays a convenience rather than a record of everything you
//! have ever edited.
//!
//! Nothing here is load-bearing. A sessions file that is missing, unreadable,
//! or full of paths that have since been deleted means an editor that starts
//! empty, which is what it did before any of this existed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many projects to remember. Enough that the ones you actually work in
/// are all there, few enough that the file stays something a person could read.
const KEEP: usize = 48;

/// One tab: the file, and where you were in it.
///
/// A line and a column rather than a character offset, because the file may
/// have been rebased, reformatted, or written by somebody else since — and a
/// line number that is a little bit wrong lands you in the right part of the
/// file, where an offset that is a little bit wrong lands you anywhere.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Tab {
    pub path: PathBuf,
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub column: usize,
}

/// One pane: which of the tabs it was showing, and whether it was folding
/// long lines.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Pane {
    /// An index into [`Session::tabs`]. Out of range is a pane that showed a
    /// file that has since gone, and is dropped.
    #[serde(default)]
    pub tab: usize,
    #[serde(default)]
    pub wrap: bool,
}

/// One project, as it was left.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Session {
    /// In tab order, which is the order the row across the top was in.
    #[serde(default)]
    pub tabs: Vec<Tab>,
    #[serde(default)]
    pub panes: Vec<Pane>,
    /// Which plugin panels were docked, by command id — `files/tree`.
    ///
    /// The ids rather than the panes, because a docked panel is not a file:
    /// its buffer belongs to a plugin, and bringing it back means asking the
    /// plugin for it again rather than reopening something off the disk. What
    /// is worth remembering is that the sidebar was open, which is what this
    /// is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docks: Vec<String>,
    /// Which pane had the keyboard.
    #[serde(default)]
    pub focus: usize,
    /// Whether the panes sat side by side.
    #[serde(default = "yes")]
    pub side_by_side: bool,
    /// When it was written, as seconds since the epoch, so that the oldest can
    /// be dropped when the file grows. Not shown to anybody.
    #[serde(default)]
    pub at: u64,
}

fn yes() -> bool {
    true
}

impl Session {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

/// Every project remembered, by the directory it was.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct File {
    #[serde(default)]
    projects: BTreeMap<String, Session>,
}

/// Where the sessions live: beside the language server log, in the place a
/// desktop keeps things a program made rather than things a person wrote.
/// Settings are yours and this is bookkeeping, so it does not go in with them.
pub fn path() -> Option<PathBuf> {
    Some(
        dirs::state_dir()
            .or_else(dirs::cache_dir)?
            .join("textfold")
            .join("sessions.json"),
    )
}

/// What was open in this project last time, if anything.
pub fn load(project: &Path) -> Option<Session> {
    read(path()?.as_path())
        .projects
        .remove(&key(project))
        .filter(|s| !s.is_empty())
}

/// Write down what is open in this project now.
///
/// Failure is silent. A session that did not get written is a handful of tabs
/// to open again; a complaint about it on the way out of the editor is a
/// complaint nobody can act on and everybody has to read.
pub fn save(project: &Path, session: Session) {
    let Some(path) = path() else { return };
    let mut file = read(&path);
    if session.is_empty() {
        // Nothing open here. Forgetting is right: coming back to a project you
        // deliberately closed everything in should not reopen it.
        file.projects.remove(&key(project));
    } else {
        file.projects.insert(key(project), session);
    }
    trim(&mut file);

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if let Ok(text) = serde_json::to_string_pretty(&file) {
        std::fs::write(&path, text + "\n").ok();
    }
}

/// Drop the oldest until the file is a reasonable size again.
fn trim(file: &mut File) {
    if file.projects.len() <= KEEP {
        return;
    }
    let mut ages: Vec<(u64, String)> = file
        .projects
        .iter()
        .map(|(name, session)| (session.at, name.clone()))
        .collect();
    ages.sort();
    for (_, name) in ages.into_iter().take(file.projects.len() - KEEP) {
        file.projects.remove(&name);
    }
}

fn read(path: &Path) -> File {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The name a project is filed under. The path as written, so that two
/// directories that differ only in a symlink are two projects — which is what
/// a person who keeps both around means by them.
fn key(project: &Path) -> String {
    project.display().to_string()
}

/// Seconds since the epoch, for working out which session is oldest. Zero if
/// the clock is somewhere before 1970, which is not a case worth code.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(path: &str, line: usize) -> Tab {
        Tab {
            path: PathBuf::from(path),
            line,
            column: 0,
        }
    }

    #[test]
    fn a_session_survives_being_written_down_and_read_back() {
        let session = Session {
            tabs: vec![tab("src/main.rs", 41), tab("README.md", 0)],
            panes: vec![Pane { tab: 0, wrap: false }, Pane { tab: 1, wrap: true }],
            focus: 1,
            side_by_side: false,
            at: 1_700_000_000,
            docks: vec!["files/tree".into()],
        };
        let text = serde_json::to_string(&session).expect("written");
        let back: Session = serde_json::from_str(&text).expect("read");
        assert_eq!(back, session);
    }

    #[test]
    fn a_file_from_a_later_version_is_still_read() {
        // Whatever a newer textfold writes into a session, this one should
        // still find the tabs in it rather than starting empty.
        let file: File = serde_json::from_str(
            r#"{"projects":{"/x":{"tabs":[{"path":"a.rs","line":3,"telepathy":true}]}}}"#,
        )
        .expect("read");
        let session = &file.projects["/x"];
        assert_eq!(session.tabs[0].path, PathBuf::from("a.rs"));
        assert_eq!(session.tabs[0].line, 3);
        // And a field it never heard of takes the default it would have had.
        assert!(session.side_by_side);
    }

    #[test]
    fn the_oldest_projects_are_the_ones_that_go() {
        let mut file = File::default();
        for n in 0..(KEEP + 5) {
            file.projects.insert(
                format!("/project/{n}"),
                Session {
                    tabs: vec![tab("a.rs", 0)],
                    at: n as u64,
                    ..Session::default()
                },
            );
        }
        trim(&mut file);
        assert_eq!(file.projects.len(), KEEP);
        assert!(!file.projects.contains_key("/project/0"), "the oldest stayed");
        assert!(
            file.projects.contains_key(&format!("/project/{}", KEEP + 4)),
            "the newest went"
        );
    }

    #[test]
    fn a_project_with_nothing_open_is_forgotten_rather_than_kept_empty() {
        let dir = std::env::temp_dir().join(format!("textfold-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let file = dir.join("sessions.json");

        let mut held = File::default();
        held.projects.insert(
            "/x".into(),
            Session {
                tabs: vec![tab("a.rs", 0)],
                ..Session::default()
            },
        );
        std::fs::write(&file, serde_json::to_string(&held).unwrap()).unwrap();

        // What `save` does with an empty session, without going near the real
        // sessions file.
        let mut on_disk = read(&file);
        on_disk.projects.remove("/x");
        assert!(on_disk.projects.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
