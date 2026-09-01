; YAML, read before the grammar's own query.
;
; The shipped file opens with `(string_scalar) @string`, and in YAML a plain
; scalar is *everything*: the keys as well as the values. Its own rules for
; picking keys out come further down the same file, and among patterns that
; claim the same bytes the first one written wins — so the catch-all takes
; them and a YAML file arrives with every line in one colour. These go first.

; ---- Keys ----

; The merge key, before the rule that would call it an ordinary one. `<<` is
; not a field, it is an instruction to fold another mapping in here, and it is
; worth being able to see that at a glance in a file full of them.
((block_mapping_pair
   key: (flow_node
     (plain_scalar
       (string_scalar) @keyword)))
 (#eq? @keyword "<<"))

(block_mapping_pair
  key: (flow_node
    [
      (double_quote_scalar)
      (single_quote_scalar)
      (plain_scalar (string_scalar))
    ] @property))

; `{ name: value }` and `[ name: value ]`, where the pair is a flow_pair
; rather than a block one.
(flow_pair
  key: (flow_node
    [
      (double_quote_scalar)
      (single_quote_scalar)
      (plain_scalar (string_scalar))
    ] @property))

; ---- Values the shipped query says nothing about ----

; A date. The grammar has a `timestamp_scalar` node and the schema this crate
; is built with never produces one, so YAML's most common non-string value
; arrives as prose. It has one shape and nothing else has it. Below the key
; rules, so a mapping whose key is a date is still a key.
((plain_scalar
   (string_scalar) @number)
 (#match? @number "^[0-9]{4}-[0-9]{2}-[0-9]{2}([Tt ][^ ].*)?$"))

; `"a\nb"`. Inside a string and more specific than one, which is what puts it
; on top of the `@string` covering the whole scalar.
(escape_sequence) @string.escape

; ---- Directives ----

; `%YAML 1.2` and `%TAG !e! tag:example.com,2000:`. The whole line is already
; an attribute; these say which part of it is the version and which is the
; handle being defined, the way a key and a value are told apart everywhere
; else in the file.
(yaml_version) @number
(tag_handle) @type
(tag_prefix) @string.special
