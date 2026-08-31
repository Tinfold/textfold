//! Two panes, compared.
//!
//! The gutter already knows how to say "this line is not what it was" — that
//! is what the bar beside a line you have edited is. Comparing two panes is
//! the same question asked about a different pair of texts, so it is the same
//! diff, the same marks and the same column, and there is nothing new to learn
//! in order to read it.
//!
//! What a diff needs beyond that is *alignment*. Two files scrolled
//! independently are two files; two files whose matching lines sit on the same
//! row are a diff. So the pane that does not have the keyboard is scrolled to
//! follow the one that does, using the lines the two have in common, and a
//! block inserted on one side pushes the other side's view along with it.
//!
//! Whole lines rather than words within a line. A terminal has one column per
//! character and no room for a second colour underneath the first, and a
//! word-level diff that has to be drawn as a line-level one anyway is work
//! nobody sees.

use crate::doc::{DocId, Document};
use crate::git::Mark;

/// One column of a pair of matched line numbers — which of the two a lookup
/// reads, and which it answers with, depending on the side asking.
type Column = fn(&(usize, usize)) -> usize;

/// Which of the two panes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn other(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// One pane of a comparison: which pane it is on the screen, and what is in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Half {
    pane: usize,
    doc: DocId,
    /// The document version this was worked out from, so a keystroke in either
    /// file is noticed without comparing them again on every frame.
    version: i32,
}

/// Two panes being compared.
pub struct Diff {
    left: Half,
    right: Half,
    /// Lines that differ, in each side's own numbering.
    left_marks: Vec<(usize, Mark)>,
    right_marks: Vec<(usize, Mark)>,
    /// Lines the two have in common, as `(left, right)` pairs, increasing in
    /// both. What the scrolling is lined up on.
    pairs: Vec<(usize, usize)>,
}

impl Diff {
    /// Compare what is in two panes.
    pub fn new(left: (usize, &Document), right: (usize, &Document)) -> Self {
        let (left_pane, left_doc) = left;
        let (right_pane, right_doc) = right;
        let (a, b) = (left_doc.text(), right_doc.text());
        Self {
            left: Half {
                pane: left_pane,
                doc: left_doc.id,
                version: left_doc.version,
            },
            right: Half {
                pane: right_pane,
                doc: right_doc.id,
                version: right_doc.version,
            },
            // Each side is marked against the other, which is the same diff run
            // both ways round. A line reported as `Removed` on the right is a
            // line the left has and the right does not, which is exactly what
            // the right-hand gutter should say.
            right_marks: crate::git::marks(&a, &b),
            left_marks: crate::git::marks(&b, &a),
            pairs: crate::git::aligned(&a, &b),
        }
    }

    /// The panes being compared, in screen order.
    pub fn panes(&self) -> (usize, usize) {
        (self.left.pane, self.right.pane)
    }

    /// Which side a pane is, or `None` for a pane that is not in this
    /// comparison.
    pub fn side_of(&self, pane: usize) -> Option<Side> {
        if pane == self.left.pane {
            Some(Side::Left)
        } else if pane == self.right.pane {
            Some(Side::Right)
        } else {
            None
        }
    }

    fn half(&self, side: Side) -> &Half {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    fn marks(&self, side: Side) -> &[(usize, Mark)] {
        match side {
            Side::Left => &self.left_marks,
            Side::Right => &self.right_marks,
        }
    }

    /// What to draw in the gutter beside a line of a pane.
    pub fn mark(&self, pane: usize, line: usize) -> Option<Mark> {
        let side = self.side_of(pane)?;
        let marks = self.marks(side);
        marks
            .binary_search_by_key(&line, |(at, _)| *at)
            .ok()
            .map(|at| marks[at].1)
    }

    /// How many lines differ, for saying so in the status bar.
    pub fn differing(&self) -> usize {
        self.left_marks.len().max(self.right_marks.len())
    }

    /// Whether the two panes hold the same text.
    pub fn same(&self) -> bool {
        self.left_marks.is_empty() && self.right_marks.is_empty()
    }

    /// Whether this comparison still describes what is on the screen.
    ///
    /// A pane closed, a different file shown in one of them, or either file
    /// edited all make it stale. The first two mean it should be dropped; the
    /// third means it should be worked out again, which is what
    /// [`Diff::current_for`] separates out.
    pub fn describes(&self, panes: &[(usize, DocId)]) -> bool {
        [self.left, self.right].iter().all(|half| {
            panes
                .iter()
                .any(|(pane, doc)| *pane == half.pane && *doc == half.doc)
        })
    }

    /// Whether it is still up to date with the text in those documents.
    pub fn current_for(&self, left: &Document, right: &Document) -> bool {
        self.left.version == left.version && self.right.version == right.version
    }

    /// The line in the other pane that belongs beside `line`.
    ///
    /// Between two lines the files have in common, the offset from the last
    /// one is kept: inside a block that differs, the two sides scroll together
    /// line for line, which is what stops a diff from lurching every time the
    /// view crosses into a change.
    pub fn beside(&self, pane: usize, line: usize) -> Option<usize> {
        let side = self.side_of(pane)?;
        let (from, to): (Column, Column) = match side {
            Side::Left => (|p| p.0, |p| p.1),
            Side::Right => (|p| p.1, |p| p.0),
        };
        // The last line they agree on at or before this one.
        let at = self.pairs.partition_point(|pair| from(pair) <= line);
        Some(match at {
            0 => line,
            _ => {
                let pair = &self.pairs[at - 1];
                to(pair) + (line - from(pair))
            }
        })
    }

    /// The pane on the other side, for scrolling it to follow.
    pub fn other_pane(&self, pane: usize) -> Option<usize> {
        let side = self.side_of(pane)?;
        Some(self.half(side.other()).pane)
    }

    /// The next line of a pane at or after `from` that differs, so that the
    /// keys for stepping through your own changes step through a comparison
    /// when there is one.
    ///
    /// A run of differing lines is one difference: a step lands on the start
    /// of the next block, not on the next line of this one.
    pub fn next_change(&self, pane: usize, from: usize, forwards: bool) -> Option<usize> {
        let side = self.side_of(pane)?;
        let marks = self.marks(side);
        let starts = marks
            .iter()
            .map(|(at, _)| *at)
            .enumerate()
            .filter(|(n, at)| *n == 0 || marks[n - 1].0 + 1 != *at)
            .map(|(_, at)| at);
        if forwards {
            starts.clone().find(|at| *at > from).or_else(|| starts.min())
        } else {
            let before: Vec<usize> = starts.clone().filter(|at| *at < from).collect();
            before.last().copied().or_else(|| starts.max())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Document;

    fn doc(id: u32, text: &str) -> Document {
        let mut doc = Document::scratch(
            DocId(id),
            format!("buffer {id}"),
            crate::doc::Indent::Spaces(4),
        );
        doc.set_text(text);
        doc
    }

    fn compare(a: &str, b: &str) -> (Diff, Document, Document) {
        let (left, right) = (doc(1, a), doc(2, b));
        let diff = Diff::new((0, &left), (1, &right));
        (diff, left, right)
    }

    #[test]
    fn two_files_that_are_the_same_have_nothing_to_say() {
        let (diff, ..) = compare("one\ntwo\n", "one\ntwo\n");
        assert!(diff.same());
        assert_eq!(diff.differing(), 0);
        assert_eq!(diff.mark(0, 0), None);
        assert_eq!(diff.mark(1, 0), None);
    }

    #[test]
    fn a_line_only_one_side_has_is_marked_on_that_side() {
        // The right has an extra line.
        let (diff, ..) = compare("one\ntwo\n", "one\nextra\ntwo\n");
        assert_eq!(diff.mark(1, 1), Some(Mark::Added), "the extra line");
        assert_eq!(diff.mark(1, 0), None);
        assert_eq!(diff.mark(1, 2), None);
        // And the left says something is missing where it would have gone.
        assert!(diff.mark(0, 0).is_some() || diff.mark(0, 1).is_some());
    }

    #[test]
    fn matching_lines_sit_beside_each_other() {
        let (diff, ..) = compare("one\ntwo\nthree\n", "one\nextra\ntwo\nthree\n");
        assert_eq!(diff.beside(0, 0), Some(0), "the first line of each");
        assert_eq!(diff.beside(0, 1), Some(2), "\"two\" moved down one");
        assert_eq!(diff.beside(0, 2), Some(3));
        // And back the other way.
        assert_eq!(diff.beside(1, 2), Some(1));
        assert_eq!(diff.beside(1, 3), Some(2));
    }

    #[test]
    fn inside_a_block_that_differs_the_two_sides_move_together() {
        let (diff, ..) = compare("same\na\nb\nc\nend\n", "same\nx\ny\nz\nend\n");
        // Nothing in the middle lines up, so the offset from "same" is kept
        // rather than everything snapping to the next line they agree on.
        assert_eq!(diff.beside(0, 1), Some(1));
        assert_eq!(diff.beside(0, 2), Some(2));
        assert_eq!(diff.beside(0, 3), Some(3));
        assert_eq!(diff.beside(0, 4), Some(4), "\"end\"");
    }

    #[test]
    fn a_pane_that_is_not_in_the_comparison_is_not_answered_about() {
        let (diff, ..) = compare("one\n", "two\n");
        assert_eq!(diff.side_of(7), None);
        assert_eq!(diff.mark(7, 0), None);
        assert_eq!(diff.beside(7, 0), None);
        assert_eq!(diff.other_pane(7), None);
    }

    #[test]
    fn the_other_pane_is_the_one_you_are_not_in() {
        let (diff, ..) = compare("one\n", "two\n");
        assert_eq!(diff.other_pane(0), Some(1));
        assert_eq!(diff.other_pane(1), Some(0));
    }

    #[test]
    fn stepping_walks_blocks_rather_than_lines() {
        // Two separate differences, the first of them three lines long.
        let (diff, ..) = compare(
            "keep\na\nb\nc\nkeep2\nkeep3\nd\nkeep4\n",
            "keep\nx\ny\nz\nkeep2\nkeep3\nw\nkeep4\n",
        );
        let first = diff.next_change(1, 0, true).expect("a difference");
        assert_eq!(first, 1);
        let second = diff.next_change(1, first, true).expect("another");
        assert_eq!(second, 6, "it stepped inside the first block");
    }

    #[test]
    fn an_edit_makes_a_comparison_out_of_date_without_making_it_wrong() {
        let (diff, left, mut right) = compare("one\n", "one\n");
        assert!(diff.current_for(&left, &right));
        right.set_text("two\n");
        assert!(!diff.current_for(&left, &right));
    }

    #[test]
    fn a_comparison_stops_describing_panes_that_have_moved_on() {
        let (diff, left, right) = compare("one\n", "two\n");
        assert!(diff.describes(&[(0, left.id), (1, right.id)]));
        assert!(
            !diff.describes(&[(0, left.id)]),
            "a pane was closed and the comparison did not notice"
        );
        assert!(
            !diff.describes(&[(0, left.id), (1, DocId(99))]),
            "a different file was shown and the comparison did not notice"
        );
    }
}
