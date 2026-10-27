//! Provider-owned account setup contracts, without process, network or storage authority.
//!
//! An authenticated CLI is not an API key or permission to execute Work. Native
//! callers must separately qualify confinement and obtain action-time consent.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::dbg_macro, clippy::print_stderr, clippy::print_stdout)]
#![cfg_attr(
    not(test),
    deny(clippy::panic, clippy::unreachable, clippy::unwrap_used)
)]

/// Managed Codex app-server account protocol. This module performs no I/O.
pub mod codex;
/// Provider-reported quota fixtures and capability projections; no fetch authority.
pub mod quota;
/// Strict, non-secret classification of official CLI status responses.
pub mod status;

/// CLI-oriented account setup policy; these are not Work model provider IDs.
/// Historical CLI classifications remain separate from active model providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountProvider {
    /// The Codex-managed ChatGPT CLI route, separate from direct SIWC.
    ChatGpt,
    /// A Claude account used only in the original Claude Code client.
    Claude,
    /// Provider accounts supported by the installed OpenCode version.
    OpenCode,
    /// Google's consumer Antigravity subscription.
    Antigravity,
}

/// What the vendor supports, independently of Zephium's execution availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountRoute {
    /// Managed login/status/logout through Codex app-server, not token import.
    ManagedAppServer,
    /// Only the unmodified original CLI may own subscription authentication.
    OriginalCli,
    /// Methods and entitlement depend on the provider and installed CLI version.
    ProviderDependentCli,
    /// Consumer terms do not permit wrapping this service in a third-party app.
    UnsupportedConsumerIntegration,
}

impl AccountProvider {
    /// Vendor-supported route. This is not execution permission.
    pub const fn route(self) -> AccountRoute {
        match self {
            Self::ChatGpt => AccountRoute::ManagedAppServer,
            Self::Claude => AccountRoute::OriginalCli,
            Self::OpenCode => AccountRoute::ProviderDependentCli,
            Self::Antigravity => AccountRoute::UnsupportedConsumerIntegration,
        }
    }

    /// Official policy or authentication reference, reviewed on 2026-10-25.
    pub const fn documentation(self) -> &'static str {
        match self {
            Self::ChatGpt => "https://developers.openai.com/codex/app-server/",
            Self::Claude => "https://code.claude.com/docs/en/legal-and-compliance",
            Self::OpenCode => "https://opencode.ai/docs/providers/",
            Self::Antigravity => "https://antigravity.google/docs/faq",
        }
    }

    /// Why this CLI integration route remains unavailable in this build.
    /// This does not describe direct Windows SIWC availability.
    ///
    /// A future qualified operation must use its own native broker; it must not
    /// open Zephium's legacy ambient command execution gate.
    pub const fn unavailable(self) -> AccountUnavailable {
        match self {
            Self::Antigravity => AccountUnavailable::ProviderUnsupported,
            _ => AccountUnavailable::RunnerNotQualified,
        }
    }
}

/// Closed availability reasons suitable for native UI projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountUnavailable {
    /// File, credential, network and lifecycle confinement is not qualified.
    RunnerNotQualified,
    /// The vendor does not support consumer subscription integration here.
    ProviderUnsupported,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_account_route_grants_production_execution() {
        for provider in [
            AccountProvider::ChatGpt,
            AccountProvider::Claude,
            AccountProvider::OpenCode,
        ] {
            assert_eq!(
                provider.unavailable(),
                AccountUnavailable::RunnerNotQualified
            );
        }
        assert_eq!(
            AccountProvider::Antigravity.unavailable(),
            AccountUnavailable::ProviderUnsupported
        );
        assert_eq!(
            AccountProvider::Antigravity.route(),
            AccountRoute::UnsupportedConsumerIntegration
        );
    }

    #[test]
    fn references_are_fixed_https_without_credentials_or_fragments() {
        for provider in [
            AccountProvider::ChatGpt,
            AccountProvider::Claude,
            AccountProvider::OpenCode,
            AccountProvider::Antigravity,
        ] {
            let url = url::Url::parse(provider.documentation()).unwrap();
            assert_eq!(url.scheme(), "https");
            assert!(url.username().is_empty());
            assert!(url.password().is_none());
            assert!(url.fragment().is_none());
        }
    }
}
