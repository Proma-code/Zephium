//! Source-specific, secret-free Codex quota presentation.
//!
//! Native composition projects the canonical provider-accounts QuotaState into
//! this DTO. No method reads an account, starts a process or admits model work.
//! Credits, local token use, API prices and plan names are never quota authority.
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CodexQuotaSourceV1 {
    OriginalAppServer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CodexQuotaStateV1 {
    NotChecked,
    Current,
    Stale,
    Unavailable,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CodexQuotaUnavailableV1 {
    NotConnected,
    SignedOut,
    NotReported,
}

/// Observation failure only. None of these values signs out an account or
/// grants/revokes permission to send a turn; native custody is a separate state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CodexQuotaFaultV1 {
    Changed,
    Busy,
    RefreshLimited,
    Clock,
    InvalidObservation,
    TooLarge,
    Transport,
    Timeout,
    ProviderRefused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CodexQuotaPlanV1 {
    Free,
    Go,
    Plus,
    Pro,
    Team,
    Business,
    Enterprise,
    Education,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CodexQuotaWindowKindV1 {
    Primary,
    Secondary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CodexQuotaWindowV1 {
    /// Original bucket position, not a guessed daily/weekly plan window.
    pub kind: CodexQuotaWindowKindV1,
    /// Canonical parser bounds this integer below JavaScript's exact range.
    #[specta(type = Option<f64>)]
    pub duration_minutes: Option<u64>,
    /// Native parser validates the original 0–100 contract. Null is unknown.
    pub used_percent: Option<f64>,
    /// Canonical derived value only when validated used_percent was reported.
    pub remaining_percent: Option<f64>,
    /// Exact decimal epoch milliseconds; no client-side allowance refill.
    pub resets_at_ms: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CodexQuotaBucketV1 {
    /// Original metered bucket ID, not a provider account identifier.
    pub id: Option<String>,
    /// Bounded original label. Render as ordinary escaped text, never HTML/URL.
    pub name: Option<String>,
    /// Informational only; never creates a missing window or included allowance.
    pub plan: Option<CodexQuotaPlanV1>,
    pub windows: Vec<CodexQuotaWindowV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct CodexQuotaViewV1 {
    pub source: CodexQuotaSourceV1,
    pub state: CodexQuotaStateV1,
    /// Original quota observation's account epoch, without JavaScript u64 loss.
    /// Healthy dormancy may retain it only as stale reading provenance. A new
    /// owner, account or profile clears it before projecting another epoch.
    pub account_epoch: Option<String>,
    pub checking: bool,
    pub refresh_needed: bool,
    /// Original full-read observation time; sparse updates never renew it.
    pub observed_at_ms: Option<String>,
    pub attempted_at_ms: Option<String>,
    /// Independent original buckets; never summed or converted into API credit.
    pub buckets: Vec<CodexQuotaBucketV1>,
    pub unavailable: Option<CodexQuotaUnavailableV1>,
    pub error: Option<CodexQuotaFaultV1>,
}

impl Default for CodexQuotaViewV1 {
    fn default() -> Self {
        Self {
            source: CodexQuotaSourceV1::OriginalAppServer,
            state: CodexQuotaStateV1::NotChecked,
            account_epoch: None,
            checking: false,
            refresh_needed: false,
            observed_at_ms: None,
            attempted_at_ms: None,
            buckets: Vec::new(),
            unavailable: None,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initial_view_is_not_fabricated_quota_or_entitlement() {
        let value = serde_json::to_value(CodexQuotaViewV1::default()).unwrap();
        assert_eq!(value["source"], "original_app_server");
        assert_eq!(value["state"], "not_checked");
        assert_eq!(value["buckets"], json!([]));
        assert!(value["account_epoch"].is_null());
        for forbidden in [
            "token",
            "email",
            "account_id",
            "auth_url",
            "credits",
            "cost",
            "entitled",
            "ordinary_usage_allowed",
        ] {
            assert!(value.get(forbidden).is_none());
        }
    }

    #[test]
    fn unknown_window_values_stay_null_not_zero_or_guessed_period() {
        let window = CodexQuotaWindowV1 {
            kind: CodexQuotaWindowKindV1::Primary,
            duration_minutes: None,
            used_percent: None,
            remaining_percent: None,
            resets_at_ms: None,
        };
        let value = serde_json::to_value(window).unwrap();
        for key in [
            "duration_minutes",
            "used_percent",
            "remaining_percent",
            "resets_at_ms",
        ] {
            assert!(value[key].is_null());
        }
    }

    #[test]
    fn reported_zero_and_exact_epoch_remain_distinct_from_unknown() {
        let view = CodexQuotaViewV1 {
            state: CodexQuotaStateV1::Current,
            account_epoch: Some(u64::MAX.to_string()),
            observed_at_ms: Some("1800000000000".into()),
            buckets: vec![CodexQuotaBucketV1 {
                id: Some("codex".into()),
                name: Some("Codex".into()),
                plan: Some(CodexQuotaPlanV1::Plus),
                windows: vec![CodexQuotaWindowV1 {
                    kind: CodexQuotaWindowKindV1::Primary,
                    duration_minutes: Some(300),
                    used_percent: Some(0.0),
                    remaining_percent: Some(100.0),
                    resets_at_ms: None,
                }],
            }],
            ..Default::default()
        };
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["account_epoch"], u64::MAX.to_string());
        assert_eq!(value["buckets"][0]["windows"][0]["used_percent"], 0.0);
        assert_eq!(
            serde_json::from_value::<CodexQuotaViewV1>(value).unwrap(),
            view
        );
    }

    #[test]
    fn stale_reading_retains_original_observation_and_separate_error() {
        let view = CodexQuotaViewV1 {
            state: CodexQuotaStateV1::Stale,
            account_epoch: Some("7".into()),
            observed_at_ms: Some("100".into()),
            attempted_at_ms: Some("200".into()),
            refresh_needed: true,
            error: Some(CodexQuotaFaultV1::Timeout),
            ..Default::default()
        };
        let value = serde_json::to_value(view).unwrap();
        assert_eq!(value["observed_at_ms"], "100");
        assert_eq!(value["attempted_at_ms"], "200");
        assert_eq!(value["state"], "stale");
        assert_eq!(value["error"], "timeout");
    }

    #[test]
    fn generated_surface_has_no_generic_provider_or_account_authority() {
        let types = specta::Types::default().register::<CodexQuotaViewV1>();
        let generated = specta_typescript::Typescript::default()
            .export(&types, specta_serde::Format)
            .unwrap();
        assert!(generated.contains("original_app_server"));
        assert!(generated.contains("account_epoch: string | null"));
        assert!(generated.contains("used_percent: number | null"));
        assert!(generated.contains("duration_minutes: number | null"));
        for forbidden in [
            "auth_url",
            "access_token",
            "account_id",
            "api_price",
            "entitled",
        ] {
            assert!(!generated.contains(forbidden));
        }
    }
}
