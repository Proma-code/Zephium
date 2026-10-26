# Standards for this feature

Existing project rules prevail over generic skill examples.

- Follow `frame/AGENTS.md` and relevant repository documentation. Do not manually
  edit generated IPC bindings; update authoritative Rust contract/export paths.
- Preserve feature/domain boundaries, public `index.ts` entrypoints, transient
  document lifetime, semantic tokens, keyboard access and async settlement.
- Rust: typed errors, narrow public APIs, explicit operation/profile ownership,
  bounded parsing/I/O and no secret-bearing diagnostics. Compilation alone is not
  correctness or native qualification. Use existing dependencies where practical.
- Keep official OAuth authority separate from original CLI credentials and API
  keys. Validate native caller scope independently from UI affordances.
- Tests use deterministic synthetic inputs and repository-native fixtures. Favor
  behavior/contract assertions over implementation mirrors or broad snapshots.
- Changes must stay on the Windows feature scope; no unrelated formatting,
  dependency updates, native gate removal or permission expansion.
- Preserve true check exit statuses. Missing tools/platforms are blocked/not-run,
  not passes. Do not weaken aggregate gates to hide baseline failures.
