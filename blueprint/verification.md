# Verification plan and evidence ledger

This is a **plan with explicit pending evidence**, not a pass report. Created
while implementation was in progress; do not copy earlier reported test counts
without exact command/output and checkpoint. The lead owns product-test execution.

## Acceptance matrix

| ID | Observable acceptance | Required evidence | Adoption status |
| --- | --- | --- | --- |
| A1 | Direct ChatGPT uses own official client; issuer/redirect/PKCE/state/nonce/identity validation rejects adversarial input | OAuth fixture tests + official-contract review | Not independently verified here |
| A2 | Cancel/timeout/disposal/profile change prevents stale completion or token overwrite | Service/state tests + Windows callback teardown | Pending |
| A3 | Tokens remain native and redacted; vault records are isolated by intended profile/account | Token/vault synthetic tests + Windows Credential Manager checks | Pending native evidence |
| A4 | Refresh/disconnect affect only the intended account; failures remain distinguishable | Service tests; synthetic vault/network failure scenarios | Pending |
| A5 | Model/catalog/stream limits and malformed/error paths are bounded; no assumed entitlement | Inference tests; no real paid calls | Pending |
| A6 | API keys, custom endpoints, selected models and MCP data remain unchanged | Targeted regression tests + diff/data ownership review | Pending |
| A7 | Claude/OpenCode keep original routes and native gate; Antigravity accurately pending | Policy/CLI tests + UI labels review | Pending |
| A8 | UI covers keyboard, repeated clicks, retries, Cancel, navigation and stale resolution | Browser component tests and Windows focus review | Pending native evidence |
| A9 | Product docs match actual implementation, unsupported states are visible | Final diff review | Pending |
| A10 | Required aggregate checks are current and independent review is checkpoint-bound | Commands below + separate independent review | Pending |

## Existing commands to run after relevant final changes

From repository root; use the pinned installed toolchains. These are instructions,
not evidence that they ran. Do not invoke a login command to test setup guidance.

```sh
cargo test -p zephium-chatgpt --locked
cargo clippy -p zephium-chatgpt --all-targets --locked -- -D warnings
cargo test -p zephium-provider-accounts --locked
cargo clippy -p zephium-provider-accounts --all-targets --locked -- -D warnings
cargo test -p zephium-app --features work-runtime,work-execution --locked work_connections::cli::tests
cargo fmt --all --check
pnpm -C frame check
pnpm -C frame test:component src/features/settings/tests/AiSubscriptions.component.test.ts
pnpm -C frame test:component src/features/settings/tests/ChatGptAccount.component.test.ts
pnpm -C frame build
cargo xtask check-frame-styles
```

The direct-account component test command applies once the lead's test file is
present; a missing file is **not run**, not a passing zero-test run. Include any
new domain/model lifecycle test selectors and applicable native-contract/export
checks discovered by the lead. Run the complete final checks required by
`frame/AGENTS.md`; narrow selectors do not replace them.

## Windows qualification — not run by this packet

Use an isolated Windows checkout/profile and the reviewed executable. Record
commit and executable hash, toolchain, Windows and WebView2 versions, command exit
codes and sanitized scenario evidence. Validate native compilation and typed
caller restrictions, loopback callback lifecycle, cancellation/close/restart,
per-profile Credential Manager ownership and keyboard/focus behavior. Synthetic
fixtures only under current authorization. Do not alter a user's normal browser
profile or log in/out of a real vendor account. A live flow needs later explicit
account/test authorization; absence of that approval is not permission to bypass.

## Evidence table

Append actual observations; never overwrite failures with an unsupported PASS.
After relevant edits, mark previous evidence stale and rerun applicable checks.

| Time UTC | Check / acceptance IDs | Target SHA or dirty-tree fingerprint | Result | Sanitized evidence / limit |
| --- | --- | --- | --- | --- |
| — | Product check suite | In-progress working tree | NOT RUN by skills curator | Implementation lead records actual output separately |
| — | Windows native qualification | No frozen target yet | NOT RUN | Requires supported Windows executor and isolated authorized test profile |
| — | Real OAuth/account/inference E2E | No target | NOT RUN / not currently authorized | No account grant or paid inference performed |

Skills-pack structural validation is recorded separately in
`../docs/skills-validation.md`; it does not satisfy any application acceptance row.

## Safe manual observation guide (not executed evidence)

In the already-running reviewed test app, inspect Settings → AI. Confirm provider
labels and unavailable/pending explanations, expand/collapse setup help by
keyboard, and confirm existing API-key controls remain accessible. Do not click
through real sign-in/consent or disconnect an existing account. Inspect Cancel,
retry and error behavior only through approved synthetic fixtures. A guide is
not itself a successful test or user acceptance.

## Cloud checkpoint evidence — 2026-10-26 (Windows runtime still pending)

This is implementation evidence, not completed acceptance for all four providers.

- Rust 1.95.0: `cargo test -p zephium-chatgpt --lib --locked`: 63 passed
  at the auth review snapshot (real synthetic RSA and loopback HTTP; no real grant).
- Strict all-target Clippy for `zephium-chatgpt` and
  `zephium-provider-accounts`: passed at that snapshot.
- `work_models::tests` with work-runtime + work-execution: 3 passed. The existing
  17 profile-deletion tests passed; a new erasure-failure/journal retry test is being
  added and its result must be recorded separately before claiming that path.
- Account IPC: 3 passed, 1 intentionally ignored generation command; byte-exact
  generated drift plus closed command/native registration/caller/profile checks.
- Final renderer aggregate: 999 unit tests / 169 files passed; typecheck 0 errors,
  0 warnings; ESLint, Stylelint, Prettier, Knip and production build passed.
  Existing graph budgets remain unchanged; new lazy destinations have measured
  entries. `cargo xtask check-frame-styles`: 138 emitted CSS files passed.
- Windows trial packaging portable checks: 6 Node guards, 4 Rust build-config
  tests, 4 credential namespace tests with trial enabled and 4 without, config and
  credentials strict Clippy, pinned Tauri merged schema and workflow lint passed.
- Nine project-local skills/four pinned sources: inventory/hash/license/reference
  validator passed. This verifies provenance and configuration, not application
  behavior or obedience to every skill.

Separate limitations: Windows compilation/installer execution remains pending CI;
real Windows system-browser/vault/WebView coexistence and an explicit user grant
are not exercised here. The 23 authored browser component regressions could not
run under this cloud Chromium environment; parent-owned CUA synthetic renderer
checks are a distinct, narrower evidence source, not a substitute. Claude and
OpenCode live subscription linking, a newly validated local-gateway workflow and
numeric live provider quota remain unimplemented at this checkpoint.


Additional profile-deletion regressions: 15 tests passed with both work-runtime
and work-execution enabled. The reduced work-runtime-only configuration did not
compile and is not counted as passing. Independent portable review was performed
separately; its detailed records are not published here. This is not final Windows
or full-feature approval. Numeric live quotas and remaining providers stay open.
