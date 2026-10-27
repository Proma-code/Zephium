<script lang="ts">
  import { onDestroy, tick } from "svelte";
  import {
    type CodexSession,
    loadCodexSession,
    codexEffort,
    codexFault,
    codexStatus,
    codexCapability,
    codexModelSelection,
    supportsCodexEffort,
    codexFavouriteKey,
    filterCodexCatalog,
  } from "$domain/codex";
  import type { CodexEffortSelectionV1, CodexModelV1 } from "$shared/ipc/codex-bindings";
  import { favourites, toggleFavourite } from "$domain/ai";
  import Icon from "$shared/ui/Icon";
  import * as m from "$shared/i18n/messages";
  import { Tick02Icon, StarIcon } from "../../lib/icons";
  import ModelCard, { type NativeCardSubject } from "./ModelCard.svelte";

  let {
    profile,
    onchosen,
    onstate,
    oncount,
    query = "",
    favouritesOnly = false,
    listId,
    onactive = () => {},
  }: {
    profile: string;
    onchosen: () => void;
    onstate: (connected: boolean) => void;
    oncount: (count: number) => void;
    query?: string;
    favouritesOnly?: boolean;
    listId: string;
    onactive?: (id: string | undefined) => void;
  } = $props();
  const uid = $props.id();
  let session = $state.raw<CodexSession | null>(null);
  let loadFailed = $state(false);
  $effect(() => {
    const selectedProfile = profile;
    let disposed = false;
    let owner: CodexSession | null = null;
    loadFailed = false;
    void loadCodexSession()
      .then((loaded) => {
        if (disposed) return;
        owner = new loaded.CodexSession(selectedProfile);
        session = owner;
        void owner.start().catch(() => {
          if (!disposed) loadFailed = true;
        });
      })
      .catch(() => {
        if (!disposed) loadFailed = true;
      });
    return () => {
      disposed = true;
      owner?.dispose();
      if (session === owner) session = null;
    };
  });
  const view = $derived(session?.view ?? null);
  const catalog = $derived(view?.state === "connected" ? view.models : []);
  const starred = $derived(new Set(favourites()));
  const rows = $derived(filterCodexCatalog(catalog, query, favouritesOnly, starred));
  const choice = $derived(view?.page_selected ? view.selection : null);
  let pending = $state<string | null>(null);
  const current = $derived(catalog.find((row) => row.id === (pending ?? choice?.model)) ?? null);
  const card = $derived<NativeCardSubject | null>(
    current
      ? {
          native: {
            id: current.id,
            name: current.name,
            source: m.codex_source(),
            detail: codexCapability(view),
          },
          levels: current.efforts,
          effort:
            choice?.model === current.id
              ? choice.effort
              : current.default_effort !== null
                ? { kind: "default" }
                : null,
          defaultAvailable: current.default_effort !== null,
        }
      : null,
  );
  $effect(() => {
    onstate(view?.state === "connected");
    oncount(catalog.length);
  });
  let active = $state<string | null>(null);
  $effect(() => {
    if (!rows.some((row) => row.id === active)) active = rows[0]?.id ?? null;
    const index = rows.findIndex((row) => row.id === active);
    onactive(index < 0 ? undefined : `${uid}-${index}`);
  });
  let list = $state<HTMLElement>();
  let pill = $state<HTMLElement>();
  let control = $state<{ effortEngaged: () => boolean }>();
  let expanded = $state(false);
  let fold: ReturnType<typeof setTimeout> | undefined;
  export function collapse() {
    clearTimeout(fold);
    if (!expanded) return false;
    expanded = false;
    void tick().then(() => pill?.focus({ preventScroll: true }));
    return true;
  }
  function rest(delay = 3000) {
    clearTimeout(fold);
    if (expanded)
      fold = setTimeout(() => {
        // A focused reader or held pointer has not left this control. The
        // next focus/pointer event can restart rest; do not poll or steal it.
        if (!control?.effortEngaged()) collapse();
      }, delay);
  }
  function toggle() {
    expanded = !expanded;
    rest();
  }
  onDestroy(() => clearTimeout(fold));
  async function choose(row: CodexModelV1) {
    const owner = session;
    if (!owner || owner.busy) return;
    const selection = codexModelSelection(row, choice);
    if (!selection) {
      pending = row.id;
      return;
    }
    if (await owner.select(selection)) onchosen();
  }
  async function effort(level: CodexEffortSelectionV1) {
    const owner = session;
    const row = current;
    if (!owner || !row || owner.busy || !supportsCodexEffort(row, level)) return;
    const wasPending = pending !== null;
    if (await owner.select({ model: row.id, effort: level })) {
      pending = null;
      rest(1600);
      if (wasPending) onchosen();
    }
  }
  export function navigate(event: KeyboardEvent) {
    const target = event.target as HTMLElement;
    if (target.closest('button, [role="slider"]')) return;
    const at = rows.findIndex((row) => row.id === active);
    let index: number | null = null;
    if (event.key === " " && !(target instanceof HTMLInputElement)) {
      const row = rows[at] ?? rows[0];
      if (row) {
        event.preventDefault();
        toggleFavourite(codexFavouriteKey(row.id));
      }
      return;
    }
    if (event.key === "ArrowDown") index = Math.min(rows.length - 1, at + 1);
    else if (event.key === "ArrowUp") index = Math.max(0, at - 1);
    else if (event.key === "Home") index = 0;
    else if (event.key === "End") index = rows.length - 1;
    else if (event.key === "Enter") {
      event.preventDefault();
      const row = rows[at] ?? rows[0];
      if (row) void choose(row);
      return;
    }
    if (index !== null) {
      event.preventDefault();
      active = rows[index]?.id ?? null;
      if (active)
        list
          ?.querySelector(`[data-id="${CSS.escape(active)}"]`)
          ?.scrollIntoView({ block: "nearest" });
    }
  }
</script>

{#if card}
  <ModelCard
    bind:this={control}
    subject={card}
    {expanded}
    region="{uid}-effort"
    bind:pill
    ontoggle={toggle}
    onNativeEffort={(level) => void effort(level)}
    onstir={() => rest()}
  />
  {#if pending && card.effort === null}
    <div class="efforts" role="group" aria-label={m.codex_pick_effort()}>
      {#each card.levels as level (level)}<button
          type="button"
          disabled={!!session?.busy}
          onclick={() => void effort({ kind: "explicit", effort: level })}
          >{codexEffort(level)}</button
        >{/each}
    </div>
  {/if}
{:else}<p class="note" role="status">
    {codexCapability(view) ?? codexStatus(view)}
  </p>{/if}
{#if loadFailed}<p class="note failure" role="alert">{m.codex_load_failed()}</p>{/if}
{#if view?.error}<p class="note failure" role="alert">{codexFault(view.error)}</p>{/if}
{#if session?.observationFailed}<p class="note failure" role="alert">
    {m.codex_observation_failed()}
  </p>{/if}
<!-- Native IDs stay native; these are display rows, never Work model entries. -->
<div
  class="list"
  id={listId}
  role="listbox"
  aria-label={m.codex_model()}
  aria-activedescendant={active && rows.some((row) => row.id === active)
    ? `${uid}-${rows.findIndex((row) => row.id === active)}`
    : undefined}
  tabindex="0"
  bind:this={list}
  onkeydown={navigate}
  onpointermove={(event) => {
    const id = (event.target as Element).closest<HTMLElement>("[data-id]")?.dataset.id;
    if (id) active = id;
  }}
  onpointerleave={() => (active = choice?.model ?? null)}
  onpointerdown={(event) => event.preventDefault()}
  onclick={(event) => {
    const id = (event.target as Element).closest<HTMLElement>("[data-id]")?.dataset.id;
    const row = rows.find((row) => row.id === id);
    if (row) void choose(row);
  }}
>
  {#each rows as row, index (row.id)}
    <div
      class="option"
      id="{uid}-{index}"
      role="option"
      aria-label={row.name}
      data-id={row.id}
      data-active={row.id === active ? "" : undefined}
      aria-selected={row.id === choice?.model}
      aria-disabled={!!session?.busy || undefined}
    >
      <span class="option-name">{row.name}</span>
      <button
        type="button"
        class="star"
        tabindex="-1"
        aria-pressed={starred.has(codexFavouriteKey(row.id))}
        aria-label={starred.has(codexFavouriteKey(row.id))
          ? m.model_unfavourite({ model: row.name })
          : m.model_favourite({ model: row.name })}
        onclick={(event) => {
          event.stopPropagation();
          toggleFavourite(codexFavouriteKey(row.id));
        }}><Icon icon={StarIcon} size={14} strokeWidth={1.8} /></button
      >
      <span class="check" aria-hidden="true"
        >{#if row.id === choice?.model}<Icon
            icon={Tick02Icon}
            size={14}
            strokeWidth={2.2}
          />{/if}</span
      >
    </div>
  {/each}
  {#if !rows.length}<p class="empty">
      {query.trim()
        ? m.model_no_match({ query: query.trim() })
        : favouritesOnly
          ? m.model_favourites_empty()
          : m.codex_picker_settings()}
    </p>{/if}
</div>

<style>
  .list {
    position: relative;
    flex: 1;
    min-block-size: min(102px, 30%);
    margin-block-start: 6px;
    padding: 2px 8px 12px;
    overflow-y: auto;
    overscroll-behavior: contain;
    outline: none;
    scrollbar-width: thin;
    mask-image: linear-gradient(to bottom, black calc(100% - 14px), transparent);
  }

  .option {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    min-block-size: 34px;
    padding-inline: 12px 6px;
    border-radius: var(--radius-row);
    cursor: default;
  }

  .option[data-active] {
    background: var(--row-active);
  }

  .option[aria-disabled="true"] {
    opacity: 0.45;
  }

  .option-name {
    flex: 1;
    min-inline-size: 0;
    overflow: hidden;
    color: var(--color-label-secondary, var(--color-text));
    font-size: var(--text-body);
    font-weight: 450;
    line-height: 18px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .option[aria-selected="true"] .option-name,
  .option[data-active] .option-name {
    color: var(--color-text);
  }

  .option[aria-selected="true"] .option-name {
    font-weight: 550;
  }

  .star {
    display: grid;
    place-items: center;
    inline-size: 24px;
    block-size: 28px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--color-faint);
    cursor: default;
    opacity: 0;
  }

  .star[aria-pressed="true"],
  .star:hover {
    color: var(--color-text);
  }

  .star:focus-visible {
    outline: 2px solid var(--color-ring);
  }

  .option:hover .star,
  .option[data-active] .star,
  .star[aria-pressed="true"] {
    opacity: 1;
  }

  .check {
    display: grid;
    place-items: center;
    inline-size: 28px;
    block-size: 28px;
    color: var(--color-text);
  }

  .empty,
  .note {
    margin: 0;
    padding: 10px 20px;
    color: var(--color-faint);
    font-size: var(--text-label);
    line-height: 16px;
  }

  .note {
    flex: none;
  }

  .failure {
    color: var(--color-danger);
  }

  .efforts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 6px 12px;
  }

  .efforts button {
    border: 0;
    border-radius: var(--radius-control-compact);
    padding: 6px 10px;
    background: var(--color-fill);
    color: var(--color-text);
    font: inherit;
  }

  .efforts button:focus-visible {
    outline: 2px solid var(--color-ring);
  }
</style>
