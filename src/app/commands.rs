//! Everything textfold can be told to do: the command table.
//!
//! One row per command, and the row is the only place that command is written
//! down: the name a settings file binds a key to, the group and the line the
//! palette shows, what it does to the text, and what it actually does. The key
//! bindings, the palette and the context menus all read this, so there is no
//! second list for somebody to forget.
//!
//! It lives here rather than in [`crate::cmd`] because a row *is* behaviour —
//! it names a method on `App`, and those are this module's to reach.

use super::*;

macro_rules! commands {
    ($($konst:ident => $name:literal, $group:ident, $behaviour:ident, $about:literal,
        $run:expr;)*) => {
        pub const BUILT_IN: &[Spec] = &[
            $(Spec {
                name: $name,
                group: Group::$group,
                behaviour: Behaviour::$behaviour,
                about: $about,
                run: $run,
            },)*
        ];

        /// A constant per command, so that a menu row or a default binding
        /// names one the way it always did. Worked out from the table at
        /// compile time: a constant naming a command that is not in the table
        /// does not build.
        ///
        /// Every command gets one whether or not anything in this build
        /// happens to name it — it is the handle on the row, not a convenience
        /// for whoever needed one first.
        #[allow(dead_code)]
        impl Cmd {
            $(pub const $konst: Cmd = Cmd::at(index_of($name));)*
        }
    };
}

/// Where a name sits in the table, worked out while compiling.
pub(crate) const fn index_of(name: &str) -> u16 {
    let mut at = 0;
    while at < BUILT_IN.len() {
        if same(BUILT_IN[at].name, name) {
            return at as u16;
        }
        at += 1;
    }
    panic!("a command constant naming a command that is not in the table");
}

pub(crate) const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut at = 0;
    while at < a.len() {
        if a[at] != b[at] {
            return false;
        }
        at += 1;
    }
    true
}

commands! {
    NEW => "new", File, Passive, "Start an empty buffer",
        |app| app.new_buffer();
    OPEN => "open", File, Passive, "Open a file by name, fuzzily",
        |app| app.open_files_picker();
    OPEN_PATH => "open-path", File, Passive, "Open a file by typing its path, exactly",
        |app| app.open_prompt(PromptKind::OpenPath);
    SAVE => "save", File, Passive, "Write this file to disk",
        |app| app.save(None);
    SAVE_AS => "save-as", File, Passive, "Write this file somewhere else",
        |app| app.open_prompt(PromptKind::SaveAs);
    SAVE_ALL => "save-all", File, Passive, "Write every changed file",
        |app| app.save_all();
    RELOAD => "reload", File, Passive, "Read this file again, throwing away changes",
        |app| app.reload();
    CLOSE => "close", File, Passive, "Close this buffer, asking about unsaved changes",
        |app| app.close(false);
    CLOSE_FORCE => "close!", File, Passive, "Close this buffer, changes and all",
        |app| app.close(true);
    CLOSE_OTHERS => "close-others", File, Passive, "Close every buffer but this one",
        |app| app.close_many(Keep::Others);
    CLOSE_SAVED => "close-saved", File, Passive, "Close every buffer with nothing unsaved in it",
        |app| app.close_many(Keep::Unsaved);
    CLOSE_ALL => "close-all", File, Passive, "Close every buffer",
        |app| app.close_many(Keep::Nothing);
    COPY_PATH => "copy-path", File, Passive, "Copy this file's full path",
        |app| app.copy_path(false);
    COPY_RELATIVE_PATH => "copy-relative-path", File, Passive, "Copy this file's path from the project root",
        |app| app.copy_path(true);
    NEXT_BUFFER => "next-buffer", File, Passive, "The buffer after this one",
        |app| app.step_buffer(1);
    PREV_BUFFER => "prev-buffer", File, Passive, "The buffer before this one",
        |app| app.step_buffer(-1);
    MOVE_TAB_LEFT => "move-tab-left", File, Passive, "Move this tab one place towards the front",
        |app| app.step_tab(-1);
    MOVE_TAB_RIGHT => "move-tab-right", File, Passive, "Move this tab one place towards the back",
        |app| app.step_tab(1);
    BUFFERS => "buffers", File, Passive, "Pick from the open buffers",
        |app| app.open_buffers_picker();
    QUIT => "quit", File, Passive, "Leave, asking about unsaved changes",
        |app| app.leave(false);
    QUIT_FORCE => "quit!", File, Passive, "Leave, changes and all",
        |app| app.leave(true);
    MOVE_LEFT => "left", Move, Passive, "One character left",
        |app| app.motion(Motion::Left, false);
    MOVE_RIGHT => "right", Move, Passive, "One character right",
        |app| app.motion(Motion::Right, false);
    MOVE_UP => "up", Move, Passive, "One line up",
        |app| app.motion(Motion::Up, false);
    MOVE_DOWN => "down", Move, Passive, "One line down",
        |app| app.motion(Motion::Down, false);
    MOVE_WORD_LEFT => "word-left", Move, Passive, "To the start of the word before",
        |app| app.motion(Motion::WordLeft, false);
    MOVE_WORD_RIGHT => "word-right", Move, Passive, "To the end of the word after",
        |app| app.motion(Motion::WordRight, false);
    MOVE_LINE_START => "line-start", Move, Passive, "To the first thing on the line, then to column one",
        |app| app.motion(Motion::LineStart, false);
    MOVE_LINE_END => "line-end", Move, Passive, "To the end of the line",
        |app| app.motion(Motion::LineEnd, false);
    MOVE_PAGE_UP => "page-up", Move, Passive, "A screenful up",
        |app| app.motion(Motion::PageUp, false);
    MOVE_PAGE_DOWN => "page-down", Move, Passive, "A screenful down",
        |app| app.motion(Motion::PageDown, false);
    MOVE_DOC_START => "doc-start", Move, Passive, "To the top of the file",
        |app| app.motion(Motion::DocStart, false);
    MOVE_DOC_END => "doc-end", Move, Passive, "To the bottom of the file",
        |app| app.motion(Motion::DocEnd, false);
    MOVE_PARA_UP => "para-up", Move, Passive, "To the blank line above",
        |app| app.motion(Motion::ParaUp, false);
    MOVE_PARA_DOWN => "para-down", Move, Passive, "To the blank line below",
        |app| app.motion(Motion::ParaDown, false);
    MATCH_BRACKET => "match-bracket", Move, Passive, "To the bracket matching this one",
        |app| app.go_to_matching_bracket();
    GOTO_LINE => "goto-line", Move, Passive, "Jump to a line by number",
        |app| app.open_prompt(PromptKind::GotoLine);
    RECORD_MACRO => "record-macro", Edit, Passive, "Start remembering what you do, or stop and keep it",
        |app| app.record_macro();
    PLAY_MACRO => "play-macro", Edit, Passive, "Do what was recorded, again",
        |app| app.play_macro();
    TOGGLE_BOOKMARK => "toggle-bookmark", Move, Passive, "Mark this line to come back to, or take the mark off",
        |app| app.toggle_bookmark();
    NEXT_BOOKMARK => "next-bookmark", Move, Passive, "To the next bookmark in this file",
        |app| app.bookmark_step(true);
    PREV_BOOKMARK => "prev-bookmark", Move, Passive, "To the bookmark before",
        |app| app.bookmark_step(false);
    BOOKMARKS => "bookmarks", Move, Passive, "Every bookmark in every open file, as a list",
        |app| app.open_bookmarks_picker();
    CLEAR_BOOKMARKS => "clear-bookmarks", Move, Passive, "Take every bookmark in this file away",
        |app| app.clear_bookmarks_here();
    JUMP_BACK => "jump-back", Move, Passive, "Back to where you were before the last jump",
        |app| app.jump(false);
    JUMP_FORWARD => "jump-forward", Move, Passive, "Forward again",
        |app| app.jump(true);
    SCROLL_UP => "scroll-up", Move, Passive, "Move the view up, leaving the cursor",
        |app| app.scroll(-3);
    SCROLL_DOWN => "scroll-down", Move, Passive, "Move the view down, leaving the cursor",
        |app| app.scroll(3);
    CENTRE_CURSOR => "centre-cursor", Move, Passive, "Put the cursor's line in the middle of the screen",
        |app| app.centre();
    EXTEND_LEFT => "extend-left", Select, Passive, "Select one character left",
        |app| app.motion(Motion::Left, true);
    EXTEND_RIGHT => "extend-right", Select, Passive, "Select one character right",
        |app| app.motion(Motion::Right, true);
    EXTEND_UP => "extend-up", Select, Passive, "Select one line up",
        |app| app.motion(Motion::Up, true);
    EXTEND_DOWN => "extend-down", Select, Passive, "Select one line down",
        |app| app.motion(Motion::Down, true);
    EXTEND_WORD_LEFT => "extend-word-left", Select, Passive, "Select to the word before",
        |app| app.motion(Motion::WordLeft, true);
    EXTEND_WORD_RIGHT => "extend-word-right", Select, Passive, "Select to the word after",
        |app| app.motion(Motion::WordRight, true);
    EXTEND_LINE_START => "extend-line-start", Select, Passive, "Select to the start of the line",
        |app| app.motion(Motion::LineStart, true);
    EXTEND_LINE_END => "extend-line-end", Select, Passive, "Select to the end of the line",
        |app| app.motion(Motion::LineEnd, true);
    EXTEND_PAGE_UP => "extend-page-up", Select, Passive, "Select a screenful up",
        |app| app.motion(Motion::PageUp, true);
    EXTEND_PAGE_DOWN => "extend-page-down", Select, Passive, "Select a screenful down",
        |app| app.motion(Motion::PageDown, true);
    EXTEND_DOC_START => "extend-doc-start", Select, Passive, "Select to the top of the file",
        |app| app.motion(Motion::DocStart, true);
    EXTEND_DOC_END => "extend-doc-end", Select, Passive, "Select to the bottom of the file",
        |app| app.motion(Motion::DocEnd, true);
    SELECT_ALL => "select-all", Select, Passive, "Select the whole file",
        |app| app.select_all();
    SELECT_LINE => "select-line", Select, Passive, "Select this line, then the one below",
        |app| app.select_line();
    SELECT_WORD => "select-word", Select, Passive, "Select the word under the cursor",
        |app| app.select_word();
    EXPAND_SELECTION => "expand-selection", Select, Passive, "Grow the selection to the syntax around it",
        |app| app.expand_selection();
    ADD_CURSOR_ABOVE => "add-cursor-above", Select, Passive, "Another cursor on the line above",
        |app| app.add_cursor_above();
    ADD_CURSOR_BELOW => "add-cursor-below", Select, Passive, "Another cursor on the line below",
        |app| app.add_cursor_below();
    ADD_CURSOR_NEXT_MATCH => "add-cursor-next-match", Select, Passive, "Another cursor at the next copy of this word",
        |app| app.add_cursor_at_next_match();
    SELECT_ALL_MATCHES => "select-all-matches", Select, Passive, "A cursor at every copy of this word",
        |app| app.select_every_match();
    CURSORS_TO_LINE_ENDS => "cursors-to-line-ends", Select, Passive, "A cursor at the end of every selected line",
        |app| app.cursors_to_line_ends();
    COLLAPSE_CURSORS => "collapse-cursors", Select, Passive, "Back to one cursor",
        |app| app.collapse_cursors();
    INSERT_NEWLINE => "newline", Edit, Types, "Break the line, keeping the indentation",
        |app| app.insert_newline();
    DELETE_BACKWARD => "delete-backward", Edit, Types, "Rub out the character before",
        |app| app.delete_backward();
    DELETE_FORWARD => "delete-forward", Edit, Types, "Rub out the character after",
        |app| app.delete_forward();
    DELETE_WORD_BACKWARD => "delete-word-backward", Edit, Edits, "Rub out the word before",
        |app| app.delete_word_backward();
    DELETE_WORD_FORWARD => "delete-word-forward", Edit, Edits, "Rub out the word after",
        |app| app.delete_word_forward();
    DELETE_TO_LINE_START => "delete-to-line-start", Edit, Edits, "Rub out back to the start of the line",
        |app| app.delete_to_line_start();
    DELETE_TO_LINE_END => "delete-to-line-end", Edit, Edits, "Rub out to the end of the line",
        |app| app.delete_to_line_end();
    DELETE_LINE => "delete-line", Edit, Edits, "Take out the whole line",
        |app| app.delete_line();
    DUPLICATE_LINE => "duplicate-line", Edit, Edits, "Another copy of the line below it",
        |app| app.duplicate_line();
    MOVE_LINE_UP => "move-line-up", Edit, Edits, "Swap this line with the one above",
        |app| app.move_line_up();
    MOVE_LINE_DOWN => "move-line-down", Edit, Edits, "Swap this line with the one below",
        |app| app.move_line_down();
    JOIN_LINES => "join-lines", Edit, Edits, "Pull the next line onto this one",
        |app| app.join_lines();
    INDENT => "indent", Edit, Edits, "Push the line right one level",
        |app| app.on_tab(false);
    ACCEPT_HINT => "accept-hint", Edit, Edits, "Take the suggestion a plugin is offering",
        |app| app.accept_hint();
    UNINDENT => "unindent", Edit, Edits, "Pull the line left one level",
        |app| app.on_tab(true);
    TOGGLE_COMMENT => "toggle-comment", Edit, Edits, "Comment the selected lines out, or back in",
        |app| app.toggle_comment();
    UNDO => "undo", Edit, Edits, "Put back what you just changed",
        |app| app.undo(true);
    REDO => "redo", Edit, Edits, "Do it again after all",
        |app| app.undo(false);
    COPY => "copy", Edit, Passive, "Copy the selection, or the line if nothing is selected",
        |app| app.copy(false);
    CUT => "cut", Edit, Edits, "Cut the selection, or the line if nothing is selected",
        |app| app.copy(true);
    PASTE => "paste", Edit, Edits, "Put back what was copied",
        |app| app.paste();
    UPPER_CASE => "upper-case", Edit, Edits, "Make the selection shout",
        |app| app.change_case(edit::Case::Upper);
    LOWER_CASE => "lower-case", Edit, Edits, "Make the selection quiet",
        |app| app.change_case(edit::Case::Lower);
    TITLE_CASE => "title-case", Edit, Edits, "Every word in the selection with a capital letter",
        |app| app.change_case(edit::Case::Title);
    SORT_LINES => "sort-lines", Edit, Edits, "Put the selected lines in order — the whole file, with nothing selected",
        |app| app.shuffle_lines(edit::Shuffle::Sort);
    SORT_LINES_BACKWARDS => "sort-lines-backwards", Edit, Edits, "The same, the other way up",
        |app| app.shuffle_lines(edit::Shuffle::SortBackwards);
    REVERSE_LINES => "reverse-lines", Edit, Edits, "Turn the selected lines back to front",
        |app| app.shuffle_lines(edit::Shuffle::Reverse);
    UNIQUE_LINES => "unique-lines", Edit, Edits, "Drop every line that is already there, keeping the first of each",
        |app| app.shuffle_lines(edit::Shuffle::Unique);
    FIND => "find", Search, Passive, "Search this file as you type",
        |app| app.open_prompt(PromptKind::Find);
    FIND_NEXT => "find-next", Search, Passive, "The next hit",
        |app| app.find_step(1);
    FIND_PREV => "find-prev", Search, Passive, "The one before",
        |app| app.find_step(-1);
    FIND_WORD_UNDER_CURSOR => "find-word", Search, Passive, "Search for the word the cursor is on",
        |app| app.find_word_under_cursor();
    REPLACE => "replace", Search, Edits, "Search and replace in this file",
        |app| app.open_prompt(PromptKind::ReplaceFind);
    REVERT_HUNK => "revert-hunk", Edit, Edits, "Put this stretch of the file back as it was committed",
        |app| app.revert_hunk();
    STAGE_HUNK => "stage-hunk", Edit, Passive, "Put this stretch of the file into git's index",
        |app| app.stage_hunk();
    BLAME_LINE => "blame-line", Search, Passive, "Who last touched this line, and when, and why",
        |app| app.blame_line();
    NEXT_CONFLICT => "next-conflict", Search, Passive, "To the next place git could not merge on its own",
        |app| app.conflict_step(true);
    PREV_CONFLICT => "prev-conflict", Search, Passive, "To the conflict before",
        |app| app.conflict_step(false);
    TAKE_OURS => "take-ours", Edit, Edits, "Settle this conflict by keeping your side of it",
        |app| app.take_side(true);
    TAKE_THEIRS => "take-theirs", Edit, Edits, "Settle this conflict by keeping their side of it",
        |app| app.take_side(false);
    NEXT_CHANGE => "next-change", Search, Passive, "To the next line that differs from the last commit",
        |app| app.change_step(true);
    PREV_CHANGE => "prev-change", Search, Passive, "To the change before",
        |app| app.change_step(false);
    REPLACE_IN_PROJECT => "replace-in-project", Search, Edits, "Replace something in every file in the project",
        |app| app.open_prompt(PromptKind::ProjectReplaceFind);
    GREP => "grep", Search, Passive, "Search every file in the project",
        |app| app.open_grep_picker();
    COMPLETION => "completion", Code, Passive, "Suggest what comes next",
        |app| app.ask_for_completions(None, true);
    GOTO_DEFINITION => "goto-definition", Code, Passive, "Where this is defined",
        |app| app.ask_goto(Goto::Definition);
    GOTO_TYPE_DEFINITION => "goto-type-definition", Code, Passive, "Where its type is defined",
        |app| app.ask_goto(Goto::Type);
    GOTO_IMPLEMENTATION => "goto-implementation", Code, Passive, "Where it is implemented",
        |app| app.ask_goto(Goto::Implementation);
    INCOMING_CALLS => "incoming-calls", Code, Passive, "What calls this",
        |app| app.ask_calls(true);
    OUTGOING_CALLS => "outgoing-calls", Code, Passive, "What this calls",
        |app| app.ask_calls(false);
    RUN_CODE_LENS => "run-code-lens", Code, Passive, "Do what the server is offering on this line",
        |app| app.run_code_lens();
    TOGGLE_INLAY_HINTS => "toggle-inlay-hints", View, Passive, "Show or hide the types the code does not say",
        |app| app.toggle_setting("inlay_hints");
    TOGGLE_CODE_LENSES => "toggle-code-lenses", View, Passive, "Show or hide the servers' notes about each line",
        |app| app.toggle_setting("code_lenses");
    REFERENCES => "references", Code, Passive, "Everywhere this is used",
        |app| app.ask_references();
    HOVER => "hover", Code, Passive, "What the language server knows about this",
        |app| app.ask_hover(app.view().cursor());
    RENAME => "rename", Code, Edits, "Rename this everywhere it appears",
        |app| app.start_rename();
    CODE_ACTION => "code-action", Code, Edits, "What the language server offers to do about this",
        |app| app.ask_code_actions();
    FIX_IT => "fix-it", Code, Edits, "Do the obvious thing about the problem here: add the import, fix the typo",
        |app| app.fix_it();
    FIX_ALL => "fix-all", Code, Edits, "Apply every fix the servers would make to this file on their own",
        |app| app.fix_all(&[SOURCE_FIX_ALL.to_string()]);
    ORGANIZE_IMPORTS => "organize-imports", Code, Edits, "Put this file's imports in order and drop the unused ones",
        |app| app.fix_all(&[SOURCE_ORGANIZE_IMPORTS.to_string()]);
    FORMAT => "format", Code, Edits, "Reformat the file",
        |app| app.format();
    FORMAT_AND_FIX => "format-and-fix", Code, Edits, "Reformat the file and apply the servers' own fixes",
        |app| app.format_and_fix();
    SYMBOLS => "symbols", Code, Passive, "Pick from what this file defines",
        |app| app.ask_symbols();
    WORKSPACE_SYMBOLS => "workspace-symbols", Code, Passive, "Pick from what the project defines",
        |app| app.open_workspace_symbols();
    DIAGNOSTICS => "diagnostics", Code, Passive, "Pick from the problems found",
        |app| app.open_diagnostics_picker();
    NEXT_DIAGNOSTIC => "next-diagnostic", Code, Passive, "To the next problem",
        |app| app.step_diagnostic(1);
    PREV_DIAGNOSTIC => "prev-diagnostic", Code, Passive, "To the problem before",
        |app| app.step_diagnostic(-1);
    SIGNATURE_HELP => "signature-help", Code, Passive, "What arguments this call takes",
        |app| app.ask_signature();
    PYTHON_ENVIRONMENT => "python-environment", Code, Passive, "Choose which Python this project uses",
        |app| app.open_environment_picker();
    RESTART_SERVERS => "restart-servers", Code, Passive, "Start the language servers again",
        |app| app.restart_servers();
    SERVER_STATUS => "server-status", Code, Passive, "What the language servers are doing",
        |app| app.show_server_status();

    // Debugging. Every one of these is Passive: a debugger looks at your
    // program, and none of them so much as touches the text.
    DEBUG => "debug", Code, Passive, "Run this file under a debugger, or carry on from where it stopped",
        |app| app.debug();
    DEBUG_STOP => "debug-stop", Code, Passive, "Stop the program and the debugger with it",
        |app| app.stop_debugging();
    DEBUG_PAUSE => "debug-pause", Code, Passive, "Stop the running program where it is",
        |app| app.debug_pause();
    DEBUG_STEP_OVER => "debug-step-over", Code, Passive, "The next line, calls and all",
        |app| app.debug_step(crate::dap::Step::Over);
    DEBUG_STEP_INTO => "debug-step-into", Code, Passive, "Into the call on this line",
        |app| app.debug_step(crate::dap::Step::Into);
    DEBUG_STEP_OUT => "debug-step-out", Code, Passive, "Out of this function, back to whoever called it",
        |app| app.debug_step(crate::dap::Step::Out);
    TOGGLE_BREAKPOINT => "toggle-breakpoint", Code, Passive, "Stop here when the program reaches this line",
        |app| app.toggle_breakpoint();
    CLEAR_BREAKPOINTS => "clear-breakpoints", Code, Passive, "Take every breakpoint in this file away",
        |app| app.clear_breakpoints_here();
    CLEAR_ALL_BREAKPOINTS => "clear-all-breakpoints", Code, Passive, "Take every breakpoint in every open file away",
        |app| app.clear_all_breakpoints();
    DEBUG_PANEL => "debug-panel", Code, Passive, "Show the stack, the values and what the program printed",
        |app| app.toggle_debug_panel();
    DEBUG_EVALUATE => "debug-evaluate", Code, Passive, "Work out what an expression comes to, where the program is stopped",
        |app| app.ask_debug_evaluate();
    DEBUG_OUTPUT => "debug-output", Code, Passive, "Everything the program being debugged has printed, in a buffer",
        |app| app.show_program_output();
    BUILD => "build", Code, Passive, "Turn this file into something that can be run",
        |app| app.build();
    BUILD_OUTPUT => "build-output", Code, Passive, "Everything the last build printed, in a buffer",
        |app| app.show_build_output();
    DEBUG_ATTACH => "debug-attach", Code, Passive, "Attach the debugger to a program that is already running",
        |app| app.open_attach_picker();
    COMMAND_PALETTE => "command-palette", View, Passive, "Everything textfold can do, by name",
        |app| app.open_commands_picker();
    SPLIT => "split", View, Passive, "Another pane onto the same file",
        |app| app.split();
    CLOSE_PANE => "close-pane", View, Passive, "Close this pane",
        |app| app.close_pane();
    FOCUS_NEXT_PANE => "focus-next-pane", View, Passive, "Into the next pane",
        |app| app.focus_pane(1);
    FOCUS_PREV_PANE => "focus-prev-pane", View, Passive, "Into the pane before",
        |app| app.focus_pane(-1);
    SWAP_SPLIT_DIRECTION => "swap-split-direction", View, Passive, "Side by side, or one above the other",
        |app| app.swap_split_direction();
    DIFF_PANES => "diff-panes", View, Passive, "Compare the two panes, and scroll them together",
        |app| app.toggle_diff();
    THEME_PICKER => "theme", View, Passive, "Pick a set of colours",
        |app| app.open_theme_picker();
    NEXT_THEME => "next-theme", View, Passive, "The next set of colours along",
        |app| app.step_theme(1);
    PREV_THEME => "prev-theme", View, Passive, "The set before",
        |app| app.step_theme(-1);
    TOGGLE_LINE_NUMBERS => "toggle-line-numbers", View, Passive, "Line numbers on or off",
        |app| app.toggle_setting("line_numbers");
    TOGGLE_RELATIVE_NUMBERS => "toggle-relative-numbers", View, Passive, "Count from the cursor instead of the top",
        |app| app.toggle_setting("relative_numbers");
    FOLD => "fold", View, Passive, "Fold away the block, function or string the cursor is in",
        |app| app.fold_here();
    UNFOLD => "unfold", View, Passive, "Bring back what is folded here",
        |app| { if !app.unfold_here() { app.say("nothing folded here"); } };
    TOGGLE_FOLD => "toggle-fold", View, Passive, "Fold what is here, or bring it back",
        |app| app.toggle_fold();
    FOLD_ALL => "fold-all", View, Passive, "Fold every top-level thing, leaving the file as a list of what is in it",
        |app| app.fold_all();
    UNFOLD_ALL => "unfold-all", View, Passive, "Bring the whole file back",
        |app| app.unfold_all();
    TOGGLE_WRAP => "toggle-wrap", View, Passive, "Fold long lines, or let them run off the side",
        |app| app.toggle_wrap();
    TOGGLE_WHITESPACE => "toggle-whitespace", View, Passive, "Show spaces and tabs",
        |app| app.toggle_setting("show_whitespace");
    TOGGLE_MOUSE => "toggle-mouse", View, Passive, "Let the terminal have the mouse back",
        |app| app.toggle_setting("mouse");
    SET_LANGUAGE => "set-language", View, Passive, "Say what language this file is",
        |app| app.open_language_picker();
    SETTINGS => "settings", View, Passive, "Change a setting, and keep it",
        |app| app.open_settings_picker();
    RESTORE_SESSION => "restore-session", View, Passive, "Open again the files that were open here last time",
        |app| app.bring_back_session();
    PLUGINS => "plugins", View, Passive, "Turn languages and language servers on or off",
        |app| app.open_plugins_picker();
    PLUGIN_SETTINGS => "plugin-settings", View, Passive, "Change what a plugin is told, and keep it across updates",
        |app| app.open_plugin_settings_picker();
    INSTALL_PLUGIN => "install-plugin", View, Passive, "Install a plugin, or what one needs to work",
        |app| app.open_install_picker();
    UNINSTALL_PLUGIN => "uninstall-plugin", View, Passive, "Take a plugin off this machine",
        |app| app.open_uninstall_picker();
    UPDATE_PLUGINS => "update-plugins", View, Passive, "Fetch a newer version of any plugin that has one",
        |app| app.open_update_picker();
    CONTEXT_MENU => "context-menu", Edit, Passive, "What can be done where the cursor is",
        |app| app.open_context_menu();
    ESCAPE => "escape", Help, Passive, "Close what is open, or drop back to one cursor",
        |app| app.escape();
    HELP => "help", Help, Passive, "The keys, and what they do",
        |app| app.overlay = Overlay::Help(0);
    ABOUT => "about", Help, Passive, "Which textfold this is",
        |app| app.say(format!(
                        "textfold {} — {} languages, {} themes",
                        env!("CARGO_PKG_VERSION"),
                        lang::names().len(),
                        app.themes.entries.len()
                    ));
}

