//! The few things textfold asks of the terminal itself.

use std::io::Write;

use base64::Engine;

/// Put text on the system clipboard, by asking the terminal to do it.
///
/// This is OSC 52, and the reason it is worth the trouble is `ssh`: a copy
/// made in an editor running on a server ends up on the clipboard of the
/// machine in front of you, which no amount of talking to the local X server
/// can manage. A terminal that does not support it ignores the sequence, and
/// textfold's own clipboard still works — so this can only help.
pub fn to_clipboard(text: &str) {
    // Terminals stop reading these somewhere around 100KB, and a copy that is
    // silently truncated is worse than one that never happened.
    if text.is_empty() || text.len() > 74_000 {
        return;
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{encoded}\x07").ok();
    out.flush().ok();
}
