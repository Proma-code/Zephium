<script lang="ts">
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

  .assist-actions {
    padding-inline-start: 25px;
  }
</style>
