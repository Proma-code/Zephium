<script lang="ts">
  import * as m from "$shared/i18n/messages";
  import type { WorkServerDraftV1 } from "$shared/ipc/bindings";
  import { tabs } from "$domain/tabs";
  import { ConnectionsSession } from "$domain/connections";
  import Button from "$shared/ui/Button";
  import Icon from "$shared/ui/Icon";
  import SettingsGroup from "$shared/ui/SettingsGroup";
  import Plug01Icon from "@hugeicons/core-free-icons/Plug01Icon";
  import CliRow from "../connections/CliRow.svelte";
  import ServerEditor from "../connections/ServerEditor.svelte";
  import ServerRow from "../connections/ServerRow.svelte";

  const profile = $derived(tabs.profile());
  let session = $state.raw<ConnectionsSession | null>(null);
  $effect(() => {
    const id = profile && profile.kind !== "incognito" ? profile.id : null;
    if (!id) return;
    const owner = new ConnectionsSession(id);
    session = owner;
    void owner.start();
    return () => {
      owner.dispose();
      if (session === owner) session = null;
    };
  });

  const TOOLS = ["gh", "git", "codex", "claude", "opencode"] as const;
  const servers = $derived(session?.servers ?? []);
  const taken = $derived(servers.map((row) => row.server.id));
  let adding = $state(false);

  async function add(draft: WorkServerDraftV1) {
    if (await session?.save(draft)) adding = false;
  }
</script>

{#if !profile || profile.kind === "incognito"}
  <p class="connections-note">{m.work_regular_profile()}</p>
{:else if session?.unavailable}
  <p class="connections-note" role="alert">{m.connections_unavailable()}</p>
{:else}
  <SettingsGroup title={m.connections_mac_group()} description={m.connections_mac_note()}>
    {#each TOOLS as id (id)}<CliRow
        {id}
        cli={session?.clis?.find((row) => row.id === id) ?? null}
      />{/each}
  </SettingsGroup>

  <SettingsGroup title={m.connections_servers_group()} description={m.connections_servers_note()}>
    {#if session}{#each servers as row (row.server.id)}<ServerRow
          {session}
          {row}
          {taken}
        />{/each}{/if}
    <div class="server-add" data-open={adding}>
      <div class="head">
        <span class="tile" aria-hidden="true"
          ><Icon icon={Plug01Icon} size={17} strokeWidth={1.5} /></span
        >
        <div class="copy">
          <h3>{m.connections_add_title()}</h3>
          <p>{m.connections_add_desc()}</p>
        </div>
        {#if !adding}<Button size="compact" disabled={!session} onclick={() => (adding = true)}
            >{m.connections_add()}</Button
          >{/if}
      </div>
      {#if adding}<ServerEditor
          editing={null}
          {taken}
          saving={session?.busy === "save"}
          onpreview={(draft) => session?.preview(draft) ?? Promise.resolve(null)}
          onsave={(draft) => void add(draft)}
          oncancel={() => {
            void session?.cancelPreview();
            adding = false;
          }}
        />{/if}
    </div>
  </SettingsGroup>
  {#if session?.failed === "save" || session?.failed?.startsWith("remove:")}<p
      class="connections-note"
      role="alert"
    >
      {m.connections_save_failed()}
    </p>{/if}
{/if}

<style>
  .connections-note {
    margin: -20px 16px 28px;
    font-size: var(--text-label);
    line-height: 1.5;
    color: var(--color-danger);
  }

  .connections-note:first-child {
    margin: 0 16px;
    color: var(--color-muted);
  }

  .server-add {
    position: relative;
    box-sizing: border-box;
    padding: 12px 18px 12px 14px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: calc(var(--row-page) - 24px);
  }

  .tile {
    display: grid;
    flex: none;
    inline-size: 32px;
    block-size: 32px;
    border: 1px dashed var(--color-border-strong);
    border-radius: var(--radius-control);
    box-sizing: border-box;
    color: var(--color-muted);
    place-items: center;
  }

  .copy {
    flex: 1;
    min-width: 0;
  }

  h3 {
    margin: 0;
    color: var(--color-text);
    font-size: var(--text-page-title);
    font-weight: 500;
    line-height: 19px;
    letter-spacing: -0.008em;
  }

  p {
    margin: 2px 0 0;
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
  }
</style>
