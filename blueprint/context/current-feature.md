# Feature: Windows Codex subscription integration

**Branch:** `windows/subscription-accounts`
**Status:** integrated qualification in progress; not ready for delivery
**Scope:** personal Windows development; designated publisher only; no PR or merge

## Goal

Connect the user's ChatGPT subscription through the original Codex runtime inside
Zephium's existing browser experience. **Codex** is the only subscription linking
choice; API keys stay separate. Preserve existing non-Codex provider configuration.

## Architecture boundaries

- `zephium-codex-broker`: verified official executable admission, owned process,
  protocol/account lifecycle, original catalog/effort/quota and bounded tool calls.
- Core/Store: canonical Page source selection, profile/grant identity, original
  thread associations and exact dispatch/settlement/removal transactions.
- Desktop: current native caller/profile admission, owned worker queue, delivery
  fencing and composition with the existing Page dispatcher.
- Frame: existing compact picker/Settings, transient state and truthful projections;
  no credentials, provider thread adoption or independent authority.
- Retired connector compatibility: preserve old records and schema versions; erase
  exact local credential targets only on explicit profile deletion. No legacy
  inspection, login, disconnect, refresh or inference route remains.

## Acceptance

- Explicit fresh original login, cancel distinct from logout, protected per-profile
  storage, no borrowed account and no credentials in renderer or CI.
- Original model names and genuinely supported effort choices, including Default
  semantics; no silent clamping, renamed modes or paid/source fallback.
- Original in-browser quota with source/account epoch, optional windows and stale
  observations; unknown is neither zero nor a fabricated refill.
- Same original conversation across normal restart and compatible runtime update.
  Unknown dispatch is never replayed; incompatible history requires an explicit
  new conversation without flattening or deleting the old transcript.
- Browser tools use the existing Page authority/approval/action limits. Text-only
  is an internal milestone, not final parity. Ultra requires its genuine owned
  child lifecycle, not merely an effort enum.
- Explicit removal settles owned processes before authorized cleanup, retains
  partial recovery evidence and never deletes ambient Codex data. Never-configured
  profiles require no CLI launch or newly created Codex home.
- Existing API/Work roles remain separate and usable. Clean install with retained
  trial data must not adopt an old direct grant or alter ordinary Zephium/Codex.
- Actual Windows compilation, generated bindings, browser component/build budgets,
  native synthetic controls and final independent review all pass on exact source.
  Portable mocks and real-account protocol probes are different evidence.

## Working rules

Use disjoint modules and immutable review snapshots. Keep current setup and
architecture documentation concise; detailed test receipts remain private. No
other-provider expansion, automatic runtime download, user-data purge or CI login.
Real account tests require explicit user authorization and remain outside CI.
See [verification](../verification.md) and [connection setup](../../docs/subscription-accounts.md).
