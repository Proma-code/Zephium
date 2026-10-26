//! Explicit disposable-Windows qualification, never real account credentials.

const REQUIRED_ENVIRONMENT: [(&str, &str); 7] = [
    ("GITHUB_ACTIONS", "true"),
    ("GITHUB_REPOSITORY", "Proma-code/Zephium"),
    ("GITHUB_REF", "refs/heads/windows/subscription-accounts"),
    ("GITHUB_EVENT_NAME", "push"),
    ("RUNNER_ENVIRONMENT", "github-hosted"),
    ("RUNNER_OS", "Windows"),
    ("RUNNER_ARCH", "X64"),
];

fn qualification_allowed(mut value: impl FnMut(&str) -> Option<String>) -> bool {
    REQUIRED_ENVIRONMENT
        .iter()
        .all(|(name, expected)| value(name).as_deref() == Some(*expected))
}

#[test]
fn only_the_explicit_disposable_fork_environment_is_admitted() {
    assert!(qualification_allowed(|name| REQUIRED_ENVIRONMENT
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| (*value).into())));
}

#[test]
fn missing_or_changed_execution_scope_is_rejected() {
    for (changed, _) in REQUIRED_ENVIRONMENT {
        for replacement in [None, Some("different")] {
            assert!(!qualification_allowed(|name| {
                if name == changed {
                    replacement.map(str::to_owned)
                } else {
                    REQUIRED_ENVIRONMENT
                        .iter()
                        .find(|(key, _)| *key == name)
                        .map(|(_, value)| (*value).into())
                }
            }));
        }
    }
}

#[cfg(all(windows, debug_assertions, feature = "windows-subscription-trial"))]
mod windows {
    use super::*;
    use crate::token::Identity;
    use crate::vault::{prefix, AccountVault, Stored, SystemVault, VaultError, CHUNK};
    use zeroize::Zeroizing;

    const NAMESPACE: &str = "app.zephium.subscription-trial.vault-fixture";

    fn fresh_profile() -> String {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).expect("fixture randomness unavailable");
        zephium_core::ids::ProfileId::from(u128::from_be_bytes(bytes)).to_string()
    }

    fn targets(profile: &str) -> Result<Vec<String>, zephium_credentials::VaultError> {
        let exact = prefix(NAMESPACE, profile).expect("invalid synthetic fixture scope");
        let _turn = zephium_credentials::turn()?;
        zephium_credentials::list(&exact)
    }

    fn record(size: usize, byte: u8) -> Stored {
        Stored {
            identity: Some(Identity {
                issuer: "https://auth.openai.com".into(),
                subject: "synthetic-native-vault-fixture".into(),
                client_id: "oaiapp_native_vault_fixture".into(),
                host_id: "urn:uuid:12345678-1234-4123-8123-123456789abc".into(),
                email: None,
            }),
            pending_registration: None,
            tokens: (size > 0).then(|| Zeroizing::new(vec![byte; size])),
        }
    }

    fn matches_record(value: &Stored, size: usize, byte: u8) -> bool {
        value.identity.as_ref().is_some_and(|identity| {
            identity.subject == "synthetic-native-vault-fixture"
                && identity.client_id == "oaiapp_native_vault_fixture"
        }) && value.pending_registration.is_none()
            && match &value.tokens {
                Some(bytes) => bytes.len() == size && bytes.iter().all(|value| *value == byte),
                None => size == 0,
            }
    }

    #[test]
    #[ignore = "writes fresh synthetic trial credentials; explicit disposable Windows CI only"]
    fn native_trial_vault_roundtrip_preserves_other_fresh_profile_and_cleans_up() {
        assert!(
            qualification_allowed(|name| std::env::var(name).ok()),
            "native fixture requires the reviewed disposable Windows fork job"
        );
        let profiles = [fresh_profile(), fresh_profile()];
        assert!(profiles[0] != profiles[1], "fixture identity collision");
        // Never read/adopt an existing record or probe the product namespace.
        // A collision fails before either profile is written or cleanup admitted.
        for profile in &profiles {
            assert!(
                targets(profile)
                    .expect("fixture preflight unavailable")
                    .is_empty(),
                "fixture namespace was not empty"
            );
        }
        let vault = SystemVault::new(NAMESPACE).expect("fixture namespace invalid");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for profile in &profiles {
                assert!(matches!(vault.load(profile), Err(VaultError::Missing)));
            }
            assert!(
                !vault
                    .save(&profiles[0], &record(6000, b'a'))
                    .unwrap()
                    .cleanup_pending
            );
            assert!(matches_record(
                &vault.load(&profiles[0]).unwrap(),
                6000,
                b'a'
            ));
            assert_eq!(
                targets(&profiles[0]).unwrap().len(),
                1 + 6000_usize.div_ceil(CHUNK)
            );
            assert!(
                !vault
                    .save(&profiles[1], &record(8001, b'b'))
                    .unwrap()
                    .cleanup_pending
            );
            assert!(
                !vault
                    .save(&profiles[0], &record(12001, b'c'))
                    .unwrap()
                    .cleanup_pending
            );
            assert!(matches_record(
                &vault.load(&profiles[0]).unwrap(),
                12001,
                b'c'
            ));
            assert_eq!(
                targets(&profiles[0]).unwrap().len(),
                1 + 12001_usize.div_ceil(CHUNK)
            );
            assert!(matches_record(
                &vault.load(&profiles[1]).unwrap(),
                8001,
                b'b'
            ));
            assert!(
                !vault
                    .save(&profiles[0], &record(0, 0))
                    .unwrap()
                    .cleanup_pending
            );
            assert!(matches_record(&vault.load(&profiles[0]).unwrap(), 0, 0));
            assert_eq!(targets(&profiles[0]).unwrap().len(), 1);
            vault.forget(&profiles[0]).unwrap();
            assert!(matches!(vault.load(&profiles[0]), Err(VaultError::Missing)));
            assert!(targets(&profiles[0]).unwrap().is_empty());
            assert!(matches_record(
                &vault.load(&profiles[1]).unwrap(),
                8001,
                b'b'
            ));
        }));
        // Attempt both exact owned scopes even after an assertion panic. Failure
        // is never called successful cleanup. Process termination remains outside
        // this fixture's claim and can leave synthetic records on its disposable VM.
        let mut cleaned = true;
        for profile in &profiles {
            if vault.forget(profile).is_err() {
                cleaned = false;
            }
            if !targets(profile).is_ok_and(|remaining| remaining.is_empty()) {
                cleaned = false;
            }
        }
        assert!(cleaned, "native fixture cleanup could not be confirmed");
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
