//! One model call for the lead or a helper: bounded retries on transient
//! failures, the run's deadline, and usage charged to the run in its own
//! accounting.
use std::sync::Arc;
use std::time::Duration;

use zephium_core::work::{model::*, runtime::*};
use zephium_ipc::work::WorkActivityV1;

use super::run::LeadRun;

/// A resolved model and its ready client.
#[derive(Clone)]
pub struct LeadModel {
    pub entry: WorkModelEntry,
    pub client: Arc<dyn WorkModelClient>,
}

/// The models a run uses, one per role.
#[derive(Clone)]
pub struct WorkLeadModels {
    pub lead: LeadModel,
    /// Helpers that act on pages.
    pub page: LeadModel,
    /// Helpers that read and summarize.
    pub light: LeadModel,
}
impl WorkLeadModels {
    pub fn for_role(&self, role: WorkModelRole) -> &LeadModel {
        match role {
            WorkModelRole::Page => &self.page,
            WorkModelRole::Light | WorkModelRole::Decision => &self.light,
            WorkModelRole::Lead => &self.lead,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallFailure {
    /// Nothing was sent or nothing was charged; the note says why.
    Refused(WorkModelError),
    /// A ChatGPT request may have dispatched, or output preceded a terminal
    /// refusal. Retain unknown usage conservatively without inventing a bill.
    ChatGptUnknownOutcome {
        fault: ChatGptModelFault,
        reservation: WorkUsage,
    },
    /// A Go request may have consumed usage without a confirmed response.
    GoUsageUnknown {
        fault: OpenCodeGoModelFault,
        reservation: WorkUsage,
    },
    /// A local gateway request may have reached upstream without measured usage.
    GatewayUsageUnknown {
        fault: GatewayModelFault,
        reservation: WorkUsage,
    },
    /// The call may have been charged, with no usable answer.
    Lost,
    /// The run stopped or ran out of time while the call ran.
    Stopped,
}

const CALL_TIMEOUT: Duration = Duration::from_secs(240);
const RETRIES: usize = 2;
/// Per million tokens, in micro-USD, when the catalog has no price.
const UNPRICED: WorkModelPrice = WorkModelPrice {
    input: 15_000_000,
    cached_input: 1_500_000,
    output: 60_000_000,
};

/// Tokens a call bills: fresh input and output. Cached input is charged in
/// the cost at its own price but not counted against the token limit.
pub(crate) fn usage(entry: &WorkModelEntry, usage: WorkModelUsage) -> Option<WorkUsage> {
    if !usage.is_reported() {
        return None;
    }
    let cached = usage.cached_input_tokens.min(usage.input_tokens);
    let fresh = usage.input_tokens - cached;
    let tokens = fresh.saturating_add(usage.output_tokens);
    let (cost, accounting) = match (usage.cost_micros, &entry.price) {
        (Some(cost), _) => (cost, WorkUsageAccounting::Exact),
        (None, Some(price)) => (
            priced(price, fresh, cached, usage.output_tokens),
            WorkUsageAccounting::Exact,
        ),
        (None, None) => (
            priced(&UNPRICED, fresh, cached, usage.output_tokens),
            WorkUsageAccounting::ConservativeReservation,
        ),
    };
    Some(WorkUsage {
        model_tokens: u32::try_from(tokens).unwrap_or(u32::MAX),
        cost_micro_usd: u32::try_from(cost).unwrap_or(u32::MAX),
        operations: 1,
        accounting,
    })
}
fn priced(price: &WorkModelPrice, fresh: u64, cached: u64, output: u64) -> u64 {
    let part = |tokens: u64, rate: u64| (u128::from(tokens) * u128::from(rate)).div_ceil(1_000_000);
    u64::try_from(
        part(fresh, price.input) + part(cached, price.cached_input) + part(output, price.output),
    )
    .unwrap_or(u64::MAX)
}

fn unavailable_usage_error(source_id: &str) -> WorkModelError {
    if source_id.starts_with("chatgpt/") {
        WorkModelError::ChatGpt {
            fault: ChatGptModelFault::Diagnostic {
                stage: ChatGptFailureStage::Complete,
                status: None,
                issue: ChatGptFailureIssue::UsageUnavailable,
                field: None,
            },
        }
    } else if source_id.starts_with("gateway/") {
        WorkModelError::Gateway {
            fault: GatewayModelFault::UsageUnavailable,
            usage_unknown: true,
        }
    } else if source_id.starts_with("opencode-go/") {
        WorkModelError::OpenCodeGo {
            fault: OpenCodeGoModelFault::UsageUnavailable,
            usage_unknown: true,
        }
    } else {
        WorkModelError::Protocol
    }
}

fn protected_source_failure(
    source_id: &str,
    error: WorkModelError,
    observed_output: bool,
    remaining: WorkExecutionLimits,
) -> Option<CallFailure> {
    let reservation = WorkUsage {
        model_tokens: remaining.model_tokens,
        cost_micro_usd: remaining.cost_micro_usd,
        operations: 1,
        accounting: WorkUsageAccounting::ConservativeReservation,
    };
    match error {
        // Direct ChatGPT transport uses Protocol for a may-have-dispatched
        // call that never reached a confirmed terminal, even before text.
        WorkModelError::Protocol if source_id.starts_with("chatgpt/") => {
            Some(CallFailure::ChatGptUnknownOutcome {
                fault: ChatGptModelFault::Unavailable,
                reservation,
            })
        }
        WorkModelError::ChatGpt { fault } if observed_output || fault.unknown_outcome() => {
            Some(CallFailure::ChatGptUnknownOutcome { fault, reservation })
        }
        WorkModelError::OpenCodeGo {
            fault,
            usage_unknown,
        } if usage_unknown || observed_output => {
            Some(CallFailure::GoUsageUnknown { fault, reservation })
        }
        WorkModelError::Gateway {
            fault,
            usage_unknown,
        } if usage_unknown || observed_output => {
            Some(CallFailure::GatewayUsageUnknown { fault, reservation })
        }
        _ => None,
    }
}

/// Runs one call and charges the run. Transient refusals retry twice,
/// honouring the provider's retry-after within the deadline.
pub(crate) async fn call(
    run: &LeadRun,
    model: &LeadModel,
    request: WorkModelRequest,
    role: WorkModelRole,
    completed_tool: Option<&(dyn Fn(WorkModelToolCall) + Send + Sync)>,
) -> Result<(WorkModelOutcome, WorkUsage), CallFailure> {
    let started = std::time::Instant::now();
    let first_event = std::sync::atomic::AtomicU64::new(0);
    let completed = std::sync::atomic::AtomicBool::new(false);
    let events = |event: WorkModelEvent| {
        let _ = first_event.compare_exchange(
            0,
            (started.elapsed().as_millis() as u64).max(1),
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
        if let WorkModelEvent::ToolCall(tool) = &event {
            completed.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(completed_tool) = completed_tool {
                completed_tool(tool.clone());
            }
        }
        if let WorkModelEvent::Search { .. } = event {
            run.activity(WorkActivityV1::Searching);
        }
    };
    let mut attempt = 0;
    loop {
        if run.cancelled().await {
            return Err(CallFailure::Stopped);
        }
        let deadline = run
            .probe
            .deadline()
            .min(std::time::Instant::now() + CALL_TIMEOUT);
        let result = tokio::select! {
            biased;
            _ = run.probe.wait_cancelled() => {
                // A sent call may still be billed even when its answer is never
                // received. Keep the reservation and never replay it on Stop.
                let remaining = run.remaining();
                run.charge(WorkUsage {
                    model_tokens: remaining.model_tokens,
                    cost_micro_usd: remaining.cost_micro_usd,
                    operations: 1,
                    accounting: WorkUsageAccounting::ConservativeReservation,
                });
                run.cancelled().await;
                return Err(CallFailure::Stopped);
            }
            result = tokio::time::timeout_at(
                deadline.into(), model.client.call(request.clone(), &events),
            ) => result,
        };
        let error = match result {
            Ok(Ok(outcome)) if !outcome.usage.is_reported() => {
                unavailable_usage_error(&model.entry.id)
            }
            Ok(Ok(outcome)) => {
                let Some(charged) = usage(&model.entry, outcome.usage) else {
                    return Err(CallFailure::Lost);
                };
                let first = first_event.load(std::sync::atomic::Ordering::Relaxed);
                run.report(super::WorkLeadDiagnostic::ModelSettled {
                    role,
                    stop: outcome.stop,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    first_event_ms: (first != 0).then_some(first),
                    output_tokens: outcome.usage.output_tokens,
                    reasoning_tokens: outcome.usage.reasoning_tokens,
                });
                run.charge(charged);
                return Ok((outcome, charged));
            }
            Ok(Err(error)) => error,
            Err(_) => {
                return Err(if run.cancelled().await {
                    CallFailure::Stopped
                } else {
                    CallFailure::Lost
                })
            }
        };
        run.report(super::WorkLeadDiagnostic::ModelRefused { error });
        if completed.load(std::sync::atomic::Ordering::Relaxed)
            && error != WorkModelError::Cancelled
        {
            return Err(CallFailure::Lost);
        }
        if let Some(
            failure @ (CallFailure::ChatGptUnknownOutcome { reservation, .. }
            | CallFailure::GatewayUsageUnknown { reservation, .. }
            | CallFailure::GoUsageUnknown { reservation, .. }),
        ) = protected_source_failure(
            &model.entry.id,
            error,
            first_event.load(std::sync::atomic::Ordering::Relaxed) != 0,
            run.remaining(),
        ) {
            // The provider can omit usage or fail after dispatch. Keep the
            // local reservation and the precise fault; do not retry or claim
            // that the account was charged this conservative amount.
            run.charge(reservation);
            return Err(failure);
        }
        let wait = match error {
            WorkModelError::Hosted {
                refusal: HostedRefusal::AlreadySent,
                ..
            } => return Err(CallFailure::Lost),
            WorkModelError::Cancelled => return Err(CallFailure::Stopped),
            WorkModelError::Protocol => return Err(CallFailure::Lost),
            _ => retry_wait(error, attempt),
        };
        match wait {
            Some(wait) if attempt < RETRIES => {
                attempt += 1;
                tokio::select! {
                    biased;
                    _ = run.probe.wait_cancelled() => {
                        run.cancelled().await;
                        return Err(CallFailure::Stopped);
                    }
                    _ = tokio::time::sleep(wait) => {},
                }
            }
            _ => return Err(CallFailure::Refused(error)),
        }
    }
}

/// Narrow retry admission. Explicit gateway and subscription sources never
/// inherit API transport retries, even before the first visible text delta.
fn retry_wait(error: WorkModelError, attempt: usize) -> Option<Duration> {
    match error {
        WorkModelError::RateLimited { retry_after_ms } => rate_limit_wait(retry_after_ms),
        WorkModelError::Overloaded | WorkModelError::Network => {
            Some(Duration::from_secs(2 + 3 * attempt as u64))
        }
        _ => None,
    }
}

/// How long to wait out a rate limit, or none when the server asked for more
/// than a run holds a call for: it is never asked again sooner than it said.
fn rate_limit_wait(retry_after_ms: Option<u64>) -> Option<Duration> {
    let wait = retry_after_ms.unwrap_or(4_000).max(1_000);
    (wait <= 30_000).then(|| Duration::from_millis(wait))
}

/// The text of an assistant turn, joined.
pub(crate) fn text(parts: &[WorkModelPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            WorkModelPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub(crate) fn tool_calls(parts: &[WorkModelPart]) -> Vec<WorkModelToolCall> {
    parts
        .iter()
        .filter_map(|part| match part {
            WorkModelPart::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

/// A line for the person from what the model said: its first line, as
/// plain text, bounded, never a paragraph.
pub(crate) fn say_line(text: &str) -> Option<String> {
    let line = plain(text.lines().map(str::trim).find(|line| !line.is_empty())?);
    let line = line.trim_start_matches(['#', '-', '>', ' ']).trim();
    (!line.is_empty()).then(|| clip(line, 240))
}

/// Markdown emphasis and code marks removed: the island and headlines are
/// set as plain type.
pub(crate) fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    for (index, c) in chars.iter().enumerate() {
        let word = |at: Option<&char>| at.is_some_and(|c| c.is_alphanumeric());
        let marker = match c {
            '*' | '`' => true,
            // Underscores inside words (snake_case) stay.
            '_' => {
                !(word(index.checked_sub(1).and_then(|i| chars.get(i)))
                    && word(chars.get(index + 1)))
            }
            _ => false,
        };
        if !marker {
            out.push(*c);
        }
    }
    out.trim().to_owned()
}

/// At most `max` bytes, cut at a character boundary.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(1);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_subscription_refusal_preserves_unknown_usage_and_exact_fault() {
        let remaining = WorkExecutionLimits {
            model_tokens: 1234,
            cost_micro_usd: 5678,
            operations: 9,
            timeout_seconds: 60,
            max_workers: 1,
        };
        for fault in [
            ChatGptModelFault::UsageLimit,
            ChatGptModelFault::Unavailable,
            ChatGptModelFault::SignedOut,
        ] {
            let error = WorkModelError::ChatGpt { fault };
            assert_eq!(
                protected_source_failure("chatgpt/fixture", error, false, remaining),
                None
            );
            assert_eq!(
                protected_source_failure("chatgpt/fixture", error, true, remaining),
                Some(CallFailure::ChatGptUnknownOutcome {
                    fault,
                    reservation: WorkUsage {
                        model_tokens: 1234,
                        cost_micro_usd: 5678,
                        operations: 1,
                        accounting: WorkUsageAccounting::ConservativeReservation
                    },
                })
            );
        }
        assert_eq!(
            protected_source_failure(
                "openai/fixture",
                WorkModelError::Unauthorized,
                true,
                remaining
            ),
            None
        );
    }

    #[test]
    fn chatgpt_protocol_without_output_retains_unknown_budget_and_source_fault() {
        let remaining = WorkExecutionLimits {
            model_tokens: 321,
            cost_micro_usd: 654,
            operations: 8,
            timeout_seconds: 30,
            max_workers: 1,
        };
        assert_eq!(
            protected_source_failure(
                "chatgpt/fixture",
                WorkModelError::Protocol,
                false,
                remaining
            ),
            Some(CallFailure::ChatGptUnknownOutcome {
                fault: ChatGptModelFault::Unavailable,
                reservation: WorkUsage {
                    model_tokens: 321,
                    cost_micro_usd: 654,
                    operations: 1,
                    accounting: WorkUsageAccounting::ConservativeReservation,
                },
            })
        );
        assert_eq!(retry_wait(WorkModelError::Protocol, 0), None);
        // Ordinary API protocol errors keep their original Lost path, not an
        // account-specific refusal or fabricated exact usage.
        for source in ["openai/fixture", "compatible/fixture"] {
            assert_eq!(
                protected_source_failure(source, WorkModelError::Protocol, false, remaining),
                None
            );
        }
    }

    #[test]
    fn diagnostic_stage_preserves_unknown_usage_before_any_delta_and_limits_remain_terminal() {
        let remaining = WorkExecutionLimits {
            model_tokens: 321,
            cost_micro_usd: 654,
            operations: 8,
            timeout_seconds: 30,
            max_workers: 1,
        };
        for (stage, status, unknown) in [
            (ChatGptFailureStage::Request, None, false),
            (ChatGptFailureStage::Credential, None, false),
            (ChatGptFailureStage::Send, None, true),
            (ChatGptFailureStage::Http, Some(401), false),
            (ChatGptFailureStage::Http, Some(429), false),
            (ChatGptFailureStage::Http, Some(408), false),
            (ChatGptFailureStage::Http, Some(302), false),
            (ChatGptFailureStage::Http, Some(500), true),
            (ChatGptFailureStage::Http, Some(503), true),
            (ChatGptFailureStage::Stream, Some(200), true),
            (ChatGptFailureStage::Complete, Some(200), true),
        ] {
            for issue in [
                ChatGptFailureIssue::Unavailable,
                ChatGptFailureIssue::Timeout,
                ChatGptFailureIssue::Protocol,
                ChatGptFailureIssue::UsageLimit,
                ChatGptFailureIssue::UsageUnavailable,
                ChatGptFailureIssue::Cancelled,
            ] {
                let fault = ChatGptModelFault::Diagnostic {
                    stage,
                    status,
                    issue,
                    field: None,
                };
                let error = WorkModelError::ChatGpt { fault };
                assert_eq!(fault.issue(), issue);
                assert_eq!(retry_wait(error, 0), None);
                let failure = protected_source_failure("chatgpt/fixture", error, false, remaining);
                let expected_unknown = unknown
                    || (stage == ChatGptFailureStage::Http
                        && matches!(
                            issue,
                            ChatGptFailureIssue::Protocol | ChatGptFailureIssue::Timeout
                        ));
                assert_eq!(failure.is_some(), expected_unknown, "{fault:?}");
                if let Some(CallFailure::ChatGptUnknownOutcome {
                    fault: actual,
                    reservation,
                }) = failure
                {
                    assert_eq!(actual, fault);
                    assert_eq!(reservation.model_tokens, remaining.model_tokens);
                    assert_eq!(
                        reservation.accounting,
                        WorkUsageAccounting::ConservativeReservation
                    );
                }
            }
        }
    }

    #[test]
    fn gateway_dispatch_uncertainty_reserves_budget_and_never_retries() {
        let remaining = WorkExecutionLimits {
            model_tokens: 1234,
            cost_micro_usd: 5678,
            operations: 9,
            timeout_seconds: 60,
            max_workers: 1,
        };
        for fault in [
            GatewayModelFault::NotConfigured,
            GatewayModelFault::Unauthorized,
            GatewayModelFault::Forbidden,
            GatewayModelFault::RateLimited,
            GatewayModelFault::Unavailable,
            GatewayModelFault::Unsupported,
            GatewayModelFault::Protocol,
            GatewayModelFault::Incomplete,
            GatewayModelFault::UsageUnavailable,
        ] {
            for usage_unknown in [false, true] {
                let error = WorkModelError::Gateway {
                    fault,
                    usage_unknown,
                };
                assert_eq!(retry_wait(error, 0), None);
                let result =
                    protected_source_failure("gateway/fixture/model", error, false, remaining);
                assert_eq!(result.is_some(), usage_unknown);
                if let Some(CallFailure::GatewayUsageUnknown {
                    reservation,
                    fault: saved,
                }) = result
                {
                    assert_eq!(saved, fault);
                    assert_eq!(reservation.model_tokens, remaining.model_tokens);
                    assert_eq!(reservation.cost_micro_usd, remaining.cost_micro_usd);
                    assert_eq!(
                        reservation.accounting,
                        WorkUsageAccounting::ConservativeReservation
                    );
                }
                assert!(matches!(
                    protected_source_failure("gateway/fixture/model", error, true, remaining),
                    Some(CallFailure::GatewayUsageUnknown { .. })
                ));
            }
        }
        assert!(
            retry_wait(WorkModelError::Network, 0).is_some(),
            "existing API behavior stays unchanged"
        );
    }

    #[test]
    fn a_rate_limit_is_waited_out_or_handed_back_never_cut_short() {
        assert_eq!(rate_limit_wait(None), Some(Duration::from_secs(4)));
        assert_eq!(rate_limit_wait(Some(250)), Some(Duration::from_secs(1)));
        assert_eq!(rate_limit_wait(Some(30_000)), Some(Duration::from_secs(30)));
        assert_eq!(rate_limit_wait(Some(30_001)), None);
        assert_eq!(rate_limit_wait(Some(3_600_000)), None);
    }

    fn entry(price: Option<WorkModelPrice>) -> WorkModelEntry {
        WorkModelEntry {
            id: "openai/gpt-6".into(),
            model: WorkModelRef {
                provider: WorkModelProvider::OpenAi,
                wire: WorkModelWire::OpenAiResponses,
                model: "gpt-6".into(),
            },
            display_name: "GPT-6".into(),
            roles: vec![WorkModelRole::Lead],
            recommended: true,
            context_window: 400_000,
            max_output: 32_000,
            supports: WorkModelSupports {
                tools: true,
                vision: true,
                prompt_cache: true,
                reasoning: true,
                native_search: true,
            },
            price,
        }
    }

    #[test]
    fn unavailable_measurements_are_never_priced_as_zero() {
        assert_eq!(usage(&entry(None), WorkModelUsage::unavailable()), None);
        assert_eq!(
            usage(&entry(Some(UNPRICED)), WorkModelUsage::unavailable()),
            None
        );
        let error = unavailable_usage_error("chatgpt/fixture");
        let remaining = WorkExecutionLimits {
            model_tokens: 44,
            cost_micro_usd: 55,
            operations: 2,
            timeout_seconds: 1,
            max_workers: 1,
        };
        let Some(CallFailure::ChatGptUnknownOutcome { reservation, fault }) =
            protected_source_failure("chatgpt/fixture", error, false, remaining)
        else {
            panic!("accounting halt")
        };
        assert_eq!(reservation.model_tokens, 44);
        assert_eq!(reservation.cost_micro_usd, 55);
        assert_eq!(
            reservation.accounting,
            WorkUsageAccounting::ConservativeReservation
        );
        assert_eq!(fault.issue(), ChatGptFailureIssue::UsageUnavailable);
        assert_eq!(retry_wait(error, 0), None);
    }

    #[test]
    fn cached_input_is_priced_but_not_counted_against_the_token_limit() {
        let price = WorkModelPrice {
            input: 2_000_000,
            cached_input: 200_000,
            output: 8_000_000,
        };
        let used = usage(
            &entry(Some(price)),
            WorkModelUsage {
                input_tokens: 10_000,
                cached_input_tokens: 8_000,
                cache_write_input_tokens: 0,
                output_tokens: 500,
                reasoning_tokens: 0,
                cost_micros: None,
                ..WorkModelUsage::default()
            },
        )
        .unwrap();
        assert_eq!(used.model_tokens, 2_500);
        assert_eq!(used.cost_micro_usd, 4_000 + 1_600 + 4_000);
        assert_eq!(used.accounting, WorkUsageAccounting::Exact);
        let unpriced = usage(
            &entry(None),
            WorkModelUsage {
                input_tokens: 1_000,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 100,
                reasoning_tokens: 0,
                cost_micros: None,
                ..WorkModelUsage::default()
            },
        )
        .unwrap();
        assert_eq!(
            unpriced.accounting,
            WorkUsageAccounting::ConservativeReservation
        );
        assert_eq!(unpriced.cost_micro_usd, 15_000 + 6_000);
    }

    #[test]
    fn the_line_for_the_person_is_one_bounded_line() {
        assert_eq!(
            say_line("\n## Looking at flights\nmore"),
            Some("Looking at flights".into())
        );
        assert_eq!(say_line("  "), None);
        assert!(say_line(&"word ".repeat(200)).unwrap().len() <= 243);
    }
    #[test]
    fn go_failure_accounting_never_retries_or_infers_free_usage() {
        let remaining = WorkExecutionLimits {
            model_tokens: 100,
            cost_micro_usd: 200,
            operations: 10,
            timeout_seconds: 30,
            max_workers: 1,
        };
        for unknown in [false, true] {
            let error = WorkModelError::OpenCodeGo {
                fault: OpenCodeGoModelFault::RateLimited,
                usage_unknown: unknown,
            };
            assert_eq!(retry_wait(error, 0), None);
            let classified =
                protected_source_failure("opencode-go/fixture/model", error, false, remaining);
            assert_eq!(
                matches!(classified, Some(CallFailure::GoUsageUnknown { .. })),
                unknown
            );
            assert!(matches!(
                protected_source_failure("opencode-go/fixture/model", error, true, remaining),
                Some(CallFailure::GoUsageUnknown { .. })
            ));
        }
    }
}
