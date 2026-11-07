; Lo's additions to the TypeScript and JavaScript tags queries, for what they leave out. Their
; own patterns come first, so they win when both tag the same name. For the TypeScript and TSX
; grammars, and so for JavaScript, which Zed reads with the TSX grammar.

; Type aliases, enums and namespaces.
(type_alias_declaration
  name: (type_identifier) @name) @definition.type

(enum_declaration
  name: (identifier) @name) @definition.enum

(internal_module
  name: (identifier) @name) @definition.module

; Top-level constants and variables. Those holding functions are already functions.
(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name) @definition.constant))

(program
  (export_statement
    declaration: (lexical_declaration
      (variable_declarator
        name: (identifier) @name) @definition.constant)))

; Class fields and interface properties.
(public_field_definition
  name: (property_identifier) @name) @definition.field

(property_signature
  name: (property_identifier) @name) @definition.field

; Classes extended: `class Circle extends Base`.
(extends_clause
  value: (identifier) @name) @reference.class

; Types wherever they're named, not only in annotations: `implements Shape`, `Shape[]`.
(type_identifier) @name @reference.type
