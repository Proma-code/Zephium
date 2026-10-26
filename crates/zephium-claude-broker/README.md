# Original Claude Code Windows broker candidate

This is **not yet a working account connection**. Availability remains closed
while Windows qualification and app integration are in progress. It does not
read/import another app's credentials or turn a subscription into an API key.
The generic Work command gate is unchanged.

The candidate uses the original vendor client with its own private account
configuration, fixed commands and bounded text-only output. Subscription login
is retained: it never uses `--bare`. Network access is described as public
Internet, **not** a provider-host-only allowlist.

## Checks

```sh
cargo test -p zephium-claude-broker --locked
cargo clippy -p zephium-claude-broker --all-targets --locked -- -D warnings
```

Portable checks are not Windows runtime evidence. The debug-only Windows
`qualify-windows.ps1` script first runs credential-free synthetic fixtures. Only
if those pass does it download—not install—the exact original Claude 2.1.310
Windows x64 image, verify its pinned bytes and signature, and run `--version`
and restricted safe `--help`. It never runs login, status/logout or inference.
No vendor binary is checked in; this is not an updater or production approval.
The debug runtime also evaluates `registryRead`, which is broader than a single
registry catalog. Passing fixtures does not establish safety for every registry
key or enable a production connection.

Account linking, real conversation behavior and app integration remain pending.
Any real account grant or inference requires separate explicit authorization.

References: [CLI contract](https://code.claude.com/docs/en/cli-reference),
[authentication](https://code.claude.com/docs/en/authentication),
[binary integrity](https://code.claude.com/docs/en/setup#binary-integrity-and-code-signing),
[hosting terms](https://code.claude.com/docs/en/legal-and-compliance).
