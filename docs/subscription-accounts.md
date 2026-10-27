# Windows subscription accounts

This personal fork uses **Codex** for ChatGPT subscription connections. It is
not an upstream release. Use the separate [Windows trial](windows-subscription-trial.md)
to keep the ordinary Zephium installation and data separate.

## Codex connection

The Windows integration uses an existing compatible official Codex CLI. It does
not install another copy, borrow the Codex desktop application's credentials, or
change the user's ordinary Codex configuration. Zephium manages a separate home
and protected credential namespace for each browser profile.

In Settings → AI, **Codex** owns connection, cancellation, disconnect and quota
controls. The sidebar's OpenAI picker offers **Codex** and **API key** separately.
Model names are the original catalog names; effort choices come from the same
catalog. Selecting a sidebar model does not change the separately configured
Work roles. A failed or unavailable Codex connection never selects an API key or
another paid source automatically.

Only the original runtime performs account login. Canceling an unfinished login
is not logout. Ordinary browser close preserves the managed account. Disconnect
and profile removal are explicit operations; interrupted cleanup remains visible
until its original operation can be safely reconciled.

Runtime identity is independent of account and conversation identity. Before a
new image can use a managed account, the native owner verifies official release
provenance, the held executable bytes and the required protocol capabilities.
Compatible runtime changes preserve the same managed home and original thread
associations. A genuinely unsupported runtime must not delete the account,
convert credentials, replay an uncertain message or fabricate continued history.

Quota is original Codex account data, not API billing. The view preserves the
source, observation time and any reported windows. Missing values remain unknown;
a failed refresh can leave a previous reading marked stale. Refreshing quota
is not a model request.

## Retained direct-account records

The earlier direct ChatGPT sign-in route is retired. It is not offered for new
connections or model selection. Existing source IDs, conversation history and
stored grants are not silently converted or deleted. A saved selection of that
retired source requires an explicit choice of Codex or another configured source.

Settings shows advanced legacy recovery only when a record exists or its presence
cannot be verified. Observation does not create an identity, refresh a token or
contact the provider. Explicit disconnect retains a truthful indication when
remote revocation could not be confirmed.

## Other configured sources

OpenAI API keys and existing local gateway/OpenCode Go configurations keep their
separate identities and Work settings. They are not Codex login or automatic
fallback routes. This integration does not add another subscription provider.

## Qualification boundary

The connection is still under integrated Windows qualification. Original-runtime
startup and synthetic ownership tests are distinct from real account use, browser
tool execution and authenticated cross-version conversation continuity. An
internal text-only preview is not the completed browser integration.

The [broker README](../crates/zephium-codex-broker/README.md) describes the native
ownership and runtime-admission boundary. CI uses disposable synthetic data and
never receives a user's account credentials.
