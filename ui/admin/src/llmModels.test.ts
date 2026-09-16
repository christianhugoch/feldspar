/**
 * A provider's models as the admin screens present them: the agent form's
 * model pick-list, and the summary a model row shows.
 */

import { describe, expect, it } from "vitest";

import {
  capabilitySummary,
  formatPrice,
  formatTokens,
  modelOptions,
  namesToOffer,
  type ModelItem,
} from "./llmModels";

function model(name: string, isDefault = false): ModelItem {
  return {
    id: name,
    provider_id: "p1",
    name,
    description: "",
    is_default: isDefault,
    config: {},
    capabilities: {},
    prices: {},
  };
}

describe("the agent form's model pick-list", () => {
  it("offers the provider default first, naming it, then each model with the default marked", () => {
    const options = modelOptions([model("claude-sonnet-5", true), model("claude-haiku-4-5")], "");
    expect(options).toEqual([
      { value: "", label: "Provider default (claude-sonnet-5)" },
      { value: "claude-haiku-4-5", label: "claude-haiku-4-5" },
      { value: "claude-sonnet-5", label: "claude-sonnet-5 (default)" },
    ]);
  });

  it("says when the provider has no default", () => {
    expect(modelOptions([model("gpt-5.1")], "")[0].label).toBe("Provider default (none set)");
    expect(modelOptions([], "")[0].label).toBe("Provider default (none set)");
  });

  it("keeps a model the agent names that has no row, marked missing", () => {
    const options = modelOptions([model("claude-sonnet-5", true)], "claude-opus-4-1");
    expect(options[options.length - 1]).toEqual({
      value: "claude-opus-4-1",
      label: "claude-opus-4-1 (missing)",
    });
    // Not when it exists.
    expect(modelOptions([model("claude-sonnet-5", true)], "claude-sonnet-5")).toHaveLength(2);
  });
});

describe("a model row's summary", () => {
  it("shows a blank price as unknown, never as free", () => {
    expect(formatPrice(null)).toBe("unknown");
    expect(formatPrice(undefined)).toBe("unknown");
    expect(formatPrice(0)).toBe("0/M");
    expect(formatPrice(3)).toBe("3/M");
  });

  it("abbreviates token counts", () => {
    expect(formatTokens(200_000)).toBe("200k");
    expect(formatTokens(1_000_000)).toBe("1M");
    expect(formatTokens(32_000)).toBe("32k");
    expect(formatTokens(512)).toBe("512");
  });

  it("lists what the resolved capabilities add", () => {
    expect(
      capabilitySummary({
        parallel_tool_calls: true,
        parallel_tool_calls_default: false,
        native_apply_patch: true,
        reasoning_replay: true,
        prompt_caching: "automatic",
        edit_format: "apply_patch",
        context_window: 400_000,
        working_budget: 100_000,
        vision: true,
      }),
    ).toEqual([
      "400k window",
      "100k budget",
      "apply_patch",
      "automatic caching",
      "vision",
      "reasoning replay",
      "native apply_patch",
    ]);
  });

  it("offers only fetched names that still have no row", () => {
    expect(namesToOffer(["a", "b", "c"], [model("b")])).toEqual(["a", "c"]);
  });
});
