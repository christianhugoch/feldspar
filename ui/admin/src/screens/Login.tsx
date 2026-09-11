// Login screen: shown when a user exists but nobody is authenticated. A
// successful login starts a session cookie; a 401 means bad credentials.

import { useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Form from "react-bootstrap/Form";

import { api, errorStatus } from "../api";
import { SaltcornLogo } from "../icons";
import { CenteredPage } from "../layout";

export function Login({ onLoggedIn }: { onLoggedIn: () => void }) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.login({ email, password });
      onLoggedIn();
    } catch (err) {
      setError(
        errorStatus(err) === 401
          ? "Invalid email or password."
          : "Login failed. Please try again.",
      );
      setBusy(false);
    }
  };

  return (
    <CenteredPage>
      <div className="text-center mb-4">
        <span className="navbar-brand d-inline-flex align-items-center gap-2">
          <SaltcornLogo className="h-6" />
            <div className="ms-2 login-logo">
              <div className="saltcorn-label">Saltcorn</div>
              <div className="feldspar-label">Feldspar</div>
            </div>
        </span>
      </div>
      <Card className="card-md">
        <Card.Body>
          <Card.Title as="h1" className="h3 text-center mb-4">
            Sign in
          </Card.Title>
          {error && <Alert variant="danger">{error}</Alert>}
          <Form onSubmit={submit}>
            <Form.Group className="mb-3" controlId="loginEmail">
              <Form.Label>Email</Form.Label>
              <Form.Control
                type="email"
                value={email}
                autoComplete="username"
                required
                onChange={(e) => setEmail(e.target.value)}
              />
            </Form.Group>
            <Form.Group className="mb-3" controlId="loginPassword">
              <Form.Label>Password</Form.Label>
              <Form.Control
                type="password"
                value={password}
                autoComplete="current-password"
                required
                onChange={(e) => setPassword(e.target.value)}
              />
            </Form.Group>
            <div className="form-footer">
              <Button type="submit" className="w-100" disabled={busy}>
                {busy ? "Signing in…" : "Sign in"}
              </Button>
            </div>
          </Form>
        </Card.Body>
      </Card>
    </CenteredPage>
  );
}
