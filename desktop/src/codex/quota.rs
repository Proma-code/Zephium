//! Pure projection of the current original Codex account's quota observation.
use zephium_ipc::codex_quota::*;
use zephium_provider_accounts::quota::{
    QuotaFault, QuotaPlan, QuotaReading, QuotaSource, QuotaState, QuotaUnavailable, WindowKind,
};

pub(super) fn project(
    state: &QuotaState,
    profile_revision: u64,
    account_revision: u64,
    checking: bool,
    refresh_needed: bool,
    attempted_at_ms: Option<u64>,
) -> CodexQuotaViewV1 {
    let mut view = CodexQuotaViewV1 {
        account_epoch: (account_revision != 0).then(|| account_revision.to_string()),
        checking,
        refresh_needed,
        attempted_at_ms: attempted_at_ms.map(|value| value.to_string()),
        ..Default::default()
    };
    if profile_revision == 0 || account_revision == 0 {
        view.state = CodexQuotaStateV1::Unavailable;
        view.error = Some(CodexQuotaFaultV1::Changed);
        return view;
    }
    match state {
        QuotaState::Unavailable(reason) => {
            view.state = CodexQuotaStateV1::Unavailable;
            view.unavailable = match reason {
                QuotaUnavailable::ObserverNotConnected => {
                    Some(CodexQuotaUnavailableV1::NotConnected)
                }
                QuotaUnavailable::SignedOut => Some(CodexQuotaUnavailableV1::SignedOut),
                QuotaUnavailable::NotReported => Some(CodexQuotaUnavailableV1::NotReported),
                QuotaUnavailable::NoVerifiedContract | QuotaUnavailable::ProviderUnsupported => {
                    view.error = Some(CodexQuotaFaultV1::InvalidObservation);
                    None
                }
            };
        }
        QuotaState::Error(error) => {
            view.state = CodexQuotaStateV1::Error;
            view.error = Some(fault(*error));
        }
        QuotaState::Available(reading) => {
            view.state = CodexQuotaStateV1::Current;
            if !reading_into(reading, profile_revision, account_revision, &mut view) {
                return invalid(view);
            }
        }
        QuotaState::Stale {
            reading,
            fault: error,
        } => {
            view.state = CodexQuotaStateV1::Stale;
            view.error = error.map(fault);
            if !reading_into(reading, profile_revision, account_revision, &mut view) {
                return invalid(view);
            }
        }
    }
    view
}

fn invalid(mut view: CodexQuotaViewV1) -> CodexQuotaViewV1 {
    view.state = CodexQuotaStateV1::Error;
    view.error = Some(CodexQuotaFaultV1::InvalidObservation);
    view.observed_at_ms = None;
    view.buckets.clear();
    view
}

fn reading_into(
    reading: &QuotaReading,
    profile_revision: u64,
    account_revision: u64,
    view: &mut CodexQuotaViewV1,
) -> bool {
    if reading.provenance.source != QuotaSource::CodexAppServer
        || reading.provenance.profile_revision != profile_revision
        || reading.provenance.account_revision != account_revision
        || reading.gateway_spend.is_some()
    {
        return false;
    }
    let mut buckets = Vec::with_capacity(reading.buckets.len());
    for bucket in &reading.buckets {
        let mut windows = Vec::with_capacity(bucket.windows.len());
        for window in &bucket.windows {
            let kind = match window.kind {
                WindowKind::Primary => CodexQuotaWindowKindV1::Primary,
                WindowKind::Secondary => CodexQuotaWindowKindV1::Secondary,
                WindowKind::FiveHour | WindowKind::SevenDay => return false,
            };
            windows.push(CodexQuotaWindowV1 {
                kind,
                duration_minutes: window.duration_minutes,
                used_percent: window.used_percent,
                remaining_percent: window.remaining_percent,
                resets_at_ms: window.resets_at_ms.map(|value| value.to_string()),
            });
        }
        buckets.push(CodexQuotaBucketV1 {
            id: bucket.id.clone(),
            name: bucket.name.clone(),
            plan: bucket.plan.map(plan),
            windows,
        });
    }
    view.observed_at_ms = Some(reading.captured_at_ms.to_string());
    view.buckets = buckets;
    true
}

fn plan(value: QuotaPlan) -> CodexQuotaPlanV1 {
    match value {
        QuotaPlan::Free => CodexQuotaPlanV1::Free,
        QuotaPlan::Go => CodexQuotaPlanV1::Go,
        QuotaPlan::Plus => CodexQuotaPlanV1::Plus,
        QuotaPlan::Pro => CodexQuotaPlanV1::Pro,
        QuotaPlan::Team => CodexQuotaPlanV1::Team,
        QuotaPlan::Business => CodexQuotaPlanV1::Business,
        QuotaPlan::Enterprise => CodexQuotaPlanV1::Enterprise,
        QuotaPlan::Education => CodexQuotaPlanV1::Education,
        QuotaPlan::Other => CodexQuotaPlanV1::Other,
    }
}
fn fault(value: QuotaFault) -> CodexQuotaFaultV1 {
    match value {
        QuotaFault::MessageTooLarge => CodexQuotaFaultV1::TooLarge,
        QuotaFault::InvalidTimestamp | QuotaFault::OlderObservation => CodexQuotaFaultV1::Clock,
        QuotaFault::SourceMismatch | QuotaFault::Retired => CodexQuotaFaultV1::Changed,
        QuotaFault::Transport => CodexQuotaFaultV1::Transport,
        QuotaFault::TimedOut => CodexQuotaFaultV1::Timeout,
        QuotaFault::ProviderRefused => CodexQuotaFaultV1::ProviderRefused,
        _ => CodexQuotaFaultV1::InvalidObservation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zephium_provider_accounts::quota::{QuotaBucket, QuotaProvenance, QuotaWindow};
    fn reading() -> QuotaReading {
        QuotaReading {
            provenance: QuotaProvenance {
                source: QuotaSource::CodexAppServer,
                profile_revision: 3,
                account_revision: 5,
            },
            captured_at_ms: 123,
            buckets: vec![QuotaBucket {
                id: Some("codex".into()),
                name: Some("Codex".into()),
                plan: Some(QuotaPlan::Pro),
                windows: vec![QuotaWindow {
                    kind: WindowKind::Primary,
                    duration_minutes: None,
                    used_percent: Some(0.0),
                    remaining_percent: Some(100.0),
                    resets_at_ms: None,
                }],
            }],
            gateway_spend: None,
        }
    }
    #[test]
    fn preserves_reported_zero_and_unknown_metadata_without_price_or_guessed_period() {
        let view = project(
            &QuotaState::Available(reading()),
            3,
            5,
            false,
            false,
            Some(120),
        );
        assert_eq!(view.state, CodexQuotaStateV1::Current);
        assert_eq!(view.account_epoch.as_deref(), Some("5"));
        assert_eq!(view.observed_at_ms.as_deref(), Some("123"));
        assert_eq!(view.buckets[0].name.as_deref(), Some("Codex"));
        assert_eq!(view.buckets[0].windows[0].used_percent, Some(0.0));
        assert_eq!(view.buckets[0].windows[0].duration_minutes, None);
        assert_eq!(view.buckets[0].windows[0].resets_at_ms, None);
    }
    #[test]
    fn stale_observation_retains_original_timestamp_and_failure_is_not_account_state() {
        let view = project(
            &QuotaState::Stale {
                reading: reading(),
                fault: Some(QuotaFault::TimedOut),
            },
            3,
            5,
            true,
            true,
            Some(200),
        );
        assert_eq!(view.state, CodexQuotaStateV1::Stale);
        assert_eq!(view.error, Some(CodexQuotaFaultV1::Timeout));
        assert_eq!(view.observed_at_ms.as_deref(), Some("123"));
        assert_eq!(view.attempted_at_ms.as_deref(), Some("200"));
        assert_eq!(view.buckets.len(), 1);
    }
    #[test]
    fn wrong_source_epoch_and_foreign_window_never_cross_projection() {
        for n in 0..4 {
            let mut r = reading();
            match n {
                0 => r.provenance.source = QuotaSource::ClaudeCodeStatusLine,
                1 => r.provenance.profile_revision = 4,
                2 => r.provenance.account_revision = 6,
                _ => r.buckets[0].windows[0].kind = WindowKind::FiveHour,
            }
            let view = project(&QuotaState::Available(r), 3, 5, false, false, None);
            assert_eq!(view.state, CodexQuotaStateV1::Error);
            assert!(view.buckets.is_empty());
            assert!(view.observed_at_ms.is_none());
        }
    }
}
