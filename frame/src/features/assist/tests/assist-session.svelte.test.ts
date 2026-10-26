import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { PageAssistAskResponseV1 } from "$shared/ipc/bindings";
import { AssistSession } from "../lib/assist-session.svelte";
import { status } from "./fixtures";

const native = vi.hoisted(() => ({ ask: vi.fn(), read: vi.fn(), close: vi.fn() }));
vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings({
    pageAssistAsk: native.ask,
    pageAssistStatus: native.read,
    pageAssistClose: native.close,
  });
});
vi.mock("$shared/ipc/native-events", () => ({
  events: { pageAssistChanged: { listen: async () => () => {} } },
}));
const owners: AssistSession[] = [];
const flush = async () => {
  await Promise.resolve();
  await Promise.resolve();
};
function deferred() {
  let resolve!: (value: PageAssistAskResponseV1) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<PageAssistAskResponseV1>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function owner(profile = "profile-a") {
  const session = new AssistSession(profile);
  owners.push(session);
  await session.start();
  session.follow("space-a", "tab-a");
  await flush();
  return session;
}
function draft(session: AssistSession) {
  session.picks = [
    {
      tab: "tab-new",
      pick: "pick-new",
      label: "New selection",
      host: "example.test",
      url: "https://example.test",
    },
  ];
  session.others = ["tab-other"];
  session.pageOff = true;
  session.selectionOn = true;
  session.notice = { reason: "busy" };
}
function expectDraft(session: AssistSession) {
  expect(session.picks.map((pick) => pick.pick)).toEqual(["pick-new"]);
  expect(session.others).toEqual(["tab-other"]);
  expect(session.pageOff).toBe(true);
  expect(session.selectionOn).toBe(true);
  expect(session.notice).toEqual({ reason: "busy" });
}
beforeEach(() => {
  for (const command of Object.values(native)) command.mockReset();
  native.close.mockResolvedValue(true);
  native.read.mockImplementation(async (space: string, tab: string | null) =>
    status([], {
      space,
      tab,
      conversation: { ...status().conversation, id: `conversation-${space}` },
    }),
  );
});
afterEach(() => {
  for (const session of owners.splice(0)) session.dispose();
});

test.each(["success", "refused", "rejected"] as const)(
  "late %s from the old Space cannot modify a new draft or release its pending send",
  async (outcome) => {
    const old = deferred();
    const next = deferred();
    native.ask.mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise);
    const session = await owner();
    const first = session.ask("Old message");
    session.follow("space-b", "tab-b");
    await flush();
    expect(session.sending).toBe(false);
    const second = session.ask("New message");
    draft(session);
    if (outcome === "success") old.resolve({ reply: 1, fault: null });
    else if (outcome === "refused") old.resolve({ reply: null, fault: { reason: "invalid" } });
    else old.reject(new Error("synthetic old failure"));
    expect(await first).toBe(false);
    expectDraft(session);
    expect(session.sending).toBe(true);
    expect(native.ask.mock.calls[0]![0]).toMatchObject({
      space: "space-a",
      conversation: "conversation-space-a",
    });
    next.resolve({ reply: 2, fault: null });
    expect(await second).toBe(true);
    expect(session.sending).toBe(false);
    expect(session.picks).toEqual([]);
  },
);

test("switching tabs protects the new page and selection flags from old admission", async () => {
  const pending = deferred();
  native.ask.mockReturnValue(pending.promise);
  const session = await owner();
  const sending = session.ask("Old tab");
  session.follow("space-a", "tab-b");
  await flush();
  draft(session);
  pending.resolve({ reply: 1, fault: null });
  expect(await sending).toBe(false);
  expectDraft(session);
  expect(session.sending).toBe(false);
});

test("a replacement conversation and an A→B→A navigation both fence the old send", async () => {
  for (const replace of [false, true]) {
    const pending = deferred();
    native.ask.mockReturnValue(pending.promise);
    const session = await owner();
    const sending = session.ask("Old conversation");
    if (replace) {
      native.read.mockResolvedValueOnce(
        status([], { conversation: { ...status().conversation, id: "replacement" } }),
      );
      session.refresh();
    } else {
      session.follow("space-b", "tab-b");
      await flush();
      session.follow("space-a", "tab-a");
    }
    await flush();
    draft(session);
    pending.resolve({ reply: 1, fault: null });
    expect(await sending).toBe(false);
    expectDraft(session);
    expect(session.sending).toBe(false);
  }
});

test("same-conversation status reconciliation does not strand a valid send", async () => {
  const pending = deferred();
  native.ask.mockReturnValue(pending.promise);
  const session = await owner();
  const sending = session.ask("Current conversation");
  session.refresh();
  await flush();
  expect(session.sending).toBe(true);
  pending.resolve({ reply: 1, fault: null });
  expect(await sending).toBe(true);
  expect(session.sending).toBe(false);
});

test("disposing a profile ignores late completion without mutating its replacement", async () => {
  const old = deferred();
  const next = deferred();
  native.ask.mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise);
  const previous = await owner();
  const first = previous.ask("Old profile");
  previous.dispose();
  const current = await owner("profile-b");
  const second = current.ask("New profile");
  draft(current);
  old.resolve({ reply: 1, fault: null });
  expect(await first).toBe(false);
  expect(previous.sending).toBe(false);
  expect(previous.notice).toBeNull();
  expectDraft(current);
  expect(current.sending).toBe(true);
  expect(await previous.ask("After disposal")).toBe(false);
  expect(native.ask).toHaveBeenCalledTimes(2);
  next.resolve({ reply: 2, fault: null });
  expect(await second).toBe(true);
});
