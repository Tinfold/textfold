//! Finding, going places, and everything that involves a language server.
//!
//! Searching this file and searching the project, replacing in one and in all
//! of them, and the questions put to a language server — where is this
//! defined, what is it, what could be done about it.

use super::*;

impl App {
    /// The focused document and the servers, borrowed apart so that both can
    /// be used at once. Nearly every question for a server needs the document
    /// it is about, and this is how to have both without copying the file.
    pub(super) fn doc_and_lsp(&mut self) -> (&Document, &mut Servers) {
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

    pub(super) fn lsp_open_here(&mut self) {
        let id = self.view().doc;
        self.lsp_open(id);
    }

    pub(super) fn lsp_open(&mut self, id: DocId) {
        let App { docs, lsp, .. } = self;
        if let Some(doc) = docs.iter().find(|d| d.id == id) {
            lsp.open(doc);
        }
        // Whatever the servers have already said about this file, now that
        // there is a rope to say it against. A server that pushes its
        // findings said them once, when it checked the project, and will not
        // say them again for a file that has not changed — so a buffer that
        // waited for the next answer would wait for ever.
        self.take_stored_diagnostics(id);
        // A file nobody has asked about yet. The colours, the hints and the
        // notes are all questions about the whole of it, and this is the
        // moment there is a whole of it to ask about.
        self.ask_the_servers_about_this_file();
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
    pub(super) fn catch_a_host_up(&mut self, id: HostId) {
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
    pub(super) fn search(&self, needle: &str, from: usize, forwards: bool, wrap: bool) -> Option<Range> {
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
    pub(super) fn find_from_prompt(&mut self, by: isize) {
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

    pub(super) fn find_step(&mut self, by: isize) {
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

    pub(super) fn count_matches(&self, needle: &str) -> usize {
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

    pub(super) fn replace_all(&mut self, needle: &str, with: &str) {
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
        let changes: Vec<crate::doc::Change> = occurrences(&text, needle)
            .into_iter()
            .filter_map(|byte| {
                let start = doc.rope.byte_to_char(byte);
                let end = start + needle.chars().count();
                // Outside the selection, when there is one to be outside of.
                let inside = whole || (start >= limit.start() && end <= limit.end());
                inside.then(|| crate::doc::Change::replace(start, end, with))
            })
            .collect();

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

    /// Work out which files a project-wide replace would touch, and ask.
    ///
    /// On a thread, like everything else that walks a project: a repository
    /// with sixty thousand files in it is a second or two of reading, and an
    /// editor that stops answering the keyboard for a second or two has
    /// stopped being an editor. Nothing is changed here — the walk only
    /// produces the question.
    pub(super) fn find_what_to_replace(&mut self, needle: String, with: String) {
        if needle.is_empty() {
            return;
        }
        let root = self.project.clone();
        let tx = self.tx.clone();
        self.say(format!("looking for {needle}…"));
        std::thread::Builder::new()
            .name("replace".into())
            .spawn(move || {
                let mut files = Vec::new();
                let mut over = 0usize;
                for entry in ignore::WalkBuilder::new(&root).build().flatten() {
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        continue;
                    }
                    let path = entry.into_path();
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        // Not text. Not our business — and certainly not
                        // something to rewrite.
                        continue;
                    };
                    let found = occurrences(&text, &needle).len();
                    if found == 0 {
                        continue;
                    }
                    // Past the limit they are counted rather than kept. A
                    // replacement that would open nine hundred buffers is one
                    // somebody wants to hear the size of before it happens.
                    if files.len() >= REPLACE_AT_MOST {
                        over += 1;
                        continue;
                    }
                    files.push((path, found));
                }
                tx.send(Event::ToReplace(Box::new(Replace {
                    needle,
                    with,
                    files,
                    over,
                })))
                .ok();
            })
            .ok();
    }

    /// Say what a project-wide replace would do, and wait to be told to do it.
    pub(super) fn ask_before_replacing(&mut self, what: Replace) {
        if what.files.is_empty() {
            return self.say(format!("no {} in any file", what.needle));
        }
        let hits: usize = what.files.iter().map(|(_, n)| n).sum();
        let with = match what.with.is_empty() {
            true => "nothing".to_string(),
            false => format!("{:?}", what.with),
        };
        let over = match what.over {
            0 => String::new(),
            n => format!(", and {n} more than it will open at once"),
        };
        self.overlay = Overlay::Confirm(Confirm {
            message: format!(
                "replace {} with {with} — {} in {}{over}",
                what.needle,
                count("place", hits),
                count("file", what.files.len()),
            ),
            choices: vec![
                ('r', "replace them, leaving the files unsaved".into()),
                ('c', "leave them alone".into()),
            ],
            then: Then::ReplaceEverywhere(Box::new(what)),
        });
    }

    /// Do it: every file that matched, as an ordinary edit to an ordinary
    /// buffer.
    ///
    /// Not a rewrite of the files on the disk. Every other thing that changes
    /// text in this editor goes through a buffer, and going round that for
    /// this one would mean a replacement across forty files that cannot be
    /// undone, cannot be looked at first, and tells no language server that
    /// anything happened. So each file is opened, changed and left unsaved:
    /// the margin shows what moved, undo works a file at a time, and
    /// `save-all` is the moment it reaches the disk.
    pub(super) fn replace_everywhere(&mut self, what: Replace) {
        let mut changed = 0usize;
        let mut files = 0usize;
        let mut refused = Vec::new();
        let here = self.view().doc;
        for (path, _) in &what.files {
            let id = match self.docs.iter().find(|d| d.path.as_deref() == Some(path)) {
                Some(doc) => doc.id,
                None => {
                    let id = self.new_id();
                    match Document::open(id, path, self.default_indent()) {
                        Ok(doc) => {
                            self.docs.push(doc);
                            self.lsp_open(id);
                            id
                        }
                        Err(_) => continue,
                    }
                }
            };
            // A file that cannot be written is a file to leave alone rather
            // than to fill with changes nobody can save.
            if self.doc(id).is_some_and(|d| d.read_only) {
                refused.push(short(path, &self.project));
                continue;
            }
            let n = self.replace_in_doc(id, &what.needle, &what.with);
            if n > 0 {
                changed += n;
                files += 1;
            }
        }
        self.session_changed();
        // Back where you were. Opening thirty buffers should not also move you
        // into the last one of them.
        self.show(here);

        if changed == 0 {
            return self.say("nothing was replaced");
        }
        let left = match refused.len() {
            0 => String::new(),
            n => format!(" — {n} read-only, left alone"),
        };
        self.say_good(format!(
            "replaced {} in {}, unsaved{left}",
            count("place", changed),
            count("file", files),
        ));
    }

    /// Replace every occurrence in one buffer, as one undoable edit. Answers
    /// how many there were.
    pub(super) fn replace_in_doc(&mut self, id: DocId, needle: &str, with: &str) -> usize {
        let Some(doc) = self.doc(id) else { return 0 };
        let text = doc.rope.to_string();
        let changes: Vec<crate::doc::Change> = occurrences(&text, needle)
            .into_iter()
            .map(|byte| {
                let start = doc.rope.byte_to_char(byte);
                crate::doc::Change::replace(start, start + needle.chars().count(), with)
            })
            .collect();
        if changes.is_empty() {
            return 0;
        }
        let count = changes.len();
        let Some(doc) = self.doc_mut(id) else { return 0 };
        // A buffer nobody is looking at has no cursors of its own; the panes
        // that are looking at one are told where everything went by
        // [`App::after_edit_to`], the same as for any other edit.
        let before = Selections::single(Range::point(0));
        let edits = doc.apply_atomic(changes, &before);
        self.after_edit_to(id, edits, None);
        count
    }

    pub(super) fn start_grep(&mut self, query: &str) {
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

    pub(super) fn go_to_line(&mut self, line: usize) {
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

    pub(super) fn go_to(&mut self, line: usize, column: usize) {
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
    pub(super) fn centre_if_off_screen(&mut self) {
        let at = self.view().cursor();
        let line = text::line_of(&self.here().rope, at);
        let view = self.view();
        let (top, height) = (view.top, view.height());
        if line < top || line >= top + height {
            self.centre();
        }
    }

    pub(super) fn jump(&mut self, forwards: bool) {
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

    pub(super) fn go_to_matching_bracket(&mut self) {
        let at = self.view().cursor();
        match edit::match_bracket(self.here(), at) {
            Some(found) => {
                self.view_mut().sel = Selections::single(Range::point(found));
                self.scroll_into_view();
            }
            None => self.say("the cursor is not on a bracket"),
        }
    }

    /// Fold away everything under the cursor: the function, the block, the
    /// string.
    ///
    /// The line the thing starts on stays, because it is the line that says
    /// what was folded — `fn draw_gutter(…)` is worth more on the screen than
    /// a row saying "38 lines". Everything after it, to the end of the thing,
    /// goes.
    pub(super) fn fold_here(&mut self) {
        let at = self.view().cursor();
        let doc = self.here();
        let Some(syntax) = &doc.syntax else {
            return self.say("no parse tree for this file, so nothing to fold by");
        };
        let byte = doc.rope.char_to_byte(at);
        let Some((from, to)) = syntax.foldable_at(byte, &doc.rope) else {
            return self.say("nothing to fold here");
        };
        let (from, to) = (doc.rope.byte_to_char(from), doc.rope.byte_to_char(to));
        match self.fold(from, to) {
            0 => self.say("nothing to fold here"),
            n => self.say(format!("folded {}", count("line", n))),
        }
    }

    /// Add one fold, keeping the cursors somewhere they can be seen. Answers
    /// how many lines went.
    pub(super) fn fold(&mut self, from: usize, to: usize) -> usize {
        let doc = self.here();
        let first = text::line_of(&doc.rope, from);
        let last = text::line_of(&doc.rope, to.min(doc.len_chars()));
        if last <= first {
            return 0;
        }
        // The fold starts at the end of its first line: that line stays on
        // screen, and holding a position on it rather than the line number
        // means an edit above cannot leave the fold around the wrong thing.
        let start = text::line_end(&doc.rope, first);
        let end = text::line_end(&doc.rope, last);
        let view = self.view_mut();
        if view.folds.iter().any(|(a, b)| *a == start && *b == end) {
            return 0;
        }
        view.folds.push((start, end));
        self.cursors_into_view();
        self.scroll_into_view();
        last - first
    }

    /// Bring back whatever is folded under the cursor. Answers whether
    /// anything was.
    ///
    /// The innermost first, so that unfolding a folded function inside a
    /// folded class opens the class and then the function, one keystroke each,
    /// rather than opening everything at once.
    pub(super) fn unfold_here(&mut self) -> bool {
        let at = self.view().cursor();
        let line = text::line_of(&self.here().rope, at);
        self.unfold_line(line)
    }

    /// Bring back the innermost fold covering a line.
    pub(super) fn unfold_line(&mut self, line: usize) -> bool {
        let rope = self.here().rope.clone();
        let view = self.view_mut();
        let mut best: Option<(usize, usize)> = None;
        for (from, to) in &view.folds {
            let first = text::line_of(&rope, *from);
            let last = text::line_of(&rope, *to);
            // The line a fold is folded onto, or any line it covers — so it
            // can be opened from the row that is still on the screen.
            if line < first || line > last {
                continue;
            }
            if best.is_none_or(|(a, b)| last - first < b - a) {
                best = Some((first, last));
                continue;
            }
        }
        let Some((first, _)) = best else { return false };
        view.folds
            .retain(|(from, _)| text::line_of(&rope, *from) != first);
        true
    }

    /// Fold what is here, or bring it back if it is already folded. One key
    /// for both, which is what people expect of a triangle in a margin.
    pub(super) fn toggle_fold(&mut self) {
        if self.unfold_here() {
            return self.say("unfolded");
        }
        self.fold_here();
    }

    /// Fold every top-level thing in the file: the file as a list of what is
    /// in it.
    pub(super) fn fold_all(&mut self) {
        let doc = self.here();
        let Some(syntax) = &doc.syntax else {
            return self.say("no parse tree for this file, so nothing to fold by");
        };
        let ranges: Vec<(usize, usize)> = syntax
            .foldable_top_level(&doc.rope)
            .into_iter()
            .map(|(from, to)| (doc.rope.byte_to_char(from), doc.rope.byte_to_char(to)))
            .collect();
        let mut folded = 0;
        for (from, to) in ranges {
            if self.fold(from, to) > 0 {
                folded += 1;
            }
        }
        match folded {
            0 => self.say("nothing left to fold"),
            n => self.say_good(format!("folded {}", count("thing", n))),
        }
    }

    /// Bring the whole file back.
    pub(super) fn unfold_all(&mut self) {
        let had = self.view().folds.len();
        self.view_mut().folds.clear();
        match had {
            0 => self.say("nothing is folded"),
            n => self.say_good(format!("{} unfolded", count("fold", n))),
        }
    }

    /// Move any cursor that is inside a fold to the line the fold is folded
    /// onto.
    ///
    /// A cursor nobody can see is a cursor that types where nobody is looking,
    /// which is the one thing folding must never allow.
    pub(super) fn cursors_into_view(&mut self) {
        let rope = self.here().rope.clone();
        let folded = self.view().folded(&rope);
        if folded.is_empty() {
            return;
        }
        let view = self.view_mut();
        view.sel.map(|range| {
            let mut head = range.head;
            for (first, last) in &folded {
                let line = text::line_of(&rope, head);
                if line > *first && line <= *last {
                    head = text::line_end(&rope, *first);
                }
            }
            Range::point(head)
        });
    }

    pub(super) fn expand_selection(&mut self) {
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

    pub(super) fn step_diagnostic(&mut self, by: isize) {
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

    pub(super) fn show_server_status(&mut self) {
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

    pub(super) fn ask_goto(&mut self, what: Goto) {
        let at = self.view().cursor();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.goto(doc, at, what).is_none() {
            let label = what.label();
            self.say(format!("no language server that can find a {label}"));
        }
    }

    pub(super) fn ask_references(&mut self) {
        let at = self.view().cursor();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.references(doc, at).is_none() {
            self.say("no language server that can find uses");
        }
    }

    pub(super) fn ask_hover(&mut self, at: usize) {
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

    pub(super) fn ask_symbols(&mut self) {
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.symbols(doc).is_none() {
            self.say("no language server that can list what this file defines");
        }
    }

    pub(super) fn ask_workspace_symbols(&mut self, query: &str) {
        let (doc, lsp) = self.doc_and_lsp();
        let query = query.to_string();
        if lsp.workspace_symbols(doc, &query, None).is_none() && query.is_empty() {
            self.say("no language server that can search the project");
        }
    }

    pub(super) fn ask_code_actions(&mut self) {
        let range = self.view().sel.primary();
        let id = self.view().doc;
        let (doc, lsp) = self.doc_and_lsp();
        let asked = lsp.code_actions(doc, range);
        if asked.is_empty() {
            return self.say("no language server with anything to offer");
        }
        self.offer = Some(Gathered::new(id, range.start(), asked));
    }

    /// Lay the file out: every formatter that has anything to do with it, in
    /// the order you have said they go in.
    ///
    /// The same list a save runs, through the same queue — see
    /// [`App::format_steps`]. It used to ask the language server and nobody
    /// else, which meant that a project formatted by `prettier` came out one
    /// way when you saved it and another way when you asked for it.
    pub(super) fn format(&mut self) {
        if self.before_save.is_some() {
            return;
        }
        let id = self.view().doc;
        let steps = self.format_steps(id, true);
        if steps.is_empty() {
            return self.say("nothing here knows how to format this file");
        }
        self.begin(id, steps, false, None);
    }

    pub(super) fn start_rename(&mut self) {
        if !self.lsp.can(self.here(), "renameProvider") {
            return self.say("no language server that can rename this");
        }
        // Ask first, where the server will say. Being told that a keyword
        // cannot be renamed is worth more before you have typed a new name for
        // it than after — and the answer carries the server's own idea of what
        // is being renamed, which is a better thing to put in the box than the
        // word the cursor happens to be touching.
        let at = self.view().cursor();
        let (doc, lsp) = self.doc_and_lsp();
        if lsp.prepare_rename(doc, at).is_some() {
            return;
        }
        self.open_prompt(PromptKind::Rename);
    }

    /// Ask for completions. `asked_for` separates a keystroke that means
    /// "suggest something" from the editor deciding to ask on its own — only
    /// the first is worth an answer of "there is nobody to ask", and the
    /// second would otherwise put that on the screen every time you typed a
    /// word in a plain text file.
    pub(super) fn ask_for_completions(&mut self, triggered: Option<char>, asked_for: bool) {
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
    pub(super) fn accept_completion(&mut self) {
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
    pub(super) fn take_suggestion(&mut self, index: usize) {
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
    pub(super) fn completion_key(&mut self, key: Key) -> bool {
        // Asked before the list is borrowed, and the reason is below: with a
        // plugin's suggestion on the screen as well, Tab belongs to that one.
        let offered = self.hint_showing();
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
            (KeyCode::Enter, _) => {
                self.accept_completion();
                return true;
            }
            // Tab, unless a plugin is offering something too. Two things
            // wanting one key is the whole of why taking a Copilot suggestion
            // used to be impossible with the list up: the list took Tab, the
            // suggestion had nothing else, and there was no way to say which
            // you meant. So they have a key each — Enter takes the row that is
            // lit, Tab takes the greyed-out text at the cursor — and the one
            // that is *in the text in front of you* gets the key that is
            // already pointing at it.
            (KeyCode::Tab, KeyModifiers::NONE) if !offered => {
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
    pub(super) fn hint_key(&mut self, key: Key) -> bool {
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
    pub(super) fn problem_lines(&self, at: usize) -> Vec<DocLine> {
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

    pub(super) fn hover_at_screen(&mut self, column: u16, row: u16) {
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
