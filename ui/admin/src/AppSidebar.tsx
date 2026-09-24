// The Applications section of the sidebar: a picker for the current application,
// and under it the links for working on that one (`appNav.ts` decides which).
//
// The picker is Tabler's own sidebar submenu — a `nav-item dropdown` — rather
// than a `<select>`. A native select cannot be made to look like the rest of a
// dark Tabler sidebar, and it has nothing to offer a folded one; the submenu is
// styled for both by Tabler itself: unfolded, the menu opens inline under the
// toggle like any submenu, and folded to a rail it flies out beside it. The
// toggle's own label is the current application's name, so which one the links
// below are about is on screen whenever the sidebar is wide enough to say so
// (folded, it is the toggle's hover label).
//
// Which application is current is remembered across visits, and a route that
// is about an application makes that application current — so opening one's
// views from the applications list brings the sidebar along.

import { useEffect, useRef, useState, type ReactNode } from "react";

import { api } from "./api";
import {
  buildApplication,
  buildStatus,
  showAppOutcome,
  useAppActions,
} from "./appActions";
import {
  appIdFromRoute,
  appNavLinks,
  builderAgentFor,
  linkActive,
  onApplicationsList,
  type AppNavLink,
} from "./appNav";
import { splitRoute } from "./builder";
import type { ListAgentsResponse, ListApplicationsResponse } from "./client";
import {
  IconApps,
  IconBooks,
  IconCode,
  IconExternalLink,
  IconFile,
  IconHammer,
  IconLanguage,
  IconLayoutDashboard,
  IconMessagePlus,
  IconSettings,
} from "./icons";
import { T, useT } from "./i18n";

type AppItem = ListApplicationsResponse[number];
type AgentItem = ListAgentsResponse[number];

const CURRENT_APP_KEY = "saltcorn-admin-application";

const LINK_ICONS: Record<AppNavLink["id"], ReactNode> = {
  "app-link": <IconExternalLink />,
  "edit-code": <IconCode />,
  build: <IconHammer />,
  chat: <IconMessagePlus />,
  views: <IconLayoutDashboard />,
  pages: <IconFile />,
  library: <IconBooks />,
  translations: <IconLanguage />,
  settings: <IconSettings />,
};

function storedAppId(): string | null {
  try {
    return window.localStorage.getItem(CURRENT_APP_KEY);
  } catch {
    return null;
  }
}

/** The application picker and the current application's links, as the `<li>`s
 * of the sidebar's nav list. */
export function ApplicationsNav({ route, folded }: { route: string; folded: boolean }) {
  const { path } = splitRoute(route);
  const actions = useAppActions();
  const [apps, setApps] = useState<AppItem[] | null>(null);
  const [agents, setAgents] = useState<AgentItem[]>([]);
  const [currentId, setCurrentId] = useState<string | null>(storedAppId);
  const [open, setOpen] = useState(false);
  const pickerRef = useRef<HTMLLIElement>(null);

  // Fetched again on every route change, and whenever a screen says the set
  // changed without one (a delete on the list stays on the list): the names,
  // frameworks and builder agents shown here are other screens' to edit.
  useEffect(() => {
    let live = true;
    api
      .listApplications()
      .then((found) => live && setApps(found))
      .catch(() => live && setApps((prev) => prev ?? []));
    // A server without agents installed has no builder agents to offer a chat
    // with — the links simply leave that one out.
    api
      .listAgents()
      .then((found) => live && setAgents(found))
      .catch(() => live && setAgents([]));
    return () => {
      live = false;
    };
  }, [route, actions.version]);

  useEffect(() => {
    const fromRoute = appIdFromRoute(path);
    if (fromRoute) setCurrentId(fromRoute);
    setOpen(false);
  }, [path]);

  useEffect(() => {
    try {
      if (currentId) window.localStorage.setItem(CURRENT_APP_KEY, currentId);
      else window.localStorage.removeItem(CURRENT_APP_KEY);
    } catch {
      // A remembered choice is a convenience; the picker works without it.
    }
  }, [currentId]);

  // Folded, the menu is a flyout over the page, and a flyout that only closes
  // from its own toggle is one that gets left open over whatever is under it.
  useEffect(() => {
    if (!open) return;
    const onDown = (event: MouseEvent) => {
      if (!pickerRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const current = apps?.find((app) => app.id === currentId) ?? null;
  const label = current
    ? current.name
    : apps && apps.length === 0
      ? "No applications"
      : "Choose application";
  const links = current ? appNavLinks(current, builderAgentFor(current, agents), window.location) : [];
  const listActive = onApplicationsList(path);

  const choose = (app: AppItem) => {
    setCurrentId(app.id);
    setOpen(false);
  };

  return (
    <>
      <li
        ref={pickerRef}
        className={`nav-item dropdown${listActive ? " active" : ""}`}
      >
        <a
          className={`nav-link dropdown-toggle${open ? " show" : ""}`}
          href="#"
          role="button"
          aria-expanded={open}
          aria-haspopup="menu"
          aria-label={`Current application: ${label}. Choose another`}
          title={folded ? label : undefined}
          onClick={(event) => {
            event.preventDefault();
            setOpen((o) => !o);
          }}
        >
          <span className="nav-link-icon d-md-none d-lg-inline-block">
            <IconApps />
          </span>
          <span
            className={`nav-link-title app-picker-title${current ? "" : " text-secondary"}`}
          >
            {label}
          </span>
        </a>
        <div className={`dropdown-menu${open ? " show" : ""}`} role="menu">
          {apps?.map((app) => (
            <button
              key={app.id}
              type="button"
              role="menuitemradio"
              aria-checked={app.id === current?.id}
              className={`dropdown-item${app.id === current?.id ? " active" : ""}`}
              onClick={() => choose(app)}
            >
              {app.name}
            </button>
          ))}
          {apps?.length === 0 && (
            <span className="dropdown-item disabled"><T text="No applications yet" /></span>
          )}
          <div className="dropdown-divider" />
          <a
            className={`dropdown-item${path === "/applications" ? " active" : ""}`}
            href="#/applications"
            role="menuitem"
          >
            <T text="All applications" />
          </a>
          <a
            className={`dropdown-item${path === "/applications/new" ? " active" : ""}`}
            href="#/applications/new"
            role="menuitem"
          >
            <T text="New application" />
          </a>
        </div>
      </li>
      {current &&
        links.map((link) => (
          <AppLink
            key={link.id}
            link={link}
            app={current}
            active={linkActive(link, path)}
            folded={folded}
            busy={link.id === "build" && buildStatus(actions, current.id) === "building"}
          />
        ))}
    </>
  );
}

/** One of the current application's links: somewhere to go, or (Build)
 * something to do, which reports back through `appActions`. */
function AppLink({
  link,
  app,
  active,
  folded,
  busy,
}: {
  link: AppNavLink;
  app: AppItem;
  active: boolean;
  folded: boolean;
  busy: boolean;
}) {
  const text = busy ? "Building…" : link.label;
  const content = (
    <>
      <span className="nav-link-icon d-md-none d-lg-inline-block">{LINK_ICONS[link.id]}</span>
      <span className="nav-link-title">{text}</span>
    </>
  );
  const title = folded ? text : link.id === "build"
    ? "Rewrite this application's generated client, hooks and schema from its current definition, then build it"
    : undefined;

  return (
    <li className={active ? "nav-item active" : "nav-item"}>
      {link.href ? (
        <a
          className={active ? "nav-link active" : "nav-link"}
          href={link.href}
          aria-current={active ? "page" : undefined}
          target={link.external ? "_blank" : undefined}
          rel={link.external ? "noreferrer" : undefined}
          title={title}
        >
          {content}
        </a>
      ) : (
        <button
          type="button"
          className="nav-link w-100"
          disabled={busy}
          title={title}
          onClick={() => void buildApplication(app)}
        >
          {content}
        </button>
      )}
    </li>
  );
}

/** The news from a build or a client update, over whichever screen the admin is
 * on when it arrives. Tabler's toast, top right, until dismissed: a failed
 * build's diagnostics are the thing to read, so it does not time out. */
export function AppOutcomeToast() {
  const { t } = useT();
  const { outcome } = useAppActions();
  if (!outcome) return null;
  return (
    <div className="toast-container position-fixed top-0 end-0 p-3">
      <div
        className={`toast show app-outcome-toast border-${outcome.ok ? "success" : "danger"}`}
        role={outcome.ok ? "status" : "alert"}
        aria-live={outcome.ok ? "polite" : "assertive"}
      >
        <div className="toast-header">
          <strong className={`me-auto text-${outcome.ok ? "success" : "danger"}`}>
            {outcome.title}
          </strong>
          <button
            type="button"
            className="btn-close"
            aria-label={t("Close")}
            onClick={() => showAppOutcome(null)}
          />
        </div>
        <div className="toast-body">
          <pre className="mb-0 text-break text-pre-wrap app-outcome-log">{outcome.text}</pre>
        </div>
      </div>
    </div>
  );
}
