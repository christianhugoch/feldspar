// `ckeditor4-react`, as the vendored builder imports it (TODO "The builder" 7.3).
// `build.mjs` resolves the specifier here for files in `vendor/` only, and this
// file's own import of it is the real package.
//
// **Why.** The real `CKEditor` loads the editor from `cdn.ckeditor.com` unless it
// is given an `editorUrl`, or `window.CKEDITOR` already exists. `Text.js` gives no
// URL, so whether the admin's browser fetched CKEditor from a third party would
// depend on whether the host document happened to load it first. This points
// every instance at `dist/ckeditor/`, beside this bundle: v1's CKEditor 4.16.2,
// on this origin, whichever script arrives first.

import { CKEditor as RealCKEditor } from "ckeditor4-react";

export * from "ckeditor4-react";

/** The vendored CKEditor, relative to this bundle. */
export const CKEDITOR_URL = new URL("./ckeditor/ckeditor.js", import.meta.url).href;

export function CKEditor(props: Parameters<typeof RealCKEditor>[0]) {
  return <RealCKEditor {...props} editorUrl={CKEDITOR_URL} />;
}
