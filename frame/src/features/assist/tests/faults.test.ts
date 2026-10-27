import { expect, test } from "vitest";
import type {
  ChatGptFailureIssue,
  ChatGptModelFault,
  WorkModelFaultV1,
} from "$shared/ipc/bindings";
import { availabilityView, faultView } from "../lib/faults";

const model = (fault: WorkModelFaultV1, retry_after_ms: number | null = null) =>
  faultView({ reason: "model", fault, retry_after_ms });

test("out of credits sends the person to buy more, not to retry", () => {
  const view = model("hosted_usage");
  expect(view.fix?.command).toBe("settings.account");
  expect(view.retry).toBe("no");
});

test("a rate limit waits, then offers the same question again", () => {
  expect(model("rate_limited", 12_000)).toMatchObject({ retry: "wait", fix: null, wait: 12 });
});

test("a missing or refused key leads to Settings → AI", () => {
  expect(model("missing_key").fix?.command).toBe("settings.ai");
  expect(model("key_refused").fix?.command).toBe("settings.ai");
  expect(faultView({ reason: "no_model" }).fix?.command).toBe("settings.ai");
});

test("a request over the provider plan's per-minute limit is not retried", () => {
  expect(model("plan_limit")).toMatchObject({ retry: "no", fix: { command: "settings.ai" } });
});

test("a plan a model needs and the daily limit point at the account", () => {
  expect(model("plan_required").fix?.command).toBe("settings.account");
  expect(model("daily_limit").message).toContain("resets");
});

test("a page that changed may be asked again; no selection may not", () => {
  expect(faultView({ reason: "page_changed" }).retry).toBe("now");
  expect(faultView({ reason: "no_selection" }).retry).toBe("no");
});

test("every page that is not attached has words of its own, and says whether talk goes on", () => {
  const kinds = [
    "ai_off",
    "private",
    "sensitive",
    "not_web",
    "no_model",
    "unsupported",
    "loading",
    "unavailable",
  ] as const;
  const titles = kinds.map((kind) => availabilityView(kind).title);
  expect(new Set(titles).size).toBe(titles.length);
  expect(availabilityView("private").talks).toBe(false);
  expect(availabilityView("ai_off").talks).toBe(false);
  expect(availabilityView("sensitive").talks).toBe(true);
  expect(availabilityView("not_web").talks).toBe(true);
});

test("a reply that ran too long may be sent again", () => {
  expect(faultView({ reason: "too_long" }).retry).toBe("now");
});

test("a page still loading is asked again, not called unavailable", () => {
  const view = faultView({ reason: "loading" });
  expect(view.retry).toBe("now");
  expect(view.message).toContain("still loading");
  expect(faultView({ reason: "invalid" }).retry).toBe("no");
});

test("a provider limit without a native deadline offers manual retry without an invented countdown", () => {
  const view = model("rate_limited");
  expect(view).toMatchObject({ retry: "now", wait: null, fix: null });
  expect(view.message).toBe("The selected provider has limited this request. Try again later.");
  expect(view.message).not.toContain("Zephium AI");
});

test("only a valid positive native retry interval becomes a countdown, without a fabricated cap", () => {
  for (const interval of [0, -1, NaN, Infinity, 1.5])
    expect(model("rate_limited", interval)).toMatchObject({ retry: "now", wait: null });
  expect(model("rate_limited", 1501)).toMatchObject({ retry: "wait", wait: 2 });
  expect(model("rate_limited", 900_000)).toMatchObject({ retry: "wait", wait: 900 });
});

test("ChatGPT HTTP evidence stays specific without guessing a usage reset or account remedy", () => {
  const view = faultView({
    reason: "chat_gpt",
    fault: {
      diagnostic: { field: null, stage: "http", status: 429, issue: "usage_limit" },
    },
  });
  expect(view).toEqual({
    message: "ChatGPT limited this request. Try again later.",
    fix: null,
    retry: "now",
    diagnostic: { code: "CHATGPT/HTTP/429/USAGE_LIMIT", uncertain: false },
  });
  expect(view.wait).toBeUndefined();
});

test.each(["signed_out", "not_eligible", "unsupported"] as const)(
  "ChatGPT %s points to the selected connection, never Zephium service sign-in",
  (issue) => {
    const view = faultView({
      reason: "chat_gpt",
      fault: {
        diagnostic: { field: null, stage: "credential", status: null, issue },
      },
    });
    expect(view.fix?.command).toBe("settings.ai");
    expect(view.retry).toBe("no");
    expect(view.diagnostic?.uncertain).toBe(false);
    expect(view.message).not.toContain("Zephium AI");
  },
);

test.each(["timeout", "unavailable", "incomplete", "protocol", "cancelled"] as const)(
  "ChatGPT %s offers only explicit retry, without inventing account or timing state",
  (issue) => {
    const view = faultView({
      reason: "chat_gpt",
      fault: {
        diagnostic: { field: null, stage: "send", status: null, issue },
      },
    });
    expect(view).toMatchObject({ retry: "now", fix: null, diagnostic: { uncertain: true } });
    expect(view.wait).toBeUndefined();
  },
);

test("pre-dispatch cancellation is not presented as signed-out or possible provider usage", () => {
  expect(
    faultView({
      reason: "chat_gpt",
      fault: {
        diagnostic: { field: null, stage: "credential", status: null, issue: "cancelled" },
      },
    }),
  ).toMatchObject({
    message: "The ChatGPT request was stopped.",
    fix: null,
    diagnostic: { code: "CHATGPT/CREDENTIAL/-/CANCELLED", uncertain: false },
  });
});

test("post-admission and unclassified HTTP failures warn that the request may have used quota", () => {
  for (const stage of ["request", "credential", "send", "http", "stream", "complete"] as const) {
    const view = faultView({
      reason: "chat_gpt",
      fault: {
        diagnostic: { field: null, stage, status: null, issue: "protocol" },
      },
    });
    expect(view.diagnostic?.uncertain).toBe(["send", "http", "stream", "complete"].includes(stage));
  }
});

test("all native ChatGPT issues produce finite copyable codes and actionable text", () => {
  const issues = [
    "retired",
    "invalid_request",
    "signed_out",
    "not_eligible",
    "usage_limit",
    "unavailable",
    "unsupported",
    "timeout",
    "protocol",
    "incomplete",
    "usage_unavailable",
    "cancelled",
  ] satisfies ChatGptFailureIssue[];
  for (const issue of issues) {
    const view = faultView({
      reason: "chat_gpt",
      fault: {
        diagnostic: { field: null, stage: "complete", status: 200, issue },
      },
    });
    expect(view.message).toContain("ChatGPT");
    expect(view.diagnostic?.code).toMatch(/^CHATGPT\/COMPLETE\/200\/[A-Z_]+$/u);
  }
  expect(faultView({ reason: "chat_gpt", fault: "signed_out" }).diagnostic?.code).toBe(
    "CHATGPT/LEGACY/-/SIGNED_OUT",
  );
});

test("unknown diagnostic strings and invalid statuses are never copied from the wire", () => {
  const fault = {
    diagnostic: {
      field: null,
      stage: "private-provider-data",
      status: "secret",
      issue: "raw-provider-message",
    },
  } as unknown as ChatGptModelFault;
  const view = faultView({ reason: "chat_gpt", fault });
  expect(view.diagnostic).toEqual({ code: "CHATGPT/UNKNOWN/-/PROTOCOL", uncertain: false });
  expect(JSON.stringify(view)).not.toMatch(/private-provider-data|secret|raw-provider-message/u);
  for (const status of [0, 99, 600, NaN, Infinity, 401.5]) {
    expect(
      faultView({
        reason: "chat_gpt",
        fault: {
          diagnostic: { field: null, stage: "http", status, issue: "protocol" },
        },
      }).diagnostic?.code,
    ).toBe("CHATGPT/HTTP/-/PROTOCOL");
  }
});

test("HTTP body failures stay uncertain even when the received status would otherwise be a refusal", () => {
  for (const status of [400, 401, 403, 429, 500, 502, 599]) {
    for (const issue of [
      "protocol",
      "timeout",
      "signed_out",
      "not_eligible",
      "usage_limit",
      "unavailable",
    ] as const) {
      const view = faultView({
        reason: "chat_gpt",
        fault: {
          diagnostic: { field: null, stage: "http", status, issue },
        },
      });
      expect(view.diagnostic?.uncertain).toBe(
        status >= 500 || issue === "protocol" || issue === "timeout",
      );
    }
  }
});

test("only finite request-field categories enter a copyable ChatGPT HTTP400 diagnostic", () => {
  for (const field of [
    "model",
    "reasoning",
    "tools",
    "input",
    "store",
    "stream",
    "unknown",
  ] as const) {
    expect(
      faultView({
        reason: "chat_gpt",
        fault: {
          diagnostic: { stage: "http", status: 400, issue: "invalid_request", field },
        },
      }).diagnostic?.code,
    ).toBe(`CHATGPT/HTTP/400/INVALID_REQUEST/${field.toUpperCase()}`);
  }
  const unsafe = {
    diagnostic: {
      stage: "http",
      status: 400,
      issue: "invalid_request",
      field: "tools[0].private-value",
    },
  } as unknown as ChatGptModelFault;
  expect(faultView({ reason: "chat_gpt", fault: unsafe }).diagnostic?.code).toBe(
    "CHATGPT/HTTP/400/INVALID_REQUEST/UNKNOWN",
  );
});

test("missing accounting for a continuation is a halt, not a malformed or rejected text response", () => {
  const view = faultView({
    reason: "chat_gpt",
    fault: {
      diagnostic: { stage: "complete", status: 200, issue: "usage_unavailable", field: null },
    },
  });
  expect(view).toMatchObject({
    message:
      "ChatGPT completed the response without usage details. Tool execution paused; no further request was sent.",
    retry: "no",
    fix: null,
    diagnostic: { uncertain: true },
  });
});

test("native Codex history states preserve distinct evidence without automatic replay", () => {
  const uncertain = faultView({ reason: "native_conversation", fault: "history_uncertain" });
  expect(uncertain).toMatchObject({
    retry: "no",
    diagnostic: { code: "CODEX/HISTORY_UNCERTAIN", uncertain: false },
  });
  expect(uncertain.message).toContain("final reply");
  const diverged = faultView({ reason: "native_conversation", fault: "history_diverged" });
  expect(diverged).toMatchObject({
    retry: "no",
    diagnostic: { code: "CODEX/HISTORY_DIVERGED", uncertain: false },
  });
  expect(diverged.message).toContain("not part of the original Codex thread");
  expect(diverged.message).not.toContain("signed out");
});

test.each([
  "unavailable",
  "unsupported_input",
  "scope_changed",
  "protocol",
  "limit",
  "outcome_unknown",
  "history_uncertain",
  "history_diverged",
  "upstream",
  "timeout",
  "cancelled",
] as const)("Codex %s remains a finite native diagnostic", (fault) => {
  const view = faultView({ reason: "native_conversation", fault });
  expect(view.diagnostic?.code).toBe(`CODEX/${fault.toUpperCase()}`);
  expect(view.message).not.toContain("Zephium AI");
  expect(view.retry).toBe("no");
});

test("native proven not-sent is distinct from uncertain output and never auto-retries", () => {
  const view = faultView({ reason: "native_conversation", fault: "not_sent" });
  expect(view.message).toBe(
    "This message was not sent to Codex. You can continue the conversation.",
  );
  expect(view.retry).toBe("no");
  expect(view.diagnostic).toEqual({ code: "CODEX/NOT_SENT", uncertain: false });
  expect(
    faultView({ reason: "native_conversation", fault: "outcome_unknown" }).diagnostic?.uncertain,
  ).toBe(true);
});

test("a retired direct source is actionable without a retry, inferred send or billing fallback", () => {
  const view = faultView({ reason: "chat_gpt", fault: "retired" });
  expect(view.message).toContain("retired");
  expect(view.retry).toBe("no");
  expect(view.diagnostic).toEqual({ code: "CHATGPT/LEGACY/-/RETIRED", uncertain: false });
  expect(view.fix).not.toBeNull();
});

test("known Codex completion with interrupted browser activity retains a local-only warning", () => {
  expect(faultView({ reason: "native_conversation", fault: "local_interruption" })).toEqual({
    message:
      "Codex finished this reply, but browser activity was interrupted. Check the page before continuing.",
    fix: null,
    retry: "no",
    diagnostic: { code: "CODEX/LOCAL_INTERRUPTION", uncertain: false },
  });
});
