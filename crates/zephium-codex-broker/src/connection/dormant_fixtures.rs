//! Fixed synthetic physical owner controls. These execute only the copied
//! fixture helper, never an original provider or a Page/browser effect.
use super::*;
use crate::connection::{fixture_peer, fixtures::MemoryAssociations};
use std::path::Path;
#[derive(Clone, Copy)]
pub(crate) enum Scenario {
    First,
    Resume,
    Idle,
    WakeReadInvalid,
    Nonzero,
    LateFrame,
    Blocked,
    PendingQuota,
    QuotaNoReply,
    QuotaDuplicate,
    QuotaForeign,
    QuotaAccount,
}
impl Scenario {
    pub(crate) fn args(self) -> &'static [&'static str] {
        match self {
            Self::First => &["transport-child", "dormant-first"],
            Self::Resume => &["transport-child", "dormant-resume"],
            Self::Idle => &["transport-child", "dormant-idle"],
            Self::WakeReadInvalid => &["transport-child", "dormant-wake-read-invalid"],
            Self::Nonzero => &["transport-child", "dormant-nonzero"],
            Self::LateFrame => &["transport-child", "dormant-late-frame"],
            Self::Blocked => &["transport-child", "dormant-blocked"],
            Self::PendingQuota => &["transport-child", "dormant-pending-quota"],
            Self::QuotaNoReply => &["transport-child", "dormant-quota-no-reply"],
            Self::QuotaDuplicate => &["transport-child", "dormant-quota-duplicate"],
            Self::QuotaForeign => &["transport-child", "dormant-quota-foreign"],
            Self::QuotaAccount => &["transport-child", "dormant-quota-account"],
        }
    }
    pub(crate) fn quota(self) -> bool {
        matches!(
            self,
            Self::PendingQuota
                | Self::QuotaNoReply
                | Self::QuotaDuplicate
                | Self::QuotaForeign
                | Self::QuotaAccount
        )
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "dormant-first" => Self::First,
            "dormant-resume" => Self::Resume,
            "dormant-idle" => Self::Idle,
            "dormant-wake-read-invalid" => Self::WakeReadInvalid,
            "dormant-nonzero" => Self::Nonzero,
            "dormant-late-frame" => Self::LateFrame,
            "dormant-blocked" => Self::Blocked,
            "dormant-pending-quota" => Self::PendingQuota,
            "dormant-quota-no-reply" => Self::QuotaNoReply,
            "dormant-quota-duplicate" => Self::QuotaDuplicate,
            "dormant-quota-foreign" => Self::QuotaForeign,
            "dormant-quota-account" => Self::QuotaAccount,
            _ => return None,
        })
    }
}
fn boot(
    lease: ProfileLease,
    package: &Path,
    scenario: Scenario,
    store: MemoryAssociations,
    canonical: durable::Scope,
    epoch: u64,
    wake: Option<&crate::profile::Record>,
) -> Result<Connection, &'static str> {
    let profile = lease.record.profile;
    if lease.record.incarnation != canonical.incarnation {
        return Err("dormant-lease-scope");
    }
    if wake.is_some_and(|expected| !crate::dormancy::same_home(expected, &lease.record)) {
        return Err("dormant-lease-record");
    }
    let (session, runtime) = windows::fixtures::dormant_session(scenario, lease, package)?;
    let capability = crate::browser_capability::BrowserCapability::fixture(&session)
        .map_err(|_| "dormant-fixture-capability")?;
    let scope = Scope::new(
        format!("{}-{}", profile.leaf(), hex(&canonical.incarnation)),
        [81; 16],
        epoch,
    )
    .map_err(|_| "dormant-scope")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let stop = Arc::new(AtomicBool::new(false));
    let budget = PreparationBudget::bounded(Instant::now(), deadline);
    let mut owner = Connection::bootstrap_session(
        scope,
        session,
        runtime,
        StoreBinding {
            associations: Box::new(store),
            scope: canonical,
        },
        ConfigProjection::V164Plus,
        NativePreparation {
            budget,
            removal: None,
            existing_owner: Some(canonical),
            expected_record: wake.cloned().map(Box::new),
            wake_deadline: wake.map(|_| deadline),
            wake_cancel: wake.map(|_| stop.clone()),
        },
        &stop,
    )
    .map_err(|_| "dormant-bootstrap")?;
    owner.browser_capability = Some(capability);
    Ok(owner)
}
fn drive(owner: &mut Connection, scenario: Scenario) -> Result<(), &'static str> {
    let scope = owner.scope().clone();
    let stop = AtomicBool::new(false);
    let until = Instant::now() + Duration::from_secs(20);
    let mut catalog = false;
    let mut started = false;
    let mut terminal = false;
    let mut closed = false;
    let turns = matches!(scenario, Scenario::First | Scenario::Resume);
    loop {
        if Instant::now() >= until {
            return Err("dormant-drive-timeout");
        }
        for event in owner
            .poll(&scope, &stop)
            .map_err(|_| "dormant-drive-poll")?
        {
            match event.into_event() {
                Event::Account(AccountState::ChatGpt) if turns && !catalog => {
                    catalog = true;
                    owner
                        .refresh_catalog(&scope, &stop)
                        .map_err(|_| "dormant-catalog")?;
                }
                Event::Catalog(_) if turns && !started => {
                    started = true;
                    let resumed = matches!(scenario, Scenario::Resume);
                    owner
                        .start_text(
                            &scope,
                            TextRequest {
                                admission: TextAdmission::from_native(
                                    Arc::new(AtomicBool::new(false)),
                                    Arc::new(AtomicU64::new(1)),
                                    1,
                                ),
                                reply: if resumed { 2 } else { 1 },
                                previous_reply: resumed.then_some(1),
                                key: ConversationKey::from_native("dormant-page".into())
                                    .ok_or("dormant-key")?,
                                new_conversation: !resumed,
                                model: "synthetic-picker".into(),
                                effort: EffortSelection::Explicit(crate::catalog::Effort::High),
                                text: if resumed {
                                    "synthetic second $skill_two"
                                } else {
                                    "synthetic first $skill_one"
                                }
                                .into(),
                            },
                            &stop,
                        )
                        .map_err(|_| "dormant-turn")?;
                }
                Event::Conversation(conversation::Event::Terminal(result)) => {
                    if result.outcome != conversation::Outcome::Completed
                        || result.thread_id != fixture_peer::THREAD
                    {
                        return Err("dormant-terminal");
                    }
                    terminal = true;
                }
                Event::Conversation(conversation::Event::Closed { thread_id, .. }) => {
                    if thread_id != fixture_peer::THREAD {
                        return Err("dormant-closed-thread");
                    }
                    closed = true;
                }
                _ => (),
            }
        }
        if scenario.quota()
            && owner
                .pending
                .values()
                .any(|p| matches!(p.kind, PendingKind::Quota(_)))
        {
            return Ok(());
        }
        if owner.ready_to_dormant(&scope) && (!turns || (terminal && closed)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn fixture(
    scenario: Scenario,
) -> Result<
    (
        tempfile::TempDir,
        Connection,
        MemoryAssociations,
        durable::Scope,
    ),
    &'static str,
> {
    let root = tempfile::Builder::new()
        .prefix("Zephium.DormantOwner.")
        .tempdir()
        .map_err(|_| "dormant-root")?;
    let profile = ProfileId::from_native([83; 16]).ok_or("dormant-profile")?;
    let lease =
        ProfileLease::fixture_protected(root.path(), profile).map_err(|_| "dormant-lease")?;
    let mut store = MemoryAssociations::default();
    let canonical = match store.call(durable::Call::Open {
        incarnation: lease.record.incarnation,
    }) {
        durable::Reply::Scope(scope) => scope,
        _ => return Err("dormant-store"),
    };
    let mut owner = boot(
        lease,
        &root.path().join("package-old"),
        scenario,
        store.clone(),
        canonical,
        1,
        None,
    )?;
    drive(&mut owner, scenario)?;
    Ok((root, owner, store, canonical))
}
fn denied(package: &Path, target: &Path) -> Result<(), &'static str> {
    if std::fs::rename(package, target)
        .err()
        .and_then(|e| e.raw_os_error())
        != Some(32)
    {
        return Err("dormant-live-package-not-held");
    }
    Ok(())
}
pub(crate) fn qualify() -> Result<(), &'static str> {
    let (root, owner, mut store, canonical) = fixture(Scenario::First)?;
    let scope = owner.scope().clone();
    let profile = owner
        .runtime
        .as_ref()
        .and_then(|r| r.persistent_identity())
        .ok_or("dormant-identity")?
        .0;
    let old = root.path().join("package-old");
    let moved = root.path().join("package-retired");
    denied(&old, &moved)?;
    let record = store.call(durable::Call::Load {
        scope: canonical,
        policy: durable::ConversationPolicy::TextOnlyLegacy,
        conversation: "dormant-page".into(),
    });
    let receipt = owner
        .try_dormant(&scope, Instant::now() + Duration::from_secs(10))
        .map_err(|_| "dormant-healthy-close")?;
    if receipt.profile() != profile || receipt.scope() != &scope {
        return Err("dormant-receipt-correlation");
    }
    std::fs::rename(&old, &moved).map_err(|_| "dormant-package-rename-after-receipt")?;
    if store.call(durable::Call::InspectOwner) != durable::Reply::Owner(Some(canonical))
        || store.call(durable::Call::Load {
            scope: canonical,
            policy: durable::ConversationPolicy::TextOnlyLegacy,
            conversation: "dormant-page".into(),
        }) != record
    {
        return Err("dormant-store-mutated");
    }
    // Same incarnation alone must not permit a replacement root/config/owner.
    // The shared record join runs before even the fixed helper package exists.
    for change in 0..4 {
        let lease = ProfileLease::fixture_existing_for_wake(
            root.path(),
            receipt.identity.profile,
            receipt.identity.canonical.incarnation,
        )
        .map_err(|_| "dormant-negative-existing-home")?;
        let mut previous = *receipt.identity.record.clone();
        match change {
            0 => previous.nodes[0].file[0] ^= 1,
            1 => previous.config[0] ^= 1,
            2 => previous.environments[0] ^= 1,
            _ => previous.owner[0] ^= 1,
        }
        let package = root.path().join("package-refused");
        if !matches!(
            boot(
                lease,
                &package,
                Scenario::Idle,
                store.clone(),
                canonical,
                2,
                Some(&previous),
            ),
            Err("dormant-lease-record")
        ) || package.exists()
        {
            return Err("dormant-changed-record-spawned");
        }
    }
    let lease = ProfileLease::fixture_existing_for_wake(
        root.path(),
        receipt.identity.profile,
        receipt.identity.canonical.incarnation,
    )
    .map_err(|_| "dormant-existing-wake")?;
    let mut failed = boot(
        lease,
        &root.path().join("package-wake-failed"),
        Scenario::WakeReadInvalid,
        store.clone(),
        receipt.identity.canonical,
        2,
        Some(&receipt.identity.record),
    )?;
    failed.wake_origin = Some(receipt.identity);
    let failed_scope = failed.scope().clone();
    let foreign = Scope::new("foreign".into(), [82; 16], 1).map_err(|_| "dormant-foreign")?;
    failed = match failed.into_failed_dormant_wake(&foreign, Fault::Protocol) {
        Err(owner) => *owner,
        Ok(_) => return Err("dormant-foreign-wake-promoted"),
    };
    let until = Instant::now() + Duration::from_secs(10);
    let failure = loop {
        if Instant::now() >= until {
            return Err("dormant-wake-failure-timeout");
        }
        if let Err(fault) = failed.poll(&failed_scope, &AtomicBool::new(false)) {
            failed = match failed.into_failed_dormant_wake(&foreign, fault) {
                Err(owner) => *owner,
                Ok(_) => return Err("dormant-closed-foreign-wake-promoted"),
            };
            break failed
                .into_failed_dormant_wake(&failed_scope, fault)
                .map_err(|_| "dormant-wake-failure-extraction")?;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if failure.profile() != profile || failure.scope() != &scope {
        return Err("dormant-failed-wake-correlation");
    }
    std::fs::rename(
        root.path().join("package-wake-failed"),
        root.path().join("package-wake-failed-retired"),
    )
    .map_err(|_| "dormant-failed-wake-pins")?;
    // The next fixture is an explicit retry, never an automatic model replay.
    let lease = ProfileLease::fixture_existing_for_wake(
        root.path(),
        failure.identity.profile,
        failure.identity.canonical.incarnation,
    )
    .map_err(|_| "dormant-retry-existing-wake")?;
    let mut second = boot(
        lease,
        &root.path().join("package-new"),
        Scenario::Resume,
        store.clone(),
        failure.identity.canonical,
        3,
        Some(&failure.identity.record),
    )?;
    second.wake_origin = Some(failure.identity);
    drive(&mut second, Scenario::Resume)?;
    if second.wake_origin.is_some() || second.wake_deadline.is_some() {
        return Err("dormant-success-retained-retry");
    }
    let second_scope = second.scope().clone();
    let final_receipt = second
        .try_dormant(&second_scope, Instant::now() + Duration::from_secs(10))
        .map_err(|_| "dormant-second-close")?;
    if final_receipt.identity.canonical != canonical {
        return Err("dormant-grant-changed");
    }
    drop(final_receipt);
    root.close().map_err(|_| "dormant-cleanup")?;
    println!("codex-dormant-package-release-same-thread-wake: native-code=0");
    for (scenario, marker) in [
        (Scenario::Nonzero, "codex-dormant-nonzero-not-healthy"),
        (Scenario::LateFrame, "codex-dormant-late-frame-not-healthy"),
        (Scenario::Blocked, "codex-dormant-forced-close-not-healthy"),
    ] {
        let (root, owner, _, _) = fixture(scenario)?;
        let scope = owner.scope().clone();
        let failure = match owner.try_dormant(&scope, Instant::now() + Duration::from_millis(250)) {
            Err(failure) => failure,
            Ok(_) => return Err("dormant-failed-promoted"),
        };
        let (kind, fault, owner) = failure.into_parts();
        if kind != DormantFailureKind::Failed || !owner.is_closed() {
            return Err("dormant-failed-custody");
        }
        let owner = match owner.into_failed_dormant_wake(&scope, fault) {
            Err(owner) => owner,
            Ok(_) => return Err("dormant-ordinary-owner-minted-wake"),
        };
        denied(
            &root.path().join("package-old"),
            &root.path().join("package-retired"),
        )?;
        drop(owner);
        std::fs::rename(
            root.path().join("package-old"),
            root.path().join("package-retired"),
        )
        .map_err(|_| "dormant-failed-drop-release")?;
        root.close().map_err(|_| "dormant-failed-cleanup")?;
        println!("{marker}: native-code=0");
    }
    for (scenario, marker, valid) in [
        (
            Scenario::PendingQuota,
            "codex-dormant-expired-quota-late-reply-inert",
            true,
        ),
        (
            Scenario::QuotaNoReply,
            "codex-dormant-expired-quota-absent-unknown",
            true,
        ),
        (
            Scenario::QuotaDuplicate,
            "codex-dormant-quota-duplicate-refused",
            false,
        ),
        (
            Scenario::QuotaForeign,
            "codex-dormant-quota-foreign-refused",
            false,
        ),
        (
            Scenario::QuotaAccount,
            "codex-dormant-quota-account-change-refused",
            false,
        ),
    ] {
        let (root, owner, _, _) = fixture(scenario)?;
        let scope = owner.scope().clone();
        // An active quota read is not covered by the expired-only drain.
        let failure = match owner.try_dormant(&scope, Instant::now() + Duration::from_secs(1)) {
            Err(f) => f,
            Ok(_) => return Err("dormant-active-quota-promoted"),
        };
        let (kind, _, owner) = failure.into_parts();
        if kind != DormantFailureKind::NotReady || owner.is_closed() {
            return Err("dormant-active-quota-owner-lost");
        }
        let mut owner = *owner;
        for pending in owner.pending.values_mut() {
            if matches!(pending.kind, PendingKind::Quota(_)) {
                pending.deadline = Instant::now();
            }
        }
        let _ = owner
            .poll(&scope, &AtomicBool::new(false))
            .map_err(|_| "dormant-quota-expiry")?;
        if !owner
            .pending
            .values()
            .any(|p| matches!(&p.kind,PendingKind::Quota(q) if q.ticket.is_none()))
        {
            return Err("dormant-quota-tombstone");
        }
        if matches!(
            owner.quota(&scope).map_err(|_| "dormant-quota-view")?.state,
            zephium_provider_accounts::quota::QuotaState::Available(_)
        ) {
            return Err("dormant-quota-false-success");
        }
        match owner.try_dormant(&scope, Instant::now() + Duration::from_secs(5)) {
            Ok(receipt) if valid => {
                if !receipt.quota_unknown() {
                    return Err("dormant-quota-unknown-lost");
                }
                drop(receipt);
            }
            Err(failure) if !valid => {
                let (kind, _, owner) = failure.into_parts();
                if kind != DormantFailureKind::Failed || !owner.is_closed() {
                    return Err("dormant-quota-failure-not-settled");
                }
                drop(owner);
            }
            _ => return Err("dormant-quota-drain-disposition"),
        }
        std::fs::rename(
            root.path().join("package-old"),
            root.path().join("package-retired"),
        )
        .map_err(|_| "dormant-quota-package-release")?;
        root.close().map_err(|_| "dormant-quota-cleanup")?;
        println!("{marker}: native-code=0");
    }
    Ok(())
}

/// Credential-free original image/home/physical continuity only. No original
/// thread or turn is created; the synthetic control above owns that distinct proof.
pub(crate) fn qualify_original(first: &Path, second: &Path) -> Result<(), &'static str> {
    let root = tempfile::Builder::new()
        .prefix("Zephium.DormantOriginal.")
        .tempdir()
        .map_err(|_| "dormant-original-root")?;
    let profile = ProfileId::from_native([89; 16]).ok_or("dormant-original-profile")?;
    let stop = Arc::new(AtomicBool::new(false));
    let installed = root.path().join("installed-package");
    std::fs::create_dir(&installed).map_err(|_| "dormant-original-package")?;
    let executable = installed.join("codex.exe");
    let initial_deadline = Instant::now() + Duration::from_secs(120);
    let prepared = prepare_copy(
        first,
        &executable,
        crate::pin::ReviewedCli::V163,
        initial_deadline,
        &stop,
    )?;
    let lease = ProfileLease::fixture_protected(root.path(), profile)
        .map_err(|_| "dormant-original-lease")?;
    let original_record = lease.record.clone();
    let mut store = MemoryAssociations::default();
    let canonical = match store.call(durable::Call::Open {
        incarnation: lease.record.incarnation,
    }) {
        durable::Reply::Scope(scope) => scope,
        _ => return Err("dormant-original-store"),
    };
    let budget = PreparationBudget::bounded(Instant::now(), initial_deadline);
    let mut owner = Connection::bootstrap(
        profile,
        1,
        lease,
        prepared,
        Box::new(store.clone()),
        NativePreparation {
            budget,
            removal: None,
            existing_owner: Some(canonical),
            expected_record: Some(Box::new(original_record.clone())),
            wake_deadline: Some(initial_deadline),
            wake_cancel: Some(stop.clone()),
        },
        &stop,
    )
    .map_err(|_| "dormant-original-first-bootstrap")?;
    drive_signed_out(&mut owner, initial_deadline)?;
    if !owner.browser_capable(owner.scope()) || owner.release_version() != Some("0.163.0") {
        return Err("dormant-original-first-version");
    }
    denied(&installed, &root.path().join("retired-package"))?;
    let scope = owner.scope().clone();
    let receipt = owner
        .try_dormant(&scope, Instant::now() + Duration::from_secs(30))
        .map_err(|_| "dormant-original-first-close")?;
    std::fs::rename(&installed, root.path().join("retired-package"))
        .map_err(|_| "dormant-original-package-replacement")?;
    // Wake joins existing canonical state/home BEFORE any new image process.
    if store.call(durable::Call::InspectOwner)
        != durable::Reply::Owner(Some(receipt.identity.canonical))
    {
        return Err("dormant-original-wake-store");
    }
    let lease = ProfileLease::fixture_existing_for_wake(
        root.path(),
        receipt.identity.profile,
        receipt.identity.canonical.incarnation,
    )
    .map_err(|_| "dormant-original-existing-home")?;
    if lease.record != original_record {
        return Err("dormant-original-home-replaced");
    }
    let wake_deadline = Instant::now() + Duration::from_secs(120);
    std::fs::create_dir(&installed).map_err(|_| "dormant-original-new-package")?;
    let prepared = prepare_copy(
        second,
        &executable,
        crate::pin::ReviewedCli::V164,
        wake_deadline,
        &stop,
    )?;
    let budget = PreparationBudget::bounded(Instant::now(), wake_deadline);
    let mut next = Connection::bootstrap(
        profile,
        2,
        lease,
        prepared,
        Box::new(store.clone()),
        NativePreparation {
            budget,
            removal: None,
            existing_owner: Some(receipt.identity.canonical),
            expected_record: Some(receipt.identity.record.clone()),
            wake_deadline: Some(wake_deadline),
            wake_cancel: Some(stop.clone()),
        },
        &stop,
    )
    .map_err(|_| "dormant-original-wake-bootstrap")?;
    drop(receipt);
    drive_signed_out(&mut next, wake_deadline)?;
    if !next.browser_capable(next.scope())
        || next.release_version() != Some("0.164.0")
        || next.scope() == &scope
    {
        return Err("dormant-original-fresh-image-scope");
    }
    let scope = next.scope().clone();
    let receipt = next
        .try_dormant(&scope, Instant::now() + Duration::from_secs(30))
        .map_err(|_| "dormant-original-wake-close")?;
    if receipt.identity.canonical != canonical
        || store.call(durable::Call::InspectOwner) != durable::Reply::Owner(Some(canonical))
    {
        return Err("dormant-original-grant-changed");
    }
    drop(receipt);
    root.close().map_err(|_| "dormant-original-cleanup")?;
    println!("codex-dormant-original-0.163.0-to-0.164.0: native-code=0");
    Ok(())
}
fn prepare_copy(
    source: &Path,
    target: &Path,
    expected: crate::pin::ReviewedCli,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<CompatibleImage, &'static str> {
    let source_image = crate::image::CliCandidate::open(source)
        .map_err(|_| "dormant-copy-source-open")?
        .hash(deadline, cancel)
        .map_err(|_| "dormant-copy-source-hash")?;
    if source_image.reference() != Some(expected) {
        return Err("dormant-copy-source-pin");
    }
    let source_image = source_image
        .admit_reference()
        .map_err(|_| "dormant-copy-source-admission")?;
    let mut input = std::fs::File::open(source_image.path()).map_err(|_| "dormant-copy-reader")?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|_| "dormant-copy-create-new")?;
    std::io::copy(&mut input, &mut output).map_err(|_| "dormant-copy-bytes")?;
    output.sync_all().map_err(|_| "dormant-copy-sync")?;
    drop(output);
    drop(input);
    drop(source_image);
    let image = crate::image::CliCandidate::open(target)
        .map_err(|_| "dormant-copy-held-open")?
        .hash(deadline, cancel)
        .map_err(|_| "dormant-copy-held-hash")?;
    if image.reference() != Some(expected) {
        return Err("dormant-copy-held-pin");
    }
    let image = Arc::new(
        image
            .admit_reference()
            .map_err(|_| "dormant-copy-held-admission")?,
    );
    CompatibleImage::prepare(image, deadline, cancel)
        .and_then(|prepared| prepared.with_browser(deadline, cancel))
        .map_err(|_| "dormant-copy-compatible")
}
fn drive_signed_out(owner: &mut Connection, deadline: Instant) -> Result<(), &'static str> {
    let scope = owner.scope().clone();
    let stop = AtomicBool::new(false);
    let mut observed = false;
    loop {
        if Instant::now() >= deadline {
            return Err("dormant-original-read-deadline");
        }
        let events = owner
            .poll(&scope, &stop)
            .map_err(|_| "dormant-original-poll")?;
        let quota = owner.quota(&scope).map_err(|_| "dormant-original-quota")?;
        for event in events {
            if event.scope() != &scope || event.conversation().is_some() {
                return Err("dormant-original-foreign-event");
            }
            match event.into_event() {
                Event::Account(AccountState::SignedOut)
                    if owner.state() == AccountState::SignedOut =>
                {
                    observed = true;
                }
                Event::Quota(view)
                    if crate::quota_owner::qualify_initial_quota(
                        &scope,
                        &scope,
                        false,
                        owner.state(),
                        &quota,
                        &view,
                    ) => {}
                _ => return Err("dormant-original-unexpected-event"),
            }
        }
        if observed && owner.ready_to_dormant(&scope) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
