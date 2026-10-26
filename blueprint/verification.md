# Windows subscription trial — test status

This is an experimental checkpoint, not full acceptance of all requested providers.
Detailed internal review and verification records are kept separately.

## Portable checks completed on 2026-10-26

- ChatGPT: 70 tests; local gateway: 22; OpenCode Go account ownership and quota lifecycle: 37 passed.
- Account/gateway/Go IPC: 23 passed; four explicit generation commands are
  normally ignored. Generated binding checks passed.
- Local gateway transport: 13 fixtures; OpenCode Go transport: 37 passed,
  including 18 explicit quota-read fixtures.
- Quota projection: 22 fixtures and 16 existing account tests passed. These do
  not establish live quota retrieval.
- Focused model routing: six; shared run-source tests: 13; call/accounting: seven;
  profile-deletion selector: 20 passed.
- An expanded app subset passed 951 tests with 11 ignored. Ten unchanged fixtures
  requiring a different Unix temporary-directory layout were excluded on the
  Linux cloud host. The unfiltered run failed those fixtures and two consequent
  shared-lock checks; both shared-lock checks passed when rerun separately.
  This is not an unfiltered full-suite pass.
- Strict Clippy passed for the account and transport crates. App Clippy completed
  with existing unrelated warnings, so it is not a warning-free app check.
- Final renderer: 1,087 tests in 174 files, typecheck, lint, formatting and unused-code
  checks passed. Production build and all 143 emitted style files passed. Five
  additional quota browser cases are included for the next Windows Chromium run.
- Trial packaging helpers: seven Node tests, six Rust configuration tests and
  synthetic PowerShell staging/installer guard tests passed. Two shared account
  registry concurrency fixtures also passed. Guard/parser checks
  do not install or uninstall an application.
- Project skill inventory validation passed for nine skills from four sources.

Tests used synthetic data, not real provider accounts or paid inference.
Browser observations using mocked native responses are separate from Windows
application testing and do not prove a real account connection.

## Windows status

Earlier runs exposed a timing-sensitive frontend test, build/check ordering,
component-test selectors and a platform-dependent fixture socket mode. Repairs
were tested and published without weakening the behavior assertions.

Subsequent Windows runs passed actual Chromium component tests, frame build,
native account tests and focused app/IPC regressions. Separate fixture, lint and
new-component selector failures were corrected and published. The current Go
and quota increment still requires its own completed Windows run. Skipped steps
are not counted as passes.

Windows desktop compilation, installer execution, side-by-side operation and
native account behavior are not yet fully verified. No installer is represented
as ready here. A disposable-runner install/reinstall/uninstall smoke is planned
with synthetic preservation fixtures; it is not real installed-app coexistence.
Claude account linking remains open. Go now has manual quota retrieval code,
but live account data and its Windows behavior remain unverified.

The current [user guide](../docs/subscription-accounts.md) describes available
routes; the [Windows trial guide](../docs/windows-subscription-trial.md) describes
how to recognize the separate experimental package. No pull request, merge or
release is part of this work.
