//! Candidate native auth handoff. This is not a vendor stdout parser or an opener.
//! No production constructor admits this module while qualification is closed.
use crate::{
    custody::{Custody, Owner, Phase, Ticket},
    plan::Operation,
};
use std::{
    collections::HashMap,
    fmt,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{Duration, Instant},
};

const MAX_CODE_BYTES: usize = 4096;
const MAX_HANDOFF_BYTES: usize = 8192;
const LOGIN_DEADLINE: Duration = Duration::from_secs(300);
// Native profiles only; bound retained revision tombstones across repeated
// profile creation/deletion in one application process.
const MAX_ADMISSION_PROFILES: usize = 4096;

/// Native worker lifetime, separate from any controller/view. One Claude account
/// slot per native profile; generation changes cannot evade an older live lease.
pub(crate) struct Admission {
    profile: u128,
    revision: u64,
}
struct AdmissionSlot {
    revision: u64,
    lease: Weak<Admission>,
}
fn admissions() -> &'static Mutex<HashMap<u128, AdmissionSlot>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u128, AdmissionSlot>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
impl Admission {
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }
}
pub(super) fn reserve_admission(profile: u128) -> Result<Arc<Admission>, AuthError> {
    reserve_admission_for(profile, None)
}
pub(super) fn reserve_admission_for(
    profile: u128,
    expected_revision: Option<u64>,
) -> Result<Arc<Admission>, AuthError> {
    let mut slots = admissions().lock().map_err(|_| AuthError::Closed)?;
    if !slots.contains_key(&profile) && slots.len() >= MAX_ADMISSION_PROFILES {
        return Err(AuthError::Exhausted);
    }
    let slot = slots.entry(profile).or_insert(AdmissionSlot {
        revision: 0,
        lease: Weak::new(),
    });
    if slot.lease.upgrade().is_some() {
        return Err(AuthError::Busy);
    }
    if expected_revision.is_some_and(|revision| revision != slot.revision) {
        return Err(AuthError::Stale);
    }
    // Keep the monotonic revision even while idle. Removing idle slots would
    // allow an old status selection to match a newly created login owner.
    slot.revision = slot.revision.checked_add(1).ok_or(AuthError::Exhausted)?;
    let lease = Arc::new(Admission {
        profile,
        revision: slot.revision,
    });
    slot.lease = Arc::downgrade(&lease);
    Ok(lease)
}
pub(super) fn revision_is_current(profile: u128, revision: u64) -> bool {
    admissions().lock().is_ok_and(|slots| {
        slots
            .get(&profile)
            .is_some_and(|slot| slot.revision == revision)
    })
}

/// An owning worker panicked without proving settlement. Keep its native profile
/// admission occupied for the process lifetime; a dropped view cannot clear it.
/// This is not evidence that native child/pipe teardown succeeded.
pub(super) fn retain_uncertain(admission: Arc<Admission>) {
    static HELD: OnceLock<Mutex<HashMap<u128, Arc<Admission>>>> = OnceLock::new();
    let mut held = HELD
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.entry(admission.profile).or_insert(admission);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthError {
    Stale,
    Busy,
    Order,
    InvalidInput,
    InvalidHandoff,
    Closed,
    Expired,
    OpenFailed,
    Exhausted,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Attempt {
    ticket: Ticket,
    controller: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum State {
    Preparing,
    Waiting,
    CodeRequested,
    CodeSubmitted,
    Stopping,
    StopUnconfirmed,
    Exited,
}

/// One manual response, not a token importer or a model/tool input. No Clone,
/// Serialize or raw Debug. Its allocation is overwritten on every owned drop.
pub(crate) struct ManualCode(zeroize::Zeroizing<Vec<u8>>);
impl ManualCode {
    pub(crate) fn new(value: &str) -> Result<Self, AuthError> {
        // A single opaque printable ASCII code, potentially containing '#'. Never
        // permit a second command/response, terminal controls or an empty Enter.
        if value.is_empty()
            || value.len() > MAX_CODE_BYTES
            || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(AuthError::InvalidInput);
        }
        let mut bytes = Vec::with_capacity(value.len() + 1);
        bytes.extend_from_slice(value.as_bytes());
        Ok(Self(zeroize::Zeroizing::new(bytes)))
    }
    fn write_to(mut self, output: &mut impl Write) -> std::io::Result<()> {
        self.0.push(b'\n');
        output.write_all(&self.0)
    }
}
impl fmt::Debug for ManualCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManualCode([REDACTED])")
    }
}

struct Handoff(zeroize::Zeroizing<String>);
impl Handoff {
    fn candidate(value: &str) -> Result<Self, AuthError> {
        // These exact vendor origins/authorize path are a candidate allowlist,
        // NOT evidence that this pinned CLI emits a supported handoff protocol.
        // No suffix matches, arbitrary SSO URL, file URL, userinfo or fragments.
        if value.len() > MAX_HANDOFF_BYTES
            || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
            || value.contains('\\')
        {
            return Err(AuthError::InvalidHandoff);
        }
        let url = url::Url::parse(value).map_err(|_| AuthError::InvalidHandoff)?;
        let authority = value
            .strip_prefix("https://")
            .and_then(|tail| tail.split('/').next())
            .ok_or(AuthError::InvalidHandoff)?;
        let invalid = authority.contains('%')
            || authority.contains('@')
            || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() != Some(443)
            || !matches!(
                url.host_str(),
                Some("claude.ai" | "console.anthropic.com" | "platform.claude.com")
            )
            || url.path() != "/oauth/authorize"
            || url.query().is_none_or(str::is_empty)
            || url.query_pairs().any(|(key, _)| {
                matches!(
                    key.as_ref(),
                    "code" | "access_token" | "refresh_token" | "id_token" | "client_secret"
                )
            });
        let canonical = zeroize::Zeroizing::new(url.into());
        if invalid {
            return Err(AuthError::InvalidHandoff);
        }
        Ok(Self(canonical))
    }
}
impl fmt::Debug for Handoff {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Handoff([REDACTED])")
    }
}

/// Only the owning native stdin worker receives this endpoint. A single pending
/// item is bounded at 4096 bytes; dropping the sender gives EOF, never a new grant.
pub(crate) struct InputReceiver {
    receiver: Receiver<ManualCode>,
    cancelled: Arc<AtomicBool>,
    expires: Instant,
    admission: Arc<Admission>,
}
impl InputReceiver {
    pub(crate) fn admission(&self) -> Arc<Admission> {
        Arc::clone(&self.admission)
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.expires
    }
    /// Physical child exit sets `finished`, so an idle writer cannot keep custody
    /// forever waiting for a view that has already disappeared.
    pub(crate) fn write_one(
        self,
        output: &mut impl Write,
        finished: &AtomicBool,
    ) -> Result<(), AuthError> {
        self.write_one_clock(output, finished, Instant::now)
    }
    fn write_one_clock(
        self,
        output: &mut impl Write,
        finished: &AtomicBool,
        now: impl Fn() -> Instant,
    ) -> Result<(), AuthError> {
        loop {
            if now() >= self.expires {
                return Err(AuthError::Expired);
            }
            if self.cancelled.load(Ordering::Acquire) || finished.load(Ordering::Acquire) {
                return Err(AuthError::Closed);
            }
            match self.receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(code) => {
                    if now() >= self.expires {
                        return Err(AuthError::Expired);
                    }
                    // Cancellation can race channel delivery. Recheck before the
                    // bounded write; native job termination breaks blocked writes.
                    if self.cancelled.load(Ordering::Acquire) || finished.load(Ordering::Acquire) {
                        return Err(AuthError::Closed);
                    }
                    return code.write_to(output).map_err(|_| AuthError::Closed);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }
}
struct Active {
    _admission: Arc<Admission>,
    attempt: Attempt,
    owner: Owner,
    expires: Instant,
    state: State,
    handoff: Option<Handoff>,
    browser_opened: bool,
    input: Option<SyncSender<ManualCode>>,
    cancelled: Arc<AtomicBool>,
}
/// Single-account native controller. Caller/profile validation must happen in the
/// native bridge, never from model text or an untrusted renderer-supplied owner.
pub(crate) struct Controller {
    owner: Owner,
    instance: u64,
    custody: Custody,
    active: Option<Active>,
}
impl Controller {
    pub(crate) fn new(owner: Owner) -> Result<Self, AuthError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let instance = NEXT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| AuthError::Exhausted)?;
        Ok(Self {
            owner,
            instance,
            custody: Custody::default(),
            active: None,
        })
    }
    pub(crate) fn reserve(
        &mut self,
        owner: Owner,
        now: Instant,
    ) -> Result<(Attempt, InputReceiver, Arc<AtomicBool>), AuthError> {
        if owner != self.owner {
            return Err(AuthError::Stale);
        }
        let expires = now.checked_add(LOGIN_DEADLINE).ok_or(AuthError::Expired)?;
        let admission = reserve_admission(owner.profile)?;
        let ticket = self
            .custody
            .reserve(owner, Operation::StartLogin)
            .map_err(|_| AuthError::Busy)?;
        let attempt = Attempt {
            ticket,
            controller: self.instance,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let (input, receiver) = mpsc::sync_channel(1);
        self.active = Some(Active {
            _admission: Arc::clone(&admission),
            attempt,
            owner,
            expires,
            state: State::Preparing,
            handoff: None,
            browser_opened: false,
            input: Some(input),
            cancelled: Arc::clone(&cancelled),
        });
        Ok((
            attempt,
            InputReceiver {
                receiver,
                cancelled: Arc::clone(&cancelled),
                expires,
                admission,
            },
            cancelled,
        ))
    }
    fn active(&mut self, attempt: Attempt) -> Result<&mut Active, AuthError> {
        self.active
            .as_mut()
            .filter(|active| active.attempt == attempt)
            .ok_or(AuthError::Stale)
    }
    fn interactive(
        &mut self,
        owner: Owner,
        attempt: Attempt,
        now: Instant,
    ) -> Result<&mut Active, AuthError> {
        if owner != self.owner {
            return Err(AuthError::Stale);
        }
        let active = self.active(attempt)?;
        if owner != active.owner {
            return Err(AuthError::Stale);
        }
        if now >= active.expires {
            self.cancel(owner, attempt)?;
            return Err(AuthError::Expired);
        }
        let active = self.active(attempt)?;
        if matches!(
            active.state,
            State::Stopping | State::StopUnconfirmed | State::Exited
        ) {
            return Err(AuthError::Closed);
        }
        Ok(active)
    }
    /// Native worker only: report actual successful process creation, even if a
    /// cancel raced CreateProcess. Such a child cannot be abandoned as uncreated.
    pub(crate) fn spawned(&mut self, attempt: Attempt) -> Result<(), AuthError> {
        self.active(attempt)?;
        self.custody
            .spawned(attempt.ticket)
            .map_err(|_| AuthError::Order)?;
        let active = self.active(attempt)?;
        if active.state == State::Preparing {
            active.state = State::Waiting;
        }
        Ok(())
    }
    pub(crate) fn handoff(
        &mut self,
        owner: Owner,
        attempt: Attempt,
        value: &str,
        now: Instant,
    ) -> Result<(), AuthError> {
        let active = self.interactive(owner, attempt, now)?;
        if active.state != State::Waiting || active.handoff.is_some() {
            return Err(AuthError::Order);
        }
        active.handoff = Some(Handoff::candidate(value)?);
        Ok(())
    }
    /// Native opener callback runs only on the user's explicit Open Browser action.
    /// Neither raw URL nor query is returned to presentation/model state.
    pub(crate) fn open_browser(
        &mut self,
        owner: Owner,
        attempt: Attempt,
        now: Instant,
        open: impl FnOnce(&str) -> Result<(), ()>,
    ) -> Result<(), AuthError> {
        let active = self.interactive(owner, attempt, now)?;
        if active.browser_opened {
            return Err(AuthError::Order);
        }
        let handoff = active.handoff.as_ref().ok_or(AuthError::Order)?;
        open(handoff.0.as_str()).map_err(|_| AuthError::OpenFailed)?;
        active.browser_opened = true;
        Ok(())
    }
    /// Candidate typed prompt notification. No real vendor-text recognizer is wired.
    pub(crate) fn code_requested(
        &mut self,
        owner: Owner,
        attempt: Attempt,
        now: Instant,
    ) -> Result<(), AuthError> {
        let active = self.interactive(owner, attempt, now)?;
        if active.state != State::Waiting {
            return Err(AuthError::Order);
        }
        active.state = State::CodeRequested;
        Ok(())
    }
    pub(crate) fn submit_code(
        &mut self,
        owner: Owner,
        attempt: Attempt,
        code: ManualCode,
        now: Instant,
    ) -> Result<(), AuthError> {
        let active = self.interactive(owner, attempt, now)?;
        if active.state != State::CodeRequested {
            return Err(AuthError::Order);
        }
        active
            .input
            .take()
            .ok_or(AuthError::Closed)?
            .try_send(code)
            .map_err(|_| AuthError::Closed)?;
        active.state = State::CodeSubmitted;
        Ok(())
    }
    pub(crate) fn cancel(&mut self, owner: Owner, attempt: Attempt) -> Result<(), AuthError> {
        if owner != self.owner || self.active(attempt)?.owner != owner {
            return Err(AuthError::Stale);
        }
        self.stop(attempt)
    }
    fn stop(&mut self, attempt: Attempt) -> Result<(), AuthError> {
        self.active(attempt)?;
        self.custody
            .request_stop(attempt.ticket)
            .map_err(|_| AuthError::Stale)?;
        let active = self.active(attempt)?;
        active.cancelled.store(true, Ordering::Release);
        active.input = None;
        active.handoff = None;
        if active.state != State::Exited {
            active.state = State::Stopping;
        }
        Ok(())
    }
    pub(crate) fn retire(&mut self, replacement: Owner) -> Result<(), AuthError> {
        if let Some(active) = self.active.as_ref() {
            self.stop(active.attempt)?;
        }
        self.owner = replacement;
        Ok(())
    }
    pub(crate) fn stop_unconfirmed(&mut self, attempt: Attempt) -> Result<(), AuthError> {
        self.active(attempt)?;
        self.custody
            .stop_unconfirmed(attempt.ticket)
            .map_err(|_| AuthError::Order)?;
        self.active(attempt)?.state = State::StopUnconfirmed;
        Ok(())
    }
    pub(crate) fn abandon_uncreated(&mut self, attempt: Attempt) -> Result<(), AuthError> {
        self.active(attempt)?;
        self.custody
            .abandon_before_spawn(attempt.ticket)
            .map_err(|_| AuthError::Order)?;
        let active = self.active(attempt)?;
        active.cancelled.store(true, Ordering::Release);
        active.input = None;
        active.handoff = None;
        self.active = None;
        Ok(())
    }
    pub(crate) fn observed_exit(&mut self, attempt: Attempt, code: i32) -> Result<(), AuthError> {
        self.active(attempt)?;
        self.custody
            .observed_exit(attempt.ticket, code)
            .map_err(|_| AuthError::Order)?;
        let active = self.active(attempt)?;
        active.cancelled.store(true, Ordering::Release);
        active.input = None;
        active.handoff = None;
        active.state = State::Exited;
        Ok(())
    }
    pub(crate) fn settled_pipe(&mut self, attempt: Attempt, pipe: usize) -> Result<(), AuthError> {
        self.active(attempt)?;
        self.custody
            .settled_pipe(attempt.ticket, pipe)
            .map_err(|_| AuthError::Order)
    }
    /// Exit zero only permits a later separately authorized native status recheck;
    /// this result is never a Connected state or a subscription entitlement.
    pub(crate) fn release(&mut self, attempt: Attempt) -> Result<i32, AuthError> {
        self.active(attempt)?;
        let code = self
            .custody
            .release(attempt.ticket)
            .map_err(|_| AuthError::Order)?;
        self.active = None;
        Ok(code)
    }
    pub(crate) fn phase(&self) -> Option<Phase> {
        self.custody.phase()
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        if let Some(active) = self.active.as_ref() {
            active.cancelled.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> Owner {
        thread_local! {
            static PROFILE: u128 = {
                static NEXT_PROFILE: AtomicU64 = AtomicU64::new(1);
                u128::from(NEXT_PROFILE.fetch_add(1, Ordering::Relaxed))
            };
        }
        Owner {
            profile: PROFILE.with(|profile| *profile),
            account_generation: 2,
            profile_generation: 3,
        }
    }
    fn settle(controller: &mut Controller, attempt: Attempt) {
        controller.observed_exit(attempt, 0).unwrap();
        for pipe in 0..3 {
            controller.settled_pipe(attempt, pipe).unwrap();
        }
        assert_eq!(controller.release(attempt), Ok(0));
    }
    #[test]
    fn manual_code_is_single_bounded_redacted_response() {
        for invalid in [
            "",
            "two lines\n",
            "one\rtwo",
            "with space",
            "\0",
            "\u{1b}",
            "é",
        ] {
            assert!(ManualCode::new(invalid).is_err());
        }
        assert!(ManualCode::new(&"x".repeat(MAX_CODE_BYTES + 1)).is_err());
        let code = ManualCode::new("synthetic-code#synthetic-state").unwrap();
        assert!(!format!("{code:?}").contains("synthetic"));
        let mut output = Vec::new();
        code.write_to(&mut output).unwrap();
        assert_eq!(output, b"synthetic-code#synthetic-state\n");
    }
    #[test]
    fn handoff_is_exact_https_authorize_origin_and_redacted() {
        let valid = "https://claude.ai/oauth/authorize?state=synthetic-private";
        let handoff = Handoff::candidate(valid).unwrap();
        assert!(!format!("{handoff:?}").contains("synthetic"));
        for invalid in [
            "http://claude.ai/oauth/authorize?state=x",
            "https://claude.ai.evil.test/oauth/authorize?state=x",
            "https://claude.ai@evil.test/oauth/authorize?state=x",
            "https://user@claude.ai/oauth/authorize?state=x",
            "https://@claude.ai/oauth/authorize?state=x",
            "https://claude.ai:444/oauth/authorize?state=x",
            "https://claude.ai/oauth/authorize?state=x#fragment",
            "https://%63laude.ai/oauth/authorize?state=x",
            "https://claude.ai./oauth/authorize?state=x",
            "https://claude.ai\\@evil.test/oauth/authorize?state=x",
            "https://claude.ai/oauth/authorize?code=secret",
            "https://claude.ai/other?state=x",
            "https://claude.ai/oauth/authorize",
            "https://claude.ai/oauth/authorize?state=x\n",
        ] {
            assert!(
                Handoff::candidate(invalid).is_err(),
                "invalid candidate accepted"
            );
        }
    }
    #[test]
    fn prompt_is_required_one_code_only_and_no_implicit_browser_open() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (a, receiver, _) = c.reserve(owner(), now).unwrap();
        c.spawned(a).unwrap();
        assert_eq!(
            c.submit_code(owner(), a, ManualCode::new("premature").unwrap(), now),
            Err(AuthError::Order)
        );
        c.handoff(
            owner(),
            a,
            "https://claude.ai/oauth/authorize?state=synthetic",
            now,
        )
        .unwrap();
        let mut called = false;
        c.open_browser(owner(), a, now, |_| {
            called = true;
            Ok(())
        })
        .unwrap();
        assert!(called);
        assert_eq!(
            c.open_browser(owner(), a, now, |_| panic!("duplicate open")),
            Err(AuthError::Order)
        );
        c.code_requested(owner(), a, now).unwrap();
        c.submit_code(owner(), a, ManualCode::new("synthetic").unwrap(), now)
            .unwrap();
        assert_eq!(
            c.submit_code(owner(), a, ManualCode::new("duplicate").unwrap(), now),
            Err(AuthError::Order)
        );
        let mut output = Vec::new();
        receiver
            .write_one(&mut output, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(output, b"synthetic\n");
        settle(&mut c, a);
    }
    #[test]
    fn cancel_retains_custody_and_prevents_queued_code_delivery() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (a, receiver, cancel) = c.reserve(owner(), now).unwrap();
        c.spawned(a).unwrap();
        c.code_requested(owner(), a, now).unwrap();
        c.submit_code(owner(), a, ManualCode::new("synthetic").unwrap(), now)
            .unwrap();
        c.cancel(owner(), a).unwrap();
        assert!(cancel.load(Ordering::Acquire));
        c.stop_unconfirmed(a).unwrap();
        assert_eq!(c.phase(), Some(Phase::StopUnconfirmed));
        assert!(matches!(c.reserve(owner(), now), Err(AuthError::Busy)));
        let mut output = Vec::new();
        assert_eq!(
            receiver.write_one(&mut output, &AtomicBool::new(false)),
            Err(AuthError::Closed)
        );
        assert!(output.is_empty());
        assert_eq!(c.release(a), Err(AuthError::Order));
        settle(&mut c, a);
    }
    #[test]
    fn profile_retirement_blocks_stale_actions_but_not_native_cleanup() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (a, receiver, cancel) = c.reserve(owner(), now).unwrap();
        c.spawned(a).unwrap();
        let next = Owner {
            profile_generation: 4,
            ..owner()
        };
        c.retire(next).unwrap();
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(c.code_requested(owner(), a, now), Err(AuthError::Stale));
        assert_eq!(
            c.open_browser(next, a, now, |_| panic!("stale open")),
            Err(AuthError::Stale)
        );
        assert!(matches!(c.reserve(next, now), Err(AuthError::Busy)));
        settle(&mut c, a);
        drop(receiver);
        let (b, _, _) = c.reserve(next, now).unwrap();
        assert_ne!(a, b);
        assert_eq!(c.cancel(next, a), Err(AuthError::Stale));
        c.abandon_uncreated(b).unwrap();
    }
    #[test]
    fn deadline_starts_at_reservation_and_invalidates_handoff() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (a, _, cancelled) = c.reserve(owner(), now).unwrap();
        c.spawned(a).unwrap();
        assert_eq!(
            c.code_requested(owner(), a, now + LOGIN_DEADLINE),
            Err(AuthError::Expired)
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(c.phase(), Some(Phase::StopRequested));
    }
    #[test]
    fn actual_exit_settles_idle_stdin_and_all_three_ends_are_required() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (a, receiver, _) = c.reserve(owner(), now).unwrap();
        c.spawned(a).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            receiver.write_one(&mut output, &AtomicBool::new(true)),
            Err(AuthError::Closed)
        );
        c.observed_exit(a, 0).unwrap();
        c.settled_pipe(a, 0).unwrap();
        c.settled_pipe(a, 1).unwrap();
        assert_eq!(c.release(a), Err(AuthError::Order));
        c.settled_pipe(a, 2).unwrap();
        assert_eq!(c.release(a), Ok(0));
    }
    #[test]
    fn cancellation_racing_creation_cannot_abandon_child() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (a, _, _) = c.reserve(owner(), now).unwrap();
        c.cancel(owner(), a).unwrap();
        c.spawned(a).unwrap();
        assert_eq!(c.abandon_uncreated(a), Err(AuthError::Order));
        assert_eq!(c.phase(), Some(Phase::StopRequested));
        settle(&mut c, a);
    }
    #[test]
    fn dropped_view_controller_cancels_without_auth_or_token_operation() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (_, receiver, cancel) = c.reserve(owner(), now).unwrap();
        drop(c);
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(
            receiver.write_one(&mut Vec::new(), &AtomicBool::new(false)),
            Err(AuthError::Closed)
        );
    }
    #[test]
    fn controller_recreation_cannot_reuse_an_old_attempt() {
        let now = Instant::now();
        let mut first = Controller::new(owner()).unwrap();
        let (old, _, _) = first.reserve(owner(), now).unwrap();
        first.abandon_uncreated(old).unwrap();
        let mut second = Controller::new(owner()).unwrap();
        let (current, _, _) = second.reserve(owner(), now).unwrap();
        assert_ne!(old, current);
        assert_eq!(second.cancel(owner(), old), Err(AuthError::Stale));
        assert_eq!(second.settled_pipe(old, 0), Err(AuthError::Stale));
        assert_eq!(second.release(old), Err(AuthError::Stale));
        second.abandon_uncreated(current).unwrap();
    }
    #[test]
    fn idle_writer_settles_on_cancel_and_channel_eof() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (a, input, cancelled) = c.reserve(owner(), now).unwrap();
        let worker =
            std::thread::spawn(move || input.write_one(&mut Vec::new(), &AtomicBool::new(false)));
        cancelled.store(true, Ordering::Release);
        assert_eq!(worker.join().unwrap(), Err(AuthError::Closed));
        c.abandon_uncreated(a).unwrap();
        let (_, input, _) = c.reserve(owner(), now).unwrap();
        c.active.as_mut().unwrap().input = None;
        let mut bytes = Vec::new();
        assert_eq!(input.write_one(&mut bytes, &AtomicBool::new(false)), Ok(()));
        assert!(bytes.is_empty());
    }
    #[test]
    fn writer_checks_admission_deadline_without_a_ui_tick() {
        let mut c = Controller::new(owner()).unwrap();
        let now = Instant::now();
        let (_, input, _) = c.reserve(owner(), now).unwrap();
        let deadline = input.deadline();
        assert_eq!(
            input.write_one_clock(&mut Vec::new(), &AtomicBool::new(false), || deadline),
            Err(AuthError::Expired)
        );
    }
    #[test]
    fn deadline_crossing_receive_never_writes_the_code() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (a, input, _) = c.reserve(owner(), now).unwrap();
        c.spawned(a).unwrap();
        c.code_requested(owner(), a, now).unwrap();
        c.submit_code(owner(), a, ManualCode::new("synthetic").unwrap(), now)
            .unwrap();
        let calls = std::cell::Cell::new(0);
        let mut bytes = Vec::new();
        assert_eq!(
            input.write_one_clock(&mut bytes, &AtomicBool::new(false), || {
                let call = calls.get();
                calls.set(call + 1);
                if call == 0 {
                    now
                } else {
                    now + LOGIN_DEADLINE
                }
            }),
            Err(AuthError::Expired)
        );
        assert_eq!(calls.get(), 2);
        assert!(bytes.is_empty());
    }
    #[test]
    fn replacing_controller_cannot_release_a_native_worker_lease() {
        let now = Instant::now();
        let mut first = Controller::new(owner()).unwrap();
        let (a, input, _) = first.reserve(owner(), now).unwrap();
        first.spawned(a).unwrap();
        let native_worker_lease = input.admission();
        drop(first);
        drop(input);
        let replacement_owner = Owner {
            account_generation: 9,
            profile_generation: 9,
            ..owner()
        };
        let mut replacement = Controller::new(replacement_owner).unwrap();
        assert!(matches!(
            replacement.reserve(replacement_owner, now),
            Err(AuthError::Busy)
        ));
        drop(native_worker_lease); // actual native code does this only after reap/join
        let (b, endpoint, _) = replacement.reserve(replacement_owner, now).unwrap();
        assert_eq!(replacement.release(a), Err(AuthError::Stale));
        replacement.abandon_uncreated(b).unwrap();
        assert!(matches!(
            replacement.reserve(replacement_owner, now),
            Err(AuthError::Busy)
        ));
        drop(endpoint);
        assert!(replacement.reserve(replacement_owner, now).is_ok());
    }
    #[test]
    fn abandoning_uncreated_attempt_revokes_a_retained_endpoint() {
        let now = Instant::now();
        let mut c = Controller::new(owner()).unwrap();
        let (attempt, input, cancelled) = c.reserve(owner(), now).unwrap();
        c.abandon_uncreated(attempt).unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        drop(c);
        let mut bytes = Vec::new();
        assert_eq!(
            input.write_one(&mut bytes, &AtomicBool::new(false)),
            Err(AuthError::Closed)
        );
        assert!(bytes.is_empty());
    }
}
