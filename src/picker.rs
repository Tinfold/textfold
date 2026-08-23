//! The list that opens over everything: files, commands, symbols, problems,
//! colours, whatever a language server offered to do.
//!
//! One widget for all of them, because they are all the same thing — a list,
//! a box to narrow it with, and a way to choose. Learning the file picker
//! teaches you the command palette and the symbol list for free, and a
//! keystroke that works in one works in all of them.
//!
//! Narrowing is fuzzy, in the way people have come to expect: `mrs` finds
//! `src/main.rs`, and matched letters are shown lit up so you can see why
//! something is on the list.

use std::path::PathBuf;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::layout::Rect;
use serde_json::Value;

use crate::cmd::Cmd;
use crate::doc::{DocId, Severity};
use crate::lang::LangId;
use crate::lsp::ServerId;

/// What choosing a row does.
#[derive(Clone, Debug)]
pub enum Choice {
    /// Run a command, as though its key had been pressed.
    Command(Cmd),
    /// Open a file.
    Path(PathBuf),
    /// Switch to a buffer already open.
    Buffer(DocId),
    /// Somewhere in the file you are in.
    Here(usize),
    /// Somewhere in some file, from a language server or a search.
    There {
        path: PathBuf,
        line: usize,
        column: usize,
    },
    Theme(String),
    Language(LangId),
    /// Something a language server offered to do about the code.
    Action(ServerId, Box<Value>),
    /// A setting, toggled or cycled in place rather than chosen and closed.
    Setting(&'static str),
}

/// One row.
#[derive(Clone, Debug)]
pub struct Row {
    /// What is matched against and shown first.
    pub label: String,
    /// The quieter half: a path, a type, a description.
    pub detail: Option<String>,
    /// A short word on the left, in colour: `error`, `edit`, `fn`.
    pub tag: Option<String>,
    /// The key that also does this, for commands.
    pub key: Option<String>,
    /// Colours a row by what it is, where that means something.
    pub severity: Option<Severity>,
    pub choice: Choice,
}

impl Row {
    pub fn new(label: impl Into<String>, choice: Choice) -> Self {
        Self {
            label: label.into(),
            detail: None,
            tag: None,
            key: None,
            severity: None,
            choice,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    pub fn key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
    }

    pub fn severity(mut self, severity: Severity) -> Self {
        self.severity = Some(severity);
        self
    }
}

/// What kind of list this is. The picker itself does not care; the editor does,
/// because choosing a row means different things and because some lists are
/// rebuilt as you type rather than narrowed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Files,
    Commands,
    Buffers,
    Symbols,
    /// Symbols across the project, which the server works out afresh for every
    /// query rather than us narrowing a list it sent once.
    WorkspaceSymbols,
    Diagnostics,
    Themes,
    Languages,
    Actions,
    References,
    /// Lines matching a search across the project, also worked out afresh.
    Grep,
    Settings,
}

impl Kind {
    /// What to call it at the top of the box.
    pub fn title(&self) -> &'static str {
        match self {
            Kind::Files => "Open file",
            Kind::Commands => "Commands",
            Kind::Buffers => "Buffers",
            Kind::Symbols => "Symbols in this file",
            Kind::WorkspaceSymbols => "Symbols in the project",
            Kind::Diagnostics => "Problems",
            Kind::Themes => "Colours",
            Kind::Languages => "Language",
            Kind::Actions => "What can be done here",
            Kind::References => "Used here",
            Kind::Grep => "Search the project",
            Kind::Settings => "Settings",
        }
    }

    /// The line under the box, saying what this list can do that is not
    /// obvious. Empty where nothing needs saying.
    pub fn hint(&self) -> &'static str {
        match self {
            Kind::Files => "> commands   @ symbols   # project symbols   : line",
            Kind::Themes => "Moving through the list tries each one on",
            Kind::Settings => "Enter changes the setting and keeps it",
            _ => "",
        }
    }
}

/// The list, the box, and where you are in it.
pub struct Picker {
    pub kind: Kind,
    pub query: String,
    /// Where the caret is in the query, in characters.
    pub caret: usize,
    /// Everything, in the order it was given.
    rows: Vec<Row>,
    /// Which rows are showing, best match first, with the letters that matched.
    shown: Vec<(usize, Vec<u32>)>,
    pub cursor: usize,
    pub top: usize,
    /// Where the list was last drawn, so a click can find a row.
    pub area: Rect,
    /// The row a theme picker started from, to put back if you change your
    /// mind. Only used where moving through the list changes something.
    pub restore: Option<String>,
    matcher: Matcher,
}

impl Picker {
    pub fn new(kind: Kind, rows: Vec<Row>) -> Self {
        let mut it = Self {
            kind,
            query: String::new(),
            caret: 0,
            rows,
            shown: Vec::new(),
            cursor: 0,
            top: 0,
            area: Rect::default(),
            restore: None,
            // Paths get the path-aware ranking, which knows that a match in
            // the file name beats a match in a directory three levels up.
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
        };
        it.refilter();
        it
    }

    /// Replace the rows, keeping the query. For a list the server rebuilds as
    /// you type.
    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.refilter();
    }

    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    pub fn len(&self) -> usize {
        self.shown.len()
    }

    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// The rows on show, in order, with the positions of the letters that
    /// matched so the drawing can light them up.
    pub fn visible(&self) -> impl Iterator<Item = (&Row, &[u32])> {
        self.shown
            .iter()
            .map(|(at, indices)| (&self.rows[*at], indices.as_slice()))
    }

    pub fn row(&self, index: usize) -> Option<&Row> {
        self.shown.get(index).map(|(at, _)| &self.rows[*at])
    }

    pub fn selected(&self) -> Option<&Row> {
        self.row(self.cursor)
    }

    pub fn type_char(&mut self, c: char) {
        let at = self.byte_at(self.caret);
        self.query.insert(at, c);
        self.caret += 1;
        self.refilter();
    }

    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let at = self.byte_at(self.caret - 1);
        self.query.remove(at);
        self.caret -= 1;
        self.refilter();
    }

    pub fn delete(&mut self) {
        let at = self.byte_at(self.caret);
        if at < self.query.len() {
            self.query.remove(at);
            self.refilter();
        }
    }

    /// Take back the word before the caret, which is what Ctrl-W does in every
    /// box you have ever typed a path into.
    pub fn delete_word(&mut self) {
        let mut at = self.caret;
        while at > 0 && self.nth(at - 1).is_some_and(char::is_whitespace) {
            at -= 1;
        }
        while at > 0 && self.nth(at - 1).is_some_and(|c| !c.is_whitespace()) {
            at -= 1;
        }
        let from = self.byte_at(at);
        let to = self.byte_at(self.caret);
        self.query.replace_range(from..to, "");
        self.caret = at;
        self.refilter();
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.caret = 0;
        self.refilter();
    }

    pub fn move_caret(&mut self, by: isize) {
        let len = self.query.chars().count();
        self.caret = (self.caret as isize + by).clamp(0, len as isize) as usize;
    }

    fn nth(&self, at: usize) -> Option<char> {
        self.query.chars().nth(at)
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.query
            .char_indices()
            .nth(chars)
            .map(|(at, _)| at)
            .unwrap_or(self.query.len())
    }

    /// Move through the list, wrapping round the ends — a list you cannot fall
    /// off is a list you can hold a key down in.
    pub fn step(&mut self, by: isize) {
        if self.shown.is_empty() {
            self.cursor = 0;
            return;
        }
        let len = self.shown.len() as isize;
        self.cursor = (self.cursor as isize + by).rem_euclid(len) as usize;
        self.follow();
    }

    pub fn select(&mut self, at: usize) {
        self.cursor = at.min(self.shown.len().saturating_sub(1));
        self.follow();
    }

    /// How many rows the box has room for, from the last time it was drawn.
    pub fn height(&self) -> usize {
        self.area.height.max(1) as usize
    }

    /// Settle the scroll against the height the box turned out to be.
    ///
    /// A picker is built and scrolled to a row before anything knows how tall
    /// it will be — the theme list opens on the theme you are using — so the
    /// first scroll is made against a guess. This is the drawing correcting
    /// it, and without it a list that fits in the box can still open halfway
    /// down itself.
    pub fn fit(&mut self, height: usize) {
        self.area.height = height as u16;
        let last = self.shown.len().saturating_sub(height);
        self.top = self.top.min(last);
        self.follow();
    }

    fn follow(&mut self) {
        let height = self.height();
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + height {
            self.top = self.cursor + 1 - height;
        }
    }

    /// Narrow the list to what the query matches, best first.
    fn refilter(&mut self) {
        let query = self.query.trim();
        self.shown.clear();

        if query.is_empty() {
            // No query means the order the rows were given in, which is
            // deliberate everywhere: commands are grouped, problems are worst
            // first, buffers are most recent first.
            self.shown
                .extend((0..self.rows.len()).map(|at| (at, Vec::new())));
        } else {
            // Smart case: a lower-case query ignores case, and a query with a
            // capital in it means it.
            let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize, Vec<u32>)> = Vec::new();
            for (at, row) in self.rows.iter().enumerate() {
                // Matched against the label and the detail together, so that
                // typing a directory name finds files inside it.
                let haystack = match &row.detail {
                    Some(detail) => format!("{} {detail}", row.label),
                    None => row.label.clone(),
                };
                let mut indices = Vec::new();
                let text = Utf32Str::new(&haystack, &mut buf);
                if let Some(score) = pattern.indices(text, &mut self.matcher, &mut indices) {
                    indices.sort_unstable();
                    indices.dedup();
                    // Only the part of the match that falls on the label can
                    // be lit up; the detail is drawn separately.
                    let label_len = row.label.chars().count() as u32;
                    indices.retain(|&i| i < label_len);
                    scored.push((score, at, indices));
                }
            }
            // Best first; ties keep the order they were given in, so a list
            // that was sorted for a reason stays sorted where scores agree.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.shown
                .extend(scored.into_iter().map(|(_, at, indices)| (at, indices)));
        }

        self.cursor = self.cursor.min(self.shown.len().saturating_sub(1));
        self.top = 0;
        self.follow();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(labels: &[&str]) -> Vec<Row> {
        labels
            .iter()
            .map(|l| Row::new(*l, Choice::Here(0)))
            .collect()
    }

    #[test]
    fn nothing_typed_shows_everything_in_the_order_given() {
        let picker = Picker::new(Kind::Files, rows(&["b", "a", "c"]));
        let shown: Vec<&str> = picker.visible().map(|(row, _)| row.label.as_str()).collect();
        assert_eq!(shown, ["b", "a", "c"]);
    }

    #[test]
    fn letters_scattered_through_a_path_still_find_it() {
        let mut picker = Picker::new(
            Kind::Files,
            rows(&["src/main.rs", "docs/readme.md", "src/theme.rs"]),
        );
        for c in "mrs".chars() {
            picker.type_char(c);
        }
        assert_eq!(
            picker.selected().map(|r| r.label.as_str()),
            Some("src/main.rs")
        );
    }

    #[test]
    fn the_letters_that_matched_are_known_so_they_can_be_shown() {
        let mut picker = Picker::new(Kind::Files, rows(&["main.rs"]));
        picker.type_char('m');
        picker.type_char('n');
        let (_, indices) = picker.visible().next().expect("a match");
        assert_eq!(indices, [0, 3]);
    }

    #[test]
    fn a_query_matching_nothing_shows_nothing_rather_than_everything() {
        let mut picker = Picker::new(Kind::Files, rows(&["one", "two"]));
        for c in "zzzz".chars() {
            picker.type_char(c);
        }
        assert!(picker.is_empty());
        assert!(picker.selected().is_none());
    }

    #[test]
    fn the_detail_is_searched_as_well_as_the_label() {
        let picker_rows = vec![
            Row::new("main", Choice::Here(0)).detail("src/deep/place.rs"),
            Row::new("other", Choice::Here(0)).detail("elsewhere.rs"),
        ];
        let mut picker = Picker::new(Kind::Files, picker_rows);
        for c in "deep".chars() {
            picker.type_char(c);
        }
        assert_eq!(picker.len(), 1);
        assert_eq!(picker.selected().map(|r| r.label.as_str()), Some("main"));
    }

    #[test]
    fn moving_through_the_list_wraps_round() {
        let mut picker = Picker::new(Kind::Files, rows(&["a", "b", "c"]));
        picker.area = Rect::new(0, 0, 40, 3);
        picker.step(-1);
        assert_eq!(picker.selected().map(|r| r.label.as_str()), Some("c"));
        picker.step(1);
        assert_eq!(picker.selected().map(|r| r.label.as_str()), Some("a"));
    }

    #[test]
    fn the_query_is_edited_where_the_caret_is() {
        let mut picker = Picker::new(Kind::Files, rows(&["x"]));
        for c in "hello".chars() {
            picker.type_char(c);
        }
        picker.move_caret(-2);
        picker.type_char('-');
        assert_eq!(picker.query, "hel-lo");
        picker.delete_word();
        assert_eq!(picker.query, "lo");
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_cursor_in_view() {
        let labels: Vec<String> = (0..100).map(|i| format!("row {i}")).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let mut picker = Picker::new(Kind::Files, rows(&refs));
        picker.area = Rect::new(0, 0, 40, 10);
        picker.select(50);
        assert!(picker.top <= 50 && 50 < picker.top + 10);
    }
}
