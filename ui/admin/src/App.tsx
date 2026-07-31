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
import type { AuthStatusResponse } from "./client";
import {
  IconApps,
  IconBolt,
  IconChevronLeft,
  IconChevronRight,
  IconFolder,
  IconLogout,
  IconMoon,
  IconRobot,
  IconShieldLock,
  IconSun,
  IconTable,
  IconUsers,
  SaltcornLogo,
} from "./icons";
import { useNarrowSidebar, useTheme } from "./layout";
import { Applications } from "./screens/Applications";
import { ApplicationForm } from "./screens/ApplicationForm";
import { FileManager } from "./screens/FileManager";
import { FileStores } from "./screens/FileStores";
import { FileStoreForm } from "./screens/FileStoreForm";
import { FirstUser } from "./screens/FirstUser";
import { LlmProviders } from "./screens/LlmProviders";
import { LlmProviderForm } from "./screens/LlmProviderForm";
import { Login } from "./screens/Login";
import { Roles } from "./screens/Roles";
import { Tables } from "./screens/Tables";
import { TableDetail } from "./screens/TableDetail";
import { Triggers } from "./screens/Triggers";
import { TriggerForm } from "./screens/TriggerForm";
import { Users } from "./screens/Users";

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

const NAV: NavItem[] = [
  { href: "#/tables", label: "Tables", icon: <IconTable />, matches: ["/tables"] },
  {
    href: "#/applications",
    label: "Applications",
    icon: <IconApps />,
    matches: ["/applications"],
  },
  { href: "#/triggers", label: "Triggers", icon: <IconBolt />, matches: ["/triggers"] },
  {
    href: "#/file-stores",
    label: "Files",
    icon: <IconFolder />,
    matches: ["/file-stores", "/files"],
  },
  {
    href: "#/llm-providers",
    label: "Agents",
    icon: <IconRobot />,
    matches: ["/llm-providers"],
  },
  { href: "#/users", label: "Users", icon: <IconUsers />, matches: ["/users"] },
  { href: "#/roles", label: "Roles", icon: <IconShieldLock />, matches: ["/roles"] },
];

/** The authenticated admin shell: Tabler's vertical layout around the screen. */
function Shell({ user, onLogout }: { user: CurrentUser; onLogout: () => void }) {
  const route = useHashRoute();
  const [menuOpen, setMenuOpen] = useState(false);
  const [theme, toggleTheme] = useTheme();
  const [narrow, toggleNarrow] = useNarrowSidebar();

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
    <div className={narrow ? "page sidebar-narrow" : "page"}>
      <aside className="navbar navbar-vertical navbar-expand-lg" data-bs-theme="dark">
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
            <a href="#/tables" className="d-flex align-items-center gap-2" aria-label="Saltcorn">
              <SaltcornLogo />
              <span>Saltcorn</span>
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
            className={menuOpen ? "collapse navbar-collapse show" : "collapse navbar-collapse"}
            id="sidebar-menu"
          >
            <ul className="navbar-nav pt-lg-3">
              {NAV.map((item) => {
                const active = item.matches.some((prefix) => route.startsWith(prefix));
                return (
                  <li key={item.href} className={active ? "nav-item active" : "nav-item"}>
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
                aria-label={narrow ? "Widen the sidebar" : "Narrow the sidebar to icons"}
                title={narrow ? "Widen the sidebar" : "Narrow the sidebar to icons"}
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
        <Screen route={route} />
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
    </div>
  );
}

/** Light/dark switch. One button that shows the scheme it would switch *to*,
 * which is how Tabler's own header reads (it swaps two links; we swap an icon). */
function ThemeToggle({ theme, onToggle }: { theme: string; onToggle: () => void }) {
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
        {dark ? <IconSun className="icon-1" /> : <IconMoon className="icon-1" />}
      </button>
    </div>
  );
}

/** Resolve the current hash route to a screen. */
function Screen({ route }: { route: string }) {
  const tableMatch = route.match(/^\/tables\/([^/]+)$/);
  if (tableMatch) {
    return <TableDetail table={decodeURIComponent(tableMatch[1])} />;
  }
  if (route === "/applications/new") {
    return <ApplicationForm />;
  }
  const editMatch = route.match(/^\/applications\/([^/]+)\/edit$/);
  if (editMatch) {
    return <ApplicationForm appId={decodeURIComponent(editMatch[1])} />;
  }
  if (route.startsWith("/applications")) {
    return <Applications />;
  }
  if (route === "/triggers/new") {
    return <TriggerForm />;
  }
  const triggerEditMatch = route.match(/^\/triggers\/([^/]+)\/edit$/);
  if (triggerEditMatch) {
    return <TriggerForm triggerId={decodeURIComponent(triggerEditMatch[1])} />;
  }
  if (route.startsWith("/triggers")) {
    return <Triggers />;
  }
  if (route === "/file-stores/new") {
    return <FileStoreForm />;
  }
  const storeEditMatch = route.match(/^\/file-stores\/([^/]+)\/edit$/);
  if (storeEditMatch) {
    return <FileStoreForm storeId={decodeURIComponent(storeEditMatch[1])} />;
  }
  if (route.startsWith("/file-stores")) {
    return <FileStores />;
  }
  // `/files/<store>` opens at the root; `/files/<store>/<dir>` opens in a
  // directory, which is what an application row links to (§2.4).
  const filesMatch = route.match(/^\/files\/([^/]+)(?:\/(.*))?$/);
  if (filesMatch) {
    const dir = (filesMatch[2] ?? "")
      .split("/")
      .filter((s) => s.length > 0)
      .map(decodeURIComponent)
      .join("/");
    return <FileManager store={decodeURIComponent(filesMatch[1])} initialDir={dir} />;
  }
  if (route === "/llm-providers/new") {
    return <LlmProviderForm />;
  }
  const providerEditMatch = route.match(/^\/llm-providers\/([^/]+)\/edit$/);
  if (providerEditMatch) {
    return <LlmProviderForm providerId={decodeURIComponent(providerEditMatch[1])} />;
  }
  if (route.startsWith("/llm-providers")) {
    return <LlmProviders />;
  }
  if (route.startsWith("/users")) {
    return <Users />;
  }
  if (route.startsWith("/roles")) {
    return <Roles />;
  }
  return <Tables />;
}
