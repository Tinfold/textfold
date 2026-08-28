#!/usr/bin/env python3
"""cargo, as a textfold plugin.

The point of this file is not cargo. It is that it is about three hundred
lines of Python with no dependencies, and it does things a `tools` entry in a
manifest cannot do at all:

  * it *holds* a running build, so a second one while the first is going is
    refused with a reason rather than starting a second cargo;
  * it *streams*, so where cargo has got to appears while the build is still
    running and you are still typing;
  * it can be *told to stop*, because there is something there to tell;
  * it keeps a **panel** — a buffer of its own that it fills with styled,
    clickable lines, drives with its own keys, and updates as the build runs.

The panel is the whole interface. Nothing here opens a tab of text at you:
what there is to read is in the panel, and `o` shows or hides the compiler's
own words underneath the list.

**On rust-analyzer.** textfold already runs rust-analyzer, and textfold's Rust
plugin configures it to run `cargo clippy` on save — so the compiler's errors
are already in your margin. This plugin therefore leaves the margin alone by
default and keeps its findings in its panel; `d` mirrors them into the margin
for anyone who has turned rust-analyzer's own checking off and wants them
there. It also builds in a target directory of its own, because two cargos
sharing one take turns on the lock and tread on each other's fingerprints.

The protocol is JSON-RPC 2.0 with LSP's framing — a Content-Length header, a
blank line, then that many bytes. The whole of it is `read_message` and `send`
below, about twenty lines, which is why a plugin can be written in anything.
"""

import json
import os
import subprocess
import sys
import threading

PANEL = "cargo/report"

# What cargo prints at the start of a line when it is saying where it has got
# to, rather than what it found.
PROGRESS = ("Compiling", "Checking", "Building", "Running", "Finished", "Updating")

SEVERITY = {"error": "error", "warning": "warning", "note": "info", "help": "hint"}

# The panel's own keys. A panel is handed the keystrokes that would otherwise
# have changed the text — which in a read-only buffer are going spare — so
# plain letters are free and nothing anybody knows is taken: Ctrl-P is still
# the palette, the arrows still move, Ctrl-W still closes the tab.
RUNS = [("c", "check"), ("b", "build"), ("t", "test"), ("l", "clippy")]


class Editor:
    """The half of the conversation that goes back to textfold."""

    def __init__(self, out):
        self.out = out
        # stdout is written from the cargo threads as well as the main one, and
        # a half-written frame would desynchronise the stream for good.
        self.lock = threading.Lock()

    def send(self, message):
        body = json.dumps(message).encode("utf-8")
        with self.lock:
            self.out.write(b"Content-Length: %d\r\n\r\n" % len(body))
            self.out.write(body)
            self.out.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def answer(self, request_id, result=None):
        self.send({"jsonrpc": "2.0", "id": request_id, "result": result})

    def refuse(self, request_id, message):
        self.send({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32000, "message": message},
        })

    def ask(self, request_id, method, params):
        """A question for the person, sent with an id so an answer comes back.

        The reply arrives later, on the ordinary loop — the plugin does not sit
        and wait for somebody to make up their mind.
        """
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})

    def say(self, text, kind=""):
        self.notify("status/say", {"text": text, "kind": kind})

    def open(self, path, line, column):
        self.notify("open", {"path": path, "line": line, "column": column})

    def panel(self, lines):
        self.notify("panel/set", {"panel": PANEL, "lines": lines})

    def diagnostics(self, items):
        """Everything this plugin currently thinks is wrong, all at once.

        A plugin's problems replace its own previous ones and leave everybody
        else's alone, so this cannot clear what rust-analyzer put there.
        """
        self.notify("diagnostics/set", {"items": items})


def read_message(stream):
    """One framed message, or None when the editor has gone."""
    length = None
    while True:
        line = stream.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1])
    if length is None:
        return None
    return json.loads(stream.read(length))


class Busy(Exception):
    """Something the person asked for that cannot happen, and why."""


class Cargo:
    """What this plugin remembers, which is the whole reason it is a plugin."""

    def __init__(self, editor, root):
        self.editor = editor
        self.root = root
        self.running = None
        self.what = ""
        self.problems = []
        # Lines cargo printed that were not problems: its progress, and for
        # `cargo test` the test results, which are the part somebody wanted.
        self.output = []
        self.showing = False
        self.show_output = False
        # Off by default. rust-analyzer is already putting the compiler's
        # errors in the margin — see the note at the top of this file.
        self.in_margin = False

    # ---- running it ----

    def start(self, what, args, problems=True):
        if self.running is not None:
            raise Busy(f"cargo {self.what} is still going — x stops it")
        try:
            self.running = subprocess.Popen(
                ["cargo"] + args + (["--message-format=json"] if problems else []),
                cwd=self.root,
                # A target directory of our own. rust-analyzer is very likely
                # running its own cargo in the ordinary one, and two cargos
                # sharing a target directory take turns on the lock and tread
                # on each other's fingerprints — which shows up as a build that
                # says "Finished" and reports none of the errors that are
                # plainly there. Costs disk, saves an afternoon.
                env={**os.environ,
                     "CARGO_TERM_COLOR": "never",
                     "CARGO_TARGET_DIR": os.path.join(self.root, "target", "textfold")},
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                stdin=subprocess.DEVNULL,
            )
        except FileNotFoundError:
            raise Busy("cargo is not installed")
        self.what = what
        self.problems = []
        self.output = []
        self.mirror()
        self.editor.say(f"cargo {what}…")
        self.report()

        process = self.running
        threading.Thread(target=self._read_problems, args=(process,), daemon=True).start()
        threading.Thread(target=self._watch, args=(process, what), daemon=True).start()

    def stop(self):
        process, what = self.running, self.what
        if process is None:
            raise Busy("nothing is running")
        self.running = None
        process.kill()
        self.editor.say(f"stopped cargo {what}")
        self.report()

    def _read_problems(self, process):
        """cargo's JSON, turned into problems as each one arrives.

        Not at the end. The first error in a two-minute build is in the panel a
        second after the compiler found it, which is the whole difference
        between this and running cargo in a shell next door.
        """
        for raw in process.stdout:
            try:
                message = json.loads(raw)
            except ValueError:
                # `cargo test` prints its results here, in between the JSON.
                self.output.append(raw.decode("utf-8", "replace").rstrip("\n"))
                continue
            if message.get("reason") != "compiler-message":
                continue
            found = self._as_problems(message.get("message") or {})
            if not found or self.running is not process:
                continue
            self.problems.extend(found)
            self.mirror()
            self.report()

    def _as_problems(self, message):
        level = SEVERITY.get(message.get("level"))
        if level is None:
            return []
        text = message.get("message") or ""
        code = (message.get("code") or {}).get("code")
        out = []
        for span in message.get("spans") or []:
            if not span.get("is_primary"):
                continue
            out.append({
                "path": os.path.join(self.root, span["file_name"]),
                # cargo counts lines and columns from one; textfold counts from
                # zero, as it does everywhere.
                "line": span["line_start"] - 1,
                "column": span["column_start"] - 1,
                "end_line": span["line_end"] - 1,
                "end_column": span["column_end"] - 1,
                "severity": level,
                "message": text if not span.get("label") else f"{text}: {span['label']}",
                "code": code,
                "source": "cargo",
            })
        return out

    def _watch(self, process, what):
        """cargo's progress, and how it went. Nothing waits on any of this."""
        for raw in process.stderr:
            line = raw.decode("utf-8", "replace").rstrip("\n")
            self.output.append(line)
            if line.strip().startswith(PROGRESS):
                self.editor.say(line.strip())
            self.report()
        code = process.wait()

        # A run that was stopped on purpose is not a run that failed.
        if self.running is not process:
            return
        self.running = None
        found = len(self.problems)
        if code == 0:
            self.editor.say(f"cargo {what}: ok", kind="good")
        elif found:
            self.editor.say(
                f"cargo {what}: {found} problem{'' if found == 1 else 's'}", kind="bad"
            )
        else:
            self.editor.say(f"cargo {what} failed", kind="bad")
        self.report()

    # ---- the panel ----

    def mirror(self):
        """Put the problems in the margin, or take them back out of it."""
        self.editor.diagnostics(self.problems if self.in_margin else [])

    def report(self):
        """The panel, whole. Sent every time anything changes.

        A panel is tens of lines and this is a few times a second at worst, so
        there is nothing to diff and nothing that can fall out of step with
        what is on the screen.
        """
        if not self.showing:
            return

        state = "running" if self.running else ("done" if self.what else "")
        lines = [
            {"spans": [
                {"text": f"cargo {self.what}" if self.what else "cargo", "style": "keyword"},
                {"text": "   " + state, "style": "string" if self.running else "muted"},
                {"text": f"   {len(self.problems)} found" if self.problems else "",
                 "style": "error"},
            ]},
            {"spans": [
                {"text": "  ".join(f"{key} {name}" for key, name in RUNS), "style": "muted"},
                {"text": "   x stop   o output   d margin   ↵ go", "style": "muted"},
            ]},
            "",
        ]

        if not self.problems:
            lines += [
                {"spans": [{"text": "  nothing to report" if self.what else "  press c to check",
                            "style": "muted"}]},
                "",
            ]

        # Grouped by file, in the order the compiler found them, so the panel
        # reads the way the build read.
        seen = []
        for p in self.problems:
            if p["path"] not in seen:
                seen.append(p["path"])
        for path in seen:
            lines.append({"spans": [
                {"text": os.path.relpath(path, self.root), "style": "type"},
            ]})
            for p in self.problems:
                if p["path"] != path:
                    continue
                lines.append({"spans": [
                    {"text": "  " + f"{p['line'] + 1}:{p['column'] + 1}".ljust(9),
                     "style": "muted"},
                    {"text": p["severity"].ljust(8),
                     "style": "error" if p["severity"] == "error" else "warning"},
                    # The clickable part. What goes in `action` is the plugin's
                    # own business — the editor hands it straight back and
                    # never looks inside it.
                    {"text": p["message"].split("\n")[0],
                     "style": "string",
                     "action": f"go:{path}:{p['line']}:{p['column']}"},
                ]})
            lines.append("")

        if self.show_output and self.output:
            lines.append({"spans": [{"text": "── what cargo said ──", "style": "comment"}]})
            # The tail of it. A panel is for reading, not for archiving, and a
            # dependency tree's worth of "Compiling" is not worth scrolling.
            for line in self.output[-200:]:
                lines.append({"spans": [{"text": "  " + line, "style": "comment"}]})
        self.editor.panel(lines)

    def key(self, pressed):
        """A key somebody pressed in the panel."""
        for key, name in RUNS:
            if pressed == key:
                return self.start(name, [name])
        if pressed == "x":
            return self.stop()
        if pressed == "o":
            self.show_output = not self.show_output
            return self.report()
        if pressed == "d":
            self.in_margin = not self.in_margin
            self.mirror()
            self.editor.say(
                "cargo problems are in the margin as well" if self.in_margin
                else "cargo problems are in this panel only"
            )
            return self.report()
        # Anything else is a key this panel has nothing to say about. Not an
        # error: it was going spare either way.


COMMANDS = {
    "cargo/check": ("check", ["check"]),
    "cargo/build": ("build", ["build"]),
    "cargo/test": ("test", ["test"]),
    "cargo/clippy": ("clippy", ["clippy"]),
}


def main():
    editor = Editor(sys.stdout.buffer)
    cargo = None
    # Questions this plugin has asked the person, by the id it asked under, so
    # an answer arriving later knows what it is an answer to. The same trick
    # the editor plays on us, played back.
    asked = {}
    next_id = [1000]

    def ask(what, method, params):
        next_id[0] += 1
        asked[next_id[0]] = what
        editor.ask(next_id[0], method, params)

    while True:
        message = read_message(sys.stdin.buffer)
        if message is None:
            return
        method = message.get("method")
        request_id = message.get("id")
        params = message.get("params") or {}

        # An answer to something we asked.
        if method is None and request_id in asked:
            what = asked.pop(request_id)
            answer = message.get("result")
            try:
                if isinstance(what, tuple) and what[0] == "menu":
                    chosen, where = answer, what[1]
                    if chosen == "go" and where and where.startswith("go:"):
                        path, line, column = where[3:].rsplit(":", 2)
                        editor.open(path, int(line), int(column))
                    elif chosen:
                        cargo.key(chosen)
                    continue
                if what == "clean" and answer is True:
                    cargo.start("clean", ["clean"], problems=False)
                elif what == "problem" and answer:
                    for p in cargo.problems:
                        if answer == f"{p['path']}:{p['line']}":
                            editor.open(p["path"], p["line"], p["column"])
                            break
            except Busy as why:
                editor.say(str(why), kind="bad")
            continue

        if method == "panel/opened":
            cargo.showing = True
            cargo.report()
        elif method == "panel/closed":
            cargo.showing = False
        elif method == "panel/context":
            # A right click in the panel. The editor hands over where it
            # landed and whatever this plugin had marked there; what to put in
            # the menu is the plugin's business, and the menu opens where the
            # pointer is rather than in the middle of the screen.
            rows = [{"label": f"cargo {name}", "value": key} for key, name in RUNS]
            where = params.get("action")
            if where:
                rows = [{"label": "Go to it", "value": "go"}, None] + rows
            if cargo.running:
                rows += [None, {"label": f"Stop cargo {cargo.what}", "value": "x"}]
            ask(("menu", where), "menu", {"items": rows})
        elif method == "panel/action":
            what = params.get("action") or ""
            if what.startswith("go:"):
                path, line, column = what[3:].rsplit(":", 2)
                editor.open(path, int(line), int(column))
        elif method == "panel/key":
            try:
                cargo.key(params.get("key") or "")
            except Busy as why:
                editor.say(str(why), kind="bad")
        elif method == "initialize":
            cargo = Cargo(editor, params.get("root") or ".")
            editor.answer(request_id, {"capabilities": {"commands": True, "panel": True}})
        elif method == "command/run":
            name = params.get("id")
            try:
                if name == "cargo/stop":
                    cargo.stop()
                elif name == "cargo/clean":
                    # Throwing away a build is worth asking about first, and
                    # the box it is asked in is the editor's own.
                    ask("clean", "confirm", {"text": "Throw away the build and start again?"})
                elif name == "cargo/problems":
                    if not cargo.problems:
                        raise Busy("nothing to go to — nothing has been found yet")
                    ask("problem", "pick", {
                        "title": "Problems cargo found",
                        "items": [{
                            "label": p["message"].split("\n")[0][:70],
                            "value": f"{p['path']}:{p['line']}",
                            "detail": f"{os.path.basename(p['path'])}:{p['line'] + 1}",
                            "tag": p["severity"],
                        } for p in cargo.problems],
                    })
                elif name in COMMANDS:
                    what, args = COMMANDS[name]
                    cargo.start(what, args)
                else:
                    raise Busy(f"no such command: {name}")
                # Accepted, not finished. What happens next arrives as status
                # lines and panel updates, and the editor is free meanwhile.
                editor.answer(request_id)
            except Busy as why:
                editor.refuse(request_id, str(why))
        elif method == "exit":
            return
        elif request_id is not None:
            # Every request must be answered, including the ones we do not
            # understand, or whoever asked waits forever.
            editor.refuse(request_id, f"cargo plugin has no {method}")


if __name__ == "__main__":
    main()
