# Zephium project skill boundaries

These are reviewed, pinned project-local adaptations, not a global installer.
Read only the skill relevant to the current work. The repository's existing
instructions, especially `frame/AGENTS.md`, and current user scope govern.

- One active feature: Windows subscription accounts on
  `windows/subscription-accounts`. Multiple workers share that feature and own
  disjoint paths. Do not switch branches or create competing feature specs.
- Personal development: no pull request, merge, auto-merge, release or deployment.
  Only the designated code lead may commit or push the authorized Windows branch,
  once GitHub write access is available. No skill grants extra Git authority.
- Windows is the only native target in scope. Host-side pure Rust/frontend checks
  are useful but not Windows runtime qualification. No macOS/Linux feature work.
- Preserve the native CLI execution gate, caller/profile scope, CSP, generated
  IPC ownership and credential isolation. Do not read/import another app's tokens.
- ChatGPT target is direct official Sign in with ChatGPT, using Zephium-owned
  registration. Claude/OpenCode stay on their original vendor paths under the
  existing native qualification gate. Antigravity custom-gateway research remains
  pending; no unsupported consumer-login or entitlement claim.
- Tests use synthetic credentials/messages and isolated fixtures. No real login,
  logout, OAuth consent, paid inference, account changes or user browser profile.
- No upstream installer, hook, background monitor, package upgrade, external
  security upload or telemetry is installed. Code snippets are illustrative.
  Never print secret-bearing matches; report redacted path/category only.
- Use existing Rust 1.95 toolchain, pnpm workspace and Svelte 5 architecture.
  Do not add React/SvelteKit, blanket 80% coverage gates or new dependencies merely
  because a generic upstream skill recommends them.
- Read [the active feature](../../blueprint/context/current-feature.md) and
  [verification plan](../../blueprint/verification.md) before changing behavior.
  Record actual output and limitations. A planned command, green compile or
  browser mock is not proof of native account linking.
