//! Which key does what.
//!
//! The scheme textfold ships is the one nearly every program outside a
//! terminal already uses: Ctrl-S saves, Ctrl-Z undoes, Ctrl-F finds, arrows
//! move and Shift-arrows select. That is not a lack of imagination. Somebody
//! who has never opened this program before should be able to type into it and
//! get their work out again, and every terminal editor that decided otherwise
//! spends the rest of its life explaining how to quit.
//!
//! Nothing in the default scheme needs a terminal that can tell Ctrl-Shift-P
//! from Ctrl-P. Where a binding like that is offered it is a second way to
//! reach something already reachable, so a plain `xterm` over `ssh` loses
//! nothing. A *modified arrow* is a different matter and is used: an arrow
//! goes down the wire as an escape sequence with a number in it saying which
//! modifiers were held, so Ctrl-Shift-Up really is its own keystroke on every
//! terminal, which Ctrl-Shift-P is not.
//!
//! Your own bindings go in the settings file, by command name:
//!
//! ```json
//! { "keys": { "save": ["ctrl-s", "f2"], "quit": [] } }
//! ```
//!
//! Naming a command replaces every key it had, so an empty list unbinds it.
//! Commands you do not mention keep what they came with.

use std::collections::{BTreeMap, HashMap};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cmd::Cmd;

/// A keystroke, with the modifiers settled into one spelling so that two ways
/// of describing one key are one key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Key {
    /// A keystroke as it arrived from the terminal, in the one spelling.
    ///
    /// Shift on a letter is not a modifier, it is a different letter: a
    /// terminal reports Shift-A as `A`, and one with the extended protocol
    /// reports it as `A` *and* a shift flag. Both have to mean the same thing
    /// or half the bindings would only work on half the terminals.
    pub fn from_event(event: KeyEvent) -> Self {
        let mut mods = event.modifiers
            & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match event.code {
            KeyCode::Char(c) => {
                let c = if mods.contains(KeyModifiers::SHIFT) {
                    c.to_uppercase().next().unwrap_or(c)
                } else {
                    c
                };
                // The character already says what shift did.
                mods.remove(KeyModifiers::SHIFT);
                KeyCode::Char(c)
            }
            // The one exception: a terminal reporting Backtab has already
            // folded shift into the code, and one reporting Shift-Tab has not.
            KeyCode::BackTab => {
                mods.insert(KeyModifiers::SHIFT);
                KeyCode::Tab
            }
            other => other,
        };
        Self { code, mods }
    }

    /// A keystroke from the way it is written in a config file:
    /// `ctrl-s`, `alt-shift-up`, `f12`, `ctrl-/`, `space`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut mods = KeyModifiers::NONE;
        // Split on `-`, but the last piece is the key, which may itself be
        // `-`. `ctrl--` is Ctrl and the minus key.
        let mut parts: Vec<&str> = text.split('-').collect();
        let mut name = parts.pop()?.to_string();
        if name.is_empty() && !parts.is_empty() {
            // Trailing `-` was the key itself.
            name = "-".into();
            parts.pop();
        }
        for part in parts {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" | "c" => mods |= KeyModifiers::CONTROL,
                "alt" | "meta" | "opt" | "option" | "a" | "m" => mods |= KeyModifiers::ALT,
                "shift" | "s" => mods |= KeyModifiers::SHIFT,
                _ => return None,
            }
        }

        let lower = name.to_lowercase();
        let code = match lower.as_str() {
            "space" => KeyCode::Char(' '),
            "enter" | "return" | "cr" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "esc" | "escape" => KeyCode::Esc,
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            // The key beside the right Ctrl on a full keyboard, which has
            // meant "context menu" since 1994.
            "menu" | "apps" => KeyCode::Menu,
            _ => {
                if let Some(n) = lower.strip_prefix('f')
                    && let Ok(n) = n.parse::<u8>()
                    && (1..=24).contains(&n)
                {
                    KeyCode::F(n)
                } else {
                    let mut chars = name.chars();
                    let c = chars.next()?;
                    if chars.next().is_some() {
                        return None;
                    }
                    KeyCode::Char(c)
                }
            }
        };

        // Through the same door as a real keystroke, so `shift-a` and `A` are
        // the same binding rather than two that shadow each other.
        Some(Self::from_event(KeyEvent::new(code, mods)))
    }

    /// How to write this key, for the help screen and the palette.
    pub fn show(&self) -> String {
        let mut out = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            out.push_str("Ctrl-");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            out.push_str("Alt-");
        }
        // A capital letter *is* shift, and writing `Ctrl-K` for what is really
        // Ctrl-Shift-K would send somebody to the wrong key.
        let shifted_letter = matches!(self.code, KeyCode::Char(c) if c.is_uppercase());
        if self.mods.contains(KeyModifiers::SHIFT) || shifted_letter {
            out.push_str("Shift-");
        }
        match self.code {
            KeyCode::Char(' ') => out.push_str("Space"),
            KeyCode::Char(c) => out.extend(c.to_lowercase()),
            KeyCode::Enter => out.push_str("Enter"),
            KeyCode::Tab => out.push_str("Tab"),
            KeyCode::Esc => out.push_str("Esc"),
            KeyCode::Backspace => out.push_str("Backspace"),
            KeyCode::Delete => out.push_str("Delete"),
            KeyCode::Insert => out.push_str("Insert"),
            KeyCode::Left => out.push('←'),
            KeyCode::Right => out.push('→'),
            KeyCode::Up => out.push('↑'),
            KeyCode::Down => out.push('↓'),
            KeyCode::Home => out.push_str("Home"),
            KeyCode::End => out.push_str("End"),
            KeyCode::PageUp => out.push_str("PgUp"),
            KeyCode::PageDown => out.push_str("PgDn"),
            KeyCode::F(n) => out.push_str(&format!("F{n}")),
            other => out.push_str(&format!("{other:?}")),
        }
        out
    }

    /// How this keystroke is written in a settings file: `ctrl-s`, `f6`, `r`.
    ///
    /// The inverse of [`Key::parse`], and not the same as [`Key::show`] —
    /// `show` is for a person reading a menu and has arrows in it, this is for
    /// a plugin matching on what was pressed, and a plugin author matching on
    /// the spelling they would have used in their own manifest is one thing
    /// fewer to look up.
    pub fn spelled(&self) -> String {
        let mut out = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            out.push_str("ctrl-");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            out.push_str("alt-");
        }
        let shifted_letter = matches!(self.code, KeyCode::Char(c) if c.is_uppercase());
        if self.mods.contains(KeyModifiers::SHIFT) || shifted_letter {
            out.push_str("shift-");
        }
        match self.code {
            KeyCode::Char(' ') => out.push_str("space"),
            KeyCode::Char(c) => out.extend(c.to_lowercase()),
            KeyCode::Enter => out.push_str("enter"),
            KeyCode::Tab => out.push_str("tab"),
            KeyCode::Esc => out.push_str("esc"),
            KeyCode::Backspace => out.push_str("backspace"),
            KeyCode::Delete => out.push_str("delete"),
            KeyCode::Insert => out.push_str("insert"),
            KeyCode::Left => out.push_str("left"),
            KeyCode::Right => out.push_str("right"),
            KeyCode::Up => out.push_str("up"),
            KeyCode::Down => out.push_str("down"),
            KeyCode::Home => out.push_str("home"),
            KeyCode::End => out.push_str("end"),
            KeyCode::PageUp => out.push_str("pageup"),
            KeyCode::PageDown => out.push_str("pagedown"),
            KeyCode::F(n) => out.push_str(&format!("f{n}")),
            other => out.push_str(&format!("{other:?}").to_lowercase()),
        }
        out
    }

    /// The character this keystroke would type, if it would type one. A key
    /// with Ctrl or Alt on it is a command, not a character; Shift is already
    /// in the character itself.
    pub fn as_typed(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(c)
                if !self.mods.contains(KeyModifiers::CONTROL)
                    && !self.mods.contains(KeyModifiers::ALT) =>
            {
                Some(c)
            }
            _ => None,
        }
    }
}

/// The scheme textfold ships.
///
/// Read it as the answer to "what would this key do in any other program".
/// Where a command has two keys, the first is the one shown in the help.
const DEFAULTS: &[(Cmd, &[&str])] = &[
    // Files. Ctrl-Q leaves, because Ctrl-C is copy here as it is everywhere
    // else, and an editor you cannot get out of is the oldest joke in the
    // trade.
    (Cmd::QUIT, &["ctrl-q"]),
    (Cmd::SAVE, &["ctrl-s"]),
    (Cmd::SAVE_AS, &["alt-s"]),
    (Cmd::CLOSE, &["ctrl-w"]),
    (Cmd::NEW, &["ctrl-n"]),
    (Cmd::OPEN, &["ctrl-p", "ctrl-o"]),
    // Two keys because this one is also typed by other programs rather than
    // by people: `alt-e` is short, and F4 is one self-contained escape
    // sequence, which is the safer thing to write down a pipe.
    (Cmd::OPEN_PATH, &["alt-e", "f4"]),
    (Cmd::BUFFERS, &["ctrl-b"]),
    (Cmd::NEXT_BUFFER, &["alt-."]),
    (Cmd::PREV_BUFFER, &["alt-,"]),
    // The keys VS Code and every browser use for moving a tab along, and a
    // modified special key rather than a modified letter — those always
    // encode which modifiers were held, so this arrives on a plain terminal
    // as well as on one with the extended protocol.
    (Cmd::MOVE_TAB_LEFT, &["ctrl-shift-pageup"]),
    (Cmd::MOVE_TAB_RIGHT, &["ctrl-shift-pagedown"]),
    (Cmd::COMMAND_PALETTE, &["alt-x", "ctrl-shift-p"]),
    (Cmd::HELP, &["f1"]),

    // Moving.
    (Cmd::MOVE_LEFT, &["left"]),
    (Cmd::MOVE_RIGHT, &["right"]),
    (Cmd::MOVE_UP, &["up"]),
    (Cmd::MOVE_DOWN, &["down"]),
    (Cmd::MOVE_WORD_LEFT, &["ctrl-left", "alt-left"]),
    (Cmd::MOVE_WORD_RIGHT, &["ctrl-right", "alt-right"]),
    (Cmd::MOVE_LINE_START, &["home"]),
    (Cmd::MOVE_LINE_END, &["end"]),
    (Cmd::MOVE_PAGE_UP, &["pageup"]),
    (Cmd::MOVE_PAGE_DOWN, &["pagedown"]),
    (Cmd::MOVE_DOC_START, &["ctrl-home"]),
    (Cmd::MOVE_DOC_END, &["ctrl-end"]),
    (Cmd::MOVE_PARA_UP, &["ctrl-up"]),
    (Cmd::MOVE_PARA_DOWN, &["ctrl-down"]),
    (Cmd::MATCH_BRACKET, &["alt-b"]),
    (Cmd::GOTO_LINE, &["ctrl-g"]),
    (Cmd::JUMP_BACK, &["alt-["]),
    (Cmd::JUMP_FORWARD, &["alt-]"]),
    // Places you said you were coming back to. `m` for mark, and letters
    // rather than brackets because a terminal sends `{` for shift-`[` and a
    // binding written the other way would never arrive.
    (Cmd::TOGGLE_BOOKMARK, &["alt-shift-m"]),
    (Cmd::NEXT_BOOKMARK, &["alt-shift-n"]),
    (Cmd::PREV_BOOKMARK, &["alt-shift-b"]),
    (Cmd::CENTRE_CURSOR, &["alt-m"]),

    // Selecting. Shift and a movement, which is what it is everywhere.
    (Cmd::EXTEND_LEFT, &["shift-left"]),
    (Cmd::EXTEND_RIGHT, &["shift-right"]),
    (Cmd::EXTEND_UP, &["shift-up"]),
    (Cmd::EXTEND_DOWN, &["shift-down"]),
    (Cmd::EXTEND_WORD_LEFT, &["ctrl-shift-left", "alt-shift-left"]),
    (Cmd::EXTEND_WORD_RIGHT, &["ctrl-shift-right", "alt-shift-right"]),
    (Cmd::EXTEND_LINE_START, &["shift-home"]),
    (Cmd::EXTEND_LINE_END, &["shift-end"]),
    (Cmd::EXTEND_PAGE_UP, &["shift-pageup"]),
    (Cmd::EXTEND_PAGE_DOWN, &["shift-pagedown"]),
    (Cmd::EXTEND_DOC_START, &["ctrl-shift-home"]),
    (Cmd::EXTEND_DOC_END, &["ctrl-shift-end"]),
    (Cmd::SELECT_ALL, &["ctrl-a"]),
    (Cmd::SELECT_LINE, &["ctrl-l"]),
    (Cmd::EXPAND_SELECTION, &["alt-="]),
    // Another cursor above or below is Ctrl-Alt-arrow in every editor that has
    // the feature, and a desktop is allowed to take a keystroke before the
    // terminal under it ever sees it — GNOME and KDE both take exactly that
    // one for switching workspace. So the first key here is Ctrl-Shift-arrow,
    // which nothing above the terminal wants and which every terminal encodes,
    // and Ctrl-Alt-arrow is bound beside it for the desktops that leave it
    // alone. The first is what the help screen shows, because a key in the
    // help that does nothing is worse than no help.
    (Cmd::ADD_CURSOR_ABOVE, &["ctrl-shift-up", "ctrl-alt-up"]),
    (Cmd::ADD_CURSOR_BELOW, &["ctrl-shift-down", "ctrl-alt-down"]),
    (Cmd::ADD_CURSOR_NEXT_MATCH, &["ctrl-d"]),
    (Cmd::SELECT_ALL_MATCHES, &["ctrl-shift-l"]),
    (Cmd::CURSORS_TO_LINE_ENDS, &["alt-shift-i"]),
    (Cmd::COLLAPSE_CURSORS, &["alt-shift-c"]),

    // Changing text.
    (Cmd::INSERT_NEWLINE, &["enter"]),
    (Cmd::DELETE_BACKWARD, &["backspace"]),
    (Cmd::DELETE_FORWARD, &["delete"]),
    (Cmd::DELETE_WORD_BACKWARD, &["ctrl-backspace", "alt-backspace"]),
    (Cmd::DELETE_WORD_FORWARD, &["ctrl-delete", "alt-delete"]),
    (Cmd::DELETE_LINE, &["ctrl-shift-k"]),
    (Cmd::DUPLICATE_LINE, &["alt-shift-down", "alt-shift-up"]),
    (Cmd::MOVE_LINE_UP, &["alt-up"]),
    (Cmd::MOVE_LINE_DOWN, &["alt-down"]),
    (Cmd::JOIN_LINES, &["alt-j"]),
    // Recording what you do, and doing it again.
    (Cmd::RECORD_MACRO, &["alt-shift-r"]),
    (Cmd::PLAY_MACRO, &["alt-shift-p"]),
    (Cmd::INDENT, &["tab"]),
    (Cmd::UNINDENT, &["shift-tab"]),
    // Terminals disagree about Ctrl-/: some send it as Ctrl-_, which is the
    // same byte with a different name. Both are bound rather than explained.
    (Cmd::TOGGLE_COMMENT, &["ctrl-/", "ctrl-_"]),
    (Cmd::UNDO, &["ctrl-z"]),
    (Cmd::REDO, &["ctrl-y", "ctrl-shift-z"]),
    (Cmd::COPY, &["ctrl-c"]),
    (Cmd::CUT, &["ctrl-x"]),
    (Cmd::PASTE, &["ctrl-v"]),

    // Finding.
    (Cmd::FIND, &["ctrl-f"]),
    (Cmd::FIND_NEXT, &["f3"]),
    (Cmd::FIND_PREV, &["shift-f3"]),
    (Cmd::FIND_WORD_UNDER_CURSOR, &["alt-f"]),
    (Cmd::REPLACE, &["ctrl-h"]),
    // Ctrl-Shift-F is the key everyone reaches for, and it is also the one
    // most likely never to arrive: a terminal without the extended keyboard
    // protocol cannot tell it from Ctrl-F, and tmux, screen and a fair number
    // of desktops take it for themselves. So it is bound, and so are two keys
    // that always get through — Alt-G, and F7 for anything driving the
    // terminal down a pipe. Alt-G is shown in the help, because a key in the
    // help that does nothing on your machine is worse than no help.
    (Cmd::GREP, &["alt-g", "ctrl-shift-f", "f7"]),
    // Beside the search it is the other half of: what Ctrl-Shift-H is
    // elsewhere, without asking a terminal to tell Ctrl-Shift-H from Ctrl-H.
    (Cmd::REPLACE_IN_PROJECT, &["alt-shift-g"]),
    // Beside the keys for the next problem, since stepping through your own
    // changes is the same shape of thing as stepping through the compiler's.
    //
    // Ctrl and not bare F9, which every debugger ever written uses for a
    // breakpoint. That is the one key here textfold does not get to have an
    // opinion about: somebody who has used any other editor will press F9
    // expecting a breakpoint, and a key that does something else instead is
    // worse than a key that does nothing.
    (Cmd::NEXT_CHANGE, &["ctrl-f9"]),
    (Cmd::PREV_CHANGE, &["ctrl-shift-f9"]),

    // Language servers.
    (Cmd::COMPLETION, &["ctrl-space"]),
    (Cmd::GOTO_DEFINITION, &["f12", "ctrl-enter"]),
    (Cmd::REFERENCES, &["shift-f12"]),
    (Cmd::HOVER, &["alt-k"]),
    (Cmd::RENAME, &["f2"]),
    (Cmd::CODE_ACTION, &["alt-enter", "ctrl-."]),
    // The one that puts the missing import in without a list in between,
    // which is what you want ninety times out of a hundred.
    (Cmd::FIX_IT, &["alt-i"]),
    (Cmd::FORMAT, &["alt-shift-f"]),
    (Cmd::NEXT_DIAGNOSTIC, &["f8"]),
    (Cmd::PREV_DIAGNOSTIC, &["shift-f8"]),
    (Cmd::DIAGNOSTICS, &["alt-d"]),
    (Cmd::SYMBOLS, &["alt-o", "ctrl-shift-o"]),
    // Not Ctrl-Shift-Space: with shift folded into the character, that is the
    // same keystroke as Ctrl-Space, which already asks for completions.
    (Cmd::SIGNATURE_HELP, &["alt-p"]),

    // Debugging. The keys every debugger written in the last thirty years
    // uses, which is the whole of the argument for them — somebody who has
    // used Visual Studio, VS Code, IntelliJ or Eclipse already knows these,
    // and somebody who has not is going to look them up in one of those.
    (Cmd::DEBUG, &["f5"]),
    (Cmd::DEBUG_STOP, &["shift-f5"]),
    (Cmd::DEBUG_STEP_OVER, &["f10"]),
    (Cmd::DEBUG_STEP_INTO, &["f11"]),
    (Cmd::DEBUG_STEP_OUT, &["shift-f11"]),
    (Cmd::TOGGLE_BREAKPOINT, &["f9"]),
    (Cmd::DEBUG_PANEL, &["alt-5"]),
    // The key the same four editors use for a build, and a spare for the
    // terminals and desktops that keep Ctrl-Shift for themselves. Ctrl-B is
    // already the list of buffers and stays that way: a key people press
    // twenty times an hour beats one they press when a compile is due.
    (Cmd::BUILD, &["ctrl-shift-b", "f6"]),

    // Panes and the view.
    (Cmd::SPLIT, &["alt-v"]),
    (Cmd::CLOSE_PANE, &["alt-q"]),
    (Cmd::FOCUS_NEXT_PANE, &["alt-w"]),
    (Cmd::SWAP_SPLIT_DIRECTION, &["alt-\\"]),
    (Cmd::DIFF_PANES, &["alt-c"]),
    (Cmd::THEME_PICKER, &["alt-t"]),
    (Cmd::TOGGLE_LINE_NUMBERS, &["alt-n"]),
    (Cmd::TOGGLE_WRAP, &["alt-z"]),
    // Folding away what is under the cursor. `h` for hide: `f` is the search
    // for the word under the cursor and every other letter that means folding
    // is already something else.
    (Cmd::TOGGLE_FOLD, &["alt-h"]),
    (Cmd::FOLD_ALL, &["alt-shift-h"]),
    (Cmd::UNFOLD_ALL, &["alt-shift-u"]),

    // What a keyboard with a menu key sends, and the key Windows and GTK have
    // both meant by it for thirty years for keyboards without one.
    (Cmd::CONTEXT_MENU, &["shift-f10", "menu"]),

    (Cmd::ESCAPE, &["esc"]),
];

/// The bindings in force: what textfold ships, with your changes folded in.
pub struct Keys {
    map: HashMap<Key, Cmd>,
    /// The other way round, for showing "Ctrl-S" beside "save". Kept in step
    /// with the map rather than worked out on demand, since the help screen
    /// and every palette row asks for it.
    shown: BTreeMap<Cmd, Vec<Key>>,
    /// Bindings in the settings file that name nothing, worth one complaint at
    /// startup: a key that silently does nothing is a bad afternoon.
    pub problems: Vec<String>,
}

impl Default for Keys {
    fn default() -> Self {
        Self::new(&BTreeMap::new())
    }
}

impl Keys {
    /// Just the scheme textfold ships, with nothing on top of it.
    ///
    /// Split out from [`Keys::new`] because "what the editor itself binds" is
    /// a different question from "what is bound on this machine", and a test
    /// asking the first one should not get a different answer depending on
    /// which plugins the person running it happens to have installed.
    fn built_in() -> Self {
        let mut keys = Self {
            map: HashMap::new(),
            shown: BTreeMap::new(),
            problems: Vec::new(),
        };
        for (cmd, bindings) in DEFAULTS {
            for text in *bindings {
                match Key::parse(text) {
                    Some(key) => keys.bind(key, *cmd),
                    // A default that will not parse is our mistake, and a test
                    // below catches it before anybody sees this.
                    None => keys
                        .problems
                        .push(format!("{text:?} is not a key (built in)")),
                }
            }
        }
        keys
    }

    /// The scheme, with `overrides` — command name to list of keys — on top.
    pub fn new(overrides: &BTreeMap<String, Vec<String>>) -> Self {
        let mut keys = Self::built_in();
        // What the plugins would like bound. Only where the key is going
        // spare: a plugin gets to suggest a key, not to take one. A plugin
        // that quietly rebound Ctrl-S would be a plugin nobody could install
        // safely, and the answer if you want it anyway is one line in your own
        // settings, which is read after this and wins.
        for plugin in crate::plugin::active() {
            for (name, bindings) in &plugin.keys {
                let Some(cmd) = crate::cmd::by_name(name) else {
                    keys.problems.push(format!(
                        "{}: there is no command called {name:?}",
                        plugin.id
                    ));
                    continue;
                };
                for text in bindings {
                    match Key::parse(text) {
                        Some(key) => keys.suggest(key, cmd),
                        None => keys
                            .problems
                            .push(format!("{}: {text:?} is not a key", plugin.id)),
                    }
                }
            }
        }

        for (name, bindings) in overrides {
            let Some(cmd) = crate::cmd::by_name(name) else {
                keys.problems
                    .push(format!("there is no command called {name:?}"));
                continue;
            };
            // Naming a command takes back every key it had, so that a
            // rebinding does not leave the old key still working.
            for key in keys.shown.remove(&cmd).unwrap_or_default() {
                keys.map.remove(&key);
            }
            for text in bindings {
                match Key::parse(text) {
                    Some(key) => keys.bind(key, cmd),
                    None => keys.problems.push(format!("{text:?} is not a key")),
                }
            }
        }
        keys
    }

    fn bind(&mut self, key: Key, cmd: Cmd) {
        // A key bound twice belongs to whoever asked last: that is what makes
        // an override an override.
        if let Some(old) = self.map.insert(key, cmd)
            && let Some(list) = self.shown.get_mut(&old)
        {
            list.retain(|k| *k != key);
        }
        self.shown.entry(cmd).or_default().push(key);
    }

    /// Bind a key only if it is going spare.
    ///
    /// What a plugin's suggestions get. The difference between this and
    /// [`Keys::bind`] is the whole of what makes a plugin safe to install: it
    /// can have the keys nothing else wanted, and it cannot take one you were
    /// already using.
    fn suggest(&mut self, key: Key, cmd: Cmd) {
        if !self.map.contains_key(&key) {
            self.bind(key, cmd);
        }
    }

    /// What this keystroke does, if anything.
    pub fn lookup(&self, key: Key) -> Option<Cmd> {
        self.map.get(&key).copied()
    }

    /// The keys that reach a command, best first.
    pub fn keys_for(&self, cmd: Cmd) -> &[Key] {
        self.shown.get(&cmd).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The one key to show beside a command in a list.
    pub fn shortcut(&self, cmd: Cmd) -> Option<String> {
        self.keys_for(cmd).first().map(Key::show)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_textfold_ships_is_a_key() {
        let keys = Keys::default();
        assert!(keys.problems.is_empty(), "{:?}", keys.problems);
    }

    #[test]
    fn nothing_shipped_is_bound_to_two_things() {
        let mut seen: HashMap<Key, Cmd> = HashMap::new();
        for (cmd, bindings) in DEFAULTS {
            for text in *bindings {
                let key = Key::parse(text).expect("parses");
                if let Some(other) = seen.insert(key, *cmd) {
                    panic!("{text} is both {} and {}", other.name(), cmd.name());
                }
            }
        }
    }

    #[test]
    fn nothing_is_reachable_only_by_a_key_the_desktop_takes() {
        // A compositor gets a keystroke before the terminal running under it
        // does, and both GNOME and KDE bind Ctrl-Alt with an arrow to
        // switching workspace. Shipping one of those is fine — plenty of
        // people have turned it off. Shipping one as the *only* way to reach a
        // command means shipping a command that, on a stock desktop, does
        // nothing at all and gives no sign of why.
        let taken = |key: &Key| {
            key.mods.contains(KeyModifiers::CONTROL)
                && key.mods.contains(KeyModifiers::ALT)
                && matches!(
                    key.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
                )
        };
        let keys = Keys::default();
        for cmd in crate::cmd::all() {
            let bound = keys.keys_for(cmd);
            assert!(
                bound.is_empty() || !bound.iter().all(taken),
                "{} can only be reached by a key the desktop takes first",
                cmd.name()
            );
        }
    }

    #[test]
    fn the_key_shown_for_a_command_is_one_that_arrives() {
        // The help screen and the palette show the first binding, so the first
        // binding has to be the one that works.
        let keys = Keys::default();
        assert_eq!(
            keys.shortcut(Cmd::ADD_CURSOR_ABOVE).as_deref(),
            Some("Ctrl-Shift-\u{2191}")
        );
        assert_eq!(
            keys.shortcut(Cmd::ADD_CURSOR_BELOW).as_deref(),
            Some("Ctrl-Shift-\u{2193}")
        );
        let press = |code, mods| keys.lookup(Key::from_event(KeyEvent::new(code, mods)));
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(press(KeyCode::Up, ctrl_shift), Some(Cmd::ADD_CURSOR_ABOVE));
        assert_eq!(press(KeyCode::Down, ctrl_shift), Some(Cmd::ADD_CURSOR_BELOW));
        // A modified arrow is one escape sequence with a number in it, so
        // Ctrl-Shift-Up is a different key from Ctrl-Up rather than the same
        // byte twice — which is why this pair can be the one shown.
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(press(KeyCode::Up, ctrl), Some(Cmd::MOVE_PARA_UP));
        // Still bound, for a desktop that leaves them alone.
        let ctrl_alt = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert_eq!(press(KeyCode::Up, ctrl_alt), Some(Cmd::ADD_CURSOR_ABOVE));
        assert_eq!(press(KeyCode::Down, ctrl_alt), Some(Cmd::ADD_CURSOR_BELOW));
    }

    #[test]
    fn the_ordinary_keys_all_do_the_ordinary_thing() {
        let keys = Keys::default();
        let press = |code, mods| keys.lookup(Key::from_event(KeyEvent::new(code, mods)));
        assert_eq!(
            press(KeyCode::Char('s'), KeyModifiers::CONTROL),
            Some(Cmd::SAVE)
        );
        assert_eq!(
            press(KeyCode::Char('z'), KeyModifiers::CONTROL),
            Some(Cmd::UNDO)
        );
        assert_eq!(press(KeyCode::Left, KeyModifiers::SHIFT), Some(Cmd::EXTEND_LEFT));
        assert_eq!(press(KeyCode::Esc, KeyModifiers::NONE), Some(Cmd::ESCAPE));
    }

    #[test]
    fn shift_on_a_letter_is_the_letter_however_the_terminal_says_it() {
        // A plain terminal: the character is already capital.
        let plain = Key::from_event(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::CONTROL));
        // One with the extended protocol: capital *and* a shift flag.
        let extended = Key::from_event(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        // And one that sends the lower-case letter with the flag.
        let third = Key::from_event(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(plain, extended);
        assert_eq!(plain, third);
        assert_eq!(Key::parse("ctrl-shift-p"), Some(plain));
    }

    #[test]
    fn shift_tab_and_backtab_are_one_key() {
        let backtab = Key::from_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        let shift_tab = Key::from_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(backtab, shift_tab);
        assert_eq!(Keys::default().lookup(backtab), Some(Cmd::UNINDENT));
    }

    #[test]
    fn a_capital_letter_is_written_as_the_shift_it_is() {
        // Ctrl-Shift-K arrives as a capital K with control on it; showing that
        // as "Ctrl-K" would name a different key that does something else.
        assert_eq!(Key::parse("ctrl-shift-k").unwrap().show(), "Ctrl-Shift-k");
        assert_eq!(Key::parse("ctrl-k").unwrap().show(), "Ctrl-k");
        assert_eq!(Key::parse("shift-left").unwrap().show(), "Shift-\u{2190}");
    }

    #[test]
    fn keys_are_written_the_way_people_write_them() {
        assert!(Key::parse("ctrl-s").is_some());
        assert!(Key::parse("alt-shift-up").is_some());
        assert!(Key::parse("f12").is_some());
        assert_eq!(Key::parse("space").and_then(|k| k.as_typed()), Some(' '));
        // The key itself can be a dash.
        assert_eq!(
            Key::parse("ctrl--"),
            Some(Key {
                code: KeyCode::Char('-'),
                mods: KeyModifiers::CONTROL
            })
        );
        assert_eq!(Key::parse("hyper-s"), None);
        assert_eq!(Key::parse(""), None);
    }

    #[test]
    fn rebinding_a_command_takes_back_the_key_it_had() {
        let mut overrides = BTreeMap::new();
        overrides.insert("save".to_string(), vec!["f4".to_string()]);
        let keys = Keys::new(&overrides);
        assert_eq!(
            keys.lookup(Key::parse("f4").unwrap()),
            Some(Cmd::SAVE)
        );
        assert_eq!(keys.lookup(Key::parse("ctrl-s").unwrap()), None);
    }

    #[test]
    fn an_empty_list_unbinds() {
        let mut overrides = BTreeMap::new();
        overrides.insert("quit".to_string(), vec![]);
        let keys = Keys::new(&overrides);
        assert_eq!(keys.lookup(Key::parse("ctrl-q").unwrap()), None);
        // And the rest of the scheme is untouched.
        assert_eq!(keys.lookup(Key::parse("ctrl-s").unwrap()), Some(Cmd::SAVE));
    }

    #[test]
    fn a_command_that_does_not_exist_is_worth_saying_so() {
        let mut overrides = BTreeMap::new();
        overrides.insert("frobnicate".to_string(), vec!["f9".to_string()]);
        let keys = Keys::new(&overrides);
        assert_eq!(keys.problems.len(), 1);
        assert!(keys.problems[0].contains("frobnicate"));
    }

    #[test]
    fn a_typing_key_is_told_from_a_command_key() {
        assert_eq!(Key::parse("a").and_then(|k| k.as_typed()), Some('a'));
        assert_eq!(Key::parse("shift-a").and_then(|k| k.as_typed()), Some('A'));
        assert_eq!(Key::parse("ctrl-a").and_then(|k| k.as_typed()), None);
        assert_eq!(Key::parse("alt-a").and_then(|k| k.as_typed()), None);
    }

    #[test]
    fn a_key_is_spelled_the_way_a_settings_file_spells_it() {
        // Which means it reads back: whatever a plugin is told was pressed,
        // it could have written in its own manifest to ask for.
        for text in ["ctrl-s", "alt-shift-up", "f12", "enter", "r", "space", "ctrl-."] {
            let key = Key::parse(text).unwrap_or_else(|| panic!("{text} should parse"));
            assert_eq!(key.spelled(), text);
            assert_eq!(Key::parse(&key.spelled()), Some(key), "{text} did not read back");
        }
        // A capital letter is shift, said the one way rather than two.
        assert_eq!(Key::parse("K").map(|k| k.spelled()), Some("shift-k".into()));
    }

    #[test]
    fn a_plugin_may_have_the_keys_nobody_else_wanted() {
        // The built-ins alone: whether the spare key is spare must not depend
        // on which plugins the person running the tests has installed.
        let mut keys = Keys::built_in();
        let taken = Key::parse("ctrl-s").expect("a key");
        let spare = Key::parse("ctrl-f6").expect("a key");
        let was = keys.lookup(taken);
        assert!(was.is_some(), "ctrl-s should already do something");

        keys.suggest(taken, Cmd::ABOUT);
        assert_eq!(
            keys.lookup(taken),
            was,
            "a plugin took a key that was already doing something"
        );

        keys.suggest(spare, Cmd::ABOUT);
        assert_eq!(keys.lookup(spare), Some(Cmd::ABOUT));
    }

    #[test]
    fn what_you_bind_yourself_beats_what_a_plugin_suggested() {
        let mut keys = Keys::built_in();
        let spare = Key::parse("ctrl-f6").expect("a key");
        keys.suggest(spare, Cmd::ABOUT);
        // Which is what an override does: `bind`, not `suggest`.
        keys.bind(spare, Cmd::HELP);
        assert_eq!(keys.lookup(spare), Some(Cmd::HELP));
    }
}
