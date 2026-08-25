//! Everything textfold can be told to do, as one list.
//!
//! One name for each action, used in three places at once: the key bindings
//! that ship, the key bindings you write, and the command palette. There is no
//! second list to keep in step, so a command that exists is a command you can
//! bind and a command you can search for, without anybody remembering to add
//! it in three files.
//!
//! The one-line description is not decoration. It is what the palette shows
//! and, for most people, the only documentation they will read, so it says
//! what the command does rather than restating its name.

macro_rules! commands {
    ($($variant:ident => $name:literal, $group:ident, $about:literal;)*) => {
        /// One thing textfold can be told to do.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
        pub enum Cmd {
            $($variant,)*
        }

        /// Every command there is, in the order written below — which is the
        /// order the palette shows them in when you have not typed anything,
        /// so it is grouped the way a person would look for them.
        pub const ALL: &[Cmd] = &[$(Cmd::$variant,)*];

        impl Cmd {
            /// The name in a config file and in the palette.
            pub fn name(&self) -> &'static str {
                match self { $(Cmd::$variant => $name,)* }
            }

            /// The line under the name in the palette.
            pub fn about(&self) -> &'static str {
                match self { $(Cmd::$variant => $about,)* }
            }

            pub fn group(&self) -> Group {
                match self { $(Cmd::$variant => Group::$group,)* }
            }

            /// The command of that name, for a key binding in a config file.
            pub fn from_name(name: &str) -> Option<Cmd> {
                let name = name.trim();
                match name { $($name => Some(Cmd::$variant),)* _ => None }
            }
        }
    };
}

/// What part of the editor a command belongs to. Shown beside it in the
/// palette, so a list of ninety things reads as six lists of fifteen.
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
        }
    }
}

commands! {
    // ---- Files and buffers ----
    New => "new", File, "Start an empty buffer";
    Open => "open", File, "Open a file by name, fuzzily";
    OpenPath => "open-path", File, "Open a file by typing its path, exactly";
    Save => "save", File, "Write this file to disk";
    SaveAs => "save-as", File, "Write this file somewhere else";
    SaveAll => "save-all", File, "Write every changed file";
    Reload => "reload", File, "Read this file again, throwing away changes";
    Close => "close", File, "Close this buffer, asking about unsaved changes";
    CloseForce => "close!", File, "Close this buffer, changes and all";
    CloseOthers => "close-others", File, "Close every buffer but this one";
    CloseSaved => "close-saved", File, "Close every buffer with nothing unsaved in it";
    CloseAll => "close-all", File, "Close every buffer";
    CopyPath => "copy-path", File, "Copy this file's full path";
    CopyRelativePath => "copy-relative-path", File, "Copy this file's path from the project root";
    NextBuffer => "next-buffer", File, "The buffer after this one";
    PrevBuffer => "prev-buffer", File, "The buffer before this one";
    Buffers => "buffers", File, "Pick from the open buffers";
    Quit => "quit", File, "Leave, asking about unsaved changes";
    QuitForce => "quit!", File, "Leave, changes and all";

    // ---- Moving ----
    MoveLeft => "left", Move, "One character left";
    MoveRight => "right", Move, "One character right";
    MoveUp => "up", Move, "One line up";
    MoveDown => "down", Move, "One line down";
    MoveWordLeft => "word-left", Move, "To the start of the word before";
    MoveWordRight => "word-right", Move, "To the end of the word after";
    MoveLineStart => "line-start", Move, "To the first thing on the line, then to column one";
    MoveLineEnd => "line-end", Move, "To the end of the line";
    MovePageUp => "page-up", Move, "A screenful up";
    MovePageDown => "page-down", Move, "A screenful down";
    MoveDocStart => "doc-start", Move, "To the top of the file";
    MoveDocEnd => "doc-end", Move, "To the bottom of the file";
    MoveParaUp => "para-up", Move, "To the blank line above";
    MoveParaDown => "para-down", Move, "To the blank line below";
    MatchBracket => "match-bracket", Move, "To the bracket matching this one";
    GotoLine => "goto-line", Move, "Jump to a line by number";
    JumpBack => "jump-back", Move, "Back to where you were before the last jump";
    JumpForward => "jump-forward", Move, "Forward again";
    ScrollUp => "scroll-up", Move, "Move the view up, leaving the cursor";
    ScrollDown => "scroll-down", Move, "Move the view down, leaving the cursor";
    CentreCursor => "centre-cursor", Move, "Put the cursor's line in the middle of the screen";

    // ---- Extending a selection ----
    ExtendLeft => "extend-left", Select, "Select one character left";
    ExtendRight => "extend-right", Select, "Select one character right";
    ExtendUp => "extend-up", Select, "Select one line up";
    ExtendDown => "extend-down", Select, "Select one line down";
    ExtendWordLeft => "extend-word-left", Select, "Select to the word before";
    ExtendWordRight => "extend-word-right", Select, "Select to the word after";
    ExtendLineStart => "extend-line-start", Select, "Select to the start of the line";
    ExtendLineEnd => "extend-line-end", Select, "Select to the end of the line";
    ExtendPageUp => "extend-page-up", Select, "Select a screenful up";
    ExtendPageDown => "extend-page-down", Select, "Select a screenful down";
    ExtendDocStart => "extend-doc-start", Select, "Select to the top of the file";
    ExtendDocEnd => "extend-doc-end", Select, "Select to the bottom of the file";
    SelectAll => "select-all", Select, "Select the whole file";
    SelectLine => "select-line", Select, "Select this line, then the one below";
    SelectWord => "select-word", Select, "Select the word under the cursor";
    ExpandSelection => "expand-selection", Select, "Grow the selection to the syntax around it";
    AddCursorAbove => "add-cursor-above", Select, "Another cursor on the line above";
    AddCursorBelow => "add-cursor-below", Select, "Another cursor on the line below";
    AddCursorNextMatch => "add-cursor-next-match", Select, "Another cursor at the next copy of this word";
    SelectAllMatches => "select-all-matches", Select, "A cursor at every copy of this word";
    CursorsToLineEnds => "cursors-to-line-ends", Select, "A cursor at the end of every selected line";
    CollapseCursors => "collapse-cursors", Select, "Back to one cursor";

    // ---- Changing text ----
    InsertNewline => "newline", Edit, "Break the line, keeping the indentation";
    DeleteBackward => "delete-backward", Edit, "Rub out the character before";
    DeleteForward => "delete-forward", Edit, "Rub out the character after";
    DeleteWordBackward => "delete-word-backward", Edit, "Rub out the word before";
    DeleteWordForward => "delete-word-forward", Edit, "Rub out the word after";
    DeleteToLineStart => "delete-to-line-start", Edit, "Rub out back to the start of the line";
    DeleteToLineEnd => "delete-to-line-end", Edit, "Rub out to the end of the line";
    DeleteLine => "delete-line", Edit, "Take out the whole line";
    DuplicateLine => "duplicate-line", Edit, "Another copy of the line below it";
    MoveLineUp => "move-line-up", Edit, "Swap this line with the one above";
    MoveLineDown => "move-line-down", Edit, "Swap this line with the one below";
    JoinLines => "join-lines", Edit, "Pull the next line onto this one";
    Indent => "indent", Edit, "Push the line right one level";
    Unindent => "unindent", Edit, "Pull the line left one level";
    ToggleComment => "toggle-comment", Edit, "Comment the selected lines out, or back in";
    Undo => "undo", Edit, "Put back what you just changed";
    Redo => "redo", Edit, "Do it again after all";
    Copy => "copy", Edit, "Copy the selection, or the line if nothing is selected";
    Cut => "cut", Edit, "Cut the selection, or the line if nothing is selected";
    Paste => "paste", Edit, "Put back what was copied";
    UpperCase => "upper-case", Edit, "Make the selection shout";
    LowerCase => "lower-case", Edit, "Make the selection quiet";

    // ---- Finding ----
    Find => "find", Search, "Search this file as you type";
    FindNext => "find-next", Search, "The next hit";
    FindPrev => "find-prev", Search, "The one before";
    FindWordUnderCursor => "find-word", Search, "Search for the word the cursor is on";
    Replace => "replace", Search, "Search and replace in this file";
    NextChange => "next-change", Search, "To the next line that differs from the last commit";
    PrevChange => "prev-change", Search, "To the change before";
    Grep => "grep", Search, "Search every file in the project";

    // ---- Language servers ----
    Completion => "completion", Code, "Suggest what comes next";
    GotoDefinition => "goto-definition", Code, "Where this is defined";
    GotoTypeDefinition => "goto-type-definition", Code, "Where its type is defined";
    GotoImplementation => "goto-implementation", Code, "Where it is implemented";
    References => "references", Code, "Everywhere this is used";
    Hover => "hover", Code, "What the language server knows about this";
    Rename => "rename", Code, "Rename this everywhere it appears";
    CodeAction => "code-action", Code, "What the language server offers to do about this";
    FixIt => "fix-it", Code, "Do the obvious thing about the problem here: add the import, fix the typo";
    Format => "format", Code, "Reformat the file";
    Symbols => "symbols", Code, "Pick from what this file defines";
    WorkspaceSymbols => "workspace-symbols", Code, "Pick from what the project defines";
    Diagnostics => "diagnostics", Code, "Pick from the problems found";
    NextDiagnostic => "next-diagnostic", Code, "To the next problem";
    PrevDiagnostic => "prev-diagnostic", Code, "To the problem before";
    SignatureHelp => "signature-help", Code, "What arguments this call takes";
    PythonEnvironment => "python-environment", Code, "Choose which Python this project uses";
    RestartServers => "restart-servers", Code, "Start the language servers again";
    ServerStatus => "server-status", Code, "What the language servers are doing";

    // ---- The view ----
    CommandPalette => "command-palette", View, "Everything textfold can do, by name";
    Split => "split", View, "Another pane onto the same file";
    ClosePane => "close-pane", View, "Close this pane";
    FocusNextPane => "focus-next-pane", View, "Into the next pane";
    FocusPrevPane => "focus-prev-pane", View, "Into the pane before";
    SwapSplitDirection => "swap-split-direction", View, "Side by side, or one above the other";
    DiffPanes => "diff-panes", View, "Compare the two panes, and scroll them together";
    ThemePicker => "theme", View, "Pick a set of colours";
    NextTheme => "next-theme", View, "The next set of colours along";
    PrevTheme => "prev-theme", View, "The set before";
    ToggleLineNumbers => "toggle-line-numbers", View, "Line numbers on or off";
    ToggleRelativeNumbers => "toggle-relative-numbers", View, "Count from the cursor instead of the top";
    ToggleWrap => "toggle-wrap", View, "Fold long lines, or let them run off the side";
    ToggleWhitespace => "toggle-whitespace", View, "Show spaces and tabs";
    ToggleMouse => "toggle-mouse", View, "Let the terminal have the mouse back";
    SetLanguage => "set-language", View, "Say what language this file is";
    Settings => "settings", View, "Change a setting, and keep it";

    // ---- Getting out of things ----
    ContextMenu => "context-menu", Edit, "What can be done where the cursor is";

    Escape => "escape", Help, "Close what is open, or drop back to one cursor";
    Help => "help", Help, "The keys, and what they do";
    About => "about", Help, "Which textfold this is";
}

impl Cmd {
    /// Whether this changes the text, and so needs a document that can be
    /// changed. Checked in one place so that a read-only file cannot be
    /// edited by a route somebody forgot about.
    pub fn writes(&self) -> bool {
        matches!(
            self,
            Cmd::InsertNewline
                | Cmd::DeleteBackward
                | Cmd::DeleteForward
                | Cmd::DeleteWordBackward
                | Cmd::DeleteWordForward
                | Cmd::DeleteToLineStart
                | Cmd::DeleteToLineEnd
                | Cmd::DeleteLine
                | Cmd::DuplicateLine
                | Cmd::MoveLineUp
                | Cmd::MoveLineDown
                | Cmd::JoinLines
                | Cmd::Indent
                | Cmd::Unindent
                | Cmd::ToggleComment
                | Cmd::Undo
                | Cmd::Redo
                | Cmd::Cut
                | Cmd::Paste
                | Cmd::UpperCase
                | Cmd::LowerCase
                | Cmd::Replace
                | Cmd::Rename
                | Cmd::Format
                | Cmd::CodeAction
                | Cmd::FixIt
        )
    }

    /// Whether doing this leaves the cursors where a following keystroke could
    /// join the same undo. Anything that moves rather than types closes the
    /// current undo step, so that undo goes back to a place you recognise.
    pub fn breaks_undo(&self) -> bool {
        !matches!(
            self,
            Cmd::InsertNewline | Cmd::DeleteBackward | Cmd::DeleteForward
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_command_has_its_own_name() {
        let mut seen = HashSet::new();
        for cmd in ALL {
            assert!(seen.insert(cmd.name()), "two commands called {}", cmd.name());
        }
    }

    #[test]
    fn a_name_finds_the_command_it_names() {
        for cmd in ALL {
            assert_eq!(Cmd::from_name(cmd.name()), Some(*cmd));
        }
        assert_eq!(Cmd::from_name("fly-to-the-moon"), None);
    }

    #[test]
    fn descriptions_say_something_the_name_does_not() {
        for cmd in ALL {
            let about = cmd.about();
            assert!(!about.is_empty(), "{} says nothing", cmd.name());
            assert!(
                about.chars().next().is_some_and(char::is_uppercase),
                "{} starts lowercase",
                cmd.name()
            );
        }
    }
}
