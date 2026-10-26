import type {
  GoViewV1,
  GoFaultV1,
  GoBillingSourceV1,
  GoBillingViewV1,
  GoQuotaViewV1,
  GoQuotaWindowV1,
  GoQuotaFaultV1,
} from "$shared/ipc/go-bindings";
import * as m from "$shared/i18n/messages";
import { quotaTime } from "./quota";

const faults: Record<GoFaultV1, () => string> = {
  invalid: m.go_fault_invalid,
  not_configured: m.go_fault_not_configured,
  changed: m.go_fault_changed,
  vault: m.go_fault_vault,
  busy: m.go_fault_busy,
  cancelled: m.go_fault_cancelled,
  unauthorized: m.go_fault_unauthorized,
  forbidden: m.go_fault_forbidden,
  rate_limited: m.go_fault_rate_limited,
  unavailable: m.go_unavailable,
  timeout: m.go_fault_timeout,
  protocol: m.go_fault_protocol,
  too_large: m.go_fault_too_large,
  incomplete: m.go_fault_incomplete,
  usage_unavailable: m.go_fault_usage_unavailable,
  missing_conversation: m.go_fault_missing_conversation,
};
export function goFault(fault: GoFaultV1): string {
  return faults[fault]();
}
export function goStatus(view: GoViewV1 | null): string {
  if (!view) return m.go_loading();
  if (view.checking) return m.go_checking();
  if (view.state === "configured") return m.go_configured();
  if (view.state === "catalogued") return m.go_catalogued({ count: view.models });
  if (view.state === "unavailable") return m.go_unavailable();
  return m.go_unconfigured();
}
/** A dated provider report is evidence about one request, not a plan or a remaining allowance. */
export function goBilling(view: GoViewV1 | null): string | null {
  const billing: GoBillingViewV1 | null | undefined = view?.last_billing;
  if (!billing || !/^[0-9]{1,16}$/u.test(billing.observed_at_ms)) return null;
  const milliseconds = Number(billing.observed_at_ms);
  if (!Number.isSafeInteger(milliseconds)) return null;
  const time = new Date(milliseconds);
  if (Number.isNaN(time.getTime())) return null;
  const sources: Record<GoBillingSourceV1, () => string> = {
    free: m.go_billing_free,
    balance: m.go_billing_balance,
    go: m.go_billing_go,
    go_plus: m.go_billing_go_plus,
    byok: m.go_billing_byok,
  };
  return m.go_last_billing({ source: sources[billing.source](), time: time.toLocaleString() });
}

const quotaFaults: Record<GoQuotaFaultV1, () => string> = {
  invalid: m.go_fault_invalid,
  not_configured: m.go_fault_not_configured,
  changed: m.go_fault_changed,
  vault: m.go_fault_vault,
  busy: m.go_fault_busy,
  cancelled: m.go_fault_cancelled,
  unauthorized: m.go_quota_fault_unauthorized,
  no_entitlement: m.go_quota_no_entitlement,
  forbidden: m.go_quota_fault_forbidden,
  endpoint_unavailable: m.go_quota_fault_endpoint,
  rate_limited: m.go_quota_fault_rate_limited,
  timeout: m.go_quota_fault_timeout,
  unavailable: m.go_quota_fault_unavailable,
  protocol: m.go_quota_fault_protocol,
  too_large: m.go_quota_fault_too_large,
};
export function goQuotaFault(fault: GoQuotaFaultV1): string {
  return Object.hasOwn(quotaFaults, fault) ? quotaFaults[fault]() : m.go_quota_fault_protocol();
}
export function goQuotaStatus(quota: GoQuotaViewV1 | undefined): string {
  if (!quota) return m.go_quota_not_checked();
  if (quota.state === "current") return m.go_quota_current();
  if (quota.state === "stale") return m.go_quota_stale();
  if (quota.state === "no_entitlement") return m.go_quota_no_entitlement();
  if (quota.state === "unavailable") return m.go_quota_unavailable();
  return m.go_quota_not_checked();
}
export function goQuotaWindow(kind: GoQuotaWindowV1["kind"]): string {
  const labels = {
    rolling: m.go_quota_rolling,
    weekly: m.go_quota_weekly,
    monthly: m.go_quota_monthly,
  };
  return labels[kind]();
}
export function goQuotaRemaining(percent: number | null): string {
  if (percent === null || !Number.isFinite(percent) || percent < 0 || percent > 100)
    return m.go_quota_unavailable();
  return m.go_quota_remaining({
    percent: new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(percent),
  });
}
export function goQuotaTime(value: string | null): string | null {
  const milliseconds = quotaTime(value);
  return milliseconds === null ? null : new Date(milliseconds).toLocaleString();
}
