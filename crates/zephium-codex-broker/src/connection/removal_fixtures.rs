//! Fixed synthetic removal transcripts. These never execute original account
//! calls or product erasure; all state belongs to a newly created fixture root.
use super::fixtures::MemoryAssociations;
use super::*;
use crate::profile::PreparedRemoval;

#[derive(Clone, Copy)]
pub(crate) enum Scenario {
    SignedOut,
    PendingQuota,
    PendingCatalog,
    MissingNotification,
    InvalidatedRead,
    LateInvalidation,
    NonzeroExit,
    CancelDuringFinish,
}
impl Scenario {
    pub(crate) fn args(self) -> &'static [&'static str] {
        match self {
            Self::SignedOut => &["transport-child", "removal-signed-out"],
            Self::PendingCatalog => &["transport-child", "removal-pending-catalog"],
            Self::PendingQuota => &["transport-child", "removal-pending-quota"],
            Self::MissingNotification => &["transport-child", "removal-no-notification"],
            Self::InvalidatedRead => &["transport-child", "removal-invalidated-read"],
            Self::LateInvalidation => &["transport-child", "removal-late-invalidation"],
            Self::NonzeroExit => &["transport-child", "removal-nonzero"],
            Self::CancelDuringFinish => &["transport-child", "removal-cancel-finish"],
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "removal-signed-out" => Some(Self::SignedOut),
            "removal-pending-catalog" => Some(Self::PendingCatalog),
            "removal-pending-quota" => Some(Self::PendingQuota),
            "removal-no-notification" => Some(Self::MissingNotification),
            "removal-invalidated-read" => Some(Self::InvalidatedRead),
            "removal-late-invalidation" => Some(Self::LateInvalidation),
            "removal-nonzero" => Some(Self::NonzeroExit),
            "removal-cancel-finish" => Some(Self::CancelDuringFinish),
            _ => None,
        }
    }
}
pub(crate) fn qualify() -> Result<(), &'static str> {
    for (scenario, marker) in [
        (
            Scenario::PendingCatalog,
            "codex-removal-pending-catalog-drained-no-pagination",
        ),
        (
            Scenario::PendingQuota,
            "codex-removal-pending-quota-drained-before-settlement",
        ),
        (
            Scenario::SignedOut,
            "codex-removal-signedout-still-logout-same-lease",
        ),
        (
            Scenario::MissingNotification,
            "codex-removal-ack-without-notification-refused",
        ),
        (
            Scenario::InvalidatedRead,
            "codex-removal-inflight-invalidation-refused",
        ),
        (
            Scenario::LateInvalidation,
            "codex-removal-late-invalidation-refused",
        ),
        (Scenario::NonzeroExit, "codex-removal-nonzero-exit-refused"),
        (
            Scenario::CancelDuringFinish,
            "codex-removal-cancel-during-finish-refused",
        ),
    ] {
        run(scenario)?;
        println!("{marker}: native-code=0");
    }
    Ok(())
}
fn run(scenario: Scenario) -> Result<(), &'static str> {
    let parent = tempfile::Builder::new()
        .prefix("Zephium.RemovalOwnerFixture.")
        .tempdir()
        .map_err(|_| "removal-fixture-parent")?;
    let profile = ProfileId::from_native([51; 16]).ok_or("removal-fixture-profile")?;
    let lease = ProfileLease::fixture_protected(parent.path(), profile)
        .map_err(|_| "removal-fixture-lease")?;
    let incarnation = lease.record.incarnation;
    let (session, runtime) = windows::fixtures::removal_session(scenario, lease)?;
    let mut associations = MemoryAssociations::default();
    let store_scope = match associations.call(durable::Call::Open { incarnation }) {
        durable::Reply::Scope(value) => value,
        _ => return Err("removal-fixture-store"),
    };
    let scope = Scope::new(
        format!("{}-{}", profile.leaf(), hex(&incarnation)),
        [9; 16],
        1,
    )
    .map_err(|_| "removal-fixture-scope")?;
    let stop = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(20);
    let cancelled = Arc::new(AtomicBool::new(false));
    let generation = Arc::new(AtomicU64::new(1));
    let admission = RemovalAdmission::from_native(cancelled.clone(), generation, 1)
        .ok_or("removal-fixture-admission")?;
    let mut owner = Connection::bootstrap_session(
        scope.clone(),
        session,
        runtime.clone(),
        StoreBinding {
            associations: Box::new(associations),
            scope: store_scope,
        },
        ConfigProjection::V164Plus,
        NativePreparation {
            budget: PreparationBudget::removal(Instant::now(), deadline),
            removal: if matches!(scenario, Scenario::PendingQuota | Scenario::PendingCatalog) {
                None
            } else {
                Some(
                    removal::AccountPreparation::new(&admission, deadline)
                        .map_err(|_| "removal-fixture-setup")?,
                )
            },
            existing_owner: Some(store_scope),
        },
        &stop,
    )
    .map_err(|_| "removal-fixture-bootstrap")?;
    // The initial read is still pending here. Queue/bootstrap work must not
    // renew this removal obligation's original absolute limit.
    if owner.account_deadline != Some(deadline)
        || !owner.pending.values().any(|pending| {
            matches!(pending.kind, PendingKind::Account) && pending.deadline == deadline
        })
    {
        return Err("removal-fixture-startup-deadline-renewed");
    }
    let finish_directory = runtime.temp().to_owned();
    // This host must not retain an extra runtime Arc during consuming settlement.
    drop(runtime);
    let initial = if matches!(scenario, Scenario::PendingQuota | Scenario::PendingCatalog) {
        AccountState::ChatGpt
    } else {
        AccountState::SignedOut
    };
    while owner.state() != initial {
        if Instant::now() >= deadline {
            return Err("removal-fixture-initial-timeout");
        }
        owner
            .poll(&scope, &stop)
            .map_err(|_| "removal-fixture-initial-poll")?;
        std::thread::sleep(Duration::from_millis(5));
    }
    let old_quota_epoch = owner
        .quota(&scope)
        .map_err(|_| "removal-fixture-quota-scope")?
        .account_revision;

    if matches!(scenario, Scenario::PendingCatalog) {
        owner
            .refresh_catalog(&scope, &stop)
            .map_err(|_| "removal-fixture-catalog-start")?;
        owner
            .attach_removal_preparation(&scope, &admission, deadline)
            .map_err(|_| "removal-fixture-catalog-attach")?;
        if owner.can_begin_removal(&scope)
            || owner.removal.is_some()
            || owner.association_scope != store_scope
        {
            return Err("removal-fixture-catalog-early-retirement");
        }
        while !owner.can_begin_removal(&scope) {
            if Instant::now() >= deadline {
                return Err("removal-fixture-catalog-drain-timeout");
            }
            let events = owner
                .poll(&scope, &stop)
                .map_err(|_| "removal-fixture-catalog-drain")?;
            if events
                .iter()
                .any(|event| matches!(event.event, Event::Catalog(_)))
            {
                return Err("removal-fixture-catalog-stale-admission");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if owner.catalog.is_some() || owner.catalog_builder.is_some() {
            return Err("removal-fixture-catalog-not-retired");
        }
    }
    let attempt = RemovalAttemptId::fresh_native().map_err(|_| "removal-fixture-attempt")?;
    if !owner.can_begin_removal(&scope) {
        return Err("removal-fixture-initial-preflight");
    }
    owner
        .begin_removal(&scope, attempt, admission, deadline)
        .map_err(|_| "removal-fixture-begin")?;
    if owner.can_begin_removal(&scope) {
        return Err("removal-fixture-repeated-begin");
    }
    if owner.association_scope.grant_epoch != store_scope.grant_epoch + 1 {
        return Err("removal-fixture-grant-ack");
    }
    let mut poll_failed = false;
    let mut quota_drain_observed = false;
    loop {
        if matches!(scenario, Scenario::PendingQuota)
            && owner.removal_observation() == CredentialObservation::SignedOutObservedAfterLogout
            && owner
                .pending
                .values()
                .any(|p| matches!(p.kind, PendingKind::Quota(_)))
            && !quota_drain_observed
        {
            if owner.removal_ready(&scope) {
                return Err("removal-fixture-early-quota-settlement");
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(finish_directory.join("removal-quota-drain-observed"))
                .map_err(|_| "removal-fixture-quota-barrier")?;
            quota_drain_observed = true;
        }
        if owner.removal_ready(&scope) {
            if matches!(scenario, Scenario::PendingQuota) {
                let view = owner
                    .quota(&scope)
                    .map_err(|_| "removal-fixture-quota-scope")?;
                if view.account_revision <= old_quota_epoch
                    || view.checking
                    || view.state
                        != zephium_provider_accounts::quota::QuotaState::Unavailable(
                            zephium_provider_accounts::quota::QuotaUnavailable::SignedOut,
                        )
                {
                    return Err("removal-fixture-old-quota-authority");
                }
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err("removal-fixture-protocol-timeout");
        }
        if matches!(scenario, Scenario::MissingNotification)
            && owner.removal_observation() == CredentialObservation::LogoutAcknowledged
        {
            // Exact elapsed-deadline control, not a production timeout change.
            owner
                .removal
                .as_mut()
                .ok_or("removal-fixture-intent")?
                .deadline = Instant::now();
        }
        match owner.poll(&scope, &stop) {
            Ok(_) => (),
            Err(Fault::Timeout) if matches!(scenario, Scenario::MissingNotification) => {
                poll_failed = true;
                break;
            }
            Err(Fault::Protocol) if matches!(scenario, Scenario::InvalidatedRead) => {
                poll_failed = true;
                break;
            }
            Err(_) => return Err("removal-fixture-protocol"),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let cancel_thread = matches!(scenario, Scenario::CancelDuringFinish).then(|| {
        std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(10);
            while !finish_directory.join("removal-finish-started").exists() {
                if Instant::now() >= until {
                    return Err("removal-fixture-finish-barrier");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            cancelled.store(true, Ordering::Release);
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(finish_directory.join("removal-finish-cancelled"))
                .map_err(|_| "removal-fixture-cancel-barrier")?;
            Ok(())
        })
    });
    let result = owner.prepare_removal();
    if let Some(thread) = cancel_thread {
        thread.join().map_err(|_| "removal-fixture-cancel-join")??;
    }
    match (scenario, result) {
        (Scenario::SignedOut | Scenario::PendingQuota | Scenario::PendingCatalog, Ok(settled)) => {
            if matches!(scenario, Scenario::PendingQuota) && !quota_drain_observed {
                return Err("removal-fixture-quota-not-drained");
            }
            if settled.observation() != CredentialObservation::SignedOutObservedAfterLogout {
                return Err("removal-fixture-observation");
            }
            let prepared =
                PreparedRemoval::from_settled(settled).map_err(|_| "removal-fixture-prepared")?;
            drop(prepared);
        }
        (Scenario::SignedOut | Scenario::PendingQuota | Scenario::PendingCatalog, Err(_))
        | (_, Ok(_)) => return Err("removal-fixture-false-capability"),
        (_, Err(mut failure)) => {
            if failure.held_intent().is_none() {
                return Err("removal-fixture-lost-custody");
            }
            if matches!(
                scenario,
                Scenario::MissingNotification | Scenario::InvalidatedRead
            ) && (!poll_failed
                || failure.observation() == CredentialObservation::SignedOutObservedAfterLogout)
            {
                return Err("removal-fixture-false-observation");
            }
            if matches!(scenario, Scenario::LateInvalidation | Scenario::NonzeroExit)
                && (failure.fault() != Fault::Transport
                    || failure.observation() != CredentialObservation::SignedOutObservedAfterLogout)
            {
                return Err("removal-fixture-settlement-disposition");
            }
            if matches!(scenario, Scenario::CancelDuringFinish)
                && (failure.fault() != Fault::Cancelled
                    || failure.observation() != CredentialObservation::SignedOutObservedAfterLogout)
            {
                return Err("removal-fixture-cancel-disposition");
            }
            drop(failure);
        }
    }
    if !matches!(
        ProfileLease::fixture(&parent.path().join("removal-owned-parent"), profile),
        Err(crate::profile::native::Error::RecoveryRequired)
    ) {
        return Err("removal-fixture-intent-not-retained");
    }
    // Test-root disposal only after all children/pins have settled and dropped.
    // This is not evidence for the separately reviewed product erasure API.
    parent.close().map_err(|_| "removal-fixture-dispose")
}
