import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { loadCodexSession, type CodexSession } from "$domain/codex";
import type { CodexSelectionV1, CodexViewV1 } from "$shared/ipc/codex-bindings";
import { codexView } from "$shared/testing/codex";
import { CodexEffortIntent } from "../lib/codex-effort.svelte";
import { CodexPickerObservation } from "../lib/codex-picker";

const native = vi.hoisted(() => ({ read: vi.fn(), select: vi.fn() }));
vi.mock("$shared/ipc/codex-bindings", () => ({
  commands: { codexAccount: native.read, codexSelectModel: native.select },
}));
vi.mock("$shared/ipc/native-events", () => ({
  events: { workModelsChanged: { listen: async () => () => {} } },
}));
const choice = (effort: "low" | "medium" | "high"): CodexSelectionV1 => ({
  model: "original",
  effort: { kind: "explicit", effort },
});
const view = (selection = choice("low")) =>
  codexView("profile", {
    state: "connected",
    page_selected: true,
    selection,
    models: [
      {
        id: "original",
        name: "Original",
        efforts: ["low", "medium", "high"],
        default_effort: "low",
      },
    ],
  });
let owner: CodexSession;
let intent: CodexEffortIntent;
let finish: (value: CodexViewV1) => void;
const confirmed = vi.fn();
beforeEach(async () => {
  vi.useFakeTimers();
  native.read.mockReset().mockResolvedValue(view());
  native.select.mockReset().mockImplementation(
    () =>
      new Promise<CodexViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  confirmed.mockReset();
  const { CodexSession } = await loadCodexSession();
  owner = new CodexSession("profile");
  await owner.start();
  intent = new CodexEffortIntent(owner, confirmed);
});
afterEach(() => {
  intent.cancel();
  owner.dispose();
  vi.useRealTimers();
});
const flush = async () => {
  await vi.advanceTimersByTimeAsync(0);
};

test("a 500ms commit retains only latest intent, never projects it as committed", async () => {
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("low"));
  intent.request(choice("high"));
  expect(intent.desired).toEqual(choice("high"));
  expect(owner.view?.selection).toEqual(choice("low"));
  expect(native.select).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(500);
  finish(view(choice("medium")));
  await flush();
  expect(native.select).toHaveBeenCalledTimes(2);
  expect(native.select).toHaveBeenLastCalledWith(
    "profile",
    choice("high"),
    "test-catalog-generation",
  );
  finish(view(choice("high")));
  await flush();
  expect(intent.desired).toBeNull();
  expect(owner.view?.selection).toEqual(choice("high"));
  expect(confirmed).toHaveBeenCalledTimes(1);
});

test.each(["busy", "outcome_unknown", "scope_changed"] as const)(
  "%s drops queued intent without replay",
  async (error) => {
    intent.request(choice("medium"));
    await flush();
    intent.request(choice("high"));
    finish({ ...view(), error });
    await flush();
    expect(native.select).toHaveBeenCalledTimes(1);
    expect(intent.desired).toBeNull();
    expect(confirmed).not.toHaveBeenCalled();
  },
);

test("unobserved commit stops queue and passively reconciles without retry", async () => {
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("high"));
  await vi.advanceTimersByTimeAsync(20000);
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(native.read).toHaveBeenCalledTimes(2);
  expect(intent.desired).toBeNull();
  finish(view(choice("medium")));
  await flush();
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(confirmed).not.toHaveBeenCalled();
});

test("catalog replacement fences later effort even when first commit succeeded", async () => {
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("high"));
  finish({ ...view(choice("medium")), catalog_generation: "replacement" });
  await flush();
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(intent.desired).toBeNull();
  expect(confirmed).not.toHaveBeenCalled();
});

test("disposing the interaction drops unsent intent without canceling the admitted native commit", async () => {
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("high"));
  intent.cancel();
  finish(view(choice("medium")));
  await flush();
  expect(owner.view?.selection).toEqual(choice("medium"));
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(confirmed).not.toHaveBeenCalled();
});

test("profile disposal ignores old completion and drops unsent intent", async () => {
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("high"));
  owner.dispose();
  finish(view(choice("medium")));
  await flush();
  expect(owner.view).toBeNull();
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(intent.desired).toBeNull();
  expect(confirmed).not.toHaveBeenCalled();
});

test("cancel drops only queued intent and allows a later fresh gesture", async () => {
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("high"));
  intent.cancelPending();
  finish(view(choice("medium")));
  await flush();
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(confirmed).not.toHaveBeenCalled();
  intent.request(choice("high"));
  await flush();
  expect(native.select).toHaveBeenCalledTimes(2);
  finish(view(choice("high")));
  await flush();
  expect(confirmed).toHaveBeenCalledTimes(1);
});

test("disconnect while a write is outstanding cannot carry its queue into a new account", async () => {
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("high"));
  finish(codexView("profile", { catalog_generation: null }));
  await flush();
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(intent.desired).toBeNull();
  expect(confirmed).not.toHaveBeenCalled();
});

test("ordinary pane close detaches UI callbacks but commits the latest desired effort", async () => {
  const closedPane = vi.fn();
  const detach = intent.attach(closedPane);
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("high"));
  detach();
  finish(view(choice("medium")));
  await flush();
  expect(native.select).toHaveBeenCalledTimes(2);
  expect(native.select).toHaveBeenLastCalledWith(
    "profile",
    choice("high"),
    "test-catalog-generation",
  );
  finish(view(choice("high")));
  await flush();
  expect(owner.view?.selection).toEqual(choice("high"));
  expect(intent.desired).toBeNull();
  expect(closedPane).not.toHaveBeenCalled();
  expect(confirmed).not.toHaveBeenCalled();
});

test("a reopened pane shares the sending lane without reviving its detached predecessor", async () => {
  const closedPane = vi.fn();
  const reopenedPane = vi.fn();
  const detach = intent.attach(closedPane);
  intent.request(choice("medium"));
  await flush();
  intent.request(choice("high"));
  detach();
  intent.attach(reopenedPane);
  // A late cleanup from an older pane must not detach its replacement.
  detach();
  finish(view(choice("medium")));
  await flush();
  expect(native.select).toHaveBeenCalledTimes(2);
  finish(view(choice("high")));
  await flush();
  expect(closedPane).not.toHaveBeenCalled();
  expect(reopenedPane).toHaveBeenCalledTimes(1);
});

test("whole picker disposal aborts its observer and discards its queued effort", async () => {
  const scope = new CodexPickerObservation("profile");
  const session = (await scope.acquire())!;
  await session.start();
  const queue = scope.effort!;
  queue.request(choice("medium"));
  await flush();
  queue.request(choice("high"));
  scope.dispose();
  finish(view(choice("medium")));
  await flush();
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(session.view).toBeNull();
  expect(scope.effort).toBeNull();
  expect(queue.desired).toBeNull();
});
