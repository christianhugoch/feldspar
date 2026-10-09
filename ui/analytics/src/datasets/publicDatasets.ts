// The public datasets "Get datasets" offers (`sc_api::public_datasets`): what
// the picker says about each, and about an install while it runs. Pure, so the
// words are tested without a server; `PublicDatasets.tsx` is the picker.

import type { GetPublicDatasetInstallResponse, ListPublicDatasetsResponse } from "../client";

export type PublicDataset = ListPublicDatasetsResponse[number];
export type InstallJob = GetPublicDatasetInstallResponse;

/** The picker's groups, in its order. */
export const CATEGORIES = ["tabular", "hierarchical", "time_series", "spatial"] as const;
export type Category = (typeof CATEGORIES)[number];

type Translate = (text: string, args?: Record<string, string | number>) => string;

/** A category's name. */
export function categoryName(category: string, t: Translate): string {
  switch (category) {
    case "tabular":
      return t("Tabular");
    case "hierarchical":
      return t("Hierarchical");
    case "time_series":
      return t("Time series");
    case "spatial":
      return t("Spatial");
    default:
      return category;
  }
}

/** A category's badge colour (Tabler's light variants). */
export function categoryColour(category: string): string {
  switch (category) {
    case "tabular":
      return "blue-lt";
    case "hierarchical":
      return "purple-lt";
    case "time_series":
      return "orange-lt";
    case "spatial":
      return "green-lt";
    default:
      return "secondary-lt";
  }
}

/** A download's size in kB or MB, as the picker shows it. */
export function formatBytes(bytes: number, locale?: string): string {
  const number = (n: number, digits: number) =>
    n.toLocaleString(locale, { maximumFractionDigits: digits });
  if (bytes < 1000 * 1000) return `${number(Math.max(1, Math.round(bytes / 1000)), 0)} kB`;
  const mb = bytes / (1000 * 1000);
  return `${number(mb, mb < 10 ? 1 : 0)} MB`;
}

/** Whether `entry` is shown under `category` (or every one) for `query`. */
export function matches(entry: PublicDataset, category: Category | "all", query: string): boolean {
  if (category !== "all" && entry.category !== category) return false;
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const text = [entry.title, entry.description, entry.key, ...entry.tables].join(" ").toLowerCase();
  return words.every((w) => text.includes(w));
}

/** What a running install is doing, as a sentence. */
export function jobText(job: InstallJob, t: Translate, locale?: string): string {
  const n = (v: number) => v.toLocaleString(locale);
  switch (job.stage) {
    case "downloading":
      return job.total > 1
        ? t("Downloading {file} (file {i} of {n})", {
            file: job.subject ?? "",
            i: job.done + 1,
            n: job.total,
          })
        : t("Downloading {file}", { file: job.subject ?? "" });
    case "creating":
      return t("Creating the tables");
    case "importing":
      return t("Writing {table}: {done} of about {total} rows", {
        table: job.subject ?? "",
        done: n(job.done),
        total: n(job.total),
      });
    case "finishing":
      return t("Creating the dataset");
    default:
      return t("Starting");
  }
}

/** How far through a running install is, in percent, for its bar: the
 * downloads are the first tenth, the rows the rest. `null` while that is not
 * known. */
export function jobPercent(job: InstallJob): number | null {
  if (job.status !== "running" || !job.stage) return null;
  const share = (done: number, total: number) => (total > 0 ? Math.min(1, done / total) : 0);
  switch (job.stage) {
    case "downloading":
      return Math.round(10 * share(job.done, job.total));
    case "creating":
      return 10;
    case "importing":
      return Math.round(10 + 85 * share(job.done, job.total));
    case "finishing":
      return 98;
    default:
      return null;
  }
}
