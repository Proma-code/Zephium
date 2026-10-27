import "$styles/global.css";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { commands, page, userEvent } from "vitest/browser";
import type { CodexSelectionV1, CodexViewV1 } from "$shared/ipc/codex-bindings";
import { codexView } from "$shared/testing/codex";
import { CodexPickerObservation } from "../lib/codex-picker";
import ModelCodex from "../components/bar/ModelCodex.svelte";
import ModelCodexGestureStage from "./ModelCodexGestureStage.svelte";

declare module "vitest/browser" {
  interface BrowserCommands {
    dragCodexEffortToEnd: () => Promise<void>;
  }
}

const native = vi.hoisted(() => ({
  read: vi.fn(),
  select: vi.fn(),
  listen: vi.fn(),
  stop: vi.fn(),
}));
vi.mock("$shared/ipc/codex-bindings", () => ({
  commands: { codexAccount: native.read, codexSelectModel: native.select },
}));
vi.mock("$shared/ipc/native-events", async (importOriginal) => {
  const original = await importOriginal<typeof import("$shared/ipc/native-events")>();
  return {
    ...original,
    events: { ...original.events, workModelsChanged: { listen: native.listen } },
  };
});
const choice = (effort: "low" | "medium" | "high"): CodexSelectionV1 => ({
  model: "original",
  effort: { kind: "explicit", effort },
});
const catalog = (selection = choice("low"), profile = "profile") =>
  codexView(profile, {
    state: "connected",
    selection,
    page_selected: true,
    models: [
      {
        id: "original",
        name: "Original",
        efforts: ["low", "medium", "high"],
        default_effort: "low",
      },
    ],
  });
let observation: CodexPickerObservation;
let finish: (view: CodexViewV1) => void;
const onchosen = vi.fn();
const props = () => ({
  observation,
  onchosen,
  onstate: vi.fn(),
  oncount: vi.fn(),
  listId: "codex-models",
});
beforeEach(async () => {
  await page.viewport(600, 500);
  native.read.mockReset().mockResolvedValue(catalog());
  native.select.mockReset().mockImplementation(
    () =>
      new Promise<CodexViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  native.stop.mockReset();
  native.listen.mockReset().mockResolvedValue(native.stop);
  onchosen.mockReset();
  observation = new CodexPickerObservation("profile");
});
afterEach(() => observation.dispose());

async function openEffort() {
  await page.getByRole("button", { name: "Effort: Low", exact: true }).click();
  const slider = page.getByRole("slider", { name: "Effort", exact: true });
  await expect.element(slider).toHaveFocus();
  return slider;
}

test("keyboard intent stays responsive during a delayed native commit without optimistic success", async () => {
  const screen = await render(ModelCodex, props());
  const slider = await openEffort();
  await userEvent.keyboard("{ArrowRight}{ArrowRight}");
  await expect.element(slider).toHaveAttribute("aria-valuetext", "High");
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(native.select).toHaveBeenLastCalledWith(
    "profile",
    choice("medium"),
    "test-catalog-generation",
  );
  await expect.element(page.getByRole("status")).toHaveTextContent("Applying effort…");
  expect(screen.container.querySelector(".pill-label")?.textContent).toBe("Low");
  expect(onchosen).not.toHaveBeenCalled();
  finish(catalog(choice("medium")));
  await expect.poll(() => native.select.mock.calls.length).toBe(2);
  expect(native.select).toHaveBeenLastCalledWith(
    "profile",
    choice("high"),
    "test-catalog-generation",
  );
  finish(catalog(choice("high")));
  await expect.element(slider).toHaveAttribute("aria-valuetext", "High");
  await expect.poll(() => screen.container.querySelector(".pill-label")?.textContent).toBe("High");
  expect(page.getByText("Applying effort…").query()).toBeNull();
});

test("pointer steps retain the final intent while a rejected commit rolls the preview back", async () => {
  const screen = await render(ModelCodex, props());
  const slider = await openEffort();
  const element = slider.element() as HTMLElement;
  const box = element.getBoundingClientRect();
  // Exercise the real handlers and pointer capture seam in Chromium. Browser
  // generated capture requires an active device pointer, so stub capture only.
  const capture = vi.spyOn(element, "setPointerCapture").mockImplementation(() => {});
  element.dispatchEvent(
    new PointerEvent("pointerdown", {
      bubbles: true,
      pointerId: 4,
      button: 0,
      clientX: box.left + 8,
    }),
  );
  element.dispatchEvent(
    new PointerEvent("pointermove", {
      bubbles: true,
      pointerId: 4,
      clientX: box.left + box.width / 2,
    }),
  );
  await expect.poll(() => native.select.mock.calls.length).toBe(1);
  element.dispatchEvent(
    new PointerEvent("pointermove", { bubbles: true, pointerId: 4, clientX: box.right - 8 }),
  );
  element.dispatchEvent(
    new PointerEvent("pointerup", { bubbles: true, pointerId: 4, clientX: box.right - 8 }),
  );
  await expect.element(slider).toHaveAttribute("aria-valuetext", "High");
  expect(native.select).toHaveBeenCalledTimes(1);
  finish({ ...catalog(), error: "busy" });
  await expect.element(slider).toHaveAttribute("aria-valuetext", "Low");
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(screen.container.querySelector(".pill-label")?.textContent).toBe("Low");
  capture.mockRestore();
});

test("reopening retains rows immediately, reconciles passively and never multiplies listeners", async () => {
  const first = await render(ModelCodex, props());
  await expect.element(page.getByRole("option", { name: "Original", exact: true })).toBeVisible();
  expect(native.read).toHaveBeenCalledTimes(1);
  await first.unmount();
  native.read.mockImplementation(() => new Promise(() => {}));
  const second = await render(ModelCodex, props());
  await expect.element(page.getByRole("option", { name: "Original", exact: true })).toBeVisible();
  await expect.element(page.getByRole("listbox")).toHaveAttribute("aria-busy", "true");
  expect(native.read).toHaveBeenCalledTimes(2);
  expect(native.listen).toHaveBeenCalledTimes(1);
  expect(native.stop).not.toHaveBeenCalled();
  await second.unmount();
  observation.dispose();
  expect(native.stop).toHaveBeenCalledTimes(1);
});

test("closing cancels only queued effort, and remount shows actual admitted completion", async () => {
  const first = await render(ModelCodex, props());
  await openEffort();
  await userEvent.keyboard("{ArrowRight}{ArrowRight}");
  expect(native.select).toHaveBeenCalledTimes(1);
  await first.unmount();
  finish(catalog(choice("medium")));
  native.read.mockResolvedValue(catalog(choice("medium")));
  await render(ModelCodex, props());
  await expect
    .element(page.getByRole("button", { name: "Effort: Medium", exact: true }))
    .toBeVisible();
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(onchosen).not.toHaveBeenCalled();
});

test("profile replacement cannot display old rows or adopt its late read", async () => {
  let oldRead!: (view: CodexViewV1) => void;
  native.read.mockImplementation(
    () =>
      new Promise<CodexViewV1>((resolve) => {
        oldRead = resolve;
      }),
  );
  const first = await render(ModelCodex, props());
  await expect.poll(() => native.read.mock.calls.length).toBe(1);
  await first.unmount();
  observation.dispose();
  observation = new CodexPickerObservation("other");
  native.read.mockResolvedValue(codexView("other"));
  await render(ModelCodex, props());
  oldRead(catalog());
  await expect.poll(() => native.read.mock.calls.length).toBe(2);
  expect(page.getByRole("option", { name: "Original", exact: true }).query()).toBeNull();
  expect(native.stop).toHaveBeenCalledTimes(1);
});

test("real captured drag, keyboard repeats and close/reopen preserve first-and-latest intent", async () => {
  const screen = await render(ModelCodexGestureStage, { observation });
  let slider = await openEffort();
  const element = slider.element() as HTMLElement;
  const moves: { trusted: boolean; captured: boolean }[] = [];
  const record = (event: PointerEvent) => {
    if (event.buttons === 1)
      moves.push({
        trusted: event.isTrusted,
        captured: element.hasPointerCapture(event.pointerId),
      });
  };
  element.addEventListener("pointermove", record);
  try {
    // This command uses Playwright's real mouse down/move/up. No event dispatch,
    // pointer-capture stub, CSS override or successful native response is involved.
    await commands.dragCodexEffortToEnd();
  } finally {
    element.removeEventListener("pointermove", record);
  }
  expect(moves.length).toBeGreaterThanOrEqual(2);
  expect(moves.every((move) => move.trusted && move.captured)).toBe(true);
  await expect.element(slider).toHaveAttribute("aria-valuetext", "High");
  expect(native.select).toHaveBeenCalledTimes(1);
  expect(native.select).toHaveBeenLastCalledWith(
    "profile",
    choice("medium"),
    "test-catalog-generation",
  );
  expect(screen.container.querySelector(".pill-label")?.textContent).toBe("Low");
  await expect.element(page.getByText("Applying effort…", { exact: true })).toBeVisible();
  finish(catalog(choice("medium")));
  await expect.poll(() => native.select.mock.calls.length).toBe(2);
  expect(native.select).toHaveBeenLastCalledWith(
    "profile",
    choice("high"),
    "test-catalog-generation",
  );
  finish(catalog(choice("high")));
  await expect.poll(() => screen.container.querySelector(".pill-label")?.textContent).toBe("High");

  // Real keyboard input starts another unresolved write and changes the desired
  // detent again. Closing cancels only the second, still-unsent intent.
  await userEvent.keyboard("{ArrowLeft}{ArrowLeft}");
  await expect.element(slider).toHaveAttribute("aria-valuetext", "Low");
  expect(native.select).toHaveBeenCalledTimes(3);
  expect(native.select).toHaveBeenLastCalledWith(
    "profile",
    choice("medium"),
    "test-catalog-generation",
  );
  expect(screen.container.querySelector(".pill-label")?.textContent).toBe("High");
  await page.getByRole("button", { name: "Close picker", exact: true }).click();
  expect(slider.query()).toBeNull();
  finish(catalog(choice("medium")));
  native.read.mockResolvedValue(catalog(choice("medium")));
  await page.getByRole("button", { name: "Open picker", exact: true }).click();
  await page.getByRole("button", { name: "Effort: Medium", exact: true }).click();
  slider = page.getByRole("slider", { name: "Effort", exact: true });
  await expect.element(slider).toHaveAttribute("aria-valuetext", "Medium");
  expect(native.select).toHaveBeenCalledTimes(3);
  expect(native.listen).toHaveBeenCalledTimes(1);
  expect(native.read).toHaveBeenCalledTimes(2);
  expect(page.getByText("Applying effort…", { exact: true }).query()).toBeNull();
});
