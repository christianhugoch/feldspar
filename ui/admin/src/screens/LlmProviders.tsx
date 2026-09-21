// LLM providers list: every model endpoint an admin has connected.
//
// The file-stores list with a different noun, minus the two states that made
// that screen interesting. There is no `--llm-provider` flag, so every provider
// has a row and every row is editable; and there is no "connected" state to
// show, because connecting a provider builds an HTTP client and sends nothing —
// whether it *works* is a request, which is what the form's Test connection
// button makes.
//
// The summary column deliberately renders whatever settings a backend declares
// rather than reaching for a `model` only some might have. That is the same
// discipline `FileStores` keeps, and it is what lets a backend added later
// appear here with no change: the values it shows are already redacted by the
// server, so an API key summarises as the sentinel and nothing here has to know
// which setting was the secret.

import { useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import type { ListLlmProvidersResponse } from "../client";
import { navigate } from "../App";
import { IconPlus } from "../icons";
import { PageBody, PageHeader } from "../layout";
import { asString } from "../settings";
import { T, useT } from "../i18n";

type ProviderItem = ListLlmProvidersResponse[number];

/** A one-line summary of a provider's settings, from whatever its backend
 * declared. Long values are elided so one setting cannot push the rest of the
 * row off the screen. */
function configSummary(provider: ProviderItem): string {
  const config = provider.config;
  if (!config || typeof config !== "object") return "";
  return Object.entries(config as Record<string, unknown>)
    .map(([key, value]) => `${key}: ${elide(asString(value))}`)
    .join(", ");
}

/** A value shortened to fit a table cell. */
function elide(value: string, max = 48): string {
  return value.length > max ? `${value.slice(0, max)}…` : value;
}

export function LlmProviders() {
  const { t } = useT();
  const [providers, setProviders] = useState<ProviderItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      setProviders(await api.listLlmProviders());
    } catch {
      setError("Could not load LLM providers.");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const remove = async (provider: ProviderItem) => {
    if (
      !window.confirm(
        t(
          'Remove the LLM provider "{name}"?\n\nAnything configured to use it will stop working until it is repointed.',
          { name: provider.name },
        ),
      )
    ) {
      return;
    }
    setError(null);
    try {
      await api.deleteLlmProvider(provider.id);
      await load();
    } catch (err) {
      // The server names what still references it, so surface its message
      // rather than a generic failure.
      setError(errorMessage(err, "Could not remove the LLM provider."));
    }
  };

  return (
    <>
      <PageHeader
        pretitle="Agents"
        title={t("LLM providers")}
        actions={
          <Button onClick={() => navigate("/llm-providers/new")}>
            <IconPlus className="icon-2" />
            <T text="New provider" />
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}

        <div className="card">
          <Table hover responsive className="card-table table-vcenter">
            <thead>
              <tr>
                <th><T text="Name" /></th>
                <th><T text="Backend" /></th>
                <th><T text="Settings" /></th>
                <th className="text-end"><T text="Actions" /></th>
              </tr>
            </thead>
            <tbody>
              {providers?.length === 0 && (
                <tr>
                  <td colSpan={4} className="text-muted">
                    <T text="No LLM providers yet. Add one to give an agent a model to talk to." />
                  </td>
                </tr>
              )}
              {providers?.map((provider) => (
                <tr key={provider.id}>
                  <td>
                    {provider.name}
                    {provider.description && (
                      <div className="text-muted small">{provider.description}</div>
                    )}
                  </td>
                  <td>{provider.backend}</td>
                  <td className="text-break small">{configSummary(provider)}</td>
                  <td className="text-end">
                    <div className="btn-list justify-content-end flex-nowrap align-items-center">
                      <Button
                        size="sm"
                        variant="outline-secondary"
                        href={`#/llm-providers/${encodeURIComponent(provider.id)}/edit`}
                      >
                        <T text="Edit" />
                      </Button>
                      <Button
                        size="sm"
                        variant="outline-danger"
                        onClick={() => void remove(provider)}
                      >
                        <T text="Remove" />
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        </div>

        <p className="text-muted small mt-3">
          <T text="API keys are stored in the Saltcorn database and are never sent back to this screen — an existing key shows as ••••••••. They are not encrypted at rest, so treat database access as key access." />
        </p>
      </PageBody>
    </>
  );
}
