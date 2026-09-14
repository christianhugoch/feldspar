// Top-level admin app: bootstraps auth state and gates the three top-level
// states the design calls for — create-first-user, login, and the authenticated
// admin shell — then routes between the admin screens with a tiny hash router
// (no router dependency, and no inline styles, so the strict CSP holds).
//
// The shell is Tabler's **vertical layout**: a dark sidebar holding the brand
// and the section links, and a `.page-wrapper` beside it in which each screen
// renders its own `PageHeader` + `PageBody` (see `layout.tsx`). The sidebar
// collapse on narrow screens is React state toggling Bootstrap's `show` class
// rather than Bootstrap's own JS — the SPA already owns the DOM, so vendoring a
// second script to add one class would buy nothing.

import { useCallback, useEffect, useState, type ReactNode } from "react";
import Spinner from "react-bootstrap/Spinner";

import { api } from "./api";
import { splitRoute, stepParam } from "./builder";
import { PoppedChats } from "./PoppedChats";
import { useChatWindows } from "./chatWindows";
import type { AuthStatusResponse } from "./client";
import {
  IconApps,
  IconBolt,
  IconChartHistogram,
  IconChevronLeft,
  IconChevronRight,
  IconFolder,
  IconLogout,
  IconMoon,
  IconRobot,
  IconSettings,
  IconSun,
  IconTable,
  IconUsers,
  SaltcornLogo,
} from "./icons";
import { useNarrowSidebar, useTheme } from "./layout";
import { AgentChat } from "./screens/AgentChat";
import { AgentForm } from "./screens/AgentForm";
import { Agents } from "./screens/Agents";
import { Applications } from "./screens/Applications";
import { ApplicationForm } from "./screens/ApplicationForm";
import { ApplicationLibrary } from "./screens/ApplicationLibrary";
import { ApplicationViews } from "./screens/ApplicationViews";
import { PageProperties } from "./screens/PageProperties";
import { ViewEditor } from "./screens/ViewEditor";
import { DbConnections } from "./screens/DbConnections";
import { FileManager } from "./screens/FileManager";
import { FileStores } from "./screens/FileStores";
import { FileStoreForm } from "./screens/FileStoreForm";
import { FirstUser } from "./screens/FirstUser";
import { GraphqlExplorer } from "./screens/GraphqlExplorer";
import { LlmProviders } from "./screens/LlmProviders";
import { LlmProviderForm } from "./screens/LlmProviderForm";
import { Login } from "./screens/Login";
import { ModelForm } from "./screens/ModelForm";
import { ModelInstance } from "./screens/ModelInstance";
import { Models } from "./screens/Models";
import { Roles } from "./screens/Roles";
import { Settings } from "./screens/Settings";
import { Tables } from "./screens/Tables";
import { TableData } from "./screens/TableData";
import { TableDetail } from "./screens/TableDetail";
import { RunDetail } from "./screens/RunDetail";
import { Triggers } from "./screens/Triggers";
import { TriggerForm } from "./screens/TriggerForm";
import { Users } from "./screens/Users";
import { WorkflowEditor } from "./screens/WorkflowEditor";
import { WorkflowRuns } from "./screens/WorkflowRuns";

/** The authenticated user, as reported by `authStatus` / `login`. */
export type CurrentUser = NonNullable<AuthStatusResponse["current_user"]>;

/** Subscribe to `location.hash`, normalised to a path like `/tables`. */
function useHashRoute(): string {
  const read = () => window.location.hash.replace(/^#/, "") || "/tables";
  const [route, setRoute] = useState(read);
  useEffect(() => {
    const onChange = () => setRoute(read());
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return route;
}

/** Navigate by updating the hash (the router above reacts to it). */
export function navigate(path: string): void {
  window.location.hash = path;
}

/**
 * Where a file store is edited as code: the IDE (design §12.1).
 *
 * Not a hash route — the IDE is a **separate page** with its own bundle, served
 * at `/ide/`, because VS Code initializes once per page. So this is an ordinary
 * link that leaves the SPA, and the browser's Back button is what comes back.
 */
export function ideUrl(store: string): string {
  return `/ide/?store=${encodeURIComponent(store)}`;
}

export function App() {
  const [status, setStatus] = useState<AuthStatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.authStatus());
      setError(null);
    } catch {
      setError("Could not reach the server. Is it running?");
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (error) {
    return (
      <div className="page page-center">
        <div className="container container-tight py-4">
          <div className="alert alert-danger">{error}</div>
        </div>
      </div>
    );
  }

  if (!status) {
    return (
      <div className="page page-center">
        <div className="container container-tight py-4 text-center">
          <Spinner animation="border" role="status" />
        </div>
      </div>
    );
  }

  if (!status.any_user_exists) {
    return <FirstUser onCreated={refresh} />;
  }

  if (!status.current_user) {
    return <Login onLoggedIn={refresh} />;
  }

  return <Shell user={status.current_user} onLogout={refresh} />;
}

/** One entry in the sidebar: where it goes, what it is called, and which routes
 * count as "here" (a detail screen is still its section). */
type NavItem = {
  href: string;
  label: string;
  icon: ReactNode;
  /** Route prefixes that light this entry up. */
  matches: string[];
};

export const NAV: NavItem[] = [
  {
    href: "#/tables",
    label: "Tables",
    icon: <IconTable />,
    // The database connections list is part of this section rather than one of
    // its own, the way LLM providers hang off Agents: a connection exists to put
    // tables in the tables list, and the way to it is a button on that screen.
    matches: ["/tables", "/db-connections"],
  },
  {
    href: "#/applications",
    label: "Applications",
    icon: <IconApps />,
    matches: ["/applications"],
  },
  {
    href: "#/triggers",
    label: "Triggers",
    icon: <IconBolt />,
    matches: ["/triggers"],
  },
  {
    href: "#/file-stores",
    label: "Files",
    icon: <IconFolder />,
    matches: ["/file-stores", "/files"],
  },
  {
    href: "#/models",
    label: "Models",
    icon: <IconChartHistogram />,
    // Beside Agents rather than under Tables: a model is a question asked *of*
    // a table, and the section it belongs to is the one about answering
    // questions rather than the one about storing rows. A dataset has no entry
    // of its own on purpose — it belongs to its model and has no life without
    // one (§3).
    matches: ["/models", "/model-instances"],
  },
  {
    href: "#/agents",
    label: "Agents",
    icon: <IconRobot />,
    // The providers list is part of this section rather than one of its own: an
    // LLM provider exists to be pointed at by an agent, and nothing else in the
    // admin UI has any use for one.
    matches: ["/agents", "/llm-providers"],
  },
  {
    href: "#/users",
    label: "Users",
    icon: <IconUsers />,
    // Roles hang off this section rather than standing beside it, the way LLM
    // providers hang off Agents: a role exists to be held by a user, and the
    // way to the list is a button on the Users screen.
    matches: ["/users", "/roles"],
  },
  {
    href: "#/settings",
    label: "Settings",
    icon: <IconSettings />,
    // Last, and one entry however many sections it grows: settings are about
    // the *installation* rather than about anything in it, and an admin looks
    // for them in one place rather than under whichever thing they configure.
    matches: ["/settings"],
  },
];

/** The authenticated admin shell: Tabler's vertical layout around the screen. */
function Shell({
  user,
  onLogout,
}: {
  user: CurrentUser;
  onLogout: () => void;
}) {
  const route = useHashRoute();
  const [menuOpen, setMenuOpen] = useState(false);
  const [theme, toggleTheme] = useTheme();
  const [narrow, toggleNarrow] = useNarrowSidebar();
  // A docked chat covers the bottom-right corner of whatever is underneath it.
  // For most screens that is an overlay doing what an overlay does; for the
  // chat *screen* it would be a window sitting on the send button of the page's
  // own composer, so the page is told how much of the corner is taken and keeps
  // clear of it (`admin.css`). Minimized windows are two rows of pixels along
  // the very bottom and are not worth narrowing a transcript for.
  const docked = useChatWindows().filter(
    (chat) => chat.mode === "docked",
  ).length;
  const corner =
    docked === 0 ? "" : ` chat-corner-taken chat-corner-${Math.min(docked, 3)}`;

  // A tap on a sidebar link should close the sidebar it was in; on a wide
  // screen the collapse is not rendered as a drawer, so this is a no-op there.
  useEffect(() => setMenuOpen(false), [route]);

  const logout = async () => {
    try {
      await api.logout();
    } finally {
      onLogout();
    }
  };

  return (
    // `sidebar-narrow` is the whole of the icons-only mode: it is on `.page`
    // because both the sidebar's width and the page wrapper's matching offset
    // hang off it (see `admin.css`), and they have to change together.
    <div className={`page${narrow ? " sidebar-narrow" : ""}${corner}`}>
      <aside
        className="navbar navbar-vertical navbar-expand-lg"
        data-bs-theme="dark"
      >
        <div className="container-fluid">
          <button
            className="navbar-toggler"
            type="button"
            aria-controls="sidebar-menu"
            aria-expanded={menuOpen}
            aria-label="Toggle navigation"
            onClick={() => setMenuOpen((open) => !open)}
          >
            <span className="navbar-toggler-icon" />
          </button>
          {/* No `navbar-brand-autodark` here: that class flips a monochrome
              logo to white for a dark sidebar, and this one has its own
              colours to keep. */}
          <div className="navbar-brand">
            <a
              href="#/tables"
              className="d-flex align-items-center gap-2"
              aria-label="Saltcorn"
            >
              <div className="d-flex">
                <SaltcornLogo />
                <div className="ms-2">
                  <div className="saltcorn-label">Saltcorn</div>
                  <div className="feldspar-label">Feldspar</div>
                </div>
              </div>
            </a>
          </div>
          {/* On a narrow screen the collapse is shut by default, so the account
              controls sit in this always-visible row instead of at the foot of
              the menu (where the wide layout keeps them). */}
          <div className="navbar-nav flex-row d-lg-none">
            <ThemeToggle theme={theme} onToggle={toggleTheme} />
            <div className="nav-item ms-2">
              <button
                type="button"
                className="nav-link px-0"
                onClick={() => void logout()}
                aria-label="Log out"
                title={`Log out (${user.email})`}
              >
                <IconLogout className="icon-1" />
              </button>
            </div>
          </div>
          <div
            className={
              menuOpen
                ? "collapse navbar-collapse show"
                : "collapse navbar-collapse"
            }
            id="sidebar-menu"
          >
            <ul className="navbar-nav pt-lg-3">
              {NAV.map((item) => {
                const active = item.matches.some((prefix) =>
                  route.startsWith(prefix),
                );
                return (
                  <li
                    key={item.href}
                    className={active ? "nav-item active" : "nav-item"}
                  >
                    <a
                      className={active ? "nav-link active" : "nav-link"}
                      href={item.href}
                      aria-current={active ? "page" : undefined}
                      // Narrowed, the icon is all there is to go on, so the
                      // name becomes the hover label. Expanded it is already
                      // on screen and a tooltip would only repeat it.
                      title={narrow ? item.label : undefined}
                    >
                      <span className="nav-link-icon d-md-none d-lg-inline-block">
                        {item.icon}
                      </span>
                      <span className="nav-link-title">{item.label}</span>
                    </a>
                  </li>
                );
              })}
            </ul>
            {/* The nav list above is `flex-grow: 1` in a column collapse, so
                everything below it settles at the bottom of the sidebar. The
                width switch is only offered on `lg` and up: below that the
                sidebar is a drawer, which has no width to give back. */}
            <div className="d-none d-lg-flex justify-content-end px-3 pb-2">
              <button
                type="button"
                className="btn btn-icon btn-ghost-secondary"
                onClick={toggleNarrow}
                aria-pressed={narrow}
                aria-label={
                  narrow ? "Widen the sidebar" : "Narrow the sidebar to icons"
                }
                title={
                  narrow ? "Widen the sidebar" : "Narrow the sidebar to icons"
                }
              >
                {narrow ? (
                  <IconChevronRight className="icon-2" />
                ) : (
                  <IconChevronLeft className="icon-2" />
                )}
              </button>
            </div>
            <div className="d-none d-lg-block px-3 py-3 border-top">
              {/* Narrowed there is no room for an address, so the email moves
                  into the log-out button's tooltip (see `admin.css`). */}
              <div className="text-secondary text-truncate mb-2 sidebar-wide-only">
                {user.email}
              </div>
              <div className="d-flex align-items-center gap-2 flex-wrap">
                <button
                  type="button"
                  className="btn btn-outline-secondary btn-sm"
                  onClick={() => void logout()}
                  title={`Log out (${user.email})`}
                >
                  <IconLogout className="icon-2" />
                  <span className="sidebar-wide-only">Log out</span>
                </button>
                <ThemeToggle theme={theme} onToggle={toggleTheme} />
              </div>
            </div>
          </div>
        </div>
      </aside>

      <div className="page-wrapper">
        <Screen route={route} user={user} />
        <footer className="footer footer-transparent d-print-none">
          <div className="container-xl">
            <div className="row text-center align-items-center flex-row-reverse">
              <div className="col-12 col-lg-auto mt-3 mt-lg-0">
                <span className="text-secondary">Saltcorn</span>
              </div>
            </div>
          </div>
        </footer>
      </div>

      {/* Outside the routed screen, and outside the page wrapper: a popped-out
          chat is furniture of the whole admin, and it stays in the corner while
          everything above changes underneath it (`PoppedChats.tsx`). */}
      <PoppedChats />
    </div>
  );
}

/** Light/dark switch. One button that shows the scheme it would switch *to*,
 * which is how Tabler's own header reads (it swaps two links; we swap an icon). */
function ThemeToggle({
  theme,
  onToggle,
}: {
  theme: string;
  onToggle: () => void;
}) {
  const dark = theme === "dark";
  return (
    <div className="nav-item">
      <button
        type="button"
        className="nav-link px-0"
        onClick={onToggle}
        title={dark ? "Enable light mode" : "Enable dark mode"}
        aria-label={dark ? "Enable light mode" : "Enable dark mode"}
      >
        {dark ? (
          <IconSun className="icon-1" />
        ) : (
          <IconMoon className="icon-1" />
        )}
      </button>
    </div>
  );
}

/** Resolve the current hash route to a screen.
 *
 * `user` reaches only the screens that are *about* the signed-in admin rather
 * than about a record — today the GraphQL explorer, which runs its queries under
 * that admin's own authority and has to say whose. */
function Screen({ route, user }: { route: string; user: CurrentUser }) {
  // Routes are matched on their path; a query is what a screen is opened with
  // (the builder's way back to `views/:name?step=n`).
  const { path, query } = splitRoute(route);
  const tableDataMatch = path.match(/^\/tables\/([^/]+)\/data$/);
  if (tableDataMatch) {
    return <TableData table={decodeURIComponent(tableDataMatch[1])} />;
  }
  const tableMatch = path.match(/^\/tables\/([^/]+)$/);
  if (tableMatch) {
    return <TableDetail table={decodeURIComponent(tableMatch[1])} />;
  }
  if (path === "/applications/new") {
    return <ApplicationForm />;
  }
  const graphqlMatch = path.match(/^\/applications\/([^/]+)\/graphql$/);
  if (graphqlMatch) {
    return (
      <GraphqlExplorer
        appId={decodeURIComponent(graphqlMatch[1])}
        user={user}
      />
    );
  }
  const viewEditMatch = path.match(
    /^\/applications\/([^/]+)\/views\/([^/]+)$/,
  );
  if (viewEditMatch) {
    return (
      <ViewEditor
        key={route}
        appId={decodeURIComponent(viewEditMatch[1])}
        name={decodeURIComponent(viewEditMatch[2])}
        initialStep={stepParam(query)}
      />
    );
  }
  // `pages/new` before `pages/:name/properties`; a page named "new" has its
  // properties at `pages/new/properties`, so the two never meet.
  const newPageMatch = path.match(/^\/applications\/([^/]+)\/pages\/new$/);
  if (newPageMatch) {
    return (
      <PageProperties key={path} appId={decodeURIComponent(newPageMatch[1])} name={null} />
    );
  }
  const pagePropertiesMatch = path.match(
    /^\/applications\/([^/]+)\/pages\/([^/]+)\/properties$/,
  );
  if (pagePropertiesMatch) {
    return (
      <PageProperties
        key={path}
        appId={decodeURIComponent(pagePropertiesMatch[1])}
        name={decodeURIComponent(pagePropertiesMatch[2])}
      />
    );
  }
  const libraryMatch = path.match(/^\/applications\/([^/]+)\/library$/);
  if (libraryMatch) {
    return <ApplicationLibrary appId={decodeURIComponent(libraryMatch[1])} />;
  }
  const viewsMatch = path.match(/^\/applications\/([^/]+)\/(views|pages)$/);
  if (viewsMatch) {
    return (
      <ApplicationViews
        appId={decodeURIComponent(viewsMatch[1])}
        tab={viewsMatch[2] as "views" | "pages"}
      />
    );
  }
  const editMatch = path.match(
    /^\/applications\/([^/]+)\/(edit|app-settings)$/,
  );
  if (editMatch) {
    return (
      <ApplicationForm
        appId={decodeURIComponent(editMatch[1])}
        tab={editMatch[2] === "app-settings" ? "app-settings" : "settings"}
      />
    );
  }
  if (path.startsWith("/applications")) {
    return <Applications />;
  }
  if (path === "/triggers/new") {
    return <TriggerForm />;
  }
  // "Create trigger" on a table's own page: the same form, with that table
  // already chosen as the one the trigger fires on.
  const triggerForTableMatch = path.match(/^\/triggers\/new\/([^/]+)$/);
  if (triggerForTableMatch) {
    return <TriggerForm table={decodeURIComponent(triggerForTableMatch[1])} />;
  }
  const triggerEditMatch = path.match(/^\/triggers\/([^/]+)\/edit$/);
  if (triggerEditMatch) {
    return <TriggerForm triggerId={decodeURIComponent(triggerEditMatch[1])} />;
  }
  // A workflow is a trigger **body** (§10.3, decision 1), so its editor and its
  // runs hang off the trigger's id rather than standing beside it as an entity
  // of their own — there is no `/workflows/…` because there is no workflow to
  // address without a trigger.
  const workflowMatch = path.match(/^\/triggers\/([^/]+)\/workflow$/);
  if (workflowMatch) {
    return <WorkflowEditor triggerId={decodeURIComponent(workflowMatch[1])} />;
  }
  const workflowRunsMatch = path.match(/^\/triggers\/([^/]+)\/runs$/);
  if (workflowRunsMatch) {
    return (
      <WorkflowRuns triggerId={decodeURIComponent(workflowRunsMatch[1])} />
    );
  }
  if (path.startsWith("/triggers")) {
    return <Triggers />;
  }
  if (path === "/file-stores/new") {
    return <FileStoreForm />;
  }
  const storeEditMatch = path.match(/^\/file-stores\/([^/]+)\/edit$/);
  if (storeEditMatch) {
    return <FileStoreForm storeId={decodeURIComponent(storeEditMatch[1])} />;
  }
  if (path.startsWith("/file-stores")) {
    return <FileStores />;
  }
  // `/files/<store>` opens at the root; `/files/<store>/<dir>` opens in a
  // directory, which is what an application row links to (§2.4).
  const filesMatch = path.match(/^\/files\/([^/]+)(?:\/(.*))?$/);
  if (filesMatch) {
    const dir = (filesMatch[2] ?? "")
      .split("/")
      .filter((s) => s.length > 0)
      .map(decodeURIComponent)
      .join("/");
    return (
      <FileManager store={decodeURIComponent(filesMatch[1])} initialDir={dir} />
    );
  }
  if (path === "/agents/new") {
    return <AgentForm />;
  }
  // A chat is addressed by the agent's **name**, not its id: it is what the run
  // history is keyed by (§11.4) and what the socket's `start` carries, so a
  // bookmarked chat URL says which agent it is.
  const agentChatMatch = path.match(/^\/agents\/([^/]+)\/chat$/);
  if (agentChatMatch) {
    return <AgentChat agent={decodeURIComponent(agentChatMatch[1])} />;
  }
  const agentEditMatch = path.match(/^\/agents\/([^/]+)\/edit$/);
  if (agentEditMatch) {
    return <AgentForm agentId={decodeURIComponent(agentEditMatch[1])} />;
  }
  if (path.startsWith("/agents")) {
    return <Agents />;
  }
  if (path === "/llm-providers/new") {
    return <LlmProviderForm />;
  }
  const providerEditMatch = path.match(/^\/llm-providers\/([^/]+)\/edit$/);
  if (providerEditMatch) {
    return (
      <LlmProviderForm providerId={decodeURIComponent(providerEditMatch[1])} />
    );
  }
  if (path.startsWith("/llm-providers")) {
    return <LlmProviders />;
  }
  if (path.startsWith("/db-connections")) {
    return <DbConnections />;
  }
  if (path === "/models/new") {
    return <ModelForm />;
  }
  // A fit is addressed by its own id, as `getModelInstance` is: which model it
  // is of is the server's answer, not the URL's.
  const instanceMatch = path.match(/^\/model-instances\/([^/]+)$/);
  if (instanceMatch) {
    const instanceId = decodeURIComponent(instanceMatch[1]);
    // Keyed, so moving from one fit to another **remounts** rather than
    // re-rendering: the screen holds a fit's own answers (the "try a row" box's
    // reply, most of all), and one fit's answer shown under another fit's
    // coefficients is exactly the confident-wrong-answer this milestone is most
    // careful about.
    return <ModelInstance key={instanceId} instanceId={instanceId} />;
  }
  // The model *is* its form: a model is edited and refitted continuously, so
  // there is no read-only screen it would be opened into first.
  const modelMatch = path.match(/^\/models\/([^/]+)$/);
  if (modelMatch) {
    return <ModelForm modelId={decodeURIComponent(modelMatch[1])} />;
  }
  if (path.startsWith("/models")) {
    return <Models />;
  }
  // A run is addressed by its own id, as `getRun` is: which workflow it is of is
  // the server's answer, not the URL's.
  const runMatch = path.match(/^\/runs\/([^/]+)$/);
  if (runMatch) {
    return <RunDetail runId={decodeURIComponent(runMatch[1])} />;
  }
  if (path.startsWith("/users")) {
    return <Users />;
  }
  if (path.startsWith("/roles")) {
    return <Roles />;
  }
  if (path.startsWith("/settings")) {
    return <Settings />;
  }
  return <Tables />;
}
