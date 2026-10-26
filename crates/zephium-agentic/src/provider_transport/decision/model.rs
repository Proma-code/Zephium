//! Typed decisions answered by the person's chosen model.
//!
//! The answers arrive as the arguments of one forced `typed_decision` tool
//! whose schema is the request's own answer schema; the same validator that
//! checks an emulated answer checks them. Admission, the reservation and the
//! transport's capacity and shutdown are the OpenAI emulation's own.

use zephium_core::work::model::{
    WorkModelError, WorkModelEvent, WorkModelMessage, WorkModelOutcome, WorkModelPart,
    WorkModelRequest, WorkModelStop, WorkModelSystemBlock, WorkModelTool, WorkModelToolChoice,
};

use super::*;
use crate::provider_transport::model_client::AgentProviderModelClient;

const DECISION_TOOL: &str = "typed_decision";
const DECISION_TOOL_DESCRIPTION: &str =
    "Return every answer as this tool's arguments, exactly matching its schema.";
/// Largest decision input reserved, as for the OpenAI emulation.
const MAX_DECISION_INPUT_TOKENS: u32 = 32_768;
const MAX_DECISION_OUTPUT_TOKENS: u32 = 4_096;
/// Envelope tokens beyond the request bytes, as for chosen-model page turns.
const DECISION_ENVELOPE_TOKENS: u32 = 4_096;
const MODEL_LABEL: &str = "chosen_model";

/// One policy-admitted typed-decision call on a chosen model.
pub struct ModelDecisionCall<'a> {
    transport: &'a AgentProviderTransport,
    model: &'a AgentProviderModelClient,
}

impl<'a> ModelDecisionCall<'a> {
    /// Shares the session transport's capacity and shutdown.
    pub fn new(transport: &'a AgentProviderTransport, model: &'a AgentProviderModelClient) -> Self {
        Self { transport, model }
    }

    fn output_tokens(&self) -> u32 {
        MAX_DECISION_OUTPUT_TOKENS.min(self.model.max_output_tokens())
    }

    /// Reserves the full input and output ceilings at the model's highest
    /// rates before any disclosure.
    pub fn call_budget(&self) -> Result<AgentModelCallBudget, AgentPolicyError> {
        let output = self.output_tokens();
        let cost = self
            .model
            .reservation_cost_micro_usd(u64::from(MAX_DECISION_INPUT_TOKENS), u64::from(output))
            .ok_or(AgentPolicyError::Budget)?;
        AgentModelCallBudget::try_new(0, output, cost)
    }

    fn request(&self, request: &DecisionRequest) -> Result<WorkModelRequest, AgentPolicyError> {
        let context =
            serde_json::json!({"state": request.state(), "questions": request.questions()});
        if contains_secret(&context) {
            return Err(AgentPolicyError::PayloadMismatch);
        }
        let content =
            serde_json::to_string(&context).map_err(|_| AgentPolicyError::PayloadMismatch)?;
        if content.len() > zephium_core::work::agent::MAX_AGENT_CONTEXT_BYTES {
            return Err(AgentPolicyError::Budget);
        }
        Ok(WorkModelRequest {
            image_fence: None,
            model: self.model.model().clone(),
            system: vec![WorkModelSystemBlock {
                text: EMULATION_INSTRUCTIONS.to_owned(),
                cache: true,
            }],
            tools: vec![WorkModelTool {
                name: DECISION_TOOL.to_owned(),
                description: DECISION_TOOL_DESCRIPTION.to_owned(),
                schema: request.answer_schema(),
                strict: false,
            }],
            messages: vec![WorkModelMessage::User(vec![WorkModelPart::Text(content)])],
            max_output_tokens: self.output_tokens(),
            reasoning: None,
            native_search: false,
            parallel_tools: false,
            tool_choice: WorkModelToolChoice::Named(DECISION_TOOL.to_owned()),
            volatile_tail: 0,
            cache_key: None,
        })
    }

    /// Uses the same filtered observation, account policy and original
    /// deadline as the OpenAI emulation. Usage above the reservation, a lost
    /// call or a deadline is charged the reservation.
    #[allow(clippy::too_many_arguments)]
    pub async fn evaluate_observation(
        &self,
        policy: &mut AgentRunPolicy,
        call: AgentModelCallRequest,
        observation: &SemanticObservation,
        projection: &DecisionObservation,
        deadline: Instant,
        cancellation: &AgentProviderCancellation,
        accounting: Option<&mut dyn DecisionCallAccounting>,
    ) -> Result<AdmittedDecisionOutput, AgentPolicyError> {
        let budget = self.call_budget()?;
        if call.budget() != budget || deadline <= Instant::now() || cancellation.is_cancelled() {
            return Err(AgentPolicyError::Budget);
        }
        let deadline = deadline.min(Instant::now() + EMULATION_CALL_TIMEOUT);
        let request = self.request(projection.request())?;
        let request_bytes = u32::try_from(
            request
                .system
                .iter()
                .map(|block| block.text.len())
                .chain(
                    request
                        .tools
                        .iter()
                        .map(|tool| tool.description.len() + tool.schema.to_string().len()),
                )
                .chain(request.messages.iter().map(|message| {
                    match message {
                        WorkModelMessage::User(parts) => parts
                            .iter()
                            .map(|part| match part {
                                WorkModelPart::Text(text) => text.len(),
                                _ => 0,
                            })
                            .sum(),
                        _ => 0,
                    }
                }))
                .sum::<usize>(),
        )
        .map_err(|_| AgentPolicyError::Budget)?;
        let input_tokens = request_bytes
            .checked_add(DECISION_ENVELOPE_TOKENS)
            .filter(|tokens| *tokens <= MAX_DECISION_INPUT_TOKENS)
            .ok_or(AgentPolicyError::Budget)?;
        let owned = DecisionPolicyGuard::admit(
            policy,
            call,
            observation,
            projection,
            input_tokens,
            request_bytes,
            accounting,
        )?;
        let started = Instant::now();
        let result = self.run(request, deadline, cancellation).await;
        let mut diagnostic = DecisionCallDiagnostic {
            backend: DecisionBackendKind::Emulation,
            model: MODEL_LABEL,
            question_count: projection.question_count(),
            state_bytes: projection.state_bytes(),
            input_tokens: None,
            elapsed_millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            http_status: None,
            attempts: None,
            failure: None,
            envelope_failure: None,
            rejected_envelope: None,
            confidence_buckets: [[0; 4]; 3],
        };
        let ceiling = move |failure| DecisionCallOutput {
            exact_usage: false,
            response: Err(failure),
            charged_usage: DecisionUsage {
                input_tokens,
                output_tokens: budget.output_tokens(),
            },
            cost_micro_usd: budget.cost_micro_usd(),
            diagnostic: DecisionCallDiagnostic {
                failure: Some(failure),
                ..diagnostic
            },
        };
        let output = match result {
            Err(failure) => ceiling(failure),
            Ok(outcome) => {
                let usage = outcome.usage;
                let within = usage.input_tokens <= u64::from(input_tokens)
                    && usage.output_tokens <= u64::from(budget.output_tokens());
                match (within, self.model.usage_cost_micro_usd(&usage)) {
                    (true, Some(cost)) if cost <= budget.cost_micro_usd() => {
                        let charged = DecisionUsage {
                            input_tokens: u32::try_from(usage.input_tokens).unwrap_or(u32::MAX),
                            output_tokens: u32::try_from(usage.output_tokens).unwrap_or(u32::MAX),
                        };
                        diagnostic.input_tokens = Some(charged.input_tokens);
                        diagnostic.attempts = Some(1);
                        let response = answer(&outcome).and_then(|bytes| {
                            projection
                                .request()
                                .decode_emulation(&bytes, charged)
                                .map_err(|_| DecisionCallFailure::InvalidAnswer)
                        });
                        diagnostic.failure = response.as_ref().err().copied();
                        diagnostic.confidence_buckets = confidence_buckets(&response);
                        DecisionCallOutput {
                            exact_usage: true,
                            response,
                            charged_usage: charged,
                            cost_micro_usd: cost,
                            diagnostic,
                        }
                    }
                    _ => ceiling(DecisionCallFailure::InvalidAnswer),
                }
            }
        };
        owned.settle(output, projection.input_stats(), request_bytes)
    }

    async fn run(
        &self,
        request: WorkModelRequest,
        deadline: Instant,
        cancellation: &AgentProviderCancellation,
    ) -> Result<WorkModelOutcome, DecisionCallFailure> {
        let mut slot = self
            .transport
            .reserve_key(TransportSlotKey::Planning(ulid::Ulid::new()))
            .map_err(|error| match error {
                ReserveError::Capacity | ReserveError::Duplicate => DecisionCallFailure::Capacity,
                _ => DecisionCallFailure::Unavailable,
            })?;
        {
            let _gate = self
                .transport
                .shared
                .shutdown
                .lock_commit_gate()
                .ok_or(DecisionCallFailure::Cancelled)?;
            if self.transport.shared.shutdown.is_cancelled() || cancellation.is_cancelled() {
                return Err(DecisionCallFailure::Cancelled);
            }
            slot.mark_committed();
        }
        let events = |_: WorkModelEvent| {};
        let result = tokio::select! {
            biased;
            () = self.transport.shared.shutdown.cancelled() => Err(DecisionCallFailure::Cancelled),
            () = cancellation.cancelled() => Err(DecisionCallFailure::Cancelled),
            outcome = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.model.client().call(request, &events),
            ) => match outcome {
                Ok(Ok(outcome)) if !outcome.usage.is_reported() => Err(DecisionCallFailure::Unavailable),
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(WorkModelError::RateLimited { .. })) => Err(DecisionCallFailure::RateLimited),
                Ok(Err(WorkModelError::Overloaded)) => Err(DecisionCallFailure::Overloaded),
                Ok(Err(WorkModelError::Unauthorized | WorkModelError::MissingKey)) => {
                    Err(DecisionCallFailure::Unauthorized)
                }
                Ok(Err(WorkModelError::BadRequest | WorkModelError::ContextTooLong)) => {
                    Err(DecisionCallFailure::InvalidRequest)
                }
                Ok(Err(WorkModelError::Cancelled)) => Err(DecisionCallFailure::Cancelled),
                Ok(Err(_)) => Err(DecisionCallFailure::Unavailable),
                Err(_) => Err(DecisionCallFailure::Deadline),
            },
        };
        // Every ending is charged by the caller, the reservation when unknown,
        // so the slot completes rather than sealing the shared transport.
        slot.mark_completed();
        result
    }
}

/// The answer object: the forced tool's arguments, or plain text from a
/// provider that ignored the forced tool. Anything else is invalid.
fn answer(outcome: &WorkModelOutcome) -> Result<Vec<u8>, DecisionCallFailure> {
    if outcome.stop == WorkModelStop::Refused {
        return Err(DecisionCallFailure::InvalidAnswer);
    }
    let mut text = String::new();
    for part in &outcome.assistant {
        match part {
            WorkModelPart::ToolCall(call) if call.name == DECISION_TOOL => {
                return serde_json::to_vec(&call.arguments)
                    .map_err(|_| DecisionCallFailure::InvalidAnswer);
            }
            WorkModelPart::ToolCall(_) => return Err(DecisionCallFailure::InvalidAnswer),
            WorkModelPart::Text(piece) if text.len() + piece.len() <= MAX_RESPONSE_BYTES => {
                text.push_str(piece)
            }
            WorkModelPart::Text(_) => return Err(DecisionCallFailure::InvalidAnswer),
            _ => {}
        }
    }
    if text.trim().is_empty() {
        return Err(DecisionCallFailure::InvalidAnswer);
    }
    Ok(text.into_bytes())
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
