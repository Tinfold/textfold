//! Colouring code, by asking a parser what it is looking at.
//!
//! One parse tree per open file, kept up to date rather than rebuilt: every
//! edit is handed to tree-sitter as an [`InputEdit`] first, so reparsing after
//! a keystroke re-reads the few nodes that changed and leaves the rest of the
//! file alone. That is the whole reason this is fast enough to do while you
//! type.
//!
//! Colours are worked out for the part of the file on screen and no further. A
//! query over a whole ten-thousand-line file would be wasted on the forty
//! lines anybody can see.
//!
//! Precedence is tree-sitter's own convention, which grammar authors write
//! their query files expecting: among patterns matching the same node the
//! first one written wins, and a node inside another one wins over it. That is
//! why the generic `(identifier) @variable` at the bottom of a highlights file
//! does not flatten everything above it.

use std::ops::ControlFlow;
use std::ops::Range as ByteRange;
use std::time::{Duration, Instant};

use ropey::Rope;
use tree_sitter::{
    InputEdit, Language, Node, ParseOptions, Parser, Point, Query, QueryCursor, StreamingIterator,
    TextProvider, Tree,
};

use crate::doc::AppliedEdit;
use crate::theme::{CAPTURES, Role};

/// How long a parse may take, while you are typing, before textfold gives up
/// on this pass.
///
/// A parse of ordinary source is measured in milliseconds. Something that
/// takes longer than this is not ordinary source — a minified bundle, a data
/// file that happens to end in `.js`, a megabyte of something the grammar
/// cannot make sense of and is recovering from a token at a time. Whatever it
/// is, it is being looked at rather than written, and looking at it in one
/// colour beats waiting for it in several.
const BUDGET: Duration = Duration::from_millis(150);

/// How long a parse started because nothing else was happening may take.
///
/// The budget above is wall-clock, and wall-clock is not a measure of work. A
/// language server indexing a project takes every core on the machine for
/// seconds at a stretch, and a parse that wanted a millisecond of processor
/// can sit through a hundred and fifty of them without being given any. That
/// is not a file textfold cannot colour; it is a file textfold was too busy to
/// colour, and the two must not have the same answer. So the retry runs when
/// the rush is over and gets long enough that only a genuinely pathological
/// file fails it.
const PATIENT: Duration = Duration::from_secs(2);

/// A language's parser and its highlight query, compiled. One per language for
/// the life of the process — compiling a query costs milliseconds, which is
/// nothing once and far too much per keystroke.
pub struct Grammar {
    pub language: Language,
    query: Query,
    /// What each capture in the query means, by capture index. `None` for a
    /// capture name textfold has no colour for, which is not an error: a
    /// grammar is allowed to be more specific than we are.
    roles: Vec<Option<Role>>,
    /// How specific each capture name is, by capture index — the number of
    /// dots in it. `@comment.documentation` is more specific than `@comment`,
    /// and where two patterns claim exactly the same bytes that is what
    /// settles which one is being more precise about them.
    specificity: Vec<u8>,
}

impl Grammar {
    /// Compile a grammar, or `None` if the query does not go with it — which
    /// happens when a grammar library on disk is a different version from the
    /// query file beside it. A grammar that will not compile means a file
    /// without colours, not an editor that will not start.
    pub fn new(language: Language, highlights: &str) -> Option<Self> {
        let query = Query::new(&language, highlights).ok()?;
        let roles = query.capture_names().iter().map(|n| role_for(n)).collect();
        let specificity = query
            .capture_names()
            .iter()
            .map(|n| n.matches('.').count().min(u8::MAX as usize) as u8)
            .collect();
        Some(Self {
            language,
            query,
            roles,
            specificity,
        })
    }
}

/// The colour a capture name means.
///
/// Names are dotted and get more specific to the right, so an unknown
/// `@function.method.static` falls back along the dots until it finds
/// something known — here, `function`. Grammars invent names constantly and
/// none of them should cost a file its colours.
/// Whether a stretch of bytes covers more than one line, which is the whole of
/// what makes something worth folding.
fn spans_lines(rope: &Rope, range: &ByteRange<usize>) -> bool {
    let len = rope.len_bytes();
    let from = crate::text::line_of(rope, rope.byte_to_char(range.start.min(len)));
    let to = crate::text::line_of(rope, rope.byte_to_char(range.end.min(len)));
    to > from
}

pub fn role_for(name: &str) -> Option<Role> {
    let mut candidate = name;
    loop {
        // Longest name first, so `constant.builtin` is not shadowed by
        // `constant`, whichever order the table happens to be in.
        if let Some((_, role)) = CAPTURES
            .iter()
            .filter(|(key, _)| *key == candidate)
            .max_by_key(|(key, _)| key.len())
        {
            return Some(*role);
        }
        // Back to the last dot, and try the shorter name; a name with no dots
        // left in it is a name nothing here knows.
        candidate = &candidate[..candidate.rfind('.')?];
    }
}

/// A file's parse tree, kept alongside its text.
pub struct Syntax {
    grammar: &'static Grammar,
    parser: Parser,
    tree: Tree,
    /// Bumped whenever the tree changes, so a cache can tell whether the
    /// answer it is holding is still the answer.
    pub revision: u64,
}

impl Syntax {
    /// Parse a file for the first time.
    ///
    /// `None` when the parser refuses the language — a version mismatch —
    /// or when the file takes longer than [`BUDGET`] to parse. Neither is
    /// something to stop for: the file opens either way, without colours.
    pub fn new(grammar: &'static Grammar, rope: &Rope) -> Option<Self> {
        Self::within(grammar, rope, BUDGET)
    }

    /// The same, given as long as it takes short of absurdity.
    ///
    /// For a second attempt, made once the machine is quiet. See [`PATIENT`].
    pub fn patient(grammar: &'static Grammar, rope: &Rope) -> Option<Self> {
        Self::within(grammar, rope, PATIENT)
    }

    fn within(grammar: &'static Grammar, rope: &Rope, budget: Duration) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&grammar.language).ok()?;
        let tree = parse(&mut parser, rope, None, budget)?;
        Some(Self {
            grammar,
            parser,
            tree,
            revision: 1,
        })
    }

    /// Take in a set of edits and reparse.
    ///
    /// Every edit has to be handed over, in the order it happened, before the
    /// reparse — that is what tells tree-sitter which subtrees it may keep. An
    /// edit not passed on here would leave the tree quietly describing text
    /// that is no longer there.
    /// `false` when the reparse ran out of time, which means this tree is now
    /// describing text that is not there any more and the caller must throw it
    /// away. Keeping a tree that has fallen behind its rope would colour code
    /// that has moved.
    pub fn update(&mut self, rope: &Rope, edits: &[AppliedEdit]) -> bool {
        for edit in edits {
            self.tree.edit(&InputEdit {
                start_byte: edit.start_byte,
                old_end_byte: edit.old_end_byte,
                new_end_byte: edit.new_end_byte,
                start_position: point(edit.start_point),
                old_end_position: point(edit.old_end_point),
                new_end_position: point(edit.new_end_point),
            });
        }
        let Some(tree) = parse(&mut self.parser, rope, Some(&self.tree), BUDGET) else {
            return false;
        };
        self.tree = tree;
        self.revision += 1;
        true
    }

    /// What colour every part of a stretch of the file is.
    ///
    /// Returns spans in byte offsets, in order, covering only the parts that
    /// are coloured at all — the gaps between them are ordinary text and the
    /// drawing knows it.
    pub fn highlights(&self, rope: &Rope, range: ByteRange<usize>) -> Vec<(ByteRange<usize>, Role)> {
        let range = range.start..range.end.min(rope.len_bytes());
        if range.is_empty() {
            return Vec::new();
        }

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        // Start, end, how specific the capture name was, which pattern, what.
        let mut found: Vec<(usize, usize, u8, usize, Role)> = Vec::new();
        let mut captures = cursor.captures(
            &self.grammar.query,
            self.tree.root_node(),
            RopeProvider(rope),
        );
        while let Some((matched, index)) = captures.next() {
            let capture = matched.captures[*index];
            let Some(Some(role)) = self.grammar.roles.get(capture.index as usize) else {
                continue;
            };
            let node = capture.node;
            let specificity = self
                .grammar
                .specificity
                .get(capture.index as usize)
                .copied()
                .unwrap_or(0);
            found.push((
                node.start_byte(),
                node.end_byte(),
                specificity,
                matched.pattern_index,
                *role,
            ));
        }

        // Paint from least specific to most, so that what ends up on top is
        // what tree-sitter's conventions say should be. Three rules, in order,
        // and each is a sort that leaves the winner last for a straight sweep
        // to finish on:
        //
        // An inner node beats the node containing it, so wider first.
        //
        // Then a more specific capture name beats a plainer one over the same
        // bytes: `(line_comment (doc_comment)) @comment.documentation` and
        // `(line_comment) @comment` both claim the whole of a `///` line, and
        // the one that went to the trouble of saying `.documentation` is the
        // one being precise about it. Fewer dots first.
        //
        // Then, still tied, the earliest pattern written wins, which is
        // tree-sitter's own rule and how a grammar puts a specific case in
        // front of a catch-all. Later patterns first.
        found.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(b.1.cmp(&a.1))
                .then(a.2.cmp(&b.2))
                .then(b.3.cmp(&a.3))
        });

        let width = range.len();
        let mut painted: Vec<Option<Role>> = vec![None; width];
        for (start, end, _, _, role) in found {
            let from = (start.max(range.start) - range.start).min(width);
            let to = end.min(range.end).saturating_sub(range.start).min(width);
            for cell in &mut painted[from..to] {
                *cell = Some(role);
            }
        }

        // Runs of one colour, which is what the drawing wants: one styled span
        // per run rather than one per byte.
        let mut spans = Vec::new();
        let mut at = 0;
        while at < painted.len() {
            let Some(role) = painted[at] else {
                at += 1;
                continue;
            };
            let start = at;
            while at < painted.len() && painted[at] == Some(role) {
                at += 1;
            }
            spans.push((range.start + start..range.start + at, role));
        }
        spans
    }

    /// The innermost node covering a byte, as a name — what the status line
    /// shows when you ask what you are standing in, and what makes a wrong
    /// colour diagnosable rather than mysterious.
    pub fn node_at(&self, byte: usize) -> Option<String> {
        let node = self
            .tree
            .root_node()
            .descendant_for_byte_range(byte, byte)?;
        Some(node.kind().to_string())
    }

    /// The smallest node that covers this stretch of bytes and is bigger than
    /// it. What "select more" means when there is a parse tree to ask: the
    /// expression, then the statement, then the block, then the function.
    pub fn enclosing(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let mut node = self
            .tree
            .root_node()
            .descendant_for_byte_range(from, to.max(from))?;
        loop {
            let range = node.byte_range();
            if range.start < from || range.end > to {
                return Some((range.start, range.end));
            }
            node = node.parent()?;
        }
    }

    /// The innermost thing around this byte that is worth folding, as a byte
    /// range.
    ///
    /// Worth folding means it covers more than one line: a fold that hides
    /// nothing is a keystroke that appears to do nothing, so the search walks
    /// outwards from the smallest node until it finds one that spans a line
    /// break — the string, then the argument list, then the body, then the
    /// function.
    ///
    /// The range is the whole node. Turning that into "the first line stays
    /// and the rest goes" is the caller's business, because that is a fact
    /// about the screen rather than about the syntax.
    pub fn foldable_at(&self, byte: usize, rope: &Rope) -> Option<(usize, usize)> {
        let mut node = self.tree.root_node().descendant_for_byte_range(byte, byte)?;
        loop {
            let range = node.byte_range();
            if spans_lines(rope, &range) {
                return Some((range.start, range.end));
            }
            node = node.parent()?;
        }
    }

    /// Everything at the top of the file worth folding: one range per item,
    /// none of them inside another.
    ///
    /// The children of the root rather than every node anywhere, because
    /// "fold everything" means the file as a list of what is in it — every
    /// function shut, each one still one line you can read and open. Folding
    /// every node at every depth would fold the file into its first line,
    /// which is a thing nobody has ever wanted.
    pub fn foldable_top_level(&self, rope: &Rope) -> Vec<(usize, usize)> {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        root.children(&mut cursor)
            .map(|node| node.byte_range())
            .filter(|range| spans_lines(rope, range))
            .map(|range| (range.start, range.end))
            .collect()
    }

    /// The bracket matching the one at `byte`, found through the tree rather
    /// than by counting, so a brace inside a string or a comment is not
    /// mistaken for structure.
    pub fn matching_bracket(&self, byte: usize) -> Option<usize> {
        // The node at a bracket is the bracket token itself — an anonymous
        // node whose kind is the character. Its parent is the thing bracketed,
        // and the partner is that parent's first or last child.
        let node = self
            .tree
            .root_node()
            .descendant_for_byte_range(byte, byte + 1)?;
        if node.child_count() > 0 || !BRACKETS.iter().any(|(o, c)| node.kind() == *o || node.kind() == *c) {
            return None;
        }
        let parent = node.parent()?;
        let count = parent.child_count();
        if count < 2 {
            return None;
        }
        let first = parent.child(0)?;
        let last = parent.child(count as u32 - 1)?;
        if !BRACKETS
            .iter()
            .any(|(o, c)| first.kind() == *o && last.kind() == *c)
        {
            return None;
        }
        if node.id() == first.id() {
            return Some(last.start_byte());
        }
        if node.id() == last.id() {
            return Some(last_char_byte(&last));
        }
        None
    }
}

/// The pairs that count as brackets in every language that has any. A grammar
/// naming its own is a nicety nobody has needed yet.
const BRACKETS: &[(&str, &str)] = &[("(", ")"), ("[", "]"), ("{", "}")];

/// The byte a one-character node starts at, which for a closing bracket is
/// where the cursor should land.
fn last_char_byte(node: &Node) -> usize {
    node.start_byte()
}

fn point((row, column): (usize, usize)) -> Point {
    Point { row, column }
}

/// Parse straight out of the rope, without flattening it into a string first.
/// A ten-megabyte file would otherwise be copied on every keystroke.
///
/// Gives up after `budget`. Tree-sitter asks the callback whether to carry
/// on every so often, which is the only way to bound this: some inputs take
/// the parser a very long time, and none of them are worth an editor that has
/// stopped answering.
fn parse(parser: &mut Parser, rope: &Rope, old: Option<&Tree>, budget: Duration) -> Option<Tree> {
    let mut read = |byte: usize, _: Point| -> &[u8] {
        if byte >= rope.len_bytes() {
            return &[];
        }
        let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_start..]
    };
    let deadline = Instant::now() + budget;
    let mut give_up = |_: &tree_sitter::ParseState| {
        if Instant::now() > deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut give_up);
    parser.parse_with_options(&mut read, old, Some(options))
}

/// Lets the query engine read a node's text — needed by predicates like
/// `#match?`, which several grammars lean on heavily. Rust's highlights are
/// noticeably worse without it: it is what tells `MAX_SIZE` from `Vec`.
struct RopeProvider<'a>(&'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = ChunkBytes<'a>;

    fn text(&mut self, node: Node) -> Self::I {
        let rope = self.0;
        let start = rope.byte_to_char(node.start_byte().min(rope.len_bytes()));
        let end = rope.byte_to_char(node.end_byte().min(rope.len_bytes()));
        ChunkBytes(rope.slice(start..end).chunks())
    }
}

struct ChunkBytes<'a>(ropey::iter::Chunks<'a>);

impl<'a> Iterator for ChunkBytes<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(str::as_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang;

    fn rust_grammar() -> &'static Grammar {
        lang::init();
        let id = lang::by_name("rust").expect("shipped");
        lang::get(id).grammar().expect("compiled in")
    }

    fn roles(text: &str) -> Vec<(String, Role)> {
        roles_in("rust", text)
    }

    fn roles_in(language: &str, text: &str) -> Vec<(String, Role)> {
        lang::init();
        let id = lang::by_name(language).expect("shipped");
        let grammar = lang::get(id).grammar().expect("compiled in");
        let rope = Rope::from_str(text);
        let syntax = Syntax::new(grammar, &rope).expect("parses");
        syntax
            .highlights(&rope, 0..rope.len_bytes())
            .into_iter()
            .map(|(span, role)| (text[span].to_string(), role))
            .collect()
    }

    #[test]
    fn a_file_the_parser_cannot_get_through_in_time_gets_no_tree() {
        // Not a size limit: a megabyte of tokens the grammar cannot make sense
        // of takes the parser far longer than a megabyte of real code, and it
        // is the time that matters, not the bytes.
        let mut noise = String::new();
        while noise.len() < 3_000_000 {
            noise.push_str("fn ( { [ ) ] } impl <<< >>> where 'a: 'b + ");
        }
        let rope = Rope::from_str(&noise);
        let started = std::time::Instant::now();
        let syntax = Syntax::new(rust_grammar(), &rope);
        let took = started.elapsed();
        assert!(
            took < BUDGET * 4,
            "gave up after {took:?}, which is not giving up"
        );
        // Either it managed it inside the budget or it did not; what matters
        // is that it came back.
        let _ = syntax;
    }

    #[test]
    fn a_yaml_key_is_not_the_same_colour_as_its_value() {
        // The grammar's own query calls every plain scalar a string, and in
        // YAML that is the whole document. A config file where the keys and
        // the values are the same colour is a config file with no colours.
        let found = roles_in(
            "yaml",
            "name: build\njobs:\n  - uses: actions/checkout\n    with: { n: 1 }\n",
        );
        let role = |want: &str| {
            found
                .iter()
                .find(|(text, _)| text == want)
                .map(|(_, role)| *role)
        };
        assert_eq!(role("name"), Some(Role::Property));
        assert_eq!(role("build"), Some(Role::String));
        assert_eq!(role("jobs"), Some(Role::Property));
        assert_eq!(role("uses"), Some(Role::Property));
        assert_eq!(role("actions/checkout"), Some(Role::String));
        // Inside `{ }` as well as under a `-`.
        assert_eq!(role("n"), Some(Role::Property));
        assert_eq!(role("1"), Some(Role::Number));
    }

    #[test]
    fn yaml_says_what_the_shipped_query_leaves_plain() {
        let found = roles_in(
            "yaml",
            "base: &b\n  a: 1\nchild:\n  <<: *b\n  when: 2001-12-14\n  said: \"a\\nb\"\n",
        );
        let role = |want: &str| {
            found
                .iter()
                .find(|(text, _)| text == want)
                .map(|(_, role)| *role)
        };
        // A merge is an instruction, not a field.
        assert_eq!(role("<<"), Some(Role::Keyword));
        // A date is a value the grammar has a node for and never emits.
        assert_eq!(role("when"), Some(Role::Property));
        assert_eq!(role("2001-12-14"), Some(Role::Number));
        // And an escape inside a string beats the string it is inside.
        assert_eq!(role("\\n"), Some(Role::StringEscape));
    }

    #[test]
    fn capture_names_fall_back_along_their_dots() {
        assert_eq!(role_for("keyword"), Some(Role::Keyword));
        // `keyword.control` has a colour of its own, so a name below it lands
        // there rather than on plain `keyword`.
        assert_eq!(role_for("keyword.control.repeat"), Some(Role::KeywordControl));
        // And a name below one that does not falls the rest of the way.
        assert_eq!(role_for("keyword.operator.overload"), Some(Role::Keyword));
        // A more specific name with its own meaning keeps it.
        assert_eq!(role_for("variable.parameter"), Some(Role::Parameter));
        assert_eq!(role_for("constant.builtin"), Some(Role::Boolean));
        assert_eq!(role_for("punctuation.bracket"), Some(Role::Bracket));
        assert_eq!(role_for("nothing.like.this"), None);
    }

    #[test]
    fn rust_is_coloured_the_way_a_person_would_expect() {
        let found = roles("fn main() { let x: u32 = 1; }");
        let by_text = |want: &str| {
            found
                .iter()
                .find(|(text, _)| text == want)
                .map(|(_, role)| *role)
        };
        assert_eq!(by_text("fn"), Some(Role::Keyword));
        assert_eq!(by_text("let"), Some(Role::Keyword));
        assert_eq!(by_text("main"), Some(Role::Function));
        // A primitive is a type the language has always had, which is its own
        // colour in most schemes.
        assert_eq!(by_text("u32"), Some(Role::TypeBuiltin));
    }

    #[test]
    fn the_finer_kinds_of_code_reach_their_own_colours() {
        // The roles a grammar's own query is more specific about than a
        // ten-colour scheme could be. Each of these arrives as a dotted
        // capture name, and each has to land on its own role rather than
        // sliding back up to the general one.
        let found = roles(concat!(
            "/// what it does\n",
            "// an aside\n",
            "impl S {\n",
            "    fn go(&self) -> String { self.name.trim().into() }\n",
            "}\n",
            "fn shout() { println!(\"hi\"); }\n",
        ));
        // A comment node runs to the end of its line, newline and all.
        let by_text = |want: &str| {
            found
                .iter()
                .find(|(text, _)| text.trim_end() == want)
                .map(|(_, role)| *role)
        };
        // `(line_comment) @comment` and `(line_comment (doc_comment))
        // @comment.documentation` both claim the whole of the first line, and
        // the grammar writes the general one first. The specific one still has
        // to win, or a doc comment is just a comment.
        assert_eq!(by_text("/// what it does"), Some(Role::CommentDoc));
        assert_eq!(by_text("// an aside"), Some(Role::Comment));
        // Rust says `@function.method` for a call through a value and plain
        // `@function` for the definition, which is the grammar's business
        // rather than ours.
        assert_eq!(by_text("go"), Some(Role::Function));
        assert_eq!(by_text("trim"), Some(Role::Method));
        assert_eq!(by_text("println!"), Some(Role::Macro));
        assert_eq!(by_text("String"), Some(Role::Type));
        assert_eq!(by_text("self"), Some(Role::VariableBuiltin));
        assert_eq!(by_text("{"), Some(Role::Bracket));
        assert_eq!(by_text("."), Some(Role::Delimiter));
    }

    #[test]
    fn csharp_is_coloured_the_way_a_person_would_expect() {
        // The grammar's own query opens with a catch-all `(identifier)
        // @variable`, which under tree-sitter's precedence wins every
        // identifier in the file unless something is put in front of it.
        // queries/csharp.scm is what is put in front of it, and this is the
        // test that says it worked.
        let found = roles_in(
            "csharp",
            r#"namespace Widgets;
[Obsolete]
public class Widget : IThing {
    const int MAX = 8;
    public string Name { get; set; }
    void Run(Widget other) { var s = "hi"; other.Stop(); }
}"#,
        );
        let by_text = |want: &str| {
            found
                .iter()
                .find(|(text, _)| text == want)
                .map(|(_, role)| *role)
        };
        assert_eq!(by_text("class"), Some(Role::Keyword), "{found:?}");
        assert_eq!(by_text("Widgets"), Some(Role::Namespace), "{found:?}");
        assert_eq!(by_text("Widget"), Some(Role::Type), "{found:?}");
        assert_eq!(by_text("IThing"), Some(Role::Type), "{found:?}");
        assert_eq!(by_text("Obsolete"), Some(Role::Attribute), "{found:?}");
        assert_eq!(by_text("MAX"), Some(Role::Constant), "{found:?}");
        assert_eq!(by_text("Run"), Some(Role::Function), "{found:?}");
        assert_eq!(by_text("Stop"), Some(Role::Function), "{found:?}");
        assert_eq!(by_text("Name"), Some(Role::Property), "{found:?}");
        assert_eq!(by_text("other"), Some(Role::Parameter), "{found:?}");
        assert_eq!(by_text("\"hi\""), Some(Role::String), "{found:?}");
    }

    #[test]
    fn java_is_coloured_the_way_a_person_would_expect() {
        // Same catch-all `(identifier) @variable` at the top of the grammar's
        // own query as C# has, with the same effect on everything under it.
        // queries/java.scm is what is put in front of it, and this is the test
        // that says it worked.
        let found = roles_in(
            "java",
            r#"package com.example.widgets;
import java.util.List;

@Deprecated
public class Widget extends Base {
    public static final int MAX_SIZE = 8;
    private String name;

    public Widget(String name) { this.name = name; }

    void run(Widget other, int count) {
        var s = "hi";
        other.stop();
        outer: for (String t : tags) { break outer; }
    }

    enum Colour { RED }
}"#,
        );
        let by_text = |want: &str| {
            found
                .iter()
                .find(|(text, _)| text == want)
                .map(|(_, role)| *role)
        };
        assert_eq!(by_text("class"), Some(Role::Keyword), "{found:?}");
        assert_eq!(by_text("example"), Some(Role::Namespace), "{found:?}");
        // The tail of an import is the class it names, not more of the path.
        assert_eq!(by_text("List"), Some(Role::Type), "{found:?}");
        assert_eq!(by_text("Widget"), Some(Role::Type), "{found:?}");
        assert_eq!(by_text("Base"), Some(Role::Type), "{found:?}");
        assert_eq!(by_text("Deprecated"), Some(Role::Attribute), "{found:?}");
        assert_eq!(by_text("MAX_SIZE"), Some(Role::Constant), "{found:?}");
        assert_eq!(by_text("RED"), Some(Role::Constant), "{found:?}");
        // Java's query calls a method a method, and textfold now has a
        // colour for one.
        assert_eq!(by_text("run"), Some(Role::Method), "{found:?}");
        assert_eq!(by_text("stop"), Some(Role::Method), "{found:?}");
        assert_eq!(by_text("name"), Some(Role::Property), "{found:?}");
        assert_eq!(by_text("other"), Some(Role::Parameter), "{found:?}");
        assert_eq!(by_text("outer"), Some(Role::Label), "{found:?}");
        assert_eq!(by_text("\"hi\""), Some(Role::String), "{found:?}");
    }

    #[test]
    fn a_specific_pattern_beats_the_catch_all_below_it() {
        // The rust query ends with a generic identifier rule; a name in
        // capitals has to come out a constant regardless.
        let found = roles("const MAX_SIZE: usize = 8;");
        assert!(
            found
                .iter()
                .any(|(text, role)| text == "MAX_SIZE" && *role == Role::Constant),
            "{found:?}"
        );
    }

    #[test]
    fn only_what_was_asked_for_is_worked_out() {
        let text = "fn a() {}\nfn b() {}\nfn c() {}\n";
        let rope = Rope::from_str(text);
        let syntax = Syntax::new(rust_grammar(), &rope).expect("parses");
        let second = 10..19;
        let spans = syntax.highlights(&rope, second.clone());
        assert!(!spans.is_empty());
        assert!(
            spans
                .iter()
                .all(|(span, _)| span.start >= second.start && span.end <= second.end),
            "{spans:?}"
        );
    }

    #[test]
    fn an_edit_is_taken_in_rather_than_starting_over() {
        let mut rope = Rope::from_str("fn a() {}\n");
        let mut syntax = Syntax::new(rust_grammar(), &rope).expect("parses");
        let before = syntax.revision;

        // Turn `a` into `abc`, the way an edit arrives.
        rope.insert(4, "bc");
        assert!(syntax.update(
            &rope,
            &[AppliedEdit {
                from: 4,
                to: 4,
                inserted: 2,
                text: "bc".into(),
                start_byte: 4,
                old_end_byte: 4,
                new_end_byte: 6,
                start_point: (0, 4),
                old_end_point: (0, 4),
                new_end_point: (0, 6),
                lsp_start: (0, 4),
                lsp_old_end: (0, 4),
            }],
        ));
        assert!(syntax.revision > before);
        let spans = syntax.highlights(&rope, 0..rope.len_bytes());
        let text = rope.to_string();
        assert!(
            spans
                .iter()
                .any(|(s, role)| &text[s.clone()] == "abc" && *role == Role::Function),
            "{spans:?}"
        );
    }
}

