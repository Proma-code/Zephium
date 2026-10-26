import type { GatewayFaultV1, GatewayViewV1 } from "$shared/ipc/gateway-bindings";
import * as m from "$shared/i18n/messages";

const faults: Record<GatewayFaultV1, () => string> = {
  invalid: m.gateway_fault_invalid,
  not_configured: m.gateway_fault_not_configured,
  vault: m.gateway_fault_vault,
  busy: m.gateway_fault_busy,
  changed: m.gateway_fault_changed,
  cancelled: m.gateway_fault_cancelled,
  unauthorized: m.gateway_fault_unauthorized,
  forbidden: m.gateway_fault_forbidden,
  rate_limited: m.gateway_fault_rate_limited,
  unavailable: m.gateway_unavailable,
  timeout: m.gateway_fault_timeout,
  protocol: m.gateway_fault_protocol,
  too_large: m.gateway_fault_too_large,
  incomplete: m.gateway_fault_incomplete,
  usage_unavailable: m.gateway_unknown_usage,
};

export function gatewayFault(fault: GatewayFaultV1): string {
  return faults[fault]();
}

export function gatewayStatus(view: GatewayViewV1 | null): string {
  if (!view) return m.gateway_loading();
  if (view.checking) return m.gateway_checking();
  if (view.state === "configured") return m.gateway_configured();
  if (view.state === "checked") return m.gateway_checked({ count: view.models });
  if (view.state === "unavailable") return m.gateway_unavailable();
  return m.gateway_unconfigured();
}

/** A native timestamp is observational evidence, not a quota reset or an entitlement. */
export function gatewayCheckedAt(view: GatewayViewV1 | null): string | null {
  const value = view?.checked_at_ms;
  if (!value || !/^[0-9]{1,16}$/u.test(value)) return null;
  const milliseconds = Number(value);
  if (!Number.isSafeInteger(milliseconds)) return null;
  const date = new Date(milliseconds);
  return Number.isNaN(date.getTime()) ? null : date.toLocaleString();
}
