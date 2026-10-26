# Windows subscription trial — test status

This remains an experimental personal fork, not full acceptance of every
requested provider. No real account grant or paid inference was used in these
tests. Detailed internal reviews are kept separately.

## Checks completed on 2026-10-26

- Portable account, transport, quota, routing and profile-removal regressions
  passed. The latest renderer checks passed **1,104 unit tests in 175 files**,
  typechecking, lint, formatting and unused-code checks. Both frame builds and
  all 143 emitted style files passed.
- On Windows, the account/picker Chromium component tests, native account and
  focused routing tests, trial configuration checks, strict desktop Clippy and
  complete generated IPC binding checks passed at source
  `9b21629b96c7c6385ba2257a7426cb9be5797c84`.
- The Windows native vault fixture passed using fresh synthetic trial profiles.
  It exercised credential roundtrip, replacement, profile separation and cleanup,
  not a real provider account.
- The original pinned Claude client passed credential-free version/help startup
  checks. This does not enable Claude account linking or establish login and
  conversation behavior.
- Packaging identity/guard checks and the nine-skill inventory validation passed.
  Pure guard tests alone do not prove Windows installation or application startup.

[Windows run for that source](https://github.com/Proma-code/Zephium/actions/runs/40279899377).

## Installer and application status

The Windows installer job for the source above completed successfully on
2026-10-26. It built and staged the unsigned installer, checked its exact bundled
payload identity, and passed disposable install, same-artifact reinstall and
uninstall checks with synthetic preservation fixtures. The workflow uploaded a
temporary installer artifact. No application was launched by that smoke test.

The next raw-executable runtime check is still pending. It is limited to fresh
first-run initialization, an observed responsive application window and orderly
close. It is not complete UI testing or proof of installed-resource behavior.
Five additional context-budget component cases also await their exact Windows run.

Real side-by-side operation with an existing Zephium installation, live provider
sign-in, model requests and account quota responses still need explicit user
validation. Synthetic preservation fixtures are not an existing installation.
Do not disable Windows protection to run an unsigned experimental installer.

## Remaining test limits

- The separate full Windows Work run at the source above completed with
  **914 application tests passed, 47 failed and 14 skipped**. Its provider suite
  passed 1,231 tests and durable composition passed 186 tests. The overall run
  therefore failed despite the successful installer job. Fixture repairs and
  additional diagnostics are awaiting a new exact-source Windows run; the
  intentionally gated positive cases are not counted as passes.
- An expanded Linux app subset passed 951 tests with 11 ignored. The unfiltered
  run failed ten unchanged temporary-directory-layout fixtures and two consequent
  shared-lock checks. This is not an unfiltered full-suite pass.
- Claude account linking remains in development. OpenCode Go has manual quota
  retrieval code, but these tests used synthetic responses. Numeric direct
  ChatGPT and generic local-gateway quota remain unavailable.

See the [connection guide](../docs/subscription-accounts.md) and
[Windows trial guide](../docs/windows-subscription-trial.md). There is no pull
request, merge or public release in this work.
