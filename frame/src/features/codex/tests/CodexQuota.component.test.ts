import "$styles/global.css";
import { expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import type { CodexQuotaViewV1 } from "$shared/ipc/codex-bindings";
import CodexQuota from "../components/CodexQuota.svelte";

function quota(): CodexQuotaViewV1 {
  return {
    source: "original_app_server",
    state: "stale",
    account_epoch: "1",
    checking: false,
    refresh_needed: true,
    observed_at_ms: "1000",
    attempted_at_ms: "1000",
    unavailable: null,
    error: null,
    buckets: [
      {
        id: "codex",
        name: "Codex",
        plan: "plus",
        windows: [
          {
            kind: "primary",
            duration_minutes: 300,
            used_percent: 100,
            remaining_percent: 0,
            resets_at_ms: null,
          },
          {
            kind: "secondary",
            duration_minutes: null,
            used_percent: null,
            remaining_percent: null,
            resets_at_ms: null,
          },
        ],
      },
    ],
  };
}

test("quota stays in-app with distinct zero, unknown and stale windows", async () => {
  const refresh = vi.fn();
  const screen = await render(CodexQuota, { value: quota(), onrefresh: refresh });
  await expect.element(screen.getByText("0% remaining", { exact: true })).toBeVisible();
  await expect.element(screen.getByText("5-hour window", { exact: true })).toBeVisible();
  await expect
    .element(screen.getByText("Remaining quota not reported", { exact: true }))
    .toBeVisible();
  await expect
    .element(screen.getByText("Last reading · may be out of date", { exact: true }))
    .toBeVisible();
  await expect.element(screen.getByRole("link")).not.toBeInTheDocument();
  expect(refresh).not.toHaveBeenCalled();
  await screen.getByRole("button", { name: "Refresh quota", exact: true }).click();
  expect(refresh).toHaveBeenCalledTimes(1);
  await screen.unmount();
});

test("malformed quota is a local observation failure, not a sign-out action", async () => {
  const refresh = vi.fn();
  const screen = await render(CodexQuota, {
    value: { unsafe: "<script>secret</script>" },
    onrefresh: refresh,
  });
  await expect
    .element(screen.getByText("Quota could not be refreshed. Your chat connection is unchanged."))
    .toBeVisible();
  await expect
    .element(screen.getByRole("button", { name: "Refresh quota", exact: true }))
    .toBeVisible();
  await expect.element(screen.getByText("<script>secret</script>")).not.toBeInTheDocument();
  expect(refresh).not.toHaveBeenCalled();
  await screen.unmount();
});

test("reported whole-day windows are readable without assuming a weekly plan", async () => {
  const reading = quota();
  reading.buckets[0]!.windows[0]!.duration_minutes = 10080;
  const screen = await render(CodexQuota, { value: reading, onrefresh: vi.fn() });
  await expect.element(screen.getByText("7-day window", { exact: true })).toBeVisible();
  await expect.element(screen.getByText("Secondary window", { exact: true })).toBeVisible();
  await screen.unmount();
});

test("plans are shown only for each explicitly reported bucket", async () => {
  const reading = quota();
  reading.buckets = [
    { ...reading.buckets[0]!, id: "personal", name: "Personal", plan: "plus" },
    { ...reading.buckets[0]!, id: "work", name: "Work", plan: "business" },
    { ...reading.buckets[0]!, id: "unknown", name: "Unknown", plan: null },
  ];
  const screen = await render(CodexQuota, { value: reading, onrefresh: vi.fn() });
  await expect.element(screen.getByText("Plan: Plus", { exact: true })).toBeVisible();
  await expect.element(screen.getByText("Plan: Business", { exact: true })).toBeVisible();
  await expect.element(screen.getByText("Plan: Other", { exact: true })).not.toBeInTheDocument();
  await screen.unmount();
});
