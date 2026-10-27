import type { CodexViewV1, CodexFaultV1 } from "$shared/ipc/codex-bindings";
import * as m from "$shared/i18n/messages";
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
export const codexFault = (fault: CodexFaultV1) => faults[fault]();
export function codexStatus(view: CodexViewV1 | null): string {
  if (!view) return m.codex_loading();
  return {
    unavailable: m.codex_unavailable,
    preparing: m.codex_preparing,
    signed_out: m.codex_signed_out,
    signing_in: m.codex_signing_in,
    connected: m.codex_connected,
    recovery_required: m.codex_recovery,
  }[view.state]();
}
