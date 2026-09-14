// The vendored builder, as `src/` sees it. It is v1's JSX with no types of its
// own, so its one entry point is declared here; `build.mjs` resolves the
// specifier to `vendor/saltcorn-builder/index.js`.
declare module "@saltcorn/builder" {
  /** v1's `renderBuilder(id, options, layout, mode)`: `options` and `layout` are
   * URI-encoded JSON, as `saltcorn-markup/builder.ts` passes them. */
  export function renderBuilder(id: string, options: string, layout: string, mode: string): void;
}
