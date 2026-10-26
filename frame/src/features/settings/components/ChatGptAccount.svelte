<script lang="ts">
  import { ChatGptSession, chatGptFault, chatGptStatus } from "$domain/chatgpt";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";
  import SettingsGroup from "$shared/ui/SettingsGroup";

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
  const signingIn = $derived(session?.signingIn ?? false);
  const busy = $derived(session?.busy ?? null);
  const status = $derived(chatGptStatus(view, signingIn));
</script>

<SettingsGroup title={m.ai_chatgpt_title()}>
  <div class="chatgpt-account">
    <p class="status" aria-live="polite">{status}</p>
    {#if view?.state === "connected" && !signingIn}
      <p class="metadata">
        <span
          >{view.models
            ? m.ai_chatgpt_available_models({ count: view.models })
            : m.ai_chatgpt_no_models()}</span
        >
        <span aria-hidden="true">·</span>
        <span>{m.model_chatgpt_quota_unknown()}</span>
      </p>
    {:else if view?.state === "signed_out" && !signingIn}
      <p>{m.ai_chatgpt_consent()}</p>
    {/if}
    <div class="actions">
      {#if signingIn || view?.state === "signed_out"}
        <Button
          size="compact"
          variant={signingIn ? "secondary" : "primary"}
          pending={busy === "cancel"}
          disabled={signingIn
            ? !!busy && busy !== "sign_in" && busy !== "cancel"
            : !!busy || !!session?.checking}
          onclick={() => void (signingIn ? session?.cancel() : session?.signIn())}
          >{signingIn ? m.ai_chatgpt_cancel() : m.ai_chatgpt_continue()}</Button
        >
      {:else if view?.state === "connected"}
        <Button
          size="compact"
          pending={busy === "models"}
          disabled={!!busy}
          onclick={() => void session?.refreshModels()}>{m.ai_chatgpt_refresh_models()}</Button
        >
        <Button
          size="compact"
          pending={busy === "sign_out"}
          disabled={!!busy}
          onclick={() => void session?.signOut()}>{m.ai_chatgpt_disconnect()}</Button
        >
      {/if}
      {#if session?.observationFailed || session?.pollingPaused || view?.state === "unavailable"}
        <Button
          size="compact"
          pending={session?.checking}
          disabled={!!busy}
          onclick={() => void session?.refresh()}>{m.ai_chatgpt_refresh()}</Button
        >
      {/if}
    </div>
    {#if session?.observationFailed}<p class="fault" role="alert">
        {m.ai_chatgpt_observation_failed()}
      </p>{/if}
    {#if view?.error && !busy}<p class="fault" role="alert">{chatGptFault(view.error)}</p>{/if}
    {#if view?.revocation_unconfirmed}<p class="fault" role="alert">
        {m.ai_chatgpt_revocation()}
      </p>{/if}
    {#if view?.state === "connected" && !signingIn}
      <details class="details">
        <summary>{m.ai_chatgpt_details()}</summary>
        <p>{m.ai_chatgpt_note()}</p>
        <p>{m.ai_chatgpt_quota_unavailable()}</p>
      </details>
    {/if}
  </div>
</SettingsGroup>

<style>
  .chatgpt-account {
    padding-block: 12px 16px;
    padding-inline: 14px 18px;
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

  .status {
    color: var(--color-text);
    font-size: var(--text-page-title);
    font-weight: 500;
  }

  .metadata {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-block-start: 12px;
  }

  .details {
    margin-block-start: 10px;
  }

  summary {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding-block: 4px;
    color: var(--color-muted);
    font-size: var(--text-label);
    list-style: none;
    cursor: default;
  }

  summary::-webkit-details-marker {
    display: none;
  }

  summary::before {
    inline-size: 0;
    block-size: 0;
    border-block: 4px solid transparent;
    border-inline-start: 5px solid currentcolor;
    content: "";
    transition: rotate var(--motion-fast) var(--ease-out);
  }

  .details[open] summary::before {
    rotate: 90deg;
  }

  summary:hover {
    color: var(--color-text);
  }

  summary:focus-visible {
    outline: 2px solid var(--color-ring);
    outline-offset: 2px;
  }

  .details[open] summary {
    margin-block-end: 6px;
  }

  @media (prefers-reduced-motion: reduce) {
    summary::before {
      transition: none;
    }
  }

  .fault {
    margin-block-start: 8px;
    color: var(--color-danger);
  }
</style>
