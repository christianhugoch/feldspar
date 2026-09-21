// One model: its dataset, its provider, its hyperparameter space, its split —
// and the fits it has had (TODO "Predictive models", tasks 6.2, 6.3, 6.4).
//
// Three cards and one form, and the order is the order the questions come in:
// **which data**, then **which provider and with what settings**, then **how the
// rows are divided**. The dataset is first because everything else is about it —
// a provider's own form is built over the dataset's columns, and its label
// picker cannot offer `price` until `price` is a column of this dataset.
//
// The dataset builder is a **column list**, each row a name and a formula, with
// a picker that writes formulas into it (§2). There is deliberately no second
// vocabulary of "field / joinfield / aggregation": the picker's three groups all
// write the one expression language the calc-field editor already speaks, so an
// admin who wants `log(price)` types it and an admin who wants
// `neighbourhoodⱵaverage_income` clicks for it. The preview beside it is what
// makes that a thing you can see the answer of before you fit against it — and
// the types it reports are the *data's*, which is what the provider's form is
// built from and what no schema carries.
//
// **Fit saves first.** A fit of what is on the screen and a save of what is on
// the screen are the same intention, and the alternative is a button that
// silently fits the last saved version of a form the admin has been editing.

import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import Alert from "react-bootstrap/Alert";
import Button from "react-bootstrap/Button";
import Card from "react-bootstrap/Card";
import Col from "react-bootstrap/Col";
import Form from "react-bootstrap/Form";
import Row from "react-bootstrap/Row";
import Spinner from "react-bootstrap/Spinner";
import Table from "react-bootstrap/Table";

import { api, errorMessage } from "../api";
import { navigate } from "../App";
import type { PreviewDatasetResponse } from "../client";
import { catalog, type TableInfo } from "../codeTypes";
import { IconArrowLeft, IconPlus, IconTrash } from "../icons";
import { PageBody, PageHeader, StatusBadge } from "../layout";
import {
  DEFAULT_SPLIT,
  MAX_GRID_POINTS,
  buildHyperparameters,
  formatTimestamp,
  formulaChoices,
  gridPoints,
  headlineMetric,
  instanceLabel,
  orderInstances,
  outcomeSummary,
  printGridValue,
  readDataset,
  readHyperparameters,
  readMetrics,
  readOutcome,
  readSplit,
  suggestColumnName,
  uniqueColumnName,
  type DatasetColumn,
  type InstanceItem,
  type ProviderItem,
} from "../models";
import { SettingsFields, buildConfig, readConfig } from "../settings";
import { fitTone } from "./Models";
import { T, useT } from "../i18n";

/** How long the form waits after a keystroke before asking the server what the
 * dataset answers. Long enough that typing a formula is not a request per
 * character, short enough that stopping typing shows the rows. */
const DEBOUNCE_MS = 600;

/** How often a `fitting` instance is re-read (§8: the row is the registry, so
 * the screen polls — there is nothing to await). */
const POLL_MS = 1500;

/** The split as the form edits it: four boxes of text, so a half-typed `0.` is
 * not a number this form has to have an opinion about. */
type SplitForm = { train: string; validation: string; test: string; seed: string };

export function ModelForm({ modelId }: { modelId?: string }) {
  const { t } = useT();
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  const [busy, setBusy] = useState(false);

  const [id, setId] = useState<string | null>(modelId ?? null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [table, setTable] = useState("");
  const [columns, setColumns] = useState<DatasetColumn[]>([]);
  const [filter, setFilter] = useState("");
  const [provider, setProvider] = useState("");
  const [config, setConfig] = useState<Record<string, string>>({});
  const [hyper, setHyper] = useState<Record<string, string>>({});
  const [split, setSplit] = useState<SplitForm>({
    train: String(DEFAULT_SPLIT.train),
    validation: String(DEFAULT_SPLIT.validation),
    test: String(DEFAULT_SPLIT.test),
    seed: "0",
  });

  const [tables, setTables] = useState<string[]>([]);
  const [schema, setSchema] = useState<TableInfo[]>([]);
  const [providers, setProviders] = useState<ProviderItem[]>([]);
  const [resolved, setResolved] = useState(false);
  const [preview, setPreview] = useState<PreviewDatasetResponse | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [instances, setInstances] = useState<InstanceItem[]>([]);

  // The dataset as the API takes it, and as the two lookups key off. Stringified
  // because that is what a query parameter carries and what an effect can
  // compare — a fresh object every render would re-fetch on every keystroke.
  const dataset = useMemo(
    () => ({
      table,
      columns: columns.filter((c) => c.name.trim() !== "" || c.expr.trim() !== ""),
      filter: filter.trim() === "" ? null : filter.trim(),
    }),
    [table, columns, filter],
  );
  const datasetJson = JSON.stringify(dataset);
  const configJson = JSON.stringify(buildConfig(specOf(providers, provider), config));

  // --- loading ---------------------------------------------------------------

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const [tableList, schemaList] = await Promise.all([
          api.listTables(),
          catalog().catch((): TableInfo[] => []),
        ]);
        let existing = null;
        if (modelId) existing = await api.getModel(modelId);
        if (cancelled) return;
        setTables(tableList.map((t) => t.name));
        setSchema(schemaList);
        if (existing) {
          const stored = readDataset(existing.dataset, existing.table_name);
          setName(existing.name);
          setDescription(existing.description);
          setTable(stored.table);
          setColumns(stored.columns);
          setFilter(stored.filter ?? "");
          setProvider(existing.provider);
          setConfig(readConfig(existing.configuration));
          setHyper(readHyperparameters(existing.hyperparameters));
          const storedSplit = readSplit(existing.split);
          setSplit({
            train: String(storedSplit.train),
            validation: String(storedSplit.validation),
            test: String(storedSplit.test),
            seed: String(storedSplit.seed),
          });
          if (existing.error) setError(existing.error);
        } else {
          setTable(tableList[0]?.name ?? "");
        }
        setReady(true);
      } catch (err) {
        if (!cancelled) setLoadError(errorMessage(err, "Could not load this model."));
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [modelId]);

  // The preview: what this dataset answers, and the types it answers in. Skipped
  // while there are no columns, because "it has no columns" is a sentence about
  // a dataset that has not been built yet rather than one that is wrong.
  useEffect(() => {
    if (!ready) return undefined;
    const parsed = JSON.parse(datasetJson) as typeof dataset;
    if (parsed.table === "" || parsed.columns.length === 0) {
      setPreview(null);
      setPreviewError(null);
      return undefined;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void api
        .previewDataset({ dataset: parsed, limit: 10 })
        .then((answer) => {
          if (cancelled) return;
          setPreview(answer);
          setPreviewError(null);
        })
        .catch((err: unknown) => {
          if (cancelled) return;
          setPreview(null);
          setPreviewError(errorMessage(err, "This dataset could not be read."));
        });
    }, DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [datasetJson, ready]);

  // The providers, resolved against this dataset and this configuration where
  // they can be: `config_spec` then offers *these* columns and `outcome` says
  // what a fit would produce. A dataset that cannot be read falls back to the
  // unresolved declaration rather than an empty picker — the form still works,
  // and the preview beside it is where the reason is.
  useEffect(() => {
    if (!ready) return undefined;
    let cancelled = false;
    const parsed = JSON.parse(datasetJson) as typeof dataset;
    const usable = parsed.table !== "" && parsed.columns.length > 0;
    const timer = window.setTimeout(() => {
      const query = usable ? { dataset: datasetJson, configuration: configJson } : undefined;
      void api
        .listModelProviders(query)
        .then((listed) => {
          if (cancelled) return;
          setProviders(listed.providers);
          setResolved(Boolean(usable));
        })
        .catch(() => {
          if (!usable) return;
          void api
            .listModelProviders()
            .then((listed) => {
              if (cancelled) return;
              setProviders(listed.providers);
              setResolved(false);
            })
            .catch(() => undefined);
        });
    }, DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [datasetJson, configJson, ready]);

  const loadInstances = useCallback(async (model: string) => {
    try {
      setInstances(orderInstances(await api.listModelInstances(model)));
    } catch {
      // A model that has just been created has no instances and no endpoint
      // trouble worth a banner; the list simply stays empty.
    }
  }, []);

  useEffect(() => {
    if (id) void loadInstances(id);
  }, [id, loadInstances]);

  // The poll (§8). A fit is a spawned task and the row is the registry, so the
  // screen asks again until nothing says `fitting` — and stops, rather than
  // holding a timer open on a screen where nothing is happening.
  const fitting = instances.some((instance) => instance.status === "fitting");
  useEffect(() => {
    if (!id || !fitting) return undefined;
    const timer = window.setTimeout(() => void loadInstances(id), POLL_MS);
    return () => window.clearTimeout(timer);
  }, [id, fitting, instances, loadInstances]);

  // --- what the form knows ---------------------------------------------------

  const chosen = providers.find((p) => p.name === provider);
  const outcome = readOutcome(chosen?.outcome);
  const hyperSpace = buildHyperparameters(chosen?.hyperparameters ?? [], hyper);
  const points = gridPoints(hyperSpace);
  const validationRows = Number(split.validation) > 0;
  const choices = useMemo(() => formulaChoices(schema, table), [schema, table]);
  const grouped = useMemo(() => {
    const groups = new Map<string, { label: string; expr: string; index: number }[]>();
    choices.forEach((choice, index) => {
      const list = groups.get(choice.group) ?? [];
      list.push({ label: choice.label, expr: choice.expr, index });
      groups.set(choice.group, list);
    });
    return [...groups.entries()];
  }, [choices]);

  const addColumn = (index: number) => {
    const choice = choices[index];
    if (!choice) return;
    setColumns((list) => [
      ...list,
      {
        name: uniqueColumnName(
          choice.name,
          list.map((c) => c.name),
        ),
        expr: choice.expr,
      },
    ]);
  };

  const editColumn = (index: number, over: Partial<DatasetColumn>) =>
    setColumns((list) => list.map((c, i) => (i === index ? { ...c, ...over } : c)));

  // --- saving and fitting ----------------------------------------------------

  /** Everything on this form as `saveModel` takes it. */
  const body = () => ({
    id,
    name: name.trim(),
    description: description.trim(),
    provider,
    dataset,
    configuration: buildConfig(specOf(providers, provider), config),
    hyperparameters: hyperSpace,
    split: {
      train: Number(split.train),
      validation: Number(split.validation),
      test: Number(split.test),
      seed: Math.trunc(Number(split.seed)) || 0,
    },
    attributes: {},
  });

  /** Save, and answer the model's id — the same path the Fit button takes,
   * because fitting what is on the screen means saving it first. */
  const save = async (): Promise<string> => {
    const saved = await api.saveModel(body());
    setId(saved.id);
    setError(saved.error ?? null);
    // A new model now has a URL of its own, so a reload comes back to it rather
    // than to an empty form.
    if (!modelId) window.location.hash = `/models/${encodeURIComponent(saved.id)}`;
    return saved.id;
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await save();
    } catch (err) {
      // The server's own refusal names the dataset column, the setting or the
      // hyperparameter that is wrong, which is the message to show.
      setError(errorMessage(err, "Could not save the model."));
    } finally {
      setBusy(false);
    }
  };

  const fit = async () => {
    setBusy(true);
    setError(null);
    try {
      const saved = await save();
      await api.fitModel(saved, { name: null, description: null });
      await loadInstances(saved);
    } catch (err) {
      setError(errorMessage(err, "Could not start the fit."));
    } finally {
      setBusy(false);
    }
  };

  const activate = async (instance: InstanceItem) => {
    try {
      await api.activateModelInstance(instance.id);
      if (id) await loadInstances(id);
    } catch (err) {
      setError(errorMessage(err, "Could not activate that fit."));
    }
  };

  const removeInstance = async (instance: InstanceItem) => {
    if (
      !window.confirm(
        t('Remove the fit "{name}"?', { name: instanceLabel(instance) }),
      )
    ) {
      return;
    }
    try {
      await api.deleteModelInstance(instance.id);
      if (id) await loadInstances(id);
    } catch (err) {
      setError(errorMessage(err, "Could not remove that fit."));
    }
  };

  if (loadError) {
    return (
      <PageBody>
        <Alert variant="danger">{loadError}</Alert>
      </PageBody>
    );
  }
  if (!ready) {
    return (
      <PageBody>
        <div className="py-5 text-center">
          <Spinner animation="border" role="status" />
        </div>
      </PageBody>
    );
  }

  return (
    <>
      <PageHeader
        pretitle="Models"
        title={modelId ? name || "Model" : "New model"}
        actions={
          <Button variant="outline-secondary" onClick={() => navigate("/models")}>
            <IconArrowLeft className="icon-2" />
            <T text="Back" />
          </Button>
        }
      />
      <PageBody>
        {error && <Alert variant="danger">{error}</Alert>}

        <Form onSubmit={(e) => void submit(e)}>
          <Row>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="modelName">
                <Form.Label>
                  <T text="Name" /><span className="text-danger"> *</span>
                </Form.Label>
                <Form.Control value={name} required onChange={(e) => setName(e.target.value)} />
                <Form.Text muted>
                  <T text="What a" /> <code>predict_row</code> <T text="action names this model by." />
                </Form.Text>
              </Form.Group>
            </Col>
            <Col md={6}>
              <Form.Group className="mb-3" controlId="modelDescription">
                <Form.Label><T text="Description" /></Form.Label>
                <Form.Control
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                />
              </Form.Group>
            </Col>
          </Row>

          {/* --- the dataset ------------------------------------------------ */}
          <Card className="mb-3">
            <Card.Header><T text="Dataset" /></Card.Header>
            <Card.Body>
              <Row>
                <Col md={4}>
                  <Form.Group className="mb-3" controlId="modelTable">
                    <Form.Label>
                      <T text="Table" /><span className="text-danger"> *</span>
                    </Form.Label>
                    <Form.Select value={table} onChange={(e) => setTable(e.target.value)}>
                      {tables.every((t) => t !== table) && table !== "" && (
                        <option value={table}>
                          {t("{name} (missing)", { name: table })}
                        </option>
                      )}
                      {tables.map((t) => (
                        <option key={t} value={t}>
                          {t}
                        </option>
                      ))}
                    </Form.Select>
                    <Form.Text muted>
                      <T text="The table every formula below is written over. Changing it leaves the columns as they are — they are formulas, and most of them will not resolve over another table." />
                    </Form.Text>
                  </Form.Group>
                </Col>
                <Col md={8}>
                  <Form.Group className="mb-3" controlId="modelFilter">
                    <Form.Label><T text="Filter" /></Form.Label>
                    <Form.Control
                      className="font-monospace"
                      value={filter}
                      placeholder={t("sold")}
                      onChange={(e) => setFilter(e.target.value)}
                    />
                    <Form.Text muted>
                      <T text="One boolean formula deciding which rows are in the data, or blank for all of them." /> <code>user</code> <T text="and the operation flags may not be used: a dataset has no caller." />
                    </Form.Text>
                  </Form.Group>
                </Col>
              </Row>

              <Table size="sm" className="mb-2">
                <thead>
                  <tr>
                    <th style={{ width: "30%" }}><T text="Column" /></th>
                    <th><T text="Formula" /></th>
                    <th style={{ width: "1%" }} />
                  </tr>
                </thead>
                <tbody>
                  {columns.length === 0 && (
                    <tr>
                      <td colSpan={3} className="text-muted">
                        <T text="No columns yet. Pick one below, or add a blank row and write a formula." />
                      </td>
                    </tr>
                  )}
                  {columns.map((column, index) => (
                    <tr key={index}>
                      <td>
                        <Form.Control
                          size="sm"
                          value={column.name}
                          aria-label={`Column ${index + 1} name`}
                          onChange={(e) => editColumn(index, { name: e.target.value })}
                        />
                      </td>
                      <td>
                        <Form.Control
                          size="sm"
                          className="font-monospace"
                          value={column.expr}
                          aria-label={`Column ${index + 1} formula`}
                          onChange={(e) => {
                            const expr = e.target.value;
                            // A name the picker suggested follows the formula
                            // while it is still the suggestion; one the admin
                            // typed is theirs and is left alone.
                            const suggested = suggestColumnName(column.expr);
                            editColumn(index, {
                              expr,
                              ...(column.name === suggested || column.name === ""
                                ? { name: suggestColumnName(expr) }
                                : {}),
                            });
                          }}
                        />
                      </td>
                      <td>
                        <Button
                          size="sm"
                          variant="outline-danger"
                          aria-label={`Remove column ${index + 1}`}
                          onClick={() => setColumns((list) => list.filter((_, i) => i !== index))}
                        >
                          <IconTrash className="icon-2" />
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </Table>

              <div className="d-flex gap-2 align-items-start flex-wrap">
                {/* The picker writes a formula and nothing else (§2): three
                    groups, one language, and every choice is editable in the
                    row it lands in. */}
                <Form.Select
                  className="w-auto"
                  value=""
                  aria-label={t("Add a column")}
                  onChange={(e) => addColumn(Number(e.target.value))}
                >
                  <option value=""><T text="Add a field, join path or aggregation…" /></option>
                  {grouped.map(([group, list]) => (
                    <optgroup key={group} label={group}>
                      {list.map((choice) => (
                        <option key={choice.expr} value={String(choice.index)}>
                          {choice.label}
                        </option>
                      ))}
                    </optgroup>
                  ))}
                </Form.Select>
                <Button
                  variant="outline-secondary"
                  onClick={() => setColumns((list) => [...list, { name: "", expr: "" }])}
                >
                  <IconPlus className="icon-2" />
                  <T text="Blank column" />
                </Button>
              </div>

              <hr />

              <h4 className="h5"><T text="Preview" /></h4>
              {previewError && <Alert variant="warning">{previewError}</Alert>}
              {!previewError && !preview && (
                <p className="text-muted mb-0">
                  <T text="Add a column to see the first rows and the types they came back as." />
                </p>
              )}
              {preview && (
                <>
                  {preview.split_error && (
                    <Alert variant="warning">
                      {t(
                        "{problem} — the dataset reads, and a fit cannot divide it.",
                        { problem: preview.split_error },
                      )}
                    </Alert>
                  )}
                  <div className="table-responsive">
                    <Table size="sm" className="table-vcenter">
                      <thead>
                        <tr>
                          {preview.columns.map((column) => (
                            <th key={column.name}>
                              {column.name}
                              <div className="text-muted fw-normal small">{column.type}</div>
                            </th>
                          ))}
                        </tr>
                      </thead>
                      <tbody>
                        {preview.rows.map((row, index) => (
                          <tr key={index}>
                            {preview.columns.map((column) => (
                              <td key={column.name} className="text-nowrap">
                                {cellText((row as Record<string, unknown>)[column.name])}
                              </td>
                            ))}
                          </tr>
                        ))}
                      </tbody>
                    </Table>
                  </div>
                  <p className="text-muted small mb-0">
                    <T text="The first rows, and the type each column’s values came back as — which is what the provider’s form below is built from." />
                    {preview.primary_key &&
                      ` ${t("Split by {column}.", { column: preview.primary_key })}`}
                  </p>
                </>
              )}
            </Card.Body>
          </Card>

          {/* --- the provider ----------------------------------------------- */}
          <Card className="mb-3">
            <Card.Header><T text="Provider" /></Card.Header>
            <Card.Body>
              <Row>
                <Col md={5}>
                  <Form.Group className="mb-3" controlId="modelProvider">
                    <Form.Label>
                      <T text="Model provider" /><span className="text-danger"> *</span>
                    </Form.Label>
                    <Form.Select value={provider} onChange={(e) => setProvider(e.target.value)}>
                      <option value="">—</option>
                      {/* A provider whose module was uninstalled under a saved
                          model is still shown, or saving this form would
                          silently repoint the model at another one. */}
                      {provider !== "" && providers.every((p) => p.name !== provider) && (
                        <option value={provider}>
                          {t("{name} (not on this server)", { name: provider })}
                        </option>
                      )}
                      {providers.map((p) => (
                        <option key={p.name} value={p.name}>
                          {p.name}
                          {p.module ? ` (${p.module})` : ""}
                        </option>
                      ))}
                    </Form.Select>
                    {chosen && <Form.Text muted>{chosen.description}</Form.Text>}
                  </Form.Group>
                </Col>
                <Col md={7}>
                  <Form.Label><T text="Outcome" /></Form.Label>
                  <div className="mb-3">
                    {outcome ? (
                      <StatusBadge tone="blue">{outcomeSummary(outcome)}</StatusBadge>
                    ) : (
                      <span className="text-muted">
                        {chosen?.outcome_error ??
                          (resolved
                            ? "Pick a provider."
                            : "Build a dataset that reads, and this says what a fit would produce.")}
                      </span>
                    )}
                  </div>
                  {chosen?.standardise && (
                    <Form.Text muted className="d-block">
                      <T text="This provider is handed standardised features, so its parameters are in standard deviations rather than the data's own units." />
                    </Form.Text>
                  )}
                </Col>
              </Row>

              {chosen && chosen.config_spec.length > 0 && (
                <>
                  <hr />
                  <h4 className="h5"><T text="Settings" /></h4>
                  {!resolved && (
                    <p className="text-muted small">
                      <T text="The dataset could not be read, so a setting that would offer this dataset's columns is a text box here." />
                    </p>
                  )}
                  <SettingsFields
                    spec={chosen.config_spec}
                    values={config}
                    onChange={(key, value) => setConfig((c) => ({ ...c, [key]: value }))}
                    idPrefix="model-config"
                  />
                </>
              )}

              {chosen && chosen.hyperparameters.length > 0 && (
                <>
                  <hr />
                  <h4 className="h5"><T text="Hyperparameters" /></h4>
                  <p className="text-muted small">
                    <T text="A value, or several separated by commas — a fit runs every combination of the lists, scores each on the validation rows and reports the winner. A blank box leaves the provider's own default." />
                  </p>
                  <Row>
                    {chosen.hyperparameters.map((field) => (
                      <Col md={4} key={field.name}>
                        <Form.Group className="mb-3" controlId={`model-hyper-${field.name}`}>
                          <Form.Label>{field.label}</Form.Label>
                          <Form.Control
                            className="font-monospace"
                            value={hyper[field.name] ?? ""}
                            placeholder={printGridValue(field.default) || "the default"}
                            onChange={(e) =>
                              setHyper((h) => ({ ...h, [field.name]: e.target.value }))
                            }
                          />
                        </Form.Group>
                      </Col>
                    ))}
                  </Row>
                  {points > 1 && (
                    <p className="text-muted small mb-0">
                      {points > MAX_GRID_POINTS
                        ? t(
                            "{count} combinations, and each one is a fit — more than the {cap} a fit will run.",
                            { count: points, cap: MAX_GRID_POINTS },
                          )
                        : t("{count} combinations, and each one is a fit.", {
                            count: points,
                          })}{" "}
                      {!validationRows &&
                        t(
                          "A search scores its points on the validation rows, and this split has none — give it some below.",
                        )}
                    </p>
                  )}
                  {points === 0 && (
                    <p className="text-danger small mb-0">
                      <T text="One of these is an empty list, which is a search over nothing." />
                    </p>
                  )}
                </>
              )}
            </Card.Body>
          </Card>

          {/* --- the split --------------------------------------------------- */}
          <Card className="mb-3">
            <Card.Header><T text="Split" /></Card.Header>
            <Card.Body>
              <Row>
                {(["train", "validation", "test"] as const).map((part) => (
                  <Col md={3} key={part}>
                    <Form.Group className="mb-3" controlId={`model-split-${part}`}>
                      <Form.Label className="text-capitalize">{part}</Form.Label>
                      <Form.Control
                        type="number"
                        step="0.05"
                        min="0"
                        max="1"
                        value={split[part]}
                        onChange={(e) => setSplit((s) => ({ ...s, [part]: e.target.value }))}
                      />
                    </Form.Group>
                  </Col>
                ))}
                <Col md={3}>
                  <Form.Group className="mb-3" controlId="model-split-seed">
                    <Form.Label><T text="Seed" /></Form.Label>
                    <Form.Control
                      type="number"
                      value={split.seed}
                      onChange={(e) => setSplit((s) => ({ ...s, seed: e.target.value }))}
                    />
                  </Form.Group>
                </Col>
              </Row>
              {!splitSums(split) && (
                <p className="text-danger small mb-2">
                  <T text="The three fractions must sum to 1." />
                </p>
              )}
              <p className="text-muted small mb-0">
                <T text="Which side of the split a row falls on is a hash of its primary key and the seed, not a shuffle — so new rows arriving keep every old row where it was, and the test metric of this fit is comparable with the test metric of the last one. The fractions are therefore approximate; each fit records the counts it got." />
              </p>
            </Card.Body>
          </Card>

          <div className="btn-list mb-4">
            <Button type="submit" disabled={busy}>
              {busy ? "Saving…" : id ? "Save changes" : "Create model"}
            </Button>
            <Button variant="success" disabled={busy} onClick={() => void fit()}>
              <T text="Fit" />
            </Button>
            <span className="text-muted small align-self-center">
              <T text="Fitting saves this model first, so what is fitted is what is on the screen." />
            </span>
          </div>
        </Form>

        {/* --- the fits ----------------------------------------------------- */}
        {id && (
          <Card className="mb-3">
            <Card.Header><T text="Fits" /></Card.Header>
            <Table hover responsive className="card-table table-vcenter">
              <thead>
                <tr>
                  <th><T text="Fit" /></th>
                  <th><T text="Status" /></th>
                  <th><T text="Result" /></th>
                  <th><T text="Hyperparameters" /></th>
                  <th className="text-end"><T text="Actions" /></th>
                </tr>
              </thead>
              <tbody>
                {instances.length === 0 && (
                  <tr>
                    <td colSpan={5} className="text-muted">
                      <T text="Not fitted yet. A fit reads every row of the dataset and runs on the server; this list says how it went." />
                    </td>
                  </tr>
                )}
                {instances.map((instance) => (
                  <tr key={instance.id}>
                    <td>
                      <a href={`#/model-instances/${encodeURIComponent(instance.id)}`}>
                        {instanceLabel(instance)}
                      </a>
                      {/* An unnamed fit is already addressed by when it
                          happened, so the time is not printed under itself. */}
                      {instance.name.trim() !== "" && (
                        <div className="text-muted small">
                          {formatTimestamp(instance.created)}
                        </div>
                      )}
                    </td>
                    <td>
                      <div className="d-flex align-items-center gap-2">
                        <StatusBadge tone={fitTone(instance.status)}>
                          {instance.status}
                        </StatusBadge>
                        {instance.active && <StatusBadge tone="green"><T text="active" /></StatusBadge>}
                      </div>
                      {/* A fit that failed says so **here**, because the request
                          that started it returned long before it failed. */}
                      {instance.error && (
                        <div className="text-danger small">{instance.error}</div>
                      )}
                    </td>
                    <td>
                      {headlineMetric(readMetrics(instance.metrics)) ??
                        outcomeSummary(readOutcome(instance.outcome))}
                    </td>
                    <td className="text-muted small font-monospace">
                      {hyperparameterText(instance.hyperparameters)}
                    </td>
                    <td className="text-end">
                      <div className="btn-list justify-content-end flex-nowrap">
                        {instance.status === "fitted" && !instance.active && (
                          <Button
                            size="sm"
                            variant="outline-primary"
                            onClick={() => void activate(instance)}
                          >
                            <T text="Activate" />
                          </Button>
                        )}
                        <Button
                          size="sm"
                          variant="outline-secondary"
                          href={`#/model-instances/${encodeURIComponent(instance.id)}`}
                        >
                          <T text="Open" />
                        </Button>
                        <Button
                          size="sm"
                          variant="outline-danger"
                          onClick={() => void removeInstance(instance)}
                        >
                          <T text="Remove" />
                        </Button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </Table>
            <Card.Footer className="text-muted small">
              <T text="A fit runs on the server and this list polls until it finishes. Nothing survives a restart: an instance still fitting when the server stops is failed at boot, because there is no cancel and no way to pick it back up." />
            </Card.Footer>
          </Card>
        )}
      </PageBody>
    </>
  );
}

/** The chosen provider's settings declaration, or none while nothing is chosen. */
function specOf(providers: ProviderItem[], provider: string) {
  return providers.find((p) => p.name === provider)?.config_spec ?? [];
}

/** Whether the three fractions sum to 1, which the server requires — a split
 * that silently normalised them would hold out a different fraction than the
 * instance says it did. */
function splitSums(split: SplitForm): boolean {
  const sum = Number(split.train) + Number(split.validation) + Number(split.test);
  return Number.isFinite(sum) && Math.abs(sum - 1) < 1e-9;
}

/** A preview cell: a null is an em dash rather than the word "null", and an
 * object is its JSON, because a dataset column can be one. */
function cellText(value: unknown): string {
  if (value === null || value === undefined) return "—";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

/** The hyperparameter point a fit used, on one line. */
function hyperparameterText(raw: unknown): string {
  if (!raw || typeof raw !== "object") return "—";
  const entries = Object.entries(raw as Record<string, unknown>);
  if (entries.length === 0) return "—";
  return entries.map(([key, value]) => `${key}=${printGridValue(value)}`).join(" ");
}
