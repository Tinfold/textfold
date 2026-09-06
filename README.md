# textfold

A terminal text editor for the keyboard and the mouse.

It's one Rust binary with syntax colouring and language servers already inside
it. Open a Rust file in a Cargo project and rust-analyzer starts on its own,
with completions, diagnostics, go-to-definition, rename and code actions. No
plugin manager to set up first, no config file to write.

The keys are the ones everything else on your computer uses. Ctrl-S saves,
Ctrl-Z undoes, Ctrl-F finds, Ctrl-Q quits. There's no mode to be in and no mode
to get out of.

```sh
textfold src/main.rs
textfold src/main.rs:42:8    # paste what a compiler printed
textfold                     # empty buffer
```

## Install

```sh
cargo build --release
./target/release/textfold
```

Needs Rust 1.89 or newer. The tree-sitter grammars are C and get compiled in,
so the first build takes a couple of minutes. After that it's a single file you
can copy anywhere.

## Getting started

Open a file and type. When you want something you can't see:

| | |
|---|---|
| **F1** | every key, and what it does |
| **Ctrl-P** | open a file by fuzzy name |
| **Alt-X** | every command there is, with its key beside it |

The command palette is the one to learn. Everything textfold can do is in it,
and each row shows the key that also does it, so it teaches you the keyboard
while you use it.

The bottom line tells you where you are: the file, its problems, the language,
the position, the colours. Every one of those is also a button.

## What's in it

- **Language servers.** LSP over stdio, one process per server per project.
  Completions, diagnostics, hover, go to definition, find references, rename
  across files, code actions, signature help, formatting, call hierarchy,
  inlay hints and semantic colouring. Nothing blocks the editor while a server
  thinks.
- **Debugging.** DAP, so `gdb`, `lldb-dap` and `debugpy` all work. Breakpoints
  in the margin, a stack and a values panel, step in and over and out, and
  attach to something already running. C, C++ and Rust build first, then debug
  what came out.
- **Colouring.** Tree-sitter, updated incrementally as you type, worked out
  only for the lines on screen. 17 languages coloured out of the box.
- **Folding.** Alt-H folds whatever the cursor is inside, from the parse tree
  rather than from indentation. Alt-Shift-H turns a file into a list of what's
  in it.
- **Multiple cursors.** Ctrl-D adds one at the next copy of the word, Alt-click
  puts one anywhere, Alt-drag selects a rectangle. Every command works on all
  of them.
- **Panes.** Up to four, split either way, draggable dividers. Alt-C diffs two
  of them against each other and keeps the matching lines level.
- **Git.** Your branch and your changed lines in the gutter, step through
  hunks, revert one, stage one, blame a line, walk merge conflicts. It's also
  a decent `core.editor`.
- **The mouse works.** Click, drag, double and triple click, right-click menus,
  drag tabs to reorder them, click a fold mark to open it, drag a pane divider.
  Everything you can do with the keyboard you can do with the mouse.
- **Sessions.** Close with thirty tabs open, come back to thirty tabs, per
  project.
- **Themes.** 24 built in, `Alt-T` to try them on, drop a JSON file in
  `~/.config/textfold/themes/` for your own.

## Language servers

They ship as plugins, fetched the first time you install one:

```sh
textfold --list-plugins    # what's on, and what's on but has nothing to run
textfold --install ruff
```

Or run `install-plugin` from the palette, which lists what isn't working yet
and fetches it. There's a plugin each for rust-analyzer, pyright, ruff,
tsserver, gopls, clangd, jdtls, omnisharp, taplo, marksman, and the bash, yaml,
json, html, css and docker servers.

Everything lands in `~/.local/share/textfold/tools/`, never system-wide, and
last on the `PATH` so your own copies still win. Delete that directory to undo
every install.

If you'd rather install them yourself, do that. textfold finds them on your
`PATH` like anything else.

## Config

`~/.config/textfold/config.json`. All of it is optional.

```json
{
  "theme": "kanagawa",
  "tab_width": 4,
  "spaces": true,
  "format_on_save": true,
  "keys": { "save": ["ctrl-s", "f2"] }
}
```

Most settings are also in the palette under `settings`, and changing one there
writes it back for you. Only what you changed gets written.

## Plugins

A plugin is a JSON manifest. It can add a language, a grammar, a language
server, a debug adapter, a theme, keys, or a program to run on your buffer. If
it needs to remember things between keystrokes it can be a program of its own,
in any language, talking JSON on a pipe. There are working examples (cargo,
copilot, a file tree) in
[textfold-plugins](https://github.com/Tinfold/textfold-plugins).

Yours go in `~/.config/textfold/plugins/`. Nothing textfold ships takes a route
your own plugin can't.

## Docs

[docs/manual.md](docs/manual.md) is the full thing: every key, every setting,
every manifest field, and why most of it works the way it does.

## Contributing

```sh
cargo fmt --all
cargo build
cargo test      # a few hundred tests, none need a terminal or a network
cargo clippy --all-targets -- -D warnings
```

Those four are what CI runs, on Linux and macOS. Formatting is rustfmt's
default, so run `cargo fmt` before you commit and there's nothing to argue
about. `./scripts/install-hooks.sh` sets up a pre-commit hook that does it for
you.

## Licence

MIT.
