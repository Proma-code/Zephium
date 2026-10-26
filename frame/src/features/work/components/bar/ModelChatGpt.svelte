<script lang="ts">
  import { onDestroy } from "svelte";
  import { settle } from "$domain/operations";
  import { commands } from "$shared/ipc/bindings";
  import { ChatGptSession, chatGptFault, chatGptStatus } from "$domain/chatgpt";
  import Button from "$shared/ui/Button";
  import IconButton from "$shared/ui/IconButton";
  import { ArrowReloadHorizontalIcon, ArrowUpRight01Icon, Plug01Icon } from "../../lib/icons";
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
  const status = $derived(chatGptStatus(view, signingIn));
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
  <div class="summary">
    <p class="status" aria-live="polite" title={status}>{status}</p>
    {#if view?.state === "connected" && !signingIn}
      <div class="tools">
        <IconButton
          icon={ArrowReloadHorizontalIcon}
          label={m.ai_chatgpt_refresh_models()}
          pending={busy === "models"}
          disabled={!!busy}
          onclick={() => void session?.refreshModels()}
        />
        <IconButton
          icon={Plug01Icon}
          label={m.ai_chatgpt_disconnect()}
          pending={busy === "sign_out"}
          disabled={!!busy}
          onclick={() => void session?.signOut()}
        />
        <IconButton
          icon={ArrowUpRight01Icon}
          label={m.ai_chatgpt_usage()}
          pending={usagePending}
          onclick={() => void openUsage()}
        />
      </div>
    {/if}
  </div>
  {#if signingIn}
    <p>{m.model_chatgpt_pending_note()}</p>
  {:else if view?.state === "signed_out"}
    <p>{m.ai_chatgpt_consent()}</p>
  {:else if view?.state === "connected"}
    {#if !hasModels}<p>{m.ai_chatgpt_no_models()}</p>{/if}
    <p class="quota">{m.model_chatgpt_quota_unknown()}</p>
  {/if}
  {#if signingIn || view?.state === "signed_out" || session?.observationFailed || session?.pollingPaused || view?.state === "unavailable"}
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
  {/if}
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
    flex: 0 1 auto;
    min-block-size: 0;
    max-block-size: 132px;
    padding: 6px 14px;
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

  .summary {
    display: flex;
    align-items: center;
    gap: 8px;
    min-inline-size: 0;
  }

  .status {
    min-inline-size: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-text);
    font-weight: 550;
  }

  .tools {
    display: flex;
    flex: none;
    gap: 2px;
    margin-inline-start: auto;
  }

  .quota {
    font-size: var(--text-caption);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-block-start: 10px;
  }

  .empty .summary {
    max-inline-size: 100%;
    flex-wrap: wrap;
    justify-content: center;
  }

  .empty .status {
    white-space: normal;
  }

  .empty .tools {
    margin-inline-start: 0;
  }

  .empty .actions {
    justify-content: center;
  }

  .fault {
    margin-block-start: 8px;
    color: var(--color-danger);
  }
</style>
