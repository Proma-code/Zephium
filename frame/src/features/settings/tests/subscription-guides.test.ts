import { describe, expect, test } from "vitest";
import { subscriptionGuides } from "../lib/subscription-guides";

describe("subscription setup documentation", () => {
  test("only official guides and fixed manual commands are offered", () => {
    expect(subscriptionGuides.map((guide) => guide.id)).toEqual([
      "claude",
      "antigravity",
      "opencode",
    ]);
    expect(subscriptionGuides.map((guide) => new URL(guide.documentation).hostname)).toEqual([
      "code.claude.com",
      "antigravity.google",
      "opencode.ai",
    ]);
    for (const guide of subscriptionGuides) {
      expect(new URL(guide.documentation).protocol).toBe("https:");
      expect(guide.support).not.toBe("available");
    }
    expect(
      subscriptionGuides.flatMap((guide) =>
        guide.support === "native_runner_required" ? [guide.command] : [],
      ),
    ).toEqual(["claude auth login", "opencode auth login"]);
  });

  test("unsupported Antigravity consumer linking has no login command", () => {
    const antigravity = subscriptionGuides.find((guide) => guide.id === "antigravity");
    expect(antigravity?.support).toBe("unsupported");
    expect(antigravity).not.toHaveProperty("command");
    expect(antigravity?.documentation).toBe("https://antigravity.google/docs/faq");
  });
});
