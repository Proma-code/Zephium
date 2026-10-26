//! Fresh and disk-reopened signed-out native Ports; production remains unavailable.
use crate::{
    account::{Control, Error, Request},
    custody::Owner,
};
use std::time::Instant;

struct FreshOnly {
    owner: Owner,
    attempted: bool,
}
impl FreshOnly {
    fn new(owner: Owner) -> Self {
        Self {
            owner,
            attempted: false,
        }
    }
    fn admit(&mut self, request: &Request, control: &Control, now: Instant) -> Result<(), Error> {
        if control.owner() != self.owner {
            return Err(Error::Stale);
        }
        if now >= control.deadline() {
            return Err(Error::Expired);
        }
        if control.cancelled(now) {
            return Err(Error::Cancelled);
        }
        if self.attempted {
            return Err(Error::Closed);
        }
        if !matches!(request, Request::ReadStatus) {
            return Err(Error::Process);
        }
        // Consume before native preparation, not after a successful parse.
        // Even an interrupted attempt cannot discover a previously used profile.
        self.attempted = true;
        Ok(())
    }
}

/// Reopened resources start with no authentication projection. Both halves of
/// the persisted incarnation remain bound; Owner alone is not a store identity.
struct RetainedStatusOnly {
    owner: Owner,
    key: crate::registration::ProfileKey,
}
impl RetainedStatusOnly {
    fn admit(
        &self,
        request: &Request,
        control: &Control,
        actual_key: crate::registration::ProfileKey,
        now: Instant,
    ) -> Result<(), Error> {
        if control.owner() != self.owner
            || actual_key != self.key
            || self.key.profile != self.owner.profile.to_be_bytes()
        {
            return Err(Error::Stale);
        }
        if now >= control.deadline() {
            return Err(Error::Expired);
        }
        if control.cancelled(now) {
            return Err(Error::Cancelled);
        }
        if !matches!(request, Request::ReadStatus) {
            return Err(Error::Process);
        }
        Ok(())
    }
}

// Closed diagnostic vocabulary only; never prints arbitrary error/provider text.
fn process_failure_category(error: &'static str) -> i32 {
    match error {
        "cancelled before creation" => 1,
        "cancelled or resume failed" => 2,
        "confined process creation failed" => 3,
        "retained environment proof failed" => 4,
        "retained environment binding changed" => 5,
        "retained status owner mismatch" => 6,
        "retained status profile rejected" => 7,
        "retained status runtime rejected" => 8,
        "retained status custody missing" => 9,
        _ => 0,
    }
}

#[cfg(all(windows, feature = "native-fixtures"))]
mod native {
    use super::*;
    use crate::windows::{
        fixtures::{self, Root},
        process::{FreshSession, Layout},
        security::{HeldImage, Profile},
        signature,
    };
    use crate::{
        account::{AccountOwner, Port, Reply},
        protocol::AuthStatus,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    /// First bounded use of a durable factory lease. Only newly registered
    /// synthetic profiles may exercise this adapter during qualification; no
    /// production constructor/UI or original login operation calls it yet.
    pub(in crate::windows) struct RegisteredStatusPort {
        policy: RetainedStatusOnly,
        prepared: crate::windows::account_store::PreparedStore,
    }
    impl RegisteredStatusPort {
        fn from_prepared(
            prepared: crate::windows::account_store::PreparedStore,
        ) -> Result<(AccountOwner, Self), &'static str> {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT_LIFETIME: AtomicU64 = AtomicU64::new(1);
            prepared
                .verify()
                .map_err(|_| "registered status lease rejected")?;
            let key = prepared.profile_key();
            let generation = NEXT_LIFETIME
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| "registered status lifetime exhausted")?;
            let identity = Owner {
                profile: u128::from_be_bytes(key.profile),
                account_generation: 1,
                profile_generation: generation,
            };
            let owner =
                AccountOwner::new(identity).map_err(|_| "registered status owner unavailable")?;
            Ok((
                owner,
                Self {
                    policy: RetainedStatusOnly {
                        owner: identity,
                        key,
                    },
                    prepared,
                },
            ))
        }
        pub(in crate::windows) fn close(self) -> Result<(), &'static str> {
            self.prepared
                .verify()
                .map_err(|_| "registered status close unconfirmed")?;
            // No persistent directory, OS profile or cached image is deleted.
            // A failed ActiveStore independently retains DIRTY/quarantined custody.
            drop(self);
            Ok(())
        }
    }
    impl Port for RegisteredStatusPort {
        fn run(&mut self, request: Request, control: &Control) -> Result<Reply, Error> {
            use crate::windows::{account_store::ActiveStore, process};
            self.policy.admit(
                &request,
                control,
                self.prepared.profile_key(),
                Instant::now(),
            )?;
            let active = self
                .prepared
                .begin_operation()
                .map_err(|_| Error::Process)?;
            crate::windows::diagnostic("durable-status-dirty-admitted", 0);
            // Full hash/trust preparation finished before this AccountOwner
            // existed. The exact image is still held nonwrite/nondelete; this
            // admission rechecked its fixed cache identity/policy under DIRTY.
            crate::windows::diagnostic("durable-status-image-held", 0);
            crate::windows::diagnostic(
                "durable-status-before-run-budget-ms",
                control
                    .deadline()
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(i32::MAX as u128) as i32,
            );
            let call = process::run_store_status(
                active,
                self.prepared.image(),
                self.policy.key,
                self.policy.owner,
                control,
            )
            .map_err(|error| {
                crate::windows::diagnostic(
                    "durable-status-process-failure",
                    process_failure_category(error),
                );
                crate::windows::diagnostic(
                    "durable-status-failure-expired",
                    i32::from(Instant::now() >= control.deadline()),
                );
                Error::Process
            })?;
            crate::windows::diagnostic("durable-status-native-returned", 0);
            let status = ActiveStore::finish_status_fixture(call).map_err(|_| Error::Protocol)?;
            // CLEAN concerns native operation settlement only. Status comes only
            // from the bounded original-client readback, not the registration.
            Ok(Reply::Status(status))
        }
    }

    enum RegistrationMode {
        Fresh,
        Reopen(crate::registration::ProfileKey),
    }
    /// Synthetic anchor authority is prepared by test setup only. All profile
    /// create/reopen, image work and AccountOwner creation run inside the actual
    /// lifecycle reservation, under its native scope/shared admission/deadline.
    pub(in crate::windows) struct RegisteredStatusFactory<'a> {
        factory: &'a crate::windows::account_store::registration::TrialStoreFactory,
        mode: RegistrationMode,
        source: Option<signature::VerifiedImage>,
        observed_key: Option<crate::registration::ProfileKey>,
    }
    impl<'a> RegisteredStatusFactory<'a> {
        pub(in crate::windows) fn fresh_fixture(
            factory: &'a crate::windows::account_store::registration::TrialStoreFactory,
            image: signature::VerifiedImage,
        ) -> Self {
            Self {
                factory,
                mode: RegistrationMode::Fresh,
                source: Some(image),
                observed_key: None,
            }
        }
        pub(in crate::windows) fn reopened_fixture(
            factory: &'a crate::windows::account_store::registration::TrialStoreFactory,
            image: signature::VerifiedImage,
            key: crate::registration::ProfileKey,
        ) -> Self {
            Self {
                factory,
                mode: RegistrationMode::Reopen(key),
                source: Some(image),
                observed_key: None,
            }
        }
        pub(in crate::windows) fn observed_key(&self) -> Option<crate::registration::ProfileKey> {
            self.observed_key
        }
    }
    impl crate::lifecycle::Factory for RegisteredStatusFactory<'_> {
        fn prepare(
            &mut self,
            control: &crate::lifecycle::PreparationControl,
        ) -> Result<crate::lifecycle::Resource, crate::lifecycle::Error> {
            use crate::{
                lifecycle::{Error, Resource},
                registration::PolicyVersion,
            };
            let source = self.source.take().ok_or(Error::Preparation)?;
            if control.cancelled(Instant::now()) {
                return Err(Error::Stale);
            }
            let profile = control.scope().profile();
            let lease = match self.mode {
                RegistrationMode::Fresh => self.factory.create(profile, PolicyVersion::DataV1),
                RegistrationMode::Reopen(expected) => {
                    if expected.profile != profile {
                        return Err(Error::Stale);
                    }
                    self.factory.reopen(profile, PolicyVersion::DataV1)
                }
            }
            .map_err(|_| Error::Preparation)?;
            let key = lease.profile_key();
            if matches!(self.mode, RegistrationMode::Reopen(expected) if expected != key) {
                return Err(Error::Stale);
            }
            let prepared = lease
                .prepare_verified_image(&source, control)
                .map_err(|_| Error::Preparation)?;
            if control.cancelled(Instant::now()) {
                return Err(Error::Stale);
            }
            let (owner, port) =
                RegisteredStatusPort::from_prepared(prepared).map_err(|_| Error::Preparation)?;
            if owner.view().status.is_some() {
                return Err(Error::Preparation);
            }
            let resource = Resource::new(control.scope(), owner, port)?;
            self.observed_key = Some(key);
            Ok(resource)
        }
    }

    pub(in crate::windows) struct FreshStatusPort {
        policy: FreshOnly,
        session: FreshSession,
        // Field order retains the fresh root until session resources are dropped.
        root: Root,
    }
    impl FreshStatusPort {
        /// Exclusively new SID + empty directories + exact pinned signed image.
        /// No caller-selected directory, existing account or generic binary.
        pub(in crate::windows) fn create(
            original: &HeldImage,
        ) -> Result<(AccountOwner, Self), &'static str> {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "clock unavailable")?
                .as_nanos();
            let suffix = format!("{}.{}.FreshStatus", std::process::id(), stamp);
            let identity = Owner {
                profile: stamp,
                account_generation: 1,
                profile_generation: 1,
            };
            let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}"))?;
            let root = Root::create(
                std::env::temp_dir().join(format!("Zephium.ClaudeFixture.{suffix}")),
                &profile,
            )?;
            let layout = Layout::create(&root.0, &profile)?;
            for path in [
                &layout.config,
                &layout.home,
                &layout.appdata,
                &layout.localappdata,
                &layout.temporary,
                &layout.work,
            ] {
                if std::fs::read_dir(path)
                    .map_err(|_| "fresh status empty-root proof failed")?
                    .next()
                    .is_some()
                {
                    return Err("fresh status root was not empty; no status admitted");
                }
            }
            let target = root.0.join("claude.exe");
            profile.copy_image(&original.path, &target)?;
            let image = signature::VerifiedImage::open(&target)?;
            let owner =
                AccountOwner::new(identity).map_err(|_| "fresh status owner unavailable")?;
            Ok((
                owner,
                Self {
                    policy: FreshOnly::new(identity),
                    session: FreshSession::new(identity, image, layout, profile),
                    root,
                },
            ))
        }
        pub(in crate::windows) fn close(self) -> Result<(), &'static str> {
            self.session.close()?;
            self.root.cleanup()
        }
    }
    impl Port for FreshStatusPort {
        fn run(&mut self, request: Request, control: &Control) -> Result<Reply, Error> {
            self.policy.admit(&request, control, Instant::now())?;
            let expected_directory = self
                .session
                .config()
                .to_str()
                .ok_or(Error::Protocol)?
                .to_owned();
            let expected_projects = self
                .session
                .config()
                .join("projects")
                .to_str()
                .ok_or(Error::Protocol)?
                .to_owned();
            let settled = self.session.run(control);
            let mut output = settled.into_output().map_err(|_| Error::Process)?;
            fixtures::receipt("fresh-empty-status", &output);
            let stdout = zeroize::Zeroizing::new(std::mem::take(&mut output.stdout));
            let stderr = zeroize::Zeroizing::new(std::mem::take(&mut output.stderr));
            if output.stopped.is_some() || !stderr.is_empty() {
                return Err(Error::Process);
            }
            if let Err(reason) = crate::fresh_status::validate(
                &stdout,
                output.code,
                &expected_directory,
                &expected_projects,
            ) {
                crate::windows::diagnostic("fresh-status-rejected", reason as i32);
                if let Some(facts) = crate::fresh_status::shape(&stdout, &expected_projects) {
                    crate::windows::diagnostic("fresh-status-extra-keys", facts.extra_keys);
                    crate::windows::diagnostic("fresh-status-extra-nulls", facts.extra_nulls);
                    crate::windows::diagnostic("fresh-status-known-extra-shape", facts.known_extra);
                    crate::windows::diagnostic(
                        "fresh-status-identity-fields",
                        facts.identity_fields,
                    );
                }
                return Err(Error::Protocol);
            }
            // Owned buffers zeroize on every path; serde may make temporary copies.
            // Neither raw provider output nor an unparsed connected state escapes.
            Ok(Reply::Status(AuthStatus::SignedOut))
        }
    }
}
#[cfg(all(windows, feature = "native-fixtures"))]
pub(super) use native::{FreshStatusPort, RegisteredStatusFactory};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        account::{AccountOwner, Reply},
        plan::TurnInput,
        protocol::AuthStatus,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    fn identity() -> Owner {
        static NEXT: AtomicU64 = AtomicU64::new(1 << 50);
        Owner {
            profile: NEXT.fetch_add(1, Ordering::Relaxed).into(),
            account_generation: 2,
            profile_generation: 3,
        }
    }
    #[test]
    fn all_owner_fields_bind_before_attempt_consumption() {
        let native = identity();
        let owner = AccountOwner::new(native).unwrap();
        let now = Instant::now();
        let work = owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .unwrap();
        let result = work.run(
            &mut |request: Request, control: &Control| {
                for wrong in [
                    Owner {
                        profile: native.profile + 1,
                        ..native
                    },
                    Owner {
                        account_generation: native.account_generation + 1,
                        ..native
                    },
                    Owner {
                        profile_generation: native.profile_generation + 1,
                        ..native
                    },
                ] {
                    let mut policy = FreshOnly::new(wrong);
                    assert_eq!(policy.admit(&request, control, now), Err(Error::Stale));
                    assert!(!policy.attempted);
                }
                Ok(Reply::Status(AuthStatus::SignedOut))
            },
            || now,
        );
        assert_eq!(result, Ok(Reply::Status(AuthStatus::SignedOut)));
    }
    #[test]
    fn no_login_logout_or_turn_is_admitted_by_fresh_status_factory() {
        let native = identity();
        let owner = AccountOwner::new(native).unwrap();
        let now = Instant::now();
        let work = owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .unwrap();
        work.run(
            &mut |_: Request, control: &Control| {
                let mut policy = FreshOnly::new(native);
                for denied in [
                    Request::Login,
                    Request::Logout,
                    Request::Turn {
                        input: TurnInput::new("synthetic").unwrap(),
                        selected: AuthStatus::ClaudeAccount,
                    },
                ] {
                    assert_eq!(policy.admit(&denied, control, now), Err(Error::Process));
                    assert!(!policy.attempted);
                }
                assert_eq!(policy.admit(&Request::ReadStatus, control, now), Ok(()));
                Ok(Reply::Status(AuthStatus::SignedOut))
            },
            || now,
        )
        .unwrap();
    }
    #[test]
    fn one_shot_consumed_before_process_or_parser_failure() {
        let native = identity();
        let owner = AccountOwner::new(native).unwrap();
        let now = Instant::now();
        let mut policy = FreshOnly::new(native);
        let work = owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .unwrap();
        assert_eq!(
            work.run(
                &mut |request: Request, control: &Control| {
                    policy.admit(&request, control, now)?;
                    Err(Error::Process)
                },
                || now
            ),
            Err(Error::Process)
        );
        assert!(owner.drained());
        let retry = owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .unwrap();
        assert_eq!(
            retry.run(
                &mut |request: Request, control: &Control| {
                    policy.admit(&request, control, now)?;
                    panic!("a fresh profile must not be reused")
                },
                || now
            ),
            Err(Error::Closed)
        );
    }
    #[test]
    fn deadline_and_cancel_are_checked_before_consuming_attempt() {
        let native = identity();
        let owner = AccountOwner::new(native).unwrap();
        let now = Instant::now();
        let work = owner
            .begin(owner.scope().unwrap(), Request::ReadStatus, now)
            .unwrap();
        assert_eq!(
            work.run(
                &mut |request: Request, control: &Control| {
                    let mut policy = FreshOnly::new(native);
                    assert_eq!(
                        policy.admit(&request, control, control.deadline()),
                        Err(Error::Expired)
                    );
                    assert!(!policy.attempted);
                    owner.close();
                    assert_eq!(policy.admit(&request, control, now), Err(Error::Cancelled));
                    assert!(!policy.attempted);
                    Err(Error::Cancelled)
                },
                || now
            ),
            Err(Error::Closed)
        );
    }
    #[test]
    fn process_failure_diagnostics_never_relax_or_expose_unknown_messages() {
        assert_eq!(process_failure_category("cancelled before creation"), 1);
        assert_eq!(process_failure_category("cancelled or resume failed"), 2);
        assert_eq!(
            process_failure_category("confined process creation failed"),
            3
        );
        assert_eq!(
            process_failure_category("retained environment proof failed"),
            4
        );
        assert_eq!(
            process_failure_category("retained status custody missing"),
            9
        );
        assert_eq!(process_failure_category("synthetic unknown message"), 0);
    }
    #[test]
    fn retained_status_binds_full_key_and_allows_fresh_readback_only() {
        let native = identity();
        let key = crate::registration::ProfileKey {
            profile: native.profile.to_be_bytes(),
            incarnation: [7; 16],
        };
        let policy = RetainedStatusOnly { owner: native, key };
        let owner = AccountOwner::new(native).unwrap();
        assert_eq!(owner.view().status, None);
        for _ in 0..2 {
            let now = Instant::now();
            let work = owner
                .begin(owner.scope().unwrap(), Request::ReadStatus, now)
                .unwrap();
            let result = work.run(
                &mut |request: Request, control: &Control| {
                    assert_eq!(policy.admit(&request, control, key, now), Ok(()));
                    let wrong_owner = RetainedStatusOnly {
                        owner: Owner {
                            profile_generation: native.profile_generation + 1,
                            ..native
                        },
                        key,
                    };
                    assert_eq!(
                        wrong_owner.admit(&request, control, key, now),
                        Err(Error::Stale)
                    );
                    for byte in [0, 15] {
                        let mut wrong = key;
                        wrong.incarnation[byte] ^= 1;
                        assert_eq!(
                            policy.admit(&request, control, wrong, now),
                            Err(Error::Stale)
                        );
                    }
                    let mut wrong = key;
                    wrong.profile[0] ^= 1;
                    assert_eq!(
                        policy.admit(&request, control, wrong, now),
                        Err(Error::Stale)
                    );
                    for denied in [
                        Request::Login,
                        Request::Logout,
                        Request::Turn {
                            input: TurnInput::new("synthetic").unwrap(),
                            selected: AuthStatus::ClaudeAccount,
                        },
                    ] {
                        assert_eq!(
                            policy.admit(&denied, control, key, now),
                            Err(Error::Process)
                        );
                    }
                    assert_eq!(
                        policy.admit(&request, control, key, control.deadline()),
                        Err(Error::Expired)
                    );
                    Ok(Reply::Status(AuthStatus::SignedOut))
                },
                || now,
            );
            assert_eq!(result, Ok(Reply::Status(AuthStatus::SignedOut)));
        }
    }
}
