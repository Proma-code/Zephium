import { describe, expect, test } from "vitest";
import type { WorkModelEntry, WorkModelsV1 } from "$shared/ipc/bindings";
import {
  contextSize,
  dollars,
  effortLevels,
  modelEffort,
  keyedProviders,
  locked,
  modelName,
  modelConnection,
  modelContext,
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
    expect(shortName(entry("open_ai", "gpt-6-sol", "GPT-6 Sol", true))).toBe("GPT-6 Sol");
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

describe("retired direct-account catalog", () => {
  const retained = () =>
    entry("open_ai", "gpt-example", "GPT Example", false, ["lead", "page"], {
      id: "chatgpt/gpt-example",
      price: null,
    });
  test("stale direct entries cannot offer a second subscription route or effort", () => {
    for (const keys of [{}, { open_ai: "valid" as const }]) {
      const view = models({ keys });
      const old = retained();
      view.entries.push(old);
      view.efforts.push({ id: old.id, levels: ["high"], effort: "high" });
      view.chosen.page = old.id;
      const before = structuredClone(view);
      for (const role of ["lead", "page", "decision"] as const) {
        const groups = pickerGroups(view, role, [old]);
        expect(groups.ready.flatMap((group) => group.entries)).not.toContainEqual(old);
        expect(searchEntries(groups.ready, "chatgpt")).toEqual([]);
      }
      expect(effortLevels(view, old.id)).toEqual([]);
      expect(view).toEqual(before);
      expect(modelConnection(old)).toBe("chatgpt");
      expect(modelName(old)).toBe("GPT Example");
    }
  });
  test("retirement neither grants API access nor removes configured API models", () => {
    for (const configured of [false, true]) {
      const view = models({ keys: configured ? { open_ai: "valid" } : {} });
      view.entries.push(retained());
      const groups = pickerGroups(view, "lead");
      expect(usable(view, "open_ai")).toBe(configured);
      expect(
        groups.ready.find((group) => group.provider === "open_ai")?.entries ?? [],
      ).toHaveLength(configured ? 3 : 0);
      expect(groups.needsKey.includes("open_ai")).toBe(!configured);
    }
  });
});

test("OpenAI names stay clean while connection metadata distinguishes identical native models", () => {
  const api = entry("open_ai", "gpt-example", "GPT Example", false);
  const subscription = { ...api, id: "chatgpt/gpt-example" };
  expect(modelName(api)).toBe("GPT Example");
  expect(modelConnection(api)).toBe("api");
  expect(modelConnection(subscription)).toBe("chatgpt");
  expect(modelName(subscription)).toBe("GPT Example");
  expect(modelName({ ...subscription, display_name: "GPT Example · ChatGPT" })).toBe("GPT Example");
  expect(modelName({ ...api, display_name: "GPT Example · API" })).toBe("GPT Example");
  expect(api.display_name).toBe("GPT Example");
  expect(api.id).toBe("openai/gpt-example");
  const view = models({ keys: { open_ai: "valid" } });
  view.entries.push(api, subscription);
  const groups = pickerGroups(view, "lead");
  expect(searchEntries(groups.ready, "gpt example api")).toEqual([api]);
  expect(searchEntries(groups.ready, "gpt example chatgpt")).toEqual([]);
});

test("all retired connection entries are excluded without modifying saved state or API access", () => {
  const view = models({
    keys: Object.fromEntries(keyedProviders.map((provider) => [provider, "valid"])),
    base: "http://localhost:11434/v1",
  });
  const endpoint = entry("compatible", "local", "Local", false);
  for (const [provider, prefix] of [
    ["open_ai", "chatgpt"],
    ["compatible", "gateway"],
    ["open_code_go", "opencode-go"],
  ] as const) {
    const retired = entry(provider, "example", "Example", false, ["lead", "page", "light"], {
      id: `${prefix}/saved/example`,
    });
    view.entries.push(retired);
    view.chosen.page = retired.id;
    view.efforts.push({ id: retired.id, levels: ["high"], effort: "high" });
    const before = structuredClone(view);
    for (const role of ["lead", "page", "light"] as const) {
      const offered = pickerGroups(view, role, [endpoint, retired]).ready.flatMap(
        (group) => group.entries,
      );
      expect(offered.some((row) => /^(chatgpt|gateway|opencode-go)\//u.test(row.id))).toBe(false);
      expect(offered).toContainEqual(endpoint);
    }
    expect(effortLevels(view, retired.id)).toEqual([]);
    expect(view).toEqual(before);
  }
  expect(keyedProviders).toEqual([
    "anthropic",
    "open_ai",
    "google",
    "xai",
    "mistral",
    "deep_seek",
    "groq",
    "cerebras",
    "open_router",
    "compatible",
  ]);
  for (const provider of keyedProviders) expect(usable(view, provider)).toBe(true);
});

describe("model context provenance", () => {
  test.each([
    ["open_ai", "chatgpt/example", "32K context budget"],
    ["compatible", `gateway/${"a".repeat(32)}/example`, "32K context budget"],
    ["open_code_go", `opencode-go/${"a".repeat(32)}/example`, "32K context budget"],
    ["open_ai", "openai/example", "32K context"],
    ["compatible", "compatible/example", "32K context"],
    ["anthropic", "anthropic/example", "32K context"],
    ["cloud", "zephium/example", "32K context"],
    ["google", "chatgpt/example", "32K context"],
  ] as const)("%s %s has source-appropriate capacity wording", (provider, id, expected) => {
    const model = entry(provider, "example", "Example", false, ["lead"], {
      id,
      context_window: 32768,
      max_output: 8192,
    });
    expect(modelContext(model)).toBe(expected);
    expect(modelName(model)).toBe("Example");
    expect(model.context_window).toBe(32768);
    expect(model.max_output).toBe(8192);
  });
});

test("selected-model effort does not depend on a hosted account or a Work lead", () => {
  const value = models({});
  const id = "chatgpt/example";
  value.efforts.push({ id, levels: ["low", "medium", "high", "xhigh", "max"], effort: "medium" });
  value.effective.page = id;
  expect(value.effort).toBeNull();
  expect(value.effective.lead).toBeNull();
  expect(value.cloud.signed_in).toBe(false);
  expect(modelEffort(value, id)).toBe("medium");
  value.effort = "max";
  expect(modelEffort(value, id)).toBe("medium");
});

test("missing or inconsistent entry effort is not guessed from a different model", () => {
  const value = models({ lead: "openai/gpt-6-luna", effort: "max" });
  const id = "chatgpt/example";
  value.efforts.push({ id, levels: ["low", "medium", "high"], effort: null });
  expect(modelEffort(value, id)).toBeNull();
  value.efforts[value.efforts.length - 1]!.effort = "max";
  expect(modelEffort(value, id)).toBeNull();
  expect(modelEffort(value, "chatgpt/missing")).toBeNull();
  expect(modelEffort(null, id)).toBeNull();
});
