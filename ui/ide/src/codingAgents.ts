/**
 * Which agents can work on the store this page is editing (design §12.1, §11.2).
 *
 * Creating an application creates the agent that builds it: a `coding` trait
 * scoped to the application's source directory, plus `build_application` for the
 * app itself (§13.3). That agent is the one an admin wants in the editor — it
 * already knows which project it is looking at and what it may change — so the
 * IDE does not invent an agent, or a scope, or a prompt. It finds the ones whose
 * `coding` trait is pointed at **this store** and offers those.
 *
 * The match is on the trait's configuration rather than on the builder agent's
 * name, so a hand-made agent over the same store is offered on exactly the same
 * terms as a generated one — which is the point: what qualifies an agent here is
 * that its file tools reach the tree in the explorer, not who created it.
 */

import type { ListAgentsResponse, ListApplicationsResponse } from "./client";
import { applicationsBuiltFrom } from "./applications";

/** The trait that reads, searches and edits a file store (`TRAIT_CODING`). */
const TRAIT_CODING = "coding";
/** Its file-store setting (`TRAIT_CFG_STORE`). */
const CFG_STORE = "store";
/** Its sub-directory setting (`TRAIT_CFG_ROOT`). */
const CFG_ROOT = "root";
/** Its "may create and change files" grant (`TRAIT_CFG_MAY_EDIT`). */
const CFG_MAY_EDIT = "may_edit";

/** One agent this store can be worked on with. */
export interface StoreAgent {
  /** The agent's name, as the chat socket's `start` frame names it. */
  readonly name: string;
  /** Its one-line description, as the admin wrote it. */
  readonly description: string;
  /**
   * The store-relative directory its file tools are scoped to — `""` for the
   * whole store. A path outside it is not something this agent can read, which
   * is worth saying in the chat rather than discovering by being refused.
   */
  readonly root: string;
  /** Whether it may write, as its trait is configured. */
  readonly mayEdit: boolean;
  /** The application built from that directory, when there is one. */
  readonly application: string | null;
  /** Why the agent cannot run, when the server says it cannot. */
  readonly error: string | null;
}

/**
 * The agents scoped to `store`, the ones that build an application first.
 *
 * An agent with a broken definition — a provider that was deleted, a trait
 * configured against a store that has gone away — is **kept**. It carries its
 * reason, and a chat that answers with that reason is more use than an agent
 * that silently is not offered: the agent screen is one click away and the
 * reason is what tells the admin to go there.
 */
export function codingAgentsForStore(
  agents: ListAgentsResponse,
  applications: ListApplicationsResponse,
  store: string,
): StoreAgent[] {
  const built = applicationsBuiltFrom(applications, store);
  const found: StoreAgent[] = [];
  for (const agent of agents) {
    for (const trait of agent.traits) {
      if (trait.trait !== TRAIT_CODING) continue;
      const config = (trait.config ?? {}) as Record<string, unknown>;
      if (config[CFG_STORE] !== store) continue;
      const root = normalizePath(
        typeof config[CFG_ROOT] === "string" ? config[CFG_ROOT] : "",
      );
      found.push({
        name: agent.name,
        description: agent.description,
        root,
        mayEdit: config[CFG_MAY_EDIT] === true,
        application:
          built.find((candidate) => candidate.sourcePath === root)?.application
            .name ?? null,
        error: agent.error ?? null,
      });
      // One entry per agent even when its trait is configured twice over two
      // directories of the same store: the chat talks to the agent, and the
      // agent has both.
      break;
    }
  }
  return found.sort(byApplicationThenName);
}

/**
 * An application's agent before a bare one, then by name.
 *
 * The first is the one the chat opens on, and "the agent that builds the app
 * whose source this is" is the answer an admin who opened the IDE on an
 * application's store was looking for.
 */
function byApplicationThenName(a: StoreAgent, b: StoreAgent): number {
  if ((a.application == null) !== (b.application == null))
    return a.application == null ? 1 : -1;
  return a.name.localeCompare(b.name);
}

/** A store-relative path as the store spells one: no leading, trailing or `./` parts. */
function normalizePath(path: string): string {
  return path.replace(/^\.\//, "").replace(/^\/+/, "").replace(/\/+$/, "");
}
