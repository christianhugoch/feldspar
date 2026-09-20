; JSX: the `<T text="…">` element, the attributes the lint reads, and the text
; nodes it reads (tasks 2.1, 2.2).
;
; Four patterns, matched on `pattern_index` rather than on capture names, so an
; element's attributes are walked once in Rust instead of once per combination
; of the `attribute` field — which is what a single pattern over a multiple
; field would produce.
(jsx_opening_element name: (identifier) @element) @element_node

(jsx_self_closing_element name: (identifier) @element) @element_node

(jsx_attribute (property_identifier) @attribute (string) @value)

(jsx_text) @text
