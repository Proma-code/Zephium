import type { CodexViewV1, CodexFaultV1 } from "$shared/ipc/codex-bindings";
import * as m from "$shared/i18n/messages";
/** Keep optional connection labels out of effort-only consumers of the public barrel. */
export function codexFault(fault: CodexFaultV1): string {
  const faults: Record<CodexFaultV1, () => string> = {
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
  return faults[fault]();
}
/** Observed native capability, not a renderer permission or a connection-state guess. */
export function codexCapability(view: CodexViewV1 | null): string | null {
  if (view?.state !== "connected" || view.runtime !== "active") return null;
  return view.text_only ? m.codex_browser_unavailable() : m.codex_browser_ready();
}
export function codexStatus(view: CodexViewV1 | null): string {
  if (!view) return m.codex_loading();
  if (view.runtime === "preparing") return m.codex_preparing();
  if (view.state === "connected" && view.runtime === "dormant") return m.codex_dormant();
  if (view.state === "connected" && view.runtime === "unavailable") return m.codex_unavailable();
  return {
    unavailable: m.codex_unavailable,
    preparing: m.codex_preparing,
    signed_out: m.codex_signed_out,
    signing_in: m.codex_signing_in,
    connected: m.codex_connected,
    recovery_required: m.codex_recovery,
  }[view.state]();
}
