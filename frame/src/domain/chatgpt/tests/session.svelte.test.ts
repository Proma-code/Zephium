import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ChatGptViewV1 } from "$shared/ipc/chatgpt-bindings";
import { chatgptView } from "$shared/testing/chatgpt";
import { ChatGptSession } from "../session.svelte";

const native = vi.hoisted(() => ({
  read: vi.fn(),
  signIn: vi.fn(),
  cancel: vi.fn(),
  signOut: vi.fn(),
  models: vi.fn(),
  changed: null as (() => void) | null,
}));
vi.mock("$shared/ipc/chatgpt-bindings", async () => {
  const { mockChatGptBindings } = await import("$shared/testing/chatgpt");
  return mockChatGptBindings({
    chatgptAccount: native.read,
    chatgptSignIn: native.signIn,
    chatgptCancel: native.cancel,
    chatgptSignOut: native.signOut,
    chatgptRefreshModels: native.models,
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
const otherProfile = "00000000000000000000000002";
const sessions: ChatGptSession[] = [];
const owner = (id = profile) => {
  const session = new ChatGptSession(id);
  sessions.push(session);
  return session;
};
const foreignAttempt = "00000000-0000-4000-8000-000000000002";
const pending = (attempt = foreignAttempt) =>
  chatgptView(profile, { state: "signing_in", login_attempt: attempt });
const connected = () =>
  chatgptView(profile, { state: "connected", email: "reader@example.com", models: 3 });

beforeEach(() => {
  vi.useFakeTimers();
  for (const command of [native.read, native.signIn, native.cancel, native.signOut, native.models])
    command.mockReset();
  native.read.mockResolvedValue(chatgptView(profile));
  native.signIn.mockImplementation(async (_profile: string, attempt: string) => pending(attempt));
  native.cancel.mockResolvedValue(chatgptView(profile, { error: "cancelled" }));
  native.signOut.mockResolvedValue(chatgptView(profile));
  native.models.mockResolvedValue(connected());
});

afterEach(async () => {
  for (const session of sessions.splice(0)) session.dispose();
  await Promise.resolve();
  vi.useRealTimers();
});

test("account observation never starts login or selects a model", async () => {
  const session = owner();
  await session.start();
  await session.start();
  expect(native.read).toHaveBeenCalledExactlyOnceWith(profile);
  expect(session.view?.state).toBe("signed_out");
  expect(native.signIn).not.toHaveBeenCalled();
  expect(native.models).not.toHaveBeenCalled();
});

test("sign-in is single-flight and only native completion becomes connected", async () => {
  const session = owner();
  await session.start();
  const first = session.signIn();
  expect(await session.signIn()).toBe(false);
  await first;
  expect(native.signIn).toHaveBeenCalledExactlyOnceWith(profile, expect.any(String));
  expect(session.signingIn).toBe(true);
  expect(session.view?.state).toBe("signing_in");
  native.read.mockResolvedValue(connected());
  await vi.advanceTimersByTimeAsync(1000);
  expect(session.view?.state).toBe("connected");
  expect(session.view?.models).toBe(3);
  expect(session.signingIn).toBe(false);
});

test("another profile's account is never adopted", async () => {
  native.read.mockResolvedValue({ ...connected(), profile: otherProfile });
  const session = owner();
  await session.start();
  expect(session.view).toBeNull();
  expect(session.observationFailed).toBe(true);
  expect(await session.signIn()).toBe(false);
});

test("cancel wins over a stale start response", async () => {
  let finish!: (view: ChatGptViewV1) => void;
  native.signIn.mockImplementation(
    () => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)),
  );
  const session = owner();
  await session.start();
  const signingIn = session.signIn();
  await Promise.resolve();
  await session.cancel();
  finish(pending());
  await signingIn;
  expect(native.cancel).toHaveBeenCalledExactlyOnceWith(profile, native.signIn.mock.calls[0]![1]);
  expect(session.view?.state).toBe("signed_out");
  expect(session.signingIn).toBe(false);
});

test("disposing a pending owned flow cancels it and ignores late account data", async () => {
  let finish!: (view: ChatGptViewV1) => void;
  native.signIn.mockImplementation(
    () => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)),
  );
  const session = owner();
  await session.start();
  const signingIn = session.signIn();
  await Promise.resolve();
  session.dispose();
  await Promise.resolve();
  finish(connected());
  await signingIn;
  expect(native.cancel).toHaveBeenCalledExactlyOnceWith(profile, native.signIn.mock.calls[0]![1]);
  expect(session.view).toBeNull();
  expect(session.busy).toBeNull();
});

test("leaving a completed account or observing someone else's flow never disconnects it", async () => {
  native.read.mockResolvedValueOnce(connected()).mockResolvedValueOnce(pending());
  const completed = owner();
  await completed.start();
  completed.dispose();
  const observer = owner();
  await observer.start();
  observer.dispose();
  await Promise.resolve();
  expect(native.cancel).not.toHaveBeenCalled();
  expect(native.signOut).not.toHaveBeenCalled();
});

test("denied authorization remains signed out and can be retried explicitly", async () => {
  native.signIn.mockResolvedValue(chatgptView(profile, { error: "denied" }));
  const session = owner();
  await session.start();
  await session.signIn();
  expect(session.view?.state).toBe("signed_out");
  expect(session.view?.error).toBe("denied");
  expect(session.signingIn).toBe(false);
  await session.signIn();
  expect(native.signIn).toHaveBeenCalledTimes(2);
});

test("a read timeout is inconclusive, not signed out or connected", async () => {
  native.read.mockImplementation(() => new Promise<ChatGptViewV1>(() => {}));
  const session = owner();
  const started = session.start();
  await vi.advanceTimersByTimeAsync(5000);
  await started;
  expect(session.view).toBeNull();
  expect(session.observationFailed).toBe(true);
  expect(native.signIn).not.toHaveBeenCalled();
});

test("a lost start answer preserves cancellation instead of pretending login failed", async () => {
  native.signIn.mockImplementation(() => new Promise<ChatGptViewV1>(() => {}));
  const session = owner();
  await session.start();
  const signingIn = session.signIn();
  await vi.advanceTimersByTimeAsync(20000);
  await signingIn;
  expect(session.signingIn).toBe(true);
  expect(session.observationFailed).toBe(true);
  expect(session.view?.state).not.toBe("connected");
  await session.cancel();
  expect(session.signingIn).toBe(false);
});

test("bounded polling pauses unconfirmed and only explicit refresh resumes it", async () => {
  const session = owner();
  await session.start();
  await session.signIn();
  native.read.mockResolvedValue(pending());
  await vi.advanceTimersByTimeAsync(305000);
  expect(session.pollingPaused).toBe(true);
  expect(session.observationFailed).toBe(true);
  expect(session.view?.state).toBe("signing_in");
  const reads = native.read.mock.calls.length;
  await vi.advanceTimersByTimeAsync(10000);
  expect(native.read).toHaveBeenCalledTimes(reads);
  expect(native.cancel).not.toHaveBeenCalled();
  native.read.mockResolvedValue(connected());
  await session.refresh();
  expect(session.view?.state).toBe("connected");
  expect(session.pollingPaused).toBe(false);
});

test("disconnect reports unconfirmed remote revocation without preserving local connection", async () => {
  native.read.mockResolvedValue(connected());
  native.signOut.mockResolvedValue(chatgptView(profile, { revocation_unconfirmed: true }));
  const session = owner();
  await session.start();
  await session.signOut();
  expect(session.view?.state).toBe("signed_out");
  expect(session.view?.revocation_unconfirmed).toBe(true);
  expect(native.signOut).toHaveBeenCalledExactlyOnceWith(profile);
});

test("a stale read cannot restore an account after disconnect", async () => {
  native.read.mockResolvedValueOnce(connected());
  const session = owner();
  await session.start();
  let finish!: (view: ChatGptViewV1) => void;
  native.read.mockImplementation(() => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)));
  const reading = session.refresh();
  await Promise.resolve();
  await session.signOut();
  finish(connected());
  await reading;
  expect(session.view?.state).toBe("signed_out");
});

test("native account changes invalidate an older read and are reconciled once", async () => {
  const session = owner();
  await session.start();
  let finish!: (view: ChatGptViewV1) => void;
  native.read.mockImplementationOnce(
    () => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)),
  );
  native.read.mockResolvedValue(connected());
  const reading = session.refresh();
  await Promise.resolve();
  native.changed?.();
  native.changed?.();
  finish(chatgptView(profile));
  await reading;
  await vi.advanceTimersByTimeAsync(0);
  expect(native.read).toHaveBeenCalledTimes(3);
  expect(session.view?.state).toBe("connected");
});

test("a native busy answer never transfers ownership of another attempt", async () => {
  native.signIn.mockResolvedValue({ ...pending(), error: "busy" });
  const session = owner();
  await session.start();
  await session.signIn();
  session.dispose();
  await Promise.resolve();
  expect(native.cancel).not.toHaveBeenCalled();
});

test("an explicit cancel targets the exact observed attempt", async () => {
  native.read.mockResolvedValue(pending());
  const session = owner();
  await session.start();
  await session.cancel();
  expect(native.cancel).toHaveBeenCalledExactlyOnceWith(profile, foreignAttempt);
});

test("a newly started flow has a fresh attempt id, never reuses its canceled predecessor", async () => {
  const session = owner();
  await session.start();
  await session.signIn();
  await session.cancel();
  await session.signIn();
  const first = native.signIn.mock.calls[0]![1];
  const second = native.signIn.mock.calls[1]![1];
  expect(first).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u);
  expect(second).not.toBe(first);
  expect(native.cancel).toHaveBeenCalledExactlyOnceWith(profile, first);
});
