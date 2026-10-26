import "$styles/global.css";
import { beforeEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { gatewayView } from "$shared/testing/gateway";
import type { GatewayViewV1 } from "$shared/ipc/gateway-bindings";
import { emitNativeEvent } from "$shared/testing/native-events";
import LocalGateway from "../components/LocalGateway.svelte";
const native = vi.hoisted(() => ({
  read: vi.fn(),
  save: vi.fn(),
  check: vi.fn(),
  disconnect: vi.fn(),
}));
vi.mock("$shared/ipc/gateway-bindings", async () => {
  const { mockGatewayBindings } = await import("$shared/testing/gateway");
  return mockGatewayBindings({
    gatewayStatus: native.read,
    gatewaySave: native.save,
    gatewayCheck: native.check,
    gatewayDisconnect: native.disconnect,
  });
});
const profile = "00000000000000000000000001";
const id = "a".repeat(32);
const base = "http://127.0.0.1:8080/v1";
const configured = (changes: Partial<GatewayViewV1> = {}) =>
  gatewayView(profile, {
    state: "configured",
    connection_id: id,
    label: "My gateway",
    base,
    ...changes,
  });
beforeEach(() => {
  for (const command of Object.values(native)) command.mockReset();
  native.read.mockResolvedValue(gatewayView(profile));
  native.save.mockResolvedValue(configured());
  native.check.mockResolvedValue(
    configured({ state: "checked", models: 2, checked_at_ms: "1700000000000" }),
  );
  native.disconnect.mockResolvedValue(gatewayView(profile));
});

test("opening setup observes only and discloses local external ownership", async () => {
  const screen = await render(LocalGateway, { profile });
  await expect.element(screen.getByText("No local gateway configured")).toBeVisible();
  await expect
    .element(screen.getByText("Antigravity consumer subscription linking", { exact: false }))
    .toBeVisible();
  await expect
    .element(screen.getByText("Gateway quota and pricing are not reported."))
    .toBeVisible();
  expect(native.read).toHaveBeenCalledExactlyOnceWith(profile);
  expect(native.check).not.toHaveBeenCalled();
  expect(native.save).not.toHaveBeenCalled();
});

test("save requires an exact loopback address and the gateway's own key; check is separate", async () => {
  const screen = await render(LocalGateway, { profile });
  await screen.getByRole("button", { name: "Set up local gateway" }).click();
  await expect.element(screen.getByLabelText("Connection label")).toHaveFocus();
  await screen.getByLabelText("Connection label").fill("My gateway");
  await screen.getByLabelText("Local gateway address").fill("http://localhost:8080/v1");
  await screen.getByLabelText("Gateway’s own key").fill("fixture-key");
  await expect.element(screen.getByRole("button", { name: "Save gateway" })).toBeDisabled();
  await screen.getByLabelText("Local gateway address").fill(base);
  await screen.getByRole("button", { name: "Save gateway" }).click();
  expect(native.save).toHaveBeenCalledExactlyOnceWith(
    profile,
    null,
    "My gateway",
    base,
    "fixture-key",
  );
  await expect.element(screen.getByText("Configured — catalog not checked")).toBeVisible();
  expect(native.check).not.toHaveBeenCalled();
  await screen.getByRole("button", { name: "Check models" }).click();
  expect(native.check).toHaveBeenCalledExactlyOnceWith(profile, id);
  await expect.element(screen.getByText("Catalog checked · 2 models")).toBeVisible();
  await expect
    .element(screen.getByText("It does not send a prompt", { exact: false }))
    .toBeVisible();
});

test("a pending save is single-flight and clears the secret before confirmation", async () => {
  let finish!: (view: GatewayViewV1) => void;
  native.save.mockImplementation(
    () =>
      new Promise<GatewayViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const screen = await render(LocalGateway, { profile });
  await screen.getByRole("button", { name: "Set up local gateway" }).click();
  await screen.getByLabelText("Connection label").fill("My gateway");
  await screen.getByLabelText("Local gateway address").fill(base);
  await screen.getByLabelText("Gateway’s own key").fill("fixture-key");
  await screen.getByRole("button", { name: "Save gateway" }).click();
  await expect.element(screen.getByRole("button", { name: "Save gateway" })).toBeDisabled();
  await expect.element(screen.getByLabelText("Gateway’s own key")).toHaveValue("");
  expect(native.save).toHaveBeenCalledTimes(1);
  finish(configured());
  await expect.element(screen.getByText("Configured — catalog not checked")).toBeVisible();
});

test("a replaced connection invalidates an open draft rather than updating its CAS silently", async () => {
  native.read.mockResolvedValue(configured());
  const screen = await render(LocalGateway, { profile });
  await screen.getByRole("button", { name: "Edit local gateway" }).click();
  await screen.getByLabelText("Gateway’s own key").fill("new-fixture-key");
  native.read.mockResolvedValue(
    configured({ connection_id: "b".repeat(32), label: "Replacement" }),
  );
  emitNativeEvent("workModelsChanged", { version: 1 });
  await expect.element(screen.getByRole("alert")).toHaveTextContent("connection changed");
  await expect.element(screen.getByRole("button", { name: "Save gateway" })).toBeDisabled();
  expect(native.save).not.toHaveBeenCalled();
  await screen.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect.element(screen.getByRole("button", { name: "Edit local gateway" })).toHaveFocus();
  await screen.getByRole("button", { name: "Edit local gateway" }).click();
  await expect.element(screen.getByLabelText("Connection label")).toHaveValue("Replacement");
  await expect.element(screen.getByLabelText("Gateway’s own key")).toHaveValue("");
});

test("leaving a pending save ignores its late data and never disconnects the admitted native operation", async () => {
  let finish!: (view: GatewayViewV1) => void;
  native.save.mockImplementation(
    () =>
      new Promise<GatewayViewV1>((resolve) => {
        finish = resolve;
      }),
  );
  const screen = await render(LocalGateway, { profile });
  await screen.getByRole("button", { name: "Set up local gateway" }).click();
  await screen.getByLabelText("Connection label").fill("Private draft");
  await screen.getByLabelText("Local gateway address").fill(base);
  await screen.getByLabelText("Gateway’s own key").fill("fixture-key");
  await screen.getByRole("button", { name: "Save gateway" }).click();
  await screen.unmount();
  const nextProfile = "00000000000000000000000002";
  native.read.mockResolvedValue(gatewayView(nextProfile));
  const next = await render(LocalGateway, { profile: nextProfile });
  finish(configured());
  await expect.element(next.getByText("No local gateway configured")).toBeVisible();
  await expect.element(next.getByText("Private draft")).not.toBeInTheDocument();
  expect(native.disconnect).not.toHaveBeenCalled();
});

test("failed check preserves configuration and offers explicit retry, not a success state", async () => {
  native.read.mockResolvedValue(configured());
  native.check.mockResolvedValue(configured({ error: "unauthorized" }));
  const screen = await render(LocalGateway, { profile });
  await screen.getByRole("button", { name: "Check models" }).click();
  await expect.element(screen.getByRole("alert")).toHaveTextContent("did not accept its key");
  await expect.element(screen.getByText("Configured — catalog not checked")).toBeVisible();
  await screen.getByRole("button", { name: "Check models" }).click();
  expect(native.check).toHaveBeenCalledTimes(2);
  expect(native.save).not.toHaveBeenCalled();
});

test("unavailable support cannot save or check a gateway", async () => {
  native.read.mockResolvedValue(gatewayView(profile, { state: "unavailable" }));
  const screen = await render(LocalGateway, { profile });
  await expect
    .element(screen.getByText("Local gateways are unavailable in this build or profile."))
    .toBeVisible();
  await expect
    .element(screen.getByRole("button", { name: "Set up local gateway" }))
    .not.toBeInTheDocument();
  await screen.getByRole("button", { name: "Check connection status" }).click();
  expect(native.save).not.toHaveBeenCalled();
  expect(native.check).not.toHaveBeenCalled();
});
