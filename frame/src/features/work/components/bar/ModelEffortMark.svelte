<script lang="ts" generics="T extends string">
  let { levels, level }: { levels: readonly T[]; level: T } = $props();

  const reach = $derived(
    levels.length > 1 ? Math.max(0, levels.indexOf(level)) / (levels.length - 1) : 1,
  );
</script>

<!-- The effort track in small: a groove filled as far as the bead, which runs on gradient at Max. -->
<span class="mark" data-level={level} style:--reach={reach} aria-hidden="true">
  <span class="groove"><span class="fill"></span></span>
  <span class="bead"></span>
</span>

<style>
  .mark {
    --alive-a: color-mix(in oklab, var(--color-soft-sky) 66%, var(--color-text));
    --alive-b: color-mix(in oklab, var(--color-soft-lilac) 66%, var(--color-text));
    --bead: 6px;

    position: relative;
    display: block;
    flex: none;
    inline-size: 22px;
    block-size: 8px;
  }

  .groove {
    position: absolute;
    inset-block: 3px;
    inset-inline: 0;
    overflow: hidden;
    border-radius: var(--radius-capsule);
    background: color-mix(in oklab, currentcolor 22%, transparent);
  }

  .fill {
    position: absolute;
    inset-block: 0;
    inset-inline-start: 0;
    inline-size: calc(var(--bead) / 2 + var(--reach) * (100% - var(--bead)));
    border-radius: inherit;
    background: currentcolor;
    opacity: 0.6;
    transition: inline-size var(--motion-base) var(--ease-out);
  }

  .bead {
    position: absolute;
    inset-block-start: 1px;
    inset-inline-start: calc(var(--reach) * (100% - var(--bead)));
    inline-size: var(--bead);
    block-size: var(--bead);
    border-radius: 50%;
    background: currentcolor;
    transition: inset-inline-start var(--motion-base) var(--ease-out);
  }

  .mark[data-level="max"] :is(.fill, .bead) {
    background: linear-gradient(90deg, var(--alive-a), var(--alive-b));
    opacity: 1;
  }

  @media (prefers-reduced-motion: reduce) {
    .fill,
    .bead {
      transition: none;
    }
  }
</style>
