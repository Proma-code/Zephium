<script lang="ts">
  import {
    codexQuotaProjection,
    codexQuotaAt,
    codexQuotaPercent,
    codexQuotaDate,
    codexQuotaDuration,
  } from "$domain/codex";
  import type { CodexQuotaPlanV1 } from "$shared/ipc/codex-bindings";
  import Button from "$shared/ui/Button";
  import * as m from "$shared/i18n/messages";

  const planLabels: Record<CodexQuotaPlanV1, () => string> = {
    free: () => m.codex_quota_plan_free(),
    go: () => m.codex_quota_plan_go(),
    plus: () => m.codex_quota_plan_plus(),
    pro: () => m.codex_quota_plan_pro(),
    team: () => m.codex_quota_plan_team(),
    business: () => m.codex_quota_plan_business(),
    enterprise: () => m.codex_quota_plan_enterprise(),
    education: () => m.codex_quota_plan_education(),
    other: () => m.codex_quota_plan_other(),
  };

  let {
    value,
    busy = false,
    onrefresh,
  }: { value: unknown; busy?: boolean; onrefresh: () => void } = $props();
  // The native observer uses the same 30-second freshness horizon. Aging only
  // changes presentation; it never refills a window or starts a provider read.
  const quota = $derived(codexQuotaAt(codexQuotaProjection(value), Date.now(), 30_000));
  const observed = $derived(codexQuotaDate(quota.observed_at_ms));
</script>

<section class="quota" aria-label={m.codex_quota_title()}>
  <div class="heading">
    <h3>{m.codex_quota_title()}</h3>
    <Button
      size="compact"
      pending={quota.checking}
      disabled={busy || quota.checking}
      onclick={onrefresh}
    >
      {m.codex_quota_refresh()}
    </Button>
  </div>
  <p class="source">
    {m.codex_quota_source()}{#if observed}
      · {m.codex_quota_checked({ time: observed })}{/if}
  </p>
  {#if quota.state === "stale"}<p class="stale">{m.codex_quota_stale()}</p>{/if}
  {#each quota.buckets as bucket, index (bucket.id ?? index)}
    <div class="bucket">
      <h4>{bucket.name ?? bucket.id ?? m.codex_quota_bucket()}</h4>
      {#if bucket.plan !== null}<p>
          {m.codex_quota_plan({ plan: planLabels[bucket.plan]() })}
        </p>{/if}
      {#each bucket.windows as window (window.kind)}
        {@const remaining = codexQuotaPercent(window.remaining_percent)}
        {@const reset = codexQuotaDate(window.resets_at_ms)}
        {@const duration = codexQuotaDuration(window.duration_minutes)}
        <div class="window">
          <div class="window-heading">
            <span
              >{duration?.unit === "day"
                ? m.codex_quota_days({ count: duration.count })
                : duration?.unit === "hour"
                  ? m.codex_quota_hours({ count: duration.count })
                  : duration?.unit === "minute"
                    ? m.codex_quota_duration({ minutes: duration.count })
                    : window.kind === "primary"
                      ? m.codex_quota_primary()
                      : m.codex_quota_secondary()}</span
            >
            <strong
              >{remaining !== null
                ? m.codex_quota_remaining({ percent: remaining })
                : m.codex_quota_unknown()}</strong
            >
          </div>
          {#if reset}<p>{m.codex_quota_reset({ time: reset })}</p>{/if}
        </div>
      {/each}
    </div>
  {/each}
  {#if quota.error}
    <p class="fault" role="status">{m.codex_quota_failed()}</p>
  {:else if quota.checking && quota.buckets.length === 0}
    <p role="status">{m.codex_quota_checking()}</p>
  {:else if quota.unavailable === "not_reported"}
    <p>{m.codex_quota_not_reported()}</p>
  {:else if quota.buckets.length === 0}
    <p>{m.codex_quota_not_checked()}</p>
  {/if}
</section>

<style>
  .quota {
    border-block-start: 1px solid var(--color-border);
    margin-block-start: 12px;
    padding-block-start: 10px;
  }

  .heading,
  .window-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    flex-wrap: wrap;
  }

  h3,
  h4,
  p {
    margin: 0;
    font-size: var(--text-label);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  h3,
  h4,
  strong {
    color: var(--color-text);
    font-weight: 500;
  }

  p,
  .window-heading {
    color: var(--color-muted);
    font-size: var(--text-label);
  }

  .window {
    margin-block-start: 4px;
  }

  .source,
  .stale,
  .bucket,
  .window + .window,
  .fault {
    margin-block-start: 6px;
  }

  .fault {
    color: var(--color-danger);
  }
</style>
