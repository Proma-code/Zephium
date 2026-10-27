import type { ChatGptFaultV1, ChatGptViewV1 } from "$shared/ipc/chatgpt-bindings";
import * as m from "$shared/i18n/messages";

const faults: Record<ChatGptFaultV1, () => string> = {
  invalid: m.ai_chatgpt_fault_invalid,
  offline: m.ai_chatgpt_fault_offline,
  cancelled: m.ai_chatgpt_fault_cancelled,
  timeout: m.ai_chatgpt_fault_timeout,
  protocol: m.ai_chatgpt_fault_protocol,
  vault: m.ai_chatgpt_fault_vault,
  unavailable: m.ai_chatgpt_unavailable,
  busy: m.ai_chatgpt_fault_busy,
};

export function chatGptFault(fault: ChatGptFaultV1): string {
  return faults[fault]();
}

export function chatGptStatus(view: ChatGptViewV1 | null): string {
  if (!view) return m.ai_chatgpt_checking();
  if (view.state === "unavailable" || view.presence === "unavailable")
    return m.ai_legacy_recovery_unavailable();
  if (view.presence === "stored") return m.ai_legacy_record_saved();
  return m.ai_legacy_record_removed();
}
