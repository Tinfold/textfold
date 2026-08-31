//! What is open over the editor: lists, prompts, questions, and the help.
//!
//! One box for all of them. The fuzzy list that opens files is the one that
//! runs commands and the one that shows what a language server offered, so
//! learning it once is learning all of them.

use super::*;

impl App {
    pub(super) fn open_prompt(&mut self, kind: PromptKind) {
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
            PromptKind::ReplaceFind | PromptKind::ProjectReplaceFind => self.last_search.clone(),
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

    pub(super) fn prompt_key(&mut self, key: Key) {
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
    pub(super) fn on_prompt_changed(&mut self) {
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

    pub(super) fn accept_prompt(&mut self) {
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
            PromptKind::DebugAddress => {
                self.overlay = Overlay::None;
                let Some((host, port)) = crate::dap::read_address(&input) else {
                    return self.say_bad(format!(
                        "{input:?} is not a port or a host and port — 5005, or 10.0.0.2:5005"
                    ));
                };
                self.remember_address(&format!("{host}:{port}"));
                self.attach_with(None, Some((host, port)));
            }
            PromptKind::DebugEvaluate => {
                self.overlay = Overlay::None;
                if input.is_empty() {
                    return;
                }
                // The answer goes into the panel rather than the status line:
                // it is very often longer than a line, and it is the sort of
                // thing you want to still be there after you have stepped.
                self.debug.evaluate(&input);
                self.open_debug_panel();
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
            PromptKind::ProjectReplaceFind => {
                if input.is_empty() {
                    self.overlay = Overlay::None;
                    return;
                }
                self.last_search = input.clone();
                prompt.kind = PromptKind::ProjectReplaceWith;
                prompt.held = input;
                prompt.input.clear();
                prompt.caret = 0;
            }
            PromptKind::ProjectReplaceWith => {
                self.overlay = Overlay::None;
                // The replacement may well be nothing — "take that word out
                // everywhere" is half of what this is for — so an empty box is
                // an answer here rather than a cancellation.
                self.find_what_to_replace(held, input);
            }
        }
    }

    // ---- Reading a hover ----

    /// Keys while the hover has the keyboard. Answers whether it took the key.
    pub(super) fn hover_key(&mut self, key: Key) -> bool {
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
    pub(super) fn hover_to_buffer(&mut self) {
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

    pub(super) fn menu_key(&mut self, key: Key) {
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

    pub(super) fn do_menu(&mut self, action: menu::Action) {
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
    pub(super) fn key_for(&self, cmd: Cmd) -> Option<String> {
        self.keys.shortcut(cmd)
    }

    /// The menu for a place in the text: right-clicking code, or the
    /// context-menu key.
    ///
    /// The same commands the keyboard has, in the order a person looks for
    /// them: what to do with the selection, then what the language server
    /// knows, then the rest.
    pub(super) fn text_menu(&self, anchor: (u16, u16)) -> Menu {
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
                row("Fix it", Cmd::FIX_IT).enabled(self.fixes.found.is_some()),
                row("What can be done here…", Cmd::CODE_ACTION)
                    .enabled(can("codeActionProvider") && writable),
                row("What is this?", Cmd::HOVER).enabled(can("hoverProvider")),
                menu::Item::divider(),
                row("Select line", Cmd::SELECT_LINE),
                row("Select all", Cmd::SELECT_ALL),
                // Folding, from the mouse. There is nothing in a menu that a
                // keystroke cannot do, and this is the keystroke's other door.
                row("Fold this", Cmd::TOGGLE_FOLD).enabled(self.here().syntax.is_some()),
                row("Unfold everything", Cmd::UNFOLD_ALL).enabled(!self.view().folds.is_empty()),
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
    pub(super) fn tab_menu(&self, id: DocId, anchor: (u16, u16)) -> Menu {
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
    pub(super) fn toggle_diff(&mut self) {
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

    pub(super) fn compare(&self, left: usize, right: usize) -> Option<crate::diff::Diff> {
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
    pub(super) fn check_diff(&mut self) {
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
    pub(super) fn follow_diff(&mut self) {
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
    pub(super) fn change_step(&mut self, forwards: bool) {
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
        let changed = self.git.changed_lines(id);
        self.say(format!("{} changed", count("line", changed)));
    }

    /// Go looking for a name across the project, having been given only the
    /// name.
    ///
    /// This is what Ctrl-clicking a type in a docstring has to mean. There is
    /// no "definition" to ask for — the name is in a paragraph of prose, not
    /// in the code — so the question becomes the one a person would ask
    /// instead: where in this project is there something called that?
    pub(super) fn look_up(&mut self, name: &str) {
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
    pub(super) fn first_use_of(&self, name: &str) -> Option<usize> {
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
    pub(super) fn look_up_by_name(&mut self, name: &str) {
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
    pub(super) fn open_context_menu(&mut self) {
        let anchor = crate::ui::cursor_cell(self).unwrap_or((self.screen.x, self.screen.y));
        self.overlay = Overlay::Menu(self.text_menu(anchor));
    }

    pub(super) fn confirm_key(&mut self, key: Key) {
        let Overlay::Confirm(confirm) = &self.overlay else {
            return;
        };
        let then = confirm.then.clone();
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
            (Then::ReplaceEverywhere(what), 'r') => self.replace_everywhere(*what),
            _ => {}
        }
    }

    pub(super) fn help_key(&mut self, key: Key) {
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

    pub(super) fn open_files_picker(&mut self) {
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

    pub(super) fn start_walk(&mut self) {
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

    pub(super) fn open_commands_picker(&mut self) {
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

    pub(super) fn open_buffers_picker(&mut self) {
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

    pub(super) fn open_theme_picker(&mut self) {
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

    pub(super) fn open_language_picker(&mut self) {
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

    pub(super) fn open_settings_picker(&mut self) {
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
                "inlay_hints",
                "Show the types and names the code does not say",
                self.config.inlay_hints(),
            ),
            setting_row(
                "code_lenses",
                "Show what a server has to say about each line",
                self.config.code_lenses(),
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
    pub(super) fn open_plugins_picker(&mut self) {
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
    pub(super) fn toggle_plugin(&mut self, id: &str) {
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
    pub(super) fn plugins_changed(&mut self) {
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

    /// Pick a plugin to have an opinion about.
    pub(super) fn open_plugin_settings_picker(&mut self) {
        let rows: Vec<Row> = crate::plugin::all()
            .iter()
            .map(|plugin| {
                let mine = crate::plugin::settings_path(&plugin.id)
                    .is_some_and(|path| path.is_file());
                Row::new(
                    plugin.name.clone(),
                    Choice::PluginSettings(plugin.id.clone()),
                )
                .detail(format!("{} — {}", plugin.id, plugin.detail()))
                .tag(match mine {
                    true => "yours",
                    false => "default",
                })
            })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Plugins, rows));
    }

    /// What the plugin ships on the left, what you say about it on the right.
    ///
    /// Two panes rather than one file, because the question anybody has while
    /// writing this is *what could I say here* — and the answer is the thing
    /// on the left, which is why Sublime has shown its defaults beside your
    /// settings for twenty years. The left one is read-only: it is the
    /// plugin's, it is replaced whole every time the plugin updates, and
    /// editing it would be writing into the one file an update throws away.
    pub(super) fn edit_plugin_settings(&mut self, id: &str) {
        let Some(plugin) = crate::plugin::find(id) else {
            return self.say_bad(format!("there is no plugin called {id}"));
        };
        let Some(path) = crate::plugin::settings_path(id) else {
            return self.say_bad("there is nowhere to keep settings on this machine");
        };
        // Made the first time you ask, with the shape of the thing in it, so
        // that an empty file is not a blank page and a guess.
        if !path.exists() {
            if let Some(dir) = path.parent()
                && let Err(e) = std::fs::create_dir_all(dir)
            {
                return self.say_bad(format!("{}: {e}", dir.display()));
            }
            if let Err(e) = std::fs::write(&path, crate::plugin::settings_stub(plugin)) {
                return self.say_bad(format!("{}: {e}", path.display()));
            }
        }

        // The shipped half, in a buffer of its own that nothing types into.
        let shipped = self.new_scratch();
        if let Some(doc) = self.doc_mut(shipped) {
            doc.name = format!("{id} (shipped)");
            doc.rope = ropey::Rope::from_str(&crate::plugin::shipped_manifest(plugin));
            doc.language = lang::by_name("json").unwrap_or(LangId::PLAIN);
            doc.read_only = true;
            doc.mark_saved();
        }
        // Side by side, whatever the panes were doing before: this is a
        // comparison, and a comparison stacked one above the other is two
        // half-height windows on two long files.
        while self.ordinary_panes() > 1 {
            if let Some(at) = self.panes.iter().rposition(|p| p.dock.is_none()) {
                self.panes.remove(at);
            }
        }
        self.focus = self.beside_the_docks().unwrap_or(0);
        self.side_by_side = true;
        // Opened before the pane is pinned, since pinning is what stops a pane
        // being pointed at anything else.
        self.close_settings_panes();
        self.show(shipped);
        let left = self.focus;
        self.run(Cmd::SPLIT);
        self.open_path(&path);
        let yours = self.view().doc;
        // The manifest half shows the manifest and nothing else. Opening a
        // file into it would leave you comparing your settings against
        // something that is not what they are settings for.
        self.panes[left].pinned = true;
        self.settings_pair = Some((shipped, yours));
        self.say(format!(
            "{id}: what it ships on the left, what you say on the right"
        ));
    }

    /// Whether this buffer is one half of a plugin's settings.
    pub(super) fn is_settings_half(&self, id: DocId) -> bool {
        self.settings_pair
            .is_some_and(|(shipped, yours)| shipped == id || yours == id)
    }

    /// Put both halves of a plugin's settings away.
    ///
    /// One thing to look at is one thing to close. Shutting your own settings
    /// and being left with the manifest is being left with half a comparison
    /// and a buffer there is nothing to do with.
    ///
    /// Answers whether there was anything to close, so that the ordinary
    /// close can go on and do the ordinary thing when there was not.
    pub(super) fn close_settings_panes(&mut self) -> bool {
        let Some((shipped, yours)) = self.settings_pair.take() else {
            return false;
        };
        // The panes first, so that nothing is left pointing at a buffer that
        // is about to go.
        for id in [shipped, yours] {
            while let Some(at) = self
                .panes
                .iter()
                .position(|pane| pane.doc == id && pane.dock.is_none())
                .filter(|_| self.ordinary_panes() > 1)
            {
                self.panes.remove(at);
            }
        }
        for pane in &mut self.panes {
            pane.pinned = false;
        }
        self.focus = self.focus.min(self.panes.len().saturating_sub(1));
        // The manifest is a buffer textfold made to be read beside something
        // else, so it goes with it. Yours is a real file and stays open, the
        // way any file you were editing does.
        self.close_doc(shipped);
        if self.panes.iter().all(|pane| pane.doc != yours)
            && let Some(at) = self.beside_the_docks()
        {
            // Nothing is showing your settings any more — the pane that was
            // has gone — so put the pane that is left somewhere sensible.
            let fallback = self.most_recent().unwrap_or(yours);
            self.focus = at;
            if self.panes[at].doc == shipped {
                self.show(fallback);
            }
        }
        true
    }

    // ---- Installing one ----

    /// Everything textfold could fetch: a plugin that is here and needs a
    /// program, and a package sitting somewhere nobody has installed from yet.
    ///
    ///
    /// One list, because from where you are sitting "install pyright" and
    /// "install this plugin somebody gave me" are the same sentence. Which of
    /// the two a row happens to be is textfold's business.
    pub(super) fn open_install_picker(&mut self) {
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
    pub(super) fn open_uninstall_picker(&mut self) {
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
    pub(super) fn open_update_picker(&mut self) {
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
    pub(super) fn refreshed(&mut self, problems: Vec<String>) {
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

    pub(super) fn start_install(&mut self, id: &str) {
        let found = crate::pack::find(id, crate::pack::Sources::of(&self.config));
        let plan = found.and_then(|package| crate::pack::install(&package));
        self.start_plan(plan);
    }

    pub(super) fn start_uninstall(&mut self, id: &str) {
        self.start_plan(crate::pack::uninstall(id));
    }

    /// Set a plan going on a thread, and say what it is about to do.
    ///
    /// What it will do is said out loud before it does it. A plugin's
    /// installer runs programs on your machine, and the least an editor can do
    /// is name them on the way past rather than after the fact.
    pub(super) fn start_plan(&mut self, plan: Result<crate::pack::Plan, String>) {
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
    pub(super) fn on_package(&mut self, progress: crate::pack::Progress) {
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
    pub(super) fn open_environment_picker(&mut self) {
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
    pub(super) fn python_root(&self) -> Option<PathBuf> {
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
    pub(super) fn use_environment(&mut self, root: &Path) {
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

    pub(super) fn open_diagnostics_picker(&mut self) {
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

    pub(super) fn open_grep_picker(&mut self) {
        self.overlay = Overlay::Picker(Picker::new(Kind::Grep, Vec::new()));
    }

    pub(super) fn open_workspace_symbols(&mut self) {
        self.overlay = Overlay::Picker(Picker::new(Kind::WorkspaceSymbols, Vec::new()));
        self.ask_workspace_symbols("");
    }

    pub(super) fn picker_key(&mut self, key: Key) {
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
    pub(super) fn after_picker_moved(&mut self) {
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
    pub(super) fn choose(&mut self) {
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
            Choice::Process(pid) => self.attach_to(pid),
            Choice::Setting(which) => {
                self.toggle_setting(which);
                self.redraw_list(Self::open_settings_picker);
            }
            Choice::Plugin(id) => {
                self.toggle_plugin(&id);
                self.redraw_list(Self::open_plugins_picker);
            }
            Choice::PluginSettings(id) => self.edit_plugin_settings(&id),
            Choice::Install(id) => self.start_install(&id),
            Choice::Uninstall(id) => self.start_uninstall(&id),
        }
    }

    /// Build a list again, keeping what was typed into it and where you were.
    ///
    /// For the two lists you change things from rather than choose out of:
    /// the ticks have to be right afterwards, and closing the list to say so
    /// would mean opening it again for every switch you threw.
    pub(super) fn redraw_list(&mut self, again: fn(&mut Self)) {
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
pub(crate) fn action_rows(offered: &[(ServerId, Value)]) -> Vec<Row> {
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

pub(crate) fn setting_row(key: &'static str, about: &str, on: bool) -> Row {
    Row::new(about, Choice::Setting(key)).tag(if on { "on" } else { "off" })
}

impl Prompt {
    pub(super) fn insert(&mut self, c: char) {
        let at = self.byte_at(self.caret);
        self.input.insert(at, c);
        self.caret += 1;
    }

    pub(super) fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let at = self.byte_at(self.caret - 1);
        self.input.remove(at);
        self.caret -= 1;
    }

    pub(super) fn delete(&mut self) {
        let at = self.byte_at(self.caret);
        if at < self.input.len() {
            self.input.remove(at);
        }
    }

    pub(super) fn delete_word(&mut self) {
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

    pub(super) fn byte_at(&self, chars: usize) -> usize {
        self.input
            .char_indices()
            .nth(chars)
            .map(|(at, _)| at)
            .unwrap_or(self.input.len())
    }
}

/// `~/…` as a person writes it.
pub(crate) fn expand_path(text: &str) -> PathBuf {
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(text)
}
