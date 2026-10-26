//! Process-free custody model. Cancel is a request, not proof that a child exited.
use crate::plan::Operation;

/// Native owner and generation tuple. Renderer-provided values are not authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Owner {
    /// Isolated native profile identity (trial and ordinary app must differ).
    pub profile: u128,
    /// Account replacement generation within that profile.
    pub account_generation: u64,
    /// Native profile lifetime generation.
    pub profile_generation: u64,
}
/// Exact operation identity, including its native owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ticket {
    owner: Owner,
    sequence: u64,
}
/// Physical state of the occupied process slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// Reserved before process creation; a deadline is already running.
    Reserved,
    /// Child process is owned and running.
    Running,
    /// Termination requested; exit has not been observed.
    StopRequested,
    /// Termination could not yet be established; slot remains occupied.
    StopUnconfirmed,
    /// Exit observed, but a pipe/writer still holds custody.
    ExitedPendingPipes,
}
/// Closed ownership/custody failure, without account or text data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustodyError {
    /// Another operation still owns the physical slot.
    Busy,
    /// Caller, generation or operation identity does not match.
    Stale,
    /// Invalid ordering; cannot claim exit or release from this state.
    InvalidTransition,
    /// The monotonic operation identity is exhausted.
    Exhausted,
}
struct Active {
    created: bool,
    ticket: Ticket,
    operation: Operation,
    phase: Phase,
    exit: Option<i32>,
    pipes: [bool; 3],
}
/// One serialized account-operation slot; dropping a UI view cannot release it.
#[derive(Default)]
pub struct Custody {
    next: u64,
    active: Option<Active>,
}
impl Custody {
    /// Reserve before any native preparation or spawn.
    pub fn reserve(&mut self, owner: Owner, operation: Operation) -> Result<Ticket, CustodyError> {
        if self.active.is_some() {
            return Err(CustodyError::Busy);
        }
        self.next = self.next.checked_add(1).ok_or(CustodyError::Exhausted)?;
        let ticket = Ticket {
            owner,
            sequence: self.next,
        };
        self.active = Some(Active {
            created: false,
            ticket,
            operation,
            phase: Phase::Reserved,
            exit: None,
            pipes: [false; 3],
        });
        Ok(ticket)
    }
    fn matching(&mut self, ticket: Ticket) -> Result<&mut Active, CustodyError> {
        self.active
            .as_mut()
            .filter(|active| active.ticket == ticket)
            .ok_or(CustodyError::Stale)
    }
    /// Whether creation is still admitted. The native owner rechecks immediately
    /// before CreateProcess and still records a successful in-flight creation.
    pub fn may_create(&mut self, ticket: Ticket) -> Result<bool, CustodyError> {
        let active = self.matching(ticket)?;
        Ok(!active.created && active.phase == Phase::Reserved)
    }
    /// Record actual successful creation, including a cancellation racing it.
    /// Cancellation intent is retained; an observed child can never be abandoned
    /// as an uncreated reservation merely because the UI canceled first.
    pub fn spawned(&mut self, ticket: Ticket) -> Result<(), CustodyError> {
        let active = self.matching(ticket)?;
        if active.created || active.exit.is_some() {
            return Err(CustodyError::InvalidTransition);
        }
        active.created = true;
        if active.phase == Phase::Reserved {
            active.phase = Phase::Running;
        }
        Ok(())
    }
    /// Abort only a reservation proved never to have created a process.
    pub fn abandon_before_spawn(&mut self, ticket: Ticket) -> Result<(), CustodyError> {
        if self.matching(ticket)?.created {
            return Err(CustodyError::InvalidTransition);
        }
        self.active = None;
        Ok(())
    }
    /// Request cancellation without releasing the slot.
    pub fn request_stop(&mut self, ticket: Ticket) -> Result<(), CustodyError> {
        let active = self.matching(ticket)?;
        if active.exit.is_none() {
            active.phase = Phase::StopRequested;
        }
        Ok(())
    }
    /// Record a failed/uncertain stop while retaining ownership.
    pub fn stop_unconfirmed(&mut self, ticket: Ticket) -> Result<(), CustodyError> {
        let active = self.matching(ticket)?;
        if active.phase != Phase::StopRequested {
            return Err(CustodyError::InvalidTransition);
        }
        active.phase = Phase::StopUnconfirmed;
        Ok(())
    }
    /// Record actual native process exit; never infer it from stream completion.
    pub fn observed_exit(&mut self, ticket: Ticket, code: i32) -> Result<(), CustodyError> {
        let active = self.matching(ticket)?;
        if !active.created || active.exit.is_some() {
            return Err(CustodyError::InvalidTransition);
        }
        active.exit = Some(code);
        active.phase = Phase::ExitedPendingPipes;
        Ok(())
    }
    /// Record one settled transport end: stdin writer=0, stdout=1, stderr=2.
    pub fn settled_pipe(&mut self, ticket: Ticket, pipe: usize) -> Result<(), CustodyError> {
        let active = self.matching(ticket)?;
        let settled = active
            .pipes
            .get_mut(pipe)
            .ok_or(CustodyError::InvalidTransition)?;
        if *settled {
            return Err(CustodyError::InvalidTransition);
        }
        *settled = true;
        Ok(())
    }
    /// Release only after exit and every pipe owner has settled.
    pub fn release(&mut self, ticket: Ticket) -> Result<i32, CustodyError> {
        let active = self.matching(ticket)?;
        if !active.pipes.iter().all(|done| *done) {
            return Err(CustodyError::InvalidTransition);
        }
        let code = active.exit.ok_or(CustodyError::InvalidTransition)?;
        self.active = None;
        Ok(code)
    }
    /// Current physical state, without exposing user data.
    pub fn phase(&self) -> Option<Phase> {
        self.active.as_ref().map(|a| a.phase)
    }
    /// Operation holding custody, including auth mutation versus inference.
    pub fn operation(&self) -> Option<Operation> {
        self.active.as_ref().map(|a| a.operation)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> Owner {
        Owner {
            profile: 1,
            account_generation: 2,
            profile_generation: 3,
        }
    }
    #[test]
    fn cancel_does_not_release_custody_and_all_pipes_are_required() {
        let mut slot = Custody::default();
        let ticket = slot.reserve(owner(), Operation::TextTurn).unwrap();
        slot.spawned(ticket).unwrap();
        slot.request_stop(ticket).unwrap();
        slot.stop_unconfirmed(ticket).unwrap();
        assert_eq!(
            slot.reserve(owner(), Operation::Logout),
            Err(CustodyError::Busy)
        );
        assert_eq!(slot.release(ticket), Err(CustodyError::InvalidTransition));
        slot.observed_exit(ticket, 1).unwrap();
        for pipe in 0..2 {
            slot.settled_pipe(ticket, pipe).unwrap();
        }
        assert_eq!(slot.release(ticket), Err(CustodyError::InvalidTransition));
        slot.settled_pipe(ticket, 2).unwrap();
        assert_eq!(slot.release(ticket), Ok(1));
        assert!(slot.reserve(owner(), Operation::Logout).is_ok());
    }
    #[test]
    fn cancellation_before_creation_can_release_only_proven_uncreated_reservation() {
        let mut slot = Custody::default();
        let ticket = slot.reserve(owner(), Operation::StartLogin).unwrap();
        slot.request_stop(ticket).unwrap();
        assert!(!slot.may_create(ticket).unwrap());
        slot.abandon_before_spawn(ticket).unwrap();
        let next = slot.reserve(owner(), Operation::ReadStatus).unwrap();
        slot.spawned(next).unwrap();
        slot.request_stop(next).unwrap();
        assert_eq!(
            slot.abandon_before_spawn(next),
            Err(CustodyError::InvalidTransition)
        );
    }
    #[test]
    fn no_stale_account_profile_or_operation_completion() {
        let mut slot = Custody::default();
        let old = slot.reserve(owner(), Operation::StartLogin).unwrap();
        slot.abandon_before_spawn(old).unwrap();
        let new = slot.reserve(owner(), Operation::ReadStatus).unwrap();
        assert_eq!(slot.spawned(old), Err(CustodyError::Stale));
        let mut foreign = new;
        foreign.owner.profile_generation += 1;
        assert_eq!(slot.request_stop(foreign), Err(CustodyError::Stale));
        slot.request_stop(new).unwrap();
        assert!(!slot.may_create(new).unwrap());
        slot.spawned(new).unwrap(); // cancellation raced an already in-flight create
        assert_eq!(slot.phase(), Some(Phase::StopRequested));
        assert_eq!(
            slot.abandon_before_spawn(new),
            Err(CustodyError::InvalidTransition)
        );
        assert_eq!(
            slot.reserve(owner(), Operation::Logout),
            Err(CustodyError::Busy)
        );
    }
}
