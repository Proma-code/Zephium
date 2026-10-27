use super::*;
use crate::token::Record;
use crate::vault::SaveOutcome;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use zeroize::Zeroizing;

const PROFILE: &str = "00000000000000000000000001";
const HOST: &str = "urn:uuid:12345678-1234-4123-8123-123456789abc";
fn tokens() -> Tokens {
    Tokens(Record {
        version: 1,
        identity: Identity {
            issuer: "https://auth.openai.com".into(),
            subject: "fixture".into(),
            client_id: "oaiapp_fixture".into(),
            host_id: HOST.into(),
            email: None,
        },
        access_token: Zeroizing::new("synthetic-access".into()),
        refresh_token: Zeroizing::new("synthetic-refresh".into()),
        id_token: Zeroizing::new("synthetic-id".into()),
        nonce: Zeroizing::new("synthetic-nonce".into()),
        scopes: vec![
            "openid".into(),
            "resource.invoke".into(),
            "chatgpt.tokens.use.direct".into(),
        ],
        expires_at: 1,
    })
}
fn record() -> Stored {
    let token = tokens();
    Stored {
        identity: Some(token.identity().clone()),
        tokens: Some(token.to_bytes().unwrap()),
        pending_registration: None,
    }
}
fn pending() -> Stored {
    Stored {
        identity: None,
        tokens: None,
        pending_registration: Some(PendingRegistration::new(HOST, "oaiapp_pending").unwrap()),
    }
}
fn copy(record: &Stored) -> Stored {
    Stored {
        identity: record.identity.clone(),
        pending_registration: record.pending_registration.clone(),
        tokens: record.tokens.clone(),
    }
}
#[derive(Default)]
struct MemoryVault {
    record: StdMutex<Option<Stored>>,
    reads: AtomicUsize,
    saves: AtomicUsize,
    forgets: AtomicUsize,
    inaccessible: AtomicBool,
    save_fails: AtomicBool,
    cleanup_pending: AtomicBool,
}
impl AccountVault for MemoryVault {
    fn load(&self, _: &str) -> Result<Stored, VaultError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.inaccessible.load(Ordering::SeqCst) {
            return Err(VaultError::Inaccessible);
        }
        self.record
            .lock()
            .unwrap()
            .as_ref()
            .map(copy)
            .ok_or(VaultError::Missing)
    }
    fn save(&self, _: &str, record: &Stored) -> Result<SaveOutcome, VaultError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        if self.save_fails.load(Ordering::SeqCst) {
            return Err(VaultError::Inaccessible);
        }
        *self.record.lock().unwrap() = Some(copy(record));
        Ok(SaveOutcome {
            cleanup_pending: self.cleanup_pending.load(Ordering::SeqCst),
        })
    }
    fn forget(&self, _: &str) -> Result<(), VaultError> {
        self.forgets.fetch_add(1, Ordering::SeqCst);
        if self.inaccessible.load(Ordering::SeqCst) {
            return Err(VaultError::Inaccessible);
        }
        *self.record.lock().unwrap() = None;
        Ok(())
    }
}
#[derive(Default)]
struct Fake {
    revocations: AtomicUsize,
    fail: AtomicBool,
    block: AtomicBool,
    started: Notify,
    release: Notify,
}
impl Transport for Fake {
    fn revoke<'a>(&'a self, _: &'a Tokens) -> PortFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            self.revocations.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            if self.block.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                Err(AuthError::Transport)
            } else {
                Ok(())
            }
        })
    }
}
fn fixture(record: Option<Stored>) -> (Arc<Service>, Arc<MemoryVault>, Arc<Fake>) {
    let vault = Arc::new(MemoryVault::default());
    *vault.record.lock().unwrap() = record;
    let transport = Arc::new(Fake::default());
    let service = Arc::new(Service {
        profile: PROFILE.into(),
        host_id: HOST.into(),
        transport: transport.clone(),
        vault: vault.clone(),
        mutation: Mutex::new(()),
        state: RwLock::new(State {
            foreground: true,
            ..Default::default()
        }),
        changed: Arc::new(|| {}),
        tasks: Arc::new(Tasks::default()),
    });
    (service, vault, transport)
}
#[tokio::test]
async fn clean_profile_read_confirms_missing_without_any_write_or_network() {
    let (service, vault, transport) = fixture(None);
    assert_eq!(service.view().presence, Presence::Unavailable);
    service.load().await.unwrap();
    service.load().await.unwrap();
    assert_eq!(service.view().presence, Presence::Missing);
    assert!(!service.view().connected);
    assert_eq!(vault.reads.load(Ordering::SeqCst), 1);
    assert_eq!(vault.saves.load(Ordering::SeqCst), 0);
    assert_eq!(vault.forgets.load(Ordering::SeqCst), 0);
    assert_eq!(transport.revocations.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn pending_registration_is_visible_without_email_connection_or_native_writes() {
    let (service, vault, transport) = fixture(Some(pending()));
    service.load().await.unwrap();
    assert_eq!(service.view().presence, Presence::Stored);
    assert!(!service.view().connected);
    assert!(service.view().email.is_none());
    assert_eq!(vault.saves.load(Ordering::SeqCst), 0);
    assert_eq!(transport.revocations.load(Ordering::SeqCst), 0);
    service.sign_out().await.unwrap();
    assert_eq!(service.view().presence, Presence::Stored);
    assert!(vault.load(PROFILE).unwrap().pending_registration.is_some());
    assert_eq!(transport.revocations.load(Ordering::SeqCst), 0);
    service.erase().await.unwrap();
    assert_eq!(service.view().presence, Presence::Missing);
    assert!(matches!(vault.load(PROFILE), Err(VaultError::Missing)));
}
#[tokio::test]
async fn corrupt_mismatched_or_inaccessible_record_never_becomes_missing() {
    let (service, vault, _) = fixture(Some(record()));
    vault.inaccessible.store(true, Ordering::SeqCst);
    assert_eq!(service.load().await, Err(Fault::Vault));
    assert_eq!(service.view().presence, Presence::Unavailable);
    vault.inaccessible.store(false, Ordering::SeqCst);
    vault.record.lock().unwrap().as_mut().unwrap().tokens =
        Some(Zeroizing::new(b"not-json".to_vec()));
    assert_eq!(service.load().await, Err(Fault::Vault));
    assert_eq!(service.view().presence, Presence::Unavailable);
    let mut different = pending();
    different.pending_registration.as_mut().unwrap().host_id =
        "urn:uuid:12345678-1234-4123-8123-123456789abd".into();
    *vault.record.lock().unwrap() = Some(different);
    assert_eq!(service.load().await, Err(Fault::Vault));
    assert_eq!(service.view().presence, Presence::Unavailable);
    assert_eq!(vault.saves.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn expired_grant_observation_does_not_refresh_or_rewrite_stored_credentials() {
    let (service, vault, transport) = fixture(Some(record()));
    service.load().await.unwrap();
    assert_eq!(service.view().presence, Presence::Stored);
    assert!(
        service.view().connected,
        "projection means stored grant, not live access"
    );
    assert_eq!(vault.saves.load(Ordering::SeqCst), 0);
    assert_eq!(transport.revocations.load(Ordering::SeqCst), 0);
    assert_eq!(
        Tokens::from_bytes(&vault.load(PROFILE).unwrap().tokens.unwrap())
            .unwrap()
            .0
            .expires_at,
        1
    );
}
#[tokio::test]
async fn explicit_disconnect_clears_tokens_retains_registration_and_reports_remote_failure() {
    let (service, vault, transport) = fixture(Some(record()));
    transport.fail.store(true, Ordering::SeqCst);
    service.sign_out().await.unwrap();
    assert!(!service.view().connected);
    assert!(service.view().revocation_unconfirmed);
    let stored = vault.load(PROFILE).unwrap();
    assert!(stored.tokens.is_none());
    assert!(stored.identity.is_some());
    transport.fail.store(false, Ordering::SeqCst);
    service.sign_out().await.unwrap();
    assert!(!service.view().revocation_unconfirmed);
    assert_eq!(transport.revocations.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn committed_disconnect_reports_pending_cleanup_without_reactivating_grant() {
    let (service, vault, _) = fixture(Some(record()));
    vault.cleanup_pending.store(true, Ordering::SeqCst);
    assert_eq!(service.sign_out().await, Ok(()));
    assert_eq!(service.view().fault, Some(Fault::Vault));
    assert!(!service.view().connected);
    assert!(vault.load(PROFILE).unwrap().tokens.is_none());
}
#[tokio::test]
async fn failed_local_disconnect_is_not_reported_as_successful_deletion() {
    let (service, vault, _) = fixture(Some(record()));
    vault.save_fails.store(true, Ordering::SeqCst);
    assert_eq!(service.sign_out().await, Err(Fault::Vault));
    assert_eq!(service.view().fault, Some(Fault::Vault));
    assert!(vault.load(PROFILE).unwrap().tokens.is_some());
    vault.save_fails.store(false, Ordering::SeqCst);
    assert_eq!(service.sign_out().await, Ok(()));
    assert!(vault.load(PROFILE).unwrap().tokens.is_none());
}
#[tokio::test]
async fn dropped_disconnect_waiter_does_not_abandon_revocation_or_owned_drain() {
    let (service, vault, transport) = fixture(Some(record()));
    transport.block.store(true, Ordering::SeqCst);
    let waiter = tokio::spawn({
        let service = service.clone();
        async move { service.sign_out().await }
    });
    transport.started.notified().await;
    waiter.abort();
    let _ = waiter.await;
    service.close();
    assert!(!service.closed_and_drained());
    assert!(vault.load(PROFILE).unwrap().tokens.is_none());
    transport.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), service.drained())
        .await
        .unwrap();
    assert!(service.closed_and_drained());
}
#[tokio::test]
async fn stale_profile_scope_cannot_read_disconnect_or_recreate_registration() {
    let (service, vault, transport) = fixture(Some(record()));
    let scope = service.scope().unwrap();
    service.set_foreground(false);
    service.set_foreground(true);
    assert_eq!(service.load_scoped(scope).await, Err(Fault::Cancelled));
    assert_eq!(service.sign_out_scoped(scope).await, Err(Fault::Cancelled));
    assert_eq!(vault.reads.load(Ordering::SeqCst), 0);
    assert_eq!(vault.saves.load(Ordering::SeqCst), 0);
    assert_eq!(transport.revocations.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn queued_disconnect_does_not_mutate_after_profile_bounce() {
    let (service, vault, _) = fixture(Some(record()));
    service.load().await.unwrap();
    let held = service.mutation.lock().await;
    let scope = service.scope().unwrap();
    let task = tokio::spawn({
        let service = service.clone();
        async move { service.sign_out_scoped(scope).await }
    });
    while service.tasks.state.lock().unwrap().active == 0 {
        tokio::task::yield_now().await;
    }
    service.set_foreground(false);
    service.set_foreground(true);
    drop(held);
    assert_eq!(task.await.unwrap(), Err(Fault::Cancelled));
    assert!(service.view().connected);
    assert_eq!(vault.saves.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn erasure_retry_never_claims_missing_until_vault_acknowledges() {
    let (service, vault, _) = fixture(Some(pending()));
    service.load().await.unwrap();
    vault.inaccessible.store(true, Ordering::SeqCst);
    assert_eq!(service.erase().await, Err(Fault::Vault));
    assert_eq!(service.view().presence, Presence::Stored);
    vault.inaccessible.store(false, Ordering::SeqCst);
    service.erase().await.unwrap();
    assert_eq!(service.view().presence, Presence::Missing);
    assert!(matches!(vault.load(PROFILE), Err(VaultError::Missing)));
    service.drained().await;
}
