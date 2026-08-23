; textfold's corrections to tree-sitter-java's own highlights.
;
; Read before the grammar's file rather than after it, because among patterns
; matching one node the first written is the one that wins. A rule here is a
; rule the grammar never gets to disagree with.

; The grammar's file opens with a catch-all `(identifier) @variable` and only
; then says what the identifiers actually are. Under tree-sitter's precedence
; that catch-all wins every one of them, so a Java file comes out with its
; method names, annotations and constants all coloured as plain variables.
; Types escape it — `(type_identifier)` is a node of its own — which is what
; makes the damage easy to miss. Most of what follows is the grammar's own
; rules hoisted above it so they get a say; the rest is what it never had.

;; Methods

(method_declaration name: (identifier) @function)
(method_invocation name: (identifier) @function)
(annotation_type_element_declaration name: (identifier) @function)
(method_reference (identifier) @function .)

;; Constructors

(constructor_declaration name: (identifier) @constructor)

;; Annotations

(annotation name: (identifier) @attribute)
(marker_annotation name: (identifier) @attribute)
(annotation name: (scoped_identifier name: (identifier) @attribute))
(marker_annotation name: (scoped_identifier name: (identifier) @attribute))

;; Types
;
; The declarations name themselves with a plain `(identifier)`, so all of them
; need saying here. `record` the grammar's own file never had at all.

(class_declaration name: (identifier) @type)
(interface_declaration name: (identifier) @type)
(enum_declaration name: (identifier) @type)
(record_declaration name: (identifier) @type)
(annotation_type_declaration name: (identifier) @type)

; A capitalised name in front of a dot is a class being reached through —
; `Files.readString`, `System.out` — which is convention rather than grammar,
; and the only way to tell a static call from a call on a variable.
((method_invocation object: (identifier) @type)
 (#match? @type "^[A-Z]"))
((field_access object: (identifier) @type)
 (#match? @type "^[A-Z]"))
((method_reference . (identifier) @type)
 (#match? @type "^[A-Z]"))

;; Packages and imports
;
; `com.example.widgets` nests to the left, so the parts are captured one at a
; time: capturing the whole `scoped_identifier` would lose to the identifiers
; inside it, an inner node beating the node containing it. A capitalised last
; part is the class an import names rather than more of the path.

((scoped_identifier name: (identifier) @type)
 (#match? @type "^[A-Z]"))
(scoped_identifier scope: (identifier) @module)
(scoped_identifier name: (identifier) @module)
(package_declaration (identifier) @module)
(import_declaration (identifier) @module)

;; Constants
;
; Ahead of the fields and parameters below, so that a name in capitals is a
; constant wherever it turns up rather than only where the grammar has no
; better idea.

(enum_constant name: (identifier) @constant)
((identifier) @constant
 (#match? @constant "^_*[A-Z][A-Z\\d_]+$"))

;; Fields

(field_declaration declarator: (variable_declarator name: (identifier) @property))
(field_access field: (identifier) @property)

;; Parameters

(formal_parameter name: (identifier) @variable.parameter)
(spread_parameter (variable_declarator name: (identifier) @variable.parameter))
(catch_formal_parameter name: (identifier) @variable.parameter)
(lambda_expression parameters: (identifier) @variable.parameter)
(inferred_parameters (identifier) @variable.parameter)

;; Labels

(labeled_statement (identifier) @label)
(break_statement (identifier) @label)
(continue_statement (identifier) @label)
