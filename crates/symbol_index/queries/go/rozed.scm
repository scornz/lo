; rozed's additions to tree-sitter-go's tags query, for what it leaves out. The grammar's own
; patterns come first, so they win when both tag the same name.

; Package-level constants and variables.
(source_file
  (const_declaration
    (const_spec
      name: (identifier) @name) @definition.constant))

(source_file
  (var_declaration
    (var_spec
      name: (identifier) @name) @definition.variable))

(source_file
  (var_declaration
    (var_spec_list
      (var_spec
        name: (identifier) @name) @definition.variable)))

; Interface methods and struct fields.
(method_elem
  name: (field_identifier) @name) @definition.method

(field_declaration
  name: (field_identifier) @name) @definition.field

; Fields and package members used without a call: `c.Radius`, `math.Pi`.
(selector_expression
  field: (field_identifier) @name) @reference.field
