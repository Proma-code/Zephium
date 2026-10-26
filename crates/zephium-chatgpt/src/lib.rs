//! Direct, local Sign in with ChatGPT for this open-source desktop app.
//!
//! OAuth credentials belong to Zephium's own dynamic registration. They never
//! come from Codex, browser cookies, another app, or Zephium's hosted service.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Bounded direct Responses streaming and account-specific model catalog.
pub mod inference;
/// Official public-client PKCE and OIDC validation.
pub mod oauth;
/// Per-profile credential and login lifecycle.
pub mod service;
/// Secret-bearing tokens with redacted diagnostics.
pub mod token;
/// Isolated native credential storage.
pub mod vault;
