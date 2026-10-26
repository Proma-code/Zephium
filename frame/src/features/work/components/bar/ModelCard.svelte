<script module lang="ts">
  import type { WorkModelEntry, WorkModelReasoning } from "$shared/ipc/bindings";
  import type { Tier } from "./model-tier";

  export type CardSubject = {
    entry: WorkModelEntry;
    tier: Tier | null;
    levels: readonly WorkModelReasoning[];
    effort: WorkModelReasoning | null;
  };
</script>

<script lang="ts">
  import {
    contextSize,
    dollars,
    effortName,
    modelName,
    modelConnection,
    connectionName,
  } from "$domain/ai";
  import Icon from "$shared/ui/Icon";
  import ProviderMark from "$shared/ui/ProviderMark";
  import * as m from "$shared/i18n/messages";
  import { ArrowDown01Icon } from "../../lib/icons";
  import ModelEffort from "./ModelEffort.svelte";
  import ModelEffortMark from "./ModelEffortMark.svelte";
  import "$shared/ui/Button/control.css";
  import "$shared/ui/Shine/shine.css";

  let {
    subject,
    expanded,
    region,
    ontoggle,
    onEffort,
    onstir,
    pill = $bindable(),
  }: {
    subject: CardSubject;
    /** The card shows the effort control in place of the model. */
    expanded: boolean;
    /** The id of the face the effort button opens. */
    region: string;
    ontoggle: () => void;
    onEffort: (level: WorkModelReasoning) => void;
    /** Something is happening in the effort control: the person is still using it. */
    onstir?: () => void;
    pill?: HTMLElement;
  } = $props();

  const CHARACTERS: Record<Tier, () => string> = {
    light: m.model_character_light,
    balanced: m.model_character_balanced,
    flagship: m.model_character_flagship,
  };

  const name = $derived(modelName(subject.entry));

  /** What the model is like, then only what limits it. */
  const character = $derived.by(() => {
    const { entry, tier } = subject;
    const words: string[] = [];
    const connection = modelConnection(entry);
    if (connection)
      words.push(
        m.model_connection_info({
          source: connectionName(connection),
        }),
      );
    if (tier) words.push(CHARACTERS[tier]());
    else if (entry.model.provider === "compatible") words.push(m.model_character_own());
    if (entry.context_window < 500_000)
      words.push(m.model_context({ size: contextSize(entry.context_window) }));
    if (!entry.supports.vision) words.push(m.model_no_images());
    return words.join(" · ");
  });

  const price = $derived(
    subject.entry.price
      ? m.model_price({
          input: dollars(subject.entry.price.input),
          output: dollars(subject.entry.price.output),
        })
      : undefined,
  );

  const live = $derived(subject.levels.length > 1 && subject.effort !== null);

  /** The control is built while it shows and for the moment it leaves, so its orb never turns unseen. */
  let built = $state(false);
  let face = $state<HTMLElement>();
  $effect(() => {
    if (expanded && built)
      face?.querySelector<HTMLElement>('[role="slider"]')?.focus({ preventScroll: true });
  });
  $effect(() => {
    if (expanded) {
      built = true;
      return;
    }
    const timer = setTimeout(() => (built = false), 360);
    return () => clearTimeout(timer);
  });
</script>

<!-- One card, two faces: the model, or how long it thinks. They never show together. -->
<section
  class="card"
  aria-label={name}
  aria-describedby={character ? `${region}-description` : undefined}
  data-face={expanded ? "effort" : "model"}
>
  {#if character}<span class="sr-only" id={`${region}-description`}>{character}</span>{/if}
  <div class="face model" inert={expanded}>
    {#key subject.entry.id}<div class="identity" aria-hidden="true">
        <span class="tile ui-shine"
          ><ProviderMark provider={subject.entry.model.provider} size={20} /></span
        >
        <span class="who">
          <span class="name" title={price}>{name}</span>
          {#if character}<span class="character">{character}</span>{/if}
        </span>
      </div>{/key}
    {#if live && subject.effort}<button
        bind:this={pill}
        type="button"
        class="pill ui-control ui-shine ui-shine-press"
        aria-expanded={expanded}
        aria-controls={region}
        aria-label={m.model_effort_choose({ level: effortName(subject.effort) })}
        onclick={ontoggle}
      >
        <span class="pill-mark"
          ><ModelEffortMark levels={subject.levels} level={subject.effort} /></span
        >
        <span class="pill-label" data-level={subject.effort}>{effortName(subject.effort)}</span>
        <span class="pill-chevron"><Icon icon={ArrowDown01Icon} size={12} strokeWidth={2.2} /></span
        >
      </button>{:else}<span class="fixed-effort">{m.model_effort_fixed()}</span>{/if}
  </div>
  <div
    class="face effort"
    id={region}
    role="presentation"
    inert={!expanded}
    bind:this={face}
    onpointerdown={onstir}
    onpointermove={onstir}
    onkeydown={onstir}
  >
    {#if built}<ModelEffort
        levels={subject.levels}
        value={subject.effort}
        onchange={onEffort}
      />{/if}
  </div>
</section>

<style>
  .card {
    display: grid;
    flex: none;
    margin: 8px 8px 0;
    border-radius: var(--radius-row);
    background: var(--color-fill);
  }

  .face {
    grid-area: 1 / 1;
    display: flex;
    align-items: center;
    gap: 10px;
    min-inline-size: 0;
    block-size: 60px;
    box-sizing: border-box;
    transition:
      opacity var(--motion-base) var(--ease-out),
      transform var(--motion-slow) var(--ease-emphasized);
  }

  .model {
    padding-inline: 12px 10px;
  }

  .effort {
    opacity: 0;
    transform: translateY(6px) scale(0.98);
  }

  .card[data-face="effort"] .model {
    opacity: 0;
    transform: translateY(-6px) scale(0.98);
  }

  .card[data-face="effort"] .effort {
    opacity: 1;
    transform: none;
  }

  .identity {
    display: flex;
    flex: 1;
    align-items: center;
    gap: 10px;
    min-inline-size: 0;
    animation: card-in var(--motion-base) var(--ease-out);
  }

  .tile {
    --shine-radius: var(--radius-control-compact);

    display: grid;
    flex: none;
    inline-size: 34px;
    block-size: 34px;
    place-items: center;
  }

  .who {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 1px;
    min-inline-size: 0;
  }

  .name {
    overflow: hidden;
    color: var(--color-text);
    font-size: var(--text-body);
    font-weight: 600;
    line-height: 18px;
    letter-spacing: -0.01em;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .character {
    overflow: hidden;
    color: var(--color-muted);
    font-size: var(--text-caption);
    line-height: 14px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .pill {
    --alive-a: color-mix(in oklab, var(--color-soft-sky) 66%, var(--color-text));
    --alive-b: color-mix(in oklab, var(--color-soft-lilac) 66%, var(--color-text));

    flex: none;
    padding-inline: 9px 7px;
    color: var(--color-muted);
  }

  .pill:hover {
    color: var(--color-text);
  }

  .pill-mark {
    display: grid;
    place-items: center;
  }

  .pill-label {
    color: var(--color-text);
    font-weight: 550;
    letter-spacing: -0.003em;
    white-space: nowrap;
  }

  .pill-label[data-level="max"] {
    background: linear-gradient(90deg, var(--alive-a), var(--alive-b));
    background-clip: text;
    color: transparent;
  }

  .pill-chevron {
    display: grid;
    place-items: center;
    opacity: 0.7;
  }

  .fixed-effort {
    flex: none;
    white-space: nowrap;
    padding-inline: 4px;
    color: var(--color-faint);
    font-size: var(--text-caption);
  }

  @keyframes card-in {
    from {
      opacity: 0;
      transform: translateY(3px);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .identity {
      animation: none;
    }

    .face {
      transition: opacity var(--motion-fast) linear;
    }

    .effort,
    .card[data-face="effort"] .model {
      transform: none;
    }
  }
</style>
