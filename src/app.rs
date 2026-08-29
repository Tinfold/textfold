//! The editor itself: what is open, what is on screen, and what a keystroke
//! or a click means.
//!
//! Everything arrives here as an [`Event`] — a keystroke, a click, a message
//! from a language server, a list of files a thread went and found. There is
//! one channel and one loop, so there is one place where the order of things
//! is decided and no locks anywhere near the text.
//!
//! Keystrokes are looked at in one order, always: whatever is open on top of
//! the editor first, then the completion list, then the key bindings, then —
//! if it would type a character — the text. That order is what makes Escape
//! reliable and typing never surprising.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use serde_json::{Value, json};

use crate::cmd::{self, Behaviour, Cmd, Group, Spec};
use crate::plugin::{Output, Tool};
use crate::config::{Config, LineNumbers};
use crate::doc::{Diagnostic, DocId, Document, Indent, OnDisk, Severity};
use crate::edit::{self, Motion};
use crate::git::Tracker;
use crate::keys::{Key, Keys};
use crate::lang::{self, LangId};
use crate::host::{HostId, Hosts};
use crate::lsp::{Ask, Goto, Incoming, ServerId, Servers};
use crate::menu::{self, Menu};
use crate::picker::{Choice, Kind, Picker, Row};
use crate::text::{self, Range, Selections};
use crate::theme::{Role, Theme, Themes};
use crate::view::{self, View};

/// Everything that can happen.
pub enum Event {
    /// A key, a click, a resize, a paste.
    Term(TermEvent),
    /// A language server said something.
    Lsp(ServerId, Incoming),
    /// A thread finished walking the project.
    Files(Vec<PathBuf>),
    /// A thread finished searching the project.
    Found(String, Vec<Row>),
    /// A plugin's own program said something.
    Plugin(HostId, Incoming),
    /// A program the editor ran for a plugin has finished, and the plugin is
    /// still waiting to be told how it went.
    PluginRan(Box<crate::host::Ran>),
    /// A tool a plugin runs has finished. Boxed because it carries everything
    /// the program printed, and an event that is occasionally a megabyte
    /// should not make every keystroke a megabyte to move about.
    Tool(Box<crate::tool::Finished>),
    /// An install or an uninstall has got somewhere. Boxed for the same
    /// reason: `npm install` has a great deal to say for itself.
    Package(Box<crate::pack::Progress>),
    /// The package repositories have been asked what they have, with whatever
    /// could not be reached. Nothing has been installed by it.
    Refreshed(Vec<String>),
}

/// A plugin waiting on an answer from the person at the keyboard.
///
/// One slot rather than a list, because only one overlay can be up at a time
/// and these are all overlays. Whatever puts the box on the screen fills this
/// in; whatever takes the box away has to empty it, one way or the other — a
/// plugin left waiting on a box that has gone is a plugin that has hung.
pub struct Asked {
    host: HostId,
    request: Value,
}

/// Who wanted a file read again.
///
/// The difference is what may be guessed at. Reading a file somebody asked for
/// can do the best it can with whatever is there; rewriting a buffer under
/// them on a timer has to be sure, because they were not watching for it and
/// have no reason to suspect it happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reread {
    /// Somebody ran `reload`.
    Asked,
    /// The timer noticed the file had changed.
    OnATimer,
}

/// An install or an uninstall that is running.
///
/// The log is kept here rather than written straight into a buffer because
/// what an installer prints arrives in pieces and a buffer being rewritten
/// under you eleven times is worse than one that appears when it is finished.
pub struct Installing {
    id: String,
    /// Whether this is taking something away, for the words used about it.
    removing: bool,
    log: String,
}

/// What came of a plugin asking the editor for something.
enum Answer {
    /// Worked out on the spot.
    Now(Value),
    /// Started, and the reply goes back when it finishes. The one case where
    /// nothing is sent yet — and it has a name rather than being a silence,
    /// so that forgetting to answer looks different from deciding not to.
    Later,
    /// No, and why. A plugin left waiting on a reply that never comes is a
    /// plugin that has hung with nothing on the screen to say so.
    No(String),
}

impl From<Result<Value, String>> for Answer {
    fn from(result: Result<Value, String>) -> Self {
        match result {
            Ok(value) => Answer::Now(value),
            Err(why) => Answer::No(why),
        }
    }
}

/// How long the mouse has to sit still over a word before textfold asks what
/// it is. Long enough not to fire while you are moving across the screen,
/// short enough that you do not wonder whether it is going to.
const HOVER_DELAY: Duration = Duration::from_millis(400);

/// How long a message stays in the status line.
const MESSAGE_TIME: Duration = Duration::from_secs(6);

/// Two clicks closer together than this are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// How long the cursor has to sit on a problem before the editor asks what
/// could be done about it. Long enough that walking along a line of red is one
/// question rather than forty.
const FIX_DELAY: Duration = Duration::from_millis(250);

/// How many times a language server may say "not now" about the fixes for one
/// spot before the editor takes it for an answer.
const FIX_TRIES: u8 = 4;

/// The two kinds of "fix the whole file" every language server agrees on the
/// names of. Written down once rather than spelled out at each of the four
/// places that ask for them.
const SOURCE_FIX_ALL: &str = "source.fixAll";
const SOURCE_ORGANIZE_IMPORTS: &str = "source.organizeImports";

/// How long a save waits for the servers to say what they would fix before
/// going ahead without them. Long enough for a linter, short enough that a
/// wedged server costs you a pause rather than your file.
const BEFORE_SAVE_WAIT: Duration = Duration::from_millis(1500);

/// How often what is open is written down. Often enough that a crash costs a
/// few tabs, rarely enough that opening a directory's worth of files is not a
/// write per file.
const SESSION_WRITE_EVERY: Duration = Duration::from_secs(3);

/// How often the diff against the last commit is worked out again.
const GIT_CHECK_EVERY: Duration = Duration::from_millis(150);

/// How often the open files are looked at on disk. Often enough that a `git
/// checkout` in the next window is noticed while you are still thinking about
/// it, rarely enough that a hundred open files cost nothing.
const DISK_CHECK_EVERY: Duration = Duration::from_millis(1200);

/// How soon to look again when a file was in the middle of changing.
///
/// A file is only read once it has looked the same twice running, so this is
/// what that costs: the wait between the two looks. Short, because it is the
/// delay on noticing an ordinary `git checkout` as well as the patience shown
/// to a file being written — and cheap, because it only happens while
/// something is actually moving.
const SETTLE_CHECK_EVERY: Duration = Duration::from_millis(250);

/// How long the cursor has to sit still before the plugins are told where it
/// is. Cursor motion is the highest-frequency event there is, and a plugin
/// that asks a language model where the cursor is should be asked once when
/// you stop, not forty times on the way.
const SELECTION_SETTLES: Duration = Duration::from_millis(120);

/// How long to wait after a keystroke before asking for completions, so that
/// typing a word is one request rather than six.
const COMPLETION_DELAY: Duration = Duration::from_millis(120);

/// How fast a tab held against the end of the row walks along. Slow enough to
/// stop on the one you meant, quick enough not to be a wait.
const TAB_STEP_EVERY: Duration = Duration::from_millis(140);

/// What is open over the editor.
pub enum Overlay {
    None,
    Picker(Picker),
    Prompt(Prompt),
    Confirm(Confirm),
    /// The keys and what they do, scrolled to this row.
    Help(usize),
    /// A short list of what can be done right here, opened where you clicked.
    Menu(Menu),
}

/// A single line to type into.
pub struct Prompt {
    pub kind: PromptKind,
    pub input: String,
    pub caret: usize,
    /// Where the cursors were when this opened, to put back if you change
    /// your mind. Searching moves you around as you type, and Escape has to
    /// undo that.
    pub origin: Option<Selections>,
    /// The search term, while the second half of a replace is being typed.
    pub held: String,
    /// Whether Enter has been pressed in this search: whether where the cursor
    /// has got to is somewhere you meant to go, or only somewhere typing took
    /// it on the way.
    pub committed: bool,
    /// A label of its own, for a prompt whose kind cannot know what it is
    /// asking — which so far means one a plugin put up.
    pub label: Option<String>,
}

impl Prompt {
    /// An empty one of a kind, for a caller that will fill in the rest.
    pub fn new(kind: PromptKind) -> Self {
        Prompt {
            kind,
            input: String::new(),
            caret: 0,
            origin: None,
            held: String::new(),
            committed: false,
            label: None,
        }
    }

    /// What to write at the left of the box.
    pub fn label(&self) -> &str {
        match &self.label {
            Some(given) => given,
            None => self.kind.label(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptKind {
    GotoLine,
    /// A path, typed or pasted in full. The fuzzy list is the nicer way to
    /// open a file by hand; this is the way to open one you already know the
    /// name of, and the way another program can say which.
    OpenPath,
    SaveAs,
    Rename,
    /// Search this file, moving as you type.
    Find,
    /// The search half of a replace.
    ReplaceFind,
    /// The replacement half.
    ReplaceWith,
    /// A question a plugin asked. What it says is the plugin's, so the label
    /// here is only the fallback for one that said nothing.
    PluginAsked,
}

impl PromptKind {
    pub fn label(&self) -> &'static str {
        match self {
            PromptKind::GotoLine => "Go to line",
            PromptKind::OpenPath => "Open",
            PromptKind::SaveAs => "Save as",
            PromptKind::Rename => "Rename to",
            PromptKind::Find => "Find",
            PromptKind::ReplaceFind => "Replace what",
            PromptKind::ReplaceWith => "Replace with",
            PromptKind::PluginAsked => "A plugin asks",
        }
    }
}

/// A question with two or three answers.
pub struct Confirm {
    pub message: String,
    pub choices: Vec<(char, String)>,
    pub then: Then,
}

/// What a confirmation is about.
#[derive(Clone, Copy, Debug)]
pub enum Then {
    Close(DocId),
    Quit,
    Reload(DocId),
    /// A plugin asked, and is waiting to be told which way it went.
    PluginAsked,
}

/// One thing a language server offered to insert.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub label: String,
    /// The rest of the name, which a server sends apart from it: the
    /// arguments of a function, or the `(use std::collections::HashMap)` that
    /// says this one arrives with an import. Shown against the label and left
    /// out of the matching, so that typing `HashMap` matches `HashMap`.
    pub suffix: Option<String>,
    pub detail: Option<String>,
    pub kind: &'static str,
    /// The colour that word is drawn in: the one the thing itself has in the
    /// file. See [`completion_role`].
    pub role: Role,
    /// What actually goes in, and over what — a server usually says exactly
    /// which characters it means to replace, and taking its word for it is
    /// what makes completing in the middle of a word work.
    pub replace: Option<(usize, usize)>,
    pub insert: String,
    /// Imports and the like, to put in at the same time.
    pub also: Vec<(usize, usize, String)>,
    /// What to sort by, which is not always what to show.
    pub sort: String,
    pub about: Option<String>,
    /// The item exactly as the server sent it, to hand back when asking for
    /// the rest of it.
    raw: Value,
    /// How far along asking the server for the rest of it has got.
    resolve: Resolve,
}

/// Whether a suggestion has had the parts a server is allowed to leave out
/// filled in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Resolve {
    /// Nobody has asked. Either the cursor has not been on it, or there is no
    /// server to ask.
    Unasked,
    /// Asked, and the answer has not come back.
    Waiting,
    /// As complete as it is going to get — the answer came back, the answer
    /// failed, or this server fills its suggestions in to begin with.
    Done,
}

/// The list of suggestions under the cursor.
pub struct Completion {
    pub doc: DocId,
    /// Who answered, for asking them about one suggestion in more detail.
    server: ServerId,
    /// Whether the server said this list was as much of an answer as it had
    /// room for. It nearly always is where imports are concerned: a server
    /// asked about `Ha` will not list every unimported name in every crate,
    /// it lists some of them and says so. Narrowing such a list as you type
    /// hides the thing you are typing towards, so it is asked again instead.
    incomplete: bool,
    /// Where the word being completed starts, so that typing narrows the list
    /// instead of asking again.
    pub start: usize,
    all: Vec<Suggestion>,
    shown: Vec<usize>,
    pub cursor: usize,
    pub top: usize,
    pub area: Rect,
}

impl Completion {
    pub fn visible(&self) -> impl Iterator<Item = &Suggestion> {
        self.shown.iter().map(|at| &self.all[*at])
    }

    pub fn len(&self) -> usize {
        self.shown.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    pub fn selected(&self) -> Option<&Suggestion> {
        self.shown.get(self.cursor).map(|at| &self.all[*at])
    }

    pub fn height(&self) -> usize {
        self.area.height.max(1) as usize
    }

    pub fn step(&mut self, by: isize) {
        if self.shown.is_empty() {
            return;
        }
        let len = self.shown.len() as isize;
        self.cursor = (self.cursor as isize + by).rem_euclid(len) as usize;
        let height = self.height();
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + height {
            self.top = self.cursor + 1 - height;
        }
    }

    /// Narrow to what has been typed since the list arrived.
    fn narrow(&mut self, typed: &str) {
        self.shown.clear();
        let lower = typed.to_lowercase();
        let mut scored: Vec<(u8, usize)> = Vec::new();
        for (at, item) in self.all.iter().enumerate() {
            if typed.is_empty() {
                scored.push((0, at));
                continue;
            }
            let label = item.label.to_lowercase();
            // Three tiers, and no fuzzier than that: a completion list is
            // ranked by the server already, and re-ranking it by cleverness
            // moves the thing you wanted away from the top.
            let rank = if item.label.starts_with(typed) {
                0
            } else if label.starts_with(&lower) {
                1
            } else if subsequence(&label, &lower) {
                2
            } else {
                continue;
            };
            scored.push((rank, at));
        }
        scored.sort_by_key(|(rank, at)| (*rank, self.all[*at].sort.clone(), *at));
        self.shown.extend(scored.into_iter().map(|(_, at)| at));
        self.cursor = 0;
        self.top = 0;
    }
}

/// One line about files that need a person to look at them, worst first.
///
/// Names while there is one name to give, and a count once there are several,
/// because "nine files" is the useful part of nine names.
fn disk_news(clashed: &[String], gone: &[String]) -> Option<String> {
    let said = |names: &[String], one: &str, many: &str| match names.len() {
        0 => None,
        1 => Some(format!("{} {one}", names[0])),
        n => Some(format!("{n} files {many}")),
    };
    let clashed = said(
        clashed,
        "changed on disk, and has unsaved changes here",
        "changed on disk and have unsaved changes here",
    );
    let gone = said(gone, "is gone from disk", "are gone from disk");
    match (clashed, gone) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Whether a kind of code is the kind that has a definition to go to.
///
/// Types, functions and the names of things, but not the words the language
/// itself is made of, not literals, and not local variables and parameters —
/// a definition of `self` or of `0` is not a thing.
fn names_something(role: Role) -> bool {
    matches!(
        role,
        Role::Type
            | Role::TypeBuiltin
            | Role::Function
            | Role::FunctionBuiltin
            | Role::Method
            | Role::Constructor
            | Role::Macro
            | Role::Namespace
            | Role::Property
            | Role::Constant
            | Role::Attribute
    )
}

/// The parts of a line of markdown that were written as code: the runs inside
/// backticks, and the text of a link.
///
/// Character ranges, because that is what a column on the screen is. Nothing
/// here is a markdown parser — it is looking for the two shapes a language
/// server uses to say "this word is a name", and anything it misses is a word
/// that does not light up, which is the safe way to be wrong.
fn code_spans_in_prose(text: &str) -> Vec<std::ops::Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        match chars[at] {
            '`' => {
                // ``` and `` are both openers; the same run of them closes it.
                let ticks = chars[at..].iter().take_while(|c| **c == '`').count();
                let from = at + ticks;
                let mut to = from;
                while to < chars.len() {
                    if chars[to] == '`'
                        && chars[to..].iter().take_while(|c| **c == '`').count() == ticks
                    {
                        break;
                    }
                    to += 1;
                }
                if to < chars.len() && to > from {
                    out.push(from..to);
                }
                at = (to + ticks).max(at + 1);
            }
            // `[text](url)` and `[text][ref]`. The text is the part a person
            // reads and the part worth following; the URL is for a browser.
            '[' => {
                let from = at + 1;
                match chars[from..].iter().position(|c| *c == ']') {
                    Some(len) if len > 0 => {
                        let close = from + len;
                        // Only where it really is a link. A bare `[` in prose
                        // is a bracket.
                        let linked = chars
                            .get(close + 1)
                            .is_some_and(|c| *c == '(' || *c == '[' || *c == ':');
                        // `[`Foo`](url)` is the ordinary shape, so the
                        // backticks come off and the range is the name itself.
                        let mut start = from;
                        let mut end = close;
                        while start < end && chars[start] == '`' {
                            start += 1;
                        }
                        while end > start && chars[end - 1] == '`' {
                            end -= 1;
                        }
                        if linked
                            && end > start
                            && !out.iter().any(|had| had.start <= start && end <= had.end)
                        {
                            out.push(start..end);
                        }
                        // Past the `]`, whatever was trimmed off inside it, so
                        // the backtick that closed the name is not read as one
                        // opening another.
                        at = close + 1;
                    }
                    _ => at += 1,
                }
            }
            _ => at += 1,
        }
    }
    out
}

/// A name in a box of documentation that can be followed, and where on the
/// screen its letters are.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Link {
    pub word: String,
    pub row: u16,
    pub from: u16,
    pub to: u16,
}

/// The word at a column of a line of rendered documentation, and the columns
/// it occupies.
///
/// The same idea of a word the editor uses in code — letters, digits and
/// underscores — because what is being clicked on in a docstring is a type or
/// a function name, and the punctuation round it is prose.
///
/// A single letter is not a name anybody means to follow; `T` and `a` are
/// everywhere in a paragraph of prose and lighting them up would make the
/// whole box twitch as the pointer crossed it.
fn word_span(line: &str, column: usize) -> Option<(String, usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let part = |c: &char| c.is_alphanumeric() || *c == '_';
    if !chars.get(column).is_some_and(part) {
        return None;
    }
    let start = chars[..column]
        .iter()
        .rposition(|c| !part(c))
        .map_or(0, |at| at + 1);
    let end = chars[column..]
        .iter()
        .position(|c| !part(c))
        .map_or(chars.len(), |at| column + at);
    let word: String = chars[start..end].iter().collect();
    // Nor is a bare number, which is a length or a version and not a type.
    let worth_following =
        word.chars().count() > 1 && word.chars().any(|c| c.is_alphabetic() || c == '_');
    worth_following.then_some((word, start, end))
}


/// Whether every letter of `needle` appears in `haystack`, in order.
fn subsequence(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|want| chars.any(|c| c == want))
}

/// A box of text floating over the editor: what something is, or what
/// arguments it takes.
pub struct Popup {
    /// What is drawn: the text folded to the width the box turned out to be.
    /// Everything that answers a click or a scroll works in these, because
    /// these are the rows a person is looking at.
    pub lines: Vec<DocLine>,
    /// The same text before it was folded, kept so that a box which changes
    /// width — the terminal is resized, or a glance becomes something you
    /// asked to read — folds the original again rather than folding what was
    /// already folded.
    source: Vec<DocLine>,
    /// The width `lines` was folded to, so that redrawing at the same width
    /// costs nothing.
    folded_at: usize,
    /// Where in the document it is about, so it can be drawn beside it and
    /// closed when you move away.
    pub at: usize,
    pub scroll: usize,
    /// Whether the keyboard is in the box rather than in the text.
    ///
    /// A hover that steals every key would be a hover you had to escape from
    /// to carry on typing, so it does not: it appears, and it goes away the
    /// moment you touch anything. Asking for it a second time is what says
    /// you want to read it rather than glance at it, and from then until you
    /// leave it the arrows scroll it and it stays put.
    pub focused: bool,
    /// Where it was last drawn, inside its border, for answering clicks and
    /// for working out which line the pointer is on.
    pub area: Rect,
    /// The same including the border, which is what decides whether the
    /// pointer is still in the box. A pointer on the frame has not left.
    pub outer: Rect,
    /// Where the pointer is inside the box, if it is inside it. What makes a
    /// name under the pointer light up as something you can click.
    pub pointer: Option<(u16, u16)>,
    /// What has been dragged over, as (line, character) pairs into `lines`:
    /// where the drag started, and where it has got to.
    ///
    /// In the text's own coordinates rather than the screen's, so that
    /// scrolling the box carries the selection with the words it is on rather
    /// than leaving it behind on a row.
    pub select: Option<(Spot, Spot)>,
}

/// A place in a box of documentation: which line, and how far into it.
pub type Spot = (usize, usize);

impl Popup {
    pub fn new(lines: Vec<DocLine>, at: usize) -> Self {
        Self {
            source: lines.clone(),
            folded_at: 0,
            lines,
            at,
            scroll: 0,
            focused: false,
            area: Rect::default(),
            outer: Rect::default(),
            pointer: None,
            select: None,
        }
    }

    /// Fold the text to the width the box has turned out to be.
    ///
    /// Called by the drawing, which is the only thing that knows how wide the
    /// box is. Where a line was already scrolled to, it stays put: the row
    /// you were reading is found again in the folded text rather than the
    /// box jumping back to the top every time the terminal is resized.
    pub fn fold_to(&mut self, width: usize) {
        if self.folded_at == width {
            return;
        }
        // Which line of the unfolded text is on the top row now, so that it
        // can be put back on the top row afterwards.
        let was = self.unfolded_at(self.scroll);
        self.lines = self.source.iter().flat_map(|line| line.wrap(width)).collect();
        self.folded_at = width;
        self.scroll = self.folded_at_line(was);
        // A selection is in the folded text's coordinates and there is no
        // honest way to carry it across a refold, so it goes.
        self.select = None;
    }

    /// Which line of the unfolded text a folded row came from.
    fn unfolded_at(&self, row: usize) -> usize {
        let mut seen = 0;
        for (at, line) in self.source.iter().enumerate() {
            let folded = match self.folded_at {
                0 => 1,
                width => line.wrap(width).len(),
            };
            if seen + folded > row {
                return at;
            }
            seen += folded;
        }
        self.source.len().saturating_sub(1)
    }

    /// And back again: the first row of the folded text that line turned into.
    fn folded_at_line(&self, line: usize) -> usize {
        self.source
            .iter()
            .take(line)
            .map(|l| l.wrap(self.folded_at).len())
            .sum()
    }

    /// How many rows of text the box shows.
    fn rows(&self) -> usize {
        (self.area.height as usize).max(1)
    }

    /// The furthest it can be scrolled: far enough that the last line is on
    /// the bottom row, and no further.
    ///
    /// Scrolling past that would empty the box out from the top, which is the
    /// thing that makes a popup feel as though it is falling apart while you
    /// read it.
    fn furthest(&self) -> usize {
        self.lines.len().saturating_sub(self.rows())
    }

    pub fn scroll_by(&mut self, by: isize) {
        self.scroll = (self.scroll as isize + by).clamp(0, self.furthest() as isize) as usize;
    }

    /// Which line of the text is on a given screen row.
    fn line_at(&self, row: u16) -> Option<usize> {
        if row < self.area.y || row >= self.area.y + self.area.height {
            return None;
        }
        let at = self.scroll + (row - self.area.y) as usize;
        (at < self.lines.len()).then_some(at)
    }

    /// The name under a point in the box, and where it is on that row.
    ///
    /// The columns come back so the drawing can underline exactly the letters
    /// that would be followed, rather than the whole line or nothing.
    pub fn link_at(&self, column: u16, row: u16) -> Option<Link> {
        let line = self.line_at(row)?;
        if column < self.area.x || column >= self.area.x + self.area.width {
            return None;
        }
        let at = (column - self.area.x) as usize;
        let line = &self.lines[line];
        // Only where the markup said this was a name rather than a word.
        if !line.links.iter().any(|range| range.contains(&at)) {
            return None;
        }
        let (word, start, end) = word_span(&line.text, at)?;
        Some(Link {
            word,
            row,
            from: self.area.x + start as u16,
            to: self.area.x + (end.min(self.area.width as usize)) as u16,
        })
    }

    /// The place in the text under a point on the screen.
    ///
    /// Clamped rather than refused: dragging off the end of a short line
    /// should take the whole of it, which is what dragging does everywhere.
    pub fn spot_at(&self, column: u16, row: u16) -> Option<Spot> {
        if self.lines.is_empty() {
            return None;
        }
        let down = row.saturating_sub(self.area.y) as usize;
        let line = (self.scroll + down).min(self.lines.len() - 1);
        let across = column.saturating_sub(self.area.x) as usize;
        let width = self.lines[line].text.chars().count();
        Some((line, across.min(width)))
    }

    /// The selection the right way round, whichever way it was dragged.
    fn selected(&self) -> Option<(Spot, Spot)> {
        let (anchor, head) = self.select?;
        Some(if anchor <= head {
            (anchor, head)
        } else {
            (head, anchor)
        })
    }

    /// Which characters of one line are selected, for the drawing.
    pub fn selected_on(&self, line: usize) -> Option<(usize, usize)> {
        let (start, end) = self.selected()?;
        if line < start.0 || line > end.0 {
            return None;
        }
        let width = self.lines.get(line)?.text.chars().count();
        let from = if line == start.0 { start.1 } else { 0 };
        // A line selected through to the next one takes its line break with
        // it, which is one column past its last character.
        let to = if line == end.0 { end.1 } else { width + 1 };
        (to > from).then_some((from, to.min(width)))
    }

    /// Grow the selection to the word around where it was made, for a double
    /// click.
    pub fn take_word(&mut self) {
        let Some((line, at)) = self.select.map(|(_, head)| head) else {
            return;
        };
        let Some(text) = self.lines.get(line).map(|l| &l.text) else {
            return;
        };
        let chars: Vec<char> = text.chars().collect();
        let part = |c: &char| c.is_alphanumeric() || *c == '_';
        let at = at.min(chars.len().saturating_sub(1));
        if !chars.get(at).is_some_and(part) {
            return;
        }
        let from = chars[..at].iter().rposition(|c| !part(c)).map_or(0, |n| n + 1);
        let to = chars[at..]
            .iter()
            .position(|c| !part(c))
            .map_or(chars.len(), |n| at + n);
        self.select = Some(((line, from), (line, to)));
    }

    /// The whole line the selection is sitting on, for a Ctrl-C with nothing
    /// dragged over — which is what Ctrl-C means everywhere else in the
    /// editor.
    fn line_text(&self, line: usize) -> String {
        self.lines
            .get(line)
            .map(|l| if l.text == RULE { String::new() } else { l.text.clone() })
            .unwrap_or_default()
    }

    /// What has been dragged over, as text.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selected()?;
        if start == end {
            // A bare caret takes its line, the way it does in the editor.
            return Some(self.line_text(start.0));
        }
        let mut out = String::new();
        for line in start.0..=end.0 {
            if line > start.0 {
                out.push('\n');
            }
            let text = self.line_text(line);
            let width = text.chars().count();
            let from = if line == start.0 { start.1 } else { 0 };
            let to = if line == end.0 { end.1.min(width) } else { width };
            if to > from {
                out.extend(text.chars().skip(from).take(to - from));
            }
        }
        Some(out)
    }
}

/// Fixes the language server is holding ready for the problem under the
/// cursor.
pub struct Gathered {
    /// Which buffer they are about.
    pub doc: DocId,
    /// Where in it the question was about. An answer that comes back after the
    /// cursor has left is an answer to a question nobody is asking any more.
    pub at: usize,
    /// Servers still to answer.
    waiting: Vec<ServerId>,
    /// What each one offered, kept apart by who said it, so that a server
    /// answering twice replaces its own findings and nobody else's — and so
    /// that choosing a row knows which server to send it back to.
    from: Vec<(ServerId, Vec<Value>)>,
    /// Whether these have already been put on the screen as a list. Once they
    /// have, a slow server's answer fills the list in where it stands and
    /// never opens it again: a menu that reappears a second after you closed
    /// it is a menu fighting you.
    shown: bool,
}

impl Gathered {
    fn new(doc: DocId, at: usize, asked: Vec<ServerId>) -> Self {
        Self {
            doc,
            at,
            waiting: asked,
            from: Vec::new(),
            shown: false,
        }
    }

    /// Take one server's answer.
    fn take(&mut self, server: ServerId, value: Value) {
        self.waiting.retain(|id| *id != server);
        let actions: Vec<Value> = match value {
            Value::Array(items) => items
                .into_iter()
                .filter(|a| a.get("title").and_then(Value::as_str).is_some())
                .collect(),
            _ => Vec::new(),
        };
        match self.from.iter_mut().find(|(id, _)| *id == server) {
            Some((_, held)) => *held = actions,
            None => self.from.push((server, actions)),
        }
    }

    /// Whether everybody has answered.
    pub fn settled(&self) -> bool {
        self.waiting.is_empty()
    }

    /// Everything offered, in the order the servers are attached to the file,
    /// each still knowing which server it came from.
    pub fn actions(&self) -> Vec<(ServerId, &Value)> {
        self.from
            .iter()
            .flat_map(|(id, actions)| actions.iter().map(move |a| (*id, a)))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.from.iter().map(|(_, a)| a.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The shortest useful thing to call the first one, for a status bar with
    /// a line to spare and not a line to waste.
    pub fn headline(&self) -> Option<&str> {
        self.from
            .iter()
            .flat_map(|(_, actions)| actions.iter())
            .find_map(|a| a.get("title").and_then(Value::as_str))
    }
}

/// A file being got ready to be written.
///
/// Saving a Python file can mean four round trips before a byte is written:
/// ask ruff what it would fix, apply that, ask it to sort the imports, apply
/// that, ask the formatter to lay the result out, apply that, and only then
/// write. Each of those answers comes back on the event loop like everything
/// else, so what is left to do has to be written down rather than held on a
/// stack.
struct BeforeSave {
    doc: DocId,
    /// What is left to do, in order.
    ///
    /// One at a time, and this is the whole reason there is a queue rather
    /// than a set. Every one of these answers with a set of edits at positions
    /// in the file *as it was when it was asked*, so the first one applied
    /// moves everything a second one was pointing at. Doing them one after
    /// another means each is about the file as it actually is; doing them all
    /// at once and applying what comes back is how "save" quietly deletes a
    /// line.
    left: Vec<Step>,
    /// The one outstanding, so that a late answer to a question we have given
    /// up on is not applied to a file it is no longer about.
    doing: Option<Step>,
    /// Whether to write the file at the end. `false` is somebody having asked
    /// for the tidying on its own, without a save behind it.
    write: bool,
    /// Where to write it, for a "save as". Carried the whole way rather than
    /// looked up at the end: by the time the formatter has answered, the
    /// buffer still has its old path, and writing to that would put the
    /// reformatted text back in the file you were saving *away* from.
    to: Option<PathBuf>,
    /// When to stop waiting on the outstanding one. A server that never
    /// answers must not mean a file that is never saved.
    due: Instant,
}

/// One thing that has to happen before a file is written.
#[derive(Clone, PartialEq, Debug)]
enum Step {
    /// One kind of fix — `source.fixAll`, `source.organizeImports` — put to
    /// one language server.
    Fix(String, ServerId),
    /// A program a plugin brought that rewrites the file: `black`, `gofmt`.
    Rewrite(&'static Tool),
    /// The language server's own formatter.
    Format,
}

/// Something to tell the person using the editor.
pub struct Status {
    pub text: String,
    pub tone: Tone,
    pub at: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Plain,
    Good,
    Bad,
}

impl Status {
    fn quiet() -> Self {
        Self {
            text: String::new(),
            tone: Tone::Plain,
            at: Instant::now() - MESSAGE_TIME,
        }
    }

    pub fn showing(&self) -> bool {
        !self.text.is_empty() && self.at.elapsed() < MESSAGE_TIME
    }
}

/// What the mouse is in the middle of doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drag {
    /// Selecting text, character by character.
    Text,
    /// Selecting text a word at a time, having started with a double click.
    Words {
        anchor_start: usize,
        anchor_end: usize,
    },
    /// A line at a time, having started with a triple click.
    Lines { anchor: usize },
    /// Moving the view with the scroll bar.
    Scrollbar,
    /// Pulling a docked pane's divider to make it wider or narrower.
    DockEdge {
        /// Which pane, by its place in the list. Held rather than looked up
        /// again, because a drag that started on one sidebar must not jump to
        /// another if the panes are renumbered underneath it.
        pane: usize,
    },
    /// Carrying a tab along the row, to put it somewhere else in the order.
    Tab {
        id: DocId,
        /// Where the pointer is, kept so that holding still over an arrow at
        /// the end of the row keeps the tab moving. A drag only reports when
        /// the pointer moves, so without this, "keep going that way" would
        /// mean waggling the mouse.
        at: (u16, u16),
        /// When the last such step happened, so holding it there walks the tab
        /// along at a readable pace rather than firing it off the end.
        stepped: Instant,
    },
    /// Dragging over the text of a hover, to copy part of it.
    Popup,
}

pub struct App {
    pub config: Config,
    pub themes: Themes,
    pub theme: Theme,
    pub keys: Keys,

    docs: Vec<Document>,
    next_doc: u32,
    /// Which buffer was looked at when, so the buffer list is in the order a
    /// person thinks about their buffers rather than the order they opened.
    seen: HashMap<DocId, u64>,
    clock: u64,

    pub panes: Vec<View>,
    pub focus: usize,
    /// Two panes being compared, if you have asked for that. See [`crate::diff`].
    pub diff: Option<crate::diff::Diff>,
    /// Whether panes sit side by side. The other way is one above the other.
    pub side_by_side: bool,

    pub overlay: Overlay,
    pub completion: Option<Completion>,
    pub hover: Option<Popup>,
    pub signature: Option<Popup>,
    pub status: Status,

    pub lsp: Servers,
    /// The plugins that are programs rather than tables.
    pub hosts: Hosts,
    /// A plugin waiting on a box that is on the screen.
    plugin_waiting: Option<Asked>,
    /// Where the caret was last drawn, for anything that opens beside it.
    pub caret: Option<(u16, u16)>,
    tx: Sender<Event>,
    /// Whether the package repositories have been asked what they have yet.
    /// The difference between "there is nothing newer" and "nobody has looked
    /// yet", which are different answers to the same question.
    checked_for_updates: bool,

    /// What was cut or copied. Kept here as well as handed to the terminal,
    /// because the terminal will not hand it back.
    pub clipboard: String,
    /// The last thing searched for, so that "find next" works after the search
    /// box has closed.
    pub last_search: String,
    /// Where files come from for the file picker, and where a project-wide
    /// search searches.
    pub project: PathBuf,
    /// What git says about the project and the files open from it: the
    /// branch, and which lines differ from the last commit.
    pub git: Tracker,
    /// Files found by the walking thread, kept so the picker opens instantly
    /// the second time.
    files: Option<Vec<PathBuf>>,
    files_walking: bool,

    pub quit: bool,
    pub mouse_on: bool,
    /// The whole screen, as last drawn.
    pub screen: Rect,

    /// Where the tabs were drawn, and what is under each: which buffer, and
    /// whether that spot is its close cross. Filled in by the drawing every
    /// frame, so a click is answered by what is actually on the screen rather
    /// than by working out where it ought to be.
    pub tab_hits: Vec<(Rect, DocId, bool)>,
    /// How far along the row of tabs the visible part starts, in columns. More
    /// files than fit across a terminal is the ordinary case once you have
    /// been working for an hour, so the row scrolls.
    pub tab_scroll: u16,
    /// The ‹ › at the ends of the tab row, and where each one scrolls to.
    /// Answered before [`App::tab_hits`], since an arrow sits on top of the
    /// tab it borrowed its column from.
    pub tab_nudges: Vec<(Rect, u16)>,
    /// The same for the status bar, whose parts are buttons.
    pub status_hits: Vec<(Rect, Cmd)>,

    drag: Option<Drag>,
    last_click: Option<(Instant, u16, u16, u8)>,
    /// Where the mouse has been sitting still, and since when.
    resting: Option<(Instant, u16, u16)>,
    /// When to ask for completions, having waited for typing to stop.
    completion_due: Option<Instant>,
    /// When to tell the plugins where the cursor has ended up, and what they
    /// were last told, so that nothing is sent twice.
    selection_due: Option<Instant>,
    selection_told: Option<(DocId, usize)>,
    /// What the language server would do about the problem under the cursor,
    /// fetched before anybody asks so that it can be offered rather than
    /// waited for. This is the whole of "you have not imported that" being
    /// something you can see instead of something you have to go looking for.
    pub fixes: Option<Gathered>,
    /// What every server offered to do about the selection, gathering as they
    /// answer, for the list somebody asked for by hand.
    offer: Option<Gathered>,
    /// A save waiting on the servers' own fixes and on the formatter.
    before_save: Option<BeforeSave>,
    /// Where the fixes were last asked about, so the same question is not
    /// asked twice for a cursor that has not moved.
    fixes_at: Option<(DocId, usize)>,
    /// When to ask, having waited for the cursor to stop. Walking along a line
    /// of red would otherwise be one request per character.
    fixes_due: Option<Instant>,
    /// How many times we have asked about this spot and been turned away. A
    /// server that is still catching up answers "content modified" rather than
    /// answering, and the first ask after a file opens nearly always is.
    fixes_tries: u8,
    /// Whether the first copy of the session has said where copied text goes.
    /// Once, because "copied 12 characters through wl-copy and OSC 52" is
    /// worth reading the first time and noise every time after.
    said_clipboard: bool,
    /// When the diff against the last commit was last worked out.
    git_checked: Instant,
    /// When the open files were last looked at on disk. A `stat` per open file
    /// is cheap, but not so cheap that it is worth doing several times a
    /// second for the sake of noticing a `git checkout` a moment sooner.
    disk_checked: Instant,
    /// A suggestion that was taken while the server was still working out
    /// what it brings with it, to be put in the moment that comes back.
    /// Pressing Tab a fraction before the import has been worked out should
    /// mean waiting a fraction, not going without the import.
    accept_when_resolved: Option<usize>,
    /// Whether what is open has changed since the session was last written
    /// down, and when it last was. Written on a timer rather than on every
    /// change so that opening forty files is not forty writes.
    session_dirty: bool,
    session_written: Instant,

    /// Whether a file was in the middle of changing at the last look, which is
    /// what makes the next one come sooner.
    unsettled: bool,

    /// The install or uninstall that is running, and everything it has said.
    ///
    /// One at a time. Two installs at once would be two `npm install`s
    /// fighting over the same directory, and the second one is nearly always
    /// somebody pressing Enter twice.
    installing: Option<Installing>,
}

impl App {
    pub fn new(config: Config, tx: Sender<Event>) -> Self {
        let mut config = config;
        // The plugins decide what the languages are, so what is switched off
        // has to be known before the language table is built. The settings go
        // in by reference because ids that have been renamed are brought up to
        // date here rather than being half-obeyed forever.
        crate::plugin::init(&mut config.plugins);
        crate::jdk::configure(config.java_home.as_deref());
        lang::init();
        // The commands have to be settled before the keys are read: a plugin
        // can bring one, and a binding for it has to find something to bind to.
        crate::cmd::init();
        let themes = Themes::load();
        let theme = themes
            .by_name(config.theme_name())
            .unwrap_or(crate::theme::FALLBACK);
        let keys = Keys::new(&config.keys);
        let lsp = Servers::new(tx.clone());
        let hosts = Hosts::new(tx.clone());
        let project = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let mut app = Self {
            keys,
            themes,
            theme,
            docs: Vec::new(),
            next_doc: 0,
            seen: HashMap::new(),
            clock: 0,
            panes: Vec::new(),
            focus: 0,
            diff: None,
            side_by_side: true,
            overlay: Overlay::None,
            completion: None,
            checked_for_updates: false,
            hover: None,
            signature: None,
            status: Status::quiet(),
            lsp,
            hosts,
            plugin_waiting: None,
            unsettled: false,
            installing: None,
            caret: None,
            tx,
            clipboard: String::new(),
            last_search: String::new(),
            project,
            git: Tracker::default(),
            files: None,
            files_walking: false,
            quit: false,
            mouse_on: config.mouse(),
            screen: Rect::new(0, 0, 80, 24),
            tab_hits: Vec::new(),
            tab_scroll: 0,
            tab_nudges: Vec::new(),
            status_hits: Vec::new(),
            drag: None,
            last_click: None,
            resting: None,
            completion_due: None,
            selection_due: None,
            selection_told: None,
            fixes: None,
            offer: None,
            before_save: None,
            fixes_at: None,
            fixes_due: None,
            fixes_tries: 0,
            said_clipboard: false,
            git_checked: Instant::now() - GIT_CHECK_EVERY,
            disk_checked: Instant::now(),
            accept_when_resolved: None,
            session_dirty: false,
            session_written: Instant::now(),
            config,
        };
        // Any Python environment chosen before, so a project opens pointing at
        // the same interpreter it was left pointing at.
        for (project, env) in &app.config.python_environments {
            app.lsp
                .environments
                .insert(PathBuf::from(project), PathBuf::from(env));
        }
        app.git.open(&app.project.clone());
        let scratch = app.new_scratch();
        app.panes.push(View::new(scratch, app.config.wrap()));
        app.complain_about_settings();
        app
    }

    /// Anything wrong with the settings, once, at the start. A theme file with
    /// a typo in it or a key bound to a command that does not exist is worth
    /// one line; finding out by noticing something missing is not.
    fn complain_about_settings(&mut self) {
        let mut problems: Vec<String> = Vec::new();
        problems.extend(self.themes.problems.iter().cloned());
        problems.extend(self.keys.problems.iter().cloned());
        problems.extend(lang::all().problems.iter().cloned());
        if let Some(first) = problems.first() {
            let more = problems.len() - 1;
            self.say_bad(match more {
                0 => first.clone(),
                _ => format!("{first} (and {more} more)"),
            });
        }
    }

    // ---- Documents and panes ----

    pub fn docs(&self) -> &[Document] {
        &self.docs
    }

    pub fn doc(&self, id: DocId) -> Option<&Document> {
        self.docs.iter().find(|d| d.id == id)
    }

    pub fn doc_mut(&mut self, id: DocId) -> Option<&mut Document> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    pub fn view(&self) -> &View {
        &self.panes[self.focus.min(self.panes.len() - 1)]
    }

    pub fn view_mut(&mut self) -> &mut View {
        let at = self.focus.min(self.panes.len() - 1);
        &mut self.panes[at]
    }

    /// The document in the focused pane. There is always one.
    pub fn here(&self) -> &Document {
        let id = self.view().doc;
        self.doc(id).expect("a pane always shows a document")
    }

    pub(crate) fn here_mut(&mut self) -> &mut Document {
        let id = self.view().doc;
        self.doc_mut(id).expect("a pane always shows a document")
    }

    /// Both halves at once, which nearly every operation needs and the borrow
    /// checker will not give out piecemeal.
    fn pair(&mut self) -> (&mut Document, &mut View) {
        let at = self.focus.min(self.panes.len() - 1);
        let id = self.panes[at].doc;
        let doc = self
            .docs
            .iter_mut()
            .find(|d| d.id == id)
            .expect("a pane always shows a document");
        (doc, &mut self.panes[at])
    }

    fn new_id(&mut self) -> DocId {
        self.next_doc += 1;
        DocId(self.next_doc)
    }

    fn default_indent(&self) -> Indent {
        if self.config.spaces() {
            Indent::Spaces(self.config.tab_width())
        } else {
            Indent::Tabs
        }
    }

    fn new_scratch(&mut self) -> DocId {
        let id = self.new_id();
        let untitled = self.docs.iter().filter(|d| d.path.is_none()).count();
        let name = match untitled {
            0 => "untitled".to_string(),
            n => format!("untitled {}", n + 1),
        };
        let doc = Document::scratch(id, name, self.default_indent());
        self.docs.push(doc);
        self.touch(id);
        id
    }

    fn touch(&mut self, id: DocId) {
        self.clock += 1;
        self.seen.insert(id, self.clock);
    }

    /// Open a file, or switch to it if it is already open.
    pub fn open_path(&mut self, path: &Path) {
        let path = crate::doc::absolute(path);
        if let Some(existing) = self
            .docs
            .iter()
            .find(|d| d.path.as_deref() == Some(path.as_path()))
            .map(|d| d.id)
        {
            self.show(existing);
            return;
        }
        if path.is_dir() {
            // A directory is not a file, but it is a perfectly good thing to
            // have meant: search inside it.
            self.project = path;
            self.git.open(&self.project.clone());
            self.files = None;
            self.open_files_picker();
            return;
        }
        let id = self.new_id();
        match Document::open(id, &path, self.default_indent()) {
            Ok(doc) => {
                let missing = doc.path.as_ref().is_some_and(|p| !p.exists());
                self.docs.push(doc);
                self.show(id);
                // The empty buffer nobody typed in is not worth keeping once
                // there is a real file to look at.
                self.drop_untouched_scratch(id);
                self.session_changed();
                if missing {
                    self.say(format!("{} is new", short(&path, &self.project)));
                }
                self.lsp_open(id);
            }
            Err(e) => self.say_bad(format!("{e}")),
        }
    }

    /// Show a document in the focused pane, back where that pane last was in
    /// it.
    fn show(&mut self, id: DocId) {
        self.touch(id);
        self.session_changed();
        // Never into a sidebar. Standing in a tree of files and asking to open
        // one of them means open it *in the editor* — a file explorer that
        // replaced itself with the file you clicked would have thrown away the
        // tree to show you one leaf of it. The only thing a docked pane ever
        // shows is the panel it was opened for.
        if self.panes.get(self.focus).is_some_and(|p| p.dock.is_some())
            && self.doc(id).is_none_or(|d| d.panel.is_none())
            && let Some(at) = self.beside_the_docks()
        {
            self.focus = at;
        }
        let at = self.focus.min(self.panes.len() - 1);
        // Somewhere sensible to be if this pane has never shown this file:
        // wherever another pane has it open, and otherwise the top.
        let selections = self
            .panes
            .iter()
            .find(|p| p.doc == id)
            .map(|p| p.sel.clone())
            .unwrap_or_default();
        let len = self.doc(id).map(Document::len_chars).unwrap_or(0);
        let wrap = self.view().wrap;
        self.panes[at].revisit(id, selections, len);
        self.panes[at].wrap = wrap;
        self.dismiss_popups();
        self.scroll_into_view();
        self.lsp_open(id);
    }

    // ---- What was open last time ----

    /// Note that the tabs have moved on, so the session gets written soon.
    fn session_changed(&mut self) {
        self.session_dirty = true;
    }

    /// What is open now, as something that can be written down.
    ///
    /// A buffer with no file behind it is left out: there is nothing to open
    /// again, and remembering the *name* of an empty untitled buffer would
    /// bring back a tab with nothing in it.
    fn session(&self) -> crate::session::Session {
        let mut tabs = Vec::new();
        let mut of_doc: HashMap<DocId, usize> = HashMap::new();
        // The focused pane knows where every file it has shown was; a file it
        // has never shown falls back to wherever another pane had it.
        let here = self.focus.min(self.panes.len().saturating_sub(1));
        for doc in &self.docs {
            let Some(path) = &doc.path else { continue };
            let at = self
                .panes
                .get(here)
                .and_then(|pane| pane.place_in(doc.id))
                .or_else(|| self.panes.iter().find_map(|pane| pane.place_in(doc.id)))
                .unwrap_or(0);
            let (line, column) = doc.point_at_char(at);
            of_doc.insert(doc.id, tabs.len());
            tabs.push(crate::session::Tab {
                path: path.clone(),
                line,
                column,
            });
        }
        let panes: Vec<crate::session::Pane> = self
            .panes
            .iter()
            // A dock shows a plugin's own buffer, which is not a file and not
            // a tab. It comes back by its id below rather than as a pane.
            .filter(|pane| pane.dock.is_none())
            .filter_map(|pane| {
                Some(crate::session::Pane {
                    tab: *of_doc.get(&pane.doc)?,
                    wrap: pane.wrap,
                })
            })
            .collect();
        let docks: Vec<String> = self
            .panes
            .iter()
            .filter(|pane| pane.dock.is_some())
            .filter_map(|pane| self.doc(pane.doc)?.panel.as_ref().map(|p| p.id.clone()))
            .collect();
        crate::session::Session {
            focus: here.min(panes.len().saturating_sub(1)),
            side_by_side: self.side_by_side,
            at: crate::session::now(),
            tabs,
            panes,
            docks
        }
    }

    /// Write down what is open, if it has changed and it has been a moment.
    pub fn remember_session(&mut self, now: bool) {
        // A textfold with nowhere to keep its settings has nowhere to keep a
        // session either — which is also what stops a test run from writing
        // over the tabs of whoever is running it.
        if !self.config.is_stored() || !self.config.restore_session() {
            return;
        }
        if !self.session_dirty && !now {
            return;
        }
        if !now && self.session_written.elapsed() < SESSION_WRITE_EVERY {
            return;
        }
        self.session_dirty = false;
        self.session_written = Instant::now();
        crate::session::save(&self.project.clone(), self.session());
    }

    /// Open again what was open here last time.
    ///
    /// The files go in in the order the row of tabs was in, each landing where
    /// its cursor was, and then the panes are put back. A file that has since
    /// been deleted is skipped rather than opened empty — coming back to a
    /// project should not invent files in it.
    /// `asked` separates somebody pressing the key from textfold trying it on
    /// its own at startup: only the first is worth being told "there was
    /// nothing here", and the second would say it every time you opened the
    /// editor somewhere new.
    pub fn restore_session(&mut self, asked: bool) -> usize {
        let Some(session) = crate::session::load(&self.project.clone()) else {
            if asked {
                self.say("nothing was open here last time");
            }
            return 0;
        };
        self.apply_session(&session, asked)
    }

    /// Open what a session describes. Split from the reading so that a test
    /// can hand one over rather than going through the file every textfold on
    /// this machine shares.
    fn apply_session(&mut self, session: &crate::session::Session, asked: bool) -> usize {
        let already: Vec<PathBuf> = self.docs.iter().filter_map(|d| d.path.clone()).collect();
        let mut opened: Vec<Option<DocId>> = Vec::new();
        for tab in &session.tabs {
            if !tab.path.exists() || already.contains(&tab.path) {
                opened.push(None);
                continue;
            }
            self.open_path(&tab.path);
            let landed = self.view().doc;
            self.go_to(tab.line, tab.column);
            opened.push(Some(landed));
        }
        let count = opened.iter().flatten().count();
        if count == 0 {
            if asked {
                self.say("the files that were open here have gone");
            }
            return 0;
        }

        // The panes, once there is something to put in them. A layout that
        // cannot be rebuilt — because the file one pane had is gone — is not
        // worth half-rebuilding, so it is only restored where every pane has
        // somewhere to point.
        let wanted: Option<Vec<(DocId, bool)>> = session
            .panes
            .iter()
            .map(|pane| {
                let doc = *opened.get(pane.tab)?.as_ref()?;
                Some((doc, pane.wrap))
            })
            .collect();
        if let Some(wanted) = wanted.filter(|w| w.len() > 1 && w.len() <= 4) {
            self.side_by_side = session.side_by_side;
            while self.panes.len() > 1 {
                self.panes.pop();
            }
            self.focus = 0;
            for (at, (doc, wrap)) in wanted.iter().enumerate() {
                if at > 0 {
                    self.split();
                }
                self.focus = at;
                self.show(*doc);
                self.panes[at].wrap = *wrap;
            }
            self.focus = session.focus.min(self.panes.len() - 1);
        }
        // And the sidebars, last, so that restoring them does not renumber
        // the panes the layout above just built. Opening one starts the
        // plugin behind it, which is what a panel command does anywhere —
        // asking for the thing is what makes it run.
        //
        // Which pane had the focus is remembered as its place among the panes
        // showing a file, because inserting a sidebar on the left renumbers
        // everything after it.
        let focused = self
            .panes
            .iter()
            .take(self.focus)
            .filter(|p| p.dock.is_none())
            .count();
        for id in &session.docks {
            let Some(command) = crate::plugin::active()
                .flat_map(|p| &p.commands)
                .find(|c| &c.id == id && c.opens_panel && c.dock.is_some())
            else {
                continue;
            };
            self.run_plugin_command(command);
        }
        // Opening a sidebar takes the focus, which is right when you have just
        // asked for one and wrong when it is only being put back where it was.
        // The pane that had it gets it back.
        if !session.docks.is_empty()
            && let Some(at) = self
                .panes
                .iter()
                .enumerate()
                .filter(|(_, p)| p.dock.is_none())
                .map(|(at, _)| at)
                .nth(focused)
        {
            self.focus = at;
        }
        self.scroll_into_view();
        self.session_dirty = true;
        count
    }

    /// Close the empty untouched buffer textfold starts with, once there is
    /// something real open.
    fn drop_untouched_scratch(&mut self, keep: DocId) {
        let disposable: Vec<DocId> = self
            .docs
            .iter()
            .filter(|d| {
                d.id != keep
                    && d.path.is_none()
                    && d.len_chars() == 0
                    && !d.is_modified()
                    && !self.panes.iter().any(|p| p.doc == d.id)
            })
            .map(|d| d.id)
            .collect();
        for id in disposable {
            self.docs.retain(|d| d.id != id);
            self.seen.remove(&id);
        }
    }

    /// Close a buffer, having already decided that it is all right to.
    fn close_doc(&mut self, id: DocId) {
        if let Some(path) = self.doc(id).and_then(|d| d.path.clone()) {
            self.lsp.did_close(&path);
            self.hosts.closed(&path);
        }
        // A panel that has been closed is one the plugin can stop keeping up
        // to date, and one it should be told about before it sends the next
        // set of lines into nothing.
        if let Some((plugin, panel)) = self
            .doc(id)
            .and_then(|d| d.panel.as_ref())
            .map(|p| (p.plugin.clone(), p.id.clone()))
        {
            self.tell_panel(&plugin, "panel/closed", json!({ "panel": panel }));
        }
        self.docs.retain(|d| d.id != id);
        self.seen.remove(&id);
        self.git.forget(id);
        self.session_changed();
        if self.docs.is_empty() {
            let fresh = self.new_scratch();
            for pane in &mut self.panes {
                pane.show(fresh, Selections::default());
            }
        } else {
            // Panes showing it move to whatever was looked at most recently.
            let fallback = self.most_recent().unwrap_or(self.docs[0].id);
            for pane in &mut self.panes {
                if pane.doc == id {
                    pane.show(fallback, Selections::default());
                }
            }
        }
        // After the panes have moved off it, not before: pointing a pane
        // somewhere else is what puts away where it was, and putting away
        // where it was in a buffer that has gone is what we are avoiding.
        for pane in &mut self.panes {
            pane.forget(id);
        }
    }

    fn most_recent(&self) -> Option<DocId> {
        self.docs
            .iter()
            .map(|d| d.id)
            .max_by_key(|id| self.seen.get(id).copied().unwrap_or(0))
    }

    // ---- Saying things ----

    pub fn say(&mut self, text: impl Into<String>) {
        self.status = Status {
            text: text.into(),
            tone: Tone::Plain,
            at: Instant::now(),
        };
    }

    pub fn say_good(&mut self, text: impl Into<String>) {
        self.status = Status {
            text: text.into(),
            tone: Tone::Good,
            at: Instant::now(),
        };
    }

    pub fn say_bad(&mut self, text: impl Into<String>) {
        self.status = Status {
            text: text.into(),
            tone: Tone::Bad,
            at: Instant::now(),
        };
    }

    // ---- The loop ----

    /// How long to wait for the next event before doing the rounds anyway.
    /// Short while something is on a timer, long while nothing is.
    pub fn idle(&self) -> Duration {
        if self.completion_due.is_some()
            || self.selection_due.is_some()
            || self.fixes_due.is_some()
            || self.resting.is_some()
            || self.status.showing()
            || self.docs.iter().any(|d| d.wants_recolour())
            || matches!(self.drag, Some(Drag::Tab { .. }))
        {
            Duration::from_millis(60)
        } else {
            Duration::from_millis(400)
        }
    }

    /// Things that happen because time passed rather than because anything
    /// was pressed.
    pub fn tick(&mut self) {
        if let Some(due) = self.completion_due
            && due <= Instant::now()
        {
            self.completion_due = None;
            self.ask_for_completions(None, false);
        }
        if let Some(due) = self.selection_due
            && due <= Instant::now()
        {
            self.selection_due = None;
            self.tell_plugins_where_the_cursor_is();
        }
        if let Some((since, column, row)) = self.resting
            && since.elapsed() >= HOVER_DELAY
        {
            self.resting = None;
            self.hover_at_screen(column, row);
        }
        self.check_fixes();
        self.check_before_save();
        self.remember_session(false);
        self.check_colours();
        self.check_diff();
        self.check_dragged_tab();
        // Sooner while something is moving, so that the extra look a settling
        // file costs is a quarter of a second rather than another whole cycle.
        let due = match self.unsettled {
            true => SETTLE_CHECK_EVERY,
            false => DISK_CHECK_EVERY,
        };
        if self.disk_checked.elapsed() >= due {
            self.disk_checked = Instant::now();
            self.check_disk();
            self.git.poll_head();
        }
        self.check_git();
    }

    /// A tab held over one of the arrows at the end of the row keeps moving
    /// that way.
    ///
    /// Without this, a row with more tabs than fit could only be reordered as
    /// far as the edge of the screen: the tab you are carrying is the one the
    /// drawing keeps in view, so its neighbour on the far side is off the
    /// screen and there is nothing to compare the pointer against. Holding it
    /// over the arrow says "keep going", and every step scrolls the row along
    /// to follow, because the tab being carried is also the current one.
    ///
    /// On a timer rather than on pointer movement: a drag is only reported
    /// when the mouse moves, and "hold it here" should not mean "waggle it
    /// here".
    fn check_dragged_tab(&mut self) {
        let Some(Drag::Tab { id, at, stepped }) = self.drag else {
            return;
        };
        if stepped.elapsed() < TAB_STEP_EVERY {
            return;
        }
        let Some((_, to)) = self
            .tab_nudges
            .iter()
            .find(|(area, _)| hits(*area, at.0, at.1))
        else {
            return;
        };
        // Which arrow it is, by which way it would scroll from here.
        let step: isize = if *to < self.tab_scroll { -1 } else { 1 };
        let now = self.docs.iter().position(|d| d.id == id).unwrap_or(0) as isize;
        if now + step < 0 || now + step >= self.docs.len() as isize {
            return;
        }
        self.move_tab(id, (now + step) as usize);
        if let Some(Drag::Tab { stepped, .. }) = &mut self.drag {
            *stepped = Instant::now();
        }
    }

    /// Colour again anything a parse gave up on.
    ///
    /// A parse that runs out of time usually says the machine was busy, not
    /// that the file is unparseable — a language server waking up and taking
    /// every core for a second is enough to do it. Left alone that turns a
    /// busy moment into a file that stays grey until you close and reopen it,
    /// which is exactly what it looks like when a language server has just
    /// started and filled the screen with underlines. So the attempt is made
    /// again once things are quiet, with a budget suited to not being in a
    /// hurry, and only a file that fails that several times over is written
    /// off.
    ///
    /// One per pass. Two large files that both want it can wait a turn each
    /// rather than making one turn of the loop take twice as long.
    fn check_colours(&mut self) {
        let Some(doc) = self.docs.iter_mut().find(|d| d.wants_recolour()) else {
            return;
        };
        doc.recolour();
    }

    /// Ask what could be done about the problem under the cursor, once per
    /// place the cursor comes to rest on one.
    ///
    /// Only where there is a diagnostic: an editor that asked a language
    /// server for advice about every character you moved past would be an
    /// editor that spent its life waiting for one.
    fn check_fixes(&mut self) {
        let id = self.view().doc;
        let at = self.view().cursor();
        if self.fixes_at != Some((id, at)) {
            // The cursor has moved, so last time's answer is about somewhere
            // else. Ask again once it stops.
            self.fixes = None;
            self.fixes_at = Some((id, at));
            self.fixes_tries = 0;
            let on_a_problem = self
                .doc(id)
                .is_some_and(|d| d.diagnostics.iter().any(|p| p.range.contains(at)));
            self.fixes_due = on_a_problem.then(|| Instant::now() + FIX_DELAY);
            return;
        }
        let Some(due) = self.fixes_due else { return };
        if due > Instant::now() {
            return;
        }
        self.fixes_due = None;
        let range = Range::point(at);
        let App { docs, lsp, .. } = self;
        let Some(doc) = docs.iter().find(|d| d.id == id) else {
            return;
        };
        // Every server that does code actions, not the first one: `ruff` knows
        // how to take an unused import out and `pyright` does not, and which
        // of the two answers first is a race nobody should be running.
        let asked = lsp.quick_fixes(doc, range);
        if !asked.is_empty() {
            self.fixes = Some(Gathered::new(id, at, asked));
        }
    }

    /// Ask again after being turned away, up to a point.
    ///
    /// A few times rather than for ever: a server that will not answer this
    /// question is a server we should stop asking, and the cost of being wrong
    /// about that is one code action nobody was told about.
    fn retry_fixes(&mut self, doc: DocId, at: usize) {
        if self.fixes_at != Some((doc, at)) || self.fixes_tries >= FIX_TRIES {
            return;
        }
        self.fixes_tries += 1;
        self.fixes_due = Some(Instant::now() + FIX_DELAY * 2);
    }

    fn take_quick_fixes(&mut self, server: ServerId, doc: DocId, at: usize, value: Value) {
        // Anything that came back about somewhere the cursor has since left is
        // an answer to a question nobody is asking any more.
        if self.fixes_at != Some((doc, at)) {
            return;
        }
        let Some(gathered) = self.fixes.as_mut().filter(|g| g.doc == doc && g.at == at) else {
            return;
        };
        gathered.take(server, value);
        if gathered.is_empty() && gathered.settled() {
            // Nobody had anything. Better to have nothing waiting than an
            // empty list the status bar has to describe.
            self.fixes = None;
        }
    }

    /// Do the obvious thing about the problem under the cursor.
    ///
    /// One fix means one keystroke: the import goes in and you carry on
    /// typing, which is the whole point and the reason nobody should have to
    /// scroll to the top of a file to add a line they already know the text
    /// of. Several means a list, because there is a choice to make.
    fn fix_it(&mut self) {
        let Some(fixes) = self.fixes.as_ref().filter(|g| !g.is_empty()) else {
            // Nothing waiting: it may simply not have come back yet, or there
            // may be nothing wrong here at all.
            let on_a_problem = {
                let at = self.view().cursor();
                self.here().diagnostics.iter().any(|d| d.range.contains(at))
            };
            return self.say(if on_a_problem {
                "no fix offered for this"
            } else {
                "nothing wrong here to fix"
            });
        };
        let offered: Vec<(ServerId, Value)> = fixes
            .actions()
            .into_iter()
            .map(|(id, action)| (id, action.clone()))
            .collect();
        if let [(server, action)] = offered.as_slice() {
            let (server, action) = (*server, action.clone());
            let title = action
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("fixed it")
                .to_string();
            self.fixes = None;
            self.do_code_action(server, action);
            return self.say_good(title);
        }
        // Several, from one server or from two. There is a choice to make, so
        // it is made in a list rather than guessed at.
        self.show_actions(offered);
    }

    /// Work out which lines differ from the last commit, for whatever is on
    /// the screen.
    ///
    /// Only the panes, rather than every open buffer: a diff is cheap but not
    /// free, and a mark in a gutter nobody is looking at is worth nothing. The
    /// buffers behind the other tabs catch up the moment you switch to them.
    fn check_git(&mut self) {
        if !self.git.watching() {
            return;
        }
        // A diff of a large file is under a millisecond, but so is a keystroke,
        // and there is no reason to spend one on the other. Marks that lag
        // typing by a tenth of a second are marks nobody notices lagging.
        if self.git_checked.elapsed() < GIT_CHECK_EVERY {
            return;
        }
        self.git_checked = Instant::now();
        self.refresh_git();
    }

    /// The work `check_git` decides how often to do.
    fn refresh_git(&mut self) {
        let showing: Vec<DocId> = self.panes.iter().map(|p| p.doc).collect();
        for id in showing {
            let App { docs, git, .. } = self;
            if let Some(doc) = docs.iter().find(|d| d.id == id) {
                git.refresh(doc);
            }
        }
    }

    /// Notice files written by something that is not this editor.
    ///
    /// A build that reformats, a `git checkout`, the same file open somewhere
    /// else. A buffer with nothing unsaved in it is simply read again, because
    /// there is nothing to lose and looking at text that is no longer in the
    /// file is worse than useless. A buffer with unsaved changes is left
    /// exactly alone and marked, because only the person editing it can say
    /// which side wins.
    fn check_disk(&mut self) {
        let ids: Vec<DocId> = self
            .docs
            .iter()
            .filter(|d| d.path.is_some())
            .map(|d| d.id)
            .collect();
        let auto = self.config.reload_on_change();
        self.unsettled = false;
        let mut reloaded: Vec<String> = Vec::new();
        let mut clashed: Vec<String> = Vec::new();
        let mut waiting: Vec<String> = Vec::new();
        let mut gone: Vec<String> = Vec::new();

        for id in ids {
            let Some(doc) = self.doc_mut(id) else {
                continue;
            };
            let now = doc.check_disk();
            if now == OnDisk::Same {
                continue;
            }
            // A file that is *still* changing has no settled contents to read.
            // Something is part way through writing it, and what a read gets
            // is a snapshot of a file mid-write — very often cut in the middle
            // of a character, which is how a buffer fills up with rubbish that
            // then never goes away. So it is left alone and looked at again
            // shortly, which is also what keeps a log being appended to from
            // replacing your buffer several times a second.
            if now == OnDisk::Changed && !doc.has_settled() {
                self.unsettled = true;
                continue;
            }
            // Whether this is the same state we have already dealt with.
            // Compared by what is actually on disk rather than by "is it
            // changed", which cannot tell a change we mentioned a second ago
            // from a second change since.
            if !doc.is_news() {
                continue;
            }
            doc.noted();
            let name = doc.name.clone();
            let modified = doc.is_modified();
            match now {
                OnDisk::Gone => gone.push(name),
                // Only the person editing it can say which side wins, so it is
                // marked and left. Nothing is read.
                OnDisk::Changed if modified => clashed.push(name),
                OnDisk::Changed if !auto => waiting.push(name),
                // Nobody asked for this one, so it is only taken where it can
                // be taken without guessing — see `take_from_disk`. What it
                // would not take is left marked as changed and mentioned, so
                // `reload` is still there to do it deliberately.
                OnDisk::Changed => match self.take_from_disk(id, Reread::OnATimer) {
                    Ok(_) => reloaded.push(name),
                    Err(_) => waiting.push(name),
                },
                OnDisk::Same => {}
            }
        }

        // One line, whatever happened, and the worst of it first: an unsaved
        // buffer that no longer matches its file is the thing you have to do
        // something about.
        if let Some(said) = disk_news(&clashed, &gone) {
            self.say_bad(said);
        } else if !waiting.is_empty() {
            self.say(match waiting.len() {
                1 => format!("{} changed on disk — reload to take it", waiting[0]),
                n => format!("{n} files changed on disk — reload to take them"),
            });
        } else if !reloaded.is_empty() {
            self.say(match reloaded.len() {
                1 => format!("{} changed on disk — read again", reloaded[0]),
                n => format!("{n} files changed on disk — read again"),
            });
        }
    }

    pub fn handle(&mut self, event: Event) {
        self.handled(event);
        // A box a plugin put up may have been taken away by whatever that was
        // — Escape, a click outside, a command that opened something else.
        // Swept here rather than at each of the dozen places an overlay is
        // dismissed from, so that none of them has to remember there was a
        // plugin behind it.
        self.sweep_plugin_question();
        self.notice_the_cursor_moved();
    }

    fn handled(&mut self, event: Event) {
        match event {
            Event::Term(TermEvent::Key(key)) => self.on_key(key),
            Event::Term(TermEvent::Mouse(mouse)) => self.on_mouse(mouse),
            Event::Term(TermEvent::Paste(text)) => self.on_paste(&text),
            Event::Term(TermEvent::Resize(width, height)) => {
                self.screen = Rect::new(0, 0, width, height);
            }
            Event::Term(_) => {}
            Event::Lsp(id, message) => self.on_lsp(id, message),
            Event::Plugin(id, message) => self.on_plugin(id, message),
            Event::Package(progress) => self.on_package(*progress),
            Event::Refreshed(problems) => self.refreshed(problems),
            Event::PluginRan(ran) => {
                let ran = *ran;
                if let Some(host) = self.hosts.get_mut(ran.host) {
                    host.answer(
                        ran.request,
                        json!({
                            "ok": ran.ok, "code": ran.code,
                            "out": ran.out, "err": ran.err,
                        }),
                    );
                }
            }
            Event::Files(files) => {
                self.files_walking = false;
                // A walk that found exactly what the last one did leaves the
                // box alone. Rebuilding the rows would be the same rows, and
                // it would put the list back to the top under somebody who is
                // in the middle of using it.
                let same = self.files.as_ref().is_some_and(|had| *had == files);
                if !same
                    && let Overlay::Picker(picker) = &mut self.overlay
                    && picker.kind == Kind::Files
                {
                    let rows = file_rows(&files, &self.project);
                    picker.set_rows(rows);
                }
                self.files = Some(files);
            }
            Event::Tool(done) => self.on_tool(*done),
            Event::Found(query, rows) => {
                if let Overlay::Picker(picker) = &mut self.overlay
                    && picker.kind == Kind::Grep
                    && picker.query.trim() == query
                {
                    picker.set_rows(rows);
                }
            }
        }
    }

    // ---- Keys ----

    fn on_key(&mut self, event: KeyEvent) {
        // A terminal with the extended protocol reports releases too, and a
        // release is not a press.
        if event.kind == KeyEventKind::Release {
            return;
        }
        let key = Key::from_event(event);

        // One key that means the same thing whatever is on top of the editor.
        // It is not really for people — they have Ctrl-P — but for whatever is
        // driving the terminal: a file manager in the pane next door, sshman
        // sending a file over. None of them can see what is on the screen, so
        // a key they would have to get out of a box first is a key that types
        // a path into somebody's file.
        //
        // Only where it would not otherwise type a character: somebody who has
        // bound this to a plain letter meant that letter in the text, not in
        // the middle of a search box.
        if key.as_typed().is_none() && self.keys.lookup(key) == Some(Cmd::OPEN_PATH) {
            return self.open_prompt(PromptKind::OpenPath);
        }

        // A hover you have asked to read takes the keys that read it, and
        // hands back everything else — along with itself, since a key that
        // means something in the text is a key that has finished with the box.
        if self.hover.as_ref().is_some_and(|h| h.focused) && self.hover_key(key) {
            return;
        }

        match &mut self.overlay {
            Overlay::Picker(_) => return self.picker_key(key),
            Overlay::Prompt(_) => return self.prompt_key(key),
            Overlay::Confirm(_) => return self.confirm_key(key),
            Overlay::Help(_) => return self.help_key(key),
            Overlay::Menu(_) => return self.menu_key(key),
            Overlay::None => {}
        }

        // The completion list gets first refusal on the handful of keys that
        // steer it, and nothing else.
        if self.completion.is_some() && self.completion_key(key) {
            return;
        }

        // An offer on the screen gets the handful of keys that steer it, the
        // way the completion list does, and nothing else.
        if self.hint_key(key) {
            return;
        }

        // A panel is a plugin's own buffer, and gets the keys that would
        // otherwise have *changed the text* — because a panel's text is not
        // yours to change, so those keys are going spare. Everything else
        // still does exactly what it always does: Ctrl-P is still the palette
        // and the arrows still move, so a plugin cannot take a key anybody
        // knows. The same rule as `Keys::suggest`, applied to a buffer.
        if self.panel_wants(key) {
            // Enter on something the plugin marked as doing something does
            // that; otherwise the key goes on to the plugin like any other.
            if key.code == KeyCode::Enter && self.panel_action_at(self.view().cursor()) {
                return;
            }
            self.send_panel_key(key);
            return;
        }
        if let Some(cmd) = self.keys.lookup(key) {
            self.run(cmd);
            return;
        }
        if let Some(c) = key.as_typed() {
            self.type_char(c);
        }
    }

    fn type_char(&mut self, c: char) {
        if self.refuse_if_read_only() {
            return;
        }
        let auto_pairs = self.config.auto_pairs();
        let (doc, view) = self.pair();
        let edits = edit::insert_char(doc, view, c, auto_pairs);
        self.after_edit(edits);

        // Typing keeps the completion list, narrowing it and asking again
        // where the server had more to say; anything else closes it.
        self.refresh_completion();

        if self.config.auto_completion() && self.lsp.can(self.here(), "completionProvider") {
            let triggers = self
                .lsp
                .who_can(self.here(), "completionProvider")
                .and_then(|id| self.lsp.get(id))
                .map(|s| s.completion_triggers())
                .unwrap_or_default();
            if triggers.contains(&c) {
                self.completion = None;
                self.ask_for_completions(Some(c), false);
            } else if self.completion.is_none() && (c.is_alphanumeric() || c == '_') {
                // Wait for typing to stop, so a word is one request.
                self.completion_due = Some(Instant::now() + COMPLETION_DELAY);
            }
        }

        let signature_triggers = self
            .lsp
            .who_can(self.here(), "signatureHelpProvider")
            .and_then(|id| self.lsp.get(id))
            .map(|s| s.signature_triggers())
            .unwrap_or_default();
        if signature_triggers.contains(&c) {
            let at = self.view().cursor();
            let (doc, lsp) = self.doc_and_lsp();
            lsp.signature(doc, at);
        } else if c == ')' || c == '\n' {
            self.signature = None;
        }
    }

    /// What has been typed since the completion list arrived, or `None` if the
    /// cursor has wandered out of the word it was completing.
    fn typed_since_completion(&self) -> Option<String> {
        let completion = self.completion.as_ref()?;
        let view = self.view();
        if view.doc != completion.doc || view.sel.len() != 1 {
            return None;
        }
        let at = view.cursor();
        if at < completion.start {
            return None;
        }
        let doc = self.doc(completion.doc)?;
        Some(doc.slice(Range::new(completion.start, at)))
    }

    fn refuse_if_read_only(&mut self) -> bool {
        if self.here().read_only {
            let name = self.here().name.clone();
            self.say_bad(format!("{name} is read-only"));
            return true;
        }
        false
    }

    /// Everything to do after the text changed.
    fn after_edit(&mut self, edits: Vec<crate::doc::AppliedEdit>) {
        let id = self.view().doc;
        let focus = self.focus.min(self.panes.len() - 1);
        self.after_edit_to(id, edits, Some(focus));
    }

    /// Everything that has to happen after a document's text changes, for a
    /// document that is not necessarily the one being looked at.
    ///
    /// `absorbed` names the pane that has already taken the edits in — the one
    /// that made them. A change from somewhere other than a keystroke, like a
    /// file being re-read underneath us, has no such pane and passes `None`.
    fn after_edit_to(
        &mut self,
        id: DocId,
        edits: Vec<crate::doc::AppliedEdit>,
        absorbed: Option<usize>,
    ) {
        if edits.is_empty() {
            return;
        }
        let len = self.doc(id).map(Document::len_chars).unwrap_or(0);

        // Every other pane looking at this document has cursors that were
        // pointing at text that has moved.
        for (at, pane) in self.panes.iter_mut().enumerate() {
            if Some(at) != absorbed && pane.doc == id {
                pane.absorb(&edits, len);
            }
        }
        // So do the diagnostics: a warning about line ten belongs on the line
        // that text is on now, not on whatever ended up there.
        if let Some(doc) = self.doc_mut(id) {
            for diagnostic in &mut doc.diagnostics {
                let mut range = diagnostic.range;
                for edit in &edits {
                    range = Range::new(edit.map(range.anchor), edit.map(range.head));
                }
                diagnostic.range = range.clamped(len);
            }
        }

        let App { docs, lsp, hosts, .. } = self;
        if let Some(doc) = docs.iter().find(|d| d.id == id) {
            lsp.did_change(doc, &edits);
            hosts.changed(doc, &edits);
        }
        if let Some(doc) = self.doc_mut(id) {
            doc.take_pending();
        }
        // An offer was about the text as it was. The text has moved on, so the
        // offer is about something that is no longer there — the same rule an
        // edit computed against an old version gets, arrived at from the other
        // side.
        if self.doc(id).is_some_and(|d| d.hint.is_some()) {
            let plugin = self.doc(id).and_then(|d| d.hint.as_ref()).map(|h| h.plugin.clone());
            if let Some(doc) = self.doc_mut(id) {
                doc.hint = None;
            }
            if let Some(plugin) = plugin {
                self.tell_panel(&plugin, "hint/dropped", json!({ "why": "the text changed" }));
            }
        }
        self.hover = None;
        if self.view().doc == id {
            self.scroll_into_view();
        }
    }

    fn scroll_into_view(&mut self) {
        let tab_width = self.config.tab_width();
        let pad = self.config.scrolloff();
        let at = self.focus.min(self.panes.len() - 1);
        let id = self.panes[at].doc;
        let Some(index) = self.docs.iter().position(|d| d.id == id) else {
            return;
        };
        let (docs, panes) = (&self.docs, &mut self.panes);
        view::scroll_to_cursor(&mut panes[at], &docs[index], tab_width, pad);
    }

    // ---- Running commands ----

    /// Do what a command says.
    ///
    /// The command itself is a number; what it means comes out of the
    /// registry, so this is the same three lines whether the row was written
    /// in the table below or brought along by a plugin.
    pub fn run(&mut self, cmd: Cmd) {
        let behaviour = cmd.behaviour();
        if cmd.writes() && self.refuse_if_read_only() {
            return;
        }
        if !behaviour.joins() {
            self.here_mut().close_revision();
        }
        // Backspace is the other key that leaves you still completing a word,
        // and it narrows the list rather than closing it — which it cannot do
        // if the list has already gone.
        if cmd != Cmd::COMPLETION && cmd != Cmd::DELETE_BACKWARD {
            self.completion = None;
            self.completion_due = None;
            self.accept_when_resolved = None;
        }

        match cmd.run() {
            cmd::Run::Built(run) => run(self),
            cmd::Run::Tool(tool) => self.run_tool(tool),
            cmd::Run::Plugin(command) => self.run_plugin_command(command),
            cmd::Run::Gone => self.say(format!(
                "{} came from a plugin that is switched off",
                cmd.name()
            )),
        }
    }

    fn select_all(&mut self) {
        let (doc, view) = self.pair();
        edit::select_all(doc, view);
    }

    fn select_line(&mut self) {
        let (doc, view) = self.pair();
        edit::select_line(doc, view);
        self.scroll_into_view();
    }

    fn select_word(&mut self) {
        let (doc, view) = self.pair();
        edit::select_word(doc, view);
    }

    fn add_cursor_above(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        edit::add_cursor_vertically(doc, view, tab_width, false);
        self.scroll_into_view();
    }

    fn add_cursor_below(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        edit::add_cursor_vertically(doc, view, tab_width, true);
        self.scroll_into_view();
    }

    fn add_cursor_at_next_match(&mut self) {
        let (doc, view) = self.pair();
        let found = edit::add_cursor_next_match(doc, view);
        if !found {
            self.say("no more of those");
        } else {
            self.scroll_into_view();
        }
    }

    fn select_every_match(&mut self) {
        let (doc, view) = self.pair();
        let count = edit::select_all_matches(doc, view);
        if count > 1 {
            self.say(format!("{count} cursors"));
        }
    }

    fn cursors_to_line_ends(&mut self) {
        let (doc, view) = self.pair();
        edit::cursors_to_line_ends(doc, view);
    }

    fn collapse_cursors(&mut self) {
        self.view_mut().sel.collapse_to_primary();
        self.scroll_into_view();
    }

    fn insert_newline(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        let mut edits = edit::newline(doc, view, tab_width);
        edits.extend(edit::newline_closing(doc, view, tab_width));
        self.after_edit(edits);
        self.completion = None;
    }

    fn delete_backward(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        let edits = edit::delete_backward(doc, view, tab_width);
        self.after_edit(edits);
        self.refresh_completion();
    }

    fn delete_forward(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_forward(doc, view);
        self.after_edit(edits);
    }

    fn delete_word_backward(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_word_backward(doc, view);
        self.after_edit(edits);
    }

    fn delete_word_forward(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_word_forward(doc, view);
        self.after_edit(edits);
    }

    fn delete_to_line_start(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_to_line_start(doc, view);
        self.after_edit(edits);
    }

    fn delete_to_line_end(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_to_line_end(doc, view);
        self.after_edit(edits);
    }

    fn delete_line(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_line(doc, view);
        self.after_edit(edits);
    }

    fn duplicate_line(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::duplicate_line(doc, view);
        self.after_edit(edits);
    }

    fn move_line_up(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::move_lines(doc, view, false);
        self.after_edit(edits);
    }

    fn move_line_down(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::move_lines(doc, view, true);
        self.after_edit(edits);
    }

    fn join_lines(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::join_lines(doc, view);
        self.after_edit(edits);
    }

    fn toggle_comment(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        match edit::toggle_comment(doc, view, tab_width) {
            Some(edits) => self.after_edit(edits),
            None => {
                let name = lang::get(self.here().language).name.clone();
                self.say(format!("textfold does not know how to comment {name}"));
            }
        }
    }

    fn upper_case(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::change_case(doc, view, true);
        self.after_edit(edits);
    }

    fn lower_case(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::change_case(doc, view, false);
        self.after_edit(edits);
    }

    fn paste(&mut self) {
        let text = self.system_clipboard();
        if text.is_empty() {
            self.say("nothing to paste");
        } else {
            let (doc, view) = self.pair();
            let edits = edit::insert_atomic(doc, view, &text);
            self.after_edit(edits);
        }
    }

    fn new_buffer(&mut self) {
        let id = self.new_scratch();
        self.show(id);
    }

    fn find_word_under_cursor(&mut self) {
        let at = self.view().cursor();
        match text::word_text_at(&self.here().rope, at) {
            Some(word) => {
                self.last_search = word;
                self.find_step(1);
            }
            None => self.say("the cursor is not on a word"),
        }
    }

    fn ask_signature(&mut self) {
        let at = self.view().cursor();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.signature(doc, at).is_none() {
            self.say("no language server here");
        }
    }

    fn restart_servers(&mut self) {
        self.lsp.restart();
        let docs: Vec<DocId> = self.docs.iter().map(|d| d.id).collect();
        for id in docs {
            self.lsp_open(id);
        }
        self.say("starting the language servers again");
    }

    fn swap_split_direction(&mut self) {
        self.side_by_side = !self.side_by_side;
    }

    fn toggle_wrap(&mut self) {
        let wrap = !self.view().wrap;
        self.view_mut().wrap = wrap;
        self.view_mut().left = 0;
        self.scroll_into_view();
        self.say(if wrap {
            "long lines fold"
        } else {
            "long lines run off the side"
        });
    }

    fn bring_back_session(&mut self) {
        let count = self.restore_session(true);
        if count > 0 {
            self.say_good(format!(
                "brought back {count} {}",
                plural("file", count)
            ));
        }
    }

    fn motion(&mut self, motion: Motion, extend: bool) {
        let tab_width = self.config.tab_width();
        let far = matches!(motion, Motion::DocStart | Motion::DocEnd);
        if far {
            self.view_mut().mark_jump();
        }
        let (doc, view) = self.pair();
        edit::move_cursors(doc, view, motion, extend, tab_width);
        self.dismiss_popups();
        self.scroll_into_view();
    }

    fn dismiss_popups(&mut self) {
        self.hover = None;
        self.signature = None;
    }

    /// Keep the open list of suggestions honest after an edit that changed
    /// the word being completed.
    ///
    /// Narrowing what is already on the screen answers the keystroke without
    /// a round trip, and for a list the server called complete that is the
    /// whole of it. For one it called partial it is not: the name you are
    /// typing towards may not be in the list at all — a server asked about
    /// `Ha` offers a few of the unimported names it could reach and says
    /// there are more — so the question is asked again as well, with what is
    /// already there standing in until the answer arrives.
    fn refresh_completion(&mut self) {
        self.accept_when_resolved = None;
        let typed = self.typed_since_completion();
        let Some(completion) = &mut self.completion else {
            return;
        };
        let incomplete = completion.incomplete;
        match typed {
            Some(prefix) => {
                completion.narrow(&prefix);
                if completion.is_empty() && !incomplete {
                    self.completion = None;
                }
            }
            // The cursor has left the word this list was about.
            None => {
                self.completion = None;
                return;
            }
        }
        if incomplete {
            self.completion_due = Some(Instant::now() + COMPLETION_DELAY);
            // An empty list is a box with nothing in it. Better to take it
            // off the screen and let the answer put it back.
            if self.completion.as_ref().is_some_and(Completion::is_empty) {
                self.completion = None;
            }
        }
        self.resolve_selected();
    }

    fn scroll(&mut self, rows: isize) {
        let tab_width = self.config.tab_width();
        let at = self.focus.min(self.panes.len() - 1);
        let id = self.panes[at].doc;
        let Some(index) = self.docs.iter().position(|d| d.id == id) else {
            return;
        };
        let (docs, panes) = (&self.docs, &mut self.panes);
        view::scroll_by(&mut panes[at], &docs[index], tab_width, rows);
    }

    fn centre(&mut self) {
        let line = text::line_of(&self.here().rope, self.view().cursor());
        let height = self.view().height();
        let view = self.view_mut();
        view.top = line.saturating_sub(height / 2);
        view.top_row = 0;
    }

    fn on_tab(&mut self, out: bool) {
        // Tab with a completion list open takes the suggestion, which is what
        // it does everywhere and why it is bound to indent rather than the
        // other way round.
        if self.completion.is_some() && !out {
            self.accept_completion();
            return;
        }
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        let edits = edit::indent(doc, view, tab_width, out);
        self.after_edit(edits);
    }

    fn undo(&mut self, backwards: bool) {
        let (doc, view) = self.pair();
        let done = if backwards { doc.undo() } else { doc.redo() };
        let Some((edits, selections)) = done else {
            self.say(if backwards {
                "nothing to undo"
            } else {
                "nothing to redo"
            });
            return;
        };
        view.sel = selections;
        view.sel.clamp(doc.len_chars());
        self.after_edit(edits);
        self.scroll_into_view();
    }

    /// What Ctrl-V should put in.
    ///
    /// Whatever is on the desktop's clipboard, where that can be asked for,
    /// so that a copy made in a browser pastes into the editor without going
    /// through the terminal's own paste key. Where it cannot, what Ctrl-C last
    /// took, which is the most this can honestly know.
    fn system_clipboard(&mut self) -> String {
        if let Some(text) = crate::term::from_clipboard() {
            self.clipboard = text;
        }
        self.clipboard.clone()
    }

    fn copy(&mut self, cut: bool) {
        // Copying with nothing selected takes the line, which is what people
        // mean by Ctrl-C on a line they are standing on.
        let took_lines = self.view().sel.ranges().iter().all(Range::is_empty);
        if took_lines {
            let (doc, view) = self.pair();
            edit::select_line(doc, view);
        }
        let doc = self.here();
        let text: Vec<String> = self
            .view()
            .sel
            .ranges()
            .iter()
            .map(|range| doc.slice(*range))
            .collect();
        self.clipboard = text.join("\n");
        crate::term::to_clipboard(&self.clipboard);

        if cut {
            let (doc, view) = self.pair();
            let edits = edit::insert(doc, view, "");
            self.after_edit(edits);
        } else if took_lines {
            // Put the cursor back rather than leaving the line selected: you
            // asked to copy it, not to select it.
            self.view_mut().sel.collapse_selections();
        }
        let count = self.clipboard.chars().count();
        let did = if cut { "cut" } else { "copied" };
        if self.said_clipboard {
            self.say(format!("{did} {count} characters"));
        } else {
            // Where a copy goes is the one thing about a terminal editor
            // nobody can work out by looking, so it is said once, on the first
            // copy, and then never again.
            self.said_clipboard = true;
            self.say(format!(
                "{did} {count} characters — {}",
                crate::term::clipboard_story()
            ));
        }
    }

    fn on_paste(&mut self, text: &str) {
        if self.refuse_if_read_only() {
            return;
        }
        match &mut self.overlay {
            Overlay::Picker(picker) => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    picker.type_char(c);
                }
                return;
            }
            Overlay::Prompt(prompt) => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    prompt.insert(c);
                }
                self.on_prompt_changed();
                return;
            }
            // A menu has nothing to type into. Pasting is you having finished
            // with it, so it closes and the text goes where it was going.
            Overlay::Menu(_) => self.overlay = Overlay::None,
            _ => {}
        }
        if self.hover.as_ref().is_some_and(|h| h.focused) {
            self.hover = None;
        }
        // A pasted `\r\n` is the terminal's idea of a line break, not the
        // file's; the rope only ever holds `\n`.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (doc, view) = self.pair();
        let edits = edit::insert_atomic(doc, view, &text);
        self.after_edit(edits);
    }

    fn escape(&mut self) {
        // In order of how much is in the way, so one press takes off one
        // layer and you never lose something you did not mean to.
        if self.completion.is_some() {
            self.completion = None;
        } else if self.hover.is_some() || self.signature.is_some() {
            self.dismiss_popups();
        } else if self.view().sel.len() > 1 {
            self.view_mut().sel.collapse_to_primary();
            self.scroll_into_view();
        } else if !self.view().sel.primary().is_empty() {
            self.view_mut().sel.collapse_selections();
        } else if self.status.showing() {
            self.status = Status::quiet();
        }
    }

    // ---- Files ----

    /// Write the file, reformatting it first if that is what you have asked
    /// for.
    ///
    /// This may not write anything itself. Every step before the bytes go
    /// down is a round trip to a language server — the fixes it would make,
    /// and then the formatter — so a save with either of those turned on is a
    /// queue of questions that ends in a write. [`App::write_now`] is the end
    /// of that queue, and is separate so that the save which follows a format
    /// does not ask for another one.
    fn save(&mut self, to: Option<PathBuf>) {
        if self.before_save.is_some() {
            // Already on its way. A second Ctrl-S while the servers are
            // thinking should not start the whole dance again.
            return;
        }
        let id = self.view().doc;
        if self.here().path.is_none() && to.is_none() {
            // Nowhere to write it yet, so there is nothing to get ready.
            return self.write_now(to);
        }
        let mut steps = self.fix_steps(id, self.config.code_actions_on_save());
        // A tool that rewrites the file is a formatter, so it goes where the
        // formatter goes: after the fixes, which put text in, and before the
        // write, which is the point of all this.
        steps.extend(self.rewriters_on_save(id).into_iter().map(Step::Rewrite));
        if self.config.format_on_save() {
            steps.push(Step::Format);
        }
        if steps.is_empty() {
            return self.write_now(to);
        }
        self.begin(id, steps, true, to);
    }

    /// Ask every server what it would fix in this file on its own, and do it.
    ///
    /// The other half of "reformat": a formatter lays code out and a linter
    /// takes the unused import away, and they are two different requests to
    /// two different servers. This is the second one, on its own, for when you
    /// want the fixes without the reflow — or when the formatter is somebody
    /// else's job entirely.
    fn fix_all(&mut self, kinds: &[String]) {
        if self.before_save.is_some() {
            return;
        }
        let id = self.view().doc;
        let steps = self.fix_steps(id, kinds);
        if steps.is_empty() {
            return self.say("no language server here with fixes of its own");
        }
        self.begin(id, steps, false, None);
    }

    /// Both halves of tidying a file up: the servers' own fixes, and then the
    /// formatter.
    ///
    /// In that order, and not the other way round. A fix puts text in — an
    /// import, a rewritten call — and the formatter is what lays the result
    /// out; formatting first and fixing afterwards leaves the fix sitting
    /// there unformatted.
    fn format_and_fix(&mut self) {
        if self.before_save.is_some() {
            return;
        }
        let id = self.view().doc;
        let both = [SOURCE_FIX_ALL.to_string(), SOURCE_ORGANIZE_IMPORTS.to_string()];
        let mut steps = self.fix_steps(id, &both);
        if steps.is_empty() {
            return self.format();
        }
        steps.push(Step::Format);
        self.begin(id, steps, false, None);
    }

    /// One step per kind of fix per server that can answer for one.
    fn fix_steps(&self, doc: DocId, kinds: &[String]) -> Vec<Step> {
        if kinds.is_empty() {
            return Vec::new();
        }
        let Some(open) = self.docs.iter().find(|d| d.id == doc) else {
            return Vec::new();
        };
        let servers = self.lsp.who_all_can(open, "codeActionProvider");
        kinds
            .iter()
            .flat_map(|kind| {
                servers
                    .iter()
                    .map(move |id| Step::Fix(kind.clone(), *id))
            })
            .collect()
    }

    /// The tools a plugin asked to be run on every save that rewrite the file.
    fn rewriters_on_save(&self, doc: DocId) -> Vec<&'static Tool> {
        let Some(language) = self.doc(doc).map(|d| lang::get(d.language).name.clone()) else {
            return Vec::new();
        };
        crate::cmd::all()
            .iter()
            .filter_map(|cmd| cmd.tool())
            .filter(|tool| {
                tool.on_save && tool.output == Output::Replace && tool.wants(&language)
            })
            .collect()
    }

    fn begin(&mut self, doc: DocId, steps: Vec<Step>, write: bool, to: Option<PathBuf>) {
        self.before_save = Some(BeforeSave {
            doc,
            left: steps,
            doing: None,
            write,
            to,
            due: Instant::now(),
        });
        self.advance();
    }

    /// Start the next step, or finish up when there are none left.
    fn advance(&mut self) {
        loop {
            let Some(before) = &mut self.before_save else {
                return;
            };
            let Some(step) = before.left.first().cloned() else {
                let write = before.write;
                let to = before.to.take();
                self.before_save = None;
                if write {
                    self.write_now(to);
                }
                return;
            };
            before.left.remove(0);
            before.doing = Some(step.clone());
            before.due = Instant::now() + BEFORE_SAVE_WAIT;

            let doc = before.doc;
            let started = match step {
                Step::Fix(kind, server) => {
                    let App { docs, lsp, .. } = self;
                    docs.iter()
                        .find(|d| d.id == doc)
                        .is_some_and(|open| lsp.source_action(open, &kind, server))
                }
                Step::Rewrite(tool) => self.start_tool(tool, doc),
                Step::Format => self.start_formatter(doc),
            };
            if started {
                return;
            }
            // That server has gone, or the tool would not start. Go on to the
            // next rather than waiting for an answer that is not coming.
        }
    }

    /// Ask the language server's own formatter. Answers whether there was one.
    fn start_formatter(&mut self, id: DocId) -> bool {
        let tab_width = self.config.tab_width();
        let spaces = self
            .doc(id)
            .is_some_and(|d| matches!(d.indent, Indent::Spaces(_)));
        let App { docs, lsp, .. } = self;
        docs.iter()
            .find(|d| d.id == id)
            .filter(|doc| doc.path.is_some())
            .is_some_and(|doc| lsp.format(doc, tab_width, spaces).is_some())
    }

    /// One server's answer about what it would fix in the whole file.
    ///
    /// At most one action is taken from each answer, and then the next step
    /// starts afresh. Nobody is choosing between these — they are the fixes a
    /// server is certain enough about to have called `source.fixAll` — but
    /// they still cannot be stacked up and applied together, because each was
    /// worked out against the file as it was.
    fn take_source_actions(&mut self, server: ServerId, doc: DocId, version: i32, value: Value) {
        let waiting = self.before_save.as_ref().is_some_and(|b| {
            b.doc == doc && matches!(b.doing, Some(Step::Fix(_, id)) if id == server)
        });
        if !waiting {
            return;
        }
        if let Some(before) = &mut self.before_save {
            before.doing = None;
        }
        // A file that moved on while the server was thinking. The edits are
        // about text that is no longer there, so they are dropped — but the
        // save that was waiting on them should still happen.
        if self.doc(doc).map(|d| d.version) == Some(version)
            && let Value::Array(actions) = value
            && let Some(action) = actions
                .into_iter()
                .find(|a| a.get("title").and_then(Value::as_str).is_some())
        {
            self.do_code_action(server, action);
        }
        self.advance();
    }

    /// A step that has been waiting too long is given up on. A file you
    /// pressed Ctrl-S on is a file that gets written.
    fn check_before_save(&mut self) {
        let waited = self
            .before_save
            .as_ref()
            .is_some_and(|b| b.doing.is_some() && b.due <= Instant::now());
        if waited {
            if let Some(before) = &mut self.before_save {
                before.doing = None;
            }
            self.advance();
        }
    }

    /// Whether a save is waiting on this step right now.
    fn waiting_on(&self, step: &Step) -> bool {
        self.before_save
            .as_ref()
            .is_some_and(|b| b.doing.as_ref() == Some(step))
    }

    // ---- Tools a plugin brought ----

    /// Run a tool on the file in front of you.
    ///
    /// Nothing here waits: the program is started on a thread and the answer
    /// arrives as an event, the same way a language server's does. A test run
    /// that takes a minute costs a minute of it running, not a minute of the
    /// editor being gone.
    fn run_tool(&mut self, tool: &'static Tool) {
        let id = self.view().doc;
        let language = lang::get(self.here().language).name.clone();
        if !tool.wants(&language) {
            return self.say(format!("{} is not for {language} files", tool.name));
        }
        if self.here().path.is_none() {
            return self.say(format!("{} needs a file on disk to work on", tool.name));
        }
        if self.start_tool(tool, id) {
            self.say(format!("running {}…", tool.name));
        }
    }

    /// Run one of a plugin's commands.
    ///
    /// Nothing waits here. The command goes down the pipe and the next
    /// keystroke is handled; whatever the plugin has to say about it arrives
    /// later on the same channel the keyboard arrives on. A plugin that takes
    /// four minutes to build a firmware image cannot make the cursor stutter,
    /// because the cursor is not waiting on it.
    fn run_plugin_command(&mut self, command: &'static crate::plugin::Command) {
        let language = lang::get(self.here().language).name.clone();
        if !command.wants(&language) {
            return self.say(format!("{} is not for {language} files", command.name));
        }
        let path = self.here().path.clone();
        let (line, column) = self.here().point_at_char(self.view().cursor());
        // What is selected, if anything. An empty selection is `null` rather
        // than an empty string: "nothing is selected" and "the empty string is
        // selected" are different answers, and a plugin should not have to
        // guess which it got.
        let doc = self.here();
        let selection: Option<String> = match self
            .view()
            .sel
            .ranges()
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| doc.slice(*r))
            .collect::<Vec<String>>()
        {
            taken if taken.is_empty() => None,
            taken => Some(taken.join("\n")),
        };
        // What the command is being run *on*. A plugin that does not care can
        // ignore all of it; one that does should not have to ask. Counted
        // from zero, as everything inside the editor is.
        let context = json!({
            "file": path,
            "language": language,
            "line": line,
            "column": column,
            "selection": selection,
        });
        // A buffer with no file of its own — a plugin's own output, say —
        // still belongs to the project you are working in, and that is the
        // project the command is about.
        let from = path
            .clone()
            .unwrap_or_else(|| self.project.clone());
        if command.opens_panel {
            // Opening a panel is not something the plugin does; it is
            // something the editor does, and then tells the plugin about so
            // that it has somewhere to put its lines.
            //
            // Running a docked panel's command again puts it away, and then
            // there is nothing to tell it to fill. Saying `panel/opened` here
            // would be telling a plugin its sidebar had just appeared at the
            // moment it went.
            if !self.open_panel(command) {
                let (plugin, id) = (command.plugin.clone(), command.id.clone());
                self.tell_panel(&plugin, "panel/closed", json!({ "panel": id }));
                return self.take_plugin_problems();
            }
        }
        self.hosts.run(command, Some(&from), context);
        self.take_plugin_problems();
    }

    /// Put a plugin's panel in front of you, making the buffer if this is the
    /// first time.
    ///
    /// The same buffer each time, so opening it twice is going back to it
    /// rather than ending up with two.
    ///
    /// Answers whether the panel is on the screen afterwards, which for a
    /// docked one is not always yes: running its command again is how you put
    /// it away.
    fn open_panel(&mut self, command: &'static crate::plugin::Command) -> bool {
        let id = self.panel_buffer(command);
        let Some(dock) = command.dock else {
            // An ordinary panel is a tab, which is the right answer for
            // something you read and then leave.
            self.show(id);
            return true;
        };
        // A docked panel is a switch, not a tab: running its command again is
        // how you get rid of it. That is what "collapsible" means from the
        // keyboard, and a sidebar you can only open would be a sidebar
        // everybody closes by quitting.
        if let Some(at) = self.pane_showing_docked(id) {
            self.panes.remove(at);
            self.focus = self.focus.min(self.panes.len().saturating_sub(1));
            self.session_changed();
            return false;
        }
        self.dock_panel(id, dock);
        true
    }

    /// The buffer behind a panel, made the first time it is asked for.
    fn panel_buffer(&mut self, command: &'static crate::plugin::Command) -> DocId {
        if let Some(id) = self
            .docs
            .iter()
            .find(|d| d.panel.as_ref().is_some_and(|p| p.id == command.id))
            .map(|d| d.id)
        {
            return id;
        }
        let id = self.new_scratch();
        if let Some(doc) = self.doc_mut(id) {
            doc.name = command.name.clone();
            // Nothing types into a panel: what is in it belongs to the
            // plugin, and a half-typed-in panel would be a buffer whose text
            // and whose colours disagree.
            doc.read_only = true;
            doc.panel = Some(crate::doc::Panel {
                plugin: command.plugin.clone(),
                id: command.id.clone(),
                spans: Vec::new(),
                actions: Vec::new(),
            });
        }
        id
    }

    /// Which pane is showing this buffer as a dock, if one is.
    fn pane_showing_docked(&self, id: DocId) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.doc == id && pane.dock.is_some())
    }

    /// Put a buffer in a pane pinned to an edge, and go there.
    ///
    /// Beside the middle rather than in it: the pane it opens next to keeps
    /// what it was showing, which is the whole point of a dock — you asked
    /// for a tree of files, not for the file you were reading to go away.
    fn dock_panel(&mut self, id: DocId, dock: crate::view::Dock) {
        let mut pane = crate::view::View::new(id, false);
        pane.dock = Some(dock);
        // On the side it belongs to, so the order of the panes matches the
        // order they are drawn in and Tab walks them left to right.
        let at = match dock.edge {
            crate::view::Edge::Left => 0,
            _ => self.panes.len(),
        };
        self.panes.insert(at, pane);
        self.focus = at;
        self.session_changed();
    }

    /// Start a tool, quietly. Answers whether it is on its way — a step in a
    /// save asks this rather than `run_tool`, because a tool that would not
    /// start must not leave the save waiting for it.
    fn start_tool(&mut self, tool: &'static Tool, id: DocId) -> bool {
        let Some(path) = self.doc(id).and_then(|d| d.path.clone()) else {
            return false;
        };
        let root = lang::project_root(&path, &tool.roots);

        // The same placeholders a language server's settings may use, so that
        // a Python tool lands in the project's environment without any of that
        // being written into the editor as a special case, plus the one thing
        // a tool needs that a server does not: which file.
        let environment = self.lsp.environment_for(&root);
        let mut vars = crate::venv::Vars::new(&root, environment.as_ref());
        vars.set("file", path.display().to_string());
        let args: Vec<String> = tool.args.iter().filter_map(|a| vars.fill(a)).collect();
        let mut env: Vec<(String, String)> = Vec::new();
        if let Some(found) = &environment {
            // A tool run in a project with an environment should be the one
            // inside it: `black` from the venv, not whichever is on PATH.
            env.push(("VIRTUAL_ENV".into(), found.root.display().to_string()));
            let path_var = std::env::var("PATH").unwrap_or_default();
            env.push((
                "PATH".into(),
                format!("{}:{path_var}", found.bin().display()),
            ));
        }

        let Some(doc) = self.doc(id) else { return false };
        let version = doc.version;
        let stdin = tool.stdin.then(|| doc.rope.to_string());
        let tx = self.tx.clone();
        match crate::tool::spawn(tool, id, version, &root, args, env, stdin, tx) {
            Ok(()) => true,
            Err(why) => {
                self.say_bad(why);
                false
            }
        }
    }

    /// A tool has finished. Do as its plugin said with what it printed.
    fn on_tool(&mut self, done: crate::tool::Finished) {
        let Some(tool) = crate::cmd::by_name(&done.tool).and_then(|c| c.tool()) else {
            // Its plugin was switched off while it was running.
            return;
        };
        // A save may be standing behind this one waiting its turn.
        let in_a_save = self.waiting_on(&Step::Rewrite(tool));
        if in_a_save && let Some(before) = &mut self.before_save {
            before.doing = None;
        }
        let complaint = done
            .err
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string();

        match tool.output {
            Output::Replace => self.take_tool_text(tool, done, &complaint),
            Output::Show => {
                let text = match done.out.trim().is_empty() {
                    true => done.err.clone(),
                    false => done.out.clone(),
                };
                if text.trim().is_empty() {
                    return self.say(format!("{} said nothing", tool.name));
                }
                self.show_in_a_buffer(&format!("{} output", tool.name), &text);
            }
            Output::Problems => self.take_tool_problems(tool, &done),
            Output::Ignore => match done.ok {
                true => self.say_good(format!("{} finished", tool.name)),
                false => self.say_bad(match complaint.is_empty() {
                    true => format!("{} failed", tool.name),
                    false => format!("{}: {complaint}", tool.name),
                }),
            },
        }
        if in_a_save {
            self.advance();
        }
    }

    /// What a formatter printed, put back into the buffer.
    fn take_tool_text(&mut self, tool: &'static Tool, done: crate::tool::Finished, why: &str) {
        if !done.ok {
            return self.say_bad(match why.is_empty() {
                true => format!("{} would not run", tool.name),
                false => format!("{}: {why}", tool.name),
            });
        }
        if self.doc(done.doc).map(|d| d.version) != Some(done.version) {
            // The file moved on while it was thinking, so what came back is
            // about text that is no longer there. Putting it in would undo
            // whatever was typed in the meantime.
            return self.say(format!("{} answered too late — the file has moved on", tool.name));
        }
        if done.out.trim().is_empty() {
            // A tool that printed nothing has almost certainly failed in a way
            // it did not admit to, and emptying somebody's file over it is not
            // a recoverable kind of wrong.
            return self.say_bad(format!("{} printed nothing — the file is untouched", tool.name));
        }
        let Some(doc) = self.doc_mut(done.doc) else {
            return;
        };
        if doc.rope == done.out.as_str() {
            return self.say(format!("{} had nothing to change", tool.name));
        }
        let len = doc.len_chars();
        let sel = crate::text::Selections::single(Range::point(0));
        let edits = doc.apply_atomic(
            vec![crate::doc::Change::replace(0, len, done.out.clone())],
            &sel,
        );
        self.after_edit_to(done.doc, edits, None);
        self.say_good(format!("{} reformatted this", tool.name));
    }

    /// What a linter printed, read as problems and shown in the margin.
    fn take_tool_problems(&mut self, tool: &'static Tool, done: &crate::tool::Finished) {
        let Some(pattern) = &tool.pattern else {
            return self.say_bad(format!(
                "{} is set to find problems but says nothing about how to read them",
                tool.name
            ));
        };
        let told = crate::doc::Told::Tool(tool.id.as_str());
        // A tool sends its complete opinion every time, so its old findings go
        // and everybody else's stay — the same rule a language server gets.
        for doc in &mut self.docs {
            doc.diagnostics.retain(|d| d.told != told);
        }

        let mut both = done.out.clone();
        both.push('\n');
        both.push_str(&done.err);
        let found = crate::tool::problems(pattern, &both);
        let mut count = 0;
        for problem in found {
            let full = match problem.file.is_absolute() {
                true => problem.file.clone(),
                false => self.project.join(&problem.file),
            };
            let Some(id) = self
                .docs
                .iter()
                .find(|d| {
                    d.path.as_deref() == Some(full.as_path())
                        || d.path.as_deref() == Some(problem.file.as_path())
                })
                .map(|d| d.id)
            else {
                // About a file that is not open. Perfectly normal for a tool
                // pointed at a whole project.
                continue;
            };
            let Some(doc) = self.doc_mut(id) else { continue };
            let at = doc.char_at_lsp_point(problem.line, problem.column);
            let end = doc.char_at_lsp_point(problem.line, problem.column + 1);
            doc.diagnostics.push(crate::doc::Diagnostic {
                range: Range::new(at, end.max(at)),
                severity: problem.severity,
                message: problem.message,
                source: Some(tool.name.clone()),
                code: None,
                data: None,
                told,
            });
            count += 1;
        }
        match count {
            0 if done.ok => self.say_good(format!("{}: nothing to report", tool.name)),
            0 => self.say(format!("{} found nothing it could read", tool.name)),
            n => self.say(format!("{}: {n} {}", tool.name, plural("problem", n))),
        }
    }

    /// Put some text in a buffer of its own, for reading rather than editing,
    /// and go to it.
    fn show_in_a_buffer(&mut self, name: &str, text: &str) {
        self.put_in_a_buffer(name, text, true);
    }

    /// The same, saying whether to go to it.
    ///
    /// A tool you just ran should show you what it printed: you asked half a
    /// second ago and you are waiting. A plugin's build finishing four minutes
    /// later should not take the cursor out of whatever you got on with in the
    /// meantime, which is why a plugin has to ask for that rather than getting
    /// it by default.
    fn put_in_a_buffer(&mut self, name: &str, text: &str, focus: bool) {
        // The same buffer each time, so running a test suite twice does not
        // leave two tabs of output to close.
        let existing = self
            .docs
            .iter()
            .find(|d| d.path.is_none() && d.name == name)
            .map(|d| d.id);
        let id = match existing {
            Some(id) => id,
            None => {
                let id = self.new_scratch();
                if let Some(doc) = self.doc_mut(id) {
                    doc.name = name.to_string();
                }
                id
            }
        };
        if let Some(doc) = self.doc_mut(id) {
            let len = doc.len_chars();
            let sel = crate::text::Selections::single(Range::point(0));
            let edits =
                doc.apply_atomic(vec![crate::doc::Change::replace(0, len, text.to_string())], &sel);
            doc.mark_saved();
            self.after_edit_to(id, edits, None);
        }
        if focus {
            self.show(id);
            self.view_mut().sel = crate::text::Selections::single(Range::point(0));
            self.scroll_into_view();
        }
    }

    /// The tools a plugin asked to be run every time this file is saved.
    fn tools_on_save(&mut self, doc: DocId) {
        let Some(language) = self
            .doc(doc)
            .map(|d| lang::get(d.language).name.clone())
        else {
            return;
        };
        let wanted: Vec<&'static Tool> = crate::cmd::all()
            .iter()
            .filter_map(|cmd| cmd.tool())
            .filter(|tool| {
                tool.on_save && tool.output != Output::Replace && tool.wants(&language)
            })
            .collect();
        for tool in wanted {
            self.start_tool(tool, doc);
        }
    }

    fn write_now(&mut self, to: Option<PathBuf>) {
        let id = self.view().doc;
        let path = match to.or_else(|| self.doc(id).and_then(|d| d.path.clone())) {
            Some(path) => path,
            None => return self.open_prompt(PromptKind::SaveAs),
        };
        if self.config.trim_trailing_whitespace() {
            self.trim_trailing_whitespace();
        }

        let final_newline = self.config.final_newline();
        let Some(doc) = self.doc_mut(id) else { return };
        match doc.save_to(&path, final_newline) {
            Ok(()) => {
                let name = doc.name.clone();
                let lines = doc.len_lines();
                let App { docs, lsp, hosts, .. } = self;
                if let Some(doc) = docs.iter().find(|d| d.id == id) {
                    lsp.did_save(doc);
                    hosts.saved(doc);
                    // A buffer that has just been given a name is a buffer a
                    // language server has never heard of.
                    lsp.open(doc);
                    hosts.opened_buffer(doc);
                }
                // Saving is how a file git has never seen becomes one it has,
                // and how a "save as" becomes a different file entirely.
                self.git.forget_baseline(id);
                self.say_good(format!("saved {name}, {lines} lines"));
                // And whatever a plugin asked to have run over the file every
                // time it is written. Not the ones that rewrite it — those went
                // in before the write, where they belong — but the linters,
                // whose whole job is to look at what has just been saved.
                self.tools_on_save(id);
            }
            Err(e) => self.say_bad(format!("{e}")),
        }
    }

    fn save_all(&mut self) {
        let ids: Vec<DocId> = self
            .docs
            .iter()
            .filter(|d| d.is_modified() && d.path.is_some())
            .map(|d| d.id)
            .collect();
        let count = ids.len();
        let final_newline = self.config.final_newline();
        let mut failed = Vec::new();
        for id in ids {
            let Some(doc) = self.doc_mut(id) else {
                continue;
            };
            let Some(path) = doc.path.clone() else {
                continue;
            };
            if let Err(e) = doc.save_to(&path, final_newline) {
                failed.push(format!("{e}"));
                continue;
            }
            let App { docs, lsp, hosts, .. } = self;
            if let Some(doc) = docs.iter().find(|d| d.id == id) {
                lsp.did_save(doc);
                hosts.saved(doc);
            }
            self.git.forget_baseline(id);
        }
        match failed.first() {
            Some(problem) => self.say_bad(problem.clone()),
            None if count == 0 => self.say("nothing to save"),
            None => self.say_good(format!("saved {count} files")),
        }
    }

    fn trim_trailing_whitespace(&mut self) {
        let (doc, view) = self.pair();
        let mut changes = Vec::new();
        for line in 0..doc.len_lines() {
            let start = text::line_start(&doc.rope, line);
            let end = text::line_end(&doc.rope, line);
            let mut at = end;
            while at > start && doc.rope.char(at - 1).is_whitespace() {
                at -= 1;
            }
            if at < end {
                changes.push(crate::doc::Change::delete(at, end));
            }
        }
        if changes.is_empty() {
            return;
        }
        let before = view.sel.clone();
        let edits = doc.apply_atomic(changes, &before);
        view.absorb(&edits, doc.len_chars());
        self.after_edit(edits);
    }

    fn reload(&mut self) {
        let id = self.view().doc;
        if self.doc(id).is_some_and(Document::is_modified) {
            self.overlay = Overlay::Confirm(Confirm {
                message: format!("{} has unsaved changes", self.here().name),
                choices: vec![
                    ('r', "read the file again, losing them".into()),
                    ('c', "keep them".into()),
                ],
                then: Then::Reload(id),
            });
            return;
        }
        self.do_reload(id);
    }

    fn do_reload(&mut self, id: DocId) {
        match self.take_from_disk(id, Reread::Asked) {
            Ok(true) => self.say_good("read again from disk"),
            Ok(false) => self.say("already what is on disk"),
            Err(e) => self.say_bad(format!("{e}")),
        }
    }

    /// Replace a buffer's text with what is on the file now, keeping where
    /// everybody was looking.
    ///
    /// The new text goes in as an ordinary edit rather than as a new
    /// `Document`. That is what makes the rest of the editor keep working
    /// across a re-read: cursors are carried by the same code that carries
    /// them across a paste, language servers are told what changed instead of
    /// being left holding the old text, and the whole thing can be undone.
    ///
    /// Answers whether anything actually differed.
    fn take_from_disk(&mut self, id: DocId, why: Reread) -> anyhow::Result<bool> {
        let Some(path) = self.doc(id).and_then(|d| d.path.clone()) else {
            anyhow::bail!("this buffer has no file to read");
        };
        // Content and stamp from the same moment, or nothing. Taking the text
        // as it was at one instant and the stamp as it was at another is how a
        // buffer ends up holding half a file forever: the stamp says it is up
        // to date, so nothing ever looks again.
        let Some((bytes, stamp)) = crate::doc::read_whole(&path)? else {
            anyhow::bail!(
                "{} is being written to — nothing was read",
                path.display()
            );
        };
        // Text that is not valid UTF-8 comes in as replacement characters,
        // which is the right answer for a file you asked to open and the wrong
        // one for a buffer being rewritten under you on a timer. It is also
        // what half a file looks like when the half ends in the middle of a
        // character, so refusing it here is the last of the three guards
        // against a torn read.
        if why == Reread::OnATimer && std::str::from_utf8(&bytes).is_err() {
            anyhow::bail!("{} is not text — reload to read it anyway", path.display());
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let text = if text.contains("\r\n") {
            text.replace("\r\n", "\n")
        } else {
            text
        };

        let Some(doc) = self.doc_mut(id) else {
            anyhow::bail!("that buffer has gone");
        };
        if doc.rope == text.as_str() {
            doc.took_from_disk(stamp);
            return Ok(false);
        }
        let len = doc.len_chars();
        let sel = crate::text::Selections::single(Range::point(0));
        let edits = doc.apply_atomic(vec![crate::doc::Change::replace(0, len, text)], &sel);
        doc.took_from_disk(stamp);
        self.after_edit_to(id, edits, None);
        Ok(true)
    }

    fn close(&mut self, force: bool) {
        let id = self.view().doc;
        if !force && self.doc(id).is_some_and(Document::is_modified) {
            self.overlay = Overlay::Confirm(Confirm {
                message: format!("{} has unsaved changes", self.here().name),
                choices: vec![
                    ('s', "save and close".into()),
                    ('d', "close without saving".into()),
                    ('c', "keep it open".into()),
                ],
                then: Then::Close(id),
            });
            return;
        }
        self.close_doc(id);
    }

    /// Close several buffers at once, from a tab menu or the palette.
    ///
    /// Anything with unsaved changes in it is left open and counted, rather
    /// than asking about each one in turn: a question per file is a question
    /// nobody reads by the fourth time, and closing a tab is not worth losing
    /// work over. What is left behind says so.
    fn close_many(&mut self, keep: Keep) {
        let here = self.view().doc;
        let doomed: Vec<DocId> = self
            .docs
            .iter()
            .filter(|d| match keep {
                Keep::Others => d.id != here,
                Keep::Unsaved => !d.is_modified(),
                Keep::Nothing => true,
            })
            .map(|d| d.id)
            .collect();
        let mut closed = 0;
        let mut kept = 0;
        for id in doomed {
            if self.doc(id).is_some_and(Document::is_modified) {
                kept += 1;
                continue;
            }
            self.close_doc(id);
            closed += 1;
        }
        match (closed, kept) {
            (0, 0) => self.say("nothing to close"),
            (n, 0) => self.say(format!("closed {n} {}", plural("buffer", n))),
            (n, k) => self.say(format!(
                "closed {n} {}, kept {k} with unsaved changes",
                plural("buffer", n)
            )),
        }
    }

    /// Put this file's path on the clipboard. What you want when you are about
    /// to name it to something else — a shell, a colleague, a stack trace.
    fn copy_path(&mut self, relative: bool) {
        let Some(path) = self.here().path.clone() else {
            return self.say("this buffer has no file behind it");
        };
        let text = if relative {
            short(&path, &self.project)
        } else {
            path.display().to_string()
        };
        self.clipboard = text.clone();
        crate::term::to_clipboard(&text);
        self.say(format!("copied {text}"));
    }

    fn leave(&mut self, force: bool) {
        let unsaved: Vec<String> = self
            .docs
            .iter()
            .filter(|d| d.is_modified())
            .map(|d| d.name.clone())
            .collect();
        if !force && !unsaved.is_empty() {
            self.overlay = Overlay::Confirm(Confirm {
                message: match unsaved.len() {
                    1 => format!("{} has unsaved changes", unsaved[0]),
                    n => format!("{n} files have unsaved changes"),
                },
                choices: vec![
                    ('s', "save them all and leave".into()),
                    ('d', "leave without saving".into()),
                    ('c', "stay".into()),
                ],
                then: Then::Quit,
            });
            return;
        }
        self.quit = true;
    }

    fn step_buffer(&mut self, by: isize) {
        if self.docs.len() < 2 {
            return;
        }
        let here = self.view().doc;
        let at = self.docs.iter().position(|d| d.id == here).unwrap_or(0) as isize;
        let len = self.docs.len() as isize;
        let next = self.docs[(at + by).rem_euclid(len) as usize].id;
        self.show(next);
    }

    // ---- The order of the tabs ----

    /// Put a buffer at a particular place in the row of tabs.
    ///
    /// The row is the order of `docs`, so this is that list being reordered.
    /// Nothing anywhere holds onto a position in it — a buffer is named by its
    /// [`DocId`] everywhere else — which is what makes moving one about a
    /// question of moving one about, rather than of finding everything that
    /// would now be pointing at the wrong file.
    ///
    /// Answers whether anything moved.
    fn move_tab(&mut self, id: DocId, to: usize) -> bool {
        let Some(from) = self.docs.iter().position(|d| d.id == id) else {
            return false;
        };
        let to = to.min(self.docs.len().saturating_sub(1));
        if from == to {
            return false;
        }
        let doc = self.docs.remove(from);
        self.docs.insert(to, doc);
        self.session_changed();
        true
    }

    /// Move the tab you are looking at one place along.
    ///
    /// It stops at the ends rather than wrapping. Stepping *between* buffers
    /// wraps, because going round is how you visit them all; moving one wraps
    /// a file from the front of the row to the back, which is never what
    /// somebody nudging a tab along meant.
    fn step_tab(&mut self, by: isize) {
        if self.docs.len() < 2 {
            return;
        }
        let here = self.view().doc;
        let at = self.docs.iter().position(|d| d.id == here).unwrap_or(0) as isize;
        let to = at + by;
        if to < 0 || to >= self.docs.len() as isize {
            return self.say(match by < 0 {
                true => "this tab is already first",
                false => "this tab is already last",
            });
        }
        self.move_tab(here, to as usize);
    }

    /// The tab being carried about, for the drawing to show as picked up.
    pub fn dragging_tab(&self) -> Option<DocId> {
        match self.drag {
            Some(Drag::Tab { id, .. }) => Some(id),
            _ => None,
        }
    }

    /// Where each tab is on the screen: one span per file, rather than the two
    /// hit boxes — the name and the cross — it is drawn as.
    ///
    /// In screen order, and only the ones on the screen: a tab scrolled off
    /// the end has no span, which is why dragging past the edge is answered by
    /// the arrows there rather than by this.
    fn tab_spans(&self) -> Vec<(DocId, u16, u16)> {
        let mut out: Vec<(DocId, u16, u16)> = Vec::new();
        for (area, id, _) in &self.tab_hits {
            match out.iter_mut().find(|(seen, ..)| seen == id) {
                Some(span) => {
                    span.1 = span.1.min(area.x);
                    span.2 = span.2.max(area.x + area.width);
                }
                None => out.push((*id, area.x, area.x + area.width)),
            }
        }
        out.sort_by_key(|(_, from, _)| *from);
        out
    }

    /// Carry a tab to where the pointer is.
    ///
    /// The rule is the one that makes this feel right rather than the obvious
    /// one. "Move it to whichever tab the pointer is over" oscillates: put a
    /// narrow tab where a wide one was and the pointer is left over the wide
    /// one again, which asks for the swap back, and the two trade places for
    /// as long as you hold the mouse still. So a tab only ever moves one place
    /// at a time, and only once the pointer is past the *middle* of the
    /// neighbour it would trade with — which is far enough that after the
    /// trade the pointer is not past the middle of anything, and it settles.
    fn drag_tab(&mut self, id: DocId, column: u16, row: u16) {
        if !self.tab_row(column, row) {
            return;
        }
        let spans = self.tab_spans();
        let Some(here) = spans.iter().position(|(seen, ..)| *seen == id) else {
            return;
        };
        let (_, from, to) = spans[here];
        let step = if column >= to {
            1
        } else if column < from {
            -1
        } else {
            return;
        };
        let Some(neighbour) = here
            .checked_add_signed(step)
            .and_then(|next| spans.get(next))
        else {
            // The far end of the row, or a neighbour scrolled off the screen.
            // Holding it over the arrow there is what keeps it going.
            return;
        };
        let (_, their_from, their_to) = *neighbour;
        let middle = their_from + (their_to - their_from) / 2;
        let past = match step {
            1 => column >= middle,
            _ => column <= middle,
        };
        if !past {
            return;
        }
        let at = self.docs.iter().position(|d| d.id == id).unwrap_or(0);
        if let Some(to) = at.checked_add_signed(step) {
            self.move_tab(id, to);
        }
    }

    // ---- Panes ----

    fn split(&mut self) {
        if self.ordinary_panes() >= 4 {
            return self.say("four panes is as many as fit");
        }
        let mut copy = View::new(self.view().doc, self.view().wrap);
        copy.sel = self.view().sel.clone();
        copy.top = self.view().top;
        // Never a copy of the dock. Splitting a sidebar would give you two
        // sidebars, which is not what anybody means by it.
        copy.dock = None;
        let at = self.focus.min(self.panes.len().saturating_sub(1));
        self.panes.insert(at + 1, copy);
        self.focus = at + 1;
        self.session_changed();
    }

    fn close_pane(&mut self) {
        let at = self.focus.min(self.panes.len().saturating_sub(1));
        let docked = self.panes.get(at).is_some_and(|p| p.dock.is_some());
        // A dock is always closable — it is a thing you put there, and the
        // editor is still an editor without it. What has to survive is the
        // last pane showing a file.
        if !docked && self.ordinary_panes() < 2 {
            return self.say("that is the only pane");
        }
        self.panes.remove(at);
        self.focus = at.min(self.panes.len().saturating_sub(1));
        self.session_changed();
    }

    /// How many panes are showing a buffer in the middle rather than sitting
    /// on an edge.
    fn ordinary_panes(&self) -> usize {
        self.panes.iter().filter(|p| p.dock.is_none()).count()
    }

    /// The pane in the middle to put something in, from wherever the focus is.
    ///
    /// The nearest one after the focus, wrapping — so from a sidebar on the
    /// left it is the pane immediately to its right, which is the one anybody
    /// pointing at the sidebar is looking at.
    fn beside_the_docks(&self) -> Option<usize> {
        let len = self.panes.len();
        (0..len)
            .map(|step| (self.focus + step) % len)
            .find(|at| self.panes[*at].dock.is_none())
    }

    fn focus_pane(&mut self, by: isize) {
        let len = self.panes.len() as isize;
        self.focus = ((self.focus as isize + by).rem_euclid(len)) as usize;
        self.dismiss_popups();
        self.completion = None;
    }

    // ---- Settings ----

    fn step_theme(&mut self, by: isize) {
        let named = self.themes.cycle(self.config.theme_name(), by);
        self.theme = named.theme;
        self.config.theme = Some(named.name.clone());
        self.remember_settings();
        self.say(match named.about {
            Some(about) => format!("{} — {about}", named.name),
            None => named.name,
        });
    }

    fn set_theme(&mut self, name: &str) {
        if let Some(theme) = self.themes.by_name(name) {
            self.theme = theme;
            self.config.theme = Some(name.to_string());
        }
    }

    fn toggle_setting(&mut self, which: &str) {
        let said = match which {
            "line_numbers" => {
                let off = self.config.line_numbers() == LineNumbers::Off;
                self.config.line_numbers = Some(if off { "absolute" } else { "off" }.into());
                if off {
                    "line numbers on"
                } else {
                    "line numbers off"
                }
            }
            "relative_numbers" => {
                let relative = matches!(
                    self.config.line_numbers(),
                    LineNumbers::Relative | LineNumbers::Both
                );
                self.config.line_numbers = Some(if relative { "absolute" } else { "both" }.into());
                if relative {
                    "line numbers count from the top"
                } else {
                    "line numbers count from the cursor"
                }
            }
            "show_whitespace" => {
                let on = !self.config.show_whitespace();
                self.config.show_whitespace = Some(on);
                if on {
                    "showing spaces and tabs"
                } else {
                    "not showing spaces and tabs"
                }
            }
            "mouse" => {
                let on = !self.config.mouse();
                self.config.mouse = Some(on);
                self.mouse_on = on;
                if on {
                    "the mouse is textfold's"
                } else {
                    "the mouse is the terminal's — select and copy as usual"
                }
            }
            "wrap" => {
                let on = !self.config.wrap();
                self.config.wrap = Some(on);
                for pane in &mut self.panes {
                    pane.wrap = on;
                    pane.left = 0;
                }
                if on {
                    "long lines fold"
                } else {
                    "long lines run off the side"
                }
            }
            "auto_completion" => {
                let on = !self.config.auto_completion();
                self.config.auto_completion = Some(on);
                if on {
                    "completions appear as you type"
                } else {
                    "completions only when asked for"
                }
            }
            "auto_pairs" => {
                let on = !self.config.auto_pairs();
                self.config.auto_pairs = Some(on);
                if on {
                    "brackets close themselves"
                } else {
                    "brackets are yours to close"
                }
            }
            "format_on_save" => {
                let on = !self.config.format_on_save();
                self.config.format_on_save = Some(on);
                if on {
                    "formatting on save"
                } else {
                    "not formatting on save"
                }
            }
            "spaces" => {
                let on = !self.config.spaces();
                self.config.spaces = Some(on);
                if on {
                    "new files use spaces"
                } else {
                    "new files use tabs"
                }
            }
            "trim_trailing_whitespace" => {
                let on = !self.config.trim_trailing_whitespace();
                self.config.trim_trailing_whitespace = Some(on);
                if on {
                    "trailing spaces go on save"
                } else {
                    "trailing spaces stay"
                }
            }
            "code_actions_on_save" => {
                let on = self.config.code_actions_on_save().is_empty();
                self.config.code_actions_on_save = Some(match on {
                    true => vec![
                        SOURCE_FIX_ALL.to_string(),
                        SOURCE_ORGANIZE_IMPORTS.to_string(),
                    ],
                    false => Vec::new(),
                });
                if on {
                    "the servers fix what they can on save"
                } else {
                    "the servers leave the file alone on save"
                }
            }
            "restore_session" => {
                let on = !self.config.restore_session();
                self.config.restore_session = Some(on);
                if on {
                    "the tabs come back next time"
                } else {
                    "textfold starts empty"
                }
            }
            "underline_colour" => {
                let on = !self.config.underline_colour();
                self.config.underline_colour = Some(if on { "on" } else { "off" }.into());
                crate::term::set_underline_colour(on);
                if on {
                    "problems are underlined in colour — if the file has gone \
                     strange, this terminal does not have it"
                } else {
                    "problems are underlined plainly"
                }
            }
            _ => return,
        };
        self.remember_settings();
        self.say(said);
    }

    fn remember_settings(&mut self) {
        if let Err(e) = self.config.save() {
            self.say_bad(format!("could not write the settings: {e}"));
        }
    }
}

/// "place" or "places", so that a count of one does not read like a bug.
fn places(n: usize) -> &'static str {
    if n == 1 { "place" } else { "places" }
}

/// `buffer` or `buffers`, for the counts a status line reports.
fn plural(word: &'static str, n: usize) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

/// Which buffers a bulk close leaves alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Keep {
    /// Everything but the one you are looking at.
    Others,
    /// Everything with unsaved changes in it.
    Unsaved,
    /// Nothing.
    Nothing,
}

/// A path as it is worth showing: relative to the project when it is inside
/// it, and shortened with `~` when it is in your home directory.
pub fn short(path: &Path, project: &Path) -> String {
    if let Ok(rest) = path.strip_prefix(project) {
        return rest.display().to_string();
    }
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// The rows for the file picker.
fn file_rows(files: &[PathBuf], project: &Path) -> Vec<Row> {
    files
        .iter()
        .map(|path| {
            let shown = short(path, project);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| shown.clone());
            let parent = shown
                .rsplit_once('/')
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_default();
            let mut row = Row::new(name, Choice::Path(path.clone()));
            if !parent.is_empty() {
                row = row.detail(parent);
            }
            row
        })
        .collect()
}

// ---------------------------------------------------------------------------
// What is open over the editor: lists, prompts, questions, and the help.
// ---------------------------------------------------------------------------

impl App {
    fn open_prompt(&mut self, kind: PromptKind) {
        let input = match kind {
            PromptKind::SaveAs => self
                .here()
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            PromptKind::Rename => {
                text::word_text_at(&self.here().rope, self.view().cursor()).unwrap_or_default()
            }
            // The search box opens empty. Opening it is nearly always the
            // start of looking for something else, and a box with the last
            // thing in it is a box you have to clear before you can type —
            // the previous search is not lost, it is still what F3 finds.
            PromptKind::Find => String::new(),
            // Replace is the exception: "find that, and now change it" is the
            // usual way round, so the last search is what you meant.
            PromptKind::ReplaceFind => self.last_search.clone(),
            _ => String::new(),
        };
        let caret = input.chars().count();
        self.overlay = Overlay::Prompt(Prompt {
            kind,
            input,
            caret,
            origin: matches!(kind, PromptKind::Find).then(|| self.view().sel.clone()),
            held: String::new(),
            committed: false,
            label: None,
        });
        self.completion = None;
        self.dismiss_popups();
    }

    fn prompt_key(&mut self, key: Key) {
        let Overlay::Prompt(prompt) = &mut self.overlay else {
            return;
        };
        match (key.code, key.mods) {
            (KeyCode::Esc, _) => {
                // Searching moved the cursor as you typed; changing your mind
                // has to put it back where it was. Unless you have pressed
                // Enter, which is saying you meant to go there — leaving after
                // that leaves you where you walked to.
                let origin = prompt.origin.take().filter(|_| !prompt.committed);
                self.overlay = Overlay::None;
                if let Some(origin) = origin {
                    self.view_mut().sel = origin;
                    self.scroll_into_view();
                }
                return;
            }
            // In the search box Enter is "the next one", not "done": looking
            // through the hits is the whole of what you are doing, and having
            // to reach for another key to keep going is what makes people
            // close the box and press F3 instead. Escape is how you leave.
            (KeyCode::Enter, mods) if prompt.kind == PromptKind::Find => {
                let back = mods.contains(KeyModifiers::SHIFT) || mods.contains(KeyModifiers::ALT);
                self.find_from_prompt(if back { -1 } else { 1 });
                return;
            }
            (KeyCode::Enter, _) => return self.accept_prompt(),
            (KeyCode::Backspace, KeyModifiers::CONTROL)
            | (KeyCode::Char('w'), KeyModifiers::CONTROL) => prompt.delete_word(),
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                prompt.input.clear();
                prompt.caret = 0;
            }
            (KeyCode::Backspace, _) => prompt.backspace(),
            (KeyCode::Delete, _) => prompt.delete(),
            (KeyCode::Left, _) => prompt.caret = prompt.caret.saturating_sub(1),
            (KeyCode::Right, _) => {
                prompt.caret = (prompt.caret + 1).min(prompt.input.chars().count())
            }
            (KeyCode::Home, _) => prompt.caret = 0,
            (KeyCode::End, _) => prompt.caret = prompt.input.chars().count(),
            // The arrows and F3 do the same as Enter, for the hands that are
            // already there.
            (KeyCode::Down, _) => {
                if prompt.kind == PromptKind::Find {
                    self.find_from_prompt(1);
                }
                return;
            }
            (KeyCode::Up, _) => {
                if prompt.kind == PromptKind::Find {
                    self.find_from_prompt(-1);
                }
                return;
            }
            (KeyCode::F(3), mods) => {
                if prompt.kind == PromptKind::Find {
                    let back = mods.contains(KeyModifiers::SHIFT);
                    self.find_from_prompt(if back { -1 } else { 1 });
                }
                return;
            }
            _ => match key.as_typed() {
                Some(c) => prompt.insert(c),
                None => return,
            },
        }
        self.on_prompt_changed();
    }

    /// Searching happens as you type, so that you can stop typing the moment
    /// you can see what you were looking for.
    fn on_prompt_changed(&mut self) {
        let Overlay::Prompt(prompt) = &self.overlay else {
            return;
        };
        if prompt.kind != PromptKind::Find {
            return;
        }
        let needle = prompt.input.clone();
        let origin = prompt.origin.clone();
        let committed = prompt.committed;
        if needle.is_empty() {
            // Clearing the box puts you back where you started — unless Enter
            // has already taken you somewhere on purpose.
            if let Some(origin) = origin.filter(|_| !committed) {
                self.view_mut().sel = origin;
                self.scroll_into_view();
            }
            return;
        }
        // From where the search started, so that typing another letter
        // narrows the same hit rather than jumping to the next one. Once Enter
        // has moved you on purpose, that place is where you now are.
        let from = if committed {
            self.view().sel.primary().start()
        } else {
            origin
                .as_ref()
                .map(|sel| sel.primary().start())
                .unwrap_or(0)
        };
        match self.search(&needle, from, true, true) {
            Some(range) => {
                self.view_mut().sel = Selections::single(range);
                self.scroll_into_view();
            }
            None => {
                if let Some(origin) = origin {
                    self.view_mut().sel = origin;
                }
            }
        }
    }

    fn accept_prompt(&mut self) {
        let Overlay::Prompt(prompt) = &mut self.overlay else {
            return;
        };
        let kind = prompt.kind;
        let input = prompt.input.trim().to_string();
        let held = prompt.held.clone();

        match kind {
            PromptKind::PluginAsked => {
                self.overlay = Overlay::None;
                self.settle_plugin_question(json!(input));
            }
            PromptKind::GotoLine => {
                self.overlay = Overlay::None;
                match input.parse::<usize>() {
                    Ok(line) if line >= 1 => self.go_to_line(line - 1),
                    _ => self.say_bad("that is not a line number"),
                }
            }
            PromptKind::OpenPath => {
                self.overlay = Overlay::None;
                if input.is_empty() {
                    return;
                }
                // Relative to the project, the way every path you would type
                // is. `join` leaves an absolute path alone, so both work.
                let path = self.project.join(expand_path(&input));
                self.open_path(&path);
            }
            PromptKind::SaveAs => {
                self.overlay = Overlay::None;
                if input.is_empty() {
                    return self.say("no name, no file");
                }
                self.save(Some(expand_path(&input)));
            }
            PromptKind::Rename => {
                self.overlay = Overlay::None;
                if input.is_empty() {
                    return;
                }
                let at = self.view().cursor();
                let App {
                    docs,
                    lsp,
                    panes,
                    focus,
                    ..
                } = self;
                let id = panes[(*focus).min(panes.len() - 1)].doc;
                let asked = docs
                    .iter()
                    .find(|d| d.id == id)
                    .and_then(|doc| lsp.rename(doc, at, &input));
                if asked.is_none() {
                    self.say("no language server that can rename this");
                }
            }
            PromptKind::Find => {
                // Keep where the search landed rather than putting it back.
                self.last_search = input;
                self.overlay = Overlay::None;
            }
            PromptKind::ReplaceFind => {
                if input.is_empty() {
                    self.overlay = Overlay::None;
                    return;
                }
                self.last_search = input.clone();
                prompt.kind = PromptKind::ReplaceWith;
                prompt.held = input;
                prompt.input.clear();
                prompt.caret = 0;
            }
            PromptKind::ReplaceWith => {
                self.overlay = Overlay::None;
                self.replace_all(&held, &input);
            }
        }
    }

    // ---- Reading a hover ----

    /// Keys while the hover has the keyboard. Answers whether it took the key.
    fn hover_key(&mut self, key: Key) -> bool {
        let Some(hover) = &mut self.hover else {
            return false;
        };
        // A page is a screenful less a row, so that the line you were reading
        // when you pressed it is still there to pick up from.
        let page = hover.rows().saturating_sub(1).max(1) as isize;
        let furthest = hover.furthest();
        match (key.code, key.mods) {
            (KeyCode::Esc, _) => self.hover = None,
            (KeyCode::Up, _) => hover.scroll_by(-1),
            (KeyCode::Down, _) => hover.scroll_by(1),
            (KeyCode::PageUp, _) => hover.scroll_by(-page),
            (KeyCode::PageDown, _) | (KeyCode::Char(' '), KeyModifiers::NONE) => {
                hover.scroll_by(page)
            }
            (KeyCode::Home, _) => hover.scroll = 0,
            (KeyCode::End, _) => hover.scroll = furthest,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => match hover.selected_text() {
                Some(text) => {
                    self.clipboard = text.clone();
                    crate::term::to_clipboard(&text);
                    let count = text.chars().count();
                    self.say(format!("copied {count} characters"));
                }
                None => self.say("drag over the part you want, then Ctrl-C"),
            },
            (KeyCode::Enter, _) => self.hover_to_buffer(),
            // Anything else is you carrying on with the file.
            _ => {
                self.hover = None;
                return false;
            }
        }
        true
    }

    /// Put what the hover says into a buffer of its own.
    ///
    /// A box that floats over the text can only ever be read; a buffer is the
    /// thing this editor already knows how to scroll, search, select and copy
    /// out of, and it stays open in a tab while you go back to the code it is
    /// about. Rather than teaching a popup to be an editor, the popup becomes
    /// one.
    fn hover_to_buffer(&mut self) {
        let Some(hover) = self.hover.take() else {
            return;
        };
        let text = hover
            .lines
            .iter()
            .map(|line| if line.text == RULE { "" } else { &line.text })
            .collect::<Vec<_>>()
            .join("\n");
        // The first line of a hover is nearly always the signature, which is
        // the best short name there is for what the tab holds.
        let title = hover
            .lines
            .iter()
            .map(|line| line.text.trim())
            .find(|line| !line.is_empty() && *line != RULE)
            .unwrap_or("documentation");
        let name = format!("docs: {}", text::truncate(title, 40));

        let id = self.new_id();
        let mut doc = Document::scratch(id, name, self.default_indent());
        doc.set_text(&text);
        // Markdown, because that is what a language server sends and what
        // makes the fences and the headings read as themselves.
        doc.language = lang::by_name("markdown").unwrap_or(LangId::PLAIN);
        doc.reparse();
        doc.mark_saved();
        self.docs.push(doc);
        self.show(id);
    }

    // ---- Context menus ----

    fn menu_key(&mut self, key: Key) {
        let Overlay::Menu(m) = &mut self.overlay else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Up => m.step(-1),
            KeyCode::Down => m.step(1),
            KeyCode::Home => {
                m.cursor = 0;
                if m.chosen().is_none() {
                    m.step(1);
                }
            }
            KeyCode::End => {
                m.cursor = m.len().saturating_sub(1);
                if m.chosen().is_none() {
                    m.step(-1);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let chosen = m.chosen();
                self.overlay = Overlay::None;
                if let Some(action) = chosen {
                    self.do_menu(action);
                }
            }
            _ => {}
        }
    }

    fn do_menu(&mut self, action: menu::Action) {
        match action {
            menu::Action::Run(cmd) => self.run(cmd),
            menu::Action::RunOn(id, cmd) => {
                if self.doc(id).is_some() {
                    self.show(id);
                }
                self.run(cmd);
            }
            menu::Action::Divide => {}
            // A row a plugin put there. The menu is already gone by the time
            // this runs, so the answer is the last thing that happens to it.
            menu::Action::Chosen(value) => self.settle_plugin_question(json!(value)),
        }
    }

    /// What the key on the keyboard for this command is called, for the right
    /// of a menu row. `None` where nothing is bound to it, which is a row you
    /// can still choose — it just has nothing to teach.
    fn key_for(&self, cmd: Cmd) -> Option<String> {
        self.keys.shortcut(cmd)
    }

    /// The menu for a place in the text: right-clicking code, or the
    /// context-menu key.
    ///
    /// The same commands the keyboard has, in the order a person looks for
    /// them: what to do with the selection, then what the language server
    /// knows, then the rest.
    fn text_menu(&self, anchor: (u16, u16)) -> Menu {
        // Each row asks about the thing it offers rather than about servers in
        // general. A file can have two servers attached where only one of them
        // knows what a definition is, and a row lit because *something* is
        // running is a row that does nothing when you click it.
        //
        // "Can anything here do this", not "can the first one" — and what is
        // behind the row asks all of them too, so a row that is lit because
        // the linter can do it is a row the linter answers.
        let can = |capability: &str| self.lsp.can(self.here(), capability);
        let writable = !self.here().read_only;
        let selected = !self.view().sel.primary().is_empty();
        let word = text::word_text_at(&self.here().rope, self.view().cursor()).is_some();
        let can_undo = self.here().can_undo();
        let can_redo = self.here().can_redo();

        let row = |label: &str, cmd: Cmd| menu::Item::new(label, cmd).key(self.key_for(cmd));
        Menu::new(
            vec![
                row("Cut", Cmd::CUT).enabled(writable),
                row("Copy", Cmd::COPY),
                row("Paste", Cmd::PASTE).enabled(writable),
                menu::Item::divider(),
                row("Undo", Cmd::UNDO).enabled(writable && can_undo),
                row("Redo", Cmd::REDO).enabled(writable && can_redo),
                menu::Item::divider(),
                row("Go to definition", Cmd::GOTO_DEFINITION).enabled(can("definitionProvider")),
                row("Find references", Cmd::REFERENCES).enabled(can("referencesProvider")),
                row("Rename…", Cmd::RENAME).enabled(can("renameProvider") && writable),
                row("Fix it", Cmd::FIX_IT).enabled(self.fixes.is_some()),
                row("What can be done here…", Cmd::CODE_ACTION)
                    .enabled(can("codeActionProvider") && writable),
                row("What is this?", Cmd::HOVER).enabled(can("hoverProvider")),
                menu::Item::divider(),
                row("Select line", Cmd::SELECT_LINE),
                row("Select all", Cmd::SELECT_ALL),
                row("Comment out", Cmd::TOGGLE_COMMENT).enabled(writable),
                row("Reformat the file", Cmd::FORMAT)
                    .enabled(can("documentFormattingProvider") && writable),
                // Two rows rather than one, because they are two different
                // things and a file usually wants both: the formatter lays
                // the code out, and this is what takes the unused import
                // away. Lit whenever anything attached to the file does code
                // actions at all — which server has the fixes is not
                // something a person should have to know.
                row("Fix what can be fixed", Cmd::FIX_ALL)
                    .enabled(can("codeActionProvider") && writable),
                row("Tidy the imports", Cmd::ORGANIZE_IMPORTS)
                    .enabled(can("codeActionProvider") && writable),
                menu::Item::divider(),
                row("Find this word", Cmd::FIND_WORD_UNDER_CURSOR).enabled(word || selected),
                row("Find it in every file", Cmd::GREP),
            ],
            anchor,
        )
    }

    /// The menu for a tab.
    fn tab_menu(&self, id: DocId, anchor: (u16, u16)) -> Menu {
        let named = self.doc(id).is_some_and(|d| d.path.is_some());
        let modified = self.doc(id).is_some_and(Document::is_modified);
        let others = self.docs.len() > 1;
        let any_saved = self.docs.iter().any(|d| !d.is_modified());

        let at = self.docs.iter().position(|d| d.id == id);
        let first = at == Some(0);
        let last = at.is_some_and(|at| at + 1 == self.docs.len());

        let row = |label: &str, cmd: Cmd| menu::Item::on(id, label, cmd).key(self.key_for(cmd));
        Menu::new(
            vec![
                row("Save", Cmd::SAVE).enabled(modified || !named),
                row("Read again from disk", Cmd::RELOAD).enabled(named),
                menu::Item::divider(),
                row("Move left", Cmd::MOVE_TAB_LEFT).enabled(!first),
                row("Move right", Cmd::MOVE_TAB_RIGHT).enabled(!last),
                menu::Item::divider(),
                row("Close", Cmd::CLOSE),
                row("Close the others", Cmd::CLOSE_OTHERS).enabled(others),
                row("Close the saved ones", Cmd::CLOSE_SAVED).enabled(any_saved),
                row("Close them all", Cmd::CLOSE_ALL),
                menu::Item::divider(),
                row("Copy its path", Cmd::COPY_PATH).enabled(named),
                row("Copy its path from here", Cmd::COPY_RELATIVE_PATH).enabled(named),
                menu::Item::divider(),
                row("Open it in another pane", Cmd::SPLIT),
            ],
            anchor,
        )
    }

    // ---- Comparing two panes ----

    /// Turn a comparison of two panes on, or off again.
    ///
    /// The pane with the keyboard against the one beside it. Which is which on
    /// the screen decides which is "left": a comparison whose sides were the
    /// order you happened to click in would read backwards half the time.
    fn toggle_diff(&mut self) {
        if self.diff.is_some() {
            self.diff = None;
            return self.say("comparing: off");
        }
        // Only panes showing a file. Comparing the code against a tree of
        // file names is not a thing anybody means by "compare the two panes".
        let ordinary: Vec<usize> = (0..self.panes.len())
            .filter(|at| self.panes[*at].dock.is_none())
            .collect();
        if ordinary.len() < 2 {
            return self.say("two panes to compare — Alt-V opens another");
        }
        let here = self.focus.min(self.panes.len() - 1);
        let at = ordinary.iter().position(|p| *p == here).unwrap_or(0);
        let here = ordinary[at];
        let there = ordinary[(at + 1) % ordinary.len()];
        let (left, right) = (here.min(there), here.max(there));
        let Some(diff) = self.compare(left, right) else {
            return self.say("nothing to compare");
        };
        let said = match (diff.same(), diff.differing()) {
            (true, _) => "comparing: the two are the same".to_string(),
            (_, 1) => "comparing: one line differs".to_string(),
            (_, n) => format!("comparing: {n} lines differ"),
        };
        self.diff = Some(diff);
        self.say_good(said);
    }

    fn compare(&self, left: usize, right: usize) -> Option<crate::diff::Diff> {
        let a = self.doc(self.panes.get(left)?.doc)?;
        let b = self.doc(self.panes.get(right)?.doc)?;
        Some(crate::diff::Diff::new((left, a), (right, b)))
    }

    /// Keep a comparison in step with the panes and the text.
    ///
    /// A pane closed or pointed at another file ends it — the thing being
    /// compared is gone. An edit to either side only makes it out of date, and
    /// out of date is worked out again: a diff that stopped answering the
    /// moment you fixed one of the differences would be a diff you had to keep
    /// switching back on.
    fn check_diff(&mut self) {
        let Some(diff) = &self.diff else { return };
        let showing: Vec<(usize, DocId)> = self
            .panes
            .iter()
            .enumerate()
            .map(|(at, pane)| (at, pane.doc))
            .collect();
        if !diff.describes(&showing) {
            self.diff = None;
            return;
        }
        let (left, right) = diff.panes();
        let current = match (
            self.doc(self.panes[left].doc),
            self.doc(self.panes[right].doc),
        ) {
            (Some(a), Some(b)) => diff.current_for(a, b),
            _ => false,
        };
        if !current {
            self.diff = self.compare(left, right);
        }
        self.follow_diff();
    }

    /// Scroll the pane you are not in to sit beside the one you are.
    ///
    /// This is the whole difference between two files open at once and a diff.
    /// Only the pane without the keyboard is moved, so the one you are reading
    /// never jumps under you.
    fn follow_diff(&mut self) {
        let Some(diff) = &self.diff else { return };
        let here = self.focus.min(self.panes.len() - 1);
        let Some(there) = diff.other_pane(here) else {
            return;
        };
        let Some(top) = diff.beside(here, self.panes[here].top) else {
            return;
        };
        let Some(other) = self.panes.get(there) else {
            return;
        };
        let lines = self
            .doc(other.doc)
            .map(|d| d.len_lines())
            .unwrap_or(1)
            .saturating_sub(1);
        let top = top.min(lines);
        if self.panes[there].top != top {
            self.panes[there].top = top;
            self.panes[there].top_row = 0;
        }
    }

    /// To the next or previous line that differs from the last commit.
    ///
    /// A run of changed lines is one change, so this walks the edits you have
    /// made rather than the lines they touched.
    fn change_step(&mut self, forwards: bool) {
        // While two panes are being compared, "the next change" means the next
        // difference between them. It is the same question about a different
        // pair of texts, so it is the same key.
        if let Some(diff) = &self.diff {
            let here = self.focus.min(self.panes.len() - 1);
            let at = text::line_of(&self.here().rope, self.view().cursor());
            let Some(line) = diff.next_change(here, at, forwards) else {
                return self.say("the two panes are the same");
            };
            self.view_mut().mark_jump();
            self.go_to_line(line);
            return;
        }
        if !self.git.watching() {
            return self.say("this file is not in a git repository");
        }
        let id = self.view().doc;
        let here = text::line_of(&self.here().rope, self.view().cursor());
        let Some(line) = self.git.next_change(id, here, forwards) else {
            return self.say(match self.git.tracking(id) {
                true => "nothing here differs from the last commit".into(),
                false => format!("git has never seen {}", self.here().name),
            });
        };
        self.view_mut().mark_jump();
        self.go_to_line(line);
        let count = self.git.changed_lines(id);
        self.say(format!("{count} changed {}", plural("line", count)));
    }

    /// Go looking for a name across the project, having been given only the
    /// name.
    ///
    /// This is what Ctrl-clicking a type in a docstring has to mean. There is
    /// no "definition" to ask for — the name is in a paragraph of prose, not
    /// in the code — so the question becomes the one a person would ask
    /// instead: where in this project is there something called that?
    fn look_up(&mut self, name: &str) {
        // The best answer by far is the one Ctrl-clicking the code would have
        // given, and it is available whenever the file itself uses the name:
        // ask the server what is defined at that spot. That is what reaches a
        // type in another crate, with the right one of the nine things called
        // `HashMap` rather than a list of all nine.
        if let Some(at) = self.first_use_of(name) {
            let want = name.to_string();
            let (doc, lsp) = self.doc_and_lsp();
            if lsp
                .goto_or(doc, at, Goto::Definition, Some(want))
                .is_some()
            {
                self.view_mut().mark_jump();
                return;
            }
        }
        self.look_up_by_name(name);
    }

    /// Where in this file the name is used, as a word rather than as part of
    /// a longer one.
    ///
    /// A position in real code is the only thing a language server can answer
    /// "what is this?" about; a word in a paragraph of prose is not one.
    fn first_use_of(&self, name: &str) -> Option<usize> {
        if name.is_empty() {
            return None;
        }
        let text = self.here().rope.to_string();
        let part = |c: char| c.is_alphanumeric() || c == '_';
        let mut from = 0;
        while let Some(found) = text[from..].find(name) {
            let at = from + found;
            let before = text[..at].chars().next_back();
            let after = text[at + name.len()..].chars().next();
            if !before.is_some_and(part) && !after.is_some_and(part) {
                return Some(text[..at].chars().count());
            }
            from = at + name.len();
        }
        None
    }

    /// Go looking for a name by name, because there is nowhere to ask about
    /// it from.
    fn look_up_by_name(&mut self, name: &str) {
        let (doc, lsp) = self.doc_and_lsp();
        if lsp
            .workspace_symbols(doc, name, Some(name.to_string()))
            .is_none()
        {
            return self.say(format!("no language server that can look up {name}"));
        }
        // The list opens straight away, with the name already in it, so that
        // the wait is a list filling in rather than nothing happening. One
        // answer replaces it with the place itself.
        self.overlay = Overlay::Picker(Picker::searching(Kind::WorkspaceSymbols, name));
    }

    /// The context-menu key: no pointer, so the menu opens at the cursor.
    fn open_context_menu(&mut self) {
        let anchor = crate::ui::cursor_cell(self).unwrap_or((self.screen.x, self.screen.y));
        self.overlay = Overlay::Menu(self.text_menu(anchor));
    }

    fn confirm_key(&mut self, key: Key) {
        let Overlay::Confirm(confirm) = &self.overlay else {
            return;
        };
        let then = confirm.then;
        let answer = match key.code {
            // The editor's own questions have a third way out — save, discard,
            // or change your mind. A plugin's has two, so changing your mind
            // *is* the answer of no, and the plugin is told so rather than
            // left looking at a box that will not close.
            KeyCode::Esc if matches!(then, Then::PluginAsked) => Some('n'),
            KeyCode::Esc => Some('c'),
            KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        let Some(answer) = answer else { return };
        if !confirm.choices.iter().any(|(c, _)| *c == answer) {
            return;
        }
        self.overlay = Overlay::None;
        match (then, answer) {
            // Its own arm before the general "cancel", because a plugin's
            // question has no cancel: escaping it is an answer of no, and the
            // plugin has to hear one or the other.
            (Then::PluginAsked, _) => self.settle_plugin_question(json!(answer == 'y')),
            (_, 'c') => {}
            (Then::Close(id), 's') => {
                self.save(None);
                if !self.doc(id).is_some_and(Document::is_modified) {
                    self.close_doc(id);
                }
            }
            (Then::Close(id), 'd') => self.close_doc(id),
            (Then::Quit, 's') => {
                self.save_all();
                if !self.docs.iter().any(Document::is_modified) {
                    self.quit = true;
                }
            }
            (Then::Quit, 'd') => self.quit = true,
            (Then::Reload(id), 'r') => self.do_reload(id),
            _ => {}
        }
    }

    fn help_key(&mut self, key: Key) {
        let Overlay::Help(scroll) = &mut self.overlay else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::F(1) | KeyCode::Char('q') => {
                self.overlay = Overlay::None
            }
            KeyCode::Down | KeyCode::Char('j') => *scroll += 1,
            KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => *scroll += 15,
            KeyCode::PageUp => *scroll = scroll.saturating_sub(15),
            KeyCode::Home => *scroll = 0,
            _ => {}
        }
    }

    // ---- The lists ----

    fn open_files_picker(&mut self) {
        // What was found last time, so the box has something in it straight
        // away, and a fresh walk every time regardless: a project is not a
        // fixed thing. A build writes files, a checkout brings some and takes
        // others, and a list from when textfold started is a list of the files
        // that existed then rather than the ones that are there now.
        let rows = match &self.files {
            Some(files) => file_rows(files, &self.project),
            None => Vec::new(),
        };
        self.start_walk();
        self.overlay = Overlay::Picker(Picker::new(Kind::Files, rows));
    }

    fn start_walk(&mut self) {
        if self.files_walking {
            return;
        }
        self.files_walking = true;
        let root = self.project.clone();
        let tx = self.tx.clone();
        // Walking a large repository takes long enough to notice, and there is
        // no reason to notice it: the box opens straight away and fills in.
        std::thread::Builder::new()
            .name("walk".into())
            .spawn(move || {
                let mut found = Vec::new();
                for entry in ignore::WalkBuilder::new(&root).build().flatten() {
                    if found.len() >= 50_000 {
                        break;
                    }
                    if entry.file_type().is_some_and(|t| t.is_file()) {
                        found.push(entry.into_path());
                    }
                }
                found.sort();
                tx.send(Event::Files(found)).ok();
            })
            .ok();
    }

    fn open_commands_picker(&mut self) {
        // A tool for another language is not something you can do here, so it
        // is not offered here. Everything else is: a command you cannot use
        // right now still tells you it exists, which is half of what a palette
        // is for.
        let language = lang::get(self.here().language).name.clone();
        let rows: Vec<Row> = crate::cmd::all()
            .iter()
            .filter(|cmd| cmd.tool().is_none_or(|tool| tool.wants(&language)))
            .filter(|cmd| {
                cmd.plugin_command()
                    .is_none_or(|command| command.wants(&language))
            })
            .map(|cmd| {
                Row::new(cmd.name(), Choice::Command(*cmd))
                    .detail(cmd.about())
                    .tag(cmd.group().label())
                    .key(self.keys.shortcut(*cmd))
            })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Commands, rows));
    }

    fn open_buffers_picker(&mut self) {
        let mut order: Vec<&Document> = self.docs.iter().collect();
        // Most recently looked at first, and the one you are in second — which
        // is what makes one press and Enter flip back to the last file.
        order.sort_by_key(|d| std::cmp::Reverse(self.seen.get(&d.id).copied().unwrap_or(0)));
        let here = self.view().doc;
        let rows: Vec<Row> = order
            .iter()
            .map(|doc| {
                let mut row = Row::new(doc.name.clone(), Choice::Buffer(doc.id));
                if let Some(path) = &doc.path {
                    row = row.detail(short(path, &self.project));
                }
                if doc.is_modified() {
                    row = row.tag("edited");
                } else if doc.id == here {
                    row = row.tag("here");
                }
                row
            })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Buffers, rows));
    }

    fn open_theme_picker(&mut self) {
        let rows: Vec<Row> = self
            .themes
            .entries
            .iter()
            .map(|named| {
                let mut row = Row::new(named.name.clone(), Choice::Theme(named.name.clone()));
                if let Some(about) = &named.about {
                    row = row.detail(about.clone());
                }
                row
            })
            .collect();
        let mut picker = Picker::new(Kind::Themes, rows);
        // Trying each one on as you move through the list is the only way to
        // choose colours; the one you started with goes back if you escape.
        picker.restore = Some(self.config.theme_name().to_string());
        let at = self
            .themes
            .entries
            .iter()
            .position(|n| n.name == self.config.theme_name())
            .unwrap_or(0);
        picker.select(at);
        self.overlay = Overlay::Picker(picker);
    }

    fn open_language_picker(&mut self) {
        let here = self.here().language;
        let rows: Vec<Row> = lang::names()
            .into_iter()
            .map(|(id, name)| {
                let mut row = Row::new(name, Choice::Language(id));
                let language = lang::get(id);
                let mut about = Vec::new();
                if language.has_grammar() {
                    about.push("coloured".to_string());
                }
                if let Some(server) = language.servers.first() {
                    about.push(server.command.clone());
                }
                if !about.is_empty() {
                    row = row.detail(about.join(", "));
                }
                if id == here {
                    row = row.tag("this file");
                }
                row
            })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Languages, rows));
    }

    fn open_settings_picker(&mut self) {
        let numbers = self.config.line_numbers();
        let rows = vec![
            setting_row("wrap", "Fold long lines", self.config.wrap()),
            setting_row(
                "line_numbers",
                "Show line numbers",
                numbers != LineNumbers::Off,
            ),
            setting_row(
                "relative_numbers",
                "Count line numbers from the cursor",
                matches!(numbers, LineNumbers::Relative | LineNumbers::Both),
            ),
            setting_row(
                "show_whitespace",
                "Show spaces and tabs",
                self.config.show_whitespace(),
            ),
            setting_row("mouse", "Let textfold have the mouse", self.config.mouse()),
            setting_row(
                "auto_completion",
                "Suggest as you type",
                self.config.auto_completion(),
            ),
            setting_row(
                "auto_pairs",
                "Close brackets and quotes",
                self.config.auto_pairs(),
            ),
            setting_row(
                "format_on_save",
                "Reformat when saving",
                self.config.format_on_save(),
            ),
            setting_row(
                "code_actions_on_save",
                "Apply the servers' own fixes when saving",
                !self.config.code_actions_on_save().is_empty(),
            ),
            setting_row(
                "trim_trailing_whitespace",
                "Drop trailing spaces when saving",
                self.config.trim_trailing_whitespace(),
            ),
            setting_row(
                "spaces",
                "Indent new files with spaces",
                self.config.spaces(),
            ),
            setting_row(
                "restore_session",
                "Open the same files again next time",
                self.config.restore_session(),
            ),
            setting_row(
                "underline_colour",
                "Colour the underline under a problem",
                self.config.underline_colour(),
            ),
        ];
        self.overlay = Overlay::Picker(Picker::new(Kind::Settings, rows));
    }

    /// Every language and language server there is, and which are on.
    ///
    /// One list rather than two, with the servers under the plugin that brings
    /// them: what you want to switch off is nearly always one server, and
    /// finding it means finding its language first.
    fn open_plugins_picker(&mut self) {
        let mut rows = Vec::new();
        for plugin in crate::plugin::all() {
            let on = crate::plugin::is_on(&plugin.id);
            let missing = plugin.missing();
            rows.push(
                Row::new(plugin.name.clone(), Choice::Plugin(plugin.id.clone()))
                    .detail(match missing.is_empty() {
                        true => match plugin.version_label() {
                            Some(version) => {
                                format!("{} {version} — {}", plugin.id, plugin.detail())
                            }
                            None => format!("{} — {}", plugin.id, plugin.detail()),
                        },
                        // A row that says `on` beside a language server nobody
                        // has installed is a row that lies, and the lie is the
                        // one people spend an afternoon on.
                        false => format!(
                            "{} — {} — needs {}",
                            plugin.id,
                            plugin.detail(),
                            missing.join(", ")
                        ),
                    })
                    .tag(match (on, missing.is_empty()) {
                        (false, _) => "off",
                        (true, true) => "on",
                        (true, false) => "needs",
                    }),
            );
            // A plugin that brings a program of its own says so before it is
            // switched on, not after. "This adds a language" and "this runs a
            // program of its own" are different decisions, and the list is
            // where they are told apart.
            if let Some(host) = &plugin.host {
                let running = self
                    .hosts
                    .all()
                    .iter()
                    .find(|h| h.plugin == plugin.id && h.is_ready());
                rows.push(
                    Row::new(
                        format!("  {}", host.command),
                        Choice::Plugin(plugin.id.clone()),
                    )
                    .detail(match running {
                        Some(h) => format!("its own program — running in {}", h.root.display()),
                        None => format!("its own program — runs {}", host.command),
                    })
                    .tag(match (on, running.is_some(), self.hosts.given_up_on(&plugin.id)) {
                        (false, _, _) => "off",
                        // Said plainly rather than shown as on: a row that
                        // looks fine and does nothing is the worst of the
                        // three things this tag can say.
                        (_, _, true) => "gave up",
                        (_, true, _) => "running",
                        _ => "on",
                    }),
                );
            }
            for tool in &plugin.tools {
                let ready = on && crate::plugin::is_on(&tool.id);
                rows.push(
                    Row::new(format!("  {}", tool.name), Choice::Plugin(tool.id.clone()))
                        .detail(format!("{} — runs {}", tool.id, tool.command))
                        .tag(if ready { "on" } else { "off" }),
                );
            }
            rows.extend(server_rows(plugin, |id| on && crate::plugin::is_on(id)));
        }
        self.overlay = Overlay::Picker(Picker::new(Kind::Plugins, rows));
    }

    /// Turn a plugin or one of its servers on or off, and mean it now.
    ///
    /// Everything downstream is built from the plugins rather than checking
    /// them as it goes, so the way to change one's mind is to build it all
    /// again: the language table, and then the servers, which are stopped and
    /// started so that a linter you have just switched off stops sending
    /// diagnostics rather than leaving its last ones on the screen.
    fn toggle_plugin(&mut self, id: &str) {
        let on = !crate::plugin::is_on(id);
        if on && let Some((plugin, _)) = id.split_once('/') && !crate::plugin::is_on(plugin) {
            // Switching on a server whose plugin is off would look like
            // nothing happening, so switch the plugin on with it.
            crate::plugin::set(plugin, true, &mut self.config.plugins);
        }
        crate::plugin::set(id, on, &mut self.config.plugins);
        self.remember_settings();

        // A plugin switched off stops its own program too, and one switched
        // on again gets its crash count cleared — which is what makes
        // "switch it off and on again" the way to give a plugin you have just
        // fixed another go.
        let plugin = id.split_once('/').map(|(p, _)| p).unwrap_or(id);
        self.hosts.stop_plugin(plugin);
        self.plugins_changed();

        let name = crate::plugin::find(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string());
        self.say(format!("{name}: {}", if on { "on" } else { "off" }));
    }

    /// Build everything the plugins decide, again.
    ///
    /// Everything downstream is built from the plugins rather than checking
    /// them as it goes, so the way to change one's mind is to build it all
    /// again: the language table, the commands, the keys and the colours, and
    /// then the servers, which are stopped and started so that a linter that
    /// has just gone stops sending diagnostics rather than leaving its last
    /// ones on the screen.
    ///
    /// The same work whether a switch was thrown or a plugin was installed,
    /// which is the point of it having a name.
    fn plugins_changed(&mut self) {
        crate::lang::rebuild();
        crate::cmd::rebuild();
        self.keys = Keys::new(&self.config.keys);
        self.themes = Themes::load();
        let wanted = self.config.theme_name().to_string();
        self.set_theme(&wanted);
        for doc in &mut self.docs {
            doc.redetect_language();
        }
        self.lsp.restart();
        let docs: Vec<DocId> = self.docs.iter().map(|d| d.id).collect();
        for doc in docs {
            self.lsp_open(doc);
        }
    }

    // ---- Installing one ----

    /// Everything textfold could fetch: a plugin that is here and needs a
    /// program, and a package sitting somewhere nobody has installed from yet.
    ///
    ///
    /// One list, because from where you are sitting "install pyright" and
    /// "install this plugin somebody gave me" are the same sentence. Which of
    /// the two a row happens to be is textfold's business.
    fn open_install_picker(&mut self) {
        let found = crate::pack::available(crate::pack::Sources::of(&self.config));
        if found.is_empty() {
            return self.say(format!(
                "nothing to install — every plugin has what it needs, and there is nothing new in {}",
                crate::repo::repositories(self.config.package_repositories())
                    .iter()
                    .map(|r| r.name.clone())
                    .chain(
                        crate::pack::package_dirs(self.config.package_paths())
                            .iter()
                            .map(|d| d.display().to_string())
                    )
                    .collect::<Vec<_>>()
                    .join(" or ")
            ));
        }
        let rows: Vec<Row> = found
            .iter()
            .map(|p| {
                Row::new(p.name.clone(), Choice::Install(p.id.clone()))
                    .detail(format!("{} — {}", p.id, p.detail()))
                    .tag(p.tag())
            })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Install, rows));
    }

    /// Everything that could be taken off this machine again.
    ///
    /// Not the same as the plugins list. A language definition built into the
    /// binary has nothing removing it could mean — switching it off is what
    /// you want, and that is the other list.
    fn open_uninstall_picker(&mut self) {
        let found = crate::pack::removable_plugins();
        if found.is_empty() {
            return self.say(
                "nothing to remove — no plugin here was installed by textfold, or knows how to undo one",
            );
        }
        let rows: Vec<Row> = found
            .iter()
            .map(|p| {
                Row::new(p.name.clone(), Choice::Uninstall(p.id.clone()))
                    .detail(format!("{} — {}", p.id, p.origin.label()))
                    .tag(p.tag())
            })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Uninstall, rows));
    }

    /// What has a newer version to be had than the one installed.
    ///
    /// A list rather than a button, because updating is the one thing in here
    /// that changes what runs on your machine without your having asked for
    /// that particular plugin today. What is offered is said, and choosing is
    /// yours; there is no arm of this that installs anything on its own.
    fn open_update_picker(&mut self) {
        let found = crate::pack::updates(crate::pack::Sources::of(&self.config));
        if found.is_empty() {
            // Which is the ordinary answer, and worth telling apart from a
            // refresh that never happened.
            return self.say(match self.checked_for_updates {
                true => "everything is at the newest version there is".to_string(),
                false => "nothing newer has been heard of yet — the repositories are still being asked".to_string(),
            });
        }
        let rows: Vec<Row> = found
            .iter()
            .map(|p| {
                Row::new(p.name.clone(), Choice::Install(p.id.clone()))
                    .detail(format!("{} — {}", p.id, p.detail()))
                    .tag(p.tag())
            })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Install, rows));
    }

    /// Ask the repositories what they have, on a thread.
    ///
    /// Nothing waits for it and nothing is installed by it. What it changes is
    /// whether the plugins list has an `update` beside anything, and whether
    /// there is a line in the status bar saying so — an editor that fetched
    /// and ran new versions of things on its own at startup would be a
    /// different and much worse program.
    pub fn check_for_updates(&mut self) {
        if !self.config.check_for_updates() {
            return;
        }
        let repositories = self.config.package_repositories().to_vec();
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("refresh-packages".into())
            .spawn(move || {
                let problems = crate::pack::refresh(&repositories);
                tx.send(Event::Refreshed(problems)).ok();
            })
            .ok();
    }

    /// The repositories have been asked. Say if there is anything new, once.
    fn refreshed(&mut self, problems: Vec<String>) {
        self.checked_for_updates = true;
        let updates = crate::pack::updates(crate::pack::Sources::of(&self.config));
        if !updates.is_empty() {
            let key = self
                .keys
                .shortcut(Cmd::UPDATE_PLUGINS)
                .map(|key| format!(" ({key})"))
                .unwrap_or_default();
            let names: Vec<&str> = updates.iter().map(|p| p.id.as_str()).take(3).collect();
            let rest = updates.len().saturating_sub(names.len());
            let listed = match rest {
                0 => names.join(", "),
                n => format!("{} and {n} more", names.join(", ")),
            };
            return self.say(format!("newer: {listed} — update-plugins{key}"));
        }
        // A repository that could not be reached is worth saying once, and
        // only where there was nothing better to say: somebody who is offline
        // knows, and does not need telling every time they open the editor.
        if let Some(first) = problems.into_iter().next() {
            self.say(first);
        }
    }

    fn start_install(&mut self, id: &str) {
        let found = crate::pack::find(id, crate::pack::Sources::of(&self.config));
        let plan = found.and_then(|package| crate::pack::install(&package));
        self.start_plan(plan);
    }

    fn start_uninstall(&mut self, id: &str) {
        self.start_plan(crate::pack::uninstall(id));
    }

    /// Set a plan going on a thread, and say what it is about to do.
    ///
    /// What it will do is said out loud before it does it. A plugin's
    /// installer runs programs on your machine, and the least an editor can do
    /// is name them on the way past rather than after the fact.
    fn start_plan(&mut self, plan: Result<crate::pack::Plan, String>) {
        let plan = match plan {
            Ok(plan) => plan,
            Err(why) => return self.say_bad(why),
        };
        if let Some(already) = &self.installing {
            return self.say(format!("{} is still going", already.id));
        }
        if plan.is_empty() {
            return self.say(format!("{} has nothing to do — it is already here", plan.name));
        }
        let mut log = format!("{}\n\n", plan.name);
        for line in plan.lines() {
            log.push_str(&format!("  {line}\n"));
        }
        // Where it is going, in the log, because "what did this put on my
        // machine and where" is the question you ask afterwards.
        if !plan.removing {
            match (plan.touches_system(), crate::pack::tools_dir()) {
                (true, _) => log.push_str("\nSome of this installs system-wide.\n"),
                (false, Some(tools)) => {
                    log.push_str(&format!("\nInto {}\n", tools.display()))
                }
                (false, None) => {}
            }
        }
        log.push('\n');
        self.installing = Some(Installing {
            id: plan.id.clone(),
            removing: plan.removing,
            log,
        });
        let doing = match plan.removing {
            true => "removing",
            false => "installing",
        };
        self.say(format!("{doing} {}…", plan.name));
        if let Err(why) = plan.spawn(self.tx.clone()) {
            self.installing = None;
            self.say_bad(why);
        }
    }

    /// Something an install had to say.
    fn on_package(&mut self, progress: crate::pack::Progress) {
        use crate::pack::Note;
        let Some(installing) = &mut self.installing else {
            return;
        };
        if installing.id != progress.id {
            return;
        }
        match progress.note {
            Note::Doing { at, of, about } => {
                installing.log.push_str(&format!("--- {about}\n"));
                let where_in = match of {
                    0 => String::new(),
                    _ => format!("{at} of {of}: "),
                };
                let id = installing.id.clone();
                self.say(format!("{id}: {where_in}{about}"));
            }
            Note::Skipped { about, why } => {
                installing
                    .log
                    .push_str(&format!("--- {about}\n    skipped: {why}\n"));
            }
            Note::Did { about, ok, output } => {
                installing.log.push_str(&output);
                if !output.ends_with('\n') {
                    installing.log.push('\n');
                }
                if !ok {
                    installing.log.push_str(&format!("    {about} failed\n"));
                }
            }
            Note::Done { ok, why } => {
                let Some(done) = self.installing.take() else {
                    return;
                };
                let name = format!(
                    "{} {}",
                    if done.removing { "remove" } else { "install" },
                    done.id
                );
                // The plugin files have changed under us, so everything built
                // out of them is built again — which is what makes a plugin
                // you have just installed work where you are standing rather
                // than the next time you start the editor.
                crate::plugin::reload();
                self.plugins_changed();
                // A plugin that has just been removed should stop, and one
                // that has just arrived should get its chance to start.
                self.hosts.stop_plugin(&done.id);
                match ok {
                    // Put where it can be read, without taking the cursor: you
                    // asked for a plugin, not for a wall of npm output.
                    true => {
                        self.put_in_a_buffer(&name, &done.log, false);
                        self.say_good(why);
                    }
                    // A failure is the one case worth showing you, because the
                    // reason is in there and nowhere else.
                    false => {
                        self.put_in_a_buffer(&name, &done.log, true);
                        self.say_bad(format!("{}: {why}", done.id));
                    }
                }
            }
        }
    }

    /// The Python environments this project could be using.
    ///
    /// The list is offered rather than a choice being made silently, because a
    /// project with two of them is a project where only the person sitting
    /// there knows which one they meant — and because being pointed at the
    /// wrong one is not a small loss of polish. A type checker that cannot see
    /// the libraries a file imports does not go quiet; it reports at length on
    /// code that is correct.
    fn open_environment_picker(&mut self) {
        let Some(root) = self.python_root() else {
            return self.say("this file is not part of a Python project");
        };
        let found = crate::venv::found(&root);
        if found.is_empty() {
            return self.say(format!(
                "no Python environment found in {} — a .venv beside the project is what is looked for",
                root.display()
            ));
        }
        let using = self.lsp.environment_for(&root).map(|e| e.root);
        let rows: Vec<Row> = found
            .into_iter()
            .map(|env| {
                let here = Some(&env.root) == using.as_ref();
                let row = Row::new(env.name.clone(), Choice::Environment(env.root.clone()))
                    .detail(format!("{} — {}", env.about, env.root.display()));
                match here {
                    true => row.tag("using"),
                    false => row,
                }
            })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Environments, rows));
    }

    /// The root of the Python project the current file is in, by the same
    /// markers the language server is given.
    fn python_root(&self) -> Option<PathBuf> {
        let path = self.here().path.clone()?;
        let language = lang::by_name("python")?;
        if self.here().language != language {
            return None;
        }
        let config = lang::get(language).servers.first()?;
        Some(lang::project_root(&path, &config.roots))
    }

    /// Point this project's language servers at an environment, and remember
    /// it. Remembered because a choice you have to make again every morning is
    /// not a choice, it is a chore.
    fn use_environment(&mut self, root: &Path) {
        let Some(project) = self.python_root() else {
            return;
        };
        self.lsp
            .environments
            .insert(project.clone(), root.to_path_buf());
        self.config.python_environments.insert(
            project.display().to_string(),
            root.display().to_string(),
        );
        self.remember_settings();

        // The servers were started pointing somewhere else, and there is no
        // way to tell one it was wrong about which Python a project uses. They
        // go and come back.
        self.lsp.restart();
        let docs: Vec<DocId> = self.docs.iter().map(|d| d.id).collect();
        for id in docs {
            self.lsp_open(id);
        }
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        self.say_good(format!("Python: {name} — the language servers are starting again"));
    }

    fn open_diagnostics_picker(&mut self) {
        let here = self.view().doc;
        let mut rows: Vec<Row> = Vec::new();
        for doc in &self.docs {
            let mut sorted: Vec<&Diagnostic> = doc.diagnostics.iter().collect();
            sorted.sort_by_key(|d| (d.severity, d.range.start()));
            for d in sorted {
                let line = text::line_of(&doc.rope, d.range.start()) + 1;
                let where_ = match &doc.path {
                    Some(path) if doc.id != here => {
                        format!("{}:{line}", short(path, &self.project))
                    }
                    _ => format!("line {line}"),
                };
                let choice = match (&doc.path, doc.id == here) {
                    (_, true) => Choice::Here(d.range.start()),
                    (Some(path), false) => Choice::There {
                        path: path.clone(),
                        line: line - 1,
                        column: 0,
                    },
                    (None, false) => Choice::Buffer(doc.id),
                };
                rows.push(
                    Row::new(d.message.lines().next().unwrap_or("").to_string(), choice)
                        .detail(where_)
                        .tag(
                            d.source
                                .clone()
                                .unwrap_or_else(|| d.severity.label().into()),
                        )
                        .severity(d.severity),
                );
            }
        }
        if rows.is_empty() {
            return self.say_good("nothing wrong that anybody has mentioned");
        }
        self.overlay = Overlay::Picker(Picker::new(Kind::Diagnostics, rows));
    }

    fn open_grep_picker(&mut self) {
        self.overlay = Overlay::Picker(Picker::new(Kind::Grep, Vec::new()));
    }

    fn open_workspace_symbols(&mut self) {
        self.overlay = Overlay::Picker(Picker::new(Kind::WorkspaceSymbols, Vec::new()));
        self.ask_workspace_symbols("");
    }

    fn picker_key(&mut self, key: Key) {
        let Overlay::Picker(picker) = &mut self.overlay else {
            return;
        };
        match (key.code, key.mods) {
            (KeyCode::Esc, _) => {
                // A theme tried on and not chosen goes back.
                let restore = picker.restore.clone();
                self.overlay = Overlay::None;
                if let Some(name) = restore {
                    self.set_theme(&name);
                }
                return;
            }
            (KeyCode::Enter, _) => return self.choose(),
            (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => picker.step(-1),
            (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => picker.step(1),
            (KeyCode::PageUp, _) => {
                let by = picker.height() as isize;
                picker.step(-by);
            }
            (KeyCode::PageDown, _) => {
                let by = picker.height() as isize;
                picker.step(by);
            }
            (KeyCode::Home, _) => picker.select(0),
            (KeyCode::End, _) => {
                let last = picker.len().saturating_sub(1);
                picker.select(last);
            }
            (KeyCode::Backspace, KeyModifiers::CONTROL)
            | (KeyCode::Char('w'), KeyModifiers::CONTROL) => picker.delete_word(),
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => picker.clear(),
            (KeyCode::Backspace, _) => picker.backspace(),
            (KeyCode::Delete, _) => picker.delete(),
            (KeyCode::Left, _) => picker.move_caret(-1),
            (KeyCode::Right, _) => picker.move_caret(1),
            _ => match key.as_typed() {
                Some(c) => {
                    // One box, several lists: a mark typed at the very start
                    // says which list you actually wanted. It saves learning
                    // four keys, and it is discoverable because the hint under
                    // the box says so.
                    if picker.kind == Kind::Files && picker.query.is_empty() {
                        match c {
                            '>' => return self.open_commands_picker(),
                            '@' => return self.run(Cmd::SYMBOLS),
                            '#' => return self.open_workspace_symbols(),
                            ':' => return self.open_prompt(PromptKind::GotoLine),
                            _ => {}
                        }
                    }
                    picker.type_char(c);
                }
                None => return,
            },
        }
        self.after_picker_moved();
    }

    /// Some lists do something as you move through them: colours are tried on,
    /// and a list the server builds is asked for again.
    fn after_picker_moved(&mut self) {
        let Overlay::Picker(picker) = &self.overlay else {
            return;
        };
        let kind = picker.kind;
        let query = picker.query.trim().to_string();
        match kind {
            Kind::Themes => {
                if let Some(Choice::Theme(name)) = picker.selected().map(|r| r.choice.clone()) {
                    self.set_theme(&name);
                }
            }
            Kind::WorkspaceSymbols => self.ask_workspace_symbols(&query),
            Kind::Grep => self.start_grep(&query),
            _ => {}
        }
    }

    /// Take the row under the cursor.
    fn choose(&mut self) {
        let Overlay::Picker(picker) = &self.overlay else {
            return;
        };
        let Some(row) = picker.selected() else {
            // Enter with nothing matching, in the file picker, means the name
            // you typed — which is how you make a new file.
            if picker.kind == Kind::Files && !picker.query.trim().is_empty() {
                let name = picker.query.trim().to_string();
                self.overlay = Overlay::None;
                let path = self.project.join(expand_path(&name));
                self.open_path(&path);
            }
            return;
        };
        let choice = row.choice.clone();
        let kind = picker.kind;
        // A settings list stays open, because changing one setting usually
        // means changing another.
        if !matches!(kind, Kind::Settings | Kind::Plugins) {
            self.overlay = Overlay::None;
        }

        match choice {
            Choice::PluginItem(value) => self.settle_plugin_question(json!(value)),
            Choice::Command(cmd) => self.run(cmd),
            Choice::Path(path) => self.open_path(&path),
            Choice::Buffer(id) => self.show(id),
            Choice::Here(at) => {
                self.view_mut().mark_jump();
                let len = self.here().len_chars();
                self.view_mut().sel = Selections::single(Range::point(at.min(len)));
                self.scroll_into_view();
                self.centre_if_off_screen();
            }
            Choice::At {
                target,
                line,
                column,
            } => {
                let (target, line, column) = (target.clone(), line, column);
                self.view_mut().mark_jump();
                self.go_to_target(target, line, column);
            }
            Choice::There { path, line, column } => {
                self.view_mut().mark_jump();
                self.open_path(&path);
                self.go_to(line, column);
            }
            Choice::Theme(name) => {
                self.set_theme(&name);
                self.remember_settings();
                self.say(format!("colours: {name}"));
            }
            Choice::Language(id) => {
                self.here_mut().set_language(id);
                let name = lang::get(id).name.clone();
                self.lsp_open_here();
                self.say(format!("this file is {name}"));
            }
            Choice::Action(server, action) => self.do_code_action(server, *action),
            Choice::Environment(root) => self.use_environment(&root),
            Choice::Setting(which) => {
                self.toggle_setting(which);
                self.redraw_list(Self::open_settings_picker);
            }
            Choice::Plugin(id) => {
                self.toggle_plugin(&id);
                self.redraw_list(Self::open_plugins_picker);
            }
            Choice::Install(id) => self.start_install(&id),
            Choice::Uninstall(id) => self.start_uninstall(&id),
        }
    }

    /// Build a list again, keeping what was typed into it and where you were.
    ///
    /// For the two lists you change things from rather than choose out of:
    /// the ticks have to be right afterwards, and closing the list to say so
    /// would mean opening it again for every switch you threw.
    fn redraw_list(&mut self, again: fn(&mut Self)) {
        let held = match &self.overlay {
            Overlay::Picker(p) => Some((p.cursor, p.query.clone())),
            _ => None,
        };
        again(self);
        if let (Overlay::Picker(picker), Some((cursor, query))) = (&mut self.overlay, held) {
            for c in query.chars() {
                picker.type_char(c);
            }
            picker.select(cursor);
        }
    }
}

/// Code actions as rows, tagged with what kind of thing each is and, where
/// more than one server is offering, which one said so. Two servers with a
/// fix each for the same line is the ordinary case for Python, and "which of
/// these came from the linter" is the question you are actually asking.
fn action_rows(offered: &[(ServerId, Value)]) -> Vec<Row> {
    let several = offered.iter().map(|(id, _)| *id).collect::<HashSet<_>>().len() > 1;
    offered
        .iter()
        .filter_map(|(id, item)| {
            let title = item.get("title").and_then(Value::as_str)?;
            let mut row = Row::new(title.to_string(), Choice::Action(*id, Box::new(item.clone())));
            if let Some(kind) = item.get("kind").and_then(Value::as_str) {
                row = row.tag(kind.split('.').next_back().unwrap_or(kind).to_string());
            }
            if several {
                row = row.detail(format!("server {}", id.0 + 1));
            }
            Some(row)
        })
        .collect()
}

fn setting_row(key: &'static str, about: &str, on: bool) -> Row {
    Row::new(about, Choice::Setting(key)).tag(if on { "on" } else { "off" })
}

impl Prompt {
    fn insert(&mut self, c: char) {
        let at = self.byte_at(self.caret);
        self.input.insert(at, c);
        self.caret += 1;
    }

    fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let at = self.byte_at(self.caret - 1);
        self.input.remove(at);
        self.caret -= 1;
    }

    fn delete(&mut self) {
        let at = self.byte_at(self.caret);
        if at < self.input.len() {
            self.input.remove(at);
        }
    }

    fn delete_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut at = self.caret;
        while at > 0 && chars.get(at - 1).is_some_and(|c| c.is_whitespace()) {
            at -= 1;
        }
        while at > 0 && chars.get(at - 1).is_some_and(|c| !c.is_whitespace()) {
            at -= 1;
        }
        let from = self.byte_at(at);
        let to = self.byte_at(self.caret);
        self.input.replace_range(from..to, "");
        self.caret = at;
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.input
            .char_indices()
            .nth(chars)
            .map(|(at, _)| at)
            .unwrap_or(self.input.len())
    }
}

/// `~/…` as a person writes it.
fn expand_path(text: &str) -> PathBuf {
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(text)
}

// ---------------------------------------------------------------------------
// Finding, going places, and everything that involves a language server.
// ---------------------------------------------------------------------------

impl App {
    /// The focused document and the servers, borrowed apart so that both can
    /// be used at once. Nearly every question for a server needs the document
    /// it is about, and this is how to have both without copying the file.
    fn doc_and_lsp(&mut self) -> (&Document, &mut Servers) {
        let App {
            docs,
            lsp,
            panes,
            focus,
            ..
        } = self;
        let id = panes[(*focus).min(panes.len() - 1)].doc;
        let doc = docs
            .iter()
            .find(|d| d.id == id)
            .expect("a pane always shows a document");
        (doc, lsp)
    }

    fn lsp_open_here(&mut self) {
        let id = self.view().doc;
        self.lsp_open(id);
    }

    fn lsp_open(&mut self, id: DocId) {
        let App { docs, lsp, .. } = self;
        if let Some(doc) = docs.iter().find(|d| d.id == id) {
            lsp.open(doc);
        }
        // Starting a server is where "it is not installed" is found out, and
        // the status line is here rather than there.
        if let Some(problem) = self.lsp.problems.pop() {
            self.lsp.problems.clear();
            self.say(problem);
        }

        // And the same moment is what starts a plugin that said it wanted to
        // know about this kind of file. One funnel for both, so that a plugin
        // cannot be woken by a route a language server is not.
        let opened = self
            .doc(id)
            .and_then(|d| d.path.clone())
            .map(|path| (path, lang::get(self.doc(id).map(|d| d.language).unwrap_or(lang::LangId::PLAIN)).name.clone()));
        if let Some((path, language)) = opened {
            self.hosts.opened(&path, &language);
            let App { docs, hosts, .. } = self;
            if let Some(doc) = docs.iter().find(|d| d.id == id) {
                hosts.opened_buffer(doc);
            }
            self.take_plugin_problems();
        }
    }

    /// Everything already open, for a plugin that has only just come up.
    ///
    /// A plugin started by the eleventh file opened should still know about
    /// the first ten — otherwise what it is told depends on the order somebody
    /// happened to open their tabs in.
    fn catch_a_host_up(&mut self, id: HostId) {
        if !self.hosts.get(id).is_some_and(|h| h.is_ready()) {
            return;
        }
        let ids: Vec<DocId> = self.docs.iter().map(|d| d.id).collect();
        for doc_id in ids {
            let App { docs, hosts, .. } = self;
            if let Some(doc) = docs.iter().find(|d| d.id == doc_id) {
                hosts.opened_buffer(doc);
            }
        }
    }

    // ---- Searching ----

    /// The next occurrence of `needle`, from `from`, in the focused document.
    fn search(&self, needle: &str, from: usize, forwards: bool, wrap: bool) -> Option<Range> {
        if needle.is_empty() {
            return None;
        }
        let doc = self.here();
        let text = doc.rope.to_string();
        // A lower-case search ignores case; a search with a capital in it
        // means the capital. Nobody has ever wanted the other rule.
        let sensitive = needle.chars().any(char::is_uppercase);
        let (hay, pin) = if sensitive {
            (text.clone(), needle.to_string())
        } else {
            (text.to_lowercase(), needle.to_lowercase())
        };
        // Lowercasing can change how many bytes a character takes, which would
        // put every offset out. Where it does, search the original and accept
        // that the search is case-sensitive for that file.
        let (hay, pin) = if hay.len() == text.len() {
            (hay, pin)
        } else {
            (text.clone(), needle.to_string())
        };

        let from_byte = doc.rope.char_to_byte(from.min(doc.len_chars()));
        let found = if forwards {
            hay.get(from_byte..)
                .and_then(|rest| rest.find(&pin))
                .map(|at| from_byte + at)
                .or_else(|| wrap.then(|| hay.find(&pin)).flatten())
        } else {
            hay.get(..from_byte)
                .and_then(|start| start.rfind(&pin))
                .or_else(|| wrap.then(|| hay.rfind(&pin)).flatten())
        }?;
        let start = doc.rope.byte_to_char(found);
        Some(Range::new(start, start + pin.chars().count()))
    }

    /// Step to the next or previous hit from inside the search box, leaving
    /// the box open.
    fn find_from_prompt(&mut self, by: isize) {
        let Overlay::Prompt(prompt) = &mut self.overlay else {
            return;
        };
        let needle = prompt.input.clone();
        if needle.is_empty() {
            // Nothing typed: fall back on the last thing that was, so Ctrl-F
            // then Enter still means "that again".
            let last = self.last_search.clone();
            if last.is_empty() {
                return;
            }
            if let Overlay::Prompt(prompt) = &mut self.overlay {
                prompt.caret = last.chars().count();
                prompt.input = last;
                prompt.committed = true;
            }
            return self.on_prompt_changed();
        }
        prompt.committed = true;
        self.last_search = needle;
        self.find_step(by);
    }

    fn find_step(&mut self, by: isize) {
        let needle = self.last_search.clone();
        if needle.is_empty() {
            return self.open_prompt(PromptKind::Find);
        }
        let here = self.view().sel.primary();
        // From just past this hit, or just before it, so that repeating steps
        // rather than finding the same one again.
        let from = if by > 0 {
            here.start() + 1
        } else {
            here.start()
        };
        match self.search(&needle, from, by > 0, true) {
            Some(range) => {
                self.view_mut().mark_jump();
                self.view_mut().sel = Selections::single(range);
                self.scroll_into_view();
                self.centre_if_off_screen();
                let count = self.count_matches(&needle);
                self.say(format!("{needle} — {count} in this file"));
            }
            None => self.say(format!("no {needle}")),
        }
    }

    /// How many times a string appears in this file, for the search box to
    /// show while you are still typing it.
    /// Which hit the cursor is sitting on, counting from one, and how many
    /// there are. "3 of 12" is what tells you whether pressing Enter again is
    /// worth doing.
    ///
    /// `None` for the number when the cursor is not on a hit, which is the
    /// case the moment you move away from one.
    pub fn match_place_of(&self, needle: &str) -> (Option<usize>, usize) {
        let total = self.count_matches(needle);
        if total == 0 {
            return (None, 0);
        }
        let doc = self.here();
        let text = doc.rope.to_string();
        let sensitive = needle.chars().any(char::is_uppercase);
        let (hay, pin) = if sensitive {
            (text, needle.to_string())
        } else {
            (text.to_lowercase(), needle.to_lowercase())
        };
        // Lowercasing can change how many bytes a character takes; where it
        // does, the byte offsets below would not line up with the rope, so
        // there is no honest answer to give.
        if hay.len() != doc.rope.len_bytes() {
            return (None, total);
        }
        let want = doc.rope.char_to_byte(self.view().sel.primary().start());
        let at = hay.match_indices(&pin).position(|(byte, _)| byte == want);
        (at.map(|n| n + 1), total)
    }

    fn count_matches(&self, needle: &str) -> usize {
        if needle.is_empty() {
            return 0;
        }
        let text = self.here().rope.to_string();
        let sensitive = needle.chars().any(char::is_uppercase);
        if sensitive {
            text.matches(needle).count()
        } else {
            text.to_lowercase().matches(&needle.to_lowercase()).count()
        }
    }

    fn replace_all(&mut self, needle: &str, with: &str) {
        if needle.is_empty() {
            return;
        }
        // Only inside the selection, if there is one worth calling a
        // selection — which is how you replace in a function rather than a
        // file without a second kind of command.
        let limit = self.view().sel.primary();
        let whole = limit.len() < 2;

        let doc = self.here();
        let text = doc.rope.to_string();
        let sensitive = needle.chars().any(char::is_uppercase);
        let hay = if sensitive {
            text.clone()
        } else {
            text.to_lowercase()
        };
        let pin = if sensitive {
            needle.to_string()
        } else {
            needle.to_lowercase()
        };
        let usable = hay.len() == text.len();
        let hay = if usable { hay } else { text.clone() };
        let pin = if usable { pin } else { needle.to_string() };

        let mut changes = Vec::new();
        let mut at = 0;
        while let Some(offset) = hay[at..].find(&pin) {
            let found = at + offset;
            let start = doc.rope.byte_to_char(found);
            let end = start + pin.chars().count();
            let inside = whole || (start >= limit.start() && end <= limit.end());
            if inside {
                changes.push(crate::doc::Change::replace(start, end, with));
            }
            at = found + pin.len().max(1);
        }

        if changes.is_empty() {
            return self.say(format!("no {needle}"));
        }
        let count = changes.len();
        let (doc, view) = self.pair();
        let before = view.sel.clone();
        let edits = doc.apply_atomic(changes, &before);
        view.absorb(&edits, doc.len_chars());
        view.sel.collapse_selections();
        self.after_edit(edits);
        self.say_good(format!(
            "replaced {count}{}",
            if whole { "" } else { " in the selection" }
        ));
    }

    fn start_grep(&mut self, query: &str) {
        let query = query.trim().to_string();
        if query.len() < 2 {
            return;
        }
        let root = self.project.clone();
        let project = self.project.clone();
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("grep".into())
            .spawn(move || {
                let sensitive = query.chars().any(char::is_uppercase);
                let needle = if sensitive {
                    query.clone()
                } else {
                    query.to_lowercase()
                };
                let mut rows = Vec::new();
                'files: for entry in ignore::WalkBuilder::new(&root).build().flatten() {
                    if rows.len() >= 500 {
                        break;
                    }
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        continue;
                    }
                    let path = entry.into_path();
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        // Not text. Not our business.
                        continue;
                    };
                    for (number, line) in text.lines().enumerate() {
                        let hay = if sensitive {
                            line.to_string()
                        } else {
                            line.to_lowercase()
                        };
                        let Some(column) = hay.find(&needle) else {
                            continue;
                        };
                        rows.push(
                            Row::new(
                                line.trim().chars().take(160).collect::<String>(),
                                Choice::There {
                                    path: path.clone(),
                                    line: number,
                                    column: line[..column.min(line.len())].chars().count(),
                                },
                            )
                            .detail(format!(
                                "{}:{}",
                                short(&path, &project),
                                number + 1
                            )),
                        );
                        if rows.len() >= 500 {
                            break 'files;
                        }
                    }
                }
                tx.send(Event::Found(query, rows)).ok();
            })
            .ok();
    }

    // ---- Going places ----

    fn go_to_line(&mut self, line: usize) {
        self.view_mut().mark_jump();
        let doc = self.here();
        let line = line.min(doc.len_lines().saturating_sub(1));
        let at = text::first_non_blank(&doc.rope, line);
        self.view_mut().sel = Selections::single(Range::point(at));
        self.centre();
        self.scroll_into_view();
    }

    /// Put the cursor on a line and column, for a place named on the command
    /// line.
    pub fn jump_to(&mut self, line: usize, column: usize) {
        self.go_to(line, column);
    }

    fn go_to(&mut self, line: usize, column: usize) {
        let doc = self.here();
        let line = line.min(doc.len_lines().saturating_sub(1));
        let start = text::line_start(&doc.rope, line);
        let end = text::line_end(&doc.rope, line);
        let at = (start + column).min(end);
        self.view_mut().sel = Selections::single(Range::point(at));
        self.centre_if_off_screen();
        self.scroll_into_view();
    }

    /// Put the cursor in the middle only when it was not already showing.
    /// Jumping somewhere on screen should not throw the screen about.
    fn centre_if_off_screen(&mut self) {
        let at = self.view().cursor();
        let line = text::line_of(&self.here().rope, at);
        let view = self.view();
        let (top, height) = (view.top, view.height());
        if line < top || line >= top + height {
            self.centre();
        }
    }

    fn jump(&mut self, forwards: bool) {
        let at = self.focus.min(self.panes.len() - 1);
        let jump = if forwards {
            self.panes[at].jump_forward()
        } else {
            self.panes[at].jump_back()
        };
        let Some(jump) = jump else {
            return self.say(if forwards {
                "nowhere forward to go"
            } else {
                "nowhere back to go"
            });
        };
        if jump.doc != self.panes[at].doc && self.doc(jump.doc).is_some() {
            let selections = Selections::single(Range::point(jump.at));
            self.panes[at].show(jump.doc, selections);
            self.touch(jump.doc);
        } else {
            let len = self.here().len_chars();
            self.panes[at].sel = Selections::single(Range::point(jump.at.min(len)));
        }
        self.centre_if_off_screen();
        self.scroll_into_view();
    }

    fn go_to_matching_bracket(&mut self) {
        let at = self.view().cursor();
        match edit::match_bracket(self.here(), at) {
            Some(found) => {
                self.view_mut().sel = Selections::single(Range::point(found));
                self.scroll_into_view();
            }
            None => self.say("the cursor is not on a bracket"),
        }
    }

    fn expand_selection(&mut self) {
        let range = self.view().sel.primary();
        let doc = self.here();
        let Some(syntax) = &doc.syntax else {
            // Without a parse tree the next best thing is the word, then the
            // line, which is what expanding means to a person anyway.
            let (doc, view) = self.pair();
            if view.sel.primary().is_empty() {
                edit::select_word(doc, view);
            } else {
                edit::select_line(doc, view);
            }
            return;
        };
        let from = doc.rope.char_to_byte(range.start());
        let to = doc.rope.char_to_byte(range.end());
        match syntax.enclosing(from, to) {
            Some((start, end)) => {
                let start = doc.rope.byte_to_char(start);
                let end = doc.rope.byte_to_char(end);
                self.view_mut().sel = Selections::single(Range::new(start, end));
                self.scroll_into_view();
            }
            None => self.say("that is the whole file"),
        }
    }

    fn step_diagnostic(&mut self, by: isize) {
        let at = self.view().cursor();
        let doc = self.here();
        if doc.diagnostics.is_empty() {
            return self.say("nothing wrong in this file");
        }
        let mut sorted: Vec<&Diagnostic> = doc.diagnostics.iter().collect();
        sorted.sort_by_key(|d| d.range.start());
        let next = if by > 0 {
            sorted
                .iter()
                .find(|d| d.range.start() > at)
                .or_else(|| sorted.first())
        } else {
            sorted
                .iter()
                .rev()
                .find(|d| d.range.start() < at)
                .or_else(|| sorted.last())
        };
        let Some(found) = next else { return };
        let (start, message, severity) =
            (found.range.start(), found.message.clone(), found.severity);
        self.view_mut().mark_jump();
        let len = self.here().len_chars();
        self.view_mut().sel = Selections::single(Range::point(start.min(len)));
        self.centre_if_off_screen();
        self.scroll_into_view();
        match severity {
            Severity::Error => self.say_bad(message),
            _ => self.say(message),
        }
    }

    fn show_server_status(&mut self) {
        if self.lsp.all().is_empty() {
            let language = lang::get(self.here().language);
            return match language.servers.first() {
                Some(server) => self.say(format!(
                    "no server running — {} would be started for a file in a project",
                    server.command
                )),
                None => self.say(format!(
                    "textfold knows no language server for {}",
                    language.name
                )),
            };
        }
        let lines: Vec<String> = self
            .lsp
            .all()
            .iter()
            .map(|server| {
                let state = match &server.state {
                    crate::lsp::State::Starting => "starting".to_string(),
                    crate::lsp::State::Dead(why) => why.clone(),
                    crate::lsp::State::Ready => server
                        .busy_with()
                        .map(str::to_string)
                        .unwrap_or_else(|| "ready".into()),
                };
                format!(
                    "{} ({}): {state}",
                    server.name,
                    short(&server.root, &self.project)
                )
            })
            .collect();
        self.say(lines.join("   "));
    }

    // ---- Asking a language server ----

    fn ask_goto(&mut self, what: Goto) {
        let at = self.view().cursor();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.goto(doc, at, what).is_none() {
            let label = what.label();
            self.say(format!("no language server that can find a {label}"));
        }
    }

    fn ask_references(&mut self) {
        let at = self.view().cursor();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.references(doc, at).is_none() {
            self.say("no language server that can find uses");
        }
    }

    fn ask_hover(&mut self, at: usize) {
        // Asking for a hover that is already on the screen is asking to read
        // it rather than glance at it.
        if let Some(hover) = &mut self.hover {
            if !hover.focused {
                hover.focused = true;
                self.say(
                    "arrows scroll, drag to select, Ctrl-C copies, Enter opens it in a tab",
                );
                return;
            }
            return self.hover_to_buffer();
        }
        // What is wrong here, if anything is, goes up now: it is already known
        // and the box should not wait on a server to say what textfold could
        // have said immediately.
        let problems = self.problem_lines(at);
        if !problems.is_empty() {
            self.hover = Some(Popup::new(problems, at));
        }
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.hover(doc, at).is_none() && self.hover.is_none() {
            // Without a server, say what the parser knows. It is not much, but
            // it is true, and it is better than a box saying nothing.
            let doc = self.here();
            let byte = doc.rope.char_to_byte(at.min(doc.len_chars()));
            match doc.syntax.as_ref().and_then(|s| s.node_at(byte)) {
                Some(kind) => self.say(format!("{kind} — no language server here")),
                None => self.say("no language server here"),
            }
        }
    }

    fn ask_symbols(&mut self) {
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.symbols(doc).is_none() {
            self.say("no language server that can list what this file defines");
        }
    }

    fn ask_workspace_symbols(&mut self, query: &str) {
        let (doc, lsp) = self.doc_and_lsp();
        let query = query.to_string();
        if lsp.workspace_symbols(doc, &query, None).is_none() && query.is_empty() {
            self.say("no language server that can search the project");
        }
    }

    fn ask_code_actions(&mut self) {
        let range = self.view().sel.primary();
        let id = self.view().doc;
        let (doc, lsp) = self.doc_and_lsp();
        let asked = lsp.code_actions(doc, range);
        if asked.is_empty() {
            return self.say("no language server with anything to offer");
        }
        self.offer = Some(Gathered::new(id, range.start(), asked));
    }

    fn format(&mut self) {
        let tab_width = self.config.tab_width();
        let spaces = matches!(self.here().indent, Indent::Spaces(_));
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.format(doc, tab_width, spaces).is_none() {
            self.say("no language server that can format this");
        }
    }

    fn start_rename(&mut self) {
        if !self.lsp.can(self.here(), "renameProvider") {
            return self.say("no language server that can rename this");
        }
        self.open_prompt(PromptKind::Rename);
    }

    /// Ask for completions. `asked_for` separates a keystroke that means
    /// "suggest something" from the editor deciding to ask on its own — only
    /// the first is worth an answer of "there is nobody to ask", and the
    /// second would otherwise put that on the screen every time you typed a
    /// word in a plain text file.
    fn ask_for_completions(&mut self, triggered: Option<char>, asked_for: bool) {
        if self.view().sel.len() > 1 {
            // Completing at forty cursors is a question with forty answers.
            return;
        }
        let at = self.view().cursor();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.completion(doc, at, triggered).is_none() && asked_for {
            self.say("no language server here");
        }
    }

    /// Take the suggestion under the cursor.
    ///
    /// Not always at once: a suggestion whose import the server has not
    /// worked out yet is taken when it has, which is a few milliseconds and
    /// no keystrokes away.
    fn accept_completion(&mut self) {
        let Some(completion) = &self.completion else {
            return;
        };
        let Some(&index) = completion.shown.get(completion.cursor) else {
            self.completion = None;
            return;
        };
        if completion.all[index].resolve != Resolve::Done {
            self.resolve_selected();
            if self.completion.as_ref().is_some_and(|completion| {
                completion.all[index].resolve == Resolve::Waiting
            }) {
                self.accept_when_resolved = Some(index);
                return;
            }
        }
        self.take_suggestion(index);
    }

    /// Put one suggestion in, with whatever else has to go in with it.
    fn take_suggestion(&mut self, index: usize) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        self.accept_when_resolved = None;
        let Some(item) = completion.all.get(index).cloned() else {
            return;
        };
        if self.view().doc != completion.doc {
            return;
        }

        // What the server said to replace, or the word we started from. The
        // server's answer is better where there is one: it knows that
        // completing `foo.ba` means replacing `ba` and not `foo.ba`.
        let at = self.view().cursor();
        let (from, to) = item.replace.unwrap_or((completion.start, at));
        let len = self.here().len_chars();
        let mut changes = vec![crate::doc::Change::replace(
            from.min(len),
            to.clamp(from, len).max(at.min(len)),
            item.insert.clone(),
        )];
        // Imports and the like go in at the same time and as one undo.
        for (start, end, text) in &item.also {
            changes.push(crate::doc::Change::replace(
                (*start).min(len),
                (*end).clamp(*start, len),
                text.clone(),
            ));
        }
        // Sorted by where each starts and then by how much it covers, which
        // matters where an import goes in at the very spot the word being
        // completed starts: the changes are applied back to front, and a
        // change of no width has to be the one that goes in last if it is to
        // end up in front of the word rather than inside it.
        changes.sort_by_key(|c| (c.from, c.to));

        let (doc, view) = self.pair();
        let before = view.sel.clone();
        let edits = doc.apply_atomic(changes, &before);
        view.absorb(&edits, doc.len_chars());
        // The cursor goes to the end of what was put in, wherever mapping
        // would otherwise have left it. Everything that went in ahead of the
        // word — the import, usually — moves that end along.
        let mut landed = (from + item.insert.chars().count()) as isize;
        for (start, end, text) in &item.also {
            if *end <= from {
                landed += text.chars().count() as isize - (end - start) as isize;
            }
        }
        let landed = landed.max(0) as usize;
        view.sel = Selections::single(Range::point(landed.min(doc.len_chars())));
        self.after_edit(edits);
    }

    /// The handful of keys the completion list answers to. Everything else
    /// falls through to the editor, so typing keeps working.
    fn completion_key(&mut self, key: Key) -> bool {
        let Some(completion) = &mut self.completion else {
            return false;
        };
        match (key.code, key.mods) {
            (KeyCode::Up, KeyModifiers::NONE) => completion.step(-1),
            (KeyCode::Down, KeyModifiers::NONE) => completion.step(1),
            (KeyCode::PageUp, _) => {
                let by = completion.height() as isize;
                completion.step(-by);
            }
            (KeyCode::PageDown, _) => {
                let by = completion.height() as isize;
                completion.step(by);
            }
            (KeyCode::Enter, _) | (KeyCode::Tab, KeyModifiers::NONE) => {
                self.accept_completion();
                return true;
            }
            (KeyCode::Esc, _) => {
                self.completion = None;
                self.accept_when_resolved = None;
                return true;
            }
            _ => return false,
        }
        // Whatever the cursor landed on, find out the rest of it now rather
        // than at the moment it is taken.
        self.resolve_selected();
        true
    }

    /// The keys that steer an offer a plugin has made, while it is showing.
    ///
    /// Written like [`App::completion_key`] and for the same reason: these are
    /// not bindings, they are what a box on the screen does while it is on the
    /// screen. Tab takes it because Tab takes a suggestion in every editor
    /// that offers one, and Tab is still indent every other time — the key is
    /// not conditional, the offer is.
    fn hint_key(&mut self, key: Key) -> bool {
        if !self.hint_showing() {
            return false;
        }
        match (key.code, key.mods) {
            (KeyCode::Tab, KeyModifiers::NONE) => self.accept_hint(),
            (KeyCode::Esc, _) => self.drop_hint("waved away"),
            _ => return false,
        }
        true
    }

    /// What is wrong at a spot, written for a hover.
    ///
    /// A language server says what it thinks of a piece of code twice over: as
    /// a squiggle under it, and as a sentence you have to go somewhere else to
    /// read. Everywhere outside a terminal the sentence is simply *there* when
    /// you point at the squiggle, and that is where a person is already
    /// looking, so it goes in the box with everything else.
    ///
    /// Worst first, so an error is not below a hint about the same word, and
    /// each one says who said it: two servers on one file disagree constantly
    /// and "which of you thinks this" is the first question anybody asks.
    fn problem_lines(&self, at: usize) -> Vec<DocLine> {
        let doc = self.here();
        let mut here: Vec<&Diagnostic> = doc
            .diagnostics
            .iter()
            .filter(|d| d.range.contains(at) || (d.range.is_empty() && d.range.start() == at))
            .collect();
        here.sort_by_key(|d| d.severity);
        let mut lines = Vec::new();
        for problem in here {
            if !lines.is_empty() {
                lines.push(DocLine::prose(String::new()));
            }
            let who = match (&problem.source, &problem.code) {
                (Some(source), Some(code)) => Some(format!("{source} {code}")),
                (Some(source), None) => Some(source.clone()),
                (None, Some(code)) => Some(code.clone()),
                (None, None) => None,
            };
            lines.push(DocLine::prose(match who {
                Some(who) => format!("{} ({who})", problem.severity.label()),
                None => problem.severity.label().to_string(),
            }));
            // A message is often several lines, and a server that wrote them
            // separately meant them separately.
            for line in problem.message.lines() {
                lines.push(DocLine::prose(line.to_string()));
            }
        }
        lines
    }

    fn hover_at_screen(&mut self, column: u16, row: u16) {
        if self.hover.as_ref().is_some_and(|h| h.focused) {
            return;
        }
        let Some(at) = self.position_at(column, row) else {
            return;
        };
        // Over a name, or over something a server has complained about. The
        // second is not always the first: a warning can sit on a bracket, on
        // an operator, or on a stretch of whitespace, and pointing at it is
        // still the way you ask what is wrong there.
        let problems = self.problem_lines(at);
        if problems.is_empty() && text::word_text_at(&self.here().rope, at).is_none() {
            return;
        }
        // What is already known goes up straight away rather than after a
        // round trip to a server that may have nothing to add, or may be busy,
        // or may not be there at all.
        if !problems.is_empty() {
            self.hover = Some(Popup::new(problems, at));
        }
        let (doc, lsp) = self.doc_and_lsp();
        lsp.hover(doc, at);
    }
}

// ---------------------------------------------------------------------------
// What a language server sends back.
// ---------------------------------------------------------------------------

impl App {
    fn on_lsp(&mut self, id: ServerId, message: Incoming) {
        match message {
            Incoming::Notification { method, params } => self.on_notification(id, &method, params),
            Incoming::Request {
                id: request_id,
                method,
                params,
            } => {
                // Answer first, act second: a server waiting on a reply is a
                // server that has stopped.
                self.lsp.respond(id, request_id.clone(), &method, &params);
                if method == "workspace/applyEdit"
                    && let Some(edit) = params.get("edit")
                {
                    let count = self.apply_workspace_edit(edit);
                    if count > 0 {
                        self.say_good(format!("changed {count} {}", places(count)));
                    }
                }
            }
            Incoming::Response {
                id: request,
                result,
            } => self.on_response(id, request, result),
            Incoming::Exited(why) => {
                let name = self
                    .lsp
                    .get(id)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "a language server".into());
                self.lsp.died(id, why.clone());
                // Not being installed is the ordinary case and not worth a red
                // line; it is worth one line saying what would have run.
                self.say(format!("{name}: {why}"));
            }
        }
    }

    /// Everything a plugin's own program says.
    ///
    /// The same three kinds of message a language server sends, read with a
    /// different vocabulary. A request is answered before anything else
    /// happens with it, because a plugin waiting on a reply is a plugin that
    /// has stopped — and unlike a language server, a plugin is usually
    /// something somebody in this building wrote and is still debugging.
    fn on_plugin(&mut self, id: HostId, message: Incoming) {
        match message {
            Incoming::Notification { method, params } => {
                // Nobody is waiting on an answer, so a refusal has nowhere to
                // go but the status line — which is where a plugin author
                // needs it anyway.
                if let Answer::No(why) = self.plugin_asked(id, &method, &params, None) {
                    self.say_bad(format!("{}: {why}", self.plugin_name(id)));
                }
            }
            Incoming::Request {
                id: request_id,
                method,
                params,
            } => {
                let answer = self.plugin_asked(id, &method, &params, Some(&request_id));
                if let Some(host) = self.hosts.get_mut(id) {
                    match answer {
                        Answer::Now(result) => host.answer(request_id, result),
                        Answer::No(why) => host.refuse(request_id, &why),
                        Answer::Later => {}
                    }
                }
            }
            Incoming::Response { id: request, result } => {
                let Some(ask) = self.hosts.get_mut(id).and_then(|h| h.claim(request)) else {
                    return;
                };
                match (ask, result) {
                    (crate::host::Ask::Initialize, Ok(result)) => {
                        self.hosts.ready(id, result);
                        self.catch_a_host_up(id);
                    }
                    (crate::host::Ask::Initialize, Err(why)) => {
                        self.hosts.died(id, why.clone());
                        self.say_bad(format!("{}: {why}", self.plugin_name(id)));
                    }
                    // A command that finished quietly finished. One that
                    // failed says so, because the person pressed a key and is
                    // owed an answer either way.
                    (crate::host::Ask::Command(_), Ok(_)) => {}
                    (crate::host::Ask::Command(name), Err(why)) => {
                        self.say_bad(format!("{name}: {why}"))
                    }
                }
            }
            Incoming::Exited(why) => {
                let name = self.plugin_name(id);
                self.hosts.died(id, why.clone());
                self.say(format!("{name}: {why}"));
            }
        }
        self.take_plugin_problems();
    }

    /// Start the clock on telling the plugins where the cursor is.
    ///
    /// Swept once per event, like the plugin questions, rather than at each of
    /// the hundred places a cursor can move from — every arrow key, every
    /// click, every jump, every edit. One place that notices is one place that
    /// cannot be forgotten in the hundred and first.
    fn notice_the_cursor_moved(&mut self) {
        let now = (self.view().doc, self.view().cursor());
        if self.selection_told == Some(now) {
            return;
        }
        // An offer was made about where the cursor *was*. Moving away from it
        // is declining it, the same as it would be in any editor.
        if self.here().hint.is_some() && self.here().hint.as_ref().is_some_and(|h| h.at != now.1) {
            self.drop_hint("moved");
        }
        self.selection_told = Some(now);
        self.selection_due = Some(Instant::now() + SELECTION_SETTLES);
    }

    /// Where the cursor is, for the plugins that asked about this language.
    ///
    /// Only those: a plugin that never asked to be told the text of a file has
    /// no use for a running commentary on where you are in it.
    fn tell_plugins_where_the_cursor_is(&mut self) {
        let id = self.view().doc;
        let at = self.view().cursor();
        let Some((path, line, column, version)) = self.doc(id).and_then(|doc| {
            let (line, column) = doc.point_at_char(at);
            Some((doc.path.clone()?, line, column, doc.version))
        }) else {
            return;
        };
        let App { docs, hosts, .. } = self;
        let Some(doc) = docs.iter().find(|d| d.id == id) else {
            return;
        };
        hosts.selection_changed(
            doc,
            json!({
                "path": path,
                "version": version,
                "line": line,
                "column": column,
            }),
        );
    }

    /// Answer the plugin waiting on a box, and forget it.    /// Answer the plugin waiting on a box, and forget it.
    ///
    /// Called with `Value::Null` for "they changed their mind", which is an
    /// answer: a plugin that put a list up and got nothing back would wait for
    /// ever, and Escape is the commonest thing anybody does to a list.
    fn settle_plugin_question(&mut self, answer: Value) {
        let Some(asked) = self.plugin_waiting.take() else {
            return;
        };
        if let Some(host) = self.hosts.get_mut(asked.host) {
            host.answer(asked.request, answer);
        }
    }

    /// A box a plugin put up has gone without being answered.
    ///
    /// Swept once per event rather than at each of the dozen places an overlay
    /// can be dismissed from. Escape, a click outside, a command that opens
    /// something else — all of them close the box, and none of them should
    /// have to remember there was a plugin behind it.
    fn sweep_plugin_question(&mut self) {
        if self.plugin_waiting.is_some() && matches!(self.overlay, Overlay::None) {
            self.settle_plugin_question(Value::Null);
        }
    }

    /// Put a plugin's question on the screen and remember who asked it.
    fn ask_for_plugin(&mut self, id: HostId, request: Option<&Value>, overlay: Overlay) -> Answer {
        let Some(request) = request.cloned() else {
            return Answer::No("that has to be asked, not told".into());
        };
        // A second question while the first is still on the screen: the older
        // one is answered with nothing rather than left hanging, because its
        // box is about to be replaced by this one.
        self.settle_plugin_question(Value::Null);
        self.overlay = overlay;
        self.plugin_waiting = Some(Asked {
            host: id,
            request,
        });
        Answer::Later
    }

    /// Which buffer a plugin means: the one it named, or the one in front of
    /// you if it named none.
    fn plugin_means(&self, params: &Value) -> Result<DocId, String> {
        let Some(path) = params.get("path").and_then(Value::as_str) else {
            return Ok(self.view().doc);
        };
        let path = Path::new(path);
        self.docs
            .iter()
            .find(|d| d.path.as_deref() == Some(path))
            .map(|d| d.id)
            .ok_or_else(|| format!("{} is not open", path.display()))
    }

    /// An edit a plugin worked out, applied the way a keystroke would be.
    ///
    /// Versioned, and **refused** rather than applied when the buffer has
    /// moved on: a plugin that computed a fix against version 40 of a file
    /// that is now at 43 is holding an edit for text that is no longer there,
    /// and applying it would corrupt the file rather than fix it.
    fn plugin_edit(&mut self, params: &Value) -> Result<Value, String> {
        let id = self.plugin_means(params)?;
        let doc = self.doc(id).ok_or("that buffer is not open")?;
        if let Some(against) = params.get("version").and_then(Value::as_i64)
            && against != doc.version as i64
        {
            return Err(format!(
                "that was worked out against version {against}, and this is {}",
                doc.version
            ));
        }
        if doc.read_only {
            return Err(format!("{} is read-only", doc.name));
        }

        // Lines and columns, both counted in characters from zero — the same
        // numbers a plugin is given for a diagnostic, and the same ones the
        // editor counts in everywhere.
        let changes: Vec<crate::doc::Change> = params
            .get("edits")
            .and_then(Value::as_array)
            .ok_or("an edit needs some edits")?
            .iter()
            .filter_map(|edit| {
                let at = |line: &str, column: &str| -> Option<usize> {
                    let row = edit.get(line)?.as_u64()? as usize;
                    let col = edit.get(column).and_then(Value::as_u64).unwrap_or(0) as usize;
                    Some(doc.char_at_point(row, col))
                };
                let from = at("line", "column")?;
                let to = at("end_line", "end_column").unwrap_or(from).max(from);
                let text = edit.get("text").and_then(Value::as_str).unwrap_or_default();
                Some(crate::doc::Change::replace(from, to, text.to_string()))
            })
            .collect();
        if changes.is_empty() {
            return Err("none of those edits said where to go".into());
        }
        // Through the same door a language server's edits go through, which
        // is what makes a plugin's work one thing to undo, and what tells the
        // language servers about it without a plugin having to.
        Ok(json!({ "applied": self.apply_changes_to(id, changes) }))
    }

    /// Text a plugin is offering to put in where the cursor is.
    ///
    /// Shown, not inserted. Until it is taken the file is exactly as it was,
    /// which is the whole difference between an offer and an edit — and it is
    /// why this needs no version check: nothing has happened to the text yet.
    fn plugin_hint(&mut self, id: HostId, params: &Value) -> Result<Value, String> {
        let Some(plugin) = self.hosts.get(id).map(|h| h.plugin.clone()) else {
            return Err("that plugin is not running".into());
        };
        let doc_id = self.plugin_means(params)?;
        let text = params
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let cursor = self.view().cursor();
        let Some(doc) = self.doc_mut(doc_id) else {
            return Err("that buffer is not open".into());
        };
        // Cleared by an empty offer, which is how a plugin says "never mind"
        // without a second message.
        if text.is_empty() {
            doc.hint = None;
            return Ok(json!({ "showing": false }));
        }
        let at = match (
            params.get("line").and_then(Value::as_u64),
            params.get("column").and_then(Value::as_u64),
        ) {
            (Some(line), column) => {
                doc.char_at_point(line as usize, column.unwrap_or(0) as usize)
            }
            // Nothing said means where the cursor is, which is what an inline
            // suggestion nearly always means.
            _ => cursor,
        };
        // An offer about somewhere the cursor is not is an offer nobody would
        // see, and one that would surprise them if they walked into it later.
        if at != cursor {
            doc.hint = None;
            return Ok(json!({ "showing": false }));
        }
        doc.hint = Some(crate::doc::Hint { plugin, at, text });
        Ok(json!({ "showing": true }))
    }

    /// Whether there is an offer on the screen to take.
    fn hint_showing(&self) -> bool {
        self.here()
            .hint
            .as_ref()
            .is_some_and(|hint| hint.at == self.view().cursor())
    }

    /// Put the offered text in, as an ordinary edit.
    ///
    /// Through the same door a keystroke goes through, so it is one thing to
    /// undo and the language servers hear about it — a plugin's suggestion
    /// becomes your text the moment you take it, and is your text in every way
    /// after that.
    fn accept_hint(&mut self) {
        let id = self.view().doc;
        let Some(hint) = self.doc_mut(id).and_then(|doc| doc.hint.take()) else {
            return;
        };
        let count = self.apply_changes_to(
            id,
            vec![crate::doc::Change::replace(hint.at, hint.at, hint.text.clone())],
        );
        if count > 0 {
            // The cursor goes to the end of what was put in, which is where
            // you would be if you had typed it.
            let to = hint.at + hint.text.chars().count();
            let len = self.here().len_chars();
            self.view_mut().sel = Selections::single(Range::point(to.min(len)));
            self.scroll_into_view();
        }
        let plugin = hint.plugin.clone();
        self.tell_panel(&plugin, "hint/taken", json!({ "text": hint.text }));
    }

    /// Take the offer away, and say why, so the plugin knows whether to make
    /// another one.
    fn drop_hint(&mut self, why: &str) {
        let id = self.view().doc;
        let Some(hint) = self.doc_mut(id).and_then(|doc| doc.hint.take()) else {
            return;
        };
        let plugin = hint.plugin.clone();
        self.tell_panel(&plugin, "hint/dropped", json!({ "why": why }));
    }

    /// Everything a plugin's panel says, all at once.
    ///
    /// The whole panel each time rather than a diff. A panel is tens of lines
    /// and changes a few times a second at worst, so sending the lot is
    /// simpler on both sides and impossible to desynchronise. If somebody ever
    /// builds a ten-thousand-line register view, a patch message can be added
    /// then — the shape here leaves room for one.
    fn plugin_panel(&mut self, id: HostId, params: &Value) -> Result<Value, String> {
        let Some(plugin) = self.hosts.get(id).map(|h| h.plugin.clone()) else {
            return Err("that plugin is not running".into());
        };
        let wanted = params
            .get("panel")
            .and_then(Value::as_str)
            .ok_or("which panel?")?
            .to_string();
        let Some(doc_id) = self
            .docs
            .iter()
            .find(|d| {
                d.panel
                    .as_ref()
                    .is_some_and(|p| p.id == wanted && p.plugin == plugin)
            })
            .map(|d| d.id)
        else {
            // Sent about a panel nobody has opened. Not an error a plugin can
            // do much about — it may have been closed while the message was
            // in flight — but worth saying rather than swallowing.
            return Err(format!("{wanted} is not open"));
        };

        let (text, spans, actions) = panel_lines(
            params
                .get("lines")
                .and_then(Value::as_array)
                .ok_or("a panel needs some lines")?,
        );

        // Where every pane showing this panel was, as a line and a column
        // rather than as an offset into the text.
        //
        // A refresh replaces the whole buffer, and an offset carried through
        // that lands wherever the mapping puts it — which for a replacement of
        // everything is the end. That is the bug it looks like: opening a
        // directory in a file tree sends the panel to the bottom, because the
        // text got longer and the cursor went with it. A line is what somebody
        // reading a panel is actually standing on, and a line survives the
        // lines below it changing.
        let places: Vec<(usize, usize, usize, usize)> = self
            .panes
            .iter()
            .enumerate()
            .filter(|(_, pane)| pane.doc == doc_id)
            .map(|(at, pane)| {
                let doc = self.doc(doc_id);
                let (line, column) = doc
                    .map(|d| d.point_at_char(pane.sel.primary().head))
                    .unwrap_or((0, 0));
                (at, line, column, pane.top)
            })
            .collect();

        let Some(doc) = self.doc_mut(doc_id) else {
            return Err(format!("{wanted} is not open"));
        };
        let was = doc.len_chars();
        let sel = Selections::single(Range::point(0));
        let lines = text.lines().count();
        let edits = doc.apply_atomic(
            vec![crate::doc::Change::replace(0, was, text)],
            &sel,
        );
        if let Some(panel) = &mut doc.panel {
            panel.spans = spans;
            panel.actions = actions;
        }
        doc.mark_saved();
        // A panel is replaced whole every time the plugin has something new to
        // say, and every one of those would otherwise leave a revision holding
        // the whole old text behind it. A tree that redraws on each keystroke
        // would grow a history of every shape it has ever had — and none of it
        // is reachable, because undo in a buffer you cannot type into has
        // nothing to give back.
        doc.forget_history();
        self.after_edit_to(doc_id, edits, None);

        // And back to the same line, clamped to a panel that may have got
        // shorter. Put back after the edit has been applied and mapped, so
        // this is the last word on where the cursor is.
        for (at, line, column, top) in places {
            let Some(doc) = self.doc(doc_id) else { break };
            let line = line.min(doc.len_lines().saturating_sub(1));
            let start = crate::text::line_start(&doc.rope, line);
            let end = crate::text::line_end(&doc.rope, line);
            let head = (start + column).min(end);
            let top = top.min(doc.len_lines().saturating_sub(1));
            if let Some(pane) = self.panes.get_mut(at) {
                pane.sel = Selections::single(Range::point(head));
                pane.top = top;
            }
        }
        self.scroll_into_view();
        Ok(json!({ "lines": lines }))
    }

    /// Move a panel to an edge, resize it, or take it off one.
    ///
    /// The manifest says where a panel goes by default, so that the editor can
    /// lay it out before the plugin has ever run. This is the other half: a
    /// plugin that wants to widen its tree because somebody has opened a deep
    /// directory, or to move to the bottom because what it is showing is a
    /// list rather than a tree, can say so while it is running.
    fn plugin_dock(&mut self, id: HostId, params: &Value) -> Result<Value, String> {
        let plugin = self
            .hosts
            .get(id)
            .map(|h| h.plugin.clone())
            .ok_or("that plugin is not running")?;
        let wanted = params
            .get("panel")
            .and_then(Value::as_str)
            .ok_or("which panel?")?
            .to_string();
        let doc = self
            .docs
            .iter()
            .find(|d| {
                d.panel
                    .as_ref()
                    .is_some_and(|p| p.id == wanted && p.plugin == plugin)
            })
            .map(|d| d.id)
            .ok_or_else(|| format!("{wanted} is not open"))?;

        let size = params
            .get("size")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, u16::MAX as u64) as u16);
        // `"none"` is how a plugin says "put it back in a tab", which is the
        // only way to say it that is not a second method.
        let edge = match params.get("edge").and_then(Value::as_str) {
            None => None,
            Some(said) if said.trim().eq_ignore_ascii_case("none") => {
                if let Some(at) = self.pane_showing_docked(doc) {
                    self.panes.remove(at);
                    self.focus = self.focus.min(self.panes.len().saturating_sub(1));
                }
                self.show(doc);
                self.session_changed();
                return Ok(json!({ "edge": Value::Null }));
            }
            Some(said) => Some(
                crate::view::Edge::parse(said)
                    .ok_or_else(|| format!("{said:?} is not an edge — left, right or bottom"))?,
            ),
        };

        match self.pane_showing_docked(doc) {
            // Already docked: change what was asked about and leave the rest.
            Some(at) => {
                let dock = self.panes[at].dock.get_or_insert(crate::view::Dock::new(
                    crate::view::Edge::Left,
                    None,
                ));
                if let Some(edge) = edge {
                    // A dock that changes edge changes what its size means, so
                    // one that was not also given a size gets the default for
                    // where it is going rather than a width used as a height.
                    *dock = crate::view::Dock::new(edge, size);
                } else if let Some(size) = size {
                    dock.size = size;
                }
            }
            None => {
                let edge = edge.ok_or("which edge?")?;
                self.dock_panel(doc, crate::view::Dock::new(edge, size));
            }
        }
        self.session_changed();
        let at = self.pane_showing_docked(doc);
        let dock = at.and_then(|at| self.panes[at].dock);
        Ok(json!({
            "edge": dock.map(|d| d.edge.label()),
            "size": dock.map(|d| d.size),
        }))
    }

    /// The path a plugin named, under the project.
    ///
    /// Always under it. A file explorer is a thing that sends paths back, and
    /// a plugin that could be talked into `../../.ssh/id_rsa` by a directory
    /// name is a plugin nobody should run. Everything here is resolved and
    /// then checked to be inside the project textfold was opened on.
    fn plugin_path(&self, params: &Value, key: &str) -> Result<PathBuf, String> {
        let said = params
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("{key}: which path?"))?;
        let full = crate::doc::absolute(&self.project.join(expand_path(said)));
        let root = crate::doc::absolute(&self.project);
        if !full.starts_with(&root) {
            return Err(format!("{said} is outside {}", root.display()));
        }
        Ok(full)
    }

    /// Make a file, or a directory where the name ends in a separator.
    fn plugin_file_create(&mut self, params: &Value) -> Result<Value, String> {
        let path = self.plugin_path(params, "path")?;
        let directory = params
            .get("directory")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if path.exists() {
            return Err(format!("{} is already there", path.display()));
        }
        if directory {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            return Ok(json!({ "path": path.display().to_string() }));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        // Left empty rather than opened. What to do with a file you have just
        // made is the person's business, and a plugin that made forty of them
        // should not have opened forty tabs.
        std::fs::write(&path, "").map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(json!({ "path": path.display().to_string() }))
    }

    /// Move a file or a directory, and take the buffers with it.
    ///
    /// The reason this is the editor's job and not `mv`: a buffer open on a
    /// file that has been renamed underneath it is a buffer that will save to
    /// the old name, and a language server still being told about a path that
    /// no longer exists. A plugin shelling out could not fix either.
    fn plugin_file_rename(&mut self, params: &Value) -> Result<Value, String> {
        let from = self.plugin_path(params, "from")?;
        let to = self.plugin_path(params, "to")?;
        if !from.exists() {
            return Err(format!("there is no {}", from.display()));
        }
        if to.exists() {
            return Err(format!("{} is already there", to.display()));
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::rename(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;

        // Everything open under the old name, whether it was the file itself
        // or something inside the directory.
        let moved: Vec<(DocId, PathBuf, PathBuf)> = self
            .docs
            .iter()
            .filter_map(|doc| {
                let was = doc.path.clone()?;
                let rest = was.strip_prefix(&from).ok()?.to_path_buf();
                Some((doc.id, was, to.join(rest)))
            })
            .collect();
        for (id, was, now) in &moved {
            // Told under the name it knows, then told again under the new one.
            // A language server left holding a path that no longer exists goes
            // on reporting problems in a file nobody can open.
            self.lsp.did_close(was);
            self.hosts.closed(was);
            if let Some(doc) = self.doc_mut(*id) {
                doc.rename_to(now.clone());
            }
            self.lsp_open(*id);
        }
        self.session_changed();
        Ok(json!({
            "path": to.display().to_string(),
            "buffers": moved.len(),
        }))
    }

    /// Take a file or a directory away, and close what was open in it.
    fn plugin_file_delete(&mut self, params: &Value) -> Result<Value, String> {
        let path = self.plugin_path(params, "path")?;
        if !path.exists() {
            return Err(format!("there is no {}", path.display()));
        }
        // Anything with unsaved changes in it stops this. A plugin may not
        // throw away work nobody has been asked about — and the plugin has
        // `confirm` for asking, which is a box the person can read.
        let unsaved: Vec<&str> = self
            .docs
            .iter()
            .filter(|doc| {
                doc.path.as_ref().is_some_and(|p| p.starts_with(&path)) && doc.is_modified()
            })
            .map(|doc| doc.name.as_str())
            .collect();
        if !unsaved.is_empty() {
            return Err(format!("{} has unsaved changes", unsaved.join(", ")));
        }
        let inside: Vec<DocId> = self
            .docs
            .iter()
            .filter(|doc| doc.path.as_ref().is_some_and(|p| p.starts_with(&path)))
            .map(|doc| doc.id)
            .collect();
        match path.is_dir() {
            true => std::fs::remove_dir_all(&path),
            false => std::fs::remove_file(&path),
        }
        .map_err(|e| format!("{}: {e}", path.display()))?;
        for id in &inside {
            self.close_doc(*id);
        }
        Ok(json!({ "buffers": inside.len() }))
    }

    /// Do whatever the plugin marked the text under the cursor as doing.
    ///
    /// Answers whether there was anything there, so that Enter in a panel with
    /// nothing under it goes on to mean what Enter usually means rather than
    /// being quietly eaten.
    fn panel_action_at(&mut self, at: usize) -> bool {
        let Some(doc) = self.docs.iter().find(|d| d.id == self.view().doc) else {
            return false;
        };
        let Some(panel) = &doc.panel else { return false };
        let Some((_, action)) = panel
            .actions
            .iter()
            .find(|(range, _)| range.start() <= at && at < range.end())
        else {
            return false;
        };
        let (plugin, id, action) = (panel.plugin.clone(), panel.id.clone(), action.clone());
        self.tell_panel(&plugin, "panel/action", json!({ "panel": id, "action": action }));
        true
    }

    /// Where on the screen to open something that belongs beside the cursor.
    ///
    /// The caret's own cell where there is one. A pane with no caret in it —
    /// one that is not focused — falls back to its top corner, which is where
    /// a menu about that pane should go anyway.
    fn cursor_on_screen(&self) -> (u16, u16) {
        self.caret.unwrap_or_else(|| {
            let area = self.view().area;
            (area.x, area.y)
        })
    }

    /// Whether a keystroke belongs to the plugin whose panel you are in.
    ///
    /// The rule: a panel gets the keys that would otherwise have **changed the
    /// text**. A panel's text is not yours to change, so those keys are going
    /// spare — and everything else still does exactly what it always does, so
    /// a plugin cannot take a key anybody knows. The same bargain as
    /// `Keys::suggest`, made for a buffer instead of for a binding.
    fn panel_wants(&self, key: Key) -> bool {
        self.here().panel.is_some() && self.keys.lookup(key).is_none_or(|cmd| cmd.writes())
    }

    /// Hand a keystroke to the plugin whose panel is in front of you.
    fn send_panel_key(&mut self, key: Key) {
        let Some((plugin, id)) = self
            .here()
            .panel
            .as_ref()
            .map(|p| (p.plugin.clone(), p.id.clone()))
        else {
            return;
        };
        let at = self.view().cursor();
        let (line, column) = self.here().point_at_char(at);
        self.tell_panel(
            &plugin,
            "panel/key",
            // Where the cursor was as well as which key: nearly everything a
            // panel does with a key it does to the row you are standing on,
            // and making every plugin work that out for itself from a cursor
            // it was never told about would be silly.
            json!({ "panel": id, "key": key.spelled(), "line": line, "column": column }),
        );
    }

    /// Say something to whichever host is running a plugin, about a panel.
    fn tell_panel(&mut self, plugin: &str, method: &str, params: Value) {
        let id = self
            .hosts
            .all()
            .iter()
            .position(|h| h.plugin == plugin && h.is_ready())
            .map(HostId);
        if let Some(host) = id.and_then(|id| self.hosts.get_mut(id)) {
            host.notify_out(method, params);
        }
    }

    /// Problems a plugin found, in the margin beside the language server's.
    ///
    /// Namespaced by plugin, so a fresh set from one replaces only its own
    /// findings. A plugin cannot clear clangd's, and clangd cannot clear its.
    fn plugin_diagnostics(&mut self, id: HostId, params: &Value) -> Result<Value, String> {
        let Some(plugin) = self
            .hosts
            .get(id)
            .and_then(|h| crate::plugin::find(&h.plugin))
        else {
            return Err("that plugin is not running".into());
        };
        let told = crate::doc::Told::Plugin(plugin.id.as_str());
        let name = plugin.name.clone();

        // A plugin says everything it thinks about a file at once, so a set
        // that names a file replaces what it said about that file; one that
        // names none replaces everything it has said.
        let only: Option<PathBuf> = params
            .get("path")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        for doc in &mut self.docs {
            if only.as_deref().is_none_or(|p| doc.path.as_deref() == Some(p)) {
                doc.diagnostics.retain(|d| d.told != told);
            }
        }

        let items = params
            .get("items")
            .and_then(Value::as_array)
            .ok_or("diagnostics need some items")?;
        let mut count = 0;
        for item in items {
            let path = item
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .or_else(|| only.clone());
            let Some(doc_id) = self
                .docs
                .iter()
                .find(|d| match &path {
                    Some(p) => d.path.as_deref() == Some(p.as_path()),
                    None => false,
                })
                .map(|d| d.id)
            else {
                // About a file that is not open. Perfectly normal for a plugin
                // that has just built a whole project.
                continue;
            };
            let Some(doc) = self.doc_mut(doc_id) else {
                continue;
            };
            let row = item.get("line").and_then(Value::as_u64).unwrap_or(0) as usize;
            let col = item.get("column").and_then(Value::as_u64).unwrap_or(0) as usize;
            let end_row = item
                .get("end_line")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(row);
            let end_col = item
                .get("end_column")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                // Nothing said about where it ends means the one character it
                // starts at, so that there is something to underline.
                .unwrap_or(col + 1);
            let from = doc.char_at_point(row, col);
            let to = doc.char_at_point(end_row, end_col);
            doc.diagnostics.push(crate::doc::Diagnostic {
                range: Range::new(from, to.max(from)),
                severity: match item.get("severity").and_then(Value::as_str) {
                    Some("error") => crate::doc::Severity::Error,
                    Some("info") => crate::doc::Severity::Info,
                    Some("hint") => crate::doc::Severity::Hint,
                    _ => crate::doc::Severity::Warning,
                },
                message: item
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("something is wrong here")
                    .to_string(),
                source: Some(
                    item.get("source")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| name.clone()),
                ),
                code: item
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                data: None,
                told,
            });
            count += 1;
        }
        Ok(json!({ "shown": count }))
    }

    /// Which plugin a host belongs to, for a line in the status bar.
    fn plugin_name(&self, id: HostId) -> String {
        self.hosts
            .get(id)
            .map(|h| h.plugin.clone())
            .unwrap_or_else(|| "a plugin".into())
    }

    /// Anything the host machinery wanted to say, moved to the status line —
    /// it runs in the middle of other work and the screen is not its to write
    /// on.
    fn take_plugin_problems(&mut self) {
        let problems = std::mem::take(&mut self.hosts.problems);
        // A grammar is compiled the first time a file of its language is
        // shown, so a plugin that brought one broken says so here rather than
        // at startup — and says so at all, which it did not before.
        let problems = problems
            .into_iter()
            .chain(crate::lang::take_grammar_problems());
        if let Some(first) = problems.into_iter().next() {
            self.say_bad(first);
        }
    }

    /// One thing a plugin asked the editor to do.
    ///
    /// The rule this list is written against: **a plugin may do nothing a
    /// keystroke cannot**. Every arm goes through the same door a person does,
    /// so a plugin's work is undoable, themed and consistent for free, and
    /// there is no second implementation of anything to drift.
    fn plugin_asked(
        &mut self,
        id: HostId,
        method: &str,
        params: &Value,
        // The JSON-RPC id, where this came as a question rather than as a
        // statement. `run` is the one thing that needs it, because the answer
        // is sent from a thread long after this returns.
        request: Option<&Value>,
    ) -> Answer {
        let text = |key: &str| -> String {
            params
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match method {
            "status/say" => {
                let words = text("text");
                if words.trim().is_empty() {
                    return Answer::No("said nothing".into());
                }
                match params.get("kind").and_then(Value::as_str) {
                    Some("good") => self.say_good(words),
                    Some("bad") => self.say_bad(words),
                    _ => self.say(words),
                }
                Answer::Now(Value::Null)
            }
            "buffer/show" => {
                let name = match text("name") {
                    empty if empty.trim().is_empty() => format!("{} output", self.plugin_name(id)),
                    given => given,
                };
                // A plugin has to ask to be taken to. Most of the time it
                // should not: what it has to say arrives when it arrives, and
                // where the cursor is belongs to whoever is typing.
                let focus = params
                    .get("focus")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.put_in_a_buffer(&name, &text("text"), focus);
                Answer::Now(Value::Null)
            }
            "buffer/read" => match self
                .plugin_means(params)
                .and_then(|id| self.doc(id).ok_or_else(|| "that buffer is not open".into()))
            {
                Ok(doc) => Answer::Now(json!({
                    "path": doc.path,
                    "language": lang::get(doc.language).name,
                    "version": doc.version,
                    "text": doc.text(),
                })),
                Err(why) => Answer::No(why),
            },
            "buffer/edit" => self.plugin_edit(params).into(),
            "panel/set" => self.plugin_panel(id, params).into(),
            "panel/dock" => self.plugin_dock(id, params).into(),
            "file/create" => self.plugin_file_create(params).into(),
            "file/rename" => self.plugin_file_rename(params).into(),
            "file/delete" => self.plugin_file_delete(params).into(),
            "hint/set" => self.plugin_hint(id, params).into(),
            // The editor's own list, prompt and yes/no, lent out. A plugin
            // asking "which board?" gets the same box, the same keys and the
            // same colours as Ctrl-P, which is the point: it should look like
            // textfold rather than like a plugin.
            "pick" => {
                let rows: Vec<Row> = params
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                // A bare string is both what is shown and what
                                // comes back, which is most lists.
                                if let Some(label) = item.as_str() {
                                    return Some(Row::new(
                                        label,
                                        Choice::PluginItem(label.to_string()),
                                    ));
                                }
                                let label = item.get("label").and_then(Value::as_str)?;
                                let value = item
                                    .get("value")
                                    .and_then(Value::as_str)
                                    .unwrap_or(label);
                                let mut row =
                                    Row::new(label, Choice::PluginItem(value.to_string()));
                                if let Some(detail) = item.get("detail").and_then(Value::as_str) {
                                    row = row.detail(detail);
                                }
                                if let Some(tag) = item.get("tag").and_then(Value::as_str) {
                                    row = row.tag(tag);
                                }
                                Some(row)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if rows.is_empty() {
                    return Answer::No("there was nothing in that list".into());
                }
                let mut picker = Picker::new(Kind::PluginPick, rows);
                picker.called = Some(match text("title") {
                    empty if empty.trim().is_empty() => self.plugin_name(id),
                    given => given,
                });
                self.ask_for_plugin(id, request, Overlay::Picker(picker))
            }
            "prompt" => {
                let mut prompt = Prompt::new(PromptKind::PluginAsked);
                prompt.label = Some(match text("title") {
                    empty if empty.trim().is_empty() => format!("{}?", self.plugin_name(id)),
                    given => given,
                });
                prompt.input = text("value");
                prompt.caret = prompt.input.chars().count();
                self.ask_for_plugin(id, request, Overlay::Prompt(prompt))
            }
            "confirm" => {
                let message = match text("text") {
                    empty if empty.trim().is_empty() => {
                        return Answer::No("a question needs asking".into());
                    }
                    given => given,
                };
                let confirm = Confirm {
                    message,
                    choices: vec![('y', "yes".into()), ('n', "no".into())],
                    then: Then::PluginAsked,
                };
                self.ask_for_plugin(id, request, Overlay::Confirm(confirm))
            }
            // A menu where the cursor is, rather than in the middle of the
            // screen. The difference between `pick` and this is the same
            // difference the editor's own two lists have: `pick` is for
            // choosing out of hundreds by typing part of a name, a menu is for
            // the handful of things that make sense right here, read rather
            // than searched, and it has to appear where you are.
            "menu" => {
                let items: Vec<menu::Item> = params
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| {
                                // A bare string is a row that is its own
                                // answer; a null is a divider.
                                if item.is_null() {
                                    return menu::Item::divider();
                                }
                                if let Some(label) = item.as_str() {
                                    return menu::Item::chosen(label, label);
                                }
                                let label =
                                    item.get("label").and_then(Value::as_str).unwrap_or("");
                                let value = item
                                    .get("value")
                                    .and_then(Value::as_str)
                                    .unwrap_or(label);
                                menu::Item::chosen(label, value).enabled(
                                    item.get("enabled")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(true),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !items.iter().any(|item| matches!(item.action, menu::Action::Chosen(_))) {
                    return Answer::No("there was nothing in that menu".into());
                }
                // Where the cursor is on the screen. A click has already put
                // the cursor where it landed, so a menu asked for after a
                // click on a panel row opens on that row.
                let anchor = self.cursor_on_screen();
                self.ask_for_plugin(id, request, Overlay::Menu(menu::Menu::new(items, anchor)))
            }
            "open" => {
                let path = text("path");
                if path.trim().is_empty() {
                    return Answer::No("open needs a path".into());
                }
                let path = self.project.join(expand_path(&path));
                self.open_path(&path);
                if let Some(line) = params.get("line").and_then(Value::as_u64) {
                    let column = params.get("column").and_then(Value::as_u64).unwrap_or(0);
                    self.jump_to(line as usize, column as usize);
                }
                Answer::Now(Value::Null)
            }
            "diagnostics/set" => self.plugin_diagnostics(id, params).into(),
            "run" => {
                let command = text("command");
                if command.trim().is_empty() {
                    return Answer::No("run needs something to run".into());
                }
                // Notified rather than asked. There is nowhere to send the
                // answer, and a program run for nobody is a program run by
                // accident.
                let Some(request) = request.cloned() else {
                    return Answer::No("run has to be asked, not told".into());
                };
                let args = params
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let cwd = params.get("cwd").and_then(Value::as_str).map(PathBuf::from);
                match self.hosts.run_program(id, request, &command, args, cwd) {
                    // Answered from the thread, when the program is done.
                    Ok(()) => Answer::Later,
                    Err(why) => Answer::No(why),
                }
            }
            // Deliberately not a silence: a plugin author who has misspelt a
            // method, or reached for one textfold does not have yet, should
            // find that out from the editor rather than from nothing
            // happening.
            _ => Answer::No(format!("textfold has no {method}")),
        }
    }

    fn on_notification(&mut self, id: ServerId, method: &str, params: Value) {
        match method {
            "textDocument/publishDiagnostics" => self.take_diagnostics(id, &params),
            "$/progress" => self.lsp.progress(id, &params),
            "window/showMessage" => {
                let text = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .replace('\n', " ");
                if text.is_empty() {
                    return;
                }
                match params.get("type").and_then(Value::as_u64) {
                    Some(1) => self.say_bad(text),
                    _ => self.say(text),
                }
            }
            "window/logMessage" => {
                if let Some(server) = self.lsp.get_mut(id) {
                    server.message = params
                        .get("message")
                        .and_then(Value::as_str)
                        .map(|m| m.replace('\n', " "));
                }
            }
            _ => {}
        }
    }

    fn take_diagnostics(&mut self, id: ServerId, params: &Value) {
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return;
        };
        let Some(path) = crate::lsp::path_of(uri) else {
            return;
        };
        let Some(doc_id) = self
            .docs
            .iter()
            .find(|d| d.path.as_deref() == Some(path.as_path()))
            .map(|d| d.id)
        else {
            // About a file we do not have open. Perfectly normal — a server
            // checks the whole crate.
            return;
        };
        let App { docs, lsp, .. } = self;
        let Some(doc) = docs.iter().find(|d| d.id == doc_id) else {
            return;
        };
        let Some(fresh) = lsp.diagnostics_for(id, params, doc) else {
            return;
        };
        let Some(doc) = docs.iter_mut().find(|d| d.id == doc_id) else {
            return;
        };
        // A server sends its complete opinion every time, so its old findings
        // go and everybody else's stay.
        doc.diagnostics.retain(|d| d.told != crate::doc::Told::Server(id.0));
        doc.diagnostics.extend(fresh);

        // What is wrong here has changed, so what could be done about it has
        // too — and the cursor may have been sitting on this spot since before
        // there was anything wrong with it, which is exactly what opening a
        // file at a compiler's line and column looks like.
        if self.fixes_at.is_some_and(|(doc, _)| doc == doc_id) {
            self.fixes = None;
            self.fixes_at = None;
        }
    }

    fn on_response(&mut self, id: ServerId, request: i64, result: Result<Value, String>) {
        let Some(ask) = self.lsp.get_mut(id).and_then(|s| s.claim(request)) else {
            return;
        };
        let value = match result {
            Ok(value) => value,
            Err(why) => {
                // A failed request for something the editor asked for on its
                // own — completions as you type, fixes for the problem under
                // the cursor — is not worth a word.
                if let Ask::ResolveCompletion { index, .. } = ask {
                    // Nothing more is coming. Take what there is rather than
                    // leave a keystroke unanswered.
                    if let Some(item) = self.suggestion_mut(index) {
                        item.resolve = Resolve::Done;
                    }
                    self.accept_if_waiting(index);
                    return;
                }
                if let Ask::QuickFixes { doc, at } = ask {
                    // "content modified" is the usual one, and it means the
                    // server was still catching up when we asked rather than
                    // that there is nothing to offer. Ask again.
                    self.retry_fixes(doc, at);
                } else if !matches!(
                    ask,
                    Ask::Completion { .. } | Ask::ResolveCompletion { .. } | Ask::Signature { .. }
                ) {
                    self.say_bad(why);
                }
                return;
            }
        };

        match ask {
            Ask::Initialize => {
                let App { docs, lsp, .. } = self;
                let open: Vec<&Document> = docs.iter().collect();
                lsp.ready(id, value, &open);
            }
            Ask::Completion { doc, at, version } => {
                self.take_completions(id, doc, at, version, value)
            }
            Ask::Hover { doc, at } => self.take_hover(doc, at, value),
            Ask::Goto {
                doc,
                what,
                fallback,
            } => self.take_goto(doc, what, fallback, value),
            Ask::References => self.take_references(value),
            Ask::Symbols { doc } => self.take_symbols(doc, value),
            Ask::WorkspaceSymbols { going } => self.take_workspace_symbols(going, value),
            Ask::Rename { to } => {
                let count = self.apply_workspace_edit(&value);
                match count {
                    0 => self.say("nothing to rename"),
                    n => self.say_good(format!("renamed to {to} in {n} {}", places(n))),
                }
            }
            Ask::Format { doc, version } => self.take_format(doc, version, value),
            Ask::CodeActions { doc, at } => self.take_code_actions(id, doc, at, value),
            Ask::SourceActions { doc, version } => {
                self.take_source_actions(id, doc, version, value)
            }
            Ask::QuickFixes { doc, at } => self.take_quick_fixes(id, doc, at, value),
            Ask::ClassFile { uri, line, column } => self.take_class_file(uri, line, column, value),
            Ask::Signature { doc, at } => self.take_signature(doc, at, value),
            Ask::ResolveAction => self.do_code_action(id, value),
            Ask::ResolveCompletion { doc, index } => {
                self.take_resolved_completion(doc, index, value)
            }
            Ask::Command => {}
        }
    }

    fn take_completions(
        &mut self,
        server: ServerId,
        doc: DocId,
        at: usize,
        version: i32,
        value: Value,
    ) {
        // An answer about a file that has changed underneath it is an answer
        // to a question nobody is asking any more.
        if self.view().doc != doc || self.doc(doc).map(|d| d.version) != Some(version) {
            return;
        }
        let items = match &value {
            Value::Array(items) => items.clone(),
            other => other
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        };
        let incomplete = value
            .get("isIncomplete")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if items.is_empty() {
            self.completion = None;
            return;
        }

        let document = self.here();
        // Where the word being completed starts — or the cursor itself, when
        // there is no word yet. `word_start` reaches back over whitespace to
        // find the previous word, which is right for moving and wrong here:
        // completing after a space would take the word before it as typed and
        // narrow every suggestion away.
        let start = match at.checked_sub(1).map(|i| document.rope.char(i)) {
            Some(c) if text::class_of(c) == text::Class::Word => {
                text::word_start(&document.rope, at)
            }
            _ => at,
        };
        // The word being completed, as the server would have seen it.
        let suggestions: Vec<Suggestion> = items
            .iter()
            .filter_map(|item| suggestion_from(item, document, at))
            .collect();
        if suggestions.is_empty() {
            self.completion = None;
            return;
        }

        let typed = document.slice(Range::new(start.min(at), at));
        let mut completion = Completion {
            doc,
            server,
            incomplete,
            start,
            all: suggestions,
            shown: Vec::new(),
            cursor: 0,
            top: 0,
            area: Rect::default(),
        };
        completion.narrow(&typed);
        self.completion = (!completion.is_empty()).then_some(completion);
        self.accept_when_resolved = None;
        self.resolve_selected();
    }

    /// A list of suggestions, as though a server had just sent one. For the
    /// tests that are about what reaches the screen rather than about what
    /// the editor is holding.
    #[cfg(test)]
    pub(crate) fn suggest_for_test(&mut self, at: usize, incomplete: bool, items: Value) {
        let (doc, version) = (self.here().id, self.here().version);
        self.take_completions(
            crate::lsp::ServerId(0),
            doc,
            at,
            version,
            serde_json::json!({ "isIncomplete": incomplete, "items": items }),
        );
    }

    /// Ask what else there is to know about the suggestion under the cursor.
    ///
    /// Asked as soon as the list arrives and again as it is stepped through,
    /// rather than when something is taken: an import that has to be fetched
    /// before the name can go in is an import you would wait for, and waiting
    /// is what this is here to stop.
    fn resolve_selected(&mut self) {
        let Some(completion) = &mut self.completion else {
            return;
        };
        let (doc, server) = (completion.doc, completion.server);
        let Some(&index) = completion.shown.get(completion.cursor) else {
            return;
        };
        let item = &mut completion.all[index];
        if item.resolve != Resolve::Unasked {
            return;
        }
        let raw = item.raw.clone();
        let asked = self.lsp.resolve_completion(server, doc, index, &raw);
        // A server that does not answer that question has already told us
        // everything it is going to.
        if let Some(item) = self.suggestion_mut(index) {
            item.resolve = if asked {
                Resolve::Waiting
            } else {
                Resolve::Done
            };
        }
    }

    fn suggestion_mut(&mut self, index: usize) -> Option<&mut Suggestion> {
        self.completion.as_mut()?.all.get_mut(index)
    }

    /// Put what came back into the suggestion it was about.
    ///
    /// Only the parts a server is allowed to leave out of the first answer.
    /// What goes in and over what was settled when the list was drawn, and a
    /// resolved item is not permitted to change it.
    fn take_resolved_completion(&mut self, doc: DocId, index: usize, value: Value) {
        let document = match self.completion.as_ref() {
            Some(completion) if completion.doc == doc => match self.doc(doc) {
                Some(document) => document,
                None => return,
            },
            // An answer about a list that has been typed past or closed.
            _ => return,
        };
        let at = self.view().cursor();
        let Some(filled) = suggestion_from(&value, document, at) else {
            return;
        };
        let Some(completion) = &mut self.completion else {
            return;
        };
        let Some(item) = completion.all.get_mut(index) else {
            return;
        };
        if !filled.also.is_empty() {
            item.also = filled.also;
        }
        item.about = filled.about.or_else(|| item.about.take());
        item.detail = filled.detail.or_else(|| item.detail.take());
        item.suffix = filled.suffix.or_else(|| item.suffix.take());
        item.resolve = Resolve::Done;
        self.accept_if_waiting(index);
    }

    /// Take the suggestion that was taken before it was ready, now that it is.
    fn accept_if_waiting(&mut self, index: usize) {
        if self.accept_when_resolved == Some(index) {
            self.accept_when_resolved = None;
            self.take_suggestion(index);
        }
    }

    fn take_hover(&mut self, doc: DocId, at: usize, value: Value) {
        if self.view().doc != doc {
            return;
        }
        let here = self.doc(doc).map(|d| d.language).unwrap_or(LangId::PLAIN);
        let said = markup_lines(value.get("contents"), here);
        // What is wrong here goes above what this is, because a person who
        // pointed at a squiggle asked about the squiggle. The box that is
        // already up says it too — this is the same box being replaced now
        // that the server has answered — so it must not be dropped.
        let problems = self.problem_lines(at);
        let mut lines = problems;
        if !said.is_empty() {
            if !lines.is_empty() {
                lines.push(DocLine::prose(RULE.to_string()));
            }
            lines.extend(said);
        }
        if lines.is_empty() {
            return;
        }
        // A hover over something red is a hover over something you may be
        // about to fix. Saying so here is where a person is already looking.
        if let Some(fixes) = self.fixes.as_ref().filter(|f| f.doc == doc)
            && let Some(title) = fixes.headline()
        {
            let key = self
                .keys
                .shortcut(Cmd::FIX_IT)
                .unwrap_or_else(|| "Alt-i".into());
            lines.push(DocLine::prose(RULE.to_string()));
            lines.push(DocLine::prose(format!("{key}: {title}")));
        }
        self.hover = Some(Popup::new(lines, at));
    }

    fn take_signature(&mut self, doc: DocId, at: usize, value: Value) {
        if self.view().doc != doc {
            return;
        }
        let signatures = value.get("signatures").and_then(Value::as_array);
        let Some(signatures) = signatures.filter(|s| !s.is_empty()) else {
            self.signature = None;
            return;
        };
        let which = value
            .get("activeSignature")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let signature = signatures.get(which).or_else(|| signatures.first());
        let Some(signature) = signature else { return };
        let label = signature
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let here = self.doc(doc).map(|d| d.language).unwrap_or(LangId::PLAIN);
        let mut lines = vec![DocLine::prose(label)];
        lines.extend(
            markup_lines(signature.get("documentation"), here)
                .into_iter()
                .take(4),
        );
        self.signature = Some(Popup::new(lines, at));
    }

    fn take_goto(&mut self, doc: DocId, what: Goto, fallback: Option<String>, value: Value) {
        if self.view().doc != doc {
            return;
        }
        let places = locations(&value);
        match places.len() {
            0 => match fallback {
                Some(name) => self.look_up_by_name(&name),
                None => self.say(format!("no {} found", what.label())),
            },
            1 => {
                let (target, line, column) = places[0].clone();
                self.view_mut().mark_jump();
                self.go_to_target(target, line, column);
            }
            _ => {
                let project = self.project.clone();
                let rows: Vec<Row> = places
                    .into_iter()
                    .map(|(target, line, column)| {
                        Row::new(
                            target.label(&project),
                            Choice::At {
                                target,
                                line,
                                column,
                            },
                        )
                        .detail(format!("line {}", line + 1))
                    })
                    .collect();
                self.view_mut().mark_jump();
                self.overlay = Overlay::Picker(Picker::new(Kind::References, rows));
            }
        }
    }

    /// Go where a language server pointed.
    fn go_to_target(&mut self, target: Target, line: usize, column: usize) {
        match target {
            Target::File(path) => {
                self.open_path(&path);
                self.go_to(line, column);
            }
            Target::Inside(uri) => {
                // A class inside a jar. Only the server that named it can hand
                // over the text, so the jump finishes when the answer arrives.
                if let Some(existing) = self
                    .docs
                    .iter()
                    .find(|d| d.origin.as_deref() == Some(uri.as_str()))
                    .map(|d| d.id)
                {
                    self.show(existing);
                    self.go_to(line, column);
                    return;
                }
                let (doc, lsp) = self.doc_and_lsp();
                if lsp.class_file(doc, &uri, line, column).is_none() {
                    self.say("that is inside a library this server will not open");
                }
            }
        }
    }

    /// Put the text of a class that lives inside a jar into a buffer.
    fn take_class_file(&mut self, uri: String, line: usize, column: usize, value: Value) {
        let Some(text) = value.as_str().filter(|t| !t.is_empty()) else {
            return self.say("the server had nothing to show for that");
        };
        let project = self.project.clone();
        let name = Target::Inside(uri.clone()).label(&project);
        let id = self.new_id();
        let mut doc = Document::scratch(id, name, self.default_indent());
        doc.set_text(text);
        doc.language = lang::by_name("java").unwrap_or(LangId::PLAIN);
        doc.reparse();
        doc.mark_saved();
        // There is no file to write it back to, and a decompiled class is not
        // something anybody means to edit.
        doc.read_only = true;
        doc.origin = Some(uri);
        self.docs.push(doc);
        self.show(id);
        self.go_to(line, column);
    }

    fn take_references(&mut self, value: Value) {
        let places = locations(&value);
        if places.is_empty() {
            return self.say("used nowhere the server knows of");
        }
        let project = self.project.clone();
        let rows: Vec<Row> = places
            .into_iter()
            .map(|(target, line, column)| {
                let where_ = target.label(&project);
                // The line of code itself, where the file is one we have.
                let preview = match &target {
                    Target::File(path) => self
                        .docs
                        .iter()
                        .find(|d| d.path.as_deref() == Some(path.as_path()))
                        .and_then(|d| {
                            (line < d.len_lines()).then(|| {
                                let start = text::line_start(&d.rope, line);
                                let end = text::line_end(&d.rope, line);
                                d.rope.slice(start..end).to_string().trim().to_string()
                            })
                        }),
                    Target::Inside(_) => None,
                };
                Row::new(
                    preview.unwrap_or_else(|| where_.clone()),
                    Choice::At {
                        target,
                        line,
                        column,
                    },
                )
                .detail(format!("{where_}:{}", line + 1))
            })
            .collect();
        self.view_mut().mark_jump();
        self.overlay = Overlay::Picker(Picker::new(Kind::References, rows));
    }

    fn take_symbols(&mut self, doc: DocId, value: Value) {
        if self.view().doc != doc {
            return;
        }
        let mut rows = Vec::new();
        let Some(document) = self.doc(doc) else {
            return;
        };
        collect_symbols(&value, document, 0, &mut rows);
        if rows.is_empty() {
            return self.say("nothing this file defines that the server will name");
        }
        self.view_mut().mark_jump();
        self.overlay = Overlay::Picker(Picker::new(Kind::Symbols, rows));
    }

    fn take_workspace_symbols(&mut self, going: Option<String>, value: Value) {
        let Value::Array(items) = &value else { return };
        let project = self.project.clone();
        let rows: Vec<Row> = items
            .iter()
            .filter_map(|item| {
                let name = item.get("name")?.as_str()?.to_string();
                let location = item.get("location")?;
                let path = crate::lsp::path_of(location.get("uri")?.as_str()?)?;
                let (line, column) = crate::lsp::point_of(location.get("range")?.get("start")?)?;
                let mut row = Row::new(
                    name,
                    Choice::There {
                        path: path.clone(),
                        line,
                        column,
                    },
                )
                .detail(format!("{}:{}", short(&path, &project), line + 1));
                if let Some(kind) = item.get("kind").and_then(Value::as_u64) {
                    row = row.tag(symbol_kind(kind));
                }
                Some(row)
            })
            .collect();
        // A name followed out of a docstring is a question with one right
        // answer, not a list to browse: one hit goes there, and the list this
        // opened with goes away with it.
        if let Some(name) = going {
            match rows.len() {
                0 => {
                    self.overlay = Overlay::None;
                    return self.say(format!("nothing in this project called {name}"));
                }
                1 => {
                    if let Choice::There { path, line, column } = &rows[0].choice {
                        let (path, line, column) = (path.clone(), *line, *column);
                        self.overlay = Overlay::None;
                        self.view_mut().mark_jump();
                        self.open_path(&path);
                        self.go_to(line, column);
                        return;
                    }
                }
                _ => {}
            }
        }
        if let Overlay::Picker(picker) = &mut self.overlay
            && picker.kind == Kind::WorkspaceSymbols
        {
            picker.set_rows(rows);
        }
    }

    fn take_format(&mut self, doc: DocId, version: i32, value: Value) {
        let in_a_save = self.waiting_on(&Step::Format)
            && self.before_save.as_ref().is_some_and(|b| b.doc == doc);
        if in_a_save && let Some(before) = &mut self.before_save {
            before.doing = None;
        }
        // A file that moved on while the formatter was thinking. Applying
        // these edits now would scramble it — but a save that was waiting on
        // them should still happen, or Ctrl-S would have done nothing.
        if self.doc(doc).map(|d| d.version) != Some(version) {
            if in_a_save {
                self.advance();
            }
            return;
        }
        let count = match &value {
            Value::Array(edits) => self.apply_edits_to(doc, edits),
            _ => 0,
        };
        if in_a_save {
            self.advance();
        } else if count > 0 {
            self.say_good("formatted");
        }
    }

    /// One server's answer to "what can be done here", added to whatever the
    /// others have said.
    ///
    /// The list opens on the first answer and grows as the rest arrive, rather
    /// than waiting for the slowest server. Waiting would be the tidier
    /// listing and the worse editor: `ruff` answers in a few milliseconds and
    /// `pyright` can take a second, and a menu that appears a second after you
    /// asked for it is a menu you have already given up on.
    fn take_code_actions(&mut self, id: ServerId, doc: DocId, at: usize, value: Value) {
        let Some(offer) = self.offer.as_mut().filter(|g| g.doc == doc && g.at == at) else {
            return;
        };
        offer.take(id, value);
        let (settled, empty) = (offer.settled(), offer.is_empty());
        if empty {
            if settled {
                self.offer = None;
                // Only once everybody has been heard from. Saying it on the
                // first empty answer would put "nothing to offer here" on the
                // screen a moment before the list arrived.
                self.say("nothing to offer here");
            }
            return;
        }
        let offered: Vec<(ServerId, Value)> = self
            .offer
            .as_ref()
            .map(|g| {
                g.actions()
                    .into_iter()
                    .map(|(id, a)| (id, a.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let first_time = self.offer.as_ref().is_some_and(|g| !g.shown);
        match &mut self.overlay {
            // Already open and still the same list: fill it in where it
            // stands, keeping whatever has been typed into it.
            Overlay::Picker(picker) if picker.kind == Kind::Actions => {
                picker.set_rows(action_rows(&offered));
            }
            // Nothing in the way, and this is the first thing to arrive.
            Overlay::None if first_time => {
                self.show_actions(offered);
                if let Some(offer) = &mut self.offer {
                    offer.shown = true;
                }
            }
            // Something else is on the screen, or the list has been closed
            // again. Whoever asked has moved on, and a late answer is not
            // worth taking the screen away from what they are doing now.
            _ => {}
        }
        if settled {
            self.offer = None;
        }
    }

    /// Put a set of code actions up as a list to choose from.
    fn show_actions(&mut self, offered: Vec<(ServerId, Value)>) {
        let rows = action_rows(&offered);
        if rows.is_empty() {
            return self.say("nothing to offer here");
        }
        self.overlay = Overlay::Picker(Picker::new(Kind::Actions, rows));
    }

    /// Do what a code action says. Some carry their edit; some carry a
    /// command for the server to run; and some carry neither until asked.
    fn do_code_action(&mut self, id: ServerId, action: Value) {
        if let Some(edit) = action.get("edit") {
            let count = self.apply_workspace_edit(edit);
            if count > 0 {
                self.say_good(format!("changed {count} {}", places(count)));
            }
        } else if action.get("command").is_some() {
            let command = match action.get("command") {
                // A `CodeAction` holds a whole command object; a bare
                // `Command` *is* one.
                Some(Value::Object(_)) => action.get("command").cloned().unwrap_or(Value::Null),
                _ => action.clone(),
            };
            self.lsp.execute(id, &command);
        } else if !self.lsp.resolve_action(id, &action) {
            self.say("the server offered that but will not say what it means");
        }
    }

    /// Apply a `WorkspaceEdit`: text edits across any number of files.
    ///
    /// Files that are not open get opened, and left open and modified rather
    /// than written behind your back. A rename that touches nine files should
    /// be nine tabs you can look at and undo, not nine files quietly changed
    /// on disk.
    fn apply_workspace_edit(&mut self, edit: &Value) -> usize {
        let mut changed = 0;

        // Two spellings of the same thing, and servers use both.
        if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
            for (uri, edits) in changes {
                let Some(path) = crate::lsp::path_of(uri) else {
                    continue;
                };
                let Some(edits) = edits.as_array() else {
                    continue;
                };
                changed += self.apply_edits_to_path(&path, edits);
            }
        }
        if let Some(documents) = edit.get("documentChanges").and_then(Value::as_array) {
            for entry in documents {
                // `documentChanges` can also hold file creations and renames.
                // Those are refused rather than half-done: an editor that
                // deletes a file because a code action said so is an editor
                // nobody trusts twice.
                if entry.get("kind").is_some() {
                    self.say("that would create or move files, which textfold will not do");
                    continue;
                }
                let Some(uri) = entry
                    .get("textDocument")
                    .and_then(|d| d.get("uri"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let Some(path) = crate::lsp::path_of(uri) else {
                    continue;
                };
                let Some(edits) = entry.get("edits").and_then(Value::as_array) else {
                    continue;
                };
                changed += self.apply_edits_to_path(&path, edits);
            }
        }
        changed
    }

    fn apply_edits_to_path(&mut self, path: &Path, edits: &[Value]) -> usize {
        let id = match self
            .docs
            .iter()
            .find(|d| d.path.as_deref() == Some(path))
            .map(|d| d.id)
        {
            Some(id) => id,
            None => {
                let id = self.new_id();
                match Document::open(id, path, self.default_indent()) {
                    Ok(doc) => {
                        self.docs.push(doc);
                        self.touch(id);
                        id
                    }
                    Err(e) => {
                        self.say_bad(format!("{e}"));
                        return 0;
                    }
                }
            }
        };
        self.apply_edits_to(id, edits)
    }

    /// Turn a server's text edits into one undoable change to one document.
    fn apply_edits_to(&mut self, id: DocId, edits: &[Value]) -> usize {
        let Some(doc) = self.doc(id) else { return 0 };
        let changes: Vec<crate::doc::Change> = edits
            .iter()
            .filter_map(|edit| {
                let range = edit.get("range")?;
                let (from_line, from_char) = crate::lsp::point_of(range.get("start")?)?;
                let (to_line, to_char) = crate::lsp::point_of(range.get("end")?)?;
                let text = edit.get("newText")?.as_str()?.to_string();
                let from = doc.char_at_lsp_point(from_line, from_char);
                let to = doc.char_at_lsp_point(to_line, to_char).max(from);
                Some(crate::doc::Change::replace(from, to, text))
            })
            .collect();
        self.apply_changes_to(id, changes)
    }

    /// One document, one undoable change, whoever worked the edits out.
    ///
    /// Shared by the language servers and the plugins deliberately: the
    /// sorting, the overlap check, the panes and the undo step are the awkward
    /// parts, and having two of them would mean having one that is wrong.
    fn apply_changes_to(&mut self, id: DocId, mut changes: Vec<crate::doc::Change>) -> usize {
        if changes.is_empty() {
            return 0;
        }
        // Edits arrive against the file as it is, in no particular order and
        // never overlapping. Sorting is all that is needed to make them a
        // transaction.
        changes.sort_by_key(|c| (c.from, c.to));
        changes.dedup_by(|a, b| a.from == b.from && a.to == b.to && a.text == b.text);
        if changes.windows(2).any(|pair| pair[0].to > pair[1].from) {
            self.say_bad("those edits overlap each other; nothing was changed");
            return 0;
        }
        let count = changes.len();

        // Every pane looking at this document has to hear about it, including
        // the focused one — which may not even be showing this file.
        let App { docs, panes, .. } = self;
        let Some(doc) = docs.iter_mut().find(|d| d.id == id) else {
            return 0;
        };
        let anchor = panes
            .iter()
            .find(|p| p.doc == id)
            .map(|p| p.sel.clone())
            .unwrap_or_default();
        let applied = doc.apply_atomic(changes, &anchor);
        let len = doc.len_chars();
        for pane in panes.iter_mut().filter(|p| p.doc == id) {
            pane.absorb(&applied, len);
        }

        let App { docs, lsp, hosts, .. } = self;
        if let Some(doc) = docs.iter().find(|d| d.id == id) {
            lsp.did_change(doc, &applied);
            hosts.changed(doc, &applied);
        }
        if let Some(doc) = self.doc_mut(id) {
            doc.take_pending();
        }
        self.scroll_into_view();
        count
    }
}

/// A panel's lines, as text, colours and the parts that do something.
///
/// Worked out together in one pass, so that a span can never point at text
/// that is not there — which it could if the text and the ranges were built
/// separately and one of them was changed later.
#[allow(clippy::type_complexity)]
fn panel_lines(
    lines: &[Value],
) -> (
    String,
    Vec<(Range, crate::theme::Role)>,
    Vec<(Range, String)>,
) {
    let mut text = String::new();
    let mut spans: Vec<(Range, crate::theme::Role)> = Vec::new();
    let mut actions: Vec<(Range, String)> = Vec::new();
    let mut at = 0usize;
    let nothing = Vec::new();
    for line in lines {
        // A bare string is a line with nothing marked in it, which is most
        // lines in most panels.
        if let Some(plain) = line.as_str() {
            text.push_str(plain);
            text.push('\n');
            at += plain.chars().count() + 1;
            continue;
        }
        for span in line.get("spans").and_then(Value::as_array).unwrap_or(&nothing) {
            let words = span.get("text").and_then(Value::as_str).unwrap_or_default();
            if words.is_empty() {
                continue;
            }
            // Characters, not bytes: a panel with a box-drawing character or
            // an accent in it must still line its colours up with its text.
            let end = at + words.chars().count();
            let range = Range::new(at, end);
            if let Some(role) = span.get("style").and_then(Value::as_str).and_then(panel_role) {
                spans.push((range, role));
            }
            if let Some(action) = span.get("action").and_then(Value::as_str) {
                actions.push((range, action.to_string()));
            }
            text.push_str(words);
            at = end;
        }
        text.push('\n');
        at += 1;
    }
    (text, spans, actions)
}

/// What a plugin's style name means, in the theme's terms.
///
/// Names rather than colours, on purpose. A panel asking for `keyword` is
/// themed with everything else and re-themes for free when the person switches
/// — where a plugin picking `#7FBDA7` would be a plugin that looks wrong on
/// eleven of the sixteen themes and cannot be fixed from outside.
///
/// The names are tree-sitter's, which the editor already knows, plus a couple
/// a plugin author would reach for that a grammar has no use for.
fn panel_role(name: &str) -> Option<crate::theme::Role> {
    match name {
        // Not a capture any grammar produces, and the first thing anybody
        // wants for the quiet half of a line.
        "muted" | "dim" => Some(crate::theme::Role::Comment),
        "warning" => Some(crate::theme::Role::Attribute),
        _ => crate::syntax::role_for(name),
    }
}

/// The rows one plugin's servers get in the plugins list.
///
/// `on` says whether a server id is switched on, which is the plugin's switch
/// and the server's own together. Handed in rather than asked for, so that
/// this can be read against a plugin that is not in the registry — which every
/// language server now is, since they are fetched rather than built in.
fn server_rows(plugin: &crate::plugin::Plugin, on: impl Fn(&str) -> bool) -> Vec<Row> {
    plugin
        .servers
        .iter()
        // A plugin that *is* one server is one row, not a row and an indented
        // copy of itself with the same switch on it.
        .filter(|server| server.id != plugin.id)
        .map(|server| {
            Row::new(
                format!("  {}", server.name),
                Choice::Plugin(server.id.clone()),
            )
            .detail(match plugin.languages.len() > 1 {
                // Which of the plugin's languages it is for, where that is a
                // question at all.
                true => format!(
                    "{} — runs {} for {}",
                    server.id,
                    server.command,
                    server.for_what()
                ),
                false => format!("{} — runs {}", server.id, server.command),
            })
            // Off with its plugin, and said so, rather than shown as on and
            // quietly doing nothing.
            .tag(match on(&server.id) {
                true => "on",
                false => "off",
            })
        })
        .collect()
}

/// The least a dock may be dragged down to, and the least it must leave
/// behind. Kept here rather than in the drawing because a drag has to refuse
/// the same sizes the layout would have clamped — a width that only looked
/// right because it was clamped is a width that springs back the moment the
/// terminal is resized.
const MIN_DOCK: u16 = 4;
const MIN_MIDDLE_ROOM: u16 = 20;

/// A line that is nothing but this is a horizontal rule, to be drawn as one.
pub const RULE: &str = "\u{2500}";

/// Coloured stretches of a line, as byte ranges into it, in order and not
/// overlapping.
pub type Spans = Vec<(std::ops::Range<usize>, Role)>;

/// One line of a popup: the text, and where the colours go in it.
///
/// Prose has no spans and is drawn in one colour. A line lifted out of a
/// fenced code block has the same spans the editor itself would give that
/// code, so a docstring's example reads as code rather than as a paragraph
/// that happens to contain brackets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocLine {
    pub text: String,
    /// Empty for anything that is not code, which is drawn in one colour.
    pub spans: Spans,
    /// The parts of this line that name something, as character ranges: the
    /// only parts a pointer can follow.
    ///
    /// Documentation is mostly prose, and prose is full of words. "the",
    /// "cursor", "document" and "primary" are not things to go to the
    /// definition of, and a box where every word lights up as you sweep across
    /// it is a box that has stopped meaning anything by lighting up. So this
    /// is worked out where the markup is read, when it is still known which
    /// letters were code and which were a sentence.
    pub links: Vec<std::ops::Range<usize>>,
}

impl DocLine {
    /// A line of prose. What is followable in it is whatever the markdown
    /// marked as code: `` `Foo` `` and the text of a `[`Foo`](…)` link, which
    /// is how every language server writes a name it means as a name.
    pub fn prose(text: impl Into<String>) -> Self {
        let text = text.into();
        let links = code_spans_in_prose(&text);
        Self {
            text,
            spans: Vec::new(),
            links,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Break a line too wide for the box into as many lines as it takes.
    ///
    /// Documentation arrives with the line breaks whoever wrote it chose, and
    /// a server sends a signature as one line however long it is. Cutting one
    /// off with an ellipsis loses exactly the half that says what the
    /// arguments are, and a box that only scrolls downwards has nowhere to
    /// put the rest — so it folds, the way the editor folds a long line.
    ///
    /// A fold keeps the indentation of the line it came from, so a bulleted
    /// list stays a list and a wrapped line of code stays under its own
    /// block. It breaks at a space where there is one and mid-word where
    /// there is not, because a Rust type with no spaces in it is a thing that
    /// happens and a row holding one character is not an improvement.
    pub fn wrap(&self, width: usize) -> Vec<DocLine> {
        // Below this there is no room for an indent and a word both, and the
        // folding turns into a column of single letters.
        let width = width.max(8);
        if self.text == RULE || crate::text::str_width(&self.text) <= width {
            return vec![self.clone()];
        }
        let chars: Vec<(usize, char)> = self.text.char_indices().collect();
        // What a fold is indented by: whatever the line itself was, unless
        // that leaves too little of the row to be worth folding into.
        let leading = chars.iter().take_while(|(_, c)| c.is_whitespace()).count();
        let indent: String = self.text[..byte_at(&chars, &self.text, leading)].to_string();
        let indent = match crate::text::str_width(&indent) + 8 <= width {
            true => indent,
            false => String::new(),
        };
        let indent_columns = crate::text::str_width(&indent);

        let mut out = Vec::new();
        let mut start = 0;
        let mut first = true;
        while start < chars.len() {
            let room = match first {
                true => width,
                false => width.saturating_sub(indent_columns).max(1),
            };
            // How far along the row we can get, and the last place a space
            // offered to break.
            let mut used = 0;
            let mut at = start;
            let mut after_space = None;
            while at < chars.len() {
                let mut buf = [0u8; 4];
                let wide = crate::text::str_width(chars[at].1.encode_utf8(&mut buf)).max(1);
                if used + wide > room {
                    break;
                }
                used += wide;
                at += 1;
                if chars[at - 1].1 == ' ' {
                    after_space = Some(at);
                }
            }
            let end = match at >= chars.len() {
                // The rest of it fits, so there is nothing left to break at.
                // Looking for a space here is what would fold `and on` after
                // `and` for no reason at all.
                true => chars.len(),
                // A break at the start of the row is no break at all: it
                // would hand the next row the same problem and never finish.
                false => match after_space {
                    Some(after) if after > start => after,
                    _ => at.max(start + 1).min(chars.len()),
                },
            };
            out.push(self.slice(&chars, start..end, &indent, first));
            if end >= chars.len() {
                break;
            }
            start = end;
            // A fold does not begin with the spaces it broke on.
            while start < chars.len() && chars[start].1 == ' ' {
                start += 1;
            }
            first = false;
        }
        // The whole line was spaces past the first row, which is nothing to
        // show and would otherwise be an empty row hanging off the bottom.
        if out.is_empty() {
            out.push(self.clone());
        }
        out
    }

    /// One row of a folded line: the characters `range` covers, under the
    /// indent, with the colours and the names that were on that stretch
    /// carried across and moved to where they now sit.
    fn slice(
        &self,
        chars: &[(usize, char)],
        range: std::ops::Range<usize>,
        indent: &str,
        first: bool,
    ) -> DocLine {
        let lead = match first {
            true => "",
            false => indent,
        };
        let from = byte_at(chars, &self.text, range.start);
        let to = byte_at(chars, &self.text, range.end);
        // Trailing spaces were how the break was chosen, not something to
        // draw.
        let body = self.text[from..to].trim_end();
        let text = format!("{lead}{body}");
        let bytes = from..from + body.len();
        let spans = self
            .spans
            .iter()
            .filter_map(|(span, role)| {
                let start = span.start.max(bytes.start);
                let end = span.end.min(bytes.end);
                (start < end).then(|| {
                    (
                        start - bytes.start + lead.len()..end - bytes.start + lead.len(),
                        *role,
                    )
                })
            })
            .collect();
        let lead_columns = lead.chars().count();
        let body_chars = body.chars().count();
        let links = self
            .links
            .iter()
            .filter_map(|link| {
                let start = link.start.max(range.start);
                let end = link.end.min(range.start + body_chars);
                (start < end).then(|| {
                    start - range.start + lead_columns..end - range.start + lead_columns
                })
            })
            .collect();
        DocLine { text, spans, links }
    }
}

/// Where character `at` begins in the string, with the end of it standing in
/// for one character past the last.
fn byte_at(chars: &[(usize, char)], text: &str, at: usize) -> usize {
    chars.get(at).map_or(text.len(), |(byte, _)| *byte)
}

/// A `MarkupContent`, a `MarkedString`, or a list of either, as lines a
/// terminal can show.
///
/// Markdown is flattened rather than rendered: fences go, headings lose their
/// hashes, and what is left is the sentences and the code, which is what
/// anybody was reading it for.
///
/// The code keeps its colours. A docstring is mostly an example, and an
/// example in the same colours as the file it came from is the difference
/// between reading it and deciphering it. `here` is the language of the file
/// being looked at, used for a fence that does not say — servers write plain
/// ```` ``` ```` around a signature constantly, and it is never another
/// language.
fn markup_lines(value: Option<&Value>, here: LangId) -> Vec<DocLine> {
    /// One `MarkupContent` or `MarkedString` before its markdown is read.
    struct Block {
        text: String,
        /// `Some` where the server said outright that the whole of this is
        /// code, which is what the old `MarkedString` form does. The language
        /// inside is the one it named, or `None` for one nothing here can
        /// parse — it is still code either way, and must not be read as
        /// markdown: `#include` is not a heading.
        code: Option<Option<LangId>>,
    }

    fn text_of(value: &Value, out: &mut Vec<Block>) {
        match value {
            Value::String(s) => out.push(Block {
                text: s.clone(),
                code: None,
            }),
            Value::Array(items) => items.iter().for_each(|item| text_of(item, out)),
            Value::Object(map) => {
                if let Some(Value::String(s)) = map.get("value") {
                    out.push(Block {
                        text: s.clone(),
                        code: map
                            .get("language")
                            .and_then(Value::as_str)
                            .map(lang::by_tag),
                    });
                }
            }
            _ => {}
        }
    }

    let Some(value) = value else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    text_of(value, &mut blocks);

    let mut lines: Vec<DocLine> = Vec::new();
    for block in blocks {
        match block.code {
            Some(lang) => push_code(&mut lines, &block.text, lang),
            None => push_markdown(&mut lines, &block.text, here),
        }
        if !lines.last().is_some_and(DocLine::is_empty) {
            lines.push(DocLine::prose(""));
        }
    }
    while lines.last().is_some_and(DocLine::is_empty) {
        lines.pop();
    }
    lines
}

/// Read one block of markdown, keeping the fenced code apart from the prose.
fn push_markdown(lines: &mut Vec<DocLine>, text: &str, here: LangId) {
    let mut code = String::new();
    let mut fenced: Option<Option<LangId>> = None;

    for line in text.lines() {
        let bare = line.trim_start();
        if bare.starts_with("```") || bare.starts_with("~~~") {
            match fenced.take() {
                // The end of a block: colour all of it at once, which is the
                // only way to get it right. A line at a time cannot tell a
                // string that runs over two lines from two strings.
                Some(lang) => {
                    push_code(lines, &code, lang);
                    code.clear();
                }
                None => {
                    // ```rust, ```rust,no_run, ```python title=x: the tag is
                    // the first word, and the rest is for a renderer we are
                    // not.
                    let info = bare.trim_start_matches(['`', '~']);
                    let tag = info
                        .split([',', ' ', '\t', '{'])
                        .next()
                        .unwrap_or_default()
                        .trim();
                    let lang = match tag.is_empty() {
                        true => (here != LangId::PLAIN).then_some(here),
                        false => lang::by_tag(tag),
                    };
                    fenced = Some(lang);
                }
            }
            // The fence itself says nothing a reader needs.
            continue;
        }
        if fenced.is_some() {
            code.push_str(line);
            code.push('\n');
            continue;
        }

        let line = line.trim_end();
        // A markdown rule is a rule, not three hyphens. Left as one
        // character for the drawing to stretch, since only the drawing
        // knows how wide the box turned out.
        let bare = line.trim();
        let line = if bare.len() >= 3 && bare.chars().all(|c| c == '-' || c == '_' || c == '*') {
            RULE
        } else {
            line.trim_start_matches('#').trim_start_matches(' ')
        };
        // Two blank lines in a row are one blank line.
        if line.is_empty() && lines.last().is_some_and(DocLine::is_empty) {
            continue;
        }
        lines.push(DocLine::prose(line));
    }

    // A fence nobody closed. Servers truncate documentation, so this happens.
    if let Some(lang) = fenced
        && !code.is_empty()
    {
        push_code(lines, &code, lang);
    }
}

/// Add a fenced block, coloured if there is a grammar for it.
///
/// Code is kept as it was written: no headings to strip, no rules to find, and
/// blank lines left alone, because in code they are the shape of the thing.
fn push_code(lines: &mut Vec<DocLine>, code: &str, lang: Option<LangId>) {
    let spans = lang.and_then(|lang| code_spans(code, lang));
    for (at, line) in code.lines().enumerate() {
        let text = line.trim_end().to_string();
        // Trailing whitespace went, so anything coloured past the new end goes
        // with it.
        let mut spans: Vec<_> = spans
            .as_ref()
            .and_then(|rows| rows.get(at))
            .cloned()
            .unwrap_or_default();
        spans.retain_mut(|(range, _)| {
            range.end = range.end.min(text.len());
            range.start < range.end
        });
        // In code, the names are the ones the grammar called names. A
        // keyword, a string or a number is not somewhere to go.
        let links = spans
            .iter()
            .filter(|(_, role)| names_something(*role))
            .filter_map(|(range, _)| {
                let start = text.get(..range.start)?.chars().count();
                let len = text.get(range.clone())?.chars().count();
                Some(start..start + len)
            })
            .collect();
        lines.push(DocLine { text, spans, links });
    }
}

/// Colour a whole fenced block, as spans within each of its lines.
///
/// `None` where the language has no grammar or the parser would not take it,
/// which is the ordinary case for most of the languages a docstring quotes and
/// means the code is drawn in one colour.
fn code_spans(code: &str, lang: LangId) -> Option<Vec<Spans>> {
    let grammar = lang::get(lang).grammar()?;
    let rope = ropey::Rope::from_str(code);
    let syntax = crate::syntax::Syntax::new(grammar, &rope)?;
    let spans = syntax.highlights(&rope, 0..rope.len_bytes());

    let mut rows = Vec::new();
    // Where this line starts in the block, and the first span that might still
    // reach it — a span can cover several lines, so the pointer only moves
    // past one once it has ended.
    let mut at = 0;
    let mut first = 0;
    for line in code.lines() {
        let end = at + line.len();
        while first < spans.len() && spans[first].0.end <= at {
            first += 1;
        }
        let mut row = Vec::new();
        for (range, role) in spans[first..].iter().take_while(|(r, _)| r.start < end) {
            let from = range.start.max(at) - at;
            let to = range.end.min(end) - at;
            if from < to {
                row.push((from..to, *role));
            }
        }
        rows.push(row);
        // Past the newline that `lines` took off.
        at = end + 1;
    }
    Some(rows)
}

/// `Location`, `Location[]`, or `LocationLink[]`, as places.
/// Somewhere a language server can point at.
///
/// Nearly always a file. Java is the exception worth the enum: `jdtls` answers
/// "where is this defined" for anything out of a jar with a `jdt://` URI,
/// which is not a file and never will be — the class is inside an archive, and
/// the only way to see it is to ask the server to hand the text over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    File(PathBuf),
    /// A URI only the server that gave it out can make sense of.
    Inside(String),
}

impl Target {
    /// What to call it in a list of places.
    fn label(&self, project: &Path) -> String {
        match self {
            Target::File(path) => short(path, project),
            // `jdt://contents/rt.jar/java.util/List.class?=…` — everything
            // after the `?` is for the server, and everything a person wants
            // is the two parts before it.
            Target::Inside(uri) => {
                let head = uri.split('?').next().unwrap_or(uri);
                let mut parts = head.rsplit('/');
                let file = parts.next().unwrap_or(head);
                match parts.next() {
                    Some(package) => format!("{package}.{}", file.trim_end_matches(".class")),
                    None => head.to_string(),
                }
            }
        }
    }
}

fn locations(value: &Value) -> Vec<(Target, usize, usize)> {
    fn one(value: &Value, out: &mut Vec<(Target, usize, usize)>) {
        // A `LocationLink` names the target differently from a `Location`,
        // and servers pick whichever they like.
        let uri = value
            .get("uri")
            .or_else(|| value.get("targetUri"))
            .and_then(Value::as_str);
        let range = value
            .get("range")
            .or_else(|| value.get("targetSelectionRange"))
            .or_else(|| value.get("targetRange"));
        if let (Some(uri), Some(range)) = (uri, range)
            && let Some(start) = range.get("start").and_then(crate::lsp::point_of)
        {
            let target = match crate::lsp::path_of(uri) {
                Some(path) => Target::File(path),
                None => Target::Inside(uri.to_string()),
            };
            out.push((target, start.0, start.1));
        }
    }
    let mut out = Vec::new();
    match value {
        Value::Array(items) => items.iter().for_each(|item| one(item, &mut out)),
        Value::Object(_) => one(value, &mut out),
        _ => {}
    }
    out
}

/// One suggestion, from the object a server sent.
fn suggestion_from(item: &Value, doc: &Document, at: usize) -> Option<Suggestion> {
    let label = item.get("label")?.as_str()?.trim().to_string();
    if label.is_empty() {
        return None;
    }
    // What a server sends beside the label rather than in it. `detail` goes
    // against the name — arguments, or the import this one brings with it —
    // and `description` is the dimmer note off to the right.
    let details = item.get("labelDetails");
    let suffix = details
        .and_then(|d| d.get("detail"))
        .and_then(Value::as_str)
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty());
    let description = details
        .and_then(|d| d.get("description"))
        .and_then(Value::as_str)
        .map(|d| d.replace('\n', " "))
        .filter(|d| !d.is_empty());
    let edit = item.get("textEdit");
    let range = edit.and_then(|e| {
        e.get("range")
            .or_else(|| e.get("replace"))
            .or_else(|| e.get("insert"))
    });
    let replace = range.and_then(|range| {
        let start = crate::lsp::point_of(range.get("start")?)?;
        let end = crate::lsp::point_of(range.get("end")?)?;
        let from = doc.char_at_lsp_point(start.0, start.1);
        let to = doc.char_at_lsp_point(end.0, end.1).max(from);
        // A range that does not reach the cursor is about somewhere else, and
        // acting on it would edit text nobody is looking at.
        (from <= at).then_some((from, to))
    });

    let insert = edit
        .and_then(|e| e.get("newText"))
        .and_then(Value::as_str)
        .or_else(|| item.get("insertText").and_then(Value::as_str))
        .unwrap_or(&label)
        .to_string();
    // Snippet support is deliberately not claimed, but servers send them
    // anyway. Taking the placeholders out leaves something usable rather than
    // something with `${1:self}` in it.
    let insert = strip_snippet(&insert);

    let also = item
        .get("additionalTextEdits")
        .and_then(Value::as_array)
        .map(|edits| {
            edits
                .iter()
                .filter_map(|edit| {
                    let range = edit.get("range")?;
                    let start = crate::lsp::point_of(range.get("start")?)?;
                    let end = crate::lsp::point_of(range.get("end")?)?;
                    Some((
                        doc.char_at_lsp_point(start.0, start.1),
                        doc.char_at_lsp_point(end.0, end.1),
                        edit.get("newText")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    Some(Suggestion {
        kind: completion_kind(item.get("kind").and_then(Value::as_u64).unwrap_or(0)),
        role: completion_role(item.get("kind").and_then(Value::as_u64).unwrap_or(0)),
        // Where the name lives beats what type it has, when a server says
        // both: the reason to be looking at this list is often that you do
        // not remember which module the name is in.
        detail: description.or_else(|| {
            item.get("detail")
                .and_then(Value::as_str)
                .map(|d| d.replace('\n', " "))
        }),
        sort: item
            .get("sortText")
            .and_then(Value::as_str)
            .unwrap_or(&label)
            .to_string(),
        about: markup_lines(item.get("documentation"), LangId::PLAIN)
            .into_iter()
            .next()
            .map(|line| line.text)
            .filter(|s| !s.is_empty()),
        replace,
        insert,
        label,
        suffix,
        also,
        raw: item.clone(),
        resolve: Resolve::Unasked,
    })
}

/// Take the placeholders out of a snippet, leaving the text.
fn strip_snippet(text: &str) -> String {
    if !text.contains('$') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // An escaped dollar is a dollar.
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // `${1:name}` — keep the name, drop the rest.
            Some('{') => {
                chars.next();
                let mut inner = String::new();
                let mut depth = 1;
                for c in chars.by_ref() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    if depth > 0 {
                        inner.push(c);
                    }
                }
                if let Some((_, name)) = inner.split_once(':') {
                    out.push_str(name);
                }
            }
            // `$1` — nothing to keep.
            Some(c) if c.is_ascii_digit() => {
                while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                    chars.next();
                }
            }
            _ => out.push('$'),
        }
    }
    out
}

/// What colour a completion is drawn in, from LSP's numbering.
///
/// The same colour the thing itself would have in the file. A list of forty
/// suggestions all in one colour is a list you have to read a word at a time
/// to find the method among the fields; give each kind the colour it already
/// has three lines up in the editor and the shape of the list is legible
/// before any of it has been read.
///
/// It is not a decoration and it is not a new vocabulary — a class in the
/// list is the colour a class is, a keyword is the colour a keyword is — so
/// there is nothing here to learn that reading the file has not taught
/// already, and a theme that has been thought about is thought about here too.
fn completion_role(n: u64) -> Role {
    match n {
        2 | 3 => Role::Function,      // method, function
        4 => Role::Constructor,       // constructor
        5 | 10 => Role::Property,     // field, property
        6 => Role::Variable,          // variable
        7 | 22 => Role::Type,         // class, struct
        8 => Role::Type,              // interface
        9 => Role::Namespace,         // module
        11 | 12 => Role::Constant,    // unit, value
        13 => Role::Type,             // enum
        14 => Role::Keyword,          // keyword
        15 => Role::Macro,            // snippet
        16 => Role::String,           // colour
        17 | 19 => Role::String,      // file, folder
        18 => Role::Variable,         // reference
        20 => Role::Constant,         // enum member
        21 => Role::Constant,         // constant
        23 => Role::Attribute,        // event
        24 => Role::Operator,         // operator
        25 => Role::Type,             // type parameter
        // Plain text, and anything a later LSP invents. Neither is a thing
        // with a colour of its own, and guessing one would be worse than the
        // ordinary foreground.
        _ => Role::Variable,
    }
}

/// What a completion is, in a word, from LSP's numbering.
fn completion_kind(n: u64) -> &'static str {
    match n {
        1 => "text",
        2 => "method",
        3 => "fn",
        4 => "new",
        5 => "field",
        6 => "var",
        7 => "class",
        8 => "trait",
        9 => "mod",
        10 => "prop",
        11 => "unit",
        12 => "value",
        13 => "enum",
        14 => "keyword",
        15 => "snippet",
        16 => "colour",
        17 => "file",
        18 => "ref",
        19 => "folder",
        20 => "member",
        21 => "const",
        22 => "struct",
        23 => "event",
        24 => "op",
        25 => "type",
        _ => "",
    }
}

/// And the same for symbols.
fn symbol_kind(n: u64) -> &'static str {
    match n {
        1 => "file",
        2 => "mod",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "prop",
        8 => "field",
        9 => "new",
        10 => "enum",
        11 => "trait",
        12 => "fn",
        13 => "var",
        14 => "const",
        15 => "str",
        16 => "num",
        17 => "bool",
        18 => "array",
        22 => "variant",
        23 => "struct",
        26 => "type",
        _ => "",
    }
}

/// Symbols, flattened: a `DocumentSymbol` tree indents, a `SymbolInformation`
/// list does not, and servers send whichever they feel like.
fn collect_symbols(value: &Value, doc: &Document, depth: usize, out: &mut Vec<Row>) {
    let Value::Array(items) = value else { return };
    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let range = item
            .get("selectionRange")
            .or_else(|| item.get("range"))
            .or_else(|| item.get("location").and_then(|l| l.get("range")));
        let Some((line, column)) = range
            .and_then(|r| r.get("start"))
            .and_then(crate::lsp::point_of)
        else {
            continue;
        };
        let at = doc.char_at_lsp_point(line, column);
        let mut row = Row::new(format!("{}{name}", "  ".repeat(depth)), Choice::Here(at))
            .detail(format!("line {}", line + 1));
        if let Some(kind) = item.get("kind").and_then(Value::as_u64) {
            row = row.tag(symbol_kind(kind));
        }
        if let Some(detail) = item.get("detail").and_then(Value::as_str)
            && !detail.is_empty()
        {
            row = row.detail(detail.replace('\n', " "));
        }
        out.push(row);
        if let Some(children) = item.get("children") {
            collect_symbols(children, doc, depth + 1, out);
        }
    }
}

// ---------------------------------------------------------------------------
// The mouse.
//
// Everything reachable by keyboard is reachable by mouse, because half the
// people who open an editor reach for the mouse first and there is no good
// reason to make them wrong. Click to put the cursor somewhere, drag to
// select, double click for a word, triple click for a line. The line numbers
// select lines. The tabs switch and close files. The things in the status bar
// are buttons: the language name opens the language list, the position opens
// "go to line", the count of problems opens the problem list.
// ---------------------------------------------------------------------------

impl App {
    fn on_mouse(&mut self, event: MouseEvent) {
        if !self.mouse_on {
            return;
        }
        let (column, row) = (event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(column, row, event.modifiers),
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(column, row),
            MouseEventKind::Up(_) => self.drag = None,
            MouseEventKind::ScrollUp => self.wheel(column, row, -3),
            MouseEventKind::ScrollDown => self.wheel(column, row, 3),
            MouseEventKind::ScrollLeft => self.pan(column, row, -4),
            MouseEventKind::ScrollRight => self.pan(column, row, 4),
            MouseEventKind::Moved => self.mouse_moved(column, row),
            MouseEventKind::Down(MouseButton::Right) => self.right_click(column, row),
            MouseEventKind::Down(MouseButton::Middle) => {
                if let Some(at) = self.position_at(column, row) {
                    self.place_cursor(at, false, false);
                    self.run(Cmd::PASTE);
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, column: u16, row: u16, mods: KeyModifiers) {
        // How many clicks this is: two or three in the same place, quickly, is
        // a word or a line.
        let now = Instant::now();
        let count = match self.last_click {
            Some((when, c, r, n))
                if now.duration_since(when) < DOUBLE_CLICK
                    && c.abs_diff(column) <= 1
                    && r == row =>
            {
                (n % 3) + 1
            }
            _ => 1,
        };
        self.last_click = Some((now, column, row, count));
        self.click_away_from_suggestions(column, row);

        // A divider, before anything else looks at where the click landed. It
        // is chrome rather than text, and dragging it is the only thing it
        // does.
        if let Some(pane) = self.grip_at(column, row) {
            self.drag = Some(Drag::DockEdge { pane });
            return;
        }

        // The context menu is on top of everything, including the list.
        if let Overlay::Menu(m) = &mut self.overlay {
            let area = m.area;
            // What was clicked, rather than what was highlighted. They are
            // usually the same row, and assuming so meant a click on a divider
            // ran the highlight instead — which, in a menu that opens with
            // "Cut" lit, cut your selection.
            let chosen = hits(area, column, row).then(|| m.at((row - area.y) as usize + m.scroll));
            self.overlay = Overlay::None;
            if let Some(Some(action)) = chosen {
                self.do_menu(action);
            }
            return;
        }

        // A hover you can see is a hover you can click into: clicking it puts
        // the keyboard in it rather than moving the cursor to whatever text is
        // behind it, and Ctrl-clicking a name in it goes looking for that name
        // the way Ctrl-clicking a name in the code does.
        if let Some(hover) = &mut self.hover
            && matches!(self.overlay, Overlay::None)
            && hits(hover.outer, column, row)
        {
            let link = hover.link_at(column, row);
            hover.focused = true;
            hover.pointer = Some((column, row));
            if mods.contains(KeyModifiers::CONTROL)
                && let Some(link) = link
            {
                self.hover = None;
                return self.look_up(&link.word);
            }
            // Otherwise it is text, and clicking text is where a selection
            // starts — the same gesture as in the editor, because from where
            // you are sitting it is the same thing.
            if let Some(spot) = hover.spot_at(column, row) {
                hover.select = Some((spot, spot));
                if count >= 2 {
                    hover.take_word();
                } else {
                    self.drag = Some(Drag::Popup);
                }
            }
            return;
        }

        // A list on top of everything gets the click, and a click outside it
        // closes it — which is what clicking away from a menu means.
        if let Overlay::Picker(picker) = &mut self.overlay {
            let area = picker.area;
            if row >= area.y
                && row < area.y + area.height
                && column >= area.x
                && column < area.x + area.width
            {
                let at = picker.top + (row - area.y) as usize;
                if at < picker.len() {
                    picker.select(at);
                    self.after_picker_moved();
                    self.choose();
                }
            } else {
                // Clicking away from a list is closing it, which means the
                // same as Escape — including putting back a theme that was
                // only being tried on.
                let restore = picker.restore.clone();
                self.overlay = Overlay::None;
                if let Some(name) = restore {
                    self.set_theme(&name);
                }
            }
            return;
        }
        if !matches!(self.overlay, Overlay::None) {
            return;
        }

        // The tabs. The ‹ › at the ends first: each one is drawn over a column
        // that belongs to the tab beneath it, and the arrow is what is on the
        // screen there.
        if let Some(to) = self
            .tab_nudges
            .iter()
            .find(|(area, _)| hits(*area, column, row))
            .map(|(_, to)| *to)
        {
            self.tab_scroll = to;
            return;
        }
        if let Some((id, close)) = self
            .tab_hits
            .iter()
            .find(|(area, _, _)| hits(*area, column, row))
            .map(|(_, id, close)| (*id, *close))
        {
            if close {
                let here = self.view().doc;
                self.show(id);
                if self.doc(id).is_some_and(Document::is_modified) {
                    self.close(false);
                } else {
                    self.close_doc(id);
                    if self.doc(here).is_some() && here != id {
                        self.show(here);
                    }
                }
            } else {
                self.show(id);
                // And it is now held, so moving the pointer carries it along
                // the row. A press that never moves is just a click, because
                // a tab that has not gone anywhere has not been reordered.
                self.drag = Some(Drag::Tab {
                    id,
                    at: (column, row),
                    stepped: Instant::now(),
                });
            }
            return;
        }

        // The status bar.
        if let Some(cmd) = self
            .status_hits
            .iter()
            .find(|(area, _)| hits(*area, column, row))
            .map(|(_, cmd)| *cmd)
        {
            return self.run(cmd);
        }

        // The completion list.
        if let Some(completion) = &mut self.completion {
            let area = completion.area;
            if hits(area, column, row) {
                let at = completion.top + (row - area.y) as usize;
                if at < completion.len() {
                    completion.cursor = at;
                    // A row that has never been under the cursor has never
                    // been asked about, so this goes through the same wait as
                    // a Tab does rather than dropping the import.
                    self.accept_completion();
                }
                return;
            }
        }

        let Some(pane) = self.pane_at(column, row) else {
            return;
        };
        if pane != self.focus {
            self.focus = pane;
            self.dismiss_popups();
            self.completion = None;
        }
        let view = &self.panes[pane];
        let frame = view.frame;

        // The scroll bar down the right edge.
        if frame.width > 1 && column == frame.x + frame.width - 1 {
            self.drag = Some(Drag::Scrollbar);
            return self.scroll_to_bar(row);
        }
        // The line numbers: clicking one takes the line.
        if column < view.area.x {
            let Some(at) = self.position_at(view.area.x, row) else {
                return;
            };
            self.place_cursor(at, false, false);
            let (doc, view) = self.pair();
            edit::select_line(doc, view);
            self.drag = Some(Drag::Lines { anchor: at });
            return;
        }

        let Some(at) = self.position_at(column, row) else {
            return;
        };
        // A panel is a plugin's own buffer, and the parts of it the plugin
        // marked as doing something do it when you click them — which is what
        // "clickable" has meant on a screen for forty years.
        if self.panel_action_at(at) {
            self.place_cursor(at, false, false);
            return;
        }
        // Ctrl-click is what every editor has taught people goes to the
        // definition of the thing under the pointer.
        if mods.contains(KeyModifiers::CONTROL) {
            self.place_cursor(at, false, false);
            return self.run(Cmd::GOTO_DEFINITION);
        }
        match count {
            2 => {
                let word = text::word_around(&self.here().rope, at);
                self.view_mut().sel = Selections::single(word.forward());
                self.drag = Some(Drag::Words {
                    anchor_start: word.start(),
                    anchor_end: word.end(),
                });
            }
            3 => {
                self.place_cursor(at, false, false);
                let (doc, view) = self.pair();
                edit::select_line(doc, view);
                self.drag = Some(Drag::Lines { anchor: at });
            }
            _ => {
                self.place_cursor(
                    at,
                    mods.contains(KeyModifiers::SHIFT),
                    mods.contains(KeyModifiers::ALT),
                );
                self.drag = Some(Drag::Text);
            }
        }
        self.dismiss_popups();
    }

    /// The pointer went past, without any button held.
    ///
    /// Three things want to know: a menu, whose highlight follows the pointer
    /// the way every menu's does; a hover, which lights up the name under the
    /// pointer and stays open while you are inside it; and the editor itself,
    /// where sitting still over a word is a question.
    fn mouse_moved(&mut self, column: u16, row: u16) {
        if let Overlay::Menu(menu) = &mut self.overlay {
            let area = menu.area;
            if hits(area, column, row) {
                menu.point_at((row - area.y) as usize + menu.scroll);
            }
            return;
        }
        if let Some(hover) = &mut self.hover
            && hits(hover.outer, column, row)
        {
            // Inside the box. It stays, whether or not it has the keyboard,
            // because a box that vanished as you reached for it could never be
            // clicked on at all.
            hover.pointer = Some((column, row));
            self.resting = None;
            return;
        }
        if let Some(hover) = &mut self.hover {
            hover.pointer = None;
        }
        // Sitting still over a word is a question. Moving is not.
        match self.resting {
            Some((_, c, r)) if c == column && r == row => {}
            _ => {
                self.resting = Some((Instant::now(), column, row));
                // A hover you have asked to read stays while you move about;
                // one that appeared on its own goes as soon as you look away.
                if self.hover.as_ref().is_some_and(|h| !h.focused) {
                    self.hover = None;
                }
            }
        }
    }

    /// The right button asks what can be done here.
    ///
    /// On a tab that is about the file; anywhere in the text it is about the
    /// code under the pointer. Clicking inside a selection keeps it, because
    /// "select this, then right-click, then copy" is the whole reason the menu
    /// is there and moving the cursor first would throw the selection away.
    /// Close the list of suggestions, unless the click landed on it.
    ///
    /// Clicking somewhere else is going somewhere else, and a list of
    /// completions for a word you have left is worse than no list at all: it
    /// still owns Tab and Enter, so the next thing you press finishes a word
    /// that is no longer under the cursor. Every editor closes it on a click
    /// away, which is why nobody ever thinks about this until one does not.
    ///
    /// An empty list counts as not clicked on. It is not drawn, so its last
    /// known place on the screen is not a place, and a click there is a click
    /// on the text underneath.
    fn click_away_from_suggestions(&mut self, column: u16, row: u16) {
        let on_the_list = self
            .completion
            .as_ref()
            .is_some_and(|list| !list.is_empty() && hits(list.area, column, row));
        if !on_the_list {
            self.completion = None;
        }
    }

    fn right_click(&mut self, column: u16, row: u16) {
        // Asking what can be done here is leaving whatever word you were
        // part-way through, so the suggestions go — including where the menu
        // is about to be drawn over the top of them.
        self.completion = None;

        // A menu already open is closed by a second right click, the way a
        // second press of any key that opens something closes it.
        if matches!(self.overlay, Overlay::Menu(_)) {
            self.overlay = Overlay::None;
            return;
        }
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        if let Some(id) = self
            .tab_hits
            .iter()
            .find(|(area, _, _)| hits(*area, column, row))
            .map(|(_, id, _)| *id)
        {
            let menu = self.tab_menu(id, (column, row));
            self.overlay = Overlay::Menu(menu);
            return;
        }
        let Some(pane) = self.pane_at(column, row) else {
            return;
        };
        if pane != self.focus {
            self.focus = pane;
            self.completion = None;
        }
        self.dismiss_popups();
        if let Some(at) = self.position_at(column, row) {
            let inside = self
                .view()
                .sel
                .ranges()
                .iter()
                .any(|range| !range.is_empty() && range.start() <= at && at < range.end());
            if !inside {
                self.place_cursor(at, false, false);
            }
        }
        // A panel is a plugin's buffer, and the editor's own menu for it is
        // Cut and Paste greyed out — true, and no use to anybody. The gesture
        // goes to the plugin instead, with where it landed and whatever it had
        // marked there, so it can put up a menu of its own.
        if let Some((plugin, panel)) = self
            .here()
            .panel
            .as_ref()
            .map(|p| (p.plugin.clone(), p.id.clone()))
        {
            let at = self.view().cursor();
            let (line, column) = self.here().point_at_char(at);
            let action = self
                .here()
                .panel
                .as_ref()
                .and_then(|p| {
                    p.actions
                        .iter()
                        .find(|(range, _)| range.start() <= at && at < range.end())
                })
                .map(|(_, action)| action.clone());
            return self.tell_panel(
                &plugin,
                "panel/context",
                json!({ "panel": panel, "line": line, "column": column, "action": action }),
            );
        }
        let menu = self.text_menu((column, row));
        self.overlay = Overlay::Menu(menu);
    }

    /// Put the cursor somewhere. `extend` keeps the anchor, `add` leaves the
    /// cursors that were already there — Alt-click, which is how you get a
    /// second cursor without leaving the mouse.
    fn place_cursor(&mut self, at: usize, extend: bool, add: bool) {
        let view = self.view_mut();
        if add {
            view.sel.push(Range::point(at));
        } else if extend {
            let anchor = view.sel.primary().anchor;
            view.sel = Selections::single(Range::new(anchor, at));
        } else {
            view.sel = Selections::single(Range::point(at));
        }
        view.goal = None;
        self.scroll_into_view();
    }

    /// Which docked pane's divider is under this point, if any.
    fn grip_at(&self, column: u16, row: u16) -> Option<usize> {
        self.panes.iter().position(|pane| {
            pane.grip.is_some_and(|grip| {
                column >= grip.x
                    && column < grip.x + grip.width
                    && row >= grip.y
                    && row < grip.y + grip.height
            })
        })
    }

    /// Make a dock as wide, or as tall, as the pointer says.
    ///
    /// Measured from the far edge of the dock rather than by how far the
    /// pointer has moved, so the divider stays under the pointer instead of
    /// drifting away from it over a long drag.
    fn resize_dock(&mut self, pane: usize, column: u16, row: u16) {
        let screen = self.screen;
        let Some(view) = self.panes.get(pane) else { return };
        let (Some(dock), frame) = (view.dock, view.frame) else {
            return;
        };
        let wanted = match dock.edge {
            crate::view::Edge::Left => column.saturating_sub(frame.x) + 1,
            crate::view::Edge::Right => frame.right().saturating_sub(column),
            crate::view::Edge::Bottom => frame.bottom().saturating_sub(row),
        };
        // Never so narrow there is nothing in it, and never so wide the middle
        // is squeezed out — the layout clamps the second of those too, but a
        // size that only looks right because it was clamped is a size that
        // springs back the moment the terminal is resized.
        let room = match dock.edge.is_side() {
            true => screen.width,
            false => screen.height.saturating_sub(2),
        };
        let most = room.saturating_sub(MIN_MIDDLE_ROOM).max(MIN_DOCK);
        let size = wanted.clamp(MIN_DOCK, most);
        if let Some(view) = self.panes.get_mut(pane)
            && let Some(dock) = &mut view.dock
            && dock.size != size
        {
            dock.size = size;
            self.session_changed();
        }
    }

    fn drag_to(&mut self, column: u16, row: u16) {
        match self.drag {
            Some(Drag::Popup) => {
                let Some(hover) = &mut self.hover else { return };
                // Dragging off the top or bottom scrolls, so a selection can
                // be longer than the box is tall.
                if row < hover.area.y {
                    hover.scroll_by(-1);
                } else if row >= hover.area.y + hover.area.height {
                    hover.scroll_by(1);
                }
                if let Some(spot) = hover.spot_at(column, row)
                    && let Some((anchor, _)) = hover.select
                {
                    hover.select = Some((anchor, spot));
                }
            }
            Some(Drag::Scrollbar) => self.scroll_to_bar(row),
            Some(Drag::DockEdge { pane }) => self.resize_dock(pane, column, row),
            Some(Drag::Tab { id, .. }) => {
                if let Some(Drag::Tab { at, .. }) = &mut self.drag {
                    *at = (column, row);
                }
                self.drag_tab(id, column, row);
            }
            Some(Drag::Text) => {
                let Some(at) = self.position_at(column, row) else {
                    return;
                };
                let anchor = self.view().sel.primary().anchor;
                self.view_mut().sel = Selections::single(Range::new(anchor, at));
                self.scroll_into_view();
            }
            Some(Drag::Words {
                anchor_start,
                anchor_end,
            }) => {
                let Some(at) = self.position_at(column, row) else {
                    return;
                };
                // Dragging after a double click grows a word at a time, in
                // whichever direction you go.
                let word = text::word_around(&self.here().rope, at);
                let range = if word.start() < anchor_start {
                    Range::new(anchor_end, word.start())
                } else {
                    Range::new(anchor_start, word.end())
                };
                self.view_mut().sel = Selections::single(range);
                self.scroll_into_view();
            }
            Some(Drag::Lines { anchor }) => {
                let Some(at) = self.position_at(column, row) else {
                    return;
                };
                let doc = self.here();
                let first = text::line_of(&doc.rope, anchor.min(at));
                let last = text::line_of(&doc.rope, anchor.max(at));
                let start = text::line_start(&doc.rope, first);
                let end = if last + 1 < doc.len_lines() {
                    text::line_start(&doc.rope, last + 1)
                } else {
                    doc.len_chars()
                };
                let range = if at < anchor {
                    Range::new(end, start)
                } else {
                    Range::new(start, end)
                };
                self.view_mut().sel = Selections::single(range);
                self.scroll_into_view();
            }
            None => {}
        }
    }

    fn wheel(&mut self, column: u16, row: u16, by: isize) {
        // Whatever is on top scrolls, then whichever pane the pointer is over
        // — which is what makes reading two files side by side work.
        match &mut self.overlay {
            Overlay::Picker(picker) => {
                picker.step(by.signum() * 3);
                return self.after_picker_moved();
            }
            Overlay::Help(scroll) => {
                *scroll = (*scroll as isize + by * 2).max(0) as usize;
                return;
            }
            Overlay::Menu(menu) => return menu.step(by.signum()),
            _ => {}
        }
        // The wheel over the tabs walks along them. A vertical wheel is what
        // most mice have, and "there are more tabs that way" is the only thing
        // scrolling can mean on a row one line tall.
        if self.tab_row(column, row) {
            return self.scroll_tabs(by * 2);
        }
        if let Some(completion) = &mut self.completion
            && hits(completion.area, column, row)
        {
            completion.step(by.signum());
            self.resolve_selected();
            return;
        }
        // The wheel over a hover scrolls the hover, not the file behind it.
        if let Some(hover) = &mut self.hover
            && (hover.focused || hits(hover.outer, column, row))
        {
            hover.scroll_by(by);
            return;
        }
        let Some(pane) = self.pane_at(column, row) else {
            return;
        };
        let tab_width = self.config.tab_width();
        let id = self.panes[pane].doc;
        let Some(index) = self.docs.iter().position(|d| d.id == id) else {
            return;
        };
        let (docs, panes) = (&self.docs, &mut self.panes);
        view::scroll_by(&mut panes[pane], &docs[index], tab_width, by);
    }

    fn pan(&mut self, column: u16, row: u16, by: isize) {
        if self.tab_row(column, row) {
            return self.scroll_tabs(by);
        }
        if self.view().wrap {
            return;
        }
        let left = self.view().left as isize + by;
        self.view_mut().left = left.max(0) as usize;
    }

    /// Whether a point is on the row of tabs. The wheel there scrolls the tabs
    /// rather than the file, because there is nothing else it could sensibly
    /// mean and twenty open files need scrolling somehow.
    fn tab_row(&self, column: u16, row: u16) -> bool {
        row == self.screen.y
            && column >= self.screen.x
            && column < self.screen.x + self.screen.width
    }

    /// Move the row of tabs sideways. The far end is worked out by the drawing,
    /// which is the only thing that knows how wide the tabs came out, so this
    /// only has to keep it from going negative.
    fn scroll_tabs(&mut self, by: isize) {
        let at = self.tab_scroll as isize + by;
        self.tab_scroll = at.clamp(0, u16::MAX as isize) as u16;
    }

    /// Move the view to where the scroll bar was grabbed.
    fn scroll_to_bar(&mut self, row: u16) {
        let pane = self.focus.min(self.panes.len() - 1);
        let frame = self.panes[pane].frame;
        if frame.height == 0 {
            return;
        }
        let along = (row.saturating_sub(frame.y)) as f32 / frame.height as f32;
        let lines = self.here().len_lines();
        let top = (along * lines as f32) as usize;
        self.panes[pane].top = top.min(lines.saturating_sub(1));
        self.panes[pane].top_row = 0;
    }

    /// Which pane a point is in.
    fn pane_at(&self, column: u16, row: u16) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| hits(pane.frame, column, row))
    }

    /// What character a point is over, in whichever pane it is in.
    pub fn position_at(&self, column: u16, row: u16) -> Option<usize> {
        let pane = self.pane_at(column, row)?;
        let view = &self.panes[pane];
        let area = view.area;
        if row < area.y || row >= area.y + area.height {
            return None;
        }
        let doc = self.doc(view.doc)?;
        // A click left of the text is the start of the line, not nothing:
        // clicking the line numbers should still put you somewhere.
        let across = column
            .saturating_sub(area.x)
            .min(area.width.saturating_sub(1));
        Some(view::position_at_screen(
            view,
            doc,
            self.config.tab_width(),
            (row - area.y) as usize,
            across as usize,
        ))
    }
}

/// Whether a point is inside a rectangle.
fn hits(area: Rect, column: u16, row: u16) -> bool {
    area.width > 0
        && area.height > 0
        && column >= area.x
        && column < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
}

// ---- Everything textfold can be told to do ----

/// The command table.
///
/// One row per command, and the row is the only place that command is written
/// down: the name a settings file binds a key to, the group and the line the
/// palette shows, what it does to the text, and what it actually does. The key
/// bindings, the palette and the context menus all read this, so there is no
/// second list for somebody to forget.
///
/// It lives here rather than in [`crate::cmd`] because a row *is* behaviour
/// now — it names a method on `App`, and those are this module's to reach.
macro_rules! commands {
    ($($konst:ident => $name:literal, $group:ident, $behaviour:ident, $about:literal,
        $run:expr;)*) => {
        pub const BUILT_IN: &[Spec] = &[
            $(Spec {
                name: $name,
                group: Group::$group,
                behaviour: Behaviour::$behaviour,
                about: $about,
                run: $run,
            },)*
        ];

        /// A constant per command, so that a menu row or a default binding
        /// names one the way it always did. Worked out from the table at
        /// compile time: a constant naming a command that is not in the table
        /// does not build.
        ///
        /// Every command gets one whether or not anything in this build
        /// happens to name it — it is the handle on the row, not a convenience
        /// for whoever needed one first.
        #[allow(dead_code)]
        impl Cmd {
            $(pub const $konst: Cmd = Cmd::at(index_of($name));)*
        }
    };
}

/// Where a name sits in the table, worked out while compiling.
const fn index_of(name: &str) -> u16 {
    let mut at = 0;
    while at < BUILT_IN.len() {
        if same(BUILT_IN[at].name, name) {
            return at as u16;
        }
        at += 1;
    }
    panic!("a command constant naming a command that is not in the table");
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut at = 0;
    while at < a.len() {
        if a[at] != b[at] {
            return false;
        }
        at += 1;
    }
    true
}

commands! {
    NEW => "new", File, Passive, "Start an empty buffer",
        |app| app.new_buffer();
    OPEN => "open", File, Passive, "Open a file by name, fuzzily",
        |app| app.open_files_picker();
    OPEN_PATH => "open-path", File, Passive, "Open a file by typing its path, exactly",
        |app| app.open_prompt(PromptKind::OpenPath);
    SAVE => "save", File, Passive, "Write this file to disk",
        |app| app.save(None);
    SAVE_AS => "save-as", File, Passive, "Write this file somewhere else",
        |app| app.open_prompt(PromptKind::SaveAs);
    SAVE_ALL => "save-all", File, Passive, "Write every changed file",
        |app| app.save_all();
    RELOAD => "reload", File, Passive, "Read this file again, throwing away changes",
        |app| app.reload();
    CLOSE => "close", File, Passive, "Close this buffer, asking about unsaved changes",
        |app| app.close(false);
    CLOSE_FORCE => "close!", File, Passive, "Close this buffer, changes and all",
        |app| app.close(true);
    CLOSE_OTHERS => "close-others", File, Passive, "Close every buffer but this one",
        |app| app.close_many(Keep::Others);
    CLOSE_SAVED => "close-saved", File, Passive, "Close every buffer with nothing unsaved in it",
        |app| app.close_many(Keep::Unsaved);
    CLOSE_ALL => "close-all", File, Passive, "Close every buffer",
        |app| app.close_many(Keep::Nothing);
    COPY_PATH => "copy-path", File, Passive, "Copy this file's full path",
        |app| app.copy_path(false);
    COPY_RELATIVE_PATH => "copy-relative-path", File, Passive, "Copy this file's path from the project root",
        |app| app.copy_path(true);
    NEXT_BUFFER => "next-buffer", File, Passive, "The buffer after this one",
        |app| app.step_buffer(1);
    PREV_BUFFER => "prev-buffer", File, Passive, "The buffer before this one",
        |app| app.step_buffer(-1);
    MOVE_TAB_LEFT => "move-tab-left", File, Passive, "Move this tab one place towards the front",
        |app| app.step_tab(-1);
    MOVE_TAB_RIGHT => "move-tab-right", File, Passive, "Move this tab one place towards the back",
        |app| app.step_tab(1);
    BUFFERS => "buffers", File, Passive, "Pick from the open buffers",
        |app| app.open_buffers_picker();
    QUIT => "quit", File, Passive, "Leave, asking about unsaved changes",
        |app| app.leave(false);
    QUIT_FORCE => "quit!", File, Passive, "Leave, changes and all",
        |app| app.leave(true);
    MOVE_LEFT => "left", Move, Passive, "One character left",
        |app| app.motion(Motion::Left, false);
    MOVE_RIGHT => "right", Move, Passive, "One character right",
        |app| app.motion(Motion::Right, false);
    MOVE_UP => "up", Move, Passive, "One line up",
        |app| app.motion(Motion::Up, false);
    MOVE_DOWN => "down", Move, Passive, "One line down",
        |app| app.motion(Motion::Down, false);
    MOVE_WORD_LEFT => "word-left", Move, Passive, "To the start of the word before",
        |app| app.motion(Motion::WordLeft, false);
    MOVE_WORD_RIGHT => "word-right", Move, Passive, "To the end of the word after",
        |app| app.motion(Motion::WordRight, false);
    MOVE_LINE_START => "line-start", Move, Passive, "To the first thing on the line, then to column one",
        |app| app.motion(Motion::LineStart, false);
    MOVE_LINE_END => "line-end", Move, Passive, "To the end of the line",
        |app| app.motion(Motion::LineEnd, false);
    MOVE_PAGE_UP => "page-up", Move, Passive, "A screenful up",
        |app| app.motion(Motion::PageUp, false);
    MOVE_PAGE_DOWN => "page-down", Move, Passive, "A screenful down",
        |app| app.motion(Motion::PageDown, false);
    MOVE_DOC_START => "doc-start", Move, Passive, "To the top of the file",
        |app| app.motion(Motion::DocStart, false);
    MOVE_DOC_END => "doc-end", Move, Passive, "To the bottom of the file",
        |app| app.motion(Motion::DocEnd, false);
    MOVE_PARA_UP => "para-up", Move, Passive, "To the blank line above",
        |app| app.motion(Motion::ParaUp, false);
    MOVE_PARA_DOWN => "para-down", Move, Passive, "To the blank line below",
        |app| app.motion(Motion::ParaDown, false);
    MATCH_BRACKET => "match-bracket", Move, Passive, "To the bracket matching this one",
        |app| app.go_to_matching_bracket();
    GOTO_LINE => "goto-line", Move, Passive, "Jump to a line by number",
        |app| app.open_prompt(PromptKind::GotoLine);
    JUMP_BACK => "jump-back", Move, Passive, "Back to where you were before the last jump",
        |app| app.jump(false);
    JUMP_FORWARD => "jump-forward", Move, Passive, "Forward again",
        |app| app.jump(true);
    SCROLL_UP => "scroll-up", Move, Passive, "Move the view up, leaving the cursor",
        |app| app.scroll(-3);
    SCROLL_DOWN => "scroll-down", Move, Passive, "Move the view down, leaving the cursor",
        |app| app.scroll(3);
    CENTRE_CURSOR => "centre-cursor", Move, Passive, "Put the cursor's line in the middle of the screen",
        |app| app.centre();
    EXTEND_LEFT => "extend-left", Select, Passive, "Select one character left",
        |app| app.motion(Motion::Left, true);
    EXTEND_RIGHT => "extend-right", Select, Passive, "Select one character right",
        |app| app.motion(Motion::Right, true);
    EXTEND_UP => "extend-up", Select, Passive, "Select one line up",
        |app| app.motion(Motion::Up, true);
    EXTEND_DOWN => "extend-down", Select, Passive, "Select one line down",
        |app| app.motion(Motion::Down, true);
    EXTEND_WORD_LEFT => "extend-word-left", Select, Passive, "Select to the word before",
        |app| app.motion(Motion::WordLeft, true);
    EXTEND_WORD_RIGHT => "extend-word-right", Select, Passive, "Select to the word after",
        |app| app.motion(Motion::WordRight, true);
    EXTEND_LINE_START => "extend-line-start", Select, Passive, "Select to the start of the line",
        |app| app.motion(Motion::LineStart, true);
    EXTEND_LINE_END => "extend-line-end", Select, Passive, "Select to the end of the line",
        |app| app.motion(Motion::LineEnd, true);
    EXTEND_PAGE_UP => "extend-page-up", Select, Passive, "Select a screenful up",
        |app| app.motion(Motion::PageUp, true);
    EXTEND_PAGE_DOWN => "extend-page-down", Select, Passive, "Select a screenful down",
        |app| app.motion(Motion::PageDown, true);
    EXTEND_DOC_START => "extend-doc-start", Select, Passive, "Select to the top of the file",
        |app| app.motion(Motion::DocStart, true);
    EXTEND_DOC_END => "extend-doc-end", Select, Passive, "Select to the bottom of the file",
        |app| app.motion(Motion::DocEnd, true);
    SELECT_ALL => "select-all", Select, Passive, "Select the whole file",
        |app| app.select_all();
    SELECT_LINE => "select-line", Select, Passive, "Select this line, then the one below",
        |app| app.select_line();
    SELECT_WORD => "select-word", Select, Passive, "Select the word under the cursor",
        |app| app.select_word();
    EXPAND_SELECTION => "expand-selection", Select, Passive, "Grow the selection to the syntax around it",
        |app| app.expand_selection();
    ADD_CURSOR_ABOVE => "add-cursor-above", Select, Passive, "Another cursor on the line above",
        |app| app.add_cursor_above();
    ADD_CURSOR_BELOW => "add-cursor-below", Select, Passive, "Another cursor on the line below",
        |app| app.add_cursor_below();
    ADD_CURSOR_NEXT_MATCH => "add-cursor-next-match", Select, Passive, "Another cursor at the next copy of this word",
        |app| app.add_cursor_at_next_match();
    SELECT_ALL_MATCHES => "select-all-matches", Select, Passive, "A cursor at every copy of this word",
        |app| app.select_every_match();
    CURSORS_TO_LINE_ENDS => "cursors-to-line-ends", Select, Passive, "A cursor at the end of every selected line",
        |app| app.cursors_to_line_ends();
    COLLAPSE_CURSORS => "collapse-cursors", Select, Passive, "Back to one cursor",
        |app| app.collapse_cursors();
    INSERT_NEWLINE => "newline", Edit, Types, "Break the line, keeping the indentation",
        |app| app.insert_newline();
    DELETE_BACKWARD => "delete-backward", Edit, Types, "Rub out the character before",
        |app| app.delete_backward();
    DELETE_FORWARD => "delete-forward", Edit, Types, "Rub out the character after",
        |app| app.delete_forward();
    DELETE_WORD_BACKWARD => "delete-word-backward", Edit, Edits, "Rub out the word before",
        |app| app.delete_word_backward();
    DELETE_WORD_FORWARD => "delete-word-forward", Edit, Edits, "Rub out the word after",
        |app| app.delete_word_forward();
    DELETE_TO_LINE_START => "delete-to-line-start", Edit, Edits, "Rub out back to the start of the line",
        |app| app.delete_to_line_start();
    DELETE_TO_LINE_END => "delete-to-line-end", Edit, Edits, "Rub out to the end of the line",
        |app| app.delete_to_line_end();
    DELETE_LINE => "delete-line", Edit, Edits, "Take out the whole line",
        |app| app.delete_line();
    DUPLICATE_LINE => "duplicate-line", Edit, Edits, "Another copy of the line below it",
        |app| app.duplicate_line();
    MOVE_LINE_UP => "move-line-up", Edit, Edits, "Swap this line with the one above",
        |app| app.move_line_up();
    MOVE_LINE_DOWN => "move-line-down", Edit, Edits, "Swap this line with the one below",
        |app| app.move_line_down();
    JOIN_LINES => "join-lines", Edit, Edits, "Pull the next line onto this one",
        |app| app.join_lines();
    INDENT => "indent", Edit, Edits, "Push the line right one level",
        |app| app.on_tab(false);
    ACCEPT_HINT => "accept-hint", Edit, Edits, "Take the suggestion a plugin is offering",
        |app| app.accept_hint();
    UNINDENT => "unindent", Edit, Edits, "Pull the line left one level",
        |app| app.on_tab(true);
    TOGGLE_COMMENT => "toggle-comment", Edit, Edits, "Comment the selected lines out, or back in",
        |app| app.toggle_comment();
    UNDO => "undo", Edit, Edits, "Put back what you just changed",
        |app| app.undo(true);
    REDO => "redo", Edit, Edits, "Do it again after all",
        |app| app.undo(false);
    COPY => "copy", Edit, Passive, "Copy the selection, or the line if nothing is selected",
        |app| app.copy(false);
    CUT => "cut", Edit, Edits, "Cut the selection, or the line if nothing is selected",
        |app| app.copy(true);
    PASTE => "paste", Edit, Edits, "Put back what was copied",
        |app| app.paste();
    UPPER_CASE => "upper-case", Edit, Edits, "Make the selection shout",
        |app| app.upper_case();
    LOWER_CASE => "lower-case", Edit, Edits, "Make the selection quiet",
        |app| app.lower_case();
    FIND => "find", Search, Passive, "Search this file as you type",
        |app| app.open_prompt(PromptKind::Find);
    FIND_NEXT => "find-next", Search, Passive, "The next hit",
        |app| app.find_step(1);
    FIND_PREV => "find-prev", Search, Passive, "The one before",
        |app| app.find_step(-1);
    FIND_WORD_UNDER_CURSOR => "find-word", Search, Passive, "Search for the word the cursor is on",
        |app| app.find_word_under_cursor();
    REPLACE => "replace", Search, Edits, "Search and replace in this file",
        |app| app.open_prompt(PromptKind::ReplaceFind);
    NEXT_CHANGE => "next-change", Search, Passive, "To the next line that differs from the last commit",
        |app| app.change_step(true);
    PREV_CHANGE => "prev-change", Search, Passive, "To the change before",
        |app| app.change_step(false);
    GREP => "grep", Search, Passive, "Search every file in the project",
        |app| app.open_grep_picker();
    COMPLETION => "completion", Code, Passive, "Suggest what comes next",
        |app| app.ask_for_completions(None, true);
    GOTO_DEFINITION => "goto-definition", Code, Passive, "Where this is defined",
        |app| app.ask_goto(Goto::Definition);
    GOTO_TYPE_DEFINITION => "goto-type-definition", Code, Passive, "Where its type is defined",
        |app| app.ask_goto(Goto::Type);
    GOTO_IMPLEMENTATION => "goto-implementation", Code, Passive, "Where it is implemented",
        |app| app.ask_goto(Goto::Implementation);
    REFERENCES => "references", Code, Passive, "Everywhere this is used",
        |app| app.ask_references();
    HOVER => "hover", Code, Passive, "What the language server knows about this",
        |app| app.ask_hover(app.view().cursor());
    RENAME => "rename", Code, Edits, "Rename this everywhere it appears",
        |app| app.start_rename();
    CODE_ACTION => "code-action", Code, Edits, "What the language server offers to do about this",
        |app| app.ask_code_actions();
    FIX_IT => "fix-it", Code, Edits, "Do the obvious thing about the problem here: add the import, fix the typo",
        |app| app.fix_it();
    FIX_ALL => "fix-all", Code, Edits, "Apply every fix the servers would make to this file on their own",
        |app| app.fix_all(&[SOURCE_FIX_ALL.to_string()]);
    ORGANIZE_IMPORTS => "organize-imports", Code, Edits, "Put this file's imports in order and drop the unused ones",
        |app| app.fix_all(&[SOURCE_ORGANIZE_IMPORTS.to_string()]);
    FORMAT => "format", Code, Edits, "Reformat the file",
        |app| app.format();
    FORMAT_AND_FIX => "format-and-fix", Code, Edits, "Reformat the file and apply the servers' own fixes",
        |app| app.format_and_fix();
    SYMBOLS => "symbols", Code, Passive, "Pick from what this file defines",
        |app| app.ask_symbols();
    WORKSPACE_SYMBOLS => "workspace-symbols", Code, Passive, "Pick from what the project defines",
        |app| app.open_workspace_symbols();
    DIAGNOSTICS => "diagnostics", Code, Passive, "Pick from the problems found",
        |app| app.open_diagnostics_picker();
    NEXT_DIAGNOSTIC => "next-diagnostic", Code, Passive, "To the next problem",
        |app| app.step_diagnostic(1);
    PREV_DIAGNOSTIC => "prev-diagnostic", Code, Passive, "To the problem before",
        |app| app.step_diagnostic(-1);
    SIGNATURE_HELP => "signature-help", Code, Passive, "What arguments this call takes",
        |app| app.ask_signature();
    PYTHON_ENVIRONMENT => "python-environment", Code, Passive, "Choose which Python this project uses",
        |app| app.open_environment_picker();
    RESTART_SERVERS => "restart-servers", Code, Passive, "Start the language servers again",
        |app| app.restart_servers();
    SERVER_STATUS => "server-status", Code, Passive, "What the language servers are doing",
        |app| app.show_server_status();
    COMMAND_PALETTE => "command-palette", View, Passive, "Everything textfold can do, by name",
        |app| app.open_commands_picker();
    SPLIT => "split", View, Passive, "Another pane onto the same file",
        |app| app.split();
    CLOSE_PANE => "close-pane", View, Passive, "Close this pane",
        |app| app.close_pane();
    FOCUS_NEXT_PANE => "focus-next-pane", View, Passive, "Into the next pane",
        |app| app.focus_pane(1);
    FOCUS_PREV_PANE => "focus-prev-pane", View, Passive, "Into the pane before",
        |app| app.focus_pane(-1);
    SWAP_SPLIT_DIRECTION => "swap-split-direction", View, Passive, "Side by side, or one above the other",
        |app| app.swap_split_direction();
    DIFF_PANES => "diff-panes", View, Passive, "Compare the two panes, and scroll them together",
        |app| app.toggle_diff();
    THEME_PICKER => "theme", View, Passive, "Pick a set of colours",
        |app| app.open_theme_picker();
    NEXT_THEME => "next-theme", View, Passive, "The next set of colours along",
        |app| app.step_theme(1);
    PREV_THEME => "prev-theme", View, Passive, "The set before",
        |app| app.step_theme(-1);
    TOGGLE_LINE_NUMBERS => "toggle-line-numbers", View, Passive, "Line numbers on or off",
        |app| app.toggle_setting("line_numbers");
    TOGGLE_RELATIVE_NUMBERS => "toggle-relative-numbers", View, Passive, "Count from the cursor instead of the top",
        |app| app.toggle_setting("relative_numbers");
    TOGGLE_WRAP => "toggle-wrap", View, Passive, "Fold long lines, or let them run off the side",
        |app| app.toggle_wrap();
    TOGGLE_WHITESPACE => "toggle-whitespace", View, Passive, "Show spaces and tabs",
        |app| app.toggle_setting("show_whitespace");
    TOGGLE_MOUSE => "toggle-mouse", View, Passive, "Let the terminal have the mouse back",
        |app| app.toggle_setting("mouse");
    SET_LANGUAGE => "set-language", View, Passive, "Say what language this file is",
        |app| app.open_language_picker();
    SETTINGS => "settings", View, Passive, "Change a setting, and keep it",
        |app| app.open_settings_picker();
    RESTORE_SESSION => "restore-session", View, Passive, "Open again the files that were open here last time",
        |app| app.bring_back_session();
    PLUGINS => "plugins", View, Passive, "Turn languages and language servers on or off",
        |app| app.open_plugins_picker();
    INSTALL_PLUGIN => "install-plugin", View, Passive, "Install a plugin, or what one needs to work",
        |app| app.open_install_picker();
    UNINSTALL_PLUGIN => "uninstall-plugin", View, Passive, "Take a plugin off this machine",
        |app| app.open_uninstall_picker();
    UPDATE_PLUGINS => "update-plugins", View, Passive, "Fetch a newer version of any plugin that has one",
        |app| app.open_update_picker();
    CONTEXT_MENU => "context-menu", Edit, Passive, "What can be done where the cursor is",
        |app| app.open_context_menu();
    ESCAPE => "escape", Help, Passive, "Close what is open, or drop back to one cursor",
        |app| app.escape();
    HELP => "help", Help, Passive, "The keys, and what they do",
        |app| app.overlay = Overlay::Help(0);
    ABOUT => "about", Help, Passive, "Which textfold this is",
        |app| app.say(format!(
                        "textfold {} — {} languages, {} themes",
                        env!("CARGO_PKG_VERSION"),
                        lang::names().len(),
                        app.themes.entries.len()
                    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc;

    /// An editor with nothing of yours in it: the settings are the defaults
    /// rather than whatever is in your home directory, so a test does not pass
    /// or fail depending on whose machine it is on.
    fn editor() -> (App, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(Config::default(), tx);
        app.screen = Rect::new(0, 0, 100, 30);
        for pane in &mut app.panes {
            pane.area = Rect::new(6, 1, 90, 28);
            pane.frame = Rect::new(0, 1, 100, 28);
        }
        (app, rx)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("textfold-app-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).expect("a place to work");
        dir.join(name)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            if c == '\n' {
                app.run(Cmd::INSERT_NEWLINE);
            } else {
                app.type_char(c);
            }
        }
    }

    /// What a plugin asked for, answered the way the event loop answers it.
    fn plugin_asks(app: &mut App, method: &str, params: serde_json::Value) -> Result<Value, String> {
        match app.plugin_asked(HostId(0), method, &params, Some(&json!(1))) {
            Answer::Now(value) => Ok(value),
            Answer::No(why) => Err(why),
            Answer::Later => Ok(json!("later")),
        }
    }

    /// A keystroke through the whole loop, so that everything `handle` does
    /// afterwards — including noticing that a box has gone — happens too.
    fn pressed(app: &mut App, key: &str) {
        let key = Key::parse(key).expect("a key");
        app.handle(Event::Term(TermEvent::Key(KeyEvent::new(key.code, key.mods))));
    }

    #[test]
    fn a_list_a_plugin_put_up_answers_with_the_row_that_was_picked() {
        let (mut app, _rx) = editor();
        let asked = app.plugin_asked(
            HostId(0),
            "pick",
            &json!({ "title": "Which board?", "items": [
                { "label": "Nucleo F401RE", "value": "f401re" },
                { "label": "Discovery F407", "value": "f407" }
            ]}),
            Some(&json!(1)),
        );
        assert!(matches!(asked, Answer::Later), "the person has not answered yet");
        assert!(app.plugin_waiting.is_some());
        match &app.overlay {
            Overlay::Picker(picker) => assert_eq!(picker.title(), "Which board?"),
            _ => panic!("no list went up"),
        }

        pressed(&mut app, "down");
        pressed(&mut app, "enter");
        assert!(
            app.plugin_waiting.is_none(),
            "the plugin should have been answered"
        );
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn changing_your_mind_about_a_plugins_question_is_still_an_answer() {
        // The property that matters most about all of these: Escape is the
        // commonest thing anybody does to a box, and a plugin that got nothing
        // back would wait for ever.
        for (method, params) in [
            ("pick", json!({ "items": ["one", "two"] })),
            ("prompt", json!({ "title": "Which port?" })),
            ("confirm", json!({ "text": "Erase the chip?" })),
            ("menu", json!({ "items": ["Input", "Output"] })),
        ] {
            let (mut app, _rx) = editor();
            app.plugin_asked(HostId(0), method, &params, Some(&json!(1)));
            assert!(app.plugin_waiting.is_some(), "{method} did not ask");
            pressed(&mut app, "esc");
            assert!(
                app.plugin_waiting.is_none(),
                "{method} left the plugin waiting on a box that had gone"
            );
        }
    }

    #[test]
    fn a_plugins_second_question_does_not_leave_the_first_hanging() {
        // The second box replaces the first on the screen, so the first has to
        // be answered with nothing rather than quietly forgotten.
        let (mut app, _rx) = editor();
        app.plugin_asked(HostId(0), "prompt", &json!({}), Some(&json!(1)));
        let first = app.plugin_waiting.as_ref().map(|a| a.request.clone());
        assert_eq!(first, Some(json!(1)));

        app.plugin_asked(HostId(0), "confirm", &json!({ "text": "sure?" }), Some(&json!(2)));
        assert_eq!(
            app.plugin_waiting.as_ref().map(|a| a.request.clone()),
            Some(json!(2)),
            "the second question should be the one waiting now"
        );
    }

    #[test]
    fn a_plugins_menu_opens_where_the_cursor_is_and_answers_what_was_picked() {
        let (mut app, _rx) = editor();
        app.caret = Some((40, 12));

        let asked = app.plugin_asked(
            HostId(0),
            "menu",
            &json!({ "items": [
                { "label": "Go to it", "value": "go" },
                null,
                { "label": "Input",  "value": "in" },
                { "label": "Analog", "value": "analog", "enabled": false }
            ]}),
            Some(&json!(1)),
        );
        assert!(matches!(asked, Answer::Later));

        match &app.overlay {
            Overlay::Menu(menu) => {
                // Where the pointer is, not the middle of the screen. That is
                // the whole difference between this and `pick`.
                assert_eq!(menu.anchor, (40, 12));
                assert_eq!(menu.len(), 4, "the divider is a row too");
            }
            _ => panic!("no menu opened"),
        }

        pressed(&mut app, "enter");
        assert!(app.plugin_waiting.is_none(), "the plugin should have its answer");
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn a_menu_with_nothing_to_choose_is_not_put_up_at_all() {
        // Dividers are rows but not choices. A menu of nothing but lines
        // would be a box you cannot get out of except by escaping it.
        let (mut app, _rx) = editor();
        match app.plugin_asked(HostId(0), "menu", &json!({ "items": [null, null] }), Some(&json!(1))) {
            Answer::No(why) => assert!(why.contains("nothing")),
            _ => panic!("it should have been turned down"),
        }
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn a_question_told_rather_than_asked_is_turned_down() {
        // A notification has no id, so there is nowhere to send the answer.
        // Better to say so than to put a box on the screen that answers into
        // the void.
        let (mut app, _rx) = editor();
        match app.plugin_asked(HostId(0), "pick", &json!({ "items": ["a"] }), None) {
            Answer::No(why) => assert!(why.contains("asked")),
            _ => panic!("it should have been turned down"),
        }
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn a_panel_gets_the_keys_that_would_have_changed_the_text() {
        let (mut app, _rx) = editor();
        let key = |text: &str| Key::parse(text).expect("a key");

        // In an ordinary buffer, nothing is a plugin's.
        assert!(!app.panel_wants(key("r")));

        let id = app.view().doc;
        if let Some(doc) = app.doc_mut(id) {
            doc.read_only = true;
            doc.panel = Some(crate::doc::Panel {
                plugin: "cargo".into(),
                id: "cargo/report".into(),
                spans: Vec::new(),
                actions: Vec::new(),
            });
        }

        // A plain letter would have typed a character, and a panel is not
        // yours to type into — so it is the plugin's.
        assert!(app.panel_wants(key("r")));
        assert!(app.panel_wants(key("c")));
        // So is Enter, which would have made a newline.
        assert!(app.panel_wants(key("enter")));

        // But nothing anybody knows is taken. Every one of these still does
        // what it does everywhere else in the editor.
        for text in ["ctrl-p", "ctrl-w", "ctrl-q", "down", "ctrl-f", "alt-,", "f8"] {
            assert!(
                !app.panel_wants(key(text)),
                "{text} should still be the editor's"
            );
        }
    }

    /// An offer, as a plugin would have made it.
    fn suggesting(app: &mut App, text: &str) {
        let at = app.view().cursor();
        let id = app.view().doc;
        if let Some(doc) = app.doc_mut(id) {
            doc.hint = Some(crate::doc::Hint {
                plugin: "copilot".into(),
                at,
                text: text.into(),
            });
        }
    }

    #[test]
    fn taking_a_suggestion_puts_it_in_as_one_thing_to_undo() {
        let (mut app, _rx) = editor();
        typed(&mut app, "let x = ");
        suggesting(&mut app, "1 + 2;");

        assert!(app.hint_showing());
        pressed(&mut app, "tab");
        assert_eq!(app.here().text(), "let x = 1 + 2;");
        // The cursor ends where it would have if you had typed it.
        assert_eq!(app.view().cursor(), "let x = 1 + 2;".chars().count());
        // And it is your text now, in every way — including undoably.
        app.run(Cmd::UNDO);
        assert_eq!(app.here().text(), "let x = ");
    }

    #[test]
    fn tab_is_still_tab_when_nothing_is_being_offered() {
        // The key is not conditional, the offer is. An editor where Tab
        // stopped indenting because a plugin was installed would be an editor
        // nobody would install the plugin into.
        let (mut app, _rx) = editor();
        typed(&mut app, "x");
        assert!(!app.hint_showing());
        pressed(&mut app, "tab");
        assert!(
            app.here().text().starts_with('x') && app.here().text().len() > 1,
            "tab should have indented, got {:?}",
            app.here().text()
        );
    }

    #[test]
    fn an_offer_goes_when_you_walk_away_from_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "hello");
        suggesting(&mut app, " world");
        assert!(app.hint_showing());

        pressed(&mut app, "left");
        assert!(
            app.here().hint.is_none(),
            "moving off an offer is declining it"
        );
    }

    #[test]
    fn an_offer_goes_when_the_text_it_was_about_changes() {
        // It was worked out against the text as it was. The same rule an edit
        // computed against an old version gets, arrived at from the other side.
        let (mut app, _rx) = editor();
        typed(&mut app, "hello");
        suggesting(&mut app, " world");
        typed(&mut app, "!");
        assert!(app.here().hint.is_none());
    }

    #[test]
    fn escape_waves_an_offer_away_without_taking_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "hello");
        suggesting(&mut app, " world");
        pressed(&mut app, "esc");
        assert!(app.here().hint.is_none());
        assert_eq!(app.here().text(), "hello", "escape should not have taken it");
    }

    #[test]
    fn a_panels_colours_line_up_with_its_text() {
        let (text, spans, actions) = panel_lines(&[
            json!({ "spans": [
                { "text": "USART2", "style": "keyword" },
                { "text": "  TX ", "style": "muted" },
                { "text": "PA2", "style": "string", "action": "pin:PA2" }
            ]}),
            json!(""),
            json!("plain line"),
        ]);
        assert_eq!(text, "USART2  TX PA2\n\nplain line\n");

        // Every span points at exactly the words it was given.
        let at = |r: Range| text.chars().skip(r.start()).take(r.len()).collect::<String>();
        assert_eq!(at(spans[0].0), "USART2");
        assert_eq!(at(spans[1].0), "  TX ");
        assert_eq!(at(spans[2].0), "PA2");
        assert_eq!(actions.len(), 1, "only one span said it does anything");
        assert_eq!(at(actions[0].0), "PA2");
        assert_eq!(actions[0].1, "pin:PA2");
    }

    #[test]
    fn a_panel_is_counted_in_characters_and_not_in_bytes() {
        // A box-drawing character is three bytes and one column. Counting
        // bytes here would put every colour on a line after the first
        // non-ASCII character in the wrong place.
        let (text, spans, _) = panel_lines(&[json!({ "spans": [
            { "text": "▸ ", "style": "muted" },
            { "text": "ADC1", "style": "keyword" }
        ]})]);
        assert_eq!(text, "▸ ADC1\n");
        let second = spans[1].0;
        assert_eq!(
            text.chars().skip(second.start()).take(second.len()).collect::<String>(),
            "ADC1"
        );
    }

    #[test]
    fn a_style_a_plugin_asks_for_is_the_themes_own() {
        // Names rather than colours, so a panel is themed with everything
        // else. Tree-sitter's names, which the editor already knows...
        assert_eq!(panel_role("keyword"), Some(crate::theme::Role::Keyword));
        assert_eq!(panel_role("string"), Some(crate::theme::Role::String));
        // ...as specific as the theme actually goes...
        assert_eq!(
            panel_role("keyword.control"),
            Some(crate::theme::Role::KeywordControl)
        );
        // ...and falling back along the dots when it goes further, the way a
        // grammar's capture does.
        assert_eq!(panel_role("keyword.made.up"), Some(crate::theme::Role::Keyword));
        // ...plus the couple a plugin author reaches for that no grammar has.
        assert_eq!(panel_role("muted"), Some(crate::theme::Role::Comment));
        // A name nobody knows is drawn as ordinary text rather than refused:
        // a panel with one style misspelt should still be a readable panel.
        assert_eq!(panel_role("fuchsia"), None);
    }

    #[test]
    fn only_the_marked_parts_of_a_panel_do_anything() {
        let (mut app, _rx) = editor();
        let id = app.view().doc;
        if let Some(doc) = app.doc_mut(id) {
            doc.panel = Some(crate::doc::Panel {
                plugin: "cargo".into(),
                id: "cargo/report".into(),
                spans: Vec::new(),
                actions: vec![(Range::new(4, 9), "go:somewhere".into())],
            });
        }
        // Inside the marked stretch, and at its first character.
        assert!(app.panel_action_at(4));
        assert!(app.panel_action_at(8));
        // Just past the end, and before the start. Enter there should go on to
        // mean what Enter usually means rather than being quietly eaten.
        assert!(!app.panel_action_at(9));
        assert!(!app.panel_action_at(3));
    }

    #[test]
    fn a_plugin_asking_for_something_textfold_does_not_do_is_told_so() {
        // Not a silence. A plugin author who has misspelt a method, or reached
        // for one that does not exist yet, should hear it from the editor
        // rather than watch nothing happen.
        let (mut app, _rx) = editor();
        assert_eq!(
            plugin_asks(&mut app, "buffer/incinerate", json!({})),
            Err("textfold has no buffer/incinerate".into())
        );
    }

    #[test]
    fn a_plugin_can_read_a_buffer_and_change_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "one\ntwo\nthree");

        let read = plugin_asks(&mut app, "buffer/read", json!({})).expect("it should read");
        assert_eq!(read["text"], "one\ntwo\nthree");
        let version = read["version"].clone();

        // Line and column both counted from zero, in characters.
        let done = plugin_asks(
            &mut app,
            "buffer/edit",
            json!({ "version": version, "edits": [
                { "line": 1, "column": 0, "end_line": 1, "end_column": 3, "text": "TWO" }
            ]}),
        )
        .expect("it should apply");
        assert_eq!(done["applied"], 1);
        assert_eq!(app.here().text(), "one\nTWO\nthree");

        // And it went through the same door a keystroke does, so it is one
        // thing to undo — which is the whole reason for insisting on that.
        app.run(Cmd::UNDO);
        assert_eq!(app.here().text(), "one\ntwo\nthree");
    }

    #[test]
    fn an_edit_worked_out_against_older_text_is_refused_rather_than_applied() {
        let (mut app, _rx) = editor();
        typed(&mut app, "hello");
        let stale = app.here().version;
        typed(&mut app, " there");
        assert_ne!(app.here().version, stale, "typing should move the version on");

        // A plugin holding an edit for text that is no longer there would
        // corrupt the file rather than fix it, so it is turned down and told
        // why — not applied, and not silently dropped either.
        let refused = plugin_asks(
            &mut app,
            "buffer/edit",
            json!({ "version": stale, "edits": [
                { "line": 0, "column": 0, "end_line": 0, "end_column": 5, "text": "goodbye" }
            ]}),
        );
        assert!(
            refused.is_err_and(|why| why.contains(&stale.to_string())),
            "a stale edit should say which version it was for"
        );
        assert_eq!(app.here().text(), "hello there");
    }

    #[test]
    fn a_plugin_that_says_nothing_is_not_given_the_status_line() {
        let (mut app, _rx) = editor();
        assert!(plugin_asks(&mut app, "status/say", json!({ "text": "  " })).is_err());
        assert!(plugin_asks(&mut app, "status/say", json!({ "text": "building" })).is_ok());
    }

    #[test]
    fn problems_from_a_plugin_that_is_not_running_go_nowhere() {
        // The id names no host, which is what a message arriving after one has
        // died looks like.
        let (mut app, _rx) = editor();
        assert_eq!(
            plugin_asks(&mut app, "diagnostics/set", json!({ "items": [] })),
            Err("that plugin is not running".into())
        );
    }

    /// The keystroke another program sends to say "open this", as bytes on the
    /// way in rather than as a call to `open_path`.
    fn keyed(app: &mut App, key: &str) {
        let key = Key::parse(key).expect("a key");
        app.on_key(KeyEvent::new(key.code, key.mods));
    }

    /// A completion list as a server would have sent it, for a file with
    /// `at` characters of a word typed so far.
    fn suggested(app: &mut App, at: usize, incomplete: bool, items: Value) {
        app.suggest_for_test(at, incomplete, items);
    }

    fn offered(title: &str, kind: &str) -> Value {
        serde_json::json!({ "title": title, "kind": kind })
    }

    #[test]
    fn what_two_servers_offer_ends_up_in_one_list() {
        // Which is the whole of the Python case: `ruff` knows how to take the
        // unused import out, `pyright` knows where the missing one lives, and
        // asking only whichever answers first gets you one of those.
        let (linter, checker) = (ServerId(0), ServerId(1));
        let mut gathered = Gathered::new(DocId(1), 12, vec![linter, checker]);
        assert!(!gathered.settled(), "nobody has answered yet");

        gathered.take(linter, serde_json::json!([offered("Remove unused import", "quickfix")]));
        assert!(!gathered.settled(), "the other one is still thinking");
        assert_eq!(gathered.len(), 1);

        gathered.take(checker, serde_json::json!([offered("Add import os", "quickfix")]));
        assert!(gathered.settled());
        let titles: Vec<&str> = gathered
            .actions()
            .iter()
            .filter_map(|(_, a)| a.get("title").and_then(Value::as_str))
            .collect();
        assert_eq!(titles, ["Remove unused import", "Add import os"]);
        // And each row still knows who to send the choice back to.
        assert_eq!(gathered.actions()[0].0, linter);
        assert_eq!(gathered.actions()[1].0, checker);
    }

    #[test]
    fn a_server_answering_twice_replaces_its_own_and_leaves_the_rest() {
        let (linter, checker) = (ServerId(0), ServerId(1));
        let mut gathered = Gathered::new(DocId(1), 0, vec![linter, checker]);
        gathered.take(linter, serde_json::json!([offered("First go", "quickfix")]));
        gathered.take(checker, serde_json::json!([offered("From the checker", "quickfix")]));
        gathered.take(linter, serde_json::json!([offered("Second go", "quickfix")]));
        let titles: Vec<&str> = gathered
            .actions()
            .iter()
            .filter_map(|(_, a)| a.get("title").and_then(Value::as_str))
            .collect();
        assert_eq!(titles, ["Second go", "From the checker"]);
    }

    #[test]
    fn a_server_with_nothing_to_say_does_not_hold_the_list_up() {
        let (quiet, useful) = (ServerId(0), ServerId(1));
        let mut gathered = Gathered::new(DocId(1), 0, vec![quiet, useful]);
        gathered.take(quiet, Value::Null);
        assert!(gathered.is_empty());
        assert!(!gathered.settled());
        gathered.take(useful, serde_json::json!([offered("Fix it", "quickfix")]));
        assert!(gathered.settled());
        assert_eq!(gathered.len(), 1);
        assert_eq!(gathered.headline(), Some("Fix it"));
    }

    #[test]
    fn every_row_says_which_server_offered_it_when_two_did() {
        let both = vec![
            (ServerId(0), offered("From the linter", "quickfix")),
            (ServerId(1), offered("From the checker", "quickfix")),
        ];
        let rows = action_rows(&both);
        assert!(rows.iter().all(|r| r.detail.is_some()), "{rows:?}");
        // One server offering two things needs no such note: there is nothing
        // to tell apart.
        let one = vec![
            (ServerId(0), offered("A", "quickfix")),
            (ServerId(0), offered("B", "quickfix")),
        ];
        let rows = action_rows(&one);
        assert!(rows.iter().all(|r| r.detail.is_none()));
        assert_eq!(rows[0].tag.as_deref(), Some("quickfix"));
    }

    #[test]
    fn what_is_open_and_where_you_are_in_it_is_what_gets_written_down() {
        let (mut app, _rx) = editor();
        let one = scratch("session-one.rs");
        let two = scratch("session-two.rs");
        std::fs::write(&one, "fn a() {}\nfn b() {}\n").unwrap();
        std::fs::write(&two, "// notes\n").unwrap();
        app.open_path(&one);
        app.open_path(&two);
        // Back to the first, and down a line in it.
        let first = app.docs[0].id;
        app.show(first);
        app.go_to(1, 3);

        let session = app.session();
        let paths: Vec<&std::path::Path> =
            session.tabs.iter().map(|t| t.path.as_path()).collect();
        assert_eq!(paths, [one.as_path(), two.as_path()], "tab order");
        assert_eq!((session.tabs[0].line, session.tabs[0].column), (1, 3));
        // One pane, showing the file it is showing.
        assert_eq!(session.panes.len(), 1);
        assert_eq!(session.panes[0].tab, 0);
        std::fs::remove_file(&one).ok();
        std::fs::remove_file(&two).ok();
    }

    #[test]
    fn a_session_opens_the_tabs_again_where_they_were() {
        let one = scratch("restore-one.rs");
        let two = scratch("restore-two.rs");
        std::fs::write(&one, "a\nb\nc\nd\n").unwrap();
        std::fs::write(&two, "x\ny\n").unwrap();

        let (mut app, _rx) = editor();
        let session = crate::session::Session {
            tabs: vec![
                crate::session::Tab {
                    path: one.clone(),
                    line: 2,
                    column: 0,
                },
                crate::session::Tab {
                    path: two.clone(),
                    line: 1,
                    column: 1,
                },
            ],
            panes: Vec::new(),
            focus: 0,
            side_by_side: true,
            at: 0,
            docks: Vec::new(),
        };
        assert_eq!(app.apply_session(&session, false), 2);

        let names: Vec<&str> = app.docs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["restore-one.rs", "restore-two.rs"]);
        // The last one opened is the one you are looking at, and it is where
        // it was left.
        assert_eq!(app.here().name, "restore-two.rs");
        assert_eq!(app.here().point_at_char(app.view().cursor()), (1, 1));
        // And the one behind it kept its own place, which is the whole point
        // of writing a line down per tab rather than one for the session.
        let first = app.docs[0].id;
        app.show(first);
        assert_eq!(app.here().point_at_char(app.view().cursor()), (2, 0));

        std::fs::remove_file(&one).ok();
        std::fs::remove_file(&two).ok();
    }

    #[test]
    fn a_file_that_has_gone_since_is_skipped_rather_than_made_again() {
        let here = scratch("restore-here.rs");
        std::fs::write(&here, "still here\n").unwrap();
        let gone = scratch("restore-gone.rs");
        std::fs::remove_file(&gone).ok();

        let (mut app, _rx) = editor();
        let session = crate::session::Session {
            tabs: vec![
                crate::session::Tab {
                    path: gone.clone(),
                    line: 0,
                    column: 0,
                },
                crate::session::Tab {
                    path: here.clone(),
                    line: 0,
                    column: 0,
                },
            ],
            ..crate::session::Session::default()
        };
        assert_eq!(app.apply_session(&session, false), 1);
        let names: Vec<&str> = app.docs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["restore-here.rs"]);
        std::fs::remove_file(&here).ok();
    }

    #[test]
    fn the_panes_come_back_as_they_were() {
        let one = scratch("panes-one.rs");
        let two = scratch("panes-two.rs");
        std::fs::write(&one, "a\n").unwrap();
        std::fs::write(&two, "b\n").unwrap();

        let (mut app, _rx) = editor();
        let session = crate::session::Session {
            tabs: vec![
                crate::session::Tab {
                    path: one.clone(),
                    line: 0,
                    column: 0,
                },
                crate::session::Tab {
                    path: two.clone(),
                    line: 0,
                    column: 0,
                },
            ],
            panes: vec![
                crate::session::Pane { tab: 1, wrap: false },
                crate::session::Pane { tab: 0, wrap: true },
            ],
            focus: 1,
            side_by_side: false,
            at: 0,
            docks: Vec::new(),
        };
        app.apply_session(&session, false);
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.doc(app.panes[0].doc).map(|d| d.name.clone()).unwrap(), "panes-two.rs");
        assert_eq!(app.doc(app.panes[1].doc).map(|d| d.name.clone()).unwrap(), "panes-one.rs");
        assert!(app.panes[1].wrap, "the pane's own folding came back");
        assert_eq!(app.focus, 1);
        assert!(!app.side_by_side);
        std::fs::remove_file(&one).ok();
        std::fs::remove_file(&two).ok();
    }

    #[test]
    fn a_name_the_file_has_not_imported_shows_where_it_comes_from() {
        // What rust-analyzer sends for a name you have not imported: the
        // module in the label details rather than in the label, so that what
        // you typed still matches what you are being offered.
        let (mut app, _rx) = editor();
        typed(&mut app, "HashMa");
        suggested(
            &mut app,
            6,
            true,
            json!([{
                "label": "HashMap",
                "labelDetails": {
                    "detail": "(use std::collections::HashMap)",
                    "description": "HashMap<K, V>",
                },
            }]),
        );

        let item = app.completion.as_ref().expect("a list").selected().unwrap();
        assert_eq!(item.label, "HashMap");
        assert_eq!(item.suffix.as_deref(), Some("(use std::collections::HashMap)"));
        assert_eq!(item.detail.as_deref(), Some("HashMap<K, V>"));
    }

    #[test]
    fn a_partial_list_is_asked_for_again_rather_than_narrowed_to_nothing() {
        // A server asked about two characters offers some of what it could
        // reach and says there is more. Narrowing that is how a name you are
        // typing towards disappears before you have finished typing it.
        let (mut app, _rx) = editor();
        typed(&mut app, "Ha");
        suggested(&mut app, 2, true, json!([{ "label": "Handle" }]));
        assert_eq!(app.completion.as_ref().map(Completion::len), Some(1));

        typed(&mut app, "s");
        assert!(app.completion.is_none(), "nothing left matching `Has`");
        assert!(
            app.completion_due.is_some(),
            "the server has more to say and has not been asked"
        );
    }

    #[test]
    fn clicking_away_closes_the_list_of_suggestions() {
        let (mut app, _rx) = editor();
        typed(&mut app, "Ha");
        suggested(
            &mut app,
            2,
            false,
            json!([{ "label": "Handle" }, { "label": "Hasty" }]),
        );
        assert!(app.completion.is_some(), "nothing was suggested to close");

        // Somewhere in the text, well away from where the list was drawn.
        app.click(20, 12, KeyModifiers::NONE);
        assert!(
            app.completion.is_none(),
            "the list is still there over a word nobody is typing any more"
        );
    }

    #[test]
    fn clicking_a_suggestion_still_takes_it() {
        // The other half: closing on a click away must not close it on the
        // click that was choosing something from it.
        let (mut app, _rx) = editor();
        typed(&mut app, "Ha");
        suggested(
            &mut app,
            2,
            false,
            json!([{ "label": "Handle" }, { "label": "Hasty" }]),
        );
        // Where the drawing would have put it.
        let list = app.completion.as_mut().expect("a list");
        list.area = Rect::new(4, 2, 24, 2);

        app.click(6, 3, KeyModifiers::NONE);
        assert!(app.completion.is_none(), "the list stayed open");
        assert_eq!(app.here().rope.to_string().trim_end(), "Hasty");
    }

    #[test]
    fn right_clicking_closes_the_list_of_suggestions() {
        let (mut app, _rx) = editor();
        typed(&mut app, "Ha");
        suggested(&mut app, 2, false, json!([{ "label": "Handle" }]));
        app.right_click(1, 1);
        assert!(app.completion.is_none());
        assert!(matches!(app.overlay, Overlay::Menu(_)), "no menu opened");
    }

    #[test]
    fn a_complete_list_narrows_where_it_stands() {
        // The other half of it: a server that said it gave a full answer is
        // taken at its word, and typing does not go back to it.
        let (mut app, _rx) = editor();
        typed(&mut app, "Ha");
        suggested(
            &mut app,
            2,
            false,
            json!([{ "label": "Handle" }, { "label": "Hasty" }]),
        );
        typed(&mut app, "s");

        assert_eq!(app.completion.as_ref().map(Completion::len), Some(1));
        assert!(app.completion_due.is_none());
    }

    #[test]
    fn backspace_narrows_the_list_rather_than_closing_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "Has");
        suggested(
            &mut app,
            3,
            false,
            json!([{ "label": "Handle" }, { "label": "Hasty" }]),
        );
        assert_eq!(app.completion.as_ref().map(Completion::len), Some(1));

        app.run(Cmd::DELETE_BACKWARD);
        assert_eq!(
            app.completion.as_ref().map(Completion::len),
            Some(2),
            "backspacing to `Ha` matches both again"
        );
    }

    #[test]
    fn the_import_arrives_with_the_name_even_when_it_is_worked_out_late() {
        // Servers send the name first and the import it needs only when asked
        // about that one suggestion. Taking it before the answer is back has
        // to wait for the answer, not go without it.
        let (mut app, _rx) = editor();
        app.here_mut().language = crate::lang::LangId::PLAIN;
        typed(&mut app, "HashMa");
        suggested(&mut app, 6, false, json!([{ "label": "HashMap" }]));

        // As though a server had been asked and had not answered yet.
        let index = app.completion.as_ref().unwrap().shown[0];
        app.suggestion_mut(index).unwrap().resolve = Resolve::Waiting;
        app.accept_completion();

        assert_eq!(app.here().rope.to_string(), "HashMa", "nothing put in yet");
        assert_eq!(app.accept_when_resolved, Some(index));

        let doc = app.here().id;
        app.take_resolved_completion(
            doc,
            index,
            json!({
                "label": "HashMap",
                "additionalTextEdits": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 0 },
                    },
                    "newText": "use std::collections::HashMap;\n",
                }],
            }),
        );

        assert_eq!(
            app.here().rope.to_string(),
            "use std::collections::HashMap;\nHashMap",
        );
        // And the cursor is after the name, not still up where the import
        // pushed the line it was on out of the way.
        assert_eq!(app.view().cursor(), app.here().len_chars());
        assert!(app.completion.is_none());
        assert_eq!(app.accept_when_resolved, None);
    }

    #[test]
    fn an_import_that_never_comes_does_not_eat_the_keystroke() {
        // A server that fails the question has still been answered: the name
        // goes in without the import rather than nothing going in at all.
        let (mut app, _rx) = editor();
        typed(&mut app, "HashMa");
        suggested(&mut app, 6, false, json!([{ "label": "HashMap" }]));
        let index = app.completion.as_ref().unwrap().shown[0];
        app.suggestion_mut(index).unwrap().resolve = Resolve::Waiting;
        app.accept_completion();

        app.on_response(
            crate::lsp::ServerId(0),
            0,
            Err("content modified".to_string()),
        );
        // Nothing claimed that request, so the editor is still waiting on it;
        // the resolve coming back empty is what actually unsticks it.
        let doc = app.here().id;
        app.take_resolved_completion(doc, index, json!({ "label": "HashMap" }));

        assert_eq!(app.here().rope.to_string(), "HashMap");
    }

    #[test]
    fn a_path_can_be_opened_by_typing_it_rather_than_finding_it() {
        let (mut app, _rx) = editor();
        let path = scratch("typed-path.txt");
        std::fs::write(&path, "already here\n").expect("written");

        keyed(&mut app, "alt-e");
        typed_into_prompt(&mut app, &path.display().to_string());
        app.accept_prompt();

        assert_eq!(app.here().path.as_deref(), Some(path.as_path()));
        assert_eq!(app.here().rope.to_string(), "already here\n");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn the_key_that_opens_a_path_works_with_something_else_already_open() {
        // The program sending it cannot see the screen, so a list, a prompt or
        // a question in the way has to give up the key rather than eat it.
        let (mut app, _rx) = editor();
        for opened in [Cmd::OPEN, Cmd::FIND, Cmd::COMMAND_PALETTE] {
            app.run(opened);
            keyed(&mut app, "alt-e");
            assert!(
                matches!(&app.overlay, Overlay::Prompt(p) if p.kind == PromptKind::OpenPath),
                "{opened:?} swallowed it"
            );
            app.overlay = Overlay::None;
        }
    }

    #[test]
    fn a_key_bound_to_opening_a_path_still_types_where_typing_is_meant() {
        // Bound to a plain letter, it is a letter first: a global key that
        // stole `e` from every search box would be worse than no global key.
        let mut config = Config::default();
        config.keys.insert("open-path".into(), vec!["e".into()]);
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(config, tx);
        app.screen = Rect::new(0, 0, 100, 30);
        app.run(Cmd::FIND);
        keyed(&mut app, "e");
        match &app.overlay {
            Overlay::Prompt(prompt) => assert_eq!(prompt.input, "e"),
            _ => panic!("the search box closed"),
        }
    }

    fn typed_into_prompt(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    #[test]
    fn opening_the_file_list_goes_and_looks_again() {
        // A file written since textfold started is a file you can open. The
        // list from last time is shown at once, and a fresh walk is under way
        // behind it.
        let (mut app, rx) = editor();
        let dir = scratch("walk").parent().unwrap().to_path_buf();
        app.project = dir.clone();
        // What a walk found before the file existed, which is the state
        // textfold is in for the rest of an afternoon.
        app.files = Some(vec![dir.join("old.txt")]);
        std::fs::write(dir.join("new.txt"), "made just now\n").expect("written");

        app.run(Cmd::OPEN);
        assert!(matches!(&app.overlay, Overlay::Picker(p) if p.kind == Kind::Files));
        let shown = match &app.overlay {
            Overlay::Picker(picker) => picker.len(),
            _ => 0,
        };
        assert_eq!(shown, 1, "the list from last time shows first");

        // The walking thread reports what is there now, and the box follows.
        let found = loop {
            match rx.recv().expect("the walk answers") {
                Event::Files(found) => break found,
                _ => continue,
            }
        };
        assert!(
            found.iter().any(|p| p.ends_with("new.txt")),
            "the fresh walk missed a file written after startup: {found:?}"
        );
        app.handle(Event::Files(found));
        assert!(matches!(&app.overlay, Overlay::Picker(p)
            if p.visible().any(|(row, _)| row.label == "new.txt")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_docstring_is_coloured_the_way_the_code_in_it_would_be() {
        lang::init();
        let rust = lang::by_tag("rust").expect("shipped");
        let hover = serde_json::json!({
            "kind": "markdown",
            "value": "Adds two numbers.\n\n```rust\nfn add(a: u32) -> u32\n```\n",
        });
        let lines = markup_lines(Some(&hover), rust);

        let prose = lines.iter().find(|l| l.text == "Adds two numbers.");
        assert!(prose.is_some_and(|l| l.spans.is_empty()), "{lines:?}");

        let code = lines
            .iter()
            .find(|l| l.text == "fn add(a: u32) -> u32")
            .expect("the example survived the fence");
        let coloured = |want: &str| {
            code.spans
                .iter()
                .find(|(range, _)| &code.text[range.clone()] == want)
                .map(|(_, role)| *role)
        };
        assert_eq!(coloured("fn"), Some(Role::Keyword));
        assert_eq!(coloured("add"), Some(Role::Function));
        assert_eq!(coloured("u32"), Some(Role::TypeBuiltin));
    }

    #[test]
    fn a_fence_that_says_nothing_is_the_language_you_are_looking_at() {
        lang::init();
        let rust = lang::by_tag("rust").expect("shipped");
        let hover = serde_json::json!({ "value": "```\nlet x = 1;\n```" });
        let lines = markup_lines(Some(&hover), rust);
        let code = lines.iter().find(|l| l.text == "let x = 1;").expect("kept");
        assert!(
            code.spans.iter().any(|(_, role)| *role == Role::Keyword),
            "{code:?}"
        );

        // And a fence naming a language nothing here can parse is left plain
        // rather than coloured as whatever file you happened to be in.
        let hover = serde_json::json!({ "value": "```brainfuck\nlet x = 1;\n```" });
        let lines = markup_lines(Some(&hover), rust);
        let code = lines.iter().find(|l| l.text == "let x = 1;").expect("kept");
        assert!(code.spans.is_empty(), "{code:?}");
    }

    #[test]
    fn a_block_the_server_calls_code_is_not_read_as_markdown() {
        lang::init();
        // The old `MarkedString` form, naming a language nothing here parses.
        // It is still code: its hashes are not headings and its dashes are not
        // a rule.
        let hover = serde_json::json!({
            "language": "cmake",
            "value": "#include <stdio.h>\n---\n",
        });
        let lines = markup_lines(Some(&hover), LangId::PLAIN);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["#include <stdio.h>", "---"]);
    }

    #[test]
    fn colouring_a_block_does_not_colour_past_the_end_of_a_line() {
        lang::init();
        let python = lang::by_tag("py").expect("shipped");
        // A string over two lines: one span in the tree, two lines on screen,
        // and neither of them may reach outside itself.
        let hover = serde_json::json!({
            "value": "```python\nx = \"\"\"one\ntwo\"\"\"\n```",
        });
        let lines = markup_lines(Some(&hover), python);
        for line in &lines {
            for (range, _) in &line.spans {
                assert!(
                    range.end <= line.text.len() && range.start < range.end,
                    "{line:?}"
                );
            }
        }
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|(_, role)| *role == Role::String)),
            "{lines:?}"
        );
    }

    #[test]
    fn coming_back_to_a_tab_comes_back_to_where_you_were_in_it() {
        let (mut app, _rx) = editor();
        let one = scratch("place-one.txt");
        let two = scratch("place-two.txt");
        std::fs::write(&one, "line\n".repeat(400)).expect("written");
        std::fs::write(&two, "other\n".repeat(400)).expect("written");

        app.open_path(&one);
        app.go_to_line(300);
        let (top, at) = (app.view().top, app.view().cursor());
        assert!(top > 0, "line 300 of 400 is not at the top of the screen");

        app.open_path(&two);
        assert_eq!(
            app.view().top,
            0,
            "a file never seen before opens at the top"
        );

        app.open_path(&one);
        assert_eq!(app.view().top, top, "the view came back somewhere else");
        assert_eq!(
            app.view().cursor(),
            at,
            "the cursor came back somewhere else"
        );
        std::fs::remove_dir_all(one.parent().unwrap()).ok();
    }

    #[test]
    fn a_file_written_by_something_else_is_noticed_and_read_again() {
        let (mut app, _rx) = editor();
        let path = scratch("changed-underneath.txt");
        std::fs::write(&path, "before\n").expect("written");
        app.open_path(&path);
        assert_eq!(app.here().on_disk, OnDisk::Same);

        // A formatter, a `git checkout`, the same file open next door.
        std::fs::write(&path, "after\n").expect("written");
        // Twice: one look cannot tell a file that has just changed from one
        // that is halfway through being written, so nothing is read until it
        // has looked the same twice. The second look comes a quarter of a
        // second later rather than a whole cycle — see `SETTLE_CHECK_EVERY`.
        app.check_disk();
        assert!(app.unsettled, "the first sighting is not enough");
        assert_eq!(app.here().rope.to_string(), "before\n");
        app.check_disk();

        assert!(!app.unsettled);
        assert_eq!(app.here().rope.to_string(), "after\n");
        assert!(
            !app.here().is_modified(),
            "reading a file is not editing it"
        );
        assert_eq!(app.here().on_disk, OnDisk::Same);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_file_with_unsaved_changes_is_marked_rather_than_overwritten() {
        let (mut app, _rx) = editor();
        let path = scratch("clash.txt");
        std::fs::write(&path, "before\n").expect("written");
        app.open_path(&path);
        typed(&mut app, "mine ");

        std::fs::write(&path, "theirs\n").expect("written");
        app.check_disk();

        assert!(
            app.here().rope.to_string().starts_with("mine "),
            "unsaved work was thrown away"
        );
        assert_eq!(app.here().on_disk, OnDisk::Changed);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_file_that_has_gone_is_not_read_as_an_empty_one() {
        let (mut app, _rx) = editor();
        let path = scratch("vanishing.txt");
        std::fs::write(&path, "here for now\n").expect("written");
        app.open_path(&path);
        std::fs::remove_file(&path).expect("removed");
        app.check_disk();

        assert_eq!(app.here().on_disk, OnDisk::Gone);
        assert_eq!(app.here().rope.to_string(), "here for now\n");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn re_reading_a_file_can_be_undone() {
        let (mut app, _rx) = editor();
        let path = scratch("undo-reload.txt");
        std::fs::write(&path, "one\n").expect("written");
        app.open_path(&path);
        std::fs::write(&path, "two\n").expect("written");
        app.do_reload(app.view().doc);
        assert_eq!(app.here().rope.to_string(), "two\n");

        app.run(Cmd::UNDO);
        assert_eq!(app.here().rope.to_string(), "one\n");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_line_you_have_changed_since_the_commit_is_marked_and_can_be_jumped_to() {
        use std::process::{Command, Stdio};
        let dir = std::env::temp_dir().join(format!("textfold-appgit-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("a place to work");
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()
                .filter(|s| s.success())
        };
        // No git on this machine is not a failing test.
        let Some(_) = run(&["init", "-q"])
            .and_then(|_| run(&["config", "user.email", "nobody@example.invalid"]))
            .and_then(|_| run(&["config", "user.name", "Nobody"]))
        else {
            std::fs::remove_dir_all(&dir).ok();
            return;
        };
        let path = dir.join("tracked.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").expect("written");
        if run(&["add", "tracked.txt"])
            .and_then(|_| run(&["commit", "-qm", "first"]))
            .is_none()
        {
            std::fs::remove_dir_all(&dir).ok();
            return;
        }

        let (mut app, _rx) = editor();
        app.project = dir.clone();
        app.git.open(&dir);
        app.open_path(&path);
        app.refresh_git();
        assert_eq!(
            app.git.changed_lines(app.view().doc),
            0,
            "nothing changed yet"
        );

        app.go_to_line(1);
        typed(&mut app, "changed ");
        app.refresh_git();

        let id = app.view().doc;
        assert_eq!(app.git.mark(id, 1), Some(crate::git::Mark::Changed));
        assert_eq!(app.git.mark(id, 0), None);

        app.run(Cmd::MOVE_DOC_START);
        app.run(Cmd::NEXT_CHANGE);
        assert_eq!(
            text::line_of(&app.here().rope, app.view().cursor()),
            1,
            "the jump did not land on the changed line"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_search_box_opens_empty_but_the_last_search_is_still_there() {
        let (mut app, _rx) = editor();
        typed(&mut app, "alpha beta alpha");
        app.last_search = "alpha".into();
        app.run(Cmd::FIND);
        match &app.overlay {
            Overlay::Prompt(p) => assert_eq!(p.input, "", "the box kept the last search"),
            other => panic!("no search box: {:?}", matches!(other, Overlay::None)),
        }
        // Which is not the same as forgetting it.
        assert_eq!(app.last_search, "alpha");
    }

    #[test]
    fn enter_in_the_search_box_walks_the_matches_without_closing_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "alpha beta alpha gamma alpha");
        app.run(Cmd::MOVE_DOC_START);
        app.run(Cmd::FIND);
        typed_into_prompt(&mut app, "alpha");
        let first = app.view().sel.primary().start();

        keyed(&mut app, "enter");
        assert!(
            matches!(&app.overlay, Overlay::Prompt(p) if p.kind == PromptKind::Find),
            "Enter closed the box"
        );
        let second = app.view().sel.primary().start();
        assert!(
            second > first,
            "Enter did not move on: {first} then {second}"
        );

        keyed(&mut app, "enter");
        let third = app.view().sel.primary().start();
        assert!(third > second);

        // And back the way it came.
        keyed(&mut app, "shift-enter");
        assert_eq!(app.view().sel.primary().start(), second);
    }

    #[test]
    fn leaving_the_search_box_keeps_where_enter_took_you() {
        let (mut app, _rx) = editor();
        typed(&mut app, "alpha beta alpha");
        app.run(Cmd::MOVE_DOC_START);
        app.run(Cmd::FIND);
        typed_into_prompt(&mut app, "alpha");
        keyed(&mut app, "enter");
        let landed = app.view().sel.primary().start();
        keyed(&mut app, "esc");
        assert_eq!(app.view().sel.primary().start(), landed);
    }

    #[test]
    fn changing_your_mind_about_a_search_puts_the_cursor_back() {
        let (mut app, _rx) = editor();
        typed(&mut app, "alpha beta alpha");
        app.run(Cmd::MOVE_DOC_START);
        let was = app.view().sel.primary().start();
        app.run(Cmd::FIND);
        typed_into_prompt(&mut app, "beta");
        assert_ne!(
            app.view().sel.primary().start(),
            was,
            "typing did not search"
        );
        keyed(&mut app, "esc");
        assert_eq!(app.view().sel.primary().start(), was);
    }

    #[test]
    fn closing_the_other_tabs_keeps_the_one_you_are_in() {
        let (mut app, _rx) = editor();
        for name in ["a.txt", "b.txt", "c.txt"] {
            let path = scratch(name);
            std::fs::write(&path, "text\n").expect("written");
            app.open_path(&path);
        }
        let here = app.view().doc;
        assert_eq!(app.docs().len(), 3);
        app.run(Cmd::CLOSE_OTHERS);
        assert_eq!(app.docs().len(), 1);
        assert_eq!(app.view().doc, here);
        std::fs::remove_dir_all(scratch("a.txt").parent().unwrap()).ok();
    }

    #[test]
    fn closing_everything_leaves_unsaved_work_open() {
        let (mut app, _rx) = editor();
        let saved = scratch("saved.txt");
        std::fs::write(&saved, "on disk\n").expect("written");
        app.open_path(&saved);
        app.run(Cmd::NEW);
        typed(&mut app, "not saved anywhere");

        app.run(Cmd::CLOSE_ALL);
        let left: Vec<String> = app.docs().iter().map(|d| d.name.clone()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(app.here().is_modified());
        std::fs::remove_dir_all(saved.parent().unwrap()).ok();
    }

    #[test]
    fn pointing_at_a_problem_says_what_is_wrong_with_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "let x = 1");
        app.here_mut().diagnostics = vec![crate::doc::Diagnostic {
            range: Range::new(4, 5),
            severity: crate::doc::Severity::Error,
            message: "cannot find value `x` in this scope".into(),
            source: Some("rustc".into()),
            code: Some("E0425".into()),
            data: None,
            told: crate::doc::Told::Server(0),
        }];
        let said: Vec<String> = app
            .problem_lines(4)
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert_eq!(
            said,
            vec![
                "error (rustc E0425)".to_string(),
                "cannot find value `x` in this scope".to_string(),
            ]
        );
        assert!(
            app.problem_lines(8).is_empty(),
            "somewhere with nothing wrong with it should say nothing"
        );
    }

    #[test]
    fn the_worst_problem_at_a_spot_is_read_first() {
        let (mut app, _rx) = editor();
        typed(&mut app, "let x = 1");
        let at = |severity, message: &str| crate::doc::Diagnostic {
            range: Range::new(4, 5),
            severity,
            message: message.into(),
            source: None,
            code: None,
            data: None,
            told: crate::doc::Told::Server(0),
        };
        app.here_mut().diagnostics = vec![
            at(crate::doc::Severity::Hint, "unused"),
            at(crate::doc::Severity::Error, "undefined"),
        ];
        let said: Vec<String> = app.problem_lines(4).into_iter().map(|l| l.text).collect();
        assert_eq!(said.first().map(String::as_str), Some("error"));
        assert!(said.contains(&"undefined".to_string()));
        assert!(said.contains(&"unused".to_string()));
        assert!(
            said.iter().position(|l| l == "undefined")
                < said.iter().position(|l| l == "unused"),
            "the hint came before the error: {said:?}"
        );
    }

    #[test]
    fn asking_about_a_problem_with_no_server_still_shows_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "let x = 1");
        app.here_mut().diagnostics = vec![crate::doc::Diagnostic {
            range: Range::new(4, 5),
            severity: crate::doc::Severity::Warning,
            message: "x is never read".into(),
            source: Some("clippy".into()),
            code: None,
            data: None,
            told: crate::doc::Told::Server(0),
        }];
        app.ask_hover(4);
        let hover = app.hover.as_ref().expect("no box appeared");
        assert!(
            hover.lines.iter().any(|l| l.text == "x is never read"),
            "{:?}",
            hover.lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
    }

    /// Two panes, each showing a file of its own, ready to be compared.
    ///
    /// `tag` names the pair, because these run beside each other and two tests
    /// writing to one path is two tests failing at random.
    fn two_panes(tag: &str, left: &str, right: &str) -> (App, mpsc::Receiver<Event>, PathBuf) {
        let (mut app, rx) = editor();
        let a = scratch(&format!("{tag}-a.txt"));
        let b = scratch(&format!("{tag}-b.txt"));
        std::fs::write(&a, left).expect("written");
        std::fs::write(&b, right).expect("written");
        app.open_path(&a);
        app.run(Cmd::SPLIT);
        app.open_path(&b);
        assert_eq!(app.panes.len(), 2, "the split did not happen");
        (app, rx, a.parent().expect("a directory").to_path_buf())
    }

    #[test]
    fn comparing_two_panes_marks_what_differs_on_both_sides() {
        let (mut app, _rx, dir) = two_panes("dcmp", "one\ntwo\nthree\n", "one\nextra\ntwo\nthree\n");
        app.run(Cmd::DIFF_PANES);
        let diff = app.diff.as_ref().expect("nothing was compared");
        assert!(!diff.same());
        let (left, right) = diff.panes();
        assert_eq!(
            diff.mark(right, 1),
            Some(crate::git::Mark::Added),
            "the line only the right has was not marked"
        );
        assert!(
            (0..3).any(|line| diff.mark(left, line).is_some()),
            "the left said nothing about a line it is missing"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn comparing_scrolls_the_other_pane_to_line_up() {
        let mut left = String::from("head\n");
        let mut right = String::from("head\n");
        // Ten lines only the right has, then a hundred the two share.
        for n in 0..10 {
            right.push_str(&format!("only-right-{n}\n"));
        }
        for n in 0..100 {
            left.push_str(&format!("shared-{n}\n"));
            right.push_str(&format!("shared-{n}\n"));
        }
        let (mut app, _rx, dir) = two_panes("dscroll", &left, &right);
        // The focus is the pane opened second, which is the right-hand file.
        app.run(Cmd::DIFF_PANES);
        let (left_pane, right_pane) = app.diff.as_ref().expect("compared").panes();
        let here = app.focus.min(app.panes.len() - 1);
        let there = if here == left_pane { right_pane } else { left_pane };

        app.panes[here].top = 40;
        app.tick();
        let want = app
            .diff
            .as_ref()
            .expect("compared")
            .beside(here, 40)
            .expect("a line beside it");
        assert_eq!(
            app.panes[there].top, want,
            "the other pane did not follow: {} vs {want}",
            app.panes[there].top
        );
        assert_ne!(
            app.panes[there].top, 40,
            "it followed by copying the number rather than by lining up"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_edit_is_taken_into_account_without_asking_again() {
        let (mut app, _rx, dir) = two_panes("dedit", "one\ntwo\n", "one\nTWO\n");
        app.run(Cmd::DIFF_PANES);
        assert!(!app.diff.as_ref().expect("compared").same());

        // Make the two agree. The comparison should notice on its own.
        app.run(Cmd::SELECT_ALL);
        typed(&mut app, "one\ntwo\n");
        app.tick();
        assert!(
            app.diff.as_ref().expect("compared").same(),
            "the comparison did not keep up with the text"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn closing_a_pane_ends_the_comparison() {
        let (mut app, _rx, dir) = two_panes("dclose", "one\n", "two\n");
        app.run(Cmd::DIFF_PANES);
        assert!(app.diff.is_some());
        app.run(Cmd::CLOSE_PANE);
        app.tick();
        assert!(app.diff.is_none(), "a comparison of one pane");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn comparing_needs_two_panes() {
        let (mut app, _rx) = editor();
        app.run(Cmd::DIFF_PANES);
        assert!(app.diff.is_none());
    }

    #[test]
    fn right_clicking_inside_a_selection_keeps_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "one two three");
        app.run(Cmd::SELECT_ALL);
        let was = app.view().sel.primary();

        // Somewhere in the middle of the line, which is inside the selection.
        app.right_click(10, 1);
        assert!(matches!(app.overlay, Overlay::Menu(_)), "no menu opened");
        assert_eq!(
            app.view().sel.primary(),
            was,
            "the selection was thrown away"
        );
    }

    #[test]
    fn a_tab_held_against_the_end_of_a_full_row_keeps_moving() {
        let (mut app, _rx) = editor();
        for _ in 0..4 {
            app.run(Cmd::NEW);
        }
        let id = app.view().doc;
        let was = app.docs.iter().position(|d| d.id == id).expect("open");
        assert!(was > 0, "there is nowhere to move it from");

        // The row as the drawing would have left it with more tabs than fit:
        // an arrow at the left end, whose scroll target is behind where the
        // row is now, and the pointer holding the tab over it.
        app.tab_scroll = 8;
        app.tab_nudges = vec![(Rect::new(0, 0, 1, 1), 0)];
        app.drag = Some(Drag::Tab {
            id,
            at: (0, 0),
            stepped: Instant::now() - TAB_STEP_EVERY,
        });

        app.tick();
        assert_eq!(
            app.docs.iter().position(|d| d.id == id),
            Some(was - 1),
            "holding a tab against the arrow did not walk it along"
        );

        // And not again straight away: it walks at a pace you can stop at.
        app.tick();
        assert_eq!(app.docs.iter().position(|d| d.id == id), Some(was - 1));
    }

    #[test]
    fn a_tab_held_against_the_end_does_not_walk_off_it() {
        let (mut app, _rx) = editor();
        app.run(Cmd::NEW);
        let id = app.docs[0].id;
        app.show(id);
        app.tab_scroll = 4;
        app.tab_nudges = vec![(Rect::new(0, 0, 1, 1), 0)];
        app.drag = Some(Drag::Tab {
            id,
            at: (0, 0),
            stepped: Instant::now() - TAB_STEP_EVERY,
        });
        app.tick();
        assert_eq!(
            app.docs.iter().position(|d| d.id == id),
            Some(0),
            "the first tab was moved before the first tab"
        );
    }

    #[test]
    fn letting_go_of_a_tab_ends_the_drag() {
        let (mut app, _rx) = editor();
        app.run(Cmd::NEW);
        let id = app.view().doc;
        app.drag = Some(Drag::Tab {
            id,
            at: (0, 0),
            stepped: Instant::now(),
        });
        assert_eq!(app.dragging_tab(), Some(id));
        app.handle(Event::Term(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })));
        assert!(app.drag.is_none(), "the tab is still being carried");
        assert_eq!(app.dragging_tab(), None);
    }

    #[test]
    fn a_tab_menu_offers_moving_it_and_says_where_it_cannot_go() {
        let (mut app, _rx) = editor();
        app.run(Cmd::NEW);
        app.run(Cmd::NEW);
        let first = app.docs[0].id;
        let menu = app.tab_menu(first, (0, 0));
        let row = |cmd: Cmd| {
            menu.items
                .iter()
                .find(|i| i.action == crate::menu::Action::RunOn(first, cmd))
                .unwrap_or_else(|| panic!("no row for {cmd:?}"))
        };
        assert!(
            !row(Cmd::MOVE_TAB_LEFT).enabled,
            "the first tab was offered a move left"
        );
        assert!(row(Cmd::MOVE_TAB_RIGHT).enabled);
    }

    #[test]
    fn right_clicking_a_tab_offers_things_about_that_tab() {
        let (mut app, _rx) = editor();
        let path = scratch("tab-menu.txt");
        std::fs::write(&path, "text\n").expect("written");
        app.open_path(&path);
        let id = app.view().doc;
        app.tab_hits = vec![(Rect::new(0, 0, 10, 1), id, false)];

        app.right_click(3, 0);
        let Overlay::Menu(menu) = &app.overlay else {
            panic!("no menu");
        };
        assert!(
            menu.items
                .iter()
                .any(|i| matches!(i.action, crate::menu::Action::RunOn(_, Cmd::CLOSE_OTHERS))),
            "a tab menu with nothing about tabs in it"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_suggestion_is_drawn_in_the_colour_that_kind_of_thing_has_in_the_file() {
        // A list of forty suggestions all in one colour is a list you have to
        // read a word at a time to find the method among the fields.
        let role = |n| completion_role(n);
        // A method is a function, a field is a property, a class is a type,
        // and a keyword is a keyword. Nothing here is a new vocabulary.
        assert_eq!(role(2), Role::Function);
        assert_eq!(role(3), Role::Function);
        assert_eq!(role(5), Role::Property);
        assert_eq!(role(7), Role::Type);
        assert_eq!(role(14), Role::Keyword);
        assert_eq!(role(21), Role::Constant);
        // The four kinds that get asked about most are four different
        // colours, which is the whole point of doing this at all.
        let four = [role(3), role(5), role(7), role(14)];
        for (at, one) in four.iter().enumerate() {
            assert!(
                !four[at + 1..].contains(one),
                "{four:?} has two of the same colour in it"
            );
        }
        // Something a later LSP invents is drawn as ordinary text rather than
        // as a guess.
        assert_eq!(role(99), Role::Variable);
    }

    #[test]
    fn a_line_too_wide_for_the_box_folds_rather_than_being_cut_off() {
        // The whole complaint: a box that only scrolls downwards and elides
        // sideways is showing you the first half of every sentence in it.
        let line = DocLine::prose("the quick brown fox jumps over the lazy dog");
        let folded = line.wrap(20);
        let text: Vec<&str> = folded.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, ["the quick brown fox", "jumps over the lazy", "dog"]);
        // Nothing was lost and nothing was added.
        assert_eq!(text.join(" "), "the quick brown fox jumps over the lazy dog");
        for row in &folded {
            assert!(crate::text::str_width(&row.text) <= 20);
        }
    }

    #[test]
    fn a_line_that_already_fits_is_left_exactly_as_it_was() {
        let line = DocLine::prose("short enough");
        assert_eq!(line.wrap(40), vec![line.clone()]);
        // And a rule stays one character, because the drawing is what
        // stretches it and only the drawing knows how wide the box turned out.
        let rule = DocLine::prose(RULE.to_string());
        assert_eq!(rule.wrap(4), vec![rule.clone()]);
    }

    #[test]
    fn a_fold_keeps_the_indentation_of_the_line_it_came_from() {
        // Otherwise a bulleted list stops being a list at its first long item.
        let line = DocLine::prose("    an indented sentence that runs on and on");
        let text: Vec<String> = line.wrap(20).into_iter().map(|l| l.text).collect();
        assert_eq!(
            text,
            ["    an indented", "    sentence that", "    runs on and on"]
        );
    }

    #[test]
    fn a_word_with_no_spaces_in_it_is_broken_rather_than_left_hanging() {
        // A Rust type is one long word constantly, and a row holding a single
        // character is not an improvement on eliding.
        let line = DocLine::prose("BTreeMap<String,Vec<Something::Awfully::Long>>");
        let text: Vec<String> = line.wrap(16).into_iter().map(|l| l.text).collect();
        assert_eq!(text.concat(), "BTreeMap<String,Vec<Something::Awfully::Long>>");
        for row in &text {
            assert!(crate::text::str_width(row) <= 16, "{row:?}");
        }
    }

    #[test]
    fn folding_carries_the_colours_and_the_names_across_to_where_they_now_sit() {
        // A folded line whose spans still pointed at the unfolded offsets
        // would colour the wrong letters, and a name you could no longer
        // click is a name you can no longer go to the definition of.
        let mut lines = Vec::new();
        push_code(
            &mut lines,
            "let mapping: BTreeMap<String, u32> = BTreeMap::new();
",
            lang::by_name("rust"),
        );
        let line = lines.first().expect("one line of code");
        assert!(!line.spans.is_empty(), "the code was coloured to begin with");
        for row in line.wrap(24) {
            for (span, _) in &row.spans {
                assert!(row.text.get(span.clone()).is_some(), "{row:?} {span:?}");
            }
            for link in &row.links {
                assert!(link.end <= row.text.chars().count(), "{row:?} {link:?}");
            }
        }
    }

    #[test]
    fn a_folded_hover_can_be_read_all_the_way_down_and_keeps_its_place() {
        let long = "a sentence long enough that it has to be folded more than once to fit";
        let mut popup = Popup::new(vec![DocLine::prose(long); 4], 0);
        popup.fold_to(20);
        assert!(popup.lines.len() > 4, "it folded");
        // Every row is inside the box, which is the point.
        for row in &popup.lines {
            assert!(crate::text::str_width(&row.text) <= 20);
        }
        // Scrolled to the third line's first row, then folded again at
        // another width: the same line of text is still on the top row.
        popup.scroll = popup.folded_at_line(2);
        popup.fold_to(30);
        assert_eq!(popup.scroll, popup.folded_at_line(2));
        assert_eq!(popup.unfolded_at(popup.scroll), 2);
    }

    /// The parts of a rendered line a pointer would offer to follow.
    fn followable(line: &DocLine) -> Vec<String> {
        line.links
            .iter()
            .map(|range| line.text.chars().skip(range.start).take(range.len()).collect())
            .collect()
    }

    #[test]
    fn only_what_the_markup_called_code_is_worth_following() {
        let line = DocLine::prose("There is always at least one, and they are in order.");
        assert!(followable(&line).is_empty(), "prose is not a set of names");

        let line = DocLine::prose("assume, and [`Selections::normalise`](https://x/y) keeps it");
        assert_eq!(followable(&line), vec!["Selections::normalise"]);

        let line = DocLine::prose("A `HashMap` of `String` to `u32`, and a [bracket] in prose");
        assert_eq!(followable(&line), vec!["HashMap", "String", "u32"]);

        // The backtick that closed a link's name must not be read as one
        // opening another, or the rest of the line becomes a name.
        let line = DocLine::prose("[`Eq`](https://x/y) and then `Ord` and more prose");
        assert_eq!(followable(&line), vec!["Eq", "Ord"]);
    }

    #[test]
    fn a_name_in_a_docstring_is_followed_and_the_prose_around_it_is_not() {
        let line = DocLine::prose("see `Selections` for that");
        let at = |column| {
            line.links
                .iter()
                .any(|r| r.contains(&column))
                .then(|| word_span(&line.text, column).map(|(w, ..)| w))
                .flatten()
        };
        assert_eq!(at(6), Some("Selections".into()));
        assert_eq!(at(1), None, "`see` is a word, not a name");
        assert_eq!(at(19), None, "`for` is a word, not a name");
    }

    #[test]
    fn in_code_the_names_are_followed_and_the_keywords_are_not() {
        let rust = lang::by_name("rust").expect("rust");
        let mut lines = Vec::new();
        push_code(&mut lines, "let it: HashMap<String, u32> = HashMap::new();\n", Some(rust));
        let line = lines.first().expect("a line");
        let names = followable(line);
        assert!(names.contains(&"HashMap".to_string()), "{names:?}");
        assert!(names.contains(&"String".to_string()), "{names:?}");
        assert!(!names.contains(&"let".to_string()), "a keyword is not a name");
        assert!(!names.contains(&"it".to_string()), "a local is not a name");
    }

    #[test]
    fn a_word_in_a_docstring_is_the_word_and_not_the_punctuation() {
        let word_in = |line: &str, column: usize| word_span(line, column).map(|(w, ..)| w);
        let line = "fn take(list: Vec<Widget>)";
        assert_eq!(word_in(line, 15), Some("Vec".into()));
        assert_eq!(word_in(line, 18), Some("Widget".into()));
        assert_eq!(word_in(line, 17), None, "`<` is not a word");
        assert_eq!(
            word_in(line, line.len()),
            None,
            "past the end is not a word"
        );
        // A single letter is `T` or `a`, which is everywhere in a paragraph
        // of prose, and a bare number is a length rather than a type.
        assert_eq!(word_in("a Vec of T items", 0), None);
        assert_eq!(word_in("a Vec of T items", 9), None);
        assert_eq!(word_in("at most 4096 bytes", 8), None);
        assert_eq!(word_in("a Vec of T items", 2), Some("Vec".into()));
        assert_eq!(
            word_in("see [`Selections::normalise`]", 7),
            Some("Selections".into()),
            "markdown punctuation is not part of the name"
        );
    }

    #[test]
    fn typing_and_saving_puts_the_text_on_disk() {
        let (mut app, _rx) = editor();
        let path = scratch("typed.txt");
        std::fs::remove_file(&path).ok();
        app.open_path(&path);
        typed(&mut app, "hello\nworld");
        app.save(None);
        assert_eq!(
            std::fs::read_to_string(&path).expect("written"),
            "hello\nworld\n"
        );
        assert!(!app.here().is_modified());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn escape_takes_off_one_layer_at_a_time() {
        let (mut app, _rx) = editor();
        typed(&mut app, "one two three");
        app.run(Cmd::SELECT_ALL);
        app.run(Cmd::ADD_CURSOR_BELOW);
        // Whatever the cursors ended up as, Escape works back to one bare one.
        for _ in 0..4 {
            app.run(Cmd::ESCAPE);
        }
        assert_eq!(app.view().sel.len(), 1);
        assert!(app.view().sel.primary().is_empty());
    }

    #[test]
    fn find_walks_the_matches_and_wraps_round() {
        let (mut app, _rx) = editor();
        typed(&mut app, "alpha beta alpha gamma alpha");
        app.run(Cmd::MOVE_DOC_START);
        app.last_search = "alpha".into();

        app.run(Cmd::FIND_NEXT);
        let first = app.view().sel.primary().start();
        app.run(Cmd::FIND_NEXT);
        let second = app.view().sel.primary().start();
        assert!(second > first);
        app.run(Cmd::FIND_NEXT);
        app.run(Cmd::FIND_NEXT);
        // Four steps through three matches comes back to the first.
        assert_eq!(app.view().sel.primary().start(), first);
    }

    #[test]
    fn a_lower_case_search_ignores_case_and_a_capital_means_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "Thing thing THING");
        assert_eq!(app.count_matches("thing"), 3);
        assert_eq!(app.count_matches("Thing"), 1);
    }

    #[test]
    fn replacing_changes_every_match_as_one_undo() {
        let (mut app, _rx) = editor();
        typed(&mut app, "red green red blue red");
        app.run(Cmd::MOVE_DOC_START);
        app.replace_all("red", "amber");
        assert_eq!(app.here().rope.to_string(), "amber green amber blue amber");
        app.run(Cmd::UNDO);
        assert_eq!(app.here().rope.to_string(), "red green red blue red");
    }

    #[test]
    fn replacing_inside_a_selection_leaves_the_rest_alone() {
        let (mut app, _rx) = editor();
        typed(&mut app, "red red red");
        app.view_mut().sel = Selections::single(Range::new(0, 7));
        app.replace_all("red", "blue");
        assert_eq!(app.here().rope.to_string(), "blue blue red");
    }

    #[test]
    fn opening_a_file_twice_shows_the_one_already_open() {
        let (mut app, _rx) = editor();
        let path = scratch("once.txt");
        std::fs::write(&path, "content\n").expect("written");
        app.open_path(&path);
        let first = app.view().doc;
        app.run(Cmd::NEW);
        app.open_path(&path);
        assert_eq!(app.view().doc, first);
        assert_eq!(
            app.docs()
                .iter()
                .filter(|d| d.path.as_deref() == Some(path.as_path()))
                .count(),
            1
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn closing_the_last_buffer_leaves_one_to_type_in() {
        let (mut app, _rx) = editor();
        app.run(Cmd::CLOSE_FORCE);
        assert_eq!(app.docs().len(), 1);
        assert!(!app.quit);
    }

    #[test]
    fn quitting_with_unsaved_work_asks_first() {
        let (mut app, _rx) = editor();
        typed(&mut app, "unsaved");
        app.run(Cmd::QUIT);
        assert!(!app.quit);
        assert!(matches!(app.overlay, Overlay::Confirm(_)));
        // Saying no keeps everything.
        app.confirm_key(Key::parse("c").unwrap());
        assert!(!app.quit);
        assert_eq!(app.here().rope.to_string(), "unsaved");
        // Saying discard leaves.
        app.run(Cmd::QUIT);
        app.confirm_key(Key::parse("d").unwrap());
        assert!(app.quit);
    }

    #[test]
    fn the_palette_runs_what_you_choose() {
        let (mut app, _rx) = editor();
        typed(&mut app, "one\ntwo\nthree");
        app.run(Cmd::COMMAND_PALETTE);
        let Overlay::Picker(picker) = &mut app.overlay else {
            panic!("the palette did not open");
        };
        for c in "select-all".chars() {
            picker.type_char(c);
        }
        app.choose();
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.view().sel.primary().len(), app.here().len_chars());
    }

    #[test]
    fn a_setting_changed_from_the_list_takes_effect_and_the_list_stays_open() {
        let (mut app, _rx) = editor();
        let before = app.config.show_whitespace();
        app.run(Cmd::SETTINGS);
        let Overlay::Picker(picker) = &mut app.overlay else {
            panic!("the settings did not open");
        };
        let at = picker
            .visible()
            .position(|(row, _)| matches!(row.choice, Choice::Setting("show_whitespace")))
            .expect("the setting is on the list");
        picker.select(at);
        app.choose();
        assert_ne!(app.config.show_whitespace(), before);
        assert!(matches!(app.overlay, Overlay::Picker(_)));
    }

    #[test]
    fn a_pane_split_in_two_keeps_both_cursors_pointing_at_the_same_text() {
        let (mut app, _rx) = editor();
        typed(&mut app, "alpha\nbeta\ngamma");
        app.run(Cmd::SPLIT);
        assert_eq!(app.panes.len(), 2);
        // Put the other pane's cursor at the end, then type at the start.
        app.panes[0].sel = Selections::single(Range::point(app.here().len_chars()));
        app.focus = 1;
        app.view_mut().sel = Selections::single(Range::point(0));
        typed(&mut app, "XY");
        // The other pane's cursor moved along with the text it was pointing at.
        assert_eq!(app.panes[0].cursor(), app.here().len_chars());
        assert_eq!(app.here().rope.to_string(), "XYalpha\nbeta\ngamma");
    }

    #[test]
    fn a_buffer_is_not_rewritten_under_you_with_something_that_is_not_text() {
        // Half a file very often ends in the middle of a character, and a
        // lossy conversion turns the remains into replacement characters. Done
        // on a timer, to a buffer somebody is looking at, that is rubbish
        // appearing in their file from nowhere.
        let (mut app, _rx) = editor();
        let path = scratch("torn.txt");
        std::fs::write(&path, "hello\n").expect("written");
        app.open_path(&path);
        let id = app.view().doc;

        std::fs::write(&path, b"good \xff\xfe bad").expect("written");
        assert!(
            app.take_from_disk(id, Reread::OnATimer).is_err(),
            "it should refuse"
        );
        assert_eq!(app.here().rope.to_string(), "hello\n", "and leave the buffer alone");

        // Asked for outright, it still does its best — you can see the result
        // and undo it.
        assert!(app.take_from_disk(id, Reread::Asked).is_ok());
        assert!(app.here().rope.to_string().starts_with("good "));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_file_being_written_to_is_left_alone_until_it_stops() {
        let (mut app, _rx) = editor();
        let path = scratch("busy.txt");
        std::fs::write(&path, "one\n").expect("written");
        app.open_path(&path);
        let id = app.view().doc;

        // Changing every time we look. Nothing is taken, and the editor says
        // it wants to look again sooner.
        for n in 1..5 {
            std::fs::write(&path, format!("{}\n", "line\n".repeat(n))).expect("written");
            app.check_disk();
            assert!(app.unsettled, "it was still moving");
            assert_eq!(app.here().rope.to_string(), "one\n", "and was not taken");
        }

        // It stops. The next look sees it twice the same and takes it.
        app.check_disk();
        assert!(!app.unsettled);
        assert_eq!(app.doc(id).map(|d| d.rope.to_string()).as_deref(), Some("line\nline\nline\nline\n\n"));
        std::fs::remove_file(&path).ok();
    }

    /// A panel a plugin declared, as a `&'static Command` the editor can be
    /// handed. Leaked, because that is what the registry hands out and the
    /// command tables hold.
    fn docked_panel(id: &str, edge: Option<&str>, size: Option<u16>) -> &'static crate::plugin::Command {
        let dock = edge.map(|e| {
            crate::view::Dock::new(crate::view::Edge::parse(e).expect("an edge"), size)
        });
        Box::leak(Box::new(crate::plugin::Command {
            id: id.to_string(),
            name: id.split('/').next_back().unwrap_or(id).to_string(),
            about: "a panel".into(),
            plugin: id.split('/').next().unwrap_or(id).to_string(),
            behaviour: crate::cmd::Behaviour::Passive,
            languages: Vec::new(),
            opens_panel: true,
            dock,
        }))
    }

    #[test]
    fn a_docked_panel_opens_beside_the_code_rather_than_over_it() {
        // The whole point of a dock: you asked for a tree of files, not for
        // the file you were reading to go away.
        let (mut app, _rx) = editor();
        let was = app.view().doc;
        app.open_panel(docked_panel("files/tree", Some("left"), Some(30)));

        assert_eq!(app.panes.len(), 2);
        // On the left, and it has the focus, because you just asked for it.
        assert_eq!(app.focus, 0);
        assert_eq!(
            app.panes[0].dock.map(|d| (d.edge, d.size)),
            Some((crate::view::Edge::Left, 30))
        );
        // And the code is still there, still showing what it was showing.
        assert!(app.panes[1].dock.is_none());
        assert_eq!(app.panes[1].doc, was);

        // Its buffer belongs to the plugin and nothing types into it.
        let panel = app.doc(app.panes[0].doc).expect("a buffer");
        assert!(panel.read_only);
        assert_eq!(panel.panel.as_ref().map(|p| p.id.as_str()), Some("files/tree"));
    }

    #[test]
    fn opening_a_file_from_a_sidebar_puts_it_beside_the_sidebar() {
        // A file explorer that replaced itself with the file you clicked would
        // have thrown away the tree to show you one leaf of it.
        let (mut app, _rx) = editor();
        let code = app.view().doc;
        app.open_panel(docked_panel("files/tree", Some("left"), None));
        assert_eq!(app.focus, 0, "standing in the sidebar");
        let sidebar = app.panes[0].doc;

        // Whatever the plugin asked to open goes in the middle.
        app.run(Cmd::NEW);
        assert!(app.panes[0].dock.is_some(), "the sidebar is still a sidebar");
        assert_eq!(
            app.panes[0].doc, sidebar,
            "the sidebar was made to show something else"
        );
        assert_eq!(app.focus, 1, "and the focus moved out of it");
        assert_ne!(app.panes[1].doc, code, "the new buffer went in the middle");

        // The one thing a dock does show is the panel it was opened for, so
        // refreshing it must not be pushed out into the middle.
        app.focus = 0;
        app.show(sidebar);
        assert_eq!(app.focus, 0);
        assert_eq!(app.panes[0].doc, sidebar);
    }

    #[test]
    fn a_sidebar_can_be_pulled_wider_and_narrower() {
        let (mut app, _rx) = editor();
        app.screen = Rect::new(0, 0, 100, 30);
        app.open_panel(docked_panel("files/tree", Some("left"), Some(30)));
        // What the drawing would have worked out, since a drag measures
        // against where the pane actually is.
        app.panes[0].frame = Rect::new(0, 1, 30, 28);

        app.resize_dock(0, 44, 10);
        assert_eq!(app.panes[0].dock.map(|d| d.size), Some(45));

        app.panes[0].frame = Rect::new(0, 1, 45, 28);
        app.resize_dock(0, 14, 10);
        assert_eq!(app.panes[0].dock.map(|d| d.size), Some(15));

        // Never down to nothing, and never so wide the middle is squeezed out
        // — a width that only looked right because the layout clamped it is a
        // width that springs back the moment the terminal is resized.
        app.resize_dock(0, 0, 10);
        assert_eq!(app.panes[0].dock.map(|d| d.size), Some(MIN_DOCK));
        app.panes[0].frame = Rect::new(0, 1, MIN_DOCK, 28);
        app.resize_dock(0, 99, 10);
        assert_eq!(
            app.panes[0].dock.map(|d| d.size),
            Some(100 - MIN_MIDDLE_ROOM)
        );
    }

    #[test]
    fn running_a_docked_panels_command_again_puts_it_away() {
        // That is what collapsible means from the keyboard. A sidebar you can
        // only open is a sidebar everybody closes by quitting.
        let (mut app, _rx) = editor();
        let panel = docked_panel("files/tree", Some("left"), None);
        app.open_panel(panel);
        assert_eq!(app.panes.len(), 2);
        app.open_panel(panel);
        assert_eq!(app.panes.len(), 1, "it should have gone away");
        assert!(app.panes[0].dock.is_none());
        // And opening it again gets the same buffer rather than a second one.
        app.open_panel(panel);
        assert_eq!(app.panes.len(), 2);
        assert_eq!(
            app.docs.iter().filter(|d| d.panel.is_some()).count(),
            1,
            "a second buffer was made for the same panel"
        );
    }

    #[test]
    fn a_panel_with_no_edge_is_still_a_tab() {
        // Which is what a panel used to always be, and is still right for
        // something you read and then leave.
        let (mut app, _rx) = editor();
        app.open_panel(docked_panel("cargo/report", None, None));
        assert_eq!(app.panes.len(), 1, "a tab is not a pane");
        assert!(app.here().panel.is_some(), "and it is what the pane shows");
    }

    #[test]
    fn the_last_pane_showing_a_file_cannot_be_closed_but_a_dock_always_can() {
        let (mut app, _rx) = editor();
        app.open_panel(docked_panel("files/tree", Some("left"), None));
        // Standing in the dock: closing it is fine, even though it is one of
        // only two panes.
        assert_eq!(app.focus, 0);
        app.run(Cmd::CLOSE_PANE);
        assert_eq!(app.panes.len(), 1);

        // Standing in the only pane showing a file, with a dock open: still
        // refused, because what has to survive is somewhere to read code.
        app.open_panel(docked_panel("files/tree", Some("left"), None));
        app.focus = 1;
        app.run(Cmd::CLOSE_PANE);
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.status.text, "that is the only pane");
    }

    #[test]
    fn splitting_a_sidebar_does_not_give_you_two_sidebars() {
        let (mut app, _rx) = editor();
        app.open_panel(docked_panel("files/tree", Some("left"), None));
        assert_eq!(app.focus, 0);
        app.run(Cmd::SPLIT);
        assert_eq!(app.panes.len(), 3);
        assert_eq!(
            app.panes.iter().filter(|p| p.dock.is_some()).count(),
            1,
            "the copy was docked too"
        );
    }

    #[test]
    fn comparing_two_panes_ignores_the_sidebar() {
        // Comparing the code against a tree of file names is not a thing
        // anybody means by "compare the two panes".
        let (mut app, _rx) = editor();
        app.open_panel(docked_panel("files/tree", Some("left"), None));
        app.focus = 1;
        // One dock and one file pane is not two panes to compare.
        app.run(Cmd::DIFF_PANES);
        assert!(app.diff.is_none(), "{}", app.status.text);
        assert!(app.status.text.contains("two panes"), "{}", app.status.text);

        // With a real second pane it compares those two and leaves the dock
        // out of it.
        app.run(Cmd::SPLIT);
        app.run(Cmd::DIFF_PANES);
        let (left, right) = app.diff.as_ref().expect("compared").panes();
        assert!(app.panes[left].dock.is_none());
        assert!(app.panes[right].dock.is_none());
    }

    #[test]
    fn a_plugin_that_is_one_server_gets_one_row_in_the_list() {
        // It used to be a row for the plugin and an indented copy of itself
        // underneath with the same switch on it, which is one switch shown
        // twice and a list twice as long as it needs to be.
        //
        // Read against manifests rather than against the registry, because a
        // language server is fetched from a package repository now and a test
        // cannot assume one has been.
        let read = |manifest: &str, id: &str| {
            let file: crate::plugin::FilePlugin = serde_json::from_str(manifest).expect("read");
            file.into_plugin(id, crate::plugin::Source::BuiltIn).0
        };

        let pyright = read(
            r#"{"id":"pyright","name":"Pyright","languages":{"python":{"servers":[
                 {"name":"pyright","command":"pyright-langserver"}]}}}"#,
            "pyright",
        );
        assert!(
            server_rows(&pyright, |_| true).is_empty(),
            "a plugin that is one server got a second row of itself"
        );

        // And one that has several things in it still shows them, indented,
        // and says which language each is for.
        let vscode = read(
            r#"{"id":"vscode-langservers","name":"VS Code's servers","languages":{
                 "css":{"servers":[{"name":"css-language-server","command":"vscode-css-language-server"}]},
                 "html":{"servers":[{"name":"html-language-server","command":"vscode-html-language-server"}]},
                 "json":{"servers":[{"name":"json-language-server","command":"vscode-json-language-server"}]}}}"#,
            "vscode-langservers",
        );
        let rows = server_rows(&vscode, |_| true);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "  css-language-server",
                "  html-language-server",
                "  json-language-server"
            ]
        );
        assert_eq!(
            rows[0].detail.as_deref(),
            Some("vscode-langservers/css-language-server — runs vscode-css-language-server for css")
        );

        // A server switched off says so, rather than sitting there looking on
        // and quietly doing nothing.
        assert_eq!(server_rows(&vscode, |_| false)[0].tag.as_deref(), Some("off"));
    }

    #[test]
    fn installing_something_nobody_has_heard_of_says_so() {
        // A command that quietly does nothing is the failure worth testing
        // for here: an install has no result you can see until it finishes,
        // so one that never started has to say it never started.
        let (mut app, _rx) = editor();
        app.start_install("a-plugin-nobody-wrote");
        assert_eq!(app.status.tone, Tone::Bad);
        assert!(
            app.status.text.contains("a-plugin-nobody-wrote"),
            "{}",
            app.status.text
        );
        assert!(app.installing.is_none(), "and nothing is left running");
    }

    #[test]
    fn a_language_built_into_the_binary_cannot_be_uninstalled() {
        // There would be nothing for it to mean. Switching it off is the
        // thing you want, and the message says so rather than leaving you to
        // work out why nothing happened.
        let (mut app, _rx) = editor();
        app.start_uninstall("rust");
        assert_eq!(app.status.tone, Tone::Bad);
        assert!(app.status.text.contains("switch it off"), "{}", app.status.text);
    }

    #[test]
    fn one_install_at_a_time() {
        // Two `npm install`s at once is two of them fighting over the same
        // directory, and the second one is nearly always Enter pressed twice.
        let (mut app, _rx) = editor();
        app.installing = Some(Installing {
            id: "busy".into(),
            removing: false,
            log: String::new(),
        });
        app.start_plan(Ok(crate::pack::Plan {
            id: "other".into(),
            name: "Other".into(),
            removing: false,
            files: crate::pack::Files::Leave,
            steps: vec![crate::plugin::Step {
                about: "something".into(),
                run: vec!["true".into()],
                unless: None,
                when: None,
                os: Vec::new(),
                arch: Vec::new(),
                system: false,
            }],
            steps_from: None,
            needs: Vec::new(),
            see: None,
        }));
        assert!(app.status.text.contains("busy"), "{}", app.status.text);
        assert_eq!(
            app.installing.as_ref().map(|i| i.id.clone()),
            Some("busy".to_string()),
            "the one that was already going is the one still going"
        );
    }

    #[test]
    fn what_an_install_says_lands_in_a_buffer_you_can_read() {
        let (mut app, _rx) = editor();
        app.installing = Some(Installing {
            id: "zls".into(),
            removing: false,
            log: String::new(),
        });
        let note = |note| {
            Box::new(crate::pack::Progress {
                id: "zls".into(),
                note,
            })
        };
        app.on_package(*note(crate::pack::Note::Doing {
            at: 1,
            of: 1,
            about: "zls, with brew".into(),
        }));
        app.on_package(*note(crate::pack::Note::Did {
            about: "brew install zls".into(),
            ok: true,
            output: "poured from bottle\n".into(),
        }));
        app.on_package(*note(crate::pack::Note::Done {
            ok: true,
            why: "zls installed".into(),
        }));

        assert!(app.installing.is_none());
        assert_eq!(app.status.tone, Tone::Good);
        let log = app
            .docs
            .iter()
            .find(|d| d.name == "install zls")
            .expect("what it said is somewhere you can read it");
        assert!(log.rope.to_string().contains("poured from bottle"));
    }

    #[test]
    fn a_read_only_file_refuses_to_be_changed() {
        let (mut app, _rx) = editor();
        typed(&mut app, "fixed");
        let id = app.view().doc;
        app.doc_mut(id).expect("open").read_only = true;
        app.run(Cmd::DELETE_LINE);
        typed(&mut app, "more");
        assert_eq!(app.here().rope.to_string(), "fixed");
        assert_eq!(app.status.tone, Tone::Bad);
    }

    #[test]
    fn a_place_named_on_the_command_line_is_where_the_cursor_lands() {
        let (mut app, _rx) = editor();
        typed(&mut app, "one\ntwo\nthree\nfour");
        app.jump_to(2, 3);
        let at = app.view().cursor();
        assert_eq!(text::line_of(&app.here().rope, at), 2);
        assert_eq!(at - text::line_start(&app.here().rope, 2), 3);
    }
}
