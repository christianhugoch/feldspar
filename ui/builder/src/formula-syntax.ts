// A formula's syntax, checked by parsing it rather than by evaluating it.
//
// v1's builder checks a formula as it is typed by constructing a function from
// it and never calling that function:
// - `Function("return " + fml)` in `View.js`, `ViewLink.js` and `utils.js`
//   (vendored);
// - `AsyncFunction("return " + val)` in `saltcorn-common.js`'s
//   `validate_expression_elem` and `validate_bool_expression_elem`.
//
// Constructing a function from a string is `eval` to a Content-Security-Policy,
// and the builder's has no `'unsafe-eval'` (`security.rs`). So in a browser every
// one of those checks threw the policy's refusal, and the builder showed
// "Refused to evaluate a string as JavaScript" under every formula setting, valid
// or not, with a violation report each time. The jsdom tests could not see it:
// jsdom enforces no policy. The by-hand definition of done did (TODO "The
// builder" 10.4).
//
// The same check here is a parse of the source the constructor would have
// compiled, with acorn. Nothing is evaluated, so the policy keeps `script-src
// 'self'` and the checks keep v1's meaning: a formula that would not compile is
// refused with the parser's message, and one that would is accepted. The
// messages are acorn's wording, not V8's ("Unexpected token", not "Unexpected
// token ')'").

import { parse, parseExpressionAt } from "acorn";

/** What calling a syntax-checked function says. v1's three vendored checks
 * discard the function, so nothing should ever see it. */
export const NOT_EVALUATED = "The builder checks a formula's syntax and does not run it here.";

/** Throw a `SyntaxError` if `Function(...params, body)` (or `AsyncFunction`,
 * with `async`) would. */
export function checkFormulaSyntax(params: readonly string[], body: string, async = false): void {
  // The wrapper the Function constructor compiles (ECMA-262
  // CreateDynamicFunction): parameters, then the body on lines of its own, so
  // a `//` comment at the end of either cannot swallow the closing brace.
  const source = `(${async ? "async " : ""}function anonymous(${params.join(",")}\n) {\n${body}\n})`;
  let program;
  try {
    program = parse(source, { ecmaVersion: "latest", sourceType: "script" });
  } catch (error) {
    // acorn appends "(line:column)" in the wrapper's coordinates, which
    // describe nothing the admin typed.
    const message = error instanceof Error ? error.message.replace(/ \(\d+:\d+\)$/, "") : String(error);
    throw new SyntaxError(message);
  }
  // The constructor parses the parameters and the body each on their own, so
  // text that closes the wrapper and opens another (`1 }); (function () {`) is
  // a syntax error there. Here that source parses as a program of several
  // statements, so the program must be the one function, whole.
  const [only, ...rest] = program.body;
  const whole =
    rest.length === 0 &&
    only?.type === "ExpressionStatement" &&
    only.expression.type === "FunctionExpression" &&
    only.expression.start === 1 &&
    only.expression.end === source.length - 1;
  if (!whole) throw new SyntaxError("Unexpected token");
}

/** Whether `expression` is a constant that is not a boolean, as far as a parse
 * can tell: a literal number, string, `null` or regular expression. v1's
 * `validate_bool_expression_elem` finds constants by running the formula, which
 * is `eval`; `1 > 0` is a constant this does not recognise, and it is accepted. */
export function nonBooleanConstant(expression: string): boolean {
  try {
    const node = parseExpressionAt(expression, 0, { ecmaVersion: "latest" });
    if (expression.slice(node.end).trim() !== "") return false;
    return node.type === "Literal" && typeof (node as { value?: unknown }).value !== "boolean";
  } catch {
    return false;
  }
}

/** The vendored files' `Function` (`build.mjs` imports it under that name into
 * each of them, and no other file): v1's syntax check, without the `eval`. */
export function syntaxCheckedFunction(...args: unknown[]): () => never {
  const body = args.length ? String(args[args.length - 1]) : "";
  checkFormulaSyntax(args.slice(0, -1).map(String), body);
  return () => {
    throw new Error(NOT_EVALUATED);
  };
}
