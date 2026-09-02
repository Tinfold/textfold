//! Files: writing them, reading them again, closing them — and what was
//! open last time.
//!
//! Saving is the interesting one. Every step before the bytes go down is a
//! round trip to a language server, so a save with formatting or fixes turned
//! on is a queue of questions that ends in a write rather than a write.

use super::*;

impl App {
    /// Note that the tabs have moved on, so the session gets written soon.
    pub(super) fn session_changed(&mut self) {
        self.session_dirty = true;
    }

    /// What is open now, as something that can be written down.
    ///
    /// A buffer with no file behind it is left out: there is nothing to open
    /// again, and remembering the *name* of an empty untitled buffer would
    /// bring back a tab with nothing in it.
    pub(super) fn session(&self) -> crate::session::Session {
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
    pub(super) fn apply_session(&mut self, session: &crate::session::Session, asked: bool) -> usize {
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
    pub(super) fn drop_untouched_scratch(&mut self, keep: DocId) {
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
    pub(super) fn close_doc(&mut self, id: DocId) {
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

    pub(super) fn most_recent(&self) -> Option<DocId> {
        self.docs
            .iter()
            .map(|d| d.id)
            .max_by_key(|id| self.seen.get(id).copied().unwrap_or(0))
    }


    /// Write the file, reformatting it first if that is what you have asked
    /// for.
    ///
    /// This may not write anything itself. Every step before the bytes go
    /// down is a round trip to a language server — the fixes it would make,
    /// and then the formatter — so a save with either of those turned on is a
    /// queue of questions that ends in a write. [`App::write_now`] is the end
    /// of that queue, and is separate so that the save which follows a format
    /// does not ask for another one.
    pub(super) fn save(&mut self, to: Option<PathBuf>) {
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
        // Then everything that lays the file out, in whatever order you have
        // said they go in — the same list Alt-Shift-F runs, so that saving a
        // file and formatting it by hand cannot leave it looking different.
        // After the fixes, which put text in, and before the write, which is
        // the point of all this.
        steps.extend(self.format_steps(id, self.config.format_on_save()));
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
    pub(super) fn fix_all(&mut self, kinds: &[String]) {
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
    pub(super) fn format_and_fix(&mut self) {
        if self.before_save.is_some() {
            return;
        }
        let id = self.view().doc;
        let both = [SOURCE_FIX_ALL.to_string(), SOURCE_ORGANIZE_IMPORTS.to_string()];
        let mut steps = self.fix_steps(id, &both);
        if steps.is_empty() {
            return self.format();
        }
        steps.extend(self.format_steps(id, true));
        self.begin(id, steps, false, None);
    }

    /// One step per kind of fix per server that can answer for one.
    pub(super) fn fix_steps(&self, doc: DocId, kinds: &[String]) -> Vec<Step> {
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

    /// Whether one of these steps has claimed the last word on the layout,
    /// so that the language server's own formatter should not run at all.
    ///
    /// Taking the step out rather than sorting it last, which is what
    /// `formatter_order` would do. Sorting cannot express this: the default
    /// order already puts the server last, precisely so it can tidy what a
    /// tool left behind, and being tidied afterwards is the thing being
    /// refused here. A file laid out by `prettier` and then tidied by
    /// tsserver is laid out like neither of them, and the next person to run
    /// `prettier` alone puts it all back.
    ///
    /// See [`crate::plugin::Tool::instead_of_lsp`], and
    /// [`crate::plugin::Tool::supersedes_lsp`] for why a formatter nobody
    /// installed does not get a say.
    pub(super) fn lsp_is_superseded(steps: &[Step]) -> bool {
        steps.iter().any(|step| match step {
            Step::Rewrite(tool) => tool.supersedes_lsp(),
            _ => false,
        })
    }

    /// Everything that lays this file out, in the order it should happen.
    ///
    /// One list, and both doors into formatting come through it: `format`,
    /// and the formatting half of a save. They used to be different — the
    /// command asked the language server and nothing else, while the save
    /// also ran whatever tools a plugin had brought — so a project whose
    /// formatter was `prettier` got one answer from Ctrl-S and a different
    /// one from Alt-Shift-F, which is the sort of difference you only notice
    /// as a diff you did not write.
    ///
    /// `lsp` says whether the language server's own formatter is one of them.
    /// It always is for the command, because asking to format a file is
    /// asking everything that can; on a save it is what `format_on_save`
    /// decides. A tool is not asked twice: it carries its own `on_save`, and
    /// having said there when to run it does not need a second switch.
    pub(super) fn format_steps(&self, doc: DocId, lsp: bool) -> Vec<Step> {
        // A tool is a program run on a file, so a buffer that is not a file
        // yet has none. It was already skipped at the far end — `start_tool`
        // wants a path — and saying so here is what makes "nothing here
        // knows how to format this file" true rather than nearly true.
        let saved = self.doc(doc).is_some_and(|d| d.path.is_some());
        let mut steps: Vec<Step> = match saved {
            true => self.rewriters(doc).into_iter().map(Step::Rewrite).collect(),
            false => Vec::new(),
        };
        // Only where there is a server that would answer, and only where
        // nothing else has claimed the last word. A dead step costs nothing at
        // the far end — `advance` walks past one that will not start — but it
        // is the difference between "nothing here formats this file" and
        // silence, and that is worth saying.
        if lsp
            && !Self::lsp_is_superseded(&steps)
            && let Some(open) = self.doc(doc)
            && self.lsp.can(open, "documentFormattingProvider")
        {
            steps.push(Step::Format);
        }
        self.in_formatter_order(&mut steps);
        steps
    }

    /// Put the formatters in whatever order the settings ask for.
    ///
    /// A stable sort, and that is the whole design: `formatter_order` names
    /// an order rather than a set, so everything it says nothing about keeps
    /// the order it already had and sorts after everything it does name.
    /// Naming one formatter moves that one; it cannot quietly switch off a
    /// formatter you forgot to write down.
    pub(super) fn in_formatter_order(&self, steps: &mut [Step]) {
        steps.sort_by_key(|step| match step {
            Step::Rewrite(tool) => self.config.formatter_rank([&tool.id, &tool.name]),
            Step::Format => self.config.formatter_rank(["lsp", "language-server"]),
            // Not a formatter. Nothing sorts a fix, and nothing should: the
            // fixes have already run by the time these are put in order.
            Step::Fix(..) => usize::MAX,
        });
    }

    /// Whether anything at all would reformat this file, for the menu row
    /// that offers to.
    pub(super) fn can_format(&self, doc: DocId) -> bool {
        !self.format_steps(doc, true).is_empty()
    }

    /// The tools a plugin brought that rewrite the file: `black`, `gofmt`,
    /// `prettier`.
    ///
    /// `on_save` is what picks them out, and it is doing double duty on
    /// purpose: a tool that rewrites the whole file every time you save it is
    /// the formatter for that language, whatever else it calls itself, and
    /// one you have to ask for by name is not. So the same flag that puts it
    /// in the save is what makes `format` know about it.
    pub(super) fn rewriters(&self, doc: DocId) -> Vec<&'static Tool> {
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

    pub(super) fn begin(&mut self, doc: DocId, steps: Vec<Step>, write: bool, to: Option<PathBuf>) {
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
    pub(super) fn advance(&mut self) {
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
    pub(super) fn start_formatter(&mut self, id: DocId) -> bool {
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
    pub(super) fn take_source_actions(&mut self, server: ServerId, doc: DocId, version: i32, value: Value) {
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
    pub(super) fn check_before_save(&mut self) {
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
    pub(super) fn waiting_on(&self, step: &Step) -> bool {
        self.before_save
            .as_ref()
            .is_some_and(|b| b.doing.as_ref() == Some(step))
    }
}
