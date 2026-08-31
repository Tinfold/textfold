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
    /// A thread finished asking git who last touched a line.
    Blamed(String),
    /// A plugin's own program said something.
    Plugin(HostId, Incoming),
    /// A debug adapter said something, and which session it was.
    ///
    /// Named even though there is only one session at a time, because the
    /// session before it can still be talking: killing an adapter is what
    /// makes its reader thread post that it has gone, and that arrives after
    /// the next one has started. See [`crate::dap::SessionId`].
    Dap(crate::dap::SessionId, Incoming),
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
    /// A thread finished working out which files a project-wide replace would
    /// touch. Nothing has been changed by it: this is the question, and the
    /// answer is somebody pressing a key. Boxed because it carries a path per
    /// file and every other event is a few words.
    ToReplace(Box<Replace>),
}

/// A replacement across the project, worked out and waiting to be agreed to.
#[derive(Clone, Debug)]
pub struct Replace {
    pub needle: String,
    pub with: String,
    /// The files that have it in them, and how many times each — from the
    /// disk, so a file open with unsaved changes is counted again against the
    /// buffer when the replacing actually happens.
    pub files: Vec<(PathBuf, usize)>,
    /// How many matched but were left out, the walk having found more than it
    /// is willing to open at once.
    pub over: usize,
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

/// What a build was started for.
///
/// The interesting half of having a build at all. `cc -g -o main main.c` is
/// not a thing anybody wants to *watch*; it is a thing that has to have
/// happened before a debugger has anything to open, and the way to make that
/// reliably true is for the key that starts the debugger to do it. Without
/// this, the commonest first run of a C program under textfold was `gdb`
/// reporting that `main` does not exist — which is true, and says nothing
/// about what to do next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AfterBuild {
    /// Nothing. Somebody asked for a build and got one.
    Nothing,
    /// Debug the file it was built from, if it worked. A build that failed
    /// stops here: a debugger started on yesterday's binary after today's
    /// compile failed is the worst kind of working, because everything looks
    /// right and the code being stepped through is not the code on screen.
    Debug,
}

/// What came of asking for a build.
///
/// Three answers rather than two, because "this language has no build" and
/// "this language has a build and it would not run" want opposite things from
/// whoever asked. The first should fall straight through — most languages
/// compile nothing, and F5 on a Python file must not wait for a compiler that
/// was never coming. The second must stop: a debugger started because `cc` is
/// not installed is a debugger opening whatever binary was lying about from
/// last time, which looks exactly like it worked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Building {
    /// Nothing to build. Whatever was going to happen next should happen.
    NotAThing,
    /// One is running; the answer arrives in [`App::on_tool`].
    Started,
    /// There is a build and it did not run. Why has been said already.
    Refused,
}

/// What the last build printed, kept so it can be read afterwards.
///
/// A compiler says a great deal that a margin cannot hold. A linker's
/// `undefined reference to 'fizz'` names no file and no line; `make` reports
/// which recipe failed and nothing about the code; a compiler complaining
/// about a file you do not have open has nowhere to put a mark. Every one of
/// those used to be a build that failed with, from where the person is
/// sitting, no reason given — and "it failed" with no reason is the one thing
/// a build must never say, because there is nothing to do about it but go and
/// run the compiler in another window, which is the whole thing this was
/// supposed to save.
///
/// So the whole of what it printed is kept, whether it worked or not. One
/// build's worth: the interesting one is always the last.
struct Built {
    /// The tool's name, for the buffer it opens in and the line about it.
    name: String,
    ok: bool,
    /// Everything it printed, both pipes — see [`crate::tool::Finished::printed`].
    text: String,
    /// A line about the project's own build, where it failed and there is
    /// one. See [`App::build_note`].
    note: Option<String>,
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

/// How long after an edit to ask the servers about the whole file again —
/// its colours, its inlay hints, its lenses, its problems. Longer than the
/// pause before asking about the cursor, because these are questions about
/// everything rather than about one spot, and nobody is waiting on the answer
/// with a keystroke.
const EXTRAS_DELAY: Duration = Duration::from_millis(500);

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
    /// The search half of a replace across every file in the project.
    ProjectReplaceFind,
    /// The replacement half of one.
    ProjectReplaceWith,
    /// A question a plugin asked. What it says is the plugin's, so the label
    /// here is only the fallback for one that said nothing.
    PluginAsked,
    /// An expression to work out where the program is stopped.
    DebugEvaluate,
    /// Where to attach the debugger, for an adapter that meets a program at an
    /// address rather than picking one out of a list.
    DebugAddress,
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
            PromptKind::ProjectReplaceFind => "Replace in every file",
            PromptKind::ProjectReplaceWith => "Replace them with",
            PromptKind::PluginAsked => "A plugin asks",
            PromptKind::DebugEvaluate => "Value of",
            PromptKind::DebugAddress => "Attach to",
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
#[derive(Clone, Debug)]
pub enum Then {
    Close(DocId),
    /// Go ahead with a replacement across the project. See [`Replace`].
    ReplaceEverywhere(Box<Replace>),
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
    /// Selecting the same columns on every line between two points, having
    /// started with Alt held. The position it began at, in the text — rather
    /// than on the screen, because the view is free to scroll while the button
    /// is down and a screen row remembered from before that is a different
    /// line afterwards.
    Block { anchor: usize },
    /// Selecting text a word at a time, having started with a double click.
    Words {
        anchor_start: usize,
        anchor_end: usize,
    },
    /// A line at a time, having started with a triple click.
    Lines { anchor: usize },
    /// Moving the view with the scroll bar.
    Scrollbar,
    /// Pulling the divider on a pane's edge to make it wider or narrower —
    /// a sidebar against the middle, or one pane in the middle against the
    /// one before it.
    Divider {
        /// Which pane, by its place in the list. Held rather than looked up
        /// again, because a drag that started on one divider must not jump to
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

/// What the language servers would do about the problem under the cursor,
/// asked for before anybody asks for it.
///
/// This is the whole of "you have not imported that" being something you can
/// see rather than something you have to go looking for — and it is four
/// pieces of state that only mean anything together: an answer, the spot it is
/// an answer *about*, when to ask, and how many times we have asked and been
/// turned away. An answer kept after the cursor has moved is a fix for
/// somewhere else, offered for here.
#[derive(Default)]
pub struct Fixes {
    /// What came back, gathering as the servers answer.
    pub found: Option<Gathered>,
    /// Where it is about, so the same question is not asked twice for a cursor
    /// that has not moved — and so an answer that arrives late can be told it
    /// is about somewhere nobody is standing any more.
    at: Option<(DocId, usize)>,
    /// When to ask, having waited for the cursor to stop. Walking along a line
    /// of red would otherwise be one request per character.
    due: Option<Instant>,
    /// How many times we have asked about this spot and been turned away. A
    /// server that is still catching up answers "content modified" rather than
    /// answering, and the first ask after a file opens nearly always is.
    tries: u8,
}

impl Fixes {
    /// Whether what is held is about this spot.
    fn about(&self, doc: DocId, at: usize) -> bool {
        self.at == Some((doc, at))
    }

    /// Start again, about somewhere else: nothing found, nothing asked, and
    /// no answer from before carried over to a place it was not about.
    fn now_about(&mut self, doc: DocId, at: usize) {
        self.found = None;
        self.at = Some((doc, at));
        self.tries = 0;
    }

    /// Whether what is held is about this file at all — asked when the file
    /// changes under it, since a fix worked out against text that has been
    /// edited is a fix for a file that no longer exists.
    fn is_about_doc(&self, doc: DocId) -> bool {
        self.at.is_some_and(|(held, _)| held == doc)
    }

    /// Forget it entirely — the file changed under it.
    fn forget(&mut self) {
        self.found = None;
        self.at = None;
        self.tries = 0;
    }
}

/// Where things ended up on the screen, so that a click is answered by what is
/// actually there rather than by working out where it ought to be.
///
/// Filled in by the drawing, every frame, and read by the mouse. They are one
/// thing because they are forgotten as one: three lists written by two
/// different pieces of the drawing, and one of them left over from last frame
/// is a click that goes to the tab that used to be in that column.
#[derive(Default)]
pub struct Hits {
    /// Each tab: where it is, which buffer it is for, and whether that spot is
    /// its close cross.
    pub tabs: Vec<(Rect, DocId, bool)>,
    /// The ‹ › at the ends of the tab row, and where each one scrolls to.
    /// Answered before the tabs, since an arrow sits on top of the tab it
    /// borrowed its column from.
    pub nudges: Vec<(Rect, u16)>,
    /// The parts of the status bar, every one of which is a button.
    pub status: Vec<(Rect, Cmd)>,
}

impl Hits {
    /// What was on the screen is about to stop being true.
    pub fn forget(&mut self) {
        self.tabs.clear();
        self.nudges.clear();
        self.status.clear();
    }
}

/// What you are recording, and what you recorded.
///
/// One macro, not a keyboard full of them. The overwhelming majority of what
/// anybody records is "this, and now the same thing forty more times",
/// recorded and played within the minute — and a register to name is one more
/// thing to remember about a macro that will not outlive the hour.
#[derive(Default)]
struct Recorder {
    /// What is being taken down, while something is. See [`Recorded`].
    taking: Option<Vec<Recorded>>,
    /// The last thing recorded, waiting to be played again.
    kept: Vec<Recorded>,
    /// Whether it is playing. A macro with "play the macro" in the middle of
    /// it is a loop with no way out and an editor that has stopped answering
    /// the keyboard — and one recorded through a plugin could ask for exactly
    /// that, so it is refused here rather than assumed impossible.
    playing: bool,
}

impl Recorder {
    /// Whether something is being taken down.
    fn on(&self) -> bool {
        self.taking.is_some()
    }

    /// Start taking one down, throwing away the last.
    fn start(&mut self) {
        self.taking = Some(Vec::new());
    }

    /// Stop, and keep what was taken down. Answers how many steps it was, or
    /// `None` where nothing was being recorded.
    fn stop(&mut self) -> Option<usize> {
        let steps = self.taking.take()?;
        let n = steps.len();
        if n > 0 {
            self.kept = steps;
        }
        Some(n)
    }

    /// Take one thing down, if anything is being taken down.
    fn remember(&mut self, step: Recorded) {
        if let Some(steps) = &mut self.taking {
            steps.push(step);
        }
    }

    /// What there is to play.
    fn kept(&self) -> &[Recorded] {
        &self.kept
    }
}

/// One thing a macro remembers.
///
/// Commands and characters rather than keystrokes. A key is a fact about your
/// settings and a command is a fact about the editor, so a macro recorded
/// today goes on meaning the same thing after a key has been rebound — and one
/// recorded through the palette remembers what was chosen rather than the
/// eight letters typed to find it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Recorded {
    Did(Cmd),
    Typed(char),
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
    /// The debug adapter, and where the program it is running has got to.
    pub debug: crate::dap::Debugger,
    /// The buffer the debugger's panel is in, while it is open.
    debug_panel: Option<DocId>,
    /// What to do when the build that is running finishes, while one is.
    /// `None` means nothing is waiting on it. See [`AfterBuild`].
    after_build: Option<AfterBuild>,
    /// Everything the last build printed. See [`Built`].
    last_build: Option<Built>,
    /// The plugins that are programs rather than tables.
    pub hosts: Hosts,
    /// A plugin waiting on a box that is on the screen.
    plugin_waiting: Option<Asked>,
    /// Where the caret was last drawn, for anything that opens beside it.
    pub caret: Option<(u16, u16)>,
    tx: Sender<Event>,
    /// The two buffers of a plugin's settings, while they are open: what the
    /// plugin ships, and what you say about it.
    ///
    /// Held as a pair because they are one thing to look at and one thing to
    /// close. Leaving the manifest behind after you have shut your own
    /// settings would be leaving half a comparison on the screen, and it is a
    /// buffer nobody can do anything with on its own.
    settings_pair: Option<(DocId, DocId)>,
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
    /// Where the pointer is, when the terminal has told us. Kept rather than
    /// acted on and forgotten, because what is *under* the pointer is drawn
    /// every frame — see [`App::panel_action_under`] — and a panel redrawn
    /// while somebody rests on one of its buttons must not lose the highlight
    /// under their hand.
    pub pointer: Option<(u16, u16)>,
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

    /// Where everything the mouse can hit ended up. See [`Hits`].
    pub hits: Hits,
    /// How far along the row of tabs the visible part starts, in columns. More
    /// files than fit across a terminal is the ordinary case once you have
    /// been working for an hour, so the row scrolls.
    pub tab_scroll: u16,

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
    /// What could be done about the problem under the cursor. See [`Fixes`].
    pub fixes: Fixes,
    /// What every server offered to do about the selection, gathering as they
    /// answer, for the list somebody asked for by hand.
    offer: Option<Gathered>,
    /// A save waiting on the servers' own fixes and on the formatter.
    before_save: Option<BeforeSave>,
    /// When to ask the servers for the things that are about the file rather
    /// than about the cursor: its colours, its inlay hints, its lenses, and —
    /// for a server that waits to be asked — what is wrong with it. One timer
    /// for the four of them, set by an edit and by opening a file, so that
    /// typing a word is one round of questions rather than five.
    lsp_extras_due: Option<Instant>,
    /// When to ask which other places in the file are the same thing as the
    /// one under the cursor. After a pause, because walking a line with the
    /// arrow keys would otherwise be a question per character.
    highlights_due: Option<Instant>,
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

    /// What you are recording, and what you recorded. See [`Recorder`].
    recorder: Recorder,

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
        let debug = crate::dap::Debugger::new(tx.clone());
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
            settings_pair: None,
            checked_for_updates: false,
            hover: None,
            signature: None,
            status: Status::quiet(),
            lsp,
            debug,
            debug_panel: None,
            after_build: None,
            last_build: None,
            hosts,
            plugin_waiting: None,
            unsettled: false,
            installing: None,
            recorder: Recorder::default(),
            caret: None,
            tx,
            clipboard: String::new(),
            last_search: String::new(),
            project,
            pointer: None,
            git: Tracker::default(),
            files: None,
            files_walking: false,
            quit: false,
            mouse_on: config.mouse(),
            screen: Rect::new(0, 0, 80, 24),
            hits: Hits::default(),
            tab_scroll: 0,
            drag: None,
            last_click: None,
            resting: None,
            completion_due: None,
            selection_due: None,
            selection_told: None,
            fixes: Fixes::default(),
            offer: None,
            before_save: None,

            lsp_extras_due: None,
            highlights_due: None,

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
                // A file that could not be read as text opens anyway — you
                // asked for it, and looking at one is a reasonable thing to
                // want — but it says so, because what is on the screen is not
                // what is in the file and the only other clue is a scattering
                // of replacement characters.
                let unreadable = doc.bytes.label();
                self.docs.push(doc);
                self.show(id);
                // The empty buffer nobody typed in is not worth keeping once
                // there is a real file to look at.
                self.drop_untouched_scratch(id);
                self.session_changed();
                if missing {
                    self.say(format!("{} is new", short(&path, &self.project)));
                } else if let Some(why) = unreadable {
                    self.say_bad(format!(
                        "{} is {why} — shown as best it can be, and read-only",
                        short(&path, &self.project)
                    ));
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
        // Never into a sidebar, and never into a pane that is pinned to one
        // buffer — the manifest half of a plugin's settings shows the manifest
        // and nothing else.
        let here = self.panes.get(self.focus);
        let refuses = here.is_some_and(|pane| {
            (pane.dock.is_some() && self.doc(id).is_none_or(|d| d.panel.is_none()))
                || (pane.pinned && pane.doc != id)
        });
        if refuses && let Some(at) = self.somewhere_to_open() {
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
            .filter_map(|pane| {
                let panel = self.doc(pane.doc)?.panel.as_ref()?;
                // A plugin's panel only. The debugger's comes back when there
                // is something to debug, and a panel saying "nothing is being
                // debugged" put back on every start is a sidebar nobody asked
                // for.
                panel.owner.plugin()?;
                Some(panel.id.clone())
            })
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
        // Half a comparison is not worth leaving on the screen, so closing
        // either buffer takes the arrangement down with it. `close_settings_panes`
        // has already cleared the pair by the time it comes back round here,
        // so the second close is an ordinary one.
        if self.is_settings_half(id) {
            self.close_settings_panes();
        }
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
            .and_then(|p| Some((p.owner.plugin()?.to_string(), p.id.clone())))
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
            // A docked pane exists to show one panel, and when that panel has
            // gone there is nothing for it to be. Sending it to "whatever was
            // looked at most recently" turns a sidebar into a second, sideways
            // copy of the file you are editing — which is what closing the
            // debugger's panel used to do.
            self.panes.retain(|pane| pane.dock.is_none() || pane.doc != id);
            self.focus = self.focus.min(self.panes.len().saturating_sub(1));
            // The rest move to whatever was looked at most recently.
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
        // A breakpoint lives with its buffer, so closing the buffer takes it
        // away — and the adapter has to hear about that or it goes on
        // stopping in a file you have closed, which looks like a debugger
        // stopping in a file you are not even looking at.
        self.tell_debugger_about_breakpoints();
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
            || self.fixes.due.is_some()
            || self.highlights_due.is_some()
            || self.lsp_extras_due.is_some()
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
        if now_due(&mut self.completion_due) {
            self.ask_for_completions(None, false);
        }
        if now_due(&mut self.selection_due) {
            self.tell_plugins_where_the_cursor_is();
        }
        if let Some((since, column, row)) = self.resting
            && since.elapsed() >= HOVER_DELAY
        {
            self.resting = None;
            self.hover_at_screen(column, row);
        }
        self.check_fixes();
        self.check_highlights();
        self.check_lsp_extras();
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
            .hits.nudges
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
        if !self.fixes.about(id, at) {
            // The cursor has moved, so last time's answer is about somewhere
            // else. Ask again once it stops.
            self.fixes.now_about(id, at);
            let on_a_problem = self
                .doc(id)
                .is_some_and(|d| d.diagnostics.iter().any(|p| p.range.contains(at)));
            self.fixes.due = on_a_problem.then(|| Instant::now() + FIX_DELAY);
            // The cursor is somewhere else, so the words lit up as being the
            // same thing as the one it was on are about the word before.
            if let Some(doc) = self.doc_mut(id) {
                doc.said.highlights.clear();
            }
            self.highlights_due = Some(Instant::now() + FIX_DELAY);
            return;
        }
        if !now_due(&mut self.fixes.due) {
            return;
        }
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
            self.fixes.found = Some(Gathered::new(id, at, asked));
        }
    }

    /// Ask the servers everything that is about the file rather than about
    /// where the cursor is: the colours only a server can know, the types the
    /// code does not say, the notes about each line, and the problems — from
    /// the servers that wait to be asked for those rather than volunteering
    /// them.
    ///
    /// After a pause, because every one of them is a question about the whole
    /// file and typing a word would otherwise be five of each.
    fn ask_the_servers_about_this_file(&mut self) {
        self.lsp_extras_due = Some(Instant::now() + EXTRAS_DELAY);
    }

    fn check_lsp_extras(&mut self) {
        if !now_due(&mut self.lsp_extras_due) {
            return;
        }
        let want_hints = self.config.inlay_hints();
        let want_lenses = self.config.code_lenses();
        let id = self.view().doc;
        let App { docs, lsp, .. } = self;
        let Some(doc) = docs.iter().find(|d| d.id == id) else {
            return;
        };
        if doc.path.is_none() {
            // Nothing a server has ever heard of.
            return;
        }
        lsp.semantic_tokens(doc);
        lsp.pull_diagnostics(doc);
        if want_hints {
            lsp.inlay_hints(doc);
        }
        if want_lenses {
            lsp.lenses(doc);
        }
    }

    fn check_highlights(&mut self) {
        if now_due(&mut self.highlights_due) {
            self.ask_for_highlights();
        }
    }

    /// Ask where else in this file the thing under the cursor is mentioned,
    /// once the cursor has come to rest.
    fn ask_for_highlights(&mut self) {
        let id = self.view().doc;
        let at = self.view().cursor();
        let App { docs, lsp, .. } = self;
        let Some(doc) = docs.iter().find(|d| d.id == id) else {
            return;
        };
        lsp.highlights(doc, at);
    }

    /// Who calls the thing under the cursor, or what it calls.
    fn ask_calls(&mut self, incoming: bool) {
        let at = self.view().cursor();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.prepare_calls(doc, at, incoming).is_none() {
            self.say("no language server here that knows what calls what");
        }
    }

    /// Run the note a server put on this line — "Run test", "3 references".
    fn run_code_lens(&mut self) {
        let at = self.view().cursor();
        let id = self.view().doc;
        let Some(doc) = self.doc(id) else { return };
        let line = text::line_of(&doc.rope, at);
        let found = doc
            .said
            .lenses
            .iter()
            .find(|lens| text::line_of(&doc.rope, lens.at) == line)
            .cloned();
        let Some(lens) = found else {
            return self.say("nothing on this line to run");
        };
        let Some(command) = lens.command else {
            return self.say(format!("{} is a note rather than a button", lens.label));
        };
        let App { docs, lsp, .. } = self;
        let Some(doc) = docs.iter().find(|d| d.id == id) else {
            return;
        };
        let Some(server) = lsp.who_can(doc, "executeCommandProvider") else {
            return self.say("no language server that can run that");
        };
        lsp.execute(server, &command);
        self.say(format!("{}…", lens.label));
    }

    /// Ask again after being turned away, up to a point.
    ///
    /// A few times rather than for ever: a server that will not answer this
    /// question is a server we should stop asking, and the cost of being wrong
    /// about that is one code action nobody was told about.
    fn retry_fixes(&mut self, doc: DocId, at: usize) {
        if !self.fixes.about(doc, at) || self.fixes.tries >= FIX_TRIES {
            return;
        }
        self.fixes.tries += 1;
        self.fixes.due = Some(Instant::now() + FIX_DELAY * 2);
    }

    fn take_quick_fixes(&mut self, server: ServerId, doc: DocId, at: usize, value: Value) {
        // Anything that came back about somewhere the cursor has since left is
        // an answer to a question nobody is asking any more.
        if !self.fixes.about(doc, at) {
            return;
        }
        let Some(gathered) = self.fixes.found.as_mut().filter(|g| g.doc == doc && g.at == at) else {
            return;
        };
        gathered.take(server, value);
        if gathered.is_empty() && gathered.settled() {
            // Nobody had anything. Better to have nothing waiting than an
            // empty list the status bar has to describe.
            self.fixes.forget();
        }
    }

    /// Do the obvious thing about the problem under the cursor.
    ///
    /// One fix means one keystroke: the import goes in and you carry on
    /// typing, which is the whole point and the reason nobody should have to
    /// scroll to the top of a file to add a line they already know the text
    /// of. Several means a list, because there is a choice to make.
    fn fix_it(&mut self) {
        let Some(fixes) = self.fixes.found.as_ref().filter(|g| !g.is_empty()) else {
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
            self.fixes.forget();
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
            Event::Dap(id, message) => self.on_dap(id, message),
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
            Event::ToReplace(what) => self.ask_before_replacing(*what),
            Event::Blamed(said) => self.say(said),
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
        // Back on the keyboard, so nothing is under the pointer any more as
        // far as anybody is concerned. A terminal never says the pointer has
        // left the window, so without this a button stays lit under a hand
        // that moved to the keys ten minutes ago. Moving the mouse a single
        // cell brings it back.
        self.pointer = None;
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
        self.recorder.remember(Recorded::Typed(c));
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
        let mut breakpoints_moved = false;
        if let Some(doc) = self.doc_mut(id) {
            let carry = |at: usize| crate::doc::carried(at, &edits, len);
            let carry_range = |range| crate::doc::carried_range(range, &edits, len);
            for diagnostic in &mut doc.diagnostics {
                diagnostic.range = carry_range(diagnostic.range);
            }
            // And so do the breakpoints, for a stronger reason: a breakpoint
            // left on a line number while the code moved off it is a debugger
            // stopping somewhere you did not ask it to, which is an hour of
            // not believing your own program.
            let was = doc.breakpoint_lines();
            for at in &mut doc.breakpoints {
                *at = carry(*at);
            }
            doc.breakpoints.sort_unstable();
            doc.breakpoints.dedup();
            breakpoints_moved = doc.breakpoint_lines() != was;
            // And the bookmarks, for the ordinary reason: a bookmark is on a
            // piece of code rather than on a line number, and nobody tells it
            // when the code moves.
            for at in &mut doc.bookmarks {
                *at = carry(*at);
            }
            doc.bookmarks.sort_unstable();
            doc.bookmarks.dedup();
            // And everything a server worked out about the file, which is one
            // thing and moves as one. See [`crate::doc::Said::carry`].
            doc.said.carry(&edits, len);
        }
        // The adapter is running against the file as it was, so an edit that
        // moved a breakpoint has to be passed on or it goes on stopping at the
        // old line.
        //
        // Only when one actually moved. Not thrift: a panel is filled by
        // replacing its whole buffer, which arrives here as an edit like any
        // other — and telling the adapter about it would bring back a reply,
        // which refreshes the panel, which is an edit. The editor never gets
        // another keystroke. Asking whether anything moved is what makes that
        // impossible rather than merely unlikely.
        if breakpoints_moved && self.debug.is_running() {
            let where_ = self.breakpoints_now();
            self.debug.send_breakpoints(&where_);
        }

        let App { docs, lsp, hosts, .. } = self;
        if let Some(doc) = docs.iter().find(|d| d.id == id) {
            lsp.did_change(doc, &edits);
            hosts.changed(doc, &edits);
        }
        // What the servers worked out about this file is now about the file as
        // it was. Ask again, once the typing stops.
        self.ask_the_servers_about_this_file();
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
        // Everything but the two commands that work the recorder. A macro with
        // "stop recording" in it stops the recording it is played into, and
        // one with "play the macro" in it is a loop.
        if cmd != Cmd::RECORD_MACRO && cmd != Cmd::PLAY_MACRO {
            self.recorder.remember(Recorded::Did(cmd));
        }
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
        let found = self.restore_session(true);
        if found > 0 {
            self.say_good(format!("brought back {}", count("file", found)));
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
                owner: crate::doc::Owner::Plugin(command.plugin.clone()),
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
        let root = self.root_for(&path, &tool.roots);

        // The same placeholders a language server's settings may use, so that
        // a Python tool lands in the project's environment without any of that
        // being written into the editor as a special case, plus the one thing
        // a tool needs that a server does not: which file.
        let environment = self.lsp.environment_for(&root);
        let mut vars = crate::venv::Vars::new(&root, environment.as_ref());
        vars.set("file", path.display().to_string());
        if let Some(dir) = path.parent() {
            vars.set("file_dir", dir.display().to_string());
        }
        // The same names a debug adapter's launch arguments get, and for the
        // same reason: `cc -g -o ${file_stem} ${file}` is the whole of what a
        // build of one C file is, and the two halves of it are this file and
        // this file without its extension. A tool that never mentions them is
        // unaffected. See [`crate::dap::filled`].
        vars.set("file_stem", path.with_extension("").display().to_string());
        if let Some(base) = path.file_stem() {
            vars.set("file_base", base.to_string_lossy().into_owned());
        }
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

        // Taken before the answer is dealt with, because dealing with it moves
        // `done`, and because a build nobody is waiting on is an ordinary tool
        // run and should be reported as one.
        let waiting = tool.builds.then(|| self.after_build.take()).flatten();
        let built = done.ok;
        // And the whole of what a build printed is kept before anything reads
        // it for problems, because what the margin can hold is a fraction of
        // what a compiler says. See [`Built`].
        if tool.builds {
            self.last_build = Some(Built {
                name: tool.name.clone(),
                ok: done.ok,
                text: done.printed(),
                note: (!done.ok).then(|| self.build_note(tool, done.doc)).flatten(),
            });
        }

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
            Output::Problems => {
                let marked = self.take_tool_problems(tool, &done);
                // A build that failed and left no mark anywhere has said
                // nothing at all from where the person is sitting. What it
                // printed is the only account there is, so it is put in front
                // of them rather than left to be asked for — the asking is the
                // part nobody knows to do.
                if tool.builds && !done.ok && marked == 0 {
                    self.show_build_output();
                }
            }
            Output::Ignore => match done.ok {
                true => self.say_good(format!("{} finished", tool.name)),
                false => self.say_bad(match complaint.is_empty() {
                    true => format!("{} failed", tool.name),
                    false => format!("{}: {complaint}", tool.name),
                }),
            },
        }
        match (waiting, built) {
            // It compiled, so there is now something to debug. The message
            // the build left in the status line is replaced by the
            // debugger's, which is the more recent news.
            (Some(AfterBuild::Debug), true) => self.start_debugging(),
            // And when it did not, what it said is already in the margin or
            // already on the screen. All that is left to say is that the
            // debugger is not coming, and where the rest of it is.
            (Some(AfterBuild::Debug), false) => self.say_bad(format!(
                "{} failed, so there is nothing new to debug — build-output has all of it",
                tool.name
            )),
            _ => {}
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
    ///
    /// Answers how many marks it actually *placed*, which is not the same as
    /// how many it found: a problem about a file nobody has open has nowhere
    /// to go. The difference matters to whoever asked — a build that failed
    /// and put nothing anywhere visible has, from where the person is sitting,
    /// failed for no reason at all. See [`App::on_tool`].
    fn take_tool_problems(&mut self, tool: &'static Tool, done: &crate::tool::Finished) -> usize {
        let Some(pattern) = &tool.pattern else {
            self.say_bad(format!(
                "{} is set to find problems but says nothing about how to read them",
                tool.name
            ));
            return 0;
        };
        let told = crate::doc::Told::Tool(tool.id.as_str());
        // A tool sends its complete opinion every time, so its old findings go
        // and everybody else's stay — the same rule a language server gets.
        for doc in &mut self.docs {
            doc.diagnostics.retain(|d| d.told != told);
        }

        let printed = done.printed();
        let found = crate::tool::problems(pattern, &printed);
        let mut marked = 0;
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
            marked += 1;
        }
        match marked {
            0 if done.ok => self.say_good(format!("{}: nothing to report", tool.name)),
            // It failed, and nothing it said was in the shape a margin holds.
            // The first line is very often the whole story — `ld: undefined
            // reference to 'fizz'`, `make: *** [Makefile:2: main] Error 1` —
            // and is a better answer than a count of nothing.
            0 => match printed.lines().find(|line| !line.trim().is_empty()) {
                Some(first) => self.say_bad(format!("{}: {first}", tool.name)),
                None => self.say_bad(format!("{} failed and said nothing", tool.name)),
            },
            n => self.say(format!("{}: {}", tool.name, count("problem", n))),
        }
        marked
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
            // Folded, the way the debugger's panel is and for the same
            // reason. What lands in one of these is a line of prose — a
            // compiler's complaint with an absolute path in it, a traceback, a
            // test runner's account of itself — and a pane that cuts the
            // interesting half off the right-hand edge is a pane that sends
            // you to a terminal to read the thing you just asked for.
            self.view_mut().wrap = true;
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
        // What it is made of is decided on the bytes, before there are no
        // bytes left to ask. A file that has become something we cannot write
        // back makes the buffer read-only, in [`Document::took_from_disk`].
        let kind = crate::doc::Bytes::of(&bytes);
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
            doc.took_from_disk(stamp, kind);
            return Ok(false);
        }
        let len = doc.len_chars();
        let sel = crate::text::Selections::single(Range::point(0));
        let edits = doc.apply_atomic(vec![crate::doc::Change::replace(0, len, text)], &sel);
        doc.took_from_disk(stamp, kind);
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
            (n, 0) => self.say(format!("closed {}", count("buffer", n))),
            (n, k) => self.say(format!(
                "closed {}, kept {k} with unsaved changes",
                count("buffer", n)
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
        for (area, id, _) in &self.hits.tabs {
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
        // Both halves of a plugin's settings go together, whichever one you
        // were standing in when you asked.
        if self
            .panes
            .get(at)
            .is_some_and(|pane| self.is_settings_half(pane.doc))
            && self.close_settings_panes()
        {
            return;
        }
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

    /// A pane that will take whatever is being opened: not a sidebar, and not
    /// one pinned to a buffer of its own.
    ///
    /// Falls back to the first pane that is merely not a sidebar, because a
    /// screen with nowhere at all to put a file still has to put it
    /// somewhere.
    fn somewhere_to_open(&self) -> Option<usize> {
        let len = self.panes.len();
        let round = |from: usize| (0..len).map(move |step| (from + step) % len);
        round(self.focus)
            .find(|at| self.panes[*at].dock.is_none() && !self.panes[*at].pinned)
            .or_else(|| self.beside_the_docks())
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
            "inlay_hints" => {
                let on = !self.config.inlay_hints();
                self.config.inlay_hints = Some(on);
                if !on {
                    // Off means gone now, not gone the next time somebody
                    // asks a server something.
                    for doc in &mut self.docs {
                        doc.said.inlays.clear();
                    }
                }
                self.ask_the_servers_about_this_file();
                if on {
                    "showing the types the code does not say"
                } else {
                    "showing only what the code says"
                }
            }
            "code_lenses" => {
                let on = !self.config.code_lenses();
                self.config.code_lenses = Some(on);
                if !on {
                    for doc in &mut self.docs {
                        doc.said.lenses.clear();
                    }
                }
                self.ask_the_servers_about_this_file();
                if on {
                    "showing what the servers have to say about each line"
                } else {
                    "not showing the servers' notes"
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
/// `1 line`, `3 lines` — the number and the word together.
///
/// Together because they are never wanted apart, and because writing them
/// apart is exactly how a status line comes to say `1 lines`: the count is in
/// one place, the plural is worked out in another, and nothing holds the two
/// in step.
/// Whether a moment something was waiting for has come round, taking the wait
/// off if it has.
///
/// Every "ask once the typing stops" in the editor is a `Option<Instant>` and
/// the same four lines: is it set, is it time, clear it, do the thing. Written
/// once, so that the fifth one is a line rather than a fourth copy of an
/// if-let somebody could get subtly wrong.
fn now_due(when: &mut Option<Instant>) -> bool {
    let ready = when.is_some_and(|at| at <= Instant::now());
    if ready {
        *when = None;
    }
    ready
}

pub(crate) fn count(word: &'static str, n: usize) -> String {
    match n {
        1 => format!("{n} {word}"),
        _ => format!("{n} {word}s"),
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

// The rest of the module, in the pieces it falls into. Each one is a child
// rather than a neighbour, so what was private to `app` when this was one file
// is private to `app` still — and what one piece needs from another says so,
// which is the only thing that changed.
mod typing;
mod overlays;
pub(crate) use overlays::*;
mod find;
mod answers;
pub(crate) use answers::*;
mod mouse;
pub(crate) use mouse::*;
mod debug;
mod commands;
pub(crate) use commands::*;

#[cfg(test)]
mod tests;
