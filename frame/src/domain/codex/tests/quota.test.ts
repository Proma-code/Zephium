import { expect, test } from "vitest";
import type { CodexQuotaViewV1, CodexQuotaWindowV1 } from "$shared/ipc/codex-bindings";
import {
  codexQuotaAt,
  codexQuotaDate,
  codexQuotaDuration,
  codexQuotaPercent,
  codexQuotaProjection,
  codexQuotaTime,
} from "../quota";

const now = 1_700_000_000_000;
const window = (): CodexQuotaWindowV1 => ({
  kind: "primary",
  duration_minutes: 300,
  used_percent: 20,
  remaining_percent: 80,
  resets_at_ms: String(now + 600_000),
});
const reading = (): CodexQuotaViewV1 => ({
  source: "original_app_server",
  state: "current",
  account_epoch: "18446744073709551615",
  checking: false,
  refresh_needed: false,
  observed_at_ms: String(now),
  attempted_at_ms: String(now),
  buckets: [{ id: "codex", name: "Codex", plan: "plus", windows: [window()] }],
  unavailable: null,
  error: null,
});
const withWindow = (change: Record<string, unknown>) => {
  const value = reading();
  return { ...value, buckets: [{ ...value.buckets[0]!, windows: [{ ...window(), ...change }] }] };
};

test("original buckets remain independent with exact account epoch and reported metadata", () => {
  const value = reading();
  value.buckets.push({
    id: "review",
    name: "Review",
    plan: null,
    windows: [
      {
        kind: "secondary",
        duration_minutes: 10_080,
        used_percent: 90,
        remaining_percent: 10,
        resets_at_ms: null,
      },
    ],
  });
  expect(codexQuotaProjection(value)).toEqual(value);
  expect(codexQuotaProjection(value).account_epoch).toBe("18446744073709551615");
});

test("unknown window measurements are retained without guessing a plan duration", () => {
  const value = withWindow({
    duration_minutes: null,
    used_percent: null,
    remaining_percent: null,
    resets_at_ms: null,
  });
  expect(codexQuotaProjection(value)).toEqual(value);
  expect(codexQuotaPercent(null)).toBeNull();
  expect(codexQuotaDate(null)).toBeNull();
});

test("reported zero and full remaining never become unknown", () => {
  for (const used of [0, 100]) {
    const value = withWindow({ used_percent: used, remaining_percent: 100 - used });
    expect(codexQuotaProjection(value)).toEqual(value);
    expect(codexQuotaPercent(used)).toBe(`${used}%`);
  }
  expect(codexQuotaPercent(25.5)).toBe("25.5%");
});

test("native source and finite malformed fields fail only the quota projection", () => {
  for (const change of [
    { source: "direct_chatgpt" },
    { state: "entitled" },
    { checking: 1 },
    { refresh_needed: null },
    { account_epoch: "0" },
    { account_epoch: "18446744073709551616" },
    { account_epoch: "01" },
    { account_epoch: 1 },
    { account_epoch: null },
    { observed_at_ms: null },
    { attempted_at_ms: "1e3" },
    { error: "raw-provider-message" },
    { unavailable: "unknown" },
    { buckets: [] },
    { buckets: null },
  ]) {
    const account = {
      state: "connected",
      models: ["original"],
      quota: { ...reading(), ...change },
    };
    expect(codexQuotaProjection(account.quota)).toMatchObject({
      state: "unavailable",
      error: "invalid_observation",
      buckets: [],
      account_epoch: null,
    });
    expect(account.state).toBe("connected");
    expect(account.models).toEqual(["original"]);
  }
  for (const value of [undefined, null, [], {}, "quota"])
    expect(codexQuotaProjection(value).error).toBe("invalid_observation");
});

test("out-of-contract percentages are not clamped into an apparent balance", () => {
  for (const change of [
    { used_percent: -1 },
    { used_percent: 101 },
    { used_percent: Infinity },
    { used_percent: NaN },
    { remaining_percent: 101 },
    { remaining_percent: null },
    { used_percent: null },
    { remaining_percent: 50 },
    { used_percent: "20" },
  ])
    expect(codexQuotaProjection(withWindow(change)).error).toBe("invalid_observation");
  for (const value of [-1, 101, NaN, Infinity]) expect(codexQuotaPercent(value)).toBeNull();
});

test("window kinds, durations, resets and duplicate positions are bounded", () => {
  for (const change of [
    { kind: "five_hour" },
    { duration_minutes: 0 },
    { duration_minutes: -1 },
    { duration_minutes: 1.5 },
    { duration_minutes: 144_000_000_001 },
    { resets_at_ms: "8640000000000001" },
    { resets_at_ms: 1000 },
  ])
    expect(codexQuotaProjection(withWindow(change)).error).toBe("invalid_observation");
  expect(codexQuotaProjection(withWindow({ duration_minutes: 144_000_000_000 })).state).toBe(
    "current",
  );
  const value = reading();
  value.buckets[0]!.windows.push(window());
  expect(codexQuotaProjection(value).error).toBe("invalid_observation");
});

test("bucket IDs remain machine metadata and optional labels preserve null versus empty", () => {
  for (const name of [
    null,
    "",
    "حد الاستخدام",
    "x".repeat(256),
    "🙂".repeat(64),
    "<b>plain text</b>",
  ]) {
    const value = reading();
    value.buckets[0]!.name = name;
    expect(codexQuotaProjection(value).buckets[0]?.name).toBe(name);
  }
  for (const change of [
    { id: "" },
    { id: "a/b" },
    { id: "x".repeat(65) },
    { name: "x".repeat(257) },
    { name: "🙂".repeat(65) },
    { name: "bad\nlabel" },
    { name: "\ud800" },
    { name: 1 },
    { plan: "raw plan name" },
  ]) {
    const value = reading();
    expect(
      codexQuotaProjection({ ...value, buckets: [{ ...value.buckets[0]!, ...change }] }).error,
    ).toBe("invalid_observation");
  }
});

test("duplicate and excessive buckets fail; no cross-bucket sum is synthesized", () => {
  const value = reading();
  value.buckets.push({ ...value.buckets[0]! });
  expect(codexQuotaProjection(value).error).toBe("invalid_observation");
  value.buckets = Array.from({ length: 33 }, (_, i) => ({ ...value.buckets[0]!, id: String(i) }));
  expect(codexQuotaProjection(value).error).toBe("invalid_observation");
  value.buckets.pop();
  expect(codexQuotaProjection(value).buckets).toHaveLength(32);
});

test("harmless additive metadata is ignored and never forwarded as credentials or credits", () => {
  const value = reading();
  const extra = {
    ...value,
    account_id: "private",
    credits: { balance: 999 },
    access_token: "private",
    future: "description",
    buckets: [
      {
        ...value.buckets[0]!,
        account_email: "private",
        windows: [{ ...window(), entitlement: true }],
      },
    ],
  };
  expect(codexQuotaProjection(extra)).toEqual(value);
  expect(JSON.stringify(codexQuotaProjection(extra))).not.toContain("private");
});

test("initial, unavailable and error views never retain leftover account readings", () => {
  for (const state of ["not_checked", "unavailable", "error"] as const) {
    const value = { ...reading(), state };
    const projected = codexQuotaProjection(value);
    expect(projected.state).toBe(state);
    expect(projected.buckets).toEqual([]);
    expect(projected.observed_at_ms).toBeNull();
  }
});

test("transient faults remain quota-local and preserve visibly stale last-good data", () => {
  const value = {
    ...reading(),
    state: "stale",
    error: "timeout",
    checking: true,
    refresh_needed: true,
    attempted_at_ms: String(now + 1000),
  };
  expect(codexQuotaProjection(value)).toEqual(value);
  expect(codexQuotaProjection(value).observed_at_ms).toBe(String(now));
});

test("elapsed freshness and reset only mark stale; no local refill or timestamp refresh", () => {
  const value = reading();
  expect(codexQuotaAt(value, now + 299_999, 300_000)).toEqual(value);
  expect(codexQuotaAt(value, now + 300_000, 300_000)).toEqual({ ...value, state: "stale" });
  value.buckets[0]!.windows[0]!.resets_at_ms = String(now + 100);
  const stale = codexQuotaAt(value, now + 100, 300_000);
  expect(stale.buckets[0]!.windows[0]!.remaining_percent).toBe(80);
  expect(stale.observed_at_ms).toBe(String(now));
  expect(stale.state).toBe("stale");
  expect(codexQuotaAt(stale, now + 1_000_000, 300_000)).toEqual(stale);
});

test("invalid clocks or freshness bounds cannot present an observation as current", () => {
  const value = reading();
  for (const clock of [now - 1, NaN, Infinity, -Infinity, 8_640_000_000_000_001])
    expect(codexQuotaAt(value, clock, 300_000)).toEqual({ ...value, state: "stale" });
  for (const age of [-1, NaN, Infinity, 0.5])
    expect(codexQuotaAt(value, now, age).state).toBe("stale");
});

test("decimal timestamps preserve zero and reject rounding, exponents and invalid dates", () => {
  expect(codexQuotaTime("0")).toBe(0);
  expect(codexQuotaTime("8640000000000000")).toBe(8_640_000_000_000_000);
  expect(codexQuotaDate(String(now))).toBe(new Date(now).toLocaleString());
  for (const time of [
    null,
    "",
    "00",
    "01",
    "-1",
    "1.1",
    "1e3",
    "NaN",
    "9007199254740993",
    "8640000000000001",
  ])
    expect(codexQuotaTime(time)).toBeNull();
});

test("reported durations use exact whole units without plan assumptions", () => {
  for (const [minutes, unit, count] of [
    [300, "hour", 5],
    [10080, "day", 7],
    [60, "hour", 1],
    [1440, "day", 1],
    [90, "minute", 90],
    [301, "minute", 301],
    [1, "minute", 1],
  ] as const) {
    expect(codexQuotaDuration(minutes)).toEqual({ unit, count });
  }
  for (const value of [null, 0, -1, 1.5, NaN, Infinity, 144000000001])
    expect(codexQuotaDuration(value)).toBeNull();
});

test("reported bucket plans remain independent and absent plans are not guessed from windows", () => {
  const value = reading();
  value.buckets = [
    { ...value.buckets[0]!, id: "first", plan: "plus" },
    { ...value.buckets[0]!, id: "second", plan: "business" },
    { ...value.buckets[0]!, id: "unknown", plan: null },
  ];
  expect(codexQuotaProjection(value).buckets.map((bucket) => bucket.plan)).toEqual([
    "plus",
    "business",
    null,
  ]);
});
