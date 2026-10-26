import { expect, test } from "vitest";
import { goView } from "$shared/testing/opencode-go";
import { goBilling, goStatus, goFault } from "../presentation";
import { goKeyValid } from "../validation";

test("Go key bounds match native; whitespace trimming belongs to the input form", () => {
  expect(goKeyValid("a".repeat(1024))).toBe(true);
  expect(goKeyValid("!~")).toBe(true);
  for (const key of ["", "key key", " key", "key\n", "é", "a".repeat(1025)])
    expect(goKeyValid(key)).toBe(false);
});

test("saving and public model listing never claim authenticated subscription status", () => {
  const view = goView("profile");
  expect(goStatus(view)).toBe("No OpenCode Go key saved");
  expect(goStatus({ ...view, state: "configured" })).toBe("Key saved — catalog not checked");
  expect(goStatus({ ...view, state: "catalogued", models: 3 })).toBe(
    "Public catalog listed · 3 supported models",
  );
  expect(goBilling(view)).toBeNull();
  expect(goFault("unauthorized")).toContain("did not accept the key");
  expect(goFault("missing_conversation")).toContain("stable conversation");
});

test.each(["free", "balance", "go", "go_plus", "byok"] as const)(
  "billing %s is a dated last-request observation, never monthly usage",
  (source) => {
    const view = goView("profile", { last_billing: { source, observed_at_ms: "1700000000000" } });
    expect(goBilling(view)).toContain("Last provider-reported billing:");
    expect(goBilling(view)).not.toMatch(/remaining|monthly|%/u);
    for (const observed_at_ms of ["NaN", "-1", "9999999999999999"])
      expect(goBilling({ ...view, last_billing: { source, observed_at_ms } })).toBeNull();
  },
);
