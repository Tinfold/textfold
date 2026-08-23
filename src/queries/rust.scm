; textfold's corrections to tree-sitter-rust's own highlights.
;
; Read before the grammar's file rather than after it, because among patterns
; matching one node the first written is the one that wins. A rule here is a
; rule the grammar never gets to disagree with.

; The grammar's own rule for SCREAMING_CASE names carries a stray quote in its
; pattern — `"^[A-Z][A-Z\\d_]+$'"` — so it can never match anything, and every
; constant in every Rust file falls through to the rule below it and comes out
; colored as an enum variant. This is that rule, spelled correctly.
((identifier) @constant
 (#match? @constant "^[A-Z][A-Z0-9_]*$"))

; `Self` is a type wherever it appears, including where the grammar sees only
; an identifier.
((identifier) @type
 (#eq? @type "Self"))
