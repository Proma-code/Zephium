# Windows subscription trial

This is an experimental Windows x64 personal fork, not an upstream Zephium
release. Keep your ordinary Zephium installation. Do not move or copy its profile,
saved credentials or application folders into the trial.

## Recognize the separate trial

- Installed application and shortcut: **Zephium Subscription Trial**.
- Executable: `zephium-subscription-trial.exe`.
- Installation folder: `%LOCALAPPDATA%\Zephium Subscription Trial`.
- The trial starts with separate settings and accounts; it does not import them.
- Default-browser registration and upstream application updates are unavailable
  in this trial. Open at login, if enabled, applies to the trial separately.

The goal is side-by-side use, but installation/coexistence testing is still
pending. Do not treat a successful build as proof of that behavior.

## Build and download

The Windows branch workflow produces a temporary unsigned installer artifact,
not a public release. Use only an artifact for the intended source revision whose
Windows checks have completed successfully. No ready-to-install artifact is
claimed by this document.

Developers should follow `CONTRIBUTING.md` for prerequisites, then use:

```powershell
pnpm install --frozen-lockfile
node scripts/windows-trial.mjs --build
./scripts/stage-windows-trial.ps1
```

Do not use ordinary `pnpm build` for this trial: that uses the original product
configuration. A supported Microsoft WebView2 Runtime must already be present.

The artifact includes an installer, `BUILD-INFO.json`, and `SHA256SUMS`. Check the
source revision and compare the installer hash before use:

```powershell
Get-FileHash .\Zephium-Subscription-Trial-Windows-x64-setup.exe -Algorithm SHA256
Get-AuthenticodeSignature .\Zephium-Subscription-Trial-Windows-x64-setup.exe
Get-Content .\BUILD-INFO.json
```

No signing key is configured. A checksum detects changed bytes; it does not prove
publisher trust. If Windows or security software blocks the installer, stop and
review the warning. Do not disable protection to proceed.

## Before removing the trial

Disconnect Codex before uninstalling if you want to remove its connection.
Uninstalling does not itself remove retained Windows Credential Manager entries
or revoke provider access. Retired experimental connection credentials are not
silently deleted by an upgrade. Use the provider's own account controls to revoke
access if needed.

## Trial checklist — not yet completed

Use a disposable Windows test account before trying the build beside an important
installation. Confirm that:

1. Both applications retain separate Installed apps entries and shortcuts.
2. Both can open, close and restart independently.
3. A test bookmark saved in one does not appear in the other.
4. Account settings, model choices and Open at login remain independent.
5. The trial does not change the default browser or update the ordinary app.
6. Removing only the trial leaves ordinary Zephium and its data available.

Do not test destructive uninstall or upgrade scenarios against irreplaceable
personal data. Native account sign-in and provider usage require separate,
explicit user actions. See [test status](../blueprint/verification.md) for current
build/test limitations.
