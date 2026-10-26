import "$styles/global.css";
import { beforeEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import type { ChatGptViewV1 } from "$shared/ipc/chatgpt-bindings";
import { chatgptView } from "$shared/testing/chatgpt";
import ChatGptAccount from "../components/ChatGptAccount.svelte";

const native = vi.hoisted(() => ({
  read: vi.fn(),
  signIn: vi.fn(),
  cancel: vi.fn(),
  signOut: vi.fn(),
  models: vi.fn(),
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
vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings();
});

const profile = "00000000000000000000000001";
const connected = () =>
  chatgptView(profile, { state: "connected", email: "reader@example.com", models: 2 });

beforeEach(() => {
  for (const command of Object.values(native)) command.mockReset();
  native.read.mockResolvedValue(chatgptView(profile));
  native.signIn.mockImplementation(async (_profile: string, attempt: string) =>
    chatgptView(profile, { state: "signing_in", login_attempt: attempt }),
  );
  native.cancel.mockResolvedValue(chatgptView(profile, { error: "cancelled" }));
  native.signOut.mockResolvedValue(chatgptView(profile));
  native.models.mockResolvedValue(connected());
});

test("opening settings observes only, with sign-in disclosure before the explicit action", async () => {
  const screen = await render(ChatGptAccount, { profile });
  await expect.element(screen.getByText("Not connected", { exact: true })).toBeVisible();
  await expect
    .element(screen.getByText("Continue opens your system browser", { exact: false }))
    .toBeVisible();
  await expect.element(screen.getByRole("button", { name: "Continue with ChatGPT" })).toBeEnabled();
  expect(native.signIn).not.toHaveBeenCalled();
  expect(native.signOut).not.toHaveBeenCalled();
});

test("started OAuth is not connected and the person can cancel it", async () => {
  const screen = await render(ChatGptAccount, { profile });
  await screen.getByRole("button", { name: "Continue with ChatGPT" }).click();
  await expect.element(screen.getByText("Finish sign-in in your system browser.")).toBeVisible();
  await expect
    .element(screen.getByText("Connected to ChatGPT", { exact: true }))
    .not.toBeInTheDocument();
  await screen.getByRole("button", { name: "Cancel sign-in" }).click();
  await expect.element(screen.getByText("Not connected", { exact: true })).toBeVisible();
  await expect.element(screen.getByRole("alert")).toHaveTextContent("Sign-in was cancelled");
  expect(native.signIn).toHaveBeenCalledExactlyOnceWith(profile, expect.any(String));
  expect(native.cancel).toHaveBeenCalledExactlyOnceWith(profile, native.signIn.mock.calls[0]![1]);
});

test("connected account models can refresh without changing model selection", async () => {
  native.read.mockResolvedValue(connected());
  const screen = await render(ChatGptAccount, { profile });
  await expect.element(screen.getByText("Connected as reader@example.com")).toBeVisible();
  await expect
    .element(screen.getByText("2 account models available.", { exact: false }))
    .toBeVisible();
  await screen.getByRole("button", { name: "Refresh models" }).click();
  expect(native.models).toHaveBeenCalledExactlyOnceWith(profile);
  expect(native.signIn).not.toHaveBeenCalled();
  expect(native.signOut).not.toHaveBeenCalled();
});

test("disconnect does not hide an unconfirmed remote revocation", async () => {
  native.read.mockResolvedValue(connected());
  native.signOut.mockResolvedValue(chatgptView(profile, { revocation_unconfirmed: true }));
  const screen = await render(ChatGptAccount, { profile });
  await screen.getByRole("button", { name: "Disconnect ChatGPT" }).click();
  await expect.element(screen.getByText("Not connected", { exact: true })).toBeVisible();
  await expect
    .element(screen.getByRole("alert"))
    .toHaveTextContent("remote revocation could not be confirmed");
});

test("unavailable native support cannot start login", async () => {
  native.read.mockResolvedValue(chatgptView(profile, { state: "unavailable" }));
  const screen = await render(ChatGptAccount, { profile });
  await expect
    .element(screen.getByText("ChatGPT sign-in is unavailable in this build or profile."))
    .toBeVisible();
  await expect
    .element(screen.getByRole("button", { name: "Continue with ChatGPT" }))
    .not.toBeInTheDocument();
  await screen.getByRole("button", { name: "Check account status" }).click();
  expect(native.signIn).not.toHaveBeenCalled();
});

test("closing a pending owned sign-in cancels only that profile and ignores late completion", async () => {
  let finish!: (view: ChatGptViewV1) => void;
  native.signIn.mockImplementation(
    () => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)),
  );
  const screen = await render(ChatGptAccount, { profile });
  await screen.getByRole("button", { name: "Continue with ChatGPT" }).click();
  await expect.element(screen.getByRole("button", { name: "Cancel sign-in" })).toBeVisible();
  await screen.unmount();
  const nextProfile = "00000000000000000000000002";
  native.read.mockResolvedValue(chatgptView(nextProfile));
  const next = await render(ChatGptAccount, { profile: nextProfile });
  finish(connected());
  await expect.element(next.getByText("Not connected", { exact: true })).toBeVisible();
  await expect
    .element(next.getByText("reader@example.com", { exact: false }))
    .not.toBeInTheDocument();
  expect(native.cancel).toHaveBeenCalledExactlyOnceWith(profile, native.signIn.mock.calls[0]![1]);
});
