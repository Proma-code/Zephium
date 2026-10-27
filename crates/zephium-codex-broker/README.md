# Codex Windows connection

This crate supplies the original Codex app-server connection for the Windows
subscription trial. Product UI activation is separate. It uses an existing
compatible Codex CLI and does not install a second copy, import another client's
credentials, or change the user's ordinary Codex home.

The offline reference releases are OpenAI's `rust-v0.163.0` and `rust-v0.164.0`.
Other detected images require fixed-origin official release metadata and a full
held-file digest match, followed by a disposable, signed-out compatibility probe.
A version, PATH entry, or cached release hint is not executable authority. Lookup
is bounded; unsupported or unverified images are not run against account data.
Runtime versions do not define the managed profile, keyring namespace or saved
conversation identity.

The credential-free Windows qualification checks:

- the original CLI and standalone reference server's startup and owned policy;
- signed-out account state without requesting refresh;
- process exit zero, empty owned jobs and cancellation settlement;
- synthetic transport and conversation transcripts, including cold thread resume;
- owned profile creation, reopen and managed-policy migration;
- a unique nonsecret OS credential-store round trip;
- the shared startup policy against both original reference CLI releases;
- bounded experimental schema export from the same held original CLI image;
- synthetic browser-tool ownership, cancellation and joined settlement.

Original images in CI never request login, start a conversation, send a prompt,
or run a model. Synthetic peer transcripts are labeled separately. Raw account,
login and configuration payloads are not printed. The fixture feature is not a
product enablement, and ordinary builds expose no process constructor.

The trial-only actor includes original login/logout, catalog and conversation
operations with durable native thread associations. Browser conversations require
a same-image schema capability and an immutable tool-policy association. The
broker validates tool requests and passes move-only effect custody to Page; it
does not implement browser actions. Completion joins original runtime settlement
with Page's local-effect receipt. Lost receipts retain an explicit quarantine,
not permission to resend or start another turn.

Healthy dormant receipts are distinct from failures and current authority. They
are minted only after physical closure and carry data-only original managed-home
identity, not image handles or credentials. Wake uses an existing-only lease and
fresh image, protocol, account and catalog checks under one preparation deadline.
It cannot fall back to reprovisioning. Quota-only close drainage joins exact owned
request IDs without publishing a successful reading; other unresolved operations
prevent healthy dormancy. Active image custody is never weakened for an updater;
the native qualification suite tests package replacement at this physical-release
boundary.

Startup and simulated transcripts do not establish real authenticated browser
actions, Ultra, or authenticated cross-version continuity. Existing API and
retired direct-account records are not converted into Codex credentials. Actual
account access requires the user's separate official login.

Protocol references are OpenAI's [app-server interface](https://github.com/openai/codex/tree/rust-v0.164.0/codex-rs/app-server)
and its [experimental schema generator](https://github.com/openai/codex/blob/6aaac694803004a63c6f393d153c96d22521a6b9/codex-rs/app-server-protocol/src/schema.rs).
Runtime identity, protocol projection, account lifecycle, durable associations
and Page effects remain separate module boundaries.
