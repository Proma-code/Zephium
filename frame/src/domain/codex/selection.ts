import type {
  CodexEffortSelectionV1,
  CodexModelV1,
  CodexSelectionV1,
} from "$shared/ipc/codex-bindings";

/** Display validation only; original native catalog still owns admission. */
export function supportsCodexEffort(model: CodexModelV1, choice: CodexEffortSelectionV1): boolean {
  return choice.kind === "default"
    ? model.default_effort !== null
    : model.efforts.includes(choice.effort);
}
export function sameCodexEffort(a: CodexEffortSelectionV1, b: CodexEffortSelectionV1): boolean {
  return a.kind === "default"
    ? b.kind === "default"
    : b.kind === "explicit" && a.effort === b.effort;
}
/** Default is an original-runtime omission, not an explicit guessed preset. */
export function codexModelSelection(
  model: CodexModelV1,
  current: CodexSelectionV1 | null,
): CodexSelectionV1 | null {
  if (current?.model === model.id && supportsCodexEffort(model, current.effort)) return current;
  return model.default_effort !== null ? { model: model.id, effort: { kind: "default" } } : null;
}
