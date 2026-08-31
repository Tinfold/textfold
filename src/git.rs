//! What git knows about the file you are looking at.
//!
//! Mostly two things, which are the two an editor can show without getting in
//! the way: which branch you are on, and which lines you have touched since
//! the last commit. Both are read rather than written — textfold does not
//! commit, branch, merge or stash, and it never will.
//!
//! The exceptions are the two that are about the file in front of you rather
//! than about the repository, where going to another window means finding your
//! place again afterwards: putting one stretch of a file back as it was
//! committed, which is an ordinary edit to a buffer and touches nothing on
//! disk, and putting one stretch of it into the index, which is the one thing
//! in here that changes a repository — and it does that by handing git the
//! patch and letting git do it.
//!
//! There is no library here. The branch is read straight out of `.git`, which
//! is a text file and cheaper than starting a process; the committed text of
//! a file comes from `git show`, which is the one thing worth a subprocess
//! because working out what `HEAD:some/path` resolves to means reading the
//! object store, and reading the object store means implementing git.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::doc::{Document, DocId};

/// A repository, found by walking up from a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    pub root: PathBuf,
    /// `.git` itself, which for a worktree or a submodule is not `root/.git`
    /// but wherever that file points.
    dir: PathBuf,
}

impl Repo {
    /// The repository a path is in, if it is in one.
    pub fn find(from: &Path) -> Option<Self> {
        let start = if from.is_dir() {
            from
        } else {
            from.parent()?
        };
        for root in start.ancestors() {
            let marker = root.join(".git");
            if marker.is_dir() {
                return Some(Self {
                    root: root.to_path_buf(),
                    dir: marker,
                });
            }
            // A worktree or a submodule has a `.git` file holding the path of
            // the real directory. Where that path is relative, it is relative
            // to the file it was written in.
            if marker.is_file()
                && let Ok(text) = std::fs::read_to_string(&marker)
                && let Some(rest) = text.trim().strip_prefix("gitdir:")
            {
                let dir = PathBuf::from(rest.trim());
                let dir = if dir.is_absolute() { dir } else { root.join(dir) };
                return Some(Self {
                    root: root.to_path_buf(),
                    dir,
                });
            }
        }
        None
    }

    /// What to call where you are: the branch, or a short commit id when the
    /// head is detached.
    ///
    /// Read out of `.git/HEAD`, which is one line of text. Starting `git` to
    /// ask would be a process per redraw for something that changes about
    /// twice an hour.
    pub fn head(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.dir.join("HEAD")).ok()?;
        let text = text.trim();
        match text.strip_prefix("ref: ") {
            Some(reference) => Some(
                reference
                    .rsplit('/')
                    .next()
                    .unwrap_or(reference)
                    .to_string(),
            ),
            // Detached: the file holds the commit itself.
            None if text.len() >= 7 => Some(text[..7].to_string()),
            None => None,
        }
    }

    /// When the head last moved, so that a commit, a checkout or a rebase in
    /// another window is noticed without asking `git` anything.
    pub fn head_moved(&self) -> Option<SystemTime> {
        let head = std::fs::metadata(self.dir.join("HEAD"))
            .and_then(|m| m.modified())
            .ok();
        // A commit on the branch you are on rewrites the ref rather than HEAD,
        // so the newer of the two is the one that means something happened.
        let refs = std::fs::metadata(self.dir.join("refs"))
            .and_then(|m| m.modified())
            .ok();
        let packed = std::fs::metadata(self.dir.join("packed-refs"))
            .and_then(|m| m.modified())
            .ok();
        [head, refs, packed].into_iter().flatten().max()
    }

    /// The committed text of a file, as of the head.
    ///
    /// `None` for a file git has never seen, which is the ordinary case for
    /// something you have just written and is why every line of a new file is
    /// left unmarked rather than marked as added.
    pub fn committed(&self, file: &Path) -> Option<String> {
        let name = self.relative(file)?;
        let out = Command::new("git")
            // Read-only means read-only: without this, `git` will happily
            // refresh the index of a repository somebody else is committing in.
            .args(["--no-optional-locks", "-C"])
            .arg(&self.root)
            .arg("show")
            .arg(format!("HEAD:{name}"))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        Some(if text.contains("\r\n") {
            text.replace("\r\n", "\n")
        } else {
            text
        })
    }

    /// Put one stretch of a file into the index, without touching the rest.
    ///
    /// The one thing in here that writes to a repository. It is done by
    /// building the patch for that hunk and handing it to `git apply
    /// --cached`, which is what every other editor's "stage this hunk" does,
    /// and it means the staging is git's own rather than an idea of staging
    /// written here.
    ///
    /// The patch is built against `HEAD`, so a file that has been *partly*
    /// staged already may not take it — git says so, and so do we, rather than
    /// forcing something into the index that does not describe it.
    pub fn stage(&self, file: &Path, patch: &str) -> Result<(), String> {
        use std::io::Write;
        let _ = file;
        let mut child = Command::new("git")
            .args(["--no-optional-locks", "-C"])
            .arg(&self.root)
            .args(["apply", "--cached", "--unidiff-zero", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("git would not start: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("git took no input")?
            .write_all(patch.as_bytes())
            .map_err(|e| format!("git would not take the patch: {e}"))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("git did not finish: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let why = String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("git would not apply it")
            .to_string();
        Err(why)
    }

    /// Who last touched a line, and when, and why.
    ///
    /// One line rather than the file: blaming a whole file is a second or two
    /// on anything large, and the question people actually ask is about the
    /// line in front of them.
    pub fn blame(&self, file: &Path, line: usize) -> Option<String> {
        let name = self.relative(file)?;
        let out = Command::new("git")
            .args(["--no-optional-locks", "-C"])
            .arg(&self.root)
            .args(["blame", "--porcelain", "-L"])
            .arg(format!("{},{}", line + 1, line + 1))
            .arg("--")
            .arg(&name)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut commit = String::new();
        let mut who = String::new();
        let mut when = String::new();
        let mut what = String::new();
        for row in text.lines() {
            if commit.is_empty() && row.len() >= 40 {
                commit = row.chars().take(8).collect();
            }
            if let Some(rest) = row.strip_prefix("author ") {
                who = rest.to_string();
            }
            if let Some(rest) = row.strip_prefix("author-time ") {
                when = rest.to_string();
            }
            if let Some(rest) = row.strip_prefix("summary ") {
                what = rest.to_string();
            }
        }
        // A line nobody has committed yet blames as all zeroes, which is git
        // saying "you, just now" in the least readable way it could.
        if commit.chars().all(|c| c == '0') {
            return Some("not committed yet".into());
        }
        let when = when
            .parse::<i64>()
            .ok()
            .map(day_of)
            .unwrap_or_else(|| "at some point".into());
        Some(format!("{commit} · {who} · {when} · {what}"))
    }

    /// A file's name as git spells it: from the root, with forward slashes
    /// whatever this platform calls a separator.
    pub fn relative(&self, file: &Path) -> Option<String> {
        let relative = file.strip_prefix(&self.root).ok()?;
        let mut name = String::new();
        for part in relative.components() {
            if !name.is_empty() {
                name.push('/');
            }
            name.push_str(&part.as_os_str().to_string_lossy());
        }
        Some(name)
    }
}

/// How many unchanged lines to put either side of a hunk in a patch.
///
/// Three, which is what every diff has used since the eighties and what `git
/// apply` expects when it is looking for where a patch belongs.
const CONTEXT: usize = 3;

/// One hunk, written as a patch git will take.
///
/// Against `HEAD`, because that is the text we have: the base a hunk was
/// worked out from is the committed file. Where the index has already been
/// given part of this file, git will say the patch does not apply — which is
/// true, and better said by git than guessed at here.
pub fn patch_for(name: &str, old: &str, new: &str, hunk: &Hunk) -> String {
    let olds: Vec<&str> = old.lines().collect();
    let news: Vec<&str> = new.lines().collect();

    // The context has to be the same lines on both sides, which it is: it is
    // taken from outside the hunk, and outside the hunk the two agree.
    let before = CONTEXT.min(hunk.was.start).min(hunk.lines.start);
    let after = CONTEXT
        .min(olds.len().saturating_sub(hunk.was.end))
        .min(news.len().saturating_sub(hunk.lines.end));

    let mut body = String::new();
    for row in &olds[hunk.was.start - before..hunk.was.start] {
        body.push_str(&format!(" {row}\n"));
    }
    for row in &olds[hunk.was.clone()] {
        body.push_str(&format!("-{row}\n"));
    }
    for row in &news[hunk.lines.clone()] {
        body.push_str(&format!("+{row}\n"));
    }
    for row in &olds[hunk.was.end..hunk.was.end + after] {
        body.push_str(&format!(" {row}\n"));
    }

    let old_count = hunk.was.len() + before + after;
    let new_count = hunk.lines.len() + before + after;
    // A range that covers no lines is written as the line *before* the gap,
    // which is how a diff says "nothing here, and it goes after that one".
    let old_start = match old_count {
        0 => hunk.was.start,
        _ => hunk.was.start - before + 1,
    };
    let new_start = match new_count {
        0 => hunk.lines.start,
        _ => hunk.lines.start - before + 1,
    };
    format!(
        "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n\
         @@ -{old_start},{old_count} +{new_start},{new_count} @@\n{body}"
    )
}

/// Where the conflict markers are in a file being merged.
///
/// The three lines git writes into a file it could not merge on its own:
/// `<<<<<<<` above what you have, `=======` between, and `>>>>>>>` below what
/// they have. Finding them is a search for three strings — which is all any
/// tool that helps with a merge does, because that is the whole of what git
/// leaves behind.
pub fn conflicts(text: &str) -> Vec<Conflict> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut middle: Option<usize> = None;
    for (line, row) in text.lines().enumerate() {
        if row.starts_with("<<<<<<<") {
            start = Some(line);
            middle = None;
        } else if row.starts_with("=======") && start.is_some() {
            middle = Some(line);
        } else if row.starts_with(">>>>>>>")
            && let (Some(from), Some(divider)) = (start, middle)
        {
            out.push(Conflict {
                start: from,
                divider,
                end: line,
            });
            start = None;
            middle = None;
        }
    }
    out
}

/// One thing git could not merge, as the three lines it left in the file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Conflict {
    /// The `<<<<<<<` line.
    pub start: usize,
    /// The `=======` line.
    pub divider: usize,
    /// The `>>>>>>>` line.
    pub end: usize,
}

impl Conflict {
    /// The lines of what is yours: between the first marker and the divider.
    pub fn ours(&self) -> std::ops::Range<usize> {
        self.start + 1..self.divider
    }

    /// The lines of what is theirs: between the divider and the last marker.
    pub fn theirs(&self) -> std::ops::Range<usize> {
        self.divider + 1..self.end
    }
}

/// A unix timestamp as a date, without pulling in a calendar library.
///
/// Days since the epoch, walked forward a year at a time. Nobody is blaming a
/// line from the year 40,000, and the loop is a few dozen turns for the years
/// anybody's repository covers.
fn day_of(seconds: i64) -> String {
    let mut days = seconds.div_euclid(86_400);
    let mut year = 1970;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let length = if leap { 366 } else { 365 };
        if days < length {
            break;
        }
        days -= length;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while month < 12 && days >= months[month] {
        days -= months[month];
        month += 1;
    }
    format!("{year}-{:02}-{:02}", month + 1, days + 1)
}

/// What happened to a line since the last commit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// This line is new.
    Added,
    /// This line is not what it was.
    Changed,
    /// Something that was here is gone. Drawn against the line that is now in
    /// its place, since the line it is about no longer has one.
    Removed,
}

impl Mark {
    /// The one character drawn in the gutter for it.
    pub fn glyph(&self) -> char {
        match self {
            Mark::Added => '\u{2503}',
            Mark::Changed => '\u{2503}',
            Mark::Removed => '\u{2581}',
        }
    }
}

/// Which lines of `new` differ from `old`, by line number in `new`.
///
/// Sorted, and with at most one mark per line: where a block was replaced by a
/// shorter one, the lines that remain are `Changed` and the last of them also
/// carries the fact that more went — but one column cannot say two things, so
/// `Changed` wins and only a deletion with nothing left in its place is drawn
/// as `Removed`.
pub fn marks(old: &str, new: &str) -> Vec<(usize, Mark)> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let mut out = Vec::new();
    walk(&old, &new, 0, &mut out);
    // A deletion off the end of the file is marked against a line that is not
    // there; it belongs on the last one that is.
    let last = new.len().saturating_sub(1);
    for (line, _) in &mut out {
        *line = (*line).min(last);
    }
    out.sort_by_key(|(line, _)| *line);
    out.dedup_by_key(|(line, _)| *line);
    out
}

/// One stretch of a file that differs from what was committed.
///
/// Both halves, because undoing a change means putting back what was there:
/// the lines as they are now, and the lines they replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The lines in the file as it is now. Empty for a stretch that was
    /// deleted — there is nothing there any more, which is the point.
    pub lines: std::ops::Range<usize>,
    /// The lines it replaced, in the file as it was committed. Empty for
    /// lines that are new.
    pub was: std::ops::Range<usize>,
}

impl Hunk {
    /// Whether this is the hunk a cursor on this line is standing in.
    ///
    /// A deletion has no lines of its own to stand in, so it is claimed by the
    /// line that took its place — which is the line its mark is drawn against.
    pub fn holds(&self, line: usize) -> bool {
        match self.lines.is_empty() {
            true => self.lines.start == line,
            false => self.lines.contains(&line),
        }
    }
}

/// The stretches in which two texts differ.
///
/// Worked out from what they have in common rather than from what they do not:
/// [`aligned`] gives every pair of lines that are the same, and everything
/// between two of those pairs is one hunk. Which means there is one diff in
/// this module and not two — `marks` colours the margin with it, `aligned`
/// lines two panes up with it, and this puts a change back with it.
pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    let olds = old.lines().count();
    let news = new.lines().count();
    let mut pairs = aligned(old, new);
    pairs.push((olds, news));

    let mut out = Vec::new();
    // Where the last pair of matching lines was, as the line *after* it.
    let (mut o, mut n) = (0usize, 0usize);
    for (om, nm) in pairs {
        if om > o || nm > n {
            out.push(Hunk {
                lines: n..nm,
                was: o..om,
            });
        }
        (o, n) = (om + 1, nm + 1);
    }
    out
}

/// The lines that are the same in both, as pairs of line numbers.
///
/// The other half of what [`marks`] says. `marks` reports what changed;
/// this reports what did not, which is what two files have to be lined up on
/// to be read side by side. Same diff, read the other way round.
pub fn aligned(old: &str, new: &str) -> Vec<(usize, usize)> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let mut out = Vec::new();
    align(&old, &new, 0, 0, &mut out);
    out.sort_unstable();
    out
}

fn align(
    old: &[&str],
    new: &[&str],
    old_at: usize,
    new_at: usize,
    out: &mut Vec<(usize, usize)>,
) {
    // The same shape as `walk`: take the matching ends off, then line up the
    // middle on the lines that are unique to each side.
    let head = old
        .iter()
        .zip(new.iter())
        .take_while(|(a, b)| a == b)
        .count();
    if head > 0 {
        out.extend((0..head).map(|n| (old_at + n, new_at + n)));
        return align(&old[head..], &new[head..], old_at + head, new_at + head, out);
    }
    let tail = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    if tail > 0 {
        let (old_end, new_end) = (old.len() - tail, new.len() - tail);
        out.extend((0..tail).map(|n| (old_at + old_end + n, new_at + new_end + n)));
        return align(&old[..old_end], &new[..new_end], old_at, new_at, out);
    }
    if old.is_empty() || new.is_empty() {
        return;
    }
    let run = anchors(old, new);
    if run.is_empty() {
        return;
    }
    let (mut old_seen, mut new_seen) = (0, 0);
    for (old_to, new_to) in &run {
        align(
            &old[old_seen..*old_to],
            &new[new_seen..*new_to],
            old_at + old_seen,
            new_at + new_seen,
            out,
        );
        out.push((old_at + old_to, new_at + new_to));
        old_seen = old_to + 1;
        new_seen = new_to + 1;
    }
    align(
        &old[old_seen..],
        &new[new_seen..],
        old_at + old_seen,
        new_at + new_seen,
        out,
    );
}

/// The lines that are the same in both, as pairs of positions, longest first.
///
/// This is patience diff: rather than the longest common subsequence of every
/// line, the longest run of lines that appear exactly once on each side. It is
/// what makes a diff of code line up on the lines a person would have lined it
/// up on — a brace that appears four hundred times is no evidence of anything,
/// and a function signature that appears once is.
fn anchors(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    use std::collections::HashMap;

    let mut counts: HashMap<&str, (u32, u32, usize, usize)> = HashMap::new();
    for (at, line) in old.iter().enumerate() {
        let entry = counts.entry(line).or_insert((0, 0, 0, 0));
        entry.0 += 1;
        entry.2 = at;
    }
    for (at, line) in new.iter().enumerate() {
        let entry = counts.entry(line).or_insert((0, 0, 0, 0));
        entry.1 += 1;
        entry.3 = at;
    }
    // In order of where they are in the new text, so that the run below comes
    // out increasing in both.
    let mut common: Vec<(usize, usize)> = counts
        .values()
        .filter(|(a, b, ..)| *a == 1 && *b == 1)
        .map(|(_, _, old, new)| (*old, *new))
        .collect();
    common.sort_by_key(|(_, new)| *new);

    // The longest run whose old positions also increase: patience, played on
    // the old side. `piles` holds the last card of each pile, `back` how each
    // card got there, so the run can be read off the end.
    let mut piles: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = Vec::with_capacity(common.len());
    for (at, (old_at, _)) in common.iter().enumerate() {
        let pile = piles.partition_point(|&card| common[card].0 < *old_at);
        back.push((pile > 0).then(|| piles[pile - 1]));
        if pile == piles.len() {
            piles.push(at);
        } else {
            piles[pile] = at;
        }
    }
    let mut run = Vec::new();
    let mut at = piles.last().copied();
    while let Some(card) = at {
        run.push(common[card]);
        at = back[card];
    }
    run.reverse();
    run
}

/// Diff `old` against `new`, writing marks in the coordinates of `new`.
///
/// `new_at` says where this slice of `new` sits in the whole, so the marks come
/// out as line numbers in the file rather than in the fragment. There is no
/// such number for `old`, because nothing is ever reported in its coordinates —
/// a line that is gone is reported against the line now in its place.
fn walk(old: &[&str], new: &[&str], new_at: usize, out: &mut Vec<(usize, Mark)>) {
    // The ends first: nearly every edit leaves both alone, and taking them off
    // is what keeps this cheap on a large file with a small change in it.
    let head = old
        .iter()
        .zip(new.iter())
        .take_while(|(a, b)| a == b)
        .count();
    if head > 0 {
        return walk(&old[head..], &new[head..], new_at + head, out);
    }
    let tail = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    if tail > 0 {
        return walk(
            &old[..old.len() - tail],
            &new[..new.len() - tail],
            new_at,
            out,
        );
    }

    match (old.is_empty(), new.is_empty()) {
        // Nothing on either side: the two are the same here.
        (true, true) => {}
        // Lines with nothing they came from.
        (true, false) => out.extend((0..new.len()).map(|n| (new_at + n, Mark::Added))),
        // Lines that went, marked against whatever is in their place. At the
        // very end of a file there is no such line, so the last one stands in.
        (false, true) => out.push((new_at, Mark::Removed)),
        (false, false) => {
            let run = anchors(old, new);
            if run.is_empty() {
                // Nothing to line up on: this block is simply different.
                out.extend((0..new.len()).map(|n| (new_at + n, Mark::Changed)));
                return;
            }
            let mut old_seen = 0;
            let mut new_seen = 0;
            for (old_to, new_to) in &run {
                walk(
                    &old[old_seen..*old_to],
                    &new[new_seen..*new_to],
                    new_at + new_seen,
                    out,
                );
                old_seen = old_to + 1;
                new_seen = new_to + 1;
            }
            walk(&old[old_seen..], &new[new_seen..], new_at + new_seen, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Vec<(usize, Mark)> {
        marks("a\nb\nc\nd\ne\n", text)
    }

    /// A repository of our own, somewhere temporary, so a test does not
    /// depend on the state of the one it is being run in.
    fn a_repo(name: &str) -> Option<PathBuf> {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("textfold-git-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).ok()?;
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .ok()
                .filter(|s| s.success())
        };
        run(&["init", "-q", "-b", "trunk"])?;
        run(&["config", "user.email", "nobody@example.invalid"])?;
        run(&["config", "user.name", "Nobody"])?;
        std::fs::write(dir.join("file.txt"), "one\ntwo\nthree\n").ok()?;
        run(&["add", "file.txt"])?;
        run(&["commit", "-qm", "first"])?;
        Some(dir)
    }

    #[test]
    fn a_hunk_knows_both_what_is_there_now_and_what_was() {
        // Three shapes, which are all the shapes there are: something
        // changed, something added, something taken away.
        let changed = hunks("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].lines, 1..2);
        assert_eq!(changed[0].was, 1..2);

        let added = hunks("a\nc\n", "a\nb\nc\n");
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].lines, 1..2);
        assert!(added[0].was.is_empty(), "nothing was there before");

        let removed = hunks("a\nb\nc\n", "a\nc\n");
        assert_eq!(removed.len(), 1);
        assert!(removed[0].lines.is_empty(), "nothing is there now");
        assert_eq!(removed[0].was, 1..2);
        // And a deletion is claimed by the line that took its place, which is
        // the line its mark is drawn against.
        assert!(removed[0].holds(1));
    }

    #[test]
    fn a_file_that_has_not_changed_has_no_hunks() {
        assert!(hunks("a\nb\n", "a\nb\n").is_empty());
    }

    #[test]
    fn two_changes_far_apart_are_two_hunks() {
        let found = hunks("a\nb\nc\nd\ne\n", "A\nb\nc\nd\nE\n");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].lines, 0..1);
        assert_eq!(found[1].lines, 4..5);
    }

    #[test]
    fn a_patch_says_what_git_needs_to_hear() {
        let old = "one\ntwo\nthree\nfour\nfive\n";
        let new = "one\ntwo\nTHREE\nfour\nfive\n";
        let hunk = hunks(old, new).into_iter().next().expect("one hunk");
        let patch = patch_for("file.txt", old, new, &hunk);
        assert!(patch.starts_with("diff --git a/file.txt b/file.txt\n"), "{patch}");
        assert!(patch.contains("@@ -1,5 +1,5 @@\n"), "{patch}");
        assert!(patch.contains("-three\n+THREE\n"), "{patch}");
        // The context is the lines either side, and they are the same on both
        // sides of the patch because outside the hunk the two files agree.
        assert!(patch.contains(" two\n-three"), "{patch}");
        assert!(patch.contains("+THREE\n four\n"), "{patch}");
    }

    #[test]
    fn staging_a_hunk_puts_that_much_of_the_file_in_the_index() {
        let Some(dir) = a_repo("staging") else {
            return; // No git on this machine, which is not a failing test.
        };
        let file = dir.join("file.txt");
        let repo = Repo::find(&file).expect("a repository");
        let old = repo.committed(&file).expect("committed");
        // Two changes, and only the first one is staged.
        let new = "ONE\ntwo\nTHREE\n";
        std::fs::write(&file, new).expect("written");
        let hunk = hunks(&old, new).into_iter().next().expect("a hunk");
        let patch = patch_for("file.txt", &old, new, &hunk);
        repo.stage(&file, &patch).expect("git took the patch");

        let staged = std::process::Command::new("git")
            .args(["-C"])
            .arg(&dir)
            .args(["show", ":file.txt"])
            .output()
            .expect("git ran");
        assert_eq!(
            String::from_utf8_lossy(&staged.stdout),
            "ONE\ntwo\nthree\n",
            "the first change is in the index and the second is not"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn conflict_markers_are_found_with_both_sides_of_each() {
        let text = "before\n<<<<<<< HEAD\nmine\n=======\ntheirs\nalso theirs\n>>>>>>> other\nafter\n";
        let found = conflicts(text);
        assert_eq!(found.len(), 1);
        let it = found[0];
        assert_eq!(it.start, 1);
        assert_eq!(it.divider, 3);
        assert_eq!(it.end, 6);
        assert_eq!(it.ours(), 2..3);
        assert_eq!(it.theirs(), 4..6);
    }

    #[test]
    fn half_a_conflict_is_not_a_conflict() {
        // A file that talks about merge markers is not a file in a merge.
        assert!(conflicts("<<<<<<< HEAD\nmine\n").is_empty());
        assert!(conflicts("nothing here\n").is_empty());
    }

    #[test]
    fn a_line_nobody_has_committed_blames_as_itself() {
        let Some(dir) = a_repo("blame") else {
            return;
        };
        let file = dir.join("file.txt");
        let repo = Repo::find(&file).expect("a repository");
        let said = repo.blame(&file, 0).expect("git blamed it");
        assert!(said.contains("Nobody"), "{said}");
        assert!(said.contains("first"), "the summary of the commit: {said}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_repository_says_which_branch_and_what_was_committed() {
        let Some(dir) = a_repo("basics") else {
            return; // No git on this machine, which is not a failing test.
        };
        let repo = Repo::find(&dir.join("file.txt")).expect("a repository");
        assert_eq!(repo.head().as_deref(), Some("trunk"));
        assert_eq!(
            repo.committed(&dir.join("file.txt")).as_deref(),
            Some("one\ntwo\nthree\n")
        );
        // A file git has never seen has no committed text, which is not the
        // same as having committed an empty one.
        assert_eq!(repo.committed(&dir.join("new.txt")), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_change_since_the_commit_is_found_without_asking_git_again() {
        let Some(dir) = a_repo("marks") else { return };
        let repo = Repo::find(&dir).expect("a repository");
        let base = repo.committed(&dir.join("file.txt")).expect("committed");
        assert_eq!(marks(&base, "one\nTWO\nthree\n"), vec![(1, Mark::Changed)]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_nobody_touched_has_nothing_to_say() {
        assert_eq!(at("a\nb\nc\nd\ne\n"), vec![]);
    }

    #[test]
    fn a_changed_line_is_marked_where_it_is_now() {
        assert_eq!(at("a\nb\nC\nd\ne\n"), vec![(2, Mark::Changed)]);
    }

    #[test]
    fn inserted_lines_are_marked_as_added() {
        assert_eq!(
            at("a\nb\nx\ny\nc\nd\ne\n"),
            vec![(2, Mark::Added), (3, Mark::Added)]
        );
    }

    #[test]
    fn a_deletion_is_marked_against_the_line_that_took_its_place() {
        assert_eq!(at("a\nb\nd\ne\n"), vec![(2, Mark::Removed)]);
    }

    #[test]
    fn an_edit_at_each_end_leaves_the_middle_alone() {
        let marks = at("A\nb\nc\nd\nE\n");
        assert_eq!(marks, vec![(0, Mark::Changed), (4, Mark::Changed)]);
    }

    #[test]
    fn a_block_moved_down_lines_up_on_what_is_unique() {
        // The `}` lines are everywhere and prove nothing; the bodies are
        // unique and are what the two sides are matched on.
        let old = "fn a() {\n  one\n}\nfn b() {\n  two\n}\n";
        let new = "fn b() {\n  two\n}\nfn a() {\n  one\n}\n";
        let marks = marks(old, new);
        assert!(!marks.is_empty(), "a move is a change somewhere");
        // Whatever it decides moved, it cannot decide everything moved.
        assert!(marks.len() < 6, "{marks:?}");
    }

    #[test]
    fn what_is_the_same_in_two_files_lines_up() {
        let old = "one\ntwo\nthree\nfour\n";
        let new = "one\ninserted\ntwo\nthree\nfour\n";
        assert_eq!(
            aligned(old, new),
            vec![(0, 0), (1, 2), (2, 3), (3, 4)],
            "an inserted line should push the rest down by one"
        );
    }

    #[test]
    fn two_files_with_nothing_in_common_line_up_nowhere() {
        assert_eq!(aligned("a\nb\n", "x\ny\n"), Vec::new());
    }

    #[test]
    fn every_line_of_two_identical_files_lines_up_with_itself() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(aligned(text, text), vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn lining_up_never_goes_backwards() {
        let old = "a\nb\nc\nd\ne\nf\ng\n";
        let new = "g\nf\ne\nd\nc\nb\na\n";
        let pairs = aligned(old, new);
        for window in pairs.windows(2) {
            assert!(
                window[0].0 < window[1].0 && window[0].1 < window[1].1,
                "{pairs:?}"
            );
        }
    }

    #[test]
    fn a_file_that_did_not_exist_before_is_all_new() {
        let marks = marks("", "one\ntwo\n");
        assert_eq!(marks, vec![(0, Mark::Added), (1, Mark::Added)]);
    }

    #[test]
    fn emptying_a_file_leaves_one_mark_rather_than_none() {
        let marks = marks("one\ntwo\n", "");
        assert_eq!(marks, vec![(0, Mark::Removed)]);
    }
}


/// What the editor keeps about git, for the files it has open.
///
/// One repository — the project's — because two files from two repositories in
/// one window is a thing that happens and a status bar that flickers between
/// two branch names is not worth the honesty. The marks are per file, worked
/// out from the committed text rather than by asking `git` again, so typing
/// costs a diff and not a process.
#[derive(Default)]
pub struct Tracker {
    repo: Option<Repo>,
    head: Option<String>,
    /// When the head last moved, so a commit elsewhere throws the baselines
    /// away rather than leaving every line of a file you just committed marked
    /// as changed.
    moved: Option<SystemTime>,
    files: HashMap<DocId, Tracked>,
}

struct Tracked {
    /// The file as it was committed. `None` for one git has never seen, which
    /// is what leaves a brand new file unmarked rather than solid green.
    base: Option<String>,
    marks: Vec<(usize, Mark)>,
    /// The document version the marks were worked out from.
    at: i32,
}

impl Tracker {
    /// Point it at a project. Answers whether it found a repository.
    pub fn open(&mut self, project: &Path) -> bool {
        self.repo = Repo::find(project);
        self.head = self.repo.as_ref().and_then(Repo::head);
        self.moved = self.repo.as_ref().and_then(Repo::head_moved);
        self.files.clear();
        self.repo.is_some()
    }

    /// The branch, for the status bar.
    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    pub fn watching(&self) -> bool {
        self.repo.is_some()
    }

    /// Whether this file has a committed version to be compared against, and
    /// so whether its gutter needs a column for the answer.
    pub fn tracking(&self, doc: DocId) -> bool {
        self.files.get(&doc).is_some_and(|f| f.base.is_some())
    }

    pub fn mark(&self, doc: DocId, line: usize) -> Option<Mark> {
        let file = self.files.get(&doc)?;
        file.marks
            .binary_search_by_key(&line, |(at, _)| *at)
            .ok()
            .map(|at| file.marks[at].1)
    }

    /// The next line at or after `from` that differs from the commit, for
    /// stepping through your own changes.
    pub fn next_change(&self, doc: DocId, from: usize, forwards: bool) -> Option<usize> {
        let file = self.files.get(&doc)?;
        // A run of marked lines is one change, so a step lands on the start of
        // the next run rather than on the next line of this one.
        let starts = file
            .marks
            .iter()
            .map(|(at, _)| *at)
            .enumerate()
            .filter(|(n, at)| *n == 0 || file.marks[n - 1].0 + 1 != *at)
            .map(|(_, at)| at);
        if forwards {
            starts.clone().find(|at| *at > from).or_else(|| starts.min())
        } else {
            let before: Vec<usize> = starts.clone().filter(|at| *at < from).collect();
            before.last().copied().or_else(|| starts.max())
        }
    }

    /// How many lines of this file differ from the commit.
    pub fn changed_lines(&self, doc: DocId) -> usize {
        self.files.get(&doc).map_or(0, |f| f.marks.len())
    }

    /// Forget a file that has been closed.
    pub fn forget(&mut self, doc: DocId) {
        self.files.remove(&doc);
    }

    /// Notice a commit, a checkout or a rebase in another window.
    ///
    /// Everything then has to be worked out again, because every baseline is
    /// the old head's idea of these files. Answers whether anything moved.
    pub fn poll_head(&mut self) -> bool {
        let Some(repo) = &self.repo else {
            return false;
        };
        let moved = repo.head_moved();
        if moved == self.moved {
            return false;
        }
        self.moved = moved;
        self.head = repo.head();
        self.files.clear();
        true
    }

    /// Bring one file up to date: fetch its committed text if this is the
    /// first sight of it, and work out the marks if it has been edited since
    /// the last time.
    ///
    /// Answers whether anything changed, so that the caller can tell a redraw
    /// from a wasted one.
    pub fn refresh(&mut self, doc: &Document) -> bool {
        let Some(repo) = &self.repo else {
            return false;
        };
        let Some(path) = doc.path.as_deref() else {
            return false;
        };
        if !path.starts_with(&repo.root) {
            return false;
        }
        let known = self.files.get(&doc.id);
        if known.is_some_and(|f| f.at == doc.version) {
            return false;
        }
        let base = match known {
            Some(file) => file.base.clone(),
            None => repo.committed(path),
        };
        let marks = match &base {
            Some(base) => marks(base, &doc.text()),
            None => Vec::new(),
        };
        self.files.insert(
            doc.id,
            Tracked {
                base,
                marks,
                at: doc.version,
            },
        );
        true
    }

    /// Read the committed text again, for a file that has just been saved or
    /// re-read: a save can be what makes git start knowing about it, and a
    /// checkout changes what it says.
    /// Say what a buffer's committed text is, without asking git.
    ///
    /// For the tests: a revert and a stage are worth testing without a
    /// repository behind them, and where the baseline came from is not the
    /// part that can be wrong. The marks are worked out again from it, so what
    /// the margin shows and what a revert puts back cannot disagree.
    #[cfg(test)]
    pub fn remember_baseline(&mut self, doc: DocId, base: String) {
        let marks = marks(&base, "");
        self.files.insert(
            doc,
            Tracked {
                base: Some(base),
                marks,
                // No version this could have been worked out from, so the next
                // refresh does the work properly.
                at: -1,
            },
        );
    }

    /// The file as it was committed, for a buffer being tracked.
    pub fn baseline(&self, doc: DocId) -> Option<&str> {
        self.files.get(&doc)?.base.as_deref()
    }

    /// The repository the tracked files are in.
    pub fn repo(&self) -> Option<&Repo> {
        self.repo.as_ref()
    }

    pub fn forget_baseline(&mut self, doc: DocId) {
        self.files.remove(&doc);
    }
}
