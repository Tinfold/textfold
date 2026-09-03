; What is inside a line of markdown: the emphasis, the code spans, the links.
;
; A second grammar, run inside the `inline` nodes the block grammar hands over
; whole — see `Grammar::inside` and `markdown.scm`, which is the other half of
; this. Instead of the grammar's own query, for the reason given there.
;
; Bold and italic are colours here rather than weights, because a role is a
; colour and nothing else. Two of them, so that `**this**` and `*this*` are
; still telling you two different things.

(strong_emphasis) @keyword
(emphasis) @type
(emphasis_delimiter) @punctuation

; Struck-through text is text somebody has taken back, which is the one thing
; on the page that is meant to read as already gone.
(strikethrough) @comment

; ---- Code ----

(code_span) @string
(code_span_delimiter) @punctuation.delimiter

; ---- Links and images ----

; The parts of a link, in the order you read them: what it says, where it
; goes, and what it is called if it is being fetched from a definition
; further down.
[
  (link_text)
  (link_label)
  (image_description)
] @label

[
  (link_destination)
  (uri_autolink)
  (email_autolink)
] @string.special

(link_title) @string

(image ["!" "[" "]" "(" ")"] @punctuation.delimiter)
(inline_link ["[" "]" "(" ")"] @punctuation.delimiter)
(shortcut_link ["[" "]"] @punctuation.delimiter)
(collapsed_reference_link ["[" "]"] @punctuation.delimiter)
(full_reference_link ["[" "]"] @punctuation.delimiter)

; ---- Everything else that is not prose ----

; Markdown lets HTML through, and a `<br>` in the middle of a paragraph is
; markup rather than something to read.
(html_tag) @tag

[
  (backslash_escape)
  (hard_line_break)
  (entity_reference)
  (numeric_character_reference)
] @string.escape

; `$x^2$` and `$$…$$` — TeX, which is a language and not this one.
[
  (latex_block)
  (latex_span_delimiter)
] @macro
