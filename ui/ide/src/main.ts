/**
 * Saltcorn's file-store IDE (design §12.1).
 *
 * A page, not a screen: VS Code initializes once per page and cannot be unloaded,
 * so the workbench gets its own bundle and its own route (`/ide/?store=<name>`)
 * outside the admin SPA. The store is chosen by the query parameter, which is also
 * why there is no client-side router here — switching stores is a navigation.
 */
import "./style.css";

const CONTAINER_ID = "workbench";

/** The store this page edits, or `null` when the URL does not name one. */
function requestedStore(): string | null {
  const store = new URL(window.location.href).searchParams.get("store")?.trim();
  return store == null || store === "" ? null : store;
}

/**
 * The container the workbench renders into.
 *
 * `index.html` carries it, but the server also serves a bootstrap document (for a
 * request that arrives before the bundle's `index.html` is in place), so the
 * element is created when it is missing rather than assumed.
 */
function workbenchContainer(): HTMLElement {
  const existing = document.getElementById(CONTAINER_ID);
  if (existing != null) {
    return existing;
  }
  const created = document.createElement("div");
  created.id = CONTAINER_ID;
  document.body.append(created);
  return created;
}

/** Say what is wrong, in the page, rather than failing silently in the console. */
function renderMessage(title: string, detail: string, link?: { href: string; text: string }): void {
  const container = workbenchContainer();
  container.replaceChildren();
  const box = document.createElement("div");
  box.className = "startup-message";
  const heading = document.createElement("h1");
  heading.textContent = title;
  const paragraph = document.createElement("p");
  paragraph.textContent = detail;
  box.append(heading, paragraph);
  if (link != null) {
    const anchor = document.createElement("a");
    anchor.href = link.href;
    anchor.textContent = link.text;
    box.append(anchor);
  }
  container.append(box);
}

const store = requestedStore();
if (store == null) {
  renderMessage(
    "No file store named",
    "This page edits one file store, named by the `store` query parameter — for example /ide/?store=app-source.",
    { href: "/", text: "Back to the admin UI" },
  );
} else {
  document.title = `${store} — Saltcorn IDE`;
  try {
    const { bootWorkbench } = await import("./workbench");
    await bootWorkbench(store, workbenchContainer());
  } catch (err) {
    renderMessage(
      "The editor failed to start",
      err instanceof Error ? err.message : String(err),
      { href: "/", text: "Back to the admin UI" },
    );
    throw err;
  }
}
