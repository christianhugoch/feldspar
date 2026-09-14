// `@monaco-editor/react`, as the vendored builder imports it (TODO "The builder"
// 7.3). `build.mjs` resolves the specifier here for files in `vendor/` only, and
// this file's own import of it is the real package.
//
// **Why.** The real package's loader injects `<script src="<paths.vs>/loader.js">`
// and loads Monaco through AMD. By default `paths.vs` is jsDelivr, and
// `MonacoEditor.js` sets it to v1's `/monaco`. Either way it is code from a URL
// this server does not serve, under a CSP that would refuse it. The loader skips
// all of that when it is handed a Monaco instance, so this module hands it the
// ESM Monaco this bundle carries (`./monaco.ts`), and drops `paths`.
//
// **When.** The loader's `config` *replaces* the instance it holds with whatever
// the call carries, so `MonacoEditor.js`'s module-level
// `loader.config({ paths })` would clear an instance given before it. The
// instance is therefore given at the last moment: the first time an editor
// mounts, after the Monaco chunk has loaded. Until then the editor renders an
// empty box of its size.

import { useEffect, useState, type ComponentType } from "react";
import RealEditor, {
  DiffEditor as RealDiffEditor,
  loader as realLoader,
  type Monaco,
} from "@monaco-editor/react";

export * from "@monaco-editor/react";

let bundled: Promise<Monaco> | null = null;

/** Load the bundled Monaco, once, and give it to the loader. */
export function loadMonaco(): Promise<Monaco> {
  bundled ??= import("./monaco").then(({ monaco }) => {
    realLoader.config({ monaco });
    return monaco as unknown as Monaco;
  });
  return bundled;
}

type LoaderConfig = Parameters<typeof realLoader.config>[0];

/** The loader, with `paths` dropped and `init` answered by the bundled Monaco. */
export const loader: typeof realLoader = {
  ...realLoader,
  config(config: LoaderConfig) {
    const { paths: _paths, ...rest } = config as LoaderConfig & { paths?: unknown };
    if (Object.keys(rest).length) {
      void loadMonaco().then((monaco) => realLoader.config({ ...rest, monaco }));
    }
  },
  init: (() => loadMonaco().then(() => realLoader.init())) as unknown as typeof realLoader.init,
};

/** `Component`, rendered once the bundled Monaco is the loader's. */
function withBundledMonaco<P extends { className?: string; height?: string | number; width?: string | number }>(
  Component: ComponentType<P>,
): ComponentType<P> {
  return function BundledMonaco(props: P) {
    const [ready, setReady] = useState(false);
    useEffect(() => {
      let live = true;
      loadMonaco().then(
        () => live && setReady(true),
        (e) => console.error("Monaco did not load:", e),
      );
      return () => {
        live = false;
      };
    }, []);
    if (ready) return <Component {...props} />;
    return <div className={props.className} style={{ height: props.height, width: props.width }} />;
  };
}

const Editor = withBundledMonaco(RealEditor);
export const DiffEditor = withBundledMonaco(RealDiffEditor);
export { Editor };
export default Editor;

/** The real hook would start the loader itself; this one waits for the bundle. */
export function useMonaco(): Monaco | null {
  const [monaco, setMonaco] = useState<Monaco | null>(null);
  useEffect(() => {
    let live = true;
    void loadMonaco().then((m) => live && setMonaco(m));
    return () => {
      live = false;
    };
  }, []);
  return monaco;
}
