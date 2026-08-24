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
    (Cmd::Quit, &["ctrl-q"]),
    (Cmd::Save, &["ctrl-s"]),
    (Cmd::SaveAs, &["alt-s"]),
    (Cmd::Close, &["ctrl-w"]),
    (Cmd::New, &["ctrl-n"]),
    (Cmd::Open, &["ctrl-p", "ctrl-o"]),
    // Two keys because this one is also typed by other programs rather than
    // by people: `alt-e` is short, and F4 is one self-contained escape
    // sequence, which is the safer thing to write down a pipe.
    (Cmd::OpenPath, &["alt-e", "f4"]),
    (Cmd::Buffers, &["ctrl-b"]),
    (Cmd::NextBuffer, &["alt-."]),
    (Cmd::PrevBuffer, &["alt-,"]),
    (Cmd::CommandPalette, &["alt-x", "ctrl-shift-p"]),
    (Cmd::Help, &["f1"]),

    // Moving.
    (Cmd::MoveLeft, &["left"]),
    (Cmd::MoveRight, &["right"]),
    (Cmd::MoveUp, &["up"]),
    (Cmd::MoveDown, &["down"]),
    (Cmd::MoveWordLeft, &["ctrl-left", "alt-left"]),
    (Cmd::MoveWordRight, &["ctrl-right", "alt-right"]),
    (Cmd::MoveLineStart, &["home"]),
    (Cmd::MoveLineEnd, &["end"]),
    (Cmd::MovePageUp, &["pageup"]),
    (Cmd::MovePageDown, &["pagedown"]),
    (Cmd::MoveDocStart, &["ctrl-home"]),
    (Cmd::MoveDocEnd, &["ctrl-end"]),
    (Cmd::MoveParaUp, &["ctrl-up"]),
    (Cmd::MoveParaDown, &["ctrl-down"]),
    (Cmd::MatchBracket, &["alt-b"]),
    (Cmd::GotoLine, &["ctrl-g"]),
    (Cmd::JumpBack, &["alt-["]),
    (Cmd::JumpForward, &["alt-]"]),
    (Cmd::CentreCursor, &["alt-m"]),

    // Selecting. Shift and a movement, which is what it is everywhere.
    (Cmd::ExtendLeft, &["shift-left"]),
    (Cmd::ExtendRight, &["shift-right"]),
    (Cmd::ExtendUp, &["shift-up"]),
    (Cmd::ExtendDown, &["shift-down"]),
    (Cmd::ExtendWordLeft, &["ctrl-shift-left", "alt-shift-left"]),
    (Cmd::ExtendWordRight, &["ctrl-shift-right", "alt-shift-right"]),
    (Cmd::ExtendLineStart, &["shift-home"]),
    (Cmd::ExtendLineEnd, &["shift-end"]),
    (Cmd::ExtendPageUp, &["shift-pageup"]),
    (Cmd::ExtendPageDown, &["shift-pagedown"]),
    (Cmd::ExtendDocStart, &["ctrl-shift-home"]),
    (Cmd::ExtendDocEnd, &["ctrl-shift-end"]),
    (Cmd::SelectAll, &["ctrl-a"]),
    (Cmd::SelectLine, &["ctrl-l"]),
    (Cmd::ExpandSelection, &["alt-="]),
    // Another cursor above or below is Ctrl-Alt-arrow in every editor that has
    // the feature, and a desktop is allowed to take a keystroke before the
    // terminal under it ever sees it — GNOME and KDE both take exactly that
    // one for switching workspace. So the first key here is Ctrl-Shift-arrow,
    // which nothing above the terminal wants and which every terminal encodes,
    // and Ctrl-Alt-arrow is bound beside it for the desktops that leave it
    // alone. The first is what the help screen shows, because a key in the
    // help that does nothing is worse than no help.
    (Cmd::AddCursorAbove, &["ctrl-shift-up", "ctrl-alt-up"]),
    (Cmd::AddCursorBelow, &["ctrl-shift-down", "ctrl-alt-down"]),
    (Cmd::AddCursorNextMatch, &["ctrl-d"]),
    (Cmd::SelectAllMatches, &["ctrl-shift-l"]),
    (Cmd::CursorsToLineEnds, &["alt-shift-i"]),
    (Cmd::CollapseCursors, &["alt-shift-c"]),

    // Changing text.
    (Cmd::InsertNewline, &["enter"]),
    (Cmd::DeleteBackward, &["backspace"]),
    (Cmd::DeleteForward, &["delete"]),
    (Cmd::DeleteWordBackward, &["ctrl-backspace", "alt-backspace"]),
    (Cmd::DeleteWordForward, &["ctrl-delete", "alt-delete"]),
    (Cmd::DeleteLine, &["ctrl-shift-k"]),
    (Cmd::DuplicateLine, &["alt-shift-down", "alt-shift-up"]),
    (Cmd::MoveLineUp, &["alt-up"]),
    (Cmd::MoveLineDown, &["alt-down"]),
    (Cmd::JoinLines, &["alt-j"]),
    (Cmd::Indent, &["tab"]),
    (Cmd::Unindent, &["shift-tab"]),
    // Terminals disagree about Ctrl-/: some send it as Ctrl-_, which is the
    // same byte with a different name. Both are bound rather than explained.
    (Cmd::ToggleComment, &["ctrl-/", "ctrl-_"]),
    (Cmd::Undo, &["ctrl-z"]),
    (Cmd::Redo, &["ctrl-y", "ctrl-shift-z"]),
    (Cmd::Copy, &["ctrl-c"]),
    (Cmd::Cut, &["ctrl-x"]),
    (Cmd::Paste, &["ctrl-v"]),

    // Finding.
    (Cmd::Find, &["ctrl-f"]),
    (Cmd::FindNext, &["f3"]),
    (Cmd::FindPrev, &["shift-f3"]),
    (Cmd::FindWordUnderCursor, &["alt-f"]),
    (Cmd::Replace, &["ctrl-h"]),
    (Cmd::Grep, &["ctrl-shift-f"]),

    // Language servers.
    (Cmd::Completion, &["ctrl-space"]),
    (Cmd::GotoDefinition, &["f12", "ctrl-enter"]),
    (Cmd::References, &["shift-f12"]),
    (Cmd::Hover, &["alt-k"]),
    (Cmd::Rename, &["f2"]),
    (Cmd::CodeAction, &["alt-enter", "ctrl-."]),
    (Cmd::Format, &["alt-shift-f"]),
    (Cmd::NextDiagnostic, &["f8"]),
    (Cmd::PrevDiagnostic, &["shift-f8"]),
    (Cmd::Diagnostics, &["alt-d"]),
    (Cmd::Symbols, &["alt-o", "ctrl-shift-o"]),
    // Not Ctrl-Shift-Space: with shift folded into the character, that is the
    // same keystroke as Ctrl-Space, which already asks for completions.
    (Cmd::SignatureHelp, &["alt-p"]),

    // Panes and the view.
    (Cmd::Split, &["alt-v"]),
    (Cmd::ClosePane, &["alt-q"]),
    (Cmd::FocusNextPane, &["alt-w"]),
    (Cmd::SwapSplitDirection, &["alt-\\"]),
    (Cmd::ThemePicker, &["alt-t"]),
    (Cmd::ToggleLineNumbers, &["alt-n"]),
    (Cmd::ToggleWrap, &["alt-z"]),

    (Cmd::Escape, &["esc"]),
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
    /// The scheme, with `overrides` — command name to list of keys — on top.
    pub fn new(overrides: &BTreeMap<String, Vec<String>>) -> Self {
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
        for (name, bindings) in overrides {
            let Some(cmd) = Cmd::from_name(name) else {
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
        for cmd in crate::cmd::ALL {
            let bound = keys.keys_for(*cmd);
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
            keys.shortcut(Cmd::AddCursorAbove).as_deref(),
            Some("Ctrl-Shift-\u{2191}")
        );
        assert_eq!(
            keys.shortcut(Cmd::AddCursorBelow).as_deref(),
            Some("Ctrl-Shift-\u{2193}")
        );
        let press = |code, mods| keys.lookup(Key::from_event(KeyEvent::new(code, mods)));
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(press(KeyCode::Up, ctrl_shift), Some(Cmd::AddCursorAbove));
        assert_eq!(press(KeyCode::Down, ctrl_shift), Some(Cmd::AddCursorBelow));
        // A modified arrow is one escape sequence with a number in it, so
        // Ctrl-Shift-Up is a different key from Ctrl-Up rather than the same
        // byte twice — which is why this pair can be the one shown.
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(press(KeyCode::Up, ctrl), Some(Cmd::MoveParaUp));
        // Still bound, for a desktop that leaves them alone.
        let ctrl_alt = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert_eq!(press(KeyCode::Up, ctrl_alt), Some(Cmd::AddCursorAbove));
        assert_eq!(press(KeyCode::Down, ctrl_alt), Some(Cmd::AddCursorBelow));
    }

    #[test]
    fn the_ordinary_keys_all_do_the_ordinary_thing() {
        let keys = Keys::default();
        let press = |code, mods| keys.lookup(Key::from_event(KeyEvent::new(code, mods)));
        assert_eq!(
            press(KeyCode::Char('s'), KeyModifiers::CONTROL),
            Some(Cmd::Save)
        );
        assert_eq!(
            press(KeyCode::Char('z'), KeyModifiers::CONTROL),
            Some(Cmd::Undo)
        );
        assert_eq!(press(KeyCode::Left, KeyModifiers::SHIFT), Some(Cmd::ExtendLeft));
        assert_eq!(press(KeyCode::Esc, KeyModifiers::NONE), Some(Cmd::Escape));
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
        assert_eq!(Keys::default().lookup(backtab), Some(Cmd::Unindent));
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
            Some(Cmd::Save)
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
        assert_eq!(keys.lookup(Key::parse("ctrl-s").unwrap()), Some(Cmd::Save));
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
}
