<script lang="ts">
  import { onDestroy } from "svelte";
  import { observe } from "$shared/lib/observe";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";
  import Icon from "$shared/ui/Icon";
  import { commands, type PageAssistFaultV1 } from "$shared/ipc/bindings";
  import { faultView } from "../lib/faults";
  import { AlertCircleIcon } from "../lib/icons";
  import "$shared/ui/Shine/shine.css";
  import "./assist.css";

  let { fault, onretry }: { fault: PageAssistFaultV1; onretry?: () => void } = $props();

  const view = $derived(faultView(fault));
  let left = $state(0);
  $effect(() => {
    const seconds = view.retry === "wait" ? (view.wait ?? 0) : 0;
    left = seconds;
    if (seconds <= 0) return;
    const timer = setInterval(() => {
      left -= 1;
      if (left <= 0) clearInterval(timer);
    }, 1000);
    return () => clearInterval(timer);
  });
  const retries = $derived(!!onretry && view.retry !== "no");
  let copying = $state(false);
  let copiedCode = $state<string | null>(null);
  let failedCode = $state<string | null>(null);
  const lifetime = new AbortController();
  onDestroy(() => lifetime.abort());

  async function copyDiagnostic() {
    const code = view.diagnostic?.code;
    if (!code || copying) return;
    copying = true;
    failedCode = null;
    const result = await observe(
      Promise.resolve().then(() => navigator.clipboard.writeText(code)),
      2000,
      lifetime.signal,
    );
    if (lifetime.signal.aborted) return;
    copying = false;
    if (view.diagnostic?.code !== code) return;
    if (result.state === "received") copiedCode = code;
    else failedCode = code;
  }
</script>

<section
  class="fault assist-card ui-shine assist-rise"
  role="alert"
  aria-label={m.assist_fault_label()}
>
  <div class="head">
    <span class="mark" aria-hidden="true"><Icon icon={AlertCircleIcon} size={16} /></span>
    <p>{view.message}</p>
  </div>
  {#if view.diagnostic}
    <div class="diagnostic">
      <code>{view.diagnostic.code}</code>
      <Button size="compact" pending={copying} onclick={() => void copyDiagnostic()}>
        {copiedCode === view.diagnostic.code ? m.assist_copied() : m.assist_copy_diagnostic()}
      </Button>
    </div>
    {#if view.diagnostic.uncertain}<p class="diagnostic-note">
        {m.assist_chatgpt_outcome_unknown()}
      </p>{/if}
    {#if failedCode === view.diagnostic.code}<p class="diagnostic-note" role="status">
        {m.assist_diagnostic_copy_failed()}
      </p>{/if}
  {/if}
  {#if view.fix || retries}
    <div class="assist-actions">
      {#if view.fix}{@const fix = view.fix}<Button
          variant={retries ? "secondary" : "primary"}
          size="compact"
          onclick={() => void commands.runCommand(fix.command).catch(() => {})}>{fix.label}</Button
        >{/if}
      {#if retries}<Button size="compact" disabled={left > 0} onclick={onretry}
          >{left > 0 ? m.assist_retry_in({ seconds: left }) : m.assist_retry()}</Button
        >{/if}
    </div>
  {/if}
</section>

<style>
  .fault {
    padding: 12px 14px;
  }

  .head {
    display: flex;
    gap: 9px;
  }

  .head p {
    color: var(--color-label-secondary);
  }

  .mark {
    display: grid;
    flex: none;
    place-items: center;
    block-size: 19px;
    color: var(--color-warning);
  }

  .diagnostic {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-block-start: 8px;
    padding-inline-start: 25px;
  }

  code {
    color: var(--color-muted);
    font-size: var(--text-caption);
    overflow-wrap: anywhere;
    user-select: text;
  }

  .diagnostic-note {
    margin-block-start: 6px;
    padding-inline-start: 25px;
    color: var(--color-muted);
    font-size: var(--text-caption);
    line-height: 1.5;
  }

  .assist-actions {
    padding-inline-start: 25px;
  }
</style>
