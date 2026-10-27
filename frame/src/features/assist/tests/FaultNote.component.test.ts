import "$styles/global.css";
import { afterEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import type {
  ChatGptFailureIssue,
  ChatGptFailureStage,
  PageAssistFaultV1,
} from "$shared/ipc/bindings";
import FaultNote from "../components/FaultNote.svelte";

vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings({});
});

afterEach(() => vi.restoreAllMocks());

const chatGpt = (
  stage: ChatGptFailureStage = "stream",
  issue: ChatGptFailureIssue = "protocol",
): PageAssistFaultV1 => ({
  reason: "chat_gpt",
  fault: { diagnostic: { field: null, stage, status: null, issue } },
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

test("ChatGPT diagnostics copy only a finite code and retry is never automatic", async () => {
  let finish!: () => void;
  const copy = vi.spyOn(navigator.clipboard, "writeText").mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  const retry = vi.fn();
  const screen = await render(FaultNote, { fault: chatGpt(), onretry: retry });
  await expect
    .element(screen.getByRole("alert"))
    .toHaveTextContent("ChatGPT returned an unsupported response.");
  await expect.element(screen.getByText("This request may have used quota.")).toBeVisible();
  const button = screen.getByRole("button", { name: "Copy error code", exact: true });
  await button.click();
  await expect.element(button).toBeDisabled();
  (button.element() as HTMLButtonElement).click();
  expect(copy).toHaveBeenCalledTimes(1);
  expect(copy).toHaveBeenCalledWith("CHATGPT/STREAM/-/PROTOCOL");
  expect(retry).not.toHaveBeenCalled();
  finish();
  await expect.element(screen.getByRole("button", { name: "Copied", exact: true })).toBeEnabled();
  await screen.getByRole("button", { name: "Try again", exact: true }).click();
  expect(retry).toHaveBeenCalledTimes(1);
});

test("a clipboard refusal leaves selectable evidence and a working repeat action", async () => {
  const copy = vi
    .spyOn(navigator.clipboard, "writeText")
    .mockRejectedValueOnce(new Error("synthetic clipboard refusal"))
    .mockResolvedValue();
  const screen = await render(FaultNote, { fault: chatGpt("credential", "cancelled") });
  await expect.element(screen.getByText("The ChatGPT request was stopped.")).toBeVisible();
  await expect
    .element(screen.getByText("This request may have used quota."))
    .not.toBeInTheDocument();
  const button = screen.getByRole("button", { name: "Copy error code", exact: true });
  await button.click();
  await expect
    .element(screen.getByRole("status"))
    .toHaveTextContent("Copy not confirmed. Select the error code to copy it.");
  await expect
    .element(screen.getByText("CHATGPT/CREDENTIAL/-/CANCELLED", { exact: true }))
    .toBeVisible();
  await button.click();
  await expect.element(screen.getByRole("button", { name: "Copied", exact: true })).toBeEnabled();
  await expect.element(screen.getByRole("status")).not.toBeInTheDocument();
  expect(copy).toHaveBeenCalledTimes(2);
});

test("a late clipboard result cannot mark a replacement diagnostic copied", async () => {
  let finish!: () => void;
  const copy = vi
    .spyOn(navigator.clipboard, "writeText")
    .mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue();
  const screen = await render(FaultNote, { fault: chatGpt() });
  await screen.getByRole("button", { name: "Copy error code", exact: true }).click();
  await screen.rerender({ fault: chatGpt("send", "timeout") });
  finish();
  const button = screen.getByRole("button", { name: "Copy error code", exact: true });
  await expect.element(button).toBeEnabled();
  await expect
    .element(screen.getByRole("button", { name: "Copied", exact: true }))
    .not.toBeInTheDocument();
  await button.click();
  expect(copy).toHaveBeenLastCalledWith("CHATGPT/SEND/-/TIMEOUT");
});

test("closing while copying cleans up observation without touching the next fault", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  try {
    let reject!: (reason: Error) => void;
    vi.spyOn(navigator.clipboard, "writeText").mockImplementation(
      () =>
        new Promise<void>((_, no) => {
          reject = no;
        }),
    );
    const screen = await render(FaultNote, { fault: chatGpt() });
    await screen.getByRole("button", { name: "Copy error code", exact: true }).click();
    await screen.unmount();
    expect(vi.getTimerCount()).toBe(0);
    const next = await render(FaultNote, { fault: chatGpt("credential", "signed_out") });
    reject(new Error("synthetic late clipboard error"));
    await expect
      .element(next.getByRole("button", { name: "Copy error code", exact: true }))
      .toBeEnabled();
    await expect.element(next.getByRole("status")).not.toBeInTheDocument();
    await expect
      .element(
        next.getByText(
          "This legacy ChatGPT account needs attention. Open advanced account recovery in AI settings, or explicitly choose Codex for a new conversation.",
          { exact: true },
        ),
      )
      .toBeVisible();
  } finally {
    vi.useRealTimers();
  }
});

test("an inconclusive clipboard write times out without claiming success, and ignores late completion", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  try {
    let finish!: () => void;
    vi.spyOn(navigator.clipboard, "writeText").mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const screen = await render(FaultNote, { fault: chatGpt() });
    const button = screen.getByRole("button", { name: "Copy error code", exact: true });
    await button.click();
    await vi.advanceTimersByTimeAsync(2000);
    await expect.element(button).toBeEnabled();
    await expect
      .element(screen.getByRole("status"))
      .toHaveTextContent("Copy not confirmed. Select the error code to copy it.");
    finish();
    await expect
      .element(screen.getByRole("button", { name: "Copied", exact: true }))
      .not.toBeInTheDocument();
    await screen.unmount();
    expect(vi.getTimerCount()).toBe(0);
  } finally {
    vi.useRealTimers();
  }
});
