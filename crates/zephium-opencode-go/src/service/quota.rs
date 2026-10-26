//! Explicit, connection-scoped provider allowance observations. No polling or inference.
use super::*;
use zephium_agentic::lead::opencode_go::quota::{GoQuotaFault, GoQuotaSnapshot};

const FRESH_FOR_MS: u64 = 5 * 60 * 1000;

/// Closed allowance failures, independent of catalog and inference errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaFault {
    /// Invalid native request.
    Invalid,
    /// No saved connection.
    NotConfigured,
    /// The observed connection was replaced.
    Changed,
    /// Native credential storage failed.
    Vault,
    /// An explicit allowance check is already pending.
    Busy,
    /// Profile or request scope was retired.
    Cancelled,
    /// Provider refused this access key.
    Unauthorized,
    /// Provider explicitly reported no applicable allowance.
    NoEntitlement,
    /// Provider refused the request without an entitlement diagnosis.
    Forbidden,
    /// The first-party usage endpoint no longer supports this request.
    EndpointUnavailable,
    /// Provider limited the request; never automatically retried.
    RateLimited,
    /// Bounded observation expired.
    Timeout,
    /// Provider or transport unavailable.
    Unavailable,
    /// Malformed or unsupported provider observation.
    Protocol,
    /// Bounded response size exceeded.
    TooLarge,
}
impl From<GoQuotaFault> for QuotaFault {
    fn from(value: GoQuotaFault) -> Self {
        match value {
            GoQuotaFault::InvalidRequest => Self::Invalid,
            GoQuotaFault::Unauthorized => Self::Unauthorized,
            GoQuotaFault::NoEntitlement => Self::NoEntitlement,
            GoQuotaFault::Forbidden => Self::Forbidden,
            GoQuotaFault::EndpointUnavailable => Self::EndpointUnavailable,
            GoQuotaFault::RateLimited { .. } => Self::RateLimited,
            GoQuotaFault::Timeout => Self::Timeout,
            GoQuotaFault::Unavailable => Self::Unavailable,
            GoQuotaFault::Protocol => Self::Protocol,
            GoQuotaFault::TooLarge => Self::TooLarge,
        }
    }
}
impl From<Fault> for QuotaFault {
    fn from(value: Fault) -> Self {
        match value {
            Fault::Invalid => Self::Invalid,
            Fault::NotConfigured => Self::NotConfigured,
            Fault::Changed => Self::Changed,
            Fault::Vault => Self::Vault,
            Fault::Busy => Self::Busy,
            Fault::Cancelled => Self::Cancelled,
            Fault::Unauthorized => Self::Unauthorized,
            Fault::Forbidden => Self::Forbidden,
            Fault::RateLimited => Self::RateLimited,
            Fault::Protocol | Fault::Incomplete | Fault::UsageUnavailable => Self::Protocol,
            Fault::Unavailable => Self::Unavailable,
        }
    }
}

/// Freshness of an explicit provider observation, not proof of a paid plan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QuotaStatus {
    /// No check was requested for this connection.
    #[default]
    NotChecked,
    /// Complete recent provider observation.
    Current,
    /// Same-connection last-good observation has expired or refresh failed.
    Stale,
    /// No trustworthy reading is available.
    Unavailable,
    /// Provider explicitly reported no allowance entitlement.
    NoEntitlement,
}

/// Transient secret-free reading. Billing and local token accounting stay separate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuotaView {
    /// Observation freshness or absence.
    pub status: QuotaStatus,
    /// An explicit user-requested check is pending.
    pub checking: bool,
    /// Time of the last successful complete observation.
    pub checked_at_ms: Option<u64>,
    /// Start time of the latest explicit attempt.
    pub attempted_at_ms: Option<u64>,
    /// Earliest provider reset or local freshness expiry, whichever comes first.
    pub stale_at_ms: Option<u64>,
    /// All three provider windows, or none; never a partial/defaulted allowance.
    pub reading: Option<GoQuotaSnapshot>,
    /// Closed failure from the latest explicit attempt.
    pub fault: Option<QuotaFault>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Ticket {
    generation: u64,
    sequence: u64,
}
#[derive(Default)]
pub(super) struct QuotaState {
    reading: Option<GoQuotaSnapshot>,
    checked_at_ms: Option<u64>,
    attempted_at_ms: Option<u64>,
    fault: Option<QuotaFault>,
    stale: bool,
    flight: Option<Ticket>,
    sequence: u64,
}
impl QuotaState {
    pub(super) fn clear(&mut self) {
        // Never recycle a ticket when an older owned completion is still draining.
        *self = Self {
            sequence: self.sequence,
            ..Self::default()
        };
    }
    pub(super) fn retire(&mut self) {
        if self.flight.take().is_some() {
            self.fault = Some(QuotaFault::Cancelled);
        }
        self.stale = self.reading.is_some();
    }
    pub(super) fn view(&self, now: Option<u64>) -> QuotaView {
        let stale_at_ms = self
            .reading
            .zip(self.checked_at_ms)
            .and_then(|(reading, checked)| {
                checked.checked_add(FRESH_FOR_MS).map(|expiry| {
                    expiry
                        .min(reading.rolling.resets_at_ms)
                        .min(reading.weekly.resets_at_ms)
                        .min(reading.monthly.resets_at_ms)
                })
            });
        let status = if self.reading.is_some() {
            if self.stale
                || !matches!((now, self.checked_at_ms, stale_at_ms), (Some(now), Some(checked), Some(until)) if checked <= now && now < until)
            {
                QuotaStatus::Stale
            } else {
                QuotaStatus::Current
            }
        } else if self.fault == Some(QuotaFault::NoEntitlement) {
            QuotaStatus::NoEntitlement
        } else if self.fault.is_some() {
            QuotaStatus::Unavailable
        } else {
            QuotaStatus::NotChecked
        };
        QuotaView {
            status,
            checking: self.flight.is_some(),
            checked_at_ms: self.checked_at_ms,
            attempted_at_ms: self.attempted_at_ms,
            stale_at_ms,
            reading: self.reading,
            fault: self.fault,
        }
    }
}
struct FlightGuard {
    service: Arc<Service>,
    ticket: Ticket,
}
impl Drop for FlightGuard {
    fn drop(&mut self) {
        let changed = {
            let mut state = self
                .service
                .state
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if state.quota.flight == Some(self.ticket) {
                state.quota.retire();
                true
            } else {
                false
            }
        };
        if changed {
            self.service.notify();
        }
    }
}
impl Service {
    /// Explicitly observe this connection's allowance; never starts paid inference.
    pub async fn check_quota(
        self: &Arc<Self>,
        expected_id: Option<String>,
    ) -> Result<(), QuotaFault> {
        self.check_quota_scoped(self.scope().map_err(QuotaFault::from)?, expected_id)
            .await
    }
    /// Observe only within the native profile scope and connection identity admitted by IPC.
    pub async fn check_quota_scoped(
        self: &Arc<Self>,
        scope: Scope,
        expected_id: Option<String>,
    ) -> Result<(), QuotaFault> {
        let generation = self.scoped_generation(scope).map_err(QuotaFault::from)?;
        let expected = expected_id
            .filter(|id| crate::valid_id(id))
            .ok_or(QuotaFault::NotConfigured)?;
        let started = now_ms()
            .filter(|ms| *ms > 0)
            .ok_or(QuotaFault::Unavailable)?;
        let ticket = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.closed || !state.foreground || state.generation != generation {
                return Err(QuotaFault::Cancelled);
            }
            if state.loaded
                && state.config.as_ref().map(|c| c.id.as_str()) != Some(expected.as_str())
            {
                return Err(QuotaFault::Changed);
            }
            if state.quota.flight.is_some() {
                return Err(QuotaFault::Busy);
            }
            let sequence = state
                .quota
                .sequence
                .checked_add(1)
                .ok_or(QuotaFault::Unavailable)?;
            let ticket = Ticket {
                generation,
                sequence,
            };
            state.quota.sequence = sequence;
            state.quota.flight = Some(ticket);
            state.quota.attempted_at_ms = Some(started);
            ticket
        };
        let guard = FlightGuard {
            service: self.clone(),
            ticket,
        };
        self.notify();
        let service = self.clone();
        self.owned(false, async move {
            let _guard = guard;
            Ok(service.quota_owned(ticket, expected).await)
        })
        .await
        .map_err(QuotaFault::from)?
    }
    async fn quota_owned(&self, ticket: Ticket, expected: String) -> Result<(), QuotaFault> {
        let _turn = self.mutation.lock().await;
        let result = async {
            if !self.admitted(ticket.generation) {
                return Err(QuotaFault::Cancelled);
            }
            self.load_locked().await.map_err(QuotaFault::from)?;
            let config = {
                let state = self.state.read().unwrap_or_else(|e| e.into_inner());
                if state.closed
                    || !state.foreground
                    || state.generation != ticket.generation
                    || state.quota.flight != Some(ticket)
                {
                    return Err(QuotaFault::Cancelled);
                }
                let config = state.config.clone().ok_or(QuotaFault::NotConfigured)?;
                if config.id != expected {
                    return Err(QuotaFault::Changed);
                }
                config
            };
            let mut cancelled = self.cancellation.subscribe();
            if !self.admitted(ticket.generation) {
                return Err(QuotaFault::Cancelled);
            }
            tokio::select! {
                biased;
                _ = cancelled.changed() => Err(QuotaFault::Cancelled),
                result = self.transport.quota(&config) => result.map_err(QuotaFault::from),
            }
        }
        .await;
        let observed = now_ms().filter(|ms| *ms > 0);
        let result = result.and_then(|reading| {
            observed
                .map(|ms| (reading, ms))
                .ok_or(QuotaFault::Unavailable)
        });
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.closed
                || !state.foreground
                || state.generation != ticket.generation
                || state.quota.flight != Some(ticket)
            {
                return Err(QuotaFault::Cancelled);
            }
            state.quota.flight = None;
            match &result {
                Ok((reading, observed)) => {
                    state.quota.reading = Some(*reading);
                    state.quota.checked_at_ms = Some(*observed);
                    state.quota.fault = None;
                    state.quota.stale = false;
                }
                Err(fault) => {
                    state.quota.fault = Some(*fault);
                    state.quota.stale = true;
                    if matches!(fault, QuotaFault::Unauthorized | QuotaFault::NoEntitlement) {
                        state.quota.reading = None;
                        state.quota.checked_at_ms = None;
                    }
                }
            }
        }
        self.notify();
        result.map(|_| ())
    }
}

#[cfg(test)]
mod tests;
