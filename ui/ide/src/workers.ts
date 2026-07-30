/**
 * The workbench's web workers.
 *
 * VS Code constructs these itself, asking `MonacoEnvironment` for a URL by label,
 * so the page's job is only to *name* them. The `Worker` class below is a stand-in
 * that records its arguments and starts nothing: writing `new Worker(new
 * URL(…, import.meta.url), { type: 'module' })` is what makes the bundler emit the
 * worker as its own chunk, and constructing a real one here would start five
 * workers nobody asked for. This is upstream's own approach (its `fakeWorker`).
 */
class Worker {
  constructor(
    public url: URL,
    public options?: WorkerOptions,
  ) {}
}

/**
 * Label → worker, for the service overrides this bundle registers. A label that
 * is missing here is a worker the workbench asked for and cannot have, so this
 * list tracks the service list in `workbench.ts`.
 */
const workers: Partial<Record<string, Worker>> = {
  editorWorkerService: new Worker(
    new URL("@codingame/monaco-vscode-api/workers/editor.worker", import.meta.url),
    { type: "module" },
  ),
  extensionHostWorkerMain: new Worker(
    new URL("@codingame/monaco-vscode-api/workers/extensionHost.worker", import.meta.url),
    { type: "module" },
  ),
  TextMateWorker: new Worker(
    new URL("@codingame/monaco-vscode-textmate-service-override/worker", import.meta.url),
    { type: "module" },
  ),
  OutputLinkDetectionWorker: new Worker(
    new URL("@codingame/monaco-vscode-output-service-override/worker", import.meta.url),
    { type: "module" },
  ),
  LocalFileSearchWorker: new Worker(
    new URL("@codingame/monaco-vscode-search-service-override/worker", import.meta.url),
    { type: "module" },
  ),
};

/** Point VS Code's worker factory at the chunks the bundler emitted. */
export function configureWorkers(): void {
  window.MonacoEnvironment = {
    getWorkerUrl(_moduleId: string, label: string) {
      return workers[label]?.url.toString() ?? "";
    },
    getWorkerOptions(_moduleId: string, label: string) {
      return workers[label]?.options;
    },
  };
}
