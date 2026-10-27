<script lang="ts">
  import type { CodexPickerObservation } from "../lib/codex-picker";
  import ModelCodex from "../components/bar/ModelCodex.svelte";

  let { observation }: { observation: CodexPickerObservation } = $props();
  let open = $state(true);
  let pane = $state<{ collapse: () => boolean }>();
</script>

<button type="button" onclick={() => (open = !open)}>{open ? "Close picker" : "Open picker"}</button
>
{#if open}
  <button type="button" onclick={() => pane?.collapse()}>Collapse effort</button>
  <ModelCodex
    bind:this={pane}
    {observation}
    listId="gesture-models"
    onchosen={() => (open = false)}
    onstate={() => {}}
    oncount={() => {}}
  />
{/if}
