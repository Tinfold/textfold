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
    let sequence = format!("\x1b]52;c;{encoded}\x07");
    let mut out = std::io::stdout();
    out.write_all(wrapped(&sequence).as_bytes()).ok();
    out.flush().ok();
}

/// What is between textfold and the terminal that owns the clipboard.
///
/// A terminal multiplexer reads the escape sequences going past and acts on
/// the ones it understands, which for OSC 52 means eating it: the copy reaches
/// tmux and stops there. Both of them have a way of saying "this one is not
/// for you", and it is the only way a copy made inside `ssh` inside `tmux`
/// reaches the machine in front of you.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Through {
    Tmux,
    Screen,
}

fn through() -> Option<Through> {
    // `TMUX` first: inside tmux `TERM` is usually `screen` or `tmux`, and the
    // wrapping the two want is not the same.
    if set("TMUX") {
        return Some(Through::Tmux);
    }
    let term = std::env::var("TERM").unwrap_or_default();
    (term.starts_with("screen") && !term.starts_with("screen.")).then_some(Through::Screen)
}

/// One escape sequence, wrapped so that whatever is in the way passes it on.
fn wrapped(sequence: &str) -> String {
    wrap_as(through(), sequence)
}

fn wrap_as(through: Option<Through>, sequence: &str) -> String {
    match through {
        None => sequence.to_string(),
        // tmux takes the whole thing in one of its own, with every escape in
        // the payload doubled so that the inner sequence's terminator is not
        // read as the outer one's.
        Some(Through::Tmux) => {
            format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
        }
        // screen passes a device-control string through untouched, but will
        // not carry one longer than its own string buffer — so it goes in
        // pieces, each one a device-control string of its own. Nothing between
        // them is written, so the terminal sees one unbroken sequence.
        Some(Through::Screen) => {
            let mut out = String::new();
            let bytes = sequence.as_bytes();
            for chunk in bytes.chunks(400) {
                out.push_str("\x1bP");
                out.push_str(&String::from_utf8_lossy(chunk));
                out.push_str("\x1b\\");
            }
            out
        }
    }
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
    match (writer().map(|h| h.command), through()) {
        (Some(tool), _) => format!("copying through {tool} and OSC 52"),
        (None, Some(Through::Tmux)) => {
            "copying through OSC 52, wrapped for tmux — tmux also needs \
             `set -g set-clipboard on`"
                .into()
        }
        (None, Some(Through::Screen)) => "copying through OSC 52, wrapped for screen".into(),
        (None, None) => {
            "copying through OSC 52 only — install wl-clipboard or xclip for a local clipboard"
                .into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wrapping is worked out from the environment, which a test cannot
    /// change for one call without changing it for every thread at once. So
    /// the shapes are checked directly.
    #[test]
    fn tmux_gets_the_sequence_inside_one_of_its_own() {
        let out = wrap_as(Some(Through::Tmux), "\x1b]52;c;aGk=\x07");
        assert_eq!(out, "\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\");
    }

    #[test]
    fn screen_gets_it_in_pieces_that_join_back_up() {
        let sequence = format!("\x1b]52;c;{}\x07", "a".repeat(1000));
        let out = wrap_as(Some(Through::Screen), &sequence);
        let rebuilt: String = out
            .split("\x1b\\")
            .map(|piece| piece.strip_prefix("\x1bP").unwrap_or(piece))
            .collect();
        assert_eq!(rebuilt, sequence, "the pieces do not add back up");
        assert!(out.matches("\x1bP").count() > 1, "it was not split at all");
    }

    #[test]
    fn a_bare_terminal_gets_the_sequence_untouched() {
        let sequence = "\x1b]52;c;aGk=\x07";
        assert_eq!(wrap_as(None, sequence), sequence);
    }
}
