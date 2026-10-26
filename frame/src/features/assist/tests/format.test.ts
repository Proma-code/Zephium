import { expect, test } from "vitest";
import { costLine } from "../lib/format";

test.each(["ChatGPT", "OpenCode Go", "Local gateway", "OpenAI"])(
  "%s with no reported price is unavailable, not free, local, or still confirming",
  (provider) => {
    expect(costLine({ kind: "byok", model: "Example model", provider, usd: null })).toBe(
      "Example model · cost unavailable",
    );
  },
);

test("an actual own-key price remains an estimate and preserves the exact source metadata", () => {
  expect(costLine({ kind: "byok", model: "Example model", provider: "OpenAI", usd: "0.004" })).toBe(
    "Example model · ≈ $0.004 on your OpenAI key",
  );
  expect(costLine({ kind: "byok", model: "Example model", provider: "OpenAI", usd: "0" })).toBe(
    "Example model · ≈ $0 on your OpenAI key",
  );
});

test("hosted credits still distinguish pending confirmation from settled credit amounts", () => {
  expect(costLine({ kind: "hosted", model: "Example model", credits: null })).toBe(
    "Example model · confirming",
  );
  expect(costLine({ kind: "hosted", model: "Example model", credits: "1" })).toBe(
    "Example model · 1 credit",
  );
  expect(costLine({ kind: "hosted", model: "Example model", credits: "0.3" })).toBe(
    "Example model · 0.3 credits",
  );
});

test("only an explicitly native local cost retains the on-device wording", () => {
  expect(costLine({ kind: "local", model: "Example model" })).toBe(
    "Example model · on this device",
  );
});
