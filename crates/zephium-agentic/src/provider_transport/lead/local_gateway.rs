//! Explicit, isolated loopback Chat Completions transport. This connects to a
//! user-controlled gateway, not to Google OAuth or a verified subscription.
//! No endpoint discovery, redirects, proxies, retries or credential-file reads.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use serde_json::{json, Value};
use zephium_core::work::model::*;

use super::sse::{SseEvent, SseFramer};
use super::{chat, LeadSecret, LeadTarget};

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

/// Closed failures: no gateway-authored error text or secret is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayFault {
    /// Only an explicit canonical loopback /v1 base is accepted.
    InvalidEndpoint,
    /// A supported text/function request could not be formed.
    InvalidRequest,
    /// The local HTTP client could not be initialized.
    Unavailable,
    /// The gateway refused its own access key.
    Unauthorized,
    /// The gateway refused access to this request or model.
    Forbidden,
    /// The gateway is limiting requests; the hint never causes an automatic retry.
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

impl GatewayFault {
    /// Content-free classification for native Work consumers.
    pub const fn model_fault(self) -> GatewayModelFault {
        match self {
            Self::InvalidEndpoint => GatewayModelFault::NotConfigured,
            Self::InvalidRequest | Self::TooLarge => GatewayModelFault::Unsupported,
            Self::Unavailable | Self::Timeout => GatewayModelFault::Unavailable,
            Self::Unauthorized => GatewayModelFault::Unauthorized,
            Self::Forbidden => GatewayModelFault::Forbidden,
            Self::RateLimited { .. } => GatewayModelFault::RateLimited,
            Self::Protocol => GatewayModelFault::Protocol,
            Self::Incomplete => GatewayModelFault::Incomplete,
            Self::UsageUnavailable => GatewayModelFault::UsageUnavailable,
        }
    }
}

/// What is known about a failed observation, not a claim about upstream billing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewayFailure {
    /// Content-free failure classification.
    pub fault: GatewayFault,
    /// A POST may have reached the gateway. GET catalog reads are always false.
    pub dispatched: bool,
    /// At least one text, reasoning, refusal or tool delta was received.
    pub output_started: bool,
}

impl GatewayFailure {
    /// Preserve source identity and uncertainty through the Work retry boundary.
    /// Gateway failures never become the generic retriable Network/RateLimited.
    pub fn into_model_error(self) -> WorkModelError {
        WorkModelError::Gateway {
            fault: self.fault.model_fault(),
            usage_unknown: self.dispatched || self.output_started,
        }
    }

    fn before(fault: GatewayFault) -> Self {
        Self {
            fault,
            dispatched: false,
            output_started: false,
        }
    }
    fn sent(fault: GatewayFault) -> Self {
        Self {
            fault,
            dispatched: true,
            output_started: false,
        }
    }
}

/// An exact loopback endpoint with no DNS, alternate host spelling or URL extras.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoopbackBase(String);

impl LoopbackBase {
    /// Accept `http://127.0.0.1:PORT/v1` or `http://[::1]:PORT/v1` only.
    /// A single trailing slash is normalized; the port must be explicit.
    pub fn parse(input: &str) -> Result<Self, GatewayFault> {
        if input.len() > 64 || input.trim() != input {
            return Err(GatewayFault::InvalidEndpoint);
        }
        let input = input.strip_suffix('/').unwrap_or(input);
        let authority = input
            .strip_prefix("http://")
            .and_then(|s| s.strip_suffix("/v1"))
            .ok_or(GatewayFault::InvalidEndpoint)?;
        let (host, port) = authority
            .strip_prefix("127.0.0.1:")
            .map(|port| ("127.0.0.1", port))
            .or_else(|| authority.strip_prefix("[::1]:").map(|port| ("[::1]", port)))
            .ok_or(GatewayFault::InvalidEndpoint)?;
        let parsed = port
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or(GatewayFault::InvalidEndpoint)?;
        if parsed.to_string() != port {
            return Err(GatewayFault::InvalidEndpoint);
        }
        Ok(Self(format!("http://{host}:{parsed}/v1")))
    }

    /// Normalized display/persistence value, never containing credentials.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn endpoint(&self, suffix: &str) -> String {
        format!("{}/{suffix}", self.0)
    }
}

/// A gateway-provided model label, not proof of usable upstream entitlement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayModel {
    /// Exact bounded model identifier to send in the JSON request body.
    pub id: String,
    /// Plain bounded display label, defaulting to the identifier.
    pub display_name: String,
}

/// A completed response with explicitly optional measured token usage.
/// The bridge must decide how to account for missing usage before committing tools.
/// No gateway-reported dollar amount is accepted as verified upstream billing.
pub struct GatewayOutcome {
    /// A complete normal/function turn, or an explicit content refusal.
    pub stop: WorkModelStop,
    /// Assistant history and proposed tools. Tools are NOT emitted as callbacks.
    pub assistant: Vec<WorkModelPart>,
    /// Validated token counters, absent when the gateway omitted final usage.
    pub usage: Option<WorkModelUsage>,
}

impl GatewayOutcome {
    /// Convert only when measured token usage is available. This commits no tool
    /// callbacks: the native caller must still check its revocable lease first.
    pub fn require_usage(self) -> Result<WorkModelOutcome, GatewayFailure> {
        let usage = self.usage.ok_or(GatewayFailure {
            fault: GatewayFault::UsageUnavailable,
            dispatched: true,
            output_started: !self.assistant.is_empty(),
        })?;
        Ok(WorkModelOutcome {
            stop: self.stop,
            assistant: self.assistant,
            usage,
        })
    }
}

impl std::fmt::Debug for GatewayOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayOutcome")
            .field("stop", &self.stop)
            .field("parts", &self.assistant.len())
            .field("usage_known", &self.usage.is_some())
            .finish()
    }
}

/// Immutable endpoint/key binding. The native owner supplies a revocable lease,
/// serializes configuration changes, and cancels by dropping the call future.
#[derive(Clone)]
pub struct GatewayClient {
    base: LoopbackBase,
    connection_scope: String,
    target: LeadTarget,
    key: Arc<LeadSecret>,
    http: reqwest::Client,
}

impl GatewayClient {
    /// Construct without network or credential-store access.
    pub fn new(
        base: LoopbackBase,
        key: Arc<LeadSecret>,
        connection_scope: String,
    ) -> Result<Self, GatewayFault> {
        if connection_scope.len() != 32
            || !connection_scope
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(GatewayFault::InvalidRequest);
        }
        // Keys belong to this gateway, not to a provider. Validate bounded
        // header representability before retaining the client binding.
        if key
            .header("Bearer ")
            .map_err(|_| GatewayFault::InvalidRequest)?
            .as_bytes()
            .len()
            > 1031
        {
            return Err(GatewayFault::InvalidRequest);
        }
        let target =
            LeadTarget::compatible(base.as_str()).map_err(|_| GatewayFault::InvalidEndpoint)?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(READ_TIMEOUT)
            .timeout(CALL_TIMEOUT)
            .user_agent(concat!(
                "Zephium/",
                env!("CARGO_PKG_VERSION"),
                " (local-gateway)"
            ))
            .build()
            .map_err(|_| GatewayFault::Unavailable)?;
        Ok(Self {
            base,
            connection_scope,
            target,
            key,
            http,
        })
    }

    /// Explicit model-list observation only; never an inference or entitlement test.
    pub async fn models(&self) -> Result<Vec<GatewayModel>, GatewayFailure> {
        let header = self
            .key
            .header("Bearer ")
            .map_err(|_| GatewayFailure::before(GatewayFault::InvalidRequest))?;
        let mut response = self
            .http
            .get(self.base.endpoint("models"))
            .header(AUTHORIZATION, header)
            .header(ACCEPT, "application/json")
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .map_err(|e| GatewayFailure::before(transport_fault(&e)))?;
        if response.status().as_u16() != 200 {
            return Err(GatewayFailure::before(http_fault(
                response.status().as_u16(),
                response.headers().get(RETRY_AFTER),
            )));
        }
        if !media_is(&response, "application/json") {
            return Err(GatewayFailure::before(GatewayFault::Protocol));
        }
        let bytes = bounded_body(&mut response, MAX_CATALOG_BYTES)
            .await
            .map_err(GatewayFailure::before)?;
        parse_models(&bytes).map_err(GatewayFailure::before)
    }

    /// One user-authorized inference; never retries or changes endpoint/key/model.
    /// Only text progress is emitted. The native bridge commits proposed tools
    /// from the outcome after checking its lease and accounting requirements.
    pub async fn call(
        &self,
        request: WorkModelRequest,
        events: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<GatewayOutcome, GatewayFailure> {
        let body = request_body(
            &request,
            &self.target,
            self.base.as_str(),
            &self.connection_scope,
        )
        .map_err(GatewayFailure::before)?;
        let header = self
            .key
            .header("Bearer ")
            .map_err(|_| GatewayFailure::before(GatewayFault::InvalidRequest))?;
        let mut decoder = GatewayStream::new(&request, self.base.as_str(), &self.connection_scope);
        let future = async {
            let response = self
                .http
                .post(self.base.endpoint("chat/completions"))
                .header(AUTHORIZATION, header)
                .header(ACCEPT, "text/event-stream")
                .header(CONTENT_TYPE, "application/json")
                .header("accept-encoding", "identity")
                .body(body)
                .send()
                .await;
            let mut response = response.map_err(|e| GatewayFailure {
                fault: transport_fault(&e),
                dispatched: !e.is_connect(),
                output_started: false,
            })?;
            if response.status().as_u16() != 200 {
                return Err(GatewayFailure::sent(http_fault(
                    response.status().as_u16(),
                    response.headers().get(RETRY_AFTER),
                )));
            }
            if !media_is(&response, "text/event-stream") {
                return Err(GatewayFailure::sent(GatewayFault::Protocol));
            }
            loop {
                let chunk = tokio::time::timeout(READ_TIMEOUT, response.chunk())
                    .await
                    .map_err(|_| decoder.failure(GatewayFault::Timeout))?
                    .map_err(|e| decoder.failure(transport_fault(&e)))?;
                match chunk {
                    Some(chunk) => {
                        if let Some(outcome) = decoder.push(&chunk, events)? {
                            return Ok(outcome);
                        }
                    }
                    // A partial final event without its SSE delimiter is not a
                    // complete wire terminal, even if it resembles [DONE].
                    None => return Err(decoder.failure(GatewayFault::Incomplete)),
                }
            }
        };
        match tokio::time::timeout(CALL_TIMEOUT, future).await {
            Ok(result) => result,
            Err(_) => Err(decoder.failure(GatewayFault::Timeout)),
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
fn transport_fault(error: &reqwest::Error) -> GatewayFault {
    if error.is_timeout() {
        GatewayFault::Timeout
    } else {
        GatewayFault::Unavailable
    }
}
fn http_fault(status: u16, retry: Option<&reqwest::header::HeaderValue>) -> GatewayFault {
    match status {
        401 => GatewayFault::Unauthorized,
        403 => GatewayFault::Forbidden,
        429 => GatewayFault::RateLimited {
            retry_after_ms: retry
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .and_then(|s| s.checked_mul(1000))
                .filter(|ms| *ms <= 86_400_000),
        },
        408 | 504 => GatewayFault::Timeout,
        502 | 503 => GatewayFault::Unavailable,
        400 | 404 | 405 | 422 => GatewayFault::InvalidRequest,
        413 => GatewayFault::TooLarge,
        _ => GatewayFault::Protocol,
    }
}
async fn bounded_body(
    response: &mut reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, GatewayFault> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(GatewayFault::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| transport_fault(&e))? {
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err(GatewayFault::TooLarge);
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
fn parse_models(bytes: &[u8]) -> Result<Vec<GatewayModel>, GatewayFault> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(GatewayFault::TooLarge);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| GatewayFault::Protocol)?;
    if value
        .get("object")
        .is_some_and(|v| v.as_str() != Some("list"))
    {
        return Err(GatewayFault::Protocol);
    }
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or(GatewayFault::Protocol)?;
    if rows.len() > MAX_MODELS {
        return Err(GatewayFault::TooLarge);
    }
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| model_id(id))
            .ok_or(GatewayFault::Protocol)?;
        if !seen.insert(id) {
            return Err(GatewayFault::Protocol);
        }
        let name = match row.get("display_name") {
            Some(value) => value
                .as_str()
                .filter(|n| bounded_label(n, 160))
                .ok_or(GatewayFault::Protocol)?,
            None => id,
        };
        models.push(GatewayModel {
            id: id.into(),
            display_name: name.into(),
        });
    }
    Ok(models)
}

fn request_body(
    request: &WorkModelRequest,
    target: &LeadTarget,
    origin: &str,
    connection_scope: &str,
) -> Result<Vec<u8>, GatewayFault> {
    if request.model.provider != WorkModelProvider::Compatible
        || request.model.wire != WorkModelWire::ChatCompletions
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
        return Err(GatewayFault::InvalidRequest);
    }
    let mut bytes = 0usize;
    let mut charge = |value: &str| -> Result<(), GatewayFault> {
        bytes = bytes.saturating_add(value.len());
        if bytes > MAX_REQUEST_BYTES {
            Err(GatewayFault::TooLarge)
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
            return Err(GatewayFault::InvalidRequest);
        }
        let encoded =
            serde_json::to_string(&tool.schema).map_err(|_| GatewayFault::InvalidRequest)?;
        if encoded.len() > MAX_ARGUMENT_BYTES {
            return Err(GatewayFault::TooLarge);
        }
        charge(&encoded)?;
        charge(&tool.description)?;
    }
    if match &request.tool_choice {
        WorkModelToolChoice::Auto => false,
        WorkModelToolChoice::Required => request.tools.is_empty(),
        WorkModelToolChoice::Named(name) => !names.contains(name),
    } {
        return Err(GatewayFault::InvalidRequest);
    }
    for block in &request.system {
        charge(&block.text)?;
    }
    for message in &request.messages {
        let is_user = matches!(message, WorkModelMessage::User(_));
        match message {
            WorkModelMessage::User(parts) | WorkModelMessage::Assistant(parts) => {
                if parts.len() > 256 {
                    return Err(GatewayFault::TooLarge);
                }
                if !is_user
                    && !parts.iter().any(|part| {
                        matches!(part,
                    WorkModelPart::Text(text) if !text.is_empty())
                            || matches!(part, WorkModelPart::ToolCall(_))
                    })
                {
                    return Err(GatewayFault::InvalidRequest);
                }
                for part in parts {
                    if is_user && !matches!(part, WorkModelPart::Text(_)) {
                        return Err(GatewayFault::InvalidRequest);
                    }
                    match part {
                        WorkModelPart::Text(text) => charge(text)?,
                        WorkModelPart::ToolCall(call) => {
                            if !bounded_label(&call.id, 256)
                                || !tool_name(&call.name)
                                || !call.arguments.is_object()
                            {
                                return Err(GatewayFault::InvalidRequest);
                            }
                            let args = serde_json::to_string(&call.arguments)
                                .map_err(|_| GatewayFault::InvalidRequest)?;
                            if args.len() > MAX_ARGUMENT_BYTES {
                                return Err(GatewayFault::TooLarge);
                            }
                            charge(&args)?;
                        }
                        WorkModelPart::Replay(value) => {
                            if value.as_object().is_none_or(|fields| {
                                fields.len() != 5
                                    || fields.keys().any(|key| {
                                        !matches!(
                                            key.as_str(),
                                            "wire"
                                                | "origin"
                                                | "connection_scope"
                                                | "model"
                                                | "reasoning_content"
                                        )
                                    })
                            }) {
                                return Err(GatewayFault::InvalidRequest);
                            }
                            if value.get("wire").and_then(Value::as_str) != Some("local_gateway")
                                || value.get("origin").and_then(Value::as_str) != Some(origin)
                                || value.get("connection_scope").and_then(Value::as_str)
                                    != Some(connection_scope)
                                || value.get("model").and_then(Value::as_str)
                                    != Some(request.model.model.as_str())
                            {
                                return Err(GatewayFault::InvalidRequest);
                            }
                            let reasoning = value
                                .get("reasoning_content")
                                .and_then(Value::as_str)
                                .filter(|s| s.len() <= MAX_OUTPUT_BYTES)
                                .ok_or(GatewayFault::InvalidRequest)?;
                            charge(reasoning)?;
                        }
                        WorkModelPart::Image { .. } => return Err(GatewayFault::InvalidRequest),
                    }
                }
            }
            WorkModelMessage::ToolResults(results) => {
                if results.len() > MAX_TOOLS {
                    return Err(GatewayFault::TooLarge);
                }
                for result in results {
                    if !bounded_label(&result.call, 256) {
                        return Err(GatewayFault::InvalidRequest);
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
                    *value = json!({"wire":"chat","value":{"reasoning_content":value["reasoning_content"]}});
                }
            }
        }
    }
    let body = chat::body(&translated, target).map_err(|_| GatewayFault::InvalidRequest)?;
    let body = serde_json::to_vec(&body).map_err(|_| GatewayFault::InvalidRequest)?;
    if body.len() > MAX_REQUEST_BYTES {
        return Err(GatewayFault::TooLarge);
    }
    Ok(body)
}

#[derive(Default)]
struct ToolDelta {
    id: Option<String>,
    name: String,
    arguments: String,
}
struct GatewayStream {
    framer: SseFramer,
    bytes: usize,
    events: usize,
    started: bool,
    text: String,
    reasoning: String,
    refused: bool,
    finish: Option<String>,
    usage: Option<WorkModelUsage>,
    calls: BTreeMap<usize, ToolDelta>,
    allowed: HashSet<String>,
    requires_tool: bool,
    origin: String,
    connection_scope: String,
    model: String,
    response_id: Option<String>,
}
impl GatewayStream {
    fn new(request: &WorkModelRequest, origin: &str, connection_scope: &str) -> Self {
        Self {
            framer: SseFramer::default(),
            bytes: 0,
            events: 0,
            started: false,
            text: String::new(),
            reasoning: String::new(),
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
            origin: origin.into(),
            connection_scope: connection_scope.into(),
            model: request.model.model.clone(),
            response_id: None,
        }
    }
    fn failure(&self, fault: GatewayFault) -> GatewayFailure {
        GatewayFailure {
            fault,
            dispatched: true,
            output_started: self.started,
        }
    }
    fn push(
        &mut self,
        chunk: &[u8],
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<Option<GatewayOutcome>, GatewayFailure> {
        self.bytes = self.bytes.saturating_add(chunk.len());
        if self.bytes > MAX_STREAM_BYTES {
            return Err(self.failure(GatewayFault::TooLarge));
        }
        let mut frames = Vec::new();
        self.framer
            .push(chunk, &mut frames)
            .map_err(|_| self.failure(GatewayFault::Protocol))?;
        let mut result = None;
        for frame in frames {
            if result.is_some() {
                return Err(self.failure(GatewayFault::Protocol));
            }
            self.events += 1;
            if self.events > MAX_EVENTS {
                return Err(self.failure(GatewayFault::TooLarge));
            }
            result = self.event(frame, sink).map_err(|f| self.failure(f))?;
        }
        Ok(result)
    }
    fn event(
        &mut self,
        frame: SseEvent,
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<Option<GatewayOutcome>, GatewayFault> {
        if frame.data == "[DONE]" {
            return self.complete().map(Some);
        }
        let value: Value = serde_json::from_str(&frame.data).map_err(|_| GatewayFault::Protocol)?;
        if let Some(error) = value.get("error") {
            let code = error
                .get("code")
                .or_else(|| error.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            return Err(match code {
                "rate_limit_exceeded" | "insufficient_quota" => GatewayFault::RateLimited {
                    retry_after_ms: None,
                },
                "invalid_api_key" | "authentication_error" => GatewayFault::Unauthorized,
                "permission_denied" => GatewayFault::Forbidden,
                _ => GatewayFault::Protocol,
            });
        }
        if !value.is_object()
            || value
                .get("object")
                .is_some_and(|v| v.as_str() != Some("chat.completion.chunk"))
        {
            return Err(GatewayFault::Protocol);
        }
        if let Some(id) = value.get("id") {
            let id = id
                .as_str()
                .filter(|s| bounded_label(s, 256))
                .ok_or(GatewayFault::Protocol)?;
            if self.response_id.as_ref().is_some_and(|old| old != id) {
                return Err(GatewayFault::Protocol);
            }
            self.response_id = Some(id.into());
        }
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .ok_or(GatewayFault::Protocol)?;
        if choices.len() > 1 {
            return Err(GatewayFault::Protocol);
        }
        if let Some(choice) = choices.first() {
            if self.finish.is_some() || choice.get("index").and_then(Value::as_u64) != Some(0) {
                return Err(GatewayFault::Protocol);
            }
            let delta = choice
                .get("delta")
                .and_then(Value::as_object)
                .ok_or(GatewayFault::Protocol)?;
            if let Some(role) = delta.get("role") {
                if role.as_str() != Some("assistant") {
                    return Err(GatewayFault::Protocol);
                }
            }
            if let Some(content) = delta.get("content").filter(|v| !v.is_null()) {
                let text = content.as_str().ok_or(GatewayFault::Protocol)?;
                if self.text.len().saturating_add(text.len()) > MAX_OUTPUT_BYTES {
                    return Err(GatewayFault::TooLarge);
                }
                if !text.is_empty() {
                    self.started = true;
                    self.text.push_str(text);
                    sink(WorkModelEvent::Text(text.into()));
                }
            }
            if let Some(reasoning) = delta.get("reasoning_content").filter(|v| !v.is_null()) {
                let reasoning = reasoning.as_str().ok_or(GatewayFault::Protocol)?;
                if self.reasoning.len().saturating_add(reasoning.len()) > MAX_OUTPUT_BYTES {
                    return Err(GatewayFault::TooLarge);
                }
                self.started |= !reasoning.is_empty();
                self.reasoning.push_str(reasoning);
            }
            if let Some(refusal) = delta.get("refusal").filter(|v| !v.is_null()) {
                let refusal = refusal
                    .as_str()
                    .filter(|v| v.len() <= MAX_OUTPUT_BYTES)
                    .ok_or(GatewayFault::Protocol)?;
                self.refused |= !refusal.is_empty();
                self.started |= !refusal.is_empty();
            }
            if let Some(calls) = delta.get("tool_calls") {
                let calls = calls
                    .as_array()
                    .filter(|v| v.len() <= MAX_TOOLS)
                    .ok_or(GatewayFault::Protocol)?;
                for call in calls {
                    self.started = true;
                    let index = call
                        .get("index")
                        .and_then(Value::as_u64)
                        .filter(|i| *i < MAX_TOOLS as u64)
                        .ok_or(GatewayFault::Protocol)? as usize;
                    if call
                        .get("type")
                        .is_some_and(|v| v.as_str() != Some("function"))
                    {
                        return Err(GatewayFault::Protocol);
                    }
                    let pending = self.calls.entry(index).or_default();
                    if let Some(id) = call.get("id") {
                        let id = id
                            .as_str()
                            .filter(|s| bounded_label(s, 256))
                            .ok_or(GatewayFault::Protocol)?;
                        if pending.id.as_ref().is_some_and(|old| old != id) {
                            return Err(GatewayFault::Protocol);
                        }
                        pending.id = Some(id.into());
                    }
                    let function = call
                        .get("function")
                        .and_then(Value::as_object)
                        .ok_or(GatewayFault::Protocol)?;
                    for (key, target, max) in [
                        ("name", &mut pending.name, 64),
                        ("arguments", &mut pending.arguments, MAX_ARGUMENT_BYTES),
                    ] {
                        if let Some(value) = function.get(key) {
                            let value = value.as_str().ok_or(GatewayFault::Protocol)?;
                            if target.len().saturating_add(value.len()) > max {
                                return Err(GatewayFault::TooLarge);
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
                    .ok_or(GatewayFault::Protocol)?;
                self.finish = Some(finish.into());
            }
        }
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            if self.finish.is_none() || self.usage.is_some() {
                return Err(GatewayFault::Protocol);
            }
            self.usage = Some(parse_usage(usage)?);
        }
        Ok(None)
    }
    fn complete(&mut self) -> Result<GatewayOutcome, GatewayFault> {
        let finish = self.finish.as_deref().ok_or(GatewayFault::Incomplete)?;
        if finish == "length" {
            return Err(GatewayFault::Incomplete);
        }
        let refused = self.refused || finish == "content_filter";
        if (!refused && self.text.is_empty() && self.calls.is_empty())
            || (!self.calls.is_empty() && (finish != "tool_calls" || refused))
            || (finish == "tool_calls" && self.calls.is_empty())
            || (self.requires_tool && self.calls.is_empty() && !refused)
        {
            return Err(GatewayFault::Protocol);
        }
        let mut assistant = Vec::new();
        let mut ids = HashSet::new();
        if !self.reasoning.is_empty() {
            assistant.push(WorkModelPart::Replay(json!({"wire":"local_gateway","origin":self.origin,"connection_scope":self.connection_scope,"model":self.model,"reasoning_content":self.reasoning})));
        }
        if !self.text.is_empty() {
            assistant.push(WorkModelPart::Text(self.text.clone()));
        }
        for (expected, (index, pending)) in self.calls.iter().enumerate() {
            if expected != *index
                || !tool_name(&pending.name)
                || !self.allowed.contains(&pending.name)
            {
                return Err(GatewayFault::Protocol);
            }
            let id = pending.id.as_ref().ok_or(GatewayFault::Protocol)?;
            if !ids.insert(id) {
                return Err(GatewayFault::Protocol);
            }
            let arguments: Value =
                serde_json::from_str(&pending.arguments).map_err(|_| GatewayFault::Protocol)?;
            if !arguments.is_object() {
                return Err(GatewayFault::Protocol);
            }
            assistant.push(WorkModelPart::ToolCall(WorkModelToolCall {
                id: id.clone(),
                name: pending.name.clone(),
                arguments,
            }));
        }
        Ok(GatewayOutcome {
            stop: if refused {
                WorkModelStop::Refused
            } else if self.calls.is_empty() {
                WorkModelStop::EndTurn
            } else {
                WorkModelStop::ToolUse
            },
            assistant,
            usage: self.usage,
        })
    }
}
fn parse_usage(value: &Value) -> Result<WorkModelUsage, GatewayFault> {
    let input_tokens = value
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .ok_or(GatewayFault::Protocol)?;
    let output_tokens = value
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .ok_or(GatewayFault::Protocol)?;
    let total = input_tokens
        .checked_add(output_tokens)
        .ok_or(GatewayFault::Protocol)?;
    for field in ["prompt_tokens_details", "completion_tokens_details"] {
        if value
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.is_object())
        {
            return Err(GatewayFault::Protocol);
        }
    }
    let detail = |path: &str| -> Result<u64, GatewayFault> {
        match value.pointer(path) {
            Some(v) => v.as_u64().ok_or(GatewayFault::Protocol),
            None => Ok(0),
        }
    };
    let cached_input_tokens = detail("/prompt_tokens_details/cached_tokens")?;
    let reasoning_tokens = detail("/completion_tokens_details/reasoning_tokens")?;
    if cached_input_tokens > input_tokens
        || reasoning_tokens > output_tokens
        || value
            .get("total_tokens")
            .is_some_and(|v| v.as_u64() != Some(total))
    {
        return Err(GatewayFault::Protocol);
    }
    Ok(WorkModelUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        reasoning_tokens,
        ..WorkModelUsage::default()
    })
}

#[cfg(test)]
mod tests;
