# CLAUDE.md

Working notes for textfold. The user-facing manual is `docs/manual.md`; it is
the long-form reference for every feature, setting and manifest field. This
file is the short version plus the things that are only obvious from the code.

## What this is

A terminal text editor in Rust. One binary. Tree-sitter colouring, LSP, DAP,
git, and a plugin system are all in it. No modes: Ctrl-S saves, Ctrl-Z undoes,
Ctrl-Q leaves. Keyboard and mouse are both first-class.

## Commands

```sh
cargo fmt --all          # rustfmt defaults, no rustfmt.toml
cargo build              # debug; deps build at opt-level 2 (see Cargo.toml)
cargo build --release
cargo test               # ~600 tests, no terminal, network or LSP needed
cargo clippy --all-targets -- -D warnings
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, build, test and
clippy on Linux and macOS. The format check runs first and is a hard failure,
so **run `cargo fmt --all` before committing**.

`./scripts/install-hooks.sh` points `core.hooksPath` at `.githooks/`, whose
pre-commit hook formats the staged Rust files and restages them.

Rust 1.89+, edition 2024. A cold build takes a couple of minutes because the
tree-sitter grammars are C and are compiled in.

Tests that want a repository make one in a temp directory and skip themselves
where there is no `git`.

## Layout

```
src/
  main.rs      clap CLI, startup
  text.rs      positions and selections
  doc.rs       the rope, undo, reading and writing files
  edit.rs      every editing operation
  view.rs      panes, scrolling, folding, the screen<->text map
  syntax.rs    tree-sitter
  queries/     corrections to shipped highlight queries (rust, csharp, java,
               markdown, markdown-inline, yaml)
  lang.rs      the language table
  rpc.rs       framed JSON on a pipe, shared by all three protocols
  lsp.rs       language servers
  dap.rs       debug adapters
  host.rs      plugins that are a running program
  proc.rs      spawning children into their own session and process group
  plugin.rs    manifests
  plugins/     the manifests that ship (20 languages), compiled in
  pack.rs      installing and uninstalling packages
  repo.rs      package repositories and the index
  tool.rs      programs run on a buffer
  git.rs       branch and diff
  diff.rs      the diff itself
  picker.rs    the fuzzy list
  menu.rs      context menus
  keys.rs      key parsing and binding
  cmd.rs       the command registry
  theme.rs     colours
  config.rs    settings
  session.rs   what was open where
  venv.rs      finding a project's Python environment
  jdk.rs       finding JDKs
  term.rs      clipboard and the rest of what the terminal is asked for
  log.rs       the log file
  app/         state and dispatch
    mod.rs       the state and the event loop
    commands.rs  the command table
    overlays.rs  lists, prompts, questions
    find.rs      searching, replacing, questions put to a server
    answers.rs   what a server sends back
    mouse.rs · debug.rs · files.rs · panes.rs · settings.rs · tools.rs · typing.rs
    tests/       buffers, debug, editing, find, mouse, plugins, servers
  ui/          drawing: mod, pane, bars, floating, tests
themes/        the 24 themes that ship, compiled in
docs/manual.md the user manual
```

`app` is a directory rather than a file so that what was private to `app` when
it was one file stays private to `app`. A child that reaches into another says
so.

## The invariants

These are the load-bearing rules. Breaking one does not fail loudly, it
produces a class of bug that takes an afternoon to find.

**One kind of number.** Every position is a character index into the rope.
Never a byte, never a column. Bytes are what tree-sitter and LSP want; columns
are what the screen wants; neither survives an edit. Conversions live at the
edges.

**One place the text changes.** Every edit goes through one function that
returns the change in every form anything downstream needs: byte offsets and
rows for tree-sitter, UTF-16 columns for LSP, character indices for cursors.
Nothing hears about an edit separately, so nothing can fall out of step.

**Undo is an edit.** The inverse of a change is an ordinary transaction. Quick
typing merges into one action; a paste, a format or a rename stands alone.

**Everything goes through the same door.** Re-reading a file from disk is an
ordinary edit, not a fresh `Document`. So is a plugin's edit, a formatter's
reply, a rename across nine files. A plugin may do nothing a keystroke cannot.

**Cursors belong to the pane, not the file.** The same file in two panes is two
sets of cursors, and an edit in one is told to both. A pane remembers where it
was in every file it has shown.

**A folded line is worth no rows.** All screen arithmetic counts rows, not
lines, because a wrapped line has always been several rows. Nothing about
scrolling, cursor movement, clicking or drawing needs to know folding exists.

**A note drawn into a line is counted in the width of it.** Inlay hints are the
same problem as a tab and are solved in the same place: the one piece of code
that maps between screen and text. Draw without counting and every click on
that line lands in the wrong place.

**Nothing waits on anything.** Keystrokes, mouse events, server messages,
plugin replies and project walks all arrive on one channel in one loop. There
are no locks near the text. A wedged server is a queue that stops filling, not
an editor that stops drawing.

**Bounded by what is on screen.** Colours are worked out for visible lines.
Scrolling to a cursor costs the height of the pane, not the size of the file.

**Nothing textfold starts can take the terminal.** Every child gets its own
session, not merely a process group (`proc.rs`). The group is so stopping one
stops what it started; the session is because `debugpy`'s launcher calls
`tcsetpgrp` and would otherwise take the keyboard away from the editor. On
Linux each child also carries `PDEATHSIG`.

**A command is a number.** `Cmd` is an index into a registry, so a key binding,
a palette row, a menu row and a status-bar button hold the same thing whether
the command is built in or from a plugin. The built-ins are one table in
`app/commands.rs`: name, group, palette line, whether it changes the text, and
what it does. A row that does not say what it does will not compile.

**A menu is a second way to reach the keys, not a second implementation.**
Every context-menu row is a `Cmd` the editor already has.

**A buffer is not always exactly its file.** That is decided at read time and
nowhere else. A file that did not survive being read as text is marked and the
buffer is read-only, so the one door writing goes through can refuse it.

**Data, not code.** Languages, grammars, servers, colours and tools all arrive
as plugin manifests read at startup. What ships is in the same shape as what a
user writes, loaded through the same code. Switching one off rebuilds the
languages, commands, keys and colours in place rather than asking for a
restart, and ids survive the rebuild.

**Installing is data too.** A plugin gives a table of steps, not a script, so
what it will do to the machine can be read before it runs. Success is decided
by looking for the programs afterwards, never by trusting exit codes.

## Protocols

`rpc.rs` is written once and used three times: LSP, DAP and plugin hosts. Each
peer is a child process, a thread that frames JSON off its output, and a note
of what each outstanding request meant. DAP is a dialect chosen when the peer
starts, not a second copy of the framing.

A peer is a pair of streams plus, where there is one, a child process to stop.
Java's debug adapter lives inside jdtls and is reached over a port it hands
back, so the layer above does not know which it got.

Things that bite:

- Each server is told about an edit in the form it asked for at startup: full
  document or ranges. Handing a full-document server a range replaces its copy
  of the file with the characters just typed. The symptom is hover and
  completion working until the first keystroke. taplo asks for full documents.
- Code-action replies are positions in the file as it was when asked. Apply one
  at a time, re-asking after each, or the second set of edits lands in text the
  first has already moved.
- An edit from a plugin carries the buffer version it was worked out against
  and is refused if the buffer has moved on.
- A debug session has a name because a dying adapter's last message arrives
  after the next session has started on the same channel.

## Plugin manifests

Full field reference is in `docs/manual.md`. The shape:

| key | |
|---|---|
| `languages` | extensions, comment syntax, grammar, `servers`, `debuggers` |
| `tools` | programs run on the buffer: `output` of `replace`/`problems`/`show`/`ignore` |
| `host` | a program that stays running, talking framed JSON on stdio |
| `commands`, `panels` | what that program answers to, and buffers it fills |
| `themes`, `keys` | colours, and suggested bindings |
| `needs`, `install`, `uninstall`, `see` | what it needs and how to get it |
| `version` | what an update is decided by |

A language named by more than one plugin merges field by field. Servers are
added to rather than written over, except by same-name replacement. Plugin keys
are bound only if nothing already wants them.

User settings live *over* a manifest in
`~/.config/textfold/plugin-settings/<id>.json`, never inside the plugin
directory, which is replaced whole on update. Objects merge key by key; lists
replace.

Placeholders (`${root}`, `${file}`, `${venv}`, `${python}`, `${crate}`,
`${plugin}`, `${java_home}`, `${pid}`, `${port:5005}`, `${env:NAME}` and the
rest) are resolved in server `args`/`env`/`settings`/`init_options`, debug
adapter `command`/`args`/`env`/`launch`, and tool `args`. A value naming an
environment the project has not got is dropped whole rather than half-filled.

## Where things go on disk

```
~/.config/textfold/config.json             settings
~/.config/textfold/languages.json          legacy language overrides, read last
~/.config/textfold/themes/                 user themes, replace by name
~/.config/textfold/plugins/<id>/           installed plugins, plus a receipt
~/.config/textfold/plugin-settings/<id>.json
~/.config/textfold/packages/               local packages
~/.local/share/textfold/tools/bin          programs textfold fetched
$XDG_STATE_HOME/textfold/sessions.json     what was open, per project
```

Nothing is installed system-wide except `brew` and `rustup component add`
steps, which say `"system": true` and are announced before they run.
`tools/bin` goes **last** on textfold's `PATH` so a user's own copy wins.

## CLI

```
textfold [FILE[:LINE[:COLUMN]]]...
  --theme NAME  --line N  --no-mouse  --no-session
  --list-themes  --list-languages  --list-plugins  --list-packages
  --install ID-OR-PATH  --uninstall ID  --refresh  --update [ID]
  --log-path
```

## Prose style

The manual, the CLI help and the comments share a voice. If you write any of
them, match it:

- British spelling: `colour`, `behaviour`, `recognise`.
- Say what something does and why the alternative was rejected. Most comments
  in this codebase exist to record a decision, not to describe the code.
- Concrete over abstract. Name the real failure that motivated the rule.
- No marketing register, no bullet lists of adjectives.
- Headings are plain sentences: "Files that change underneath you".
