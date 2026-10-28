// The Analytics UI's own settings (`#/settings`): the base maps a map is drawn
// over, and with them the hosts the Analytics UI's policy lets a map load
// from. The admin's alone — an Analytics application's shell does not show it
// (`shows`), and the server refuses `saveMapSettings` from one.
//
// The values are the server's to check: a URL that is not one comes back as a
// sentence naming it, and nothing is saved. A saved change reaches the policy
// on the next page load, so the page says so rather than redrawing open maps.

import { useEffect, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Form from "react-bootstrap/Form";
import Spinner from "react-bootstrap/Spinner";

import { api, errorMessage } from "../api";
import type { MapSettingsResponse } from "../client";
import { T, useT } from "../i18n";
import { usePane } from "../panes";
import { changedValues, type MapValues } from "./mapSettings";

export function SettingsPage() {
  const { t } = useT();
  const pane = usePane();
  const [saved, setSaved] = useState<MapSettingsResponse | null>(null);
  const [draft, setDraft] = useState<MapValues | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [done, setDone] = useState(false);

  useEffect(() => {
    let live = true;
    api
      .mapSettings()
      .then((s) => {
        if (!live) return;
        setSaved(s);
        setDraft(s.values);
      })
      .catch((err: unknown) => live && setError(errorMessage(err, t("Could not load the settings."))));
    return () => {
      live = false;
    };
  }, [t]);

  const changes = saved && draft ? changedValues(saved.values, draft) : {};
  const dirty = Object.keys(changes).length > 0;

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setDone(false);
    setSaving(true);
    try {
      const after = await api.saveMapSettings({ values: changes });
      setSaved(after);
      setDraft(after.values);
      setDone(true);
    } catch (err) {
      setError(errorMessage(err, t("The settings could not be saved.")));
    } finally {
      setSaving(false);
    }
  };

  const set = (key: keyof MapValues) => (e: { target: { value: string } }) => {
    setDone(false);
    setDraft((d) => (d ? { ...d, [key]: e.target.value } : d));
  };

  return (
    <div className="an-page" style={{ maxWidth: "48rem" }}>
      <a href={pane.href({ name: "home" })} className="small text-secondary">
        <T text="Back to the front page" />
      </a>
      <h1 className="h2 mt-2 mb-3">
        <T text="Settings" />
      </h1>
      {error && <Alert variant="danger">{error}</Alert>}
      {!draft || !saved ? (
        !error && <Spinner animation="border" size="sm" />
      ) : (
        <Card>
          <Card.Body>
            <h2 className="h3 mb-1">
              <T text="Base maps" />
            </h2>
            <p className="text-secondary">
              <T text="The base map every map is drawn over. Maps may load tiles only from the hosts listed below." />
            </p>
            <Form onSubmit={save}>
              <Form.Group className="mb-3" controlId="settings-map-style">
                <Form.Label>
                  <T text="Base map style" />
                </Form.Label>
                <Form.Control value={draft.map_style} onChange={set("map_style")} placeholder="https://" />
                <Form.Text>
                  <T text="The URL of a MapLibre style, for a light page. Leave it empty for no base map: layers are then drawn on a plain background." />
                </Form.Text>
              </Form.Group>
              <Form.Group className="mb-3" controlId="settings-map-style-dark">
                <Form.Label>
                  <T text="Base map style (dark)" />
                </Form.Label>
                <Form.Control value={draft.map_style_dark} onChange={set("map_style_dark")} placeholder="https://" />
                <Form.Text>
                  <T text="The URL of a MapLibre style, for a dark page. Leave it empty to use the light one." />
                </Form.Text>
              </Form.Group>
              <Form.Group className="mb-3" controlId="settings-map-hosts">
                <Form.Label>
                  <T text="Other map hosts" />
                </Form.Label>
                <Form.Control
                  value={draft.map_hosts}
                  onChange={set("map_hosts")}
                  placeholder="https://tiles.example.com"
                />
                <Form.Text>
                  <T text="Comma-separated origins the style's tiles, fonts or icons come from, when they are not on the style's own host. A reference layer's host is added here when you allow it." />
                </Form.Text>
              </Form.Group>
              <div className="mb-3">
                <div className="form-label">
                  <T text="Hosts maps may load from" />
                </div>
                {saved.hosts.length === 0 ? (
                  <div className="small text-secondary">
                    <T text="None: maps load nothing from outside this server." />
                  </div>
                ) : (
                  <ul className="small mb-0" data-testid="map-hosts">
                    {saved.hosts.map((h) => (
                      <li key={h}>
                        <code>{h}</code>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
              {done && (
                <Alert variant="success">
                  <T text="Saved. Maps opened from now on use these settings; reload a page that is already open." />
                </Alert>
              )}
              <div className="d-flex gap-2">
                <Button type="submit" disabled={!dirty || saving}>
                  <T text="Save" />
                </Button>
                <Button
                  variant="outline-secondary"
                  disabled={!dirty || saving}
                  onClick={() => {
                    setDone(false);
                    setDraft(saved.values);
                  }}
                >
                  <T text="Discard changes" />
                </Button>
              </div>
            </Form>
          </Card.Body>
        </Card>
      )}
    </div>
  );
}
