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
  `c4956618e1000b5909de351418fc574405cd0849`, including the context-budget cases.
- The Windows native vault fixture passed using fresh synthetic trial profiles.
  It exercised credential roundtrip, replacement, profile separation and cleanup,
  not a real provider account.
- The original pinned Claude client passed credential-free version/help and
  fresh signed-out status checks. This does not enable Claude account linking or
  establish login, credential persistence or conversation behavior.
- Packaging identity/guard checks and the nine-skill inventory validation passed.
  Pure guard tests alone do not prove Windows installation or application startup.

[Windows run for that source](https://github.com/Proma-code/Zephium/actions/runs/40297504720).

## Installer and application status

The source above built and staged the unsigned installer, checked its exact
bundled payload identity, and passed disposable install, same-artifact reinstall
and uninstall checks with synthetic preservation fixtures on 2026-10-26.

The separate raw-executable smoke test passed first-run initialization, a
responsive application window, and a single-window close with exit code 0. The
owned process tree was empty without forced cleanup. Both Windows jobs passed,
and that exact source produced the experimental installer artifact. This does
not establish complete UI behavior or startup from an installed resource layout.

Real side-by-side operation with an existing Zephium installation, live provider
sign-in, model requests and account quota responses still need explicit user
validation. Synthetic preservation fixtures are not an existing installation.
Do not disable Windows protection to run an unsigned experimental installer.

## Remaining test limits

- The independent full Windows Work job at
  `8e28c0ea01f4670b4d034eb1422ba856355c3ec9` passed **966 application tests**,
  **1,231 provider tests** and **186 durable-composition tests**, with **18 skipped
  application cases**. This closes the earlier application test failures. These
  suites also passed in the successful `c4956618` run above. Skipped cases are not
  counted as passes.
  [Exact Windows run](https://github.com/Proma-code/Zephium/actions/runs/40294953636).
- An expanded Linux app subset passed 951 tests with 11 ignored. The unfiltered
  run failed ten unchanged temporary-directory-layout fixtures and two consequent
  shared-lock checks. This is not an unfiltered full-suite pass.
- Claude account linking remains in development. OpenCode Go has manual quota
  retrieval code, but these tests used synthetic responses. Numeric direct
  ChatGPT and generic local-gateway quota remain unavailable.

See the [connection guide](../docs/subscription-accounts.md) and
[Windows trial guide](../docs/windows-subscription-trial.md). There is no pull
request, merge or public release in this work.
