//! Explicit per-profile OpenCode Go subscription access-key ownership.
//! No OAuth, provider CLI credential import, model guessing or automatic billing fallback.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod service;
pub mod vault;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Bounded native-only key, with an explicit acknowledgement of provider billing controls.
pub struct Config {
    id: String,
    key: Zeroizing<String>,
}
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenCodeGoConfig([redacted])")
    }
}
impl Config {
    fn new(key: Zeroizing<String>, id: String, billing_acknowledged: bool) -> Result<Self, ()> {
        if !billing_acknowledged
            || !valid_id(&id)
            || key.is_empty()
            || key.len() > 1024
            || !key.bytes().all(|b| matches!(b, 0x21..=0x7e))
        {
            return Err(());
        }
        Ok(Self { id, key })
    }
}
fn valid_profile(profile: &str) -> bool {
    zephium_core::ids::ProfileId::parse(profile).is_some_and(|id| id.to_string() == profile)
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn new_id() -> Result<String, ()> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| ())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    profile: String,
    id: String,
    key: Zeroizing<String>,
    billing_acknowledgement: u16,
}
