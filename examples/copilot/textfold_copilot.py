#!/usr/bin/env python3
"""GitHub Copilot, as a textfold plugin.

Two halves, and they are worth telling apart because only one of them talks to
Copilot proper:

  * **Inline suggestions** are real. This bridges to `copilot-language-server`,
    which GitHub ships and which speaks the same JSON-RPC framing textfold's
    plugins do — so the plugin is mostly a translator between two protocols
    that already agree about how to move JSON down a pipe. It needs you signed
    in (`copilot/sign-in`) and a Copilot subscription.

  * **Chat** is a panel, and its backend is whatever you point it at. Copilot's
    language server has no conversation API — it does inline completions and
    nothing else — so there is nothing here to bridge to. The panel runs the
    command in `chat.command` and shows what comes back, which means it works
    with `gh copilot`, with a local model, or with anything that reads a
    question on the command line.

What this plugin exists to demonstrate is the editor's side: a plugin that
draws *into your text* rather than beside it, and one that holds a
conversation in a buffer of its own.

Written against textfold's plugin protocol: JSON-RPC 2.0, Content-Length
framing, on stdin and stdout.
"""

import json
import os
import re
import subprocess
import sys
import threading

PANEL = "copilot/chat"

HERE = os.path.dirname(os.path.abspath(__file__))
SERVER = os.path.join(
    HERE, "node_modules", "@github", "copilot-language-server", "dist", "language-server.js"
)


# ---------------------------------------------------------------------------
# Framing. The same twenty lines at both ends of this file, because textfold
# and the Copilot server speak the same wire format.
# ---------------------------------------------------------------------------


def read_message(stream):
    """One framed message, or None when the other end has gone."""
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


class Wire:
    """One end of a JSON-RPC conversation."""

    def __init__(self, out):
        self.out = out
        self.lock = threading.Lock()
        self.next_id = 0
        self.waiting = {}

    def send(self, message):
        body = json.dumps(message).encode("utf-8")
        with self.lock:
            self.out.write(b"Content-Length: %d\r\n\r\n" % len(body))
            self.out.write(body)
            self.out.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, then=None):
        """Ask, and write down what the answer will mean.

        The same trick textfold plays on its plugins, played on the server:
        nothing here waits for a reply, and a reply that arrives after we have
        moved on is dropped by whoever asked rather than guessed at.
        """
        with self.lock:
            self.next_id += 1
            request_id = self.next_id
        if then is not None:
            self.waiting[request_id] = then
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        return request_id

    def answer(self, request_id, result=None):
        self.send({"jsonrpc": "2.0", "id": request_id, "result": result})

    def refuse(self, request_id, message):
        self.send({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32000, "message": message},
        })

    def claim(self, request_id):
        return self.waiting.pop(request_id, None)


class Editor(Wire):
    """textfold, from this plugin's side."""

    def say(self, text, kind=""):
        self.notify("status/say", {"text": text, "kind": kind})

    def hint(self, path, line, column, text):
        """Offer some text where the cursor is. Shown, not inserted."""
        self.notify("hint/set", {"path": path, "line": line, "column": column, "text": text})

    def clear_hint(self, path):
        self.notify("hint/set", {"path": path, "text": ""})

    def panel(self, lines):
        self.notify("panel/set", {"panel": PANEL, "lines": lines})


# ---------------------------------------------------------------------------
# The Copilot language server, and the buffers it is told about.
# ---------------------------------------------------------------------------


LANGUAGE_IDS = {
    "rust": "rust", "python": "python", "javascript": "javascript",
    "typescript": "typescript", "tsx": "typescriptreact", "go": "go",
    "c": "c", "cpp": "cpp", "csharp": "csharp", "java": "java",
    "bash": "shellscript", "json": "json", "toml": "toml", "yaml": "yaml",
    "markdown": "markdown", "html": "html", "css": "css",
}


def as_uri(path):
    return "file://" + path


class Copilot:
    """The half that is really Copilot."""

    def __init__(self, editor, root):
        self.editor = editor
        self.root = root
        self.server = None
        self.wire = None
        self.ready = False
        self.status = "starting"
        self.on = True
        # Our own copy of every buffer Copilot has been told about, and which
        # version of it that was. Kept because textfold sends what changed as
        # character offsets — the two numbers you can slice with — and because
        # the file has to be sent to Copilot whole anyway.
        self.text = {}
        self.language = {}
        self.version = {}
        # Which inline-completion request is the current one. An answer to an
        # older one is about a cursor that has since moved.
        self.asking = 0
        self.offer = None

    def start(self):
        if not os.path.exists(SERVER):
            self.status = "not installed"
            return self.editor.say(
                "copilot: run `npm install` in the plugin's directory first", kind="bad"
            )
        try:
            self.server = subprocess.Popen(
                ["node", SERVER, "--stdio"],
                cwd=self.root,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            )
        except FileNotFoundError:
            self.status = "no node"
            return self.editor.say("copilot: node is not installed", kind="bad")

        self.wire = Wire(self.server.stdin)
        threading.Thread(target=self._listen, daemon=True).start()
        self.wire.request(
            "initialize",
            {
                "processId": os.getpid(),
                "rootUri": as_uri(self.root),
                "capabilities": {"workspace": {"workspaceFolders": True}},
                "initializationOptions": {
                    "editorInfo": {"name": "textfold", "version": "0.1.0"},
                    "editorPluginInfo": {"name": "textfold-copilot", "version": "0.1.0"},
                },
            },
            then=self._started,
        )

    def _listen(self):
        """Everything the server says, on a thread of its own."""
        while True:
            message = read_message(self.server.stdout)
            if message is None:
                self.ready = False
                self.status = "stopped"
                return
            if "method" in message:
                # The server asks things of its client too. Everything it asks
                # must be answered, even the things we do not do, or it waits.
                if message.get("id") is not None:
                    self.wire.answer(message["id"], None)
                continue
            then = self.wire.claim(message.get("id"))
            if then is None:
                continue
            if "error" in message:
                self.editor.say(
                    f"copilot: {message['error'].get('message', 'something went wrong')}",
                    kind="bad",
                )
                continue
            then(message.get("result"))

    def _started(self, _result):
        self.wire.notify("initialized", {})
        self.ready = True
        # Everything textfold already told us about, told onward now.
        #
        # The server takes a few seconds to come up, and files are open before
        # then — textfold does exactly this for a plugin that starts late, and
        # a plugin that is itself a front end for something slower owes the
        # same to the thing behind it. Without this the first file you look at
        # is one Copilot has never heard of, and it says so.
        for path, text in self.text.items():
            self.wire.notify("textDocument/didOpen", {"textDocument": {
                "uri": as_uri(path),
                "languageId": self.language.get(path, "plaintext"),
                "version": self.version.get(path, 0),
                "text": text,
            }})
        self.check()

    def check(self, loud=False):
        def answer(result):
            self.status = (result or {}).get("status", "unknown")
            if self.status == "OK":
                user = (result or {}).get("user") or "your account"
                if loud:
                    self.editor.say(f"copilot: signed in as {user}", kind="good")
            elif loud:
                self.editor.say(
                    f"copilot: {self.status} — run copilot/sign-in", kind="bad"
                )
        if self.ready:
            self.wire.request("checkStatus", {}, then=answer)

    def sign_in(self):
        """The device flow: a code to type into a page in your browser."""
        if not self.ready:
            raise Busy("copilot is not running")

        def got(result):
            result = result or {}
            if result.get("status") in ("AlreadySignedIn", "OK"):
                self.status = "OK"
                return self.editor.say("copilot: already signed in", kind="good")
            code = result.get("userCode")
            url = result.get("verificationUri")
            if not code:
                return self.editor.say("copilot: no code came back", kind="bad")
            # The code goes in a buffer rather than the status line: it has to
            # be read carefully and typed somewhere else, and a status line is
            # gone in six seconds.
            self.editor.notify("buffer/show", {
                "name": "copilot sign-in",
                "focus": True,
                "text": (
                    f"Copilot sign-in\n\n"
                    f"  1. open  {url}\n"
                    f"  2. enter {code}\n\n"
                    f"Then run copilot/status here to check it took.\n"
                ),
            })
            self.editor.say(f"copilot: enter {code} at {url}")

        self.wire.request("signIn", {}, then=got)

    # ---- buffers ----

    def opened(self, path, language, version, text):
        self.text[path] = text
        self.language[path] = LANGUAGE_IDS.get(language.lower(), language.lower())
        self.version[path] = version
        if not self.ready:
            return
        self.wire.notify("textDocument/didOpen", {"textDocument": {
            "uri": as_uri(path), "languageId": self.language[path],
            "version": version, "text": text,
        }})

    def changed(self, path, version, changes):
        """Apply what textfold said changed to our own copy, then send it on.

        textfold sends character offsets into the text *as it was*, which is
        exactly what slicing wants — so keeping a mirror is three lines. It
        goes to Copilot as a whole document, which LSP allows and which is one
        fewer place to get an off-by-one wrong.
        """
        if path not in self.text:
            return
        text = self.text[path]
        for change in changes:
            text = text[: change["from"]] + change["text"] + text[change["to"]:]
        self.text[path] = text
        self.version[path] = version
        if self.ready:
            self.wire.notify("textDocument/didChange", {
                "textDocument": {"uri": as_uri(path), "version": version},
                "contentChanges": [{"text": text}],
            })

    def closed(self, path):
        self.text.pop(path, None)
        self.version.pop(path, None)
        if self.ready:
            self.wire.notify("textDocument/didClose",
                             {"textDocument": {"uri": as_uri(path)}})

    # ---- suggestions ----

    def suggest(self, path, line, column):
        """Ask Copilot what would go here, and offer whatever comes back."""
        if not (self.ready and self.on and self.status == "OK"):
            return
        if path not in self.text:
            return
        self.asking += 1
        mine = self.asking

        def answer(result):
            # An answer to a question about a cursor that has since moved is
            # about text nobody is looking at any more.
            if mine != self.asking:
                return
            items = (result or {}).get("items") or []
            if not items:
                self.offer = None
                return self.editor.clear_hint(path)
            text = items[0].get("insertText") or ""
            # What Copilot sends starts where the completion starts, which may
            # be before the cursor — the part already typed is trimmed so that
            # what is drawn is only what would be added.
            start = ((items[0].get("range") or {}).get("start") or {})
            if start.get("line") == line:
                typed = max(0, column - int(start.get("character", column)))
                text = text[typed:]
            self.offer = text
            if text.strip():
                self.editor.hint(path, line, column, text)
            else:
                self.editor.clear_hint(path)

        self.wire.request("textDocument/inlineCompletion", {
            "textDocument": {"uri": as_uri(path), "version": self.version.get(path, 0)},
            "position": {"line": line, "character": column},
            "context": {"triggerKind": 2},
            "formattingOptions": {"tabSize": 4, "insertSpaces": True},
        }, then=answer)

    def taken(self):
        """Tell Copilot its suggestion was used, which is how it learns."""
        if self.ready and self.offer is not None:
            self.wire.notify("workspace/executeCommand", {
                "command": "github.copilot.didAcceptCompletionItem", "arguments": [],
            })
        self.offer = None


class Busy(Exception):
    """Something asked for that cannot happen, and why."""


# ---------------------------------------------------------------------------
# The chat panel.
# ---------------------------------------------------------------------------


# What the Copilot CLI prints under its answer: what it spent, and how to pick
# the conversation up again. True, and not what anybody asked.
#
# Matched on shape and not just on the word, because the words are ordinary
# ones. The CLI lays the footer out as a column, so a label is followed by
# enough space to line the values up — which is what tells `Tokens     ↑ 24.7k`
# apart from a sentence that happens to begin "Tokens are a unit of text".
FOOTER = re.compile(r"^(?:Changes|Tokens|Resume|Total duration|Session)\s{2,}\S"
                    r"|^AI Credits\s+\S")


def trim(text):
    """An answer without the accounting underneath it.

    Taken off the end rather than searched for, so that a line like this
    appearing in the middle of an answer is left where it is.
    """
    lines = text.rstrip().split("\n")
    while lines and (not lines[-1].strip() or FOOTER.match(lines[-1])):
        lines.pop()
    return "\n".join(lines).strip()


class Chat:
    """A conversation in a buffer.

    Copilot's language server has no conversation API, so this runs whatever
    command it is pointed at and shows what comes back. The interesting part is
    not the backend: it is that a chat window in this editor is a panel, a
    prompt and a handful of keys, and needs nothing else.
    """

    def __init__(self, editor, command):
        self.editor = editor
        self.command = command
        self.showing = False
        self.turns = []
        self.thinking = False

    def report(self):
        if not self.showing:
            return
        lines = [
            {"spans": [
                {"text": "Copilot chat", "style": "keyword"},
                {"text": "    i ask   c clear   q close", "style": "muted"},
            ]},
            "",
        ]
        if not self.turns:
            lines += [
                {"spans": [{"text": "  press i to ask something", "style": "muted"}]},
                "",
                {"spans": [{"text": f"  answered by: {' '.join(self.command)}",
                            "style": "comment"}]},
            ]
        for who, what in self.turns:
            lines.append({"spans": [
                {"text": "you" if who == "you" else "copilot",
                 "style": "type" if who == "you" else "keyword"},
            ]})
            for line in what.split("\n"):
                lines.append({"spans": [
                    {"text": "  " + line, "style": "string" if who == "you" else "comment"},
                ]})
            lines.append("")
        if self.thinking:
            lines.append({"spans": [{"text": "  thinking…", "style": "muted"}]})
        self.editor.panel(lines)

    def ask(self, question):
        if self.thinking:
            raise Busy("still waiting on the last one")
        self.turns.append(("you", question))
        self.thinking = True
        self.report()
        threading.Thread(target=self._run, args=(question,), daemon=True).start()

    def _run(self, question):
        try:
            done = subprocess.run(
                self.command + [question],
                capture_output=True, text=True, timeout=120,
            )
            said = trim(done.stdout or "") or trim(done.stderr or "")
            if not said:
                said = "(it said nothing)"
        except FileNotFoundError:
            said = (f"{self.command[0]} is not installed.\n\n"
                    "The Copilot CLI answers this panel. Install it, or point\n"
                    "`host.settings.chat.command` in this plugin's manifest at\n"
                    "anything else that takes a question as its last argument.")
        except subprocess.TimeoutExpired:
            said = "(it took too long)"
        self.turns.append(("copilot", said))
        self.thinking = False
        self.report()

    def key(self, pressed, ask):
        if pressed == "i":
            return ask("chat", "prompt", {"title": "Ask Copilot"})
        if pressed == "c":
            self.turns = []
            return self.report()


# ---------------------------------------------------------------------------


def main():
    editor = Editor(sys.stdout.buffer)
    copilot = None
    chat = None
    asked = {}
    next_id = [2000]

    def ask(what, method, params):
        next_id[0] += 1
        asked[next_id[0]] = what
        editor.send({"jsonrpc": "2.0", "id": next_id[0], "method": method, "params": params})

    while True:
        message = read_message(sys.stdin.buffer)
        if message is None:
            return
        method = message.get("method")
        request_id = message.get("id")
        params = message.get("params") or {}

        # An answer to something we asked the person.
        if method is None and request_id in asked:
            what = asked.pop(request_id)
            answer = message.get("result")
            if what == "chat" and answer:
                try:
                    chat.ask(answer)
                except Busy as why:
                    editor.say(str(why), kind="bad")
            continue

        try:
            if method == "initialize":
                root = params.get("root") or "."
                settings = params.get("settings") or {}
                command = (settings.get("chat") or {}).get("command") \
                    or ["copilot", "-p"]
                copilot = Copilot(editor, root)
                chat = Chat(editor, command)
                editor.answer(request_id, {"capabilities": {"hints": True, "panel": True}})
                copilot.start()

            elif method == "buffer/opened":
                copilot.opened(params["path"], params.get("language", ""),
                               params.get("version", 0), params.get("text", ""))
            elif method == "buffer/changed":
                copilot.changed(params["path"], params.get("version", 0),
                                params.get("changes") or [])
            elif method == "buffer/closed":
                copilot.closed(params["path"])
            elif method == "selection/changed":
                copilot.suggest(params["path"], params["line"], params["column"])
            elif method == "hint/taken":
                copilot.taken()
            elif method == "hint/dropped":
                copilot.offer = None

            elif method == "panel/opened":
                chat.showing = True
                chat.report()
            elif method == "panel/closed":
                chat.showing = False
            elif method == "panel/key":
                chat.key(params.get("key") or "", ask)

            elif method == "command/run":
                name = params.get("id")
                if name == "copilot/sign-in":
                    copilot.sign_in()
                elif name == "copilot/status":
                    copilot.check(loud=True)
                elif name == "copilot/suggestions":
                    copilot.on = not copilot.on
                    editor.say(
                        "copilot suggestions are on" if copilot.on
                        else "copilot suggestions are off"
                    )
                else:
                    raise Busy(f"no such command: {name}")
                editor.answer(request_id)

            elif method == "exit":
                return
            elif request_id is not None:
                editor.refuse(request_id, f"copilot plugin has no {method}")

        except Busy as why:
            if request_id is not None:
                editor.refuse(request_id, str(why))
            else:
                editor.say(str(why), kind="bad")
        except Exception as e:  # noqa: BLE001
            # A plugin that dies takes its panel and its suggestions with it,
            # and textfold will restart it three times and then leave it alone.
            # Saying what went wrong is more use than falling over.
            if request_id is not None:
                editor.refuse(request_id, f"{type(e).__name__}: {e}")
            else:
                editor.say(f"copilot: {type(e).__name__}: {e}", kind="bad")


if __name__ == "__main__":
    main()
