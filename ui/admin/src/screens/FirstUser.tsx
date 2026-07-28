// Create-first-user screen: shown only while no user exists yet. Creating the
// first user makes them the admin and logs them straight in (the server starts a
// session on success), so on completion we just re-check auth status.

import { useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Form from "react-bootstrap/Form";

import { api } from "../api";
import { SaltcornLogo } from "../icons";
import { CenteredPage } from "../layout";

export function FirstUser({ onCreated }: { onCreated: () => void }) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.createFirstUser({ email, password });
      onCreated();
    } catch {
      setError("Could not create the first user.");
      setBusy(false);
    }
  };

  return (
    <CenteredPage>
      <div className="text-center mb-4">
        <span className="navbar-brand d-inline-flex align-items-center gap-2">
          <SaltcornLogo className="h-6" />
          <span className="h1 mb-0">Saltcorn</span>
        </span>
      </div>
      <Card className="card-md">
        <Card.Body>
          <Card.Title as="h1" className="h3 text-center mb-2">
            Welcome to Saltcorn
          </Card.Title>
          <p className="text-secondary text-center mb-4">
            Create the first administrator to get started.
          </p>
          {error && <Alert variant="danger">{error}</Alert>}
          <Form onSubmit={submit}>
            <Form.Group className="mb-3" controlId="firstUserEmail">
              <Form.Label>Email</Form.Label>
              <Form.Control
                type="email"
                value={email}
                autoComplete="username"
                required
                onChange={(e) => setEmail(e.target.value)}
              />
            </Form.Group>
            <Form.Group className="mb-3" controlId="firstUserPassword">
              <Form.Label>Password</Form.Label>
              <Form.Control
                type="password"
                value={password}
                autoComplete="new-password"
                required
                onChange={(e) => setPassword(e.target.value)}
              />
            </Form.Group>
            <div className="form-footer">
              <Button type="submit" className="w-100" disabled={busy}>
                {busy ? "Creating…" : "Create admin"}
              </Button>
            </div>
          </Form>
        </Card.Body>
      </Card>
    </CenteredPage>
  );
}
