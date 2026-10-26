//! Read-only, fixed-origin Go allowance observations using the connection's own
//! Console key. This is a first-party implementation contract, not a stable SLA.
//! No Console OAuth, organization discovery, CLI credentials, plan inference,
//! balance reads, background polling, fallback endpoint or automatic retry.
//!
//! Reviewed first-party sources (2026-10-26):
//! - <https://github.com/anomalyco/opencode/blob/5b41e1f977db069603fe0c8f9c668a79e05b4013/packages/core/src/usage-limits.ts>
//! - <https://github.com/anomalyco/opencode/blob/9f8db119fcbd4999379129ac7734375ac23460fb/packages/console/app/src/routes/zen/go/v1/usage.ts>
//! - <https://github.com/anomalyco/opencode/issues/52227> (canonical path migration)

use super::{media_is, parse_frame, OpenCodeGoClient};
use reqwest::header::{AUTHORIZATION, RETRY_AFTER};
use serde_json::Value;
use std::time::Duration;

const MAX_QUOTA_BYTES: usize = 32 * 1024;
const QUOTA_TIMEOUT: Duration = Duration::from_secs(12);
const QUOTA_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// Content-free quota failures. Provider messages, keys and account identifiers
/// are never retained. A failure is not a zero or full allowance observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoQuotaFault {
    /// The native connection's key cannot form the required authorization header.
    InvalidRequest,
    /// HTTP 401 refused this connection's own access key.
    Unauthorized,
    /// HTTP 403 explicitly reported the reviewed `EntitlementError` category.
    NoEntitlement,
    /// HTTP 403 without the reviewed no-entitlement category; do not infer a plan.
    Forbidden,
    /// The fixed usage route is missing or no longer accepts GET (404 or 405).
    EndpointUnavailable,
    /// HTTP 429; the hint is presentation metadata, never a retry instruction.
    RateLimited {
        /// Bounded integer-seconds Retry-After converted to milliseconds.
        retry_after_ms: Option<u64>,
    },
    /// The total request or an individual response-body read expired.
    Timeout,
    /// Transient network or upstream server failure.
    Unavailable,
    /// The response did not contain supported, validated quota observations.
    Protocol,
    /// The bounded response size was exceeded.
    TooLarge,
}

/// One complete provider-reported window; never inferred from local accounting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GoQuotaWindow {
    /// Finite provider-reported used percentage, in the inclusive range 0–100.
    pub used_percent: f64,
    /// Exactly `100 - used_percent`, without clamping or a missing-value default.
    pub remaining_percent: f64,
    /// Provider reset instant normalized to Unix milliseconds, not a local timer.
    pub resets_at_ms: u64,
    /// Provider-reported refusal state, independent of the reported percentage.
    pub limited: bool,
}

/// Native-only bounded reading. No account metadata, arbitrary labels, billing
/// values, model identity or implied duration is imported. The owner supplies
/// the trusted checked-at timestamp and revalidates its connection generation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GoQuotaSnapshot {
    /// Provider's rolling window. The current contract requires it.
    pub rolling: GoQuotaWindow,
    /// Provider's weekly window. The current contract requires it.
    pub weekly: GoQuotaWindow,
    /// Provider's monthly window. The current contract requires it.
    pub monthly: GoQuotaWindow,
}

impl OpenCodeGoClient {
    /// Explicit read-only GET using this native connection's own Go API key.
    /// Uses the existing no-proxy/no-redirect/no-retry client. Dropping this future
    /// cancels local I/O; no task, callback, retry or account mutation survives it.
    /// A successful reading does not authorize inference or enable Extra Usage.
    pub async fn quota(&self) -> Result<GoQuotaSnapshot, GoQuotaFault> {
        self.quota_with_limits(QUOTA_TIMEOUT, QUOTA_READ_TIMEOUT)
            .await
    }

    async fn quota_with_limits(
        &self,
        total_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<GoQuotaSnapshot, GoQuotaFault> {
        let header = self
            .key
            .header("Bearer ")
            .map_err(|_| GoQuotaFault::InvalidRequest)?;
        let read = async {
            let mut response = self
                .http
                .get(self.endpoint("v1/usage"))
                .header(AUTHORIZATION, header)
                .header(reqwest::header::ACCEPT, "application/json")
                .header("accept-encoding", "identity")
                .timeout(total_timeout)
                .send()
                .await
                .map_err(network_fault)?;
            match response.status().as_u16() {
                200 => {
                    if !media_is(&response, "application/json") {
                        return Err(GoQuotaFault::Protocol);
                    }
                    let bytes = read_body(&mut response, read_timeout).await?;
                    parse_snapshot(&bytes)
                }
                403 => {
                    // Only the exact reviewed status/category combination can
                    // claim no entitlement. Never match provider message text.
                    if !media_is(&response, "application/json") {
                        return Err(GoQuotaFault::Forbidden);
                    }
                    let bytes = read_body(&mut response, read_timeout).await?;
                    Err(forbidden_fault(&bytes))
                }
                status => Err(status_fault(status, response.headers().get(RETRY_AFTER))),
            }
        };
        tokio::time::timeout(total_timeout, read)
            .await
            .map_err(|_| GoQuotaFault::Timeout)?
    }
}

fn network_fault(error: reqwest::Error) -> GoQuotaFault {
    if error.is_timeout() {
        GoQuotaFault::Timeout
    } else {
        GoQuotaFault::Unavailable
    }
}

fn status_fault(status: u16, retry: Option<&reqwest::header::HeaderValue>) -> GoQuotaFault {
    match status {
        401 => GoQuotaFault::Unauthorized,
        404 | 405 => GoQuotaFault::EndpointUnavailable,
        408 | 504 => GoQuotaFault::Timeout,
        413 => GoQuotaFault::TooLarge,
        429 => GoQuotaFault::RateLimited {
            retry_after_ms: retry
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|seconds| *seconds <= 86_400)
                .map(|seconds| seconds * 1000),
        },
        500..=599 => GoQuotaFault::Unavailable,
        // Redirects never follow and never try the historical endpoint.
        _ => GoQuotaFault::Protocol,
    }
}

async fn read_body(
    response: &mut reqwest::Response,
    read_timeout: Duration,
) -> Result<Vec<u8>, GoQuotaFault> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_QUOTA_BYTES as u64)
    {
        return Err(GoQuotaFault::TooLarge);
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::time::timeout(read_timeout, response.chunk())
            .await
            .map_err(|_| GoQuotaFault::Timeout)?
            .map_err(network_fault)?;
        let Some(chunk) = chunk else { return Ok(bytes) };
        if bytes.len().saturating_add(chunk.len()) > MAX_QUOTA_BYTES {
            return Err(GoQuotaFault::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
}

fn forbidden_fault(bytes: &[u8]) -> GoQuotaFault {
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(_) => return GoQuotaFault::Forbidden,
    };
    if value.get("type").and_then(Value::as_str) == Some("error")
        && value.pointer("/error/type").and_then(Value::as_str) == Some("EntitlementError")
    {
        GoQuotaFault::NoEntitlement
    } else {
        GoQuotaFault::Forbidden
    }
}

fn parse_snapshot(bytes: &[u8]) -> Result<GoQuotaSnapshot, GoQuotaFault> {
    if bytes.len() > MAX_QUOTA_BYTES {
        return Err(GoQuotaFault::TooLarge);
    }
    let raw = std::str::from_utf8(bytes).map_err(|_| GoQuotaFault::Protocol)?;
    // Reuse the transport's bounded numeric-lexeme guard: ordinary JSON parsing
    // alone could silently turn adversarial 1e-999 into a displayed zero.
    let value = parse_frame(raw).map_err(|_| GoQuotaFault::Protocol)?;
    let usage = value
        .get("usage")
        .and_then(Value::as_object)
        .ok_or(GoQuotaFault::Protocol)?;
    // The reviewed first-party decoder requires all three complete windows.
    // Any missing or malformed window fails the observation atomically so the
    // owner can retain the same connection's last-good reading as stale.
    Ok(GoQuotaSnapshot {
        rolling: usage
            .get("rolling")
            .and_then(parse_window)
            .ok_or(GoQuotaFault::Protocol)?,
        weekly: usage
            .get("weekly")
            .and_then(parse_window)
            .ok_or(GoQuotaFault::Protocol)?,
        monthly: usage
            .get("monthly")
            .and_then(parse_window)
            .ok_or(GoQuotaFault::Protocol)?,
    })
}

fn parse_window(value: &Value) -> Option<GoQuotaWindow> {
    let used_percent = value.get("percent")?.as_f64()?;
    if !used_percent.is_finite() || !(0.0..=100.0).contains(&used_percent) {
        return None;
    }
    let limited = match value.get("status")?.as_str()? {
        "ok" => false,
        "rate-limited" => true,
        _ => return None,
    };
    let resets_at_ms = parse_instant(value.get("resetsAt")?.as_str()?)?;
    Some(GoQuotaWindow {
        used_percent,
        remaining_percent: 100.0 - used_percent,
        resets_at_ms,
        limited,
    })
}

// The reviewed server emits Date.toISOString(); the maintained RFC3339 parser
// also handles explicit numeric offsets and validates the calendar. Bound input
// before parsing. Negative instants are unavailable, including sub-millisecond
// pre-epoch values that integer division alone would otherwise truncate to zero.
fn parse_instant(value: &str) -> Option<u64> {
    if !(20..=35).contains(&value.len()) {
        return None;
    }
    let instant =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()?;
    let nanos = instant.unix_timestamp_nanos();
    if nanos < 0 {
        return None;
    }
    u64::try_from(nanos / 1_000_000).ok()
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
