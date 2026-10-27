import "$styles/global.css";
import { beforeEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { chatgptView } from "$shared/testing/chatgpt";
import LegacyAccountRecovery from "../components/LegacyAccountRecovery.svelte";

const native = vi.hoisted(() => ({
  read: vi.fn(),
  signOut: vi.fn(),
}));
vi.mock("$shared/ipc/chatgpt-bindings", async () => {
  const { mockChatGptBindings } = await import("$shared/testing/chatgpt");
  return mockChatGptBindings({
    chatgptAccount: native.read,
    chatgptSignOut: native.signOut,
  });
});
vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings();
});

const profile = "00000000000000000000000001";
const connected = () =>
  chatgptView(profile, { state: "connected", presence: "stored", email: "reader@example.com" });

beforeEach(() => {
  for (const command of Object.values(native)) command.mockReset();
  native.read.mockResolvedValue(chatgptView(profile));
  native.signOut.mockResolvedValue(chatgptView(profile));
});

test("a clean profile offers no legacy card, sign-in or catalogue action", async () => {
  const screen = await render(LegacyAccountRecovery, { profile });
  await expect.poll(() => native.read.mock.calls.length).toBe(1);
  expect(screen.getByText("Advanced legacy account recovery").query()).toBeNull();
  expect(screen.getByRole("button").all()).toHaveLength(0);
  expect(native.signOut).not.toHaveBeenCalled();
});

test("retained account cleanup is collapsed, explicit and never reconnects or refreshes models", async () => {
  native.read.mockResolvedValue(connected());
  const screen = await render(LegacyAccountRecovery, { profile });
  const summary = screen.getByText("Advanced legacy account recovery");
  await expect.element(summary).toBeVisible();
  await expect
    .element(screen.getByRole("button", { name: "Disconnect ChatGPT", includeHidden: true }))
    .not.toBeVisible();
  await summary.click();
  await expect
    .element(screen.getByText("A legacy account record is retained on this device."))
    .toBeVisible();
  expect(screen.getByRole("button", { name: "Continue with ChatGPT" }).query()).toBeNull();
  expect(screen.getByRole("button", { name: "Refresh models" }).query()).toBeNull();
  await screen.getByRole("button", { name: "Disconnect ChatGPT" }).click();
  await expect.element(summary).not.toBeInTheDocument();
  expect(native.signOut).toHaveBeenCalledExactlyOnceWith(profile);
});

test("disconnect retains an unconfirmed remote revocation fact", async () => {
  native.read.mockResolvedValue(connected());
  native.signOut.mockResolvedValue(chatgptView(profile, { revocation_unconfirmed: true }));
  const screen = await render(LegacyAccountRecovery, { profile });
  await screen.getByText("Advanced legacy account recovery").click();
  await screen.getByRole("button", { name: "Disconnect ChatGPT" }).click();
  await expect
    .element(screen.getByRole("alert"))
    .toHaveTextContent("remote revocation could not be confirmed");
});

test("unmount and a replacement clean profile do not erase or reuse the retained account", async () => {
  native.read.mockResolvedValue(connected());
  const screen = await render(LegacyAccountRecovery, { profile });
  await expect.element(screen.getByText("Advanced legacy account recovery")).toBeVisible();
  await screen.unmount();
  const nextProfile = "00000000000000000000000002";
  native.read.mockResolvedValue(chatgptView(nextProfile));
  const next = await render(LegacyAccountRecovery, { profile: nextProfile });
  await expect.poll(() => native.read.mock.calls.at(-1)?.[0]).toBe(nextProfile);
  expect(next.getByText("Advanced legacy account recovery").query()).toBeNull();
  expect(native.signOut).not.toHaveBeenCalled();
});

test("a pending-registration-only record remains explicitly removable without a linked account", async () => {
  native.read.mockResolvedValue(chatgptView(profile, { presence: "stored" }));
  const screen = await render(LegacyAccountRecovery, { profile });
  await screen.getByText("Advanced legacy account recovery").click();
  await expect.element(screen.getByRole("button", { name: "Disconnect ChatGPT" })).toBeEnabled();
  await screen.getByRole("button", { name: "Disconnect ChatGPT" }).click();
  expect(native.signOut).toHaveBeenCalledExactlyOnceWith(profile);
});

test("unprovable presence offers observation, never a false clean result or blind disconnect", async () => {
  native.read.mockResolvedValue(
    chatgptView(profile, { presence: "unavailable", state: "unavailable", error: "vault" }),
  );
  const screen = await render(LegacyAccountRecovery, { profile });
  await screen.getByText("Advanced legacy account recovery").click();
  expect(screen.getByRole("button", { name: "Disconnect ChatGPT" }).query()).toBeNull();
  await screen.getByRole("button", { name: "Check account status" }).click();
  expect(native.signOut).not.toHaveBeenCalled();
});

test("a stored record with unavailable native custody cannot be blindly disconnected", async () => {
  native.read.mockResolvedValue(
    chatgptView(profile, { presence: "stored", state: "unavailable", error: "vault" }),
  );
  const screen = await render(LegacyAccountRecovery, { profile });
  await screen.getByText("Advanced legacy account recovery").click();
  await expect.element(screen.getByRole("button", { name: "Disconnect ChatGPT" })).toBeDisabled();
  await screen.getByRole("button", { name: "Check account status" }).click();
  expect(native.signOut).not.toHaveBeenCalled();
});
