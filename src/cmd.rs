//! Everything textfold can be told to do — and the machinery that lets
//! something outside textfold add to that list.
//!
//! A command is an id and nothing else: [`Cmd`] is a number into a registry.
//! That is the whole trick. Keys, the command palette, the context menus and
//! the status bar buttons all hold a `Cmd`, and none of them knows or cares
//! whether the command behind it is one textfold ships or one a plugin brought
//! with it. Adding a command is adding a row, not editing five files.
//!
//! The ones textfold ships are the table in [`crate::app::BUILT_IN`] — one row
//! each, giving the name, the group, whether it changes the text, the line the
//! palette shows, and what it actually does. There is no second list to keep
//! in step: the key bindings, the palette and the menus all read that one, and
//! a row that does not say what it does will not compile.
//!
//! Built-ins take the first block of ids, in table order, which is what lets
//! `Cmd::SAVE` be an ordinary constant. Everything contributed comes after
//! them, numbered as it is first seen and kept — so a command keeps its id
//! when a plugin is switched off and on again, and a key bound to it goes on
//! meaning the same thing.

use std::sync::{Mutex, OnceLock, RwLock};

use crate::app::App;
use crate::plugin::{Command as PluginCommand, Tool};

/// One thing textfold can be told to do.
///
/// Deliberately opaque and deliberately `Copy`: it is a number, it is cheap to
/// pass about, and nothing outside this module can take it apart and start
/// depending on which command is which.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Cmd(u16);

impl Cmd {
    /// The command with this id. Only the table's own macro should call this;
    /// everywhere else names a constant or looks a name up.
    pub(crate) const fn at(index: u16) -> Cmd {
        Cmd(index)
    }
}

/// What part of the editor a command belongs to. Shown beside it in the
/// palette, so a list of a hundred and forty things reads as eight lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    File,
    Edit,
    Move,
    Select,
    Search,
    Code,
    View,
    Help,
    /// Something a plugin runs for you: a formatter, a linter, a test run.
    Tool,
}

impl Group {
    pub fn label(&self) -> &'static str {
        match self {
            Group::File => "file",
            Group::Edit => "edit",
            Group::Move => "move",
            Group::Select => "select",
            Group::Search => "search",
            Group::Code => "code",
            Group::View => "view",
            Group::Help => "help",
            Group::Tool => "tool",
        }
    }
}

/// What a command does to the text, which is the only thing the editor needs
/// to know about one before running it.
///
/// One column rather than two lists kept by hand. Whether a command may run on
/// a read-only file and whether the next keystroke can join its undo step are
/// the same question asked twice, and asking it once in the table is what
/// stops a command from being added to one list and forgotten in the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Behaviour {
    /// Leaves the text alone. Moving, searching, opening things.
    Passive,
    /// Changes the text, and stands on its own in the undo history.
    Edits,
    /// Changes the text the way typing does: quick keystrokes in a row merge
    /// into one thing to undo.
    Types,
}

impl Behaviour {
    /// Whether this needs a document that can be changed.
    pub fn writes(&self) -> bool {
        !matches!(self, Behaviour::Passive)
    }

    /// Whether a following keystroke can join the same undo step. Anything
    /// that moves rather than types closes the current one, so that undo goes
    /// back to a place you recognise.
    pub fn joins(&self) -> bool {
        matches!(self, Behaviour::Types)
    }
}

/// One built-in command, as the table writes it.
pub struct Spec {
    pub name: &'static str,
    pub group: Group,
    pub behaviour: Behaviour,
    /// The line under the name in the palette. Not decoration: for most people
    /// it is the only documentation they will read, so it says what the
    /// command does rather than restating its name.
    pub about: &'static str,
    pub run: fn(&mut App),
}

/// What running a command means.
#[derive(Clone, Copy)]
pub enum Run {
    /// Something textfold does itself.
    Built(fn(&mut App)),
    /// A program a plugin brought with it.
    Tool(&'static Tool),
    /// Something a plugin's own long-running program does. The editor sends
    /// it the name and gets on with the next keystroke; what comes back
    /// arrives later, like everything else that is not the keyboard.
    Plugin(&'static PluginCommand),
    /// Contributed by a plugin that is switched off. The id is kept — a key
    /// bound to it should say where the command went rather than do nothing.
    Gone,
}

/// One command as the registry holds it.
pub struct Entry {
    pub name: String,
    pub about: String,
    pub group: Group,
    pub behaviour: Behaviour,
    pub run: Run,
}

// ---- The registry ----

/// Which id each command name has, in order, growing only. What makes a `Cmd`
/// mean the same thing before and after a plugin is switched off.
static NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Swapped whole when the plugins change. The old one is leaked rather than
/// dropped, because a `&'static Entry` handed out before the swap may still be
/// in a menu that is on the screen. This happens a handful of times in a
/// session, so a few stale tables is a price worth not thinking about.
static REGISTRY: OnceLock<RwLock<&'static [Entry]>> = OnceLock::new();

fn cell() -> &'static RwLock<&'static [Entry]> {
    REGISTRY.get_or_init(|| RwLock::new(build()))
}

/// Build the registry. Called once, before anything asks after a command.
pub fn init() {
    cell();
}

/// Read the plugins again and build the table afresh, for after one has been
/// turned on or off.
pub fn rebuild() {
    *cell().write().unwrap_or_else(|e| e.into_inner()) = build();
}

fn entries() -> &'static [Entry] {
    *cell().read().unwrap_or_else(|e| e.into_inner())
}

/// Every command that can be run now, in table order — which is the order the
/// palette shows them in when nothing has been typed, so it is grouped the way
/// a person would look for them.
///
/// A command whose plugin is switched off is not in this list. It still has
/// its id, so a key bound to it still resolves; it simply is not offered.
pub fn all() -> Vec<Cmd> {
    entries()
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e.run, Run::Gone))
        .map(|(at, _)| Cmd(at as u16))
        .collect()
}

/// The command of that name, for a key binding in a settings file.
pub fn by_name(name: &str) -> Option<Cmd> {
    let name = name.trim();
    entries()
        .iter()
        .position(|e| e.name == name)
        .map(|at| Cmd(at as u16))
}

fn entry(cmd: Cmd) -> Option<&'static Entry> {
    entries().get(cmd.0 as usize)
}

impl Cmd {
    /// The name in a settings file and in the palette.
    pub fn name(&self) -> &'static str {
        entry(*self).map(|e| e.name.as_str()).unwrap_or("")
    }

    pub fn about(&self) -> &'static str {
        entry(*self).map(|e| e.about.as_str()).unwrap_or("")
    }

    pub fn group(&self) -> Group {
        entry(*self).map(|e| e.group).unwrap_or(Group::Help)
    }

    pub fn behaviour(&self) -> Behaviour {
        entry(*self)
            .map(|e| e.behaviour)
            .unwrap_or(Behaviour::Passive)
    }

    /// Whether this changes the text, and so needs a document that can be
    /// changed. Asked in one place so that a read-only file cannot be edited
    /// by a route somebody forgot about.
    pub fn writes(&self) -> bool {
        self.behaviour().writes()
    }

    /// What to actually do.
    pub fn run(&self) -> Run {
        entry(*self).map(|e| e.run).unwrap_or(Run::Gone)
    }

    /// The program behind this, where it is a tool a plugin brought. What the
    /// palette asks before offering a Python formatter in a Rust file.
    pub fn tool(&self) -> Option<&'static Tool> {
        match self.run() {
            Run::Tool(tool) => Some(tool),
            _ => None,
        }
    }

    /// The plugin command behind this, where it is one. What the palette asks
    /// before offering a Rust plugin's command in a Python file.
    pub fn plugin_command(&self) -> Option<&'static PluginCommand> {
        match self.run() {
            Run::Plugin(command) => Some(command),
            _ => None,
        }
    }
}

/// Build the table: the built-ins first, in the order they are written, and
/// then whatever the plugins that are on have brought.
fn build() -> &'static [Entry] {
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if names.is_empty() {
        // The built-ins take the first block of ids, in table order, which is
        // the promise `Cmd::SAVE` and its neighbours are built on.
        names.extend(
            crate::app::BUILT_IN
                .iter()
                .map(|spec| spec.name.to_string()),
        );
    }

    let mut tools: Vec<&'static Tool> = Vec::new();
    let mut commands: Vec<&'static PluginCommand> = Vec::new();
    for plugin in crate::plugin::active() {
        for tool in &plugin.tools {
            if !crate::plugin::is_on(&tool.id) {
                continue;
            }
            if !names.contains(&tool.id) {
                names.push(tool.id.clone());
            }
            tools.push(tool);
        }
        // A command a plugin's own program answers to. It takes an id here,
        // before that program has ever been started — which is what lets the
        // palette offer it, a key bind to it, and running it be the thing
        // that starts the program.
        for command in &plugin.commands {
            if !crate::plugin::is_on(&command.id) {
                continue;
            }
            if !names.contains(&command.id) {
                names.push(command.id.clone());
            }
            commands.push(command);
        }
    }

    let table: Vec<Entry> = names
        .iter()
        .enumerate()
        .map(|(at, name)| match crate::app::BUILT_IN.get(at) {
            // A built-in, still where it was written.
            Some(spec) if spec.name == name => Entry {
                name: spec.name.to_string(),
                about: spec.about.to_string(),
                group: spec.group,
                behaviour: spec.behaviour,
                run: Run::Built(spec.run),
            },
            _ => match tools.iter().find(|t| &t.id == name) {
                Some(tool) => Entry {
                    name: tool.id.clone(),
                    about: tool.about.clone(),
                    group: Group::Tool,
                    behaviour: tool.behaviour(),
                    run: Run::Tool(tool),
                },
                None if commands.iter().any(|c| &c.id == name) => {
                    let command = commands.iter().find(|c| &c.id == name).copied();
                    let command = command.expect("just found it");
                    Entry {
                        name: command.id.clone(),
                        about: command.about.clone(),
                        group: Group::Tool,
                        behaviour: command.behaviour,
                        run: Run::Plugin(command),
                    }
                }
                // Contributed by something that is switched off now. The id
                // stays taken so that turning it back on gets it back.
                None => Entry {
                    name: name.clone(),
                    about: String::new(),
                    group: Group::Tool,
                    behaviour: Behaviour::Passive,
                    run: Run::Gone,
                },
            },
        })
        .collect();
    Box::leak(table.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_command_has_its_own_name() {
        let mut seen = HashSet::new();
        for spec in crate::app::BUILT_IN {
            assert!(seen.insert(spec.name), "two commands called {}", spec.name);
        }
    }

    #[test]
    fn a_name_finds_the_command_it_names() {
        init();
        for spec in crate::app::BUILT_IN {
            let found = by_name(spec.name).unwrap_or_else(|| panic!("{} is lost", spec.name));
            assert_eq!(found.name(), spec.name);
        }
        assert_eq!(by_name("fly-to-the-moon"), None);
    }

    #[test]
    fn the_constants_point_at_the_commands_they_are_named_for() {
        // The consts are worked out from the table at compile time, so a name
        // changed in one and not the other is a build that does not happen —
        // but that the numbering lines up at all is worth one assertion.
        init();
        assert_eq!(Cmd::SAVE.name(), "save");
        assert_eq!(Cmd::QUIT.name(), "quit");
        assert_eq!(Cmd::ABOUT.name(), "about");
    }

    #[test]
    fn descriptions_say_something_the_name_does_not() {
        for spec in crate::app::BUILT_IN {
            assert!(!spec.about.is_empty(), "{} says nothing", spec.name);
            assert!(
                spec.about.chars().next().is_some_and(char::is_uppercase),
                "{} starts lowercase",
                spec.name
            );
        }
    }

    #[test]
    fn nothing_contributed_can_take_a_built_in_name() {
        // Everything a plugin brings is named `plugin/thing`, and no built-in
        // has a slash in its name. That is what keeps the first block of ids —
        // the one the constants are worked out from — the built-ins' own.
        for spec in crate::app::BUILT_IN {
            assert!(
                !spec.name.contains('/'),
                "{} could be shadowed by a plugin",
                spec.name
            );
        }
    }

    #[test]
    fn what_a_command_does_to_the_text_is_asked_once() {
        // The two questions that used to be two hand-kept lists.
        assert!(Behaviour::Types.writes(), "typing changes the text");
        assert!(Behaviour::Types.joins());
        assert!(Behaviour::Edits.writes());
        assert!(!Behaviour::Edits.joins(), "an edit stands on its own to undo");
        assert!(!Behaviour::Passive.writes());
        assert!(!Behaviour::Passive.joins());
    }
}
