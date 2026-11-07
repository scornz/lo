; Lo's additions to tree-sitter-rust's tags query, for what it leaves out. The grammar's own
; patterns come first, so they win when both tag the same name.

; Trait methods without a body.
(function_signature_item
    name: (identifier) @name) @definition.method

; Constants and statics.
(const_item
    name: (identifier) @name) @definition.constant

(static_item
    name: (identifier) @name) @definition.constant

; Enum variants and struct fields.
(enum_variant
    name: (identifier) @name) @definition.variant

(field_declaration
    name: (field_identifier) @name) @definition.field

; Associated types in traits.
(associated_type
    name: (type_identifier) @name) @definition.type

; Calls through a path or with a turbofish: `Circle::new()`, `size_of::<T>()`.
(call_expression
    function: (scoped_identifier
        name: (identifier) @name)) @reference.call

(call_expression
    function: (generic_function
        function: [
            (identifier) @name
            (scoped_identifier name: (identifier) @name)
            (field_expression field: (field_identifier) @name)
        ])) @reference.call

; Macros through a path: `tracing::info!()`.
(macro_invocation
    macro: (scoped_identifier
        name: (identifier) @name)) @reference.call

; Types wherever they're named.
(type_identifier) @name @reference.type

; Every part of other paths: `Kind::Round`, `std::mem`, `use crate::stacks::Stack`.
(scoped_identifier
    path: (identifier) @name) @reference.path

(scoped_identifier
    name: (identifier) @name) @reference.path

; Fields read, written or set: `self.radius`, `Circle { radius: 1.0 }`.
(field_expression
    field: (field_identifier) @name) @reference.field

(field_initializer
    field: (field_identifier) @name) @reference.field
