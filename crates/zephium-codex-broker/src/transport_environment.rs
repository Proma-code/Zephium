//! Existing platform transport settings only; never account/provider configuration.
//!
//! Official Codex 0.163/0.164 `http-client::{outbound_proxy, custom_ca}` and
//! locked rustls-native-certs 0.8.3 consume this finite set. Original runtime
//! precedence, system/PAC selection, trust validation and managed network policy
//! remain authoritative. Inherited values can contain proxy credentials: never
//! log, serialize, normalize or use them as provider endpoint/auth authority.
//!
//! This portable carrier does not read the environment or access any file/network.
//! The Windows owner supplies only existing values for these fixed names, and
//! retains its separately constructed owned HOME/PATH/config environment.

use std::fmt;

/// Finite wrapper bounds, not a claim about every platform's maximum environment.
/// Oversized settings refuse the complete capture; they are never silently dropped.
const MAX_VALUE_UNITS: usize = 16 * 1024;
const MAX_BLOCK_UNITS: usize = 32 * 1024;
const NAMES: [&str; 7] = [
    "ALL_PROXY",
    "CODEX_CA_CERTIFICATE",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EnvFault {
    EmbeddedNul,
    ValueTooLarge,
    BlockTooLarge,
}

struct Entry {
    name: &'static str,
    value: Vec<u16>,
}

/// Move-only transport data; Debug deliberately exposes names only, never values.
/// This is not a credential store and makes no secure-erasure guarantee.
pub(crate) struct InheritedTransport {
    entries: Vec<Entry>,
}

impl InheritedTransport {
    /// The caller must use native Windows case-insensitive environment lookup.
    /// Empty and non-Unicode UTF-16 are retained exactly; only NUL/size is checked.
    pub(crate) fn capture(
        mut read: impl FnMut(&'static str) -> Option<Vec<u16>>,
    ) -> Result<Self, EnvFault> {
        let mut entries = Vec::new();
        // Includes a final environment-block terminator as a conservative bound.
        let mut units = 1usize;
        for name in NAMES {
            let Some(value) = read(name) else {
                continue;
            };
            if value.len() > MAX_VALUE_UNITS {
                return Err(EnvFault::ValueTooLarge);
            }
            if value.contains(&0) {
                return Err(EnvFault::EmbeddedNul);
            }
            units = units
                .checked_add(name.len() + 1 + value.len() + 1)
                .filter(|total| *total <= MAX_BLOCK_UNITS)
                .ok_or(EnvFault::BlockTooLarge)?;
            entries.push(Entry { name, value });
        }
        Ok(Self { entries })
    }

    /// Only the native environment-block builder should consume these values.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&'static str, &[u16])> {
        self.entries
            .iter()
            .map(|entry| (entry.name, entry.value.as_slice()))
    }
}

impl fmt::Debug for InheritedTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InheritedTransport")
            .field(
                "present_names",
                &self
                    .entries
                    .iter()
                    .map(|entry| entry.name)
                    .collect::<Vec<_>>(),
            )
            .field("values", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
#[path = "transport_environment/tests.rs"]
mod tests;
