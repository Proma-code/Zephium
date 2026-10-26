# Zephium: file-backed Windows account workflow

This is an intentionally lightweight, project-specific adoption of AI Blueprint,
not an execution of its installer or a claim of full CLI/dashboard compatibility.
The implementation was already underway when this packet was introduced on
2026-10-25. It does not retroactively claim spec approval, TDD order or test passes.

Start with the active `context/current-feature.md`; it captures the user's scope
before further feature extension. The local skill is
[blueprint-feature-workflow](../.agents/skills/blueprint-feature-workflow/SKILL.md).
The exact curated sources and adaptations are in
[skills inventory](../docs/skills-inventory.md).

- Product intent: `project-plan.md` and `build-plan.md`.
- Implementation scope/acceptance: `context/current-feature.md`.
- Current reality/conventions: `context/project-overview.md`,
  `context/coding-standards.md`, `context/ai-interaction.md`.
- Commands, scenarios and observed results: `verification.md`.

One active feature, multiple disjoint-path workers. There is no root `AGENTS.md`
overwrite, no change to `frame/AGENTS.md`, no upstream sample project plan copied,
and no auto-commit, PR, merge, release or deployment operation. The code lead owns
any authorized push to the user's own Windows branch. No upstream PR is wanted.

`config.json` records familiar upstream schema settings for readability, but only
the installed project skill is active: it never invokes upstream `/complete`.
`git.landing` is deliberately omitted; no upstream default landing behavior is
activated or authorized by this local adaptation.
Required Windows-native checks remain pending until a supported Windows executor
and appropriately authorized test profile are available. Portable checks do not
waive them. External account consent or paid calls are outside current tests.

Detailed independent security review records are maintained separately and are not published in this repository.
