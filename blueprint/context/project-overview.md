# Relevant repository overview

Observed during workflow adoption on 2026-10-25; code is changing concurrently.

- Rust workspace, pinned `rust-toolchain.toml` (1.95.0); Tauri 2 desktop host.
- `frame/` is Svelte 5 + TypeScript + Vite, **not SvelteKit or React**.
- Root package uses Node >=24.18.0 <25 and pnpm 11.17.0 at this checkpoint.
- `frame/AGENTS.md` and `docs/frontend.md` govern frontend structure and checks.
- Rust/native code owns durable state, caller identity and secrets. Frame owns
  presentation/transient state. `docs/security-model.md` defines trust zones.
- Existing API keys/model configuration remain separate from subscription accounts.

## Active implementation surfaces

`crates/zephium-chatgpt/` contains in-progress direct OAuth/token/vault/service and
inference work. `crates/zephium-provider-accounts/` contains prior pure policy and
protocol foundations; their presence is not proof that Codex broker login is live.
`crates/zephium-app/src/work_connections/cli.rs` owns the gated CLI discovery path.
Native host integration belongs in `desktop/` and its typed commands/bindings.
Settings changes live in `frame/src/features/settings/`, with account projections
in `frame/src/domain/chatgpt/` and catalog integration in `frame/src/domain/ai/`.

At adoption, these are dirty/untracked work-in-progress paths; the base HEAD is
`aad9ca826a47b3ca3a90830f2e8e603b83713227`. Do not treat this paragraph as a frozen
review receipt. Re-read status/diff and regenerate evidence for the final checkpoint.

`docs/subscription-accounts.md` was observed to describe an earlier Codex-managed
foundation while direct ChatGPT code was being written. The lead must reconcile
that document with the final implemented product; no compatibility claim follows
from this adoption snapshot.
