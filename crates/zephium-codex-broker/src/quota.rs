//! Account-scoped quota observation, never model admission or pricing authority.
//!
//! Reuses provider-accounts' bounded Codex parser and presentation types. Both
//! official 0.163.0 (83086188) and 0.164.0 (6aaac694) define `updated` as sparse:
//! clients may merge available values OR refetch the complete read snapshot.
//! We deliberately refetch: a sparse update never discards unrelated buckets,
//! clears nullable metadata, or renews the timestamp of a last-good full read.
//!
//! The native owner supplies current Scope/account revision, correlated decoded
//! replies, monotonic scheduling time, and wall-clock observation time. This
//! module performs no I/O, starts no owner/login, and cannot redeem reset credits.

use crate::conversation::Scope;
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use zephium_provider_accounts::quota::{
    parse_codex_rate_limits, QuotaFault, QuotaProjection, QuotaProvenance, QuotaSource, QuotaState,
    QuotaUnavailable,
};

const MIN_READ_INTERVAL: Duration = Duration::from_secs(30);
const MAX_READS_PER_SCOPE: u16 = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    ScopeChanged,
    Retired,
    Busy,
    Cadence,
    ReadBudget,
    Ticket,
    Observation(QuotaFault),
}

/// Move-only, local correlation for one admitted read; not account authority.
/// The owner separately binds its global wire ID and actual response envelope.
pub(crate) struct ReadTicket {
    observer: Arc<()>,
    serial: u16,
    update_revision: u64,
}
impl std::fmt::Debug for ReadTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadTicket")
            .field("serial", &self.serial)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Applied {
    Current,
    /// An update arrived while this read was pending; keep stale and refetch
    /// only at the next admitted cadence. Never claim its values are current.
    Superseded,
}

pub(crate) struct AccountQuota {
    scope: Scope,
    provenance: QuotaProvenance,
    observer: Arc<()>,
    projection: QuotaProjection,
    retired: bool,
    reads: u16,
    pending: Option<u16>,
    last_started: Option<Instant>,
    update_revision: u64,
    refresh_needed: bool,
}

impl AccountQuota {
    /// Native actor only, after current original-account admission. Revisions
    /// are correlation, not proof; account/config changes must retire this owner.
    pub(crate) fn new(scope: Scope, provenance: QuotaProvenance) -> Result<Self, Fault> {
        if provenance.source != QuotaSource::CodexAppServer
            || provenance.profile_revision == 0
            || provenance.account_revision == 0
        {
            return Err(Fault::ScopeChanged);
        }
        Ok(Self {
            scope,
            provenance,
            observer: Arc::new(()),
            projection: QuotaProjection::new(provenance),
            retired: false,
            reads: 0,
            pending: None,
            last_started: None,
            update_revision: 0,
            refresh_needed: true,
        })
    }

    fn current(&self, scope: &Scope, account_revision: u64) -> Result<(), Fault> {
        if self.retired {
            return Err(Fault::Retired);
        }
        if scope != &self.scope || account_revision != self.provenance.account_revision {
            return Err(Fault::ScopeChanged);
        }
        Ok(())
    }

    /// A fixed observational request. No automatic model fallback experiment,
    /// reset-credit detail polling, redemption, paid request or auth operation.
    pub(crate) fn read_params() -> Value {
        json!({"supportsLunaReserve":false,"excludeResetCreditDetails":true})
    }

    pub(crate) fn begin_read(
        &mut self,
        scope: &Scope,
        account_revision: u64,
        now: Instant,
    ) -> Result<ReadTicket, Fault> {
        self.current(scope, account_revision)?;
        if self.pending.is_some() {
            return Err(Fault::Busy);
        }
        if self.reads >= MAX_READS_PER_SCOPE {
            return Err(Fault::ReadBudget);
        }
        if self.last_started.is_some_and(|last| {
            now.checked_duration_since(last)
                .is_none_or(|age| age < MIN_READ_INTERVAL)
        }) {
            return Err(Fault::Cadence);
        }
        self.reads += 1;
        self.last_started = Some(now);
        self.pending = Some(self.reads);
        Ok(ReadTicket {
            observer: self.observer.clone(),
            serial: self.reads,
            update_revision: self.update_revision,
        })
    }

    fn finish_ticket(
        &mut self,
        scope: &Scope,
        epoch: u64,
        ticket: ReadTicket,
    ) -> Result<bool, Fault> {
        self.current(scope, epoch)?;
        if !Arc::ptr_eq(&self.observer, &ticket.observer) || self.pending != Some(ticket.serial) {
            return Err(Fault::Ticket);
        }
        self.pending = None;
        Ok(ticket.update_revision == self.update_revision)
    }

    pub(crate) fn complete_read(
        &mut self,
        scope: &Scope,
        epoch: u64,
        ticket: ReadTicket,
        result: &Value,
        at_ms: u64,
    ) -> Result<Applied, Fault> {
        if !self.finish_ticket(scope, epoch, ticket)? {
            self.refresh_needed = true;
            return Ok(Applied::Superseded);
        }
        let result = checked_payload(result)
            .and_then(|bytes| parse_codex_rate_limits(&bytes, self.provenance, at_ms));
        match result {
            Ok(reading) => {
                if let Err(fault) = self.projection.record(reading) {
                    self.refresh_needed = true;
                    self.projection.mark_stale(at_ms, 0);
                    return Err(Fault::Observation(fault));
                }
                self.refresh_needed = false;
                Ok(Applied::Current)
            }
            Err(fault) => {
                self.refresh_needed = true;
                self.projection.mark_stale(at_ms, 0);
                self.projection
                    .fail(fault, at_ms)
                    .map_err(Fault::Observation)?;
                Err(Fault::Observation(fault))
            }
        }
    }

    pub(crate) fn fail_read(
        &mut self,
        scope: &Scope,
        epoch: u64,
        ticket: ReadTicket,
        fault: QuotaFault,
        at_ms: u64,
    ) -> Result<Applied, Fault> {
        let current = self.finish_ticket(scope, epoch, ticket)?;
        self.refresh_needed = true;
        if !current {
            return Ok(Applied::Superseded);
        }
        self.projection.mark_stale(at_ms, 0);
        self.projection
            .fail(fault, at_ms)
            .map_err(Fault::Observation)?;
        Ok(Applied::Current)
    }

    /// Returns true only on the transition to a needed refresh. Repeated updates
    /// coalesce; begin_read's independent cadence/budget still applies. The
    /// actor schedules this only on its existing live, authenticated owner.
    pub(crate) fn updated(
        &mut self,
        scope: &Scope,
        epoch: u64,
        params: &Value,
        at_ms: u64,
    ) -> Result<bool, Fault> {
        self.current(scope, epoch)?;
        self.update_revision = self
            .update_revision
            .checked_add(1)
            .ok_or(Fault::ReadBudget)?;
        let newly_needed = !self.refresh_needed;
        self.refresh_needed = true;
        self.projection.mark_stale(at_ms, 0);
        // The notification has one sparse bucket, NOT a replacement map. Do
        // not accept an additive map as authority to clear/refresh other buckets.
        let bucket = params
            .get("rateLimits")
            .filter(|value| value.is_object())
            .ok_or(QuotaFault::InvalidShape);
        let parsed = bucket
            .and_then(|bucket| checked_payload(&json!({"rateLimits":bucket})))
            .and_then(|bytes| parse_codex_rate_limits(&bytes, self.provenance, at_ms));
        if let Err(fault) = parsed {
            self.projection
                .fail(fault, at_ms)
                .map_err(Fault::Observation)?;
            return Err(Fault::Observation(fault));
        }
        Ok(newly_needed)
    }

    /// Explicit native refresh intent only. It is not an upstream update and
    /// does not invalidate an in-flight read, renew freshness, or reset cadence.
    pub(crate) fn request_refresh(&mut self, scope: &Scope, epoch: u64) -> Result<(), Fault> {
        self.current(scope, epoch)?;
        self.refresh_needed = true;
        Ok(())
    }

    pub(crate) fn state(&self) -> &QuotaState {
        self.projection.state()
    }
    pub(crate) fn needs_refresh(&self) -> bool {
        !self.retired && self.refresh_needed
    }
    pub(crate) fn mark_stale(&mut self, now_ms: u64, max_age_ms: u64) {
        self.projection.mark_stale(now_ms, max_age_ms);
    }
    pub(crate) fn retire(&mut self, reason: QuotaUnavailable) {
        self.retired = true;
        self.pending = None;
        self.refresh_needed = false;
        self.projection.make_unavailable(reason);
    }
}

fn checked_payload(value: &Value) -> Result<Vec<u8>, QuotaFault> {
    if !value.get("rateLimits").is_some_and(Value::is_object) {
        return Err(QuotaFault::InvalidShape);
    }
    let bytes = serde_json::to_vec(value).map_err(|_| QuotaFault::InvalidJson)?;
    if bytes.len() > zephium_provider_accounts::quota::MAX_QUOTA_BYTES {
        return Err(QuotaFault::MessageTooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "quota/tests.rs"]
mod tests;
