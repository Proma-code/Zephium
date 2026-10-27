<script lang="ts">
  import type { CodexPickerObservation } from "../lib/codex-picker";
  import ModelCodex from "../components/bar/ModelCodex.svelte";

  let { observation }: { observation: CodexPickerObservation } = $props();
  let open = $state(true);
</script>

<button type="button" onclick={() => (open = !open)}>{open ? "Close picker" : "Open picker"}</button
>
{#if open}
  <ModelCodex
    {observation}
    listId="gesture-models"
    onchosen={() => (open = false)}
    onstate={() => {}}
    oncount={() => {}}
  />
{/if}
