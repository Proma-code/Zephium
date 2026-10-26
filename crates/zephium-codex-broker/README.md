# Codex Windows transport qualification

This crate prepares an original Codex app-server connection for the Windows
subscription trial. It does not yet enable an account connection or chat source.
The intended product path is an existing compatible Codex CLI; the standalone
app-server is a reference used to compare the same protocol in CI. Neither path
modifies the user's ordinary Codex installation, configuration or login.

The current pin is OpenAI's `rust-v0.164.0`, source
[`6aaac694803004a63c6f393d153c96d22521a6b9`](https://github.com/openai/codex/tree/6aaac694803004a63c6f393d153c96d22521a6b9).
The two original Windows x64 image hashes and distinct fixed arguments are
checked independently; a version string or a PATH lookup is not admission.

The debug-only native qualification uses a fresh disposable home and only:

- initializes private stdio;
- checks signed-out account state without requesting refresh;
- checks the owned configuration and absence of managed requirements;
- closes stdin and requires process exit zero and an empty owned job;
- checks cancellation and four fixed synthetic transport failure cases.

It never requests login, starts a conversation, sends a prompt, or runs a model.
Raw native output and account/config values are not printed. Ordinary builds
expose no process constructor. The fixture feature is not a product enablement.

Successful startup does not establish login persistence, keyring writes,
text-turn behavior, absence of ambient skill discovery, browser tools or Ultra
support. Those are separate integration and user-validation steps. Existing API
and direct Sign in with ChatGPT records are not converted into Codex credentials.
