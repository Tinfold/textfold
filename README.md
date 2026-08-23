# textfold

A terminal text editor for the keyboard **and** the mouse.

One Rust binary. Syntax colouring and language servers are already in it —
open a Rust file in a Cargo project and rust-analyzer starts on its own, with
completions, diagnostics, go-to-definition, rename and code actions. Nothing to
install first, no plugin manager, no configuration required to get a working
editor.

The keys are the ones every other program on your computer already uses.
Ctrl-S saves. Ctrl-Z undoes. Ctrl-F finds. Ctrl-Q leaves. There is no mode to
be in and no mode to get out of.

```
textfold src/main.rs
textfold src/main.rs:42:8        # what a compiler prints, pasted straight in
textfold                         # an empty buffer
```

---

## Contents

- [Building it](#building-it)
- [The first five minutes](#the-first-five-minutes)
- [Keys](#keys)
- [The mouse](#the-mouse)
- [One box, several lists](#one-box-several-lists)
- [Several cursors](#several-cursors)
- [Language servers](#language-servers)
- [Settings](#settings)
- [Colours](#colours)
- [Teaching it a language](#teaching-it-a-language)
- [When something is wrong](#when-something-is-wrong)
- [How it is put together](#how-it-is-put-together)

---

## Building it

```sh
cargo build --release
./target/release/textfold
```

Rust 1.89 or newer. The grammars are C and are compiled in, so the first build
takes a couple of minutes; after that it is a single file you can copy
anywhere.

There is nothing else to install. Language servers are the one exception, and
only for the languages you want intelligence in — see
[Language servers](#language-servers).

---

## The first five minutes

Open a file and type. That is the whole of it.

When you want something you cannot see:

| | |
|---|---|
| **F1** | every key, and what it does |
| **Ctrl-P** | open a file, by fuzzy name |
| **Alt-X** | every command there is, by name, with its key beside it |

The command palette is the important one. Everything textfold can do is in
it, searchable, and each row shows the key that also does it — so the palette
teaches you the keyboard while you use it. If you cannot remember a key, you
never need to.

The bottom line tells you where you are: the file, the problems in it, the
language, the position, the colours. Every one of those is also a button.

---

## Keys

Nothing in the default scheme needs a terminal that can tell Ctrl-Shift-P from
Ctrl-P. Where a binding like that is offered it is a second way to reach
something already reachable, so a plain `xterm` over `ssh` loses nothing. The
same goes for the keys a desktop is known to take for itself: they are offered,
never the only way in.

### Files

| Key | |
|---|---|
| Ctrl-S | save |
| Alt-S | save as |
| Ctrl-P / Ctrl-O | open a file |
| Alt-E / F4 | open a file by typing its path |
| Ctrl-N | new buffer |
| Ctrl-W | close this buffer |
| Ctrl-B | pick from the open buffers |
| Alt-, / Alt-. | previous / next buffer |
| Ctrl-Q | leave |

### Moving

| Key | |
|---|---|
| arrows | move |
| Ctrl-← / Ctrl-→ | by word |
| Home / End | start of the line's text, then column one / end of line |
| Ctrl-Home / Ctrl-End | top / bottom of the file |
| Ctrl-↑ / Ctrl-↓ | previous / next blank line |
| PgUp / PgDn | a screenful |
| Ctrl-G | go to a line by number |
| Alt-B | to the matching bracket |
| Alt-[ / Alt-] | back / forward through where you have been |
| Alt-M | put the cursor's line in the middle |

### Selecting

Shift with any movement extends the selection, as everywhere else.

| Key | |
|---|---|
| Ctrl-A | everything |
| Ctrl-L | this line, then the one below, then the one below that |
| Alt-= | grow the selection to the syntax around it |
| Ctrl-D | add a cursor at the next copy of this word |
| Ctrl-Shift-L | a cursor at every copy of this word |
| Alt-Shift-K / J | add a cursor on the line above / below |
| Ctrl-Alt-↑ / ↓ | the same, where your desktop has not taken them |
| Alt-Shift-I | a cursor at the end of every selected line |
| Esc | back to one cursor |

### Editing

| Key | |
|---|---|
| Tab / Shift-Tab | indent / unindent (whole lines, when something is selected) |
| Ctrl-/ | comment out, or uncomment |
| Alt-↑ / Alt-↓ | move the line up / down |
| Alt-Shift-↑ / ↓ | duplicate the line |
| Ctrl-Shift-K | delete the line |
| Alt-J | join the next line onto this one |
| Ctrl-Z / Ctrl-Y | undo / redo |
| Ctrl-C / Ctrl-X / Ctrl-V | copy / cut / paste (the line, if nothing is selected) |

### Finding

| Key | |
|---|---|
| Ctrl-F | find, as you type |
| F3 / Shift-F3 | next / previous |
| Alt-F | find the word under the cursor |
| Ctrl-H | find and replace |
| Ctrl-Shift-F | search every file in the project |

A lower-case search ignores case; a search with a capital in it means the
capital. Replacing with something selected replaces only inside the selection.

### Code

| Key | |
|---|---|
| Ctrl-Space | suggest |
| F12 | go to the definition |
| Shift-F12 | everywhere this is used |
| Alt-K | what the language server knows about this |
| F2 | rename, everywhere |
| Alt-Enter | what can be done here (quick fixes, imports, refactorings) |
| Alt-Shift-F | reformat the file |
| F8 / Shift-F8 | next / previous problem |
| Alt-D | all the problems, as a list |
| Alt-O | what this file defines |
| Alt-P | what arguments this call takes |

### Panes and the view

| Key | |
|---|---|
| Alt-V | another pane onto the same file |
| Alt-W | into the next pane |
| Alt-Q | close this pane |
| Alt-\ | side by side, or one above the other |
| Alt-T | pick colours |
| Alt-N | line numbers on and off |
| Alt-Z | fold long lines, or let them run off the side |

Up to four panes. Each pane has its own cursor, its own scroll position and its
own idea of whether lines fold — the same file open twice is two views of one
document, and typing in either moves the other's cursor along with the text it
was pointing at.

### Changing them

In `~/.config/textfold/config.json`, by command name:

```json
{
  "keys": {
    "save": ["ctrl-s", "f2"],
    "toggle-comment": ["ctrl-7"],
    "quit": []
  }
}
```

Naming a command replaces every key it had, so an empty list unbinds it.
Commands you do not mention keep what they came with. The names are the ones
in the command palette. `f1` shows what you actually have, not what textfold
shipped with.

---

## The mouse

Everything reachable by keyboard is reachable by mouse. Half the people who
open an editor reach for the mouse first, and there is no good reason to make
them wrong.

| | |
|---|---|
| click | put the cursor there |
| drag | select |
| double click | select the word — keep dragging to take more words |
| triple click | select the line — keep dragging to take more lines |
| click a line number | select that line |
| Shift-click | extend the selection |
| Alt-click | another cursor there |
| Ctrl-click | go to the definition |
| right click | what can be done here |
| middle click | paste |
| wheel | scroll the pane the pointer is over |
| hover | what the language server knows about the word under the pointer |
| click a tab | switch to it — the × closes it |
| drag the scroll bar | move through the file |
| click the status bar | the language, the position, the problems and the colours are buttons |

While textfold has the mouse, your terminal's own click-to-select does not
work — the clicks are coming here instead. **`toggle-mouse` hands it back.**
Run it from the palette (or bind a key) and you can select and copy the way you
normally would; run it again to take it back. `textfold --no-mouse` starts that
way, and `"mouse": false` in the settings makes it permanent.

Copying inside textfold puts the text on your system clipboard even over
`ssh`, by asking the terminal to do it. If your terminal does not support that,
textfold's own clipboard still works between its own buffers.

---

## One box, several lists

There is one fuzzy-finding box, and it is used for everything: files,
commands, symbols, problems, colours, buffers, what a language server offers to
do. Learning it once teaches you all of them.

From the file picker, the first character says which list you actually wanted:

| | |
|---|---|
| *(nothing)* | files in the project, honouring `.gitignore` |
| `>` | commands |
| `@` | what this file defines |
| `#` | what the project defines |
| `:` | go to a line |

Matching is fuzzy and the letters that matched are lit up, so you can see why
a row is on the list. `mrs` finds `src/main.rs`.

In any list: arrows or Ctrl-N/Ctrl-P move, Enter takes, Esc closes, Ctrl-W
rubs out a word, Ctrl-U clears. Clicking a row takes it; clicking outside
closes the list. In the colours list, moving through it tries each one on, and
Esc puts back the one you had.

Typing a name that matches nothing in the file picker and pressing Enter makes
that file.

The list is walked afresh every time you open it. A file written since textfold
started — by a build, by a checkout, by whoever you are working with — is a
file you can open, and what was found last time is shown straight away so the
box is never empty while it looks.

### Opening a path you already know

**Alt-E** (or **F4**) is the other door: a box for one path, taken exactly as
typed rather than fuzzily. `~` and a path relative to the project both work.

It is the one key that opens over anything else on the screen — a list, a
search box, a question. That is for the benefit of programs driving the
terminal rather than people: sshman sends
a file to the editor pane by typing keys at it, and it cannot see what is on
the screen when it does. sshman knows these keys, so a file picked over there
opens in the textfold running here.

---

## Several cursors

Every command works on all the cursors at once, because there is no such thing
as one cursor — a plain cursor is a set of one, and the code that handles a set
of one is the code that handles a set of forty. There is no second path through
typing a character that could behave differently.

The usual way in is **Ctrl-D**: it selects the word under the cursor, and each
press after that adds a cursor at the next copy of it. **Ctrl-Shift-L** takes
all of them at once. **Alt-Shift-K/J** adds a cursor straight up or down, and
**Alt-click** puts one wherever you like.

Ctrl-Alt-↑/↓ does the same, where it reaches textfold at all. GNOME and KDE
both bind it to switching workspace, and a desktop gets a keystroke before the
terminal under it does — which is why the letters are the ones written down
here. `gsettings set org.gnome.desktop.wm.keybindings switch-to-workspace-up
"[]"` (and `-down`) takes it back if you would rather have the arrows.

**Alt-Shift-I** turns "these twenty lines are selected" into "there is a cursor
at the end of all twenty", which is how you add a comma to twenty lines.

Undo puts back everything the whole set did, as one action. Esc goes back to
one cursor.

---

## Language servers

textfold speaks LSP over stdio, one process per server per project root, shared
by every file that belongs to it — opening forty Rust files starts one
rust-analyzer, not forty.

Nothing about it blocks the editor. Each server gets a thread that does nothing
but read its output and post it to the same channel the keyboard posts to. A
server that is slow, wedged, or busy indexing half a million lines cannot make
the cursor stutter, because the cursor is not waiting on it. While one is busy,
the top right says what it is doing.

What is wired up: completion (as you type, and on the characters the server
says should trigger it), diagnostics (underlined in the text, marked in the
margin, and the one under the cursor spelled out in the status bar), hover,
go to definition / type definition / implementation, find all uses, document
and workspace symbols, rename across files, code actions, signature help, and
formatting.

Documentation from a server is markdown, and what a docstring is mostly made of
is an example. The fences come off, and the code inside them is coloured by the
same parser that colours the file — a fence saying `rust`, `py` or `sh` is
taken at its word, and one saying nothing is read as the language you are
looking at. A language with no grammar here is left in one colour rather than
guessed at.

A rename or a quick fix that touches nine files opens all nine as tabs and
leaves them modified rather than writing them behind your back. Actions that
would create, move or delete files are refused; an editor that deletes a file
because a code action said so is an editor nobody trusts twice.

### What it will start, if you have it

| Language | Server |
|---|---|
| Rust | `rust-analyzer` (with clippy, all features, inlay hints) |
| Python | `pyright-langserver`, and `ruff server` beside it |
| TypeScript / JavaScript / TSX | `typescript-language-server` |
| Go | `gopls` |
| C / C++ | `clangd` |
| Bash | `bash-language-server` |
| JSON | `vscode-json-language-server` |
| TOML | `taplo` |
| YAML | `yaml-language-server` |
| Markdown | `marksman` |
| HTML / CSS | `vscode-html-language-server`, `vscode-css-language-server` |
| Dockerfile | `docker-langserver` |

Install the ones you want the way you normally would — for Rust that is
`rustup component add rust-analyzer`. A server that is not installed is
mentioned once and then left alone; the editor works exactly as well without
it, minus the intelligence.

Run `server-status` from the palette to see what is running and what it is
doing, and `restart-servers` after installing one.

### Colouring

Colouring is tree-sitter, with the grammar kept in step with your text
incrementally: every edit is handed to the parser first, so reparsing after a
keystroke re-reads the few nodes that changed. Colours are worked out for the
part of the file on screen and no further.

Grammars built in: Rust, Python, JavaScript, TypeScript, TSX, Go, C (which C++
borrows), Bash, JSON, TOML, YAML, Markdown, HTML, CSS. Rust's shipped highlight
query has a typo in it upstream that stops every SCREAMING_CASE constant from
being coloured as one; textfold reads its own correction on top of it.

A file that would take longer than a moment to parse — a minified bundle, a
megabyte of something the grammar cannot make sense of — is opened without
colours rather than kept waiting for, and the status bar says so.

---

## Settings

`~/.config/textfold/config.json`. Everything is optional; a file with nothing
in it means every default. Settings changed from inside the editor are written
back, and **only the ones you changed** — the file says what you decided rather
than repeating forty things you did not.

```json
{
  "theme": "kanagawa",
  "tab_width": 4,
  "spaces": true,
  "line_numbers": "absolute",
  "wrap": false,
  "scrolloff": 3,
  "rulers": [80, 100],
  "show_whitespace": false,
  "auto_completion": true,
  "auto_pairs": true,
  "format_on_save": false,
  "trim_trailing_whitespace": false,
  "final_newline": true,
  "mouse": true,
  "background": true,
  "enhanced_keys": true
}
```

| | |
|---|---|
| `theme` | which colours, by name |
| `tab_width` | how wide a tab is drawn |
| `spaces` | whether Tab puts in spaces — **a file that already uses tabs wins**, because a file is a fact and a setting is a wish |
| `line_numbers` | `absolute`, `relative`, `both`, or `off` |
| `wrap` | whether long lines fold |
| `scrolloff` | rows kept between the cursor and the edge |
| `rulers` | faint vertical lines at these columns |
| `show_whitespace` | middle dots for spaces, arrows for tabs |
| `auto_completion` | suggest as you type, rather than only when asked |
| `auto_pairs` | close brackets and quotes |
| `format_on_save` | run the language server's formatter first |
| `trim_trailing_whitespace` | drop trailing spaces on save |
| `final_newline` | give a file one if it has none |
| `mouse` | whether textfold captures the mouse at all |
| `background` | paint the theme's background, or leave the terminal's own |
| `enhanced_keys` | ask for the extended keyboard protocol |
| `keys` | see [Keys](#keys) |

Most of these are also in the palette under `settings`, where changing one
writes it back for you.

---

## Colours

A theme is a small JSON file naming twelve roles. The eighteen textfold ships
are built into the binary; any file dropped in `~/.config/textfold/themes/` is
loaded beside them, and one taking a name textfold already uses replaces it —
which is how you rewrite one of ours without forking anything.

```json
{
  "name": "mine",
  "about": "Where these colours came from.",

  "accent": "#7aa2f7",
  "dim":    "#565f89",
  "text":   "#c0caf5",
  "muted":  "#a9b1d6",
  "good":   "#9ece6a",
  "warn":   "#e0af68",
  "bad":    "#f7768e",
  "dir":    "#7dcfff",
  "link":   "#bb9af7",
  "exec":   "#9ece6a",
  "info":   "#2ac3de",
  "bg":     "#1a1b26",
  "on_accent": "#1a1b26"
}
```

That is a whole theme. Nothing about code is mentioned, and code is coloured
anyway: every kind of code has a meaning one of the twelve already carries.
Strings are the colour of things that worked. Comments are the colour of things
deliberately in the background. Keywords are the colour reserved for the
notable. Types are the colour of information. It is not a fudge, and it works
across every theme here.

**These are the same twelve roles, under the same names, that sshman uses — so a
theme file written for one drops into the other unchanged.**

If you want to be precise about code, say only the parts you disagree with:

```json
{
  "name": "mine",
  "base": "tokyonight",
  "syntax": {
    "comment": "#4a4a5e",
    "keyword": "#ff7a93"
  }
}
```

The code roles are `keyword`, `function`, `type`, `constructor`, `string`,
`escape`, `number`, `boolean`, `comment`, `constant`, `variable`, `parameter`,
`property`, `operator`, `punctuation`, `attribute`, `namespace`, `tag`,
`label`, `error`. There are four more for the pane itself — `selection`,
`cursorline`, `gutter`, `gutter_active` — all of which are worked out from the
twelve if you leave them out.

Colours are written as `#7aa2f7`, `#f0c`, a number for a slot in the
256-colour cube, or a name (`cyan`, `bright-red`, `default` for the terminal's
own). A theme naming no `bg` leaves your terminal's background showing through,
which is what `terminal` is.

`textfold --list-themes` lists them. `Alt-T` tries them on.

Shipped: `terminal`, `catppuccin`, `dracula`, `nord`, `tokyonight`, `gruvbox`,
`everforest`, `solarized`, `onedark`, `monokai`, `kanagawa`, `rosepine`,
`mariana`, `afterglow`, `darcula`, `ayu`, `solarized-light`, `latte`.

---

## Teaching it a language

A language is a table of facts, not code. The ones textfold ships live in
`src/languages.json`; a file of the same name in `~/.config/textfold/` is read
**on top of** it, and a language named there merges into the one here field by
field. So swapping rust-analyzer for something else is three lines and does not
mean restating the grammar and the comment syntax:

```json
{ "languages": {
  "rust": { "servers": [{ "command": "ra-multiplex" }] }
} }
```

Turning a server's settings up:

```json
{ "languages": {
  "rust": { "servers": [{
    "command": "rust-analyzer",
    "roots": ["Cargo.toml", "rust-project.json", ".git"],
    "settings": { "rust-analyzer": {
      "check": { "command": "clippy" },
      "cargo": { "allTargets": false }
    } }
  }] }
} }
```

A language textfold has never heard of is a language named there that is not
here. Colours come from any tree-sitter grammar built as a shared library —
`tree-sitter build` produces one — opened at the moment a file of that language
is first shown:

```json
{ "languages": {
  "zig": {
    "extensions": ["zig", "zon"],
    "line_comment": "//",
    "servers": [{ "command": "zls", "roots": ["build.zig"] }],
    "grammar": {
      "library":    "~/.config/textfold/grammars/zig.so",
      "highlights": "~/.config/textfold/grammars/zig-highlights.scm"
    }
  }
} }
```

Per language: `extensions`, `filenames` (for the many files with no extension),
`shebangs`, `line_comment`, `block_comment`, `brackets`, `lsp_id`, `servers`,
`grammar`. Per server: `command`, `args`, `roots`, `settings`,
`init_options`, `env`.

`roots` matters more than it looks. It is the marker files that say where a
project starts; the nearest ancestor holding one is the directory the server is
told about. A server given the wrong root indexes either far too much or
nothing at all.

`textfold --list-languages` shows what is in force.

---

## When something is wrong

**A key does nothing.** `F1` shows what you actually have bound. If a key you
set is missing, textfold says so once at startup — a key that silently does
nothing is a bad afternoon.

**No completions.** `server-status` in the palette says whether a server is
running and what it is doing. rust-analyzer is often still indexing; the top
right says so while it is. If it is not running at all, it is probably not
installed.

**A server said something and I missed it.** Servers' complaints go to a file,
not the screen. `textfold --log-path` says where.

**No colours.** The status bar says why when there is a reason worth giving.
Otherwise the language shown beside it is probably not what you thought — the
palette's `set-language` fixes that for the file in front of you, and
`languages.json` fixes it for good.

**I want my terminal's mouse back.** `toggle-mouse`, or `--no-mouse`, or
`"mouse": false`.

**A theme of mine is missing.** A file with a typo in it is complained about
once at startup, with the reason.

---

## How it is put together

Some of this is worth knowing if you are going to read the code.

**One kind of number.** Every position is a character index into the rope,
never a byte and never a column. Bytes are what tree-sitter and language
servers want; columns are what the screen wants; neither survives an edit, and
both make a `é` into a puzzle. The conversions live at the edges.

**One place the text changes.** Every edit goes through one function, which
returns the change in every form anything downstream needs it in — byte offsets
and rows for tree-sitter, UTF-16 columns for LSP, character indices for the
cursors. Nothing can quietly fall out of step with the text, because nothing
gets the chance to hear about an edit separately.

**Undo is an edit.** The inverse of a change is an ordinary transaction, so
undo is not a special path through the code and cannot drift from what it is
undoing. Quick typing merges into one action; a paste, a format or a rename
stands alone.

**Cursors belong to the pane, not the file.** The same file open in two panes
is two sets of cursors, and an edit made in one is told to both.

**Nothing waits on anything.** Keystrokes, mouse events, language server
messages and the results of walking the project all arrive on one channel, in
one loop. There are no locks anywhere near the text.

**Bounded by what is on screen.** Colours are worked out for the visible lines;
scrolling to a cursor costs the height of the pane, not the size of the file.
Ctrl-End on a two-hundred-thousand-line file is the same amount of work as
pressing Down.

The modules: `text` (positions and selections), `doc` (the rope, undo, files),
`edit` (every operation), `view` (panes, scrolling, folding, the screen↔text
map), `syntax` (tree-sitter), `lang` (the language table), `lsp` (the client),
`picker` (the fuzzy list), `keys` and `cmd` (the vocabulary), `app` (state and
dispatch), `ui` (drawing), `theme`, `config`.

---

## Licence

MIT.
