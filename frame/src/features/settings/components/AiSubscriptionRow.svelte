<script lang="ts">
  import { onDestroy } from "svelte";
  import { settle } from "$domain/operations";
  import { commands } from "$shared/ipc/bindings";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";
  import type { SubscriptionGuide } from "../lib/subscription-guides";

  let { guide }: { guide: SubscriptionGuide } = $props();
  const uid = $props.id();
  let expanded = $state(false);
  let pending = $state(false);
  let linkUnconfirmed = $state(false);
  let generation = 0;
  const lifetime = new AbortController();
  onDestroy(() => lifetime.abort());

  function toggle() {
    generation++;
    expanded = !expanded;
    linkUnconfirmed = false;
  }

  async function openGuide() {
    if (pending) return;
    pending = true;
    linkUnconfirmed = false;
    const requestGeneration = generation;
    // Admission is not completion; never interpret opening documentation as sign-in.
    const result = await settle(
      Promise.resolve().then(() => commands.browserOpenUrl(guide.documentation, true)),
      5000,
      lifetime.signal,
    );
    if (lifetime.signal.aborted) return;
    pending = false;
    if (requestGeneration !== generation) return;
    linkUnconfirmed = result.outcome !== "applied" && result.outcome !== "no_op";
  }
</script>

<div class="subscription-row">
  <div class="head">
    <div class="copy">
      <h3>{guide.name()}</h3>
      <p class="status">
        {guide.support === "unsupported"
          ? m.ai_subscriptions_unsupported()
          : m.ai_subscriptions_unavailable()}
      </p>
      <p>{guide.description()}</p>
    </div>
    <Button
      size="compact"
      aria-expanded={expanded}
      aria-controls={expanded ? uid : undefined}
      aria-label={expanded
        ? m.ai_subscriptions_hide({ provider: guide.name() })
        : m.ai_subscriptions_guidance({ provider: guide.name() })}
      onclick={toggle}>{expanded ? m.ai_subscriptions_close() : m.ai_subscriptions_show()}</Button
    >
  </div>
  {#if expanded}
    <div class="guidance" id={uid}>
      {#if guide.support === "native_runner_required"}
        <p>{m.ai_subscriptions_external()}</p>
        <code>{guide.command}</code>
      {/if}
      <p>{guide.instructions()}</p>
      <div class="actions">
        <Button size="compact" {pending} onclick={() => void openGuide()}
          >{m.ai_subscriptions_official_guide({ provider: guide.name() })}</Button
        >
      </div>
      {#if linkUnconfirmed}<p class="fault" role="alert">
          {m.ai_subscriptions_link_unconfirmed()}
        </p>{/if}
    </div>
  {/if}
</div>

<style>
  .subscription-row {
    padding-block: 12px;
    padding-inline: 16px;
  }

  :global(.subscription-row) + .subscription-row {
    border-block-start: 1px solid var(--color-border);
  }

  .head {
    display: flex;
    align-items: flex-start;
    gap: 16px;
  }

  .copy {
    flex: 1;
    min-inline-size: 0;
  }

  h3 {
    margin: 0;
    color: var(--color-text);
    font-size: var(--text-page-title);
    font-weight: 500;
    line-height: 1.5;
  }

  p {
    margin: 4px 0 0;
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
  }

  .status {
    color: var(--color-text);
  }

  .guidance {
    display: grid;
    gap: 8px;
    padding-block-start: 12px;
  }

  code {
    justify-self: start;
    max-inline-size: 100%;
    padding-block: 4px;
    padding-inline: 8px;
    border-radius: var(--radius-inset);
    background: var(--color-fill);
    color: var(--color-text);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    overflow-wrap: anywhere;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    padding-block-start: 4px;
  }

  .fault {
    color: var(--color-danger);
  }
</style>
