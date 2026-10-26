# Skills-pack validation

Observed 2026-10-25 23:32 UTC in the local Zephium checkout. This validates the
curated skill files and adoption packet, **not application behavior**.

| Check | Result | Evidence |
| --- | --- | --- |
| `python3 docs/skills-validate.py` | PASS | 9 unique skills, 4 full commit pins, 12 manifested skill/support files, 17 local references, zero errors |
| Codex skill-creator `quick_validate.py` for each installed skill | PASS | 9/9 valid frontmatter/name/description/body results |
| License integrity | PASS | Four source MIT notices retained byte-for-byte; SHA-256 matches manifest |
| Required relative dependencies | PASS | CodexKit severity rubric and Antigravity async playbook present; project links resolve |
| Missing-reference negative fixture | PASS | Validator detects deleted severity rubric |
| Modified-file negative fixture | PASS | Validator detects changed installed bytes |
| Name-collision negative fixture | PASS | Validator detects duplicated frontmatter name |
| Missing-license negative fixture | PASS | Validator detects deleted source notice |
| Restored fixture baseline | PASS | Temporary test copy returns to zero errors; tests never modified the real pack |
| `git diff --check` | PASS at observation | No whitespace errors in then-current tracked diff; not application validation |
| Ignore/discovery check | PASS | Selected skill, spec and inventory paths are not Git-ignored |
| Third-party execution | NONE | No upstream installer, hooks, supporting scripts or network auto-execution |
| Application test suites | NOT RUN by curator | Implementation lead owns actual command output and final checkpoint |
| Windows native / OAuth E2E | NOT RUN | No real login, logout, consent or paid inference performed |
| Independent application receipt | PENDING | Prepared handoff is not a passing receipt |

The custom validator is read-only and uses Python's standard library. The Codex
validator was already installed in the execution environment; it was not copied
as an application dependency. Validation does not certify every example as
compilable code or establish provider entitlements. Local adaptations and project
scope take precedence over generic example guidance.

Source-to-install digests are in [skills-manifest.json](skills-manifest.json).
Re-run structural validation after editing any installed skill/support file and
review intentional manifest hash updates. Do not change hashes simply to hide
an unexplained change.
