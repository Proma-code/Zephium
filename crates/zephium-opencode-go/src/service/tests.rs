use super::*;
use crate::vault::VaultError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Semaphore;

const PROFILE: &str = "00000000000000000000000001";
struct MemoryVault {
    records: StdMutex<HashMap<String, Zeroizing<Vec<u8>>>>,
    fail_save: AtomicBool,
    fail_forget: AtomicBool,
    save_entered: Semaphore,
    load_entered: Semaphore,
    forget_calls: AtomicUsize,
    block_load: StdMutex<Option<std::sync::mpsc::Receiver<()>>>,
    block_save: StdMutex<Option<std::sync::mpsc::Receiver<()>>>,
}
impl Default for MemoryVault {
    fn default() -> Self {
        Self {
            records: StdMutex::default(),
            fail_save: AtomicBool::new(false),
            fail_forget: AtomicBool::new(false),
            save_entered: Semaphore::new(0),
            load_entered: Semaphore::new(0),
            forget_calls: AtomicUsize::new(0),
            block_load: StdMutex::new(None),
            block_save: StdMutex::new(None),
        }
    }
}
impl Vault for MemoryVault {
    fn load(&self, profile: &str) -> Result<Option<Arc<Config>>, VaultError> {
        self.load_entered.add_permits(1);
        if let Some(block) = self.block_load.lock().unwrap().take() {
            block.recv().unwrap();
        }
        self.records
            .lock()
            .unwrap()
            .get(profile)
            .map(|b| crate::vault::decode(profile, b))
            .transpose()
    }
    fn save(&self, profile: &str, config: &Config) -> Result<(), VaultError> {
        self.save_entered.add_permits(1);
        if let Some(block) = self.block_save.lock().unwrap().take() {
            block.recv().unwrap();
        }
        if self.fail_save.load(Ordering::SeqCst) {
            return Err(VaultError::Unavailable);
        }
        self.records
            .lock()
            .unwrap()
            .insert(profile.into(), crate::vault::encode(profile, config)?);
        Ok(())
    }
    fn forget(&self, profile: &str) -> Result<(), VaultError> {
        self.forget_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_forget.load(Ordering::SeqCst) {
            return Err(VaultError::Unavailable);
        }
        self.records.lock().unwrap().remove(profile);
        Ok(())
    }
}
struct Fake {
    model_calls: AtomicUsize,
    calls: AtomicUsize,
    block_models: AtomicBool,
    block_call: AtomicBool,
    fail_models: AtomicBool,
    omit_usage: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            model_calls: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            block_models: AtomicBool::new(false),
            block_call: AtomicBool::new(false),
            fail_models: AtomicBool::new(false),
            omit_usage: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}
impl Transport for Fake {
    fn models<'a>(&'a self, _: &'a Config) -> PortFuture<'a, Result<Vec<GoModel>, GoFailure>> {
        Box::pin(async move {
            self.model_calls.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            if self.block_models.load(Ordering::SeqCst) {
                self.release.acquire().await.unwrap().forget();
            }
            if self.fail_models.load(Ordering::SeqCst) {
                return Err(GoFailure {
                    fault: GoFault::Unauthorized,
                    dispatched: false,
                    output_started: false,
                });
            }
            Ok(vec![GoModel {
                id: "fixture/model".into(),
                display_name: "Fixture".into(),
                wire: WorkModelWire::ChatCompletions,
            }])
        })
    }
    fn call<'a>(
        &'a self,
        _: &'a Config,
        _: SessionId,
        _: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> PortFuture<'a, Result<GoOutcome, GoFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            if self.block_call.load(Ordering::SeqCst) {
                self.release.acquire().await.unwrap().forget();
            }
            events(WorkModelEvent::Text("fixture response".into()));
            if self.omit_usage.load(Ordering::SeqCst) {
                return Err(GoFailure {
                    fault: GoFault::UsageUnavailable,
                    dispatched: true,
                    output_started: true,
                });
            }
            Ok(GoOutcome {
                outcome: WorkModelOutcome {
                    stop: WorkModelStop::ToolUse,
                    assistant: vec![WorkModelPart::ToolCall(WorkModelToolCall {
                        id: "call_fixture".into(),
                        name: "read_page".into(),
                        arguments: serde_json::json!({}),
                    })],
                    usage: WorkModelUsage {
                        input_tokens: 10,
                        output_tokens: 4,
                        ..WorkModelUsage::default()
                    },
                },
                billing: zephium_agentic::lead::opencode_go::GoBilling {
                    source: BillingSource::Go,
                    tokens: None,
                    cost_usd: None,
                    list_cost_usd: None,
                    allowance_consumed_usd: None,
                    estimated: false,
                },
            })
        })
    }
}
fn setup(profile: &str, vault: Arc<MemoryVault>) -> (Arc<Service>, Arc<Fake>) {
    let mut service = Service::new(profile.into(), vault, Arc::new(|| {})).unwrap();
    let fake = Arc::new(Fake::default());
    service.transport = fake.clone();
    service.set_foreground(true);
    (Arc::new(service), fake)
}
async fn save(service: &Arc<Service>) {
    service
        .configure(
            service.view().connection_id,
            Zeroizing::new("go-fixture".into()),
            true,
        )
        .await
        .unwrap();
}
fn session() -> SessionId {
    SessionId::parse("b".repeat(64)).unwrap()
}
fn request() -> WorkModelRequest {
    WorkModelRequest {
        image_fence: None,
        model: WorkModelRef {
            provider: WorkModelProvider::OpenCodeGo,
            wire: WorkModelWire::ChatCompletions,
            model: "fixture/model".into(),
        },
        system: vec![],
        tools: vec![],
        messages: vec![WorkModelMessage::User(vec![WorkModelPart::Text(
            "hello".into(),
        )])],
        max_output_tokens: 100,
        reasoning: None,
        native_search: false,
        parallel_tools: false,
        tool_choice: WorkModelToolChoice::Auto,
        volatile_tail: 0,
        cache_key: None,
    }
}
#[tokio::test]
async fn configure_and_load_never_discover_or_infer_and_catalog_is_source_scoped() {
    let vault = Arc::new(MemoryVault::default());
    let (service, fake) = setup(PROFILE, vault.clone());
    service.load().await.unwrap();
    assert!(!service.view().configured);
    save(&service).await;
    assert!(service.view().configured);
    assert!(service.entries().is_empty());
    assert_eq!(fake.model_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    assert!(service.view().checked_at_ms.is_none());
    service
        .check_models(service.view().connection_id)
        .await
        .unwrap();
    let entries = service.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].display_name, "Fixture");
    assert!(entries[0].id.starts_with("opencode-go/"));
    assert_eq!(entries[0].model.model, "fixture/model");
    assert!(entries[0].price.is_none());
    assert!(!entries[0].supports.reasoning);
    assert!(service.view().checked_at_ms.is_some());
    let (other, _) = setup("00000000000000000000000002", vault);
    other.load().await.unwrap();
    assert!(!other.view().configured);
}
#[tokio::test]
async fn every_save_rotates_source_identity_and_retires_old_lease_and_catalog() {
    let (s, _) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    s.check_models(s.view().connection_id).await.unwrap();
    let old = s.entries()[0].id.clone();
    let lease = s.lease(&old).unwrap();
    save(&s).await;
    assert!(s.entries().is_empty());
    assert!(s.lease(&old).is_none());
    assert_eq!(
        s.call(lease, session(), request(), &|_| {}).await.err(),
        Some(retired(false))
    );
    s.check_models(s.view().connection_id).await.unwrap();
    assert_ne!(s.entries()[0].id, old);
}
#[tokio::test]
async fn invalid_input_does_not_modify_existing_connection() {
    let (s, _) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    let id = s.view().connection_id;
    for (key, ack) in [("", true), ("a b", true), ("key", false)] {
        assert_eq!(
            s.configure(id.clone(), Zeroizing::new(key.into()), ack)
                .await,
            Err(Fault::Invalid)
        );
        assert_eq!(s.view().connection_id, id);
    }
}
#[tokio::test]
async fn catalog_failure_is_explicit_and_never_treated_as_connected_or_zero_quota() {
    let (s, f) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    s.check_models(s.view().connection_id).await.unwrap();
    f.fail_models.store(true, Ordering::SeqCst);
    assert_eq!(
        s.check_models(s.view().connection_id).await,
        Err(Fault::Unauthorized)
    );
    assert!(s.entries().is_empty());
    assert_eq!(s.view().fault, Some(Fault::Unauthorized));
    assert!(s.view().configured);
}
#[tokio::test]
async fn profile_bounce_retires_existing_clients() {
    let (s, _) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    s.check_models(s.view().connection_id).await.unwrap();
    let old = s.lease(&s.entries()[0].id).unwrap();
    s.set_foreground(false);
    assert!(s.entries().is_empty());
    s.set_foreground(true);
    assert_eq!(
        s.call(old, session(), request(), &|_| {}).await.err(),
        Some(retired(false))
    );
}
#[tokio::test]
async fn scope_change_cancels_catalog_and_rejects_late_observation() {
    let (s, f) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    f.block_models.store(true, Ordering::SeqCst);
    let check = tokio::spawn({
        let s = s.clone();
        async move { s.check_models(s.view().connection_id).await }
    });
    f.entered.acquire().await.unwrap().forget();
    s.set_foreground(false);
    s.set_foreground(true);
    f.release.add_permits(1);
    assert_eq!(check.await.unwrap(), Err(Fault::Cancelled));
    assert!(s.entries().is_empty());
    assert!(!s.view().checking);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_save_waiter_does_not_abandon_durable_write() {
    let vault = Arc::new(MemoryVault::default());
    let (send, receive) = std::sync::mpsc::channel();
    *vault.block_save.lock().unwrap() = Some(receive);
    let (s, _) = setup(PROFILE, vault.clone());
    let waiter = tokio::spawn({
        let s = s.clone();
        async move { save(&s).await }
    });
    vault.save_entered.acquire().await.unwrap().forget();
    waiter.abort();
    send.send(()).unwrap();
    s.drained().await;
    assert!(s.view().configured);
    assert!(vault.load(PROFILE).unwrap().is_some());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_check_cannot_start_after_profile_away_and_back() {
    let vault = Arc::new(MemoryVault::default());
    let (send, receive) = std::sync::mpsc::channel();
    *vault.block_save.lock().unwrap() = Some(receive);
    let (s, f) = setup(PROFILE, vault.clone());
    let save = tokio::spawn({
        let s = s.clone();
        async move { save(&s).await }
    });
    vault.save_entered.acquire().await.unwrap().forget();
    let check = tokio::spawn({
        let s = s.clone();
        async move { s.check_models(s.view().connection_id).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while s.tasks.state.lock().unwrap().active < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    s.set_foreground(false);
    s.set_foreground(true);
    send.send(()).unwrap();
    save.await.unwrap();
    assert_eq!(check.await.unwrap(), Err(Fault::Cancelled));
    assert_eq!(f.model_calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn failed_save_keeps_old_durable_configuration_and_reports_failure() {
    let vault = Arc::new(MemoryVault::default());
    let (s, _) = setup(PROFILE, vault.clone());
    save(&s).await;
    let old = s.view().connection_id;
    vault.fail_save.store(true, Ordering::SeqCst);
    assert_eq!(
        s.configure(
            s.view().connection_id,
            Zeroizing::new("new-fixture".into()),
            true
        )
        .await,
        Err(Fault::Vault)
    );
    assert_eq!(s.view().connection_id, old);
    assert_eq!(s.view().fault, Some(Fault::Vault));
    assert_eq!(vault.load(PROFILE).unwrap().unwrap().id, old.unwrap());
}
#[tokio::test]
async fn failed_delete_is_retryable_and_does_not_erase_another_profile() {
    let vault = Arc::new(MemoryVault::default());
    let (s, _) = setup(PROFILE, vault.clone());
    let (other, _) = setup("00000000000000000000000002", vault.clone());
    save(&s).await;
    save(&other).await;
    s.check_models(s.view().connection_id).await.unwrap();
    let old = s.lease(&s.entries()[0].id).unwrap();
    vault.fail_forget.store(true, Ordering::SeqCst);
    assert_eq!(
        s.disconnect(s.view().connection_id).await,
        Err(Fault::Vault)
    );
    assert!(s.view().configured);
    assert!(s.entries().is_empty());
    assert_eq!(
        s.call(old, session(), request(), &|_| {}).await.err(),
        Some(retired(false))
    );
    vault.fail_forget.store(false, Ordering::SeqCst);
    s.disconnect(s.view().connection_id).await.unwrap();
    assert!(!s.view().configured);
    assert!(vault.load("00000000000000000000000002").unwrap().is_some());
}
#[tokio::test]
async fn close_is_permanent_and_erase_participates_in_retryable_profile_cleanup() {
    let vault = Arc::new(MemoryVault::default());
    let (s, _) = setup(PROFILE, vault.clone());
    save(&s).await;
    s.close();
    s.set_foreground(true);
    assert_eq!(
        s.check_models(s.view().connection_id).await,
        Err(Fault::Cancelled)
    );
    vault.fail_forget.store(true, Ordering::SeqCst);
    assert_eq!(s.erase().await, Err(Fault::Vault));
    vault.fail_forget.store(false, Ordering::SeqCst);
    s.erase().await.unwrap();
    s.drained().await;
    assert!(s.closed_and_drained());
    assert!(vault.load(PROFILE).unwrap().is_none());
}
#[tokio::test]
async fn missing_usage_never_commits_tools_or_claims_free_usage() {
    let (s, f) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    s.check_models(s.view().connection_id).await.unwrap();
    let lease = s.lease(&s.entries()[0].id).unwrap();
    f.omit_usage.store(true, Ordering::SeqCst);
    let tools = AtomicUsize::new(0);
    let error = s
        .call(lease, session(), request(), &|event| {
            if matches!(event, WorkModelEvent::ToolCall(_)) {
                tools.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await
        .err()
        .unwrap();
    assert_eq!(
        error,
        WorkModelError::OpenCodeGo {
            fault: OpenCodeGoModelFault::UsageUnavailable,
            usage_unknown: true
        }
    );
    assert_eq!(tools.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn cancellation_after_dispatch_fences_late_output_and_preserves_uncertain_usage() {
    let (s, f) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    s.check_models(s.view().connection_id).await.unwrap();
    f.entered.acquire().await.unwrap().forget();
    let lease = s.lease(&s.entries()[0].id).unwrap();
    f.block_call.store(true, Ordering::SeqCst);
    let output = Arc::new(AtomicUsize::new(0));
    let call = tokio::spawn({
        let s = s.clone();
        let output = output.clone();
        async move {
            s.call(lease, session(), request(), &|_| {
                output.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .err()
        }
    });
    f.entered.acquire().await.unwrap().forget();
    s.set_foreground(false);
    f.release.add_permits(1);
    assert_eq!(call.await.unwrap(), Some(retired(true)));
    assert_eq!(output.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stale_connection_preconditions_cannot_delete_replace_or_check_a_new_key() {
    let (s, f) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    let old = s.view().connection_id;
    save(&s).await;
    s.check_models(s.view().connection_id).await.unwrap();
    let current = s.view().connection_id;
    let current_entries = s.entries();
    let count = f.model_calls.load(Ordering::SeqCst);
    assert_eq!(s.disconnect(old.clone()).await, Err(Fault::Changed));
    assert_eq!(
        s.configure(
            old.clone(),
            Zeroizing::new("old-window-fixture".into()),
            true
        )
        .await,
        Err(Fault::Changed)
    );
    assert_eq!(s.check_models(old).await, Err(Fault::Changed));
    assert_eq!(s.disconnect(None).await, Err(Fault::Changed));
    assert_eq!(s.view().connection_id, current);
    assert_eq!(s.entries(), current_entries);
    assert_eq!(f.model_calls.load(Ordering::SeqCst), count);
}

#[tokio::test]
async fn null_precondition_cannot_overwrite_a_saved_connection_not_yet_loaded() {
    let vault = Arc::new(MemoryVault::default());
    let (original, _) = setup(PROFILE, vault.clone());
    save(&original).await;
    let id = original.view().connection_id;
    let (s, f) = setup(PROFILE, vault.clone());
    assert_eq!(
        s.configure(None, Zeroizing::new("fixture".into()), true)
            .await,
        Err(Fault::Changed)
    );
    assert_eq!(s.view().connection_id, id);
    assert_eq!(vault.load(PROFILE).unwrap().unwrap().id, id.unwrap());
    assert_eq!(f.model_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn erase_waits_for_admitted_save_and_never_leaves_a_resurrected_record() {
    let vault = Arc::new(MemoryVault::default());
    let (send, receive) = std::sync::mpsc::channel();
    *vault.block_save.lock().unwrap() = Some(receive);
    let (s, _) = setup(PROFILE, vault.clone());
    let save = tokio::spawn({
        let s = s.clone();
        async move { save(&s).await }
    });
    vault.save_entered.acquire().await.unwrap().forget();
    let erase = tokio::spawn({
        let s = s.clone();
        async move { s.erase().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !s.tasks.state.lock().unwrap().closed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!s.closed_and_drained());
    s.set_foreground(true);
    assert!(s.entries().is_empty());
    send.send(()).unwrap();
    save.await.unwrap();
    erase.await.unwrap().unwrap();
    s.drained().await;
    assert!(s.closed_and_drained());
    assert!(!s.view().configured);
    assert!(vault.load(PROFILE).unwrap().is_none());
}

#[tokio::test]
async fn captured_native_scope_cannot_admit_any_command_after_profile_bounce() {
    let vault = Arc::new(MemoryVault::default());
    let (s, f) = setup(PROFILE, vault.clone());
    save(&s).await;
    let id = s.view().connection_id;
    let scope = s.scope().unwrap();
    s.set_foreground(false);
    s.set_foreground(true);
    assert_eq!(s.load_scoped(scope).await, Err(Fault::Cancelled));
    assert_eq!(
        s.configure_scoped(scope, id.clone(), Zeroizing::new("fixture".into()), true)
            .await,
        Err(Fault::Cancelled)
    );
    assert_eq!(
        s.check_models_scoped(scope, id.clone()).await,
        Err(Fault::Cancelled)
    );
    assert_eq!(
        s.disconnect_scoped(scope, id.clone()).await,
        Err(Fault::Cancelled)
    );
    assert_eq!(s.view().connection_id, id);
    assert_eq!(f.model_calls.load(Ordering::SeqCst), 0);
    assert_eq!(vault.forget_calls.load(Ordering::SeqCst), 0);
    let (other, _) = setup("00000000000000000000000002", vault);
    assert_eq!(
        other.load_scoped(s.scope().unwrap()).await,
        Err(Fault::Cancelled)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_load_disconnect_cannot_delete_after_profile_scope_retires() {
    let vault = Arc::new(MemoryVault::default());
    let (original, _) = setup(PROFILE, vault.clone());
    save(&original).await;
    let id = original.view().connection_id;
    while let Ok(permit) = vault.load_entered.try_acquire() {
        permit.forget();
    }
    let (send, receive) = std::sync::mpsc::channel();
    *vault.block_load.lock().unwrap() = Some(receive);
    let (s, _) = setup(PROFILE, vault.clone());
    let scope = s.scope().unwrap();
    let disconnect = tokio::spawn({
        let s = s.clone();
        let id = id.clone();
        async move { s.disconnect_scoped(scope, id).await }
    });
    vault.load_entered.acquire().await.unwrap().forget();
    s.set_foreground(false);
    s.set_foreground(true);
    send.send(()).unwrap();
    assert_eq!(disconnect.await.unwrap(), Err(Fault::Cancelled));
    assert_eq!(vault.forget_calls.load(Ordering::SeqCst), 0);
    assert_eq!(s.view().connection_id, id);
    assert!(vault.load(PROFILE).unwrap().is_some());
}

#[tokio::test]
async fn reported_billing_is_last_request_metadata_not_a_plan_or_quota() {
    let (s, _) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&s).await;
    s.check_models(s.view().connection_id).await.unwrap();
    assert!(s.view().last_billing.is_none());
    let lease = s.lease(&s.entries()[0].id).unwrap();
    s.call(lease, session(), request(), &|_| {}).await.unwrap();
    let reported = s.view().last_billing.unwrap();
    assert_eq!(reported.source, BillingSource::Go);
    assert!(reported.observed_at_ms > 0);
    save(&s).await;
    assert!(s.view().last_billing.is_none());
}

#[tokio::test]
async fn retirement_during_billing_notification_cannot_return_executable_outcome() {
    let retire = Arc::new(AtomicBool::new(false));
    let fake = Arc::new(Fake::default());
    let service = Arc::new_cyclic(|weak: &std::sync::Weak<Service>| {
        let weak = weak.clone();
        let retire = retire.clone();
        let mut service = Service::new(
            PROFILE.into(),
            Arc::new(MemoryVault::default()),
            Arc::new(move || {
                if retire.load(Ordering::SeqCst) {
                    weak.upgrade().unwrap().set_foreground(false);
                }
            }),
        )
        .unwrap();
        service.transport = fake.clone();
        service.set_foreground(true);
        service
    });
    save(&service).await;
    service
        .check_models(service.view().connection_id)
        .await
        .unwrap();
    let lease = service.lease(&service.entries()[0].id).unwrap();
    retire.store(true, Ordering::SeqCst);
    let tools = AtomicUsize::new(0);
    let result = service
        .call(lease, session(), request(), &|event| {
            if matches!(event, WorkModelEvent::ToolCall(_)) {
                tools.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await;
    assert_eq!(result.err(), Some(retired(true)));
    assert_eq!(tools.load(Ordering::SeqCst), 0);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    // A canceled publication is not a rollback of confirmed upstream billing.
    assert_eq!(
        service.view().last_billing.unwrap().source,
        BillingSource::Go
    );
}

#[tokio::test]
async fn retirement_during_tool_notification_cannot_return_executable_outcome() {
    let (service, fake) = setup(PROFILE, Arc::new(MemoryVault::default()));
    save(&service).await;
    service
        .check_models(service.view().connection_id)
        .await
        .unwrap();
    let lease = service.lease(&service.entries()[0].id).unwrap();
    let tools = AtomicUsize::new(0);
    let result = service
        .call(lease, session(), request(), &|event| {
            if matches!(event, WorkModelEvent::ToolCall(_)) {
                tools.fetch_add(1, Ordering::SeqCst);
                service.set_foreground(false);
            }
        })
        .await;
    assert_eq!(result.err(), Some(retired(true)));
    assert_eq!(tools.load(Ordering::SeqCst), 1);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
