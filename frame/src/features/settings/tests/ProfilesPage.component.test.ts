import "$styles/global.css";
import { expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import type { ProfilesView } from "$shared/ipc/bindings";
import { requestNew } from "$session/profile-sheet.svelte";
import ProfilesPage from "../components/sections/ProfilesPage.svelte";

const native = vi.hoisted(() => ({
  create: vi.fn(async () => ({ accepted: true, operation_id: null })),
  retryRemoval: vi.fn(async () => ({ status: "ok" as const, data: "scheduled" as const })),
  recheckRemoval: vi.fn(async () => ({ status: "ok" as const, data: "scheduled" as const })),
}));
const roster: ProfilesView = {
  removal_recovery: null,
  removals: [],
  current: "01J00000000000000000000001",
  private: false,
  profiles: [
    {
      id: "01J00000000000000000000001",
      name: "Personal",
      kind: "default",
      avatar: { style: "initials", color: 1, symbol: 0, revision: 0 },
    },
  ],
};
vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings({
    profilesGet: async () => ({ status: "ok" as const, data: roster }),
    profilesCreate: native.create,
    profilesRetryRemoval: native.retryRemoval,
    profilesRecheckRemovalInventory: native.recheckRemoval,
  });
});

test("retained removal shows precise partial facts and retry never pretends completion", async () => {
  const removed = "01J00000000000000000000002";
  roster.removals = [
    {
      profile: removed,
      phase: "awaiting_explicit_retry",
      native: "signed_out_settled",
      cleanup: "unconfirmed",
      blocked: true,
    },
  ];
  const screen = await render(ProfilesPage);
  try {
    await expect.element(screen.getByText("Profile removal", { exact: true })).toBeVisible();
    await expect
      .element(screen.getByText("Signed out locally. Account data removal is not confirmed."))
      .toBeVisible();
    await screen.getByRole("button", { name: "Retry removal", exact: true }).click();
    await expect.poll(() => native.retryRemoval.mock.calls.length).toBe(1);
    expect(native.retryRemoval).toHaveBeenLastCalledWith(removed);
    await expect.element(screen.getByText("Profile removal", { exact: true })).toBeVisible();
    await expect
      .element(screen.getByText("Managed Codex account data was removed."))
      .not.toBeInTheDocument();
  } finally {
    roster.removals = [];
    await screen.unmount();
  }
});

test("a blocked removal can reconcile protected inventory even after a verified bootstrap", async () => {
  roster.removal_recovery = "verified";
  roster.removals = [
    {
      profile: "01J00000000000000000000002",
      phase: "awaiting_explicit_retry",
      native: "unconfirmed",
      cleanup: "not_started",
      blocked: true,
    },
  ];
  const priorChecks = native.recheckRemoval.mock.calls.length;
  const priorRetries = native.retryRemoval.mock.calls.length;
  const screen = await render(ProfilesPage);
  try {
    await screen.getByRole("button", { name: "Check removal status", exact: true }).click();
    await expect.poll(() => native.recheckRemoval.mock.calls.length).toBe(priorChecks + 1);
    expect(native.retryRemoval.mock.calls.length).toBe(priorRetries);
    await expect.element(screen.getByText("Profile removal", { exact: true })).toBeVisible();
    await expect
      .element(screen.getByText("Managed Codex account data was removed."))
      .not.toBeInTheDocument();
  } finally {
    roster.removal_recovery = null;
    roster.removals = [];
    await screen.unmount();
  }
});

test("unverified inventory keeps unrelated profiles usable and only offers explicit reconciliation", async () => {
  roster.removal_recovery = "unavailable";
  roster.removals = [
    {
      profile: "01J00000000000000000000002",
      phase: "awaiting_explicit_retry",
      native: "unconfirmed",
      cleanup: "not_started",
      blocked: true,
    },
  ];
  const previousRetries = native.retryRemoval.mock.calls.length;
  const previousChecks = native.recheckRemoval.mock.calls.length;
  const screen = await render(ProfilesPage);
  try {
    await expect.element(screen.getByText("Personal", { exact: true })).toBeVisible();
    await expect
      .element(screen.getByRole("button", { name: "Retry removal", exact: true }))
      .toBeDisabled();
    await screen.getByRole("button", { name: "Check removal status", exact: true }).click();
    await expect.poll(() => native.recheckRemoval.mock.calls.length).toBe(previousChecks + 1);
    expect(native.retryRemoval.mock.calls.length).toBe(previousRetries);
    await expect
      .element(screen.getByRole("button", { name: "Retry removal", exact: true }))
      .toBeDisabled();
    await expect.element(screen.getByText("Profile removal", { exact: true })).toBeVisible();
  } finally {
    roster.removal_recovery = null;
    roster.removals = [];
    await screen.unmount();
  }
});

test("New Profile from the sidebar creates and switches; from the list it only creates", async () => {
  await page.viewport(1200, 800);
  requestNew();
  let screen = await render(ProfilesPage);
  await screen.getByLabelText("Name").fill("School");
  await screen.getByRole("button", { name: "Create and Switch" }).click();
  await expect.poll(() => native.create.mock.calls.length).toBe(1);
  expect(native.create).toHaveBeenLastCalledWith(
    "School",
    expect.objectContaining({ style: "initials", photo: null }),
    true,
  );
  await screen.unmount();

  screen = await render(ProfilesPage);
  await screen.getByRole("button", { name: "New Profile…" }).click();
  await screen.getByLabelText("Name").fill("Work");
  await screen.getByRole("button", { name: "Create Profile" }).click();
  await expect.poll(() => native.create.mock.calls.length).toBe(2);
  expect(native.create).toHaveBeenLastCalledWith(
    "Work",
    expect.objectContaining({ style: "initials", photo: null }),
    false,
  );
});
