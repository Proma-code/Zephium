---
name: ecc-verification-loop
description: Verify Zephium changes with the existing Rust and frame checks and produce
  an evidence-based pass, fail, blocked or not-run ledger without PR automation.
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

# Repository-native verification loop

Use the repository-root `blueprint/verification.md` for exact commands. Do not
copy generic npm, Python, grep-secret or PR commands into this pnpm/Rust project.

1. Run focused logic tests for the changed boundary; confirm regression tests can
   detect the defect where a safe pre-fix reproduction exists.
2. Run applicable compilation/type checks, formatting, lint and existing unit
   tests. Keep unrelated baseline failures distinct, never silently waived.
3. Run browser component tests for changed UI, including dismissal, repeated
   actions and stale asynchronous results. Use existing native mocks.
4. Run the required final aggregate gates after the last relevant edit. Preserve
   command exit status; `set -o pipefail` is necessary for shell output pipelines.
5. Review the complete diff including untracked feature files. Inspect auth,
   input validation, data ownership and redaction without printing secret values.
6. Record pass/fail/blocked/not-run, exact command, tested revision or dirty-tree
   fingerprint, evidence and remaining risk. Count tests only from actual output;
   report coverage as unmeasured unless measured by an existing configured tool.

A green browser build is not native Windows proof. Native qualification needs
the reviewed executable, isolated test profile, Windows/WebView2 version and
sanitized scenario results. No sign-in or external service effects are authorized
by this skill. The output is readiness for further local review, not permission
to publish, merge, open a PR or deploy.
