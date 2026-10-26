//! Atomic Windows Credential Manager record, distinct from API keys and OAuth.
use crate::{valid_profile, Config, Record};
use std::sync::Arc;
use zeroize::Zeroizing;

const MAX_RECORD: usize = 2000;

/// Content-free protected storage failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultError {
    /// Unsupported platform, invalid data or protected storage unavailable.
    Unavailable,
}

/// Native-only storage port; callers serialize mutations and own blocking tasks.
pub trait Vault: Send + Sync {
    /// Read only this exact profile. Missing is distinct from inaccessible.
    fn load(&self, profile: &str) -> Result<Option<Arc<Config>>, VaultError>;
    /// Atomically replace a single bounded credential record.
    fn save(&self, profile: &str, config: &Config) -> Result<(), VaultError>;
    /// Delete only this profile's local gateway record; missing is success.
    fn forget(&self, profile: &str) -> Result<(), VaultError>;
}

/// Windows-only production vault. Construction performs no credential access.
pub struct SystemVault {
    namespace: &'static str,
}
impl SystemVault {
    /// The application owns the namespace; IPC cannot supply it.
    pub fn new(namespace: &'static str) -> Result<Self, VaultError> {
        if !matches!(
            namespace,
            "app.zephium" | "app.zephium.dev" | "app.zephium.subscription-trial"
        ) {
            return Err(VaultError::Unavailable);
        }
        Ok(Self { namespace })
    }
    fn target(&self, profile: &str) -> Result<String, VaultError> {
        if !valid_profile(profile) {
            return Err(VaultError::Unavailable);
        }
        Ok(format!("{}.local-gateway|{profile}", self.namespace))
    }
}

pub(crate) fn encode(profile: &str, config: &Config) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    if !valid_profile(profile) {
        return Err(VaultError::Unavailable);
    }
    let record = Record {
        version: 1,
        profile: profile.into(),
        id: config.id.clone(),
        label: config.label.clone(),
        base: config.base.as_str().into(),
        key: config.key.clone(),
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&record).map_err(|_| VaultError::Unavailable)?);
    if bytes.len() > MAX_RECORD {
        return Err(VaultError::Unavailable);
    }
    Ok(bytes)
}

#[cfg(any(windows, test))]
pub(crate) fn decode(profile: &str, bytes: &[u8]) -> Result<Arc<Config>, VaultError> {
    if !valid_profile(profile) || bytes.len() > MAX_RECORD {
        return Err(VaultError::Unavailable);
    }
    let record: Record = serde_json::from_slice(bytes).map_err(|_| VaultError::Unavailable)?;
    if record.version != 1 || record.profile != profile {
        return Err(VaultError::Unavailable);
    }
    Config::new(record.label, record.base, record.key, record.id)
        .map(Arc::new)
        .map_err(|_| VaultError::Unavailable)
}

impl Vault for SystemVault {
    fn load(&self, profile: &str) -> Result<Option<Arc<Config>>, VaultError> {
        let target = self.target(profile)?;
        #[cfg(windows)]
        {
            let _turn = zephium_credentials::turn().map_err(|_| VaultError::Unavailable)?;
            return match zephium_credentials::read(&target) {
                Ok(bytes) => decode(profile, &bytes).map(Some),
                Err(zephium_credentials::VaultError::Missing) => Ok(None),
                Err(_) => Err(VaultError::Unavailable),
            };
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(VaultError::Unavailable)
        }
    }
    fn save(&self, profile: &str, config: &Config) -> Result<(), VaultError> {
        let target = self.target(profile)?;
        let bytes = encode(profile, config)?;
        #[cfg(windows)]
        {
            let _turn = zephium_credentials::turn().map_err(|_| VaultError::Unavailable)?;
            return zephium_credentials::write(&target, &bytes)
                .map_err(|_| VaultError::Unavailable);
        }
        #[cfg(not(windows))]
        {
            let _ = (target, bytes);
            Err(VaultError::Unavailable)
        }
    }
    fn forget(&self, profile: &str) -> Result<(), VaultError> {
        let target = self.target(profile)?;
        #[cfg(windows)]
        {
            let _turn = zephium_credentials::turn().map_err(|_| VaultError::Unavailable)?;
            return match zephium_credentials::delete(&target) {
                Ok(()) | Err(zephium_credentials::VaultError::Missing) => Ok(()),
                Err(_) => Err(VaultError::Unavailable),
            };
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err(VaultError::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PROFILE: &str = "00000000000000000000000001";
    fn config() -> Config {
        Config::new(
            "Local gateway".into(),
            "http://127.0.0.1:8045/v1".into(),
            Zeroizing::new("gateway-fixture".into()),
            "a".repeat(32),
        )
        .unwrap()
    }
    #[test]
    fn record_is_bounded_profile_bound_and_redacted() {
        let bytes = encode(PROFILE, &config()).unwrap();
        let restored = decode(PROFILE, &bytes).unwrap();
        assert_eq!(restored.base.as_str(), "http://127.0.0.1:8045/v1");
        assert_eq!(restored.id, "a".repeat(32));
        assert!(decode("00000000000000000000000002", &bytes).is_err());
        assert!(decode(PROFILE, &[b'x'; MAX_RECORD + 1]).is_err());
        assert!(!format!("{restored:?}").contains("gateway-fixture"));
        assert!(!format!("{restored:?}").contains("8045"));
    }
    #[test]
    fn exact_namespace_and_profile_are_required() {
        let trial = SystemVault::new("app.zephium.subscription-trial").unwrap();
        assert_eq!(
            trial.target(PROFILE).unwrap(),
            format!("app.zephium.subscription-trial.local-gateway|{PROFILE}")
        );
        assert!(trial.target("../profile").is_err());
        assert!(SystemVault::new("other.app").is_err());
        let prod = SystemVault::new("app.zephium").unwrap();
        assert_ne!(trial.target(PROFILE), prod.target(PROFILE));
    }
    #[test]
    fn malformed_or_extra_secret_fields_fail_closed() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&encode(PROFILE, &config()).unwrap()).unwrap();
        value["oauth_token"] = serde_json::json!("not-an-import-route");
        assert!(decode(PROFILE, &serde_json::to_vec(&value).unwrap()).is_err());
        value.as_object_mut().unwrap().remove("oauth_token");
        value["base"] = serde_json::json!("https://example.invalid/v1");
        assert!(decode(PROFILE, &serde_json::to_vec(&value).unwrap()).is_err());
    }
}
