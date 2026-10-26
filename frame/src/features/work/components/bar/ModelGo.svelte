<script lang="ts">
  import { onDestroy } from "svelte";
  import { GoSession, goStatus, goFault, goBilling } from "$domain/opencode-go";
  import { settle } from "$domain/operations";
  import { commands } from "$shared/ipc/bindings";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";
  import IconButton from "$shared/ui/IconButton";
  import {
    ArrowReloadHorizontalIcon,
    ArrowUpRight01Icon,
    Plug01Icon,
    Settings02Icon,
  } from "../../lib/icons";
  let {
    profile,
    hasModels,
    onsettings,
  }: { profile: string; hasModels: boolean; onsettings: () => void } = $props();
  let session = $state.raw<GoSession | null>(null);
  $effect(() => {
    const owner = new GoSession(profile);
    session = owner;
    void owner.start();
    return () => {
      owner.dispose();
      if (session === owner) session = null;
    };
  });
  const view = $derived(session?.view ?? null);
  const busy = $derived(session?.busy ?? null);
  const configured = $derived(view?.state === "configured" || view?.state === "catalogued");
  const locked = $derived(!!busy || !!view?.checking || !!session?.observationFailed);
  const billing = $derived(goBilling(view));
  let linkPending = $state(false);
  let linkUnconfirmed = $state(false);
  const lifetime = new AbortController();
  onDestroy(() => lifetime.abort());
  async function openConsole() {
    if (linkPending) return;
    linkPending = true;
    linkUnconfirmed = false;
    const result = await settle(
      Promise.resolve().then(() => commands.browserOpenUrl("https://opencode.ai/console/", true)),
      5000,
      lifetime.signal,
    );
    if (lifetime.signal.aborted) return;
    linkPending = false;
    linkUnconfirmed = result.outcome !== "applied" && result.outcome !== "no_op";
  }
</script>

<div class="connection" class:empty={!hasModels}>
  <div class="summary">
    <p class="status" aria-live="polite" title={goStatus(view)}>{goStatus(view)}</p>
    {#if configured}<div class="tools">
        <IconButton
          icon={ArrowReloadHorizontalIcon}
          label={m.go_check()}
          pending={busy === "check" || !!view?.checking}
          disabled={locked}
          onclick={() => void session?.check()}
        />
        <IconButton
          icon={Settings02Icon}
          label={m.go_edit()}
          disabled={locked}
          onclick={onsettings}
        />
        <IconButton
          icon={Plug01Icon}
          label={m.go_disconnect()}
          pending={busy === "disconnect"}
          disabled={locked}
          onclick={() => void session?.disconnect()}
        />
        <IconButton
          icon={ArrowUpRight01Icon}
          label={m.go_console()}
          pending={linkPending}
          onclick={() => void openConsole()}
        />
      </div>{/if}
  </div>
  {#if configured}<p>{m.go_coding_only()}</p>
    <p>{m.go_short_catalog_note()}</p>
    <p>{m.go_usage_unknown()}</p>{/if}
  {#if billing}<p>{billing}</p>
    <p>{m.go_billing_history_note()}</p>{/if}
  {#if view?.state === "unconfigured"}
    <p>{m.go_description()}</p>
    <p>{m.go_settings()}</p>
    <div class="actions"><Button size="compact" onclick={onsettings}>{m.go_setup()}</Button></div>
  {/if}
  {#if session?.observationFailed || view?.checking || view?.state === "unavailable"}
    <div class="actions">
      <Button
        size="compact"
        pending={session?.checking}
        disabled={!!busy}
        onclick={() => void session?.refresh()}>{m.go_refresh()}</Button
      >
    </div>
  {/if}
  {#if linkUnconfirmed}<p class="fault" role="alert">{m.go_link_unconfirmed()}</p>{/if}
  {#if session?.observationFailed}<p class="fault" role="alert">{m.go_observation_failed()}</p>{/if}
  {#if view?.error && !busy}<p class="fault" role="alert">{goFault(view.error)}</p>{/if}
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

  .summary {
    display: flex;
    align-items: center;
    gap: 8px;
    min-inline-size: 0;
  }

  p {
    margin: 0;
    color: var(--color-muted);
    font-size: var(--text-caption);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  p + p {
    margin-block-start: 4px;
  }

  .status {
    color: var(--color-text);
    font-size: var(--text-label);
    font-weight: 550;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .empty .status {
    white-space: normal;
  }

  .tools {
    display: flex;
    flex: none;
    gap: 2px;
    margin-inline-start: auto;
  }

  .actions {
    margin-block-start: 10px;
  }

  .empty .summary {
    flex-wrap: wrap;
    justify-content: center;
  }

  .empty .tools {
    margin-inline-start: 0;
  }

  .fault {
    color: var(--color-danger);
    margin-block-start: 8px;
  }
</style>
