//! Tools a plugin brought, and the panels a plugin puts up.
//!
//! A tool is a program run on the file in front of you, which is the half of
//! "an editor with plugins" that needs no plugin runtime at all. Nothing here
//! waits: the program is started on a thread and what it printed arrives on
//! the same channel the keyboard arrives on.

use super::*;

impl App {
    /// Run a tool on the file in front of you.
    ///
    /// Nothing here waits: the program is started on a thread and the answer
    /// arrives as an event, the same way a language server's does. A test run
    /// that takes a minute costs a minute of it running, not a minute of the
    /// editor being gone.
    pub(super) fn run_tool(&mut self, tool: &'static Tool) {
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
    pub(super) fn run_plugin_command(&mut self, command: &'static crate::plugin::Command) {
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
    pub(super) fn open_panel(&mut self, command: &'static crate::plugin::Command) -> bool {
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
    pub(super) fn panel_buffer(&mut self, command: &'static crate::plugin::Command) -> DocId {
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
    pub(super) fn pane_showing_docked(&self, id: DocId) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.doc == id && pane.dock.is_some())
    }

    /// Put a buffer in a pane pinned to an edge, and go there.
    ///
    /// Beside the middle rather than in it: the pane it opens next to keeps
    /// what it was showing, which is the whole point of a dock — you asked
    /// for a tree of files, not for the file you were reading to go away.
    pub(super) fn dock_panel(&mut self, id: DocId, dock: crate::view::Dock) {
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
    pub(super) fn start_tool(&mut self, tool: &'static Tool, id: DocId) -> bool {
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
    pub(super) fn on_tool(&mut self, done: crate::tool::Finished) {
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
    pub(super) fn take_tool_text(&mut self, tool: &'static Tool, done: crate::tool::Finished, why: &str) {
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
    pub(super) fn take_tool_problems(&mut self, tool: &'static Tool, done: &crate::tool::Finished) -> usize {
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
    pub(super) fn show_in_a_buffer(&mut self, name: &str, text: &str) {
        self.put_in_a_buffer(name, text, true);
    }

    /// The same, saying whether to go to it.
    ///
    /// A tool you just ran should show you what it printed: you asked half a
    /// second ago and you are waiting. A plugin's build finishing four minutes
    /// later should not take the cursor out of whatever you got on with in the
    /// meantime, which is why a plugin has to ask for that rather than getting
    /// it by default.
    pub(super) fn put_in_a_buffer(&mut self, name: &str, text: &str, focus: bool) {
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
    pub(super) fn tools_on_save(&mut self, doc: DocId) {
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

    pub(super) fn write_now(&mut self, to: Option<PathBuf>) {
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

    pub(super) fn save_all(&mut self) {
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

    pub(super) fn trim_trailing_whitespace(&mut self) {
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

    pub(super) fn reload(&mut self) {
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

    pub(super) fn do_reload(&mut self, id: DocId) {
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
    pub(super) fn take_from_disk(&mut self, id: DocId, why: Reread) -> anyhow::Result<bool> {
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
        // Only what differs, rather than the whole file. Every position in a
        // buffer is carried across an edit, and anything *inside* one lands at
        // the end of it — so replacing the whole buffer puts every cursor,
        // selection, diagnostic, bookmark and breakpoint on the last
        // character. On a file being appended to on a timer that is the view
        // jumping to the bottom over and over while somebody reads the middle
        // of it. See [`crate::doc::changed_span`].
        let Some((from, to, replacement)) = crate::doc::changed_span(&doc.rope, &text) else {
            doc.took_from_disk(stamp, kind);
            return Ok(false);
        };
        let sel = crate::text::Selections::single(Range::point(0));
        let edits = doc.apply_atomic(
            vec![crate::doc::Change::replace(from, to, replacement)],
            &sel,
        );
        doc.took_from_disk(stamp, kind);
        self.after_edit_to(id, edits, None);
        Ok(true)
    }

    pub(super) fn close(&mut self, force: bool) {
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
    pub(super) fn close_many(&mut self, keep: Keep) {
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
    pub(super) fn copy_path(&mut self, relative: bool) {
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

    pub(super) fn leave(&mut self, force: bool) {
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

    pub(super) fn step_buffer(&mut self, by: isize) {
        if self.docs.len() < 2 {
            return;
        }
        let here = self.view().doc;
        let at = self.docs.iter().position(|d| d.id == here).unwrap_or(0) as isize;
        let len = self.docs.len() as isize;
        let next = self.docs[(at + by).rem_euclid(len) as usize].id;
        self.show(next);
    }
}
