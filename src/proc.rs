//! What else is running on this machine.
//!
//! Here for one question: which program to attach a debugger to. A debugger
//! that can only run a program it started itself is half a debugger — the
//! interesting bugs are very often in something that has been up for hours, a
//! server holding a connection or a simulation four hours in, and the whole
//! point of attaching is that you do not have to reproduce that from the
//! beginning.
//!
//! Answering "which one" is the awkward part, and it is awkward in a way that
//! is nobody's fault: a process id is a number that means nothing to a person
//! and is different every time. So the list is the interface — the same shape
//! as every other list in textfold — and everything here exists to make the
//! right row easy to find in it: the whole command line, so three copies of
//! the same program can be told apart; the executable, so the one belonging to
//! the project you are in can be offered first; and the order, newest first,
//! because the thing you started a moment ago is what you are nearly always
//! reaching for.
//!
//! **Only your own.** Attaching to somebody else's process needs privileges
//! this editor does not have and should not want, and a list mostly full of
//! rows that cannot be chosen is a worse list than a short one.

use std::path::{Path, PathBuf};

/// One program that is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// The program's own name, short: `wordcount`.
    pub name: String,
    /// The whole command line, arguments and all. What tells three copies of
    /// the same program apart, which is the case a list like this exists for.
    pub command: String,
    /// The file it is running, where that can be read.
    ///
    /// Two jobs. It says which processes belong to the project you are in, so
    /// they can be offered first; and it is what a debugger is told to load
    /// symbols from, which is the difference between a stack of names and a
    /// stack of addresses.
    pub program: Option<PathBuf>,
    /// When it started, in whatever the machine counts in. Never shown, only
    /// compared.
    started: u64,
}

/// Every process worth offering, newest first.
pub fn running() -> Vec<Process> {
    let mut found = from_proc().unwrap_or_else(from_ps);
    // Newest first. The thing somebody started a moment ago is what they are
    // nearly always reaching for, and the machine's own daemons — up since
    // boot, and none of anybody's business here — fall to the bottom without
    // having to be named in a list of exceptions that would be wrong on the
    // next machine.
    found.sort_by(|a, b| b.started.cmp(&a.started).then(b.pid.cmp(&a.pid)));
    found
}

/// Whether this process is one of `root`'s — the project's own binary rather
/// than something else that happens to be up.
impl Process {
    pub fn is_inside(&self, root: &Path) -> bool {
        self.program
            .as_deref()
            .is_some_and(|path| path.starts_with(root))
    }
}

/// Everything running, read from `/proc`.
///
/// `None` where there is no `/proc` to read, which is every machine that is
/// not Linux — see [`from_ps`], which is slower and works anywhere.
fn from_proc() -> Option<Vec<Process>> {
    let dir = std::fs::read_dir("/proc").ok()?;
    let mine = own_uid();
    let ours = std::process::id();
    let mut found = Vec::new();
    for entry in dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        // Not the editor itself. Attaching a debugger to the thing drawing the
        // debugger is a joke that stops the screen.
        if pid == ours {
            continue;
        }
        let at = entry.path();
        if !owned_by(&at, mine) {
            continue;
        }
        // A kernel thread has no command line at all, and there is nothing
        // there for a debugger to attach to. Nor has a process that ended
        // between listing the directory and reading it, which is ordinary on a
        // busy machine and is a row to skip rather than a reason to abandon
        // the whole list.
        let Ok(raw) = std::fs::read(at.join("cmdline")) else {
            continue;
        };
        let command = from_argv(&raw);
        if command.is_empty() {
            continue;
        }
        let name = std::fs::read_to_string(at.join("comm"))
            .map(|line| line.trim().to_string())
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| command.split_whitespace().next().unwrap_or("").to_string());
        found.push(Process {
            pid,
            name,
            command,
            // Readable only for our own processes, which is all of these.
            program: std::fs::read_link(at.join("exe")).ok(),
            started: started_at(&at.join("stat")).unwrap_or(0),
        });
    }
    Some(found)
}

/// The same, out of `ps`, for a machine with no `/proc`.
///
/// Less to work with — no executable path, so nothing can be offered first for
/// being part of this project — but a list of what is running is most of the
/// value and `ps` is on every Unix there has ever been.
fn from_ps() -> Vec<Process> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-u", &own_uid().to_string(), "-o", "pid=,comm=,args="])
        .output()
    else {
        return Vec::new();
    };
    let ours = std::process::id();
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let pid = pid.trim().parse::<u32>().ok()?;
            let (name, command) = rest.trim().split_once(char::is_whitespace)?;
            (pid != ours).then(|| Process {
                pid,
                name: Path::new(name)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| name.to_string()),
                command: command.trim().to_string(),
                program: None,
                // Nothing to sort by, so `ps`'s own order stands — which is
                // near enough to oldest first, and is reversed to match.
                started: 0,
            })
        })
        .collect()
}

/// A `cmdline`, which is the arguments run together with NULs between them.
///
/// The trailing one is dropped rather than becoming an empty last argument,
/// and an embedded newline becomes a space: this ends up on one row of a list,
/// and a command line that is two rows is a list that has stopped lining up.
fn from_argv(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .split('\0')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['\n', '\r', '\t'], " ")
        .trim()
        .to_string()
}

/// Field 22 of `/proc/N/stat`, which is when the process started.
///
/// Counted from the start of the line only after the last `)`, because field
/// two is the program's own name in brackets and a program is perfectly
/// entitled to be called `my ) program`. Every parser that counts from the
/// beginning is wrong about that one, and it is the sort of wrong that shows
/// up once a year on somebody else's machine.
fn started_at(stat: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(stat).ok()?;
    let after = &text[text.rfind(')')? + 1..];
    // What follows the name is field three onwards — state, ppid, pgrp, … —
    // so field twenty-two is the twentieth of them.
    after.split_whitespace().nth(19)?.parse().ok()
}

fn own_uid() -> u32 {
    #[cfg(unix)]
    {
        // Safe: it reads a number out of the kernel and cannot fail.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Whether a `/proc` entry belongs to us. Anything else cannot be attached to
/// without privileges this editor has not got.
fn owned_by(at: &Path, uid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(at).is_ok_and(|meta| meta.uid() == uid)
    }
    #[cfg(not(unix))]
    {
        let _ = (at, uid);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_is_its_arguments_with_the_nuls_taken_out() {
        assert_eq!(from_argv(b"cc\0-g\0-o\0main\0main.c\0"), "cc -g -o main main.c");
        // A kernel thread has nothing at all, and is not something to attach
        // to.
        assert_eq!(from_argv(b""), "");
        assert_eq!(from_argv(b"\0\0"), "");
    }

    #[test]
    fn a_command_line_stays_one_line() {
        // A program can be run with a newline in an argument, and a row of a
        // list that is secretly two rows is a list that stops lining up.
        assert_eq!(from_argv(b"sh\0-c\0echo one\ntwo\0"), "sh -c echo one two");
    }

    #[test]
    fn when_a_process_started_is_read_past_its_name_rather_than_through_it() {
        // Field two is the program's own name in brackets, and a program is
        // entitled to be called `my ) program`. Counting fields from the
        // beginning gets this wrong once a year, on somebody else's machine.
        let dir = std::env::temp_dir().join(format!("textfold-stat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a place to work");
        let stat = dir.join("stat");
        // Every field after the state written as its own number, so what comes
        // back says which field it was: `starttime` is the twenty-second, and
        // a parser that miscounts by one answers 21 or 23 rather than looking
        // right.
        let rest: Vec<String> = (4..=52).map(|n| n.to_string()).collect();
        std::fs::write(&stat, format!("42 (my ) program) S {}\n", rest.join(" ")))
            .expect("written");
        assert_eq!(started_at(&stat), Some(22));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn what_is_running_includes_something_we_started_and_never_ourselves() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep is on every machine there is");
        let found = running();
        let ours = found.iter().find(|p| p.pid == child.id());
        assert!(
            ours.is_some_and(|p| p.command.contains("sleep")),
            "a process we started a moment ago was not in the list"
        );
        // Newest first, so the thing started a moment ago is near the top
        // rather than under two hundred daemons that have been up since boot.
        if let Some(at) = found.iter().position(|p| p.pid == child.id()) {
            assert!(at < 10, "it was {at} rows down");
        }
        assert!(
            !found.iter().any(|p| p.pid == std::process::id()),
            "the editor offered to attach a debugger to itself"
        );
        child.kill().ok();
        child.wait().ok();
    }
}
