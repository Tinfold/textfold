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
use std::time::{Duration, Instant};

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
    /// The parse tree, kept in step with the rope by [`Document::apply`]
    /// itself rather than by whoever calls it — a tree that has fallen behind
    /// its text is worse than no tree, so nobody gets the chance to forget.
    pub syntax: Option<Syntax>,
    /// What the language servers have said about this file: one list, with
    /// each server's own findings replaced whole when it sends new ones.
    pub diagnostics: Vec<Diagnostic>,
    /// Why this file has no colours, where the reason is worth showing — so
    /// that a file drawn in one colour is explained rather than mysterious.
    /// `None` means either that it is coloured or that textfold has no grammar
    /// for it, which the language shown beside it already says.
    pub colours_off: Option<&'static str>,

    done: Vec<Revision>,
    undone: Vec<Revision>,
    /// The revision the text on disk matches. Comparing this against the
    /// number of revisions is what "modified" means, and it is why undoing
    /// back to where you saved correctly says the file is unmodified again.
    saved_at: Option<usize>,
    /// Edits nothing has picked up yet. Drained by whatever needs them.
    pending: Vec<AppliedEdit>,
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
    /// Which server it came from, so a fresh set from one replaces only its
    /// own findings.
    pub server: usize,
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
            crlf: false,
            had_final_newline: true,
            indent,
            version: 0,
            read_only: false,
            done: Vec::new(),
            undone: Vec::new(),
            saved_at: Some(0),
            pending: Vec::new(),
            syntax: None,
            diagnostics: Vec::new(),
            colours_off: None,
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

        let (text, existed) = match std::fs::read(&path) {
            Ok(bytes) => (
                String::from_utf8_lossy(&bytes).into_owned(),
                true,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
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

        let mut doc = Self {
            id,
            rope,
            path: Some(path),
            name,
            language,
            crlf,
            had_final_newline,
            indent,
            version: 0,
            read_only,
            done: Vec::new(),
            undone: Vec::new(),
            saved_at: Some(0),
            pending: Vec::new(),
            syntax: None,
            diagnostics: Vec::new(),
            colours_off: None,
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
            // moved.
            self.syntax = None;
            self.colours_off = Some("this file parses too slowly");
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
    pub fn reparse(&mut self) {
        let language = lang::get(self.language);
        if self.rope.len_bytes() > COLOUR_LIMIT {
            self.syntax = None;
            self.colours_off = language.has_grammar().then_some("this file is very large");
            return;
        }
        self.syntax = language
            .grammar()
            .and_then(|grammar| Syntax::new(grammar, &self.rope));
        self.colours_off = match (language.has_grammar(), self.syntax.is_some()) {
            (true, false) => Some("this file parses too slowly"),
            _ => None,
        };
    }

    /// Say what language this file is, and colour it accordingly.
    pub fn set_language(&mut self, language: LangId) {
        self.language = language;
        self.reparse();
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
        Ok(())
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

