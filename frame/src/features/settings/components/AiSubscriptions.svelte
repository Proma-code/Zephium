<script lang="ts">
  import * as m from "$shared/i18n/messages";
  import SettingsGroup from "$shared/ui/SettingsGroup";
  import { subscriptionGuides } from "../lib/subscription-guides";
  import AiSubscriptionRow from "./AiSubscriptionRow.svelte";
</script>

<SettingsGroup title={m.ai_subscriptions_group()} description={m.ai_subscriptions_note()}>
  <div class="notice">
    <p>{m.ai_subscriptions_blocked()}</p>
    <p>{m.ai_subscriptions_privacy()}</p>
  </div>
  {#each subscriptionGuides as guide (guide.id)}<AiSubscriptionRow {guide} />{/each}
</SettingsGroup>

<style>
  .notice {
    padding-block: 12px;
    padding-inline: 16px;
    border-block-end: 1px solid var(--color-border);
  }

  p {
    margin: 0;
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
  }

  p + p {
    margin-block-start: 8px;
  }
</style>
