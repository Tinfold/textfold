//! Running a program on the file in front of you.
//!
//! The other half of "an editor with plugins", and the half that needs no
//! plugin runtime at all. A great deal of what people write plugins for
//! elsewhere is: run this program on my buffer, and do one of four things with
//! what it printed. That is a table, not code — see [`crate::plugin::Tool`] —
//! and this is what carries it out.
//!
//! On a thread, like everything else that talks to the world outside. A
//! formatter on a large file is a fifth of a second and a test run is a
//! minute, and neither of those is a length of time the cursor should stop
//! for. The answer comes back on the same channel the keyboard and the
//! language servers use, and is picked up between keystrokes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use crate::doc::{DocId, Severity};
use crate::plugin::Tool;

/// What a tool did, on its way back to the editor.
#[derive(Debug)]
pub struct Finished {
    /// Which tool, by the command name it answers to. Not the `&'static Tool`
    /// itself: the plugins can be rebuilt while a slow tool is still running,
    /// and a name still means something afterwards where a pointer into a
    /// table that has been replaced does not.
    pub tool: String,
    pub doc: DocId,
    /// Which version of the buffer it was given, so that output about text
    /// that has since been edited can be thrown away rather than applied.
    pub version: i32,
    /// Whether it thought it had succeeded.
    pub ok: bool,
    pub out: String,
    pub err: String,
}

/// Start one. Returns as soon as the thread is running.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    tool: &Tool,
    doc: DocId,
    version: i32,
    root: &Path,
    args: Vec<String>,
    env: Vec<(String, String)>,
    stdin: Option<String>,
    tx: Sender<crate::app::Event>,
) -> Result<(), String> {
    let mut command = Command::new(&tool.command);
    command
        .args(&args)
        .current_dir(root)
        .envs(env)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|e| {
        // The common case by far is that it is not installed, and saying so is
        // more use than an errno.
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("{} is not installed", tool.command)
        } else {
            format!("{}: {e}", tool.command)
        }
    })?;

    let name = tool.id.clone();
    std::thread::Builder::new()
        .name(format!("tool-{name}"))
        .spawn(move || {
            // The buffer goes in and the pipe is closed, or a tool that reads
            // to end of input waits for one that never comes.
            if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
                pipe.write_all(text.as_bytes()).ok();
                drop(pipe);
            }
            let done = child.wait_with_output();
            let finished = match done {
                Ok(out) => Finished {
                    tool: name,
                    doc,
                    version,
                    ok: out.status.success(),
                    out: String::from_utf8_lossy(&out.stdout).into_owned(),
                    err: String::from_utf8_lossy(&out.stderr).into_owned(),
                },
                Err(e) => Finished {
                    tool: name,
                    doc,
                    version,
                    ok: false,
                    out: String::new(),
                    err: e.to_string(),
                },
            };
            tx.send(crate::app::Event::Tool(Box::new(finished))).ok();
        })
        .map_err(|_| format!("could not run {}", tool.command))?;
    Ok(())
}

/// One problem a tool printed.
#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: PathBuf,
    /// Counted from zero, as everything inside the editor is. The line a tool
    /// prints is counted from one, and the difference is taken here.
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub message: String,
}

/// Read what a tool printed as a list of problems, using a pattern in the
/// shape every compiler-output parser since vi has used.
///
/// ```text
/// %f  the file            %l  the line        %c  the column
/// %t  a word saying how bad it is: error, warning, note
/// %m  the message, which is the rest of the line
/// %%  a per cent sign
/// ```
///
/// Everything else in the pattern is literal and has to be there. A line that
/// does not match is not a problem — tools print headers, summaries and blank
/// lines, and a parser that turned every one of those into a complaint in your
/// margin would be worse than no parser.
pub fn problems(pattern: &str, text: &str) -> Vec<Problem> {
    text.lines()
        .filter_map(|line| one_problem(pattern, line))
        .collect()
}

fn one_problem(pattern: &str, line: &str) -> Option<Problem> {
    let (mut file, mut row, mut column, mut message) = (None, None, None, None);
    let mut severity = Severity::Warning;

    let mut rest = line;
    let mut spec = pattern;
    while let Some(at) = spec.find('%') {
        // Whatever comes before the next directive has to be there, exactly.
        rest = rest.strip_prefix(&spec[..at])?;
        let what = spec.as_bytes().get(at + 1).copied()?;
        spec = &spec[at + 2..];

        if what == b'%' {
            rest = rest.strip_prefix('%')?;
            continue;
        }
        // A field runs up to whatever literal text comes after it in the
        // pattern, so `%f:%l` knows where the file name stops even on a path
        // with a colon in it: the *last* colon before the number is the one
        // that counts, and taking the shortest match gets there.
        let stop = spec.split('%').next().unwrap_or("");
        let (taken, left) = match what {
            b'm' => (rest, ""),
            _ if stop.is_empty() => (rest, ""),
            _ => {
                let end = rest.find(stop)?;
                (&rest[..end], &rest[end..])
            }
        };
        match what {
            b'f' => file = Some(PathBuf::from(taken.trim())),
            b'l' => row = Some(taken.trim().parse::<usize>().ok()?),
            b'c' => column = Some(taken.trim().parse::<usize>().ok()?),
            b'm' => message = Some(taken.trim().to_string()),
            b't' => {
                severity = match taken.trim().to_lowercase().chars().next() {
                    Some('e') => Severity::Error,
                    Some('w') => Severity::Warning,
                    _ => Severity::Info,
                }
            }
            // A directive nobody knows is a pattern nobody meant.
            _ => return None,
        }
        rest = left;
    }
    // And whatever the pattern ends with has to be there too.
    rest.strip_prefix(spec)?;

    Some(Problem {
        file: file?,
        line: row?.saturating_sub(1),
        column: column.unwrap_or(1).saturating_sub(1),
        message: message.filter(|m| !m.is_empty())?,
        severity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_a_compiler_would_print_is_read_as_a_problem() {
        let found = problems("%f:%l:%c: %m", "src/main.py:12:5: F401 'os' imported but unused");
        assert_eq!(
            found,
            [Problem {
                file: PathBuf::from("src/main.py"),
                // Counted from one on the way in and from zero on the way out.
                line: 11,
                column: 4,
                severity: Severity::Warning,
                message: "F401 'os' imported but unused".into(),
            }]
        );
    }

    #[test]
    fn how_bad_it_is_is_read_where_the_pattern_asks_for_it() {
        let found = problems(
            "%f:%l:%c: %t: %m",
            "a.sh:3:1: error: this is a bad idea\na.sh:4:1: note: and so is this",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].severity, Severity::Error);
        assert_eq!(found[1].severity, Severity::Info);
    }

    #[test]
    fn the_lines_that_are_not_problems_are_left_alone() {
        // Which is most of what a tool prints: headers, blank lines, and the
        // count at the end.
        let found = problems(
            "%f:%l:%c: %m",
            "Checking 3 files\n\nsrc/a.py:1:1: E501 line too long\nFound 1 error.\n",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message, "E501 line too long");
    }

    #[test]
    fn a_line_missing_the_part_the_pattern_needs_is_not_a_problem() {
        assert!(problems("%f:%l:%c: %m", "src/a.py:12: no column here").is_empty());
        assert!(problems("%f:%l:%c: %m", "src/a.py:12:5: ").is_empty());
    }

    #[test]
    fn a_column_the_pattern_does_not_ask_for_is_the_start_of_the_line() {
        let found = problems("%f:%l: %m", "src/a.py:12: something");
        assert_eq!(found[0].column, 0);
        assert_eq!(found[0].line, 11);
    }
}
