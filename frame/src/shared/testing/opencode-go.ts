import type { GoViewV1, GoQuotaViewV1, commands } from "../ipc/go-bindings";
type Commands = typeof commands;
export function goQuota(changes: Partial<GoQuotaViewV1> = {}): GoQuotaViewV1 {
  return {
    state: "not_checked",
    checking: false,
    checked_at_ms: null,
    attempted_at_ms: null,
    stale_at_ms: null,
    windows: [],
    error: null,
    ...changes,
  };
}
/** Synthetic complete provider reading; no real account or network access. */
export function goQuotaReading(
  now = Date.now(),
  changes: Partial<GoQuotaViewV1> = {},
): GoQuotaViewV1 {
  return goQuota({
    state: "current",
    checked_at_ms: String(now),
    attempted_at_ms: String(now),
    stale_at_ms: String(now + 300000),
    windows: ["rolling", "weekly", "monthly"].map((kind, index) => ({
      kind: kind as "rolling" | "weekly" | "monthly",
      used_percent: 20 + index * 10,
      remaining_percent: 80 - index * 10,
      resets_at_ms: String(now + (index + 1) * 3600000),
      limited: false,
    })),
    ...changes,
  });
}
export function goView(profile: string, changes: Partial<GoViewV1> = {}): GoViewV1 {
  return {
    version: 1,
    profile,
    state: "unconfigured",
    connection_id: null,
    models: 0,
    checking: false,
    catalog_checked_at_ms: null,
    last_billing: null,
    quota: goQuota(),
    error: null,
    ...changes,
  };
}
/** Strict synthetic IPC: never invokes the provider or a real credential store. */
export function mockGoBindings(overrides: Partial<Commands> = {}): { commands: Commands } {
  return {
    commands: new Proxy(overrides as Commands, {
      get(target, key, receiver) {
        if (typeof key === "symbol" || key === "then") return undefined;
        if (!Object.hasOwn(target, key)) throw new Error(`Unmocked OpenCode Go command: ${key}`);
        return Reflect.get(target, key, receiver);
      },
    }),
  };
}
