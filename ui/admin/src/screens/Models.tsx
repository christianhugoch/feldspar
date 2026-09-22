// The models list: every saved question about a table, and what the last fit of
// it came to (TODO "Predictive models", task 6.1).
//
// The agents list with a fifth noun, including the part both share: a model that
// no longer validates — a dataset column whose formula stopped resolving, a
// provider whose module was uninstalled — is **still here**, marked, with the
// reason in the badge's tooltip. Editing it is the repair, so hiding it would
// hide the screen the repair happens on.
//
// Two things are on this screen that are not on the agents one. A model carries
// its **last fit**, because that is what the list is read for: which models have
// been fitted, when, and what they scored. And a build made with
// `--no-default-features` has the two hypothesis tests and nothing else, so the
// picker's emptiness is explained here rather than being left to read like a
// bug.

import { useEffect, useState } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import { navigate } from "../App";
import { IconPlus } from "../icons";
import { PageBody, PageHeader, StatusBadge, type Tone } from "../layout";
import {
  formatTimestamp,
  headlineMetric,
  instanceLabel,
  outcomeSummary,
  readMetrics,
  readOutcome,
  type ModelItem,
} from "../models";
import { T, useT } from "../i18n";

/** How a fit's status is coloured: the one still running is *blue* rather than
 * green, because it has not answered anything yet. */
export function fitTone(status: string): Tone {
  if (status === "fitted") return "green";
  if (status === "failed") return "red";
  return "blue";
}

/** One model's last fit as a cell: what it is called, what it scored, and when. */
function LastFit({ model }: { model: ModelItem }) {
  const fit = model.active_instance ?? model.last_fit;
  if (!fit) return <span className="text-muted"><T text="Never fitted" /></span>;
  const headline = headlineMetric(readMetrics(fit.metrics));
  return (
    <>
      <div className="d-flex align-items-center gap-2">
        <StatusBadge tone={fitTone(fit.status)} title={fit.error ?? undefined}>
          {fit.status}
        </StatusBadge>
        {fit.active && <StatusBadge tone="green"><T text="active" /></StatusBadge>}
      </div>
      <div className="text-muted small">
        {instanceLabel(fit)}
        {headline && ` · ${headline}`}
      </div>
      {/* Only for a fit that *was* named: an unnamed one is already addressed by
          when it happened, and printing the time twice says nothing twice. */}
      {fit.name.trim() !== "" && (
        <div className="text-muted small">{formatTimestamp(fit.created)}</div>
      )}
    </>
  );
}

export function Models() {
  const { t } = useT();
  const [models, setModels] = useState<ModelItem[] | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      setModels(await api.listModels());
      setError(null);
    } catch (err) {
      // Includes the sentence a server built without model support answers
      // with, which is the honest thing to show on a tab that cannot work.
      setError(errorMessage(err, "Could not load the models."));
    }
  };

  useEffect(() => {
    void load();
    // The providers are read only for the notice: what this build was made
    // without is a fact about the build, and an empty picker on the model form
    // reads like a bug where this reads like the decision it is.
    void api
      .listModelProviders()
      .then((listed) => setNotice(listed.builtins_compiled_out ? (listed.notice ?? null) : null))
      .catch(() => setNotice(null));
  }, []);

  const remove = async (model: ModelItem) => {
    if (
      !window.confirm(
        t(
          'Remove the model "{name}"?\n\nIts fits go with it: an instance is not a record of what happened, it is a fit of this model, and its parameters mean nothing without the dataset they were fitted over.',
          { name: model.name },
        ),
      )
    ) {
      return;
    }
    setError(null);
    try {
      await api.deleteModel(model.id);
      await load();
    } catch (err) {
      setError(errorMessage(err, "Could not remove the model."));
    }
  };

  return (
    <>
      <PageHeader
        pretitle="Models"
        title={t("Models")}
        actions={
          <Button onClick={() => navigate("/models/new")}>
            <IconPlus className="icon-2" />
            <T text="New model" />
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}
        {notice && <Alert variant="info">{notice}</Alert>}

        <div className="card">
          <Table hover responsive className="card-table table-vcenter">
            <thead>
              <tr>
                <th><T text="Name" /></th>
                <th><T text="Table" /></th>
                <th><T text="Provider" /></th>
                <th><T text="Outcome" /></th>
                <th><T text="Last fit" /></th>
                <th className="text-end"><T text="Actions" /></th>
              </tr>
            </thead>
            <tbody>
              {models?.length === 0 && (
                <tr>
                  <td colSpan={6} className="text-muted">
                    <T text="No models yet. A model is a table, some columns computed from it, and a provider that answers a question about them." />
                  </td>
                </tr>
              )}
              {models?.map((model) => {
                const fit = model.active_instance ?? model.last_fit;
                return (
                  <tr key={model.id}>
                    <td>
                      {model.name}
                      {model.description && (
                        <div className="text-muted small">{model.description}</div>
                      )}
                      {/* The reason lives in the tooltip: it is a sentence, and
                          a sentence in a table cell would push the row apart. */}
                      {model.error && (
                        <StatusBadge tone="red" title={model.error} className="mt-1">
                          <T text="Cannot be fitted" />
                        </StatusBadge>
                      )}
                    </td>
                    <td>
                      <a href={`#/tables/${encodeURIComponent(model.table_name)}`}>
                        {model.table_name}
                      </a>
                    </td>
                    <td>{model.provider}</td>
                    {/* The outcome is a fit's, not a declaration's: a random
                        forest is a regressor or a classifier depending on the
                        type of the column its configuration names, and what
                        this build actually produced is what the fit says. */}
                    <td>{fit ? outcomeSummary(readOutcome(fit.outcome)) : "—"}</td>
                    <td>
                      <LastFit model={model} />
                    </td>
                    <td className="text-end">
                      <div className="btn-list justify-content-end flex-nowrap align-items-center">
                        <Button
                          size="sm"
                          variant="primary"
                          href={`#/models/${encodeURIComponent(model.id)}`}
                        >
                          <T text="Open" />
                        </Button>
                        <Button
                          size="sm"
                          variant="outline-danger"
                          onClick={() => void remove(model)}
                        >
                          <T text="Remove" />
                        </Button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </Table>
        </div>
       
      </PageBody>
    </>
  );
}
