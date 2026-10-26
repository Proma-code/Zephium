<script lang="ts">
  import { tick } from "svelte";
  import {
    GatewaySession,
    gatewayBaseValid,
    gatewayKeyValid,
    gatewayLabelValid,
    gatewayStatus,
    gatewayFault,
    gatewayCheckedAt,
  } from "$domain/gateway";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";
  import Field from "$shared/ui/Field";

  let { profile }: { profile: string } = $props();
  let session = $state.raw<GatewaySession | null>(null);
  let editing = $state(false);
  let draftConnection = $state<string | null>(null);
  let label = $state("");
  let base = $state("");
  let secret = $state("");
  let form = $state<HTMLFormElement>();
  let editButton = $state<HTMLButtonElement>();
  $effect(() => {
    const owner = new GatewaySession(profile);
    session = owner;
    editing = false;
    label = "";
    base = "";
    secret = "";
    void owner.start();
    return () => {
      owner.dispose();
      secret = "";
      if (session === owner) session = null;
    };
  });
  const view = $derived(session?.view ?? null);
  const busy = $derived(session?.busy ?? null);
  const locked = $derived(!!busy || !!view?.checking || !!session?.observationFailed);
  const configured = $derived(view?.state === "configured" || view?.state === "checked");
  const checkedAt = $derived(gatewayCheckedAt(view));
  const changed = $derived(editing && !!view && draftConnection !== view.connection_id);
  const valid = $derived(
    !changed && gatewayLabelValid(label) && gatewayBaseValid(base) && gatewayKeyValid(secret),
  );

  async function edit() {
    label = view?.label ?? "";
    base = view?.base ?? "";
    secret = "";
    draftConnection = view?.connection_id ?? null;
    editing = true;
    await tick();
    form?.querySelector("input")?.focus();
  }
  async function close() {
    const owner = session;
    editing = false;
    secret = "";
    await tick();
    if (session === owner) editButton?.focus();
  }
  async function save() {
    const owner = session;
    if (!owner || !valid || locked) return;
    const key = secret;
    secret = "";
    const saved = await owner.save(draftConnection, label, base, key);
    if (owner !== session) return;
    if (saved) {
      const restore =
        form?.contains(document.activeElement) || document.activeElement === document.body;
      if (restore) await close();
      else editing = false;
    }
  }
</script>

<div class="gateway">
  <p>{m.gateway_description()}</p>
  <p class="status" aria-live="polite">{gatewayStatus(view)}</p>
  {#if configured}
    <p>{view?.label}</p>
    <p class="address">{view?.base}</p>
    {#if checkedAt}<p>{m.gateway_checked_at({ time: checkedAt })}</p>{/if}
  {/if}
  {#if editing}
    <form
      bind:this={form}
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <Field
        label={m.gateway_label()}
        hint={m.gateway_label_hint()}
        error={label && !gatewayLabelValid(label) ? m.gateway_label_hint() : undefined}
        bind:value={label}
        autocomplete="off"
        disabled={locked}
        maxlength={64}
      />
      <Field
        label={m.gateway_base()}
        hint={m.gateway_base_hint()}
        error={base && !gatewayBaseValid(base) ? m.gateway_base_hint() : undefined}
        bind:value={base}
        type="url"
        placeholder="http://127.0.0.1:8080/v1"
        autocomplete="off"
        spellcheck="false"
        disabled={locked}
        maxlength={64}
      />
      <Field
        label={m.gateway_key()}
        hint={m.gateway_key_hint()}
        error={secret && !gatewayKeyValid(secret) ? m.gateway_key_invalid() : undefined}
        bind:value={secret}
        type="password"
        autocomplete="off"
        spellcheck="false"
        disabled={locked}
        maxlength={1024}
      />
      <p>{m.gateway_replace_note()}</p>
      {#if changed}<p class="fault" role="alert">{m.gateway_fault_changed()}</p>{/if}
      <div class="actions">
        <Button size="compact" disabled={!!busy} onclick={close}>{m.ai_key_cancel()}</Button>
        <Button
          size="compact"
          variant="primary"
          type="submit"
          pending={busy === "save"}
          disabled={!valid || locked}>{m.gateway_save()}</Button
        >
      </div>
    </form>
  {:else if view && view.state !== "unavailable"}
    <div class="actions">
      <Button bind:ref={editButton} size="compact" disabled={locked} onclick={() => void edit()}
        >{configured ? m.gateway_edit() : m.gateway_setup()}</Button
      >
      {#if configured}
        <Button
          size="compact"
          pending={busy === "check" || !!view.checking}
          disabled={locked}
          onclick={() => void session?.check()}>{m.gateway_check()}</Button
        >
        <Button
          size="compact"
          pending={busy === "disconnect"}
          disabled={locked}
          onclick={() => void session?.disconnect()}>{m.gateway_disconnect()}</Button
        >
      {/if}
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
  <p class="note">{m.gateway_check_note()}</p>
  <p>{m.gateway_unknown_usage()}</p>
  <p>{m.gateway_provenance()}</p>
  {#if session?.observationFailed}<p class="fault" role="alert">
      {m.gateway_observation_failed()}
    </p>{/if}
  {#if view?.error && !busy}<p class="fault" role="alert">{gatewayFault(view.error)}</p>{/if}
</div>

<style>
  .gateway {
    padding: 12px 16px 16px;
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
    margin-block-start: 12px;
  }

  .address {
    font-family: var(--font-mono);
    font-size: var(--text-caption);
  }

  form {
    display: grid;
    gap: 10px;
    margin-block-start: 12px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-block-start: 12px;
  }

  .note {
    margin-block-start: 12px;
  }

  .fault {
    color: var(--color-danger);
    margin-block-start: 8px;
  }
</style>
