; textfold's corrections to tree-sitter-c-sharp's own highlights.
;
; Read before the grammar's file rather than after it, because among patterns
; matching one node the first written is the one that wins. A rule here is a
; rule the grammar never gets to disagree with.

; The grammar's file opens with a catch-all `(identifier) @variable` and only
; then says what the identifiers actually are. Under tree-sitter's precedence
; that catch-all wins every one of them, so a C# file comes out with its class
; names, method names and types all coloured as plain variables. These are the
; grammar's own rules, hoisted above it so they get a say.

;; Methods

(method_declaration name: (identifier) @function)
(local_function_statement name: (identifier) @function)
(invocation_expression (member_access_expression name: (identifier) @function))
(invocation_expression function: (identifier) @function)

;; Types

(interface_declaration name: (identifier) @type)
(class_declaration name: (identifier) @type)
(enum_declaration name: (identifier) @type)
(struct_declaration (identifier) @type)
(record_declaration (identifier) @type)
(namespace_declaration name: (identifier) @module)
; The grammar's file knows only the block form; a file-scoped `namespace X;`
; is a different node and is what modern C# is written with.
(file_scoped_namespace_declaration name: (identifier) @module)
(file_scoped_namespace_declaration name: (qualified_name) @module)
(namespace_declaration name: (qualified_name) @module)

(constructor_declaration name: (identifier) @constructor)
(destructor_declaration name: (identifier) @constructor)
(object_creation_expression type: (identifier) @type)

(generic_name (identifier) @type)
(type_parameter (identifier) @type)
(parameter type: (identifier) @type)
(type_argument_list (identifier) @type)
(as_expression right: (identifier) @type)
(is_expression right: (identifier) @type)
(base_list (identifier) @type)
(type_parameter_constraints_clause (identifier) @type)
(_ type: (identifier) @type)

;; Attributes, parameters, members

(attribute name: (identifier) @attribute)
(parameter name: (identifier) @variable.parameter)
(enum_member_declaration (identifier) @property)
(property_declaration name: (identifier) @property)
(event_declaration name: (identifier) @property)
(member_access_expression name: (identifier) @property)

; A name in capitals is a constant by convention, and the grammar has no rule
; that says so.
((identifier) @constant
 (#match? @constant "^[A-Z][A-Z0-9_]*$"))
