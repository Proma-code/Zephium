<script lang="ts">
  import { GatewaySession, gatewayStatus, gatewayFault } from "$domain/gateway";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";
  import IconButton from "$shared/ui/IconButton";
  import { ArrowReloadHorizontalIcon, Plug01Icon, Settings02Icon } from "../../lib/icons";

  let {
    profile,
    hasModels,
    onsettings,
  }: { profile: string; hasModels: boolean; onsettings: () => void } = $props();
  let session = $state.raw<GatewaySession | null>(null);
  $effect(() => {
    const owner = new GatewaySession(profile);
    session = owner;
    void owner.start();
    return () => {
      owner.dispose();
      if (session === owner) session = null;
    };
  });
  const view = $derived(session?.view ?? null);
  const busy = $derived(session?.busy ?? null);
  const configured = $derived(view?.state === "configured" || view?.state === "checked");
  const locked = $derived(!!busy || !!view?.checking || !!session?.observationFailed);
</script>

<div class="connection" class:empty={!hasModels}>
  <div class="summary">
    <p class="status" aria-live="polite">{gatewayStatus(view)}</p>
    {#if configured}
      <div class="tools">
        <IconButton
          icon={ArrowReloadHorizontalIcon}
          label={m.gateway_check()}
          pending={busy === "check" || !!view?.checking}
          disabled={locked}
          onclick={() => void session?.check()}
        />
        <IconButton
          icon={Settings02Icon}
          label={m.gateway_edit()}
          disabled={locked}
          onclick={onsettings}
        />
        <IconButton
          icon={Plug01Icon}
          label={m.gateway_disconnect()}
          pending={busy === "disconnect"}
          disabled={locked}
          onclick={() => void session?.disconnect()}
        />
      </div>
    {/if}
  </div>
  {#if configured}<p class="address" title={view?.label ?? undefined}>{view?.base}</p>{/if}
  <p>{m.gateway_short_provenance()}</p>
  {#if configured}<p>{m.gateway_catalog_only()}</p>
    <p>{m.gateway_unknown_usage()}</p>{/if}
  {#if view?.state === "unconfigured"}
    <p>{m.gateway_settings()}</p>
    <div class="actions">
      <Button size="compact" onclick={onsettings}>{m.gateway_setup()}</Button>
    </div>
  {/if}
  {#if session?.observationFailed || view?.checking || view?.state === "unavailable"}
    <div class="actions">
      <Button
        size="compact"
        pending={session?.checking}
        disabled={!!busy}
        onclick={() => void session?.refresh()}>{m.gateway_refresh()}</Button
      >
    </div>
  {/if}
  {#if session?.observationFailed}<p class="fault" role="alert">
      {m.gateway_observation_failed()}
    </p>{/if}
  {#if view?.error && !busy}<p class="fault" role="alert">{gatewayFault(view.error)}</p>{/if}
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
  }

  .address {
    font-family: var(--font-mono);
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
