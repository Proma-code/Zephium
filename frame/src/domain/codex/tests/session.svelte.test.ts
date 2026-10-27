import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { codexView } from "$shared/testing/codex";
import type { CodexViewV1 } from "$shared/ipc/codex-bindings";
import { CodexSession } from "../session.svelte";
const native = vi.hoisted(() => ({
  read: vi.fn(),
  check: vi.fn(),
  connect: vi.fn(),
  cancel: vi.fn(),
  logout: vi.fn(),
  models: vi.fn(),
  quota: vi.fn(),
  select: vi.fn(),
}));
vi.mock("$shared/ipc/codex-bindings", () => ({
  commands: {
    codexAccount: native.read,
    codexCheckConnection: native.check,
    codexConnect: native.connect,
    codexCancelLogin: native.cancel,
    codexLogout: native.logout,
    codexRefreshModels: native.models,
    codexRefreshQuota: native.quota,
    codexSelectModel: native.select,
  },
}));
vi.mock("$shared/ipc/native-events", () => ({
  events: { workModelsChanged: { listen: async () => () => {} } },
}));
const profile = "native-profile";
const model = {
  id: "original-id",
  name: "Original Clean Name",
  efforts: ["minimal", "high"] as const,
  default_effort: "high" as const,
};
const connected = () =>
  codexView(profile, { state: "connected", models: [{ ...model, efforts: [...model.efforts] }] });
const owners: CodexSession[] = [];
function owner() {
  const session = new CodexSession(profile);
  owners.push(session);
  return session;
}
beforeEach(() => {
  vi.useFakeTimers();
  Object.values(native).forEach((mock) => mock.mockReset());
  native.read.mockResolvedValue(codexView(profile));
  native.check.mockResolvedValue(codexView(profile));
  native.connect.mockImplementation(async (_: string, attempt: string) =>
    codexView(profile, { state: "signing_in", login_attempt: attempt }),
  );
  native.cancel.mockResolvedValue(codexView(profile));
  native.logout.mockResolvedValue(codexView(profile));
  native.models.mockResolvedValue(connected());
  native.quota.mockResolvedValue(connected());
  native.select.mockImplementation(async (_: string, selection) => ({
    ...connected(),
    selection,
    page_selected: !!selection,
  }));
});
afterEach(async () => {
  owners.splice(0).forEach((session) => session.dispose());
  await Promise.resolve();
  vi.useRealTimers();
});
test("construction is inert; explicit observation never logs in, refreshes models or selects", async () => {
  const s = owner();
  expect(native.read).not.toHaveBeenCalled();
  await s.start();
  await s.start();
  expect(native.read).toHaveBeenCalledTimes(1);
  for (const command of [native.check, native.connect, native.models, native.quota, native.select])
    expect(command).not.toHaveBeenCalled();
});
test("preparing/sign-in polling observes native completion without a second login", async () => {
  native.read.mockResolvedValue(codexView(profile, { state: "preparing" }));
  const s = owner();
  await s.start();
  native.read.mockResolvedValue(codexView(profile));
  await vi.advanceTimersByTimeAsync(1000);
  await s.signIn();
  expect(await s.signIn()).toBe(false);
  native.read.mockResolvedValue(connected());
  await vi.advanceTimersByTimeAsync(1000);
  expect(s.view?.state).toBe("connected");
  expect(native.connect).toHaveBeenCalledTimes(1);
  expect(native.models).not.toHaveBeenCalled();
});
test("catalog never auto-selects, and only exact supported native efforts are admitted", async () => {
  native.read.mockResolvedValue(connected());
  const s = owner();
  await s.start();
  expect(native.select).not.toHaveBeenCalled();
  expect(await s.select({ model: model.id, effort: { kind: "explicit", effort: "max" } })).toBe(
    false,
  );
  expect(
    await s.select({
      model: "wire-name-not-picker-id",
      effort: { kind: "explicit", effort: "high" },
    }),
  ).toBe(false);
  expect(await s.select({ model: model.id, effort: { kind: "explicit", effort: "minimal" } })).toBe(
    true,
  );
  expect(native.select).toHaveBeenCalledExactlyOnceWith(profile, {
    model: model.id,
    effort: { kind: "explicit", effort: "minimal" },
  });
  expect(await s.select(null)).toBe(true);
  expect(s.view?.page_selected).toBe(false);
});
test("cancel beats a late login response, while disposing never logs out", async () => {
  let finish!: (value: CodexViewV1) => void;
  native.connect.mockImplementation(() => new Promise((resolve) => (finish = resolve)));
  const s = owner();
  await s.start();
  const login = s.signIn();
  await Promise.resolve();
  await s.cancel();
  finish(codexView(profile, { state: "signing_in", login_attempt: "stale" }));
  await login;
  expect(s.view?.state).toBe("signed_out");
  s.dispose();
  expect(native.logout).not.toHaveBeenCalled();
});
test("foreign and malformed views do not become usable", async () => {
  for (const value of [
    codexView("foreign"),
    { ...connected(), models: [null] },
    { ...connected(), models: [{ ...model, efforts: ["ultra"] }] },
    null,
  ]) {
    native.read.mockResolvedValue(value);
    const s = owner();
    await s.start();
    expect(s.view).toBeNull();
    expect(s.observationFailed).toBe(true);
    expect(await s.signIn()).toBe(false);
    s.dispose();
  }
});
test("disposing a pending owned login only cancels that attempt", async () => {
  const s = owner();
  await s.start();
  await s.signIn();
  const attempt = native.connect.mock.calls[0]![1];
  s.dispose();
  await Promise.resolve();
  expect(native.cancel).toHaveBeenCalledExactlyOnceWith(profile, attempt);
  expect(native.logout).not.toHaveBeenCalled();
});
test("mutation observation timeout is not a fabricated success or a resend", async () => {
  native.read.mockResolvedValue(connected());
  native.select.mockImplementation(() => new Promise(() => {}));
  const s = owner();
  await s.start();
  const choice = s.select({ model: model.id, effort: { kind: "explicit", effort: "high" } });
  await vi.advanceTimersByTimeAsync(20001);
  expect(await choice).toBe(false);
  expect(s.observationFailed).toBe(true);
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(s.view?.page_selected).toBe(false);
});

test("only explicit Check connection requests owner recovery; automatic observation never does", async () => {
  native.read.mockResolvedValue(codexView(profile, { state: "preparing" }));
  const s = owner();
  await s.start();
  await vi.advanceTimersByTimeAsync(1000);
  expect(native.check).not.toHaveBeenCalled();
  native.check.mockResolvedValue(codexView(profile, { state: "signed_out" }));
  expect(await s.refresh()).toBe(true);
  expect(native.check).toHaveBeenCalledExactlyOnceWith(profile);
  expect(native.connect).not.toHaveBeenCalled();
  expect(s.view?.state).toBe("signed_out");
});

test("default-only catalog stays usable and default readback is not explicit none", async () => {
  const catalog = codexView(profile, {
    state: "connected",
    models: [{ id: "default-only", name: "Original Default", efforts: [], default_effort: "none" }],
  });
  native.read.mockResolvedValue(catalog);
  const s = owner();
  await s.start();
  expect(s.observationFailed).toBe(false);
  expect(
    await s.select({ model: "default-only", effort: { kind: "explicit", effort: "none" } }),
  ).toBe(false);
  native.select.mockImplementation(async (_profile: string, selection) => ({
    ...catalog,
    selection,
    page_selected: true,
  }));
  expect(await s.select({ model: "default-only", effort: { kind: "default" } })).toBe(true);
  expect(native.select).toHaveBeenCalledExactlyOnceWith(profile, {
    model: "default-only",
    effort: { kind: "default" },
  });
  native.select.mockResolvedValue({
    ...catalog,
    page_selected: true,
    selection: { model: "default-only", effort: { kind: "explicit", effort: "none" } },
  });
  expect(await s.select({ model: "default-only", effort: { kind: "default" } })).toBe(false);
  expect(native.select).toHaveBeenCalledTimes(2);
});

test("quota refresh is explicit, account-scoped and never changes model selection", async () => {
  const s = owner();
  await s.start();
  expect(await s.refreshQuota()).toBe(false);
  expect(native.quota).not.toHaveBeenCalled();
  native.read.mockResolvedValue(connected());
  native.check.mockResolvedValue(connected());
  await s.refresh();
  expect(native.quota).not.toHaveBeenCalled();
  native.quota.mockResolvedValue({ ...connected(), quota: { malformed: true } });
  expect(await s.refreshQuota()).toBe(true);
  expect(native.quota).toHaveBeenCalledExactlyOnceWith(profile);
  expect(s.view?.state).toBe("connected");
  expect(s.view?.models[0]?.id).toBe(model.id);
  expect(native.select).not.toHaveBeenCalled();
  expect(native.connect).not.toHaveBeenCalled();
});
test("disposed quota read cannot republish a retired profile's observation", async () => {
  native.read.mockResolvedValue(connected());
  const s = owner();
  await s.start();
  let settle!: (view: CodexViewV1) => void;
  native.quota.mockImplementation(
    () =>
      new Promise((resolve) => {
        settle = resolve;
      }),
  );
  const reading = s.refreshQuota();
  await Promise.resolve();
  s.dispose();
  settle(connected());
  await reading;
  expect(s.view).toBeNull();
  expect(native.quota).toHaveBeenCalledTimes(1);
  expect(native.logout).not.toHaveBeenCalled();
});
