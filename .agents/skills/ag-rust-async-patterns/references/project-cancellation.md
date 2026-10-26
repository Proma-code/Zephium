# Account lifecycle application

Use typed operation identity/generation and profile ownership. Dropping a future
alone does not undo token issuance, a process launch or a vault write. Account
service cancellation must retire ownership before any stale result can commit.
Bound concurrent work and all I/O, retain task handles, and finish/abort/join with
bounded cleanup. Never keep a broad mutex locked across browser/user interaction.
Do not add Tokio features or helpers simply to match a playbook snippet.

Test cancel then restart, duplicate completion, timeout during callback, late
network response, and profile replacement with deterministic seams. A synthetic
reducer test is not proof of native listener/process teardown.
