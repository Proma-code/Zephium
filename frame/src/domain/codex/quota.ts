import type {
  CodexQuotaBucketV1,
  CodexQuotaFaultV1,
  CodexQuotaPlanV1,
  CodexQuotaStateV1,
  CodexQuotaUnavailableV1,
  CodexQuotaViewV1,
  CodexQuotaWindowV1,
} from "$shared/ipc/codex-bindings";

const MAX_TIME = 8_640_000_000_000_000;
const MAX_EPOCH = 18_446_744_073_709_551_615n;
const states: readonly CodexQuotaStateV1[] = [
  "not_checked",
  "current",
  "stale",
  "unavailable",
  "error",
];
const faults: readonly CodexQuotaFaultV1[] = [
  "changed",
  "busy",
  "refresh_limited",
  "clock",
  "invalid_observation",
  "too_large",
  "transport",
  "timeout",
  "provider_refused",
];
const unavailable: readonly CodexQuotaUnavailableV1[] = [
  "not_connected",
  "signed_out",
  "not_reported",
];
const plans: readonly CodexQuotaPlanV1[] = [
  "free",
  "go",
  "plus",
  "pro",
  "team",
  "business",
  "enterprise",
  "education",
  "other",
];
const record = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const member = <T extends string>(value: unknown, values: readonly T[]): value is T =>
  typeof value === "string" && values.some((candidate) => candidate === value);
const percent = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 100;
const epoch = (value: unknown): value is string =>
  typeof value === "string" && /^[1-9][0-9]{0,19}$/u.test(value) && BigInt(value) <= MAX_EPOCH;

/** Decimal native timestamps are checked before conversion; no rounding or reset inference. */
export function codexQuotaTime(value: unknown): number | null {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]{0,15})$/u.test(value)) return null;
  const milliseconds = Number(value);
  return Number.isSafeInteger(milliseconds) && milliseconds <= MAX_TIME ? milliseconds : null;
}

function label(value: unknown): value is string | null {
  return (
    value === null ||
    (typeof value === "string" &&
      value.length <= 256 &&
      !/\p{Cc}/u.test(value) &&
      [...value].every((character) => {
        const point = character.codePointAt(0)!;
        return point < 0xd800 || point > 0xdfff;
      }) &&
      new TextEncoder().encode(value).length <= 256)
  );
}

function windowProjection(value: unknown): CodexQuotaWindowV1 | null {
  if (
    !record(value) ||
    (value.kind !== "primary" && value.kind !== "secondary") ||
    (value.duration_minutes !== null &&
      !(
        typeof value.duration_minutes === "number" &&
        Number.isSafeInteger(value.duration_minutes) &&
        value.duration_minutes > 0 &&
        value.duration_minutes <= MAX_TIME / 60_000
      )) ||
    (value.resets_at_ms !== null && codexQuotaTime(value.resets_at_ms) === null)
  )
    return null;
  const used = value.used_percent;
  const remaining = value.remaining_percent;
  if (
    used === null
      ? remaining !== null
      : !percent(used) || !percent(remaining) || Math.abs(100 - used - remaining) > 0.0000001
  )
    return null;
  return {
    kind: value.kind,
    duration_minutes: value.duration_minutes as number | null,
    used_percent: used as number | null,
    remaining_percent: remaining as number | null,
    resets_at_ms: value.resets_at_ms as string | null,
  };
}

function bucketProjection(value: unknown): CodexQuotaBucketV1 | null {
  if (
    !record(value) ||
    (value.id !== null &&
      !(typeof value.id === "string" && /^[A-Za-z0-9_.-]{1,64}$/u.test(value.id))) ||
    !label(value.name) ||
    (value.plan !== null && !member(value.plan, plans)) ||
    !Array.isArray(value.windows) ||
    value.windows.length > 2
  )
    return null;
  const windows: CodexQuotaWindowV1[] = [];
  for (const item of value.windows) {
    const window = windowProjection(item);
    if (!window || windows.some((prior) => prior.kind === window.kind)) return null;
    windows.push(window);
  }
  return {
    id: value.id as string | null,
    name: value.name,
    plan: value.plan as CodexQuotaPlanV1 | null,
    windows,
  };
}

function invalidObservation(): CodexQuotaViewV1 {
  return {
    source: "original_app_server",
    state: "unavailable",
    account_epoch: null,
    checking: false,
    refresh_needed: false,
    observed_at_ms: null,
    attempted_at_ms: null,
    buckets: [],
    unavailable: null,
    error: "invalid_observation",
  };
}

/** Quota-only normalization: a bad observation never changes account, catalog or send authority. */
export function codexQuotaProjection(value: unknown): CodexQuotaViewV1 {
  if (
    !record(value) ||
    value.source !== "original_app_server" ||
    !member(value.state, states) ||
    (value.account_epoch !== null && !epoch(value.account_epoch)) ||
    typeof value.checking !== "boolean" ||
    typeof value.refresh_needed !== "boolean" ||
    (value.observed_at_ms !== null && codexQuotaTime(value.observed_at_ms) === null) ||
    (value.attempted_at_ms !== null && codexQuotaTime(value.attempted_at_ms) === null) ||
    (value.unavailable !== null && !member(value.unavailable, unavailable)) ||
    (value.error !== null && !member(value.error, faults)) ||
    !Array.isArray(value.buckets) ||
    value.buckets.length > 32
  )
    return invalidObservation();
  const reading = value.state === "current" || value.state === "stale";
  if (
    reading &&
    (!epoch(value.account_epoch) ||
      codexQuotaTime(value.observed_at_ms) === null ||
      value.unavailable !== null ||
      (value.state === "current" && value.error !== null))
  ) {
    return invalidObservation();
  }
  const buckets: CodexQuotaBucketV1[] = [];
  if (reading) {
    for (const item of value.buckets) {
      const bucket = bucketProjection(item);
      if (!bucket || buckets.some((prior) => prior.id === bucket.id)) return invalidObservation();
      buckets.push(bucket);
    }
  }
  if (reading && !buckets.some((bucket) => bucket.windows.length > 0)) return invalidObservation();
  // Explicit fields only: raw provider extras, credits and account identifiers never propagate.
  return {
    source: "original_app_server",
    state: value.state,
    account_epoch: value.account_epoch as string | null,
    checking: value.checking,
    refresh_needed: value.refresh_needed,
    observed_at_ms: reading ? (value.observed_at_ms as string) : null,
    attempted_at_ms: value.attempted_at_ms as string | null,
    buckets,
    unavailable: value.unavailable as CodexQuotaUnavailableV1 | null,
    error: value.error as CodexQuotaFaultV1 | null,
  };
}

/** Presentation-only aging under the caller's native freshness policy; no timers, reads or refill. */
export function codexQuotaAt(
  value: CodexQuotaViewV1,
  now: number,
  maxAgeMs: number,
): CodexQuotaViewV1 {
  if (value.state !== "current") return value;
  const observed = codexQuotaTime(value.observed_at_ms);
  if (
    observed === null ||
    !Number.isSafeInteger(now) ||
    now < observed ||
    now > MAX_TIME ||
    !Number.isSafeInteger(maxAgeMs) ||
    maxAgeMs < 0 ||
    now - observed >= maxAgeMs ||
    value.buckets.some((bucket) =>
      bucket.windows.some((window) => {
        const reset = codexQuotaTime(window.resets_at_ms);
        return reset !== null && now >= reset;
      }),
    )
  )
    return { ...value, state: "stale" };
  return value;
}

/** Null is unknown; reported zero remains visibly zero. Labels and explanatory copy stay in UI. */
export function codexQuotaPercent(value: number | null): string | null {
  return percent(value) ? `${value}%` : null;
}

export function codexQuotaDate(value: string | null): string | null {
  const time = codexQuotaTime(value);
  return time === null ? null : new Date(time).toLocaleString();
}

/** Exact duration presentation only; never infer a plan's daily/weekly limits. */
export function codexQuotaDuration(
  value: number | null,
): { unit: "minute" | "hour" | "day"; count: number } | null {
  if (value === null || !Number.isSafeInteger(value) || value <= 0 || value > MAX_TIME / 60_000)
    return null;
  if (value % 1440 === 0) return { unit: "day", count: value / 1440 };
  if (value % 60 === 0) return { unit: "hour", count: value / 60 };
  return { unit: "minute", count: value };
}
