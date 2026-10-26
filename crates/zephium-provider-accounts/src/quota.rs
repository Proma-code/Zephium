//! Pure quota projections, not live retrieval or authentication evidence.
//!
//! Inputs are the result/params of Codex `account/rateLimits/read`/`updated`, or
//! original Claude Code status-line JSON. A qualified native broker must supply
//! account provenance, correlate responses, and enforce its own execution gate.
//! Go uses a separate source-owned reader; its first-party quota contract is not
//! interchangeable with these CLI parsers. Direct SIWC and generic Antigravity
//! gateways still have no verified quota contract here.
//! No token stores, private endpoints, paid requests, hooks or clocks are used.
//!
//! Contracts: <https://learn.chatgpt.com/docs/app-server> and
//! <https://code.claude.com/docs/en/statusline>. Plan labels never create windows;
//! API credits, local token counts and context utilization are not quota.

use serde_json::{Map, Value};

use crate::AccountProvider;

/// Maximum observation bytes admitted before JSON decoding.
pub const MAX_QUOTA_BYTES: usize = 64 * 1024;
const MAX_BUCKETS: usize = 32;
// JavaScript Date's range, also below its exact-integer ceiling. Native UI
// projection must not turn a checked Rust timestamp into an invalid local date.
const MAX_TIMESTAMP_MS: u64 = 8_640_000_000_000_000;

/// Where quota evidence would originate, independent of authentication status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaSource {
    /// Zephium's direct official Sign in with ChatGPT, not the Codex CLI account.
    DirectChatGpt,
    /// The official Codex app-server's own managed account.
    CodexAppServer,
    /// An already-authorized original Claude Code session's status-line output.
    ClaudeCodeStatusLine,
    /// OpenCode Go included usage, not Zen credit balance or arbitrary providers.
    OpenCodeGo,
    /// An existing custom gateway, not unsupported consumer Antigravity login.
    AntigravityGateway,
}

/// Verified observation contract, never permission or a claim of a live reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaCapability {
    /// This module can parse documented output; native retrieval is not connected.
    DocumentedObservation,
    /// A first-party contract is verified; its source-owned reader is outside this module.
    FirstPartyContract,
    /// No supported machine-readable contract has been verified for this source.
    NoVerifiedContract,
    /// Consumer integration is unsupported; no gateway contract is implemented.
    Unsupported,
}

impl QuotaSource {
    /// Provider identity without conflating different accounts or billing sources.
    pub const fn provider(self) -> AccountProvider {
        match self {
            Self::DirectChatGpt | Self::CodexAppServer => AccountProvider::ChatGpt,
            Self::ClaudeCodeStatusLine => AccountProvider::Claude,
            Self::OpenCodeGo => AccountProvider::OpenCode,
            Self::AntigravityGateway => AccountProvider::Antigravity,
        }
    }

    /// Parsing capability only; none of these sources has a live reader here.
    pub const fn capability(self) -> QuotaCapability {
        match self {
            Self::CodexAppServer | Self::ClaudeCodeStatusLine => {
                QuotaCapability::DocumentedObservation
            }
            Self::DirectChatGpt => QuotaCapability::NoVerifiedContract,
            Self::OpenCodeGo => QuotaCapability::FirstPartyContract,
            Self::AntigravityGateway => QuotaCapability::Unsupported,
        }
    }

    /// Truthful initial presentation, even when the corresponding account is linked.
    pub const fn initial_unavailability(self) -> QuotaUnavailable {
        match self.capability() {
            QuotaCapability::DocumentedObservation | QuotaCapability::FirstPartyContract => {
                QuotaUnavailable::ObserverNotConnected
            }
            QuotaCapability::NoVerifiedContract => QuotaUnavailable::NoVerifiedContract,
            QuotaCapability::Unsupported => QuotaUnavailable::ProviderUnsupported,
        }
    }
}

/// Native-owned, non-secret scope; never populated from provider JSON or JavaScript.
/// Revisions must change on profile/account replacement, including signing out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuotaProvenance {
    /// Exact observation contract and account source.
    pub source: QuotaSource,
    /// Opaque native profile lifetime revision, not a path or user identifier.
    pub profile_revision: u64,
    /// Opaque native account lifetime revision, not a provider account identifier.
    pub account_revision: u64,
}

/// Closed failures; no raw provider message, account details or credentials escape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaFault {
    /// Observation exceeded the pre-decoding byte bound.
    MessageTooLarge,
    /// JSON was invalid, including non-finite JSON numbers.
    InvalidJson,
    /// Fields did not match the documented observation shape.
    InvalidShape,
    /// A percentage was nonnumeric, non-finite, negative or above the window limit.
    InvalidPercentage,
    /// A window duration was not a positive, representable integer of minutes.
    InvalidDuration,
    /// A reset was not nonnegative integer epoch seconds convertible to UI time.
    InvalidReset,
    /// A gateway dollar estimate or limit was not finite and nonnegative.
    InvalidAmount,
    /// The native capture/attempt timestamp cannot be represented in the UI.
    InvalidTimestamp,
    /// Source, profile or account does not match this observer.
    SourceMismatch,
    /// A completion predates the most recent observation or attempt.
    OlderObservation,
    /// An authoritative account/configuration change retired this observer.
    Retired,
    /// The qualified transport failed; no server text is retained.
    Transport,
    /// The native broker's observation deadline expired.
    TimedOut,
    /// The provider rejected the observation; its message is discarded.
    ProviderRefused,
}

/// Absence of quota evidence, distinct from zero remaining or unlimited usage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaUnavailable {
    /// A documented parser exists, but no qualified observation is connected.
    ObserverNotConnected,
    /// No verified public quota-read contract exists for this integration source.
    NoVerifiedContract,
    /// The provider integration is unsupported here.
    ProviderUnsupported,
    /// The intended native account is authoritatively signed out.
    SignedOut,
    /// A successful observation supplied no quota windows or gateway spend data.
    NotReported,
}

/// Optional provider-reported plan label. It never implies any particular window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaPlan {
    /// Free plan.
    Free,
    /// Go plan.
    Go,
    /// Plus plan.
    Plus,
    /// Pro plan.
    Pro,
    /// Team plan.
    Team,
    /// Business plan.
    Business,
    /// Enterprise plan.
    Enterprise,
    /// Education plan.
    Education,
    /// Another provider-reported plan; arbitrary server text is not retained.
    Other,
}

/// Provider-defined window position, not a plan-to-duration lookup table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowKind {
    /// Codex primary window, with duration supplied independently by the provider.
    Primary,
    /// Codex secondary window, with duration supplied independently by the provider.
    Secondary,
    /// Claude's explicitly named five-hour window.
    FiveHour,
    /// Claude's explicitly named seven-day window.
    SevenDay,
}

/// One reported quota window. Missing values stay unknown, including percentages.
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaWindow {
    /// Provider-defined position or name.
    pub kind: WindowKind,
    /// Reported duration; only Claude's documented named windows imply a duration.
    pub duration_minutes: Option<u64>,
    /// Validated provider usage in the inclusive range 0–100.
    pub used_percent: Option<f64>,
    /// Exactly `100 - used_percent`, only when validated usage is present.
    pub remaining_percent: Option<f64>,
    /// Provider reset converted from epoch seconds to epoch milliseconds.
    pub resets_at_ms: Option<u64>,
}

impl QuotaWindow {
    /// Expiry invalidates freshness; it never locally refills the allowance.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.resets_at_ms.is_some_and(|reset| reset <= now_ms)
    }
}

/// One provider-reported metered bucket; independent buckets are not combined.
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaBucket {
    /// Bounded machine bucket identifier, if reported; never an account identifier.
    pub id: Option<String>,
    /// Optional known plan or `Other`; no arbitrary plan or label text is retained.
    pub plan: Option<QuotaPlan>,
    /// Only windows actually present in this bucket, not inferred from its plan.
    pub windows: Vec<QuotaWindow>,
}

/// The gateway's reported spend period, separate from subscription quota windows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpendPeriod {
    /// Daily spend limit.
    Daily,
    /// Weekly spend limit.
    Weekly,
    /// Monthly spend limit.
    Monthly,
    /// A future period; arbitrary provider text is discarded.
    Other,
}

/// Estimated gateway spending, not a subscription quota or billed API balance.
/// Dollar values can lag the percentage; no remaining dollars are computed.
#[derive(Clone, Debug, PartialEq)]
pub struct GatewaySpend {
    /// Reported nonnegative percentage; it may legitimately exceed 100.
    pub used_percent: Option<f64>,
    /// Reset in epoch milliseconds, if supplied.
    pub resets_at_ms: Option<u64>,
    /// Gateway token-cost estimate, not a billed amount.
    pub estimated_used_usd: Option<f64>,
    /// Reported gateway spend limit in US dollars.
    pub limit_usd: Option<f64>,
    /// Optional gateway spend period.
    pub period: Option<SpendPeriod>,
}

/// Validated observation scoped by the trusted native caller, with no credentials.
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaReading {
    /// Provider/account/profile source; copied only from the trusted caller.
    pub provenance: QuotaProvenance,
    /// Native observation time in epoch milliseconds, not a server timestamp.
    pub captured_at_ms: u64,
    /// Reported subscription buckets, never API credits or local usage estimates.
    pub buckets: Vec<QuotaBucket>,
    /// Separate optional Claude gateway spending estimate.
    pub gateway_spend: Option<GatewaySpend>,
}

impl QuotaReading {
    fn has_observation(&self) -> bool {
        self.buckets.iter().any(|bucket| !bucket.windows.is_empty()) || self.gateway_spend.is_some()
    }

    fn is_stale(&self, now_ms: u64, max_age_ms: u64) -> bool {
        now_ms
            .checked_sub(self.captured_at_ms)
            .is_none_or(|age| age >= max_age_ms)
            || self
                .buckets
                .iter()
                .flat_map(|bucket| &bucket.windows)
                .any(|window| window.is_expired(now_ms))
            || self
                .gateway_spend
                .as_ref()
                .and_then(|spend| spend.resets_at_ms)
                .is_some_and(|reset| reset <= now_ms)
    }
}

/// Presentation state; a transient failure does not erase a last-good observation.
#[derive(Clone, Debug, PartialEq)]
pub enum QuotaState {
    /// No usable observation, not an error and not a zero balance.
    Unavailable(QuotaUnavailable),
    /// A successful observation; individual numeric fields can still be unknown.
    Available(QuotaReading),
    /// Last-good values must be visibly marked stale and must not imply entitlement.
    Stale {
        /// Retained same-account observation, including its original capture time.
        reading: QuotaReading,
        /// Optional closed failure; `None` means age or window expiry caused staleness.
        fault: Option<QuotaFault>,
    },
    /// An observation failed before any usable same-account reading was received.
    Error(QuotaFault),
}

/// Pure lifecycle projection for one native profile/account revision.
/// The host still owns request correlation and must replace this on scope changes.
#[derive(Clone, Debug)]
pub struct QuotaProjection {
    provenance: QuotaProvenance,
    state: QuotaState,
    last_attempt_at_ms: Option<u64>,
    retired: bool,
}

impl QuotaProjection {
    /// Begin honestly unavailable. This constructor never reads or connects an account.
    pub fn new(provenance: QuotaProvenance) -> Self {
        Self {
            provenance,
            state: QuotaState::Unavailable(provenance.source.initial_unavailability()),
            last_attempt_at_ms: None,
            retired: false,
        }
    }

    /// Current content-free state or explicitly scoped last-good observation.
    pub fn state(&self) -> &QuotaState {
        &self.state
    }

    /// Native scope associated with this projection, even when no data is available.
    pub fn provenance(&self) -> QuotaProvenance {
        self.provenance
    }

    /// Last observation/attempt time; failures do not change the reading's capture time.
    pub fn last_attempt_at_ms(&self) -> Option<u64> {
        self.last_attempt_at_ms
    }

    fn admit(&self, at_ms: u64) -> Result<(), QuotaFault> {
        if self.retired {
            return Err(QuotaFault::Retired);
        }
        if self.provenance.source.capability() != QuotaCapability::DocumentedObservation {
            return Err(QuotaFault::SourceMismatch);
        }
        validate_timestamp(at_ms)?;
        if self.last_attempt_at_ms.is_some_and(|last| at_ms < last) {
            return Err(QuotaFault::OlderObservation);
        }
        Ok(())
    }

    /// Apply a normalized same-account observation; authoritative absence clears old data.
    pub fn record(&mut self, reading: QuotaReading) -> Result<(), QuotaFault> {
        if reading.provenance != self.provenance {
            return Err(QuotaFault::SourceMismatch);
        }
        self.admit(reading.captured_at_ms)?;
        self.last_attempt_at_ms = Some(reading.captured_at_ms);
        self.state = if reading.has_observation() {
            if reading.is_stale(reading.captured_at_ms, u64::MAX) {
                QuotaState::Stale {
                    reading,
                    fault: None,
                }
            } else {
                QuotaState::Available(reading)
            }
        } else {
            QuotaState::Unavailable(QuotaUnavailable::NotReported)
        };
        Ok(())
    }

    /// Retain an existing reading as stale after a transient observation failure.
    pub fn fail(&mut self, fault: QuotaFault, at_ms: u64) -> Result<(), QuotaFault> {
        self.admit(at_ms)?;
        self.last_attempt_at_ms = Some(at_ms);
        self.state = match &self.state {
            QuotaState::Available(reading) | QuotaState::Stale { reading, .. } => {
                QuotaState::Stale {
                    reading: reading.clone(),
                    fault: Some(fault),
                }
            }
            QuotaState::Unavailable(_) | QuotaState::Error(_) => QuotaState::Error(fault),
        };
        Ok(())
    }

    /// Mark aged/expired readings stale using host-supplied time and freshness policy.
    /// No timer is started and no percentage is reset or refilled locally.
    pub fn mark_stale(&mut self, now_ms: u64, max_age_ms: u64) {
        if let QuotaState::Available(reading) = &self.state {
            if reading.is_stale(now_ms, max_age_ms) {
                self.state = QuotaState::Stale {
                    reading: reading.clone(),
                    fault: None,
                };
            }
        }
    }

    /// Clear data and retire the observer after authoritative account/config loss.
    /// Delayed results cannot revive it; a new native revision needs a new projection.
    pub fn make_unavailable(&mut self, reason: QuotaUnavailable) {
        self.state = QuotaState::Unavailable(reason);
        self.last_attempt_at_ms = None;
        self.retired = true;
    }
}

/// Normalize a decoded Codex rate-limit RPC result or notification params object.
/// RPC correlation/envelope validation belongs to the qualified native broker.
/// Multi-bucket data takes precedence even when empty; legacy data is not duplicated.
pub fn parse_codex_rate_limits(
    bytes: &[u8],
    provenance: QuotaProvenance,
    captured_at_ms: u64,
) -> Result<QuotaReading, QuotaFault> {
    check_source(provenance, QuotaSource::CodexAppServer, captured_at_ms)?;
    let value = decode(bytes)?;
    let root = object(&value)?;
    let mut buckets = Vec::new();
    if let Some(by_id) = present(root, "rateLimitsByLimitId") {
        let by_id = object(by_id)?;
        if by_id.len() > MAX_BUCKETS {
            return Err(QuotaFault::InvalidShape);
        }
        for (id, bucket) in by_id {
            validate_bucket_id(id)?;
            buckets.push(codex_bucket(bucket, Some(id))?);
        }
    } else if let Some(bucket) = present(root, "rateLimits") {
        buckets.push(codex_bucket(bucket, None)?);
    } else if !root.contains_key("rateLimits") && !root.contains_key("rateLimitsByLimitId") {
        return Err(QuotaFault::InvalidShape);
    }
    Ok(QuotaReading {
        provenance,
        captured_at_ms,
        buckets,
        gateway_spend: None,
    })
}

/// Normalize original Claude Code status-line JSON, not `/context` or auth output.
/// Missing rate limits are valid before a session response; no paid refresh is implied.
pub fn parse_claude_statusline(
    bytes: &[u8],
    provenance: QuotaProvenance,
    captured_at_ms: u64,
) -> Result<QuotaReading, QuotaFault> {
    check_source(
        provenance,
        QuotaSource::ClaudeCodeStatusLine,
        captured_at_ms,
    )?;
    let value = decode(bytes)?;
    let root = object(&value)?;
    let mut buckets = Vec::new();
    let mut gateway_spend = None;
    if let Some(rate_limits) = present(root, "rate_limits") {
        let rate_limits = object(rate_limits)?;
        let mut windows = Vec::new();
        for (name, kind, minutes) in [
            ("five_hour", WindowKind::FiveHour, 300),
            ("seven_day", WindowKind::SevenDay, 10080),
        ] {
            if let Some(window) = present(rate_limits, name) {
                let fields = object(window)?;
                let used_percent = optional_number(
                    fields,
                    "used_percentage",
                    QuotaFault::InvalidPercentage,
                    Some(100.0),
                )?;
                windows.push(QuotaWindow {
                    kind,
                    duration_minutes: Some(minutes),
                    used_percent,
                    remaining_percent: used_percent.map(|used| 100.0 - used),
                    resets_at_ms: reset(fields, "resets_at")?,
                });
            }
        }
        if !windows.is_empty() {
            buckets.push(QuotaBucket {
                id: None,
                plan: None,
                windows,
            });
        }
        if let Some(spend) = present(rate_limits, "spend_limit") {
            gateway_spend = Some(claude_spend(object(spend)?)?);
        }
    }
    Ok(QuotaReading {
        provenance,
        captured_at_ms,
        buckets,
        gateway_spend,
    })
}

fn decode(bytes: &[u8]) -> Result<Value, QuotaFault> {
    if bytes.len() > MAX_QUOTA_BYTES {
        return Err(QuotaFault::MessageTooLarge);
    }
    serde_json::from_slice(bytes).map_err(|_| QuotaFault::InvalidJson)
}

fn check_source(
    provenance: QuotaProvenance,
    expected: QuotaSource,
    at_ms: u64,
) -> Result<(), QuotaFault> {
    if provenance.source != expected {
        return Err(QuotaFault::SourceMismatch);
    }
    validate_timestamp(at_ms)
}

fn validate_timestamp(at_ms: u64) -> Result<(), QuotaFault> {
    if at_ms > MAX_TIMESTAMP_MS {
        Err(QuotaFault::InvalidTimestamp)
    } else {
        Ok(())
    }
}

fn object(value: &Value) -> Result<&Map<String, Value>, QuotaFault> {
    value.as_object().ok_or(QuotaFault::InvalidShape)
}

fn present<'a>(object: &'a Map<String, Value>, field: &str) -> Option<&'a Value> {
    object.get(field).filter(|value| !value.is_null())
}

fn optional_number(
    object: &Map<String, Value>,
    field: &str,
    fault: QuotaFault,
    maximum: Option<f64>,
) -> Result<Option<f64>, QuotaFault> {
    present(object, field)
        .map(|value| {
            let number = value.as_f64().ok_or(fault)?;
            if !number.is_finite() || number < 0.0 || maximum.is_some_and(|max| number > max) {
                return Err(fault);
            }
            Ok(number)
        })
        .transpose()
}

fn reset(object: &Map<String, Value>, field: &str) -> Result<Option<u64>, QuotaFault> {
    present(object, field)
        .map(|value| {
            value
                .as_u64()
                .and_then(|seconds| seconds.checked_mul(1000))
                .filter(|milliseconds| *milliseconds <= MAX_TIMESTAMP_MS)
                .ok_or(QuotaFault::InvalidReset)
        })
        .transpose()
}

fn duration(object: &Map<String, Value>) -> Result<Option<u64>, QuotaFault> {
    present(object, "windowDurationMins")
        .map(|value| {
            value
                .as_u64()
                .filter(|minutes| *minutes > 0 && *minutes <= MAX_TIMESTAMP_MS / 60_000)
                .ok_or(QuotaFault::InvalidDuration)
        })
        .transpose()
}

fn validate_bucket_id(id: &str) -> Result<(), QuotaFault> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(QuotaFault::InvalidShape);
    }
    Ok(())
}

fn codex_bucket(value: &Value, map_id: Option<&str>) -> Result<QuotaBucket, QuotaFault> {
    let fields = object(value)?;
    let reported_id = present(fields, "limitId")
        .map(|value| {
            let id = value.as_str().ok_or(QuotaFault::InvalidShape)?;
            validate_bucket_id(id)?;
            Ok(id)
        })
        .transpose()?;
    if map_id.zip(reported_id).is_some_and(|(key, id)| key != id) {
        return Err(QuotaFault::InvalidShape);
    }
    let plan = present(fields, "planType")
        .map(|value| {
            Ok(match value.as_str().ok_or(QuotaFault::InvalidShape)? {
                "free" => QuotaPlan::Free,
                "go" => QuotaPlan::Go,
                "plus" => QuotaPlan::Plus,
                "pro" => QuotaPlan::Pro,
                "team" => QuotaPlan::Team,
                "business" => QuotaPlan::Business,
                "enterprise" => QuotaPlan::Enterprise,
                "edu" | "education" => QuotaPlan::Education,
                _ => QuotaPlan::Other,
            })
        })
        .transpose()?;
    let mut windows = Vec::new();
    for (field, kind) in [
        ("primary", WindowKind::Primary),
        ("secondary", WindowKind::Secondary),
    ] {
        if let Some(window) = present(fields, field) {
            let fields = object(window)?;
            let used_percent = optional_number(
                fields,
                "usedPercent",
                QuotaFault::InvalidPercentage,
                Some(100.0),
            )?;
            windows.push(QuotaWindow {
                kind,
                duration_minutes: duration(fields)?,
                used_percent,
                remaining_percent: used_percent.map(|used| 100.0 - used),
                resets_at_ms: reset(fields, "resetsAt")?,
            });
        }
    }
    Ok(QuotaBucket {
        id: map_id.or(reported_id).map(str::to_owned),
        plan,
        windows,
    })
}

fn claude_spend(fields: &Map<String, Value>) -> Result<GatewaySpend, QuotaFault> {
    let period = present(fields, "period")
        .map(|value| {
            Ok(match value.as_str().ok_or(QuotaFault::InvalidShape)? {
                "daily" => SpendPeriod::Daily,
                "weekly" => SpendPeriod::Weekly,
                "monthly" => SpendPeriod::Monthly,
                _ => SpendPeriod::Other,
            })
        })
        .transpose()?;
    Ok(GatewaySpend {
        used_percent: optional_number(
            fields,
            "used_percentage",
            QuotaFault::InvalidPercentage,
            None,
        )?,
        resets_at_ms: reset(fields, "resets_at")?,
        estimated_used_usd: optional_number(fields, "used_usd", QuotaFault::InvalidAmount, None)?,
        limit_usd: optional_number(fields, "limit_usd", QuotaFault::InvalidAmount, None)?,
        period,
    })
}
