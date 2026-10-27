//! Recovery and source-bound compatibility for existing legacy ChatGPT grants.
//!
//! No new authorization, token refresh, model catalog or inference route remains.
//! Protected records are preserved for read-only inspection, explicit revocation
//! and profile erasure. They are never imported into Codex or rewritten as another
//! model source. The optional fixture module has no credentials or network client.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Offline legacy wire fixtures retained for Work protocol regression tests.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod inference;
/// Fixed-origin revocation of existing protected grants.
pub mod oauth;
/// Per-profile stored credential lifecycle.
pub mod service;
/// Secret-bearing tokens with redacted diagnostics.
pub mod token;
/// Isolated native credential storage.
pub mod vault;
