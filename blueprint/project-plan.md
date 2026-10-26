# Project plan — Windows subscription accounts

Extend the existing Zephium desktop application for personal Windows use.
Settings should distinguish direct ChatGPT subscription access, vendor-managed
Claude/OpenCode setup, and custom gateways from separately billed API keys.
Use official supported contracts; never borrow another application's identity or
credentials. Do not remove existing accounts, keys, endpoints, models or MCP data.

The user requested use of curated skills from their saved public repositories.
Keep a small, useful project-local set with pinned provenance, license notices,
and actual spec/review/test application, not a bulk copy of thousands of skills.

## Scope and publication

- Work only on `windows/subscription-accounts` in the user's `Proma-code/Zephium`
  fork. Public-fork visibility and direct branch publication are user-approved.
- No PR (including draft), no merge, no upstream changes, no release/deployment.
- The designated implementation lead owns Git publication. Other workers do not
  change branches, commit, push, or overwrite one another's paths.
- Windows native behavior is the product target; host-side tests are development
  signals only. No macOS or Linux feature implementation.

## Provider targets

1. **ChatGPT:** direct official Sign in with ChatGPT with Zephium-owned client
   registration, native credential ownership, bounded model/inference handling
   and truthful account/error states. No Codex-token import or hosted-service proxy.
2. **Claude/OpenCode:** original vendor-supported routes and truthful setup/status
   guidance; native execution stays gated until its existing requirements are met.
3. **Antigravity:** accurately labeled custom-gateway research only at this point.
   No claim that consumer subscription credentials work in third-party software;
   no token/cookie extraction or speculative OAuth broker.

## Success

Observable acceptance is in the active spec. Completion requires relevant final
checks and honest review/evidence, not just an implementation or successful build.
Any live account approval needed later remains a separate user action.
