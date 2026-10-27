import { describe, expect, it } from "vitest";
import type { CodexModelV1 } from "$shared/ipc/codex-bindings";
import { codexFavouriteKey, filterCodexCatalog } from "../catalog";
const models: CodexModelV1[] = [
  { id: "original/sol", name: "GPT-6.1 Sol", efforts: ["low", "high"], default_effort: "high" },
  { id: "original-astra", name: "GPT-6 Astra", efforts: ["max"], default_effort: "max" },
];
describe("native Codex catalog presentation", () => {
  it("filters original names and IDs without replacing or rewriting the catalog objects", () => {
    expect(filterCodexCatalog(models, " GPT sol ", false, new Set())).toEqual([models[0]]);
    expect(filterCodexCatalog(models, "original-astra", false, new Set())[0]).toBe(models[1]);
    expect(filterCodexCatalog(models, "missing", false, new Set())).toEqual([]);
    expect(models[0]!.id).toBe("original/sol");
  });
  it("keeps source-prefixed favourites distinct from API and direct ChatGPT choices", () => {
    const keys = new Set(["openai/original/sol", "chatgpt/original/sol", "codex/original/sol"]);
    expect(filterCodexCatalog(models, "", true, keys)).toEqual([]);
    keys.add(codexFavouriteKey(models[0]!.id));
    expect(filterCodexCatalog(models, "", true, keys)).toEqual([models[0]]);
    expect(codexFavouriteKey("a\nb")).toBe("codex/a%0Ab");
    expect(codexFavouriteKey("a/b")).not.toBe(codexFavouriteKey("a%2Fb"));
  });
  it("intersects filters without falling back when a saved model is absent", () => {
    const keys = new Set([codexFavouriteKey("old-model"), codexFavouriteKey(models[1]!.id)]);
    expect(filterCodexCatalog(models, "sol", true, keys)).toEqual([]);
    expect(filterCodexCatalog(models, "astra", true, keys)).toEqual([models[1]]);
  });
});
