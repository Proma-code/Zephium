use super::super::LeadModel;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const A: &str = "gateway/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const GO: &str = "opencode-go/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
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
#[tokio::test]
async fn retired_sources_reject_every_role_without_dispatch_or_events() {
    for prefix in ["chatgpt", A, GO, "gateway/invalid", "opencode-go/invalid"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let models = guard_run_sources(
            profile(),
            group(
                prefix,
                [
                    ready(&calls, None),
                    ready(&calls, None),
                    ready(&calls, None),
                ],
            ),
        );
        for model in [&models.lead, &models.page, &models.light] {
            for _ in 0..2 {
                assert_eq!(
                    model
                        .client
                        .call(request(model), &|_| panic!("retired source event"))
                        .await
                        .err(),
                    crate::work_models::retired_source_error(&model.entry)
                );
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn tombstone_provider_cannot_hide_behind_an_api_identifier() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut models = group(
        "openai",
        [
            ready(&calls, None),
            ready(&calls, None),
            ready(&calls, None),
        ],
    );
    models.page.entry.model.provider = WorkModelProvider::OpenCodeGo;
    let models = guard_run_sources(profile(), models);
    assert!(models
        .page
        .client
        .call(request(&models.page), &|_| panic!("event"))
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(models
        .lead
        .client
        .call(request(&models.lead), &|_| {})
        .await
        .is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn original_api_clients_preserve_identity_and_behavior() {
    for provider in crate::work_models::KEYED_PROVIDERS
        .into_iter()
        .chain([WorkModelProvider::Cloud])
    {
        let calls = Arc::new(AtomicUsize::new(0));
        let client = ready(&calls, None);
        let mut models = group("original", [client.clone(), client.clone(), client.clone()]);
        for model in [&mut models.lead, &mut models.page, &mut models.light] {
            model.entry.model.provider = provider;
        }
        let models = guard_run_sources(profile(), models);
        for model in [&models.lead, &models.page, &models.light] {
            assert!(Arc::ptr_eq(&model.client, &client));
            assert!(model.client.call(request(model), &|_| {}).await.is_ok());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}

#[test]
fn offline_responses_fixture_preserves_unknown_usage_without_tool_callbacks() {
    for missing in [true, false] {
        let mut response = serde_json::json!({"type":"response.completed","response":{
            "status":"completed", "output":[{"type":"function_call","call_id":"call_1",
            "namespace":"zephium","name":"read_page","arguments":"{}"}], "usage":null
        }});
        if missing {
            response["response"]
                .as_object_mut()
                .unwrap()
                .remove("usage");
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let models = group(
            "openai",
            [
                ready(&calls, None),
                ready(&calls, None),
                ready(&calls, None),
            ],
        );
        let mut request = request(&models.page);
        request.tools.push(WorkModelTool {
            name:"read_page".into(), description:"Read fixture".into(),
            schema:serde_json::json!({"type":"object","properties":{},"additionalProperties":false}), strict:true,
        });
        let result = zephium_work_protocol_fixtures::responses::fixture_call(
            &request,
            format!("data: {response}\n\n").as_bytes(),
            &|_| panic!("no early tool side effect"),
        );
        let (_, outcome) = result.unwrap();
        assert!(!outcome.usage.is_reported());
        assert!(outcome
            .assistant
            .iter()
            .any(|part| matches!(part, WorkModelPart::ToolCall(_))));
    }
}
