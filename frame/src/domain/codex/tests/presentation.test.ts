import { expect, test } from "vitest";
import type { CodexEffortV1, CodexFaultV1 } from "$shared/ipc/codex-bindings";
import * as m from "$shared/i18n/messages";
import { codexEffort } from "../effort";
import { codexFault } from "../presentation";

test("every native fault keeps its original localized copy", () => {
  const labels: Record<CodexFaultV1, () => string> = {
    runtime_missing: m.codex_runtime_missing,
    runtime_unsupported: m.codex_runtime_unsupported,
    invalid: m.codex_fault_invalid,
    busy: m.codex_fault_busy,
    cancelled: m.codex_fault_cancelled,
    timeout: m.codex_fault_timeout,
    protocol: m.codex_fault_protocol,
    vault: m.codex_fault_storage,
    profile_storage: m.codex_fault_storage,
    recovery_required: m.codex_recovery,
    scope_changed: m.codex_fault_changed,
    outcome_unknown: m.codex_fault_unknown,
    unavailable: m.codex_unavailable,
  };
  for (const fault of Object.keys(labels) as CodexFaultV1[]) {
    expect(codexFault(fault)).toBe(labels[fault]());
    expect(codexFault(fault)).toBe(labels[fault]());
  }
});

test("all seven explicit efforts retain their exact localized labels", () => {
  const labels: Record<CodexEffortV1, () => string> = {
    none: m.codex_effort_none,
    minimal: m.codex_effort_minimal,
    low: m.codex_effort_low,
    medium: m.codex_effort_medium,
    high: m.codex_effort_high,
    xhigh: m.codex_effort_xhigh,
    max: m.codex_effort_max,
  };
  for (const effort of Object.keys(labels) as CodexEffortV1[])
    expect(codexEffort(effort)).toBe(labels[effort]());
});
