//! The mouse.
//!
//! Everything reachable by keyboard is reachable by mouse, because half the
//! people who open an editor reach for the mouse first and there is no good
//! reason to make them wrong. Click to put the cursor somewhere, drag to
//! select, double click for a word, triple click for a line. The line numbers
//! select lines. The tabs switch and close files. The things in the status bar
//! are buttons: the language name opens the language list, the position opens
//! "go to line", the count of problems opens the problem list.

use super::*;

impl App {
    pub(super) fn on_mouse(&mut self, event: MouseEvent) {
        if !self.mouse_on {
            return;
        }
        let (column, row) = (event.column, event.row);
        // Every kind, not only a move: a click is where the pointer is too,
        // and a terminal that reports no motion at all — several do, unless
        // asked — would otherwise never light anything up.
        self.pointer = Some((column, row));
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(column, row, event.modifiers),
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(column, row),
            MouseEventKind::Up(_) => self.drag = None,
            MouseEventKind::ScrollUp => self.wheel(column, row, -3),
            MouseEventKind::ScrollDown => self.wheel(column, row, 3),
            MouseEventKind::ScrollLeft => self.pan(column, row, -4),
            MouseEventKind::ScrollRight => self.pan(column, row, 4),
            MouseEventKind::Moved => self.mouse_moved(column, row),
            MouseEventKind::Down(MouseButton::Right) => self.right_click(column, row),
            MouseEventKind::Down(MouseButton::Middle) => {
                if let Some(at) = self.position_at(column, row) {
                    self.place_cursor(at, false, false);
                    self.run(Cmd::PASTE);
                }
            }
            _ => {}
        }
    }

    pub(super) fn click(&mut self, column: u16, row: u16, mods: KeyModifiers) {
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
        // Clicking a tab is having read the label and decided; clicking
        // anything else is having left it.
        self.tip = None;
        self.click_away_from_suggestions(column, row);

        // A divider, before anything else looks at where the click landed. It
        // is chrome rather than text, and dragging it is the only thing it
        // does.
        if let Some(pane) = self.grip_at(column, row) {
            self.drag = Some(Drag::Divider { pane });
            return;
        }

        // The context menu is on top of everything, including the list.
        if let Overlay::Menu(m) = &mut self.overlay {
            let area = m.area;
            // What was clicked, rather than what was highlighted. They are
            // usually the same row, and assuming so meant a click on a divider
            // ran the highlight instead — which, in a menu that opens with
            // "Cut" lit, cut your selection.
            let chosen = hits(area, column, row).then(|| m.at((row - area.y) as usize + m.scroll));
            self.overlay = Overlay::None;
            if let Some(Some(action)) = chosen {
                self.do_menu(action);
            }
            return;
        }

        // A hover you can see is a hover you can click into: clicking it puts
        // the keyboard in it rather than moving the cursor to whatever text is
        // behind it, and Ctrl-clicking a name in it goes looking for that name
        // the way Ctrl-clicking a name in the code does.
        if let Some(hover) = &mut self.hover
            && matches!(self.overlay, Overlay::None)
            && hits(hover.outer, column, row)
        {
            let link = hover.link_at(column, row);
            hover.focused = true;
            hover.pointer = Some((column, row));
            if mods.contains(KeyModifiers::CONTROL)
                && let Some(link) = link
            {
                self.hover = None;
                return self.look_up(&link.word);
            }
            // Otherwise it is text, and clicking text is where a selection
            // starts — the same gesture as in the editor, because from where
            // you are sitting it is the same thing.
            if let Some(spot) = hover.spot_at(column, row) {
                hover.select = Some((spot, spot));
                if count >= 2 {
                    hover.take_word();
                } else {
                    self.drag = Some(Drag::Popup);
                }
            }
            return;
        }

        // A list on top of everything gets the click, and a click outside it
        // closes it — which is what clicking away from a menu means.
        if let Overlay::Picker(picker) = &mut self.overlay {
            let area = picker.area;
            if row >= area.y
                && row < area.y + area.height
                && column >= area.x
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

        // The tabs. The ‹ › at the ends first: each one is drawn over a column
        // that belongs to the tab beneath it, and the arrow is what is on the
        // screen there.
        if let Some(to) = self
            .hits.nudges
            .iter()
            .find(|(area, _)| hits(*area, column, row))
            .map(|(_, to)| *to)
        {
            self.tab_scroll = to;
            return;
        }
        if let Some((id, close)) = self
            .hits.tabs
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
                // And it is now held, so moving the pointer carries it along
                // the row. A press that never moves is just a click, because
                // a tab that has not gone anywhere has not been reordered.
                self.drag = Some(Drag::Tab {
                    id,
                    at: (column, row),
                    stepped: Instant::now(),
                });
            }
            return;
        }

        // The status bar.
        if let Some(cmd) = self
            .hits.status
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
                    // A row that has never been under the cursor has never
                    // been asked about, so this goes through the same wait as
                    // a Tab does rather than dropping the import.
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
        // The very left of the margin is the debugger's column: clicking it
        // puts a breakpoint on that line, which is the gesture every editor
        // with a debugger in it has. It is one column wide and it is the
        // blank one the line number is padded with, so it costs the numbers
        // nothing and it is where the pointer already goes.
        let rule = view.frame.x + crate::ui::rule_width(self.panes.len() as u16);
        if view.gutter > 0
            && column == rule
            && self.doc(view.doc).is_some_and(|d| d.panel.is_none())
            && let Some(at) = self.position_at(view.area.x, row)
        {
            let id = view.doc;
            let line = self
                .doc(id)
                .map(|d| crate::text::line_of(&d.rope, at))
                .unwrap_or(0);
            if let Some(doc) = self.doc_mut(id) {
                doc.toggle_breakpoint(line);
            }
            self.tell_debugger_about_breakpoints();
            return;
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
        // The "⋯ 12 lines" at the end of a folded row is a button: clicking it
        // brings the lines back. It is the gesture the little triangle in
        // every other editor's margin is, put where the mark actually is.
        if self.click_on_fold_mark(column, at) {
            return;
        }
        // A panel is a plugin's own buffer, and the parts of it the plugin
        // marked as doing something do it when you click them — which is what
        // "clickable" has meant on a screen for forty years.
        //
        // The cursor goes where you clicked only if you are still looking at
        // the same buffer afterwards. Half the actions a panel has are "take
        // me somewhere": clicking a frame in the debugger's stack opens a file
        // and puts the cursor on the line, and following that with the offset
        // of the row you clicked in the *panel* would land wherever that
        // happens to be in the file — which for a file shorter than the panel
        // is past the end of it.
        if self.panel_action_at(at) {
            // And the cursor stays where it was. What was clicked is a button:
            // it opened a file, stepped a program, folded a tree. Leaving a
            // text caret sitting in the middle of its label afterwards says
            // that something was *edited* there, in a buffer that cannot be
            // typed into — and it moves the one thing the keyboard uses to
            // pick a row, so a click would quietly change what Enter means
            // next. Clicking the panel anywhere that is *not* a button still
            // places it, which is how you get the caret somewhere on purpose.
            return;
        }
        // Ctrl-click is what every editor has taught people goes to the
        // definition of the thing under the pointer.
        if mods.contains(KeyModifiers::CONTROL) {
            self.place_cursor(at, false, false);
            return self.run(Cmd::GOTO_DEFINITION);
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
                // Alt is "another cursor" when it is a click and "the same
                // columns on every line" when it is a drag, which is the same
                // idea at two lengths: both end with a cursor on each of
                // several lines, and one of them saves you the clicks.
                let alt = mods.contains(KeyModifiers::ALT);
                self.place_cursor(at, mods.contains(KeyModifiers::SHIFT), alt);
                self.drag = Some(match alt {
                    true => Drag::Block { anchor: at },
                    false => Drag::Text,
                });
            }
        }
        self.dismiss_popups();
    }

    /// The pointer went past, without any button held.
    ///
    /// Three things want to know: a menu, whose highlight follows the pointer
    /// the way every menu's does; a hover, which lights up the name under the
    /// pointer and stays open while you are inside it; and the editor itself,
    /// where sitting still over a word is a question.
    pub(super) fn mouse_moved(&mut self, column: u16, row: u16) {
        if let Overlay::Menu(menu) = &mut self.overlay {
            let area = menu.area;
            if hits(area, column, row) {
                menu.point_at((row - area.y) as usize + menu.scroll);
            }
            return;
        }
        if let Some(hover) = &mut self.hover
            && hits(hover.outer, column, row)
        {
            // Inside the box. It stays, whether or not it has the keyboard,
            // because a box that vanished as you reached for it could never be
            // clicked on at all.
            hover.pointer = Some((column, row));
            self.resting = None;
            return;
        }
        // A label already up for the thing under the pointer stays up, and
        // wandering about inside that thing does not start the clock again:
        // a tab is several cells wide and it is one tab all the way across.
        if let Some(tip) = &self.tip {
            if hits(tip.about, column, row) {
                return;
            }
            self.tip = None;
        }
        if let Some(hover) = &mut self.hover {
            hover.pointer = None;
        }
        // Sitting still over a word is a question. Moving is not.
        match self.resting {
            Some((_, c, r)) if c == column && r == row => {}
            _ => {
                self.resting = Some((Instant::now(), column, row));
                // A hover you have asked to read stays while you move about;
                // one that appeared on its own goes as soon as you look away.
                if self.hover.as_ref().is_some_and(|h| !h.focused) {
                    self.hover = None;
                }
            }
        }
    }

    /// The right button asks what can be done here.
    ///
    /// On a tab that is about the file; anywhere in the text it is about the
    /// code under the pointer. Clicking inside a selection keeps it, because
    /// "select this, then right-click, then copy" is the whole reason the menu
    /// is there and moving the cursor first would throw the selection away.
    /// Close the list of suggestions, unless the click landed on it.
    ///
    /// Clicking somewhere else is going somewhere else, and a list of
    /// completions for a word you have left is worse than no list at all: it
    /// still owns Tab and Enter, so the next thing you press finishes a word
    /// that is no longer under the cursor. Every editor closes it on a click
    /// away, which is why nobody ever thinks about this until one does not.
    ///
    /// An empty list counts as not clicked on. It is not drawn, so its last
    /// known place on the screen is not a place, and a click there is a click
    /// on the text underneath.
    pub(super) fn click_away_from_suggestions(&mut self, column: u16, row: u16) {
        let on_the_list = self
            .completion
            .as_ref()
            .is_some_and(|list| !list.is_empty() && hits(list.area, column, row));
        if !on_the_list {
            self.completion = None;
        }
    }

    pub(super) fn right_click(&mut self, column: u16, row: u16) {
        // Asking what can be done here is leaving whatever word you were
        // part-way through, so the suggestions go — including where the menu
        // is about to be drawn over the top of them.
        self.completion = None;
        self.tip = None;

        // A menu already open is closed by a second right click, the way a
        // second press of any key that opens something closes it.
        if matches!(self.overlay, Overlay::Menu(_)) {
            self.overlay = Overlay::None;
            return;
        }
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        if let Some(id) = self
            .hits.tabs
            .iter()
            .find(|(area, _, _)| hits(*area, column, row))
            .map(|(_, id, _)| *id)
        {
            let menu = self.tab_menu(id, (column, row));
            self.overlay = Overlay::Menu(menu);
            return;
        }
        let Some(pane) = self.pane_at(column, row) else {
            return;
        };
        if pane != self.focus {
            self.focus = pane;
            self.completion = None;
        }
        self.dismiss_popups();
        if let Some(at) = self.position_at(column, row) {
            let inside = self
                .view()
                .sel
                .ranges()
                .iter()
                .any(|range| !range.is_empty() && range.start() <= at && at < range.end());
            if !inside {
                self.place_cursor(at, false, false);
            }
        }
        // A panel is a plugin's buffer, and the editor's own menu for it is
        // Cut and Paste greyed out — true, and no use to anybody. The gesture
        // goes to the plugin instead, with where it landed and whatever it had
        // marked there, so it can put up a menu of its own.
        if let Some((plugin, panel)) = self
            .here()
            .panel
            .as_ref()
            .and_then(|p| Some((p.owner.plugin()?.to_string(), p.id.clone())))
        {
            let at = self.view().cursor();
            let (line, column) = self.here().point_at_char(at);
            let action = self
                .here()
                .panel
                .as_ref()
                .and_then(|p| {
                    p.actions
                        .iter()
                        .find(|(range, _)| range.start() <= at && at < range.end())
                })
                .map(|(_, action)| action.clone());
            return self.tell_panel(
                &plugin,
                "panel/context",
                json!({ "panel": panel, "line": line, "column": column, "action": action }),
            );
        }
        let menu = self.text_menu((column, row));
        self.overlay = Overlay::Menu(menu);
    }

    /// Put the cursor somewhere. `extend` keeps the anchor, `add` leaves the
    /// cursors that were already there — Alt-click, which is how you get a
    /// second cursor without leaving the mouse.
    pub(super) fn place_cursor(&mut self, at: usize, extend: bool, add: bool) {
        // Clamped, always. A cursor past the end of a buffer is not a cursor
        // that is slightly wrong — it is a panic the moment anything asks what
        // is under it, which is the next frame. Every caller works out the
        // position from something: a click from the screen, a jump from a
        // language server, an action from a panel. Any of them can be about a
        // buffer other than the one this pane is showing now, and asking each
        // of them to remember that is asking one of them to forget.
        let at = at.min(self.here().len_chars());
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

    /// Whether a click landed on the mark at the end of a folded row, and
    /// unfolded it.
    ///
    /// The mark is drawn after the text, so a click on it is a click at or
    /// past the end of the line — which is exactly the position
    /// [`App::position_at`] answers with for anything out there. So the line
    /// has to be one with something folded onto it, and the pointer has to be
    /// out where the mark actually is rather than merely somewhere on the row:
    /// clicking the empty space a screen's width to the right of a folded line
    /// is still just putting the cursor at the end of it.
    pub(super) fn click_on_fold_mark(&mut self, column: u16, at: usize) -> bool {
        let view = self.view();
        let doc = match self.doc(view.doc) {
            Some(doc) => doc,
            None => return false,
        };
        let line = text::line_of(&doc.rope, at);
        let folded = view.folded(&doc.rope);
        let Some((_, last)) = folded.iter().find(|(first, _)| *first == line) else {
            return false;
        };
        // Where the text of that line ends on the screen, and how far the mark
        // reaches past it: " ⋯ 12 lines ".
        let end = text::line_end(&doc.rope, line);
        let tab_width = self.config.tab_width();
        let ends_at = text::visual_column(&doc.rope, end, tab_width);
        let width = crate::ui::fold_mark(last - line).chars().count();
        let left = view.area.x as usize + ends_at.saturating_sub(view.left);
        let column = column as usize;
        if column < left || column >= left + width {
            return false;
        }
        self.unfold_line(line);
        self.say("unfolded");
        true
    }

    /// Select the same columns on every line between two places: a cursor on
    /// each, with whatever lies between the two columns selected.
    ///
    /// By column on the screen rather than by character, because that is what
    /// makes a block a block: a line with a tab in it and a line without have
    /// to line up the way they look, or the rectangle you dragged is not the
    /// rectangle you get.
    ///
    /// A line too short to reach the column gets a bare cursor at its end
    /// rather than being left out. That is what makes "type at the end of all
    /// of these" work on a ragged block, and it is what every other editor
    /// does with the same drag.
    pub(super) fn select_block(&mut self, anchor: usize, head: usize) {
        let tab_width = self.config.tab_width();
        let doc = self.here();
        let len = doc.len_chars();
        let (anchor, head) = (anchor.min(len), head.min(len));
        let rope = &doc.rope;
        let from_line = text::line_of(rope, anchor);
        let to_line = text::line_of(rope, head);
        let from_col = text::visual_column(rope, anchor, tab_width);
        let to_col = text::visual_column(rope, head, tab_width);

        let mut ranges = Vec::new();
        let mut primary = 0;
        for line in from_line.min(to_line)..=from_line.max(to_line) {
            if line == to_line {
                // The one the pointer is on is the one the keyboard follows.
                primary = ranges.len();
            }
            ranges.push(Range::new(
                text::char_at_column(rope, line, from_col, tab_width),
                text::char_at_column(rope, line, to_col, tab_width),
            ));
        }
        let view = self.view_mut();
        view.sel = Selections::many(ranges, primary);
        view.goal = None;
        self.scroll_into_view();
    }

    /// Put a dragged selection in, certain that it is inside the buffer.
    ///
    /// The same bargain [`App::place_cursor`] makes, and for the same reason,
    /// but for both ends of a range rather than one. A drag holds on to where
    /// it began — a word, a line, a point — and the buffer underneath it is
    /// free to change while it is held: the debugger's panel is redrawn whole
    /// every time the program so much as prints a line. An anchor remembered
    /// from a longer version of the text is a position past the end of this
    /// one, and a position past the end is a panic on the next frame rather
    /// than a selection that looks slightly wrong.
    pub(super) fn select_dragged(&mut self, range: Range) {
        let len = self.here().len_chars();
        let range = Range::new(range.anchor.min(len), range.head.min(len));
        self.view_mut().sel = Selections::single(range);
        self.scroll_into_view();
    }

    /// Which docked pane's divider is under this point, if any.
    pub(super) fn grip_at(&self, column: u16, row: u16) -> Option<usize> {
        self.panes.iter().position(|pane| {
            pane.grip.is_some_and(|grip| {
                column >= grip.x
                    && column < grip.x + grip.width
                    && row >= grip.y
                    && row < grip.y + grip.height
            })
        })
    }

    /// Pull the divider on a pane's edge to wherever the pointer is.
    ///
    /// Two things wear the same handle. A sidebar's divider changes the
    /// sidebar's size and leaves the middle to take up the slack; a divider
    /// between two panes in the middle moves room from one to the other, since
    /// there is nowhere else for it to come from.
    pub(super) fn pull_divider(&mut self, pane: usize, column: u16, row: u16) {
        match self.panes.get(pane).and_then(|p| p.dock) {
            Some(_) => self.resize_dock(pane, column, row),
            None => self.resize_pane(pane, column, row),
        }
    }

    /// Move room across the divider between this pane and the one before it.
    ///
    /// Both are measured against where the divider actually is rather than by
    /// how far the pointer has moved, so it stays under the pointer over a
    /// long drag. What comes out is written back as *shares* — the two of them
    /// keep the proportion you dragged when the terminal is resized, rather
    /// than the number of columns it happened to work out to.
    pub(super) fn resize_pane(&mut self, pane: usize, column: u16, row: u16) {
        let ordinary: Vec<usize> = (0..self.panes.len())
            .filter(|at| self.panes[*at].dock.is_none())
            .collect();
        let Some(which) = ordinary.iter().position(|at| *at == pane).filter(|at| *at > 0) else {
            return;
        };
        let (before, here) = (ordinary[which - 1], ordinary[which]);
        let (start, whole) = match self.side_by_side {
            true => (
                self.panes[before].frame.x,
                self.panes[before].frame.width + self.panes[here].frame.width,
            ),
            false => (
                self.panes[before].frame.y,
                self.panes[before].frame.height + self.panes[here].frame.height,
            ),
        };
        let at = match self.side_by_side {
            true => column,
            false => row,
        };
        // Neither side may be dragged shut: a pane with no width has no edge
        // left to drag it back by. On a screen with no room for two of them
        // there is nothing to drag, and nothing happens.
        let least = crate::ui::least_pane(self.side_by_side);
        let most = whole.saturating_sub(least);
        if most < least {
            return;
        }
        let first = at.saturating_sub(start).clamp(least, most);

        // Everything keeps the size it has now, and the two either side of
        // this divider take what was dragged — so pulling one edge does not
        // quietly reflow the panes that were nowhere near it.
        for at in &ordinary {
            let pane = &mut self.panes[*at];
            pane.share = match self.side_by_side {
                true => pane.frame.width as f32,
                false => pane.frame.height as f32,
            }
            .max(1.0);
        }
        self.panes[before].share = first as f32;
        self.panes[here].share = (whole - first) as f32;
        self.session_changed();
    }

    /// Make a dock as wide, or as tall, as the pointer says.
    ///
    /// Measured from the far edge of the dock rather than by how far the
    /// pointer has moved, so the divider stays under the pointer instead of
    /// drifting away from it over a long drag.
    pub(super) fn resize_dock(&mut self, pane: usize, column: u16, row: u16) {
        let screen = self.screen;
        let Some(view) = self.panes.get(pane) else { return };
        let (Some(dock), frame) = (view.dock, view.frame) else {
            return;
        };
        let wanted = match dock.edge {
            crate::view::Edge::Left => column.saturating_sub(frame.x) + 1,
            crate::view::Edge::Right => frame.right().saturating_sub(column),
            crate::view::Edge::Bottom => frame.bottom().saturating_sub(row),
        };
        // Never so narrow there is nothing in it, and never so wide the middle
        // is squeezed out — the layout clamps the second of those too, but a
        // size that only looks right because it was clamped is a size that
        // springs back the moment the terminal is resized.
        let room = match dock.edge.is_side() {
            true => screen.width,
            false => screen.height.saturating_sub(2),
        };
        let most = room.saturating_sub(MIN_MIDDLE_ROOM).max(MIN_DOCK);
        let size = wanted.clamp(MIN_DOCK, most);
        if let Some(view) = self.panes.get_mut(pane)
            && let Some(dock) = &mut view.dock
            && dock.size != size
        {
            dock.size = size;
            self.session_changed();
        }
    }

    pub(super) fn drag_to(&mut self, column: u16, row: u16) {
        match self.drag {
            Some(Drag::Popup) => {
                let Some(hover) = &mut self.hover else { return };
                // Dragging off the top or bottom scrolls, so a selection can
                // be longer than the box is tall.
                if row < hover.area.y {
                    hover.scroll_by(-1);
                } else if row >= hover.area.y + hover.area.height {
                    hover.scroll_by(1);
                }
                if let Some(spot) = hover.spot_at(column, row)
                    && let Some((anchor, _)) = hover.select
                {
                    hover.select = Some((anchor, spot));
                }
            }
            Some(Drag::Scrollbar) => self.scroll_to_bar(row),
            Some(Drag::Divider { pane }) => self.pull_divider(pane, column, row),
            Some(Drag::Tab { id, .. }) => {
                if let Some(Drag::Tab { at, .. }) = &mut self.drag {
                    *at = (column, row);
                }
                self.drag_tab(id, column, row);
            }
            Some(Drag::Text) => {
                let Some(at) = self.position_in(self.focus, column, row) else {
                    return;
                };
                let anchor = self.view().sel.primary().anchor;
                self.select_dragged(Range::new(anchor, at));
            }
            Some(Drag::Block { anchor }) => {
                let Some(at) = self.position_in(self.focus, column, row) else {
                    return;
                };
                self.select_block(anchor, at);
            }
            Some(Drag::Words {
                anchor_start,
                anchor_end,
            }) => {
                let Some(at) = self.position_in(self.focus, column, row) else {
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
                self.select_dragged(range);
            }
            Some(Drag::Lines { anchor }) => {
                let Some(at) = self.position_in(self.focus, column, row) else {
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
                self.select_dragged(range);
            }
            None => {}
        }
    }

    pub(super) fn wheel(&mut self, column: u16, row: u16, by: isize) {
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
            Overlay::Menu(menu) => return menu.step(by.signum()),
            _ => {}
        }
        // The wheel over the tabs walks along them. A vertical wheel is what
        // most mice have, and "there are more tabs that way" is the only thing
        // scrolling can mean on a row one line tall.
        if self.tab_row(column, row) {
            return self.scroll_tabs(by * 2);
        }
        if let Some(completion) = &mut self.completion
            && hits(completion.area, column, row)
        {
            completion.step(by.signum());
            self.resolve_selected();
            return;
        }
        // The wheel over a hover scrolls the hover, not the file behind it.
        if let Some(hover) = &mut self.hover
            && (hover.focused || hits(hover.outer, column, row))
        {
            hover.scroll_by(by);
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

    pub(super) fn pan(&mut self, column: u16, row: u16, by: isize) {
        if self.tab_row(column, row) {
            return self.scroll_tabs(by);
        }
        if self.view().wrap {
            return;
        }
        let left = self.view().left as isize + by;
        self.view_mut().left = left.max(0) as usize;
    }

    /// A label for the chrome under the pointer, if that chrome has anything
    /// to say. Answers whether it put one up.
    ///
    /// Only the tabs, so far. A tab is as wide as a file's name and no wider,
    /// which is the right thing to look at and the wrong thing to work from
    /// the moment two of them say `mod.rs` — so resting on one says where it
    /// came from.
    pub(super) fn tip_at_screen(&mut self, column: u16, row: u16) -> bool {
        let Some(id) = self
            .hits.tabs
            .iter()
            .find(|(area, _, _)| hits(*area, column, row))
            .map(|(_, id, _)| *id)
        else {
            return false;
        };
        // The whole tab, not the one column of it the pointer happens to be
        // in: the name and the close cross are two hit boxes and one tab, and
        // a label that flickered as you crossed between them would be a
        // label about the wrong thing.
        let about = self
            .hits.tabs
            .iter()
            .filter(|(_, other, _)| *other == id)
            .map(|(area, _, _)| *area)
            .reduce(|one, other| one.union(other))
            .unwrap_or_default();
        let Some(doc) = self.docs.iter().find(|d| d.id == id) else {
            return false;
        };
        let text = match (&doc.path, &doc.origin) {
            (Some(path), _) => tilde(path),
            // Not a file: a class out of a jar, or whatever else a language
            // server handed over. Where it came from is still the answer.
            (None, Some(origin)) => origin.clone(),
            (None, None) => return false,
        };
        // A label that says what the tab already says is a box over the text
        // for nothing.
        if text == doc.name {
            return false;
        }
        self.tip = Some(Tip { text, about });
        true
    }

    /// Whether a point is on the row of tabs. The wheel there scrolls the tabs
    /// rather than the file, because there is nothing else it could sensibly
    /// mean and twenty open files need scrolling somehow.
    pub(super) fn tab_row(&self, column: u16, row: u16) -> bool {
        row == self.screen.y
            && column >= self.screen.x
            && column < self.screen.x + self.screen.width
    }

    /// Move the row of tabs sideways. The far end is worked out by the drawing,
    /// which is the only thing that knows how wide the tabs came out, so this
    /// only has to keep it from going negative.
    pub(super) fn scroll_tabs(&mut self, by: isize) {
        let at = self.tab_scroll as isize + by;
        self.tab_scroll = at.clamp(0, u16::MAX as isize) as u16;
    }

    /// Move the view to where the scroll bar was grabbed.
    pub(super) fn scroll_to_bar(&mut self, row: u16) {
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
    pub(super) fn pane_at(&self, column: u16, row: u16) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| hits(pane.frame, column, row))
    }

    /// What character a point is over, in whichever pane it is in.
    pub fn position_at(&self, column: u16, row: u16) -> Option<usize> {
        let pane = self.pane_at(column, row)?;
        let area = self.panes[pane].area;
        if row < area.y || row >= area.y + area.height {
            return None;
        }
        self.position_in(pane, column, row)
    }

    /// The stretch of a panel the pointer is resting on, where that stretch
    /// does something.
    ///
    /// A panel's actionable text is the only thing inside a pane that behaves
    /// like a button, and it used to look exactly like the text beside it: the
    /// only way to learn whether something could be clicked was to click it.
    /// Lighting it under the pointer is the oldest convention there is for
    /// "this does something", and it costs a pane with no panel in it nothing,
    /// because there are no actions to look through.
    ///
    /// Only the stretches that *do* something, which is the half that matters.
    /// A button drawn greyed out — a step in a debugger that has nothing
    /// stopped — has no action on it and so never lights up, and the pointer
    /// is never somewhere the highlight is not. See [`crate::menu`], which
    /// makes the same bargain for the same reason.
    ///
    /// Worked out while drawing rather than remembered, so that a panel
    /// rewritten under a resting pointer — the debugger's is, every time the
    /// program prints a line — comes back with the highlight in the right
    /// place rather than on whatever text has moved into the old one.
    pub fn panel_action_under(&self, pane: usize, column: u16, row: u16) -> Option<Range> {
        if self.pane_at(column, row)? != pane {
            return None;
        }
        let view = self.panes.get(pane)?;
        let area = view.area;
        // The text itself, not the margin beside it: a click left of the text
        // is the start of the line, and the start of a line that happens to
        // begin with a button is not the button.
        if column < area.x || row < area.y || row >= area.y + area.height {
            return None;
        }
        let doc = self.doc(view.doc)?;
        let panel = doc.panel.as_ref()?;
        let at = self.position_at(column, row)?;
        panel
            .actions
            .iter()
            .find(|(range, _)| range.start() <= at && at < range.end())
            .map(|(range, _)| *range)
    }

    /// The same, in a pane named rather than found under the pointer, with
    /// the point clamped into it.
    ///
    /// This is what a drag needs, and the difference is not a detail. A drag
    /// belongs to the pane it began in for the whole of its life, and the
    /// pointer is very often somewhere else — leaving the pane is *how* you
    /// select more than a screenful. Asking which pane is under the pointer
    /// answers with somebody else's buffer, and an offset into that one used
    /// in this one is a selection past the end of the text: a click in the
    /// debugger's panel dragged up into a source file two thousand characters
    /// longer than it used to be an outright crash on the next frame.
    pub(super) fn position_in(&self, pane: usize, column: u16, row: u16) -> Option<usize> {
        let view = self.panes.get(pane)?;
        let doc = self.doc(view.doc)?;
        let area = view.area;
        // A click left of the text is the start of the line, not nothing:
        // clicking the line numbers should still put you somewhere.
        let across = column
            .saturating_sub(area.x)
            .min(area.width.saturating_sub(1));
        let down = row
            .saturating_sub(area.y)
            .min(area.height.saturating_sub(1));
        Some(view::position_at_screen(
            view,
            doc,
            self.config.tab_width(),
            down as usize,
            across as usize,
        ))
    }
}

/// How many files a project-wide replace will open in one go.
///
/// A replacement is opened as buffers rather than written to the disk, so this
/// is a real limit rather than a shy one: a thousand tabs is not a review, it
/// is an editor that has become unusable in one keystroke. Anything past it is
/// counted and said out loud.
pub(crate) const REPLACE_AT_MOST: usize = 200;

/// Where `needle` appears in `text`, as byte offsets.
///
/// A lower-case needle ignores case and one with a capital in it means the
/// capital — the same bargain the find box makes, so that "replace" finds what
/// "find" just found.
///
/// Lowercasing can change how many bytes a character takes — `İ` is one
/// character and lowers to two — and where it does, the offsets into the
/// lowered copy are not offsets into the text. There is no honest answer to
/// give there, so the search is done exactly as it was typed instead.
pub(crate) fn occurrences(text: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let sensitive = needle.chars().any(char::is_uppercase);
    let (hay, pin) = match sensitive {
        true => (text.to_string(), needle.to_string()),
        false => (text.to_lowercase(), needle.to_lowercase()),
    };
    let (hay, pin) = match hay.len() == text.len() {
        true => (hay, pin),
        false => (text.to_string(), needle.to_string()),
    };
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = hay[at..].find(&pin) {
        found.push(at + offset);
        at += offset + pin.len().max(1);
    }
    found
}

/// A range as a language server writes one — two line-and-column points — as
/// character positions in this document.
pub(crate) fn range_from_lsp(range: &Value, doc: &Document) -> Option<Range> {
    let (l1, c1) = crate::lsp::point_of(range.get("start")?)?;
    let (l2, c2) = crate::lsp::point_of(range.get("end")?)?;
    Some(Range::new(
        doc.char_at_lsp_point(l1, c1),
        doc.char_at_lsp_point(l2, c2),
    ))
}

/// Whether a point is inside a rectangle.
pub(crate) fn hits(area: Rect, column: u16, row: u16) -> bool {
    area.width > 0
        && area.height > 0
        && column >= area.x
        && column < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
}
