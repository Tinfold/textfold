//! Where you are in a file, and what counts as a word.
//!
//! Every position in textfold is a character index into the rope, never a byte
//! and never a column. Bytes are what tree-sitter and language servers want,
//! and columns are what the screen wants, but neither survives an edit or a
//! change of font, and both make a `é` into a puzzle. The conversions live at
//! the edges, in [`syntax`](crate::syntax) and [`lsp`](crate::lsp) and the
//! drawing; in between there is one kind of number.

use ropey::{Rope, RopeSlice};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A selection: where it started and where the cursor is.
///
/// `anchor` is the end that stays put when you extend with shift, `head` the
/// end that moves and where the cursor is drawn. `anchor == head` is a plain
/// cursor with nothing selected, which is the ordinary case and not a special
/// one — there is no separate "cursor" type, because every cursor is a
/// selection of nothing.
///
/// A head *before* its anchor is a normal, expected thing: it is what
/// selecting backwards looks like.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
}

impl Range {
    pub fn point(at: usize) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn len(&self) -> usize {
        self.end() - self.start()
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The same span, with the cursor at the end. What you want after
    /// replacing the text under a selection.
    pub fn forward(&self) -> Self {
        Self::new(self.start(), self.end())
    }

    pub fn contains(&self, at: usize) -> bool {
        at >= self.start() && at < self.end()
    }

    /// Whether the two touch at all, counting bare cursors sitting inside
    /// one another as touching, so merging cannot leave a cursor stranded
    /// inside a selection.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.start().max(other.start()) <= self.end().min(other.end())
    }

    /// The span covering both.
    fn merged(&self, other: &Self) -> Self {
        let start = self.start().min(other.start());
        let end = self.end().max(other.end());
        // Keep whichever direction the one that moved last was going, so a
        // backwards selection that swallows a cursor stays backwards.
        if self.head < self.anchor {
            Self::new(end, start)
        } else {
            Self::new(start, end)
        }
    }

    /// The same range with both ends brought inside a document of `len`
    /// characters. What an edit somewhere else can leave behind.
    pub fn clamped(&self, len: usize) -> Self {
        Self::new(self.anchor.min(len), self.head.min(len))
    }
}

/// Every cursor in a document, and which of them is the one in charge.
///
/// There is always at least one, and they are always in order and never
/// overlapping — those two facts are what everything else is allowed to
/// assume, and [`Selections::normalise`] is what keeps them true.
///
/// The primary is the one the screen scrolls to follow and the one a
/// single-cursor question ("what line am I on?") is asked about. It survives
/// merging: if the primary is swallowed by another selection, the one that
/// swallowed it becomes primary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selections {
    ranges: Vec<Range>,
    primary: usize,
}

impl Default for Selections {
    fn default() -> Self {
        Self::single(Range::point(0))
    }
}

impl Selections {
    pub fn single(range: Range) -> Self {
        Self {
            ranges: vec![range],
            primary: 0,
        }
    }

    pub fn many(ranges: Vec<Range>, primary: usize) -> Self {
        let mut it = Self {
            ranges: if ranges.is_empty() {
                vec![Range::point(0)]
            } else {
                ranges
            },
            primary,
        };
        it.normalise();
        it
    }

    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn primary(&self) -> Range {
        self.ranges[self.primary.min(self.ranges.len() - 1)]
    }

    pub fn primary_index(&self) -> usize {
        self.primary.min(self.ranges.len() - 1)
    }

    /// Add a cursor, keeping it primary — the new one is the one you just
    /// asked for, so it is the one to follow.
    pub fn push(&mut self, range: Range) {
        self.ranges.push(range);
        self.primary = self.ranges.len() - 1;
        self.normalise();
    }

    /// Throw away every cursor but the primary. The way out of a multi-cursor
    /// edit, and what Escape does first.
    pub fn collapse_to_primary(&mut self) {
        let keep = self.primary();
        self.ranges = vec![keep];
        self.primary = 0;
    }

    /// Drop the selected spans, leaving bare cursors where the heads were.
    pub fn collapse_selections(&mut self) {
        for range in &mut self.ranges {
            *range = Range::point(range.head);
        }
        self.normalise();
    }

    /// Change every range through `f`, then put the set back in order.
    pub fn map(&mut self, mut f: impl FnMut(Range) -> Range) {
        for range in &mut self.ranges {
            *range = f(*range);
        }
        self.normalise();
    }

    /// Sort, merge anything overlapping, and keep the primary pointing at
    /// whatever became of the range it was pointing at.
    fn normalise(&mut self) {
        if self.ranges.is_empty() {
            self.ranges.push(Range::point(0));
            self.primary = 0;
            return;
        }
        let primary = self.ranges[self.primary.min(self.ranges.len() - 1)];
        self.ranges.sort_by_key(|r| (r.start(), r.end()));

        let mut merged: Vec<Range> = Vec::with_capacity(self.ranges.len());
        for range in self.ranges.drain(..) {
            match merged.last_mut() {
                Some(last) if last.overlaps(&range) => *last = last.merged(&range),
                _ => merged.push(range),
            }
        }
        // The primary is whichever range now covers where it was. It always
        // exists: either it survived, or something ate it, and either way
        // that something covers its head.
        self.primary = merged
            .iter()
            .position(|r| r == &primary)
            .or_else(|| {
                merged
                    .iter()
                    .position(|r| r.start() <= primary.head && primary.head <= r.end())
            })
            .unwrap_or(0);
        self.ranges = merged;
    }

    /// Bring every range inside a document of `len` characters.
    pub fn clamp(&mut self, len: usize) {
        for range in &mut self.ranges {
            *range = range.clamped(len);
        }
        self.normalise();
    }
}

/// What kind of character this is, for the purpose of deciding where a word
/// ends. Three classes, because two would make `foo(bar)` one word and four
/// would make people argue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Space,
    Word,
    Punct,
}

pub fn class_of(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// The character at `at`, or `None` at the end of the document.
pub fn char_at(rope: &Rope, at: usize) -> Option<char> {
    (at < rope.len_chars()).then(|| rope.char(at))
}

/// The start of the word around or before `at`.
///
/// "Before" matters: with the cursor just past `println`, the word you mean is
/// `println`, not the empty one starting where you are.
pub fn word_start(rope: &Rope, at: usize) -> usize {
    let mut at = at.min(rope.len_chars());
    if at == 0 {
        return 0;
    }
    // Step back over whitespace first, so the cursor after `foo   ` still
    // finds `foo` rather than stopping in the gap.
    while at > 0 && class_of(rope.char(at - 1)) == Class::Space {
        at -= 1;
    }
    if at == 0 {
        return 0;
    }
    let class = class_of(rope.char(at - 1));
    while at > 0 && class_of(rope.char(at - 1)) == class {
        at -= 1;
    }
    at
}

/// The end of the word at or after `at`.
pub fn word_end(rope: &Rope, at: usize) -> usize {
    let len = rope.len_chars();
    let mut at = at.min(len);
    while at < len && class_of(rope.char(at)) == Class::Space {
        at += 1;
    }
    if at >= len {
        return len;
    }
    let class = class_of(rope.char(at));
    while at < len && class_of(rope.char(at)) == class {
        at += 1;
    }
    at
}

/// The whole word sitting under `at`, for double-click and for "what is under
/// the cursor". Unlike [`word_start`] this does not reach backwards over
/// whitespace: double-clicking a gap should select the gap, not the word to
/// its left.
pub fn word_around(rope: &Rope, at: usize) -> Range {
    let len = rope.len_chars();
    let at = at.min(len);
    // At the very end of a line or file there is nothing under the cursor, so
    // take what is behind it.
    let class = match char_at(rope, at) {
        Some(c) if class_of(c) != Class::Space => class_of(c),
        _ => match at.checked_sub(1).and_then(|i| char_at(rope, i)) {
            Some(c) if class_of(c) != Class::Space => class_of(c),
            _ => Class::Space,
        },
    };
    let mut start = at;
    while start > 0 && class_of(rope.char(start - 1)) == class {
        start -= 1;
    }
    let mut end = at;
    while end < len && class_of(rope.char(end)) == class {
        end += 1;
    }
    Range::new(start, end)
}

/// The word under `at`, as text, when it is a word at all. What gets handed to
/// a language server as "the thing I am asking about", and what
/// select-next-occurrence starts from.
pub fn word_text_at(rope: &Rope, at: usize) -> Option<String> {
    let range = word_around(rope, at);
    if range.is_empty() {
        return None;
    }
    let first = rope.char(range.start());
    (class_of(first) == Class::Word).then(|| rope.slice(range.start()..range.end()).to_string())
}

/// The line `at` is on.
pub fn line_of(rope: &Rope, at: usize) -> usize {
    rope.char_to_line(at.min(rope.len_chars()))
}

/// Where a line starts.
pub fn line_start(rope: &Rope, line: usize) -> usize {
    rope.line_to_char(line.min(rope.len_lines().saturating_sub(1)))
}

/// Where a line's text ends — before its newline, not after it. This is the
/// end a person means by "end of the line".
pub fn line_end(rope: &Rope, line: usize) -> usize {
    let line = line.min(rope.len_lines().saturating_sub(1));
    let start = rope.line_to_char(line);
    let slice = rope.line(line);
    start + slice.len_chars() - trailing_newline_len(slice)
}

/// How many characters of the end of this slice are its line ending. Two for
/// a file written on Windows, and this is the only place that has to know.
fn trailing_newline_len(line: RopeSlice) -> usize {
    let len = line.len_chars();
    if len == 0 {
        return 0;
    }
    match line.char(len - 1) {
        '\n' => {
            if len >= 2 && line.char(len - 2) == '\r' {
                2
            } else {
                1
            }
        }
        '\r' => 1,
        _ => 0,
    }
}

/// The first character of a line that is not indentation — where the cursor
/// goes on the first press of Home. A line that is all whitespace has no such
/// character, so its end will do.
pub fn first_non_blank(rope: &Rope, line: usize) -> usize {
    let start = line_start(rope, line);
    let end = line_end(rope, line);
    let mut at = start;
    while at < end && rope.char(at).is_whitespace() {
        at += 1;
    }
    at
}

/// The indentation a line begins with, as text, so a new line below can start
/// with the same.
pub fn indent_of(rope: &Rope, line: usize) -> String {
    let start = line_start(rope, line);
    let end = first_non_blank(rope, line);
    rope.slice(start..end).to_string()
}

/// How many screen columns the text from the start of the line to `at`
/// occupies, given how wide a tab is.
///
/// Not a character count: a tab is worth however many columns it takes to
/// reach the next stop, and a wide glyph is worth two.
pub fn visual_column(rope: &Rope, at: usize, tab_width: usize) -> usize {
    let line = line_of(rope, at);
    let start = line_start(rope, line);
    let mut col = 0;
    for c in rope.slice(start..at).chars() {
        col += char_width(c, col, tab_width);
    }
    col
}

/// The character index on `line` that sits at or just before screen column
/// `col`. The other direction of [`visual_column`], for a mouse click.
pub fn char_at_column(rope: &Rope, line: usize, col: usize, tab_width: usize) -> usize {
    let start = line_start(rope, line);
    let end = line_end(rope, line);
    let mut at = start;
    let mut width = 0;
    while at < end {
        let step = char_width(rope.char(at), width, tab_width);
        // Landing on the left half of a wide character means that character,
        // and on the right half means the next one along — which is what
        // clicking between two things has to mean for either to be reachable.
        if width + step > col {
            return if col >= width + step.div_ceil(2) {
                at + 1
            } else {
                at
            };
        }
        width += step;
        at += 1;
    }
    end
}

/// How many columns one character takes, sitting at column `col`.
pub fn char_width(c: char, col: usize, tab_width: usize) -> usize {
    match c {
        '\t' => tab_width - (col % tab_width),
        '\n' | '\r' => 0,
        _ => {
            let mut buf = [0u8; 4];
            UnicodeWidthStr::width(&*c.encode_utf8(&mut buf)).max(1)
        }
    }
}

/// How wide a string is on screen, starting from column zero.
pub fn str_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Cut a string to fit `width` columns, ending in `…` if anything was lost.
/// For a status line and a popup, where running over is worse than eliding.
pub fn truncate(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    let mut out = String::new();
    let mut used = 0;
    for g in text.graphemes(true) {
        let w = str_width(g).max(1);
        if used + w > width - 1 {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(text: &str) -> Rope {
        Rope::from_str(text)
    }

    #[test]
    fn selections_stay_sorted_and_apart() {
        let mut sel = Selections::many(
            vec![Range::new(10, 14), Range::new(0, 4), Range::new(12, 20)],
            0,
        );
        assert_eq!(sel.len(), 2);
        assert_eq!(sel.ranges()[0], Range::new(0, 4));
        assert_eq!(sel.ranges()[1], Range::new(10, 20));
        // The primary was the first-written range, which is still its own.
        assert_eq!(sel.primary(), Range::new(10, 20));
        sel.collapse_to_primary();
        assert_eq!(sel.len(), 1);
    }

    #[test]
    fn a_cursor_swallowed_by_a_selection_hands_over_being_primary() {
        let mut sel = Selections::single(Range::point(5));
        sel.push(Range::new(0, 20));
        assert_eq!(sel.len(), 1);
        assert_eq!(sel.primary(), Range::new(0, 20));
    }

    #[test]
    fn words_end_where_the_kind_of_character_changes() {
        let r = rope("let foo_bar = baz();");
        assert_eq!(word_around(&r, 5), Range::new(4, 11));
        assert_eq!(word_text_at(&r, 5).as_deref(), Some("foo_bar"));
        // On the bracket: punctuation is its own word, and `();` is one run.
        assert_eq!(word_text_at(&r, 17), None);
        assert_eq!(word_end(&r, 0), 3);
        assert_eq!(word_start(&r, 11), 4);
    }

    #[test]
    fn a_cursor_just_past_a_word_is_still_in_it() {
        let r = rope("println");
        assert_eq!(word_text_at(&r, 7).as_deref(), Some("println"));
    }

    #[test]
    fn line_ends_stop_before_the_newline_however_it_is_spelled() {
        let unix = rope("one\ntwo\n");
        assert_eq!(line_end(&unix, 0), 3);
        let dos = rope("one\r\ntwo\r\n");
        assert_eq!(line_end(&dos, 0), 3);
        assert_eq!(line_end(&dos, 1), 8);
    }

    #[test]
    fn tabs_are_worth_what_it_takes_to_reach_the_next_stop() {
        let r = rope("\tx\ty");
        assert_eq!(visual_column(&r, 0, 4), 0);
        assert_eq!(visual_column(&r, 1, 4), 4);
        assert_eq!(visual_column(&r, 2, 4), 5);
        assert_eq!(visual_column(&r, 3, 4), 8);
        // And back the other way.
        assert_eq!(char_at_column(&r, 0, 4, 4), 1);
        assert_eq!(char_at_column(&r, 0, 8, 4), 3);
    }

    #[test]
    fn clicking_past_the_end_of_a_line_lands_on_its_end() {
        let r = rope("short\nlonger line\n");
        assert_eq!(char_at_column(&r, 0, 40, 4), 5);
    }

    #[test]
    fn truncating_leaves_room_for_the_mark_that_says_it_happened() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(str_width(&truncate("hello world", 8)), 8);
    }
}
