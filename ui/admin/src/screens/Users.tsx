// Users screen: list users and create new ones.
//
// A user's role is a reference to a row in `_sc_roles` (design §7.1, §9), so the
// form offers the roles that exist rather than a free integer: a number with no
// role behind it is a user whose privileges cannot be described, and the
// database refuses it. New roles are made on the Roles screen.

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Col from "react-bootstrap/Col";
import Form from "react-bootstrap/Form";
import Row from "react-bootstrap/Row";
import Table from "react-bootstrap/Table";

import { api } from "../api";
import type { ListUsersResponse } from "../client";
import { roleLabel, useRoles } from "../roles";

export function Users() {
  const [users, setUsers] = useState<ListUsersResponse | null>(null);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState(100);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const roles = useRoles();

  const load = async () => {
    try {
      setUsers(await api.listUsers());
    } catch {
      setError("Could not load users.");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const create = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.createUser({ email: email.trim(), password, role });
      setEmail("");
      setPassword("");
      await load();
    } catch {
      setError("Could not create the user.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <h1 className="h3 mb-4">Users</h1>
      {error && <Alert variant="danger">{error}</Alert>}

      <Row>
        <Col lg={7} className="mb-4">
          <Table hover responsive>
            <thead>
              <tr>
                <th>Email</th>
                <th>Role</th>
              </tr>
            </thead>
            <tbody>
              {users?.length === 0 && (
                <tr>
                  <td colSpan={2} className="text-muted">
                    No users yet.
                  </td>
                </tr>
              )}
              {users?.map((u) => (
                <tr key={u.id}>
                  <td>{u.email}</td>
                  <td>{roleLabel(u.role, roles)}</td>
                </tr>
              ))}
            </tbody>
          </Table>
        </Col>
        <Col lg={5} className="mb-4">
          <Card>
            <Card.Header>Add user</Card.Header>
            <Card.Body>
              <Form onSubmit={create}>
                <Form.Group className="mb-2" controlId="userEmail">
                  <Form.Label>Email</Form.Label>
                  <Form.Control
                    type="email"
                    value={email}
                    autoComplete="off"
                    required
                    onChange={(e) => setEmail(e.target.value)}
                  />
                </Form.Group>
                <Form.Group className="mb-2" controlId="userPassword">
                  <Form.Label>Password</Form.Label>
                  <Form.Control
                    type="password"
                    value={password}
                    autoComplete="new-password"
                    required
                    onChange={(e) => setPassword(e.target.value)}
                  />
                </Form.Group>
                <Form.Group className="mb-3" controlId="userRole">
                  <Form.Label>Role</Form.Label>
                  <Form.Select value={role} onChange={(e) => setRole(Number(e.target.value))}>
                    {(roles ?? []).map((r) => (
                      <option key={r.role} value={r.role}>
                        {r.name} ({r.role})
                      </option>
                    ))}
                  </Form.Select>
                  <Form.Text muted>
                    Lower is more privileged. Add roles on the <a href="#/roles">Roles</a> screen.
                  </Form.Text>
                </Form.Group>
                <Button type="submit" disabled={busy}>
                  Create user
                </Button>
              </Form>
            </Card.Body>
          </Card>
        </Col>
      </Row>
    </>
  );
}
