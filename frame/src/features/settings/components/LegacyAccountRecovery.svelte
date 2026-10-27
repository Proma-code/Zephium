<script lang="ts">
  import { ChatGptSession, chatGptFault, chatGptStatus } from "$domain/chatgpt";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";

  let { profile }: { profile: string } = $props();
  let session = $state.raw<ChatGptSession | null>(null);
  $effect(() => {
    const owner = new ChatGptSession(profile);
    session = owner;
    void owner.start();
    return () => {
      owner.dispose();
      if (session === owner) session = null;
    };
  });
  const view = $derived(session?.view ?? null);
  const busy = $derived(session?.busy ?? null);
  // Observation is not proof that a clean profile has a legacy record. This
  // recovery disclosure never offers a new sign-in or model catalogue.
  const retained = $derived(
    view?.presence === "stored" ||
      view?.presence === "unavailable" ||
      view?.revocation_unconfirmed ||
      session?.observationFailed,
  );
</script>

{#if retained}
  <details class="legacy-recovery">
    <summary>{m.ai_legacy_recovery_title()}</summary>
    <div class="content">
      <p>
        {view?.presence === "stored"
          ? m.ai_legacy_recovery_note()
          : m.ai_legacy_recovery_unavailable()}
      </p>
      <p aria-live="polite">{chatGptStatus(view)}</p>
      <div class="actions">
        {#if view?.presence === "stored"}
          <Button
            size="compact"
            pending={busy === "sign_out"}
            disabled={!!busy || view.state === "unavailable"}
            onclick={() => void session?.signOut()}>{m.ai_chatgpt_disconnect()}</Button
          >
        {/if}
        <Button
          size="compact"
          pending={session?.checking}
          disabled={!!busy}
          onclick={() => void session?.refresh()}>{m.ai_chatgpt_refresh()}</Button
        >
      </div>
      {#if session?.observationFailed}<p class="fault" role="alert">
          {m.ai_legacy_recovery_unavailable()}
        </p>{/if}
      {#if view?.error && !busy}<p class="fault" role="alert">{chatGptFault(view.error)}</p>{/if}
      {#if view?.revocation_unconfirmed}<p class="fault" role="alert">
          {m.ai_chatgpt_revocation()}
        </p>{/if}
    </div>
  </details>
{/if}

<style>
  .legacy-recovery {
    margin-block: 8px 24px;
  }
  summary {
    padding-block: 6px;
    padding-inline: 16px;
    color: var(--color-muted);
    font-size: var(--text-label);
    cursor: pointer;
  }
  summary:focus-visible {
    outline: 2px solid var(--color-ring);
    outline-offset: 2px;
  }
  .content {
    padding-block: 8px;
    padding-inline: 16px;
  }
  p {
    margin: 0;
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  p + p {
    margin-block-start: 8px;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-block-start: 12px;
  }
  .fault {
    margin-block-start: 8px;
    color: var(--color-danger);
  }
</style>
