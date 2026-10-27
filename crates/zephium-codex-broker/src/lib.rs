//! Original Codex app-server integration for the Windows subscription trial.
//! Pure catalog/protocol reducers have no process or account authority. Native
//! ownership is feature-gated and admits verified original runtimes;
//! credential-free fixtures remain separate from explicit user login/turns.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod config;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod pin;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod protocol;

#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
mod image;
#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
mod runtime;
#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
mod windows;

/// Exact original standalone app-server release, not a mutable PATH lookup.
pub const RELEASE: &str = "0.164.0";
/// Commit resolved from the annotated official release tag.
pub const SOURCE_COMMIT: &str = "6aaac694803004a63c6f393d153c96d22521a6b9";
/// SHA-256 of the original Windows x64 standalone app-server image.
pub const IMAGE_SHA256: &str = "82314aa236834a8b7240cc5fe4fdf71d4502822e476105ccbe010ef6689083d7";
/// Exact original image byte count.
pub const IMAGE_BYTES: u64 = 273_012_016;

#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
use image::PinnedImage;
#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
use runtime::RuntimePaths;

#[cfg(all(test, not(windows)))]
#[path = "windows/framing.rs"]
mod framing;

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
mod qualification;
#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub use qualification::{qualify_browser_schema, qualify_compatibility, qualify_original};

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn qualify_transport() -> Result<(), &'static str> {
    windows::fixtures::qualify_transport()
}
#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn fixture_child(mode: &str) -> Result<(), &'static str> {
    windows::fixtures::child(mode)
}

pub mod catalog;
pub use catalog::EffortSelection;
pub mod conversation;
#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
mod discovery;
mod effort_policy;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
pub mod profile;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod text_policy;

#[cfg(all(test, not(windows)))]
#[path = "discovery/plan.rs"]
mod discovery_plan;

#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
mod vault_probe;

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod managed_account;

#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
pub mod connection;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod wire;

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod notification_scope;

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod compatibility;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod release_admission;

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn qualify_owner() -> Result<(), &'static str> {
    connection::fixtures::qualify()
}

#[cfg(all(test, not(windows)))]
#[path = "windows/write_fence.rs"]
mod write_fence;

#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
mod release_fetch;

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn qualify_vault() -> Result<(), &'static str> {
    vault_probe::verify()?;
    println!("codex-vault-nonsecret-roundtrip: native-code=0");
    Ok(())
}

#[cfg(all(test, not(windows)))]
#[path = "windows/write_receipt.rs"]
mod write_receipt;

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod removal;

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod quota;

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod quota_owner;

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn qualify_quota() -> Result<(), &'static str> {
    connection::fixtures::qualify_quota()
}

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn qualify_removal() -> Result<(), &'static str> {
    connection::removal_fixtures::qualify()
}

#[cfg(all(test, not(windows)))]
#[path = "windows/environment_block.rs"]
mod environment_block;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod transport_environment;

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod preparation_budget;

// Browser actor cohort remains closed at the Page entry point until the exact
// native capability/dispatcher/local settlement join is qualified together.
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
pub mod browser_port;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod dynamic_tools;

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
#[path = "connection/browser.rs"]
mod browser_owner;

#[cfg(all(
    windows,
    any(feature = "native-fixtures", feature = "trial-integration")
))]
mod browser_capability;
#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
mod browser_schema;

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn qualify_browser_owner() -> Result<(), &'static str> {
    connection::browser_fixtures::qualify()
}

#[cfg(any(
    test,
    all(
        windows,
        any(feature = "native-fixtures", feature = "trial-integration")
    )
))]
#[path = "connection/events.rs"]
mod connection_events;
