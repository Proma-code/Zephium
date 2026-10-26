import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { GoViewV1 } from "$shared/ipc/go-bindings";
import { goView, goQuota, goQuotaReading } from "$shared/testing/opencode-go";
import { GoSession } from "../session.svelte";

const native = vi.hoisted(() => ({
  read: vi.fn(),
  save: vi.fn(),
  check: vi.fn(),
  quota: vi.fn(),
  disconnect: vi.fn(),
  changed: null as (() => void) | null,
}));
vi.mock("$shared/ipc/go-bindings", async () => {
  const { mockGoBindings } = await import("$shared/testing/opencode-go");
  return mockGoBindings({
    goStatus: native.read,
    goSave: native.save,
    goCheck: native.check,
    goQuota: native.quota,
    goDisconnect: native.disconnect,
  });
});
vi.mock("$shared/ipc/native-events", () => ({
  events: {
    workModelsChanged: {
      listen: (listener: () => void) => {
        native.changed = listener;
        return Promise.resolve(() => {
          native.changed = null;
        });
      },
    },
  },
}));
const profile = "00000000000000000000000001";
const connection = "a".repeat(32);
const configured = (changes: Partial<GoViewV1> = {}) =>
  goView(profile, {
    state: "configured",
    connection_id: connection,
    ...changes,
  });
const sessions: GoSession[] = [];
const owner = () => {
  const session = new GoSession(profile);
  sessions.push(session);
  return session;
};
const flush = () => vi.advanceTimersByTimeAsync(0);
beforeEach(() => {
  vi.useFakeTimers();
  for (const command of [native.read, native.save, native.check, native.quota, native.disconnect])
    command.mockReset();
  native.read.mockResolvedValue(goView(profile));
  native.save.mockResolvedValue(configured());
  native.check.mockResolvedValue(
    configured({ state: "catalogued", models: 2, catalog_checked_at_ms: "1700000000000" }),
  );
  native.disconnect.mockResolvedValue(goView(profile));
});
afterEach(async () => {
  for (const session of sessions.splice(0)) session.dispose();
  await Promise.resolve();
  vi.useRealTimers();
});

test("mount reads configuration only, never saves, discovers, or checks models", async () => {
  const session = owner();
  await session.start();
  await session.start();
  expect(native.read).toHaveBeenCalledExactlyOnceWith(profile);
  expect(session.view?.state).toBe("unconfigured");
  expect(native.save).not.toHaveBeenCalled();
  expect(native.check).not.toHaveBeenCalled();
  expect(native.quota).not.toHaveBeenCalled();
  expect(native.disconnect).not.toHaveBeenCalled();
});

test("saving remains configured until a separate explicit check and uses connection CAS", async () => {
  const session = owner();
  await session.start();
  expect(await session.save(null, "go-key", true)).toBe(true);
  expect(native.save).toHaveBeenCalledExactlyOnceWith(profile, null, "go-key", true);
  expect(session.view?.state).toBe("configured");
  expect(native.check).not.toHaveBeenCalled();
  expect(native.quota).not.toHaveBeenCalled();
  await session.check();
  expect(native.check).toHaveBeenCalledExactlyOnceWith(profile, connection);
  expect(session.view?.state).toBe("catalogued");
  expect(session.view?.models).toBe(2);
  await session.disconnect();
  expect(native.disconnect).toHaveBeenCalledExactlyOnceWith(profile, connection);
  expect(session.view?.state).toBe("unconfigured");
});

test("unknown or unavailable status cannot admit mutations", async () => {
  const session = owner();
  expect(await session.save(null, "go-key", true)).toBe(false);
  expect(await session.check()).toBe(false);
  expect(await session.disconnect()).toBe(false);
  native.read.mockResolvedValue(goView(profile, { state: "unavailable" }));
  await session.start();
  expect(await session.save(null, "go-key", true)).toBe(false);
  expect(native.save).not.toHaveBeenCalled();
  expect(native.check).not.toHaveBeenCalled();
  expect(native.quota).not.toHaveBeenCalled();
  expect(native.disconnect).not.toHaveBeenCalled();
});

test("profile mismatch and malformed catalogs are not adopted", async () => {
  for (const bad of [
    configured({ profile: "another" }),
    configured({ version: 2 }),
    configured({ connection_id: "x" }),
    configured({ models: -1 }),
  ]) {
    native.read.mockResolvedValue(bad);
    const session = owner();
    await session.start();
    expect(session.view).toBeNull();
    expect(session.observationFailed).toBe(true);
    expect(await session.save(null, "go-key", true)).toBe(false);
  }
  expect(native.save).not.toHaveBeenCalled();
});

test("a draft made for an older connection cannot silently replace its successor", async () => {
  native.read.mockResolvedValue(configured());
  const session = owner();
  await session.start();
  const draft = session.view!.connection_id;
  native.read.mockResolvedValue(configured({ connection_id: "b".repeat(32) }));
  native.changed?.();
  await flush();
  expect(await session.save(draft, "replacement-key", true)).toBe(false);
  expect(native.save).not.toHaveBeenCalled();
  await session.disconnect();
  expect(native.disconnect).toHaveBeenCalledExactlyOnceWith(profile, "b".repeat(32));
});

test("native CAS refusal is explicit and retains authoritative current connection", async () => {
  native.read.mockResolvedValue(configured());
  native.save.mockResolvedValue(configured({ connection_id: "b".repeat(32), error: "changed" }));
  const session = owner();
  await session.start();
  expect(await session.save(connection, "replacement-key", true)).toBe(false);
  expect(session.view?.error).toBe("changed");
  expect(session.view?.connection_id).toBe("b".repeat(32));
});

test("single-flight check does not retry or start another mutation", async () => {
  native.read.mockResolvedValue(configured());
  let finish!: (view: GoViewV1) => void;
  native.check.mockImplementation(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const session = owner();
  await session.start();
  const checking = session.check();
  await Promise.resolve();
  expect(await session.check()).toBe(false);
  expect(await session.disconnect()).toBe(false);
  expect(await session.save(connection, "go-key", true)).toBe(false);
  finish(configured({ error: "unauthorized" }));
  expect(await checking).toBe(false);
  expect(session.view?.error).toBe("unauthorized");
  expect(native.check).toHaveBeenCalledTimes(1);
});

test("a timed-out observation is inconclusive, never a successful save or automatic retry", async () => {
  native.save.mockImplementation(() => new Promise<GoViewV1>(() => {}));
  const session = owner();
  await session.start();
  const saving = session.save(null, "go-key", true);
  await vi.advanceTimersByTimeAsync(20000);
  expect(await saving).toBe(false);
  expect(session.observationFailed).toBe(true);
  expect(session.view?.state).toBe("unconfigured");
  native.read.mockResolvedValue(configured());
  await session.refresh();
  expect(session.view?.state).toBe("configured");
  expect(native.save).toHaveBeenCalledTimes(1);
  expect(native.check).not.toHaveBeenCalled();
  expect(native.quota).not.toHaveBeenCalled();
});

test("dispose ignores late saves without pretending to cancel native ownership", async () => {
  let finish!: (view: GoViewV1) => void;
  native.save.mockImplementation(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const session = owner();
  await session.start();
  const saving = session.save(null, "go-key", true);
  await Promise.resolve();
  session.dispose();
  finish(configured());
  expect(await saving).toBe(false);
  expect(session.view).toBeNull();
  expect(native.disconnect).not.toHaveBeenCalled();
  expect(native.changed).toBeNull();
});

test("stale status cannot restore a disconnected connection", async () => {
  native.read.mockResolvedValueOnce(configured());
  let finish!: (view: GoViewV1) => void;
  const session = owner();
  await session.start();
  native.read.mockImplementationOnce(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const read = session.refresh();
  await Promise.resolve();
  await session.disconnect();
  finish(configured());
  await read;
  expect(session.view?.state).toBe("unconfigured");
});

test("events during an admitted operation coalesce into one readback", async () => {
  native.read.mockResolvedValue(configured());
  let finish!: (view: GoViewV1) => void;
  native.check.mockImplementation(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const session = owner();
  await session.start();
  const checking = session.check();
  await Promise.resolve();
  native.changed?.();
  native.changed?.();
  expect(native.read).toHaveBeenCalledTimes(1);
  const checked = configured({ state: "catalogued", models: 2 });
  native.read.mockResolvedValue(checked);
  finish(checked);
  await checking;
  await flush();
  expect(native.read).toHaveBeenCalledTimes(2);
  expect(session.view?.state).toBe("catalogued");
});

test("a key alone is never enough: each save needs explicit billing acknowledgement", async () => {
  const session = owner();
  await session.start();
  expect(await session.save(null, "go-key", false)).toBe(false);
  expect(native.save).not.toHaveBeenCalled();
  expect(await session.save(null, "go-key", true)).toBe(true);
  expect(native.save).toHaveBeenCalledExactlyOnceWith(profile, null, "go-key", true);
});

test("public cataloguing does not create any last billing observation or authenticated plan", async () => {
  native.read.mockResolvedValue(configured());
  const session = owner();
  await session.start();
  await session.check();
  expect(session.view?.state).toBe("catalogued");
  expect(session.view?.last_billing).toBeNull();
  expect(native.save).not.toHaveBeenCalled();
});

test("only a native request report adds billing source and disconnect clears its observation", async () => {
  native.read.mockResolvedValue(
    configured({ last_billing: { source: "balance", observed_at_ms: "1700000000000" } }),
  );
  const session = owner();
  await session.start();
  expect(session.view?.last_billing?.source).toBe("balance");
  await session.disconnect();
  expect(session.view?.last_billing).toBeNull();
});

test("quota refresh is explicit, single-flight, and bound to this exact saved source", async () => {
  native.read.mockResolvedValue(configured());
  let finish!: (view: GoViewV1) => void;
  native.quota.mockImplementation(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const session = owner();
  await session.start();
  expect(native.quota).not.toHaveBeenCalled();
  const refreshing = session.refreshQuota();
  await Promise.resolve();
  expect(await session.refreshQuota()).toBe(false);
  expect(await session.check()).toBe(false);
  expect(await session.disconnect()).toBe(false);
  finish(configured({ quota: goQuotaReading() }));
  expect(await refreshing).toBe(true);
  expect(native.quota).toHaveBeenCalledExactlyOnceWith(profile, connection);
  expect(session.quota.state).toBe("current");
});

test("transient quota failure preserves only the native same-source stale reading and original checked time", async () => {
  const reading = goQuotaReading();
  native.read.mockResolvedValue(configured({ quota: reading }));
  native.quota.mockResolvedValue(
    configured({
      quota: {
        ...reading,
        state: "stale",
        attempted_at_ms: String(Date.now() + 1000),
        error: "timeout",
      },
    }),
  );
  const session = owner();
  await session.start();
  expect(await session.refreshQuota()).toBe(false);
  expect(session.view?.error).toBeNull();
  expect(session.quota.state).toBe("stale");
  expect(session.quota.checked_at_ms).toBe(reading.checked_at_ms);
  expect(session.quota.windows).toEqual(reading.windows);
});

test.each(["unauthorized", "no_entitlement"] as const)(
  "explicit %s clears previous quota without deleting the key",
  async (error) => {
    native.read.mockResolvedValue(configured({ quota: goQuotaReading() }));
    native.quota.mockResolvedValue(
      configured({
        quota: goQuota({
          state: error === "no_entitlement" ? "no_entitlement" : "unavailable",
          error,
        }),
      }),
    );
    const session = owner();
    await session.start();
    expect(await session.refreshQuota()).toBe(false);
    expect(session.quota.windows).toEqual([]);
    expect(session.view?.connection_id).toBe(connection);
    expect(native.disconnect).not.toHaveBeenCalled();
  },
);

test("local expiry marks quota stale with no automatic HTTP or catalog operation", async () => {
  native.read.mockResolvedValue(configured({ quota: goQuotaReading() }));
  const session = owner();
  await session.start();
  await vi.advanceTimersByTimeAsync(299999);
  expect(session.quota.state).toBe("current");
  await vi.advanceTimersByTimeAsync(1);
  expect(session.quota.state).toBe("stale");
  expect(native.quota).not.toHaveBeenCalled();
  expect(native.read).toHaveBeenCalledTimes(1);
  expect(native.check).not.toHaveBeenCalled();
  session.dispose();
  expect(vi.getTimerCount()).toBe(0);
});

test("source replacement and disconnect clear snapshots instead of carrying allowance to a new key", async () => {
  native.read.mockResolvedValue(configured({ quota: goQuotaReading() }));
  native.save.mockResolvedValue(configured({ connection_id: "b".repeat(32) }));
  const session = owner();
  await session.start();
  await session.save(connection, "replacement", true);
  expect(session.quota.state).toBe("not_checked");
  expect(session.quota.windows).toEqual([]);
  await session.disconnect();
  expect(session.quota.windows).toEqual([]);
  expect(native.quota).not.toHaveBeenCalled();
});

test("disposed quota responses cannot restore prior-profile state and release observation timers", async () => {
  native.read.mockResolvedValue(configured());
  let finish!: (view: GoViewV1) => void;
  native.quota.mockImplementation(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const session = owner();
  await session.start();
  const refreshing = session.refreshQuota();
  await Promise.resolve();
  session.dispose();
  finish(configured({ quota: goQuotaReading() }));
  expect(await refreshing).toBe(false);
  expect(session.view).toBeNull();
  expect(session.quota.windows).toEqual([]);
  expect(vi.getTimerCount()).toBe(0);
});

test("malformed quota becomes unknown without disabling independent key management", async () => {
  native.read.mockResolvedValue(configured({ quota: undefined }));
  const session = owner();
  await session.start();
  expect(session.observationFailed).toBe(false);
  expect(session.quota.state).toBe("unavailable");
  expect(session.quota.windows).toEqual([]);
  await session.disconnect();
  expect(native.disconnect).toHaveBeenCalledExactlyOnceWith(profile, connection);
});

test("unconfirmed quota observation marks old numbers stale and never retries automatically", async () => {
  native.read.mockResolvedValue(configured({ quota: goQuotaReading() }));
  native.quota.mockImplementation(() => new Promise<GoViewV1>(() => {}));
  const session = owner();
  await session.start();
  const refreshing = session.refreshQuota();
  await vi.advanceTimersByTimeAsync(20000);
  expect(await refreshing).toBe(false);
  expect(session.observationFailed).toBe(true);
  expect(session.quota.state).toBe("stale");
  expect(session.quota.windows[0]!.remaining_percent).toBe(80);
  expect(await session.refreshQuota()).toBe(false);
  expect(native.quota).toHaveBeenCalledTimes(1);
});

test("old status carrying quota cannot restore a replaced source even when the read ignores cancellation", async () => {
  native.read.mockResolvedValueOnce(configured({ quota: goQuotaReading() }));
  const session = owner();
  await session.start();
  let finish!: (view: GoViewV1) => void;
  native.read.mockImplementationOnce(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const reading = session.refresh();
  await Promise.resolve();
  native.save.mockResolvedValue(configured({ connection_id: "b".repeat(32) }));
  await session.save(connection, "replacement", true);
  finish(configured({ quota: goQuotaReading() }));
  await reading;
  expect(session.view?.connection_id).toBe("b".repeat(32));
  expect(session.quota.state).toBe("not_checked");
  expect(session.quota.windows).toEqual([]);
  expect(native.quota).not.toHaveBeenCalled();
});

test("a quota response for another connection is not admitted and reconciles local status only", async () => {
  native.read.mockResolvedValueOnce(configured());
  const session = owner();
  await session.start();
  native.quota.mockResolvedValue(
    configured({ connection_id: "b".repeat(32), quota: goQuotaReading() }),
  );
  native.read.mockResolvedValue(configured({ connection_id: "b".repeat(32) }));
  expect(await session.refreshQuota()).toBe(false);
  await flush();
  expect(session.view?.connection_id).toBe("b".repeat(32));
  expect(session.quota.windows).toEqual([]);
  expect(native.quota).toHaveBeenCalledTimes(1);
  expect(native.read).toHaveBeenCalledTimes(2);
});

test("an invalidation during quota refresh defers its old snapshot to local source readback", async () => {
  native.read.mockResolvedValueOnce(configured());
  const session = owner();
  await session.start();
  let finish!: (view: GoViewV1) => void;
  native.quota.mockImplementation(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const refreshing = session.refreshQuota();
  await Promise.resolve();
  native.read.mockResolvedValue(configured({ connection_id: "b".repeat(32) }));
  native.changed?.();
  finish(configured({ quota: goQuotaReading() }));
  expect(await refreshing).toBe(false);
  await flush();
  expect(session.view?.connection_id).toBe("b".repeat(32));
  expect(session.quota.windows).toEqual([]);
  expect(native.quota).toHaveBeenCalledTimes(1);
});

test("same-source native quota progress reconciles without showing a spurious failure", async () => {
  native.read.mockResolvedValueOnce(configured());
  const session = owner();
  await session.start();
  const complete = configured({ quota: goQuotaReading() });
  native.quota.mockImplementation(async () => {
    native.changed?.();
    return complete;
  });
  native.read.mockResolvedValue(complete);
  expect(await session.refreshQuota()).toBe(true);
  expect(session.observationFailed).toBe(false);
  expect(session.quota.state).toBe("current");
  expect(native.quota).toHaveBeenCalledTimes(1);
  expect(native.read).toHaveBeenCalledTimes(2);
});

test("clock rollback leaves a stale reading without an indefinite expiration timer", async () => {
  native.read.mockResolvedValue(configured({ quota: goQuotaReading(Date.now() + 60000) }));
  const session = owner();
  await session.start();
  expect(session.quota.state).toBe("stale");
  expect(vi.getTimerCount()).toBe(0);
  expect(native.quota).not.toHaveBeenCalled();
});
