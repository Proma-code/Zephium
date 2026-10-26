//! Fixed-origin OpenCode Go transport for explicitly authorized coding-agent
//! traffic. This uses a Zephium-owned Console key, never OpenCode CLI credentials.
//! Public catalog availability is not authentication or subscription validation.
//! Provider Extra Usage may spend balance; this client never enables it or retries.

pub mod quota;

use super::sse::{SseEvent, SseFramer};
use super::{chat, LeadSecret, LeadTarget};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use zephium_core::work::model::*;

const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_MODELS: usize = 256;
const MAX_TOOLS: usize = 64;
const MAX_EVENTS: usize = 50_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const ORIGIN: &str = "https://opencode.ai/inference/go";
const USER_AGENT: &str = concat!("Zephium/", env!("CARGO_PKG_VERSION"), " (coding-agent)");

/// Closed failures: no OpenCode Go-authored error text or secret is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoFault {
    /// A supported text/function request could not be formed.
    InvalidRequest,
    /// The local HTTP client could not be initialized.
    Unavailable,
    /// The OpenCode Go refused its own access key.
    Unauthorized,
    /// The OpenCode Go refused access to this request or model.
    Forbidden,
    /// The OpenCode Go is limiting requests; the hint never causes an automatic retry.
    RateLimited {
        /// A bounded integer-seconds Retry-After, in milliseconds, when supplied.
        retry_after_ms: Option<u64>,
    },
    /// A bounded operation expired.
    Timeout,
    /// The response violates the supported wire contract.
    Protocol,
    /// A bounded request, catalog, stream or output was exceeded.
    TooLarge,
    /// The stream was truncated, stopped at a length limit, or never completed.
    Incomplete,
    /// A completed reply omitted the token counters Work needs for accounting.
    UsageUnavailable,
}

impl GoFault {
    /// Content-free classification for native Work consumers.
    pub const fn model_fault(self) -> OpenCodeGoModelFault {
        match self {
            Self::InvalidRequest | Self::TooLarge => OpenCodeGoModelFault::Unsupported,
            Self::Unavailable | Self::Timeout => OpenCodeGoModelFault::Unavailable,
            Self::Unauthorized => OpenCodeGoModelFault::Unauthorized,
            Self::Forbidden => OpenCodeGoModelFault::Forbidden,
            Self::RateLimited { .. } => OpenCodeGoModelFault::RateLimited,
            Self::Protocol => OpenCodeGoModelFault::Protocol,
            Self::Incomplete => OpenCodeGoModelFault::Incomplete,
            Self::UsageUnavailable => OpenCodeGoModelFault::UsageUnavailable,
        }
    }
}

/// What is known about a failed observation, not a claim about upstream billing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoFailure {
    /// Content-free failure classification.
    pub fault: GoFault,
    /// A POST may have reached the OpenCode Go. GET catalog reads are always false.
    pub dispatched: bool,
    /// At least one text, reasoning, refusal or tool delta was received.
    pub output_started: bool,
}

impl GoFailure {
    /// Preserve source identity and uncertainty through the Work retry boundary.
    /// Go failures never become the generic retriable Network/RateLimited.
    pub fn into_model_error(self) -> WorkModelError {
        WorkModelError::OpenCodeGo {
            fault: self.fault.model_fault(),
            usage_unknown: self.dispatched || self.output_started,
        }
    }

    fn before(fault: GoFault) -> Self {
        Self {
            fault,
            dispatched: false,
            output_started: false,
        }
    }
    fn sent(fault: GoFault) -> Self {
        Self {
            fault,
            dispatched: true,
            output_started: false,
        }
    }
}

/// Opaque native-generated conversation identity; never prompt text or cache keys.
#[derive(Clone, Eq, PartialEq)]
pub struct SessionId(String);
impl SessionId {
    /// Accept the native SHA-256 hexadecimal identity only. No network I/O.
    pub fn parse(value: String) -> Result<Self, GoFault> {
        if !hex_id(&value, 64) {
            return Err(GoFault::InvalidRequest);
        }
        Ok(Self(value))
    }
    /// The header value, for the native transport boundary only.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionId([opaque])")
    }
}
fn hex_id(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A model at the intersection of the public catalog and the supported contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoModel {
    /// Exact documented upstream identifier.
    pub id: String,
    /// Plain bounded display name; this does not encode entitlement.
    pub display_name: String,
    /// The exact API family documented for this model, not a prefix inference.
    pub wire: WorkModelWire,
}
/// Why a listed model is not selectable through this transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedReason {
    /// No exact reviewed Go table entry; never guess a model's API family.
    UnknownModel,
    /// Anthropic or Gemini wire is not implemented by this bounded transport.
    ProtocolNotImplemented,
    /// A contributor model needs an explicit training disclosure not yet offered.
    TrainingDisclosureRequired,
}
/// An observed but intentionally unsupported public catalog row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedModel {
    /// Exact bounded identifier, not a diagnostic error string.
    pub id: String,
    /// Closed capability reason.
    pub reason: UnsupportedReason,
}
/// Public model availability only; contains no key or subscription status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoCatalog {
    /// Exact reviewed models whose wire is implemented.
    pub supported: Vec<GoModel>,
    /// All other observed rows, with closed unsupported reasons.
    pub unsupported: Vec<UnsupportedModel>,
}
/// Provider-reported funding source for this request, not an entitlement claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BillingSource {
    /// Provider reports free usage.
    Free,
    /// Provider reports balance usage, potentially enabled by Console Extra Usage.
    Balance,
    /// Provider reports Go plan allowance usage.
    Go,
    /// Provider reports Go Plus plan allowance usage.
    GoPlus,
    /// Provider reports a user-owned upstream key, possibly estimated cost.
    Byok,
}
/// A bounded nonnegative amount as normalized JSON decimal text, not a quota.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportedUsd(String);
impl ReportedUsd {
    /// Provider-reported amount for display; no floating-point budget conversion.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Billed-token breakdown, distinct from the upstream wire's usage counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoBilledTokens {
    /// Billed uncached input, excluding cache reads and writes.
    pub input: u64,
    /// Billed output, including reasoning.
    pub output: u64,
    /// Reasoning included in billed output.
    pub reasoning: u64,
    /// Billed cache reads, not necessarily part of raw wire input_tokens.
    pub cache_read: u64,
    /// Billed five-minute cache writes.
    pub cache_write_5m: u64,
    /// Billed one-hour cache writes.
    pub cache_write_1h: u64,
}
/// Billing attribution observed only after successful terminal validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoBilling {
    /// Mandatory documented attribution, never guessed from the selected plan.
    pub source: BillingSource,
    /// Reported request cost; with BYOK this may be an estimate, not a Console charge.
    pub cost_usd: Option<ReportedUsd>,
    /// Reported list price for this individual request, if supplied.
    pub list_cost_usd: Option<ReportedUsd>,
    /// Go allowance consumed by THIS REQUEST, never remaining monthly allowance.
    pub allowance_consumed_usd: Option<ReportedUsd>,
    /// Provider says BYOK cost is estimated; never a verified charge.
    pub estimated: bool,
    /// Optional separate billing counters; never used to rewrite raw wire usage.
    pub tokens: Option<GoBilledTokens>,
}
/// Fully validated outcome and separate billing provenance. No tools are emitted
/// as callbacks; the native owner must still validate its revocable lease.
pub struct GoOutcome {
    /// Completed assistant turn with measured token counters.
    pub outcome: WorkModelOutcome,
    /// Provider-reported request billing; not a guarantee of final posted charges.
    pub billing: GoBilling,
}
impl std::fmt::Debug for GoOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoOutcome")
            .field("stop", &self.outcome.stop)
            .field("parts", &self.outcome.assistant.len())
            .field("billing", &self.billing)
            .finish()
    }
}

/// Immutable fixed-origin Console-key binding. No auth-file reads or fallback.
#[derive(Clone)]
pub struct OpenCodeGoClient {
    connection_scope: String,
    key: Arc<LeadSecret>,
    http: reqwest::Client,
    #[cfg(test)]
    fixture_origin: Option<String>,
}
impl OpenCodeGoClient {
    /// Validate native connection identity and a bounded visible-ASCII key, with
    /// no network access. Every key replacement must get a new native identity.
    pub fn new(key: Arc<LeadSecret>, connection_scope: String) -> Result<Self, GoFault> {
        if !hex_id(&connection_scope, 32)
            || key
                .header("Bearer ")
                .map_err(|_| GoFault::InvalidRequest)?
                .as_bytes()
                .len()
                > 1031
        {
            return Err(GoFault::InvalidRequest);
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(READ_TIMEOUT)
            .timeout(CALL_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| GoFault::Unavailable)?;
        Ok(Self {
            connection_scope,
            key,
            http,
            #[cfg(test)]
            fixture_origin: None,
        })
    }
    fn endpoint(&self, suffix: &str) -> String {
        #[cfg(test)]
        if let Some(origin) = &self.fixture_origin {
            return format!("{origin}/{suffix}");
        }
        format!("{ORIGIN}/{suffix}")
    }
    /// Explicit unauthenticated GET of public availability. Success does not
    /// validate the stored key, subscription, balance, privacy settings or quota.
    pub async fn models(&self) -> Result<GoCatalog, GoFailure> {
        let mut response = self
            .http
            .get(self.endpoint("v1/models"))
            .header(ACCEPT, "application/json")
            .header("accept-encoding", "identity")
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .map_err(|e| GoFailure::before(transport_fault(&e)))?;
        if response.status().as_u16() != 200 {
            return Err(GoFailure::before(http_fault(
                response.status().as_u16(),
                response.headers().get(RETRY_AFTER),
            )));
        }
        if !media_is(&response, "application/json") {
            return Err(GoFailure::before(GoFault::Protocol));
        }
        let bytes = bounded_body(&mut response, MAX_CATALOG_BYTES)
            .await
            .map_err(GoFailure::before)?;
        parse_models(&bytes).map_err(GoFailure::before)
    }
    /// One authorized coding-agent call. The native owner supplies the SAME
    /// session for main/auxiliary calls in a conversation. Drop cancels local I/O,
    /// not a promise that upstream work or charges have been undone.
    pub async fn call(
        &self,
        session: &SessionId,
        request: WorkModelRequest,
        events: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<GoOutcome, GoFailure> {
        let target = LeadTarget::compatible("https://opencode.ai/inference/go/openai/v1")
            .map_err(|_| GoFailure::before(GoFault::Unavailable))?;
        let body = request_body(&request, &target, session, &self.connection_scope)
            .map_err(GoFailure::before)?;
        let suffix = match request.model.wire {
            WorkModelWire::ChatCompletions => "openai/v1/chat/completions",
            WorkModelWire::OpenAiResponses => "openai/v1/responses",
            _ => return Err(GoFailure::before(GoFault::InvalidRequest)),
        };
        let header = self
            .key
            .header("Bearer ")
            .map_err(|_| GoFailure::before(GoFault::InvalidRequest))?;
        let mut session_header = reqwest::header::HeaderValue::from_str(session.as_str())
            .map_err(|_| GoFailure::before(GoFault::InvalidRequest))?;
        session_header.set_sensitive(true);
        let mut decoder = GoStream::new(&request, session, &self.connection_scope);
        let future = async {
            let mut response = self
                .http
                .post(self.endpoint(suffix))
                .header(AUTHORIZATION, header)
                .header("x-opencode-session", session_header)
                .header(ACCEPT, "text/event-stream")
                .header(CONTENT_TYPE, "application/json")
                .header("accept-encoding", "identity")
                .body(body)
                .send()
                .await
                .map_err(|e| GoFailure {
                    fault: transport_fault(&e),
                    dispatched: !e.is_connect(),
                    output_started: false,
                })?;
            if response.status().as_u16() != 200 {
                return Err(GoFailure::sent(http_fault(
                    response.status().as_u16(),
                    response.headers().get(RETRY_AFTER),
                )));
            }
            if !media_is(&response, "text/event-stream") {
                return Err(GoFailure::sent(GoFault::Protocol));
            }
            loop {
                let chunk = tokio::time::timeout(READ_TIMEOUT, response.chunk())
                    .await
                    .map_err(|_| decoder.failure(GoFault::Timeout))?
                    .map_err(|e| decoder.failure(transport_fault(&e)))?;
                match chunk {
                    Some(chunk) => {
                        if let Some(outcome) = decoder.push(&chunk, events)? {
                            return Ok(outcome);
                        }
                    }
                    None => return Err(decoder.failure(GoFault::Incomplete)),
                }
            }
        };
        match tokio::time::timeout(CALL_TIMEOUT, future).await {
            Ok(result) => result,
            Err(_) => Err(decoder.failure(GoFault::Timeout)),
        }
    }
}

fn media_is(response: &reqwest::Response, expected: &str) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case(expected))
}
fn transport_fault(error: &reqwest::Error) -> GoFault {
    if error.is_timeout() {
        GoFault::Timeout
    } else {
        GoFault::Unavailable
    }
}
fn http_fault(status: u16, retry: Option<&reqwest::header::HeaderValue>) -> GoFault {
    match status {
        401 => GoFault::Unauthorized,
        403 => GoFault::Forbidden,
        429 => GoFault::RateLimited {
            retry_after_ms: retry
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .and_then(|s| s.checked_mul(1000))
                .filter(|ms| *ms <= 86_400_000),
        },
        408 | 504 => GoFault::Timeout,
        502 | 503 => GoFault::Unavailable,
        400 | 404 | 405 | 422 => GoFault::InvalidRequest,
        413 => GoFault::TooLarge,
        _ => GoFault::Protocol,
    }
}
async fn bounded_body(response: &mut reqwest::Response, max: usize) -> Result<Vec<u8>, GoFault> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(GoFault::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| transport_fault(&e))? {
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err(GoFault::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn bounded_label(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn model_id(value: &str) -> bool {
    bounded_label(value, 160)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b))
}
fn tool_name(value: &str) -> bool {
    bounded_label(value, 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

// Exact Go model/protocol table reviewed 2026-10-26:
// https://opencode.ai/v2/docs/console/go/#endpoints
// New model names never inherit a wire by prefix. Catalog rows must intersect.
fn table_wire(id: &str) -> Option<WorkModelWire> {
    match id {
        "grok-4.7"
        | "gpt-6-luna"
        | "gpt-5.6-luna"
        | "muse-spark-1.3-contributor"
        | "muse-spark-1.2-contributor" => Some(WorkModelWire::OpenAiResponses),
        "glm-5.3-flash"
        | "glm-5.3"
        | "glm-5.2"
        | "kimi-k3.1-pro"
        | "kimi-k3"
        | "kimi-k2.7-code"
        | "longcat-2.0"
        | "deepseek-v4.1-flash"
        | "deepseek-v4-pro"
        | "mimo-v2.6-flash"
        | "mimo-v2.6-pro"
        | "mimo-v2.5"
        | "mimo-v2.5-pro"
        | "minimax-m3.1-flash"
        | "hy4-preview"
        | "hy3"
        | "step-5-preview"
        | "beam-free" => Some(WorkModelWire::ChatCompletions),
        "claude-haiku-5-5" | "minimax-m3" | "qwen3.8-max" | "qwen3.8-flash" | "qwen3.7-plus" => {
            Some(WorkModelWire::AnthropicMessages)
        }
        "gemini-4-argon" => Some(WorkModelWire::Gemini),
        _ => None,
    }
}
fn unsupported(id: &str) -> Option<UnsupportedReason> {
    if matches!(
        id,
        "muse-spark-1.3-contributor" | "muse-spark-1.2-contributor"
    ) {
        return Some(UnsupportedReason::TrainingDisclosureRequired);
    }
    match table_wire(id) {
        Some(WorkModelWire::ChatCompletions | WorkModelWire::OpenAiResponses) => None,
        Some(_) => Some(UnsupportedReason::ProtocolNotImplemented),
        None => Some(UnsupportedReason::UnknownModel),
    }
}
fn supported_wire(id: &str) -> Option<WorkModelWire> {
    if unsupported(id).is_some() {
        None
    } else {
        table_wire(id)
    }
}
fn parse_models(bytes: &[u8]) -> Result<GoCatalog, GoFault> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(GoFault::TooLarge);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| GoFault::Protocol)?;
    if value
        .get("object")
        .is_some_and(|v| v.as_str() != Some("list"))
    {
        return Err(GoFault::Protocol);
    }
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or(GoFault::Protocol)?;
    if rows.len() > MAX_MODELS {
        return Err(GoFault::TooLarge);
    }
    let mut result = GoCatalog {
        supported: vec![],
        unsupported: vec![],
    };
    let mut seen = HashSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| model_id(s))
            .ok_or(GoFault::Protocol)?;
        if !seen.insert(id) {
            return Err(GoFault::Protocol);
        }
        if let Some(reason) = unsupported(id) {
            result.unsupported.push(UnsupportedModel {
                id: id.into(),
                reason,
            });
        } else {
            let name = match row.get("display_name") {
                Some(v) => v
                    .as_str()
                    .filter(|s| bounded_label(s, 160))
                    .ok_or(GoFault::Protocol)?,
                None => id,
            };
            result.supported.push(GoModel {
                id: id.into(),
                display_name: name.into(),
                wire: table_wire(id).ok_or(GoFault::Protocol)?,
            });
        }
    }
    Ok(result)
}

fn request_body(
    request: &WorkModelRequest,
    target: &LeadTarget,
    session: &SessionId,
    connection_scope: &str,
) -> Result<Vec<u8>, GoFault> {
    if request.model.provider != WorkModelProvider::OpenCodeGo
        || supported_wire(&request.model.model) != Some(request.model.wire)
        || !model_id(&request.model.model)
        || request.messages.len() > 512
        || request.tools.len() > MAX_TOOLS
        || request.system.len() > 64
        || request.max_output_tokens == 0
        || request.max_output_tokens > 32_768
        || request.native_search
        || request.reasoning.is_some()
        || request
            .cache_key
            .as_ref()
            .is_some_and(|key| key.len() > 256)
    {
        return Err(GoFault::InvalidRequest);
    }
    let mut bytes = 0usize;
    let mut charge = |value: &str| -> Result<(), GoFault> {
        bytes = bytes.saturating_add(value.len());
        if bytes > MAX_REQUEST_BYTES {
            Err(GoFault::TooLarge)
        } else {
            Ok(())
        }
    };
    let mut names = HashSet::new();
    for tool in &request.tools {
        if !tool_name(&tool.name)
            || !names.insert(&tool.name)
            || !tool.schema.is_object()
            || tool.description.len() > 4096
        {
            return Err(GoFault::InvalidRequest);
        }
        let encoded = serde_json::to_string(&tool.schema).map_err(|_| GoFault::InvalidRequest)?;
        if encoded.len() > MAX_ARGUMENT_BYTES {
            return Err(GoFault::TooLarge);
        }
        charge(&encoded)?;
        charge(&tool.description)?;
    }
    if match &request.tool_choice {
        WorkModelToolChoice::Auto => false,
        WorkModelToolChoice::Required => request.tools.is_empty(),
        WorkModelToolChoice::Named(name) => !names.contains(name),
    } {
        return Err(GoFault::InvalidRequest);
    }
    for block in &request.system {
        charge(&block.text)?;
    }
    for message in &request.messages {
        let is_user = matches!(message, WorkModelMessage::User(_));
        match message {
            WorkModelMessage::User(parts) | WorkModelMessage::Assistant(parts) => {
                if parts.len() > 256 {
                    return Err(GoFault::TooLarge);
                }
                if !is_user
                    && !parts.iter().any(|part| {
                        matches!(part,
                    WorkModelPart::Text(text) if !text.is_empty())
                            || matches!(part, WorkModelPart::ToolCall(_))
                    })
                {
                    return Err(GoFault::InvalidRequest);
                }
                for part in parts {
                    if is_user && !matches!(part, WorkModelPart::Text(_)) {
                        return Err(GoFault::InvalidRequest);
                    }
                    match part {
                        WorkModelPart::Text(text) => charge(text)?,
                        WorkModelPart::ToolCall(call) => {
                            if !bounded_label(&call.id, 256)
                                || !tool_name(&call.name)
                                || !call.arguments.is_object()
                            {
                                return Err(GoFault::InvalidRequest);
                            }
                            let args = serde_json::to_string(&call.arguments)
                                .map_err(|_| GoFault::InvalidRequest)?;
                            if args.len() > MAX_ARGUMENT_BYTES {
                                return Err(GoFault::TooLarge);
                            }
                            charge(&args)?;
                        }
                        WorkModelPart::Replay(value) => {
                            let replay = replay_value(
                                value,
                                request.model.wire,
                                &request.model.model,
                                connection_scope,
                                session,
                            )?;
                            let encoded = serde_json::to_string(&replay)
                                .map_err(|_| GoFault::InvalidRequest)?;
                            charge(&encoded)?;
                        }
                        WorkModelPart::Image { .. } => return Err(GoFault::InvalidRequest),
                    }
                }
            }
            WorkModelMessage::ToolResults(results) => {
                if results.len() > MAX_TOOLS {
                    return Err(GoFault::TooLarge);
                }
                for result in results {
                    if !bounded_label(&result.call, 256) {
                        return Err(GoFault::InvalidRequest);
                    }
                    charge(&result.content)?;
                }
            }
        }
    }
    // Clone only after all bounded validation; never copy arbitrary replay keys.
    let mut translated = request.clone();
    for message in &mut translated.messages {
        if let WorkModelMessage::Assistant(parts) = message {
            for part in parts {
                if let WorkModelPart::Replay(value) = part {
                    let raw = replay_value(
                        value,
                        request.model.wire,
                        &request.model.model,
                        connection_scope,
                        session,
                    )?;
                    *value = match request.model.wire {
                        WorkModelWire::ChatCompletions => {
                            json!({"wire":"chat","value":{"reasoning_content":raw}})
                        }
                        WorkModelWire::OpenAiResponses => json!({"wire":"openai","value":raw}),
                        _ => return Err(GoFault::InvalidRequest),
                    };
                }
            }
        }
    }
    let mut body = match request.model.wire {
        WorkModelWire::ChatCompletions => chat::body(&translated, target),
        WorkModelWire::OpenAiResponses => super::openai::body(&translated, target),
        _ => return Err(GoFault::InvalidRequest),
    }
    .map_err(|_| GoFault::InvalidRequest)?;
    // Session identity comes only from the native caller, never cache_key.
    if let Some(object) = body.as_object_mut() {
        object.remove("prompt_cache_key");
    }
    let body = serde_json::to_vec(&body).map_err(|_| GoFault::InvalidRequest)?;
    if body.len() > MAX_REQUEST_BYTES {
        return Err(GoFault::TooLarge);
    }
    Ok(body)
}

fn wire_name(wire: WorkModelWire) -> &'static str {
    match wire {
        WorkModelWire::ChatCompletions => "chat",
        WorkModelWire::OpenAiResponses => "responses",
        _ => "unsupported",
    }
}
fn replay_envelope(
    wire: WorkModelWire,
    model: &str,
    connection_scope: &str,
    session: &SessionId,
    value: Value,
) -> Value {
    json!({"wire":"opencode_go","protocol":wire_name(wire),"connection_scope":connection_scope,"session":session.as_str(),"model":model,"value":value})
}
fn replay_value(
    value: &Value,
    wire: WorkModelWire,
    model: &str,
    connection_scope: &str,
    session: &SessionId,
) -> Result<Value, GoFault> {
    let fields = value
        .as_object()
        .filter(|v| v.len() == 6)
        .ok_or(GoFault::InvalidRequest)?;
    for (key, expected) in [
        ("wire", "opencode_go"),
        ("protocol", wire_name(wire)),
        ("connection_scope", connection_scope),
        ("session", session.as_str()),
        ("model", model),
    ] {
        if fields.get(key).and_then(Value::as_str) != Some(expected) {
            return Err(GoFault::InvalidRequest);
        }
    }
    let raw = fields.get("value").ok_or(GoFault::InvalidRequest)?;
    match wire {
        WorkModelWire::ChatCompletions => raw
            .as_str()
            .filter(|s| s.len() <= MAX_OUTPUT_BYTES)
            .map(|s| json!(s))
            .ok_or(GoFault::InvalidRequest),
        WorkModelWire::OpenAiResponses => reasoning_item(raw).map_err(|_| GoFault::InvalidRequest),
        _ => Err(GoFault::InvalidRequest),
    }
}
fn reasoning_item(item: &Value) -> Result<Value, GoFault> {
    let object = item.as_object().ok_or(GoFault::Protocol)?;
    if object.keys().any(|k| {
        !matches!(
            k.as_str(),
            "type" | "id" | "summary" | "encrypted_content" | "status"
        )
    }) || item.get("type").and_then(Value::as_str) != Some("reasoning")
    {
        return Err(GoFault::Protocol);
    }
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| bounded_label(s, 256))
        .ok_or(GoFault::Protocol)?;
    let summary = item
        .get("summary")
        .and_then(Value::as_array)
        .filter(|s| s.len() <= 256)
        .ok_or(GoFault::Protocol)?;
    let mut count = 0usize;
    let mut normalized = Vec::new();
    for part in summary {
        if part.get("type").and_then(Value::as_str) != Some("summary_text") {
            return Err(GoFault::Protocol);
        }
        let text = part
            .get("text")
            .and_then(Value::as_str)
            .ok_or(GoFault::Protocol)?;
        count = count.saturating_add(text.len());
        if count > MAX_OUTPUT_BYTES {
            return Err(GoFault::TooLarge);
        }
        normalized.push(json!({"type":"summary_text","text":text}));
    }
    let mut value = json!({"type":"reasoning","id":id,"summary":normalized});
    if let Some(encrypted) = item.get("encrypted_content").filter(|v| !v.is_null()) {
        let text = encrypted
            .as_str()
            .filter(|s| s.len() <= MAX_OUTPUT_BYTES)
            .ok_or(GoFault::Protocol)?;
        value["encrypted_content"] = json!(text);
    }
    if item
        .get("status")
        .is_some_and(|v| v.as_str() != Some("completed"))
    {
        return Err(GoFault::Incomplete);
    }
    Ok(value)
}

#[derive(Default)]
struct ToolDelta {
    id: Option<String>,
    name: String,
    arguments: String,
}
struct ChatStream {
    started: bool,
    text: String,
    reasoning: String,
    output_bytes: usize,
    refused: bool,
    finish: Option<String>,
    usage: Option<(WorkModelUsage, GoBilling)>,
    calls: BTreeMap<usize, ToolDelta>,
    allowed: HashSet<String>,
    requires_tool: bool,
    session: SessionId,
    connection_scope: String,
    model: String,
    response_id: Option<String>,
}
impl ChatStream {
    fn new(request: &WorkModelRequest, session: &SessionId, connection_scope: &str) -> Self {
        Self {
            started: false,
            text: String::new(),
            reasoning: String::new(),
            output_bytes: 0,
            refused: false,
            finish: None,
            usage: None,
            calls: BTreeMap::new(),
            allowed: request
                .tools
                .iter()
                .filter(|tool| match &request.tool_choice {
                    WorkModelToolChoice::Named(name) => name == &tool.name,
                    _ => true,
                })
                .map(|t| t.name.clone())
                .collect(),
            requires_tool: request.tool_choice != WorkModelToolChoice::Auto,
            session: session.clone(),
            connection_scope: connection_scope.into(),
            model: request.model.model.clone(),
            response_id: None,
        }
    }
    fn event(
        &mut self,
        frame: SseEvent,
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<Option<GoOutcome>, GoFault> {
        if !matches!(frame.event.as_str(), "" | "message") {
            return Err(GoFault::Protocol);
        }
        if frame.data == "[DONE]" {
            return self.complete().map(Some);
        }
        let value = parse_frame(&frame.data)?;
        if let Some(error) = value.get("error") {
            let code = error
                .get("code")
                .or_else(|| error.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            return Err(match code {
                "rate_limit_exceeded" | "insufficient_quota" => GoFault::RateLimited {
                    retry_after_ms: None,
                },
                "invalid_api_key" | "authentication_error" => GoFault::Unauthorized,
                "permission_denied" => GoFault::Forbidden,
                _ => GoFault::Protocol,
            });
        }
        if !value.is_object()
            || value
                .get("object")
                .is_some_and(|v| v.as_str() != Some("chat.completion.chunk"))
        {
            return Err(GoFault::Protocol);
        }
        if let Some(id) = value.get("id") {
            let id = id
                .as_str()
                .filter(|s| bounded_label(s, 256))
                .ok_or(GoFault::Protocol)?;
            if self.response_id.as_ref().is_some_and(|old| old != id) {
                return Err(GoFault::Protocol);
            }
            self.response_id = Some(id.into());
        }
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .ok_or(GoFault::Protocol)?;
        if choices.len() > 1 {
            return Err(GoFault::Protocol);
        }
        if let Some(choice) = choices.first() {
            if self.finish.is_some() || choice.get("index").and_then(Value::as_u64) != Some(0) {
                return Err(GoFault::Protocol);
            }
            let delta = choice
                .get("delta")
                .and_then(Value::as_object)
                .ok_or(GoFault::Protocol)?;
            self.output_bytes = self.output_bytes.saturating_add(
                serde_json::to_vec(delta)
                    .map_err(|_| GoFault::Protocol)?
                    .len(),
            );
            if self.output_bytes > MAX_OUTPUT_BYTES {
                return Err(GoFault::TooLarge);
            }
            if delta.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "role" | "content" | "reasoning_content" | "refusal" | "tool_calls"
                )
            }) {
                return Err(GoFault::Protocol);
            }
            if let Some(role) = delta.get("role") {
                if role.as_str() != Some("assistant") {
                    return Err(GoFault::Protocol);
                }
            }
            if let Some(content) = delta.get("content").filter(|v| !v.is_null()) {
                let text = content.as_str().ok_or(GoFault::Protocol)?;
                if self.text.len().saturating_add(text.len()) > MAX_OUTPUT_BYTES {
                    return Err(GoFault::TooLarge);
                }
                if !text.is_empty() {
                    self.started = true;
                    self.text.push_str(text);
                    sink(WorkModelEvent::Text(text.into()));
                }
            }
            if let Some(reasoning) = delta.get("reasoning_content").filter(|v| !v.is_null()) {
                let reasoning = reasoning.as_str().ok_or(GoFault::Protocol)?;
                if self.reasoning.len().saturating_add(reasoning.len()) > MAX_OUTPUT_BYTES {
                    return Err(GoFault::TooLarge);
                }
                self.started |= !reasoning.is_empty();
                self.reasoning.push_str(reasoning);
            }
            if let Some(refusal) = delta.get("refusal").filter(|v| !v.is_null()) {
                let refusal = refusal
                    .as_str()
                    .filter(|v| v.len() <= MAX_OUTPUT_BYTES)
                    .ok_or(GoFault::Protocol)?;
                self.refused |= !refusal.is_empty();
                self.started |= !refusal.is_empty();
            }
            if let Some(calls) = delta.get("tool_calls") {
                let calls = calls
                    .as_array()
                    .filter(|v| v.len() <= MAX_TOOLS)
                    .ok_or(GoFault::Protocol)?;
                for call in calls {
                    self.started = true;
                    let index = call
                        .get("index")
                        .and_then(Value::as_u64)
                        .filter(|i| *i < MAX_TOOLS as u64)
                        .ok_or(GoFault::Protocol)? as usize;
                    if call
                        .get("type")
                        .is_some_and(|v| v.as_str() != Some("function"))
                    {
                        return Err(GoFault::Protocol);
                    }
                    let pending = self.calls.entry(index).or_default();
                    if let Some(id) = call.get("id") {
                        let id = id
                            .as_str()
                            .filter(|s| bounded_label(s, 256))
                            .ok_or(GoFault::Protocol)?;
                        if pending.id.as_ref().is_some_and(|old| old != id) {
                            return Err(GoFault::Protocol);
                        }
                        pending.id = Some(id.into());
                    }
                    let function = call
                        .get("function")
                        .and_then(Value::as_object)
                        .ok_or(GoFault::Protocol)?;
                    for (key, target, max) in [
                        ("name", &mut pending.name, 64),
                        ("arguments", &mut pending.arguments, MAX_ARGUMENT_BYTES),
                    ] {
                        if let Some(value) = function.get(key) {
                            let value = value.as_str().ok_or(GoFault::Protocol)?;
                            if target.len().saturating_add(value.len()) > max {
                                return Err(GoFault::TooLarge);
                            }
                            target.push_str(value);
                        }
                    }
                }
            }
            if let Some(finish) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                let finish = finish
                    .as_str()
                    .filter(|v| matches!(*v, "stop" | "tool_calls" | "content_filter" | "length"))
                    .ok_or(GoFault::Protocol)?;
                self.finish = Some(finish.into());
            }
        }
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            if self.finish.is_none() || self.usage.is_some() {
                return Err(GoFault::Protocol);
            }
            self.usage = Some(parse_usage(usage, WorkModelWire::ChatCompletions)?);
        }
        Ok(None)
    }
    fn complete(&mut self) -> Result<GoOutcome, GoFault> {
        let finish = self.finish.as_deref().ok_or(GoFault::Incomplete)?;
        if finish == "length" {
            return Err(GoFault::Incomplete);
        }
        let refused = self.refused || finish == "content_filter";
        if (!refused && self.text.is_empty() && self.calls.is_empty())
            || (!self.calls.is_empty() && (finish != "tool_calls" || refused))
            || (finish == "tool_calls" && self.calls.is_empty())
            || (self.requires_tool && self.calls.is_empty() && !refused)
        {
            return Err(GoFault::Protocol);
        }
        let mut assistant = Vec::new();
        let mut ids = HashSet::new();
        if !self.reasoning.is_empty() {
            assistant.push(WorkModelPart::Replay(replay_envelope(
                WorkModelWire::ChatCompletions,
                &self.model,
                &self.connection_scope,
                &self.session,
                json!(self.reasoning),
            )));
        }
        if !self.text.is_empty() {
            assistant.push(WorkModelPart::Text(self.text.clone()));
        }
        for (expected, (index, pending)) in self.calls.iter().enumerate() {
            if expected != *index
                || !tool_name(&pending.name)
                || !self.allowed.contains(&pending.name)
            {
                return Err(GoFault::Protocol);
            }
            let id = pending.id.as_ref().ok_or(GoFault::Protocol)?;
            if !ids.insert(id) {
                return Err(GoFault::Protocol);
            }
            let arguments: Value =
                serde_json::from_str(&pending.arguments).map_err(|_| GoFault::Protocol)?;
            if !arguments.is_object() {
                return Err(GoFault::Protocol);
            }
            assistant.push(WorkModelPart::ToolCall(WorkModelToolCall {
                id: id.clone(),
                name: pending.name.clone(),
                arguments,
            }));
        }
        let (usage, billing) = self.usage.take().ok_or(GoFault::UsageUnavailable)?;
        Ok(GoOutcome {
            outcome: WorkModelOutcome {
                stop: if refused {
                    WorkModelStop::Refused
                } else if self.calls.is_empty() {
                    WorkModelStop::EndTurn
                } else {
                    WorkModelStop::ToolUse
                },
                assistant,
                usage,
            },
            billing,
        })
    }
}

struct GoStream {
    framer: SseFramer,
    bytes: usize,
    events: usize,
    state: WireStream,
}
enum WireStream {
    Chat(Box<ChatStream>),
    Responses(Box<ResponseStream>),
}
impl GoStream {
    fn new(request: &WorkModelRequest, session: &SessionId, scope: &str) -> Self {
        let state = if request.model.wire == WorkModelWire::OpenAiResponses {
            WireStream::Responses(Box::new(ResponseStream::new(request, session, scope)))
        } else {
            WireStream::Chat(Box::new(ChatStream::new(request, session, scope)))
        };
        Self {
            framer: SseFramer::default(),
            bytes: 0,
            events: 0,
            state,
        }
    }
    fn failure(&self, fault: GoFault) -> GoFailure {
        let output_started = match &self.state {
            WireStream::Chat(s) => s.started,
            WireStream::Responses(s) => s.started,
        };
        GoFailure {
            fault,
            dispatched: true,
            output_started,
        }
    }
    fn push(
        &mut self,
        chunk: &[u8],
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<Option<GoOutcome>, GoFailure> {
        self.bytes = self.bytes.saturating_add(chunk.len());
        if self.bytes > MAX_STREAM_BYTES {
            return Err(self.failure(GoFault::TooLarge));
        }
        let mut frames = Vec::new();
        self.framer
            .push(chunk, &mut frames)
            .map_err(|_| self.failure(GoFault::Protocol))?;
        let mut outcome = None;
        for frame in frames {
            if outcome.is_some() {
                return Err(self.failure(GoFault::Protocol));
            }
            self.events += 1;
            if self.events > MAX_EVENTS {
                return Err(self.failure(GoFault::TooLarge));
            }
            outcome = match &mut self.state {
                WireStream::Chat(s) => s.event(frame, sink),
                WireStream::Responses(s) => s.event(frame, sink),
            }
            .map_err(|fault| self.failure(fault))?;
        }
        Ok(outcome)
    }
}
struct ResponseStream {
    started: bool,
    progress_bytes: usize,
    text: BTreeMap<(usize, usize), String>,
    arguments: BTreeMap<usize, String>,
    allowed: HashSet<String>,
    requires_tool: bool,
    connection_scope: String,
    session: SessionId,
    model: String,
    response_id: Option<String>,
}
impl ResponseStream {
    fn new(request: &WorkModelRequest, session: &SessionId, scope: &str) -> Self {
        Self {
            started: false,
            progress_bytes: 0,
            text: BTreeMap::new(),
            arguments: BTreeMap::new(),
            allowed: request
                .tools
                .iter()
                .filter(|tool| match &request.tool_choice {
                    WorkModelToolChoice::Named(n) => n == &tool.name,
                    _ => true,
                })
                .map(|t| t.name.clone())
                .collect(),
            requires_tool: request.tool_choice != WorkModelToolChoice::Auto,
            connection_scope: scope.into(),
            session: session.clone(),
            model: request.model.model.clone(),
            response_id: None,
        }
    }
    fn observe_id(&mut self, id: &Value) -> Result<(), GoFault> {
        let id = id
            .as_str()
            .filter(|s| bounded_label(s, 256))
            .ok_or(GoFault::Protocol)?;
        if self.response_id.as_ref().is_some_and(|old| old != id) {
            return Err(GoFault::Protocol);
        }
        self.response_id = Some(id.into());
        Ok(())
    }
    fn index(value: &Value, key: &str, max: usize) -> Result<usize, GoFault> {
        value
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| *n < max as u64)
            .map(|n| n as usize)
            .ok_or(GoFault::Protocol)
    }
    fn progress(&mut self, value: &Value) -> Result<String, GoFault> {
        let delta = value
            .get("delta")
            .and_then(Value::as_str)
            .ok_or(GoFault::Protocol)?;
        self.progress_bytes = self.progress_bytes.saturating_add(delta.len());
        if self.progress_bytes > MAX_OUTPUT_BYTES {
            return Err(GoFault::TooLarge);
        }
        self.started |= !delta.is_empty();
        Ok(delta.into())
    }
    fn event(
        &mut self,
        frame: SseEvent,
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<Option<GoOutcome>, GoFault> {
        if frame.data == "[DONE]" {
            return Err(GoFault::Incomplete);
        }
        let value = parse_frame(&frame.data)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(GoFault::Protocol)?;
        if !frame.event.is_empty() && frame.event != kind {
            return Err(GoFault::Protocol);
        }
        if let Some(id) = value.get("response_id") {
            self.observe_id(id)?;
        }
        match kind {
            "response.created" | "response.in_progress" => {
                let response = value.get("response").ok_or(GoFault::Protocol)?;
                self.observe_id(response.get("id").ok_or(GoFault::Protocol)?)?;
                if response.get("status").and_then(Value::as_str) != Some("in_progress") {
                    return Err(GoFault::Protocol);
                }
            }
            "response.output_text.delta" => {
                let index = Self::index(&value, "output_index", MAX_TOOLS)?;
                let content = Self::index(&value, "content_index", 256)?;
                let delta = self.progress(&value)?;
                self.text
                    .entry((index, content))
                    .or_default()
                    .push_str(&delta);
                if !delta.is_empty() {
                    sink(WorkModelEvent::Text(delta));
                }
            }
            "response.function_call_arguments.delta" => {
                let index = Self::index(&value, "output_index", MAX_TOOLS)?;
                let delta = self.progress(&value)?;
                let args = self.arguments.entry(index).or_default();
                if args.len().saturating_add(delta.len()) > MAX_ARGUMENT_BYTES {
                    return Err(GoFault::TooLarge);
                }
                args.push_str(&delta);
            }
            "response.refusal.delta"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta" => {
                self.progress(&value)?;
            }
            "response.output_item.added" | "response.output_item.done" => {
                Self::index(&value, "output_index", MAX_TOOLS)?;
                let item = value.get("item").ok_or(GoFault::Protocol)?;
                if !matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("message" | "function_call" | "reasoning")
                ) {
                    return Err(GoFault::Protocol);
                }
                self.started = true;
            }
            "response.content_part.added" | "response.content_part.done" => {
                Self::index(&value, "output_index", MAX_TOOLS)?;
                Self::index(&value, "content_index", 256)?;
                if !matches!(
                    value.pointer("/part/type").and_then(Value::as_str),
                    Some("output_text" | "refusal")
                ) {
                    return Err(GoFault::Protocol);
                }
            }
            "response.output_text.done"
            | "response.refusal.done"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.done" => {
                Self::index(&value, "output_index", MAX_TOOLS)?;
            }
            "response.completed" => {
                let response = value.get("response").ok_or(GoFault::Protocol)?;
                self.observe_id(response.get("id").ok_or(GoFault::Protocol)?)?;
                return self.complete(response, sink).map(Some);
            }
            "response.incomplete" => return Err(GoFault::Incomplete),
            "response.failed" | "error" => return Err(stream_fault(&value)),
            _ => return Err(GoFault::Protocol),
        }
        Ok(None)
    }
    fn complete(
        &self,
        response: &Value,
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<GoOutcome, GoFault> {
        if response.get("status").and_then(Value::as_str) != Some("completed")
            || response.get("error").is_some_and(|v| !v.is_null())
        {
            return Err(GoFault::Incomplete);
        }
        let (usage, billing) = parse_usage(
            response
                .get("usage")
                .filter(|v| !v.is_null())
                .ok_or(GoFault::UsageUnavailable)?,
            WorkModelWire::OpenAiResponses,
        )?;
        let output = response
            .get("output")
            .and_then(Value::as_array)
            .filter(|v| v.len() <= MAX_TOOLS)
            .ok_or(GoFault::Protocol)?;
        if serde_json::to_vec(output)
            .map_err(|_| GoFault::Protocol)?
            .len()
            > MAX_OUTPUT_BYTES
        {
            return Err(GoFault::TooLarge);
        }
        let mut assistant = Vec::new();
        let mut ids = HashSet::new();
        let mut item_ids = HashSet::new();
        let mut refused = false;
        let mut count = 0usize;
        let mut final_text = BTreeMap::new();
        let mut final_args = BTreeMap::new();
        let mut fallback_text = Vec::new();
        for (index, item) in output.iter().enumerate() {
            if let Some(id) = item.get("id") {
                let id = id
                    .as_str()
                    .filter(|s| bounded_label(s, 256))
                    .ok_or(GoFault::Protocol)?;
                if !item_ids.insert(id) {
                    return Err(GoFault::Protocol);
                }
            }
            if item
                .get("status")
                .is_some_and(|s| s.as_str() != Some("completed"))
            {
                return Err(GoFault::Incomplete);
            }
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if item.get("role").and_then(Value::as_str) != Some("assistant") {
                        return Err(GoFault::Protocol);
                    }
                    let content = item
                        .get("content")
                        .and_then(Value::as_array)
                        .filter(|v| v.len() <= 256)
                        .ok_or(GoFault::Protocol)?;
                    let mut text = String::new();
                    for (ci, part) in content.iter().enumerate() {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                let piece = part
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .ok_or(GoFault::Protocol)?;
                                count = count.saturating_add(piece.len());
                                if count > MAX_OUTPUT_BYTES {
                                    return Err(GoFault::TooLarge);
                                }
                                final_text.insert((index, ci), piece.to_owned());
                                text.push_str(piece);
                                if !self.text.contains_key(&(index, ci)) && !piece.is_empty() {
                                    fallback_text.push(piece.to_owned());
                                }
                            }
                            Some("refusal") => {
                                let text = part
                                    .get("refusal")
                                    .and_then(Value::as_str)
                                    .filter(|s| s.len() <= MAX_OUTPUT_BYTES)
                                    .ok_or(GoFault::Protocol)?;
                                refused |= !text.is_empty();
                            }
                            _ => return Err(GoFault::Protocol),
                        }
                    }
                    if !text.is_empty() {
                        assistant.push(WorkModelPart::Text(text));
                    }
                }
                Some("function_call") => {
                    let id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .filter(|s| bounded_label(s, 256))
                        .ok_or(GoFault::Protocol)?;
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|s| tool_name(s) && self.allowed.contains(*s))
                        .ok_or(GoFault::Protocol)?;
                    if !ids.insert(id) || item.get("namespace").is_some_and(|n| !n.is_null()) {
                        return Err(GoFault::Protocol);
                    }
                    let raw = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|s| s.len() <= MAX_ARGUMENT_BYTES)
                        .ok_or(GoFault::Protocol)?;
                    final_args.insert(index, raw.to_owned());
                    let arguments: Value =
                        serde_json::from_str(raw).map_err(|_| GoFault::Protocol)?;
                    if !arguments.is_object() {
                        return Err(GoFault::Protocol);
                    }
                    assistant.push(WorkModelPart::ToolCall(WorkModelToolCall {
                        id: id.into(),
                        name: name.into(),
                        arguments,
                    }));
                }
                Some("reasoning") => assistant.push(WorkModelPart::Replay(replay_envelope(
                    WorkModelWire::OpenAiResponses,
                    &self.model,
                    &self.connection_scope,
                    &self.session,
                    reasoning_item(item)?,
                ))),
                _ => return Err(GoFault::Protocol),
            }
        }
        for (key, text) in &self.text {
            if final_text.get(key) != Some(text) {
                return Err(GoFault::Protocol);
            }
        }
        for (key, args) in &self.arguments {
            if final_args.get(key) != Some(args) {
                return Err(GoFault::Protocol);
            }
        }
        if (refused && !ids.is_empty())
            || (!refused
                && ids.is_empty()
                && (self.requires_tool || final_text.values().all(String::is_empty)))
        {
            return Err(GoFault::Protocol);
        }
        // No tools escape. Even fallback text waits for all accounting/terminal checks.
        for text in fallback_text {
            sink(WorkModelEvent::Text(text));
        }
        Ok(GoOutcome {
            outcome: WorkModelOutcome {
                stop: if refused {
                    WorkModelStop::Refused
                } else if ids.is_empty() {
                    WorkModelStop::EndTurn
                } else {
                    WorkModelStop::ToolUse
                },
                usage,
                assistant,
            },
            billing,
        })
    }
}
fn stream_fault(value: &Value) -> GoFault {
    let code = value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match code {
        "rate_limit_exceeded" | "insufficient_quota" => GoFault::RateLimited {
            retry_after_ms: None,
        },
        "invalid_api_key" | "authentication_error" => GoFault::Unauthorized,
        "permission_denied" => GoFault::Forbidden,
        _ => GoFault::Protocol,
    }
}

// Bound nonintegral numeric precision before serde_json normalizes numbers.
// Otherwise adversarial 1e-999 could silently become a displayed zero cost.
fn parse_frame(raw: &str) -> Result<Value, GoFault> {
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            cursor += 1;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'\\' => cursor = cursor.saturating_add(2),
                    b'"' => {
                        cursor += 1;
                        break;
                    }
                    _ => cursor += 1,
                }
            }
        } else if bytes[cursor].is_ascii_digit() || bytes[cursor] == b'-' {
            let start = cursor;
            cursor += 1;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_digit() || b".eE+-".contains(&bytes[cursor]))
            {
                cursor += 1;
            }
            let token = &raw[start..cursor];
            if token.contains(['.', 'e', 'E']) {
                let (mantissa, exponent) = token.split_once(['e', 'E']).unwrap_or((token, "0"));
                let exponent = exponent.parse::<i32>().map_err(|_| GoFault::Protocol)?;
                let digits = mantissa
                    .bytes()
                    .filter(u8::is_ascii_digit)
                    .skip_while(|b| *b == b'0')
                    .count();
                let decimals = mantissa.split_once('.').map_or(0, |(_, s)| s.len());
                if exponent.unsigned_abs() > 18 || digits > 17 || decimals > 18 {
                    return Err(GoFault::Protocol);
                }
            }
        } else {
            cursor += 1;
        }
    }
    serde_json::from_str(raw).map_err(|_| GoFault::Protocol)
}

fn reported_usd(value: Option<&Value>) -> Result<Option<ReportedUsd>, GoFault> {
    let Some(value) = value else { return Ok(None) };
    let number = value.as_number().ok_or(GoFault::Protocol)?;
    let amount = number
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 1_000_000_000.0)
        .ok_or(GoFault::Protocol)?;
    // Preserve normalized decimal reporting, never convert to an exact local
    // budget charge or infer a remaining allowance from the amount.
    let text = if amount == 0.0 {
        "0".into()
    } else {
        number.to_string()
    };
    if text.len() > 48 {
        return Err(GoFault::Protocol);
    }
    Ok(Some(ReportedUsd(text)))
}
fn parse_usage(value: &Value, wire: WorkModelWire) -> Result<(WorkModelUsage, GoBilling), GoFault> {
    let (input, output, input_detail, output_detail) = match wire {
        WorkModelWire::ChatCompletions => (
            "prompt_tokens",
            "completion_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
        ),
        WorkModelWire::OpenAiResponses => (
            "input_tokens",
            "output_tokens",
            "input_tokens_details",
            "output_tokens_details",
        ),
        _ => return Err(GoFault::Protocol),
    };
    let required =
        |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).ok_or(GoFault::Protocol);
    let input_tokens = required(value, input)?;
    let output_tokens = required(value, output)?;
    let total = input_tokens
        .checked_add(output_tokens)
        .ok_or(GoFault::Protocol)?;
    if value
        .get("total_tokens")
        .is_some_and(|v| v.as_u64() != Some(total))
    {
        return Err(GoFault::Protocol);
    }
    for key in [input_detail, output_detail] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_object())
        {
            return Err(GoFault::Protocol);
        }
    }
    let detail = |root: &str, key: &str| -> Result<Option<u64>, GoFault> {
        match value.get(root).and_then(|v| v.get(key)) {
            Some(v) => v.as_u64().map(Some).ok_or(GoFault::Protocol),
            None => Ok(None),
        }
    };
    let cached = detail(input_detail, "cached_tokens")?.unwrap_or(0);
    let reasoning = detail(output_detail, "reasoning_tokens")?.unwrap_or(0);
    let opencode = value
        .get("opencode")
        .and_then(Value::as_object)
        .ok_or(GoFault::UsageUnavailable)?;
    let source = match opencode.get("billing_source").and_then(Value::as_str) {
        Some("free") => BillingSource::Free,
        Some("balance") => BillingSource::Balance,
        Some("go") => BillingSource::Go,
        Some("go-plus") => BillingSource::GoPlus,
        Some("byok") => BillingSource::Byok,
        None => return Err(GoFault::UsageUnavailable),
        _ => return Err(GoFault::Protocol),
    };
    let estimated = match opencode.get("estimated") {
        None => false,
        Some(v) => v.as_bool().ok_or(GoFault::Protocol)?,
    };
    if estimated && source != BillingSource::Byok {
        return Err(GoFault::Protocol);
    }
    let billed_tokens = if let Some(tokens) = opencode.get("tokens") {
        let tokens = GoBilledTokens {
            input: required(tokens, "input")?,
            output: required(tokens, "output")?,
            reasoning: required(tokens, "reasoning")?,
            cache_read: required(tokens, "cache_read")?,
            cache_write_5m: required(tokens, "cache_write_5m")?,
            cache_write_1h: required(tokens, "cache_write_1h")?,
        };
        if tokens.reasoning > tokens.output
            || tokens
                .input
                .checked_add(tokens.cache_read)
                .and_then(|n| n.checked_add(tokens.cache_write_5m))
                .and_then(|n| n.checked_add(tokens.cache_write_1h))
                .is_none()
        {
            return Err(GoFault::Protocol);
        }
        Some(tokens)
    } else {
        None
    };
    if cached > input_tokens || reasoning > output_tokens {
        return Err(GoFault::Protocol);
    }
    Ok((
        WorkModelUsage {
            input_tokens,
            output_tokens,
            cached_input_tokens: cached,
            cache_write_input_tokens: 0,
            reasoning_tokens: reasoning,
            cost_micros: None,
            ..WorkModelUsage::default()
        },
        GoBilling {
            source,
            cost_usd: reported_usd(value.get("cost"))?,
            list_cost_usd: reported_usd(opencode.get("list_cost"))?,
            allowance_consumed_usd: reported_usd(opencode.get("allowance"))?,
            estimated,
            tokens: billed_tokens,
        },
    ))
}

#[cfg(test)]
mod tests;
