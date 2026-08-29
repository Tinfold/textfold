//! Settings that outlive a session.
//!
//! One JSON file, in the ordinary place, with nothing required in it. An
//! absent file, an unreadable one, or one written by a future version with
//! fields we do not know about all mean the same thing: fall back to what
//! textfold would have done anyway.
//!
//! Settings changed from inside the editor are written back here, and only
//! the ones you changed — the file says what you decided rather than
//! repeating forty things you did not.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme;

/// How wide a tab is drawn, when nothing says otherwise.
pub const DEFAULT_TAB_WIDTH: usize = 4;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Which set of colours to draw in, by name — one of the files in
    /// `themes/`, or one of your own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// Whether to paint the background a theme names, or leave the terminal's
    /// own showing through. Absent means paint it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,

    /// How many columns a tab character occupies, and how far one press of
    /// Tab moves you. Absent means four.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_width: Option<usize>,

    /// Whether pressing Tab puts in spaces. Absent means it does — a file
    /// that already uses tabs is detected when it is opened, and wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spaces: Option<bool>,

    /// `"absolute"`, `"relative"`, `"both"`, or `"off"`. Absent means
    /// absolute: the numbers a person means when they say line 40.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_numbers: Option<String>,

    /// Whether long lines fold onto the next row instead of running off the
    /// side. Absent means they do not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<bool>,

    /// How many lines to keep between the cursor and the edge when scrolling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrolloff: Option<usize>,

    /// Whether to run the language server's formatter when you save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_on_save: Option<bool>,

    /// Which of a language server's own fixes to apply when you save:
    /// `["source.fixAll", "source.organizeImports"]`.
    ///
    /// Absent means none, which is the safe default for a setting that lets
    /// something else rewrite your file. This is the half of "tidy this up"
    /// that formatting is not — a formatter lays code out, and it is
    /// `source.fixAll` that takes the unused import away. Every server
    /// attached to the file is asked, so on a Python file this is what gets
    /// ruff's fixes in as well as pyright's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_actions_on_save: Option<Vec<String>>,

    /// Whether to drop trailing spaces from lines when you save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_trailing_whitespace: Option<bool>,

    /// Whether a file that does not end in a newline gets one when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_newline: Option<bool>,

    /// Whether completions appear as you type, rather than only when asked
    /// for. Absent means they do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_completion: Option<bool>,

    /// Whether typing an opening bracket or quote puts in the closing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pairs: Option<bool>,

    /// Whether a file changed on disk by something else is read again on its
    /// own. Absent means it is, but only where the buffer has no unsaved
    /// changes of its own — a conflict is always yours to settle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_on_change: Option<bool>,

    /// Whether the files open when you leave are opened again when you come
    /// back to the same directory. Absent means they are — but only where
    /// textfold was started with nothing named on the command line, since
    /// naming a file is saying what you want open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_session: Option<bool>,

    /// Whether the mouse is captured at all. Off hands clicks and drags back
    /// to the terminal, which is what you want if you select text with it to
    /// copy into something else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,

    /// Faint vertical lines at these columns, for people who care where 80 is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rulers: Option<Vec<usize>>,

    /// Whether whitespace is drawn: middle dots for spaces, arrows for tabs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_whitespace: Option<bool>,

    /// Whether to paint the underline under a problem in the colour of how
    /// bad it is: `"auto"`, `"on"`, or `"off"`.
    ///
    /// Absent means auto, which asks it only of the terminals known to
    /// understand the sequence. This is not fussiness about a colour. A
    /// terminal that has never heard of it reads the colour as four more
    /// instructions and turns your file dim, italic, and in places invisible
    /// — see [`crate::term::understands_underline_colour`]. Say `"on"` if
    /// your terminal does have it and textfold has not worked that out.
    #[serde(default, alias = "underline_color", skip_serializing_if = "Option::is_none")]
    pub underline_colour: Option<String>,

    /// Whether to try the terminal's extended keyboard protocol, which is
    /// what makes Ctrl-Shift-something and a released key distinguishable.
    /// Absent means try it; terminals that do not have it are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhanced_keys: Option<bool>,

    /// Which Python environment a project uses, by project root.
    ///
    /// Only written where you have chosen one by hand. A project with a single
    /// `.venv` beside it needs nothing here — that one is found — and this is
    /// for the project that has three, where only you know which.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub python_environments: BTreeMap<String, String>,

    /// Which plugins and language servers are off, by id: `"python/ruff":
    /// false`. Anything not named here is on.
    ///
    /// Written from the `plugins` list rather than by hand, usually. Turning
    /// off a plugin turns off the servers inside it, so `"python": false` is
    /// enough to say "leave Python alone entirely".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, bool>,

    /// Where else to look for plugins you could install, besides
    /// `~/.config/textfold/packages`.
    ///
    /// This is what makes `install-plugin` a list rather than a path you have
    /// to remember. Point it at a checkout of somebody's plugins and every
    /// directory in it with a `plugin.json` becomes a row you can choose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_paths: Vec<String>,

    /// Which Java to run a Java language server with, and to offer a Java
    /// project as a runtime to build against.
    ///
    /// Absent means look: `JAVA_HOME`, then the places JDKs are installed on
    /// this kind of machine, then the `java` on the `PATH` — see
    /// [`crate::jdk`]. This is here for the machine where that finds the
    /// wrong one, which is any machine with several JDKs on it and an opinion
    /// about which of them is the one.
    ///
    /// It is the top of a JDK, the directory with `bin/java` in it, and not
    /// the program: `"/usr/lib/jvm/java-21-openjdk"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_home: Option<String>,

    /// Where to fetch plugins from.
    ///
    /// Absent means the one textfold ships with — see
    /// [`crate::repo::DEFAULT_URL`]. Naming any repository *replaces* that
    /// rather than adding to it, so that what you get is what you said; keep
    /// it by writing it out alongside your own.
    ///
    /// ```json
    /// "package_repositories": [
    ///   { "name": "mine", "url": "https://example.invalid/plugins" }
    /// ]
    /// ```
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_repositories: Vec<crate::repo::Repository>,

    /// Whether to ask the package repositories what they have when textfold
    /// starts. Absent means it does — on a thread, so nothing waits for it,
    /// and nothing is installed by it: what a refresh changes is whether the
    /// plugins list has an `update` beside anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_for_updates: Option<bool>,

    /// Keys of your own, by what they do: `"save": ["ctrl-s", "f2"]`.
    ///
    /// Only what you have changed is written here; everything else keeps the
    /// scheme textfold ships.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, Vec<String>>,

    /// The file this came from, and where it goes back to. Not part of the
    /// file itself, and `None` when there is nowhere to write — which is also
    /// how the tests keep their hands off the real one.
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        let mut config: Self = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        config.path = path;
        config
    }

    /// Settings kept in `path` rather than the usual place.
    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            ..Self::default()
        }
    }

    /// Write the settings back, making the directory if it is not there.
    ///
    /// A failure here is worth telling someone about — a setting that did not
    /// stick is worse than one that never changed — but it is not worth
    /// stopping for, so the caller gets the words and decides.
    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Whether there is a real place on disk behind these settings.
    ///
    /// False in the tests, which is what keeps a test run from writing over
    /// the sessions and settings of whoever is running it.
    pub fn is_stored(&self) -> bool {
        self.path.is_some()
    }

    /// The theme asked for, as written. Whether there is a file of that name
    /// is [`Themes`](crate::theme::Themes)' business: a theme you have set is
    /// a theme you have set, even on a machine where its file is not copied.
    pub fn theme_name(&self) -> &str {
        self.theme
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(theme::DEFAULT)
    }

    pub fn tab_width(&self) -> usize {
        // Zero would be an infinite loop dressed as a setting.
        self.tab_width.unwrap_or(DEFAULT_TAB_WIDTH).clamp(1, 16)
    }

    pub fn spaces(&self) -> bool {
        self.spaces.unwrap_or(true)
    }

    pub fn line_numbers(&self) -> LineNumbers {
        match self
            .line_numbers
            .as_deref()
            .map(str::trim)
            .unwrap_or("absolute")
        {
            "off" | "none" | "no" => LineNumbers::Off,
            "relative" | "rel" => LineNumbers::Relative,
            "both" | "hybrid" => LineNumbers::Both,
            _ => LineNumbers::Absolute,
        }
    }

    pub fn wrap(&self) -> bool {
        self.wrap.unwrap_or(false)
    }

    pub fn scrolloff(&self) -> usize {
        self.scrolloff.unwrap_or(3).min(20)
    }

    pub fn background(&self) -> bool {
        self.background.unwrap_or(true)
    }

    pub fn format_on_save(&self) -> bool {
        self.format_on_save.unwrap_or(false)
    }

    /// The server-side fixes to apply on save. Empty means none.
    pub fn code_actions_on_save(&self) -> &[String] {
        self.code_actions_on_save.as_deref().unwrap_or(&[])
    }

    pub fn trim_trailing_whitespace(&self) -> bool {
        self.trim_trailing_whitespace.unwrap_or(false)
    }

    pub fn final_newline(&self) -> bool {
        self.final_newline.unwrap_or(true)
    }

    pub fn auto_completion(&self) -> bool {
        self.auto_completion.unwrap_or(true)
    }

    pub fn auto_pairs(&self) -> bool {
        self.auto_pairs.unwrap_or(true)
    }

    pub fn mouse(&self) -> bool {
        self.mouse.unwrap_or(true)
    }

    pub fn restore_session(&self) -> bool {
        self.restore_session.unwrap_or(true)
    }

    pub fn reload_on_change(&self) -> bool {
        self.reload_on_change.unwrap_or(true)
    }

    pub fn show_whitespace(&self) -> bool {
        self.show_whitespace.unwrap_or(false)
    }

    pub fn enhanced_keys(&self) -> bool {
        self.enhanced_keys.unwrap_or(true)
    }

    /// Whether a problem's underline is drawn in the colour of how bad it is.
    pub fn underline_colour(&self) -> bool {
        match self
            .underline_colour
            .as_deref()
            .map(str::trim)
            .unwrap_or("auto")
        {
            "on" | "yes" | "true" | "always" => true,
            "off" | "no" | "false" | "never" => false,
            _ => crate::term::understands_underline_colour(),
        }
    }

    pub fn rulers(&self) -> &[usize] {
        self.rulers.as_deref().unwrap_or(&[])
    }

    /// Where else to look for packages.
    pub fn package_paths(&self) -> &[String] {
        &self.package_paths
    }

    pub fn package_repositories(&self) -> &[crate::repo::Repository] {
        &self.package_repositories
    }

    pub fn check_for_updates(&self) -> bool {
        self.check_for_updates.unwrap_or(true)
    }
}

/// How the column down the left is numbered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineNumbers {
    Off,
    /// What line it is.
    Absolute,
    /// How far it is from the cursor, which is what you want when you are
    /// about to jump a known distance.
    Relative,
    /// Relative everywhere except the line you are on, which says which one
    /// it actually is.
    Both,
}

/// Where settings, themes and language definitions live.
///
/// `$XDG_CONFIG_HOME` if it is set, or the platform's usual place, and then
/// `textfold` inside it.
pub fn config_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("textfold"))
}

pub fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_set_means_the_defaults() {
        let config = Config::default();
        assert_eq!(config.theme_name(), theme::DEFAULT);
        assert_eq!(config.tab_width(), DEFAULT_TAB_WIDTH);
        assert!(config.spaces());
        assert_eq!(config.line_numbers(), LineNumbers::Absolute);
    }

    #[test]
    fn a_silly_tab_width_is_brought_back_to_a_sane_one() {
        let config = Config {
            tab_width: Some(0),
            ..Config::default()
        };
        assert_eq!(config.tab_width(), 1);
    }

    #[test]
    fn only_what_was_changed_is_written() {
        let dir = std::env::temp_dir().join(format!("textfold-cfg-{}", std::process::id()));
        let path = dir.join("config.json");
        let mut config = Config::at(path.clone());
        config.theme = Some("gruvbox".into());
        config.save().expect("write");

        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("gruvbox"));
        // The forty settings left alone are not in the file.
        assert!(!text.contains("tab_width"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_from_a_later_version_is_still_read() {
        // Unknown fields are what a newer textfold's settings look like from
        // here, and are not a reason to throw the rest away.
        let config: Config =
            serde_json::from_str(r#"{"theme":"nord","telepathy":true}"#).expect("read");
        assert_eq!(config.theme_name(), "nord");
    }
}
