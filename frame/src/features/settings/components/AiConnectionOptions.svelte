<script lang="ts">
  import * as m from "$shared/i18n/messages";
  let { kind, profile }: { kind: "accounts" | "gateway"; profile: string } = $props();
  let open = $state(false);
  const gateway = $derived(kind === "gateway");
  async function load() {
    if (gateway)
      return {
        kind: "gateway" as const,
        Component: (await import("./LocalGateway.svelte")).default,
      };
    return {
      kind: "accounts" as const,
      Component: (await import("./AiSubscriptions.svelte")).default,
    };
  }
</script>

<details class="other-accounts" bind:open>
  <summary>{gateway ? m.gateway_title() : m.ai_subscriptions_expand()}</summary>
  {#if open}
    {#await load()}
      <p role="status">{gateway ? m.gateway_loading() : m.ai_subscriptions_loading()}</p>
    {:then view}
      {#if view.kind === "gateway"}<view.Component {profile} />
      {:else}<view.Component />{/if}
    {:catch}
      <p role="alert">{gateway ? m.gateway_load_failed() : m.ai_subscriptions_load_failed()}</p>
    {/await}
  {/if}
</details>

<style>
  p {
    margin: 0 0 16px;
    font-size: var(--text-label);
    line-height: 1.5;
    color: var(--color-muted);
  }

  .other-accounts {
    margin-block: 8px 34px;
  }

  /* Match AccountServer's existing advanced-settings disclosure. */
  summary {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding-block: 4px;
    padding-inline: 16px;
    color: var(--color-muted);
    font-size: var(--text-label);
    font-weight: 500;
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

  .other-accounts[open] summary::before {
    rotate: 90deg;
  }

  summary:hover {
    color: var(--color-text);
  }

  summary:focus-visible {
    outline: 2px solid var(--color-ring);
    outline-offset: 2px;
  }

  .other-accounts[open] summary {
    margin-block-end: 10px;
  }

  @media (prefers-reduced-motion: reduce) {
    summary::before {
      transition: none;
    }
  }
</style>
