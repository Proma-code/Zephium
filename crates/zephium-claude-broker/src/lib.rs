//! Original Claude Code hosting boundary, not an Anthropic API credential adapter.
//!
//! The production gate is closed until the exact native image and Windows process,
//! private-directory, network and login contracts have been qualified. Portable
//! parsers and synthetic LPAC fixtures do not confer account or execution authority.
#![deny(missing_docs)]
#![deny(clippy::dbg_macro, clippy::print_stderr, clippy::print_stdout)]

#[allow(dead_code)]
mod account;
#[allow(dead_code)]
mod auth;
pub mod custody;
pub mod plan;
pub mod protocol;
mod strict_json;

#[cfg(windows)]
#[allow(dead_code)]
mod windows;

#[cfg(all(feature = "native-fixtures", not(debug_assertions)))]
compile_error!("Claude broker synthetic fixtures are forbidden in optimized builds");

/// A closed reason why the original vendor client cannot yet run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unavailable {
    /// The original, unmodified native image/runtime has no reviewed pinned identity.
    VendorImageNotQualified,
    /// Production hosting is limited to the reviewed Windows implementation.
    WindowsRequired,
}

/// Inspect the compiled broker qualification, without discovering or running a CLI.
///
/// There is deliberately no environment, file-presence, UI consent, or feature-flag
/// bypass. Native fixtures exercise the candidate runtime, not this admission gate.
pub const fn availability() -> Result<(), Unavailable> {
    if cfg!(windows) {
        Err(Unavailable::VendorImageNotQualified)
    } else {
        Err(Unavailable::WindowsRequired)
    }
}

/// Honest egress description for the proposed LPAC capability.
pub const NETWORK_POLICY: &str =
    "Outbound public Internet (internetClient); not a provider-host allowlist";

/// Run the fixed, credential-free Windows fixture scenario against a test helper.
/// This is absent from ordinary builds and never accepts an arbitrary command.
#[cfg(all(windows, feature = "native-fixtures"))]
pub fn qualify_synthetic_fixture() -> Result<(), &'static str> {
    windows::fixtures::qualify()
}

/// Debug-only actual suspended AC/LPAC token ACL controls. Neither control is
/// resumed; these measurements do not enable vendor or production execution.
#[cfg(all(windows, feature = "native-fixtures"))]
pub fn qualify_token_controls() -> Result<(), &'static str> {
    windows::fixtures::token_controls()
}

/// Debug-only synthetic interactive stdin/prompt lifecycle. This never invokes a
/// vendor login, browser, credential read, grant or account status operation.
#[cfg(all(windows, feature = "native-fixtures"))]
pub fn qualify_synthetic_auth_inputs() -> Result<(), &'static str> {
    windows::fixtures::auth_inputs()
}

/// Debug-only differential Winsock/capability experiment. This does not change
/// the vendor capability set or production qualification.
#[cfg(all(windows, feature = "native-fixtures"))]
pub fn research_network_controls() -> Result<(), &'static str> {
    windows::network_research::controls()
}

/// Fixed synthetic child for the differential experiment; reads only the bounded
/// fixture request on stdin. Not part of the vendor command vocabulary.
#[cfg(all(windows, feature = "native-fixtures"))]
pub fn research_network_child() -> Result<(), &'static str> {
    windows::network_research::child()
}

/// Debug-only startup probe of one exact hash-pinned, Authenticode-verified vendor
/// image. Only `--version` and restricted safe `--help`; no account operations.
#[cfg(all(windows, feature = "native-fixtures"))]
pub fn qualify_vendor_startup(image: &std::path::Path) -> Result<(), &'static str> {
    windows::fixtures::vendor_probe(image)
}

#[cfg(test)]
mod tests {
    #[test]
    fn portable_or_native_fixtures_never_enable_the_vendor() {
        assert!(super::availability().is_err());
        assert!(super::NETWORK_POLICY.contains("not a provider-host allowlist"));
    }
}
