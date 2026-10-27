import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ChatGptViewV1 } from "$shared/ipc/chatgpt-bindings";
import { chatgptView } from "$shared/testing/chatgpt";
import { ChatGptSession } from "../session.svelte";
const native = vi.hoisted(() => ({
  read: vi.fn(),
  signOut: vi.fn(),
  changed: null as (() => void) | null,
}));
vi.mock("$shared/ipc/chatgpt-bindings", async () => {
  const { mockChatGptBindings } = await import("$shared/testing/chatgpt");
  return mockChatGptBindings({ chatgptAccount: native.read, chatgptSignOut: native.signOut });
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
const other = "00000000000000000000000002";
const sessions: ChatGptSession[] = [];
const owner = () => {
  const value = new ChatGptSession(profile);
  sessions.push(value);
  return value;
};
const stored = () => chatgptView(profile, { presence: "stored", state: "connected" });
beforeEach(() => {
  vi.useFakeTimers();
  native.read.mockReset();
  native.signOut.mockReset();
  native.read.mockResolvedValue(chatgptView(profile));
  native.signOut.mockResolvedValue(chatgptView(profile, { presence: "stored" }));
});
afterEach(() => {
  for (const session of sessions.splice(0)) session.dispose();
  vi.useRealTimers();
});
test("clean observation is single flight and never exposes legacy login commands", async () => {
  const session = owner();
  await session.start();
  await session.start();
  expect(native.read).toHaveBeenCalledExactlyOnceWith(profile);
  expect(session.view?.presence).toBe("missing");
  expect("signIn" in session).toBe(false);
  expect("cancel" in session).toBe(false);
  expect("refreshModels" in session).toBe(false);
  expect(await session.signOut()).toBe(false);
  expect(native.signOut).not.toHaveBeenCalled();
});
test("pending-only registration can be explicitly disconnected without email or connected state", async () => {
  native.read.mockResolvedValue(chatgptView(profile, { presence: "stored" }));
  const session = owner();
  await session.start();
  expect(session.view?.email).toBeNull();
  expect(session.view?.state).toBe("signed_out");
  expect(await session.signOut()).toBe(true);
  expect(native.signOut).toHaveBeenCalledExactlyOnceWith(profile);
});
test("unavailable observation is not treated as missing or safely disconnected", async () => {
  native.read.mockResolvedValue(
    chatgptView(profile, { presence: "unavailable", state: "unavailable", error: "vault" }),
  );
  const session = owner();
  await session.start();
  expect(session.view?.presence).toBe("unavailable");
  expect(await session.signOut()).toBe(false);
  native.read.mockResolvedValue(stored());
  await session.refresh();
  expect(session.view?.presence).toBe("stored");
});
test("cross-profile and malformed presence never replace an accepted observation", async () => {
  native.read.mockResolvedValue(stored());
  const session = owner();
  await session.start();
  for (const value of [
    { ...stored(), profile: other },
    { ...stored(), presence: "unknown" },
    { ...stored(), presence: "missing" },
  ]) {
    native.read.mockResolvedValue(value);
    await session.refresh();
    expect(session.observationFailed).toBe(true);
    expect(session.view?.presence).toBe("stored");
  }
});
test("disconnect is single flight and a failed observation is not false success", async () => {
  let finish!: (view: ChatGptViewV1) => void;
  native.read.mockResolvedValue(stored());
  native.signOut.mockImplementation(
    () => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)),
  );
  const session = owner();
  await session.start();
  const disconnect = session.signOut();
  expect(await session.signOut()).toBe(false);
  await Promise.resolve();
  finish(chatgptView(profile, { presence: "stored", error: "vault" }));
  await disconnect;
  expect(native.signOut).toHaveBeenCalledTimes(1);
  expect(session.view?.error).toBe("vault");
});
test("read timeout remains inconclusive and does not create a missing projection", async () => {
  native.read.mockImplementation(() => new Promise<ChatGptViewV1>(() => {}));
  const session = owner();
  const start = session.start();
  await vi.advanceTimersByTimeAsync(5000);
  await start;
  expect(session.view).toBeNull();
  expect(session.observationFailed).toBe(true);
  expect(native.signOut).not.toHaveBeenCalled();
});
test("disposal never disconnects and ignores late account data", async () => {
  let finish!: (view: ChatGptViewV1) => void;
  native.read.mockImplementation(() => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)));
  const session = owner();
  const start = session.start();
  await vi.advanceTimersByTimeAsync(0);
  session.dispose();
  finish(stored());
  await start;
  expect(session.view).toBeNull();
  expect(native.signOut).not.toHaveBeenCalled();
});
test("events during disconnect queue one authoritative readback", async () => {
  let finish!: (view: ChatGptViewV1) => void;
  native.read.mockResolvedValue(stored());
  native.signOut.mockImplementation(
    () => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)),
  );
  const session = owner();
  await session.start();
  const disconnect = session.signOut();
  await Promise.resolve();
  native.changed?.();
  native.changed?.();
  native.read.mockResolvedValue(chatgptView(profile, { presence: "stored" }));
  finish(chatgptView(profile, { presence: "stored" }));
  await disconnect;
  await vi.advanceTimersByTimeAsync(0);
  expect(native.read).toHaveBeenCalledTimes(2);
  expect(session.view?.state).toBe("signed_out");
});
