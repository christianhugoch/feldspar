// The model screens' model: the JSON the model API carries, and the handful of
// pure functions three screens would otherwise each invent (TODO "Predictive
// models", task 6.6).
//
// Four of the endpoints' fields are declared `json` in the endpoint set and
// therefore arrive as `unknown` in the generated client — the dataset, the
// outcome, the metrics, the parameter blocks. That is right for the *wire*: they
// are a provider's vocabulary, not the endpoint's, and giving them a
// `TypeSchema` would mean the API declaring what a coefficient table is. It is
// wrong for a screen, which has to render them. So the shapes are written out
// here once, as the discriminated unions their Rust originals serialise to, and
// the screens read them through the narrowing functions below rather than
// casting at each use.
//
// The rest is arithmetic with an opinion:
//
//   - a **hyperparameter** is a value or a list of values (§11), and the form
//     edits both as one text box — so the parse and the print are here, with the
//     equivalence they hold up to stated in the tests;
//   - a **metric set** is chosen by the outcome, so nothing has to ask whether
//     an accuracy on a regression means anything;
//   - a **p-value** in a coefficient table must not print as `1.2e-16`, because
//     a column of those is unreadable and the only question anybody asks of one
//     is which side of a threshold it falls;
//   - and the **instance order** is the active fit first, then newest, because
//     the active one is what everything outside this screen means by "the model".

import { columnType, type TableInfo } from "./codeTypes";
import type {
  GetModelInstanceResponse,
  GetModelResponse,
  ListModelInstancesResponse,
  ListModelProvidersResponse,
} from "./client";
import type { FieldSpec } from "./settings";

// --- the JSON the API carries -----------------------------------------------

/** One dataset column: the name it is known by, and the formula computing it. */
export type DatasetColumn = { name: string; expr: string };

/** Which rows and which derived values make up a model's data (§2). */
export type Dataset = {
  table: string;
  columns: DatasetColumn[];
  /** One boolean formula restricting the rows, or none. */
  filter?: string | null;
};

/** The fractions a fit divides its rows by, and the seed its hash is salted
 * with (§5). */
export type Split = { train: number; validation: number; test: number; seed: number };

/** What a fit of a given configuration produces (§10) — what the UI switches on. */
export type Outcome =
  | { outcome: "regression"; label: string }
  | { outcome: "classification"; label: string; classes?: string[] }
  | { outcome: "cluster" }
  | { outcome: "embedding"; dimensions: number }
  | { outcome: "test" };

/** One class's precision, recall and F1, and how many rows actually were it. */
export type ClassMetrics = {
  class: string;
  precision: number | null;
  recall: number | null;
  f1: number | null;
  support: number;
};

/** What one split's rows scored (§7). The variant is the outcome's. */
export type Metrics =
  | { metrics: "regression"; r2: number | null; rmse: number | null; mae: number | null; rows: number }
  | {
      metrics: "classification";
      accuracy: number | null;
      classes: ClassMetrics[];
      confusion: number[][];
      rows: number;
    }
  | { metrics: "clustering"; sizes: number[]; wcss: number | null; rows: number }
  | { metrics: "embedding"; explained_variance: (number | null)[]; rows: number }
  | { metrics: "none" };

/** The metrics of each split a fit had rows for. */
export type SplitMetrics = {
  train?: Metrics | null;
  validation?: Metrics | null;
  test?: Metrics | null;
};

/** A fitted parameter, in the shape the screen renders it in (§7). */
export type ParameterBlock =
  | { block: "scalar"; name: string; value: number | null }
  | { block: "table"; name: string; columns: string[]; rows: { cells: unknown[] }[] }
  | { block: "text"; name: string; body: string };

/** Where the rows went: what was selected, what each split got, what the
 * encoding could not represent. */
export type RowCounts = {
  selected: number;
  train: number;
  validation: number;
  test: number;
  dropped: number;
};

/** One point of the hyperparameter grid and what it scored (§11). A point that
 * failed carries its sentence rather than being dropped. */
export type GridPoint = {
  hyperparameters: Record<string, unknown>;
  score?: number | null;
  error?: string | null;
};

/** How one dataset column becomes one or more matrix columns (§6) — everything
 * the fit learned about it, which is what makes applying it a lookup and never a
 * re-derivation. */
export type ColumnEncoding =
  | { encoding: "passthrough"; column: string }
  | { encoding: "standardised"; column: string; mean: number; sd: number }
  | { encoding: "one_hot"; column: string; categories: string[] }
  | { encoding: "epoch"; column: string };

/** The encoding fitted with an instance. */
export type Encoding = { columns: ColumnEncoding[] };

/** One prediction, as `predictRows` answers it. */
export type Prediction =
  | { prediction: "number"; value: number }
  | { prediction: "class"; class: string; probability?: number | null }
  | { prediction: "cluster"; cluster: number }
  | { prediction: "vector"; values: number[] };

/** One model, as the list and the form see it. */
export type ModelItem = GetModelResponse;
/** One fit, in full. */
export type InstanceDetail = GetModelInstanceResponse;
/** One fit, as a list sees it. */
export type InstanceItem = ListModelInstancesResponse[number];
/** One model provider the picker offers. */
export type ProviderItem = ListModelProvidersResponse["providers"][number];

/** The three states a fit is in (`_sc_model_instances.status`). */
export type FitStatus = "fitting" | "fitted" | "failed";

// --- reading the `unknown`s -------------------------------------------------
//
// One narrowing function per blob, each answering `null` for a value that is not
// the shape it should be. `null` rather than a thrown error because these come
// off the wire into a *screen*: an instance whose metrics could not be read
// should still show its parameters and its status, which is more than an empty
// page saying nothing.

/** A dataset off the wire, or an empty one over `table` when there is none. */
export function readDataset(raw: unknown, table = ""): Dataset {
  if (raw && typeof raw === "object") {
    const value = raw as Partial<Dataset>;
    if (typeof value.table === "string" && Array.isArray(value.columns)) {
      return {
        table: value.table,
        columns: value.columns.filter(
          (c): c is DatasetColumn =>
            Boolean(c) && typeof c.name === "string" && typeof c.expr === "string",
        ),
        filter: typeof value.filter === "string" ? value.filter : null,
      };
    }
  }
  return { table, columns: [], filter: null };
}

/** The default split: four fifths fitted, one fifth held out, no validation
 * rows — the shape of a fit with no hyperparameter search (`Split::default`). */
export const DEFAULT_SPLIT: Split = { train: 0.8, validation: 0.0, test: 0.2, seed: 0 };

/** A split off the wire, falling back to the default it was created with. */
export function readSplit(raw: unknown): Split {
  if (raw && typeof raw === "object") {
    const value = raw as Partial<Split>;
    if (
      typeof value.train === "number" &&
      typeof value.validation === "number" &&
      typeof value.test === "number"
    ) {
      return {
        train: value.train,
        validation: value.validation,
        test: value.test,
        seed: typeof value.seed === "number" ? value.seed : 0,
      };
    }
  }
  return { ...DEFAULT_SPLIT };
}

/** An outcome off the wire, or `null` for one that is absent or unrecognised. */
export function readOutcome(raw: unknown): Outcome | null {
  if (!raw || typeof raw !== "object") return null;
  const tag = (raw as { outcome?: unknown }).outcome;
  if (typeof tag !== "string") return null;
  if (!["regression", "classification", "cluster", "embedding", "test"].includes(tag)) return null;
  return raw as Outcome;
}

/** The metrics of each split, from an instance's `metrics` column. */
export function readMetrics(raw: unknown): SplitMetrics {
  if (!raw || typeof raw !== "object") return {};
  return raw as SplitMetrics;
}

/** The parameter blocks of an instance, dropping anything that is not one of
 * the three renderings. */
export function readParameters(raw: unknown[]): ParameterBlock[] {
  return raw.filter((block): block is ParameterBlock => {
    if (!block || typeof block !== "object") return false;
    const tag = (block as { block?: unknown }).block;
    return tag === "scalar" || tag === "table" || tag === "text";
  });
}

/** The grid points an instance recorded, in the order they were tried. */
export function readSearch(raw: unknown[]): GridPoint[] {
  return raw.filter((point): point is GridPoint => {
    if (!point || typeof point !== "object") return false;
    const hyper = (point as { hyperparameters?: unknown }).hyperparameters;
    return Boolean(hyper) && typeof hyper === "object";
  });
}

/** The row counts of a fit, or `null` for an instance that has not got that far. */
export function readRowCounts(raw: unknown): RowCounts | null {
  if (!raw || typeof raw !== "object") return null;
  const value = raw as Partial<RowCounts>;
  return typeof value.selected === "number"
    ? {
        selected: value.selected,
        train: value.train ?? 0,
        validation: value.validation ?? 0,
        test: value.test ?? 0,
        dropped: value.dropped ?? 0,
      }
    : null;
}

/** The encoding an instance was fitted with, or `null` for a fit that has none
 * (a hypothesis test, or one that has not finished). */
export function readEncoding(raw: unknown): Encoding | null {
  if (!raw || typeof raw !== "object") return null;
  const columns = (raw as { columns?: unknown }).columns;
  if (!Array.isArray(columns)) return null;
  return {
    columns: columns.filter((column): column is ColumnEncoding => {
      if (!column || typeof column !== "object") return false;
      const tag = (column as { encoding?: unknown }).encoding;
      return (
        tag === "passthrough" || tag === "standardised" || tag === "one_hot" || tag === "epoch"
      );
    }),
  };
}

/** A prediction off the wire, or `null` for one that is not a shape we render. */
export function readPrediction(raw: unknown): Prediction | null {
  if (!raw || typeof raw !== "object") return null;
  const tag = (raw as { prediction?: unknown }).prediction;
  if (tag === "number" || tag === "class" || tag === "cluster" || tag === "vector") {
    return raw as Prediction;
  }
  return null;
}

// --- the hyperparameter grid ------------------------------------------------

/** The most grid points a fit will run (`sc_model::MAX_GRID_POINTS`). Duplicated
 * here to warn *on the form*; the server is what refuses. */
export const MAX_GRID_POINTS = 200;

/**
 * One hyperparameter box read as what it means: a value, a list of values, or
 * nothing at all.
 *
 * The box is one control for both because §11 says they are one thing — "a list
 * of one and a scalar are the same search" — and asking the admin to tick "this
 * is a search" before typing a second number would be a second way to say it.
 * Commas separate; a blank box is `undefined` and is *not sent*, so the
 * provider's own default applies rather than a zero this form invented.
 *
 * A value that is not of the declared type is passed through as the text that
 * was typed, for the reason `buildConfig` does the same: the server validates
 * against the same declaration and its message names the hyperparameter, which
 * is a better error than anything this could invent.
 */
export function parseGridValue(text: string, type: string): unknown {
  const parts = text
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part !== "");
  if (parts.length === 0) return undefined;
  const values = parts.map((part) => coerceValue(part, type));
  // A trailing comma is how a one-element *list* is written, and it is worth
  // keeping: `[8]` and `8` fit identically, but the box the admin comes back to
  // should say what they typed.
  return values.length === 1 && !text.includes(",") ? values[0] : values;
}

/** One typed hyperparameter value from the text of it. */
function coerceValue(text: string, type: string): unknown {
  if (type === "bool") return text === "true" || text === "yes" || text === "1";
  if (type === "int" || type === "float") {
    const number = Number(text);
    return Number.isFinite(number) ? number : text;
  }
  return text;
}

/**
 * A stored hyperparameter back as the text of the box it is edited in.
 *
 * The inverse of [`parseGridValue`] **up to §11's equivalence**: a one-element
 * list prints as the bare value, because that is the same search and the shorter
 * thing to read.
 */
export function printGridValue(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (Array.isArray(value)) return value.map((one) => printGridValue(one)).join(", ");
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

/** Every hyperparameter box read into the object `saveModel` takes. */
export function buildHyperparameters(
  spec: FieldSpec[],
  values: Record<string, string>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const field of spec) {
    const parsed = parseGridValue(values[field.name] ?? "", field.type);
    if (parsed !== undefined) out[field.name] = parsed;
  }
  return out;
}

/** A stored hyperparameter object back into the boxes that edit it. */
export function readHyperparameters(raw: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (raw && typeof raw === "object") {
    for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
      out[key] = printGridValue(value);
    }
  }
  return out;
}

/**
 * How many fits this hyperparameter space comes to: the product of the lists,
 * with the scalars held fixed.
 *
 * `1` is "no search", which is the common case and the one that must not pay for
 * the uncommon one. `0` means some list is empty — a search over nothing, which
 * the server refuses, and which the form should say so about while it is still
 * being typed.
 */
export function gridPoints(hyperparameters: Record<string, unknown>): number {
  let points = 1;
  for (const value of Object.values(hyperparameters)) {
    if (Array.isArray(value)) points *= value.length;
  }
  return points;
}

// --- the outcome, and the metrics it chooses --------------------------------

/** An outcome as a sentence: what a fit of this model answers, per row. */
export function outcomeSummary(outcome: Outcome | null): string {
  if (!outcome) return "—";
  switch (outcome.outcome) {
    case "regression":
      return `Regression on ${outcome.label}`;
    case "classification":
      return `Classification of ${outcome.label}`;
    case "cluster":
      return "Clustering";
    case "embedding":
      return `Embedding (${outcome.dimensions} components)`;
    case "test":
      return "Hypothesis test";
  }
}

/** One row of the metrics table: what it is called, and what it came to. */
export type MetricRow = { label: string; value: string };

/**
 * One split's metrics as labelled numbers, chosen by the metric set's own
 * variant.
 *
 * This is the outcome-to-metric-set mapping the screen renders, and it is a
 * mapping and not a merge: a regression has no accuracy and a clustering has no
 * R², so there is no row for one. A hypothesis test scores nothing at all — its
 * parameters are the answer — and answers the empty list.
 */
export function metricRows(metrics: Metrics | null | undefined): MetricRow[] {
  if (!metrics) return [];
  switch (metrics.metrics) {
    case "regression":
      return [
        { label: "R²", value: formatNumber(metrics.r2) },
        { label: "RMSE", value: formatNumber(metrics.rmse) },
        { label: "MAE", value: formatNumber(metrics.mae) },
        { label: "Rows", value: String(metrics.rows) },
      ];
    case "classification":
      return [
        { label: "Accuracy", value: formatNumber(metrics.accuracy) },
        { label: "Classes", value: String(metrics.classes.length) },
        { label: "Rows", value: String(metrics.rows) },
      ];
    case "clustering":
      return [
        { label: "Within-cluster sum of squares", value: formatNumber(metrics.wcss) },
        { label: "Clusters", value: String(metrics.sizes.length) },
        { label: "Cluster sizes", value: metrics.sizes.join(", ") },
        { label: "Rows", value: String(metrics.rows) },
      ];
    case "embedding":
      return [
        {
          label: "Explained variance",
          value: metrics.explained_variance.map((v) => formatNumber(v)).join(", "),
        },
        {
          label: "Total explained",
          value: formatNumber(
            metrics.explained_variance.reduce((sum: number, v) => sum + (v ?? 0), 0),
          ),
        },
        { label: "Rows", value: String(metrics.rows) },
      ];
    case "none":
      return [];
  }
}

/** The one number a fit is judged by, for the list: "R² 0.94", "accuracy 0.81".
 * The **test** split's, because a metric measured on the rows the fit was
 * computed from is not a claim about anything. */
export function headlineMetric(metrics: SplitMetrics): string | null {
  const set = metrics.test ?? metrics.validation ?? metrics.train;
  if (!set) return null;
  switch (set.metrics) {
    case "regression":
      return `R² ${formatNumber(set.r2)}`;
    case "classification":
      return `accuracy ${formatNumber(set.accuracy)}`;
    case "clustering":
      return `WCSS ${formatNumber(set.wcss)}`;
    case "embedding":
      return `explained ${formatNumber(
        set.explained_variance.reduce((sum: number, v) => sum + (v ?? 0), 0),
      )}`;
    case "none":
      return null;
  }
}

// --- numbers on a screen ----------------------------------------------------

/**
 * A number as a table cell: enough digits to be worth reading and not so many
 * that a column of them cannot be scanned.
 *
 * A null is an em dash rather than a `0`: the metrics serialise a NaN as null
 * (an R² over one row is not zero, it is undefined), and printing it as a number
 * would be a wrong answer rather than a missing one.
 */
export function formatNumber(value: unknown, digits = 4): string {
  if (value === null || value === undefined) return "—";
  if (typeof value === "boolean") return value ? "yes" : "no";
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return typeof value === "string" ? value : "—";
  }
  if (Number.isInteger(value) && Math.abs(value) < 1e15) return String(value);
  const magnitude = Math.abs(value);
  if (magnitude >= 1e7 || magnitude < 1e-4) return value.toExponential(2);
  return String(Number(value.toPrecision(digits)));
}

/**
 * A p-value, which is the one number in a coefficient table nobody wants in
 * full.
 *
 * `1.2e-16` is what a regression on well-separated data actually produces, and
 * a column of those is unreadable — while the question asked of a p-value is
 * almost always which side of a threshold it falls, which `< 0.001` answers
 * better than any number of digits. Above the floor it prints to three decimals,
 * so `0.049` and `0.051` are still distinguishable, which is the one place the
 * exact value earns its space.
 */
export function formatPValue(value: unknown): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return formatNumber(value);
  if (value < 0.001) return "< 0.001";
  return value.toFixed(3);
}

/** The conventional significance marks, for the column beside a p-value. */
export function significanceStars(value: unknown): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "";
  if (value < 0.001) return "***";
  if (value < 0.01) return "**";
  if (value < 0.05) return "*";
  if (value < 0.1) return ".";
  return "";
}

/** Whether a parameter table's column holds p-values, by the names providers
 * give it (`p` here, `p-value` on a scalar, and the two spellings a provider
 * from a module is likely to use). */
export function isPValueColumn(column: string): boolean {
  const name = column.trim().toLowerCase();
  return (
    name === "p" ||
    name === "p-value" ||
    name === "p value" ||
    name === "p_value" ||
    name === "pr(>|t|)"
  );
}

/** One cell of a parameter table, formatted by what its column holds. */
export function formatParameterCell(column: string, value: unknown): string {
  if (isPValueColumn(column)) return formatPValue(value);
  if (typeof value === "string") return value;
  if (value === null || value === undefined) return "—";
  if (typeof value === "object") return JSON.stringify(value);
  return formatNumber(value);
}

/** A prediction as one line: the number, the class and its probability, the
 * cluster, or the vector. */
export function predictionSummary(prediction: Prediction | null): string {
  if (!prediction) return "—";
  switch (prediction.prediction) {
    case "number":
      return formatNumber(prediction.value);
    case "class":
      return prediction.probability == null
        ? prediction.class
        : `${prediction.class} (p ${formatNumber(prediction.probability)})`;
    case "cluster":
      return `cluster ${prediction.cluster}`;
    case "vector":
      return `[${prediction.values.map((v) => formatNumber(v)).join(", ")}]`;
  }
}

// --- the instance list ------------------------------------------------------

/**
 * The instances of a model, in the order the screen shows them: the **active**
 * fit first, then newest first.
 *
 * Active first because it is what everything outside this screen means by "the
 * model" — a trigger names the model and gets that fit — so it is the row the
 * admin came to check. Newest next because the list is a history and a refit is
 * the reason to look at one. The sort is stable in the created time, so two fits
 * started in the same millisecond keep the order the server sent them in rather
 * than swapping about between polls.
 */
export function orderInstances<T extends { active: boolean; created: string }>(
  instances: T[],
): T[] {
  return instances
    .map((instance, index) => ({ instance, index }))
    .sort((a, b) => {
      if (a.instance.active !== b.instance.active) return a.instance.active ? -1 : 1;
      if (a.instance.created !== b.instance.created) {
        return a.instance.created < b.instance.created ? 1 : -1;
      }
      return a.index - b.index;
    })
    .map((entry) => entry.instance);
}

/** What an instance is called on the screen: its name, else when it was fitted —
 * because a fit that was not named is still addressed by *when* it happened. */
export function instanceLabel(instance: { name: string; created: string }): string {
  return instance.name.trim() !== "" ? instance.name : formatTimestamp(instance.created);
}

/** A timestamp as the admin's own locale writes it, or the raw string when it is
 * not a time at all. */
export function formatTimestamp(value: string): string {
  const when = new Date(value);
  return Number.isNaN(when.getTime()) ? value : when.toLocaleString();
}

// --- the dataset builder's picker -------------------------------------------

/** One thing the picker can add to a dataset: what it is called, the formula it
 * writes, and which group it belongs to. */
export type FormulaChoice = {
  /** The group heading — "Fields", "Join fields", "Aggregations". */
  group: string;
  /** What the option reads as in the picker. */
  label: string;
  /** The formula it writes into the column (§2: what a picker writes is a
   * formula, and a user who wants `log(price)` types it). */
  expr: string;
  /** The column name it suggests, which the admin may then change. */
  name: string;
};

/**
 * Everything the picker offers over one table: its own fields, one join path per
 * key field per column of the table it points at, and one aggregation per
 * incoming key.
 *
 * Three groups and one output, because there is no second vocabulary (§2): each
 * choice writes a **formula** into an ordinary column row, and the admin can
 * edit it afterwards into something no picker would have offered.
 */
export function formulaChoices(tables: TableInfo[], table: string): FormulaChoice[] {
  const byName = new Map(tables.map((t) => [t.name, t]));
  const here = byName.get(table);
  if (!here) return [];
  const choices: FormulaChoice[] = [];

  for (const column of here.columns) {
    choices.push({ group: "Fields", label: column.name, expr: column.name, name: column.name });
  }

  for (const key of here.columns) {
    const target = key.keyTo ? byName.get(key.keyTo) : undefined;
    if (!target) continue;
    for (const far of target.columns) {
      // The key's own target field adds nothing: `authorⱵid` is `author`.
      if (far.name === "id") continue;
      choices.push({
        group: "Join fields",
        label: `${key.name}Ⱶ${far.name}  (${target.name}.${far.name})`,
        expr: `${key.name}Ⱶ${far.name}`,
        name: `${key.name}_${far.name}`,
      });
    }
  }

  for (const child of tables) {
    for (const key of child.columns) {
      if (key.keyTo !== table) continue;
      const relation = `${child.name}Ↄ${key.name}`;
      choices.push({
        group: "Aggregations",
        label: `${relation}.length  (how many ${child.name})`,
        expr: `${relation}.length`,
        name: `${child.name}_count`,
      });
      for (const value of child.columns) {
        if (value.keyTo || value.name === "id" || !isNumeric(value)) continue;
        for (const aggregate of ["sum", "avg", "max"] as const) {
          choices.push({
            group: "Aggregations",
            label: `${relation}.${aggregate}("${value.name}")`,
            expr: `${relation}.${aggregate}("${value.name}")`,
            name: `${child.name}_${value.name}_${aggregate}`,
          });
        }
      }
    }
  }

  return choices;
}

/** Whether a column holds a number, which is what an aggregation other than a
 * count can be taken over. Decided by the same mapping the code editor's types
 * use, so "what is a number here" is answered in one place. */
function isNumeric(column: TableInfo["columns"][number]): boolean {
  return columnType(column).startsWith("number");
}

/**
 * A column name for a formula somebody typed: the formula itself when it is
 * already a plain name, and a readable flattening of it when it is not.
 *
 * A dataset column's name is what the provider's label picker offers and what
 * the encoding is keyed by, so it has to be a name — `neighbourhoodⱵaverage_income`
 * is a fine formula and a poor heading. Empty means "no suggestion", which is a
 * real answer for a formula that is all punctuation.
 */
export function suggestColumnName(expr: string): string {
  const trimmed = expr.trim();
  if (trimmed === "") return "";
  if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(trimmed)) return trimmed;
  return trimmed
    .replace(/\.length\b/g, "_count")
    .replace(/[^A-Za-z0-9_]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_|_$/g, "")
    .toLowerCase();
}

// --- asking a fit about a row -----------------------------------------------

/** One box of the "try a row" form: which feature, how it is typed, and — for a
 * category — the values this fit actually saw. */
export type FeatureInput = {
  name: string;
  /** What the value has to be for the fit to accept it. */
  kind: "number" | "category" | "date";
  /** The categories the fit was shown, for a one-hot column. A value not in this
   * list is refused **by name** at predict time rather than encoded as a row of
   * zeros, so offering the list is the difference between a form that works and
   * one that produces a confident refusal. */
  categories?: string[];
  /** The dataset formula behind it, as a hint under the box. */
  expr?: string;
};

/**
 * The boxes to ask for, from the encoding this instance was fitted with.
 *
 * The **encoding** and not the dataset, because the encoding is what a
 * prediction is applied through: it names exactly the feature columns, in the
 * order they were fitted, and it never includes the label — which is the thing
 * being predicted and is usually absent from the row being asked about.
 */
export function featureInputs(
  encoding: Encoding | null,
  dataset: Dataset | null,
): FeatureInput[] {
  if (!encoding) return [];
  const formulas = new Map((dataset?.columns ?? []).map((c) => [c.name, c.expr]));
  return encoding.columns.map((column) => ({
    name: column.column,
    kind:
      column.encoding === "one_hot"
        ? ("category" as const)
        : column.encoding === "epoch"
          ? ("date" as const)
          : ("number" as const),
    categories: column.encoding === "one_hot" ? column.categories : undefined,
    expr: formulas.get(column.column),
  }));
}

/**
 * What a typed box sends: the value **as the fit's frame is typed**, not as
 * text.
 *
 * A numeric feature wants a JSON number and refuses `"100"` by name, which is
 * right — the row being predicted is encoded the way the fit was or it fails —
 * and it means the coercion belongs here, where the box is. `true`/`false` in a
 * numeric column become 1 and 0, which is what a boolean feature was
 * passed through as at fit time.
 *
 * A value that will not coerce is sent **as it was typed**, so the server's
 * refusal names the column and the value rather than this form inventing a
 * number nobody entered.
 */
export function typedFeatureValue(text: string, kind: FeatureInput["kind"]): unknown {
  const trimmed = text.trim();
  if (kind === "category") return text;
  if (kind === "number") {
    if (trimmed === "true") return 1;
    if (trimmed === "false") return 0;
    const number = Number(trimmed);
    return Number.isFinite(number) ? number : text;
  }
  // A date crosses as epoch seconds or as a string the server parses.
  const epoch = Number(trimmed);
  return Number.isFinite(epoch) && trimmed !== "" ? epoch : text;
}

/** A name not already taken by another column, by adding `_2`, `_3`, … — so
 * clicking the same aggregation twice does not produce two columns the server
 * refuses as duplicates. */
export function uniqueColumnName(name: string, taken: string[]): string {
  if (name === "") return "";
  if (!taken.includes(name)) return name;
  for (let n = 2; ; n += 1) {
    const candidate = `${name}_${n}`;
    if (!taken.includes(candidate)) return candidate;
  }
}
