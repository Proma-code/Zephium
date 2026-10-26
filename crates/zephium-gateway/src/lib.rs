//! An explicit, profile-owned connection to an existing local gateway.
//! This is not vendor OAuth, subscription verification, or an automatic fallback.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod service;
pub mod vault;

use serde::{Deserialize, Serialize};
use zephium_agentic::lead::local_gateway::LoopbackBase;
use zeroize::Zeroizing;

/// Bounded private configuration. Its secret never appears in Debug or a view.
pub struct Config {
    id: String,
    label: String,
    base: LoopbackBase,
    key: Zeroizing<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GatewayConfig([redacted])")
    }
}

impl Config {
    fn new(label: String, base: String, key: Zeroizing<String>, id: String) -> Result<Self, ()> {
        if !valid_id(&id)
            || label.is_empty()
            || label.len() > 64
            || label.trim() != label
            || label.chars().any(char::is_control)
            || key.is_empty()
            || key.len() > 1024
            || !key.bytes().all(|b| matches!(b, 0x21..=0x7e))
        {
            return Err(());
        }
        Ok(Self {
            id,
            label,
            base: LoopbackBase::parse(&base).map_err(|_| ())?,
            key,
        })
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
    label: String,
    base: String,
    key: Zeroizing<String>,
}
