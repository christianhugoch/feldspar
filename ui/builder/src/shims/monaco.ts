// Monaco, as this bundle carries it (TODO "The builder" 7.3).
//
// v1 serves a prebuilt Monaco from `/monaco` and loads it through the AMD
// loader, which is a CDN pattern in all but name. Here it is the ESM
// `monaco-editor` at v1's pinned version, built into its own chunk. That chunk is
// imported only when a code editor first mounts, so opening the builder does not
// pay for it.
//
// **Workers are same-origin files beside the bundle** (`build.mjs` builds
// `editor.worker.js` and `ts.worker.js`), found relative to this chunk's own URL.
// That is what the builder's CSP allows (`worker-src 'self'`), and it needs no
// `blob:`.
//
// The languages are the ones `MonacoEditor.js`'s `mimeToMonacoLanguage` names.
// TypeScript and JavaScript get the language service, because the builder's
// formula editors configure `typescriptDefaults`. The rest get a tokenizer only,
// which is also all v1's editors use them for.

import * as monaco from "monaco-editor/esm/vs/editor/editor.api.js";
import "monaco-editor/esm/vs/editor/editor.all.js";
import "monaco-editor/esm/vs/language/typescript/monaco.contribution.js";
import "monaco-editor/esm/vs/basic-languages/typescript/typescript.contribution.js";
import "monaco-editor/esm/vs/basic-languages/javascript/javascript.contribution.js";
import "monaco-editor/esm/vs/basic-languages/html/html.contribution.js";
import "monaco-editor/esm/vs/basic-languages/css/css.contribution.js";
import "monaco-editor/esm/vs/basic-languages/sql/sql.contribution.js";
import "monaco-editor/esm/vs/basic-languages/python/python.contribution.js";
import "monaco-editor/esm/vs/basic-languages/yaml/yaml.contribution.js";
import "monaco-editor/esm/vs/basic-languages/xml/xml.contribution.js";
import "monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution.js";

(self as unknown as { MonacoEnvironment: unknown }).MonacoEnvironment = {
  getWorker(_workerId: string, label: string) {
    const file = label === "typescript" || label === "javascript" ? "ts.worker.js" : "editor.worker.js";
    return new Worker(new URL(`./${file}`, import.meta.url), { name: label });
  },
};

export { monaco };
