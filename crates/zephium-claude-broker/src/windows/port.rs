//! One-shot fresh signed-out native Port qualification, not a production factory.
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
            let image = HeldImage::open(&target, signature::CANDIDATE_SHA256)?;
            signature::verify(&image)?;
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
pub(super) use native::FreshStatusPort;

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
}
