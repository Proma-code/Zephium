import { expect, test } from "vitest";
import { goQuota, goQuotaReading } from "$shared/testing/opencode-go";
import type { GoQuotaViewV1 } from "$shared/ipc/go-bindings";
import { quotaAt, quotaProjection, quotaTime } from "../quota";
import {
  goQuotaFault,
  goQuotaRemaining,
  goQuotaStatus,
  goQuotaTime,
  goQuotaWindow,
} from "../presentation";
const now = 1700000000000;

test("only complete valid provider windows produce remaining percentages", () => {
  const reading = goQuotaReading(now);
  expect(quotaProjection(reading)).toEqual(reading);
  expect(goQuotaWindow("rolling")).toBe("Rolling");
  expect(goQuotaRemaining(reading.windows[0]!.remaining_percent)).toBe("80% remaining");
  expect(goQuotaTime(reading.checked_at_ms)).toBe(new Date(now).toLocaleString());
  expect(goQuotaStatus(reading)).toBe("Provider-reported remaining quota");
});

test("valid zero and full remaining are not confused with absent readings", () => {
  const reading = goQuotaReading(now);
  reading.windows[0] = { ...reading.windows[0]!, used_percent: 100, remaining_percent: 0 };
  reading.windows[1] = { ...reading.windows[1]!, used_percent: 0, remaining_percent: 100 };
  expect(quotaProjection(reading).windows.map((window) => window.remaining_percent)).toEqual([
    0, 100, 60,
  ]);
  expect(quotaProjection(undefined).windows).toEqual([]);
  expect(quotaProjection(goQuota()).windows).toEqual([]);
});

test("missing, duplicate, bad percent, bad reset, and inconsistent windows remain unknown", () => {
  const reading = goQuotaReading(now);
  const malformed: unknown[] = [
    undefined,
    null,
    {},
    { ...reading, checked_at_ms: null },
    { ...reading, stale_at_ms: "NaN" },
    { ...reading, windows: [] },
    { ...reading, windows: reading.windows.slice(0, 2) },
    { ...reading, windows: [reading.windows[0]!, reading.windows[0]!, reading.windows[2]!] },
  ];
  for (const change of [
    { used_percent: null },
    { remaining_percent: null },
    { used_percent: NaN },
    { remaining_percent: -1 },
    { used_percent: 101 },
    { remaining_percent: 50 },
    { resets_at_ms: "not-a-time" },
    { kind: "daily" },
    { limited: undefined },
  ])
    malformed.push({
      ...reading,
      windows: [{ ...reading.windows[0]!, ...change }, ...reading.windows.slice(1)],
    });
  for (const value of malformed) {
    const projected = quotaProjection(value as GoQuotaViewV1);
    expect(projected.state).toBe("unavailable");
    expect(projected.windows).toEqual([]);
    expect(projected.error).toBe("protocol");
  }
});

test("refused, unavailable and not-checked states never display leftover numbers", () => {
  for (const state of ["no_entitlement", "unavailable", "not_checked"] as const) {
    const projected = quotaProjection(goQuotaReading(now, { state }));
    expect(projected.windows).toEqual([]);
    expect(projected.checked_at_ms).toBeNull();
    expect(projected.stale_at_ms).toBeNull();
  }
});

test("fresh snapshots expire at native stale time or a reported reset without resetting allowance", () => {
  const reading = goQuotaReading(now);
  expect(quotaAt(reading, now + 299999).state).toBe("current");
  expect(quotaAt(reading, now + 300000)).toEqual({ ...reading, state: "stale" });
  reading.windows[0]!.resets_at_ms = String(now - 1);
  expect(quotaAt(reading, now).state).toBe("stale");
  expect(quotaAt(reading, now).windows[0]!.remaining_percent).toBe(80);
});

test("limited is independent of rounded percentages and is never inferred from them", () => {
  const reading = goQuotaReading(now);
  reading.windows[0] = {
    ...reading.windows[0]!,
    used_percent: 99,
    remaining_percent: 1,
    limited: true,
  };
  expect(quotaProjection(reading).windows[0]!).toMatchObject({
    remaining_percent: 1,
    limited: true,
  });
  expect(goQuotaStatus({ ...reading, state: "stale" })).toContain("Stale");
  expect(goQuotaFault("no_entitlement")).toContain("no Go entitlement");
  expect(goQuotaFault("forbidden")).toContain("could not be confirmed");
  expect(goQuotaFault("unauthorized")).toContain("Previous quota was cleared");
});

test("only bounded decimal epoch timestamps are formatted", () => {
  for (const value of [null, "", "-1", "NaN", "1e3", "9999999999999999"]) {
    expect(quotaTime(value)).toBeNull();
    expect(goQuotaTime(value)).toBeNull();
  }
});

test("explicit credential or entitlement refusal cannot display an inconsistent leftover snapshot", () => {
  for (const error of ["unauthorized", "no_entitlement"] as const) {
    const projected = quotaProjection(goQuotaReading(now, { error }));
    expect(projected.windows).toEqual([]);
    expect(projected.checked_at_ms).toBeNull();
    expect(projected.error).toBe(error);
  }
});

test("clock rollback or an invalid local clock cannot present a reading as current", () => {
  const reading = goQuotaReading(now);
  for (const clock of [now - 1, NaN, Infinity, -Infinity]) {
    expect(quotaAt(reading, clock).state).toBe("stale");
    expect(quotaAt(reading, clock).windows).toEqual(reading.windows);
  }
});
