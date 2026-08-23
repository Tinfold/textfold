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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use serde_json::Value;

use crate::cmd::Cmd;
use crate::config::{Config, LineNumbers};
use crate::doc::{Diagnostic, DocId, Document, Indent, Severity};
use crate::edit::{self, Motion};
use crate::keys::{Key, Keys};
use crate::lang::{self, LangId};
use crate::lsp::{Ask, Goto, Incoming, ServerId, Servers};
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
}

/// How long the mouse has to sit still over a word before textfold asks what
/// it is. Long enough not to fire while you are moving across the screen,
/// short enough that you do not wonder whether it is going to.
const HOVER_DELAY: Duration = Duration::from_millis(400);

/// How long a message stays in the status line.
const MESSAGE_TIME: Duration = Duration::from_secs(6);

/// Two clicks closer together than this are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// How long to wait after a keystroke before asking for completions, so that
/// typing a word is one request rather than six.
const COMPLETION_DELAY: Duration = Duration::from_millis(120);

/// What is open over the editor.
pub enum Overlay {
    None,
    Picker(Picker),
    Prompt(Prompt),
    Confirm(Confirm),
    /// The keys and what they do, scrolled to this row.
    Help(usize),
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
}

/// One thing a language server offered to insert.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub label: String,
    pub detail: Option<String>,
    pub kind: &'static str,
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
}

/// The list of suggestions under the cursor.
pub struct Completion {
    pub doc: DocId,
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

/// Whether every letter of `needle` appears in `haystack`, in order.
fn subsequence(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|want| chars.any(|c| c == want))
}

/// A box of text floating over the editor: what something is, or what
/// arguments it takes.
pub struct Popup {
    pub lines: Vec<DocLine>,
    /// Where in the document it is about, so it can be drawn beside it and
    /// closed when you move away.
    pub at: usize,
    pub scroll: usize,
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
    Words { anchor_start: usize, anchor_end: usize },
    /// A line at a time, having started with a triple click.
    Lines { anchor: usize },
    /// Moving the view with the scroll bar.
    Scrollbar,
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
    /// Whether panes sit side by side. The other way is one above the other.
    pub side_by_side: bool,

    pub overlay: Overlay,
    pub completion: Option<Completion>,
    pub hover: Option<Popup>,
    pub signature: Option<Popup>,
    pub status: Status,

    pub lsp: Servers,
    tx: Sender<Event>,

    /// What was cut or copied. Kept here as well as handed to the terminal,
    /// because the terminal will not hand it back.
    pub clipboard: String,
    /// The last thing searched for, so that "find next" works after the search
    /// box has closed.
    pub last_search: String,
    /// Where files come from for the file picker, and where a project-wide
    /// search searches.
    pub project: PathBuf,
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
    /// The same for the status bar, whose parts are buttons.
    pub status_hits: Vec<(Rect, Cmd)>,

    drag: Option<Drag>,
    last_click: Option<(Instant, u16, u16, u8)>,
    /// Where the mouse has been sitting still, and since when.
    resting: Option<(Instant, u16, u16)>,
    /// When to ask for completions, having waited for typing to stop.
    completion_due: Option<Instant>,
    /// A document waiting to be written once the formatter answers. Formatting
    /// on save is a round trip to a language server, so the save cannot happen
    /// until the edits are in — saving first would write the old text and
    /// leave the reformatted text unsaved.
    save_after_format: Option<DocId>,
}

impl App {
    pub fn new(config: Config, tx: Sender<Event>) -> Self {
        lang::init();
        let themes = Themes::load();
        let theme = themes
            .by_name(config.theme_name())
            .unwrap_or(crate::theme::FALLBACK);
        let keys = Keys::new(&config.keys);
        let lsp = Servers::new(tx.clone());
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
            side_by_side: true,
            overlay: Overlay::None,
            completion: None,
            hover: None,
            signature: None,
            status: Status::quiet(),
            lsp,
            tx,
            clipboard: String::new(),
            last_search: String::new(),
            project,
            files: None,
            files_walking: false,
            quit: false,
            mouse_on: config.mouse(),
            screen: Rect::new(0, 0, 80, 24),
            tab_hits: Vec::new(),
            status_hits: Vec::new(),
            drag: None,
            last_click: None,
            resting: None,
            completion_due: None,
            save_after_format: None,
            config,
        };
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

    fn here_mut(&mut self) -> &mut Document {
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
        let untitled = self
            .docs
            .iter()
            .filter(|d| d.path.is_none())
            .count();
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
                if missing {
                    self.say(format!("{} is new", short(&path, &self.project)));
                }
                self.lsp_open(id);
            }
            Err(e) => self.say_bad(format!("{e}")),
        }
    }

    /// Show a document in the focused pane.
    fn show(&mut self, id: DocId) {
        self.touch(id);
        let selections = self
            .panes
            .iter()
            .find(|p| p.doc == id)
            .map(|p| p.sel.clone())
            .unwrap_or_default();
        let wrap = self.view().wrap;
        let at = self.focus.min(self.panes.len() - 1);
        self.panes[at].show(id, selections);
        self.panes[at].wrap = wrap;
        self.dismiss_popups();
        self.lsp_open(id);
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
        }
        self.docs.retain(|d| d.id != id);
        self.seen.remove(&id);
        if self.docs.is_empty() {
            let fresh = self.new_scratch();
            for pane in &mut self.panes {
                pane.show(fresh, Selections::default());
            }
            return;
        }
        // Panes showing it move to whatever was looked at most recently.
        let fallback = self.most_recent().unwrap_or(self.docs[0].id);
        for pane in &mut self.panes {
            if pane.doc == id {
                pane.show(fallback, Selections::default());
            }
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
        if self.completion_due.is_some() || self.resting.is_some() || self.status.showing() {
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
        if let Some((since, column, row)) = self.resting
            && since.elapsed() >= HOVER_DELAY
        {
            self.resting = None;
            self.hover_at_screen(column, row);
        }
    }

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Term(TermEvent::Key(key)) => self.on_key(key),
            Event::Term(TermEvent::Mouse(mouse)) => self.on_mouse(mouse),
            Event::Term(TermEvent::Paste(text)) => self.on_paste(&text),
            Event::Term(TermEvent::Resize(width, height)) => {
                self.screen = Rect::new(0, 0, width, height);
            }
            Event::Term(_) => {}
            Event::Lsp(id, message) => self.on_lsp(id, message),
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
        if key.as_typed().is_none() && self.keys.lookup(key) == Some(Cmd::OpenPath) {
            return self.open_prompt(PromptKind::OpenPath);
        }

        match &mut self.overlay {
            Overlay::Picker(_) => return self.picker_key(key),
            Overlay::Prompt(_) => return self.prompt_key(key),
            Overlay::Confirm(_) => return self.confirm_key(key),
            Overlay::Help(_) => return self.help_key(key),
            Overlay::None => {}
        }

        // The completion list gets first refusal on the handful of keys that
        // steer it, and nothing else.
        if self.completion.is_some() && self.completion_key(key) {
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

        // Typing keeps the completion list, narrowing it; anything else
        // closes it.
        let typed = self.typed_since_completion();
        match (&mut self.completion, typed) {
            (Some(completion), Some(prefix)) => {
                completion.narrow(&prefix);
                if completion.is_empty() {
                    self.completion = None;
                }
            }
            (Some(_), None) => self.completion = None,
            (None, _) => {}
        }

        if self.config.auto_completion() && self.lsp.primary_for(self.here()).is_some() {
            let triggers = self
                .lsp
                .primary_for(self.here())
                .and_then(|id| self.lsp.get(id))
                .map(|s| s.completion_triggers())
                .unwrap_or_default();
            if triggers.contains(&c) {
                self.completion = None;
                self.ask_for_completions(Some(c), false);
            } else if self.completion.is_none()
                && (c.is_alphanumeric() || c == '_')
            {
                // Wait for typing to stop, so a word is one request.
                self.completion_due = Some(Instant::now() + COMPLETION_DELAY);
            }
        }

        let signature_triggers = self
            .lsp
            .primary_for(self.here())
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
        if edits.is_empty() {
            return;
        }
        let id = self.view().doc;
        let len = self.doc(id).map(Document::len_chars).unwrap_or(0);

        // Every other pane looking at this document has cursors that were
        // pointing at text that has moved.
        let focus = self.focus.min(self.panes.len() - 1);
        for (at, pane) in self.panes.iter_mut().enumerate() {
            if at != focus && pane.doc == id {
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

        let App { docs, lsp, .. } = self;
        if let Some(doc) = docs.iter().find(|d| d.id == id) {
            lsp.did_change(doc, &edits);
        }
        if let Some(doc) = self.doc_mut(id) {
            doc.take_pending();
        }
        self.hover = None;
        self.scroll_into_view();
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

    pub fn run(&mut self, cmd: Cmd) {
        if cmd.writes() && self.refuse_if_read_only() {
            return;
        }
        if cmd.breaks_undo() {
            self.here_mut().close_revision();
        }
        if !matches!(cmd, Cmd::Completion) {
            self.completion = None;
            self.completion_due = None;
        }

        let tab_width = self.config.tab_width();
        match cmd {
            // ---- Moving ----
            Cmd::MoveLeft => self.motion(Motion::Left, false),
            Cmd::MoveRight => self.motion(Motion::Right, false),
            Cmd::MoveUp => self.motion(Motion::Up, false),
            Cmd::MoveDown => self.motion(Motion::Down, false),
            Cmd::MoveWordLeft => self.motion(Motion::WordLeft, false),
            Cmd::MoveWordRight => self.motion(Motion::WordRight, false),
            Cmd::MoveLineStart => self.motion(Motion::LineStart, false),
            Cmd::MoveLineEnd => self.motion(Motion::LineEnd, false),
            Cmd::MovePageUp => self.motion(Motion::PageUp, false),
            Cmd::MovePageDown => self.motion(Motion::PageDown, false),
            Cmd::MoveDocStart => self.motion(Motion::DocStart, false),
            Cmd::MoveDocEnd => self.motion(Motion::DocEnd, false),
            Cmd::MoveParaUp => self.motion(Motion::ParaUp, false),
            Cmd::MoveParaDown => self.motion(Motion::ParaDown, false),
            Cmd::ExtendLeft => self.motion(Motion::Left, true),
            Cmd::ExtendRight => self.motion(Motion::Right, true),
            Cmd::ExtendUp => self.motion(Motion::Up, true),
            Cmd::ExtendDown => self.motion(Motion::Down, true),
            Cmd::ExtendWordLeft => self.motion(Motion::WordLeft, true),
            Cmd::ExtendWordRight => self.motion(Motion::WordRight, true),
            Cmd::ExtendLineStart => self.motion(Motion::LineStart, true),
            Cmd::ExtendLineEnd => self.motion(Motion::LineEnd, true),
            Cmd::ExtendPageUp => self.motion(Motion::PageUp, true),
            Cmd::ExtendPageDown => self.motion(Motion::PageDown, true),
            Cmd::ExtendDocStart => self.motion(Motion::DocStart, true),
            Cmd::ExtendDocEnd => self.motion(Motion::DocEnd, true),

            Cmd::ScrollUp => self.scroll(-3),
            Cmd::ScrollDown => self.scroll(3),
            Cmd::CentreCursor => self.centre(),
            Cmd::MatchBracket => self.go_to_matching_bracket(),
            Cmd::GotoLine => self.open_prompt(PromptKind::GotoLine),
            Cmd::JumpBack => self.jump(false),
            Cmd::JumpForward => self.jump(true),

            // ---- Selecting ----
            Cmd::SelectAll => {
                let (doc, view) = self.pair();
                edit::select_all(doc, view);
            }
            Cmd::SelectLine => {
                let (doc, view) = self.pair();
                edit::select_line(doc, view);
                self.scroll_into_view();
            }
            Cmd::SelectWord => {
                let (doc, view) = self.pair();
                edit::select_word(doc, view);
            }
            Cmd::ExpandSelection => self.expand_selection(),
            Cmd::AddCursorAbove => {
                let (doc, view) = self.pair();
                edit::add_cursor_vertically(doc, view, tab_width, false);
                self.scroll_into_view();
            }
            Cmd::AddCursorBelow => {
                let (doc, view) = self.pair();
                edit::add_cursor_vertically(doc, view, tab_width, true);
                self.scroll_into_view();
            }
            Cmd::AddCursorNextMatch => {
                let (doc, view) = self.pair();
                let found = edit::add_cursor_next_match(doc, view);
                if !found {
                    self.say("no more of those");
                } else {
                    self.scroll_into_view();
                }
            }
            Cmd::SelectAllMatches => {
                let (doc, view) = self.pair();
                let count = edit::select_all_matches(doc, view);
                if count > 1 {
                    self.say(format!("{count} cursors"));
                }
            }
            Cmd::CursorsToLineEnds => {
                let (doc, view) = self.pair();
                edit::cursors_to_line_ends(doc, view);
            }
            Cmd::CollapseCursors => {
                self.view_mut().sel.collapse_to_primary();
                self.scroll_into_view();
            }

            // ---- Changing text ----
            Cmd::InsertNewline => {
                let (doc, view) = self.pair();
                let mut edits = edit::newline(doc, view, tab_width);
                edits.extend(edit::newline_closing(doc, view, tab_width));
                self.after_edit(edits);
                self.completion = None;
            }
            Cmd::DeleteBackward => {
                let (doc, view) = self.pair();
                let edits = edit::delete_backward(doc, view, tab_width);
                self.after_edit(edits);
                self.narrow_or_close_completion();
            }
            Cmd::DeleteForward => {
                let (doc, view) = self.pair();
                let edits = edit::delete_forward(doc, view);
                self.after_edit(edits);
            }
            Cmd::DeleteWordBackward => {
                let (doc, view) = self.pair();
                let edits = edit::delete_word_backward(doc, view);
                self.after_edit(edits);
            }
            Cmd::DeleteWordForward => {
                let (doc, view) = self.pair();
                let edits = edit::delete_word_forward(doc, view);
                self.after_edit(edits);
            }
            Cmd::DeleteToLineStart => {
                let (doc, view) = self.pair();
                let edits = edit::delete_to_line_start(doc, view);
                self.after_edit(edits);
            }
            Cmd::DeleteToLineEnd => {
                let (doc, view) = self.pair();
                let edits = edit::delete_to_line_end(doc, view);
                self.after_edit(edits);
            }
            Cmd::DeleteLine => {
                let (doc, view) = self.pair();
                let edits = edit::delete_line(doc, view);
                self.after_edit(edits);
            }
            Cmd::DuplicateLine => {
                let (doc, view) = self.pair();
                let edits = edit::duplicate_line(doc, view);
                self.after_edit(edits);
            }
            Cmd::MoveLineUp => {
                let (doc, view) = self.pair();
                let edits = edit::move_lines(doc, view, false);
                self.after_edit(edits);
            }
            Cmd::MoveLineDown => {
                let (doc, view) = self.pair();
                let edits = edit::move_lines(doc, view, true);
                self.after_edit(edits);
            }
            Cmd::JoinLines => {
                let (doc, view) = self.pair();
                let edits = edit::join_lines(doc, view);
                self.after_edit(edits);
            }
            Cmd::Indent => self.on_tab(false),
            Cmd::Unindent => self.on_tab(true),
            Cmd::ToggleComment => {
                let (doc, view) = self.pair();
                match edit::toggle_comment(doc, view, tab_width) {
                    Some(edits) => self.after_edit(edits),
                    None => {
                        let name = lang::get(self.here().language).name.clone();
                        self.say(format!("textfold does not know how to comment {name}"));
                    }
                }
            }
            Cmd::UpperCase => {
                let (doc, view) = self.pair();
                let edits = edit::change_case(doc, view, true);
                self.after_edit(edits);
            }
            Cmd::LowerCase => {
                let (doc, view) = self.pair();
                let edits = edit::change_case(doc, view, false);
                self.after_edit(edits);
            }
            Cmd::Undo => self.undo(true),
            Cmd::Redo => self.undo(false),
            Cmd::Copy => self.copy(false),
            Cmd::Cut => self.copy(true),
            Cmd::Paste => {
                let text = self.clipboard.clone();
                if text.is_empty() {
                    self.say("nothing to paste");
                } else {
                    let (doc, view) = self.pair();
                    let edits = edit::insert_atomic(doc, view, &text);
                    self.after_edit(edits);
                }
            }

            // ---- Files ----
            Cmd::New => {
                let id = self.new_scratch();
                self.show(id);
            }
            Cmd::Open => self.open_files_picker(),
            Cmd::OpenPath => self.open_prompt(PromptKind::OpenPath),
            Cmd::Save => self.save(None),
            Cmd::SaveAs => self.open_prompt(PromptKind::SaveAs),
            Cmd::SaveAll => self.save_all(),
            Cmd::Reload => self.reload(),
            Cmd::Close => self.close(false),
            Cmd::CloseForce => self.close(true),
            Cmd::Quit => self.leave(false),
            Cmd::QuitForce => self.leave(true),
            Cmd::NextBuffer => self.step_buffer(1),
            Cmd::PrevBuffer => self.step_buffer(-1),
            Cmd::Buffers => self.open_buffers_picker(),

            // ---- Searching ----
            Cmd::Find => self.open_prompt(PromptKind::Find),
            Cmd::FindNext => self.find_step(1),
            Cmd::FindPrev => self.find_step(-1),
            Cmd::FindWordUnderCursor => {
                let at = self.view().cursor();
                match text::word_text_at(&self.here().rope, at) {
                    Some(word) => {
                        self.last_search = word;
                        self.find_step(1);
                    }
                    None => self.say("the cursor is not on a word"),
                }
            }
            Cmd::Replace => self.open_prompt(PromptKind::ReplaceFind),
            Cmd::Grep => self.open_grep_picker(),

            // ---- Language servers ----
            Cmd::Completion => self.ask_for_completions(None, true),
            Cmd::GotoDefinition => self.ask_goto(Goto::Definition),
            Cmd::GotoTypeDefinition => self.ask_goto(Goto::Type),
            Cmd::GotoImplementation => self.ask_goto(Goto::Implementation),
            Cmd::References => self.ask_references(),
            Cmd::Hover => self.ask_hover(self.view().cursor()),
            Cmd::Rename => self.start_rename(),
            Cmd::CodeAction => self.ask_code_actions(),
            Cmd::Format => self.format(),
            Cmd::Symbols => self.ask_symbols(),
            Cmd::WorkspaceSymbols => self.open_workspace_symbols(),
            Cmd::Diagnostics => self.open_diagnostics_picker(),
            Cmd::NextDiagnostic => self.step_diagnostic(1),
            Cmd::PrevDiagnostic => self.step_diagnostic(-1),
            Cmd::SignatureHelp => {
                let at = self.view().cursor();
                let (doc, lsp) = self.doc_and_lsp();
                if lsp.signature(doc, at).is_none() {
                    self.say("no language server here");
                }
            }
            Cmd::RestartServers => {
                self.lsp.restart();
                let docs: Vec<DocId> = self.docs.iter().map(|d| d.id).collect();
                for id in docs {
                    self.lsp_open(id);
                }
                self.say("starting the language servers again");
            }
            Cmd::ServerStatus => self.show_server_status(),

            // ---- The view ----
            Cmd::CommandPalette => self.open_commands_picker(),
            Cmd::Split => self.split(),
            Cmd::ClosePane => self.close_pane(),
            Cmd::FocusNextPane => self.focus_pane(1),
            Cmd::FocusPrevPane => self.focus_pane(-1),
            Cmd::SwapSplitDirection => {
                self.side_by_side = !self.side_by_side;
            }
            Cmd::ThemePicker => self.open_theme_picker(),
            Cmd::NextTheme => self.step_theme(1),
            Cmd::PrevTheme => self.step_theme(-1),
            Cmd::ToggleLineNumbers => self.toggle_setting("line_numbers"),
            Cmd::ToggleRelativeNumbers => self.toggle_setting("relative_numbers"),
            Cmd::ToggleWrap => {
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
            Cmd::ToggleWhitespace => self.toggle_setting("show_whitespace"),
            Cmd::ToggleMouse => self.toggle_setting("mouse"),
            Cmd::SetLanguage => self.open_language_picker(),
            Cmd::Settings => self.open_settings_picker(),

            // ---- Getting out ----
            Cmd::Escape => self.escape(),
            Cmd::Help => self.overlay = Overlay::Help(0),
            Cmd::About => self.say(format!(
                "textfold {} — {} languages, {} themes",
                env!("CARGO_PKG_VERSION"),
                lang::names().len(),
                self.themes.entries.len()
            )),
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

    fn narrow_or_close_completion(&mut self) {
        let typed = self.typed_since_completion();
        match (&mut self.completion, typed) {
            (Some(completion), Some(prefix)) => {
                completion.narrow(&prefix);
                if completion.is_empty() {
                    self.completion = None;
                }
            }
            (Some(_), None) => self.completion = None,
            _ => {}
        }
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
        self.say(format!(
            "{} {count} characters",
            if cut { "cut" } else { "copied" }
        ));
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
            _ => {}
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
    /// Formatting is a round trip to a language server, so this may not write
    /// anything itself: it asks, and [`App::take_format`] writes when the
    /// edits arrive. That is also why [`App::write_now`] is separate — the
    /// save that follows a format must not ask for another one.
    fn save(&mut self, to: Option<PathBuf>) {
        if self.config.format_on_save() && self.save_after_format.is_none() {
            let id = self.view().doc;
            let tab_width = self.config.tab_width();
            let spaces = matches!(self.here().indent, Indent::Spaces(_));
            let (doc, lsp) = self.doc_and_lsp();
            if doc.path.is_some() && lsp.format(doc, tab_width, spaces).is_some() {
                self.save_after_format = Some(id);
                return;
            }
        }
        self.write_now(to);
    }

    fn write_now(&mut self, to: Option<PathBuf>) {
        let id = self.view().doc;
        let path = match to.or_else(|| self.doc(id).and_then(|d| d.path.clone())) {
            Some(path) => path,
            None => return self.open_prompt(PromptKind::SaveAs),
        };
        self.save_after_format = None;
        if self.config.trim_trailing_whitespace() {
            self.trim_trailing_whitespace();
        }

        let final_newline = self.config.final_newline();
        let Some(doc) = self.doc_mut(id) else { return };
        match doc.save_to(&path, final_newline) {
            Ok(()) => {
                let name = doc.name.clone();
                let lines = doc.len_lines();
                let App { docs, lsp, .. } = self;
                if let Some(doc) = docs.iter().find(|d| d.id == id) {
                    lsp.did_save(doc);
                    // A buffer that has just been given a name is a buffer a
                    // language server has never heard of.
                    lsp.open(doc);
                }
                self.say_good(format!("saved {name}, {lines} lines"));
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
            let Some(doc) = self.doc_mut(id) else { continue };
            let Some(path) = doc.path.clone() else { continue };
            if let Err(e) = doc.save_to(&path, final_newline) {
                failed.push(format!("{e}"));
                continue;
            }
            let App { docs, lsp, .. } = self;
            if let Some(doc) = docs.iter().find(|d| d.id == id) {
                lsp.did_save(doc);
            }
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
        let Some(path) = self.doc(id).and_then(|d| d.path.clone()) else {
            return self.say("this buffer has no file to read");
        };
        let indent = self.default_indent();
        match Document::open(id, &path, indent) {
            Ok(fresh) => {
                let len = fresh.len_chars();
                if let Some(at) = self.docs.iter().position(|d| d.id == id) {
                    self.docs[at] = fresh;
                }
                for pane in &mut self.panes {
                    if pane.doc == id {
                        pane.sel.clamp(len);
                    }
                }
                self.say_good("read again from disk");
            }
            Err(e) => self.say_bad(format!("{e}")),
        }
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

    // ---- Panes ----

    fn split(&mut self) {
        if self.panes.len() >= 4 {
            return self.say("four panes is as many as fit");
        }
        let mut copy = View::new(self.view().doc, self.view().wrap);
        copy.sel = self.view().sel.clone();
        copy.top = self.view().top;
        let at = self.focus.min(self.panes.len() - 1);
        self.panes.insert(at + 1, copy);
        self.focus = at + 1;
    }

    fn close_pane(&mut self) {
        if self.panes.len() < 2 {
            return self.say("that is the only pane");
        }
        let at = self.focus.min(self.panes.len() - 1);
        self.panes.remove(at);
        self.focus = at.min(self.panes.len() - 1);
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
                if off { "line numbers on" } else { "line numbers off" }
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
                if on { "showing spaces and tabs" } else { "not showing spaces and tabs" }
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
                if on { "long lines fold" } else { "long lines run off the side" }
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
                if on { "brackets close themselves" } else { "brackets are yours to close" }
            }
            "format_on_save" => {
                let on = !self.config.format_on_save();
                self.config.format_on_save = Some(on);
                if on { "formatting on save" } else { "not formatting on save" }
            }
            "spaces" => {
                let on = !self.config.spaces();
                self.config.spaces = Some(on);
                if on { "new files use spaces" } else { "new files use tabs" }
            }
            "trim_trailing_whitespace" => {
                let on = !self.config.trim_trailing_whitespace();
                self.config.trim_trailing_whitespace = Some(on);
                if on { "trailing spaces go on save" } else { "trailing spaces stay" }
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
            PromptKind::Rename => text::word_text_at(&self.here().rope, self.view().cursor())
                .unwrap_or_default(),
            // A search box opens with the last search in it, selected in the
            // sense that typing replaces it — which here means the caret is at
            // the end and Ctrl-U clears it.
            PromptKind::Find | PromptKind::ReplaceFind => self.last_search.clone(),
            _ => String::new(),
        };
        let caret = input.chars().count();
        self.overlay = Overlay::Prompt(Prompt {
            kind,
            input,
            caret,
            origin: matches!(kind, PromptKind::Find).then(|| self.view().sel.clone()),
            held: String::new(),
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
                // has to put it back where it was.
                let origin = prompt.origin.take();
                self.overlay = Overlay::None;
                if let Some(origin) = origin {
                    self.view_mut().sel = origin;
                    self.scroll_into_view();
                }
                return;
            }
            (KeyCode::Enter, _) => return self.accept_prompt(),
            (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                prompt.delete_word()
            }
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
            // Enter and the arrows step through hits without closing the box,
            // so you can look before you leap.
            (KeyCode::Down, _) | (KeyCode::F(3), _) => {
                if prompt.kind == PromptKind::Find {
                    let needle = prompt.input.clone();
                    self.last_search = needle;
                    self.find_step(1);
                }
                return;
            }
            (KeyCode::Up, _) => {
                if prompt.kind == PromptKind::Find {
                    let needle = prompt.input.clone();
                    self.last_search = needle;
                    self.find_step(-1);
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
        if needle.is_empty() {
            if let Some(origin) = origin {
                self.view_mut().sel = origin;
                self.scroll_into_view();
            }
            return;
        }
        let from = origin
            .as_ref()
            .map(|sel| sel.primary().start())
            .unwrap_or(0);
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
                let App { docs, lsp, panes, focus, .. } = self;
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

    fn confirm_key(&mut self, key: Key) {
        let Overlay::Confirm(confirm) = &self.overlay else {
            return;
        };
        let then = confirm.then;
        let answer = match key.code {
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
        let rows: Vec<Row> = crate::cmd::ALL
            .iter()
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
                "trim_trailing_whitespace",
                "Drop trailing spaces when saving",
                self.config.trim_trailing_whitespace(),
            ),
            setting_row(
                "spaces",
                "Indent new files with spaces",
                self.config.spaces(),
            ),
        ];
        self.overlay = Overlay::Picker(Picker::new(Kind::Settings, rows));
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
                        .tag(d.source.clone().unwrap_or_else(|| d.severity.label().into()))
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
            (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                picker.delete_word()
            }
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
                            '@' => return self.run(Cmd::Symbols),
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
        if !matches!(kind, Kind::Settings) {
            self.overlay = Overlay::None;
        }

        match choice {
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
            Choice::Setting(which) => {
                self.toggle_setting(which);
                // Redraw the list so the ticks are right.
                let scroll = match &self.overlay {
                    Overlay::Picker(p) => Some((p.cursor, p.query.clone())),
                    _ => None,
                };
                self.open_settings_picker();
                if let (Overlay::Picker(picker), Some((cursor, query))) =
                    (&mut self.overlay, scroll)
                {
                    for c in query.chars() {
                        picker.type_char(c);
                    }
                    picker.select(cursor);
                }
            }
        }
    }
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
            docs, lsp, panes, focus, ..
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
    pub fn count_matches_of(&self, needle: &str) -> usize {
        self.count_matches(needle)
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
                            .detail(format!("{}:{}", short(&path, &project), number + 1)),
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
        let (start, message, severity) = (
            found.range.start(),
            found.message.clone(),
            found.severity,
        );
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
                format!("{} ({}): {state}", server.name, short(&server.root, &self.project))
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
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.hover(doc, at).is_none() {
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
        if lsp.workspace_symbols(doc, &query).is_none() && query.is_empty() {
            self.say("no language server that can search the project");
        }
    }

    fn ask_code_actions(&mut self) {
        let range = self.view().sel.primary();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.code_actions(doc, range).is_none() {
            self.say("no language server with anything to offer");
        }
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
        if self.lsp.primary_for(self.here()).is_none() {
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
    fn accept_completion(&mut self) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let Some(item) = completion.selected().cloned() else {
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
        changes.sort_by_key(|c| c.from);

        let (doc, view) = self.pair();
        let before = view.sel.clone();
        let edits = doc.apply_atomic(changes, &before);
        view.absorb(&edits, doc.len_chars());
        // The cursor goes to the end of what was put in, wherever mapping
        // would otherwise have left it.
        let mut landed = from + item.insert.chars().count();
        for edit in &edits {
            if edit.from < from {
                landed = (landed as isize + edit.inserted as isize
                    - (edit.to - edit.from) as isize) as usize;
            }
        }
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
            (KeyCode::Enter, _) | (KeyCode::Tab, KeyModifiers::NONE) => self.accept_completion(),
            (KeyCode::Esc, _) => self.completion = None,
            _ => return false,
        }
        true
    }

    fn hover_at_screen(&mut self, column: u16, row: u16) {
        let Some(at) = self.position_at(column, row) else {
            return;
        };
        // Only over something worth asking about.
        if text::word_text_at(&self.here().rope, at).is_none() {
            return;
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
                    && let Some(edit) = params.get("edit") {
                        let count = self.apply_workspace_edit(edit);
                        if count > 0 {
                            self.say_good(format!("changed {count} {}", places(count)));
                        }
                    }
            }
            Incoming::Response { id: request, result } => self.on_response(id, request, result),
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
        doc.diagnostics.retain(|d| d.server != id.0);
        doc.diagnostics.extend(fresh);
    }

    fn on_response(&mut self, id: ServerId, request: i64, result: Result<Value, String>) {
        let Some(ask) = self.lsp.get_mut(id).and_then(|s| s.claim(request)) else {
            return;
        };
        let value = match result {
            Ok(value) => value,
            Err(why) => {
                // A failed request for something the editor asked for on its
                // own — completions as you type — is not worth a word.
                if !matches!(ask, Ask::Completion { .. } | Ask::Signature { .. }) {
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
            Ask::Completion { doc, at, version } => self.take_completions(doc, at, version, value),
            Ask::Hover { doc, at } => self.take_hover(doc, at, value),
            Ask::Goto { doc, what } => self.take_goto(doc, what, value),
            Ask::References => self.take_references(value),
            Ask::Symbols { doc } => self.take_symbols(doc, value),
            Ask::WorkspaceSymbols => self.take_workspace_symbols(value),
            Ask::Rename { to } => {
                let count = self.apply_workspace_edit(&value);
                match count {
                    0 => self.say("nothing to rename"),
                    n => self.say_good(format!("renamed to {to} in {n} {}", places(n))),
                }
            }
            Ask::Format { doc, version } => self.take_format(doc, version, value),
            Ask::CodeActions => self.take_code_actions(id, value),
            Ask::Signature { doc, at } => self.take_signature(doc, at, value),
            Ask::ResolveAction => self.do_code_action(id, value),
            Ask::Command => {}
        }
    }

    fn take_completions(&mut self, doc: DocId, at: usize, version: i32, value: Value) {
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
            start,
            all: suggestions,
            shown: Vec::new(),
            cursor: 0,
            top: 0,
            area: Rect::default(),
        };
        completion.narrow(&typed);
        self.completion = (!completion.is_empty()).then_some(completion);
    }

    fn take_hover(&mut self, doc: DocId, at: usize, value: Value) {
        if self.view().doc != doc {
            return;
        }
        let here = self.doc(doc).map(|d| d.language).unwrap_or(LangId::PLAIN);
        let lines = markup_lines(value.get("contents"), here);
        if lines.is_empty() {
            return;
        }
        self.hover = Some(Popup {
            lines,
            at,
            scroll: 0,
        });
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
        self.signature = Some(Popup {
            lines,
            at,
            scroll: 0,
        });
    }

    fn take_goto(&mut self, doc: DocId, what: Goto, value: Value) {
        if self.view().doc != doc {
            return;
        }
        let places = locations(&value);
        match places.len() {
            0 => self.say(format!("no {} found", what.label())),
            1 => {
                let (path, line, column) = places[0].clone();
                self.view_mut().mark_jump();
                self.open_path(&path);
                self.go_to(line, column);
            }
            _ => {
                let project = self.project.clone();
                let rows: Vec<Row> = places
                    .into_iter()
                    .map(|(path, line, column)| {
                        Row::new(
                            short(&path, &project),
                            Choice::There {
                                path: path.clone(),
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

    fn take_references(&mut self, value: Value) {
        let places = locations(&value);
        if places.is_empty() {
            return self.say("used nowhere the server knows of");
        }
        let project = self.project.clone();
        let rows: Vec<Row> = places
            .into_iter()
            .map(|(path, line, column)| {
                // The line of code itself, where the file is one we have.
                let preview = self
                    .docs
                    .iter()
                    .find(|d| d.path.as_deref() == Some(path.as_path()))
                    .and_then(|d| {
                        (line < d.len_lines()).then(|| {
                            let start = text::line_start(&d.rope, line);
                            let end = text::line_end(&d.rope, line);
                            d.rope.slice(start..end).to_string().trim().to_string()
                        })
                    })
                    .unwrap_or_else(|| short(&path, &project));
                Row::new(
                    preview,
                    Choice::There {
                        path: path.clone(),
                        line,
                        column,
                    },
                )
                .detail(format!("{}:{}", short(&path, &project), line + 1))
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
        let Some(document) = self.doc(doc) else { return };
        collect_symbols(&value, document, 0, &mut rows);
        if rows.is_empty() {
            return self.say("nothing this file defines that the server will name");
        }
        self.view_mut().mark_jump();
        self.overlay = Overlay::Picker(Picker::new(Kind::Symbols, rows));
    }

    fn take_workspace_symbols(&mut self, value: Value) {
        let Value::Array(items) = &value else { return };
        let project = self.project.clone();
        let rows: Vec<Row> = items
            .iter()
            .filter_map(|item| {
                let name = item.get("name")?.as_str()?.to_string();
                let location = item.get("location")?;
                let path = crate::lsp::path_of(location.get("uri")?.as_str()?)?;
                let (line, column) =
                    crate::lsp::point_of(location.get("range")?.get("start")?)?;
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
        if let Overlay::Picker(picker) = &mut self.overlay
            && picker.kind == Kind::WorkspaceSymbols
        {
            picker.set_rows(rows);
        }
    }

    fn take_format(&mut self, doc: DocId, version: i32, value: Value) {
        let waiting = self.save_after_format.take() == Some(doc);
        if self.doc(doc).map(|d| d.version) != Some(version) {
            // The file moved on while the formatter was thinking. Applying
            // these edits now would scramble it — but a save that was waiting
            // on them should still happen, or Ctrl-S would have done nothing.
            if waiting {
                self.write_now(None);
            }
            return;
        }
        let count = match &value {
            Value::Array(edits) => self.apply_edits_to(doc, edits),
            _ => 0,
        };
        if waiting {
            self.write_now(None);
        } else if count > 0 {
            self.say_good("formatted");
        }
    }

    fn take_code_actions(&mut self, id: ServerId, value: Value) {
        let Value::Array(items) = &value else {
            return self.say("nothing to offer here");
        };
        if items.is_empty() {
            return self.say("nothing to offer here");
        }
        let rows: Vec<Row> = items
            .iter()
            .filter_map(|item| {
                let title = item.get("title").and_then(Value::as_str)?;
                let mut row = Row::new(
                    title.to_string(),
                    Choice::Action(id, Box::new(item.clone())),
                );
                if let Some(kind) = item.get("kind").and_then(Value::as_str) {
                    row = row.tag(kind.split('.').next_back().unwrap_or(kind).to_string());
                }
                Some(row)
            })
            .collect();
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
        let mut changes: Vec<crate::doc::Change> = edits
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
        if changes.is_empty() {
            return 0;
        }
        // A server sends its edits against the file as it is, in no
        // particular order and never overlapping. Sorting is all that is
        // needed to make them a transaction.
        changes.sort_by_key(|c| (c.from, c.to));
        changes.dedup_by(|a, b| a.from == b.from && a.to == b.to && a.text == b.text);
        if changes.windows(2).any(|pair| pair[0].to > pair[1].from) {
            self.say_bad("the server sent overlapping edits; nothing was changed");
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

        let App { docs, lsp, .. } = self;
        if let Some(doc) = docs.iter().find(|d| d.id == id) {
            lsp.did_change(doc, &applied);
        }
        if let Some(doc) = self.doc_mut(id) {
            doc.take_pending();
        }
        self.scroll_into_view();
        count
    }
}

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
}

impl DocLine {
    fn prose(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            spans: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
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
        lines.push(DocLine { text, spans });
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
fn locations(value: &Value) -> Vec<(PathBuf, usize, usize)> {
    fn one(value: &Value, out: &mut Vec<(PathBuf, usize, usize)>) {
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
            && let (Some(path), Some(start)) = (
                crate::lsp::path_of(uri),
                range.get("start").and_then(crate::lsp::point_of),
            )
        {
            out.push((path, start.0, start.1));
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
        detail: item
            .get("detail")
            .and_then(Value::as_str)
            .map(|d| d.replace('\n', " ")),
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
        also,
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
        let mut row = Row::new(
            format!("{}{name}", "  ".repeat(depth)),
            Choice::Here(at),
        )
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
            MouseEventKind::ScrollLeft => self.pan(-4),
            MouseEventKind::ScrollRight => self.pan(4),
            MouseEventKind::Moved => {
                // Sitting still over a word is a question. Moving is not.
                match self.resting {
                    Some((_, c, r)) if c == column && r == row => {}
                    _ => {
                        self.resting = Some((Instant::now(), column, row));
                        if self.hover.is_some() {
                            self.hover = None;
                        }
                    }
                }
            }
            // The right button asks what can be done here, which is the one
            // thing a context menu is actually for.
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(at) = self.position_at(column, row) {
                    self.place_cursor(at, false, false);
                    self.run(Cmd::CodeAction);
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                if let Some(at) = self.position_at(column, row) {
                    self.place_cursor(at, false, false);
                    self.run(Cmd::Paste);
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

        // A list on top of everything gets the click, and a click outside it
        // closes it — which is what clicking away from a menu means.
        if let Overlay::Picker(picker) = &mut self.overlay {
            let area = picker.area;
            if row >= area.y && row < area.y + area.height && column >= area.x
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

        // The tabs.
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
        // Ctrl-click is what every editor has taught people goes to the
        // definition of the thing under the pointer.
        if mods.contains(KeyModifiers::CONTROL) {
            self.place_cursor(at, false, false);
            return self.run(Cmd::GotoDefinition);
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

    fn drag_to(&mut self, column: u16, row: u16) {
        match self.drag {
            Some(Drag::Scrollbar) => self.scroll_to_bar(row),
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
            _ => {}
        }
        if let Some(completion) = &mut self.completion
            && hits(completion.area, column, row)
        {
            completion.step(by.signum());
            return;
        }
        if let Some(hover) = &mut self.hover {
            hover.scroll = (hover.scroll as isize + by).max(0) as usize;
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

    fn pan(&mut self, by: isize) {
        if self.view().wrap {
            return;
        }
        let left = self.view().left as isize + by;
        self.view_mut().left = left.max(0) as usize;
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
        let across = column.saturating_sub(area.x).min(area.width.saturating_sub(1));
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let dir = std::env::temp_dir().join(format!(
            "textfold-app-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).expect("a place to work");
        dir.join(name)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            if c == '\n' {
                app.run(Cmd::InsertNewline);
            } else {
                app.type_char(c);
            }
        }
    }

    /// The keystroke another program sends to say "open this", as bytes on the
    /// way in rather than as a call to `open_path`.
    fn keyed(app: &mut App, key: &str) {
        let key = Key::parse(key).expect("a key");
        app.on_key(KeyEvent::new(key.code, key.mods));
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
        for opened in [Cmd::Open, Cmd::Find, Cmd::CommandPalette] {
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
        app.run(Cmd::Find);
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

        app.run(Cmd::Open);
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
        assert_eq!(coloured("u32"), Some(Role::Type));
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
            lines.iter().any(|l| l
                .spans
                .iter()
                .any(|(_, role)| *role == Role::String)),
            "{lines:?}"
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
        app.run(Cmd::SelectAll);
        app.run(Cmd::AddCursorBelow);
        // Whatever the cursors ended up as, Escape works back to one bare one.
        for _ in 0..4 {
            app.run(Cmd::Escape);
        }
        assert_eq!(app.view().sel.len(), 1);
        assert!(app.view().sel.primary().is_empty());
    }

    #[test]
    fn find_walks_the_matches_and_wraps_round() {
        let (mut app, _rx) = editor();
        typed(&mut app, "alpha beta alpha gamma alpha");
        app.run(Cmd::MoveDocStart);
        app.last_search = "alpha".into();

        app.run(Cmd::FindNext);
        let first = app.view().sel.primary().start();
        app.run(Cmd::FindNext);
        let second = app.view().sel.primary().start();
        assert!(second > first);
        app.run(Cmd::FindNext);
        app.run(Cmd::FindNext);
        // Four steps through three matches comes back to the first.
        assert_eq!(app.view().sel.primary().start(), first);
    }

    #[test]
    fn a_lower_case_search_ignores_case_and_a_capital_means_it() {
        let (mut app, _rx) = editor();
        typed(&mut app, "Thing thing THING");
        assert_eq!(app.count_matches_of("thing"), 3);
        assert_eq!(app.count_matches_of("Thing"), 1);
    }

    #[test]
    fn replacing_changes_every_match_as_one_undo() {
        let (mut app, _rx) = editor();
        typed(&mut app, "red green red blue red");
        app.run(Cmd::MoveDocStart);
        app.replace_all("red", "amber");
        assert_eq!(app.here().rope.to_string(), "amber green amber blue amber");
        app.run(Cmd::Undo);
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
        app.run(Cmd::New);
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
        app.run(Cmd::CloseForce);
        assert_eq!(app.docs().len(), 1);
        assert!(!app.quit);
    }

    #[test]
    fn quitting_with_unsaved_work_asks_first() {
        let (mut app, _rx) = editor();
        typed(&mut app, "unsaved");
        app.run(Cmd::Quit);
        assert!(!app.quit);
        assert!(matches!(app.overlay, Overlay::Confirm(_)));
        // Saying no keeps everything.
        app.confirm_key(Key::parse("c").unwrap());
        assert!(!app.quit);
        assert_eq!(app.here().rope.to_string(), "unsaved");
        // Saying discard leaves.
        app.run(Cmd::Quit);
        app.confirm_key(Key::parse("d").unwrap());
        assert!(app.quit);
    }

    #[test]
    fn the_palette_runs_what_you_choose() {
        let (mut app, _rx) = editor();
        typed(&mut app, "one\ntwo\nthree");
        app.run(Cmd::CommandPalette);
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
        app.run(Cmd::Settings);
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
        app.run(Cmd::Split);
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
    fn a_read_only_file_refuses_to_be_changed() {
        let (mut app, _rx) = editor();
        typed(&mut app, "fixed");
        let id = app.view().doc;
        app.doc_mut(id).expect("open").read_only = true;
        app.run(Cmd::DeleteLine);
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
