use super::*;

fn scope(connection: u8) -> Scope {
    Scope::new("native-profile".into(), [connection; 16], 7).unwrap()
}
fn provenance() -> QuotaProvenance {
    QuotaProvenance {
        source: QuotaSource::CodexAppServer,
        profile_revision: 7,
        account_revision: 11,
    }
}
fn observer() -> AccountQuota {
    AccountQuota::new(scope(1), provenance()).unwrap()
}
fn bucket(id: &str, used: f64) -> Value {
    json!({"limitId":id,"primary":{"usedPercent":used,"windowDurationMins":300,"resetsAt":1000},"secondary":null,"planType":"plus"})
}
fn reading() -> Value {
    json!({"rateLimits":bucket("codex",10.0),"rateLimitsByLimitId":{"codex":bucket("codex",10.0),"other":bucket("other",20.0)},"ordinaryUsageAllowed":false,"accountId":"synthetic-not-retained","credits":{"balance":"not-quota"}})
}
fn complete(q: &mut AccountQuota, at: Instant, ms: u64, value: &Value) {
    let t = q.begin_read(&scope(1), 11, at).unwrap();
    assert_eq!(
        q.complete_read(&scope(1), 11, t, value, ms),
        Ok(Applied::Current)
    );
}
fn data(q: &AccountQuota) -> &zephium_provider_accounts::quota::QuotaReading {
    match q.state() {
        QuotaState::Available(r) | QuotaState::Stale { reading: r, .. } => r,
        _ => panic!("expected reading"),
    }
}

#[test]
fn fixed_read_has_no_reserve_fallback_or_redemption() {
    assert_eq!(
        AccountQuota::read_params(),
        json!({"supportsLunaReserve":false,"excludeResetCreditDetails":true})
    );
}
#[test]
fn full_multi_bucket_preserves_native_provenance_and_optional_windows() {
    let mut q = observer();
    complete(&mut q, Instant::now(), 100, &reading());
    let r = data(&q);
    assert_eq!(r.provenance, provenance());
    assert_eq!(r.buckets.len(), 2);
    assert_eq!(r.buckets[0].id.as_deref(), Some("codex"));
    assert_eq!(r.buckets[0].windows.len(), 1);
    assert_eq!(r.buckets[0].windows[0].remaining_percent, Some(90.0));
    assert_eq!(r.buckets[0].windows[0].duration_minutes, Some(300));
    assert!(r.gateway_spend.is_none());
    assert!(!q.needs_refresh());
}
#[test]
fn optional_window_values_are_unknown_not_zero() {
    let mut q = observer();
    complete(
        &mut q,
        Instant::now(),
        100,
        &json!({"rateLimits":{"primary":{"usedPercent":null,"windowDurationMins":null,"resetsAt":null}}}),
    );
    let w = &data(&q).buckets[0].windows[0];
    assert_eq!(w.used_percent, None);
    assert_eq!(w.remaining_percent, None);
    assert_eq!(w.resets_at_ms, None);
}
#[test]
fn explicit_zero_is_reported_and_empty_map_is_authoritative_absence() {
    let now = Instant::now();
    let mut q = observer();
    complete(&mut q, now, 100, &json!({"rateLimits":bucket("codex",0.0)}));
    assert_eq!(
        data(&q).buckets[0].windows[0].remaining_percent,
        Some(100.0)
    );
    complete(
        &mut q,
        now + MIN_READ_INTERVAL,
        200,
        &json!({"rateLimits":bucket("codex",0.0),"rateLimitsByLimitId":{}}),
    );
    assert_eq!(
        q.state(),
        &QuotaState::Unavailable(QuotaUnavailable::NotReported)
    );
}
#[test]
fn sparse_update_marks_stale_without_erasing_buckets_plan_or_capture_time() {
    let mut q = observer();
    complete(&mut q, Instant::now(), 100, &reading());
    let previous = data(&q).clone();
    assert_eq!(
        q.updated(
            &scope(1),
            11,
            &json!({"rateLimits":{"limitId":"codex","primary":null,"planType":null}}),
            120
        ),
        Ok(true)
    );
    assert!(matches!(q.state(), QuotaState::Stale { fault: None, .. }));
    assert_eq!(data(&q), &previous);
    assert_eq!(
        q.updated(&scope(1), 11, &json!({"rateLimits":{}}), 130),
        Ok(false)
    );
    assert_eq!(data(&q).captured_at_ms, 100);
}
#[test]
fn notification_during_pending_read_supersedes_completion() {
    let now = Instant::now();
    let mut q = observer();
    complete(&mut q, now, 100, &reading());
    let ticket = q
        .begin_read(&scope(1), 11, now + MIN_READ_INTERVAL)
        .unwrap();
    q.updated(
        &scope(1),
        11,
        &json!({"rateLimits":bucket("codex",80.0)}),
        150,
    )
    .unwrap();
    assert_eq!(
        q.complete_read(
            &scope(1),
            11,
            ticket,
            &json!({"rateLimits":bucket("codex",30.0)}),
            160
        ),
        Ok(Applied::Superseded)
    );
    assert_eq!(data(&q).buckets[0].windows[0].used_percent, Some(10.0));
    assert!(q.needs_refresh());
    complete(
        &mut q,
        now + MIN_READ_INTERVAL * 2,
        200,
        &json!({"rateLimits":bucket("codex",81.0)}),
    );
    assert_eq!(data(&q).buckets[0].windows[0].used_percent, Some(81.0));
    assert!(!q.needs_refresh());
}
#[test]
fn storms_cannot_bypass_cadence_and_do_not_start_io() {
    let now = Instant::now();
    let mut q = observer();
    complete(&mut q, now, 100, &reading());
    for n in 0..100 {
        q.updated(&scope(1), 11, &json!({"rateLimits":{}}), 120 + n)
            .unwrap();
    }
    assert!(matches!(
        q.begin_read(&scope(1), 11, now + Duration::from_secs(29)),
        Err(Fault::Cadence)
    ));
    let ticket = q
        .begin_read(&scope(1), 11, now + MIN_READ_INTERVAL)
        .unwrap();
    assert!(matches!(
        q.begin_read(&scope(1), 11, now + MIN_READ_INTERVAL * 2),
        Err(Fault::Busy)
    ));
    assert_eq!(
        q.fail_read(&scope(1), 11, ticket, QuotaFault::Transport, 300),
        Ok(Applied::Current)
    );
    assert!(matches!(
        q.state(),
        QuotaState::Stale {
            fault: Some(QuotaFault::Transport),
            ..
        }
    ));
}
#[test]
fn observation_attempt_budget_is_fixed_per_scope() {
    let now = Instant::now();
    let mut q = observer();
    for n in 0..u32::from(MAX_READS_PER_SCOPE) {
        let t = q
            .begin_read(&scope(1), 11, now + MIN_READ_INTERVAL * n)
            .unwrap();
        q.fail_read(&scope(1), 11, t, QuotaFault::Transport, u64::from(n))
            .unwrap();
    }
    assert!(matches!(
        q.begin_read(
            &scope(1),
            11,
            now + MIN_READ_INTERVAL * u32::from(MAX_READS_PER_SCOPE)
        ),
        Err(Fault::ReadBudget)
    ));
}
#[test]
fn replaced_scope_or_epoch_cannot_apply_late_values() {
    let mut q = observer();
    let t = q.begin_read(&scope(1), 11, Instant::now()).unwrap();
    assert_eq!(
        q.complete_read(&scope(2), 11, t, &reading(), 100),
        Err(Fault::ScopeChanged)
    );
    assert!(matches!(
        q.begin_read(&scope(1), 12, Instant::now()),
        Err(Fault::ScopeChanged)
    ));
}
#[test]
fn same_metadata_new_observer_cannot_adopt_old_ticket() {
    let mut old = observer();
    let now = Instant::now();
    let old_t = old.begin_read(&scope(1), 11, now).unwrap();
    let mut new = observer();
    let new_t = new.begin_read(&scope(1), 11, now).unwrap();
    assert_eq!(
        new.complete_read(&scope(1), 11, old_t, &reading(), 100),
        Err(Fault::Ticket)
    );
    assert_eq!(
        new.complete_read(&scope(1), 11, new_t, &reading(), 100),
        Ok(Applied::Current)
    );
}
#[test]
fn account_loss_clears_values_and_rejects_delayed_read_and_updates() {
    let mut q = observer();
    let now = Instant::now();
    complete(&mut q, now, 100, &reading());
    let t = q
        .begin_read(&scope(1), 11, now + MIN_READ_INTERVAL)
        .unwrap();
    q.retire(QuotaUnavailable::SignedOut);
    assert_eq!(
        q.state(),
        &QuotaState::Unavailable(QuotaUnavailable::SignedOut)
    );
    assert!(!q.needs_refresh());
    assert_eq!(
        q.complete_read(&scope(1), 11, t, &reading(), 200),
        Err(Fault::Retired)
    );
    assert_eq!(
        q.updated(&scope(1), 11, &json!({"rateLimits":{}}), 201),
        Err(Fault::Retired)
    );
}
#[test]
fn transient_and_malformed_observation_preserve_last_good_as_stale() {
    let now = Instant::now();
    let mut q = observer();
    complete(&mut q, now, 100, &reading());
    let t = q
        .begin_read(&scope(1), 11, now + MIN_READ_INTERVAL)
        .unwrap();
    assert_eq!(
        q.complete_read(&scope(1), 11, t, &json!({"rateLimits":null}), 200),
        Err(Fault::Observation(QuotaFault::InvalidShape))
    );
    assert_eq!(data(&q).captured_at_ms, 100);
    assert!(matches!(q.state(), QuotaState::Stale { .. }));
    assert_eq!(
        q.updated(
            &scope(1),
            11,
            &json!({"rateLimits":{"primary":{"usedPercent":101}}}),
            210
        ),
        Err(Fault::Observation(QuotaFault::InvalidPercentage))
    );
}
#[test]
fn sparse_additive_map_never_authorizes_bucket_replacement() {
    let mut q = observer();
    complete(&mut q, Instant::now(), 100, &reading());
    q.updated(
        &scope(1),
        11,
        &json!({"rateLimits":{},"rateLimitsByLimitId":{}}),
        200,
    )
    .unwrap();
    assert_eq!(data(&q).buckets.len(), 2);
    assert_eq!(data(&q).captured_at_ms, 100);
}
#[test]
fn age_and_reset_staleness_never_refill_locally() {
    let mut q = observer();
    complete(&mut q, Instant::now(), 100, &reading());
    q.mark_stale(200, 50);
    assert!(matches!(q.state(), QuotaState::Stale { .. }));
    assert_eq!(data(&q).buckets[0].windows[0].remaining_percent, Some(90.0));
}
#[test]
fn source_and_zero_revisions_are_not_admitted() {
    let mut p = provenance();
    p.source = QuotaSource::DirectChatGpt;
    assert!(matches!(
        AccountQuota::new(scope(1), p),
        Err(Fault::ScopeChanged)
    ));
    p = provenance();
    p.account_revision = 0;
    assert!(matches!(
        AccountQuota::new(scope(1), p),
        Err(Fault::ScopeChanged)
    ));
}

#[test]
fn backward_observation_clock_cannot_leave_last_good_falsely_fresh() {
    let now = Instant::now();
    let mut q = observer();
    complete(&mut q, now, 100, &reading());
    let ticket = q
        .begin_read(&scope(1), 11, now + MIN_READ_INTERVAL)
        .unwrap();
    assert_eq!(
        q.complete_read(&scope(1), 11, ticket, &reading(), 99),
        Err(Fault::Observation(QuotaFault::OlderObservation))
    );
    assert!(matches!(q.state(), QuotaState::Stale { .. }));
    assert_eq!(data(&q).captured_at_ms, 100);
    assert!(q.needs_refresh());
}

#[test]
fn manual_refresh_is_not_a_sparse_update_or_cadence_reset() {
    let now = Instant::now();
    let mut q = observer();
    complete(&mut q, now, 100, &reading());
    q.request_refresh(&scope(1), 11).unwrap();
    assert!(q.needs_refresh());
    assert!(matches!(q.state(), QuotaState::Available(_)));
    assert!(matches!(
        q.begin_read(&scope(1), 11, now + Duration::from_secs(29)),
        Err(Fault::Cadence)
    ));
    let ticket = q
        .begin_read(&scope(1), 11, now + MIN_READ_INTERVAL)
        .unwrap();
    q.request_refresh(&scope(1), 11).unwrap();
    assert_eq!(
        q.complete_read(&scope(1), 11, ticket, &reading(), 200),
        Ok(Applied::Current)
    );
    assert!(!q.needs_refresh());
    q.retire(QuotaUnavailable::SignedOut);
    assert_eq!(q.request_refresh(&scope(1), 11), Err(Fault::Retired));
}
