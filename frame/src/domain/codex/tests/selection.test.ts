import { expect, test } from "vitest";
import { codexModelSelection, sameCodexEffort, supportsCodexEffort } from "../selection";
import type { CodexModelV1 } from "$shared/ipc/codex-bindings";
const model: CodexModelV1 = {
  id: "original",
  name: "Original",
  efforts: ["minimal", "high"],
  default_effort: "high",
};
test("model selection preserves default omission instead of sending the advertised value", () => {
  expect(codexModelSelection(model, null)).toEqual({
    model: "original",
    effort: { kind: "default" },
  });
});
test("same-model explicit choice is retained; another model's effort never transfers", () => {
  const explicit = { model: "original", effort: { kind: "explicit", effort: "minimal" } } as const;
  expect(codexModelSelection(model, explicit)).toEqual(explicit);
  expect(codexModelSelection(model, { ...explicit, model: "other" })).toEqual({
    model: "original",
    effort: { kind: "default" },
  });
});
test("default-only and independent-default models stay selectable without fabricated presets", () => {
  for (const row of [
    { ...model, efforts: [], default_effort: "none" as const },
    { ...model, default_effort: "max" as const },
  ]) {
    expect(codexModelSelection(row, null)?.effort).toEqual({ kind: "default" });
    expect(supportsCodexEffort(row, { kind: "default" })).toBe(true);
  }
  expect(
    supportsCodexEffort(
      { ...model, efforts: [], default_effort: "none" },
      { kind: "explicit", effort: "none" },
    ),
  ).toBe(false);
});
test("unqualified default needs a supported explicit choice, never clamping", () => {
  expect(codexModelSelection({ ...model, default_effort: null }, null)).toBeNull();
  expect(
    codexModelSelection(
      { ...model, default_effort: null },
      { model: "original", effort: { kind: "explicit", effort: "max" } },
    ),
  ).toBeNull();
});
test("readback equality is structural and default is distinct from explicit none", () => {
  expect(sameCodexEffort({ kind: "default" }, { kind: "default" })).toBe(true);
  expect(sameCodexEffort({ kind: "default" }, { kind: "explicit", effort: "none" })).toBe(false);
  expect(
    sameCodexEffort({ kind: "explicit", effort: "none" }, { kind: "explicit", effort: "none" }),
  ).toBe(true);
});
