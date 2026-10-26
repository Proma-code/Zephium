# Windows subscription accounts

This personal fork adds a **direct Sign in with ChatGPT** connection on Windows,
separate from existing OpenAI API keys. It is not an API-credit conversion or a
Codex-token import. Official contracts were reviewed on 2026-10-25.

Implementation and synthetic tests are not proof of a live subscription grant.
See [verification](../blueprint/verification.md) for the current tested checkpoint
and [Windows trial](windows-subscription-trial.md) for isolated distribution if
available. Do not replace an existing Zephium installation with an ordinary
upstream-identity build of this branch.

## Connections and models

Settings → AI has a ChatGPT account card. The sidebar model picker keeps one
OpenAI provider and distinguishes its API-key and ChatGPT connection sources.
Each source has its own model identifiers and catalog. API-key configuration,
custom endpoints, existing provider selections, Zephium accounts and MCP servers
are not replaced by subscription sign-in.

Use **Sign in**, review the disclosure, and continue in the system browser. The
native application binds its loopback callback before opening that browser. It
reports connected only after validating and storing the resulting grant. Cancel
retires that exact attempt; closing a view cannot cancel another view's attempt.
The app never asks for a pasted access token or imports another application's
credentials. No login is initiated just by viewing a provider.

Choose a ChatGPT-labelled model separately for each desired Work, Pages or Quick
role. Merely signing in does not change any selected model. Subscription models
use distinct `chatgpt/<slug>` IDs; logging out retains an inactive saved choice
instead of silently switching it to a paid API key. Subscription refusals, usage
limits and unavailable models do not trigger paid API fallback. Browser search
remains available; model-native/paid provider search is not automatically borrowed
from an API key while a subscription role is selected.

The account's model list is fetched from the official account endpoint, not
assumed identical to the API-key catalog. The initial adapter supports text and
bounded function tools. It does not advertise image input, native search or
reasoning-setting controls. Its 32,768-token context / 8,192-token output values
are conservative **local planning budgets**, not claims about vendor capacity or
subscription entitlement. A listed model may still be refused by the provider.

## Remaining quota

The current direct SIWC connection has no verified public quota-read endpoint.
The UI therefore reports that remaining quota is not provided and offers the
[official ChatGPT usage page](https://chatgpt.com/settings/usage). It never renders
unknown as 0%/100%, guesses plan windows, or subtracts local token estimates from
subscription limits. Live numeric quota is a remaining capability, not a feature
proven by this unknown state.

The separate Codex app-server rate-limit fields and Claude Code status-line
windows are not interchangeable with a direct SIWC grant. A parser for their
synthetic payloads is only a foundation until their original authorized source
is connected under a qualified native contract. OpenCode Go's console and gateway
spend are also separate sources, not generic OpenCode or Google subscription quota.

## Provider capability matrix

| Account | Implemented route | Boundary |
| --- | --- | --- |
| ChatGPT | Official own-client OAuth, Windows Credential Manager, account-specific Responses streaming | Local open-source desktop use only; no paid/remote Zephium AI relay. Native Windows and live grant qualification are tracked separately. |
| Claude | Original Claude Code setup guidance; strict synthetic status classification | No Claude.ai OAuth or extraction/reuse of Claude subscription tokens. Native CLI execution remains gated. |
| OpenCode | Official CLI setup guidance and a distinct tool entry | Provider/method/version determine authentication. No guessed auth-list parsing or generic subscription entitlement. Native CLI execution remains gated. |
| Antigravity | Official application guidance and existing API/custom-endpoint alternatives | Google consumer third-party wrapping is not vendor-supported. No private Google OAuth, token bridge, automatic gateway installation or claimed Google account connection. |

Antigravity integration is not technically impossible: external applications and
user-controlled compatible gateways exist. This change does not install or
endorse a gateway, import its Google credentials, or claim the upstream consumer
route is permitted. Existing custom endpoint settings remain independent and do
not become a native Antigravity subscription connection.

## Native security and lifecycle

`zephium-chatgpt` owns the fixed HTTPS transport, per-profile account lifecycle and
protected vault. Main-chrome commands use a small generated, secret-free IPC
module; untrusted tabs cannot invoke account authority. Native admission requires
a current persistent profile. Cancellation additionally identifies the exact
non-secret operation ID, so view disposal cannot cancel a newer attempt.

- OAuth uses a fresh state, nonce and S256 PKCE verifier with a `127.0.0.1` callback
  on an OS-selected port. The first registration uses the documented bootstrap;
  subsequent attempts retain the issued application client ID and stable opaque
  installation ID. Failed first exchanges retain **unverified registration
  metadata**, never fabricate a connected identity.
- Fixed-origin JWT/JWKS validation checks signature, algorithm, issuer, audience,
  expiry, issue time, subject and nonce. Granted plan-usage scope is mandatory.
  Redirects and token-supplied key URLs are not followed; errors are closed codes.
- Tokens never enter IPC, web storage, logs or the normal settings database.
  Windows Credential Manager records are per-profile and trial-namespace scoped.
  Large records use bounded chunks and a manifest published last. A committed
  rotation is adopted even if deleting obsolete chunks fails; that cleanup
  failure remains a visible protected-storage warning.
- Refresh is serialized per profile. Owned tasks keep a one-use refresh and vault
  publication alive if the original IPC/inference waiter disappears. No automatic
  retry of a potentially consumed refresh token or billable inference occurs.
  A crash or a storage failure after server-side rotation can require sign-in
  again; preserving old local bytes cannot guarantee the old refresh still works.
- Sign-out immediately fences output and queued Work leases, clears local active
  tokens, then attempts remote revocation. Failure to confirm revocation is shown.
  Incomplete local cleanup is also shown; retry disconnect to finish cleanup.
  Registration mapping stays, and existing API keys remain untouched.
- Native profile projections retire pending authorization and Work leases even
  if the renderer never sends Cancel. Profile erasure waits for account mutations
  and removes that profile's exact native vault records before the existing
  durable deletion journal can finalize. A cleanup failure remains retryable;
  local profile erasure does not claim remote grant revocation.
- Stale callback/catalog results cannot replace newer operations. Work clients
  hold a grant lease: even an idle old client cannot cross logout and new login.
  Shutdown closes admission and tracks owned mutations; an unproven drain is an
  unclean shutdown, not a successful cleanup claim.
- Streaming is bounded. Only a validated `response.completed` terminal event is
  success; EOF, incomplete and failed responses remain failures even after text.
  Tool calls are not emitted until terminal validation finishes.

The existing command runner gate in
`work_connections::cli::ensure_available()` stays closed. It reports unchecked
rather than falsely missing/signed-out when discovery is prohibited. The pure
Codex app-server codec is an unused foundation, not the live ChatGPT route or
permission to run a generic command. Any future CLI broker needs its own native
qualification and explicit action-time consent.

## Verification and limitations

Tests use synthetic grants, real synthetic RSA signatures, in-memory vaults,
controlled task/commit races and a local loopback HTTP fixture. They cover state,
PKCE, nonce/identity/scope validation, hostile callbacks, token rotation,
postcommit cleanup failures, cancellation, caller-drop, stale results, profile
isolation, source routing and strict stream terminals. They do not sign in to,
sign out from, or run inference against a real account.

Typical focused gates (also see the Windows-only CI workflow):

```sh
cargo test -p zephium-chatgpt -p zephium-provider-accounts -p zephium-ipc --locked
cargo clippy -p zephium-chatgpt -p zephium-provider-accounts --all-targets --locked -- -D warnings
cargo test -p zephium-app --features work-runtime,work-execution --locked work_models::tests
cargo test -p zephium-ipc generated_chatgpt_bindings_are_current --locked
pnpm -C frame check
pnpm -C frame build
cargo xtask check-frame-styles
```

Generated account bindings are produced by
`cargo test -p zephium-ipc export_chatgpt_bindings -- --ignored`; normal tests check
exact drift and the closed native command vocabulary. The existing aggregate IPC
binding generator remains unchanged.

Windows compilation, installer coexistence, system-browser opening, native vault
and WebView behavior must be verified on Windows. Linux unit tests or a browser
preview are not a substitute. A live-account smoke test requires the person's
later explicit sign-in and inference actions; no such grant was performed while
implementing this change. Other-platform feature work is out of scope.

## Official references

- [Open-source Sign in with ChatGPT](https://developers.openai.com/siwc/token-sharing-open-source),
  [sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
  [sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions),
  [models/inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference),
  [preview limits](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).
- [Claude Code CLI](https://code.claude.com/docs/en/cli-reference) and
  [subscription use boundary](https://code.claude.com/docs/en/legal-and-compliance).
- [OpenCode CLI](https://opencode.ai/docs/cli/) and
  [provider-specific configuration](https://opencode.ai/docs/providers/).
- [Antigravity FAQ](https://antigravity.google/docs/faq) and
  [terms](https://antigravity.google/terms).

Plans and preview contracts can change. Connected, model-listed, request-completed
and available-quota are different observations; none implies unlimited access.
