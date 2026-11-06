; rozed's additions to tree-sitter-python's tags query, for what it leaves out. The grammar's own
; patterns come first, so they win when both tag the same name.

; Base classes: `class Circle(shapes.Shape)`.
(class_definition
  superclasses: (argument_list
    [
      (identifier) @name
      (attribute attribute: (identifier) @name)
    ] @reference.class))

; Decorators without arguments: `@dataclass`. With arguments, they're already calls.
(decorator
  [
    (identifier) @name
    (attribute attribute: (identifier) @name)
  ] @reference.call)

; Type annotations: `radius: float`, `-> Area`, `list[Circle]`.
(type
  [
    (identifier) @name
    (attribute attribute: (identifier) @name)
  ] @reference.type)
