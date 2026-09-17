; Tags for Python, for the repo map (TODO §11).
;
; Vendored from tree-sitter-python 0.25.0 (queries/tags.scm), with imported
; names added as references.
;
; The MIT License (MIT)
; Copyright (c) 2016 Max Brunsfeld
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

(module (expression_statement (assignment left: (identifier) @name) @definition.constant))

(class_definition
  name: (identifier) @name) @definition.class

(function_definition
  name: (identifier) @name) @definition.function

(call
  function: [
      (identifier) @name
      (attribute
        attribute: (identifier) @name)
  ]) @reference.call

; --- Added for the repo map ---------------------------------------------------

(import_from_statement
  name: (dotted_name (identifier) @name)) @reference.import
