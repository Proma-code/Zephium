<script module lang="ts">
  import type { WorkModelReasoning } from "$shared/ipc/bindings";

  /** Display-only union; does not broaden Work/provider reasoning admission. */
  export type DisplayEffort = WorkModelReasoning | "none" | "minimal";

  const EFFORTS: readonly WorkModelReasoning[] = ["low", "medium", "high", "xhigh", "max"];

  /** The level a model runs `wanted` at: the closest it takes, the stronger on a tie (as Rust). */
  export function nearestEffort(
    levels: readonly WorkModelReasoning[],
    wanted: WorkModelReasoning,
  ): WorkModelReasoning | null {
    const rank = EFFORTS.indexOf(wanted);
    let best: WorkModelReasoning | null = null;
    for (const level of levels) {
      const gap = Math.abs(EFFORTS.indexOf(level) - rank);
      if (best === null || gap <= Math.abs(EFFORTS.indexOf(best) - rank)) best = level;
    }
    return best;
  }

  function stepEffort<T extends DisplayEffort>(
    levels: readonly T[],
    from: T,
    step: number,
  ): T | null {
    const at = levels.indexOf(from);
    const next = levels[Math.max(0, Math.min(levels.length - 1, at + step))];
    return next && next !== from ? next : null;
  }
</script>

<script lang="ts" generics="T extends DisplayEffort">
  import { Orb } from "$shared/ui/presence";
  import { stillWhenUnseen } from "$shared/lib/still";
  import { effortName } from "$domain/ai";
  import { codexEffort } from "$domain/codex";
  import * as m from "$shared/i18n/messages";

  let {
    levels,
    value,
    onchange,
    oncancel,
  }: {
    levels: readonly T[];
    /** The level the hero's model runs at; null when it takes none. */
    value: T | null;
    onchange: (level: T) => void;
    oncancel?: () => void;
  } = $props();

  const HINTS: Record<DisplayEffort, () => string> = {
    none: m.codex_effort_none_hint,
    minimal: m.codex_effort_minimal_hint,
    low: m.model_effort_low_hint,
    medium: m.model_effort_medium_hint,
    high: m.model_effort_high_hint,
    xhigh: m.model_effort_xhigh_hint,
    max: m.model_effort_max_hint,
  };
  /** How fast the orb turns at each level: it idles at Low and races at Max. */
  const PACE: Record<DisplayEffort, number> = {
    none: 0.12,
    minimal: 0.2,
    low: 0.28,
    medium: 0.5,
    high: 0.8,
    xhigh: 1.2,
    max: 1.75,
  };

  const nameOf = (level: DisplayEffort) =>
    level === "none" || level === "minimal" ? codexEffort(level) : effortName(level);

  const live = $derived(levels.length > 0 && value !== null);
  const index = $derived(value ? levels.indexOf(value) : -1);
  const level = $derived<DisplayEffort>(value ?? "low");
  const reach = $derived(levels.length > 1 ? Math.max(0, index) / (levels.length - 1) : 0);

  let core = $state<HTMLElement>();
  let track = $state<HTMLElement>();
  $effect(() => {
    const rate = live ? PACE[level] : 0.2;
    const host = core;
    if (!host) return;
    // The orb starts its own turn when it mounts; its pace is set once that exists. Only that turn:
    // the CSS transitions on the core keep their own time.
    const pace = () => {
      for (const run of host.getAnimations({ subtree: true }))
        if (!(run instanceof CSSTransition) && !(run instanceof CSSAnimation))
          run.updatePlaybackRate(rate);
    };
    pace();
    queueMicrotask(pace);
  });

  function key(event: KeyboardEvent) {
    if (!live || !value) return;
    const rtl = getComputedStyle(event.currentTarget as Element).direction === "rtl";
    const steps: Record<string, number> = {
      ArrowRight: rtl ? -1 : 1,
      ArrowLeft: rtl ? 1 : -1,
      ArrowUp: 1,
      ArrowDown: -1,
      PageUp: 1,
      PageDown: -1,
    };
    let next: T | null;
    if (event.key === "Home") next = levels[0] ?? null;
    else if (event.key === "End") next = levels.at(-1) ?? null;
    else if (event.key in steps) next = stepEffort(levels, value, steps[event.key]!);
    else return;
    event.preventDefault();
    event.stopPropagation();
    if (next && next !== value) onchange(next);
  }

  const THUMB = 16;
  /** The pointer is down on the track. */
  let pressed = false;
  /** The thumb is in the pointer's hand: it goes where the pointer goes, and settles on a stop at the end. */
  let following = $state(false);
  let follow = $state(0);
  let anchor = 0;
  let grab = 0;
  const shown = $derived(following ? follow : reach);

  /** How far along the track a pointer at `x` is, 0 to 1, with the thumb as wide as its margin. */
  function fractionAt(x: number) {
    const box = track!.getBoundingClientRect();
    const span = Math.max(1, box.width - THUMB);
    const fraction = Math.max(0, Math.min(1, (x - box.left - THUMB / 2) / span));
    return getComputedStyle(track!).direction === "rtl" ? 1 - fraction : fraction;
  }

  function reachFor(fraction: number) {
    const next = levels[Math.round(fraction * (levels.length - 1))];
    if (next && next !== value) onchange(next);
  }

  function down(event: PointerEvent) {
    if (event.button !== 0 || !live || !track) return;
    const element = event.currentTarget as HTMLElement;
    element.setPointerCapture(event.pointerId);
    element.focus({ preventScroll: true });
    pressed = true;
    anchor = event.clientX;
    const thumb = track.querySelector(".thumb")!.getBoundingClientRect();
    if (Math.abs(event.clientX - (thumb.left + thumb.width / 2)) <= thumb.width) {
      // On the thumb: it keeps the pointer's place on it, so it never jumps under the hand.
      grab = event.clientX - (thumb.left + thumb.width / 2);
      follow = reach;
      following = true;
    } else {
      grab = 0;
      reachFor(fractionAt(event.clientX));
    }
  }

  function move(event: PointerEvent) {
    if (!pressed || !track) return;
    if (!following && Math.abs(event.clientX - anchor) > 3) {
      follow = reach;
      following = true;
    }
    if (!following) return;
    follow = fractionAt(event.clientX - grab);
    reachFor(follow);
  }

  function release() {
    pressed = false;
    following = false;
  }
</script>

<div class="effort" data-level={value ?? undefined}>
  <span class="well" aria-hidden="true" use:stillWhenUnseen>
    <span class="halo"></span>
    <span class="core" bind:this={core}><Orb size={36} /></span>
  </span>
  <span class="words">
    <span class="name">{live ? nameOf(level) : m.model_effort_fixed()}</span>
    <span class="hint">{live ? HINTS[level]() : m.model_effort_fixed_hint()}</span>
  </span>
  {#if live && levels.length > 1}<div
      class="track"
      bind:this={track}
      role="slider"
      tabindex="0"
      aria-label={m.model_effort()}
      aria-valuemin={0}
      aria-valuemax={levels.length - 1}
      aria-valuenow={index}
      aria-valuetext={nameOf(level)}
      data-drag={following ? "" : undefined}
      style:--reach={shown}
      onkeydown={key}
      onpointerdown={down}
      onpointermove={move}
      onpointerup={release}
      onpointercancel={() => {
        release();
        oncancel?.();
      }}
      onlostpointercapture={release}
    >
      <span class="groove">
        <span class="fill"></span>
      </span>
      <span class="ticks">
        {#each levels as stop, step (stop)}<span
            class="tick"
            data-on={step <= index ? "" : undefined}
          ></span>{/each}
      </span>
      <span class="thumb"></span>
    </div>{/if}
</div>

<style>
  .effort {
    --alive-a: color-mix(in oklab, var(--color-soft-sky) 66%, var(--color-text));
    --alive-b: color-mix(in oklab, var(--color-soft-lilac) 66%, var(--color-text));

    display: flex;
    align-items: center;
    gap: 10px;
    inline-size: 100%;
    block-size: 100%;
    box-sizing: border-box;
    padding-inline: 12px 8px;
  }

  .well {
    position: relative;
    display: grid;
    flex: none;
    inline-size: 36px;
    block-size: 36px;
    place-items: center;
  }

  /* The orb gathers itself as the effort rises: dimmer and smaller at Low, full at Max. */
  .core {
    --orb-tone: var(--color-text);

    display: grid;
    place-items: center;
    opacity: 0.4;
    transform: scale(0.76);
    transition:
      opacity var(--motion-slow) var(--ease-out),
      transform var(--motion-page) var(--ease-spring);
  }

  [data-level="medium"] .core {
    opacity: 0.66;
    transform: scale(0.84);
  }

  [data-level="high"] .core {
    opacity: 0.86;
    transform: scale(0.92);
  }

  [data-level="xhigh"] .core,
  [data-level="max"] .core {
    opacity: 1;
    transform: scale(1);
  }

  [data-level="xhigh"] .core {
    --orb-tone: linear-gradient(
      180deg,
      color-mix(in oklab, var(--alive-a) 45%, var(--color-text)),
      var(--color-text)
    );
  }

  [data-level="max"] .core {
    --orb-tone: linear-gradient(
      180deg,
      var(--color-text) 10%,
      var(--alive-a) 55%,
      var(--alive-b) 90%
    );
  }

  .halo {
    position: absolute;
    inset: -10px;
    border-radius: 50%;
    background: radial-gradient(
      closest-side,
      color-mix(in oklab, var(--color-soft-lilac) 36%, transparent),
      color-mix(in oklab, var(--color-soft-sky) 14%, transparent) 58%,
      transparent
    );
    opacity: 0;
    transform: scale(0.6);
    transition:
      opacity var(--motion-slow) var(--ease-out),
      transform var(--motion-page) var(--ease-out);
  }

  [data-level="xhigh"] .halo {
    opacity: 0.4;
    transform: scale(0.8);
  }

  [data-level="max"] .halo {
    opacity: 1;
    transform: scale(1);
    animation: breathe 2.6s var(--ease-in-out) infinite alternate;
  }

  .well:global([data-still]) .halo {
    animation-play-state: paused;
  }

  /* The words hold one size for every level, so nothing moves while the thumb does: two lines,
     centred on the orb and the track. */
  .words {
    display: flex;
    flex: 1;
    flex-direction: column;
    justify-content: center;
    gap: 1px;
    min-inline-size: 0;
    block-size: 32px;
    contain: strict;
  }

  .name {
    flex: none;
    color: var(--color-text);
    white-space: nowrap;
    font-size: var(--text-body);
    font-weight: 600;
    line-height: 17px;
    letter-spacing: -0.005em;
  }

  [data-level="max"] .name {
    background: linear-gradient(90deg, var(--alive-a), var(--alive-b));
    background-clip: text;
    color: transparent;
  }

  .hint {
    flex: none;
    block-size: 14px;
    overflow: hidden;
    color: var(--color-muted);
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: var(--text-caption);
    line-height: 14px;
  }

  /* A groove with a bead on it: one detent for each level the model takes. */
  .track {
    --thumb: 16px;
    --span: 100px;
    --way: 1;

    position: relative;
    flex: none;
    inline-size: var(--span);
    block-size: 36px;
    margin-inline-end: 4px;
    outline: none;
    cursor: default;
    touch-action: none;
    contain: layout;
  }

  .track:dir(rtl) {
    --way: -1;
  }

  .groove {
    position: absolute;
    inset-block-start: 14px;
    inset-inline: 0;
    block-size: 6px;
    overflow: hidden;
    border-radius: var(--radius-capsule);
    background: color-mix(in oklab, var(--color-text) 11%, transparent);
    box-shadow: inset 0 0.5px 1px color-mix(in oklab, black 22%, transparent);
  }

  /* Both ride the compositor: the fill is a full-width bar slid under the groove's clip, the thumb
     a bead translated along the track, so a drag never lays anything out. */
  .fill {
    position: absolute;
    inset: 0;
    background: color-mix(in oklab, var(--color-text) 58%, transparent);
    translate: calc(
      var(--way) * (var(--reach) * (var(--span) - var(--thumb)) + var(--thumb) / 2 - var(--span))
    );
    transition: translate var(--motion-base) var(--ease-out);
  }

  [data-level="max"] .fill {
    background: linear-gradient(90deg, var(--alive-a), var(--alive-b));
  }

  .ticks {
    position: absolute;
    inset-block-start: 25px;
    inset-inline: calc(var(--thumb) / 2 - 0.75px);
    display: flex;
    justify-content: space-between;
  }

  .tick {
    inline-size: 1.5px;
    block-size: 4px;
    border-radius: 1px;
    background: var(--color-text);
    opacity: 0.2;
    transition: opacity var(--motion-base) var(--ease-out);
  }

  .tick[data-on] {
    opacity: 0.42;
  }

  .thumb {
    position: absolute;
    inset-block-start: 10px;
    inset-inline-start: 0;
    translate: calc(var(--way) * var(--reach) * (var(--span) - var(--thumb)));
    inline-size: var(--thumb);
    block-size: var(--thumb);
    border-radius: 50%;
    background: var(--color-text);
    box-shadow:
      0 0 0 3px var(--card, var(--color-fill)),
      0 1px 3px color-mix(in oklab, black 36%, transparent);
    transition:
      translate var(--motion-base) var(--ease-out),
      scale var(--motion-fast) var(--ease-out);
  }

  [data-level="max"] .thumb {
    background: linear-gradient(150deg, var(--alive-a), var(--alive-b));
    box-shadow:
      0 0 0 3px var(--card, var(--color-fill)),
      0 0 0 5px color-mix(in oklab, var(--alive-b) 24%, transparent);
  }

  .track[data-drag] .thumb,
  .track[data-drag] .fill {
    transition: none;
  }

  .track:hover .thumb {
    scale: 1.08;
  }

  .track:active .thumb {
    scale: 0.94;
  }

  .track:focus-visible .groove {
    box-shadow: 0 0 0 1.5px var(--color-ring);
  }

  @keyframes breathe {
    from {
      transform: scale(0.9);
    }

    to {
      transform: scale(1.08);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .core,
    .halo,
    .fill,
    .thumb,
    .tick {
      transition: none;
    }

    [data-level="max"] .halo {
      animation: none;
    }
  }

  @media (forced-colors: active) {
    .thumb,
    .tick[data-on] {
      background: CanvasText;
    }

    .fill {
      background: Highlight;
    }
  }
</style>
