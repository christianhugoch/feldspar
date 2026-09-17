/**
 * The application's coding agent, in the workbench's chat panel (design §12.1).
 *
 * VS Code's chat is the right-hand panel: a participant is asked something and
 * answers into a stream. Saltcorn already has the agent — a `coding` trait over
 * this store, created with the application (§13.3) — and already has the protocol
 * that runs one turn of it (§11.4). So what is here is a **relay**, and that is
 * the whole of the decision: the model, the tools, the grants and the transcript
 * stay on the server, where an agent's tools run as the person who asked and
 * every step is written to its run. Nothing about an agent moves into the browser
 * to make this work; the browser gains a second window onto it.
 *
 * ## The model picker is the agent picker
 *
 * VS Code will not send a chat request without a language model, so this
 * registers one — and the models it registers are **the store's agents**. That
 * is not a workaround dressed up: which LLM answers is already the agent's own
 * `provider` and `model` (§11.2), so a picker offering LLMs here would be a
 * second place to configure the same thing and the one that cannot see the
 * agent's system prompt or its traits. What the panel is genuinely choosing
 * between, when a store holds more than one project, is *agents*.
 *
 * The provider therefore answers as the agent, and the participant sends its
 * turn to whichever agent the picker names. Both go through
 * [`conversations`](Conversations), so the run is the same one either way.
 *
 * ## The tools are the agent's
 *
 * Edits appear in the explorer because the agent wrote them *to the store*, not
 * because the chat applied an edit — so the IDE drops what it has cached when a
 * turn ends, exactly as it does after a build or a pull.
 *
 * The chat extension is registered **separately** from `extension.ts`'s, and as a
 * system extension: contributing a participant that answers an unaddressed
 * message, and a model provider at all, both need API proposals, and those are
 * enabled per extension. Keeping it apart is what stops the rest of the IDE's
 * contributions from quietly running with proposals enabled too.
 */

import {
  ExtensionHostKind,
  registerExtension,
} from "@codingame/monaco-vscode-api/extensions";
import type { IExtensionManifest } from "@codingame/monaco-vscode-api/extensions";
import * as monaco from "monaco-editor";
import type * as vscode from "vscode";

import {
  AgentConversation,
  agentChatUrl,
  connectAgentChat,
  type ServerEvent,
} from "./agentChat";
import type { StoreAgent } from "./codingAgents";
import type { StoreFileSystemProvider } from "./fileSystemProvider";
import {
  CHAT_EXTENSION_ID,
  MODEL_VENDOR,
  participantContributions,
  hasChanges,
  mergeChanges,
  relayEvent,
  storeModel,
  workspacePath,
  type ParticipantContribution,
  type StoreChange,
} from "./chatRelay";
import type { StoreFiles } from "./storeFiles";

/**
 * The proposals this extension needs.
 *
 * `defaultChatParticipant` is what lets the participant answer a message that
 * mentions nobody — the only way a chat panel is usable without teaching the
 * admin an `@name`. `chatProvider` is the language-model provider. The other two
 * are the response-stream parts this relay uses.
 */
const CHAT_API_PROPOSALS = [
  "defaultChatParticipant",
  "chatParticipantAdditions",
  "chatParticipantPrivate",
  "chatProvider",
];

/** What the workbench must be told when an agent has written to the store. */
export interface StoreRefresh {
  readonly files: StoreFiles;
  readonly provider: StoreFileSystemProvider;
  /** Source control's rescan, when the store is a git working copy. */
  readonly refreshSourceControl?: () => void;
}

/** The chat extension, once it has been declared. */
export interface DeclaredChat {
  readonly getApi: () => Promise<typeof vscode>;
  /** The participants the manifest contributed, in `agents` order. */
  readonly participants: ParticipantContribution[];
}

/**
 * Declare the chat extension. **Must be called before `initialize`**, for the
 * reason `extension.ts` gives: an extension registered afterwards is a delta
 * applied to a running workbench, and this bundle has no worker-host extension
 * for that delta to be accepted by.
 *
 * `null` when the store has no agent scoped to it, which is the ordinary case
 * for a store holding assets rather than an application. Nothing is contributed
 * then — no participant, no models, and a chat view that says it has nothing to
 * talk to rather than one that offers to.
 */
export function declareAgentChat(agents: StoreAgent[]): DeclaredChat | null {
  if (agents.length === 0) return null;
  const participants = participantContributions(agents);
  const [publisher, name] = CHAT_EXTENSION_ID.split(".");
  const manifest: IExtensionManifest = {
    name,
    displayName: "Saltcorn agents",
    publisher,
    version: "1.0.0",
    engines: { vscode: "*" },
    activationEvents: ["*"],
    contributes: {
      chatParticipants: participants,
      languageModelChatProviders: [
        { vendor: MODEL_VENDOR, displayName: "Saltcorn" },
      ],
    },
    enabledApiProposals: CHAT_API_PROPOSALS,
  };
  // `system: true` is what makes `enabledApiProposals` mean anything: proposals
  // are refused to an ordinary extension, and this one is as built-in as the
  // workbench it is compiled into.
  const { getApi } = registerExtension(
    manifest,
    ExtensionHostKind.LocalProcess,
    { system: true },
  );
  return { getApi, participants };
}

/**
 * Register the participants and the model.
 *
 * The extension's *own* API — not the one `setAsDefaultApi` hands the rest of
 * this bundle — because the proposals above are enabled for this extension and
 * `chat.createChatParticipant` is reached through it.
 */
export async function activateAgentChat(
  declared: DeclaredChat,
  agents: StoreAgent[],
  store: string,
  refresh: StoreRefresh,
): Promise<vscode.Disposable[]> {
  const api = await declared.getApi();
  const conversations = new Conversations();
  return [
    registerModel(api, store, agents[0], conversations),
    ...agents.map((agent, index) =>
      registerParticipant(
        api,
        agent,
        declared.participants[index],
        store,
        refresh,
        conversations,
      ),
    ),
  ];
}

/**
 * One conversation per agent, restarted when the panel's history is empty —
 * which is what VS Code's New Chat leaves behind.
 *
 * The run outlives the socket, so a reconnection continues the same
 * conversation: the history a turn is answered against is the server's, not this
 * page's, and re-sending it from here would be a second copy to disagree with.
 */
class Conversations {
  private readonly open = new Map<string, AgentConversation>();

  /** The conversation with `agent`, starting a fresh one when `restart`. */
  with(agent: string, restart: boolean): AgentConversation {
    if (restart) {
      this.open.get(agent)?.close();
      this.open.delete(agent);
    }
    let conversation = this.open.get(agent);
    if (conversation == null) {
      conversation = new AgentConversation(agent, () =>
        connectAgentChat(agentChatUrl(window.location)),
      );
      this.open.set(agent, conversation);
    }
    return conversation;
  }

  /** A conversation of its own, kept by nobody — for a caller that must not
   * join the one the panel is having. */
  detached(agent: string): AgentConversation {
    return new AgentConversation(agent, () =>
      connectAgentChat(agentChatUrl(window.location)),
    );
  }
}

/** The text of the last user message in `messages`. */
function lastUserText(
  api: typeof vscode,
  messages: readonly vscode.LanguageModelChatRequestMessage[],
): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message.role !== api.LanguageModelChatMessageRole.User) continue;
    const text = message.content
      .filter(
        (part): part is vscode.LanguageModelTextPart =>
          part instanceof api.LanguageModelTextPart,
      )
      .map((part) => part.value)
      .join("");
    if (text !== "") return text;
  }
  return "";
}

/**
 * The placeholder model the chat needs in order to send anything at all.
 *
 * It is not user-selectable and the participants below never read it — but a
 * `sendRequest` that reaches it (`lm.selectChatModels`, another extension)
 * answers as the store's first agent rather than throwing, so the one model this
 * workbench has is a model that works.
 */
function registerModel(
  api: typeof vscode,
  store: string,
  agent: StoreAgent,
  conversations: Conversations,
): vscode.Disposable {
  const model = storeModel(store);
  const changed = new api.EventEmitter<void>();
  const registration = api.lm.registerLanguageModelChatProvider(MODEL_VENDOR, {
    provideLanguageModelChatInformation: () => [model],
    // Nothing here can count an agent's tokens: the tokenizer is the agent's
    // provider's, on the server. Zero is the honest answer to a question this
    // surface cannot answer, and nothing in the panel spends it.
    provideTokenCount: () => Promise.resolve(0),
    provideLanguageModelChatResponse: async (
      _model,
      messages,
      _options,
      progress,
      token,
    ) => {
      // A conversation of its own, so a caller doing this cannot walk into the
      // middle of the run the panel is having.
      const conversation = conversations.detached(agent.name);
      try {
        await conversation.ask(
          lastUserText(api, messages),
          (event) => {
            if (event.type === "text")
              progress.report(new api.LanguageModelTextPart(event.delta));
          },
          token,
        );
      } finally {
        conversation.close();
      }
    },
    onDidChangeLanguageModelChatInformation: changed.event,
  });
  // The model exists from the first frame; the event is what tells the chat to
  // look again, and a chat that never asked has no model to send with.
  changed.fire();
  return registration;
}

/** One participant: one turn of the one agent it was created for. */
function registerParticipant(
  api: typeof vscode,
  agent: StoreAgent,
  contribution: ParticipantContribution,
  store: string,
  refresh: StoreRefresh,
  conversations: Conversations,
): vscode.Disposable {
  const participant = api.chat.createChatParticipant(
    contribution.id,
    async (request, context, response, token) => {
      const conversation = conversations.with(
        agent.name,
        context.history.length === 0,
      );
      const changes: StoreChange[] = [];
      const sink = (event: ServerEvent) => {
        changes.push(relayEvent(event, response));
      };
      try {
        const outcome = await conversation.ask(request.prompt, sink, token);
        const changed = mergeChanges(changes);
        if (hasChanges(changed)) announce(api, changed, agent, store, refresh);
        return { metadata: { run: outcome.run, state: outcome.state } };
      } catch (err) {
        // The socket never opened: a server with no agents installed, or a
        // session that expired while the panel sat there.
        response.markdown(
          `\n\n⚠️ ${err instanceof Error ? err.message : String(err)}\n\n`,
        );
        return {};
      }
    },
  );
  participant.iconPath = new api.ThemeIcon("robot");
  return participant;
}

/**
 * Tell the workbench what the agent changed under it.
 *
 * There is no watcher over an HTTP file API (§12.1), so an agent's writes are
 * exactly the case the provider's `announceChanged` exists for: the IDE did not
 * make the change, but it knows precisely which paths it was — the tool call said
 * so — and an editor left open on stale contents is how an admin overwrites work
 * by saving.
 *
 * After a shell command nothing says which paths it was, so every cached listing
 * goes and every open, unmodified editor is told to re-read. Source control is
 * rescanned whenever anything changed, which covers a feature's commit too.
 */
function announce(
  api: typeof vscode,
  change: StoreChange,
  agent: StoreAgent,
  store: string,
  refresh: StoreRefresh,
): void {
  refresh.files.forgetEverything();
  const uris = change.paths.map((path) =>
    monaco.Uri.file(workspacePath(store, agent.root, path)),
  );
  if (change.everything) {
    for (const document of api.workspace.textDocuments) {
      if (document.uri.scheme === "file" && !document.isDirty)
        uris.push(monaco.Uri.file(document.uri.path));
    }
  }
  refresh.provider.announceChanged(uris);
  refresh.refreshSourceControl?.();
}
