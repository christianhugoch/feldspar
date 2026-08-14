// The Email tab's one act: send a message and see whether it arrives.
//
// This is the answer to a question the form cannot answer. Every other setting
// on this screen either takes effect at the next restart or is checked when it
// is saved; "is this password right, does this relay accept mail from this
// address" is a question only the network answers, and an admin who has to wait
// for a trigger to fire to hear the answer has no way to tell a wrong password
// from a wrong template.
//
// Two things about it are deliberate and would be wrong the other way:
//
// - **It sends through what is stored, not what is typed**, so the note under
//   the button says to save first. A button that tested the boxes would report
//   success on a configuration that is not the one this installation sends with.
// - **The failure is shown verbatim.** "connection refused", "authentication
//   failed" and "relay access denied" are three different problems with three
//   different fixes, and a friendlier summary would throw away the only thing
//   that makes this button worth pressing.

import { useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";

import { api } from "../api";
import { AlertBody } from "../layout";

/** What the test-email form sends: an address, or nothing at all.
 *
 * An empty box is **omitted** rather than sent as `""`, because the server's
 * default is the signed-in admin's own address and "send it to me" is what an
 * empty box means. Sending `""` would ask the server to parse the empty string
 * as a recipient. */
export function testEmailBody(to: string): { to?: string } {
  const trimmed = to.trim();
  return trimmed === "" ? {} : { to: trimmed };
}

export function TestEmail() {
  const [to, setTo] = useState("");
  const [busy, setBusy] = useState(false);
  const [sentTo, setSentTo] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const send = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setSentTo(null);
    setError(null);
    try {
      const result = await api.sendTestEmail(testEmailBody(to));
      setSentTo(result.sent_to);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not send the test message.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card mb-4">
      <div className="card-header">
        <div>
          <h3 className="card-title">Send test email</h3>
          <p className="card-subtitle text-secondary mb-0">
            Sends one message through the settings as they are <strong>stored</strong>. Save
            your changes above first, or this tests the previous configuration.
          </p>
        </div>
      </div>
      <div className="card-body">
        {error && (
          <Alert variant="danger" dismissible onClose={() => setError(null)}>
            <AlertBody>{error}</AlertBody>
          </Alert>
        )}
        {sentTo && (
          <Alert variant="success" dismissible onClose={() => setSentTo(null)}>
            <AlertBody>
              Sent to {sentTo}. The mail server accepted it — whether it is delivered is now
              between that server and the recipient's.
            </AlertBody>
          </Alert>
        )}
        <form onSubmit={(e) => void send(e)}>
          <Form.Group className="mb-3" controlId="test-email-to">
            <Form.Label>Send to</Form.Label>
            <Form.Control
              type="text"
              value={to}
              placeholder="your own address"
              onChange={(e) => setTo(e.target.value)}
            />
            <Form.Text muted>Leave empty to send it to your own account's address.</Form.Text>
          </Form.Group>
          <Button type="submit" variant="outline-primary" disabled={busy}>
            {busy ? "Sending…" : "Send test email"}
          </Button>
        </form>
      </div>
    </div>
  );
}
