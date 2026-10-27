# Repository overview

- Rust workspace, pinned toolchain, Tauri desktop host.
- `frame/`: Svelte 5, TypeScript and Vite; follow `frame/AGENTS.md` and
  `docs/frontend.md`. Do not add another frontend framework.
- Native code owns durable state, identity and secrets. Frame owns presentation
  and document-local state. `docs/security-model.md` defines trust zones.

## Current feature surfaces

`zephium-codex-broker` owns original-runtime admission, process and protocol
custody. Core/Store own durable account/thread/source/removal contracts. Desktop
joins those capabilities to the existing Page/browser authority. The frame's
`domain/codex` and lazy `features/codex` project state without granting authority.

The old `zephium-chatgpt` production surface is retained-account recovery only;
its offline protocol fixtures do not enable direct login or inference. Existing
API/Work settings remain separate from the Codex sidebar selection.

See the [active acceptance](current-feature.md) and
[connection guide](../../docs/subscription-accounts.md). Read actual source and
exact test receipts before making a compatibility or readiness claim.
