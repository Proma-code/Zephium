import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { GoViewV1 } from "$shared/ipc/go-bindings";
import { goView } from "$shared/testing/opencode-go";
import { GoSession } from "../session.svelte";

const native = vi.hoisted(() => ({
  read: vi.fn(),
  save: vi.fn(),
  check: vi.fn(),
  disconnect: vi.fn(),
  changed: null as (() => void) | null,
}));
vi.mock("$shared/ipc/go-bindings", async () => {
  const { mockGoBindings } = await import("$shared/testing/opencode-go");
  return mockGoBindings({
    goStatus: native.read,
    goSave: native.save,
    goCheck: native.check,
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
  for (const command of [native.read, native.save, native.check, native.disconnect])
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
  expect(native.disconnect).not.toHaveBeenCalled();
});

test("saving remains configured until a separate explicit check and uses connection CAS", async () => {
  const session = owner();
  await session.start();
  expect(await session.save(null, "go-key", true)).toBe(true);
  expect(native.save).toHaveBeenCalledExactlyOnceWith(profile, null, "go-key", true);
  expect(session.view?.state).toBe("configured");
  expect(native.check).not.toHaveBeenCalled();
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
