# Project plan — Windows Codex integration

Use one original-runtime **Codex** connection for ChatGPT subscription access in
Zephium's existing Windows browser. API keys and separately configured Work roles
remain independent. Do not convert or delete retained direct-account records.

## Scope

- Develop only `windows/subscription-accounts` in the user's personal fork.
- The designated publisher alone advances the branch after exact source review.
  No PR, merge, upstream change or release/deployment is authorized here.
- Windows is the product target; portable tests and isolated real-account protocol
  probes do not substitute for the final Windows/browser journey.
- Other subscription providers are deferred. Preserve their existing configured
  behavior; do not implement new account routes or automatic fallback.
- Use pinned official source contracts and the small reviewed project skill set.
  Never import another application's credentials or put user grants into CI.

The [active feature](context/current-feature.md) owns acceptance. Completion needs
actual final qualification, not a text-only preview or a successful compile.
