//! Candidate all-operation owner. Native ports remain separately qualified and
//! unavailable through the production account gate.
//! A returned port result means the owning process and every pipe have settled;
//! cancellation, a deadline and a dropped view never provide that evidence.
use crate::{
    auth::{self, Admission},
    custody::Owner,
    plan::{CommandPlan, Operation, TurnInput},
    protocol::{AuthStatus, Completion, TurnOutcome},
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Instant,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    Closed,
    Stale,
    Busy,
    Exhausted,
    Expired,
    Cancelled,
    StatusRequired,
    Protocol,
    Process,
}

/// Native-only admission snapshot, not a renderer-provided authorization token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Scope {
    instance: u64,
    epoch: u64,
    account: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Ticket {
    scope: Scope,
    sequence: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    Preparing,
    Running,
    Stopping,
    StopUnconfirmed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct View {
    pub status: Option<AuthStatus>,
    pub operation: Option<Operation>,
    pub phase: Option<Phase>,
    pub closed: bool,
}

/// The selected authentication method is explicit, including original CLI API
/// and external methods. A Claude subscription never falls back to another mode.
pub(super) enum Request {
    ReadStatus,
    Login,
    Logout,
    Turn {
        input: TurnInput,
        selected: AuthStatus,
    },
}
impl Request {
    fn operation(&self) -> Operation {
        match self {
            Self::ReadStatus => Operation::ReadStatus,
            Self::Login => Operation::StartLogin,
            Self::Logout => Operation::Logout,
            Self::Turn { .. } => Operation::TextTurn,
        }
    }
}

/// Status must already have passed the original-client parser, exit-code and
/// native-owned configuration-directory checks. Login/logout exit alone is not
/// authentication evidence. No raw stdout, credential or browser URL is exposed.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Reply {
    Status(AuthStatus),
    LoginFinished,
    LogoutFinished,
    Turn(TurnOutcome),
}
impl Reply {
    fn operation(&self) -> Operation {
        match self {
            Self::Status(_) => Operation::ReadStatus,
            Self::LoginFinished => Operation::StartLogin,
            Self::LogoutFinished => Operation::Logout,
            Self::Turn(_) => Operation::TextTurn,
        }
    }
}

/// Private blocking-worker contract, not process authority. A qualified adapter
/// must return ONLY after never-created proof or child exit + all pipe/writer
/// settlement. Uncertain custody keeps the worker blocked. It must check Control
/// before native creation and retain any process created during a cancellation.
/// There is no production implementation while native qualification is closed.
pub(super) trait Port {
    fn run(&mut self, request: Request, control: &Control) -> Result<Reply, Error>;
}

/// The native worker reads this fence; a provider/renderer cannot reset it.
pub(super) struct Control {
    owner: Owner,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}
impl Control {
    /// Immutable native resource owner; never supplied by renderer input.
    pub fn owner(&self) -> Owner {
        self.owner
    }
    pub fn cancelled(&self, now: Instant) -> bool {
        self.cancelled.load(Ordering::Acquire) || now >= self.deadline
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

struct Active {
    ticket: Ticket,
    operation: Operation,
    phase: Phase,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    // The same profile-wide lease used by the existing login controller.
    _admission: Arc<Admission>,
}
struct State {
    scope: Scope,
    next: u64,
    foreground: bool,
    closed: bool,
    status: Option<AuthStatus>,
    status_revision: Option<u64>,
    active: Option<Active>,
}
struct Inner {
    owner: Owner,
    state: Mutex<State>,
}
impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        // No callbacks/port work run under this lock. Poison still fences the
        // state, never silently restores admission or a connected projection.
        match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                let mut state = error.into_inner();
                state.closed = true;
                state.foreground = false;
                state.status = None;
                stop(&mut state);
                state
            }
        }
    }
    fn abandon(&self, ticket: Ticket) {
        let mut state = self.state();
        if state.active.as_ref().is_some_and(|a| a.ticket == ticket) {
            state.active = None;
        }
    }
    fn uncertain(&self, ticket: Ticket, admission: Arc<Admission>) {
        auth::retain_uncertain(admission);
        let mut state = self.state();
        state.closed = true;
        state.foreground = false;
        state.status = None;
        if let Some(active) = state.active.as_mut().filter(|a| a.ticket == ticket) {
            active.cancelled.store(true, Ordering::Release);
            active.phase = Phase::StopUnconfirmed;
        }
    }
}
fn stop(state: &mut State) {
    if let Some(active) = state.active.as_mut() {
        active.cancelled.store(true, Ordering::Release);
        if active.phase != Phase::StopUnconfirmed {
            active.phase = Phase::Stopping;
        }
    }
}
fn forget_status(state: &mut State) {
    state.status = None;
    state.status_revision = None;
}
fn admitted(state: &State, scope: Scope) -> Result<(), Error> {
    if state.closed {
        Err(Error::Closed)
    } else if !state.foreground || state.scope != scope {
        Err(Error::Stale)
    } else {
        Ok(())
    }
}

/// One native profile lifetime. No constructor is exported outside this crate;
/// persistent Windows configuration/cross-process ownership remain unqualified.
pub(super) struct AccountOwner {
    inner: Arc<Inner>,
}
impl AccountOwner {
    pub(super) fn native_owner(&self) -> Owner {
        self.inner.owner
    }

    pub fn new(owner: Owner) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let instance = NEXT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| Error::Exhausted)?;
        Ok(Self {
            inner: Arc::new(Inner {
                owner,
                state: Mutex::new(State {
                    scope: Scope {
                        instance,
                        epoch: 0,
                        account: 0,
                    },
                    next: 0,
                    foreground: true,
                    closed: false,
                    status: None,
                    status_revision: None,
                    active: None,
                }),
            }),
        })
    }
    pub fn scope(&self) -> Result<Scope, Error> {
        let state = self.inner.state();
        admitted(&state, state.scope)?;
        Ok(state.scope)
    }
    pub fn view(&self) -> View {
        let state = self.inner.state();
        View {
            status: if state.foreground
                && !state.closed
                && state.status_revision.is_some_and(|revision| {
                    auth::revision_is_current(self.inner.owner.profile, revision)
                }) {
                state.status
            } else {
                None
            },
            operation: state.active.as_ref().map(|a| a.operation),
            phase: state.active.as_ref().map(|a| a.phase),
            closed: state.closed,
        }
    }
    pub fn begin(&self, scope: Scope, request: Request, now: Instant) -> Result<Work, Error> {
        let mut state = self.inner.state();
        admitted(&state, scope)?;
        if state.active.is_some() {
            return Err(Error::Busy);
        }
        if let Request::Turn { selected, .. } = &request {
            if matches!(selected, AuthStatus::SignedOut | AuthStatus::Unknown)
                || state.status != Some(*selected)
            {
                return Err(Error::StatusRequired);
            }
        }
        let operation = request.operation();
        let deadline = now
            .checked_add(CommandPlan::for_operation(operation).deadline())
            .ok_or(Error::Expired)?;
        let next = state.next.checked_add(1).ok_or(Error::Exhausted)?;
        let account = if operation == Operation::TextTurn {
            state.scope.account
        } else {
            state.scope.account.checked_add(1).ok_or(Error::Exhausted)?
        };
        let expected_revision = if operation == Operation::TextTurn {
            Some(state.status_revision.ok_or(Error::StatusRequired)?)
        } else {
            None
        };
        let admission =
            match auth::reserve_admission_for(self.inner.owner.profile, expected_revision) {
                Ok(admission) => admission,
                Err(auth::AuthError::Busy) => return Err(Error::Busy),
                Err(auth::AuthError::Stale) => {
                    state.status = None;
                    state.status_revision = None;
                    return Err(Error::StatusRequired);
                }
                Err(auth::AuthError::Exhausted) => return Err(Error::Exhausted),
                Err(_) => return Err(Error::Closed),
            };
        state.next = next;
        state.scope.account = account;
        if operation != Operation::TextTurn {
            // Even a later failed auth mutation may have changed native config.
            // Status readback also retires prior model/account selections.
            state.status = None;
        }
        state.status_revision = Some(admission.revision());
        let ticket = Ticket {
            scope: state.scope,
            sequence: next,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        state.active = Some(Active {
            ticket,
            operation,
            phase: Phase::Preparing,
            cancelled: Arc::clone(&cancelled),
            deadline,
            _admission: Arc::clone(&admission),
        });
        Ok(Work {
            inner: Arc::clone(&self.inner),
            ticket,
            request: Some(request),
            control: Control {
                owner: self.inner.owner,
                cancelled,
                deadline,
            },
            admission,
            started: false,
            finished: false,
        })
    }
    pub fn cancel(&self, ticket: Ticket) -> Result<(), Error> {
        let mut state = self.inner.state();
        let active = state
            .active
            .as_mut()
            .filter(|a| a.ticket == ticket)
            .ok_or(Error::Stale)?;
        active.cancelled.store(true, Ordering::Release);
        if active.phase != Phase::StopUnconfirmed {
            active.phase = Phase::Stopping;
        }
        Ok(())
    }
    pub fn stop_unconfirmed(&self, ticket: Ticket) -> Result<(), Error> {
        let mut state = self.inner.state();
        let active = state
            .active
            .as_mut()
            .filter(|a| a.ticket == ticket)
            .ok_or(Error::Stale)?;
        if active.phase != Phase::Stopping {
            return Err(Error::Protocol);
        }
        active.phase = Phase::StopUnconfirmed;
        Ok(())
    }
    pub fn expire(&self, now: Instant) {
        let mut state = self.inner.state();
        if state.active.as_ref().is_some_and(|a| now >= a.deadline) {
            stop(&mut state);
        }
    }
    pub fn set_foreground(&self, foreground: bool) {
        let mut state = self.inner.state();
        if state.closed {
            stop(&mut state);
            return;
        }
        if state.foreground && !foreground {
            state.scope.epoch = match state.scope.epoch.checked_add(1) {
                Some(epoch) => epoch,
                None => {
                    state.closed = true;
                    state.scope.epoch
                }
            };
            state.status = None;
            stop(&mut state);
        }
        state.foreground = foreground && !state.closed;
    }
    pub fn close(&self) {
        let mut state = self.inner.state();
        state.closed = true;
        state.foreground = false;
        state.status = None;
        stop(&mut state);
    }
    pub fn drained(&self) -> bool {
        self.inner.state().active.is_none()
    }
}
impl Drop for AccountOwner {
    fn drop(&mut self) {
        self.close();
    }
}

/// This non-cloneable owner must be moved into the blocking native worker. Its
/// drop before run proves no port was invoked; drop during unwinding does not.
pub(super) struct Work {
    inner: Arc<Inner>,
    ticket: Ticket,
    request: Option<Request>,
    control: Control,
    admission: Arc<Admission>,
    started: bool,
    finished: bool,
}
impl Work {
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }
    pub fn run(mut self, port: &mut impl Port, now: impl Fn() -> Instant) -> Result<Reply, Error> {
        {
            let mut state = self.inner.state();
            admitted(&state, self.ticket.scope)?;
            let active = state
                .active
                .as_mut()
                .filter(|a| a.ticket == self.ticket)
                .ok_or(Error::Stale)?;
            if now() >= active.deadline {
                return Err(Error::Expired);
            }
            if active.cancelled.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            active.phase = Phase::Running;
        }
        self.started = true;
        let request = self.request.take().ok_or(Error::Protocol)?;
        // No native lock crosses the blocking worker or user interaction. The
        // worker rechecks Control before creation and owns a racing child.
        let result = port.run(request, &self.control);
        let mut state = self.inner.state();
        if !state
            .active
            .as_ref()
            .is_some_and(|a| a.ticket == self.ticket)
        {
            return Err(Error::Stale);
        }
        let active = state.active.take().ok_or(Error::Stale)?;
        self.finished = true; // Port returned: physical custody is settled.
        if let Err(error) = admitted(&state, self.ticket.scope) {
            forget_status(&mut state);
            return Err(error);
        }
        if now() >= active.deadline {
            forget_status(&mut state);
            return Err(Error::Expired);
        }
        if active.cancelled.load(Ordering::Acquire) {
            forget_status(&mut state);
            return Err(Error::Cancelled);
        }
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                forget_status(&mut state);
                return Err(error);
            }
        };
        if reply.operation() != active.operation {
            forget_status(&mut state);
            return Err(Error::Protocol);
        }
        if matches!(&reply, Reply::Turn(outcome) if
            matches!(outcome.completion, Completion::ProviderError | Completion::Interrupted)
            || matches!(outcome.api_error_status, Some(401 | 403)))
        {
            // A settled turn is not fresh authentication proof. Retain its
            // terminal classification, but require original status readback
            // before another explicitly selected auth mode can be used.
            forget_status(&mut state);
        }
        if let Reply::Status(status) = reply {
            state.status = Some(status);
        }
        Ok(reply)
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if self.started {
            self.inner
                .uncertain(self.ticket, Arc::clone(&self.admission));
        } else {
            self.inner.abandon(self.ticket);
        }
    }
}

#[cfg(test)]
mod tests;
