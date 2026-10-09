import { describe, expect, it } from "vitest";

import { format } from "../i18n";
import {
  categoryName,
  formatBytes,
  jobPercent,
  jobText,
  matches,
  type InstallJob,
  type PublicDataset,
} from "./publicDatasets";

const t = (text: string, args?: Record<string, string | number>) => format(text, args);

const entry = (over: Partial<PublicDataset>): PublicDataset => ({
  key: "nycflights13",
  title: "New York City flights, 2013",
  category: "time_series",
  description: "Every flight that left New York in 2013.",
  homepage: "https://example.com",
  licence: "CC0 1.0",
  licence_url: "https://example.com/licence",
  attribution: "Someone",
  tables: ["nycflights_airlines", "nycflights_flights"],
  rows: 336792,
  download_bytes: 33_206_996,
  installed: false,
  unavailable: null,
  dataset_id: null,
  job: null,
  ...over,
});

const job = (over: Partial<InstallJob>): InstallJob => ({
  key: "nycflights13",
  status: "running",
  stage: null,
  subject: null,
  done: 0,
  total: 0,
  started_at: "2026-10-09T10:00:00Z",
  finished_at: null,
  dataset_id: null,
  dataset_name: null,
  rows: null,
  error: null,
  ...over,
});

describe("the public dataset picker", () => {
  it("names the categories and sizes the downloads", () => {
    expect(categoryName("time_series", t)).toBe("Time series");
    expect(categoryName("spatial", t)).toBe("Spatial");
    expect(formatBytes(15_241, "en")).toBe("15 kB");
    expect(formatBytes(126, "en")).toBe("1 kB");
    expect(formatBytes(2_772_143, "en")).toBe("2.8 MB");
    expect(formatBytes(33_206_996, "en")).toBe("33 MB");
  });

  it("filters by category and by every word of the search, tables included", () => {
    const flights = entry({});
    expect(matches(flights, "all", "")).toBe(true);
    expect(matches(flights, "time_series", "")).toBe(true);
    expect(matches(flights, "spatial", "")).toBe(false);
    expect(matches(flights, "all", "new york")).toBe(true);
    expect(matches(flights, "all", "airlines")).toBe(true);
    expect(matches(flights, "all", "york penguins")).toBe(false);
  });

  it("says what a running install is doing and how far it has got", () => {
    expect(jobText(job({}), t)).toBe("Starting");
    expect(jobPercent(job({}))).toBeNull();

    const downloading = job({ stage: "downloading", subject: "flights.csv", done: 4, total: 5 });
    expect(jobText(downloading, t)).toBe("Downloading flights.csv (file 5 of 5)");
    expect(jobPercent(downloading)).toBe(8);
    expect(jobText(job({ stage: "downloading", subject: "iris.data", total: 1 }), t)).toBe(
      "Downloading iris.data",
    );

    const importing = job({ stage: "importing", subject: "nycflights_flights", done: 168_388, total: 336_776 });
    expect(jobText(importing, t, "en")).toBe("Writing nycflights_flights: 168,388 of about 336,776 rows");
    expect(jobPercent(importing)).toBe(53);
    // More rows than the estimate stays within the bar.
    expect(jobPercent(job({ stage: "importing", done: 12, total: 10 }))).toBe(95);

    expect(jobText(job({ stage: "finishing" }), t)).toBe("Creating the dataset");
    expect(jobPercent(job({ status: "succeeded", stage: "finishing" }))).toBeNull();
  });
});
