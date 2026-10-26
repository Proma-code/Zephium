<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import { GoSession, goKeyValid, goStatus, goFault, goBilling } from "$domain/opencode-go";
  import { settle } from "$domain/operations";
  import { commands } from "$shared/ipc/bindings";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";
  import Field from "$shared/ui/Field";
  import Checkbox from "$shared/ui/Checkbox";
  let { profile }: { profile: string } = $props();
  let session = $state.raw<GoSession | null>(null);
  let editing = $state(false);
  let draftConnection = $state<string | null>(null);
  let secret = $state("");
  let acknowledged = $state(false);
  let form = $state<HTMLFormElement>();
  let editButton = $state<HTMLButtonElement>();
  $effect(() => {
    const owner = new GoSession(profile);
    session = owner;
    editing = false;
    secret = "";
    acknowledged = false;
    void owner.start();
    return () => {
      owner.dispose();
      secret = "";
      acknowledged = false;
      if (session === owner) session = null;
    };
  });
  const view = $derived(session?.view ?? null);
  const busy = $derived(session?.busy ?? null);
  const locked = $derived(!!busy || !!view?.checking || !!session?.observationFailed);
  const configured = $derived(view?.state === "configured" || view?.state === "catalogued");
  const changed = $derived(editing && !!view && draftConnection !== view.connection_id);
  const valid = $derived(!changed && acknowledged && goKeyValid(secret.trim()));
  const billing = $derived(goBilling(view));
  async function edit() {
    draftConnection = view?.connection_id ?? null;
    secret = "";
    acknowledged = false;
    editing = true;
    await tick();
    form?.querySelector("input")?.focus();
  }
  async function close() {
    const owner = session;
    editing = false;
    secret = "";
    acknowledged = false;
    await tick();
    if (session === owner) editButton?.focus();
  }
  async function save() {
    const owner = session;
    if (!owner || !valid || locked) return;
    const key = secret.trim();
    secret = "";
    const saved = await owner.save(draftConnection, key, acknowledged);
    if (owner !== session) return;
    if (saved) {
      const restore =
        form?.contains(document.activeElement) || document.activeElement === document.body;
      if (restore) await close();
      else {
        editing = false;
        acknowledged = false;
      }
    }
  }
  let linkPending = $state<"guide" | "console" | null>(null);
  let linkUnconfirmed = $state(false);
  const lifetime = new AbortController();
  onDestroy(() => lifetime.abort());
  async function openDocument(destination: "guide" | "console") {
    if (linkPending) return;
    linkPending = destination;
    linkUnconfirmed = false;
    const result = await settle(
      Promise.resolve().then(() =>
        commands.browserOpenUrl(
          destination === "guide"
            ? "https://opencode.ai/v2/docs/console/go/"
            : "https://opencode.ai/console/",
          true,
        ),
      ),
      5000,
      lifetime.signal,
    );
    if (lifetime.signal.aborted) return;
    linkPending = null;
    linkUnconfirmed = result.outcome !== "applied" && result.outcome !== "no_op";
  }
</script>

<div class="go">
  <p>{m.go_description()}</p>
  <p class="status" aria-live="polite">{goStatus(view)}</p>
  {#if editing}
    <form
      bind:this={form}
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <Field
        label={m.go_key()}
        hint={m.go_key_hint()}
        error={secret && !goKeyValid(secret.trim()) ? m.go_key_invalid() : undefined}
        bind:value={secret}
        oninput={() => (acknowledged = false)}
        type="password"
        autocomplete="off"
        spellcheck="false"
        disabled={locked}
        maxlength={1024}
      />
      <p>{m.go_billing_note()}</p>
      <Checkbox
        label={m.go_acknowledge()}
        checked={acknowledged}
        onchange={(value) => (acknowledged = value)}
        disabled={locked}
      />
      <p>{m.go_replace_note()}</p>
      {#if changed}<p class="fault" role="alert">{m.go_fault_changed()}</p>{/if}
      <div class="actions">
        <Button size="compact" disabled={!!busy} onclick={() => void close()}
          >{m.ai_key_cancel()}</Button
        >
        <Button
          size="compact"
          variant="primary"
          type="submit"
          pending={busy === "save"}
          disabled={!valid || locked}>{m.go_save()}</Button
        >
      </div>
    </form>
  {:else if view && view.state !== "unavailable"}
    <div class="actions">
      <Button bind:ref={editButton} size="compact" disabled={locked} onclick={() => void edit()}
        >{configured ? m.go_edit() : m.go_setup()}</Button
      >
      {#if configured}
        <Button
          size="compact"
          pending={busy === "check" || !!view.checking}
          disabled={locked}
          onclick={() => void session?.check()}>{m.go_check()}</Button
        >
        <Button
          size="compact"
          pending={busy === "disconnect"}
          disabled={locked}
          onclick={() => void session?.disconnect()}>{m.go_disconnect()}</Button
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
        onclick={() => void session?.refresh()}>{m.go_refresh()}</Button
      >
    </div>
  {/if}
  <p class="note">{m.go_catalog_note()}</p>
  <p>{m.go_usage_unknown()}</p>
  {#if billing}<p>{billing}</p>
    <p>{m.go_billing_history_note()}</p>{/if}
  {#if !editing}<p>{m.go_billing_note()}</p>{/if}
  <div class="actions">
    <Button
      size="compact"
      pending={linkPending === "guide"}
      disabled={!!linkPending}
      onclick={() => void openDocument("guide")}>{m.go_guide()}</Button
    >
    <Button
      size="compact"
      pending={linkPending === "console"}
      disabled={!!linkPending}
      onclick={() => void openDocument("console")}>{m.go_console()}</Button
    >
  </div>
  {#if linkUnconfirmed}<p class="fault" role="alert">{m.go_link_unconfirmed()}</p>{/if}
  {#if session?.observationFailed}<p class="fault" role="alert">{m.go_observation_failed()}</p>{/if}
  {#if view?.error && !busy}<p class="fault" role="alert">{goFault(view.error)}</p>{/if}
</div>

<style>
  .go {
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
