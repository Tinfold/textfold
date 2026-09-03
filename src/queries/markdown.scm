; Markdown's blocks: headings, fences, lists, quotes, tables.
;
; Instead of the grammar's own query rather than in front of it, which is the
; opposite of every other file here. The shipped one is written against
; nvim-treesitter's capture names — `@text.title`, `@text.literal`,
; `@text.uri` — and textfold's colours are named for code, so almost all of it
; landed on nothing: a markdown file arrived with its `#` and its `-` coloured
; and every heading, fence and link in the colour of prose. Saying it in the
; names we already have costs one file and colours the whole document.
;
; What is inside a line of prose — the `**bold**`, the `` `code` ``, the
; links — is not here, because the block grammar does not see it. It hands the
; line over as one `inline` node and a second grammar reads that; see
; `markdown-inline.scm` and `Grammar::inside`.

; ---- Headings ----

; The line itself, both ways of writing one. A heading names the section
; under it the way a signature names a function, and it is what somebody
; scrolling is reading, so it takes the brightest colour there is.
(atx_heading (inline) @function)
(setext_heading (paragraph) @function)

[
  (atx_h1_marker)
  (atx_h2_marker)
  (atx_h3_marker)
  (atx_h4_marker)
  (atx_h5_marker)
  (atx_h6_marker)
  (setext_h1_underline)
  (setext_h2_underline)
] @punctuation

; ---- Code ----

; A fenced or indented block, whole. Nothing here parses what is inside one —
; a fence saying `rust` is still markdown to the grammar — so it is coloured
; as the one thing it certainly is: not prose. The delimiters and the language
; are inside it and say so over the top.
[
  (fenced_code_block)
  (indented_code_block)
] @string

(fenced_code_block_delimiter) @punctuation.delimiter
(info_string (language) @type)

; ---- Lists, quotes, rules ----

; `block_continuation` is the `> ` at the start of the second line of a quoted
; paragraph, and the indent under a list item. The same mark as the one that
; opened the block, so the same colour.
[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
  (thematic_break)
  (block_quote_marker)
  (block_continuation)
] @punctuation

; A box with something in it has been done, which is worth telling apart from
; one waiting to be.
(task_list_marker_checked) @constant
(task_list_marker_unchecked) @punctuation

; ---- Link reference definitions ----

; `[id]: https://example.com "Title"`, the block-level half of a link. The
; inline half of the same thing is in the other file, coloured the same way.
(link_label) @label
(link_destination) @string.special
(link_title) @string

; ---- Tables ----

; The row of dashes and colons is punctuation holding the table up; the cells
; are prose and are left as prose.
(pipe_table_delimiter_row) @punctuation
(pipe_table_header (pipe_table_cell) @property)
"|" @punctuation.delimiter

; ---- Escapes ----

[
  (backslash_escape)
  (entity_reference)
  (numeric_character_reference)
] @string.escape
