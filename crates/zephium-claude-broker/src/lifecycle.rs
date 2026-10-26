//! Native trial/profile actor retention. The public facade cannot prepare stores
//! or run a provider while the immutable production admission gate is closed.
use crate::account::{self, AccountOwner, Port, Reply, Request};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

type Profile = [u8; 16];
const PREPARATION_LIMIT: Duration = Duration::from_secs(120);

/// Profile removal evidence, not a credential-deletion command or receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Retirement {
    /// The always-closed gate admitted no registration for this profile.
    /// This is not inferred merely from an empty registry after an enabled run.
    NoStoreUnderClosedGate,
    /// A preparation or account worker has not actually returned/settled.
    Pending,
    /// Persistent storage may exist; no qualified deletion has proved erasure.
    StorageUnconfirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Closed,
    Stale,
    Busy,
    Exhausted,
    Preparation,
    Expired,
}

/// Only native registry transitions construct this scope. It is never IPC data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeScope {
    instance: u64,
    profile: Profile,
    epoch: u64,
}
impl NativeScope {
    pub(crate) fn profile(self) -> Profile {
        self.profile
    }
}
struct Pending {
    scope: NativeScope,
    cancelled: Arc<AtomicBool>,
    uncertain: bool,
    _admission: Arc<crate::auth::Admission>,
}
#[derive(Default)]
struct Entry {
    retired: bool,
    storage_possible: bool,
    preparation: Option<Pending>,
    resource: Option<Arc<Resource>>,
}
impl Entry {
    fn stop(&mut self) {
        if let Some(pending) = &self.preparation {
            pending.cancelled.store(true, Ordering::Release);
        }
        if let Some(resource) = &self.resource {
            resource.owner.close();
        }
    }
    fn drained(&self) -> bool {
        self.preparation.is_none() && self.resource.as_ref().is_none_or(|r| r.owner.drained())
    }
}
struct State {
    instance: u64,
    closed: bool,
    epoch: u64,
    selected: Option<NativeScope>,
    profiles: BTreeSet<Profile>,
    entries: BTreeMap<Profile, Entry>,
    // Explicit compiled-gate invariant, not absence inferred from a map. Any
    // future enabled construction starts conservatively with unknown history.
    closed_gate_without_history: bool,
}
struct Inner(Mutex<State>);
impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        match self.0.lock() {
            Ok(state) => state,
            Err(error) => {
                let mut state = error.into_inner();
                state.closed = true;
                state.selected = None;
                for entry in state.entries.values_mut() {
                    entry.stop();
                }
                state
            }
        }
    }
}

/// Native-only desktop trial lifecycle. This facade performs no discovery,
/// namespace I/O, authentication, provider invocation or credential deletion.
/// Dropped views cannot release preparation or account-worker custody.
pub struct TrialAccounts {
    inner: Arc<Inner>,
}
impl Default for TrialAccounts {
    fn default() -> Self {
        Self::new()
    }
}
impl TrialAccounts {
    /// Construct the currently closed trial registry without touching disk.
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let instance = NEXT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .ok();
        Self {
            inner: Arc::new(Inner(Mutex::new(State {
                instance: instance.unwrap_or(0),
                closed: instance.is_none(),
                epoch: 0,
                selected: None,
                profiles: BTreeSet::new(),
                entries: BTreeMap::new(),
                closed_gate_without_history: crate::availability().is_err(),
            }))),
        }
    }
    /// Apply the authoritative native profile snapshot. An away/back transition
    /// creates a new native epoch; removed profile identities never revive.
    pub fn profiles_changed(&self, profiles: &[Profile], active: Option<Profile>) {
        let mut state = self.inner.state();
        if state.closed {
            return;
        }
        let profiles: BTreeSet<_> = profiles.iter().copied().collect();
        let removed: Vec<_> = state.profiles.difference(&profiles).copied().collect();
        for profile in removed {
            let entry = state.entries.entry(profile).or_default();
            entry.retired = true;
            entry.stop();
        }
        state.profiles = profiles;
        let active = active.filter(|p| {
            state.profiles.contains(p) && !state.entries.get(p).is_some_and(|e| e.retired)
        });
        if state.selected.map(|scope| scope.profile) == active {
            return;
        }
        if let Some(previous) = state.selected.take() {
            if let Some(entry) = state.entries.get_mut(&previous.profile) {
                entry.stop();
            }
        }
        let Some(epoch) = state.epoch.checked_add(1) else {
            state.closed = true;
            for entry in state.entries.values_mut() {
                entry.stop();
            }
            return;
        };
        state.epoch = epoch;
        state.selected = active.map(|profile| NativeScope {
            instance: state.instance,
            profile,
            epoch,
        });
    }
    /// Permanently close admission and request cancellation. This is not evidence
    /// that a worker returned, a process exited or persistent storage was erased.
    pub fn close(&self) {
        let mut state = self.inner.state();
        state.closed = true;
        state.selected = None;
        for entry in state.entries.values_mut() {
            entry.stop();
        }
    }
    /// True only after close and every tracked preparation/account obligation
    /// has actually returned. It says nothing about persistent data deletion.
    pub fn closed_and_drained(&self) -> bool {
        let state = self.inner.state();
        state.closed && state.entries.values().all(Entry::drained)
    }
    /// Fence this native profile forever and report removal evidence. Only the
    /// never-admitted closed-gate case permits the host's unused-profile removal.
    pub fn retire_profile(&self, profile: Profile) -> Retirement {
        let mut state = self.inner.state();
        if state.selected.is_some_and(|scope| scope.profile == profile) {
            state.selected = None;
        }
        let closed_gate_without_history = state.closed_gate_without_history;
        let entry = state.entries.entry(profile).or_default();
        entry.retired = true;
        entry.stop();
        if !entry.drained() {
            Retirement::Pending
        } else if entry.storage_possible || !closed_gate_without_history {
            Retirement::StorageUnconfirmed
        } else {
            Retirement::NoStoreUnderClosedGate
        }
    }
    pub(crate) fn current_scope(&self) -> Result<NativeScope, Error> {
        let state = self.inner.state();
        if state.closed {
            return Err(Error::Closed);
        }
        state.selected.ok_or(Error::Stale)
    }
    /// Private future adapter seam; never called by the public closed facade.
    /// The reservation is installed before the caller can enter any factory I/O.
    pub(crate) fn prepare(&self, scope: NativeScope) -> Result<Preparation, Error> {
        self.prepare_at(scope, Instant::now())
    }
    fn prepare_at(&self, scope: NativeScope, now: Instant) -> Result<Preparation, Error> {
        let deadline = now.checked_add(PREPARATION_LIMIT).ok_or(Error::Exhausted)?;
        let old_resource;
        let preparation;
        {
            let mut state = self.inner.state();
            admit(&state, scope)?;
            let entry = state.entries.entry(scope.profile).or_default();
            if entry.retired {
                return Err(Error::Stale);
            }
            if !entry.drained() {
                return Err(Error::Busy);
            }
            let admission = crate::auth::reserve_admission(u128::from_be_bytes(scope.profile))
                .map_err(|error| match error {
                    crate::auth::AuthError::Busy => Error::Busy,
                    crate::auth::AuthError::Exhausted => Error::Exhausted,
                    _ => Error::Closed,
                })?;
            if let Some(resource) = &entry.resource {
                resource.owner.close();
            }
            old_resource = entry.resource.take();
            entry.storage_possible = true;
            let cancelled = Arc::new(AtomicBool::new(false));
            entry.preparation = Some(Pending {
                scope,
                cancelled: Arc::clone(&cancelled),
                uncertain: false,
                _admission: Arc::clone(&admission),
            });
            preparation = Preparation {
                inner: Arc::clone(&self.inner),
                control: PreparationControl {
                    scope,
                    cancelled,
                    deadline,
                },
                started: false,
                finished: false,
                admission,
            };
        }
        drop(old_resource); // Never run Port/resource destructors under state lock.
        Ok(preparation)
    }
    pub(crate) fn begin(
        &self,
        scope: NativeScope,
        request: Request,
        now: Instant,
    ) -> Result<OperationWork, Error> {
        let state = self.inner.state();
        admit(&state, scope)?;
        let resource = state
            .entries
            .get(&scope.profile)
            .filter(|entry| !entry.retired && entry.preparation.is_none())
            .and_then(|entry| entry.resource.as_ref())
            .filter(|resource| resource.scope == scope)
            .ok_or(Error::Stale)?;
        let work = resource
            .owner
            .begin(
                resource.owner.scope().map_err(|_| Error::Stale)?,
                request,
                now,
            )
            .map_err(|_| Error::Busy)?;
        Ok(OperationWork {
            inner: Arc::clone(&self.inner),
            resource: Arc::clone(resource),
            work: Some(work),
            port_entered: false,
            call_returned: false,
        })
    }
}
impl Drop for TrialAccounts {
    fn drop(&mut self) {
        self.close();
    }
}
fn admit(state: &State, scope: NativeScope) -> Result<(), Error> {
    if state.closed {
        Err(Error::Closed)
    } else if state.selected != Some(scope) {
        Err(Error::Stale)
    } else {
        Ok(())
    }
}

/// Trusted blocking preparation receives only native scope and cancellation.
/// Recheck before native creation; return only after all preparation work ended.
/// An adapter must retain any ambiguous native custody itself, just like Port.
pub(crate) trait Factory {
    fn prepare(&mut self, control: &PreparationControl) -> Result<Resource, Error>;
}
pub(crate) struct PreparationControl {
    scope: NativeScope,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}
impl PreparationControl {
    pub(crate) fn scope(&self) -> NativeScope {
        self.scope
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn cancelled(&self, now: Instant) -> bool {
        self.cancelled.load(Ordering::Acquire) || now >= self.deadline
    }
}
/// Pairs the actual account owner with its retained native Port. The private
/// factory adapter must validate profile/key binding when constructing the pair.
pub(crate) struct Resource {
    scope: NativeScope,
    owner: AccountOwner,
    port: Mutex<Box<dyn Port + Send>>,
}
impl Resource {
    pub(crate) fn new(
        scope: NativeScope,
        owner: AccountOwner,
        port: impl Port + Send + 'static,
    ) -> Result<Self, Error> {
        if owner.native_owner().profile.to_be_bytes() != scope.profile {
            owner.close();
            return Err(Error::Stale);
        }
        Ok(Self {
            scope,
            owner,
            port: Mutex::new(Box::new(port)),
        })
    }
}
pub(crate) struct Preparation {
    inner: Arc<Inner>,
    admission: Arc<crate::auth::Admission>,
    control: PreparationControl,
    started: bool,
    finished: bool,
}
impl Preparation {
    pub(crate) fn run(self, factory: &mut impl Factory) -> Result<(), Error> {
        self.run_at(factory, Instant::now)
    }
    fn run_at(
        mut self,
        factory: &mut impl Factory,
        now: impl Fn() -> Instant,
    ) -> Result<(), Error> {
        {
            let state = self.inner.state();
            admit(&state, self.control.scope)?;
            let now = now();
            if now >= self.control.deadline() {
                return Err(Error::Expired);
            }
            if self.control.cancelled(now) {
                return Err(Error::Stale);
            }
        }
        self.started = true;
        let result = factory.prepare(&self.control);
        let mut state = self.inner.state();
        let now = now();
        let admission = admit(&state, self.control.scope).and_then(|()| {
            if now >= self.control.deadline() {
                Err(Error::Expired)
            } else if self.control.cancelled(now) {
                Err(Error::Stale)
            } else {
                Ok(())
            }
        });
        let entry = state
            .entries
            .get_mut(&self.control.scope.profile)
            .ok_or(Error::Stale)?;
        if !entry
            .preparation
            .as_ref()
            .is_some_and(|p| p.scope == self.control.scope)
        {
            return Err(Error::Stale);
        }
        // Actual return, not cancellation/timeout, discharges preparation.
        entry.preparation = None;
        self.finished = true;
        match result {
            Ok(resource) => {
                let valid =
                    admission.is_ok() && !entry.retired && resource.scope == self.control.scope;
                if !valid {
                    resource.owner.close();
                }
                entry.resource = Some(Arc::new(resource));
                if valid {
                    Ok(())
                } else {
                    admission.and(Err(Error::Stale))
                }
            }
            Err(error) => admission.and(Err(error)),
        }
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut state = self.inner.state();
        let Some(entry) = state.entries.get_mut(&self.control.scope.profile) else {
            return;
        };
        if !entry
            .preparation
            .as_ref()
            .is_some_and(|p| p.scope == self.control.scope)
        {
            return;
        }
        if self.started {
            crate::auth::retain_uncertain(Arc::clone(&self.admission));
            let pending = entry.preparation.as_mut().expect("matching preparation");
            pending.uncertain = true;
            pending.cancelled.store(true, Ordering::Release);
            entry.retired = true;
            // Panic/drop is not factory return. Preserve obligation and resources
            // independently of the facade/waiter until process teardown.
            std::mem::forget(Arc::clone(&self.inner));
        } else {
            entry.preparation = None; // Factory was never entered.
        }
    }
}
struct BorrowedPort<'a> {
    port: &'a mut dyn Port,
    entered: &'a mut bool,
}
impl Port for BorrowedPort<'_> {
    fn run(
        &mut self,
        request: Request,
        control: &account::Control,
    ) -> Result<Reply, account::Error> {
        *self.entered = true;
        self.port.run(request, control)
    }
}
pub(crate) struct OperationWork {
    inner: Arc<Inner>,
    resource: Arc<Resource>,
    work: Option<account::Work>,
    port_entered: bool,
    call_returned: bool,
}
impl OperationWork {
    pub(crate) fn run(mut self) -> Result<Reply, account::Error> {
        self.run_inner()
    }
    fn run_inner(&mut self) -> Result<Reply, account::Error> {
        let mut port = self
            .resource
            .port
            .lock()
            .map_err(|_| account::Error::Closed)?;
        let result = self.work.take().ok_or(account::Error::Protocol)?.run(
            &mut BorrowedPort {
                port: port.as_mut(),
                entered: &mut self.port_entered,
            },
            Instant::now,
        );
        self.call_returned = true;
        result
    }
}
impl Drop for OperationWork {
    fn drop(&mut self) {
        drop(self.work.take());
        if self.port_entered && !self.call_returned {
            // Retain only THIS call's entered-but-unreturned obligation. A newer
            // admission can already occupy the AccountOwner after our return or
            // never-entered Work release; its state is not our settlement proof.
            // AccountOwner's Work independently marks started unwind uncertain.
            std::mem::forget(Arc::clone(&self.inner));
        }
    }
}

#[cfg(test)]
mod tests;
