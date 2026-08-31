//! Changing the text, and moving about in it.
//!
//! Every one of these is a line or two: the work is in [`crate::edit`], which
//! knows nothing about the editor, and these are the handful of things the
//! editor has to do around it — take the document and the pane apart so both
//! can be borrowed, and tell everybody what changed afterwards. They are here
//! rather than in `mod` because there are sixty of them and they are all the
//! same shape, which makes them the easiest sixty lines to scroll past and the
//! easiest to lose something in.

use super::*;

impl App {
    pub(super) fn select_all(&mut self) {
        let (doc, view) = self.pair();
        edit::select_all(doc, view);
    }

    pub(super) fn select_line(&mut self) {
        let (doc, view) = self.pair();
        edit::select_line(doc, view);
        self.scroll_into_view();
    }

    pub(super) fn select_word(&mut self) {
        let (doc, view) = self.pair();
        edit::select_word(doc, view);
    }

    pub(super) fn add_cursor_above(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        edit::add_cursor_vertically(doc, view, tab_width, false);
        self.scroll_into_view();
    }

    pub(super) fn add_cursor_below(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        edit::add_cursor_vertically(doc, view, tab_width, true);
        self.scroll_into_view();
    }

    pub(super) fn add_cursor_at_next_match(&mut self) {
        let (doc, view) = self.pair();
        let found = edit::add_cursor_next_match(doc, view);
        if !found {
            self.say("no more of those");
        } else {
            self.scroll_into_view();
        }
    }

    pub(super) fn select_every_match(&mut self) {
        let (doc, view) = self.pair();
        let count = edit::select_all_matches(doc, view);
        if count > 1 {
            self.say(format!("{count} cursors"));
        }
    }

    pub(super) fn cursors_to_line_ends(&mut self) {
        let (doc, view) = self.pair();
        edit::cursors_to_line_ends(doc, view);
    }

    pub(super) fn collapse_cursors(&mut self) {
        self.view_mut().sel.collapse_to_primary();
        self.scroll_into_view();
    }

    pub(super) fn insert_newline(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        let mut edits = edit::newline(doc, view, tab_width);
        edits.extend(edit::newline_closing(doc, view, tab_width));
        self.after_edit(edits);
        self.completion = None;
    }

    pub(super) fn delete_backward(&mut self) {
        let tab_width = self.config.tab_width();
        let (doc, view) = self.pair();
        let edits = edit::delete_backward(doc, view, tab_width);
        self.after_edit(edits);
        self.refresh_completion();
    }

    pub(super) fn delete_forward(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_forward(doc, view);
        self.after_edit(edits);
    }

    pub(super) fn delete_word_backward(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_word_backward(doc, view);
        self.after_edit(edits);
    }

    pub(super) fn delete_word_forward(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_word_forward(doc, view);
        self.after_edit(edits);
    }

    pub(super) fn delete_to_line_start(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_to_line_start(doc, view);
        self.after_edit(edits);
    }

    pub(super) fn delete_to_line_end(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_to_line_end(doc, view);
        self.after_edit(edits);
    }

    pub(super) fn delete_line(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::delete_line(doc, view);
        self.after_edit(edits);
    }

    pub(super) fn duplicate_line(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::duplicate_line(doc, view);
        self.after_edit(edits);
    }

    pub(super) fn move_line_up(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::move_lines(doc, view, false);
        self.after_edit(edits);
    }

    pub(super) fn move_line_down(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::move_lines(doc, view, true);
        self.after_edit(edits);
    }

    pub(super) fn join_lines(&mut self) {
        let (doc, view) = self.pair();
        let edits = edit::join_lines(doc, view);
        self.after_edit(edits);
    }

    pub(super) fn toggle_comment(&mut self) {
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

    pub(super) fn change_case(&mut self, case: edit::Case) {
        let (doc, view) = self.pair();
        let edits = edit::change_case(doc, view, case);
        self.after_edit(edits);
    }

    /// Reorder the lines under the selection, or the whole file when there is
    /// nothing selected. See [`edit::shuffle_lines`].
    pub(super) fn shuffle_lines(&mut self, how: edit::Shuffle) {
        let (doc, view) = self.pair();
        let edits = edit::shuffle_lines(doc, view, how);
        if edits.is_empty() {
            return self.say("nothing to reorder");
        }
        self.after_edit(edits);
    }

    pub(super) fn paste(&mut self) {
        let text = self.system_clipboard();
        if text.is_empty() {
            self.say("nothing to paste");
        } else {
            let (doc, view) = self.pair();
            let edits = edit::insert_atomic(doc, view, &text);
            self.after_edit(edits);
        }
    }

    pub(super) fn motion(&mut self, motion: Motion, extend: bool) {
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

    pub(super) fn dismiss_popups(&mut self) {
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
    pub(super) fn refresh_completion(&mut self) {
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

    pub(super) fn scroll(&mut self, rows: isize) {
        let tab_width = self.config.tab_width();
        let at = self.focus.min(self.panes.len() - 1);
        let id = self.panes[at].doc;
        let Some(index) = self.docs.iter().position(|d| d.id == id) else {
            return;
        };
        let (docs, panes) = (&self.docs, &mut self.panes);
        view::scroll_by(&mut panes[at], &docs[index], tab_width, rows);
    }

    pub(super) fn centre(&mut self) {
        let line = text::line_of(&self.here().rope, self.view().cursor());
        let height = self.view().height();
        let view = self.view_mut();
        view.top = line.saturating_sub(height / 2);
        view.top_row = 0;
    }

    pub(super) fn on_tab(&mut self, out: bool) {
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

    pub(super) fn undo(&mut self, backwards: bool) {
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
    pub(super) fn system_clipboard(&mut self) -> String {
        if let Some(text) = crate::term::from_clipboard() {
            self.clipboard = text;
        }
        self.clipboard.clone()
    }

    pub(super) fn copy(&mut self, cut: bool) {
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

    pub(super) fn on_paste(&mut self, text: &str) {
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

    pub(super) fn escape(&mut self) {
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
}
