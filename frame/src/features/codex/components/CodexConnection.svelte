<script lang="ts">
  import { type CodexSession, codexStatus, codexFault } from "$domain/codex";
  import Button from "$shared/ui/Button";
  import CodexQuota from "./CodexQuota.svelte";
  import * as m from "$shared/i18n/messages";
  let { session }: { session: CodexSession } = $props();
  const view = $derived(session.view);
</script>

<div class="codex-connection">
  <p class="status" aria-live="polite">{codexStatus(view)}</p>
  <p>{m.codex_picker_text_only()}</p>
  {#if view?.page_selected}<p>{m.codex_selected()}</p>{/if}
  {#if view?.state === "signed_out" || view?.state === "unavailable"}<p>{m.codex_consent()}</p>{/if}
  <div class="actions">
    {#if session.signingIn}
      <Button
        size="compact"
        pending={session.busy === "cancel"}
        disabled={!!session.busy && session.busy !== "sign_in" && session.busy !== "cancel"}
        onclick={() => void session.cancel()}>{m.codex_cancel()}</Button
      >
    {:else if view?.state === "signed_out" || view?.state === "unavailable"}
      <Button
        size="compact"
        disabled={!!session.busy || session.checking}
        onclick={() => void session.signIn()}>{m.codex_connect()}</Button
      >
    {:else if view?.state === "connected"}
      <Button
        size="compact"
        pending={session.busy === "models"}
        disabled={!!session.busy}
        onclick={() => void session.refreshModels()}>{m.codex_models()}</Button
      >
      <Button
        size="compact"
        pending={session.busy === "sign_out"}
        disabled={!!session.busy}
        onclick={() => void session.signOut()}>{m.codex_logout()}</Button
      >
    {/if}
    <Button
      size="compact"
      pending={session.checking || session.busy === "check"}
      disabled={!!session.busy}
      onclick={() => void session.refresh()}>{m.codex_refresh()}</Button
    >
  </div>
  {#if session.observationFailed || session.pollingPaused}<p role="alert" class="fault">
      {m.codex_observation_failed()}
    </p>{/if}
  {#if view?.error && !session.busy}<p role="alert" class="fault">{codexFault(view.error)}</p>{/if}
  {#if view?.page_selected && !view.models.some((row) => row.id === view.selection?.model)}<p>
      {m.codex_choice_unavailable()}
    </p>{/if}
  {#if view?.state === "connected"}
    <CodexQuota
      value={view.quota}
      busy={!!session.busy}
      onrefresh={() => void session.refreshQuota()}
    />
  {/if}
  <details>
    <summary>{m.codex_details()}</summary>
    <p>{m.codex_capability()}</p>
    <p>{m.codex_unknown_usage()}</p>
  </details>
</div>

<style>
  .codex-connection {
    padding: 12px 14px;
    overflow-y: auto;
  }

  p {
    margin: 0;
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  p + p,
  details {
    margin-block-start: 8px;
  }

  .status {
    color: var(--color-text);
    font-weight: 500;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-block: 10px;
  }

  summary {
    color: var(--color-muted);
    font-size: var(--text-label);
    cursor: pointer;
  }

  .fault {
    color: var(--color-danger);
  }
</style>
