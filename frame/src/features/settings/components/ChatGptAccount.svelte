<script lang="ts">
  import { onDestroy } from "svelte";
  import { ChatGptSession, chatGptFault, chatGptStatus } from "$domain/chatgpt";
  import { settle } from "$domain/operations";
  import { commands } from "$shared/ipc/bindings";
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

<SettingsGroup title={m.ai_chatgpt_title()} description={m.ai_chatgpt_note()}>
  <div class="chatgpt-account">
    <p class="status" aria-live="polite">{status}</p>
    {#if view?.state === "connected" && !signingIn}
      <p>
        {view.models
          ? m.ai_chatgpt_available_models({ count: view.models })
          : m.ai_chatgpt_no_models()}
      </p>
      <p>{m.ai_chatgpt_quota_unavailable()}</p>
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
    {#if view?.revocation_unconfirmed}<p class="fault" role="alert">
        {m.ai_chatgpt_revocation()}
      </p>{/if}
    {#if usageUnconfirmed}<p class="fault" role="alert">
        {m.ai_chatgpt_usage_unconfirmed()}
      </p>{/if}
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
