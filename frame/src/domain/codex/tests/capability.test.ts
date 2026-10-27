import { expect, test } from "vitest";
import { codexView } from "$shared/testing/codex";
import { codexCapability } from "../presentation";
import { validCodexView } from "../validation";

test("browser capability is a native observation, not a connected-account assumption", () => {
  const limited = codexView("profile", { state: "connected", text_only: true });
  const browser = codexView("profile", { state: "connected", text_only: false });
  expect(validCodexView(limited)).toBe(true);
  expect(validCodexView(browser)).toBe(true);
  expect(codexCapability(limited)).toBe("Browser tools unavailable");
  expect(codexCapability(browser)).toBe("Browser tools");
  expect(limited.text_only).toBe(true);
  expect(browser.selection).toBeNull();
});

test("unavailable and signed-out observations never advertise browser capability", () => {
  expect(codexCapability(null)).toBeNull();
  for (const state of [
    "unavailable",
    "preparing",
    "signed_out",
    "signing_in",
    "recovery_required",
  ] as const) {
    expect(codexCapability(codexView("profile", { state, text_only: false }))).toBeNull();
  }
});

test("missing or coerced capability values are rejected rather than promoted", () => {
  for (const text_only of [undefined, null, "false", "true", 0, 1, {}]) {
    expect(validCodexView({ ...codexView("profile"), text_only })).toBe(false);
  }
});
