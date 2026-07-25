// Top-level admin app: bootstraps auth state and gates the three top-level
// states the design calls for — create-first-user, login, and the authenticated
// admin shell — then routes between the admin screens with a tiny hash router
// (no router dependency, and no inline styles, so the strict CSP holds).

import { useCallback, useEffect, useState } from "react";
import Container from "react-bootstrap/Container";
import Nav from "react-bootstrap/Nav";
import Navbar from "react-bootstrap/Navbar";
import Spinner from "react-bootstrap/Spinner";

import { api } from "./api";
import type { AuthStatusResponse } from "./client";
import { Applications } from "./screens/Applications";
import { ApplicationForm } from "./screens/ApplicationForm";
import { FileManager } from "./screens/FileManager";
import { FileStores } from "./screens/FileStores";
import { FileStoreForm } from "./screens/FileStoreForm";
import { FirstUser } from "./screens/FirstUser";
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
      <Container className="py-5">
        <div className="alert alert-danger">{error}</div>
      </Container>
    );
  }

  if (!status) {
    return (
      <Container className="py-5 text-center">
        <Spinner animation="border" role="status" />
      </Container>
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

/** The authenticated admin shell: a nav bar plus the routed screen. */
function Shell({ user, onLogout }: { user: CurrentUser; onLogout: () => void }) {
  const route = useHashRoute();

  const logout = async () => {
    try {
      await api.logout();
    } finally {
      onLogout();
    }
  };

  return (
    <>
      <Navbar bg="dark" variant="dark" expand="lg" className="mb-4">
        <Container>
          <Navbar.Brand href="#/tables">Saltcorn</Navbar.Brand>
          <Nav className="me-auto">
            <Nav.Link href="#/tables" active={route.startsWith("/tables")}>
              Tables
            </Nav.Link>
            <Nav.Link
              href="#/applications"
              active={route.startsWith("/applications")}
            >
              Applications
            </Nav.Link>
            <Nav.Link href="#/triggers" active={route.startsWith("/triggers")}>
              Triggers
            </Nav.Link>
            <Nav.Link
              href="#/file-stores"
              active={route.startsWith("/file-stores") || route.startsWith("/files")}
            >
              Files
            </Nav.Link>
            <Nav.Link href="#/users" active={route.startsWith("/users")}>
              Users
            </Nav.Link>
            <Nav.Link href="#/roles" active={route.startsWith("/roles")}>
              Roles
            </Nav.Link>
          </Nav>
          <Navbar.Text className="me-3">{user.email}</Navbar.Text>
          <Nav>
            <Nav.Link onClick={logout}>Log out</Nav.Link>
          </Nav>
        </Container>
      </Navbar>
      <Container>
        <Screen route={route} />
      </Container>
    </>
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
  if (route.startsWith("/users")) {
    return <Users />;
  }
  if (route.startsWith("/roles")) {
    return <Roles />;
  }
  return <Tables />;
}
