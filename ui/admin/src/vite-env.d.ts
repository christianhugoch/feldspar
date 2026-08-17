/// <reference types="vite/client" />

// Vite's ambient module declarations: what `import logo from "./x.svg"` and the
// other asset imports resolve to. Without this the logo import is an untyped
// module and `tsc --noEmit` fails.

// Monaco's side-effect entry points ship as JavaScript with no declarations —
// they *register* the editor's contributions rather than exporting anything, so
// there is nothing for a `.d.ts` to describe. `CodeEditor.tsx` imports them for
// that effect alone; this is what makes an import of a module with no types not
// an error.
declare module "monaco-editor/esm/vs/editor/editor.all.js";
