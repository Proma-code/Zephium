import "$styles/global.css";
import { beforeEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { goView } from "$shared/testing/opencode-go";
import type { GoViewV1 } from "$shared/ipc/go-bindings";
import { emitNativeEvent } from "$shared/testing/native-events";
import OpenCodeGo from "../components/OpenCodeGo.svelte";
const native = vi.hoisted(() => ({
  read: vi.fn(),
  save: vi.fn(),
  check: vi.fn(),
  disconnect: vi.fn(),
  open: vi.fn(),
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
vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings({ browserOpenUrl: native.open });
});
const profile = "00000000000000000000000001";
const connection = "a".repeat(32);
const configured = (changes: Partial<GoViewV1> = {}) =>
  goView(profile, { state: "configured", connection_id: connection, ...changes });
beforeEach(() => {
  for (const command of Object.values(native)) command.mockReset();
  native.read.mockResolvedValue(goView(profile));
  native.save.mockResolvedValue(configured());
  native.check.mockResolvedValue(configured({ state: "catalogued", models: 2 }));
  native.disconnect.mockResolvedValue(goView(profile));
  native.open.mockResolvedValue({ accepted: true, operation_id: null });
});

test("opening Go settings observes only and explains coding-agent use, billing, and unknown quota", async () => {
  const screen = await render(OpenCodeGo, { profile });
  await expect.element(screen.getByText("No OpenCode Go key saved")).toBeVisible();
  await expect
    .element(screen.getByText("designed for coding agents", { exact: false }))
    .toBeVisible();
  await expect
    .element(screen.getByText("cannot override your provider setting", { exact: false }))
    .toBeVisible();
  await expect
    .element(screen.getByText("Remaining OpenCode Go quota is not provided by this connection."))
    .toBeVisible();
  expect(native.check).not.toHaveBeenCalled();
  expect(native.save).not.toHaveBeenCalled();
  expect(native.open).not.toHaveBeenCalled();
});

test("saving needs deliberate acknowledgement, resets it on key edits, and never auto-checks or verifies a plan", async () => {
  const screen = await render(OpenCodeGo, { profile });
  await screen.getByRole("button", { name: "Set up OpenCode Go" }).click();
  await expect.element(screen.getByLabelText("OpenCode Go key")).toHaveFocus();
  const acknowledgement = screen.getByRole("checkbox", {
    name: "I intend to use OpenCode Go for coding-agent work and understand that provider Extra Usage can charge my Console balance.",
  });
  await expect.element(acknowledgement).not.toBeChecked();
  await screen.getByLabelText("OpenCode Go key").fill(" fixture-key ");
  await expect.element(screen.getByRole("button", { name: "Save Go key" })).toBeDisabled();
  await acknowledgement.click();
  await expect.element(screen.getByRole("button", { name: "Save Go key" })).toBeEnabled();
  await screen.getByLabelText("OpenCode Go key").fill(" another-fixture-key ");
  await expect.element(acknowledgement).not.toBeChecked();
  await acknowledgement.click();
  await screen.getByRole("button", { name: "Save Go key" }).click();
  expect(native.save).toHaveBeenCalledExactlyOnceWith(profile, null, "another-fixture-key", true);
  await expect.element(screen.getByText("Key saved — catalog not checked")).toBeVisible();
  expect(native.check).not.toHaveBeenCalled();
  await screen.getByRole("button", { name: "List supported models" }).click();
  await expect
    .element(screen.getByText("Public catalog listed · 2 supported models"))
    .toBeVisible();
  await expect
    .element(screen.getByText("It does not verify your key", { exact: false }))
    .toBeVisible();
  expect(native.check).toHaveBeenCalledExactlyOnceWith(profile, connection);
});

test("cancel wipes the key and acknowledgement and returns focus to the setup control", async () => {
  const screen = await render(OpenCodeGo, { profile });
  await screen.getByRole("button", { name: "Set up OpenCode Go" }).click();
  await screen.getByLabelText("OpenCode Go key").fill("fixture-key");
  await screen.getByRole("checkbox").click();
  await screen.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect.element(screen.getByRole("button", { name: "Set up OpenCode Go" })).toHaveFocus();
  await screen.getByRole("button", { name: "Set up OpenCode Go" }).click();
  await expect.element(screen.getByLabelText("OpenCode Go key")).toHaveValue("");
  await expect.element(screen.getByRole("checkbox")).not.toBeChecked();
  expect(native.save).not.toHaveBeenCalled();
});

test("a replaced connection makes the open draft stale and cannot overwrite its new key", async () => {
  native.read.mockResolvedValue(configured());
  const screen = await render(OpenCodeGo, { profile });
  await screen.getByRole("button", { name: "Replace Go key" }).click();
  await screen.getByLabelText("OpenCode Go key").fill("fixture-key");
  await screen.getByRole("checkbox").click();
  native.read.mockResolvedValue(configured({ connection_id: "b".repeat(32) }));
  emitNativeEvent("workModelsChanged", { version: 1 });
  await expect.element(screen.getByRole("alert")).toHaveTextContent("Go connection changed");
  await expect.element(screen.getByRole("button", { name: "Save Go key" })).toBeDisabled();
  expect(native.save).not.toHaveBeenCalled();
});

test("pending save is single-flight and the key is cleared before native confirmation", async () => {
  let finish!: (view: GoViewV1) => void;
  native.save.mockImplementation(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const screen = await render(OpenCodeGo, { profile });
  await screen.getByRole("button", { name: "Set up OpenCode Go" }).click();
  await screen.getByLabelText("OpenCode Go key").fill("fixture-key");
  await screen.getByRole("checkbox").click();
  await screen.getByRole("button", { name: "Save Go key" }).click();
  await expect.element(screen.getByLabelText("OpenCode Go key")).toHaveValue("");
  await expect.element(screen.getByRole("button", { name: "Save Go key" })).toBeDisabled();
  expect(native.save).toHaveBeenCalledTimes(1);
  finish(configured());
  await expect.element(screen.getByText("Key saved — catalog not checked")).toBeVisible();
});

test("profile replacement ignores late results without pretending to cancel admitted native persistence", async () => {
  let finish!: (view: GoViewV1) => void;
  native.save.mockImplementation(
    () =>
      new Promise<GoViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const screen = await render(OpenCodeGo, { profile });
  await screen.getByRole("button", { name: "Set up OpenCode Go" }).click();
  await screen.getByLabelText("OpenCode Go key").fill("fixture-key");
  await screen.getByRole("checkbox").click();
  await screen.getByRole("button", { name: "Save Go key" }).click();
  await screen.unmount();
  const nextProfile = "00000000000000000000000002";
  native.read.mockResolvedValue(goView(nextProfile));
  const next = await render(OpenCodeGo, { profile: nextProfile });
  finish(configured());
  await expect.element(next.getByText("No OpenCode Go key saved")).toBeVisible();
  expect(native.disconnect).not.toHaveBeenCalled();
});

test("last provider billing source is dated and never becomes remaining quota or a current plan", async () => {
  native.read.mockResolvedValue(
    configured({
      state: "catalogued",
      models: 2,
      last_billing: { source: "balance", observed_at_ms: "1700000000000" },
    }),
  );
  const screen = await render(OpenCodeGo, { profile });
  await expect
    .element(screen.getByText("Last provider-reported billing: Console balance", { exact: false }))
    .toBeVisible();
  await expect
    .element(screen.getByText("not remaining monthly usage", { exact: false }))
    .toBeVisible();
  await expect
    .element(screen.getByText("Remaining OpenCode Go quota is not provided by this connection."))
    .toBeVisible();
  await screen.getByRole("button", { name: "Disconnect OpenCode Go" }).click();
  expect(native.disconnect).toHaveBeenCalledExactlyOnceWith(profile, connection);
  await expect
    .element(screen.getByText("Last provider-reported billing:", { exact: false }))
    .not.toBeInTheDocument();
});

test("official documentation failures settle and retry is explicit; no link opening is account connection", async () => {
  native.open.mockRejectedValueOnce(new Error("fixture unavailable"));
  const screen = await render(OpenCodeGo, { profile });
  await screen.getByRole("button", { name: "OpenCode Go guide" }).click();
  await expect.element(screen.getByRole("alert")).toHaveTextContent("Could not confirm");
  await screen.getByRole("button", { name: "OpenCode Console" }).click();
  await expect.element(screen.getByRole("alert")).not.toBeInTheDocument();
  expect(native.open).toHaveBeenNthCalledWith(1, "https://opencode.ai/v2/docs/console/go/", true);
  expect(native.open).toHaveBeenNthCalledWith(2, "https://opencode.ai/console/", true);
  expect(native.save).not.toHaveBeenCalled();
});

test("unavailable native support never exposes a save or paid test action", async () => {
  native.read.mockResolvedValue(goView(profile, { state: "unavailable" }));
  const screen = await render(OpenCodeGo, { profile });
  await expect
    .element(screen.getByText("OpenCode Go is unavailable in this build or profile."))
    .toBeVisible();
  await expect
    .element(screen.getByRole("button", { name: "Set up OpenCode Go" }))
    .not.toBeInTheDocument();
  expect(native.check).not.toHaveBeenCalled();
});
