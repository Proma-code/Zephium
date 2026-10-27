//! Fixed explicit-action budgets. These confer no method/config/turn authority.
use super::*;
use crate::demand_budget::DemandBudget;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Check,
    Logout,
    Login,
}
pub(super) struct AccountDemand {
    pub(super) budget: DemandBudget,
    pub(super) kind: Kind,
}
pub(super) fn fault(value: crate::demand_budget::Fault) -> Fault {
    match value {
        crate::demand_budget::Fault::Scope => Fault::ScopeChanged,
        crate::demand_budget::Fault::Cancelled => Fault::Cancelled,
        crate::demand_budget::Fault::Expired => Fault::Timeout,
    }
}
impl Connection {
    fn demand(
        &self,
        scope: &Scope,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<DemandBudget, Fault> {
        self.ordinary_current(scope)?;
        if self.wake_deadline.is_some() {
            return Err(Fault::Busy);
        }
        DemandBudget::new(scope.clone(), cancel.clone(), deadline, Instant::now()).map_err(fault)
    }
    fn account_action(
        &mut self,
        scope: &Scope,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
        kind: Kind,
    ) -> Result<(), Fault> {
        self.idle()?;
        self.account.removal_ready(scope).map_err(managed_fault)?;
        if self.account_demand.is_some() || self.catalog_demand.is_some() {
            return Err(Fault::Busy);
        }
        let budget = self.demand(scope, cancel, deadline)?;
        self.account_deadline = Some(budget.cap(Instant::now() + RPC));
        self.account_demand = Some(AccountDemand { budget, kind });
        Ok(())
    }
    pub fn read_account_for_demand(
        &mut self,
        scope: &Scope,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.account_action(scope, cancel, deadline, Kind::Check)?;
        let deadline = self.account_deadline.ok_or(Fault::Protocol)?;
        self.read_account_until(scope, cancel, deadline)
    }
    pub fn logout_for_demand(
        &mut self,
        scope: &Scope,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.account_action(scope, cancel, deadline, Kind::Logout)?;
        self.logout(scope, cancel)
    }
    pub fn login_for_demand(
        &mut self,
        scope: &Scope,
        ceremony_cancel: &Arc<AtomicBool>,
        action_cancel: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.account_action(scope, action_cancel, deadline, Kind::Login)?;
        self.login(scope, ceremony_cancel)
    }
    pub fn refresh_catalog_for_demand(
        &mut self,
        scope: &Scope,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.idle()?;
        if self.state() != AccountState::ChatGpt {
            return Err(Fault::NotConnected);
        }
        if self.account_demand.is_some() || self.catalog_demand.is_some() {
            return Err(Fault::Busy);
        }
        let budget = self.demand(scope, cancel, deadline)?;
        self.catalog_demand = Some(budget);
        self.refresh_catalog(scope, cancel)
    }
    pub fn request_quota_refresh_for_demand(
        &mut self,
        scope: &Scope,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<(), Fault> {
        self.ordinary_current(scope)?;
        if self.state() != AccountState::ChatGpt {
            self.publish_quota();
            return Ok(());
        }
        // A new refresh cannot replace the identity/bound of an owned request.
        if self.quota_demand.is_some()
            || self
                .pending
                .values()
                .any(|p| matches!(&p.kind,PendingKind::Quota(q) if q.ticket.is_some()))
        {
            self.publish_quota();
            return Ok(());
        }
        let budget = match self.demand(scope, cancel, deadline) {
            Ok(value) => value,
            Err(Fault::Cancelled | Fault::Timeout) => {
                self.quota_owner.abandon_manual(super::quota::wall_ms());
                self.publish_quota();
                return Ok(());
            }
            Err(value) => return Err(value),
        };
        self.quota_demand = Some(budget);
        self.quota_owner.manual();
        // Dispatch only in the existing poll loop, which supplies the distinct
        // service-stop signal; per-action cancellation is a final write fence.
        self.publish_quota();
        Ok(())
    }
    pub(super) fn check_demand_actions(&mut self) -> Result<(), Fault> {
        if self
            .wake_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return self.fail(Fault::Timeout);
        }
        let expired = [
            self.account_demand.as_ref().map(|v| &v.budget),
            self.catalog_demand.as_ref(),
        ]
        .into_iter()
        .flatten()
        .find_map(|budget| budget.check(&self.scope, Instant::now()).err());
        if let Some(value) = expired {
            return self.fail(fault(value));
        }
        if self
            .wake_cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
        {
            return self.fail(Fault::Cancelled);
        }
        Ok(())
    }
}
