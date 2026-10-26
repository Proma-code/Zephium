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
- the shared startup policy against both original reference CLI releases.

Original images in CI never request login, start a conversation, send a prompt,
or run a model. Synthetic peer transcripts are labeled separately. Raw account,
login and configuration payloads are not printed. The fixture feature is not a
product enablement, and ordinary builds expose no process constructor.

The trial-only actor includes explicit original login/logout, catalog and text
conversation operations, with durable native thread associations. This remains
an integration milestone: startup and simulated transcripts do not establish
real authenticated conversation continuity, browser tools, Ultra, or all future
runtime compatibility. Existing API and direct Sign in with ChatGPT records are
not converted into Codex credentials. Actual account access requires the user's
separate official login.
