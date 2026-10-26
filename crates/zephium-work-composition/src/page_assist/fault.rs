//! Model failures and costs in the panel's closed words.
use zephium_core::work::model::{
    GatewayModelFault, HostedRefusal, OpenCodeGoModelFault, WorkModelEntry, WorkModelError,
    WorkModelProvider, WorkModelUsage,
};
use zephium_ipc::page_assist::{PageAssistCostV1, PageAssistFaultV1, WorkModelFaultV1};
use zephium_page_assist::cost;

/// A failed model call as the person can act on it.
pub(crate) fn model_fault(error: WorkModelError, hosted: bool) -> PageAssistFaultV1 {
    let retry_after_ms = match error {
        WorkModelError::RateLimited { retry_after_ms }
        | WorkModelError::Hosted { retry_after_ms, .. } => {
            retry_after_ms.map(|ms| u32::try_from(ms).unwrap_or(u32::MAX))
        }
        _ => None,
    };
    let fault = match error {
        // These are explicit connection failures, not API-key or Zephium-plan
        // remedies. The caller terminates the reply; none grants a fallback.
        WorkModelError::ChatGpt { fault } => return PageAssistFaultV1::ChatGpt { fault },
        WorkModelError::OpenCodeGo { fault, .. } => match fault {
            OpenCodeGoModelFault::RateLimited => WorkModelFaultV1::RateLimited,
            OpenCodeGoModelFault::Unsupported => return PageAssistFaultV1::Unsupported,
            OpenCodeGoModelFault::NotConfigured
            | OpenCodeGoModelFault::Unauthorized
            | OpenCodeGoModelFault::Forbidden
            | OpenCodeGoModelFault::Unavailable
            | OpenCodeGoModelFault::Protocol
            | OpenCodeGoModelFault::Incomplete
            | OpenCodeGoModelFault::UsageUnavailable => return PageAssistFaultV1::Unavailable,
        },
        WorkModelError::Gateway { fault, .. } => match fault {
            GatewayModelFault::RateLimited => WorkModelFaultV1::RateLimited,
            GatewayModelFault::Unsupported => return PageAssistFaultV1::Unsupported,
            GatewayModelFault::NotConfigured
            | GatewayModelFault::Unauthorized
            | GatewayModelFault::Forbidden
            | GatewayModelFault::Unavailable
            | GatewayModelFault::Protocol
            | GatewayModelFault::Incomplete
            | GatewayModelFault::UsageUnavailable => return PageAssistFaultV1::Unavailable,
        },
        WorkModelError::MissingKey | WorkModelError::Unauthorized if hosted => {
            WorkModelFaultV1::SignedOut
        }
        WorkModelError::MissingKey => WorkModelFaultV1::MissingKey,
        WorkModelError::Unauthorized => WorkModelFaultV1::KeyRefused,
        WorkModelError::OverBudget if hosted => WorkModelFaultV1::HostedUsage,
        WorkModelError::OverBudget => WorkModelFaultV1::ProviderCredit,
        WorkModelError::PlanLimit => WorkModelFaultV1::PlanLimit,
        WorkModelError::Hosted { refusal, .. } => match refusal {
            HostedRefusal::PlanRequired => WorkModelFaultV1::PlanRequired,
            HostedRefusal::BillingReview => WorkModelFaultV1::BillingReview,
            HostedRefusal::OutOfCredits => WorkModelFaultV1::HostedUsage,
            HostedRefusal::DailyLimit => WorkModelFaultV1::DailyLimit,
            HostedRefusal::FreeUsagePaused => WorkModelFaultV1::FreeUsagePaused,
            HostedRefusal::ConfirmationLimited => WorkModelFaultV1::HostedBusy,
            HostedRefusal::Paused => WorkModelFaultV1::HostedPaused,
            HostedRefusal::AlreadySent
            | HostedRefusal::InvalidAudio
            | HostedRefusal::UnsupportedAudio
            | HostedRefusal::AudioTooLong => return PageAssistFaultV1::Unavailable,
        },
        WorkModelError::RateLimited { .. } => WorkModelFaultV1::RateLimited,
        WorkModelError::Overloaded => WorkModelFaultV1::ProviderDown,
        WorkModelError::Network => WorkModelFaultV1::Unreachable,
        WorkModelError::ContextTooLong => WorkModelFaultV1::TooLarge,
        WorkModelError::BadRequest | WorkModelError::Protocol | WorkModelError::Cancelled => {
            return PageAssistFaultV1::Unavailable
        }
    };
    PageAssistFaultV1::Model {
        fault,
        retry_after_ms,
    }
}

/// The provider as the cost line names it.
pub(crate) fn provider_name(provider: WorkModelProvider) -> &'static str {
    match provider {
        WorkModelProvider::OpenAi => "OpenAI",
        WorkModelProvider::Anthropic => "Anthropic",
        WorkModelProvider::Google => "Google",
        WorkModelProvider::DeepSeek => "DeepSeek",
        WorkModelProvider::OpenRouter => "OpenRouter",
        WorkModelProvider::XAi => "xAI",
        WorkModelProvider::Mistral => "Mistral",
        WorkModelProvider::Groq => "Groq",
        WorkModelProvider::Cerebras => "Cerebras",
        WorkModelProvider::Compatible => "your server",
        WorkModelProvider::OpenCodeGo => "OpenCode Go",
        WorkModelProvider::Cloud => "Zephium",
    }
}

/// The cost line for a finished call. Hosted charges are confirmed later.
pub(crate) fn cost(entry: &WorkModelEntry, usage: &WorkModelUsage) -> PageAssistCostV1 {
    let model = entry.display_name.clone();
    // A selected external account/gateway is not a local free model or an API
    // tariff. Even a same-named API price or zero estimate cannot price its bill.
    let external = match entry.model.provider {
        WorkModelProvider::OpenAi if entry.id.starts_with("chatgpt/") => Some("ChatGPT"),
        WorkModelProvider::Compatible if entry.id.starts_with("gateway/") => Some("local gateway"),
        WorkModelProvider::OpenCodeGo => Some("OpenCode Go"),
        _ => None,
    };
    if let Some(provider) = external {
        // Retain the existing wire variant; a null amount is rendered as unknown,
        // not as an API-key charge or a pending hosted charge confirmation.
        return PageAssistCostV1::Byok {
            model,
            provider: provider.into(),
            usd: None,
        };
    }
    match entry.model.provider {
        WorkModelProvider::Cloud => PageAssistCostV1::Hosted {
            model,
            credits: None,
        },
        WorkModelProvider::Compatible => PageAssistCostV1::Local { model },
        provider => {
            let micros = usage.cost_micros.or_else(|| {
                entry.price.as_ref().map(|price| {
                    let cached = usage.cached_input_tokens.min(usage.input_tokens);
                    cost::estimate_micros(
                        usage.input_tokens - cached,
                        usage.output_tokens,
                        price.input,
                        price.output,
                    )
                    .saturating_add(cost::estimate_micros(
                        cached,
                        0,
                        price.cached_input,
                        0,
                    ))
                })
            });
            PageAssistCostV1::Byok {
                model,
                provider: provider_name(provider).to_owned(),
                usd: micros.map(cost::usd),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zephium_core::work::model::ChatGptModelFault;

    #[test]
    fn faults_keep_the_retry_hint_and_hosting() {
        assert_eq!(
            model_fault(
                WorkModelError::RateLimited {
                    retry_after_ms: Some(7_000)
                },
                false
            ),
            PageAssistFaultV1::Model {
                fault: WorkModelFaultV1::RateLimited,
                retry_after_ms: Some(7_000)
            }
        );
        assert_eq!(
            model_fault(WorkModelError::MissingKey, true),
            PageAssistFaultV1::Model {
                fault: WorkModelFaultV1::SignedOut,
                retry_after_ms: None
            }
        );
        assert_eq!(
            model_fault(
                WorkModelError::Hosted {
                    refusal: HostedRefusal::DailyLimit,
                    retry_after_ms: Some(u64::MAX)
                },
                true
            ),
            PageAssistFaultV1::Model {
                fault: WorkModelFaultV1::DailyLimit,
                retry_after_ms: Some(u32::MAX)
            }
        );
        assert_eq!(
            model_fault(WorkModelError::Protocol, false),
            PageAssistFaultV1::Unavailable
        );
    }
    #[test]
    fn protected_faults_never_offer_an_api_key_or_hosted_billing_remedy() {
        let unavailable = PageAssistFaultV1::Unavailable;
        for fault in [
            ChatGptModelFault::SignedOut,
            ChatGptModelFault::Unavailable,
            ChatGptModelFault::NotEligible,
            ChatGptModelFault::UsageLimit,
            ChatGptModelFault::Unsupported,
        ] {
            assert_eq!(
                model_fault(WorkModelError::ChatGpt { fault }, true),
                PageAssistFaultV1::ChatGpt { fault }
            );
        }
        for unknown in [false, true] {
            for fault in [
                OpenCodeGoModelFault::NotConfigured,
                OpenCodeGoModelFault::Unauthorized,
                OpenCodeGoModelFault::Forbidden,
                OpenCodeGoModelFault::Unavailable,
                OpenCodeGoModelFault::Protocol,
                OpenCodeGoModelFault::Incomplete,
                OpenCodeGoModelFault::UsageUnavailable,
            ] {
                assert_eq!(
                    model_fault(
                        WorkModelError::OpenCodeGo {
                            fault,
                            usage_unknown: unknown
                        },
                        true
                    ),
                    unavailable
                );
            }
            for fault in [
                GatewayModelFault::NotConfigured,
                GatewayModelFault::Unauthorized,
                GatewayModelFault::Forbidden,
                GatewayModelFault::Unavailable,
                GatewayModelFault::Protocol,
                GatewayModelFault::Incomplete,
                GatewayModelFault::UsageUnavailable,
            ] {
                assert_eq!(
                    model_fault(
                        WorkModelError::Gateway {
                            fault,
                            usage_unknown: unknown
                        },
                        true
                    ),
                    unavailable
                );
            }
        }
    }
    #[test]
    fn protected_limits_have_no_invented_retry_deadline_and_unsupported_stays_explicit() {
        for error in [
            WorkModelError::OpenCodeGo {
                fault: OpenCodeGoModelFault::RateLimited,
                usage_unknown: true,
            },
            WorkModelError::Gateway {
                fault: GatewayModelFault::RateLimited,
                usage_unknown: true,
            },
        ] {
            assert_eq!(
                model_fault(error, false),
                PageAssistFaultV1::Model {
                    fault: WorkModelFaultV1::RateLimited,
                    retry_after_ms: None
                }
            );
        }
        for error in [
            WorkModelError::OpenCodeGo {
                fault: OpenCodeGoModelFault::Unsupported,
                usage_unknown: false,
            },
            WorkModelError::Gateway {
                fault: GatewayModelFault::Unsupported,
                usage_unknown: false,
            },
        ] {
            assert_eq!(model_fault(error, false), PageAssistFaultV1::Unsupported);
        }
        assert_eq!(
            model_fault(WorkModelError::Cancelled, false),
            PageAssistFaultV1::Unavailable
        );
    }
    fn entry(provider: WorkModelProvider, id: &str) -> WorkModelEntry {
        use zephium_core::work::model::{
            WorkModelPrice, WorkModelRef, WorkModelSupports, WorkModelWire,
        };
        WorkModelEntry {
            id: id.into(),
            model: WorkModelRef {
                provider,
                wire: WorkModelWire::ChatCompletions,
                model: "fixture".into(),
            },
            display_name: "Fixture".into(),
            roles: vec![],
            recommended: false,
            context_window: 1000,
            max_output: 100,
            supports: WorkModelSupports {
                tools: true,
                vision: false,
                prompt_cache: false,
                reasoning: false,
                native_search: false,
            },
            price: Some(WorkModelPrice {
                input: 1000,
                cached_input: 0,
                output: 1000,
            }),
        }
    }
    #[test]
    fn protected_sources_never_claim_free_local_or_an_api_estimated_charge() {
        for (provider, id, name) in [
            (WorkModelProvider::OpenAi, "chatgpt/fixture", "ChatGPT"),
            (
                WorkModelProvider::Compatible,
                "gateway/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/fixture",
                "local gateway",
            ),
            (
                WorkModelProvider::OpenCodeGo,
                "opencode-go/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/fixture",
                "OpenCode Go",
            ),
        ] {
            for amount in [None, Some(0), Some(4000)] {
                assert_eq!(
                    cost(
                        &entry(provider, id),
                        &WorkModelUsage {
                            cost_micros: amount,
                            input_tokens: 100,
                            ..Default::default()
                        }
                    ),
                    PageAssistCostV1::Byok {
                        model: "Fixture".into(),
                        provider: name.into(),
                        usd: None
                    }
                );
            }
        }
        assert_eq!(provider_name(WorkModelProvider::OpenCodeGo), "OpenCode Go");
    }
    #[test]
    fn existing_byok_local_and_hosted_cost_routes_are_unchanged() {
        assert_eq!(
            cost(
                &entry(WorkModelProvider::Compatible, "compatible/fixture"),
                &WorkModelUsage::default()
            ),
            PageAssistCostV1::Local {
                model: "Fixture".into()
            }
        );
        assert_eq!(
            cost(
                &entry(WorkModelProvider::Cloud, "cloud/fixture"),
                &WorkModelUsage::default()
            ),
            PageAssistCostV1::Hosted {
                model: "Fixture".into(),
                credits: None
            }
        );
        assert_eq!(
            cost(
                &entry(WorkModelProvider::OpenAi, "openai/fixture"),
                &WorkModelUsage {
                    cost_micros: Some(4000),
                    ..Default::default()
                }
            ),
            PageAssistCostV1::Byok {
                model: "Fixture".into(),
                provider: "OpenAI".into(),
                usd: Some("0.004".into())
            }
        );
        // A name alone never turns a regular provider into a subscription route.
        assert_eq!(
            cost(
                &entry(WorkModelProvider::Anthropic, "chatgpt/fixture"),
                &WorkModelUsage {
                    cost_micros: Some(4000),
                    ..Default::default()
                }
            ),
            PageAssistCostV1::Byok {
                model: "Fixture".into(),
                provider: "Anthropic".into(),
                usd: Some("0.004".into())
            }
        );
    }
}
