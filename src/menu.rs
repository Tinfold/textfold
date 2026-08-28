//! The little list that opens under the pointer: a context menu.
//!
//! Not the fuzzy picker. The picker is for choosing out of hundreds of things
//! by typing part of a name; a context menu is for the handful of things that
//! make sense right here, read rather than searched, and it has to appear
//! where you clicked rather than in the middle of the screen.
//!
//! Every row is a command the editor already has, so a menu is a second way to
//! reach the keys rather than a second implementation of them. That is what
//! keeps the two from drifting: there is nothing here that a keystroke cannot
//! also do, and nothing a keystroke does that a menu row could get wrong.

use ratatui::layout::Rect;

use crate::cmd::Cmd;
use crate::doc::DocId;

/// What choosing a row does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Run it on whatever is current.
    Run(Cmd),
    /// Switch to a buffer and then run it. What the tab menu needs: "close the
    /// others" means the others than the tab you right-clicked, which is not
    /// necessarily the one you were looking at.
    RunOn(DocId, Cmd),
    /// A line, not a row. Never chosen.
    Divide,
    /// Hand this back to the plugin that put the menu up.
    ///
    /// The one row that is not a command the editor already has. A menu is
    /// still a second way to reach something rather than a second
    /// implementation of it — the something is just the plugin's rather than
    /// the editor's, and the string is opaque here exactly as a panel's
    /// action is.
    Chosen(String),
}

/// One row.
#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    /// The key that also does this, so the menu teaches the keyboard rather
    /// than replacing it.
    pub key: Option<String>,
    pub action: Action,
    /// Whether it can be chosen now. A row that cannot is still drawn — that
    /// "rename" exists and is unavailable here is worth knowing, and a menu
    /// whose rows move about depending on context is a menu you cannot learn
    /// the shape of.
    pub enabled: bool,
}

impl Item {
    /// A row a plugin put there. `value` is what it gets back.
    pub fn chosen(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            key: None,
            action: Action::Chosen(value.into()),
            enabled: true,
        }
    }

    pub fn new(label: impl Into<String>, cmd: Cmd) -> Self {
        Self {
            label: label.into(),
            key: None,
            action: Action::Run(cmd),
            enabled: true,
        }
    }

    /// A row about a particular buffer rather than the current one.
    pub fn on(doc: DocId, label: impl Into<String>, cmd: Cmd) -> Self {
        Self {
            action: Action::RunOn(doc, cmd),
            ..Self::new(label, cmd)
        }
    }

    pub fn divider() -> Self {
        Self {
            label: String::new(),
            key: None,
            action: Action::Divide,
            enabled: false,
        }
    }

    pub fn key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
    }

    pub fn enabled(mut self, yes: bool) -> Self {
        self.enabled = yes;
        self
    }

    fn choosable(&self) -> bool {
        self.enabled && !matches!(self.action, Action::Divide)
    }
}

/// An open menu.
pub struct Menu {
    pub items: Vec<Item>,
    pub cursor: usize,
    /// The screen cell it grew from, which is where the pointer was.
    pub anchor: (u16, u16),
    /// The first row drawn, for a menu with more rows than the terminal has
    /// room for. A menu that simply stopped at the bottom of the screen would
    /// have rows nothing could reach — not the pointer, because they are not
    /// drawn, and not the arrows either, because the highlight would walk off
    /// into rows nobody can see. Filled in by the drawing, like `area`.
    pub scroll: usize,
    /// Where it was last drawn, for answering clicks. Filled in by the
    /// drawing every frame, like the tabs, so that a click is answered by what
    /// is on the screen rather than by working out where it ought to be.
    pub area: Rect,
}

impl Menu {
    pub fn new(items: Vec<Item>, anchor: (u16, u16)) -> Self {
        let mut menu = Self {
            items,
            cursor: 0,
            anchor,
            scroll: 0,
            area: Rect::default(),
        };
        // Open on something that can be chosen, so that the first Enter does
        // something rather than nothing.
        if !menu.items.first().is_some_and(Item::choosable) {
            menu.step(1);
        }
        menu
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// How wide the box has to be to hold all of it.
    pub fn width(&self) -> u16 {
        let widest = self
            .items
            .iter()
            .map(|item| {
                let key = item.key.as_deref().map_or(0, |k| crate::text::str_width(k) + 3);
                crate::text::str_width(&item.label) + key
            })
            .max()
            .unwrap_or(8);
        (widest + 4).clamp(14, 48) as u16
    }

    /// Move the highlight, skipping dividers and anything unavailable, and
    /// wrapping round the ends the way every menu does.
    pub fn step(&mut self, by: isize) {
        if !self.items.iter().any(Item::choosable) {
            return;
        }
        let n = self.items.len() as isize;
        let mut at = self.cursor as isize;
        for _ in 0..n {
            at = (at + by).rem_euclid(n);
            if self.items[at as usize].choosable() {
                self.cursor = at as usize;
                return;
            }
        }
    }

    /// Put the highlight where the pointer is.
    ///
    /// Onto a row that cannot be chosen as well as one that can, because the
    /// pointer is somewhere whether or not there is anything to do there, and
    /// a highlight left behind on a distant row while you point at this one
    /// would be telling you something untrue about what a click would do. The
    /// drawing shows an unavailable row differently; choosing one still does
    /// nothing. A divider is not a row and takes nothing.
    pub fn point_at(&mut self, at: usize) {
        if self
            .items
            .get(at)
            .is_some_and(|item| !matches!(item.action, Action::Divide))
        {
            self.cursor = at;
        }
    }

    /// What choosing the highlighted row means.
    pub fn chosen(&self) -> Option<Action> {
        self.at(self.cursor)
    }

    /// What choosing row `at` means — nothing, for a divider or for a row that
    /// cannot be chosen.
    ///
    /// A click asks about the row it landed on rather than about the
    /// highlight. Those are usually the same row and once were assumed to be,
    /// which meant that clicking a divider ran whatever happened to be
    /// highlighted: with the highlight where a menu opens it, clicking the
    /// line under "Paste" cut your selection.
    pub fn at(&self, at: usize) -> Option<Action> {
        let item = self.items.get(at)?;
        item.choosable().then_some(item.action.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(items: Vec<Item>) -> Menu {
        Menu::new(items, (0, 0))
    }

    #[test]
    fn a_menu_opens_on_something_you_can_choose() {
        let m = menu(vec![
            Item::divider(),
            Item::new("cut", Cmd::CUT).enabled(false),
            Item::new("copy", Cmd::COPY),
        ]);
        assert_eq!(m.cursor, 2);
        assert_eq!(m.chosen(), Some(Action::Run(Cmd::COPY)));
    }

    #[test]
    fn moving_steps_over_dividers_and_what_cannot_be_chosen() {
        let mut m = menu(vec![
            Item::new("copy", Cmd::COPY),
            Item::divider(),
            Item::new("paste", Cmd::PASTE).enabled(false),
            Item::new("find", Cmd::FIND),
        ]);
        assert_eq!(m.cursor, 0);
        m.step(1);
        assert_eq!(m.cursor, 3);
        m.step(1);
        assert_eq!(m.cursor, 0, "the end wraps round to the start");
        m.step(-1);
        assert_eq!(m.cursor, 3);
    }

    #[test]
    fn a_menu_of_nothing_choosable_chooses_nothing() {
        let mut m = menu(vec![Item::divider(), Item::new("cut", Cmd::CUT).enabled(false)]);
        m.step(1);
        assert_eq!(m.chosen(), None);
    }

    #[test]
    fn clicking_a_divider_does_nothing_rather_than_what_was_highlighted() {
        let m = menu(vec![
            Item::new("cut", Cmd::CUT),
            Item::divider(),
            Item::new("copy", Cmd::COPY),
        ]);
        assert_eq!(m.cursor, 0, "the highlight is on cut");
        assert_eq!(m.at(1), None, "the divider is not a row");
        assert_eq!(m.at(2), Some(Action::Run(Cmd::COPY)));
    }

    #[test]
    fn clicking_a_row_asks_about_that_row_not_the_highlight() {
        let m = menu(vec![
            Item::new("cut", Cmd::CUT),
            Item::new("paste", Cmd::PASTE).enabled(false),
        ]);
        assert_eq!(m.at(1), None, "an unavailable row does nothing");
    }

    #[test]
    fn pointing_at_a_divider_leaves_the_highlight_alone() {
        let mut m = menu(vec![Item::new("copy", Cmd::COPY), Item::divider()]);
        m.point_at(1);
        assert_eq!(m.cursor, 0);
    }

    #[test]
    fn pointing_at_something_unavailable_still_moves_the_highlight() {
        let mut m = menu(vec![
            Item::new("copy", Cmd::COPY),
            Item::new("paste", Cmd::PASTE).enabled(false),
        ]);
        m.point_at(1);
        assert_eq!(m.cursor, 1, "the highlight has to be where the pointer is");
        assert_eq!(m.chosen(), None, "but there is still nothing to choose");
    }

    #[test]
    fn the_arrows_still_skip_what_cannot_be_chosen() {
        let mut m = menu(vec![
            Item::new("copy", Cmd::COPY),
            Item::new("paste", Cmd::PASTE).enabled(false),
            Item::new("find", Cmd::FIND),
        ]);
        m.step(1);
        assert_eq!(m.cursor, 2);
    }
}
