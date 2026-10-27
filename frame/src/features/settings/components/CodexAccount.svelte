<script lang="ts">
  import { type CodexSession, loadCodexSession } from "$domain/codex";
  import { loadCodexConnection } from "$features/codex";
  import * as m from "$shared/i18n/messages";
  let { profile }: { profile: string } = $props();
  let open = $state(false);
  let session = $state.raw<CodexSession | null>(null);
  let loadFault = $state(false);
  $effect(() => {
    if (!open) return;
    const selectedProfile = profile;
    let disposed = false;
    let owner: CodexSession | null = null;
    loadFault = false;
    void loadCodexSession()
      .then((loaded) => {
        if (disposed) return;
        owner = new loaded.CodexSession(selectedProfile);
        session = owner;
        void owner.start().catch(() => {
          if (!disposed) loadFault = true;
        });
      })
      .catch(() => {
        if (!disposed) loadFault = true;
      });
    return () => {
      disposed = true;
      owner?.dispose();
      if (session === owner) session = null;
    };
  });
</script>

<details bind:open class="codex-account">
  <summary>{m.codex_title()}</summary>
  {#if open && session}
    {#await loadCodexConnection()}<p>{m.codex_loading()}</p>
    {:then view}<view.default {session} />
    {:catch}<p role="alert">{m.codex_load_failed()}</p>{/await}
  {:else if open}<p role={loadFault ? "alert" : "status"}>
      {loadFault ? m.codex_load_failed() : m.codex_loading()}
    </p>{/if}
</details>

<style>
  .codex-account {
    margin-block: 8px 20px;
  }

  summary {
    padding: 8px 16px;
    color: var(--color-text);
    font-size: var(--text-label);
    font-weight: 500;
    cursor: default;
  }

  summary:focus-visible {
    outline: 2px solid var(--color-ring);
    outline-offset: 2px;
  }

  p {
    padding-inline: 16px;
    color: var(--color-muted);
    font-size: var(--text-label);
  }
</style>
