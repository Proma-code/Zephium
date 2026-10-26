//! Fixed original Codex app-server transport qualification for a Windows trial.
//!
//! The application/renderer has no process, login or turn constructor here yet.
//! Pure protocol/config validation and explicitly selected credential-free
//! fixtures are separate from any future user-authorized account integration.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(any(test, all(windows, feature = "native-fixtures")))]
mod config;
#[cfg(any(test, all(windows, feature = "native-fixtures")))]
mod pin;
#[cfg(any(test, all(windows, feature = "native-fixtures")))]
mod protocol;

#[cfg(all(windows, feature = "native-fixtures"))]
mod image;
#[cfg(all(windows, feature = "native-fixtures"))]
mod runtime;
#[cfg(all(windows, feature = "native-fixtures"))]
mod windows;

/// Exact original standalone app-server release, not a mutable PATH lookup.
pub const RELEASE: &str = "0.164.0";
/// Commit resolved from the annotated official release tag.
pub const SOURCE_COMMIT: &str = "6aaac694803004a63c6f393d153c96d22521a6b9";
/// SHA-256 of the original Windows x64 standalone app-server image.
pub const IMAGE_SHA256: &str = "82314aa236834a8b7240cc5fe4fdf71d4502822e476105ccbe010ef6689083d7";
/// Exact original image byte count.
pub const IMAGE_BYTES: u64 = 273_012_016;

#[cfg(all(windows, feature = "native-fixtures"))]
use image::PinnedImage;
#[cfg(all(windows, feature = "native-fixtures"))]
use runtime::RuntimePaths;

#[cfg(all(test, not(windows)))]
#[path = "windows/framing.rs"]
mod framing;

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
mod qualification;
#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub use qualification::qualify_original;

#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn qualify_transport() -> Result<(), &'static str> {
    windows::fixtures::qualify_transport()
}
#[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
pub fn fixture_child(mode: &str) -> Result<(), &'static str> {
    windows::fixtures::child(mode)
}
