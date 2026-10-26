import type {
  GoQuotaViewV1,
  GoQuotaWindowV1,
  GoQuotaKindV1,
  GoQuotaStateV1,
} from "$shared/ipc/go-bindings";

const KINDS: readonly GoQuotaKindV1[] = ["rolling", "weekly", "monthly"];
const STATES: readonly GoQuotaStateV1[] = [
  "not_checked",
  "current",
  "stale",
  "unavailable",
  "no_entitlement",
];

export function quotaTime(value: unknown): number | null {
  if (typeof value !== "string" || !/^[0-9]{1,16}$/u.test(value)) return null;
  const milliseconds = Number(value);
  return Number.isSafeInteger(milliseconds) && !Number.isNaN(new Date(milliseconds).getTime())
    ? milliseconds
    : null;
}

function validWindow(value: GoQuotaWindowV1): boolean {
  return (
    !!value &&
    KINDS.includes(value.kind) &&
    typeof value.used_percent === "number" &&
    Number.isFinite(value.used_percent) &&
    value.used_percent >= 0 &&
    value.used_percent <= 100 &&
    typeof value.remaining_percent === "number" &&
    Number.isFinite(value.remaining_percent) &&
    value.remaining_percent >= 0 &&
    value.remaining_percent <= 100 &&
    Math.abs(value.used_percent + value.remaining_percent - 100) < 0.000001 &&
    quotaTime(value.resets_at_ms) !== null &&
    typeof value.limited === "boolean"
  );
}

/** Missing or malformed quota is unknown, never an empty allowance or a full one. */
export function quotaProjection(value: GoQuotaViewV1 | null | undefined): GoQuotaViewV1 {
  const unknown: GoQuotaViewV1 = {
    state: "unavailable",
    checking: false,
    checked_at_ms: null,
    attempted_at_ms: null,
    stale_at_ms: null,
    windows: [],
    error: "protocol",
  };
  if (
    !value ||
    !STATES.includes(value.state) ||
    typeof value.checking !== "boolean" ||
    !Array.isArray(value.windows) ||
    [value.checked_at_ms, value.attempted_at_ms, value.stale_at_ms].some(
      (time) => time !== null && quotaTime(time) === null,
    )
  )
    return unknown;
  if (value.error === "unauthorized" || value.error === "no_entitlement")
    return {
      ...value,
      state: value.error === "no_entitlement" ? "no_entitlement" : "unavailable",
      windows: [],
      checked_at_ms: null,
      stale_at_ms: null,
    };
  const snapshot = value.state === "current" || value.state === "stale";
  if (!snapshot) return { ...value, windows: [], checked_at_ms: null, stale_at_ms: null };
  if (
    quotaTime(value.checked_at_ms) === null ||
    quotaTime(value.stale_at_ms) === null ||
    value.windows.length !== KINDS.length ||
    !value.windows.every(validWindow) ||
    new Set(value.windows.map((window) => window.kind)).size !== KINDS.length
  )
    return unknown;
  return value;
}

/** Local aging changes presentation only. It never refreshes the provider or guesses a reset. */
export function quotaAt(value: GoQuotaViewV1, now: number): GoQuotaViewV1 {
  if (
    value.state === "current" &&
    (!Number.isFinite(now) ||
      now < Number(value.checked_at_ms) ||
      now >= Number(value.stale_at_ms) ||
      value.windows.some((window) => now >= Number(window.resets_at_ms)))
  )
    return { ...value, state: "stale" };
  return value;
}
