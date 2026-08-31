//! The order of the tabs, and the panes they are shown in.

use super::*;

impl App {
    /// Put a buffer at a particular place in the row of tabs.
    ///
    /// The row is the order of `docs`, so this is that list being reordered.
    /// Nothing anywhere holds onto a position in it — a buffer is named by its
    /// [`DocId`] everywhere else — which is what makes moving one about a
    /// question of moving one about, rather than of finding everything that
    /// would now be pointing at the wrong file.
    ///
    /// Answers whether anything moved.
    pub(super) fn move_tab(&mut self, id: DocId, to: usize) -> bool {
        let Some(from) = self.docs.iter().position(|d| d.id == id) else {
            return false;
        };
        let to = to.min(self.docs.len().saturating_sub(1));
        if from == to {
            return false;
        }
        let doc = self.docs.remove(from);
        self.docs.insert(to, doc);
        self.session_changed();
        true
    }

    /// Move the tab you are looking at one place along.
    ///
    /// It stops at the ends rather than wrapping. Stepping *between* buffers
    /// wraps, because going round is how you visit them all; moving one wraps
    /// a file from the front of the row to the back, which is never what
    /// somebody nudging a tab along meant.
    pub(super) fn step_tab(&mut self, by: isize) {
        if self.docs.len() < 2 {
            return;
        }
        let here = self.view().doc;
        let at = self.docs.iter().position(|d| d.id == here).unwrap_or(0) as isize;
        let to = at + by;
        if to < 0 || to >= self.docs.len() as isize {
            return self.say(match by < 0 {
                true => "this tab is already first",
                false => "this tab is already last",
            });
        }
        self.move_tab(here, to as usize);
    }

    /// The tab being carried about, for the drawing to show as picked up.
    pub fn dragging_tab(&self) -> Option<DocId> {
        match self.drag {
            Some(Drag::Tab { id, .. }) => Some(id),
            _ => None,
        }
    }

    /// Where each tab is on the screen: one span per file, rather than the two
    /// hit boxes — the name and the cross — it is drawn as.
    ///
    /// In screen order, and only the ones on the screen: a tab scrolled off
    /// the end has no span, which is why dragging past the edge is answered by
    /// the arrows there rather than by this.
    pub(super) fn tab_spans(&self) -> Vec<(DocId, u16, u16)> {
        let mut out: Vec<(DocId, u16, u16)> = Vec::new();
        for (area, id, _) in &self.hits.tabs {
            match out.iter_mut().find(|(seen, ..)| seen == id) {
                Some(span) => {
                    span.1 = span.1.min(area.x);
                    span.2 = span.2.max(area.x + area.width);
                }
                None => out.push((*id, area.x, area.x + area.width)),
            }
        }
        out.sort_by_key(|(_, from, _)| *from);
        out
    }

    /// Carry a tab to where the pointer is.
    ///
    /// The rule is the one that makes this feel right rather than the obvious
    /// one. "Move it to whichever tab the pointer is over" oscillates: put a
    /// narrow tab where a wide one was and the pointer is left over the wide
    /// one again, which asks for the swap back, and the two trade places for
    /// as long as you hold the mouse still. So a tab only ever moves one place
    /// at a time, and only once the pointer is past the *middle* of the
    /// neighbour it would trade with — which is far enough that after the
    /// trade the pointer is not past the middle of anything, and it settles.
    pub(super) fn drag_tab(&mut self, id: DocId, column: u16, row: u16) {
        if !self.tab_row(column, row) {
            return;
        }
        let spans = self.tab_spans();
        let Some(here) = spans.iter().position(|(seen, ..)| *seen == id) else {
            return;
        };
        let (_, from, to) = spans[here];
        let step = if column >= to {
            1
        } else if column < from {
            -1
        } else {
            return;
        };
        let Some(neighbour) = here
            .checked_add_signed(step)
            .and_then(|next| spans.get(next))
        else {
            // The far end of the row, or a neighbour scrolled off the screen.
            // Holding it over the arrow there is what keeps it going.
            return;
        };
        let (_, their_from, their_to) = *neighbour;
        let middle = their_from + (their_to - their_from) / 2;
        let past = match step {
            1 => column >= middle,
            _ => column <= middle,
        };
        if !past {
            return;
        }
        let at = self.docs.iter().position(|d| d.id == id).unwrap_or(0);
        if let Some(to) = at.checked_add_signed(step) {
            self.move_tab(id, to);
        }
    }

    // ---- Panes ----

    pub(super) fn split(&mut self) {
        if self.ordinary_panes() >= 4 {
            return self.say("four panes is as many as fit");
        }
        let mut copy = View::new(self.view().doc, self.view().wrap);
        copy.sel = self.view().sel.clone();
        copy.top = self.view().top;
        // Never a copy of the dock. Splitting a sidebar would give you two
        // sidebars, which is not what anybody means by it.
        copy.dock = None;
        let at = self.focus.min(self.panes.len().saturating_sub(1));
        self.panes.insert(at + 1, copy);
        self.focus = at + 1;
        self.session_changed();
    }

    pub(super) fn close_pane(&mut self) {
        let at = self.focus.min(self.panes.len().saturating_sub(1));
        // Both halves of a plugin's settings go together, whichever one you
        // were standing in when you asked.
        if self
            .panes
            .get(at)
            .is_some_and(|pane| self.is_settings_half(pane.doc))
            && self.close_settings_panes()
        {
            return;
        }
        let docked = self.panes.get(at).is_some_and(|p| p.dock.is_some());
        // A dock is always closable — it is a thing you put there, and the
        // editor is still an editor without it. What has to survive is the
        // last pane showing a file.
        if !docked && self.ordinary_panes() < 2 {
            return self.say("that is the only pane");
        }
        self.panes.remove(at);
        self.focus = at.min(self.panes.len().saturating_sub(1));
        self.session_changed();
    }

    /// How many panes are showing a buffer in the middle rather than sitting
    /// on an edge.
    pub(super) fn ordinary_panes(&self) -> usize {
        self.panes.iter().filter(|p| p.dock.is_none()).count()
    }

    /// The pane in the middle to put something in, from wherever the focus is.
    ///
    /// The nearest one after the focus, wrapping — so from a sidebar on the
    /// left it is the pane immediately to its right, which is the one anybody
    /// pointing at the sidebar is looking at.
    pub(super) fn beside_the_docks(&self) -> Option<usize> {
        let len = self.panes.len();
        (0..len)
            .map(|step| (self.focus + step) % len)
            .find(|at| self.panes[*at].dock.is_none())
    }

    /// A pane that will take whatever is being opened: not a sidebar, and not
    /// one pinned to a buffer of its own.
    ///
    /// Falls back to the first pane that is merely not a sidebar, because a
    /// screen with nowhere at all to put a file still has to put it
    /// somewhere.
    pub(super) fn somewhere_to_open(&self) -> Option<usize> {
        let len = self.panes.len();
        let round = |from: usize| (0..len).map(move |step| (from + step) % len);
        round(self.focus)
            .find(|at| self.panes[*at].dock.is_none() && !self.panes[*at].pinned)
            .or_else(|| self.beside_the_docks())
    }

    pub(super) fn focus_pane(&mut self, by: isize) {
        let len = self.panes.len() as isize;
        self.focus = ((self.focus as isize + by).rem_euclid(len)) as usize;
        self.dismiss_popups();
        self.completion = None;
    }
}
