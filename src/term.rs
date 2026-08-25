//! The few things textfold asks of the terminal itself, and the few it asks
//! of the machine around it.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use base64::Engine;

/// Put text on the system clipboard.
///
/// Two ways at once, because neither one works everywhere:
///
///   * OSC 52, which asks the terminal to do it. This is the one that works
///     over `ssh` — a copy made in an editor running on a server ends up on
///     the clipboard of the machine in front of you, which no amount of
///     talking to the local display server can manage. Not every terminal
///     implements it, and several that do have it turned off by default or
///     put a prompt in front of it, because a program that can silently
///     rewrite your clipboard is a program that can rewrite a command you are
///     about to paste into a shell.
///
///   * The clipboard tool the desktop ships, if there is one and there is a
///     desktop to talk to. This is the one that always works locally and
///     never works remotely.
///
/// Doing both means a copy lands whichever of the two is available, and where
/// both are, they land the same text.
pub fn to_clipboard(text: &str) {
    if text.is_empty() {
        return;
    }
    osc52(text);
    if let Some(helper) = writer() {
        helper.write(text);
    }
}

/// What is on the system clipboard, if we can find out without leaving the
/// terminal.
///
/// There is no OSC 52 half to this. Asking a terminal to read the clipboard
/// back means writing a query and then reading the answer out of the same
/// stream the keyboard arrives on — which the input thread is already sitting
/// on, and which a terminal that does not implement it will never answer, so
/// the wait is for a reply that is not coming. The desktop's own tool is
/// asked instead, and where there is none the editor's own clipboard stands
/// in, which is what Ctrl-C put there.
pub fn from_clipboard() -> Option<String> {
    let text = reader()?.read()?;
    (!text.is_empty()).then_some(text)
}

/// Ask the terminal to do it.
fn osc52(text: &str) {
    // Terminals stop reading these somewhere around 100KB, and a copy that is
    // silently truncated is worse than one that never happened. The desktop's
    // own tool has no such limit, so a large copy still gets there when one is
    // available.
    if text.len() > 74_000 {
        return;
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{encoded}\x07").ok();
    out.flush().ok();
}

/// One of the small programs a desktop ships for this, and how to run it.
#[derive(Clone, Copy)]
struct Helper {
    command: &'static str,
    args: &'static [&'static str],
}

impl Helper {
    /// Hand it the text on its standard input.
    ///
    /// The child is waited for on a thread of its own. Some of these — `wl-copy`
    /// most of all — stay alive holding the selection until somebody else
    /// takes it, so waiting here would stop the editor until the next time you
    /// copied anything in any program. Not waiting at all would leave a
    /// zombie behind every Ctrl-C.
    fn write(&self, text: &str) {
        let child = Command::new(self.command)
            .args(self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return };
        if let Some(stdin) = child.stdin.take() {
            let mut stdin = stdin;
            stdin.write_all(text.as_bytes()).ok();
            // Dropped, and so closed, before the wait: a tool reading to end
            // of input never sees one otherwise.
            drop(stdin);
        }
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                child.wait().ok();
            })
            .ok();
    }

    /// Read what it prints, giving up rather than waiting for ever.
    fn read(&self) -> Option<String> {
        let out = Command::new(self.command)
            .args(self.args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

fn have(command: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

fn set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// The pair of tools to use, worked out once.
///
/// Once, because this is a `PATH` walk and a handful of environment lookups,
/// and because the answer cannot change while the editor is running: a
/// display server does not appear halfway through a session.
fn helpers() -> &'static (Option<Helper>, Option<Helper>) {
    static FOUND: OnceLock<(Option<Helper>, Option<Helper>)> = OnceLock::new();
    FOUND.get_or_init(|| {
        // Wayland first, then X11, because a Wayland session usually has an
        // X11 compatibility layer as well and the native one is the one that
        // works. Neither is worth trying without a display to talk to: `xclip`
        // with no `DISPLAY` blocks rather than failing.
        if set("WAYLAND_DISPLAY") && have("wl-copy") && have("wl-paste") {
            return (
                Some(Helper { command: "wl-copy", args: &[] }),
                // Without this, a copy made in another program comes back with
                // a newline `wl-copy` added and nobody asked for.
                Some(Helper { command: "wl-paste", args: &["--no-newline"] }),
            );
        }
        if set("DISPLAY") && have("xclip") {
            return (
                Some(Helper { command: "xclip", args: &["-selection", "clipboard"] }),
                Some(Helper { command: "xclip", args: &["-selection", "clipboard", "-o"] }),
            );
        }
        if set("DISPLAY") && have("xsel") {
            return (
                Some(Helper { command: "xsel", args: &["--clipboard", "--input"] }),
                Some(Helper { command: "xsel", args: &["--clipboard", "--output"] }),
            );
        }
        if have("pbcopy") && have("pbpaste") {
            return (
                Some(Helper { command: "pbcopy", args: &[] }),
                Some(Helper { command: "pbpaste", args: &[] }),
            );
        }
        // Windows Subsystem for Linux, where the Windows clipboard is the one
        // that matters. `clip.exe` can only be written to.
        if have("clip.exe") {
            return (Some(Helper { command: "clip.exe", args: &[] }), None);
        }
        if have("termux-clipboard-set") {
            return (
                Some(Helper { command: "termux-clipboard-set", args: &[] }),
                have("termux-clipboard-get")
                    .then_some(Helper { command: "termux-clipboard-get", args: &[] }),
            );
        }
        (None, None)
    })
}

fn writer() -> Option<Helper> {
    helpers().0
}

fn reader() -> Option<Helper> {
    helpers().1
}

/// What to tell somebody whose copy did not reach the rest of their desktop.
pub fn clipboard_story() -> String {
    match (writer().map(|h| h.command), set("TMUX")) {
        (Some(tool), _) => format!("copying through {tool} and OSC 52"),
        (None, true) => {
            "copying through OSC 52 only — tmux needs `set -g set-clipboard on`".into()
        }
        (None, false) => {
            "copying through OSC 52 only — install wl-clipboard or xclip for a local clipboard"
                .into()
        }
    }
}
