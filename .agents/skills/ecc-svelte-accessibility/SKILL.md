---
name: ecc-svelte-accessibility
description: Apply semantic HTML, accessible status, keyboard and focus patterns to
  Zephium Svelte 5 account settings without introducing React or SvelteKit.
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

# Account-settings accessibility in Svelte

Use repository-root `frame/AGENTS.md` and `docs/frontend.md`; the source skill's
React examples supply accessibility concepts, not an instruction to add React.
Reuse current Svelte 5 and shared primitives, semantic tokens and locale patterns.

- Native buttons for actions, anchors for navigation, connected labels for inputs.
  Keep visible names and accessible names aligned; name icon-only controls.
- Guidance toggles expose `aria-expanded` and target an existing content ID.
  Hidden content must not retain keyboard-focusable descendants.
- Account state is text, not color alone. Announce asynchronous progress and final
  errors with an appropriate live/status region; avoid repetitive announcements.
- Disable/coalesce duplicate pending actions. Explain unavailable/unsupported
  provider states without presenting a guide as a successful account connection.
- Retain keyboard Tab/Shift+Tab and Enter/Space operation. Escape/Close/Cancel
  should dismiss the relevant view, settle pending work safely, and restore focus
  when appropriate. Do not create a custom focus trap where an existing primitive
  already owns it.
- Respect reduced motion and logical CSS. Never use raw HTML for provider errors
  or account strings; never render credentials in any form control or status.

Test through existing browser-component fixtures: open/close guides, retry after
failed native open, repeated clicks, keyboard activation, navigating away during
pending work, and stale resolution after dismissal. Browser tests do not establish
Windows WebView2 accessibility or native OAuth behavior. Record manual native
screen-reader/focus checks as pending until actually observed.
