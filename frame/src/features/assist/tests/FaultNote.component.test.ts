import "$styles/global.css";
import { expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import FaultNote from "../components/FaultNote.svelte";

vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings({});
});

test("an unknown provider retry time stays manual with no invented countdown or automatic retry", async () => {
  const retry = vi.fn();
  const screen = await render(FaultNote, {
    fault: { reason: "model", fault: "rate_limited", retry_after_ms: null },
    onretry: retry,
  });
  await expect
    .element(screen.getByRole("alert"))
    .toHaveTextContent("The selected provider has limited this request. Try again later.");
  await expect
    .element(screen.getByRole("button", { name: "Try again", exact: true }))
    .toBeEnabled();
  await expect
    .element(screen.getByRole("button", { name: /^Try again in/u }))
    .not.toBeInTheDocument();
  expect(retry).not.toHaveBeenCalled();
  await screen.getByRole("button", { name: "Try again", exact: true }).click();
  expect(retry).toHaveBeenCalledTimes(1);
});

test("a provided retry countdown decreases instead of resetting and still requires an explicit click", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
  try {
    const retry = vi.fn();
    const screen = await render(FaultNote, {
      fault: { reason: "model", fault: "rate_limited", retry_after_ms: 2000 },
      onretry: retry,
    });
    await expect.element(screen.getByRole("button", { name: "Try again in 2s" })).toBeDisabled();
    await vi.advanceTimersByTimeAsync(1000);
    await expect.element(screen.getByRole("button", { name: "Try again in 1s" })).toBeDisabled();
    await vi.advanceTimersByTimeAsync(1000);
    await expect
      .element(screen.getByRole("button", { name: "Try again", exact: true }))
      .toBeEnabled();
    expect(retry).not.toHaveBeenCalled();
    await screen.getByRole("button", { name: "Try again", exact: true }).click();
    expect(retry).toHaveBeenCalledTimes(1);
    await screen.unmount();
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    vi.useRealTimers();
  }
});

test("long native retry intervals are not shortened, and closing cleans up their timer", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
  try {
    const screen = await render(FaultNote, {
      fault: { reason: "model", fault: "rate_limited", retry_after_ms: 900_000 },
      onretry: vi.fn(),
    });
    await expect.element(screen.getByRole("button", { name: "Try again in 900s" })).toBeDisabled();
    await screen.unmount();
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    vi.useRealTimers();
  }
});
