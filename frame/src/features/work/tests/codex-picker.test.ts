import { expect, test, vi } from "vitest";
import { CodexPickerObservation } from "../lib/codex-picker";

const mock = vi.hoisted(() => ({ dispose: vi.fn(), create: vi.fn(), failLoad: false }));
vi.mock("$domain/codex", () => ({
  loadCodexSession: async () => {
    if (mock.failLoad) throw new Error("chunk unavailable");
    return {
      CodexSession: class {
        constructor(readonly profile: string) {
          mock.create(profile);
        }
        dispose() {
          mock.dispose(this.profile);
        }
      },
    };
  },
}));

test("repeated pane acquisition shares only its picker/profile owner", async () => {
  const first = new CodexPickerObservation("first");
  const [a, b] = await Promise.all([first.acquire(), first.acquire()]);
  expect(a).toBe(b);
  expect(a?.profile).toBe("first");
  expect(mock.create).toHaveBeenCalledTimes(1);
  const second = new CodexPickerObservation("second");
  expect((await second.acquire())?.profile).toBe("second");
  first.dispose();
  expect(mock.dispose).toHaveBeenCalledWith("first");
  expect(await first.acquire()).toBeNull();
  second.dispose();
});

test("disposing before lazy resolution never constructs an orphan observer", async () => {
  mock.create.mockClear();
  const observation = new CodexPickerObservation("closed");
  const pending = observation.acquire();
  observation.dispose();
  expect(await pending).toBeNull();
  expect(mock.create).not.toHaveBeenCalled();
});

test("a rejected lazy import is retried on reopening without an orphan observer", async () => {
  mock.create.mockClear();
  const observation = new CodexPickerObservation("retry");
  mock.failLoad = true;
  await expect(observation.acquire()).rejects.toThrow("chunk unavailable");
  expect(mock.create).not.toHaveBeenCalled();
  mock.failLoad = false;
  expect((await observation.acquire())?.profile).toBe("retry");
  expect(mock.create).toHaveBeenCalledExactlyOnceWith("retry");
  observation.dispose();
});
