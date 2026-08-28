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
- [Comparing two panes](#comparing-two-panes)
- [One box, several lists](#one-box-several-lists)
- [Several cursors](#several-cursors)
- [Language servers](#language-servers)
- [Settings](#settings)
- [Colours](#colours)
- [Files that change underneath you](#files-that-change-underneath-you)
- [Git](#git)
- [Reading what a language server says](#reading-what-a-language-server-says)
- [Plugins](#plugins)
  - [Installing a plugin](#installing-a-plugin)
- [Where you left off](#where-you-left-off)
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
| Ctrl-Shift-PgUp / PgDn | move this tab along the row |
| Ctrl-Q | leave |

Tabs are in the order you want them in: **drag one along the row** with the
mouse, or move the one you are on with Ctrl-Shift-PgUp and Ctrl-Shift-PgDn. A
tab being carried is drawn in the accent colour, and it swaps with a neighbour
once the pointer is past the middle of it rather than the moment it touches it
— which is what keeps a narrow tab dropped onto a wide one from trading places
back and forth under a stationary mouse. With more tabs than fit, holding one
against the ‹ or › at the end walks it that way and scrolls the row along to
follow.

Right-clicking a tab offers the rest: move it left or right, close the others,
close the saved ones, close them all, copy the path, open it in another pane.
They are commands like any other — `move-tab-left`, `move-tab-right`,
`close-others`, `close-saved`, `close-all`, `copy-path`, `copy-relative-path` —
so they can be bound to keys or run from the palette. Closing several at once
never asks about unsaved changes; it leaves those buffers open and says how
many it kept.

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
| F12, Ctrl-Enter | go to the definition |
| Shift-F12 | everywhere this is used |
| Alt-K | what the language server knows about this; again to read it |
| F2 | rename, everywhere |
| Alt-I | do the obvious thing about the problem here — usually add the import |
| Alt-Enter | what can be done here (quick fixes, imports, refactorings) |
| Shift-F10 | what can be done here, as a menu |
| Alt-Shift-F | reformat the file |
| — | `fix-all` in the palette: every fix the servers would make on their own |
| — | `organize-imports`: put the imports in order and drop the unused ones |
| — | `format-and-fix`: both, in the order that works |
| F8 / Shift-F8 | next / previous problem |
| Alt-D | all the problems, as a list |
| Alt-O | what this file defines |
| Alt-P | what arguments this call takes |

Ctrl-Enter needs a terminal that implements the extended keyboard protocol —
without one, Ctrl-Enter and Enter are the same bytes down the wire and no
program can tell them apart. F12 works everywhere.

Pointing at something a language server has complained about shows what it
said, above whatever else it knows about that spot. It works on a bracket or a
stretch of whitespace as well as on a name: a warning is not always about a
word, and pointing at the squiggle is how you ask what is wrong there. Alt-K
does the same from the keyboard, and both work with no server running at all,
because the message has already arrived.

### Panes and the view

| Key | |
|---|---|
| Alt-V | another pane onto the same file |
| Alt-W | into the next pane |
| Alt-Q | close this pane |
| Alt-\ | side by side, or one above the other |
| Alt-C | compare the two panes |
| Alt-T | pick colours |
| Alt-N | line numbers on and off |
| Alt-Z | fold long lines, or let them run off the side |
| — | `plugins` in the palette: what is on, and what to switch off |
| — | `install-plugin`: fetch a plugin, or what one needs to work |
| — | `uninstall-plugin`: take one off this machine again |
| — | `restore-session`: open again what was open here last time |

Up to four panes. Each pane has its own cursor, its own scroll position and its
own idea of whether lines fold — the same file open twice is two views of one
document, and typing in either moves the other's cursor along with the text it
was pointing at.

### Comparing two panes

Alt-C compares the pane you are in with the one beside it. The bar in the
margin — the one that usually says how a line differs from the last commit —
says instead how it differs from the other pane, on both sides at once, and the
pane you are *not* in scrolls to keep its matching lines level with yours. A
block of lines that only one side has pushes the other side's view along, so
the two stay lined up through an insertion rather than drifting a screen apart.

F9 and Shift-F9 step to the next and previous difference while a comparison is
on, the same keys that step through your own changes when one is not. The
status bar says how many lines differ, and clicking it steps too.

It keeps up on its own: edit either side and the comparison is worked out
again, so fixing a difference makes it disappear rather than leaving a stale
diff on the screen. Closing a pane, or showing another file in one, ends it.
Alt-C again turns it off.

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
in the command palette — including the ones a [plugin](#plugins) brought,
which are named `plugin/thing`:

```json
{ "keys": { "pytools/lint": ["f6"] } }
```

`f1` shows what you actually have, not what textfold shipped with.

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
| drag a tab | move it along the row |
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
a place in one: save, read again from disk, move it left or right, close, close
the others, close the saved ones, close them all, copy its path, open it in
another pane.

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
  Ghostty, kitty and Alacritty each have a setting for it. Inside tmux or
  `screen` the sequence is wrapped so that the multiplexer passes it on rather
  than eating it, which is otherwise the usual reason a copy made over `ssh`
  goes nowhere.
* Whatever your desktop ships for the job — `wl-copy`, `xclip`, `xsel`,
  `pbcopy`, `clip.exe`, `termux-clipboard-set` — where there is one and there
  is a display to talk to. This is the one that always works locally.

The first copy of a session says which of these it found, so you do not have to
guess. Ctrl-V reads the desktop's clipboard back where it can, so a copy made
in a browser pastes into textfold without going through the terminal's own
paste key.

Every copy says `copied N characters` in the status bar, and that line is the
thing to look at when a copy comes out wrong. If it is missing, the keystroke
never reached textfold — some terminals, Windows Terminal among them, bind
Ctrl-C to their *own* copy command whenever there is a selection in the
terminal, so what lands on the clipboard is whatever was selected there rather
than what was selected here. Clearing the terminal's selection first, or using
the right-button menu's Copy, tells the two apart.

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

What is wired up: completion (as you type, on the characters the server says
should trigger it, and again as you keep typing when the server said its answer
was partial — which is what puts names your file has not imported yet in the
list, with the import that comes with them), diagnostics (underlined in the text, marked in the
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

**Each server is told about an edit in the form it asked for.** A server says
at startup whether it wants the whole document on every change or just the
ranges that changed, and which one it asked for is not the editor's to choose:
by the letter of the protocol a full-document change carries a `text` and no
`range`, so a server that asked for the whole thing and is handed a range gets
either an error or — worse — a document replaced by the few characters you just
typed. Its copy stops being your file, and everything it says afterwards is
about something that does not exist. The symptom is unmistakable once you have
seen it: hover and completions work when you open a file and never again after
the first keystroke. taplo is the one that ships here asking for whole
documents; the big servers all ask for ranges, because re-reading a hundred
thousand lines on every keystroke is not free.

A rename or a quick fix that touches nine files opens all nine as tabs and
leaves them modified rather than writing them behind your back. Actions that
would create, move or delete files are refused; an editor that deletes a file
because a code action said so is an editor nobody trusts twice.

### What it will start, if you have it

Every one of these is a [plugin](#plugins), with a row in the plugins list, a
line in your settings file, and an answer to *and how do I get it*. None of
them is written into the definition of a language: `pyright` is a plugin that
says it is for Python, in the same way a plugin of yours would.

| Plugin | Language | Runs | `--install` fetches it with | Linux |
|---|---|---|---|---|
| `rust-analyzer` | Rust | `rust-analyzer` (clippy, all features, inlay hints) | rustup, brew | yes |
| `pyright` | Python | `pyright-langserver` | npm, uv, pipx | yes |
| `ruff` | Python | `ruff server` | uv, pipx, pip | yes |
| `tsserver` | JavaScript, TypeScript, TSX | `typescript-language-server` | npm | yes |
| `gopls` | Go | `gopls` | go install, brew | yes |
| `bash-language-server` | Bash | `bash-language-server` | npm | yes |
| `vscode-langservers` | JSON, HTML, CSS | the three servers VS Code ships | npm | yes |
| `taplo` | TOML | `taplo` | its own releases, cargo, brew | yes |
| `yaml-language-server` | YAML | `yaml-language-server` | npm | yes |
| `docker-langserver` | Dockerfile | `docker-langserver` | npm | yes |
| `marksman` | Markdown | `marksman` | its own releases, brew | yes |
| `clangd` | C, C++ | `clangd` | brew | brew only |
| `omnisharp` | C# | `OmniSharp -lsp` | brew | brew only |
| `jdtls` | Java | `jdtls` | brew | brew only |

**Linux and macOS both.** Everything above the line goes through a package
manager that works the same on either — `npm`, `uv`, `pipx`, `pip`, `cargo`,
`go install`, `rustup` — or, for `marksman`, straight from its own releases
with the right build picked per platform. All of them install into [textfold's
own directory](#where-it-all-goes) rather than onto your system.

The last three are the honest exceptions: `clangd`, `OmniSharp` and `jdtls` are
published as per-platform archives with no stable download address and no
cross-platform package, so the only installer textfold can offer is `brew` —
which does run on Linux, if you have it. Without brew you get the plugin, a
**needs** beside it, and the `see` link, which is where those three were before
any of this. Installing them by hand still works: textfold finds them on your
`PATH` like anything else. If you know a reliable no-`sudo` route for one of
them on Linux, it is a five-line addition to `src/plugins/<name>.json` and
nothing else.

You can install them the way you normally would, and textfold will find them.
Or let it do it — `install-plugin` in the palette lists the ones that are not
working yet and fetches what they need. See [Installing a
plugin](#installing-a-plugin).

```
textfold --list-plugins     what is on, and what is on but has nothing to run
textfold --install ruff
```

A server that is not installed is mentioned once and then left alone; the
editor works exactly as well without it, minus the intelligence. In the plugins
list it says **needs** rather than **on**, because a row that says `on` beside
a program nobody has installed is a row that lies, and it is the lie people
spend an afternoon on.

**TOML completions need a schema.** Taplo's completions are entirely
schema-driven — without one there are no keys to complete, because TOML has
none of its own. The `taplo` plugin ships schema associations for the TOML
files people actually edit (`Cargo.toml`, `pyproject.toml`, `rustfmt.toml`,
`rust-toolchain.toml`, `taplo.toml`, `netlify.toml`, `pdm.toml`), so those work
out of the box; formatting, diagnostics, symbols and folding never needed one.
For any other TOML file, put `#:schema <url>` on its first line and taplo will
use it. Associations rather than taplo's `schema.catalogs` because taplo 0.10
cannot decode either schemastore's catalog or its own index, and logs an error
on every start for its trouble.

Two of them are worth a note. OmniSharp's own releases and most distributions
call the binary `OmniSharp`, which is what textfold runs, but some package
managers install it lowercase — `{ "id": "my-csharp", "languages": { "csharp":
{ "servers": [{ "name": "omnisharp", "command": "omnisharp", "args": ["-lsp"]
}] } } }` as a plugin of your own is the whole fix, and it will take the place
of the one that ships. jdtls wants a JDK 21 or newer on `JAVA_HOME` even to
edit an older project, and writes its index into a workspace directory it picks
itself; the first file you open in a large project is slow once and quick
afterwards.

Run `server-status` from the palette to see what is running and what it is
doing, and `restart-servers` after installing one by hand — installing one with
`install-plugin` does that for you.

### When a language has two of them

A language with two servers has two for a reason, and the reason is that they
answer different questions: `ruff` finds problems, `pyright-langserver` knows
where a name is defined.

So a question that only one of them can answer goes to the one that can, rather
than to the first server there is. It matters more than it sounds: `ruff` is up
and answering in milliseconds and pyright takes seconds to read a project, so
"the first one" is, for the whole of that time, the one that cannot answer
anything.

And a question where **two answers are better than one is put to all of them**.
That is code actions: "what can be done here" asks every server attached to the
file and shows what came back as one list, filled in as the answers arrive
rather than held until the slowest has spoken. Where more than one server
offered something, each row says which. Before this, the fixes for a Python
file were whichever server's happened to be asked for, and the other's were
simply not reachable from inside the editor — which is a strange thing for an
editor to be doing with a linter it is already running.

`fix-it` (Alt-I) is the same question narrowed to the problem under the cursor,
so the fix it offers is now the best of what all of them said, not the best of
what one of them said.

### Reformatting, and fixing

These are two different things, and a file usually wants both:

- **`format`** (Alt-Shift-F) asks the formatter to lay the code out. One
  server does this — two formatters disagreeing about a file is worse than
  either of them alone — and it is the first one attached that offers it.
- **`fix-all`** asks *every* server what it would fix in this file without
  being asked about any one spot: `source.fixAll`. This is ruff's autofixes,
  and it is the half that formatting is not. A formatter will lay an unused
  import out beautifully; it will not take it away.
- **`organize-imports`** is the same for `source.organizeImports`.
- **`format-and-fix`** does the fixes and then the formatting, in that order —
  a fix puts text in, and the formatter is what lays the result out.

`code_actions_on_save` in the settings does the fixing half every time you
save, and pairs with `format_on_save`:

```json
{
  "format_on_save": true,
  "code_actions_on_save": ["source.fixAll", "source.organizeImports"]
}
```

Both are off by default, because a setting that lets something else rewrite
your file is a setting you should have to ask for. With them on, saving is: ask
each server about each kind of fix, **one question at a time**, apply what
comes back before asking the next, format the result, and then write.

One at a time is not caution, it is the only order that works. Every answer is
a set of edits at positions in the file *as it was when the question was
asked*, so the first one applied moves everything the second one was pointing
at — apply two of them to the same text and you do not get both fixes, you get
a deleted line. If a question goes unanswered for a second and a half the save
moves on without it: a file you pressed Ctrl-S on is a file that gets written.

### Python, and which Python

A Python project is almost never the Python on your `PATH`. It is the one in
the virtual environment beside it, and every package the file imports lives in
there. A type checker pointed at the wrong interpreter does not merely lose a
few completions — it reads a different set of libraries, or none, and then
reports at length on code that is perfectly correct.

So textfold looks for the environment and points the servers at it: the one
your shell is already in (`VIRTUAL_ENV`), then `.venv`, `venv`, `env` and the
rest of the usual names beside the project, then anything else in the project
with a `pyvenv.cfg` in it, then conda. The interpreter goes over as
`python.pythonPath`, and the environment's `bin` goes on the server's `PATH`.

`python-environment` in the palette lists what it found and lets you pick,
which is what a project with two of them needs — only you know which one you
meant. The choice is written to your settings and used again next time, and the
servers restart on the spot.

This is also the answer to a checker complaining that
`Settings()` is missing its arguments when `Settings` is a `pydantic-settings`
class that takes its values from a `.env` file. `pydantic-settings` declares an
`__init__` that accepts none, and a checker that cannot see the installed
package has no way to know that — it falls back to the fields and reports every
one of them as missing. Point it at the environment the package is installed in
and the complaint goes with it. If it survives that, it is a real disagreement
with the checker rather than a misconfiguration, and it belongs in the
project's own `pyrightconfig.json` or in a [plugin](#plugins) of your own:

```json
{ "id": "my-python", "languages": { "python": { "servers": [
  { "name": "pyright", "command": "pyright-langserver", "args": ["--stdio"],
    "settings": { "python": { "analysis": {
      "diagnosticSeverityOverrides": { "reportCallIssue": "none" } } } } }
] } } }
```

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

That budget is wall-clock, and wall-clock is not a measure of work: a language
server waking up and taking every core on the machine for a second can make a
parse miss a deadline it was never given the chance to meet. So missing it once
is not the end of it. The file is tried again when things are quiet, with a
budget suited to not being in a hurry, and only a file that fails that several
times over is written off — the status bar says `colouring this file again`
while it is still trying and `this file parses too slowly` once it has given
up. Without that, a busy second while a server started up left a file grey
until you closed and reopened it.

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
  "code_actions_on_save": [],
  "trim_trailing_whitespace": false,
  "final_newline": true,
  "reload_on_change": true,
  "restore_session": true,
  "mouse": true,
  "background": true,
  "enhanced_keys": true,
  "underline_colour": "auto",
  "plugins": {}
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
| `code_actions_on_save` | the servers' own fixes to apply on save — see [Reformatting, and fixing](#reformatting-and-fixing) |
| `trim_trailing_whitespace` | drop trailing spaces on save |
| `final_newline` | give a file one if it has none |
| `reload_on_change` | read a file again when something else writes it — see [Files that change underneath you](#files-that-change-underneath-you) |
| `restore_session` | open the same files again next time — see [Where you left off](#where-you-left-off) |
| `mouse` | whether textfold captures the mouse at all |
| `background` | paint the theme's background, or leave the terminal's own |
| `enhanced_keys` | ask for the extended keyboard protocol |
| `underline_colour` | `auto`, `on`, or `off` — see [When something is wrong](#when-something-is-wrong) |
| `plugins` | what is switched off — see [Plugins](#plugins) |
| `package_paths` | where else to look for plugins you could install — see [Installing a plugin](#installing-a-plugin) |
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

### A file that is still being written

A file that something else has *finished* writing and one it is halfway through
writing look identical to a single glance, and reading the second gets you a
snapshot of a file mid-write: as much of it as had been written when you
looked. Not an error, and it does not look like one — it looks like the file,
shorter. Very often the cut lands in the middle of a character, and what you
get is the file with the tail turned into replacement characters.

Three things stop that, and they are all about the same idea: never take
content you cannot vouch for.

- **Nothing is read until it has stopped moving.** A file is taken only once it
  has looked the same twice running, a quarter of a second apart. A log being
  appended to, a build's output, a download in progress: all of them simply
  wait, and are read when they stop.
- **The file is stamped on both sides of the read**, and content that does not
  come back with the same stamp it went in with is thrown away and tried again.
  This pairing matters more than the read itself: content from one moment
  stamped with metadata from another is *worse* than a torn read, because the
  stamp then says the buffer is up to date and the damage never corrects
  itself. That was a real bug, and it is why this is spelled out.
- **A buffer is never rewritten under you with something that is not text.**
  Bytes that are not valid UTF-8 are refused for an automatic re-read — that is
  what half a file looks like when the half ends mid-character — and the file
  is marked as changed instead. `reload` still reads it, because then you
  asked and you can see the result and undo it.

The cost is that an ordinary `git checkout` takes about a quarter of a second
longer to show up. The benefit is that a file being written while you watch it
cannot put rubbish in your buffer.

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

If a server has complained about that spot, what it said is at the top of the
box, worst first, each one saying which tool said it. That works over a bracket
or a run of whitespace as well as over a name — a warning is not always about a
word — and it works with no server running, since the message arrived long
before you pointed at it.

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

Mostly you never write them. The list that appears as you type is not limited
to the names your file can already see: type `HashMa` and `HashMap` is offered
with `(use std::collections::HashMap)` beside it, and taking it puts the name
where you were typing and the `use` line at the top of the file, as one edit
you can undo with one Ctrl-Z. Anything in the list that brings a line in with
it says `+` at its right-hand end. It works for whatever your language server
knows about, which for a Rust project is every crate you depend on.

The list narrows as you type where the server said it had given a full answer,
and is asked again where it said it had not — which is nearly always the case
for names you have not imported, since no server lists every name in every
crate for two characters of prefix. So the thing you are typing towards keeps
arriving as you get closer to it, rather than the list going quiet because it
was built from the first two letters.

The other way in is for a name you have already typed. When the cursor comes to
rest on something the language server has complained about, textfold quietly
asks what could be done about it. If there is an answer, it appears in the
status bar in the server's own words — `Alt-i: Import 'List' (java.util)` —
and at the bottom of the hover.

**Alt-I** does it. One fix means one keystroke: the import goes in and you
carry on typing. Several means a list. Nothing means it says so.

This is deliberately narrower than Alt-Enter, which asks for everything the
server can offer here including refactorings. Fixes are cheap to ask for and
are the answer to a question you did not know you were asking; refactorings are
expensive and are the answer to one you did.

---

## Plugins

Everything textfold knows that is not the editor itself arrives as a plugin,
and every one of them can be switched off: the languages, the grammars, the
language servers, the colours, and the programs it runs for you.

The ones that ship are the JSON files in `src/plugins/`, built into the binary.
Yours go in `~/.config/textfold/plugins/`, as `name.json` or as
`name/plugin.json`, and once loaded there is no difference between the two
kinds: nothing textfold ships is reachable by a route your own plugin cannot
take. A plugin of yours taking an id textfold already uses replaces it, which
is how you say what Rust means here without editing textfold.

A plugin has an id, and so does each thing inside it. `python` is what Python
*is* — the colours, the comment syntax, what a `.py` file is — and `pyright`
and `ruff` are two more plugins that say they are for it. Either can be
switched off, and switching off a plugin switches off everything in it.
`plugins` in the command palette is the list, with a switch beside each row;
`textfold --list-plugins` prints the same thing.

```json
{ "plugins": { "ruff": false } }
```

A plugin that has several things in it names them `plugin/thing`, so a
`pytools` plugin with two servers has `pytools/black` and `pytools/ruff`. A
plugin that *is* one thing is named once: `pyright`, not `pyright/pyright`.

Switching one takes effect where you are standing: the languages, the
commands, the keys and the colours are all built again, the servers are
stopped and started, and the buffers you have open work out what language they
are afresh. A file whose language you set by hand keeps what you told it.

**The language servers are plugins.** Not "configured like plugins" — plugins,
by the same route yours takes, with no way for textfold to reach them that you
could not reach yourself. Ten minutes with `--list-plugins` and the JSON in
`src/plugins/` is the whole of what textfold knows about `clangd`. If you had a
settings file from before this, the ids in it are brought up to date the first
time it is read, so `"python/ruff": false` goes on meaning what it meant.

### What a plugin can contribute

| | |
|---|---|
| `languages` | how to colour a language, comment it out, and which servers to run — see [Languages](#languages) |
| `tools` | programs to run on the file: formatters, linters, test runs — see [Tools](#tools) |
| `host` | a program of its own that stays running — see [A plugin that is a program](#a-plugin-that-is-a-program) |
| `commands` | things that program answers to, each a command like any other |
| `panels` | buffers that program fills — see [A panel of its own](#a-panel-of-its-own) |
| `host.settings` | whatever the plugin wants to be told about itself |
| `themes` | sets of colours, in the shape a [theme file](#colours) is in |
| `keys` | keys it would like bound, by command name |
| `needs`, `install`, `uninstall`, `see` | what it needs on the machine and how to get it — see [Installing a plugin](#installing-a-plugin) |

```json
{ "id": "pytools",
  "name": "Python tools",
  "about": "ruff as a formatter and a linter, run as programs",
  "tools": [
    { "name": "fmt", "about": "Lay this file out with ruff",
      "command": "ruff", "args": ["format", "-"],
      "languages": ["python"], "output": "replace", "on_save": true },
    { "name": "lint", "about": "Check this file with ruff",
      "command": "ruff", "args": ["check", "--output-format", "concise", "${file}"],
      "languages": ["python"], "output": "problems",
      "pattern": "%f:%l:%c: %m", "on_save": true }
  ],
  "keys": { "pytools/lint": ["f6"] } }
```

Per plugin: `id`, `name`, `about`, `enabled` (say `false` to ship one switched
off), and the contribution tables above.

**A plugin's keys are a suggestion, not a claim.** One is bound only if
nothing already wants that key, so a plugin cannot quietly take Ctrl-S, and a
plugin you install cannot break the keys you already know. If you want it
anyway, bind it yourself — your own `keys` are read last and win.

### Installing a plugin

A plugin is two things, and installing one means dealing with both: the plugin
itself — a manifest and whatever sits beside it — and the programs it needs in
order to do anything. A `pyright` plugin on your disk with no
`pyright-langserver` to run is a switch wired to nothing, which is why the
plugins list says **needs** rather than **on** for one.

`install-plugin` in the palette is one list of both kinds:

```
needs    ruff                 Lint and formatting for Python — needs ruff
needs    gopls                Types, completions and fixes for Go — needs gopls
new      cargo                Build, check, test and clippy, without leaving the editor
```

The **needs** rows are plugins that are here and are not going to work until
something is fetched. The **new** rows are packages sitting in a directory
nobody has installed from yet. From where you are sitting "install ruff" and
"install this plugin somebody gave me" are the same sentence, so they are one
list; which of the two a row happens to be is textfold's business.

`uninstall-plugin` is the other direction: it runs the plugin's `uninstall`
steps and takes away the files, if the files are ones textfold put there.

Nothing waits. An install runs on a thread, says which step it is on in the
status line, and leaves everything the programs printed in a buffer called
`install <name>` — quietly if it worked, in front of you if it did not, because
that is where the reason is.

From a terminal:

```
textfold --list-packages                 what could be installed, and from where
textfold --install ruff                  something textfold already knows of
textfold --install ./examples/cargo      a directory with a plugin.json in it
textfold --uninstall cargo
```

**Where packages come from.** Nothing is fetched from anywhere: there is no
index, no registry and no network. A package is a directory with a
`plugin.json` in it, sitting on this machine — in
`~/.config/textfold/packages`, or anywhere else you have named:

```json
{ "package_paths": ["~/src/textfold/examples", "~/work/editor-plugins"] }
```

which is what makes `install-plugin` a list rather than a path you have to
remember. Point it at a checkout of somebody's plugins and every directory in
it becomes a row you can choose. When there is somewhere to fetch packages
*from*, it will be one more kind of row in the same list.

#### Where it all goes

**Nothing textfold fetches is installed system-wide.** An editor that runs
`npm install -g` on your behalf and drops a package into the same place your
projects' toolchains live has done something you did not ask for and cannot
easily see, and the first sign of it is usually a version conflict in something
unrelated.

So there is one directory and it belongs to textfold:

```
~/.local/share/textfold/tools/bin     the programs
~/.local/share/textfold/tools         everything they are unpacked from
```

(`$XDG_DATA_HOME` if you have set it.) Removing that directory undoes
everything textfold ever installed. It is deliberately not beside your settings
— which on macOS is `~/Library/Application Support/…` — because it is full of
executables, many of them scripts whose first line names an interpreter by
path, and a space in that path is a well-known way to break them.

That directory's `bin` goes on textfold's own `PATH`, so language servers,
tools and plugins' own programs all find what is in it without any of them
having to know it exists. **Last on the `PATH`, not first**: what you have
installed yourself goes on winning, and textfold's copy is what there is when
you have not got one. An editor that quietly shadowed the `ruff` in your
virtual environment with a copy of its own would be a very difficult afternoon.

A manifest does not have to know any of this. It says `npm install --global` —
the obvious thing to write — and textfold runs it with the variable that
package manager already documents for the purpose, so "global" means global to
textfold:

| Written in a manifest | Set for it | Lands in |
|---|---|---|
| `npm install --global` | `npm_config_prefix` | `tools/bin` |
| `pip install --user` | `PYTHONUSERBASE` | `tools/bin` |
| `pipx install` | `PIPX_HOME`, `PIPX_BIN_DIR` | `tools/bin` |
| `uv tool install` | `UV_TOOL_DIR`, `UV_TOOL_BIN_DIR` | `tools/bin` |
| `cargo install` | `CARGO_INSTALL_ROOT` | `tools/bin` |
| `go install` | `GOBIN` | `tools/bin` |

Two things cannot be contained, because the program that fetches them has no
notion of installing anywhere but the system: `brew` and `rustup component
add`. A step that uses one says `"system": true`, and textfold says so before
it runs rather than after:

```
$ textfold --install rust-analyzer
rust-analyzer:
  rustup component add rust-analyzer (installs system-wide)
  brew install rust-analyzer (installs system-wide)

Some of this installs system-wide — the lines that say so above.
```

Installing copies the package into `~/.config/textfold/plugins/<id>/` and
leaves a receipt in it. The receipt is the whole of uninstall's safety:
removing a directory is not a thing to do on a guess, and it is what tells a
directory textfold copied in — which it may take away again — from one you
wrote by hand in the same place, which it may not. A plugin you symlinked in
for development is a third case: uninstalling removes *the link*, and what it
points at is your working copy and none of textfold's business.

#### Saying what a plugin needs

| | |
|---|---|
| `needs` | the programs it has to have on the `PATH`. This is what decides whether it says **on** or **needs** |
| `install` | how to get them: a list of steps, run in order |
| `uninstall` | how to put them back. Absent means removing the plugin leaves what it fetched alone |
| `see` | where to get it by hand, for when none of the steps could |

A step is:

| | |
|---|---|
| `run` | the program and its arguments, as a list |
| `about` | the line the status bar shows while it runs |
| `unless` | a program that, if it is already there, means this step has nothing to do |
| `when` | a file that has to exist for this step to be worth running |
| `os` | `"linux"`, `"macos"`, `"windows"`, or a list. Absent means any |
| `arch` | `"x86_64"`, `"aarch64"`, or a list. Absent means any |
| `system` | say `true` for a step that installs outside textfold's own directory |

`${bin}` and `${tools}` in the arguments name [textfold's own
directories](#where-it-all-goes), for a plugin whose program is published as a
build per platform rather than through a package manager. That is the whole of
what `marksman` does, and it works the same on Linux and macOS:

```json
"install": [
  { "about": "marksman, from its releases", "os": "linux", "arch": "x86_64", "unless": "marksman",
    "run": ["curl", "-fsSL", "-o", "${bin}/marksman",
            "https://github.com/artempyanykh/marksman/releases/latest/download/marksman-linux-x64"] },
  { "about": "marksman, from its releases", "os": "macos", "unless": "marksman",
    "run": ["curl", "-fsSL", "-o", "${bin}/marksman",
            "https://github.com/artempyanykh/marksman/releases/latest/download/marksman-macos"] },
  { "about": "letting it be run", "os": ["linux", "macos"], "unless": "marksman",
    "run": ["chmod", "+x", "${bin}/marksman"] }
]
```

A step for another machine is not skipped at the last moment — it is not in the
plan at all, so what textfold says it is about to do is what it is about to do.

`when` is the mirror of `unless`, and it is what makes a download safe to write
as more than one step. Fetching a program is three of them — download, unpack,
make it runnable — and the second two are only meaningful if the first
happened. Without `when`, a machine with no `curl` would skip the download (a
step whose program is missing is skipped) and then *fail* on the unpack,
stopping the install before it reached the ways of getting it that do work
there. `taplo` is the worked example: it downloads a binary if it can, and
otherwise falls through to `cargo install` and then to `brew`.

```
$ textfold --install taplo          # on a machine with no curl and no cargo
  taplo, from its releases — skipped, no curl
  unpacking it — skipped, there is no …/tools/bin/taplo.gz
  letting it be run — skipped, there is no …/tools/bin/taplo
  taplo, built from source — skipped, no cargo
  taplo, with brew — skipped, no brew

still no taplo — see https://taplo.tamasfe.dev/cli/installation/binary.html
```

```json
{ "id": "ruff", "name": "Ruff", "needs": ["ruff"],
  "install": [
    { "about": "ruff, with uv",   "run": ["uv", "tool", "install", "ruff"],   "unless": "ruff" },
    { "about": "ruff, with pipx", "run": ["pipx", "install", "ruff"],         "unless": "ruff" },
    { "about": "ruff, with pip",  "run": ["pip", "install", "--user", "ruff"], "unless": "ruff" }
  ],
  "uninstall": [{ "run": ["uv", "tool", "uninstall", "ruff"] }],
  "see": "https://docs.astral.sh/ruff/installation/" }
```

An installer is a list of programs to run rather than a script, which means you
can read what a plugin is about to do to your machine before you let it — and
textfold prints exactly that before it does any of it. There is no shell, so
there is nothing to quote wrongly and nothing a `$` can do to you. The rules
for reading one are three sentences:

- A step whose `unless` program is already there is **skipped**. There is
  nothing to do.
- A step whose *own* program is not installed is **skipped too**, and this is
  the load-bearing one: it is what lets the three steps above be three ways to
  get the same thing, with the first one that exists being the one that runs.
  A step you cannot run is not a step that failed.
- A step that runs and comes back unhappy **stops the install there**. That is
  a real failure, and carrying on past it would only make a worse mess.

Which leaves the question of whether it worked, and that is not answered by the
exit codes:

```
$ textfold --install marksman
Marksman:
  brew install marksman

  marksman, with brew — skipped, no brew

still no marksman — see https://github.com/artempyanykh/marksman/releases
```

`needs` is checked when the last step has run. Every step was cheerful, and the
program is not there, so the install failed — and it says where to go instead
of failing without a suggestion.

### Tools

A tool is a program textfold runs on the file in front of you, and it becomes
a command like any other: it is in the palette, you can bind a key to it, and
`plugins` has a switch for it. This is the half of "an editor with plugins"
that needs no plugin runtime at all — a great deal of what people write
plugins for elsewhere is *run this program on my buffer and do something with
what it printed*, and that is a table rather than code.

| | |
|---|---|
| `name` | what it is called; the command becomes `<plugin>/<name>` |
| `command`, `args` | what to run |
| `output` | what to do with what it printed — below |
| `languages` | which languages it is for. Absent means any file, and a tool for another language is not offered in this one |
| `roots` | what marks the top of the project it runs in. Absent means `.git` |
| `stdin` | whether the buffer goes in on standard input. Absent means yes for a formatter and no for everything else |
| `on_save` | whether to run it every time the file is written |
| `pattern` | how to read a line of output as a problem, for `"problems"` |

`output` is one of four things:

- **`"replace"`** — what it printed replaces the buffer. Formatters: `black -`,
  `gofmt`, `prettier`. This is the default, because most tools are this.
- **`"problems"`** — what it printed is a list of problems, read with
  `pattern`, and shown in the margin beside the language server's own.
- **`"show"`** — what it printed opens in a buffer of its own. Test runs,
  `git log`, anything you want to read rather than apply.
- **`"ignore"`** — nothing to read; textfold says whether it worked.

`pattern` is the shape every compiler-output parser since vi has used: `%f`
the file, `%l` the line, `%c` the column, `%t` a word saying how bad it is,
`%m` the message, `%%` a per cent sign. Everything else in it is literal and
has to be there — a line that does not match is not a problem, which is what
keeps a tool's headers and summary lines out of your margin.

Nothing waits. The program is started on a thread and its answer arrives on
the same channel the keyboard and the language servers use, so a test run that
takes a minute costs a minute of it running, not a minute of the editor being
gone. A tool that prints nothing when it was meant to rewrite the file is
treated as having failed, and the buffer is left alone: emptying somebody's
file over a tool that fell over quietly is not a recoverable kind of wrong.

**`on_save`** puts a tool in the right half of the save. One that rewrites the
file runs *before* the write, with the formatter, so what lands on disk is what
you end up looking at; anything else runs *after* it, because a linter's job is
to look at what has just been saved. So this is a complete `black`-and-`ruff`
setup with no language server involved at all:

```json
{ "id": "py", "tools": [
  { "name": "black", "command": "black", "args": ["-", "-q"],
    "languages": ["python"], "on_save": true },
  { "name": "ruff", "command": "ruff",
    "args": ["check", "--output-format", "concise", "${file}"],
    "languages": ["python"], "output": "problems",
    "pattern": "%f:%l:%c: %m", "on_save": true }
] }
```

### A plugin that is a program

A tool is started, prints, and dies. That is most of what people write plugins
for, and it is why `tools` exists — but it cannot do anything that has to be
*remembered* between one keystroke and the next. It cannot hold a build that is
still running, keep a connection to a debug probe, or tell you where it has got
to while it is getting there.

A plugin with a `host` is the other kind. It is a program textfold starts and
then talks to for as long as it is wanted, over its own standard input and
output. **It can be written in any language**: the whole of what it has to do
is read and write JSON on a pipe.

```json
{ "id": "cargo", "name": "Cargo",
  "about": "Build, check, test and clippy, without leaving the editor",

  "host": {
    "command": "python3",
    "args": ["${plugin}/textfold_cargo.py"],
    "roots": ["Cargo.toml"],
    "activate": ["language:Rust", "command"]
  },

  "commands": [
    { "name": "check", "about": "Check this project, without building it" },
    { "name": "test",  "about": "Run this project's tests" },
    { "name": "stop",  "about": "Stop whatever cargo is doing" }
  ],

  "keys": { "cargo/check": ["f6"] } }
```

That one is real and it is in the repository: `examples/cargo`, about a hundred
and fifty lines of Python with nothing installed to run it. To try it:

```sh
textfold --install ./examples/cargo
```

Or, if you are going to be editing it, link it in instead so that your changes
are the ones that run — `uninstall-plugin` knows the difference and will remove
the link rather than your working copy:

```sh
ln -s $PWD/examples/cargo ~/.config/textfold/plugins/cargo
```

Or put `{ "package_paths": ["~/src/textfold/examples"] }` in your settings and
both examples turn up in `install-plugin`.

Its real interface, though, is `cargo/report`: a panel it drives itself.
`c`, `b`, `t` and `l` run check, build, test and clippy; `x` stops one; `o`
shows what cargo actually said underneath the list; `Enter` or a click on a
problem goes to it. The problems appear grouped by file *while the build is
still running*. Nothing opens a tab of text at you.

It also has `cargo/problems`, which puts what it found in the editor's own
fuzzy list and jumps to the one you pick, and `cargo/clean`, which asks first.
Right-clicking a problem in its panel opens a menu under the pointer, with "Go
to it" above the four runs.

There is a second one in `examples/copilot`: GitHub Copilot, in about four
hundred lines. Its inline suggestions are real — it bridges to the
`copilot-language-server` GitHub ships, which speaks the same framing textfold's
plugins do, so the plugin is mostly a translator between two protocols that
already agree about how to move JSON down a pipe. Its chat panel is not: Copilot's
language server has no conversation API, so the panel runs whatever
`settings.chat.command` points at. Between them the two examples use every
message in the protocol.

**It stays out of rust-analyzer's way**, which is worth copying if you write
something similar. textfold already runs rust-analyzer and configures it to
run `cargo clippy` on save, so the compiler's errors are already in your
margin — the plugin therefore keeps its findings in its panel and leaves the
margin alone, and `d` mirrors them in for anyone who has turned that off. It
also builds in a target directory of its own: two cargos sharing one take
turns on the lock and tread on each other's fingerprints, which shows up as a
build that says `Finished` and reports none of the errors that are plainly
there.

Then open a Rust file and press F6. `cargo check…` appears in the status line,
then each line cargo prints about where it has got to, **then each compiler
error in the margin as the compiler finds it** — not at the end — and the
output in a buffer. Press F6 again while it is still going and it says so
rather than starting a second cargo. None of those four things is possible for
a tool.

Per host: `command` and `args` (what to run), `roots` (the files that mark the
top of a project — one process per project, as with a language server),
`activate` (below), `wants_buffers` (which languages it wants to be told the
text of; leave it out and it is told nothing), and `env`.

`${plugin}` in a command, an argument or an environment variable is the
directory the manifest was read from. A host runs in your project, not beside
its own files, so this is how a plugin points at the program it ships with.

**Nothing starts until it is wanted.** `activate` says what counts:

| | |
|---|---|
| `"language:Rust"` | a file of that language was opened |
| `"file:**/*.ioc"` | a file matching that was opened — `*` within a name, `**` across directories |
| `"command"` | one of its own commands was run |

Running one of a plugin's commands always starts it, whether or not `command`
is listed — a command in the palette that quietly did nothing would be a bug
rather than a setting. So a plugin nobody has asked anything of is a plugin
that is not running: fourteen installed and a Rust file open is one process,
not fourteen.

Each entry in `commands` becomes a command like any other: it is in the
palette, you can bind a key to it, `plugins` has a switch for it, and it takes
its id the moment the manifest is read — before the program has ever been
started, which is what lets running it be the thing that starts the program.
Say `"behaviour": "edits"` for one that changes the text, and it will be
refused on a read-only file along with everything else that does.

A command a plugin's program is given does not hold the editor up. It goes
down the pipe and the next keystroke is dealt with; whatever the plugin has to
say arrives later, on the same channel the keyboard arrives on. A plugin that
takes four minutes to build a firmware image cannot make the cursor stutter,
because the cursor is not waiting on it — and a plugin that wedges itself
entirely is a queue that stops filling rather than an editor that stops
drawing. One that falls over three times in a minute is left alone until you
switch it off and on again.

**What a plugin can ask the editor for:**

| | |
|---|---|
| `status/say` | a line in the status bar. `"kind"` of `"good"` or `"bad"` colours it |
| `buffer/show` | open some text in a buffer, for output worth reading properly. Say `"focus": true` to be taken to it — by default you are not, because a build finishing four minutes later should not move your cursor |
| `buffer/read` | the text of a buffer, its language and its version |
| `buffer/edit` | change one, as one undoable step |
| `diagnostics/set` | problems in the margin |
| `run` | run a program, and be told how it went |
| `pick` | put a list up and be told what was chosen |
| `prompt` | ask for a line of text |
| `confirm` | ask a yes-or-no question |
| `open` | open a file, optionally at a line |
| `panel/set` | fill a panel of its own with styled, clickable lines |
| `hint/set` | offer some text where the cursor is — shown, not inserted |

…and what the editor tells it: `buffer/opened` · `changed` · `saved` ·
`closed`, `selection/changed`, `command/run`, `panel/opened` · `closed` ·
`action` · `key`, and `hint/taken` · `dropped`.

`selection/changed` says where the cursor has come to rest — **come to rest**,
not where it is: cursor motion is the highest-frequency thing that happens in
an editor, and it is sent once you stop rather than forty times on the way.
Like the buffer messages, only plugins that named a language in
`wants_buffers` get it.

A plugin is handed its own `settings` from the manifest at `initialize`. The
editor carries that block and does not read it: what a plugin wants to be told
about itself is the plugin's business.

The last four are the editor's own boxes, lent out. A plugin asking "which
board?" gets the same fuzzy list as `Ctrl-P`, with the same keys, the same
colours and the same theme — which is the point: a plugin should look like
textfold rather than like a plugin. It asks with a title and some items and is
told what was picked; **changing your mind is an answer too**, so Escape sends
back nothing rather than leaving a plugin waiting for ever on a box that has
gone.

Every one of them goes through the same code a keystroke does. That is the
rule the rest of this is built to — **a plugin may do nothing a keystroke
cannot** — and it is what makes a plugin's edit undoable, tells the language
servers about it, and carries the cursors across it without a plugin having to
know any of that exists.

### Suggestions in the text

A plugin can offer text where the cursor is, drawn in place but not put in:

```json
{ "method": "hint/set",
  "params": { "path": "/src/fib.py", "line": 2, "column": 4,
              "text": "if n < 0:\n    raise ValueError(...)" } }
```

The first line of it appears after the cursor in the colour of something that
is not there yet, with `+10 lines` after it if there is more. `Tab` takes it,
`Esc` waves it away, and moving the cursor or changing the text takes the offer
back — it was worked out about the text as it was, and the text has moved on.
An empty `text` clears it, which is how a plugin says "never mind" without a
second message.

Taking it is an ordinary edit: one thing to undo, and the language servers hear
about it, because a suggestion becomes your text the moment you take it and is
your text in every way afterwards. The plugin is told `hint/taken` or
`hint/dropped` either way, so it knows whether to offer something else.

`Tab` is still indent every other time. The key is not conditional — the offer
is: while one is on the screen it takes the handful of keys that steer it, the
same way the completion list does, and the rest of the time nothing has
changed.

Only the line the cursor is on is drawn over, and only to the right of the
cursor, so nothing that is really in your file is ever covered by something
that is not.

### A panel of its own

A plugin can also have a buffer that it fills. Declare one beside the commands:

```json
{ "panels": [
  { "name": "report", "about": "What cargo found, as a list you can click" }
] }
```

It becomes a command like any other — `cargo/report` in the palette, bindable,
with a switch in `plugins` — and running it opens the panel and tells the
plugin there is somewhere to draw. What the plugin sends is a list of lines:

```json
{ "method": "panel/set", "params": { "panel": "stm32/pins", "lines": [
    { "spans": [ { "text": "USART2", "style": "keyword" },
                 { "text": "  TX ",  "style": "muted" },
                 { "text": "PA2",    "style": "string", "action": "pin:PA2" } ] },
    "",
    "a line with nothing marked in it is just a string"
] } }
```

Clicking a span that has an `action`, or pressing `Enter` on one, sends
`panel/action` back with that string. What it says is the plugin's business —
the editor hands it straight back and never looks inside it. That is the whole
interaction model, and it is enough for a tree, a form, a toolbar or a list of
problems.

A panel **is a buffer**. It is a tab, it splits, it scrolls, it has a border,
and `Alt-,` gets you back to it — because it is the same `Document` as
everything else, with two differences: it is read-only, and its colours come
from the plugin instead of from a grammar.

**Keys in a panel.** A panel is handed the keystrokes that would otherwise have
*changed the text* — which, in a buffer that is not yours to change, are going
spare. So plain letters, `Enter`, `Tab` and `Backspace` arrive as `panel/key`
with the key spelled the way a settings file spells it (`c`, `ctrl-.`, `f6`),
along with the line and column the cursor was on. Everything else still does
exactly what it does everywhere else: `Ctrl-P` is the palette, `Ctrl-W` closes
the tab, the arrows move, `F8` goes to the next problem. A plugin cannot take a
key you already know — the same bargain a plugin's suggested bindings get,
made for a buffer instead.

**Styles are named, never coloured.** A span asks for `keyword`, `string`,
`error`, `muted` — the same names a grammar's captures use, resolved against
whatever theme you are in. So a panel is themed with the rest of the editor and
re-themes for free when you switch, and a plugin author cannot pick colours that
fight your theme. A name nothing knows is drawn as ordinary text rather than
refused, so one style misspelt is not a panel you cannot read.

The plugin sends the whole panel each time rather than a patch. A panel is tens
of lines and changes a few times a second at worst, so there is nothing to
diff and nothing that can fall out of step with what is on the screen.

The panel is replaced the way any other whole-buffer change is, so the cursor
is carried across a refresh rather than thrown back to the top of a panel you
were halfway down. `panel/closed` says you have closed it, so a plugin can stop
keeping it up to date.

Two of those want spelling out.

**Positions are lines and columns, counted from zero, in characters.** Not
bytes, and not the UTF-16 that LSP counts in. That is what `diagnostics/set`
and `buffer/edit` take. A column past the end of its line means the end of
that line, which is what a compiler pointing at "column 200" of a
forty-character line means. The one exception is `buffer/changed`, which
carries plain character offsets — a plugin keeping its own copy of the text
wants the two numbers it can slice with.

**An edit says which version it was worked out against, and is refused if the
buffer has moved on.** A plugin that computed a fix against version 40 of a
file that is now at 43 is holding an edit for text that is not there any more,
and applying it would damage the file rather than mend it. So it comes back as
an error saying which version it was for, and the plugin can ask again. The
same rule the editor already applies to a formatter's reply.

Problems from a plugin sit in the margin beside the language server's, and are
**namespaced by plugin**: a fresh set from one replaces only its own findings.
A plugin cannot clear clangd's, and clangd cannot clear the plugin's. Sending
`items` with a `path` replaces what that plugin said about that file; sending
none replaces everything it has said.

Buffer traffic goes only to plugins that asked for it. `wants_buffers` in the
manifest names the languages, and a plugin that named none is told nothing at
all — the messages are `buffer/opened`, `buffer/changed`, `buffer/saved` and
`buffer/closed`, and a plugin that comes up late is told about everything
already open rather than only about what you open next.

The protocol is JSON-RPC 2.0 framed the way a language server's is — a
`Content-Length` header, a blank line, then that many bytes. That is
deliberate: nearly every language already has a library that speaks it, so a
plugin author writes handlers rather than a transport. In Python it is about
twenty lines to do by hand, which is what `examples/cargo` does so that you can
read all of it.

Anything the program writes to standard error goes to the log — `textfold
--log-path` says where — rather than onto the screen, which belongs to the
editor.

### Languages

```json
{ "id": "zig", "name": "Zig", "about": "Colours and zls",
  "languages": {
    "zig": {
      "extensions": ["zig", "zon"],
      "line_comment": "//",
      "servers": [{ "name": "zls", "command": "zls", "roots": ["build.zig"] }],
      "grammar": {
        "library":    "~/.config/textfold/grammars/zig.so",
        "highlights": "~/.config/textfold/grammars/zig-highlights.scm"
      }
    }
  } }
```

Colours come from any tree-sitter grammar built as a shared library —
`tree-sitter build` produces one — opened at the moment a file of that language
is first shown.

Per language: `extensions`, `filenames` (for the many files with no extension),
`shebangs`, `line_comment`, `block_comment`, `brackets`, `lsp_id`, `servers`,
`grammar`. Per server: `name`, `command`, `args`, `roots`, `settings`,
`init_options`, `env`.

A language named by more than one plugin **merges** field by field, in the
order the plugins load, so adding a server to Rust is three lines and does not
mean restating the grammar and the comment syntax:

```json
{ "id": "my-rust",
  "languages": { "rust": { "servers": [{ "command": "ra-multiplex" }] } } }
```

This is how the language servers work: `python` says what Python is, and
`pyright` and `ruff` are two more plugins that add a server each to it.
**Servers are added to rather than written over**, or the second plugin to be
read would take the first one's place and you would get whichever of the two
sorted last. A server of the same *name* does still take an earlier one's
place, which is what lets you swap one out rather than end up with both:

```json
{ "id": "my-rust", "languages": {
  "rust": { "servers": [{ "name": "rust-analyzer", "command": "ra-multiplex" }] } } }
```

The other way to say the same thing is `{ "plugins": { "rust-analyzer": false } }`,
which switches the shipped one off and leaves yours the only one there.

Turning a server's settings up:

```json
{ "id": "my-rust", "languages": {
  "rust": { "servers": [{
    "name": "rust-analyzer",
    "command": "rust-analyzer",
    "roots": ["Cargo.toml", "rust-project.json", ".git"],
    "settings": { "rust-analyzer": {
      "check": { "command": "clippy" },
      "cargo": { "allTargets": false }
    } }
  }] }
} }
```

`~/.config/textfold/languages.json` still works and still means what it always
did — the same `{ "languages": { … } }`, read last so it wins. It shows up in
the plugin list as `local`, so what it is doing to your languages is something
you can see and switch off rather than something you have to remember.

### Placeholders

A server's `args`, `env`, `settings` and `init_options` may use placeholders,
which is how Python's servers are told where the project's virtual environment
is without any of that being written into the editor as a special case:

| | |
|---|---|
| `${venv}` | the project's Python environment |
| `${venv_bin}` | the `bin` (or `Scripts`) inside it |
| `${python}` | the interpreter inside it |
| `${root}` | the project root |
| `${env:NAME}` | an environment variable, as textfold was started with |

A value naming an environment on a project that has none is dropped whole
rather than half-filled — the setting simply is not sent, which is right: a
`pythonPath` pointing at nothing because a substitution left a hole is worse
than no `pythonPath`. A server that mentions no placeholder is never even
looked up, so this costs nothing for every other language.

`roots` matters more than it looks. It is the marker files that say where a
project starts; the nearest ancestor holding one is the directory the server is
told about. A server given the wrong root indexes either far too much or
nothing at all. A marker is usually a file name, but `"*.sln"` is allowed and
means any file with that extension — for the projects whose marker file is
named after the project rather than after the language.

`textfold --list-languages` shows what is in force, and `--list-plugins` shows
where it came from.

---

## Where you left off

Close textfold with thirty tabs open and start it again in the same directory,
and the thirty tabs are there — in the order the row was in, each with the
cursor where you left it, and the panes arranged the way you had them.

Per directory, not per machine. "Where was I" is a question about a project,
so opening textfold in one repository does not bring back another one's tabs.
The last few dozen projects are remembered, in
`$XDG_STATE_HOME/textfold/sessions.json`.

It only happens when you name nothing on the command line: `textfold notes.md`
means open that file, not open that file and the eleven you had open on Friday.
`--no-session` starts empty regardless, `restore-session` in the palette brings
them back on demand, and `"restore_session": false` in the settings turns the
whole thing off.

A file that has been deleted since is skipped rather than opened empty, and a
project you deliberately closed everything in is forgotten rather than
remembered as empty — coming back to it should not reopen what you shut.

---

## When something is wrong

**A key does nothing.** `F1` shows what you actually have bound. If a key you
set is missing, textfold says so once at startup — a key that silently does
nothing is a bad afternoon.

**No completions.** `server-status` in the palette says whether a server is
running and what it is doing. rust-analyzer is often still indexing; the top
right says so while it is. If it is not running at all, it is probably not
installed: `plugins` says **needs** rather than **on** beside a server whose
program is missing, and `install-plugin` will fetch it.

**A server said something and I missed it.** Servers' complaints go to a file,
not the screen. `textfold --log-path` says where.

**No colours.** The status bar says why when there is a reason worth giving.
`colouring this file again` means a parse ran out of time and another attempt
is coming — usually because something else on the machine was busy — and it
sorts itself out. `this file parses too slowly` means it tried several times
and stopped. Otherwise the language shown beside it is probably not what you
thought — the palette's `set-language` fixes that for the file in front of you,
and a [plugin](#plugins) fixes it for good. If a whole language has gone quiet,
check `plugins`: it may be switched off.

**The text goes dim, italic, or invisible where there are warnings, and the
underlines come out as blocks.** Your terminal does not understand the sequence
that says "colour the underline and leave the text alone", and is reading the
colour as four more instructions instead — `2` is dim, and a colour component
that happens to be a small number asks for italic, reverse video, or conceal.
It is a real thing that real terminals do, so textfold asks for that colour only
where it knows the terminal has it: kitty, ghostty, WezTerm, foot, contour,
rio, Alacritty, iTerm2, VS Code's terminal, and anything built on VTE 0.52 or
newer. Everywhere else the underline is drawn plain, which says the same thing
— the mark in the margin already carries the colour.

The usual way to meet this is over `ssh`, where the only thing the terminal
tells the far end about itself is `TERM`, and `TERM` usually says
`xterm-256color` and nothing more. If your terminal does have coloured
underlines, say so: `"underline_colour": "on"`, or the row in `settings`. If
something else on this list is mangling the screen, `"off"` is there too.

**I want my terminal's mouse back.** `toggle-mouse`, or `--no-mouse`, or
`"mouse": false`.

**Copying does not reach anything else on my machine.** The first copy of a
session says which routes it found. If it says "OSC 52 only", install
`wl-clipboard` or `xclip` and it will use that instead; if you are inside tmux,
`set -g set-clipboard on` is what lets OSC 52 through. Some terminals ask
before letting a program write the clipboard, and some have it off entirely —
Ghostty's `clipboard-write`, kitty's `clipboard_control`, xterm's
`allowWindowOps`.

**Ctrl-C pastes something I did not copy.** Look for `copied N characters` in
the status bar. If it is not there, textfold never saw the keystroke: some
terminals take Ctrl-C for their own copy command while anything is selected in
the terminal itself, so what reaches the clipboard is that selection instead.
Clear the terminal's selection, or use the right-button menu's Copy, which
cannot be intercepted.

**Go to definition or find references does nothing.** The row is lit only when
a server attached to the file says it can answer that particular question, so a
row you can click is a row that works. If it is greyed out, `server-status`
says which servers are up — for Python, `ruff` arrives seconds before pyright
does and answers none of these.

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

**What textfold knows is data, and the data can be switched off.** Languages,
grammars, language servers, colours and tools all arrive as plugin manifests
read at startup — the ones that ship are in the same shape as the ones you
write, loaded through the same code, and listed in the same place with the
same switch beside them. Turning one off rebuilds the languages, the commands,
the keys and the colours rather than asking you to restart the editor, and an
id survives that, so a buffer that was Python is still Python when Python
comes back. Installing one is the same rebuild, which is why a plugin you have
just fetched works where you are standing.

**Installing is data too.** A plugin says what programs it needs and gives a
list of ways to get them, and that list is a table rather than a script: you
can read what it will do to your machine before you let it, and textfold prints
exactly that first. Whether it worked is decided by looking for the programs
afterwards rather than by believing the exit codes — an installer that was
cheerful about every step and fetched nothing has failed. An install and an
uninstall are the same `Plan`, run on a thread with what it says arriving as
events, or run straight through by `--install` with what it says printed.

**Everything textfold talks to that is not a person talks the same way.** A
language server and a plugin's own program are both a child process, a thread
that does nothing but frame JSON off its output, and a note of what each
outstanding question meant — written once, in `rpc`, and used twice. Which is
most of the reason a plugin can be written in any language: the format was
picked because every language already has a library for it, not because it was
the easiest to write here.

**A command is a number, and the list is open.** `Cmd` is an index into a
registry, so a key binding, a palette row, a menu row and a status-bar button
all hold the same thing whether the command behind it is one textfold ships or
one a plugin brought. The built-ins are one table — the name, the group, the
line the palette shows, what it does to the text, and what it actually does —
and that table is the only place any of them is written down. A row that does
not say what it does will not compile, which is what replaced the exhaustive
`match` that used to enforce it, and folding "does this change the text" into
the same row retired two lists that had to be kept in step by hand.

**A menu is a second way to reach the keys, not a second implementation.**
Every row of every context menu is a `Cmd` the editor already has, shown beside
the key that also runs it. There is nothing a menu can do that a keystroke
cannot, which is what keeps the two from drifting.

The modules: `text` (positions and selections), `doc` (the rope, undo, files),
`edit` (every operation), `view` (panes, scrolling, folding, the screen↔text
map), `syntax` (tree-sitter), `lang` (the language table), `rpc` (JSON-RPC on a
pipe), `lsp` and `host` (the two things that speak it), `git` (branch and diff),
`picker` (the fuzzy list), `menu` (the context menus), `keys` and `cmd` (the
vocabulary), `plugin` (the manifests) and `pack` (getting one onto the machine
and off it again), `app` (state and dispatch), `ui` (drawing), `theme`,
`config`, `term` (the clipboard, and what else the terminal is asked for).

---

## Licence

MIT.
