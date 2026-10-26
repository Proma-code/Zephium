# Windows subscription trial — test status

This is an experimental checkpoint, not full acceptance of all requested providers.
Detailed internal review and verification records are kept separately.

## Portable checks completed on 2026-10-26

- ChatGPT tests: 69 passed; local gateway tests: 21 passed.
- Account and gateway IPC tests: 18 passed; two explicit generation commands are
  normally ignored. Generated binding checks passed.
- Local gateway transport fixtures: 14 passed.
- Quota projection fixtures: 22 new tests and 16 existing account tests passed.
- Model routing: four passed; shared run-source tests: 12 passed; call/accounting:
  six passed; profile-deletion selector: 19 passed.
- Strict Clippy passed for the account and gateway crates. App Clippy completed
  with existing unrelated warnings, so it is not a warning-free app check.
- Renderer: 1,039 tests in 171 files, typecheck, lint, formatting, production build
  and all 141 emitted style files passed.
- Trial packaging helpers: seven Node tests, six Rust configuration tests and
  synthetic PowerShell staging tests passed.
- Project skill inventory validation passed for nine skills from four sources.

Tests used synthetic data, not real provider accounts or paid inference.
Browser observations using mocked native responses are separate from Windows
application testing and do not prove a real account connection.

## Windows status

The first Windows run stopped at a timing-sensitive frontend test. An unchanged
retry passed all 999 tests in that earlier checkpoint, then stopped because the
style check ran before the build. The next workflow corrects that ordering and
adds actual Chromium component tests. Skipped steps are not counted as passes.

Windows compilation, installer execution, side-by-side operation and native
account behavior remain pending. No installer is represented as ready here.
Claude and OpenCode subscription chat and numeric live quota also remain open.

The current [user guide](../docs/subscription-accounts.md) describes available
routes; the [Windows trial guide](../docs/windows-subscription-trial.md) describes
how to recognize the separate experimental package. No pull request, merge or
release is part of this work.
