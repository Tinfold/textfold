//! A file being edited: its text, its history, and what came from disk.
//!
//! A document owns the text and everything that follows from the text —
//! undo, the parse tree, the diagnostics a language server sent about it. It
//! does *not* own the cursors: the same file can be open in two panes with the
//! cursor in a different place in each, so cursors belong to a
//! [`View`](crate::view::View) and edits are told to update every view looking
//! at the document they changed.
//!
//! Every edit goes through [`Document::apply`], which is the only function
//! here that changes the rope. Everything that has to hear about an edit —
//! tree-sitter, a language server, the cursors — hears about it from what that
//! function returns, so nothing can quietly fall out of step with the text.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use ropey::Rope;

use crate::lang::{self, LangId};
use crate::syntax::Syntax;
use crate::text::{Range, Selections};

/// How big a file can be and still be worth colouring. Four megabytes is
/// past every hand-written source file and short of most generated ones.
const COLOUR_LIMIT: usize = 4 * 1024 * 1024;

/// How long two edits can be apart and still count as one thing you did.
/// Typing a word is one undo; typing a word, thinking, and typing another is
/// two.
const MERGE_WINDOW: Duration = Duration::from_millis(500);

/// How long to leave a file alone before trying to colour it again after a
/// parse gave up. Long enough that the burst of work that starved the last
/// attempt — a language server waking up and indexing a project — has had a
/// chance to be over.
const RECOLOUR_AFTER: Duration = Duration::from_millis(750);

/// How many times to try before believing it. A file that misses a two-second
/// budget four times running, spread over several seconds, really is a file
/// this parser cannot get through.
const RECOLOUR_TRIES: u8 = 4;

/// Which document, as everything outside holds onto one. An index would go
/// wrong the moment a buffer in the middle is closed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct DocId(pub u32);

/// One replacement: the characters from `from` to `to` become `text`.
///
/// An insertion is a change with `from == to`, and a deletion is one with an
/// empty `text`. There is no third kind, which is what keeps undo honest —
/// every edit is the same shape, so its inverse is too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub from: usize,
    pub to: usize,
    pub text: String,
}

impl Change {
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self {
            from: at,
            to: at,
            text: text.into(),
        }
    }

    pub fn delete(from: usize, to: usize) -> Self {
        Self {
            from,
            to,
            text: String::new(),
        }
    }

    pub fn replace(from: usize, to: usize, text: impl Into<String>) -> Self {
        Self {
            from,
            to,
            text: text.into(),
        }
    }
}

/// An edit that has happened, in every form something downstream needs it in.
///
/// Byte offsets and rows for tree-sitter, UTF-16 columns for a language
/// server, character indices for the cursors. All three are worked out here,
/// while both the before and after states are still at hand — afterwards the
/// old positions are gone and nobody can recover them.
#[derive(Clone, Debug)]
pub struct AppliedEdit {
    /// Character indices, in the document as it was.
    pub from: usize,
    pub to: usize,
    /// How many characters went in.
    pub inserted: usize,
    /// What went in, for a language server that wants the text.
    pub text: String,

    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    /// Row, and column in bytes — what tree-sitter counts in.
    pub start_point: (usize, usize),
    pub old_end_point: (usize, usize),
    pub new_end_point: (usize, usize),
    /// Row, and column in UTF-16 code units — what LSP counts in, for
    /// reasons that are Microsoft's rather than anybody's.
    pub lsp_start: (usize, usize),
    pub lsp_old_end: (usize, usize),
}

impl AppliedEdit {
    /// Where a position ends up after this edit.
    ///
    /// Anything before the edit is untouched. Anything after it slides by the
    /// difference. Anything *inside* it has had the ground taken out from
    /// under it, and goes to the end of what replaced it — which is where a
    /// cursor that was in deleted text should be, and where a diagnostic
    /// about deleted text may as well be.
    pub fn map(&self, at: usize) -> usize {
        if at <= self.from {
            at
        } else if at >= self.to {
            at - (self.to - self.from) + self.inserted
        } else {
            self.from + self.inserted
        }
    }
}

/// A step: what was done, and what would undo it. Both are ordinary
/// transactions, so undo is not a special path through the code — it is an
/// edit like any other, which is why it cannot drift from what it is undoing.
#[derive(Clone, Debug)]
struct Step {
    changes: Vec<Change>,
    inverse: Vec<Change>,
}

/// One thing you did, which may be several steps if you did it quickly.
#[derive(Clone, Debug)]
struct Revision {
    steps: Vec<Step>,
    /// Where the cursors were before, and after. Undo puts them back where
    /// they were, because an undo that leaves you somewhere else has only
    /// half worked.
    before: Selections,
    after: Selections,
    at: Instant,
    /// Whether a later edit is allowed to join this one. Some things — a
    /// paste, a format, a rename across a file — are one action by definition
    /// and should not absorb the typing that follows them.
    open: bool,
}

/// The text, where it came from, and what has been done to it.
pub struct Document {
    pub id: DocId,
    pub rope: Rope,
    /// Where it was read from and where it goes back to. `None` for a buffer
    /// that has never been saved, which is a real thing to be editing.
    pub path: Option<PathBuf>,
    /// What to call it in a tab: the file name, or `untitled 2`.
    pub name: String,
    pub language: LangId,
    /// Whether somebody said what language this is, rather than it being
    /// worked out from the name. A choice made by hand outlives a plugin
    /// being switched on or off; a guess is made again.
    pub language_chosen: bool,
    /// Whether the file on disk uses `\r\n`. The rope never does; this is
    /// remembered only so that saving does not quietly rewrite every line of
    /// somebody's file.
    pub crlf: bool,
    /// Whether the file ended with a newline when we read it.
    had_final_newline: bool,
    /// Indentation as this file actually uses it, which beats the setting: a
    /// file is a fact and a preference is not.
    pub indent: Indent,
    /// Counted up on every edit and handed to language servers, which insist
    /// on knowing which version of a file they are talking about.
    pub version: i32,
    /// Whether the file was read-only on disk.
    pub read_only: bool,
    /// Where the text came from, where that is not a file: the URI a language
    /// server handed it over under. Kept so that going to the same class in a
    /// jar twice comes back to the tab that is already open rather than making
    /// a second one.
    pub origin: Option<String>,
    /// The parse tree, kept in step with the rope by [`Document::apply`]
    /// itself rather than by whoever calls it — a tree that has fallen behind
    /// its text is worse than no tree, so nobody gets the chance to forget.
    pub syntax: Option<Syntax>,
    /// What the language servers have said about this file: one list, with
    /// each server's own findings replaced whole when it sends new ones.
    pub diagnostics: Vec<Diagnostic>,
    /// What makes this a plugin's buffer rather than a file's.
    pub panel: Option<Panel>,
    /// Where the debugger should stop, as character positions.
    ///
    /// Positions and not line numbers, so that a breakpoint follows the text
    /// it was put on. Adding a line above the one you are debugging must not
    /// silently move the breakpoint onto the line before — it is the same
    /// rule the diagnostics get, and for a stronger reason: a diagnostic
    /// pointing at the wrong line is confusing, and a breakpoint on the wrong
    /// line is an hour of not believing your own program.
    ///
    /// They live on the document rather than in the debugger for the same
    /// reason a cursor does: a breakpoint is a fact about a file, and it is
    /// there before any adapter starts and after it has gone.
    pub breakpoints: Vec<usize>,
    /// Text a plugin is offering to put in, shown but not there.
    pub hint: Option<Hint>,
    /// Why this file has no colours, where the reason is worth showing — so
    /// that a file drawn in one colour is explained rather than mysterious.
    /// `None` means either that it is coloured or that textfold has no grammar
    /// for it, which the language shown beside it already says.
    pub colours_off: Option<&'static str>,
    /// The file as it was when we last read or wrote it. What a later `stat`
    /// is compared against to notice somebody else writing it.
    stamp: Option<Stamp>,
    /// What that comparison last said. Kept rather than worked out on demand
    /// because the drawing asks every frame and the answer costs a `stat`.
    pub on_disk: OnDisk,
    /// What the file looked like at the check before this one, which is how a
    /// file that has finished changing is told from one that is still being
    /// written.
    seen: Option<Stamp>,
    /// Whether it looked the same at the last two checks.
    settled: bool,
    /// The state of the file we last said something about, or tried to read.
    ///
    /// Compared against the stamp rather than against [`OnDisk`], which is too
    /// coarse to tell "the same change we already mentioned" from "it has
    /// changed again since". Getting that wrong either repeats a message every
    /// second or — worse — leaves a file that has changed twice stuck on the
    /// first change forever.
    told: Option<Stamp>,

    done: Vec<Revision>,
    undone: Vec<Revision>,
    /// The revision the text on disk matches. Comparing this against the
    /// number of revisions is what "modified" means, and it is why undoing
    /// back to where you saved correctly says the file is unmodified again.
    saved_at: Option<usize>,
    /// Edits nothing has picked up yet. Drained by whatever needs them.
    pending: Vec<AppliedEdit>,
    /// When to try colouring this file again, after a parse ran out of time.
    ///
    /// A parse missing its deadline is very often a fact about how busy the
    /// machine was rather than about the file, so giving up on it for good is
    /// wrong: it turns a busy second into a file that is grey until you close
    /// and reopen it. `None` means there is nothing to retry — either the
    /// colours are fine or textfold has properly given up.
    recolour: Option<Instant>,
    /// How many attempts have run out of time in a row.
    recolour_tries: u8,
}

/// Something a language server said about this file.
///
/// Positions are character indices like everything else, worked out when the
/// diagnostic arrives, so that later edits carry them along with the text they
/// are about rather than leaving them pointing at whatever is now in that spot.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Severity,
    pub message: String,
    /// Which tool said so — `rustc`, `clippy`, `ruff`. Worth showing when two
    /// servers are running and only one of them is complaining.
    pub source: Option<String>,
    pub code: Option<String>,
    /// Whatever the server hung off it, kept exactly as it arrived and handed
    /// straight back when we ask what can be done about this problem.
    ///
    /// It is opaque to us and it is not optional. `ruff` puts the fix itself
    /// in here — the edit that removes the unused import — and matches a code
    /// action request to a problem by what comes back in this field. Dropping
    /// it does not lose a detail: it loses every quick fix that server has,
    /// which looks from the outside like a linter that complains and offers
    /// nothing.
    pub data: Option<serde_json::Value>,
    /// Who said so, so that a fresh set from one replaces only its own
    /// findings and leaves everybody else's alone.
    pub told: Told,
}

/// Text a plugin is offering, drawn where it would go but not in the file.
///
/// The thing an inline completion is: you can see what would happen if you
/// took it, and until you do, the file is exactly as you left it. Kept on the
/// document rather than on the pane because it is about the text — two panes
/// showing the same file are looking at the same offer.
pub struct Hint {
    /// Which plugin is offering, so that one plugin's offer cannot clear
    /// another's and the right one is told when it is taken.
    pub plugin: String,
    /// Where it would go, as a character index.
    pub at: usize,
    pub text: String,
}

/// A buffer a plugin fills, rather than one a file fills.
///
/// Deliberately a `Document` and not a new kind of pane. Splitting, scrolling,
/// focus, the tab bar and the pane border all work already, and a panel that
/// needed its own versions of them would be a second implementation of each.
/// What a panel does differently is exactly two things: where its colours come
/// from, and that parts of it do something when you press Enter on them.
pub struct Panel {
    /// Whose buffer this is.
    pub owner: Owner,
    /// Which of that plugin's panels this is: `stm32/pins`.
    pub id: String,
    /// The colours, as character ranges. Stands in for the tree-sitter
    /// highlights, which a panel has none of — the plugin says what its own
    /// text means, in names taken from the theme rather than in colours, so a
    /// panel is themed with everything else and a plugin author cannot pick
    /// colours that fight the theme.
    pub spans: Vec<(Range, crate::theme::Role)>,
    /// Which stretches do something, and what to send back when they do.
    pub actions: Vec<(Range, String)>,
}

/// Who fills a panel.
///
/// Nearly always a plugin, and the debugger is the exception worth an enum
/// rather than a reserved plugin id: the panel machinery — the colours, the
/// stretches that do something, the keys a panel gets because its text is not
/// yours to type into — is exactly what a stack and a set of variables want,
/// and none of it should have to be written twice. What differs is only where
/// an Enter on a row is sent, which is one match in one place.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Owner {
    /// A plugin, by the id its host is found by.
    Plugin(String),
    /// The debugger's own panel. There is one, because there is one session.
    Debugger,
}

impl Owner {
    /// The plugin behind it, where there is one.
    pub fn plugin(&self) -> Option<&str> {
        match self {
            Owner::Plugin(id) => Some(id),
            Owner::Debugger => None,
        }
    }
}

/// Where a complaint came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Told {
    /// A language server, by its id.
    Server(usize),
    /// A tool a plugin runs, by the command name it answers to.
    Tool(&'static str),
    /// A plugin's own program, by its plugin id. Separate from `Tool` because
    /// the two go stale differently: a tool's findings are replaced when it is
    /// run again, and a plugin's when it says so.
    Plugin(&'static str),
}

/// How bad it is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    /// Written worst first, so that sorting a list of problems puts the errors
    /// on top without anybody writing a comparison for it.
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    /// What a language server calls it: 1 through 4. Anything else is a
    /// warning, because a server that invents a fifth still means something.
    pub fn from_lsp(n: u64) -> Self {
        match n {
            1 => Severity::Error,
            3 => Severity::Info,
            4 => Severity::Hint,
            _ => Severity::Warning,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }

    /// The mark drawn in the gutter beside a line carrying one of these.
    pub fn mark(&self) -> &'static str {
        match self {
            Severity::Hint => "\u{00b7}",
            _ => "\u{25cf}",
        }
    }
}

/// What the file looked like on disk the last time we touched it: when it was
/// written and how big it was.
///
/// Two facts rather than one because a filesystem with a one-second timestamp
/// can rewrite a file inside the same second, and a rewrite that changes the
/// length is caught by the length even when the time says nothing happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl Stamp {
    /// The file as it is right now, or `None` if it is not there.
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        Some(Self {
            modified: meta.modified().ok(),
            len: meta.len(),
        })
    }
}

/// How many times to try reading a file that keeps moving under us.
///
/// Three, because the point is not to win a race against a program writing in
/// a loop — it is to get past the ordinary case, which is one write landing
/// while we happened to be reading. A file that is still moving after three
/// tries is a file that is being written continuously, and the answer to that
/// one is to wait rather than to try harder.
const READ_TRIES: usize = 3;

/// Read a file, and know that what came back is a whole file.
///
/// `std::fs::read` on a file that something else is writing gives you as much
/// of it as had been written when you looked, which is not an error and does
/// not look like one — it looks like the file, shorter. A build that
/// regenerates a file, a formatter that truncates before writing, a log being
/// appended to: read one of those at the wrong moment and you get half a file,
/// very often cut in the middle of a character.
///
/// So the file is stamped on both sides of the read, and content that does not
/// come back with the same stamp it went in with is thrown away rather than
/// used. That pairing is the whole point. Content read at one moment and
/// stamped with metadata from another is worse than a torn read on its own,
/// because [`Document::check_disk`] then compares against a stamp that says
/// the buffer is up to date and the damage never corrects itself.
///
/// `None` for a file that would not sit still.
pub fn read_whole(path: &Path) -> Result<Option<(Vec<u8>, Stamp)>> {
    for _ in 0..READ_TRIES {
        let before = Stamp::of(path);
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let after = Stamp::of(path);
        if let Some(stamp) = after
            && before == after
        {
            return Ok(Some((bytes, stamp)));
        }
    }
    Ok(None)
}

/// What has happened to the file behind a buffer since we last read or wrote
/// it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnDisk {
    /// Nothing. What is on disk is what we last read or wrote.
    Same,
    /// Somebody else wrote it: a build, a formatter, a `git checkout`, the
    /// same file open in another editor.
    Changed,
    /// It is not there any more.
    Gone,
}

/// What one level of indentation is in this file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Indent {
    Tabs,
    Spaces(usize),
}

impl Indent {
    /// The text of one level.
    pub fn unit(&self) -> String {
        match self {
            Indent::Tabs => "\t".into(),
            Indent::Spaces(n) => " ".repeat(*n),
        }
    }

    /// How many columns one level is worth, for working out how far a line is
    /// indented.
    pub fn width(&self, tab_width: usize) -> usize {
        match self {
            Indent::Tabs => tab_width,
            Indent::Spaces(n) => *n,
        }
    }
}

impl Document {
    /// An empty buffer with no file behind it.
    pub fn scratch(id: DocId, name: String, indent: Indent) -> Self {
        Self {
            id,
            rope: Rope::new(),
            path: None,
            name,
            language: LangId::PLAIN,
            language_chosen: false,
            crlf: false,
            had_final_newline: true,
            indent,
            version: 0,
            read_only: false,
            origin: None,
            done: Vec::new(),
            undone: Vec::new(),
            saved_at: Some(0),
            pending: Vec::new(),
            recolour: None,
            recolour_tries: 0,
            syntax: None,
            diagnostics: Vec::new(),
            panel: None,
            breakpoints: Vec::new(),
            hint: None,
            colours_off: None,
            stamp: None,
            seen: None,
            told: None,
            settled: true,
            on_disk: OnDisk::Same,
        }
    }

    /// Read a file. A path that is not there is not an error: it is a file you
    /// are about to write, which is how every file starts.
    pub fn open(id: DocId, path: &Path, fallback_indent: Indent) -> Result<Self> {
        let path = absolute(path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        // Stamped at the same moment as it was read, or not stamped at all.
        // A file that would not sit still still opens — you asked for it —
        // but with no stamp, so the first disk check reads it again properly
        // rather than believing a snapshot taken mid-write.
        let (text, existed, stamp) = match read_whole(&path) {
            Ok(Some((bytes, stamp))) => (
                String::from_utf8_lossy(&bytes).into_owned(),
                true,
                Some(stamp),
            ),
            Ok(None) => match std::fs::read(&path) {
                Ok(bytes) => (String::from_utf8_lossy(&bytes).into_owned(), true, None),
                Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
            },
            Err(e) => match e.downcast_ref::<std::io::Error>() {
                Some(io) if io.kind() == std::io::ErrorKind::NotFound => {
                    (String::new(), false, None)
                }
                _ => return Err(e),
            },
        };

        // `\r\n` is a fact about the file, not about the text in it. It is
        // taken out here and put back on save, so nothing between the two ever
        // has to think about it.
        let crlf = text.contains("\r\n");
        let text = if crlf { text.replace("\r\n", "\n") } else { text };
        let had_final_newline = text.is_empty() || text.ends_with('\n');

        let read_only = existed
            && std::fs::metadata(&path)
                .map(|m| m.permissions().readonly())
                .unwrap_or(false);

        let rope = Rope::from_str(&text);
        let indent = detect_indent(&rope).unwrap_or(fallback_indent);
        let language = crate::lang::detect(&path, &rope);
        let stamp = existed.then_some(stamp).flatten();

        let mut doc = Self {
            id,
            language_chosen: false,
            rope,
            path: Some(path),
            name,
            language,
            crlf,
            had_final_newline,
            indent,
            version: 0,
            read_only,
            origin: None,
            done: Vec::new(),
            undone: Vec::new(),
            saved_at: Some(0),
            pending: Vec::new(),
            recolour: None,
            recolour_tries: 0,
            syntax: None,
            diagnostics: Vec::new(),
            panel: None,
            breakpoints: Vec::new(),
            hint: None,
            colours_off: None,
            seen: stamp,
            told: stamp,
            // A file that would not sit still has no stamp, and a buffer with
            // no stamp for a file that is there reads as changed — which is
            // exactly right, and gets it re-read once it stops moving.
            settled: stamp.is_some(),
            stamp,
            on_disk: OnDisk::Same,
        };
        doc.reparse();
        Ok(doc)
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        // Ropey counts the empty stretch after a final newline as a line, and
        // for an editor that is exactly right: it is the line you would type
        // on next. But an empty rope has one line, not zero.
        self.rope.len_lines()
    }

    pub fn is_modified(&self) -> bool {
        self.saved_at != Some(self.done.len())
    }

    // ---- Breakpoints ----

    /// Which lines have a breakpoint on them, in order and without repeats.
    ///
    /// Worked out from the positions rather than stored, because the positions
    /// are what moves with the text: two breakpoints on lines that an edit has
    /// joined are one breakpoint, and the answer to "which lines" has to say
    /// so rather than saying the same line twice.
    pub fn breakpoint_lines(&self) -> Vec<usize> {
        let mut lines: Vec<usize> = self
            .breakpoints
            .iter()
            .map(|at| crate::text::line_of(&self.rope, (*at).min(self.len_chars())))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    pub fn has_breakpoint(&self, line: usize) -> bool {
        self.breakpoints
            .iter()
            .any(|at| crate::text::line_of(&self.rope, (*at).min(self.len_chars())) == line)
    }

    /// Put one on this line, or take away the one that is there.
    ///
    /// Answers whether there is one there now, so that whoever asked can tell
    /// the adapter and say so in the status line without asking again.
    pub fn toggle_breakpoint(&mut self, line: usize) -> bool {
        if line >= self.len_lines() {
            return false;
        }
        let was = self.breakpoints.len();
        let rope = &self.rope;
        let len = rope.len_chars();
        self.breakpoints
            .retain(|at| crate::text::line_of(rope, (*at).min(len)) != line);
        if self.breakpoints.len() != was {
            return false;
        }
        // The start of the line, which is a position that survives the line
        // being edited: typing at the end of it leaves the breakpoint where
        // it was, and typing a newline before it carries the breakpoint down
        // with the code it was put on.
        self.breakpoints.push(crate::text::line_start(rope, line));
        self.breakpoints.sort_unstable();
        true
    }

    /// Say that what is in the rope is what is on disk, without writing
    /// anything. For a buffer that was just re-read: the text changed, so it
    /// is a revision you can undo, but it is not an unsaved change.
    pub fn mark_saved(&mut self) {
        self.close_revision();
        self.saved_at = Some(self.done.len());
        self.accept_disk();
    }

    /// Take the edits nothing has looked at yet. Whoever calls this is
    /// responsible for them; there is only one of each.
    pub fn take_pending(&mut self) -> Vec<AppliedEdit> {
        std::mem::take(&mut self.pending)
    }

    /// Do a set of changes as one undoable action.
    ///
    /// `changes` must be in order and must not overlap — everything that
    /// builds them works from the cursors, which are already both. They are
    /// applied back to front so that no change has to be adjusted for the ones
    /// before it.
    ///
    /// `selections` is where the cursors were, so undo can put them back.
    /// Returns the edits, already recorded as pending, so a caller that wants
    /// to move its own cursors by them does not have to ask twice.
    pub fn apply(&mut self, changes: Vec<Change>, selections: &Selections) -> Vec<AppliedEdit> {
        self.apply_inner(changes, selections, true)
    }

    /// The same, but standing alone in the undo history: the next thing typed
    /// starts a new revision rather than joining this one. For pastes,
    /// formats, and anything a language server did.
    pub fn apply_atomic(&mut self, changes: Vec<Change>, selections: &Selections) -> Vec<AppliedEdit> {
        let edits = self.apply_inner(changes, selections, false);
        if let Some(last) = self.done.last_mut() {
            last.open = false;
        }
        edits
    }

    fn apply_inner(
        &mut self,
        changes: Vec<Change>,
        selections: &Selections,
        mergeable: bool,
    ) -> Vec<AppliedEdit> {
        if changes.is_empty() {
            return Vec::new();
        }
        let (edits, inverse) = self.run(&changes);

        // Anything that could be redone described a future that no longer
        // exists.
        self.undone.clear();

        let step = Step {
            changes,
            inverse,
        };
        let joins = mergeable
            && self
                .done
                .last()
                .is_some_and(|last| last.open && last.at.elapsed() < MERGE_WINDOW);
        if joins {
            let last = self.done.last_mut().expect("checked");
            last.steps.push(step);
            last.at = Instant::now();
        } else {
            self.done.push(Revision {
                steps: vec![step],
                before: selections.clone(),
                after: selections.clone(),
                at: Instant::now(),
                open: mergeable,
            });
            // A revision after the saved point means the saved point is gone
            // for good: you cannot get back to it by undoing past it.
            if self.saved_at.is_some_and(|at| at >= self.done.len()) {
                self.saved_at = None;
            }
        }
        edits
    }

    /// Where the cursors should end up after the action now being recorded.
    /// Called once the caller has worked out where they went, so that undo and
    /// redo both land somewhere sensible.
    pub fn record_selections(&mut self, after: &Selections) {
        if let Some(last) = self.done.last_mut() {
            last.after = after.clone();
        }
    }

    /// Stop the next edit joining the last one. What a cursor movement, a
    /// save, or leaving the file means for undo.
    pub fn close_revision(&mut self) {
        if let Some(last) = self.done.last_mut() {
            last.open = false;
        }
    }

    /// Whether there is anything to undo, for a menu row that should be there
    /// but greyed rather than missing.
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Undo one action, returning the edits it made and where the cursors were
    /// before it, or `None` when there is nothing to undo.
    pub fn undo(&mut self) -> Option<(Vec<AppliedEdit>, Selections)> {
        let mut revision = self.done.pop()?;
        let mut edits = Vec::new();
        // Backwards: the last thing done is the first thing undone.
        for step in revision.steps.iter().rev() {
            let (mut made, _) = self.run(&step.inverse);
            edits.append(&mut made);
        }
        revision.open = false;
        let before = revision.before.clone();
        self.undone.push(revision);
        Some((edits, before))
    }

    /// Redo one action.
    pub fn redo(&mut self) -> Option<(Vec<AppliedEdit>, Selections)> {
        let revision = self.undone.pop()?;
        let mut edits = Vec::new();
        for step in &revision.steps {
            let (mut made, _) = self.run(&step.changes);
            edits.append(&mut made);
        }
        let after = revision.after.clone();
        self.done.push(revision);
        Some((edits, after))
    }

    /// Change the rope, and work out everything anybody downstream will want
    /// to know about the change. The only place the text is written.
    fn run(&mut self, changes: &[Change]) -> (Vec<AppliedEdit>, Vec<Change>) {
        let mut edits = Vec::with_capacity(changes.len());
        let mut inverse = Vec::with_capacity(changes.len());

        // Back to front, so that applying one does not move the next.
        for change in changes.iter().rev() {
            let from = change.from.min(self.rope.len_chars());
            let to = change.to.clamp(from, self.rope.len_chars());

            let removed = self.rope.slice(from..to).to_string();
            let start_byte = self.rope.char_to_byte(from);
            let old_end_byte = self.rope.char_to_byte(to);
            let start_point = self.point_at(from);
            let old_end_point = self.point_at(to);
            let lsp_start = self.lsp_point_at(from);
            let lsp_old_end = self.lsp_point_at(to);

            if to > from {
                self.rope.remove(from..to);
            }
            if !change.text.is_empty() {
                self.rope.insert(from, &change.text);
            }

            let inserted = change.text.chars().count();
            let new_end = from + inserted;
            edits.push(AppliedEdit {
                from,
                to,
                inserted,
                text: change.text.clone(),
                start_byte,
                old_end_byte,
                new_end_byte: self.rope.char_to_byte(new_end),
                start_point,
                old_end_point,
                new_end_point: self.point_at(new_end),
                lsp_start,
                lsp_old_end,
            });
            // The inverse of "these characters became that text" is "that
            // text becomes these characters", at the place it now occupies.
            inverse.push(Change {
                from,
                to: new_end,
                text: removed,
            });
        }

        // Both lists are in the order the changes were made, which is right to
        // left. That is the order the edits have to stay in: each one's
        // positions describe the document as it stood when it was made, so
        // tree-sitter, a language server and a cursor all have to be walked
        // through them in the same sequence to end up in the same place.
        //
        // The inverse cannot stay that way. It describes an undo of the
        // finished document, and a change recorded before the ones to its left
        // were made is sitting at a position that no longer means what it did.
        // Each is shifted by what the changes further left went on to do.
        let mut shift: isize = 0;
        for (undo, edit) in inverse.iter_mut().zip(edits.iter()).rev() {
            undo.from = (undo.from as isize + shift) as usize;
            undo.to = (undo.to as isize + shift) as usize;
            shift += edit.inserted as isize - (edit.to - edit.from) as isize;
        }
        // And then back into order, so it is an ordinary transaction like any
        // other and undo needs no special path through this function.
        inverse.reverse();

        self.version += 1;
        // The tree hears about the edits here, in this order, because this is
        // the only place they exist in the order they happened.
        if let Some(syntax) = &mut self.syntax
            && !syntax.update(&self.rope, &edits)
        {
            // The reparse ran out of time, so the tree no longer describes the
            // text. Better no colours than colours belonging to text that has
            // moved — but only until the machine is quiet enough to try again.
            self.syntax = None;
            self.colours_off = Some("colouring this file again");
            self.recolour = Some(Instant::now() + RECOLOUR_AFTER);
        }
        self.pending.extend(edits.iter().cloned());
        (edits, inverse)
    }

    /// Build the parse tree from nothing. For a file just opened, and for one
    /// whose language has just been set by hand.
    ///
    /// A file past [`COLOUR_LIMIT`] does not get one. Parsing megabytes takes
    /// long enough to look like a hang, and every keystroke afterwards pays
    /// for it again; a generated file that size is being looked at, not
    /// written, and looking at it in black and white beats waiting for it.
    /// Put text into a buffer wholesale, with no undo step and nobody told.
    ///
    /// Only for a buffer the editor has just made and nothing else can be
    /// looking at yet. Anything already on the screen has to change through
    /// [`Document::apply_atomic`], so that cursors and language servers come
    /// along with it.
    pub fn set_text(&mut self, text: &str) {
        self.rope = Rope::from_str(text);
        self.version += 1;
        self.reparse();
    }

    pub fn reparse(&mut self) {
        self.recolour = None;
        self.recolour_tries = 0;
        self.parse_now(false);
    }

    /// Is it time to try colouring this file again?
    ///
    /// Asked every time round the loop, so it is a comparison and nothing
    /// else. [`Document::recolour`] is what does the work.
    pub fn wants_recolour(&self) -> bool {
        self.recolour.is_some_and(|due| due <= Instant::now())
    }

    /// Try again, with the patience of something running while nothing else
    /// is. Gives up for good after [`RECOLOUR_TRIES`].
    pub fn recolour(&mut self) {
        self.recolour = None;
        self.parse_now(true);
        if self.syntax.is_some() {
            self.recolour_tries = 0;
            return;
        }
        self.recolour_tries = self.recolour_tries.saturating_add(1);
        if self.recolour_tries < RECOLOUR_TRIES {
            // Not beaten yet, so it must not say it is: `parse_now` wrote the
            // final answer and there is another attempt to come.
            self.colours_off = Some("colouring this file again");
            self.recolour = Some(Instant::now() + RECOLOUR_AFTER * self.recolour_tries as u32);
        }
    }

    /// Build the tree, now, and say why there is none if there is none.
    ///
    /// `patient` is the difference between a parse racing the next keystroke
    /// and one with the machine to itself.
    fn parse_now(&mut self, patient: bool) {
        let language = lang::get(self.language);
        if self.rope.len_bytes() > COLOUR_LIMIT {
            self.syntax = None;
            self.recolour = None;
            self.colours_off = language.has_grammar().then_some("this file is very large");
            return;
        }
        self.syntax = language.grammar().and_then(|grammar| match patient {
            true => Syntax::patient(grammar, &self.rope),
            false => Syntax::new(grammar, &self.rope),
        });
        self.colours_off = match (language.has_grammar(), self.syntax.is_some()) {
            // Not "too slowly" yet: this is the first attempt, and saying so
            // is the difference between a file being coloured in a moment and
            // a file textfold has written off.
            (true, false) => Some(match patient {
                true => "this file parses too slowly",
                false => "colouring this file again",
            }),
            _ => None,
        };
        if self.syntax.is_none() && language.has_grammar() && !patient {
            self.recolour = Some(Instant::now() + RECOLOUR_AFTER);
        }
    }

    /// Say what language this file is, and colour it accordingly.
    pub fn set_language(&mut self, language: LangId) {
        self.language = language;
        self.language_chosen = true;
        self.reparse();
    }

    /// Work out the language again, for after the plugins have changed under
    /// us. A language somebody chose by hand is left alone.
    pub fn redetect_language(&mut self) {
        let (Some(path), false) = (self.path.clone(), self.language_chosen) else {
            return;
        };
        let found = crate::lang::detect(&path, &self.rope);
        if found != self.language {
            self.language = found;
        }
        // Even where it did not change, the grammar behind it may have: a
        // plugin switched off takes the colours with it.
        self.reparse();
    }

    /// Which line and column a character index is on, counted the way a person
    /// counts from zero. What a session writes down, because a line number
    /// still means something after the file has been edited by something else
    /// and a character offset does not.
    pub fn point_at_char(&self, at: usize) -> (usize, usize) {
        let at = at.min(self.rope.len_chars());
        let line = self.rope.char_to_line(at);
        (line, at - self.rope.line_to_char(line))
    }

    /// Row and byte column, which is what tree-sitter counts in.
    fn point_at(&self, at: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(at);
        let start = self.rope.line_to_char(line);
        let col = self.rope.char_to_byte(at) - self.rope.char_to_byte(start);
        (line, col)
    }

    /// Row and UTF-16 column, which is what LSP counts in.
    pub fn lsp_point_at(&self, at: usize) -> (usize, usize) {
        let at = at.min(self.rope.len_chars());
        let line = self.rope.char_to_line(at);
        let start = self.rope.line_to_char(line);
        let col = self
            .rope
            .slice(start..at)
            .chars()
            .map(|c| c.len_utf16())
            .sum();
        (line, col)
    }

    /// The character index a language server's row and UTF-16 column means.
    /// Out-of-range values are clamped rather than refused: a server that is
    /// one edit behind sends them all the time, and dropping the answer would
    /// be worse than pointing slightly to the left.
    /// The character index at a line and column, both counted in characters
    /// from zero — the way the editor counts everywhere else, and the way a
    /// plugin is asked to.
    ///
    /// A column past the end of its line lands at the end of that line rather
    /// than running on into the next one: a compiler pointing at "column 200"
    /// of a forty-character line means the end of it.
    pub fn char_at_point(&self, line: usize, column: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        let end = crate::text::line_end(&self.rope, line);
        (start + column).min(end)
    }

    pub fn char_at_lsp_point(&self, line: usize, utf16_col: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        let end = crate::text::line_end(&self.rope, line);
        let mut at = start;
        let mut col = 0;
        while at < end && col < utf16_col {
            col += self.rope.char(at).len_utf16();
            at += 1;
        }
        at
    }

    /// Write the file, putting back whatever the file's own conventions were.
    ///
    /// Written to a neighbouring temporary file and renamed over the original,
    /// so that a full disk or a crash halfway through leaves the old file
    /// rather than half of a new one.
    pub fn save_to(&mut self, path: &Path, final_newline: bool) -> Result<()> {
        let mut text = self.rope.to_string();
        // A file that ends without a newline is a file somebody may have meant
        // to end that way; the setting decides, and the setting's default is
        // to give it one, because nearly every tool expects it.
        if final_newline && !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if self.crlf {
            text = text.replace('\n', "\r\n");
        }

        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).ok();
        let temp = dir.join(format!(
            ".{}.textfold-{}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "buffer".into()),
            std::process::id()
        ));
        // A rename cannot preserve permissions it does not know about, so the
        // original's are copied across before it takes the original's place.
        let existing = std::fs::metadata(path).ok().map(|m| m.permissions());
        std::fs::write(&temp, text.as_bytes())
            .with_context(|| format!("writing {}", temp.display()))?;
        if let Some(perms) = existing {
            std::fs::set_permissions(&temp, perms).ok();
        }
        if let Err(e) = std::fs::rename(&temp, path) {
            std::fs::remove_file(&temp).ok();
            return Err(e).with_context(|| format!("saving {}", path.display()));
        }

        self.had_final_newline = text.is_empty() || text.ends_with('\n');
        self.saved_at = Some(self.done.len());
        self.close_revision();
        if self.path.as_deref() != Some(path) {
            let path = absolute(path);
            self.name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            self.language = crate::lang::detect(&path, &self.rope);
            self.path = Some(path);
        }
        self.read_only = false;
        // What we just wrote is what is there, so a later check has nothing to
        // complain about.
        self.stamp = Stamp::of(path);
        self.on_disk = OnDisk::Same;
        Ok(())
    }

    /// Follow a file that has been moved on disk.
    ///
    /// Not a save: the text is untouched and whether it has unsaved changes is
    /// untouched. What changes is where it will be saved *to*, what it is
    /// called in the tab row, and — since a name is how textfold decides what
    /// a file is — possibly what language it is.
    ///
    /// The stamp is taken afresh because it is about the file on disk, and the
    /// file on disk is somewhere else now. Without that the next check would
    /// see a path it has never stamped and report the file as changed
    /// underneath you the moment it was renamed.
    pub fn rename_to(&mut self, path: PathBuf) {
        let path = absolute(&path);
        self.name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.language = crate::lang::detect(&path, &self.rope);
        self.stamp = Stamp::of(&path);
        self.on_disk = OnDisk::Same;
        self.path = Some(path);
    }

    /// Throw away everything that could be undone.
    ///
    /// For a buffer whose text is not yours: a plugin's panel is replaced
    /// whole every time the plugin has something new to say, and each of those
    /// would otherwise push a revision holding the entire old text and the
    /// entire new one. A file tree that redraws on every keystroke would grow
    /// a history of every shape it has ever had, and none of it is reachable —
    /// undo in a read-only buffer has nothing to give you back.
    pub fn forget_history(&mut self) {
        self.done.clear();
        self.undone.clear();
        // Nothing was undone and nothing is pending, so it is exactly what
        // was last put in it.
        self.saved_at = Some(0);
    }

    /// Look at the file and say whether somebody else has written it.
    ///
    /// A `stat`, which is cheap, rather than a read. Called on a timer for
    /// every open file, so it has to stay that way.
    ///
    /// It also notices whether the file is *still* changing, by remembering
    /// what it looked like last time. A file being written to continuously — a
    /// log, a build's output, a download in progress — is a file that has no
    /// settled contents to read, and reading it anyway gets you a snapshot of
    /// something mid-write.
    pub fn check_disk(&mut self) -> OnDisk {
        let Some(path) = self.path.as_deref() else {
            return OnDisk::Same;
        };
        let now = Stamp::of(path);
        self.on_disk = match (self.stamp, now) {
            // A buffer for a file that does not exist yet is not a file that
            // has gone missing.
            (None, None) => OnDisk::Same,
            (None, Some(_)) => OnDisk::Changed,
            (Some(_), None) => OnDisk::Gone,
            (Some(was), Some(is)) if was == is => OnDisk::Same,
            (Some(_), Some(_)) => OnDisk::Changed,
        };
        // It has settled if it looks the same as it did a moment ago. The
        // first sighting of a change is never settled, which costs one extra
        // check before an ordinary `git checkout` is taken and is what keeps a
        // file that is halfway through being written out of your buffer.
        self.settled = self.seen == now;
        self.seen = now;
        self.on_disk
    }

    /// Whether the file has stopped changing: it looked the same at the last
    /// two checks. Only meaningful just after [`Document::check_disk`].
    pub fn has_settled(&self) -> bool {
        self.settled
    }

    /// Whether what is on disk now is something we have not already dealt
    /// with. What keeps one `git checkout` to one line in the status bar, and
    /// what makes a *second* one a fresh thing to notice.
    pub fn is_news(&self) -> bool {
        self.told != self.seen
    }

    /// Remember that we have said something about the file as it is now, or
    /// have tried to read it and would not take it.
    pub fn noted(&mut self) {
        self.told = self.seen;
    }

    /// Take what is on disk now as the truth, without reading it. For a
    /// conflict the person has looked at and decided to keep their side of.
    pub fn accept_disk(&mut self) {
        if let Some(path) = self.path.as_deref() {
            self.stamp = Stamp::of(path);
        }
        self.seen = self.stamp;
        self.told = self.stamp;
        self.settled = true;
        self.on_disk = OnDisk::Same;
    }

    /// Take a stamp read at the same moment as some content, for a buffer that
    /// has just been filled from disk.
    ///
    /// Separate from [`Document::accept_disk`], which stats afresh, because
    /// stamping content from one moment with metadata from another is the way
    /// a buffer ends up quietly wrong forever: the stamp says it is up to date
    /// and nothing ever looks again.
    pub fn took_from_disk(&mut self, stamp: Stamp) {
        self.close_revision();
        self.saved_at = Some(self.done.len());
        self.stamp = Some(stamp);
        self.seen = Some(stamp);
        self.told = Some(stamp);
        self.settled = true;
        self.on_disk = OnDisk::Same;
    }

    /// The whole text, for a language server that wants a copy.
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// A selection's text.
    pub fn slice(&self, range: Range) -> String {
        let len = self.rope.len_chars();
        self.rope
            .slice(range.start().min(len)..range.end().min(len))
            .to_string()
    }
}

/// A path from the root, so that two ways of naming one file are one file.
/// A path that cannot be made absolute — because the working directory has
/// been deleted out from under us — is left as it is rather than refused.
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// What this file already does about indentation.
///
/// A file is a fact and a setting is a wish, so a file that has made up its
/// mind wins. Only the first few hundred lines are looked at: a file that
/// disagrees with itself after line 300 was never going to be settled by
/// reading further.
fn detect_indent(rope: &Rope) -> Option<Indent> {
    let mut tabs = 0usize;
    // How often each step up in indentation was of each size. A file indented
    // four at a time has plenty of 4s; one indented two at a time has plenty
    // of 2s, and also some 4s, which is why the smallest common step wins
    // rather than the most frequent.
    let mut steps = [0usize; 9];
    let mut previous = 0usize;

    for line in rope.lines().take(400) {
        let mut spaces = 0;
        let mut is_tab = false;
        let mut any = false;
        for c in line.chars() {
            match c {
                ' ' => spaces += 1,
                '\t' => {
                    is_tab = true;
                    break;
                }
                '\n' | '\r' => break,
                _ => {
                    any = true;
                    break;
                }
            }
        }
        if is_tab {
            tabs += 1;
            continue;
        }
        if !any {
            // A blank line says nothing about indentation.
            continue;
        }
        if spaces > previous {
            let step = spaces - previous;
            if step < steps.len() {
                steps[step] += 1;
            }
        }
        previous = spaces;
    }

    let space_steps: usize = steps.iter().sum();
    if tabs > space_steps {
        return Some(Indent::Tabs);
    }
    if space_steps == 0 {
        return None;
    }
    // Two, four and eight are what people actually use; a handful of stray
    // three-space steps in a file indented by four should not decide it.
    let best = [2usize, 4, 8, 3, 6]
        .into_iter()
        .max_by_key(|&n| steps[n])
        .filter(|&n| steps[n] > 0)?;
    Some(Indent::Spaces(best))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Range;

    fn doc(text: &str) -> Document {
        let mut d = Document::scratch(DocId(0), "test".into(), Indent::Spaces(4));
        d.rope = Rope::from_str(text);
        d
    }

    fn sel(at: usize) -> Selections {
        Selections::single(Range::point(at))
    }

    #[test]
    fn a_breakpoint_goes_on_and_comes_off_the_line_you_asked_about() {
        let mut d = doc("one\ntwo\nthree\n");
        assert!(d.toggle_breakpoint(1), "it should go on");
        assert!(d.has_breakpoint(1));
        assert!(!d.has_breakpoint(0));
        assert_eq!(d.breakpoint_lines(), vec![1]);
        assert!(!d.toggle_breakpoint(1), "and off again");
        assert!(d.breakpoint_lines().is_empty());
    }

    #[test]
    fn a_breakpoint_past_the_end_of_the_file_is_not_put_anywhere() {
        let mut d = doc("one\n");
        assert!(!d.toggle_breakpoint(40));
        assert!(d.breakpoints.is_empty());
    }

    #[test]
    fn two_breakpoints_on_lines_that_have_become_one_are_one_breakpoint() {
        // The reason lines are worked out from positions rather than stored:
        // an edit can join two lines, and the answer to "which lines have a
        // breakpoint" then has to be one line rather than that line twice.
        // The adapter is told this list, and told the same line twice it sets
        // two breakpoints and reports two.
        let mut d = doc("one\ntwo\nthree\n");
        d.toggle_breakpoint(0);
        d.toggle_breakpoint(1);
        assert_eq!(d.breakpoint_lines(), vec![0, 1]);
        // Both positions now point into the same line.
        d.rope = Rope::from_str("onetwo\nthree\n");
        d.breakpoints = vec![0, 3];
        assert_eq!(d.breakpoint_lines(), vec![0]);
    }

    #[test]
    fn a_buffer_refilled_a_thousand_times_remembers_none_of_it() {
        // A plugin's panel is replaced whole every time the plugin has
        // something new to say. Without this, a file tree that redraws on each
        // keystroke grows a revision holding the whole old text for every
        // shape it has ever had — unbounded, and unreachable, since undo in a
        // buffer you cannot type into has nothing to give back.
        let mut d = doc("");
        for round in 0..1000 {
            let was = d.len_chars();
            let text = format!("line one of round {round}\nline two\nline three\n");
            d.apply_atomic(vec![Change::replace(0, was, text)], &sel(0));
            d.mark_saved();
            d.forget_history();
        }
        assert!(d.done.is_empty(), "{} revisions kept", d.done.len());
        assert!(d.undone.is_empty());
        assert!(!d.is_modified(), "it should read as exactly what was put in");

        // And undo has nothing to give back, which is the point: the history
        // is not merely hidden, it is gone.
        let before = d.rope.to_string();
        d.undo();
        assert_eq!(d.rope.to_string(), before);

        // Without forgetting, the same thousand rounds keep a thousand
        // revisions — this is what the leak looked like.
        let mut d = doc("");
        for round in 0..50 {
            let was = d.len_chars();
            d.apply_atomic(vec![Change::replace(0, was, format!("round {round}\n"))], &sel(0));
        }
        assert_eq!(d.done.len(), 50, "the history really does grow otherwise");
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("textfold-disk-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("a place to work");
        dir
    }

    /// Write a file such that its stamp really does change, on a filesystem
    /// whose timestamps may only have one-second resolution.
    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).expect("written");
    }

    #[test]
    fn a_file_read_whole_comes_back_with_the_stamp_it_was_read_under() {
        // The pairing is the point. Content from one moment stamped with
        // metadata from another is a buffer that is quietly wrong forever,
        // because the stamp says it is up to date and nothing looks again.
        let dir = scratch_dir("whole");
        let path = dir.join("a.txt");
        write(&path, "hello");
        let (bytes, stamp) = read_whole(&path).expect("read").expect("it sat still");
        assert_eq!(bytes, b"hello");
        assert_eq!(Some(stamp), Stamp::of(&path));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_is_not_there_is_an_error_rather_than_an_empty_answer() {
        let dir = scratch_dir("missing");
        let err = read_whole(&dir.join("nothing.txt")).expect_err("no file");
        assert!(err.to_string().contains("nothing.txt"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_still_being_written_is_not_taken_until_it_stops() {
        // The bug this is here for: a buffer full of half a file, or of the
        // replacement characters half a file ends in when the half lands in
        // the middle of a character.
        let dir = scratch_dir("settle");
        let path = dir.join("log.txt");
        write(&path, "one\n");

        let mut d = Document::open(DocId(0), &path, Indent::Spaces(4)).expect("opened");
        assert_eq!(d.check_disk(), OnDisk::Same);

        // It changes. The first sighting of a change is never settled, because
        // one look cannot tell "it has just changed" from "it is changing".
        write(&path, "one\ntwo\n");
        assert_eq!(d.check_disk(), OnDisk::Changed);
        assert!(!d.has_settled(), "one look is not enough to know it has stopped");

        // Still changing, still not settled, however many times we look.
        for n in 3..8 {
            write(&path, &format!("{}\n", "x\n".repeat(n)));
            assert_eq!(d.check_disk(), OnDisk::Changed);
            assert!(!d.has_settled(), "it moved again between the two looks");
        }

        // It stops. The next look sees the same thing twice and says so.
        assert_eq!(d.check_disk(), OnDisk::Changed);
        assert!(d.has_settled());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_buffer_filled_from_disk_is_stamped_with_what_it_was_filled_from() {
        let dir = scratch_dir("stamp");
        let path = dir.join("a.txt");
        write(&path, "first");
        let mut d = Document::open(DocId(0), &path, Indent::Spaces(4)).expect("opened");

        // Somebody else writes it, and we take it.
        write(&path, "second");
        let (bytes, stamp) = read_whole(&path).expect("read").expect("sat still");
        let len = d.len_chars();
        d.apply_atomic(
            vec![Change::replace(0, len, String::from_utf8(bytes).unwrap())],
            &sel(0),
        );
        d.took_from_disk(stamp);

        assert!(!d.is_modified());
        assert_eq!(d.check_disk(), OnDisk::Same, "it is up to date, and knows it");

        // And a further change is still noticed, rather than being hidden
        // behind a stamp taken at the wrong moment.
        write(&path, "third");
        assert_eq!(d.check_disk(), OnDisk::Changed);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_would_not_sit_still_to_be_opened_is_read_again_later() {
        // It opens either way — you asked for it — but with no stamp, so the
        // first disk check treats it as changed and reads it properly once it
        // has stopped moving, rather than believing a snapshot taken mid-write.
        let dir = scratch_dir("open-moving");
        let path = dir.join("a.txt");
        write(&path, "half");
        let mut d = Document::open(DocId(0), &path, Indent::Spaces(4)).expect("opened");
        d.stamp = None;
        d.seen = None;
        assert_eq!(d.check_disk(), OnDisk::Changed);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A rust file, coloured.
    fn rust(text: &str) -> Document {
        lang::init();
        let mut d = doc(text);
        d.set_language(lang::by_name("rust").expect("shipped"));
        d
    }

    #[test]
    fn colours_a_busy_moment_knocked_out_come_back() {
        let mut d = rust("fn main() {\n    let x = 1;\n}\n");
        assert!(d.syntax.is_some(), "it never had colours to lose");

        // What a parse that ran out of its budget leaves behind. The budget is
        // wall-clock, so this is what a language server taking every core for
        // a moment does to a file that is perfectly easy to parse.
        d.syntax = None;
        d.colours_off = Some("colouring this file again");
        d.recolour = Some(Instant::now());

        assert!(d.wants_recolour(), "nothing was going to try again");
        d.recolour();
        assert!(d.syntax.is_some(), "the file never got its colours back");
        assert_eq!(d.colours_off, None);
        assert!(!d.wants_recolour(), "it is still trying after it worked");
        assert_eq!(d.recolour_tries, 0);
    }

    #[test]
    fn a_file_too_large_to_colour_is_not_tried_again_for_ever() {
        let mut d = rust("fn main() {}\n");
        d.rope = Rope::from_str(&"// padding\n".repeat(COLOUR_LIMIT / 8));
        d.reparse();
        assert!(d.syntax.is_none());
        assert_eq!(d.colours_off, Some("this file is very large"));
        assert!(
            !d.wants_recolour(),
            "a file that will never be small enough is being retried"
        );
    }

    #[test]
    fn giving_up_takes_several_goes_and_then_says_so() {
        let mut d = rust("fn main() {}\n");
        // A language with a grammar, and a rope the parser will not be given.
        // Standing in for a parse that keeps running out of time.
        d.syntax = None;
        d.recolour_tries = RECOLOUR_TRIES - 1;
        d.recolour = Some(Instant::now());
        d.rope = Rope::from_str(&"x".repeat(COLOUR_LIMIT + 1));

        d.recolour();
        // Past the size limit it is not a matter of trying again, and the
        // reason given is the true one rather than "too slowly".
        assert_eq!(d.colours_off, Some("this file is very large"));
        assert!(!d.wants_recolour());
    }

    #[test]
    fn an_edit_and_its_undo_are_the_same_shape() {
        let mut d = doc("hello world");
        d.apply(vec![Change::replace(6, 11, "there")], &sel(6));
        assert_eq!(d.rope.to_string(), "hello there");
        assert!(d.is_modified());
        d.undo();
        assert_eq!(d.rope.to_string(), "hello world");
        d.redo();
        assert_eq!(d.rope.to_string(), "hello there");
    }

    #[test]
    fn undoing_several_changes_at_once_puts_all_of_them_back() {
        // The changes are applied right to left, so every inverse but the
        // leftmost is recorded at a position the finished document does not
        // agree with. Getting this wrong leaves an undo that half works.
        let mut d = doc("aaa bbb ccc");
        d.apply(
            vec![
                Change::replace(0, 3, "x"),
                Change::replace(4, 7, "yy"),
                Change::replace(8, 11, "zzz"),
            ],
            &sel(0),
        );
        assert_eq!(d.rope.to_string(), "x yy zzz");
        d.undo();
        assert_eq!(d.rope.to_string(), "aaa bbb ccc");
        d.redo();
        assert_eq!(d.rope.to_string(), "x yy zzz");
    }

    #[test]
    fn a_position_walked_through_every_edit_lands_where_it_belongs() {
        let mut d = doc("aaa bbb ccc");
        let edits = d.apply(
            vec![
                Change::replace(0, 3, "x"),
                Change::replace(4, 7, "yy"),
                Change::replace(8, 11, "zzz"),
            ],
            &sel(0),
        );
        // The very end of the old text is the very end of the new text.
        let mut at = 11;
        for edit in &edits {
            at = edit.map(at);
        }
        assert_eq!(at, d.len_chars());
    }

    #[test]
    fn quick_typing_undoes_as_one_thing() {
        let mut d = doc("");
        for (i, c) in "word".chars().enumerate() {
            d.apply(vec![Change::insert(i, c.to_string())], &sel(i));
        }
        assert_eq!(d.rope.to_string(), "word");
        d.undo();
        assert_eq!(d.rope.to_string(), "");
        assert!(d.undo().is_none());
    }

    #[test]
    fn a_paste_does_not_absorb_what_you_type_next() {
        let mut d = doc("");
        d.apply_atomic(vec![Change::insert(0, "pasted")], &sel(0));
        d.apply(vec![Change::insert(6, "!")], &sel(6));
        d.undo();
        assert_eq!(d.rope.to_string(), "pasted");
        d.undo();
        assert_eq!(d.rope.to_string(), "");
    }

    #[test]
    fn undoing_back_to_where_you_saved_is_unmodified_again() {
        let mut d = doc("start");
        assert!(!d.is_modified());
        d.apply(vec![Change::insert(5, "!")], &sel(5));
        assert!(d.is_modified());
        d.undo();
        assert!(!d.is_modified());
    }

    #[test]
    fn positions_after_an_edit_land_where_a_person_would_expect() {
        let mut d = doc("one two three");
        let edits = d.apply(vec![Change::replace(4, 7, "TWO!")], &sel(4));
        let edit = &edits[0];
        // Before the edit: untouched.
        assert_eq!(edit.map(2), 2);
        // After it: slid by one.
        assert_eq!(edit.map(8), 9);
        // Inside it: at the end of what replaced it.
        assert_eq!(edit.map(5), 8);
    }

    #[test]
    fn utf16_columns_are_what_a_language_server_is_told() {
        // An emoji outside the basic plane is two UTF-16 units and one char.
        let d = doc("x🦀y");
        assert_eq!(d.lsp_point_at(1), (0, 1));
        assert_eq!(d.lsp_point_at(2), (0, 3));
        assert_eq!(d.char_at_lsp_point(0, 3), 2);
    }

    #[test]
    fn a_file_that_has_made_up_its_mind_about_indentation_wins() {
        let tabs = Rope::from_str("fn a() {\n\tlet x = 1;\n\tif x {\n\t\tb();\n\t}\n}\n");
        assert_eq!(detect_indent(&tabs), Some(Indent::Tabs));
        let two = Rope::from_str("def a():\n  if x:\n    b()\n  c()\n");
        assert_eq!(detect_indent(&two), Some(Indent::Spaces(2)));
        let four = Rope::from_str("fn a() {\n    let x = 1;\n    if x {\n        b();\n    }\n}\n");
        assert_eq!(detect_indent(&four), Some(Indent::Spaces(4)));
        // Nothing to go on is nothing to go on, not a guess.
        assert_eq!(detect_indent(&Rope::from_str("one\ntwo\n")), None);
    }
}

#[cfg(test)]
mod point_tests {
    use super::*;

    #[test]
    fn a_line_and_column_counted_in_characters_finds_the_place() {
        let mut doc = Document::scratch(DocId(0), "test".into(), Indent::Spaces(4));
        let sel = crate::text::Selections::single(crate::text::Range::point(0));
        doc.apply_atomic(vec![Change::replace(0, 0, String::from("héllo\nwörld\n"))], &sel);

        // Characters, not bytes and not UTF-16: the accented letter is one of
        // each, and column three is past it either way.
        assert_eq!(doc.char_at_point(0, 0), 0);
        assert_eq!(doc.char_at_point(0, 3), 3);
        assert_eq!(doc.char_at_point(1, 0), 6);
        assert_eq!(doc.char_at_point(1, 5), 11);

        // A column past the end of its line stops at the end of that line
        // rather than running on into the next one — which is what a compiler
        // means when it points at column 200 of a five-character line.
        assert_eq!(doc.char_at_point(0, 200), 5);
        // And a line past the end of the file lands in the last one.
        assert_eq!(doc.char_at_point(99, 0), doc.char_at_point(2, 0));
    }
}
