import type { CodexEffortV1 } from "$shared/ipc/codex-bindings";
import * as m from "$shared/i18n/messages";

export const codexEffort = (effort: CodexEffortV1) =>
  ({
    none: m.codex_effort_none,
    minimal: m.codex_effort_minimal,
    low: m.codex_effort_low,
    medium: m.codex_effort_medium,
    high: m.codex_effort_high,
    xhigh: m.codex_effort_xhigh,
    max: m.codex_effort_max,
  })[effort]();
