//! What a language server sends back.
//!
//! Every answer arrives here, on the same channel as the keyboard, and is
//! turned into something the editor holds: a list to pick from, a colour, a
//! note in the margin, an edit.

use super::*;

impl App {
    pub(super) fn on_lsp(&mut self, id: ServerId, message: Incoming) {
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
    pub(super) fn on_plugin(&mut self, id: HostId, message: Incoming) {
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
    pub(super) fn notice_the_cursor_moved(&mut self) {
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
    pub(super) fn tell_plugins_where_the_cursor_is(&mut self) {
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
    pub(super) fn settle_plugin_question(&mut self, answer: Value) {
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
    pub(super) fn sweep_plugin_question(&mut self) {
        if self.plugin_waiting.is_some() && matches!(self.overlay, Overlay::None) {
            self.settle_plugin_question(Value::Null);
        }
    }

    /// Put a plugin's question on the screen and remember who asked it.
    pub(super) fn ask_for_plugin(&mut self, id: HostId, request: Option<&Value>, overlay: Overlay) -> Answer {
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
    pub(super) fn plugin_means(&self, params: &Value) -> Result<DocId, String> {
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
    pub(super) fn plugin_edit(&mut self, params: &Value) -> Result<Value, String> {
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
    pub(super) fn plugin_hint(&mut self, id: HostId, params: &Value) -> Result<Value, String> {
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
    pub(super) fn hint_showing(&self) -> bool {
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
    pub(super) fn accept_hint(&mut self) {
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
    pub(super) fn drop_hint(&mut self, why: &str) {
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
    pub(super) fn plugin_panel(&mut self, id: HostId, params: &Value) -> Result<Value, String> {
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
                    .is_some_and(|p| p.id == wanted && p.owner.plugin() == Some(plugin.as_str()))
            })
            .map(|d| d.id)
        else {
            // Sent about a panel nobody has opened. Not an error a plugin can
            // do much about — it may have been closed while the message was
            // in flight — but worth saying rather than swallowing.
            return Err(format!("{wanted} is not open"));
        };

        let lines = params
            .get("lines")
            .and_then(Value::as_array)
            .ok_or("a panel needs some lines")?;
        Ok(json!({ "lines": self.write_panel(doc_id, lines) }))
    }

    /// Put a set of lines into a panel's buffer, colours and all.
    ///
    /// Shared by the plugin that sent them and by the debugger, which fills
    /// its own panel the same way for the same reason: everything about a
    /// panel that is fiddly — keeping the cursor on the row somebody was
    /// reading, not growing an undo history of every shape the panel has ever
    /// had — is fiddly identically for both, and a second copy of it would
    /// drift.
    ///
    /// Answers how many lines went in.
    pub(super) fn write_panel(&mut self, doc_id: DocId, rows: &[Value]) -> usize {
        let (text, spans, actions) = panel_lines(rows);

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
            return 0;
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
        lines
    }

    /// Move a panel to an edge, resize it, or take it off one.
    ///
    /// The manifest says where a panel goes by default, so that the editor can
    /// lay it out before the plugin has ever run. This is the other half: a
    /// plugin that wants to widen its tree because somebody has opened a deep
    /// directory, or to move to the bottom because what it is showing is a
    /// list rather than a tree, can say so while it is running.
    pub(super) fn plugin_dock(&mut self, id: HostId, params: &Value) -> Result<Value, String> {
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
                    .is_some_and(|p| p.id == wanted && p.owner.plugin() == Some(plugin.as_str()))
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
    pub(super) fn plugin_path(&self, params: &Value, key: &str) -> Result<PathBuf, String> {
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
    pub(super) fn plugin_file_create(&mut self, params: &Value) -> Result<Value, String> {
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
    pub(super) fn plugin_file_rename(&mut self, params: &Value) -> Result<Value, String> {
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
    pub(super) fn plugin_file_delete(&mut self, params: &Value) -> Result<Value, String> {
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
    pub(super) fn panel_action_at(&mut self, at: usize) -> bool {
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
        let (owner, id, action) = (panel.owner.clone(), panel.id.clone(), action.clone());
        match owner {
            crate::doc::Owner::Plugin(plugin) => {
                self.tell_panel(&plugin, "panel/action", json!({ "panel": id, "action": action }))
            }
            crate::doc::Owner::Debugger => self.debug_action(&action),
        }
        true
    }

    /// Where on the screen to open something that belongs beside the cursor.
    ///
    /// The caret's own cell where there is one. A pane with no caret in it —
    /// one that is not focused — falls back to its top corner, which is where
    /// a menu about that pane should go anyway.
    pub(super) fn cursor_on_screen(&self) -> (u16, u16) {
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
    pub(super) fn panel_wants(&self, key: Key) -> bool {
        self.here().panel.is_some() && self.keys.lookup(key).is_none_or(|cmd| cmd.writes())
    }

    /// Hand a keystroke to the plugin whose panel is in front of you.
    pub(super) fn send_panel_key(&mut self, key: Key) {
        let Some((plugin, id)) = self
            .here()
            .panel
            .as_ref()
            .and_then(|p| Some((p.owner.plugin()?.to_string(), p.id.clone())))
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
    pub(super) fn tell_panel(&mut self, plugin: &str, method: &str, params: Value) {
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
    pub(super) fn plugin_diagnostics(&mut self, id: HostId, params: &Value) -> Result<Value, String> {
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
    pub(super) fn plugin_name(&self, id: HostId) -> String {
        self.hosts
            .get(id)
            .map(|h| h.plugin.clone())
            .unwrap_or_else(|| "a plugin".into())
    }

    /// Anything the host machinery wanted to say, moved to the status line —
    /// it runs in the middle of other work and the screen is not its to write
    /// on.
    pub(super) fn take_plugin_problems(&mut self) {
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
    pub(super) fn plugin_asked(
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

    pub(super) fn on_notification(&mut self, id: ServerId, method: &str, params: Value) {
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

    pub(super) fn take_diagnostics(&mut self, id: ServerId, params: &Value) {
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
        if self.fixes.is_about_doc(doc_id) {
            self.fixes.forget();
        }
    }

    pub(super) fn on_response(&mut self, id: ServerId, request: i64, result: Result<Value, String>) {
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
                if let Ask::DebugAdapter { config, .. } | Ask::DebugLaunch { config, .. } = &ask {
                    // The server was asked to start a debugger and would not.
                    // Its own words, with the adapter named, because "cannot
                    // resolve classpath" on its own does not say who said it
                    // or what you were doing at the time.
                    let name = config.name.clone();
                    self.say_bad(format!("{name} could not start: {why}"));
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
            Ask::PrepareRename { doc, at } => self.take_prepare_rename(doc, at, value),
            Ask::Highlights { doc, at, version } => self.take_highlights(doc, at, version, value),
            Ask::Lenses { doc, version } => self.take_lenses(doc, version, value),
            Ask::PrepareCalls { doc, incoming } => self.take_prepare_calls(id, doc, incoming, value),
            Ask::Calls { incoming } => self.take_calls(incoming, value),
            Ask::SemanticTokens {
                doc,
                version,
                legend,
            } => self.take_semantic_tokens(doc, version, &legend, value),
            Ask::InlayHints { doc, version } => self.take_inlay_hints(doc, version, value),
            Ask::PulledDiagnostics { doc } => self.take_pulled_diagnostics(id, doc, value),
            Ask::DebugAdapter { config, root, file } => {
                self.take_debug_adapter(*config, root, file, value)
            }
            Ask::DebugLaunch { config, root, file } => {
                self.take_debug_launch(id, *config, root, file, value)
            }
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

    pub(super) fn take_completions(
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
    pub(super) fn resolve_selected(&mut self) {
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

    pub(super) fn suggestion_mut(&mut self, index: usize) -> Option<&mut Suggestion> {
        self.completion.as_mut()?.all.get_mut(index)
    }

    /// Put what came back into the suggestion it was about.
    ///
    /// Only the parts a server is allowed to leave out of the first answer.
    /// What goes in and over what was settled when the list was drawn, and a
    /// resolved item is not permitted to change it.
    pub(super) fn take_resolved_completion(&mut self, doc: DocId, index: usize, value: Value) {
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
    pub(super) fn accept_if_waiting(&mut self, index: usize) {
        if self.accept_when_resolved == Some(index) {
            self.accept_when_resolved = None;
            self.take_suggestion(index);
        }
    }

    pub(super) fn take_hover(&mut self, doc: DocId, at: usize, value: Value) {
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
        if let Some(fixes) = self.fixes.found.as_ref().filter(|f| f.doc == doc)
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

    pub(super) fn take_signature(&mut self, doc: DocId, at: usize, value: Value) {
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

    pub(super) fn take_goto(&mut self, doc: DocId, what: Goto, fallback: Option<String>, value: Value) {
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
    pub(super) fn go_to_target(&mut self, target: Target, line: usize, column: usize) {
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
    pub(super) fn take_class_file(&mut self, uri: String, line: usize, column: usize, value: Value) {
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

    pub(super) fn take_references(&mut self, value: Value) {
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

    /// What the server said about whether this can be renamed at all.
    ///
    /// An answer of nothing is a no — and a clear one, given before anybody
    /// has typed a new name. Anything else opens the box, with the server's
    /// own idea of what is being renamed in it where it gave one: for a
    /// Python method the placeholder is `run`, where the word under the cursor
    /// might be `self.run`.
    pub(super) fn take_prepare_rename(&mut self, doc: DocId, at: usize, value: Value) {
        if self.view().doc != doc || self.view().cursor() != at {
            // The cursor has moved on. Renaming what is under it now would be
            // renaming something nobody asked about.
            return;
        }
        if value.is_null() {
            return self.say_bad("that cannot be renamed");
        }
        // Three shapes are allowed: a range, `{ range, placeholder }`, or
        // `{ defaultBehavior: true }` meaning "yes, work it out yourself".
        let placeholder = value
            .get("placeholder")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                let range = value.get("range").unwrap_or(&value);
                let doc = self.doc(doc)?;
                let (from, to) = (range.get("start")?, range.get("end")?);
                let (l1, c1) = crate::lsp::point_of(from)?;
                let (l2, c2) = crate::lsp::point_of(to)?;
                let start = doc.char_at_lsp_point(l1, c1);
                let end = doc.char_at_lsp_point(l2, c2);
                (end > start).then(|| doc.slice(Range::new(start, end)))
            });
        self.open_prompt(PromptKind::Rename);
        if let (Some(name), Overlay::Prompt(prompt)) = (placeholder, &mut self.overlay) {
            prompt.caret = name.chars().count();
            prompt.input = name;
        }
    }

    /// Everywhere in this file the thing under the cursor is mentioned.
    pub(super) fn take_highlights(&mut self, doc: DocId, at: usize, version: i32, value: Value) {
        // An answer about a cursor that has moved, or about a file that has
        // been typed in since, is a set of ranges around the wrong words.
        if self.view().doc != doc || self.view().cursor() != at {
            return;
        }
        let Some(document) = self.doc(doc) else { return };
        if document.version != version {
            return;
        }
        let Value::Array(items) = &value else {
            if let Some(document) = self.doc_mut(doc) {
                document.said.highlights.clear();
            }
            return;
        };
        let ranges: Vec<Range> = items
            .iter()
            .filter_map(|item| range_from_lsp(item.get("range")?, document))
            .collect();
        // One highlight is the word the cursor is already on, which is not
        // worth lighting anything up for.
        let ranges = if ranges.len() > 1 { ranges } else { Vec::new() };
        if let Some(document) = self.doc_mut(doc) {
            document.said.highlights = ranges;
        }
    }

    /// The notes a server offers about the lines of this file.
    pub(super) fn take_lenses(&mut self, doc: DocId, version: i32, value: Value) {
        let Some(document) = self.doc(doc) else { return };
        if document.version != version {
            return;
        }
        let Value::Array(items) = &value else { return };
        let lenses: Vec<crate::doc::Lens> = items
            .iter()
            .filter_map(|item| {
                let range = item.get("range")?;
                let (line, _) = crate::lsp::point_of(range.get("start")?)?;
                let command = item.get("command");
                // A lens whose title the server has not worked out yet needs a
                // second request to resolve, and one with nothing to say is
                // not a note. Both are simply left out.
                let label = command?.get("title")?.as_str()?.trim().to_string();
                if label.is_empty() {
                    return None;
                }
                Some(crate::doc::Lens {
                    at: text::line_start(&document.rope, line.min(document.len_lines() - 1)),
                    label,
                    command: command.cloned(),
                })
            })
            .collect();
        if let Some(document) = self.doc_mut(doc) {
            document.said.lenses = lenses;
        }
    }

    /// The server named what is under the cursor; now ask who calls it.
    pub(super) fn take_prepare_calls(&mut self, id: ServerId, doc: DocId, incoming: bool, value: Value) {
        if self.view().doc != doc {
            return;
        }
        let Some(item) = value.as_array().and_then(|items| items.first()).cloned() else {
            return self.say("nothing here that has callers");
        };
        self.lsp.calls(id, item, incoming);
    }

    /// The calls themselves, as a list to walk.
    pub(super) fn take_calls(&mut self, incoming: bool, value: Value) {
        let Value::Array(items) = &value else {
            return self.say("nothing came back");
        };
        let project = self.project.clone();
        let rows: Vec<Row> = items
            .iter()
            .filter_map(|call| {
                // Incoming calls name the caller in `from`; outgoing name the
                // callee in `to`. Everything else about them is the same.
                let item = call.get(if incoming { "from" } else { "to" })?;
                let name = item.get("name")?.as_str()?.to_string();
                let path = crate::lsp::path_of(item.get("uri")?.as_str()?)?;
                let (line, column) = crate::lsp::point_of(item.get("selectionRange")?.get("start")?)?;
                Some(
                    Row::new(
                        name,
                        Choice::There {
                            path: path.clone(),
                            line,
                            column,
                        },
                    )
                    .detail(format!("{}:{}", short(&path, &project), line + 1)),
                )
            })
            .collect();
        if rows.is_empty() {
            return self.say(match incoming {
                true => "nothing calls it",
                false => "it calls nothing",
            });
        }
        self.view_mut().mark_jump();
        self.overlay = Overlay::Picker(Picker::new(Kind::Calls, rows));
    }

    /// The colours a server worked out for this file.
    ///
    /// The answer is a flat list of numbers, five to a token, each one
    /// relative to the one before it: how many lines down, how far across,
    /// how long, which type, which modifiers. Relative because a file has a
    /// great many tokens and the difference between two of them fits in a
    /// small number — which also means one bad number puts every colour after
    /// it in the wrong place, so anything that does not add up stops the walk
    /// rather than being guessed at.
    pub(super) fn take_semantic_tokens(&mut self, doc: DocId, version: i32, legend: &[String], value: Value) {
        let Some(document) = self.doc(doc) else { return };
        if document.version != version {
            // The file has been typed in since it was asked. The positions
            // would be a few characters out, which is worse than the colours
            // being a moment late.
            return;
        }
        let Some(data) = value.get("data").and_then(Value::as_array) else {
            return;
        };
        let mut spans = Vec::new();
        let (mut line, mut column) = (0u64, 0u64);
        // Five at a time, as arrays rather than as slices, so the five names
        // below are the compiler's business rather than a length check.
        let (tokens, _) = data.as_chunks::<5>();
        for token in tokens {
            let Some([down, across, length, kind, _mods]) = token
                .iter()
                .map(Value::as_u64)
                .collect::<Option<Vec<u64>>>()
                .and_then(|it| <[u64; 5]>::try_from(it).ok())
            else {
                break;
            };
            line += down;
            // The column is relative to the token before it only while both
            // are on the same line.
            column = if down == 0 { column + across } else { across };
            let Some(role) = legend
                .get(kind as usize)
                .and_then(|name| crate::theme::semantic_role(name))
            else {
                continue;
            };
            let start = document.char_at_lsp_point(line as usize, column as usize);
            let end = document.char_at_lsp_point(line as usize, (column + length) as usize);
            if end > start {
                spans.push((Range::new(start, end), role));
            }
        }
        if let Some(document) = self.doc_mut(doc) {
            document.said.semantic = spans;
        }
    }

    /// The types and parameter names the file does not say.
    pub(super) fn take_inlay_hints(&mut self, doc: DocId, version: i32, value: Value) {
        let Some(document) = self.doc(doc) else { return };
        if document.version != version {
            return;
        }
        let Value::Array(items) = &value else { return };
        let hints: Vec<crate::doc::Inlay> = items
            .iter()
            .filter_map(|hint| {
                let (line, column) = crate::lsp::point_of(hint.get("position")?)?;
                // The label is either a string or a list of pieces with a
                // string in each.
                let label = match hint.get("label")? {
                    Value::String(text) => text.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(|part| part.get("value")?.as_str())
                        .collect(),
                    _ => return None,
                };
                let label = label.trim().to_string();
                if label.is_empty() {
                    return None;
                }
                Some(crate::doc::Inlay {
                    at: document.char_at_lsp_point(line, column),
                    text: label,
                })
            })
            .collect();
        if let Some(document) = self.doc_mut(doc) {
            document.said.inlays = hints;
        }
    }

    /// What is wrong with a file, from a server that waits to be asked.
    pub(super) fn take_pulled_diagnostics(&mut self, id: ServerId, doc: DocId, value: Value) {
        // `unchanged` means "the same as last time you asked", which is an
        // answer about a list we already have.
        if value.get("kind").and_then(Value::as_str) == Some("unchanged") {
            return;
        }
        let Some(items) = value.get("items").cloned() else {
            return;
        };
        let App { docs, lsp, .. } = self;
        let Some(document) = docs.iter().find(|d| d.id == doc) else {
            return;
        };
        let uri = document.path.as_deref().map(crate::lsp::uri_of);
        // Shaped like the notification, so that both roads lead to the one
        // piece of code that turns a server's opinion into ours.
        let params = json!({ "uri": uri, "diagnostics": items });
        let Some(fresh) = lsp.diagnostics_for(id, &params, document) else {
            return;
        };
        if let Some(document) = docs.iter_mut().find(|d| d.id == doc) {
            document
                .diagnostics
                .retain(|d| d.told != crate::doc::Told::Server(id.0));
            document.diagnostics.extend(fresh);
        }
    }

    pub(super) fn take_symbols(&mut self, doc: DocId, value: Value) {
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

    pub(super) fn take_workspace_symbols(&mut self, going: Option<String>, value: Value) {
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

    pub(super) fn take_format(&mut self, doc: DocId, version: i32, value: Value) {
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
    pub(super) fn take_code_actions(&mut self, id: ServerId, doc: DocId, at: usize, value: Value) {
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
    pub(super) fn show_actions(&mut self, offered: Vec<(ServerId, Value)>) {
        let rows = action_rows(&offered);
        if rows.is_empty() {
            return self.say("nothing to offer here");
        }
        self.overlay = Overlay::Picker(Picker::new(Kind::Actions, rows));
    }

    /// Do what a code action says. Some carry their edit; some carry a
    /// command for the server to run; and some carry neither until asked.
    pub(super) fn do_code_action(&mut self, id: ServerId, action: Value) {
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
    pub(super) fn apply_workspace_edit(&mut self, edit: &Value) -> usize {
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

    pub(super) fn apply_edits_to_path(&mut self, path: &Path, edits: &[Value]) -> usize {
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
    pub(super) fn apply_edits_to(&mut self, id: DocId, edits: &[Value]) -> usize {
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
    pub(super) fn apply_changes_to(&mut self, id: DocId, mut changes: Vec<crate::doc::Change>) -> usize {
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
pub(crate) fn panel_lines(
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
pub(crate) fn panel_role(name: &str) -> Option<crate::theme::Role> {
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
pub(crate) fn server_rows(plugin: &crate::plugin::Plugin, on: impl Fn(&str) -> bool) -> Vec<Row> {
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
pub(crate) const MIN_DOCK: u16 = 4;
pub(crate) const MIN_MIDDLE_ROOM: u16 = 20;

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
    pub(super) fn slice(
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
pub(crate) fn byte_at(chars: &[(usize, char)], text: &str, at: usize) -> usize {
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
pub(crate) fn markup_lines(value: Option<&Value>, here: LangId) -> Vec<DocLine> {
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

    pub(super) fn text_of(value: &Value, out: &mut Vec<Block>) {
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
pub(crate) fn push_markdown(lines: &mut Vec<DocLine>, text: &str, here: LangId) {
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
pub(crate) fn push_code(lines: &mut Vec<DocLine>, code: &str, lang: Option<LangId>) {
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
pub(crate) fn code_spans(code: &str, lang: LangId) -> Option<Vec<Spans>> {
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
    pub(super) fn label(&self, project: &Path) -> String {
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

pub(crate) fn locations(value: &Value) -> Vec<(Target, usize, usize)> {
    pub(super) fn one(value: &Value, out: &mut Vec<(Target, usize, usize)>) {
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
pub(crate) fn suggestion_from(item: &Value, doc: &Document, at: usize) -> Option<Suggestion> {
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
pub(crate) fn strip_snippet(text: &str) -> String {
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
pub(crate) fn completion_role(n: u64) -> Role {
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
pub(crate) fn completion_kind(n: u64) -> &'static str {
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
pub(crate) fn symbol_kind(n: u64) -> &'static str {
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
pub(crate) fn collect_symbols(value: &Value, doc: &Document, depth: usize, out: &mut Vec<Row>) {
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
