// The Analytics UI's shell (analytics TODO A1.14, A4.1, A9.3): who is signed
// in, the language and the colour scheme, a header, and the route — or two
// routes side by side, when the view is split.
//
// On the admin host the session is the admin UI's own: the server serves this
// bundle only to a signed-in admin, so reaching this code signed out means the
// session expired while the page was open. On an Analytics application's host
// (A9.3) the bundle is the application's, and its users sign in here, with the
// application's own session. The server says which it is (`analyticsShell`),
// and an application's shell has no admin links, no models, and — in fixed
// mode — nothing but the workspaces it shows.

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Form from "react-bootstrap/Form";
import Spinner from "react-bootstrap/Spinner";

import { api, errorMessage, errorStatus } from "./api";
import type { AuthStatusResponse } from "./client";
import { NewDatasetPage } from "./datasets/DatasetList";
import { DatasetPage } from "./datasets/DatasetPage";
import { Home } from "./Home";
import { I18nProvider, T, useT } from "./i18n";
import { FitRedirect, ModelCompare } from "./models/ModelCompare";
import { ModelEditor } from "./models/ModelEditor";
import { SplitView, usePane } from "./panes";
import { layoutHash, parseLayout, type Layout, type Route } from "./router";
import { landing, readApplication, shows, ShellProvider, useShell, type ShellInfo } from "./shell";
import { useTheme } from "./theme";
import { WorkspaceFrame } from "./workspaces/WorkspaceFrame";

/** The layout the hash names, kept current. */
function useLayout(): Layout {
  const [layout, setLayout] = useState<Layout>(() => parseLayout(window.location.hash));
  useEffect(() => {
    const onChange = () => setLayout(parseLayout(window.location.hash));
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return layout;
}

export function App() {
  const [status, setStatus] = useState<AuthStatusResponse | null>(null);
  const [shell, setShell] = useState<ShellInfo | "refused" | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .authStatus()
      .then(async (found) => {
        if (found.current_user) {
          try {
            const answer = await api.analyticsShell();
            setShell({ application: readApplication(answer.application), roles: answer.roles });
          } catch (err) {
            // Signed in, and below the application's role floor.
            if (errorStatus(err) === 403) setShell("refused");
            else throw err;
          }
        }
        setStatus(found);
      })
      .catch((err: unknown) => setError(errorMessage(err, "Could not reach the server.")));
  }, []);

  if (error) {
    return (
      <div className="an-page">
        <Alert variant="danger">{error}</Alert>
      </div>
    );
  }
  if (!status) {
    return (
      <div className="an-page">
        <Spinner animation="border" size="sm" />
      </div>
    );
  }
  const signedIn = status.current_user;
  return (
    <I18nProvider locale={status.locales?.current ?? "en"}>
      {!signedIn ? (
        <SignedOut />
      ) : shell === "refused" ? (
        <Refused email={signedIn.email} />
      ) : (
        <ShellProvider value={shell ?? { application: null, roles: [] }}>
          <Shell email={signedIn.email} />
        </ShellProvider>
      )}
    </I18nProvider>
  );
}

/** Nobody signed in: an application's users sign in here, and so may an admin
 * whose session ran out while the page was open. */
function SignedOut() {
  const { t } = useT();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const signIn = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      await api.login({ email: email.trim(), password });
      window.location.reload();
    } catch (err) {
      setError(errorMessage(err, t("Could not sign in.")));
    }
  };
  return (
    <div className="an-page" style={{ maxWidth: "28rem" }}>
      <Card>
        <Card.Body>
          <h1 className="h3 mb-3">
            <T text="Sign in" />
          </h1>
          {error && <Alert variant="danger">{error}</Alert>}
          <Form onSubmit={signIn}>
            <Form.Group className="mb-3" controlId="sign-in-email">
              <Form.Label>
                <T text="Email" />
              </Form.Label>
              <Form.Control
                type="email"
                autoComplete="username"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
            </Form.Group>
            <Form.Group className="mb-3" controlId="sign-in-password">
              <Form.Label>
                <T text="Password" />
              </Form.Label>
              <Form.Control
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Form.Group>
            <Button type="submit" disabled={email.trim() === "" || password === ""}>
              <T text="Sign in" />
            </Button>
          </Form>
        </Card.Body>
      </Card>
    </div>
  );
}

/** Signed in, with a role the application is not for. */
function Refused({ email }: { email: string }) {
  return (
    <div className="an-page">
      <Alert variant="warning">
        <T text="You are signed in as {email}, and this application is not open to your role." args={{ email }} />{" "}
        <SignOutButton />
      </Alert>
    </div>
  );
}

function SignOutButton() {
  return (
    <Button
      size="sm"
      variant="outline-secondary"
      onClick={async () => {
        try {
          await api.logout();
        } finally {
          window.location.reload();
        }
      }}
    >
      <T text="Sign out" />
    </Button>
  );
}

function Shell({ email }: { email: string }) {
  const { t } = useT();
  const { application } = useShell();
  const [theme, toggleTheme] = useTheme();
  const layout = useLayout();
  const split = layout.side !== null;
  return (
    <div className="an-shell">
      <header className="an-header">
        <a className="an-brand" href="#/">
          {application ? application.name : <T text="Analytics" />}
        </a>
        <span className="text-secondary small ms-auto">{email}</span>
        {application?.mode !== "fixed" && (
          <a
            className={split ? "btn btn-sm btn-secondary" : "btn btn-sm btn-outline-secondary"}
            href={layoutHash(
              split ? { main: layout.main, side: null } : { main: layout.main, side: { name: "home" } },
            )}
            aria-pressed={split}
            title={split ? t("Close the right side") : t("Open a second screen beside this one")}
          >
            <T text="Split" />
          </a>
        )}
        <Button
          size="sm"
          variant="outline-secondary"
          onClick={toggleTheme}
          aria-label={theme === "dark" ? t("Light mode") : t("Dark mode")}
        >
          {theme === "dark" ? t("Light") : t("Dark")}
        </Button>
        {application ? (
          <SignOutButton />
        ) : (
          <a className="btn btn-sm btn-outline-secondary" href="/">
            <T text="Admin" />
          </a>
        )}
      </header>
      <main className={split ? "an-main split" : "an-main"}>
        <SplitView layout={layout} render={(route) => <Page route={route} />} />
      </main>
    </div>
  );
}

function Page({ route: asked }: { route: Route }) {
  const pane = usePane();
  const { application } = useShell();
  const route = landing(application, asked);
  if (!shows(application, route)) {
    return (
      <div className="an-page">
        <Alert variant="warning">
          <T text="This application does not show that." />{" "}
          <a href={pane.href({ name: "home" })}>
            <T text="Back to the front page" />
          </a>
        </Alert>
      </div>
    );
  }
  switch (route.name) {
    case "home":
      return application?.mode === "fixed" ? <FixedHome /> : <Home />;
    case "workspace":
      return <WorkspaceFrame id={route.id} key={route.id} />;
    case "dataset":
      return <DatasetPage id={route.id} back={route.back} key={route.id} />;
    case "newDataset":
      return <NewDatasetPage table={route.table} />;
    // Keyed by the model and the fit, so moving between them builds the
    // editor again rather than showing one model's answers under another's.
    case "model":
      return <ModelEditor modelId={route.id} fit={route.fit} key={`${route.id}:${route.fit ?? ""}`} />;
    case "newModel":
      return <ModelEditor dataset={route.dataset} key={`new:${route.dataset ?? ""}`} />;
    case "compareModels":
      return <ModelCompare ids={route.ids} key={route.ids.join(",")} />;
    case "fit":
      return <FitRedirect id={route.id} key={route.id} />;
    case "notFound":
      return (
        <div className="an-page">
          <Alert variant="warning">
            <T text="There is nothing at {path}." args={{ path: route.path }} />{" "}
            <a href={pane.href({ name: "home" })}>
              <T text="Back to the front page" />
            </a>
          </Alert>
        </div>
      );
  }
}

/** A fixed application's front page, when it shows more than one workspace:
 * those workspaces, and nothing else. */
function FixedHome() {
  const pane = usePane();
  const { application } = useShell();
  return (
    <div className="an-page">
      <div className="d-flex flex-wrap gap-3">
        {(application?.workspaces ?? []).map((w) => (
          <Card key={w.id} style={{ minWidth: "16rem" }}>
            <Card.Body>
              <a className="h4 d-block mb-1" href={pane.href({ name: "workspace", id: w.id })}>
                {w.name}
              </a>
            </Card.Body>
          </Card>
        ))}
      </div>
    </div>
  );
}
