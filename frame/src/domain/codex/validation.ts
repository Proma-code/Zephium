import type { CodexViewV1, CodexEffortV1 } from "$shared/ipc/codex-bindings";
const efforts: readonly CodexEffortV1[] = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];
const states = [
  "unavailable",
  "preparing",
  "signed_out",
  "signing_in",
  "connected",
  "recovery_required",
];
const faults = [
  "runtime_missing",
  "runtime_unsupported",
  "invalid",
  "busy",
  "cancelled",
  "timeout",
  "protocol",
  "vault",
  "profile_storage",
  "recovery_required",
  "scope_changed",
  "outcome_unknown",
  "unavailable",
];
const record = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const effort = (value: unknown): value is CodexEffortV1 =>
  typeof value === "string" && efforts.includes(value as CodexEffortV1);
const effortChoice = (value: unknown): boolean =>
  record(value) &&
  ((value.kind === "default" && Object.keys(value).length === 1) ||
    (value.kind === "explicit" && Object.keys(value).length === 2 && effort(value.effort)));
const bounded = (value: unknown, max: number): value is string =>
  typeof value === "string" && value.length > 0 && value.length <= max;
/** Native remains authoritative; malformed observations never turn into a ready account. */
export function validCodexView(view: unknown): view is CodexViewV1 {
  if (
    !record(view) ||
    view.version !== 1 ||
    !bounded(view.profile, 128) ||
    typeof view.state !== "string" ||
    !states.includes(view.state) ||
    view.text_only !== true ||
    typeof view.page_selected !== "boolean" ||
    !Array.isArray(view.models) ||
    view.models.length > 256 ||
    (view.login_attempt !== null && !bounded(view.login_attempt, 128)) ||
    (view.error !== null && (typeof view.error !== "string" || !faults.includes(view.error)))
  )
    return false;
  const ids = new Set<string>();
  for (const model of view.models) {
    if (
      !record(model) ||
      !bounded(model.id, 512) ||
      !bounded(model.name, 4096) ||
      ids.has(model.id) ||
      !Array.isArray(model.efforts) ||
      (model.efforts.length === 0 && model.default_effort === null) ||
      model.efforts.length > efforts.length ||
      !model.efforts.every(effort) ||
      new Set(model.efforts).size !== model.efforts.length ||
      (model.default_effort !== null && !effort(model.default_effort))
    )
      return false;
    ids.add(model.id);
  }
  return (
    view.selection === null ||
    (record(view.selection) &&
      bounded(view.selection.model, 512) &&
      effortChoice(view.selection.effort))
  );
}
