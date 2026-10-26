<script lang="ts">
  import { commands } from "$shared/ipc/bindings";
  import Button from "$shared/ui/Button";
  import type { WorkCliRowV1 } from "$shared/ipc/bindings";
  import { serviceKey, serviceMark } from "$domain/connections";
  import Icon from "$shared/ui/Icon";
  import * as m from "$shared/i18n/messages";

  /** A tool, or `null` while its status is being asked. */
  let {
    id,
    cli,
  }: { id: "gh" | "git" | "codex" | "claude" | "opencode"; cli: WorkCliRowV1 | null } = $props();

  const NAME = {
    gh: m.connections_cli_gh,
    git: m.connections_cli_git,
    codex: m.connections_cli_codex,
    claude: m.connections_cli_claude,
    opencode: m.connections_cli_opencode,
  } as const;
  const DOES = {
    gh: m.connections_cli_gh_desc,
    git: m.connections_cli_git_desc,
    codex: m.connections_cli_codex_desc,
    claude: m.connections_cli_claude_desc,
    opencode: m.connections_cli_opencode_desc,
  } as const;
  /** What the person runs in Terminal to sign in. */
  const LOGIN = {
    gh: "gh auth login",
    git: "",
    codex: "codex login",
    claude: "claude auth login",
    opencode: "opencode auth login",
  };

  const status = $derived.by(() => {
    if (!cli) return { tone: "quiet", text: m.connections_checking() };
    switch (cli.status) {
      case "signed_in":
        return {
          tone: "good",
          text: cli.account
            ? m.connections_signed_in_as({ account: cli.account })
            : m.connections_signed_in(),
        };
      case "ready":
        return {
          tone: "good",
          text: cli.account
            ? m.connections_commits_as({ name: cli.account })
            : m.connections_ready(),
        };
      case "signed_out":
        return { tone: "attention", text: m.connections_signed_out(), command: LOGIN[id] };
      case "unknown":
        return { tone: "attention", text: m.connections_unknown() };
      default:
        return null;
    }
  });
  const install = {
    gh: "https://cli.github.com/",
    git: "https://git-scm.com/install/mac",
    codex: "https://developers.openai.com/codex/cli/",
    claude: "https://code.claude.com/docs/en/setup",
    opencode: "https://opencode.ai/docs/",
  };
  let linkFailed = $state(false);
  const missing = $derived(cli?.status === "missing");
</script>

<div class="cli-row" data-missing={missing}>
  <span class="tile" aria-hidden="true"
    ><Icon icon={serviceMark(serviceKey(id))} size={17} strokeWidth={1.5} /></span
  >
  <div class="copy">
    <h3>{NAME[id]()}</h3>
    <p>
      {#if status}<span class="status" data-tone={status.tone}
          ><span class="dot" aria-hidden="true"></span>{status.text}{#if status.command}&ensp;<code
              >{status.command}</code
            >{/if}</span
        >{:else}{DOES[id]()}{/if}
    </p>
    {#if cli?.path}<p class="path" title={cli.path}>{cli.path}</p>{/if}
    {#if linkFailed}<p role="alert">{m.ai_link_failed()}</p>{/if}
  </div>
  {#if missing}<span class="aside">{m.connections_missing()}</span><Button
      size="compact"
      onclick={() =>
        void commands.browserOpenUrl(install[id], true).catch(() => {
          linkFailed = true;
        })}>{m.connections_install()}</Button
    >{:else if cli?.version}<span class="aside version">{cli.version}</span>{/if}
</div>

<style>
  .cli-row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 12px;
    box-sizing: border-box;
    min-height: var(--row-page);
    padding: 12px 18px 12px 14px;
  }

  :global(.cli-row) + .cli-row::before {
    content: "";
    position: absolute;
    inset-inline: 58px 0;
    top: 0;
    height: 1px;
    background: var(--color-border);
  }

  .tile {
    display: grid;
    flex: none;
    inline-size: 32px;
    block-size: 32px;
    border-radius: var(--radius-control);
    background: var(--color-fill);
    color: var(--color-text);
    place-items: center;
  }

  [data-missing="true"] .tile {
    color: var(--color-faint);
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

  [data-missing="true"] h3 {
    color: var(--color-muted);
  }

  p {
    margin: 2px 0 0;
    overflow: hidden;
    color: var(--color-muted);
    font-size: var(--text-label);
    line-height: 1.5;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .dot {
    flex: none;
    inline-size: 6px;
    block-size: 6px;
    border-radius: var(--radius-capsule);
    background: var(--color-faint);
  }

  .status[data-tone="good"] .dot {
    background: var(--color-success);
  }

  .status[data-tone="attention"] .dot {
    background: var(--color-warning);
  }

  code {
    padding: 1px 5px;
    border-radius: var(--radius-inset);
    background: var(--color-fill);
    color: var(--color-text);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
  }

  .aside {
    flex: none;
    color: var(--color-faint);
    font-size: var(--text-label);
  }

  .path {
    font-family: var(--font-mono);
    font-size: var(--text-caption);
  }

  .version {
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    font-variant-numeric: tabular-nums;
  }
</style>
