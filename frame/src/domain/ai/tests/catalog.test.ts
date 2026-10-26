import { describe, expect, test } from "vitest";
import type { WorkModelEntry, WorkModelsV1 } from "$shared/ipc/bindings";
import {
  contextSize,
  dollars,
  effortLevels,
  hasChatGptModels,
  locked,
  modelName,
  pickerGroups,
  priceTier,
  searchEntries,
  shortName,
  usable,
} from "../catalog";
import { entry, models } from "$shared/testing/work-models";

describe("the picker's list", () => {
  test("groups the curated models of usable providers, recommended first, hosted first", () => {
    const view = models({
      keys: { anthropic: "valid", open_ai: "set", google: "invalid" },
      cloud: true,
    });
    const { ready, needsKey } = pickerGroups(view, "lead");
    expect(ready.map((group) => group.provider)).toEqual(["cloud", "anthropic", "open_ai"]);
    expect(ready[1]!.entries.map((item) => item.id)).toEqual([
      "anthropic/claude-opus-5-5",
      "anthropic/claude-sonnet-5-5",
      "anthropic/claude-fable-5-1",
    ]);
    expect(ready[2]!.entries.map((item) => item.id)).toEqual([
      "openai/gpt-6-luna",
      "openai/gpt-6-sol",
      "openai/gpt-6-astra",
    ]);
    // A refused key reads as needing one, not as broken; only the best-known
    // providers ask, and the rest wait in Settings.
    expect(needsKey).toEqual(["google"]);
    // Signed out, hosted models are neither offered nor listed as needing a key.
    const signedOut = pickerGroups(
      { ...view, cloud: { signed_in: false, plan: null, locked: [], required: [] } },
      "lead",
    );
    expect(signedOut.ready.map((group) => group.provider)).toEqual(["anthropic", "open_ai"]);
    expect(signedOut.needsKey).not.toContain("cloud");
    // A plan-locked hosted model stays listed to explain the plan, but a
    // catalog of only locked models cannot run anything.
    const id = view.entries.find((item) => item.model.provider === "cloud")!.id;
    const lockedView = { ...view, cloud: { ...view.cloud, locked: [id] } };
    expect(locked(lockedView, id)).toBe(true);
    expect(usable(lockedView, "cloud")).toBe(false);
    expect(usable(view, "cloud")).toBe(true);
  });

  test("a custom server adds the models it serves", () => {
    const view = models({ base: "http://localhost:11434/v1" });
    const served = [entry("compatible", "qwen4:32b", "qwen4:32b", false)];
    const { ready } = pickerGroups(view, "lead", served);
    expect(ready.map((group) => group.provider)).toEqual(["compatible"]);
    expect(ready[0]!.entries[0]!.display_name).toBe("qwen4:32b");
  });

  test("a custom server is usable once it has an address, keys only when set or valid", () => {
    const view: WorkModelsV1 = models({
      keys: { deep_seek: "invalid" },
      base: "http://localhost:11434/v1",
    });
    expect(usable(view, "compatible")).toBe(true);
    expect(usable(view, "deep_seek")).toBe(false);
    expect(usable(null, "anthropic")).toBe(false);
  });

  test("the trigger drops a family name the mark already says", () => {
    const opus: WorkModelEntry = entry("anthropic", "claude-opus-5-5", "Claude Opus 5.5", true);
    expect(shortName(opus)).toBe("Opus 5.5");
    expect(shortName(entry("open_ai", "gpt-6-sol", "GPT-6 Sol", true))).toBe("GPT-6 Sol · API");
  });
});

describe("the picker's details", () => {
  test("context windows and prices read the way a model card says them", () => {
    expect(contextSize(1_000_000)).toBe("1M");
    expect(contextSize(1_048_576)).toBe("1M");
    expect(contextSize(1_050_000)).toBe("1M");
    expect(contextSize(200_000)).toBe("200K");
    expect(contextSize(131_072)).toBe("128K");
    expect(dollars(2_000_000)).toBe("$2");
    expect(dollars(1_320_000)).toBe("$1.32");
    expect(dollars(100_000)).toBe("$0.1");
    expect(dollars(10_000)).toBe("$0.01");
    expect(priceTier({ input: 100_000, cached_input: 10_000, output: 500_000 })).toBe(1);
    expect(priceTier({ input: 2_000_000, cached_input: 200_000, output: 10_000_000 })).toBe(2);
    expect(priceTier({ input: 4_000_000, cached_input: 200_000, output: 20_000_000 })).toBe(3);
    expect(priceTier({ input: 10_000_000, cached_input: 1_000_000, output: 50_000_000 })).toBe(4);
    expect(priceTier(null)).toBeNull();
  });

  test("search finds models across providers by name, maker or id", () => {
    const view = models({ keys: { anthropic: "valid", open_ai: "valid" } });
    const { ready } = pickerGroups(view, "lead");
    expect(searchEntries(ready, "sonnet").map((item) => item.id)).toEqual([
      "anthropic/claude-sonnet-5-5",
    ]);
    expect(searchEntries(ready, "openai astra").map((item) => item.id)).toEqual([
      "openai/gpt-6-astra",
    ]);
    expect(searchEntries(ready, "  ")).toEqual([]);
  });

  test("each entry offers the efforts Rust says it takes", () => {
    const view = models({ keys: { deep_seek: "valid" }, lead: "deepseek/deepseek-v4-pro" });
    expect(effortLevels(view, "deepseek/deepseek-v4-pro")).toEqual(["low", "high"]);
    expect(effortLevels(view, "nobody/none")).toEqual([]);
    expect(view.effort).toBe("high");
  });
});

describe("ChatGPT account models", () => {
  const chatgpt = () =>
    entry("open_ai", "gpt-example", "GPT Example · ChatGPT", false, ["lead", "page"], {
      id: "chatgpt/gpt-example",
      price: null,
    });

  test("native account models work without granting access to OpenAI API models", () => {
    const view = models({});
    view.entries.push(chatgpt());
    expect(usable(view, "open_ai")).toBe(false);
    expect(hasChatGptModels(view)).toBe(true);
    const groups = pickerGroups(view, "lead");
    expect(groups.ready.find((group) => group.provider === "open_ai")?.entries).toEqual([
      chatgpt(),
    ]);
    expect(groups.needsKey).not.toContain("open_ai");
  });

  test("API key access and account models remain independent, and disconnect removes only the latter", () => {
    const view = models({ keys: { open_ai: "valid" } });
    view.entries.push(chatgpt());
    expect(
      pickerGroups(view, "lead").ready.find((group) => group.provider === "open_ai")?.entries,
    ).toContainEqual(chatgpt());
    view.entries = view.entries.filter((entry) => !entry.id.startsWith("chatgpt/"));
    expect(hasChatGptModels(view)).toBe(false);
    expect(usable(view, "open_ai")).toBe(true);
    expect(
      pickerGroups(view, "lead").ready.find((group) => group.provider === "open_ai")?.entries,
    ).toHaveLength(3);
  });

  test("endpoint and more-model results cannot invent subscription eligibility", () => {
    for (const keys of [{}, { open_ai: "valid" as const }]) {
      const view = models({ keys });
      expect(hasChatGptModels(view)).toBe(false);
      expect(
        pickerGroups(view, "lead", [chatgpt()]).ready.flatMap((group) => group.entries),
      ).not.toContainEqual(chatgpt());
    }
  });

  test("account models respect role admission and keep their billing identity in search", () => {
    const view = models({});
    view.entries.push(chatgpt());
    const groups = pickerGroups(view, "page");
    expect(searchEntries(groups.ready, "chatgpt")).toEqual([chatgpt()]);
    expect(pickerGroups(view, "decision").ready).toEqual([]);
  });
});

test("OpenAI billing labels distinguish identical models without mutating native identities", () => {
  const api = entry("open_ai", "gpt-example", "GPT Example", false);
  const subscription = { ...api, id: "chatgpt/gpt-example" };
  expect(modelName(api)).toBe("GPT Example · API");
  expect(modelName(subscription)).toBe("GPT Example · ChatGPT");
  expect(modelName({ ...subscription, display_name: "GPT Example · ChatGPT" })).toBe(
    "GPT Example · ChatGPT",
  );
  expect(modelName({ ...api, display_name: "GPT Example · API" })).toBe("GPT Example · API");
  expect(api.display_name).toBe("GPT Example");
  expect(api.id).toBe("openai/gpt-example");
  const view = models({ keys: { open_ai: "valid" } });
  view.entries.push(api, subscription);
  const groups = pickerGroups(view, "lead");
  expect(searchEntries(groups.ready, "gpt example api")).toEqual([api]);
  expect(searchEntries(groups.ready, "gpt example chatgpt")).toEqual([subscription]);
});
