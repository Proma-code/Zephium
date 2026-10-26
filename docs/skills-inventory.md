# Curated Zephium project skills

**9 skills from 4 pinned public repositories.** Installed locally in `.agents/skills/`.
These are selected project adaptations, not a bulk/global install. Namespaced entrypoints
avoid collisions; no third-party executable scripts, hooks or installer were installed.

## Sources and retained licenses

| Source | Immutable commit | License notice |
| --- | --- | --- |
| [aiblueprinthq/ai-blueprint](https://github.com/aiblueprinthq/ai-blueprint/tree/a31461341770fe2fc88b0372fc9f49f15acda652) | `a31461341770fe2fc88b0372fc9f49f15acda652` | [MIT](../.agents/skills/licenses/blueprint-LICENSE) |
| [hoavdc/CodexKit](https://github.com/hoavdc/CodexKit/tree/a41f58f34313fad3d150b97c4342e83558aae282) | `a41f58f34313fad3d150b97c4342e83558aae282` | [MIT](../.agents/skills/licenses/codexkit-LICENSE) |
| [affaan-m/everything-claude-code](https://github.com/affaan-m/everything-claude-code/tree/f3904009440af24f00617343910ee89714df8be3) | `f3904009440af24f00617343910ee89714df8be3` | [MIT](../.agents/skills/licenses/ecc-LICENSE) |
| [sickn33/antigravity-awesome-skills](https://github.com/sickn33/antigravity-awesome-skills/tree/e8d346bcb691225fb6ae51a49c353bcedb2ef61d) | `e8d346bcb691225fb6ae51a49c353bcedb2ef61d` | [MIT](../.agents/skills/licenses/antigravity-LICENSE) |

No per-skill LICENSE/NOTICE overrides were found in the selected source directories.
Root MIT notices are retained byte-for-byte. Zephium’s own license is unchanged;
third-party skill documentation keeps its notices. This inventory does not relicense
application code. Full source paths, hashes, adaptations and installed-file hashes are
in [skills-manifest.json](skills-manifest.json).

## Installed skills and actual application

“Applied” below means used to produce this feature’s specification, acceptance/test
plan, conventions or independent-review handoff. It does **not** mean the curator
implemented or passed application tests. The implementation lead was sent the skill
paths and concrete lifecycle/security guidance for remaining code and review.

| Skill | What it contributes | Applied file / evidence |
| --- | --- | --- |
| [blueprint-feature-workflow](../.agents/skills/blueprint-feature-workflow/SKILL.md) | One active feature, honest adoption, acceptance before further scope, proof/review gates | `blueprint/project-plan.md`; `blueprint/build-plan.md`; `blueprint/context/current-feature.md`; `blueprint/verification.md` |
| [codexkit-high-signal-review](../.agents/skills/codexkit-high-signal-review/SKILL.md) | Evidence-first, risk-ordered independent review with retained severity rubric | `blueprint/verification.md`; separate independent review |
| [codexkit-test-hardening](../.agents/skills/codexkit-test-hardening/SKILL.md) | Behavioral edge cases, narrow tests and realistic regression assertions | `blueprint/verification.md (A2, A4, A6, A8)` |
| [ecc-rust-patterns](../.agents/skills/ecc-rust-patterns/SKILL.md) | Typed ownership/errors, bounded state, minimal public API | `blueprint/context/coding-standards.md`; `blueprint/context/current-feature.md (Data / contracts)` |
| [ecc-rust-testing](../.agents/skills/ecc-rust-testing/SKILL.md) | Unit/integration/async synthetic fixtures and precise assertions | `blueprint/verification.md (A1–A6, existing cargo commands)` |
| [ecc-verification-loop](../.agents/skills/ecc-verification-loop/SKILL.md) | Repository-native final gates and pass/fail/blocked/not-run reporting | `blueprint/verification.md (evidence ledger and final aggregate commands)` |
| [ecc-security-review](../.agents/skills/ecc-security-review/SKILL.md) | Own-client OAuth, native authority, redaction and profile isolation | `blueprint/context/current-feature.md (Data / contracts)`; `blueprint/verification.md (A1–A7)`; `blueprint/verification.md` |
| [ecc-svelte-accessibility](../.agents/skills/ecc-svelte-accessibility/SKILL.md) | Svelte semantic controls, keyboard/focus/status and stale UI tests | `blueprint/verification.md (A8)`; `blueprint/context/current-feature.md (step 4)` |
| [ag-rust-async-patterns](../.agents/skills/ag-rust-async-patterns/SKILL.md) | Bounded concurrency, cancellation ownership and cleanup caveats | `blueprint/context/current-feature.md (operation identity and cancellation)`; `blueprint/verification.md (A2, A4)` |

## Curation and security decisions

- Read every selected upstream SKILL.md in full before applying; also read the
  required async playbook, severity rubric and the excluded coverage script.
- Consolidated Blueprint feature/implement/check/audit into one project-local skill.
  Did not run `create-ai-blueprint`, copy its sample plans, replace project README,
  add root AGENTS, or install dashboard/auto-completion machinery.
- CodexKit high-signal-review’s relative severity reference is preserved.
  Antigravity async-patterns’ entire relative playbook is preserved.
- Omitted CodexKit’s unreferenced coverage script after static review: it may invoke
  npx, suppress test failures, interpolate shell arguments and impose unrelated
  coverage policy. It was **not executed**. No executable support file is needed.
- Adapted ECC security and accessibility to Rust/native/Svelte. Generic React,
  Supabase, blockchain, credential-logging, automatic upgrade and PR snippets are
  not project instructions. Existing repo tooling and gates remain authoritative.
- Upstream examples in Rust references are illustrative: no dependency installation,
  broad upgrades or arbitrary 80% gate is authorized. Native Windows evidence
  cannot be replaced by Linux/browser checks.
- SvelteKit was inspected but excluded: this repository uses Svelte 5/Vite, not
  SvelteKit SSR. REST/GraphQL API-design and offensive OAuth tools were excluded
  because typed native IPC/official provider contracts are the relevant boundaries.
- Duplicate ECC/everything-claude-code bookmark aliases were not double-counted.
- No private bookmark export, Library ID, user secret, browsing history or account
  information was copied into this project pack. Only selected public source URLs.

## Use and verification

Read `.agents/skills/PROJECT_RULES.md`, then only relevant `SKILL.md` files.
This avoids loading the whole source catalog. Project-local skills can be read
directly by a worker even if the runtime has not refreshed its skill discovery.

Run the read-only structural/provenance check from repository root:

```sh
python3 docs/skills-validate.py
```

See [validation results](skills-validation.md). Structural validation does not prove
application behavior. The active feature remains in progress and Windows native
qualification remains pending. No PR/merge/release is permitted; authorized own-fork
Windows-branch publication belongs to the designated implementation lead only.
