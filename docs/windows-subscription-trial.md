# Windows subscription trial — separate from installed Zephium

This is an **experimental Windows x64 fork**, not an upstream Zephium release.
Keep the existing Zephium installed. Do not uninstall it, move its profile, copy
its credential files, or point the trial at its directories.

The trial keeps the existing application design. Its separate name identifies
the experiment; it does not claim that any subscription is connected or usable.
Account availability still depends on the reviewed provider contract and native
qualification gates described in `subscription-accounts.md` in the source tree.

## Isolation contract

| Boundary | This trial |
| --- | --- |
| Product / Installed apps entry | `Zephium Subscription Trial` |
| Application identifier | `app.zephium.subscription-trial` |
| Executable | `zephium-subscription-trial.exe` |
| Per-user installation | `%LOCALAPPDATA%\Zephium Subscription Trial` only |
| NSIS uninstall registry key | `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Zephium Subscription Trial` |
| Start menu folder / shortcut | `Zephium Subscription Trial` |
| Single-instance mutex / message window | Derived from `app.zephium.subscription-trial`, not `app.zephium` |
| App profile, database, browser WebView2 data and startup log | `%APPDATA%\app.zephium.subscription-trial` and its owned subdirectories |
| Local app data, where used | `%LOCALAPPDATA%\app.zephium.subscription-trial` |
| Protected native Work namespace, if admitted | `%USERPROFILE%\.zephium-native-v1\app.zephium.subscription-trial` |
| Windows Credential Manager | `app.zephium.subscription-trial.*` only |

The ordinary identifiers, executable, profile and credential items are **not
migrated or adopted**. The final Windows vault boundary maps old logical API-key,
MCP and Zephium cloud-account names into the trial namespace, including deletion
and enumeration. Explicit trial-qualified ChatGPT targets remain unchanged.
That means the trial starts without the existing app's saved API keys/accounts.

The trial cannot register as the default browser. It packages no file or deep-link
associations, never runs the production browser-registration uninstall hook, and
does not load the production application updater. The security/blocker subsystem
keeps its existing behavior within the separate profile; no security gate is
disabled. Native Work/CLI qualification requirements remain unchanged.

The installer refuses a non-default folder and refuses to overwrite an existing
`zephium-desktop.exe` or `Zephium.exe` in its trial folder. It does not install or update WebView2:
a supported Microsoft WebView2 Runtime must already be present. Windows itself
may maintain that shared runtime independently of this trial.

## Build and artifact

Use Windows with the pinned Rust/Node/pnpm versions and prerequisites in
`CONTRIBUTING.md`:

```powershell
pnpm install --frozen-lockfile
node --test scripts/windows-trial.test.mjs
cargo test --locked --manifest-path desktop/windows-trial-config-tests/Cargo.toml
node scripts/windows-trial.mjs --build
./scripts/stage-windows-trial.ps1
```

Do **not** use ordinary `pnpm build` for this trial: that is the original product
configuration. The trial's desktop feature and exact configuration are required
together by a build-time guard; the `.dev` identity is not used for release builds.
The helper permits no arbitrary configuration/signing overrides.

The `Windows subscription trial` workflow runs on pushes to
`windows/subscription-accounts` in `Proma-code/Zephium`, not on PRs or tags. It uses
read-only repository permissions, existing pinned actions and no new secrets.
It performs Windows compilation/lint, focused native account/vault tests, frame
checks and an optimized NSIS build. The artifact includes the installer, this
guide, generated NSIS identity evidence, source revision/signature status, and
`SHA256SUMS`. It is retained for 14 days; no GitHub release or updater feed is made.

There is **no Authenticode signing key configured**. Treat the installer as
unsigned unless its actual verified signature says otherwise. SHA-256 checks
detect changed bytes but do not establish publisher trust. If Windows or security
software blocks it, stop and review that warning; these instructions do not ask
you to disable protection or bypass a warning.

From the extracted artifact folder, compare the installer hash with its line in
`SHA256SUMS` and confirm `BUILD-INFO.json` names the intended source revision:

```powershell
Get-FileHash .\Zephium-Subscription-Trial-Windows-x64-setup.exe -Algorithm SHA256
Get-AuthenticodeSignature .\Zephium-Subscription-Trial-Windows-x64-setup.exe
Get-Content .\BUILD-INFO.json
```

## Manual side-by-side checklist — not yet executed

Run only after choosing to test the reviewed artifact, preferably in a disposable
Windows account or VM. Record OS/WebView2 versions, artifact SHA-256 and source
revision. Synthetic data is enough; no real account authorization or paid inference
is needed for these checks. Never put secrets or private browsing data in reports.

- [ ] Before installation, record the existing Zephium version, installation path,
  default-browser choice and a harmless existing bookmark/settings sentinel.
  Confirm the original app still starts; do not change or delete its data.
- [ ] Review the trial installer's name and destination. It must not offer to
  uninstall/upgrade ordinary **Zephium**. Cancel if it does.
- [ ] Verify the trial refuses an attempted installation into the ordinary
  Zephium folder without changing its executable. Return to the separate default
  folder for the actual trial installation.
- [ ] Confirm Windows Installed apps has two distinct entries and shortcuts.
  Check the running executable paths, not just an in-app title.
- [ ] Open original Zephium and the trial simultaneously. Launching the trial
  again must focus only its existing window; it must not forward URLs to, close,
  or replace the original application. A shared global shortcut may have one
  owner; this does not establish profile or single-instance isolation.
- [ ] Confirm the trial begins with a fresh profile and its own WebView2/data
  directories. Create a harmless trial-only bookmark/settings sentinel, restart
  both apps, and confirm neither sentinel appears in the other application.
- [ ] Confirm trial default-browser controls cannot register it, the existing
  HTTP/HTTPS choice is unchanged, and ordinary external links retain that choice.
- [ ] Confirm application update status is unavailable in the trial and no
  production release check/download/install occurs. This is separate from normal
  webpage traffic and the browser's independent security/blocker updates.
- [ ] With synthetic fixtures only, confirm trial Credential Manager operations
  create/delete only `app.zephium.subscription-trial.*` targets. Do not inspect,
  copy or delete existing secret values to perform this check.
- [ ] Close the trial and uninstall **Zephium Subscription Trial** only. Keep any
  app-data deletion checkbox unchecked unless intentionally discarding trial data.
  Verify the original app, profile, bookmark/settings sentinel, keys, shortcuts,
  browser registration and default choice still work. Trial credentials/native
  Work data are not automatically erased by the installer; no broad cleanup is
  attempted.

## Verification limits

Portable config/namespace tests are not Windows execution evidence. A successful
Windows CI build proves only the checks listed in that exact run; it does not
prove this manual installation/coexistence/uninstall checklist or interactive
WebView2 behavior. Account login/cancel/refresh/disconnect and entitlement/inference
are separate native/provider checks. No real sign-in is performed by this workflow.

Identity derivation follows the pinned Tauri implementation: app paths append the
identifier (`vendor/tauri/src/path/desktop.rs`); the single-instance plugin derives
its Windows names from that identifier; NSIS derives install/uninstall/shortcut
identity from product name and application data from bundle ID. See the official
[Tauri configuration reference](https://v2.tauri.app/reference/config/) and
[NSIS template](https://github.com/tauri-apps/tauri/blob/tauri-bundler-v2.9.4/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi).
