---
name: blueprint-feature-workflow
description: Maintain Zephium’s single file-backed feature spec, acceptance criteria,
  verification evidence and independent review handoff before extending the Windows
  account integration.
license: MIT
metadata:
  upstream: https://github.com/aiblueprinthq/ai-blueprint
  commit: a31461341770fe2fc88b0372fc9f49f15acda652
  adaptation: project-scoped
---

## Zephium integration

Read [project scope and safety rules](../PROJECT_RULES.md) before applying this
skill. Examples are reference material, not permission to install tools, change
dependencies, authenticate, expose secrets, or publish. Use this repository's
commands and conventions; generic upstream examples are not product contracts.

# File-backed feature workflow

This is a deliberately small adaptation of AI Blueprint's `feature`, `implement`,
`check` and `audit` workflow. It is not a full upstream CLI installation. It has
no dashboard, hooks, auto-commits, branch generator or completion/merge command.

## Spec before additional behavior

Read `blueprint/project-plan.md`, `blueprint/build-plan.md` and the active
`blueprint/context/current-feature.md` from the repository root. Confirm the
actual branch and dirty files without modifying other workers' changes. Product
intent comes from the user; existing code establishes current reality.

For an already-started implementation, record adoption explicitly. Do not claim
that code was preceded by this spec, that approval occurred, or that old tests
were rerun. Preserve the active scope. Before extending it, update concrete
acceptance criteria and affected areas. Resolve material security/interoperability
questions before enabling that path; reversible implementation details do not
need a new product decision. New product scope still needs user authorization.

Each build step needs an observable done-when and an appropriate test. Keep one
feature packet and one owner for each product file. Follow existing design,
public module APIs, credential ownership and platform gates. Avoid speculative
layers or dependencies.

## Build and prove

Implement the smallest coherent step; run focused checks before marking it done.
At the feature boundary run the declared aggregate checks in
`blueprint/verification.md`. Preserve true exit codes and record command, target,
revision, time, outcome and sanitized evidence location. Distinguish passed,
failed, blocked and not-run. Do not turn unknown behavior into a checked box.

Separate synthetic protocol evidence, browser component evidence and native
Windows evidence. A manual guide is not executed verification. Never perform
real account authorization as a convenient test. Recheck affected evidence after
later product or test changes.

## Review and completion boundary

Maintain `blueprint/context/findings.md` with evidence-backed findings. A repair
is fixed, not closed, until re-reviewed. Do not invent severity from speculation.
A full independent receipt requires a fresh reviewer against an exact immutable
checkpoint and spec hash; a dirty-tree review is useful but explicitly provisional.

Prepare `blueprint/context/review.md` with base/target, spec digest, exclusions,
checks required, and reviewer instructions. The builder cannot approve itself.
A changed checkpoint invalidates the receipt. Missing Windows evidence remains
visible even if all portable checks pass. No skill operation commits, pushes,
opens a PR, merges, releases or deploys.

See [adoption and commands](../../../blueprint/README.md) and the upstream pinned
source files recorded in [inventory](../../../docs/skills-inventory.md).
