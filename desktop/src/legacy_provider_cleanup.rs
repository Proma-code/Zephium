//! Compatibility-only erasure for retired connectors. No startup reads, login,
//! token refresh, inference, or remote revocation. The profile-deletion journal
//! is the only caller allowed to erase these exact, historical local targets.

pub(crate) fn install() {
    #[cfg(all(windows, feature = "work-product"))]
    zephium_app::work_models::install_legacy_provider_cleanup(std::sync::Arc::new(Cleanup));
}

#[cfg(all(windows, feature = "work-product"))]
struct Cleanup;

#[cfg(all(windows, feature = "work-product"))]
impl zephium_app::work_models::LegacyProviderCleanup for Cleanup {
    fn erase_profile(
        &self,
        profile: zephium_core::ids::ProfileId,
        deadline: std::time::Instant,
    ) -> bool {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        tauri::async_runtime::block_on(async move {
            tokio::time::timeout(
                remaining,
                tokio::task::spawn_blocking(move || {
                    let Ok(_turn) = zephium_credentials::turn_within(
                        deadline.saturating_duration_since(std::time::Instant::now()),
                    ) else {
                        return false;
                    };
                    erase(&Native, &profile.to_string(), deadline)
                }),
            )
            .await
            .is_ok_and(|result| result.unwrap_or(false))
        })
    }
}

#[cfg(any(test, all(windows, feature = "work-product")))]
trait Vault {
    fn delete(&self, target: &str) -> bool;
    fn list(&self, prefix: &str) -> Option<Vec<String>>;
}

#[cfg(all(windows, feature = "work-product"))]
struct Native;
#[cfg(all(windows, feature = "work-product"))]
impl Vault for Native {
    fn delete(&self, target: &str) -> bool {
        matches!(
            zephium_credentials::delete(target),
            Ok(()) | Err(zephium_credentials::VaultError::Missing)
        )
    }
    fn list(&self, prefix: &str) -> Option<Vec<String>> {
        zephium_credentials::list(prefix).ok()
    }
}

#[cfg(any(test, all(windows, feature = "work-product")))]
fn erase(vault: &impl Vault, profile: &str, deadline: std::time::Instant) -> bool {
    if zephium_core::ids::ProfileId::parse(profile).is_none_or(|id| id.to_string() != profile) {
        return false;
    }
    let namespace = if cfg!(feature = "windows-subscription-trial") {
        "app.zephium.subscription-trial"
    } else {
        "app.zephium"
    };
    let prefix = format!("{namespace}.chatgpt|{profile}|");
    let manifest = format!("{prefix}manifest");
    let mut complete = true;
    // Retire the manifest first, including malformed/partial old records.
    for target in [
        manifest.clone(),
        format!("{namespace}.local-gateway|{profile}"),
        format!("{namespace}.opencode-go|{profile}"),
    ] {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        complete &= vault.delete(&target);
    }
    if std::time::Instant::now() >= deadline {
        return false;
    }
    let Some(targets) = vault.list(&prefix) else {
        return false;
    };
    if targets.len() > 4096 {
        return false;
    }
    for target in targets {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        if !target.starts_with(&prefix) {
            complete = false;
            continue;
        }
        if target != manifest {
            complete &= vault.delete(&target);
        }
    }
    complete && std::time::Instant::now() < deadline
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    #[derive(Default)]
    struct Memory {
        deleted: RefCell<Vec<String>>,
        fail: bool,
        foreign: bool,
        inventory_failed: bool,
    }
    impl Vault for Memory {
        fn delete(&self, target: &str) -> bool {
            self.deleted.borrow_mut().push(target.into());
            !self.fail
        }
        fn list(&self, prefix: &str) -> Option<Vec<String>> {
            if self.inventory_failed {
                return None;
            }
            Some(vec![
                format!("{prefix}chunk-old"),
                if self.foreign {
                    "app.zephium.chatgpt|OTHER|chunk".into()
                } else {
                    format!("{prefix}manifest")
                },
            ])
        }
    }
    #[test]
    fn explicit_erasure_preserves_other_profiles_and_retires_manifest_first() {
        let vault = Memory::default();
        assert!(erase(
            &vault,
            "00000000000000000000000001",
            Instant::now() + Duration::from_secs(1)
        ));
        let deleted = vault.deleted.borrow();
        assert_eq!(deleted.len(), 4);
        assert!(deleted[0].ends_with("|manifest"));
        assert_eq!(
            deleted
                .iter()
                .all(|key| key.starts_with("app.zephium.subscription-trial.")),
            cfg!(feature = "windows-subscription-trial")
        );
        assert!(deleted
            .iter()
            .all(|key| key.contains("00000000000000000000000001")));
    }
    #[test]
    fn uncertain_or_foreign_vault_results_do_not_acknowledge_erasure() {
        for vault in [
            Memory {
                fail: true,
                ..Default::default()
            },
            Memory {
                inventory_failed: true,
                ..Default::default()
            },
            Memory {
                foreign: true,
                ..Default::default()
            },
        ] {
            assert!(!erase(
                &vault,
                "00000000000000000000000001",
                Instant::now() + Duration::from_secs(1)
            ));
            assert!(!vault
                .deleted
                .borrow()
                .iter()
                .any(|key| key.contains("OTHER")));
        }
    }
    #[test]
    fn expired_or_invalid_erasure_never_touches_vault() {
        let vault = Memory::default();
        assert!(!erase(
            &vault,
            "invalid",
            Instant::now() + Duration::from_secs(1)
        ));
        assert!(!erase(&vault, "00000000000000000000000001", Instant::now()));
        assert!(vault.deleted.borrow().is_empty());
    }
}
