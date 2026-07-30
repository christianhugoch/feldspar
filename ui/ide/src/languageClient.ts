/**
 * TypeScript semantics: the language client half (design §12.1, phase 4).
 *
 * Errors, completions and go-to-definition come from a real
 * `typescript-language-server` running **on the server**, in the store's own
 * directory, against the `node_modules` and the `tsconfig.json` that are already
 * there. This module is the browser end of that: one WebSocket to
 * `/ide/lsp/<store>`, wrapped in a language client that registers itself with the
 * workbench through the ordinary `vscode` API.
 *
 * ## Why not `monaco-languageclient`
 *
 * That package is the obvious dependency and it is the wrong one *here*. Its job
 * is a thin subclass of `vscode-languageclient`'s `BaseLanguageClient` that takes
 * a ready-made transport — but it also pins the whole `@codingame/monaco-vscode-*`
 * family, at `^35` in its unreleased line and `^25` in its released one, against
 * the `36` this workbench is built on. npm would install a second copy, and two
 * copies of `@codingame/monaco-vscode-api` are two service registries: the client
 * would register its providers with a workbench nobody is looking at. §12.1
 * rejected `@typefox/monaco-editor-react` for exactly this reason, and the same
 * reasoning applies one layer down.
 *
 * So the transport-carrying subclass is written here — it is the twenty lines
 * below — and `vscode-languageclient` itself, the actual implementation of the
 * protocol, is used directly. Nothing is reimplemented: what is skipped is a shim
 * whose only other content is a version conflict.
 */

import * as vscode from "vscode";
import {
  BaseLanguageClient,
  CloseAction,
  ErrorAction,
  type CloseHandlerResult,
  type ErrorHandlerResult,
  type LanguageClientOptions,
  type MessageTransports,
} from "vscode-languageclient";
import { toSocket, WebSocketMessageReader, WebSocketMessageWriter } from "vscode-ws-jsonrpc";

import { languageServerUrl, noSemanticsMessage } from "./languageServer";
import { storeFolderUri } from "./workspace";

/**
 * The languages a React project is written in. `json` is deliberately absent:
 * VS Code's own JSON language service is already in the workbench, and a second
 * provider would only duplicate its diagnostics.
 */
const LANGUAGES = ["typescript", "typescriptreact", "javascript", "javascriptreact"];

/**
 * A language client whose connection is already open.
 *
 * `BaseLanguageClient` has exactly one abstract member — where its messages go —
 * and everything else about it (the capabilities, the document synchronisation,
 * the provider registration, the diagnostics) is the real client's own code.
 */
class SocketLanguageClient extends BaseLanguageClient {
  constructor(
    id: string,
    name: string,
    options: LanguageClientOptions,
    private readonly transports: MessageTransports,
  ) {
    super(id, name, options);
  }

  protected createMessageTransports(): Promise<MessageTransports> {
    return Promise.resolve(this.transports);
  }
}

/**
 * Start the language client for `store`, and return what stops it.
 *
 * Nothing is awaited: the workbench must come up whether or not the store can be
 * type-checked, and a store that cannot is *told*, not stalled. The three ways
 * this ends are all end states — the socket refused, the socket lost, the client
 * failed — and none of them retries, because the failure that matters (a store
 * with no local path, a project with no dependencies) does not get better by
 * being asked again. Reconnecting is a page reload.
 */
export function registerLanguageClient(store: string): vscode.Disposable {
  const socket = new WebSocket(languageServerUrl(window.location, store));
  // `{ log: true }`: the client writes levelled records, and a plain output
  // channel would drop the levels on the floor. Created here rather than in
  // `clientOptions` so that stopping the client takes its channel with it — a
  // restart (a branch switch) would otherwise leave one behind per switch.
  const output = vscode.window.createOutputChannel("TypeScript (Saltcorn)", { log: true });
  let client: SocketLanguageClient | null = null;
  let started = false;

  socket.addEventListener("open", () => {
    started = true;
    const connection = toSocket(socket);
    client = new SocketLanguageClient(
      "saltcorn-typescript",
      "TypeScript (Saltcorn)",
      clientOptions(store, output),
      {
        reader: new WebSocketMessageReader(connection),
        writer: new WebSocketMessageWriter(connection),
      },
    );
    client.start().catch((err: unknown) => {
      void vscode.window.showWarningMessage(
        `No TypeScript semantics: the language server would not start (${describe(err)}).`,
      );
    });
  });

  // The server's refusals arrive here, in the close frame's reason: a store with
  // no local path, a project whose dependencies have never been installed, a
  // machine already running its maximum. Each is a sentence written for an admin
  // to read, so it is shown as it was sent — once, and not as an error, because
  // nothing is broken: this store simply has editing, formatting and grammars
  // without semantics (§12.1).
  socket.addEventListener("close", (event) => {
    if (!started) {
      void vscode.window.showWarningMessage(noSemanticsMessage(store, event.reason));
      return;
    }
    void client?.stop();
    client = null;
  });

  const disposable = new vscode.Disposable(() => {
    void client?.stop();
    socket.close();
    output.dispose();
  });
  current = disposable;
  return disposable;
}

/**
 * The client this page is running, so that something which invalidates the
 * server's whole view of the project can replace it.
 *
 * One page edits one store (§12.1, decision 3), so one client is the whole of
 * the state there is to keep.
 */
let current: vscode.Disposable | null = null;

/**
 * Stop the language client and start a fresh one.
 *
 * What this is for is a **branch switch**: the working copy's every file may
 * have changed at once, and the server holds an in-memory project built from the
 * old ones. Restarting is one process (§12.1) and is cheaper — in reasoning as
 * much as in milliseconds — than working out which of its beliefs survived.
 * Nothing else needs it: an ordinary save is synchronised by the protocol, and a
 * pull's changes are on the disk the server is reading from.
 */
export function restartLanguageClient(store: string): void {
  current?.dispose();
  current = null;
  registerLanguageClient(store);
}

/** What the client tells the server about the workspace it is opening. */
function clientOptions(store: string, output: vscode.LogOutputChannel): LanguageClientOptions {
  return {
    // Scheme-qualified: the store's files are `file:` URIs served by the
    // filesystem provider (§12.1), and nothing else in the workbench — VS Code's
    // own in-memory settings files, an untitled buffer — is the store's to
    // type-check.
    documentSelector: LANGUAGES.map((language) => ({
      scheme: "file",
      language,
    })),
    // The root the server is told about. It matches the folder the workbench
    // opened, which is the whole of the URI contract: the bridge on the server
    // translates between this and the store's real directory, and neither end
    // ever sees the other's paths.
    workspaceFolder: { uri: storeFolderUri(store), name: store, index: 0 },
    outputChannel: output,
    // A dead socket cannot be revived by restarting the client on top of it, and
    // a client that keeps trying turns one honest failure into a stream of
    // notifications. The `close` handler above has already said what happened.
    errorHandler: {
      error: (): ErrorHandlerResult => ({ action: ErrorAction.Shutdown }),
      closed: (): CloseHandlerResult => ({ action: CloseAction.DoNotRestart }),
    },
    initializationOptions: {
      hostInfo: "saltcorn",
    },
  };
}

/** A thrown value as a sentence. */
function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
