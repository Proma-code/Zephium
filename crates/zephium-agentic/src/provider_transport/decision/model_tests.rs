use std::sync::{Arc, Mutex};

use serde_json::Value;
use zephium_core::work::model::{
    WorkModelClient, WorkModelEntry, WorkModelFuture, WorkModelPrice, WorkModelProvider,
    WorkModelRef, WorkModelSupports, WorkModelToolCall, WorkModelUsage, WorkModelWire,
};

use super::super::tests::{admitted_fixture, fixture_answers};
use super::*;
use crate::{AgentModelUsageAccounting, AgentPolicyInstant};

struct Scripted {
    reply: Mutex<Option<WorkModelOutcome>>,
    seen: Mutex<Option<WorkModelRequest>>,
}

impl WorkModelClient for Scripted {
    fn call<'a>(
        &'a self,
        request: WorkModelRequest,
        _: &'a (dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> WorkModelFuture<'a> {
        *self.seen.lock().unwrap() = Some(request);
        let reply = self.reply.lock().unwrap().take().expect("one call");
        Box::pin(async move { Ok(reply) })
    }
}

fn model(client: Arc<Scripted>) -> AgentProviderModelClient {
    let entry = WorkModelEntry {
        id: "deepseek/deepseek-test".into(),
        model: WorkModelRef {
            provider: WorkModelProvider::DeepSeek,
            wire: WorkModelWire::ChatCompletions,
            model: "deepseek-test".into(),
        },
        display_name: "DeepSeek Test".into(),
        roles: Vec::new(),
        recommended: false,
        context_window: 128_000,
        max_output: 8_000,
        supports: WorkModelSupports {
            tools: true,
            vision: false,
            prompt_cache: true,
            reasoning: false,
            native_search: false,
        },
        price: Some(WorkModelPrice {
            input: 1_000,
            cached_input: 100,
            output: 5_000,
        }),
    };
    AgentProviderModelClient::try_new(&entry, client).unwrap()
}

fn reply(assistant: Vec<WorkModelPart>, input_tokens: u64) -> WorkModelOutcome {
    WorkModelOutcome {
        stop: WorkModelStop::ToolUse,
        usage: WorkModelUsage {
            input_tokens,
            output_tokens: 40,
            ..WorkModelUsage::default()
        },
        assistant,
    }
}

async fn decide(
    answer: impl FnOnce(&DecisionObservation) -> WorkModelOutcome,
) -> (AdmittedDecisionOutput, AgentRunPolicy, WorkModelRequest) {
    let (mut policy, call, observation, projection) = admitted_fixture();
    let client = Arc::new(Scripted {
        reply: Mutex::new(Some(answer(&projection))),
        seen: Mutex::new(None),
    });
    let model = model(client.clone());
    let transport =
        AgentProviderTransport::try_new(AgentProviderTransportConfig::STANDARD).unwrap();
    let decisions = ModelDecisionCall::new(&transport, &model);
    let call = AgentModelCallRequest::new(
        call.id(),
        call.lease(),
        call.account(),
        decisions.call_budget().unwrap(),
        AgentPolicyInstant::from_millis(101),
    );
    let output = decisions
        .evaluate_observation(
            &mut policy,
            call,
            &observation,
            &projection,
            Instant::now() + Duration::from_secs(4),
            &AgentProviderCancellation::new(),
            None,
        )
        .await
        .unwrap();
    assert!(transport.snapshot().unwrap().is_idle());
    let seen = client.seen.lock().unwrap().take().expect("called");
    (output, policy, seen)
}

#[tokio::test]
async fn a_chosen_model_answers_through_one_forced_tool_and_settles_its_usage() {
    let (output, policy, sent) = decide(|projection| {
        reply(
            vec![
                WorkModelPart::Text("Answering.".into()),
                WorkModelPart::ToolCall(WorkModelToolCall {
                    id: "call_1".into(),
                    name: DECISION_TOOL.into(),
                    arguments: fixture_answers(projection.request()),
                }),
            ],
            900,
        )
    })
    .await;
    assert!(output.call.response.is_ok());
    assert_eq!(
        output.receipt.usage_accounting(),
        AgentModelUsageAccounting::Exact
    );
    assert_eq!(output.receipt.input_tokens(), 900);
    assert_eq!(output.receipt.output_tokens(), 40);
    // 900 fresh × $0.001 + 40 out × $0.005 per million tokens, rounded up.
    assert_eq!(output.receipt.cost_micro_usd(), 2);
    assert_eq!(output.call.diagnostic.model, MODEL_LABEL);
    assert_eq!(policy.pending_model_calls(), 0);
    assert_eq!(
        sent.tool_choice,
        WorkModelToolChoice::Named(DECISION_TOOL.into())
    );
    assert_eq!(sent.system[0].text, EMULATION_INSTRUCTIONS);
    let [tool] = sent.tools.as_slice() else {
        panic!("one decision tool");
    };
    let WorkModelMessage::User(parts) = &sent.messages[0] else {
        panic!("one user turn");
    };
    let WorkModelPart::Text(context) = &parts[0] else {
        panic!("context text");
    };
    let context: Value = serde_json::from_str(context).unwrap();
    assert!(context.get("state").is_some() && context.get("questions").is_some());
    assert_eq!(tool.schema["type"], "object");
}

#[tokio::test]
async fn usage_above_the_reservation_is_charged_the_reservation() {
    let (output, policy, _) = decide(|projection| {
        reply(
            vec![WorkModelPart::ToolCall(WorkModelToolCall {
                id: "call_1".into(),
                name: DECISION_TOOL.into(),
                arguments: fixture_answers(projection.request()),
            })],
            u64::from(MAX_DECISION_INPUT_TOKENS) + 1,
        )
    })
    .await;
    assert_eq!(
        output.call.response.err(),
        Some(DecisionCallFailure::InvalidAnswer)
    );
    assert_eq!(
        output.receipt.usage_accounting(),
        AgentModelUsageAccounting::ReservationCeiling
    );
    assert_eq!(policy.pending_model_calls(), 0);
}

#[tokio::test]
async fn another_tool_or_no_answer_is_invalid_but_charged_exactly() {
    for assistant in [
        vec![WorkModelPart::ToolCall(WorkModelToolCall {
            id: "call_1".into(),
            name: "navigate".into(),
            arguments: serde_json::json!({}),
        })],
        vec![WorkModelPart::Text("  ".into())],
    ] {
        let (output, _, _) = decide(|_| reply(assistant, 700)).await;
        assert_eq!(
            output.call.response.err(),
            Some(DecisionCallFailure::InvalidAnswer)
        );
        assert_eq!(
            output.receipt.usage_accounting(),
            AgentModelUsageAccounting::Exact
        );
        assert_eq!(output.receipt.input_tokens(), 700);
    }
}

#[tokio::test]
async fn unavailable_usage_retains_reservation_without_decision_or_numeric_diagnostic() {
    let (output, policy, _) = decide(|projection| {
        let mut response = reply(
            vec![WorkModelPart::ToolCall(WorkModelToolCall {
                id: "call_1".into(),
                name: DECISION_TOOL.into(),
                arguments: fixture_answers(projection.request()),
            })],
            900,
        );
        response.usage = WorkModelUsage::unavailable();
        response
    })
    .await;
    assert_eq!(
        output.call.response.err(),
        Some(DecisionCallFailure::Unavailable)
    );
    assert!(!output.call.exact_usage);
    assert_eq!(output.call.diagnostic.input_tokens, None);
    assert_eq!(
        output.receipt.usage_accounting(),
        AgentModelUsageAccounting::ReservationCeiling
    );
    assert_eq!(policy.pending_model_calls(), 0);
}
