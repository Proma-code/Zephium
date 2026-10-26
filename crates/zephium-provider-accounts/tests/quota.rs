use zephium_provider_accounts::quota::{
    parse_claude_statusline, parse_codex_rate_limits, QuotaCapability, QuotaFault, QuotaPlan,
    QuotaProjection, QuotaProvenance, QuotaSource, QuotaState, QuotaUnavailable, SpendPeriod,
    WindowKind, MAX_QUOTA_BYTES,
};

const CAPTURED: u64 = 1_800_000_000_000;

fn provenance(source: QuotaSource) -> QuotaProvenance {
    QuotaProvenance {
        source,
        profile_revision: 1,
        account_revision: 2,
    }
}

#[test]
fn plus_windows_come_from_provider_and_credits_do_not_mean_unlimited() {
    let reading = parse_codex_rate_limits(
        include_bytes!("fixtures/quota/codex-plus.json"),
        provenance(QuotaSource::CodexAppServer),
        CAPTURED,
    )
    .unwrap();
    assert_eq!(reading.buckets.len(), 1);
    assert_eq!(reading.buckets[0].plan, Some(QuotaPlan::Plus));
    let windows = &reading.buckets[0].windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].kind, WindowKind::Primary);
    assert_eq!(windows[0].duration_minutes, Some(300));
    assert_eq!(windows[0].remaining_percent, Some(75.0));
    assert_eq!(windows[0].resets_at_ms, Some(1_800_000_300_000));
    assert_eq!(windows[1].remaining_percent, Some(39.5));
    assert_eq!(reading.captured_at_ms, CAPTURED);
    assert!(reading.gateway_spend.is_none());
}

#[test]
fn multi_bucket_pro_response_replaces_legacy_without_inventing_missing_windows() {
    let reading = parse_codex_rate_limits(
        include_bytes!("fixtures/quota/codex-pro-buckets.json"),
        provenance(QuotaSource::CodexAppServer),
        CAPTURED,
    )
    .unwrap();
    assert_eq!(reading.buckets.len(), 2);
    let codex = reading
        .buckets
        .iter()
        .find(|bucket| bucket.id.as_deref() == Some("codex"))
        .unwrap();
    assert_eq!(codex.plan, Some(QuotaPlan::Pro));
    assert_eq!(codex.windows.len(), 1);
    assert_eq!(codex.windows[0].kind, WindowKind::Secondary);
    assert_eq!(codex.windows[0].duration_minutes, Some(10080));
    assert_eq!(codex.windows[0].remaining_percent, Some(80.0));
    let other = reading
        .buckets
        .iter()
        .find(|bucket| bucket.id.as_deref() == Some("codex_other"))
        .unwrap();
    assert_eq!(other.plan, None);
    assert_eq!(other.windows[0].duration_minutes, Some(15));
    assert_eq!(other.windows[0].remaining_percent, Some(57.75));
}

#[test]
fn claude_quota_does_not_use_context_or_spend_as_subscription_balance() {
    let reading = parse_claude_statusline(
        include_bytes!("fixtures/quota/claude-statusline.json"),
        provenance(QuotaSource::ClaudeCodeStatusLine),
        CAPTURED,
    )
    .unwrap();
    assert_eq!(reading.buckets.len(), 1);
    assert_eq!(reading.buckets[0].plan, None);
    let windows = &reading.buckets[0].windows;
    assert_eq!(windows[0].kind, WindowKind::FiveHour);
    assert_eq!(windows[0].duration_minutes, Some(300));
    assert_eq!(windows[0].remaining_percent, Some(76.5));
    assert_eq!(windows[1].kind, WindowKind::SevenDay);
    assert_eq!(windows[1].remaining_percent, Some(58.75));
    let spend = reading.gateway_spend.unwrap();
    assert_eq!(spend.used_percent, Some(125.0));
    assert_eq!(spend.estimated_used_usd, Some(625.5));
    assert_eq!(spend.limit_usd, Some(500.0));
    assert_eq!(spend.period, Some(SpendPeriod::Monthly));
}

#[test]
fn absent_and_null_fields_are_unknown_not_zero_or_unlimited() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let reading = parse_codex_rate_limits(
        br#"{"rateLimits":{"planType":"pro","primary":{"usedPercent":null,"resetsAt":null},"secondary":null}}"#,
        scope, CAPTURED,
    ).unwrap();
    let window = &reading.buckets[0].windows[0];
    assert_eq!(window.used_percent, None);
    assert_eq!(window.remaining_percent, None);
    assert_eq!(window.duration_minutes, None);
    assert_eq!(window.resets_at_ms, None);
    for bytes in [
        br#"{}"#.as_slice(),
        br#"{"rate_limits":null}"#,
        br#"{"context_window":{"used_percentage":65}}"#,
    ] {
        let reading = parse_claude_statusline(
            bytes,
            provenance(QuotaSource::ClaudeCodeStatusLine),
            CAPTURED,
        )
        .unwrap();
        assert!(reading.buckets.is_empty());
        assert!(reading.gateway_spend.is_none());
    }
}

#[test]
fn missing_claude_window_remains_missing_and_spend_dollars_are_optional() {
    let reading = parse_claude_statusline(
        br#"{"rate_limits":{"five_hour":null,"seven_day":{"resets_at":1800604800},"spend_limit":{"used_percentage":0}}}"#,
        provenance(QuotaSource::ClaudeCodeStatusLine), CAPTURED,
    ).unwrap();
    assert_eq!(reading.buckets[0].windows.len(), 1);
    assert_eq!(reading.buckets[0].windows[0].kind, WindowKind::SevenDay);
    assert_eq!(reading.buckets[0].windows[0].remaining_percent, None);
    let spend = reading.gateway_spend.unwrap();
    assert_eq!(spend.used_percent, Some(0.0));
    assert_eq!(spend.estimated_used_usd, None);
    assert_eq!(spend.limit_usd, None);
    assert_eq!(spend.period, None);
}

#[test]
fn invalid_percentages_durations_and_resets_are_typed_failures() {
    for (field, value, fault) in [
        ("usedPercent", "-0.1", QuotaFault::InvalidPercentage),
        ("usedPercent", "100.1", QuotaFault::InvalidPercentage),
        ("usedPercent", "\"NaN\"", QuotaFault::InvalidPercentage),
        ("usedPercent", "true", QuotaFault::InvalidPercentage),
        ("windowDurationMins", "-1", QuotaFault::InvalidDuration),
        ("windowDurationMins", "0", QuotaFault::InvalidDuration),
        ("windowDurationMins", "1.5", QuotaFault::InvalidDuration),
        ("resetsAt", "-1", QuotaFault::InvalidReset),
        ("resetsAt", "18446744073709551", QuotaFault::InvalidReset),
        ("resetsAt", "1.5", QuotaFault::InvalidReset),
    ] {
        let input = format!(r#"{{"rateLimits":{{"primary":{{"{field}":{value}}}}}}}"#);
        assert_eq!(
            parse_codex_rate_limits(
                input.as_bytes(),
                provenance(QuotaSource::CodexAppServer),
                CAPTURED
            ),
            Err(fault),
            "{field}={value}"
        );
    }
    for value in ["-1", "100.1", "\"NaN\"", "true"] {
        let input = format!(r#"{{"rate_limits":{{"five_hour":{{"used_percentage":{value}}}}}}}"#);
        assert_eq!(
            parse_claude_statusline(
                input.as_bytes(),
                provenance(QuotaSource::ClaudeCodeStatusLine),
                CAPTURED
            ),
            Err(QuotaFault::InvalidPercentage)
        );
    }
}

#[test]
fn percentage_endpoints_and_partial_readings_remain_exact() {
    for used in [0.0, 0.1, 50.25, 99.99, 100.0] {
        let input = format!(r#"{{"rateLimits":{{"primary":{{"usedPercent":{used}}}}}}}"#);
        let reading = parse_codex_rate_limits(
            input.as_bytes(),
            provenance(QuotaSource::CodexAppServer),
            CAPTURED,
        )
        .unwrap();
        assert_eq!(
            reading.buckets[0].windows[0].remaining_percent,
            Some(100.0 - used)
        );
        assert_eq!(reading.buckets[0].windows[0].resets_at_ms, None);
    }
}

#[test]
fn malformed_oversized_and_rpc_envelopes_do_not_leak_provider_details() {
    let scope = provenance(QuotaSource::CodexAppServer);
    for input in [
        br#"null"#.as_slice(),
        br#"[]"#,
        br#"{"rateLimits":"synthetic-secret"}"#,
        br#"{"error":{"message":"synthetic-secret"}}"#,
        br#"{"result":{"rateLimits":{}}}"#,
    ] {
        let error = parse_codex_rate_limits(input, scope, CAPTURED).unwrap_err();
        assert_eq!(error, QuotaFault::InvalidShape);
        assert!(!format!("{error:?}").contains("synthetic-secret"));
    }
    assert_eq!(
        parse_codex_rate_limits(b"{", scope, CAPTURED),
        Err(QuotaFault::InvalidJson)
    );
    assert_eq!(
        parse_codex_rate_limits(&vec![b' '; MAX_QUOTA_BYTES + 1], scope, CAPTURED),
        Err(QuotaFault::MessageTooLarge)
    );
    assert_eq!(
        parse_codex_rate_limits(
            br#"{"rateLimits":{"primary":{"usedPercent":1e999}}}"#,
            scope,
            CAPTURED
        ),
        Err(QuotaFault::InvalidJson)
    );
}

#[test]
fn unsupported_sources_never_acquire_a_percentage_from_a_parser() {
    for source in [
        QuotaSource::DirectChatGpt,
        QuotaSource::OpenCodeGo,
        QuotaSource::AntigravityGateway,
    ] {
        assert_ne!(source.capability(), QuotaCapability::DocumentedObservation);
        assert_eq!(
            parse_codex_rate_limits(
                include_bytes!("fixtures/quota/codex-plus.json"),
                provenance(source),
                CAPTURED
            ),
            Err(QuotaFault::SourceMismatch)
        );
        assert_eq!(
            parse_claude_statusline(
                include_bytes!("fixtures/quota/claude-statusline.json"),
                provenance(source),
                CAPTURED
            ),
            Err(QuotaFault::SourceMismatch)
        );
        let projection = QuotaProjection::new(provenance(source));
        assert!(matches!(projection.state(), QuotaState::Unavailable(_)));
    }
    assert_eq!(
        QuotaSource::DirectChatGpt.capability(),
        QuotaCapability::NoVerifiedContract
    );
    assert_eq!(
        QuotaSource::OpenCodeGo.capability(),
        QuotaCapability::NoVerifiedContract
    );
    assert_eq!(
        QuotaSource::AntigravityGateway.capability(),
        QuotaCapability::Unsupported
    );
}

#[test]
fn transient_failure_preserves_only_same_account_reading_as_stale() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let reading = parse_codex_rate_limits(
        include_bytes!("fixtures/quota/codex-plus.json"),
        scope,
        CAPTURED,
    )
    .unwrap();
    let mut projection = QuotaProjection::new(scope);
    projection.record(reading.clone()).unwrap();
    assert!(matches!(projection.state(), QuotaState::Available(_)));
    projection
        .fail(QuotaFault::Transport, CAPTURED + 10)
        .unwrap();
    assert!(
        matches!(projection.state(), QuotaState::Stale { reading: retained, fault: Some(QuotaFault::Transport) } if retained == &reading)
    );
    assert_eq!(projection.last_attempt_at_ms(), Some(CAPTURED + 10));
    projection.make_unavailable(QuotaUnavailable::SignedOut);
    assert_eq!(
        projection.state(),
        &QuotaState::Unavailable(QuotaUnavailable::SignedOut)
    );
    assert_eq!(projection.last_attempt_at_ms(), None);
}

#[test]
fn other_account_or_older_reading_cannot_replace_current_observation() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let mut projection = QuotaProjection::new(scope);
    let reading = parse_codex_rate_limits(
        include_bytes!("fixtures/quota/codex-plus.json"),
        scope,
        CAPTURED,
    )
    .unwrap();
    projection.record(reading.clone()).unwrap();
    let mut other_account = reading.clone();
    other_account.provenance.account_revision += 1;
    assert_eq!(
        projection.record(other_account),
        Err(QuotaFault::SourceMismatch)
    );
    let mut other_profile = reading.clone();
    other_profile.provenance.profile_revision += 1;
    assert_eq!(
        projection.record(other_profile),
        Err(QuotaFault::SourceMismatch)
    );
    let mut older = reading.clone();
    older.captured_at_ms -= 1;
    assert_eq!(projection.record(older), Err(QuotaFault::OlderObservation));
    assert_eq!(
        projection.fail(QuotaFault::TimedOut, CAPTURED - 1),
        Err(QuotaFault::OlderObservation)
    );
    assert_eq!(projection.state(), &QuotaState::Available(reading));
}

#[test]
fn expired_and_age_stale_readings_are_never_refilled_locally() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let reading = parse_codex_rate_limits(
        include_bytes!("fixtures/quota/codex-plus.json"),
        scope,
        CAPTURED,
    )
    .unwrap();
    let mut projection = QuotaProjection::new(scope);
    projection.record(reading.clone()).unwrap();
    projection.mark_stale(CAPTURED + 300_000, 900_000);
    assert!(
        matches!(projection.state(), QuotaState::Stale { reading: retained, fault: None } if retained == &reading)
    );
    assert!(reading.buckets[0].windows[0].is_expired(CAPTURED + 300_000));
    assert_eq!(reading.buckets[0].windows[0].remaining_percent, Some(75.0));
    let mut age_stale = QuotaProjection::new(scope);
    age_stale.record(reading).unwrap();
    age_stale.mark_stale(CAPTURED + 100, 100);
    assert!(matches!(
        age_stale.state(),
        QuotaState::Stale { fault: None, .. }
    ));
}

#[test]
fn successful_absence_clears_prior_values_and_failure_without_reading_is_error() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let mut projection = QuotaProjection::new(scope);
    projection.fail(QuotaFault::TimedOut, CAPTURED).unwrap();
    assert_eq!(projection.state(), &QuotaState::Error(QuotaFault::TimedOut));
    let reading = parse_codex_rate_limits(
        include_bytes!("fixtures/quota/codex-plus.json"),
        scope,
        CAPTURED + 1,
    )
    .unwrap();
    projection.record(reading).unwrap();
    let absent = parse_codex_rate_limits(br#"{"rateLimits":null}"#, scope, CAPTURED + 2).unwrap();
    projection.record(absent).unwrap();
    assert_eq!(
        projection.state(),
        &QuotaState::Unavailable(QuotaUnavailable::NotReported)
    );
    assert_eq!(projection.last_attempt_at_ms(), Some(CAPTURED + 2));
}

#[test]
fn empty_multi_bucket_is_authoritative_and_null_uses_legacy() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let empty = parse_codex_rate_limits(
        br#"{"rateLimitsByLimitId":{},"rateLimits":{"primary":{"usedPercent":50}}}"#,
        scope,
        CAPTURED,
    )
    .unwrap();
    assert!(empty.buckets.is_empty());
    let legacy = parse_codex_rate_limits(
        br#"{"rateLimitsByLimitId":null,"rateLimits":{"primary":{"usedPercent":50}}}"#,
        scope,
        CAPTURED,
    )
    .unwrap();
    assert_eq!(legacy.buckets.len(), 1);
    assert_eq!(legacy.buckets[0].windows[0].remaining_percent, Some(50.0));
}

#[test]
fn provider_window_changes_override_plan_examples_and_unknown_plan_text_is_discarded() {
    let scope = provenance(QuotaSource::CodexAppServer);
    for (plan, expected) in [
        ("pro", QuotaPlan::Pro),
        ("plus", QuotaPlan::Plus),
        ("synthetic-secret", QuotaPlan::Other),
    ] {
        let input = format!(
            r#"{{"rateLimits":{{"planType":"{plan}","primary":{{"usedPercent":12,"windowDurationMins":45}}}}}}"#
        );
        let reading = parse_codex_rate_limits(input.as_bytes(), scope, CAPTURED).unwrap();
        assert_eq!(reading.buckets[0].plan, Some(expected));
        assert_eq!(reading.buckets[0].windows.len(), 1);
        assert_eq!(reading.buckets[0].windows[0].duration_minutes, Some(45));
        assert!(!format!("{reading:?}").contains("synthetic-secret"));
    }
}

#[test]
fn extra_credentials_account_details_and_labels_are_not_projected() {
    let reading = parse_codex_rate_limits(br#"{"access_token":"synthetic-secret","account":{"email":"synthetic-secret"},"rateLimits":{"limitName":"synthetic-secret","primary":{"usedPercent":50,"token":"synthetic-secret"}}}"#, provenance(QuotaSource::CodexAppServer), CAPTURED).unwrap();
    assert!(!format!("{reading:?}").contains("synthetic-secret"));
    let claude = parse_claude_statusline(br#"{"apiKey":"synthetic-secret","session_id":"synthetic-secret","rate_limits":{"five_hour":{"used_percentage":50}}}"#, provenance(QuotaSource::ClaudeCodeStatusLine), CAPTURED).unwrap();
    assert!(!format!("{claude:?}").contains("synthetic-secret"));
}

#[test]
fn malformed_bucket_identity_and_shape_fail_closed() {
    for bytes in [
        br#"{"rateLimitsByLimitId":[]}"#.as_slice(),
        br#"{"rateLimitsByLimitId":{"codex":null}}"#,
        br#"{"rateLimitsByLimitId":{"codex":{"limitId":"other"}}}"#,
        br#"{"rateLimitsByLimitId":{"bad\nkey":{}}}"#,
        br#"{"rateLimits":{"limitId":""}}"#,
        br#"{"rateLimits":{"limitId":true}}"#,
        br#"{"rateLimits":{"planType":123}}"#,
        br#"{"rateLimits":{"primary":[]}}"#,
    ] {
        assert_eq!(
            parse_codex_rate_limits(bytes, provenance(QuotaSource::CodexAppServer), CAPTURED),
            Err(QuotaFault::InvalidShape)
        );
    }
    let mut buckets = serde_json::Map::new();
    for index in 0..33 {
        buckets.insert(format!("bucket{index}"), serde_json::json!({}));
    }
    let input = serde_json::json!({"rateLimitsByLimitId": buckets}).to_string();
    assert_eq!(
        parse_codex_rate_limits(
            input.as_bytes(),
            provenance(QuotaSource::CodexAppServer),
            CAPTURED
        ),
        Err(QuotaFault::InvalidShape)
    );
}

#[test]
fn invalid_gateway_values_are_not_silently_clamped_or_counted_as_quota() {
    for (field, value, fault) in [
        ("used_percentage", "-1", QuotaFault::InvalidPercentage),
        (
            "used_percentage",
            "\"Infinity\"",
            QuotaFault::InvalidPercentage,
        ),
        ("used_usd", "-0.01", QuotaFault::InvalidAmount),
        ("limit_usd", "\"NaN\"", QuotaFault::InvalidAmount),
        (
            "resets_at",
            "18446744073709551615",
            QuotaFault::InvalidReset,
        ),
        ("resets_at", "-1", QuotaFault::InvalidReset),
        ("period", "false", QuotaFault::InvalidShape),
    ] {
        let input = format!(r#"{{"rate_limits":{{"spend_limit":{{"{field}":{value}}}}}}}"#);
        assert_eq!(
            parse_claude_statusline(
                input.as_bytes(),
                provenance(QuotaSource::ClaudeCodeStatusLine),
                CAPTURED
            ),
            Err(fault)
        );
    }
    let reading = parse_claude_statusline(
        br#"{"rate_limits":{"spend_limit":{"used_percentage":125}}}"#,
        provenance(QuotaSource::ClaudeCodeStatusLine),
        CAPTURED,
    )
    .unwrap();
    assert!(reading.buckets.is_empty());
    assert_eq!(reading.gateway_spend.unwrap().used_percent, Some(125.0));
}

#[test]
fn timestamps_are_bounded_integers_in_milliseconds_and_zero_is_expired_not_unlimited() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let reading = parse_codex_rate_limits(
        br#"{"rateLimits":{"primary":{"resetsAt":0}}}"#,
        scope,
        CAPTURED,
    )
    .unwrap();
    let window = &reading.buckets[0].windows[0];
    assert_eq!(window.resets_at_ms, Some(0));
    assert!(window.is_expired(CAPTURED));
    assert_eq!(window.remaining_percent, None);
    assert_eq!(
        parse_codex_rate_limits(br#"{"rateLimits":null}"#, scope, u64::MAX),
        Err(QuotaFault::InvalidTimestamp)
    );
    assert_eq!(
        parse_codex_rate_limits(
            br#"{"rateLimits":{"primary":{"resetsAt":8640000000001}}}"#,
            scope,
            CAPTURED
        ),
        Err(QuotaFault::InvalidReset)
    );
    assert_eq!(
        parse_codex_rate_limits(
            br#"{"rateLimits":{"primary":{"windowDurationMins":18446744073709551615}}}"#,
            scope,
            CAPTURED
        ),
        Err(QuotaFault::InvalidDuration)
    );
}

#[test]
fn retired_observer_rejects_late_success_and_failure_without_repopulating_data() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let reading = parse_codex_rate_limits(
        include_bytes!("fixtures/quota/codex-plus.json"),
        scope,
        CAPTURED,
    )
    .unwrap();
    let mut projection = QuotaProjection::new(scope);
    projection.make_unavailable(QuotaUnavailable::SignedOut);
    assert_eq!(projection.record(reading), Err(QuotaFault::Retired));
    assert_eq!(
        projection.fail(QuotaFault::Transport, CAPTURED),
        Err(QuotaFault::Retired)
    );
    assert_eq!(
        projection.state(),
        &QuotaState::Unavailable(QuotaUnavailable::SignedOut)
    );
    assert_eq!(projection.provenance(), scope);
}

#[test]
fn fresh_reading_clears_stale_failure_and_clock_regression_is_not_freshness() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let reading = parse_codex_rate_limits(
        include_bytes!("fixtures/quota/codex-plus.json"),
        scope,
        CAPTURED,
    )
    .unwrap();
    let mut projection = QuotaProjection::new(scope);
    projection.record(reading.clone()).unwrap();
    projection
        .fail(QuotaFault::Transport, CAPTURED + 1)
        .unwrap();
    projection.fail(QuotaFault::TimedOut, CAPTURED + 2).unwrap();
    assert!(
        matches!(projection.state(), QuotaState::Stale { reading: retained, fault: Some(QuotaFault::TimedOut) } if retained.captured_at_ms == CAPTURED)
    );
    let mut fresh = reading;
    fresh.captured_at_ms += 3;
    projection.record(fresh).unwrap();
    assert!(matches!(projection.state(), QuotaState::Available(_)));
    projection.mark_stale(CAPTURED - 1, 1000);
    assert!(matches!(
        projection.state(),
        QuotaState::Stale { fault: None, .. }
    ));
}

#[test]
fn an_already_expired_observation_is_stale_at_capture_without_a_timer() {
    let scope = provenance(QuotaSource::CodexAppServer);
    let reading = parse_codex_rate_limits(
        br#"{"rateLimits":{"primary":{"usedPercent":100,"resetsAt":0}}}"#,
        scope,
        CAPTURED,
    )
    .unwrap();
    let mut projection = QuotaProjection::new(scope);
    projection.record(reading).unwrap();
    assert!(
        matches!(projection.state(), QuotaState::Stale { reading, fault: None } if reading.buckets[0].windows[0].remaining_percent == Some(0.0))
    );
}
