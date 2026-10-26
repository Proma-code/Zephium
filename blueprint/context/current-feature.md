# Feature: Windows subscription-account integration

**From build-plan:** feature 1
**Build attempt:** 1 (first file-backed packet; no prior Blueprint history)
**Branch:** windows/subscription-accounts
**Base commit:** aad9ca826a47b3ca3a90830f2e8e603b83713227
**Status:** in progress — adopted after implementation began; not verified
**Scope authority:** user-requested Windows personal development; no PR or merge

## Goal

Add direct official ChatGPT account integration and truthful vendor setup guidance
to the existing Windows application without changing existing API-key workflows
or weakening native security gates.

## In scope

- Zephium-owned official ChatGPT OAuth client and per-profile native account
  lifecycle: start, cancel, status, refresh and disconnect with explicit UI actions.
- Strict OAuth/OIDC validation and bounded callback/network parsing as required by
  the documented official contract; tokens never cross the presentation boundary.
- Account-specific model/inference path, bounded streaming and truthful errors;
  subscription availability is not assumed from executable presence or labels.
- Accessible Settings presentation, repeated-action handling, cancellation,
  navigation/disposal and stale-result suppression.
- Claude/OpenCode original supported routes under the existing native gate.
- Antigravity custom-gateway route labeled pending technical research, without
  unsupported consumer subscription login or entitlement claims.
- Curated local skills, final evidence and independent review handoff.

## Out of scope

MacOS/Linux feature work; removing the CLI qualification gate; using another
app's OAuth client/tokens/cookies; collecting real account credentials for tests;
live consent or paid inference; deleting existing settings; package upgrades
unrelated to this feature; PRs, merges, release or deployment.

## Build loop

This packet was written while code already existed. Do not claim spec-first or
red-before-green history for those changes. Before additional feature behavior,
check it against the acceptance matrix below. Implementation lead owns product
files; other workers use disjoint paths. No automatic checkpoint commits.
Focused checks support each step; final aggregate checks and review determine
completion. A blocked Windows check cannot be satisfied with a browser mock.

## Build steps

- [ ] **1. Direct ChatGPT native lifecycle.** Done when trusted caller/profile
  ownership, project-owned registration, state/PKCE/nonce/identity validation,
  bounded listener/network handling, cancellation and vault behavior meet the
  actual official contract; deterministic tests reject invalid/stale input and
  record redaction; Windows native evidence remains separately required.
- [ ] **2. Account-specific models/inference.** Done when only supported models
  and capabilities are projected, stream/request limits and cancellation/errors
  are tested, and existing API-key/custom-endpoint/model-selection behavior is
  unchanged. Synthetic fixtures do not prove entitlement or live inference.
- [ ] **3. Vendor-route truthfulness.** Done when Claude/OpenCode setup is distinct
  from live linking; the CLI gate remains closed without native qualification;
  Antigravity status says research/gateway limitations rather than unsupported
  automatic consumer login. Focused policy and UI tests cover all labels/states.
- [ ] **4. Settings lifecycle/accessibility.** Done when reachable loading,
  signed-out/connected/unavailable/error states match native observations and
  keyboard, retry, duplicate click, Cancel, navigation and stale-result tests pass.
- [ ] **5. Final qualification and review.** Done when all required evidence in
  `../verification.md` is current for a frozen checkpoint, confirmed findings
  are resolved/re-reviewed, docs match shipped behavior and a fresh reviewer
  records scope and remaining risk. Native Windows gaps stay explicit until run.

## Files / areas

- `crates/zephium-chatgpt/{Cargo.toml,src/}`: OAuth, token, vault, service, inference.
- `crates/zephium-provider-accounts/`: synthetic policy/protocol foundation.
- `crates/zephium-app/src/{work_connections/cli.rs,work_models.rs}`: gate/catalog.
- `desktop/`: typed native commands, trusted caller/profile, bindings generation.
- `frame/src/domain/{chatgpt,ai}/`, `frame/src/features/settings/`, locale messages.
- `docs/subscription-accounts.md`: actual implemented behavior and limitations.
- `.agents/skills/`, `blueprint/`, `docs/skills-*`: curated workflow/evidence only.

## Data / contracts

Native state owns credentials and authoritative account status. IPC may expose
only explicitly approved presentation fields, never tokens or OAuth secrets.
Any displayed account identity is separate from diagnostics and must follow the
actual reviewed privacy contract. Native validation must not rely on a frontend
profile string alone. Operations bind to account/profile and generation; older
completion cannot overwrite newer/canceled/disposed state. Disconnect must not
clear API keys, MCP settings or another profile's account.

Error classification separates denied/expired, transient/offline, timeout,
cancelled, unavailable and protocol/storage failures where the implementation
supports them. Never claim connected/authorized solely because a guide opened,
a login request was issued, or an executable exists.

## Testing

Use `../verification.md` acceptance IDs and commands. Synthetic credentials and
fixtures only. Focused unit/component coverage, final frame aggregate/style/build
and applicable Rust tests/lints are required. Windows compile/runtime, WebView2,
Credential Manager and OAuth callback/browser lifecycle need separate evidence.

## Notes for the AI

Read relevant local skills, not the entire catalog. Existing project instructions
prevail. The code lead must verify official provider contracts and current native
integration, reconcile earlier subscription documentation, then supply actual
check evidence. No assumed pass or unsupported UI promise is acceptable.

## Open questions / qualification gaps

- Exact final native Windows behavior and end-to-end flow are not established by
  this Linux-host adoption snapshot; await supported executor/authorized testing.
- Antigravity custom-gateway contract remains technical research, not an approved
  consumer OAuth implementation. Keep it labeled pending rather than guessing.
- Final independent review target and evidence will be recorded after code freeze;
  the current dirty-tree state is not an immutable receipt.

## Sidebar connection-source refinement (added during implementation)

User screenshot and explicit coexistence requirement were reviewed after the first
Settings implementation. This is additional acceptance, not a retroactive claim
that the previous UI already met it:

- Keep one existing OpenAI provider seat in the sidebar model picker. Add a compact
  API-key / ChatGPT connection-source choice inside that provider's existing pane.
- Keep API-key setup/catalog and subscription setup/catalog independent. Show the
  source on model options and preserve both connections and saved selections.
- Offer the same native explicit consent, cancellation and status from the
  subscription pane, not only a Settings detour. No credentials or OAuth URL in
  renderer state; no login merely from opening the menu or selecting a source.
- Source switch, menu dismissal and profile change must retire only the view's
  owned pending attempt; foreign/cross-profile late results cannot take over.
- Native model routing must honor the selected source without paid fallback to
  another source, including Page/Quick helpers and search. Conservative budget
  accounting must not call a partially streamed refusal uncharged.
- Reuse the original provider rail, Settings/menu primitives, design tokens and
  error patterns; no duplicate OpenAI brand/provider identity or visual overhaul.
- Repeat focused unit/browser tests and aggregate renderer gates after this UI
  refinement. Native Windows and real-account evidence remain separate.


## Remaining-quota requirement (added during implementation)

The user additionally requested remaining quota by provider/plan, including actual
provider windows and reset times. This is **not completed by an unknown row**:

- Show provider-reported windows, percent remaining, reset and last-checked time
  only from a verified, authorized source; absent data is not zero/full quota.
- Infer neither plan windows from the plan's name nor remaining quota from local
  token/cost history. Keep API credit and gateway spend separate from subscription
  quota. Retain last-good readings as stale on transient failure; clear them on
  authoritative account/source replacement.
- Direct SIWC currently has no verified public quota-read contract. The implemented
  direct connection therefore says quota is unavailable and offers the official
  account usage page. This is a disclosed capability limit, not live quota proof.
- Codex app-server and Claude Code status-line parsing are separate potential
  sources; pure fixture parsers do not grant execution or access another app's
  credentials. OpenCode Go and third-party gateways need their own documented
  source. Numeric live quota remains pending a supported connected source.
