import "$styles/global.css";
import { beforeEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page, userEvent } from "vitest/browser";
import type { OperationAdmission } from "$shared/ipc/bindings";
import AiSubscriptions from "../components/AiSubscriptions.svelte";
import CliRow from "../components/connections/CliRow.svelte";

const native = vi.hoisted(() => ({ open: vi.fn(), status: vi.fn() }));
vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings({ browserOpenUrl: native.open, operationStatus: native.status });
});

beforeEach(() => {
  native.open.mockReset();
  native.status.mockReset();
  native.open.mockResolvedValue({ accepted: true, operation_id: null });
});

test("subscription setup is explicit about unavailable and unsupported integrations", async () => {
  await page.viewport(1000, 1100);
  const screen = await render(AiSubscriptions);
  await expect
    .element(screen.getByRole("heading", { name: "Other subscription tools" }))
    .toBeVisible();
  for (const name of [
    "Claude via Claude Code",
    "Claude via Claude Code",
    "Google Antigravity",
    "OpenCode CLI",
  ])
    await expect.element(screen.getByRole("heading", { name, exact: true })).toBeVisible();
  expect(screen.getByText("Not available in Zephium yet", { exact: true }).all()).toHaveLength(2);
  await expect.element(screen.getByText("Subscription linking is not supported")).toBeVisible();
  await expect
    .element(
      screen.getByText(
        "These CLI tools manage their own provider sessions. Their sign-ins are separate from Zephium’s API keys and OpenCode Go connection.",
      ),
    )
    .toBeVisible();
  await expect.element(screen.getByRole("button", { name: /^Sign in/ })).not.toBeInTheDocument();
  expect(native.open).not.toHaveBeenCalled();
});

test("keyboard guidance opens, closes, and reopens without account effects", async () => {
  const screen = await render(AiSubscriptions);
  const guidance = screen.getByRole("button", {
    name: "Setup guidance for Claude via Claude Code",
  });
  guidance.element().focus();
  await userEvent.keyboard("{Enter}");
  await expect.element(screen.getByText("claude auth login", { exact: true })).toBeVisible();
  await expect
    .element(screen.getByRole("button", { name: "Hide guidance for Claude via Claude Code" }))
    .toHaveAttribute("aria-expanded", "true");
  await userEvent.keyboard("{Enter}");
  await expect
    .element(screen.getByText("claude auth login", { exact: true }))
    .not.toBeInTheDocument();
  await userEvent.keyboard("{Enter}");
  await expect.element(screen.getByText("claude auth login", { exact: true })).toBeVisible();
  expect(native.open).not.toHaveBeenCalled();
});

test("Antigravity offers an explanation, not a login path", async () => {
  const screen = await render(AiSubscriptions);
  await screen.getByRole("button", { name: "Setup guidance for Google Antigravity" }).click();
  await expect
    .element(screen.getByText("Use the official Antigravity app directly.", { exact: false }))
    .toBeVisible();
  await expect.element(screen.getByText("agy", { exact: true })).not.toBeInTheDocument();
  await screen.getByRole("button", { name: "Open official Google Antigravity guide" }).click();
  expect(native.open).toHaveBeenCalledExactlyOnceWith("https://antigravity.google/docs/faq", true);
});

test("a rejected guide opening is explained and an explicit retry clears the error", async () => {
  native.open
    .mockResolvedValueOnce({ accepted: false, operation_id: null })
    .mockResolvedValueOnce({ accepted: true, operation_id: null });
  const screen = await render(AiSubscriptions);
  await screen.getByRole("button", { name: "Setup guidance for OpenCode CLI" }).click();
  const open = screen.getByRole("button", { name: "Open official OpenCode CLI guide" });
  await open.click();
  await expect.element(screen.getByRole("alert")).toHaveTextContent("Could not confirm");
  await open.click();
  await expect.element(screen.getByRole("alert")).not.toBeInTheDocument();
  expect(native.open).toHaveBeenCalledTimes(2);
});

test("an admitted native request without a disposition is never shown successful", async () => {
  native.open.mockResolvedValue({ accepted: true, operation_id: "0000000000000011" });
  native.status.mockResolvedValue({ state: "unknown" });
  const screen = await render(AiSubscriptions);
  await screen.getByRole("button", { name: "Setup guidance for Claude via Claude Code" }).click();
  await screen.getByRole("button", { name: "Open official Claude via Claude Code guide" }).click();
  await expect.element(screen.getByRole("alert")).toHaveTextContent("Could not confirm");
  expect(native.status).toHaveBeenCalledWith("0000000000000011");
  await expect.element(screen.getByText("Connected", { exact: true })).not.toBeInTheDocument();
});

test("repeated clicks stay single-flight and a closed guide ignores a late failure", async () => {
  let resolve!: (value: OperationAdmission) => void;
  native.open.mockImplementation(() => new Promise<OperationAdmission>((done) => (resolve = done)));
  const screen = await render(AiSubscriptions);
  await screen.getByRole("button", { name: "Setup guidance for OpenCode CLI" }).click();
  const open = screen.getByRole("button", { name: "Open official OpenCode CLI guide" });
  await open.click();
  await expect.element(open).toBeDisabled();
  const button = open.element();
  if (!(button instanceof HTMLButtonElement)) throw new Error("Expected a button");
  button.click();
  expect(native.open).toHaveBeenCalledTimes(1);
  await screen.getByRole("button", { name: "Hide guidance for OpenCode CLI" }).click();
  await screen.getByRole("button", { name: "Setup guidance for OpenCode CLI" }).click();
  await expect.element(open).toBeDisabled();
  resolve({ accepted: false, operation_id: null });
  await expect.element(open).not.toBeDisabled();
  await expect.element(screen.getByRole("alert")).not.toBeInTheDocument();
});

test("gated OpenCode discovery is unknown rather than missing or connected", async () => {
  const screen = await render(CliRow, {
    id: "opencode",
    cli: { id: "opencode", status: "unknown", version: null, path: null, account: null },
  });
  await expect
    .element(screen.getByRole("heading", { name: "OpenCode", exact: true }))
    .toBeVisible();
  await expect.element(screen.getByText("Sign-in verification unavailable")).toBeVisible();
  await expect.element(screen.getByRole("button", { name: "Install" })).not.toBeInTheDocument();
});
