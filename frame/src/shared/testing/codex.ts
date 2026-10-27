import type { CodexViewV1 } from "$shared/ipc/codex-bindings";
/** Synthetic original-client projection; contains no account credentials. */
export function codexView(profile: string, patch: Partial<CodexViewV1> = {}): CodexViewV1 {
  return {
    version: 1,
    profile,
    state: "signed_out",
    runtime: "active",
    models: [],
    selection: null,
    page_selected: false,
    login_attempt: null,
    error: null,
    text_only: true,
    quota: {
      source: "original_app_server",
      state: "not_checked",
      account_epoch: null,
      checking: false,
      refresh_needed: false,
      observed_at_ms: null,
      attempted_at_ms: null,
      buckets: [],
      unavailable: null,
      error: null,
    },
    ...patch,
  };
}
