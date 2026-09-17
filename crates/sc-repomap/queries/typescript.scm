; Tags for TypeScript, for the repo map (TODO §11).
;
; Vendored from tree-sitter-javascript 0.25.0 (queries/tags.scm), with the
; exported-constant definitions a project's map needs added, then
; tree-sitter-typescript's own tags below.
;
; The MIT License (MIT)
; Copyright (c) 2014 Max Brunsfeld
;
; Permission is hereby granted, free of charge, to any person obtaining a copy of
; this software and associated documentation files (the "Software"), to deal in
; the Software without restriction, including without limitation the rights to
; use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
; the Software, and to permit persons to whom the Software is furnished to do so,
; subject to the following conditions: The above copyright notice and this
; permission notice shall be included in all copies or substantial portions of
; the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
; EXPRESS OR IMPLIED.

(
  (comment)* @doc
  .
  (method_definition
    name: (property_identifier) @name) @definition.method
  (#not-eq? @name "constructor")
  (#strip! @doc "^[\\s\\*/]+|^[\\s\\*/]$")
  (#select-adjacent! @doc @definition.method)
)

(
  (comment)* @doc
  .
  [
    (class
      name: (_) @name)
    (class_declaration
      name: (_) @name)
  ] @definition.class
  (#strip! @doc "^[\\s\\*/]+|^[\\s\\*/]$")
  (#select-adjacent! @doc @definition.class)
)

(
  (comment)* @doc
  .
  [
    (function_expression
      name: (identifier) @name)
    (function_declaration
      name: (identifier) @name)
    (generator_function
      name: (identifier) @name)
    (generator_function_declaration
      name: (identifier) @name)
  ] @definition.function
  (#strip! @doc "^[\\s\\*/]+|^[\\s\\*/]$")
  (#select-adjacent! @doc @definition.function)
)

(
  (comment)* @doc
  .
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name
      value: [(arrow_function) (function_expression)]) @definition.function)
  (#strip! @doc "^[\\s\\*/]+|^[\\s\\*/]$")
  (#select-adjacent! @doc @definition.function)
)

(
  (comment)* @doc
  .
  (variable_declaration
    (variable_declarator
      name: (identifier) @name
      value: [(arrow_function) (function_expression)]) @definition.function)
  (#strip! @doc "^[\\s\\*/]+|^[\\s\\*/]$")
  (#select-adjacent! @doc @definition.function)
)

(assignment_expression
  left: [
    (identifier) @name
    (member_expression
      property: (property_identifier) @name)
  ]
  right: [(arrow_function) (function_expression)]
) @definition.function

(pair
  key: (property_identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function

(
  (call_expression
    function: (identifier) @name) @reference.call
  (#not-match? @name "^(require)$")
)

(call_expression
  function: (member_expression
    property: (property_identifier) @name)
  arguments: (_) @reference.call)

(new_expression
  constructor: (_) @name) @reference.class

(export_statement value: (assignment_expression left: (identifier) @name right: ([
 (number)
 (string)
 (identifier)
 (undefined)
 (null)
 (new_expression)
 (binary_expression)
 (call_expression)
]))) @definition.constant

; --- Added for the repo map ---------------------------------------------------



(export_statement
  declaration: (lexical_declaration
    (variable_declarator
      name: (identifier) @name
      value: [(object) (array) (string) (number) (call_expression) (new_expression)]) @definition.constant))

; --- TypeScript ---------------------------------------------------------------
;
; Vendored from tree-sitter-typescript 0.23.2 (queries/tags.scm), which is meant
; to be used together with JavaScript's: the two are one file here. Type
; aliases, enums and every type name used are added for the repo map.
;
; The MIT License (MIT)
; Copyright (c) 2017 Max Brunsfeld and the tree-sitter-typescript contributors
(function_signature
  name: (identifier) @name) @definition.function

(method_signature
  name: (property_identifier) @name) @definition.method

(abstract_method_signature
  name: (property_identifier) @name) @definition.method

(abstract_class_declaration
  name: (type_identifier) @name) @definition.class

(module
  name: (identifier) @name) @definition.module

(interface_declaration
  name: (type_identifier) @name) @definition.interface

(type_annotation
  (type_identifier) @name) @reference.type

(new_expression
  constructor: (identifier) @name) @reference.class

(type_alias_declaration
  name: (type_identifier) @name) @definition.type

(enum_declaration
  name: (identifier) @name) @definition.enum

(type_identifier) @name @reference.type
