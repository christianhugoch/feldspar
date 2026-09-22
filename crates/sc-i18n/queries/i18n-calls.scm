; The `t(…)` / `tc(…)` call sites (task 2.1).
;
; Deliberately unfiltered by name: the query finds *every* call of a bare
; identifier and `extract.rs` decides which ones are ours. A `#eq?` predicate
; here would move that decision into a file that cannot say why a call named `t`
; whose argument is not a literal is an error rather than a skip.
(call_expression
  function: (identifier) @function
  arguments: (arguments) @arguments) @call
