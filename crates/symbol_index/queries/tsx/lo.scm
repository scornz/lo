; Lo's additions for JSX, which only the TSX grammar has: components used as elements. HTML
; elements, which are lowercase, aren't references.

(jsx_opening_element
  name: (identifier) @name
  (#match? @name "^[A-Z]")) @reference.call

(jsx_self_closing_element
  name: (identifier) @name
  (#match? @name "^[A-Z]")) @reference.call
