//! Read-only classification of existing native identity and orphaned records.
use super::{Fault, Presence};
use crate::vault::{Stored, VaultError};
use zephium_core::ports::store::AppSettingReadback;

/// Read an existing installation identity without ever minting or persisting one.
pub fn existing_host(readback: AppSettingReadback) -> Result<Option<String>, Fault> {
    match readback {
        AppSettingReadback::Missing => Ok(None),
        AppSettingReadback::Value(host) if crate::token::valid_host(&host) => Ok(Some(host)),
        AppSettingReadback::Value(_) | AppSettingReadback::Unavailable => Err(Fault::Vault),
    }
}

/// Classify a canonical vault observation without adopting its host or credentials.
/// A missing installation setting alone is never proof that its vault is empty.
pub fn unbound_presence(record: Result<Stored, VaultError>) -> Presence {
    match record {
        Err(VaultError::Missing) => Presence::Missing,
        Ok(record) if record.identity.is_some() || record.pending_registration.is_some() => {
            Presence::Stored
        }
        _ => Presence::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::PendingRegistration;
    const HOST: &str = "urn:uuid:12345678-1234-4123-8123-123456789abc";
    #[test]
    fn existing_host_absence_is_distinct_from_unavailable_or_corrupt() {
        assert_eq!(existing_host(AppSettingReadback::Missing), Ok(None));
        assert_eq!(
            existing_host(AppSettingReadback::Value(HOST.into())),
            Ok(Some(HOST.into()))
        );
        assert_eq!(
            existing_host(AppSettingReadback::Unavailable),
            Err(Fault::Vault)
        );
        for value in [
            "",
            " ",
            "wrong-host",
            "urn:uuid:12345678-1234-1123-8123-123456789abc",
        ] {
            assert_eq!(
                existing_host(AppSettingReadback::Value(value.into())),
                Err(Fault::Vault)
            );
        }
    }
    #[test]
    fn missing_host_does_not_hide_pending_only_or_corrupt_vault_records() {
        assert_eq!(
            unbound_presence(Err(VaultError::Missing)),
            Presence::Missing
        );
        for fault in [VaultError::Invalid, VaultError::Inaccessible] {
            assert_eq!(unbound_presence(Err(fault)), Presence::Unavailable);
        }
        assert_eq!(
            unbound_presence(Ok(Stored {
                identity: None,
                tokens: None,
                pending_registration: Some(
                    PendingRegistration::new(HOST, "oaiapp_fixture").unwrap()
                )
            })),
            Presence::Stored
        );
    }
}
