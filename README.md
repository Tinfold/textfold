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
- [Files that change underneath you](#files-that-change-underneath-you)
- [Git](#git)
- [Reading what a language server says](#reading-what-a-language-server-says)
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

Right-clicking a tab offers the rest: close the others, close the saved ones,
close them all, copy the path, open it in another pane. They are commands like
any other — `close-others`, `close-saved`, `close-all`, `copy-path`,
`copy-relative-path` — so they can be bound to keys or run from the palette.
Closing several at once never asks about unsaved changes; it leaves those
buffers open and says how many it kept.

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
| Ctrl-Shift-↑ / ↓ | add a cursor on the line above / below |
| Ctrl-Alt-↑ / ↓ | the same, where your desktop has not taken them |
| Alt-Shift-I | a cursor at the end of every selected line |
| Alt-Shift-C | back to one cursor |
| Esc | back to one cursor, and close whatever is open |

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
| Enter / Shift-Enter | while finding: the next hit / the one before |
| F3 / Shift-F3 | next / previous, with the box closed |
| Alt-F | find the word under the cursor |
| Ctrl-H | find and replace |
| Alt-G | search every file in the project |
| F9 / Shift-F9 | next / previous change since the last commit |

Ctrl-F opens an empty box. The last search is not lost — F3 still finds it, and
pressing Enter in an empty box brings it back — but starting a search is nearly
always starting a different one, and a box you have to clear before you can
type is a box in the way.

Inside the box, Enter walks the matches and leaves the box open; the count
beside it says which one you are on, `3 of 12`. Escape closes it, and puts the
cursor back where it started unless you pressed Enter, which is you saying you
meant to go there.

Project-wide search is also on Ctrl-Shift-F and F7. Alt-G is the one in the
tables because it is the one that always arrives: Ctrl-Shift-F is
indistinguishable from Ctrl-F on a terminal without the extended keyboard
protocol, and tmux, screen and several desktops take it before the terminal
ever sees it.

A lower-case search ignores case; a search with a capital in it means the
capital. Replacing with something selected replaces only inside the selection.

### Code

| Key | |
|---|---|
| Ctrl-Space | suggest |
| F12 | go to the definition |
| Shift-F12 | everywhere this is used |
| Alt-K | what the language server knows about this; again to read it |
| F2 | rename, everywhere |
| Alt-I | do the obvious thing about the problem here — usually add the import |
| Alt-Enter | what can be done here (quick fixes, imports, refactorings) |
| Shift-F10 | what can be done here, as a menu |
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
| right click | a menu of what can be done here — the highlight follows the pointer |
| right click a tab | a menu about that file |
| middle click | paste |
| wheel | scroll the pane the pointer is over |
| hover | what the language server knows about the word under the pointer |
| click a hover | put the keyboard in it; drag to select, double click for a word |
| Ctrl-click a name in a hover | open where it is defined, in a tab |
| click a tab | switch to it — the × closes it |
| wheel over the tabs | walk along them, when there are more open than fit |
| click a ‹ or › | the next tab that way |
| drag the scroll bar | move through the file |
| click the status bar | the language, the position, the problems and the colours are buttons |

While textfold has the mouse, your terminal's own click-to-select does not
work — the clicks are coming here instead. **`toggle-mouse` hands it back.**
Run it from the palette (or bind a key) and you can select and copy the way you
normally would; run it again to take it back. `textfold --no-mouse` starts that
way, and `"mouse": false` in the settings makes it permanent.

### The right button

Right-clicking is a menu rather than a single command. In the text it offers
cut, copy and paste, undo and redo, the language server's answers — go to the
definition, find the uses, rename, what can be done here — and select, comment
and reformat. Right-clicking inside a selection keeps the selection, since
"select this, then copy it" is most of what the menu is for.

Right-clicking a tab offers the things that are about a file rather than about
a place in one: save, read again from disk, close, close the others, close the
saved ones, close them all, copy its path, open it in another pane.

Every row is a command the editor already has, and shows the key that also does
it. There is nothing in a menu that a keystroke cannot do, which is what keeps
the two from drifting apart. Shift-F10 — or the menu key, if your keyboard has
one — opens the text menu at the cursor without a mouse.

### Copying, and where it goes

Ctrl-C puts the text in three places at once, so that at least one of them is
the one you meant:

* textfold's own clipboard, which is what Ctrl-V puts back.
* Your terminal's, by OSC 52. This is the one that works over `ssh`: a copy
  made in an editor running on a server lands on the clipboard of the machine
  in front of you. Not every terminal implements it, and several that do have
  it off by default or ask first — tmux needs `set -g set-clipboard on`, and
  Ghostty, kitty and Alacritty each have a setting for it.
* Whatever your desktop ships for the job — `wl-copy`, `xclip`, `xsel`,
  `pbcopy`, `clip.exe`, `termux-clipboard-set` — where there is one and there
  is a display to talk to. This is the one that always works locally.

The first copy of a session says which of these it found, so you do not have to
guess. Ctrl-V reads the desktop's clipboard back where it can, so a copy made
in a browser pastes into textfold without going through the terminal's own
paste key.

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
all of them at once. **Ctrl-Shift-↑/↓** adds a cursor straight up or down, and
**Alt-click** puts one wherever you like.

Ctrl-Alt-↑/↓ does the same, and it is what every other editor uses, but GNOME
and KDE both bind it to switching workspace and a desktop gets a keystroke
before the terminal under it does. It stays bound for the desktops that leave
it alone; Ctrl-Shift-↑/↓ is the one written down here because it is the one
that arrives. (`gsettings set org.gnome.desktop.wm.keybindings
switch-to-workspace-up "[]"`, and `-down`, takes the other back if you would
rather have it.)

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
| C# | `OmniSharp -lsp` |
| Java | `jdtls` |
| Bash | `bash-language-server` |
| JSON | `vscode-json-language-server` |
| TOML | `taplo` |
| YAML | `yaml-language-server` |
| Markdown | `marksman` |
| HTML / CSS | `vscode-html-language-server`, `vscode-css-language-server` |
| Dockerfile | `docker-langserver` |

Install the ones you want the way you normally would — for Rust that is
`rustup component add rust-analyzer`. OmniSharp is the one whose name is worth
checking: its own releases and most distributions call the binary `OmniSharp`,
which is what textfold runs, but some package managers install it lowercase.
`{ "languages": { "csharp": { "servers": [{ "command": "omnisharp", "args":
["-lsp"] }] } } }` in your own `languages.json` is the whole fix. jdtls wants a
JDK 21 or newer on `JAVA_HOME` even to edit an older project, and writes its
index into a workspace directory it picks itself; the first file you open in a
large project is slow once and quick afterwards. A server that is not installed is
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
borrows), C#, Java, Bash, JSON, TOML, YAML, Markdown, HTML, CSS. Three of the
shipped highlight queries are wrong upstream and textfold reads its own
correction on top of them: Rust's has a typo that stops every SCREAMING_CASE
constant from being coloured as one, and C#'s and Java's each open with a
catch-all that takes every identifier in the file before their own rules get a
look in — which in Java's case leaves the types coloured, because those are a
node of their own, and everything else a plain variable.

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
  "reload_on_change": true,
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
| `reload_on_change` | read a file again when something else writes it — see [Files that change underneath you](#files-that-change-underneath-you) |
| `mouse` | whether textfold captures the mouse at all |
| `background` | paint the theme's background, or leave the terminal's own |
| `enhanced_keys` | ask for the extended keyboard protocol |
| `keys` | see [Keys](#keys) |

Most of these are also in the palette under `settings`, where changing one
writes it back for you.

---

## Colours

A theme is a small JSON file in three parts, and the parts are the three things
textfold puts on a screen. The eighteen it ships are built into the binary; any
file dropped in `~/.config/textfold/themes/` is loaded beside them, and one
taking a name textfold already uses replaces it — which is how you rewrite one
of ours without forking anything.

`ui` is the chrome: the tab row, the status bar, the borders of a picker, the
words in a dialog. Ten roles, about *tone* rather than about colour.

```json
{
  "name": "mine",
  "about": "Where these colours came from.",

  "ui": {
    "background": "#1a1b26",
    "foreground": "#c0caf5",
    "muted":      "#a9b1d6",
    "faint":      "#565f89",
    "accent":     "#7aa2f7",
    "on_accent":  "#1a1b26",
    "success":    "#9ece6a",
    "warning":    "#e0af68",
    "error":      "#f7768e",
    "info":       "#2ac3de"
  }
}
```

That is a whole theme. Nothing about code is mentioned and code is coloured
anyway: every kind of code has a tone that already means it. Strings are the
colour of things that worked. Comments are the colour of things deliberately in
the background. Numbers and types are the colour of something worth noticing.

`editor` is the pane the text is in. Eleven roles, all worked out from `ui` if
you leave them out:

`selection`, `current_line`, `gutter`, `gutter_current`, `cursor` (the block an
extra cursor is drawn as), `bracket_match`, `whitespace` (the dots and arrows,
when they are being shown), `ruler`, and the three git draws a line's history
in: `added`, `changed` and `removed`.

`syntax` is the code, and there are thirty-one of them:

`keyword`, `keyword_control`, `function`, `function_builtin`, `method`,
`macro`, `type`, `type_builtin`, `constructor`, `string`, `string_escape`,
`string_special`, `character`, `number`, `boolean`, `comment`, `comment_doc`,
`constant`, `variable`, `variable_builtin`, `parameter`, `property`,
`operator`, `punctuation`, `bracket`, `delimiter`, `attribute`, `namespace`,
`tag`, `label`, `error`.

They are tree-sitter's own capture names, so a grammar that is more specific
than textfold is falls back along the dots: `@function.method.builtin` lands on
`method` without anyone saying so. **Every theme textfold ships names all
thirty-one**, because code is what you look at all day and the ten tones,
stretched over thirty-one jobs, are only ever "close enough".

A theme of your own can say as little as it likes. Anything left out comes from
the theme named in `base`, so being precise about one thing does not mean
restating the rest:

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

Colours are written as `#7aa2f7`, `#f0c`, a number for a slot in the
256-colour cube, or a name (`cyan`, `bright-red`, `default` for the terminal's
own). A theme naming no background leaves your terminal's own showing through,
which is what `terminal` is.

A theme file written for sshman, whose twelve roles are flat at the top level
and named for a file manager's job, still reads: `text`,
`bg`, `dim`, `good`, `warn` and `bad` land where they belong, and `dir`, `link`,
`exec` and `ansi` are accepted and ignored, because textfold draws neither a
file listing nor a terminal. The names above win where a file uses both.

`textfold --list-themes` lists them. `Alt-T` tries them on.

Shipped: `terminal`, `catppuccin`, `dracula`, `nord`, `tokyonight`, `gruvbox`,
`everforest`, `solarized`, `onedark`, `monokai`, `kanagawa`, `rosepine`,
`mariana`, `afterglow`, `darcula`, `ayu`, `solarized-light`, `latte`.


---

## Files that change underneath you

textfold looks at the files it has open about once a second, and notices when
something else has written one — a build that reformats, a `git checkout`, the
same file open in another window.

* A buffer with nothing unsaved in it is simply read again. There is nothing to
  lose, and looking at text that is no longer in the file is worse than useless.
  Your cursor and your scroll position stay where they were, and the re-read is
  an ordinary undo step, so Ctrl-Z puts back what you were looking at.
* A buffer with unsaved changes of your own is **left exactly alone** and its
  tab is marked. Only you can say which side wins. `reload` takes theirs;
  Ctrl-S keeps yours.
* A file that has been deleted is marked and kept. An empty screen is not what
  "your file is gone" should look like.

`"reload_on_change": false` turns the first of those off; the marks stay either
way.

### What a tab is telling you

The one column at the right of a tab is its close cross and its state, since
they are never both wanted at once:

| | |
|---|---|
| `×` | nothing to report — click it to close the tab |
| `●` | unsaved changes |
| `≠` | something else has written this file since you last read or saved it |
| `!` | the file is not there any more |

The tab's **name** is drawn in the error or warning colour when a language
server has said something about that file, so a mistake in a file you are not
looking at is still on the screen.

---

## Git

textfold reads git; it does not write it. There is no committing, staging or
stashing here, and there never will be — that is what `git` is for, and it is
one window away.

What it does show is the two things you cannot get from the text alone:

* **Which branch you are on**, in the status bar, with a count of how many
  lines of this file differ from the last commit. Clicking it steps to the next
  one.
* **Which lines you have touched**, as a bar down the gutter: green for a line
  that was not there before, blue for one that is not what it was, red where
  something was deleted. F9 and Shift-F9 walk them — a run of changed lines
  counts as one change, so this steps through your edits rather than through
  the lines they happened to touch.

The comparison is against `HEAD`, worked out from the committed text rather
than by asking `git` on every keystroke, so typing costs a diff and not a
process. A commit, a checkout or a rebase in another window is noticed, and
everything is worked out again from the new head.

A file git has never seen gets no marks and no column, rather than every line
of it drawn as new.

---

## Reading what a language server says

Hovering over something — with the pointer, or with Alt-K — shows what the
language server knows about it in a box beside the code. That box is a glance.

Pressing Alt-K again, or clicking the box, puts the keyboard **in** it: it
grows to the height of the screen, stays put while you move the pointer, and:

| | |
|---|---|
| arrows, PgUp/PgDn, Home/End | scroll it |
| drag across it | select part of it |
| double click | select a word |
| Ctrl-C | copy what you selected — or the line, if you only clicked |
| Enter | open the whole thing in a tab |
| Esc, or any other key | back to the text |
| Ctrl-click a name in it | open where it is defined, in a tab |

The box keeps its size while you read it — it does not shrink as you scroll,
and it stops with the last line on the bottom row rather than emptying itself
out from the top.

Copying out of it is the same gesture as copying out of code: drag across the
part you want and press Ctrl-C. Nothing about it is a special "copy the
documentation" command, because it did not need to be one.

### Following a name

Names in a hover behave like links, and only names do. The words of a sentence
are not lit up: what a pointer will follow is what the markup said was code —
a run in backticks, the text of a link — and, inside a fenced example, the
parts the grammar called a type, a function, a method or a namespace. A
keyword, a string, a number and a local variable are not places to go.

Moving the pointer over one underlines it and the bottom edge says what a click
would do. Following it asks the language server the same question Ctrl-clicking
the code would: where is this defined? Where the file you are in uses that name
somewhere, that is exactly the question asked, at exactly that spot — which is
how `HashMap` in a docstring opens `std`'s own source, in another crate, rather
than a list of the nine things in your dependency tree that happen to be called
`HashMap`. Where the file never mentions it, textfold falls back to searching
the project by name: one hit opens it, several open a list, none says so.

This works whether or not the box has the keyboard — moving the pointer *into*
a hover no longer dismisses it, which is what makes it reachable with the mouse
at all.

"Open it in a tab" is the one that matters for anything longer than a
paragraph. Rather than teaching a floating box to be an editor — selection,
search, copying a fragment — the box becomes a buffer, which is the thing this
editor already knows how to do all of that to. It stays open in a tab while you
go back to the code it is about.

### Imports you have not written yet

When the cursor comes to rest on something the language server has complained
about, textfold quietly asks what could be done about it. If there is an
answer, it appears in the status bar in the server's own words —
`Alt-i: Import 'List' (java.util)` — and at the bottom of the hover.

**Alt-I** does it. One fix means one keystroke: the import goes in and you
carry on typing. Several means a list. Nothing means it says so.

This is deliberately narrower than Alt-Enter, which asks for everything the
server can offer here including refactorings. Fixes are cheap to ask for and
are the answer to a question you did not know you were asking; refactorings are
expensive and are the answer to one you did.

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
nothing at all. A marker is usually a file name, but `"*.sln"` is allowed and
means any file with that extension — for the projects whose marker file is
named after the project rather than after the language.

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

**Copying does not reach anything else on my machine.** The first copy of a
session says which routes it found. If it says "OSC 52 only", install
`wl-clipboard` or `xclip` and it will use that instead; if you are inside tmux,
`set -g set-clipboard on` is what lets OSC 52 through. Some terminals ask
before letting a program write the clipboard, and some have it off entirely —
Ghostty's `clipboard-write`, kitty's `clipboard_control`, xterm's
`allowWindowOps`.

**Ctrl-Shift-F does nothing.** Your terminal or your desktop took it. Alt-G and
F7 do the same thing and always arrive. The same is true of any
Ctrl-Shift-something: `f1` shows what is actually bound on this machine.

**Java hovers only say which jar something came from.** jdtls has the class but
not its source or its javadoc. textfold asks it to fetch both
(`java.maven.downloadSources`, `java.eclipse.downloadSources`) and to decompile
what it cannot fetch (`java.references.includeDecompiledSources`), but the
first of those needs a working Maven or Gradle setup and a network, and it
happens in the background — the first hover after opening a project can be the
jar name and the second the real thing. If it never improves, `server-status`
says whether jdtls is still importing, and `--log-path` says where it wrote its
complaints. Going to a definition inside a jar opens the class in a read-only
tab, which works whether the source was downloaded or decompiled.

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
is two sets of cursors, and an edit made in one is told to both. A pane also
remembers where it was in every file it has shown, so switching tabs is
switching back rather than reopening.

**Anything the editor does to a buffer, it does through the same door.** Reading
a file again from disk is not a fresh `Document` replacing the old one — it is
an ordinary edit, so cursors are carried by the code that carries them across a
paste, language servers are told what changed rather than left holding stale
text, and the whole thing can be undone.

**Nothing waits on anything.** Keystrokes, mouse events, language server
messages and the results of walking the project all arrive on one channel, in
one loop. There are no locks anywhere near the text.

**Bounded by what is on screen.** Colours are worked out for the visible lines;
scrolling to a cursor costs the height of the pane, not the size of the file.
Ctrl-End on a two-hundred-thousand-line file is the same amount of work as
pressing Down.

**A menu is a second way to reach the keys, not a second implementation.**
Every row of every context menu is a `Cmd` the editor already has, shown beside
the key that also runs it. There is nothing a menu can do that a keystroke
cannot, which is what keeps the two from drifting.

The modules: `text` (positions and selections), `doc` (the rope, undo, files),
`edit` (every operation), `view` (panes, scrolling, folding, the screen↔text
map), `syntax` (tree-sitter), `lang` (the language table), `lsp` (the client),
`git` (branch and diff), `picker` (the fuzzy list), `menu` (the context menus),
`keys` and `cmd` (the vocabulary), `app` (state and dispatch), `ui` (drawing),
`theme`, `config`, `term` (the clipboard, and what else the terminal is asked
for).

---

## Licence

MIT.
