<script lang="ts">
  import { onMount } from "svelte";
  import { Add01Icon } from "@hugeicons/core-free-icons";
  import type { ProfileEntryView, ProfileRemovalPhaseV1 } from "$shared/ipc/bindings";
  import { drawn, profiles, removalPresentation } from "$domain/profiles";
  import { takeNewRequest } from "$session/profile-sheet.svelte";
  import * as m from "$shared/i18n/messages";
  import Button from "$shared/ui/Button";
  import Icon from "$shared/ui/Icon";
  import ProfileAvatar, { AVATAR_COLORS } from "$shared/ui/ProfileAvatar";
  import ProfileSheet from "../profiles/ProfileSheet.svelte";

  onMount(() => {
    void profiles.retain();
    if (takeNewRequest()) create(true);
    return () => profiles.release();
  });

  const list = $derived(profiles.all());
  const removals = $derived(profiles.removals());
  const recoveryUnavailable = $derived(profiles.removalRecovery() === "unavailable");
  const removalNeedsCheck = $derived(
    removals.some((row) => row.blocked || row.phase === "awaiting_explicit_retry"),
  );
  const currentId = $derived(profiles.current()?.id ?? null);
  let sheetOpen = $state(false);
  let editing = $state.raw<ProfileEntryView | null>(null);
  let switching = $state<string | null>(null);
  let failed = $state<string | null>(null);
  let retrying = $state<string | null>(null);
  let retryUnconfirmed = $state<string | null>(null);
  let rechecking = $state(false);
  let recheckUnconfirmed = $state(false);
  const removalPhase: Record<ProfileRemovalPhaseV1, () => string> = {
    preparing_native: m.profiles_removal_preparing,
    awaiting_explicit_retry: m.profiles_removal_waiting,
    authorizing: m.profiles_removal_authorizing,
    cleanup: m.profiles_removal_cleanup,
    erasing_browser: m.profiles_removal_browser,
    finalizing: m.profiles_removal_finalizing,
    acknowledging_native: m.profiles_removal_acknowledging_native,
    failed_closed: m.profiles_removal_blocked,
  };

  async function retryRemoval(profile: string) {
    if (retrying || rechecking) return;
    retrying = profile;
    retryUnconfirmed = null;
    const admitted = await profiles.retryRemoval(profile);
    // Even "scheduled" is not completion; only canonical progress can change
    // the retained row or confirm which cleanup facts have actually happened.
    if (admitted !== "scheduled") retryUnconfirmed = profile;
    retrying = null;
  }

  async function recheckRemovalInventory() {
    if (retrying || rechecking) return;
    rechecking = true;
    recheckUnconfirmed = false;
    const observed = await profiles.recheckRemovalInventory();
    recheckUnconfirmed = observed !== "scheduled";
    rechecking = false;
  }
  /** A new profile starts on the first colour no other profile wears. */
  const nextColor = $derived(
    Array.from({ length: AVATAR_COLORS }, (_, index) => (index + 1) % AVATAR_COLORS).find(
      (color) => !list.some((profile) => profile.avatar.color === color),
    ) ?? 1,
  );

  let andSwitch = $state(false);
  function create(fromSidebar = false) {
    andSwitch = fromSidebar;
    editing = null;
    sheetOpen = true;
  }

  function edit(profile: ProfileEntryView) {
    editing = profile;
    sheetOpen = true;
  }

  async function switchTo(profile: ProfileEntryView) {
    if (switching) return;
    switching = profile.id;
    failed = null;
    if (!(await profiles.switchTo(profile.id))) failed = profile.id;
    switching = null;
  }
</script>

<section class="profiles" aria-label={m.profiles_list_label()}>
  <ul class="list">
    {#each list as profile (profile.id)}
      {@const here = profile.id === currentId}
      <li class="row" data-current={here || undefined}>
        <ProfileAvatar view={drawn(profile)} size={40} />
        <div class="copy">
          <strong>{profile.name}</strong>
          <small
            >{profile.kind === "default"
              ? m.profiles_kind_default()
              : m.profiles_kind_named()}{#if failed === profile.id}<span
                class="failed"
                role="alert"
              >
                · {m.profiles_failed()}</span
              >{/if}</small
          >
        </div>
        <div class="actions">
          {#if here}<span class="here">{m.profiles_this_window()}</span>
          {:else}<Button
              size="compact"
              pending={switching === profile.id}
              disabled={!!switching}
              aria-label={m.profiles_switch_label({ name: profile.name })}
              onclick={() => void switchTo(profile)}>{m.profiles_switch()}</Button
            >{/if}
          <Button
            size="compact"
            variant="ghost"
            aria-label={m.profiles_edit_label({ name: profile.name })}
            onclick={() => edit(profile)}>{m.profiles_edit()}</Button
          >
        </div>
      </li>
    {/each}
    {#each removals as removal (removal.profile)}
      {@const progress = removalPresentation(removal)}
      <li class="row removal" data-profile={removal.profile}>
        <div class="copy" role="status">
          <strong>{m.profiles_removal_title()}</strong>
          <small>{removalPhase[progress.phase]()}</small>
          {#if progress.fact === "signed_out_cleanup_unconfirmed"}
            <small>{m.profiles_removal_signed_out()}</small>
          {:else if progress.fact === "managed_data_removed"}
            <small>{m.profiles_removal_verified()}</small>
          {:else if progress.fact === "not_configured"}
            <small>{m.profiles_removal_not_configured()}</small>
          {/if}
          {#if retryUnconfirmed === removal.profile}
            <small class="failed" role="alert">{m.profiles_removal_retry_unconfirmed()}</small>
          {/if}
        </div>
        {#if progress.blocked || progress.phase === "awaiting_explicit_retry"}
          <Button
            size="compact"
            pending={retrying === removal.profile}
            disabled={retrying !== null || rechecking || recoveryUnavailable}
            onclick={() => void retryRemoval(removal.profile)}>{m.profiles_removal_retry()}</Button
          >
        {/if}
      </li>
    {/each}
    <li>
      <button type="button" class="add" onclick={() => create()}>
        <span class="add-mark"><Icon icon={Add01Icon} size={16} strokeWidth={1.8} /></span>
        <span>{m.profiles_new()}</span>
      </button>
    </li>
  </ul>
  {#if recoveryUnavailable || removalNeedsCheck}
    <div class="recovery" role="status">
      {#if recoveryUnavailable}<p>{m.profiles_removal_inventory_unavailable()}</p>{/if}
      <Button
        size="compact"
        pending={rechecking}
        disabled={rechecking || retrying !== null}
        onclick={() => void recheckRemovalInventory()}>{m.profiles_removal_recheck()}</Button
      >
      {#if recheckUnconfirmed}<p class="failed" role="alert">
          {m.profiles_removal_retry_unconfirmed()}
        </p>{/if}
    </div>
  {/if}
  <p class="note">{m.profiles_list_note()}</p>
</section>

<ProfileSheet
  bind:open={sheetOpen}
  profile={editing}
  current={editing?.id === currentId}
  color={nextColor}
  {andSwitch}
/>

<style>
  .profiles {
    margin-block-end: 34px;
  }

  .list {
    overflow: hidden;
    margin: 0;
    padding: 0;
    border-radius: var(--radius-card);
    background: var(--color-surface);
    box-shadow: var(--shadow-card);
    list-style: none;
  }

  .row,
  .add {
    position: relative;
    display: flex;
    align-items: center;
    gap: 14px;
    box-sizing: border-box;
    min-block-size: 68px;
    padding: 12px 14px 12px 16px;
  }

  li + li > .add::before,
  li + .row::before {
    position: absolute;
    top: 0;
    block-size: 1px;
    background: var(--color-border);
    content: "";
    inset-inline: 70px 0;
  }

  .copy {
    display: grid;
    flex: 1;
    gap: 2px;
    min-inline-size: 0;
  }

  strong,
  small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  strong {
    color: var(--color-text);
    font-size: var(--text-page-title);
    font-weight: 500;
    line-height: 19px;
    letter-spacing: -0.008em;
  }

  small {
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
  }

  .failed {
    color: var(--color-danger);
  }

  .removal small {
    white-space: normal;
  }

  .recovery {
    display: grid;
    justify-items: start;
    gap: 8px;
    margin: 12px 16px;
    color: var(--color-muted);
    font-size: var(--text-label);
  }

  .recovery p {
    margin: 0;
  }

  .actions {
    display: flex;
    flex: none;
    align-items: center;
    gap: 6px;
  }

  .here {
    padding-inline: 10px;
    color: var(--color-label-secondary);
    font-size: var(--text-label);
    font-weight: 500;
  }

  .add {
    inline-size: 100%;
    min-block-size: 56px;
    border: 0;
    background: transparent;
    color: var(--color-text);
    font: inherit;
    font-size: var(--text-page-title);
    text-align: start;
    transition: background-color var(--motion-fast) var(--ease-out);
  }

  .add:hover {
    background: var(--row-hover);
  }

  .add:focus-visible {
    outline-offset: -2px;
  }

  .add-mark {
    display: grid;
    flex: none;
    inline-size: 40px;
    block-size: 28px;
    color: var(--color-label-secondary);
    place-items: center;
  }

  .note {
    margin: 10px 16px 0;
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
  }
</style>
