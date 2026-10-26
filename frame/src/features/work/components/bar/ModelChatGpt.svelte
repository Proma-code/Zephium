<script lang="ts">
  import { onDestroy } from "svelte";
  import { settle } from "$domain/operations";
  import { commands } from "$shared/ipc/bindings";
  import { ChatGptSession, chatGptFault, chatGptStatus } from "$domain/chatgpt";
  import Button from "$shared/ui/Button";
  import * as m from "$shared/i18n/messages";

  let { profile, hasModels }: { profile: string; hasModels: boolean } = $props();
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
  let usagePending = $state(false);
  let usageUnconfirmed = $state(false);
  const lifetime = new AbortController();
  onDestroy(() => lifetime.abort());

  async function openUsage() {
    if (usagePending) return;
    usagePending = true;
    usageUnconfirmed = false;
    const result = await settle(
      Promise.resolve().then(() =>
        commands.browserOpenUrl("https://chatgpt.com/settings/usage", true),
      ),
      5000,
      lifetime.signal,
    );
    if (lifetime.signal.aborted) return;
    usagePending = false;
    usageUnconfirmed = result.outcome !== "applied" && result.outcome !== "no_op";
  }
</script>

<div class="connection" class:empty={!hasModels}>
  <p class="status" aria-live="polite">{chatGptStatus(view, signingIn)}</p>
  {#if signingIn}
    <p>{m.model_chatgpt_pending_note()}</p>
  {:else if view?.state === "signed_out"}
    <p>{m.ai_chatgpt_consent()}</p>
  {:else if view?.state === "connected"}
    <p>{hasModels ? m.model_chatgpt_models() : m.ai_chatgpt_no_models()}</p>
    <p>{m.ai_chatgpt_quota_unavailable()}</p>
  {/if}
  <div class="actions">
    {#if signingIn || view?.state === "signed_out"}
      <Button
        size="compact"
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
      <Button size="compact" pending={usagePending} onclick={() => void openUsage()}
        >{m.ai_chatgpt_usage()}</Button
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
  {#if usageUnconfirmed}<p class="fault" role="alert">
      {m.ai_chatgpt_usage_unconfirmed()}
    </p>{/if}
  {#if view?.revocation_unconfirmed}<p class="fault" role="alert">
      {m.ai_chatgpt_revocation()}
    </p>{/if}
</div>

<style>
  .connection {
    flex: none;
    min-block-size: 0;
    max-block-size: 55%;
    padding: 10px 14px;
    overflow-y: auto;
    overscroll-behavior: contain;
  }

  .connection.empty {
    display: flex;
    flex: 1;
    flex-direction: column;
    align-items: center;
    justify-content: safe center;
    gap: 10px;
    max-block-size: none;
    padding: 12px 24px 24px;
    text-align: center;
  }

  p {
    margin: 0;
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  p + p {
    margin-block-start: 6px;
  }

  .status {
    color: var(--color-text);
    font-weight: 550;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-block-start: 10px;
  }

  .empty .actions {
    justify-content: center;
  }

  .fault {
    margin-block-start: 8px;
    color: var(--color-danger);
  }
</style>
