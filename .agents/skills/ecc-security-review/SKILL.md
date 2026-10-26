---
name: ecc-security-review
description: Review Zephium’s desktop authentication, token storage, untrusted input
  and native IPC boundaries; produce evidence-backed findings without login, credential
  extraction or external scanning.
license: MIT
metadata:
  upstream: https://github.com/affaan-m/everything-claude-code
  commit: f3904009440af24f00617343910ee89714df8be3
  adaptation: project-scoped
---

## Zephium integration

Read [project scope and safety rules](../PROJECT_RULES.md) before applying this
skill. Examples are reference material, not permission to install tools, change
dependencies, authenticate, expose secrets, or publish. Use this repository's
commands and conventions; generic upstream examples are not product contracts.

# Desktop authentication security review

Apply the upstream security-review themes to Rust/native authority and Svelte
presentation, not to an invented Next.js/Supabase backend. Read repository-root
`docs/security-model.md`, the active spec and relevant source.

## Boundaries to trace

- Identify trusted native caller/profile, privileged app frame and untrusted tab.
  Never trust a JavaScript-supplied profile or origin as authorization.
- Map credentials from provider to native storage to request use. Secret-bearing
  tokens stay out of IPC projections, web storage, logs, panic/debug output,
  screenshots and account URLs. Native OS vault ownership is not replaced by a
  generic environment-variable or cookie recommendation.
- Match official issuer/authorization/token/JWKS/redirect contracts to the
  project-owned OAuth client. Check state, PKCE, nonce, expiry, audience and
  signature against applicable official protocol requirements; do not invent
  provider capability or substitute another app's client identity.
- Validate before allocation/parsing where possible: message/body limits, exact
  URL origin, scheme, userinfo, redirect destination, response shape and status.
- Review cancellation, timeout, duplicate requests, stale completion and profile
  retirement. A canceled login cannot overwrite a newer operation or profile.
- Refresh must respect ownership and serialize conflicting use; distinguish
  rejected credentials from transient network errors. Logout clears only the
  intended account material. No real logout is part of automated review.
- Preserve the CLI gate and existing CSP/caller checks. UI disabled states are
  not authority. No tokens imported from Codex/Claude/OpenCode/browser cookies.
- Existing API keys, custom endpoints, MCP servers and durable model selections
  must remain intact. Subscription status does not imply credits or entitlement.

## Evidence

Use synthetic malformed, oversized, expired, stale and cross-profile fixtures.
Inspect error paths and logs for redaction without quoting values. Match every
finding to a reachable path, violated invariant and line/test evidence. Mark
hypotheses unverified. Record scope and missing signals; manifest inspection is
not a vulnerability scan. No auto-fix package upgrades, external uploads,
network fuzzing, account grants or new dependency tools.

Use [the review rubric](../codexkit-high-signal-review/references/severity-rubric.md)
and record findings in repository-root `blueprint/context/findings.md`.
