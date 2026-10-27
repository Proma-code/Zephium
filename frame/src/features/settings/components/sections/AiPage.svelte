<script lang="ts">
  import * as m from "$shared/i18n/messages";
  import type { WorkModelEntry, WorkModelRole } from "$shared/ipc/bindings";
  import { tabs } from "$domain/tabs";
  import {
    ModelsSession,
    entryOf,
    keyedProviders,
    connectionName,
    locked,
    modelName,
    modelConnection,
    pickerGroups,
    providerName,
    usable,
  } from "$domain/ai";
  import Button from "$shared/ui/Button";
  import Select from "$shared/ui/Select";
  import SettingsGroup from "$shared/ui/SettingsGroup";
  import SettingsRow from "$shared/ui/SettingsRow";
  import AiProviderRow from "../AiProviderRow.svelte";
  import AiVoiceRow from "../AiVoiceRow.svelte";
  import AiZephiumRow from "../AiZephiumRow.svelte";
  import CodexAccount from "../CodexAccount.svelte";
  import WorkDecisions from "../WorkDecisions.svelte";

  let profile = $derived(tabs.profile());
  let session = $state.raw<ModelsSession | null>(null);
  $effect(() => {
    const id = profile && profile.kind !== "incognito" ? profile.id : null;
    if (!id) return;
    const owner = new ModelsSession(id);
    session = owner;
    void owner.start();
    return () => {
      owner.dispose();
      if (session === owner) session = null;
    };
  });

  const models = $derived(session?.models ?? null);
  // The person's own server says which models it serves.
  $effect(() => {
    if (usable(models, "compatible")) void session?.listEndpoint();
  });
  const AUTO = "auto";
  let expanded = $state(false);
  const roles: {
    role: Exclude<WorkModelRole, "decision">;
    title: () => string;
    description: () => string;
  }[] = [
    { role: "lead", title: m.ai_role_lead, description: m.ai_role_lead_desc },
    { role: "page", title: m.ai_role_page, description: m.ai_role_page_byok_desc },
    { role: "light", title: m.ai_role_light, description: m.ai_role_light_desc },
  ];
  const anyUsable = $derived(
    !!models &&
      (usable(models, "cloud") || keyedProviders.some((provider) => usable(models, provider))),
  );

  function label(entry: WorkModelEntry) {
    const provider = entry.model.provider;
    const note = [
      entry.supports.vision ? m.ai_capability_vision() : null,
      entry.supports.native_search ? m.ai_capability_search() : null,
      entry.recommended && entry.roles.includes("light") ? m.ai_capability_fast() : null,
    ]
      .filter(Boolean)
      .join(" · ");
    const name =
      provider === "anthropic" ||
      provider === "deep_seek" ||
      provider === "cloud" ||
      provider === "open_ai" ||
      provider === "compatible" ||
      provider === "open_code_go"
        ? modelName(entry)
        : `${entry.display_name} · ${providerName(provider)}`;
    return note ? `${name} · ${note}` : name;
  }

  function connectionLabel(entry: WorkModelEntry | null) {
    const connection = entry && modelConnection(entry);
    return connection ? connectionName(connection) : undefined;
  }

  function roleDescription(row: (typeof roles)[number]) {
    const selected = models?.chosen[row.role];
    const connection =
      selected && /^(chatgpt|gateway|opencode-go)\//u.test(selected)
        ? null
        : connectionLabel(entryOf(models, selected ?? models?.effective[row.role]));
    return connection
      ? `${row.description()} ${m.model_connection_info({ source: connection })}`
      : row.description();
  }

  function options(role: Exclude<WorkModelRole, "decision">) {
    const effective = entryOf(models, models?.effective[role]);
    const automatic = {
      value: AUTO,
      label: effective ? m.ai_role_auto({ model: modelName(effective) }) : m.ai_role_auto_none(),
    };
    const offered = pickerGroups(models, role, [
      ...(session?.endpoint ?? []),
      ...(expanded ? (session?.more ?? []) : []),
    ])
      .ready.flatMap((group) => group.entries)
      .filter((entry) => !/^(chatgpt|gateway|opencode-go)\//u.test(entry.id));
    const chosen = models?.chosen[role];
    const missingAccountChoice =
      (chosen?.startsWith("chatgpt/") ||
        chosen?.startsWith("gateway/") ||
        chosen?.startsWith("opencode-go/")) &&
      !offered.some((entry) => entry.id === chosen)
        ? [
            {
              value: chosen,
              label: m.ai_retired_connection_choice(),
              disabled: true,
            },
          ]
        : [];
    return [
      automatic,
      ...missingAccountChoice,
      ...offered.map((entry) =>
        locked(models, entry.id)
          ? {
              value: entry.id,
              label: m.ai_model_locked({ model: label(entry) }),
              detail: connectionLabel(entry),
              disabled: true,
            }
          : { value: entry.id, label: label(entry), detail: connectionLabel(entry) },
      ),
    ];
  }
</script>

{#if !profile || profile.kind === "incognito"}<p class="ai-note">{m.work_regular_profile()}</p>
{:else if session?.unavailable && !models}<p class="ai-note" role="alert">{m.ai_unavailable()}</p>
{:else}
  {#if !anyUsable}<p class="ai-note">{m.ai_byok_start()}</p>{/if}

  <AiZephiumRow />
  <CodexAccount profile={profile.id} />

  <SettingsGroup title={m.ai_keys_group()} description={m.ai_keys_note()}>
    {#if session}{#each keyedProviders as provider (provider)}<AiProviderRow
          {session}
          {provider}
        />{/each}{/if}
  </SettingsGroup>

  <SettingsGroup
    title={m.ai_roles_group()}
    description={anyUsable ? m.ai_roles_note() : m.ai_roles_none()}
  >
    {#each roles as row (row.role)}
      <SettingsRow title={row.title()} description={roleDescription(row)}
        ><Select
          label={row.title()}
          labelHidden
          options={options(row.role)}
          value={models?.chosen[row.role] ?? AUTO}
          disabled={!anyUsable || !!session?.busy}
          onchange={(value) => void session?.choose(row.role, value === AUTO ? null : value)}
        /></SettingsRow
      >
    {/each}
    <div class="more">
      <Button
        size="compact"
        pending={session?.listingMore}
        onclick={() => {
          expanded = true;
          void session?.listMore(keyedProviders.filter((provider) => usable(models, provider)));
        }}>{m.ai_more_models()}</Button
      >
    </div>
    {#if session?.fault?.action === "more"}<p class="ai-note" role="alert">
        {m.ai_more_failed()}
      </p>{/if}
  </SettingsGroup>
  <AiVoiceRow />
  <WorkDecisions profile={profile.id} />
{/if}

<style>
  .ai-note {
    margin: 0 16px;
    font-size: var(--text-label);
    line-height: 1.5;
    color: var(--color-muted);
  }

  .more {
    padding: 12px 14px;
  }
</style>
