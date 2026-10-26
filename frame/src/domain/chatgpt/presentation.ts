import type { ChatGptFaultV1, ChatGptViewV1 } from "$shared/ipc/chatgpt-bindings";
import * as m from "$shared/i18n/messages";

const faults: Record<ChatGptFaultV1, () => string> = {
  invalid: m.ai_chatgpt_fault_invalid,
  offline: m.ai_chatgpt_fault_offline,
  denied: m.ai_chatgpt_fault_denied,
  cancelled: m.ai_chatgpt_fault_cancelled,
  timeout: m.ai_chatgpt_fault_timeout,
  protocol: m.ai_chatgpt_fault_protocol,
  vault: m.ai_chatgpt_fault_vault,
  session_expired: m.ai_chatgpt_fault_session_expired,
  usage_limit: m.ai_chatgpt_fault_usage_limit,
  unavailable: m.ai_chatgpt_unavailable,
  busy: m.ai_chatgpt_fault_busy,
};

export function chatGptFault(fault: ChatGptFaultV1): string {
  return faults[fault]();
}

export function chatGptStatus(view: ChatGptViewV1 | null, signingIn: boolean): string {
  if (signingIn) return m.ai_chatgpt_signing_in();
  if (view?.state === "connected")
    return view.email ? m.ai_chatgpt_connected_as({ email: view.email }) : m.ai_chatgpt_connected();
  if (view?.state === "signed_out") return m.ai_chatgpt_signed_out();
  if (view?.state === "unavailable") return m.ai_chatgpt_unavailable();
  return m.ai_chatgpt_checking();
}
