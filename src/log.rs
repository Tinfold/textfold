//! The log: what textfold and everything it starts have been saying.
//!
//! One file, appended to, in the same place across every session. A terminal
//! editor cannot print anything — the screen is the editor, and the terminal
//! under it belongs to whoever ran us — so the status line is the only place
//! textfold has ever had to say anything, and the status line is a place a
//! sentence goes for four seconds and is gone. What went wrong with a language
//! server that would not start, or a plugin that installed half of itself, was
//! written into that four seconds and then nowhere.
//!
//! So everything that gets said, gets written down here as well. The `logs`
//! command opens the file, which is an ordinary file in an ordinary buffer:
//! searchable, and kept up to date while it is open by the same thing that
//! notices any other file changing underneath you.
//!
//! One file for everybody rather than one each, because what a person is doing
//! when they come looking is working out why something did not happen, and the
//! answer is as often in the other program's half of the conversation as in
//! ours. Which is also why every line is stamped with the time: two programs'
//! lines interleaved are only worth having if you can see which came first,
//! and "it went wrong at about ten past" is how anybody actually looks.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// How big the file is allowed to get before the start of it is dropped, and
/// how much is kept when that happens.
///
/// A log nobody ever trims is a log that is eventually the largest file in the
/// state directory, and none of what makes it large is ever read: what is
/// worth having is the recent end. Trimmed once, at startup, so that nothing
/// in the middle of a session ever has to wait for it.
const KEEP_UNDER: u64 = 4 << 20;
const KEEP: usize = 1 << 20;

/// Write a line, under the name of whoever is saying it.
///
/// Never fails loudly. A log that cannot be written is a small loss; an editor
/// that stops to tell you about it is a larger one.
pub fn say(name: &str, line: &str) {
    // A test run is not a session. There is no state directory that belongs to
    // one, and writing to the real one would put a few hundred lines of
    // somebody else's editor into yours.
    if cfg!(test) {
        return;
    }
    static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
    let Ok(mut file) = FILE.lock() else { return };
    if file.is_none() {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        trim(&path);
        *file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
    }
    if let Some(file) = file.as_mut() {
        let now = clock();
        // A line at a time, so that two threads writing at once interleave
        // lines rather than halves of them, and so that a language server's
        // twelve-line backtrace is twelve stamped lines rather than one.
        for line in line.lines() {
            writeln!(file, "{now} [{name}] {line}").ok();
        }
    }
}

/// The time, as the left-hand column of a line: `14:03:22`.
///
/// Local time, which is the only kind anybody reads a log in. The date is not
/// on every line — it would be the same on all of them for hours at a stretch
/// — it is in the line [`started`] writes, which is where a session begins.
fn clock() -> String {
    jiff::Zoned::now().strftime("%H:%M:%S").to_string()
}

/// Say that a session has started, which is what tells one run from the one
/// before it in a file that outlives both.
pub fn started() {
    say(
        "textfold",
        &format!(
            "---- textfold {} started {} in {} ----",
            env!("CARGO_PKG_VERSION"),
            jiff::Zoned::now().strftime("%Y-%m-%d"),
            std::env::current_dir().unwrap_or_default().display()
        ),
    );
}

/// Somewhere else for the log to be, so that a test can have one of its own
/// instead of writing into the state directory of whoever is running them.
///
/// Tests share a process, so this is global to all of them. Nothing else reads
/// it, and the one test that sets it puts it back.
#[cfg(test)]
pub(crate) static INSTEAD: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where the log is, so that the status line and `--log-path` can tell you.
pub fn path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(instead) = INSTEAD.lock()
        && instead.is_some()
    {
        return instead.clone();
    }
    Some(
        dirs::state_dir()
            .or_else(dirs::cache_dir)?
            .join("textfold")
            .join("textfold.log"),
    )
}

/// Drop the front of the file if it has grown past [`KEEP_UNDER`], keeping
/// [`KEEP`] bytes of the end and cutting at a line break so that what is left
/// starts with a whole line.
fn trim(path: &std::path::Path) {
    let Ok(text) = std::fs::read(path) else { return };
    if text.len() as u64 <= KEEP_UNDER {
        return;
    }
    let from = text.len() - KEEP;
    let from = match text[from..].iter().position(|b| *b == b'\n') {
        Some(at) => from + at + 1,
        None => from,
    };
    std::fs::write(path, &text[from..]).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_that_has_grown_too_big_keeps_its_end() {
        // The end is the part anybody reads. What has to be true after a trim
        // is that the file still starts with a whole line — half of one, with
        // no time and no `[name]` on the front of it, reads as a line of
        // something else's output and sends whoever finds it looking in the
        // wrong place.
        let path = std::env::temp_dir().join(format!("textfold-log-{}", std::process::id()));
        let line = "14:03:22 [textfold] a line of the log\n";
        let mut text = String::new();
        while text.len() as u64 <= KEEP_UNDER + KEEP as u64 {
            text.push_str(line);
        }
        let was = text.len();
        std::fs::write(&path, &text).expect("written");

        trim(&path);
        let now = std::fs::read_to_string(&path).expect("read");
        assert!(now.len() < was, "nothing was dropped");
        assert!(now.len() <= KEEP, "more than the end was kept");
        assert!(now.starts_with(line), "it starts halfway through a line");
        assert!(now.ends_with(line), "the end is what was kept");

        // And a file that is not too big is left exactly as it is.
        std::fs::write(&path, "14:03:22 [textfold] short\n").expect("written");
        trim(&path);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "14:03:22 [textfold] short\n"
        );
        std::fs::remove_file(&path).ok();
    }
}
