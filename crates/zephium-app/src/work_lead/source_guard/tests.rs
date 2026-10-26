use super::super::LeadModel;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use tokio::sync::{Notify, Semaphore};

const A: &str = "gateway/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const GO: &str = "opencode-go/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "gateway/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
fn profile() -> ProfileId {
    ProfileId::parse("00000000000000000000000001").unwrap()
}
fn outcome() -> WorkModelOutcome {
    WorkModelOutcome {
        stop: WorkModelStop::EndTurn,
        usage: WorkModelUsage::default(),
        assistant: vec![WorkModelPart::Text("fixture".into())],
    }
}
fn request(model: &LeadModel) -> WorkModelRequest {
    WorkModelRequest {
        image_fence: None,
        model: model.entry.model.clone(),
        system: vec![],
        tools: vec![],
        messages: vec![WorkModelMessage::User(vec![WorkModelPart::Text(
            "fixture".into(),
        )])],
        max_output_tokens: 16,
        reasoning: None,
        native_search: false,
        parallel_tools: false,
        tool_choice: WorkModelToolChoice::Auto,
        volatile_tail: 0,
        cache_key: None,
    }
}
fn entry(prefix: &str, model: &str) -> WorkModelEntry {
    let gateway = prefix.starts_with("gateway/");
    let go = prefix.starts_with("opencode-go/");
    WorkModelEntry {
        id: format!("{prefix}/{model}"),
        model: WorkModelRef {
            provider: if go {
                WorkModelProvider::OpenCodeGo
            } else if gateway {
                WorkModelProvider::Compatible
            } else {
                WorkModelProvider::OpenAi
            },
            wire: if gateway || go {
                WorkModelWire::ChatCompletions
            } else {
                WorkModelWire::OpenAiResponses
            },
            model: model.into(),
        },
        display_name: model.into(),
        roles: vec![],
        recommended: false,
        context_window: 32768,
        max_output: 8192,
        supports: WorkModelSupports {
            tools: true,
            vision: false,
            prompt_cache: false,
            reasoning: false,
            native_search: false,
        },
        price: None,
    }
}
fn group(prefix: &str, clients: [Arc<dyn WorkModelClient>; 3]) -> WorkLeadModels {
    let [lead, page, light] = clients;
    WorkLeadModels {
        lead: LeadModel {
            entry: entry(prefix, "lead"),
            client: lead,
        },
        page: LeadModel {
            entry: entry(prefix, "page"),
            client: page,
        },
        light: LeadModel {
            entry: entry(prefix, "light"),
            client: light,
        },
    }
}
struct Ready {
    calls: Arc<AtomicUsize>,
    error: Option<WorkModelError>,
    quiet: bool,
}
impl WorkModelClient for Ready {
    fn call<'a>(
        &'a self,
        _: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> WorkModelFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if !self.quiet {
                events(WorkModelEvent::Text("fixture".into()));
            }
            match self.error {
                Some(error) => Err(error),
                None => Ok(outcome()),
            }
        })
    }
}
fn ready(calls: &Arc<AtomicUsize>, error: Option<WorkModelError>) -> Arc<dyn WorkModelClient> {
    Arc::new(Ready {
        calls: calls.clone(),
        error,
        quiet: false,
    })
}
fn quota(prefix: &str) -> WorkModelError {
    if prefix == "chatgpt" {
        WorkModelError::ChatGpt {
            fault: ChatGptModelFault::UsageLimit,
        }
    } else if prefix.starts_with("opencode-go/") {
        WorkModelError::OpenCodeGo {
            fault: OpenCodeGoModelFault::RateLimited,
            usage_unknown: true,
        }
    } else {
        WorkModelError::Gateway {
            fault: GatewayModelFault::RateLimited,
            usage_unknown: true,
        }
    }
}
#[tokio::test]
async fn failed_page_blocks_light_and_lead_across_distinct_model_clients() {
    for prefix in ["chatgpt", A, GO] {
        let calls = Arc::new(AtomicUsize::new(0));
        let error = quota(prefix);
        let models = guard_run_sources(
            profile(),
            group(
                prefix,
                [
                    ready(&calls, None),
                    ready(&calls, Some(error)),
                    ready(&calls, None),
                ],
            ),
        );
        let events = AtomicUsize::new(0);
        let sink = |_| {
            events.fetch_add(1, Ordering::SeqCst);
        };
        assert_eq!(
            models
                .page
                .client
                .call(request(&models.page), &sink)
                .await
                .err(),
            Some(error)
        );
        assert_eq!(
            models
                .light
                .client
                .call(request(&models.light), &sink)
                .await
                .err(),
            Some(error)
        );
        assert_eq!(
            models
                .lead
                .client
                .call(request(&models.lead), &sink)
                .await
                .err(),
            Some(error)
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "role/model reassignment must not redispatch"
        );
        assert_eq!(
            events.load(Ordering::SeqCst),
            1,
            "no sibling output after failure"
        );
    }
}
#[tokio::test]
async fn different_explicit_connections_and_sources_remain_independent() {
    for (first, second) in [(A, B), (GO, "opencode-go/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut raw = group(
            first,
            [
                ready(&calls, None),
                ready(&calls, Some(quota(first))),
                ready(&calls, None),
            ],
        );
        raw.light.entry = entry(second, "light");
        raw.lead.entry = entry("chatgpt", "lead");
        let models = guard_run_sources(profile(), raw);
        assert!(models
            .page
            .client
            .call(request(&models.page), &|_| {})
            .await
            .is_err());
        assert!(models
            .light
            .client
            .call(request(&models.light), &|_| {})
            .await
            .is_ok());
        assert!(models
            .lead
            .client
            .call(request(&models.lead), &|_| {})
            .await
            .is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
#[tokio::test]
async fn a_fresh_explicit_run_and_another_profile_get_fresh_admission() {
    for prefix in ["chatgpt", A, GO] {
        let calls = Arc::new(AtomicUsize::new(0));
        for owner in [
            profile(),
            profile(),
            ProfileId::parse("00000000000000000000000002").unwrap(),
        ] {
            let models = guard_run_sources(
                owner,
                group(
                    prefix,
                    [
                        ready(&calls, None),
                        ready(&calls, Some(quota(prefix))),
                        ready(&calls, None),
                    ],
                ),
            );
            assert!(models
                .page
                .client
                .call(request(&models.page), &|_| {})
                .await
                .is_err());
            assert!(models
                .light
                .client
                .call(request(&models.light), &|_| {})
                .await
                .is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
#[tokio::test]
async fn generic_api_clients_are_unwrapped_and_keep_existing_retry_semantics() {
    let calls = Arc::new(AtomicUsize::new(0));
    let client = ready(&calls, Some(WorkModelError::Network));
    let raw = group("openai", [client.clone(), client.clone(), client.clone()]);
    let models = guard_run_sources(profile(), raw);
    assert!(Arc::ptr_eq(&models.page.client, &client));
    for model in [&models.page, &models.light] {
        assert_eq!(
            model.client.call(request(model), &|_| {}).await.err(),
            Some(WorkModelError::Network)
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn malformed_protected_entry_cannot_bypass_the_source_guard() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut raw = group(
        A,
        [
            ready(&calls, None),
            ready(&calls, None),
            ready(&calls, None),
        ],
    );
    raw.page.entry.model.provider = WorkModelProvider::OpenAi;
    let models = guard_run_sources(profile(), raw);
    let expected = WorkModelError::Gateway {
        fault: GatewayModelFault::Unsupported,
        usage_unknown: false,
    };
    assert_eq!(
        models
            .page
            .client
            .call(request(&models.page), &|_| {})
            .await
            .err(),
        Some(expected)
    );
    assert_eq!(
        models
            .light
            .client
            .call(request(&models.light), &|_| {})
            .await
            .err(),
        Some(expected)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
struct Blocked {
    calls: Arc<AtomicUsize>,
    entered: Notify,
    release: Semaphore,
    abandoned: Arc<AtomicUsize>,
    error: Option<WorkModelError>,
}
struct PendingCall {
    abandoned: Arc<AtomicUsize>,
    finished: bool,
}
impl Drop for PendingCall {
    fn drop(&mut self) {
        if !self.finished {
            self.abandoned.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl WorkModelClient for Blocked {
    fn call<'a>(
        &'a self,
        _: WorkModelRequest,
        events: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> WorkModelFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut pending = PendingCall {
                abandoned: self.abandoned.clone(),
                finished: false,
            };
            self.entered.notify_one();
            let permit = self.release.acquire().await.unwrap();
            permit.forget();
            events(WorkModelEvent::Text("fixture".into()));
            pending.finished = true;
            match self.error {
                Some(error) => Err(error),
                None => Ok(outcome()),
            }
        })
    }
}
fn blocked(error: Option<WorkModelError>) -> Arc<Blocked> {
    Arc::new(Blocked {
        calls: Arc::new(AtomicUsize::new(0)),
        entered: Notify::new(),
        release: Semaphore::new(0),
        abandoned: Arc::new(AtomicUsize::new(0)),
        error,
    })
}
fn start(model: LeadModel) -> tokio::task::JoinHandle<Result<WorkModelOutcome, WorkModelError>> {
    tokio::spawn(async move { model.client.call(request(&model), &|_| {}).await })
}
async fn observe_queued(future: &mut WorkModelFuture<'_>) {
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "same-source sibling must wait before dispatch"
        );
        Poll::Ready(())
    })
    .await;
}
#[tokio::test]
async fn dropping_a_queued_waiter_does_not_poison_a_successful_owner() {
    let owner = blocked(None);
    let calls = owner.calls.clone();
    let models = guard_run_sources(
        profile(),
        group(A, [ready(&calls, None), owner.clone(), ready(&calls, None)]),
    );
    let task = start(models.page.clone());
    tokio::time::timeout(std::time::Duration::from_secs(5), owner.entered.notified())
        .await
        .unwrap();
    let mut waiter = models.light.client.call(request(&models.light), &|_| {});
    observe_queued(&mut waiter).await;
    drop(waiter);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    owner.release.add_permits(1);
    assert!(task.await.unwrap().is_ok());
    assert!(models
        .light
        .client
        .call(request(&models.light), &|_| {})
        .await
        .is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(owner.abandoned.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn dropping_a_dispatched_owner_blocks_every_sibling_with_unknown_usage() {
    for prefix in ["chatgpt", A, GO] {
        let owner = blocked(None);
        let calls = owner.calls.clone();
        let models = guard_run_sources(
            profile(),
            group(
                prefix,
                [ready(&calls, None), owner.clone(), ready(&calls, None)],
            ),
        );
        let task = start(models.page.clone());
        tokio::time::timeout(std::time::Duration::from_secs(5), owner.entered.notified())
            .await
            .unwrap();
        let mut waiter = models.light.client.call(request(&models.light), &|_| {});
        observe_queued(&mut waiter).await;
        task.abort();
        let Err(cancelled) = task.await else {
            panic!("dispatched fixture was not cancelled")
        };
        assert!(cancelled.is_cancelled());
        let expected = if prefix == "chatgpt" {
            Kind::ChatGpt.uncertain()
        } else if prefix == GO {
            Kind::Go.uncertain()
        } else {
            Kind::Gateway.uncertain()
        };
        assert_eq!(waiter.await.err(), Some(expected));
        assert_eq!(
            models
                .lead
                .client
                .call(request(&models.lead), &|_| {})
                .await
                .err(),
            Some(expected)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(owner.abandoned.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn a_queued_role_cannot_overwrite_an_owner_failure_with_success() {
    let error = quota(A);
    let owner = blocked(Some(error));
    let calls = owner.calls.clone();
    let models = guard_run_sources(
        profile(),
        group(A, [ready(&calls, None), owner.clone(), ready(&calls, None)]),
    );
    let task = start(models.page.clone());
    tokio::time::timeout(std::time::Duration::from_secs(5), owner.entered.notified())
        .await
        .unwrap();
    let mut waiter = models.light.client.call(request(&models.light), &|_| {});
    observe_queued(&mut waiter).await;
    owner.release.add_permits(1);
    assert_eq!(task.await.unwrap().err(), Some(error));
    assert_eq!(waiter.await.err(), Some(error));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn a_different_connection_does_not_wait_on_the_active_source_lock() {
    let owner = blocked(None);
    let calls = owner.calls.clone();
    let mut raw = group(A, [ready(&calls, None), owner.clone(), ready(&calls, None)]);
    raw.light.entry = entry(B, "light");
    let models = guard_run_sources(profile(), raw);
    let task = start(models.page.clone());
    tokio::time::timeout(std::time::Duration::from_secs(5), owner.entered.notified())
        .await
        .unwrap();
    let mut other = models.light.client.call(request(&models.light), &|_| {});
    std::future::poll_fn(|cx| {
        assert!(
            matches!(other.as_mut().poll(cx), Poll::Ready(Ok(_))),
            "unrelated source must not queue behind the active source"
        );
        Poll::Ready(())
    })
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
    let _ = task.await;
}
#[tokio::test]
async fn unexpected_retriable_errors_are_closed_source_faults() {
    for prefix in ["chatgpt", A, GO] {
        let calls = Arc::new(AtomicUsize::new(0));
        let models = guard_run_sources(
            profile(),
            group(
                prefix,
                [
                    ready(&calls, None),
                    ready(&calls, Some(WorkModelError::Network)),
                    ready(&calls, None),
                ],
            ),
        );
        let expected = if prefix == "chatgpt" {
            Kind::ChatGpt.uncertain()
        } else if prefix == GO {
            Kind::Go.uncertain()
        } else {
            Kind::Gateway.uncertain()
        };
        assert_eq!(
            models
                .page
                .client
                .call(request(&models.page), &|_| {})
                .await
                .err(),
            Some(expected)
        );
        assert_eq!(
            models
                .light
                .client
                .call(request(&models.light), &|_| {})
                .await
                .err(),
            Some(expected)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn desktop_wraps_once_before_adapter_clients_are_cloned() {
    let native = std::fs::read_to_string("../../desktop/src/work_lead_run.rs").unwrap();
    let guard = native
        .find("let models = zephium_app::work_lead::guard_run_sources(profile, models);")
        .unwrap();
    assert_eq!(
        native.matches("guard_run_sources(profile, models)").count(),
        1
    );
    for clone in [
        "let search = search_backend(profile, &models)",
        "let page = page_model(&models)",
        "models.light.client.clone()",
    ] {
        assert!(
            native[guard..].contains(clone),
            "source guard must precede every adapter"
        );
    }
}

#[tokio::test]
async fn chatgpt_protocol_before_output_remains_unknown_not_an_uncharged_refusal() {
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = Arc::new(Ready {
        calls: calls.clone(),
        error: Some(WorkModelError::Protocol),
        quiet: true,
    });
    let models = guard_run_sources(
        profile(),
        group("chatgpt", [ready(&calls, None), owner, ready(&calls, None)]),
    );
    let events = AtomicUsize::new(0);
    let sink = |_| {
        events.fetch_add(1, Ordering::SeqCst);
    };
    assert_eq!(
        models
            .page
            .client
            .call(request(&models.page), &sink)
            .await
            .err(),
        Some(WorkModelError::Protocol)
    );
    assert_eq!(
        models
            .light
            .client
            .call(request(&models.light), &sink)
            .await
            .err(),
        Some(WorkModelError::Protocol)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(events.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn malformed_go_identity_provider_and_wire_never_dispatch() {
    for variant in 0..4 {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut raw = group(
            GO,
            [
                ready(&calls, None),
                ready(&calls, None),
                ready(&calls, None),
            ],
        );
        match variant {
            0 => raw.page.entry.id = "opencode-go/invalid/page".into(),
            1 => raw.page.entry.model.provider = WorkModelProvider::Compatible,
            2 => raw.page.entry.model.wire = WorkModelWire::AnthropicMessages,
            _ => raw.page.entry.id = "unscoped/page".into(),
        }
        let models = guard_run_sources(profile(), raw);
        assert_eq!(
            models
                .page
                .client
                .call(request(&models.page), &|_| {})
                .await
                .err(),
            Some(Kind::Go.unsupported())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
