import { describe, expect, it } from "vitest";
import type { ProfileRemovalViewV1 } from "$shared/ipc/bindings";
import { removalPresentation } from "./removal";

const base: ProfileRemovalViewV1 = {
  profile: "synthetic-profile",
  phase: "preparing_native",
  native: "unconfirmed",
  cleanup: "not_started",
  blocked: false,
};
describe("canonical profile removal presentation", () => {
  it("does not infer sign-out or erasure from a pending operation", () => {
    expect(removalPresentation(base).fact).toBe("unconfirmed");
  });
  it("keeps local sign-out separate from unknown cleanup", () => {
    expect(
      removalPresentation({ ...base, native: "signed_out_settled", cleanup: "unconfirmed" }).fact,
    ).toBe("signed_out_cleanup_unconfirmed");
  });
  it("does not turn a failed cleanup into verified absence", () => {
    expect(
      removalPresentation({
        ...base,
        phase: "failed_closed",
        blocked: true,
        cleanup: "unconfirmed",
      }),
    ).toEqual({ phase: "failed_closed", fact: "unconfirmed", blocked: true });
  });
  it("distinguishes proven no-data from an observed logout", () => {
    expect(removalPresentation({ ...base, native: "not_configured" }).fact).toBe("not_configured");
  });
  it("shows only the exact managed cleanup fact, not entire-profile completion", () => {
    expect(removalPresentation({ ...base, phase: "erasing_browser", cleanup: "verified" })).toEqual(
      { phase: "erasing_browser", fact: "managed_data_removed", blocked: false },
    );
  });
});
