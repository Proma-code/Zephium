use super::*;
use crate::vault::VaultError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Semaphore;
use zephium_agentic::lead::opencode_go::quota::GoQuotaWindow;

const PROFILE: &str = "00000000000000000000000001";
#[derive(Default)]
struct Memory {
    record: StdMutex<Option<Arc<Config>>>,
    fail: AtomicBool,
    block_load: StdMutex<Option<std::sync::mpsc::Receiver<()>>>,
    load_entered: StdMutex<Option<std::sync::mpsc::Sender<()>>>,
}
impl Vault for Memory {
    fn load(&self, _: &str) -> Result<Option<Arc<Config>>, VaultError> {
        if let Some(tx) = self.load_entered.lock().unwrap().take() {
            tx.send(()).unwrap();
        }
        if let Some(rx) = self.block_load.lock().unwrap().take() {
            rx.recv().unwrap();
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(VaultError::Unavailable);
        }
        Ok(self.record.lock().unwrap().clone())
    }
    fn save(&self, _: &str, config: &Config) -> Result<(), VaultError> {
        *self.record.lock().unwrap() = Some(Arc::new(
            Config::new(config.key.clone(), config.id.clone(), true).unwrap(),
        ));
        Ok(())
    }
    fn forget(&self, _: &str) -> Result<(), VaultError> {
        *self.record.lock().unwrap() = None;
        Ok(())
    }
}
struct Fake {
    calls: AtomicUsize,
    result: StdMutex<Result<GoQuotaSnapshot, GoQuotaFault>>,
    block: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}
impl Fake {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            result: StdMutex::new(Ok(reading())),
            block: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}
impl Transport for Fake {
    fn quota<'a>(&'a self, _: &'a Config) -> PortFuture<'a, Result<GoQuotaSnapshot, GoQuotaFault>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            if self.block.load(Ordering::SeqCst) {
                self.release.acquire().await.unwrap().forget();
            }
            *self.result.lock().unwrap()
        })
    }
    fn models<'a>(&'a self, _: &'a Config) -> PortFuture<'a, Result<Vec<GoModel>, GoFailure>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn call<'a>(
        &'a self,
        _: &'a Config,
        _: SessionId,
        _: WorkModelRequest,
        _: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> PortFuture<'a, Result<GoOutcome, GoFailure>> {
        Box::pin(async { panic!("quota must not invoke inference") })
    }
}
fn reading() -> GoQuotaSnapshot {
    let window = GoQuotaWindow {
        used_percent: 32.5,
        remaining_percent: 67.5,
        resets_at_ms: now_ms().unwrap() + 86_400_000,
        limited: false,
    };
    GoQuotaSnapshot {
        rolling: window,
        weekly: window,
        monthly: window,
    }
}
fn setup(memory: Arc<Memory>) -> (Arc<Service>, Arc<Fake>) {
    let mut service = Service::new(PROFILE.into(), memory, Arc::new(|| {})).unwrap();
    let fake = Arc::new(Fake::new());
    service.transport = fake.clone();
    service.set_foreground(true);
    (Arc::new(service), fake)
}
async fn save(service: &Arc<Service>) {
    service
        .configure(
            service.view().connection_id,
            Zeroizing::new("fixture-key".into()),
            true,
        )
        .await
        .unwrap();
}
async fn refresh(service: &Arc<Service>) -> Result<(), QuotaFault> {
    service.check_quota(service.view().connection_id).await
}

#[tokio::test]
async fn explicit_reading_has_provider_windows_and_never_changes_catalog_or_billing() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    service.load().await.unwrap();
    service
        .check_models(service.view().connection_id)
        .await
        .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    let catalog = service.view().checked_at_ms;
    refresh(&service).await.unwrap();
    let view = service.view();
    assert_eq!(view.quota.status, QuotaStatus::Current);
    assert!(!view.quota.checking);
    assert_eq!(view.quota.reading.unwrap().rolling.remaining_percent, 67.5);
    assert_eq!(view.checked_at_ms, catalog);
    assert!(view.last_billing.is_none());
    assert!(view.quota.checked_at_ms.is_some());
    assert!(view.quota.attempted_at_ms.is_some());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn repeated_clicks_share_one_native_flight_without_a_second_request() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    fake.block.store(true, Ordering::SeqCst);
    let owner = service.clone();
    let first = tokio::spawn(async move { refresh(&owner).await });
    fake.entered.acquire().await.unwrap().forget();
    assert!(service.view().quota.checking);
    assert_eq!(refresh(&service).await, Err(QuotaFault::Busy));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    fake.release.add_permits(1);
    first.await.unwrap().unwrap();
    assert!(!service.view().quota.checking);
}
#[tokio::test]
async fn failures_keep_only_same_connection_last_good_timestamp_as_stale() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    refresh(&service).await.unwrap();
    let original = service.view().quota;
    for fault in [
        GoQuotaFault::Timeout,
        GoQuotaFault::Unavailable,
        GoQuotaFault::Protocol,
        GoQuotaFault::Forbidden,
        GoQuotaFault::EndpointUnavailable,
        GoQuotaFault::RateLimited {
            retry_after_ms: Some(1000),
        },
        GoQuotaFault::TooLarge,
    ] {
        *fake.result.lock().unwrap() = Err(fault);
        assert_eq!(refresh(&service).await, Err(fault.into()));
        let view = service.view().quota;
        assert_eq!(view.status, QuotaStatus::Stale);
        assert_eq!(view.reading, original.reading);
        assert_eq!(view.checked_at_ms, original.checked_at_ms);
        assert_eq!(view.fault, Some(fault.into()));
    }
}
#[tokio::test]
async fn explicit_entitlement_refusal_or_unauthorized_clears_allowance_not_key() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    for fault in [GoQuotaFault::NoEntitlement, GoQuotaFault::Unauthorized] {
        *fake.result.lock().unwrap() = Ok(reading());
        refresh(&service).await.unwrap();
        let id = service.view().connection_id;
        *fake.result.lock().unwrap() = Err(fault);
        assert_eq!(refresh(&service).await, Err(fault.into()));
        let view = service.view();
        assert_eq!(view.connection_id, id);
        assert!(view.configured);
        assert!(view.quota.reading.is_none());
        assert!(view.quota.checked_at_ms.is_none());
        assert_eq!(
            view.quota.status,
            if fault == GoQuotaFault::NoEntitlement {
                QuotaStatus::NoEntitlement
            } else {
                QuotaStatus::Unavailable
            }
        );
    }
}
#[tokio::test]
async fn replacement_and_disconnect_clear_observations_and_cancel_owned_flight() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    refresh(&service).await.unwrap();
    fake.block.store(true, Ordering::SeqCst);
    fake.entered.try_acquire().unwrap().forget();
    let owner = service.clone();
    let pending = tokio::spawn(async move { refresh(&owner).await });
    fake.entered.acquire().await.unwrap().forget();
    save(&service).await;
    assert_eq!(pending.await.unwrap(), Err(QuotaFault::Cancelled));
    assert_eq!(service.view().quota.status, QuotaStatus::NotChecked);
    assert!(service.view().quota.reading.is_none());
    fake.block.store(false, Ordering::SeqCst);
    refresh(&service).await.unwrap();
    service
        .disconnect(service.view().connection_id)
        .await
        .unwrap();
    assert_eq!(service.view().quota, QuotaView::default());
}
#[tokio::test]
async fn scope_bounce_cancels_without_reopening_or_publishing_old_reading() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    fake.block.store(true, Ordering::SeqCst);
    let scope = service.scope().unwrap();
    let owner = service.clone();
    let pending = tokio::spawn(async move { refresh(&owner).await });
    fake.entered.acquire().await.unwrap().forget();
    service.set_foreground(false);
    service.set_foreground(true);
    assert_eq!(pending.await.unwrap(), Err(QuotaFault::Cancelled));
    assert!(!service.view().quota.checking);
    assert_eq!(
        service
            .check_quota_scoped(scope, service.view().connection_id)
            .await,
        Err(QuotaFault::Cancelled)
    );
    assert!(service.view().quota.reading.is_none());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn dropped_ipc_waiter_does_not_orphan_checking_or_owned_drain() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    fake.block.store(true, Ordering::SeqCst);
    let owner = service.clone();
    let pending = tokio::spawn(async move { refresh(&owner).await });
    fake.entered.acquire().await.unwrap().forget();
    pending.abort();
    fake.release.add_permits(1);
    service.drained().await;
    assert_eq!(service.view().quota.status, QuotaStatus::Current);
    assert!(!service.view().quota.checking);
    service.close();
    assert!(service.closed_and_drained());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_load_scope_bounce_and_vault_failure_never_send_a_request() {
    let memory = Arc::new(Memory::default());
    let (service, _) = setup(memory.clone());
    save(&service).await;
    let id = service.view().connection_id;
    let (tx, rx) = std::sync::mpsc::channel();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    *memory.block_load.lock().unwrap() = Some(rx);
    *memory.load_entered.lock().unwrap() = Some(entered_tx);
    let (cold, fake) = setup(memory.clone());
    let owner = cold.clone();
    let expected = id.clone();
    let pending = tokio::spawn(async move { owner.check_quota(expected).await });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    cold.set_foreground(false);
    cold.set_foreground(true);
    tx.send(()).unwrap();
    assert_eq!(pending.await.unwrap(), Err(QuotaFault::Cancelled));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    memory.fail.store(true, Ordering::SeqCst);
    let (failed, fake) = setup(memory);
    assert_eq!(failed.check_quota(id).await, Err(QuotaFault::Vault));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    assert!(!failed.view().quota.checking);
    assert_eq!(failed.view().quota.fault, Some(QuotaFault::Vault));
}
#[test]
fn freshness_uses_provider_reset_and_last_success_not_attempt_or_plan() {
    let mut state = QuotaState {
        reading: Some(reading()),
        checked_at_ms: Some(1000),
        attempted_at_ms: Some(5000),
        ..Default::default()
    };
    assert_eq!(state.view(Some(1001)).status, QuotaStatus::Current);
    assert_eq!(state.view(Some(999)).status, QuotaStatus::Stale);
    assert_eq!(state.view(Some(301000)).status, QuotaStatus::Stale);
    assert_eq!(state.view(None).status, QuotaStatus::Stale);
    state.reading.as_mut().unwrap().rolling.resets_at_ms = 1100;
    assert_eq!(state.view(Some(1099)).status, QuotaStatus::Current);
    assert_eq!(state.view(Some(1100)).status, QuotaStatus::Stale);
    assert_eq!(state.view(Some(1100)).checked_at_ms, Some(1000));
}
#[tokio::test]
async fn retired_guard_cannot_clear_a_newer_connection_flight() {
    let (service, _) = setup(Arc::new(Memory::default()));
    save(&service).await;
    let old = Ticket {
        generation: 1,
        sequence: 1,
    };
    let new = Ticket {
        generation: 2,
        sequence: 2,
    };
    service.state.write().unwrap().quota.flight = Some(new);
    drop(FlightGuard {
        service: service.clone(),
        ticket: old,
    });
    assert_eq!(service.state.read().unwrap().quota.flight, Some(new));
    service.state.write().unwrap().quota.flight = None;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_retired_completion_cannot_clear_new_scope_pending_request() {
    let memory = Arc::new(Memory::default());
    let (seed, _) = setup(memory.clone());
    save(&seed).await;
    let id = seed.view().connection_id;
    let (tx, rx) = std::sync::mpsc::channel();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    *memory.block_load.lock().unwrap() = Some(rx);
    *memory.load_entered.lock().unwrap() = Some(entered_tx);
    let (service, fake) = setup(memory);
    fake.block.store(true, Ordering::SeqCst);
    let owner = service.clone();
    let expected = id.clone();
    let old = tokio::spawn(async move { owner.check_quota(expected).await });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    service.set_foreground(false);
    service.set_foreground(true);
    let owner = service.clone();
    let new = tokio::spawn(async move { owner.check_quota(id).await });
    // Admission happens before the mutation wait. Observe it without a time delay.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if service
                .state
                .read()
                .unwrap()
                .quota
                .flight
                .is_some_and(|ticket| ticket.sequence == 2)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tx.send(()).unwrap();
    assert_eq!(old.await.unwrap(), Err(QuotaFault::Cancelled));
    fake.entered.acquire().await.unwrap().forget();
    assert!(service.view().quota.checking);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    fake.release.add_permits(1);
    new.await.unwrap().unwrap();
    assert_eq!(service.view().quota.status, QuotaStatus::Current);
}
#[tokio::test]
async fn close_cancels_observation_and_never_reopens_or_sends_after_closed() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    fake.block.store(true, Ordering::SeqCst);
    let owner = service.clone();
    let pending = tokio::spawn(async move { refresh(&owner).await });
    fake.entered.acquire().await.unwrap().forget();
    service.close();
    service.set_foreground(true);
    assert_eq!(pending.await.unwrap(), Err(QuotaFault::Cancelled));
    service.drained().await;
    assert!(service.closed_and_drained());
    assert!(!service.view().quota.checking);
    assert_eq!(refresh(&service).await, Err(QuotaFault::Cancelled));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn wrong_or_missing_connection_id_cannot_query_or_replace_cached_observation() {
    let (service, fake) = setup(Arc::new(Memory::default()));
    save(&service).await;
    refresh(&service).await.unwrap();
    let before = service.view().quota;
    assert_eq!(
        service.check_quota(None).await,
        Err(QuotaFault::NotConfigured)
    );
    assert_eq!(
        service.check_quota(Some("a".repeat(32))).await,
        Err(QuotaFault::Changed)
    );
    assert_eq!(service.view().quota, before);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
