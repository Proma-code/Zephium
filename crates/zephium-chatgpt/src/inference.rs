//! Direct, bounded Responses inference using Zephium's own ChatGPT OAuth grant.
//! No private ChatGPT endpoint, remote proxy, credential import or retry loop.

use std::collections::HashSet;
use std::time::Duration;

use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use zephium_core::work::model::*;
use zeroize::Zeroizing;

use crate::token::Tokens;

const MODELS: &str = "https://api.openai.com/v1/models";
const RESPONSES: &str = "https://api.openai.com/v1/responses";
const NAMESPACE: &str = "zephium";
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAM_BYTES: usize = 32 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_MODELS: usize = 128;
const MAX_TOOLS: usize = 128;
const MAX_EVENTS: usize = 100_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(180);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// An account-specific display model, not evidence of remaining quota.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Model {
    /// The exact server model identifier to send with inference.
    pub slug: String,
    /// Plain, bounded provider display name; not an instruction.
    pub display_name: String,
}

impl Model {
    /// A distinct explicit-choice model. Limits are conservative local adapter
    /// budgets, not claims about a vendor's context size or output entitlement.
    pub fn entry(&self) -> WorkModelEntry {
        WorkModelEntry {
            id: format!("chatgpt/{}", self.slug),
            model: WorkModelRef {
                provider: WorkModelProvider::OpenAi,
                wire: WorkModelWire::OpenAiResponses,
                model: self.slug.clone(),
            },
            display_name: self.display_name.clone(),
            roles: vec![
                WorkModelRole::Lead,
                WorkModelRole::Page,
                WorkModelRole::Light,
            ],
            recommended: false,
            context_window: 32_768,
            max_output: 8_192,
            supports: WorkModelSupports {
                tools: true,
                vision: false,
                prompt_cache: false,
                reasoning: supports_effort(&self.slug),
                native_search: false,
            },
            price: None,
        }
    }
}

// Exact account-listed slugs intersected with the official model contracts.
// Do not infer capabilities from a provider-controlled prefix or display name.
// These are the existing Work slider's supported levels (low through max);
// `None` in Work is the provider default, not the vendor's "none" effort.
// https://developers.openai.com/api/docs/models/gpt-6.1-sol
// https://developers.openai.com/api/docs/models/gpt-6-luna
// https://developers.openai.com/api/docs/guides/deployment-checklist
// https://developers.openai.com/api/docs/guides/latest-model
fn supports_effort(slug: &str) -> bool {
    matches!(
        slug,
        "gpt-6.1-sol" | "gpt-6-luna" | "gpt-6-sol" | "gpt-6-astra" | "gpt-5.6-terra"
    )
}

/// Fixed-origin client. The caller owns token refresh, profile selection and
/// revocation; dropping an outstanding future cancels its network observation.
#[derive(Clone)]
pub struct InferenceClient {
    http: reqwest::Client,
}

impl InferenceClient {
    /// Construct without making requests or accessing an account.
    pub fn new() -> Result<Self, WorkModelError> {
        let http = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(CALL_TIMEOUT)
            .user_agent(concat!("Zephium/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| WorkModelError::Network)?;
        Ok(Self { http })
    }

    /// List this grant's currently displayed models. This is not a paid inference
    /// test and must not be interpreted as proof of usable subscription quota.
    pub async fn models(&self, tokens: &Tokens) -> Result<Vec<Model>, WorkModelError> {
        let mut response = self
            .http
            .get(MODELS)
            .header(AUTHORIZATION, bearer(tokens)?)
            .header(ACCEPT, "application/json")
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| WorkModelError::Network)?;
        let status = response.status().as_u16();
        let body = bounded_body(&mut response, MAX_CATALOG_BYTES, false).await?;
        if status != 200 {
            return Err(http_error(status, &body));
        }
        parse_models(&body)
    }

    /// One preview-compatible Responses call. No request is automatically retried.
    /// Only a completed terminal response can return a successful outcome.
    pub async fn call(
        &self,
        tokens: &Tokens,
        request: WorkModelRequest,
        events: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<WorkModelOutcome, WorkModelError> {
        let body = request_body(&request)?;
        let encoded = serde_json::to_vec(&body).map_err(|_| WorkModelError::BadRequest)?;
        if encoded.len() > MAX_REQUEST_BYTES {
            return Err(WorkModelError::BadRequest);
        }
        let future = async {
            let mut response = self
                .http
                .post(RESPONSES)
                .header(AUTHORIZATION, bearer(tokens)?)
                .header(ACCEPT, "text/event-stream")
                .header(CONTENT_TYPE, "application/json")
                .body(encoded)
                .send()
                .await
                // A lost response may have been billed. The outer Work runtime
                // treats Protocol as unknown outcome rather than replaying it.
                .map_err(|_| WorkModelError::Protocol)?;
            let status = response.status().as_u16();
            if status != 200 {
                let body = bounded_body(&mut response, MAX_CATALOG_BYTES, true).await?;
                return Err(http_error(status, &body));
            }
            if !response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|header| header.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(';')
                        .next()
                        .is_some_and(|media| media.trim().eq_ignore_ascii_case("text/event-stream"))
                })
            {
                return Err(WorkModelError::Protocol);
            }
            let mut decoder = StreamDecoder::new(&request);
            loop {
                let chunk = tokio::time::timeout(IDLE_TIMEOUT, response.chunk())
                    .await
                    .map_err(|_| WorkModelError::Protocol)?
                    .map_err(|_| WorkModelError::Protocol)?;
                match chunk {
                    Some(chunk) => {
                        if let Some(outcome) = decoder.push(&chunk, events)? {
                            return Ok(outcome);
                        }
                    }
                    None => return decoder.finish(events),
                }
            }
        };
        tokio::time::timeout(CALL_TIMEOUT, future)
            .await
            .map_err(|_| WorkModelError::Protocol)?
    }
}

fn bearer(tokens: &Tokens) -> Result<HeaderValue, WorkModelError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| WorkModelError::Protocol)?
        .as_secs();
    if tokens.expires_at() <= now
        || !tokens
            .scopes()
            .iter()
            .any(|scope| scope == "chatgpt.tokens.use.direct")
    {
        return Err(chatgpt_error(ChatGptModelFault::SignedOut));
    }
    let encoded = Zeroizing::new(format!("Bearer {}", tokens.access_token()));
    let mut value =
        HeaderValue::from_str(&encoded).map_err(|_| chatgpt_error(ChatGptModelFault::SignedOut))?;
    value.set_sensitive(true);
    Ok(value)
}

async fn bounded_body(
    response: &mut reqwest::Response,
    max: usize,
    sent_inference: bool,
) -> Result<Vec<u8>, WorkModelError> {
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(WorkModelError::Protocol);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        if sent_inference {
            WorkModelError::Protocol
        } else {
            WorkModelError::Network
        }
    })? {
        if body.len().saturating_add(chunk.len()) > max {
            return Err(WorkModelError::Protocol);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn bounded_word(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn model_slug(value: &str) -> bool {
    bounded_word(value, 128)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn tool_name(value: &str) -> bool {
    bounded_word(value, 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn parse_models(bytes: &[u8]) -> Result<Vec<Model>, WorkModelError> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(WorkModelError::Protocol);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| WorkModelError::Protocol)?;
    let rows = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or(WorkModelError::Protocol)?;
    if rows.len() > MAX_MODELS {
        return Err(WorkModelError::Protocol);
    }
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        if row.get("visibility").and_then(Value::as_str) != Some("list") {
            continue;
        }
        let slug = row
            .get("slug")
            .and_then(Value::as_str)
            .ok_or(WorkModelError::Protocol)?;
        let display_name = row
            .get("display_name")
            .and_then(Value::as_str)
            .ok_or(WorkModelError::Protocol)?;
        if !model_slug(slug) || !bounded_word(display_name, 160) || !seen.insert(slug) {
            return Err(WorkModelError::Protocol);
        }
        models.push(Model {
            slug: slug.to_owned(),
            display_name: display_name.to_owned(),
        });
    }
    Ok(models)
}

fn request_body(request: &WorkModelRequest) -> Result<Value, WorkModelError> {
    if request.model.provider != WorkModelProvider::OpenAi
        || request.model.wire != WorkModelWire::OpenAiResponses
        || !model_slug(&request.model.model)
        || request.messages.len() > 512
        || request.tools.len() > MAX_TOOLS
        || request.system.len() > 64
        || request.native_search
        || (request.reasoning.is_some() && !supports_effort(&request.model.model))
    {
        return Err(WorkModelError::BadRequest);
    }
    let mut input = Vec::new();
    let mut budget = 0usize;
    let mut charge = |text: &str| -> Result<(), WorkModelError> {
        budget = budget.saturating_add(text.len());
        if budget > MAX_REQUEST_BYTES {
            Err(WorkModelError::BadRequest)
        } else {
            Ok(())
        }
    };
    for message in &request.messages {
        match message {
            WorkModelMessage::User(parts) => {
                if parts.len() > 256 {
                    return Err(WorkModelError::BadRequest);
                }
                let mut content = Vec::new();
                for part in parts {
                    match part {
                        WorkModelPart::Text(text) => {
                            charge(text)?;
                            content.push(json!({"type": "input_text", "text": text}));
                        }
                        _ => return Err(WorkModelError::BadRequest),
                    }
                }
                if !content.is_empty() {
                    input.push(json!({"type": "message", "role": "user", "content": content}));
                }
            }
            WorkModelMessage::Assistant(parts) => {
                if parts.len() > 256 {
                    return Err(WorkModelError::BadRequest);
                }
                for part in parts {
                    match part {
                        WorkModelPart::Text(text) => {
                            charge(text)?;
                            input.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text, "annotations": []}]}));
                        }
                        WorkModelPart::ToolCall(call) => {
                            if !bounded_word(&call.id, 256) || !tool_name(&call.name) {
                                return Err(WorkModelError::BadRequest);
                            }
                            let arguments = serde_json::to_string(&call.arguments)
                                .map_err(|_| WorkModelError::BadRequest)?;
                            charge(&arguments)?;
                            if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                                return Err(WorkModelError::BadRequest);
                            }
                            input.push(json!({"type":"function_call", "call_id":call.id, "name":call.name, "namespace":NAMESPACE, "arguments":arguments}));
                        }
                        WorkModelPart::Replay(value) => {
                            if value.get("wire").and_then(Value::as_str) != Some("chatgpt") {
                                return Err(WorkModelError::BadRequest);
                            }
                            let item = value.get("value").ok_or(WorkModelError::BadRequest)?;
                            let encoded = serde_json::to_string(item)
                                .map_err(|_| WorkModelError::BadRequest)?;
                            charge(&encoded)?;
                            input.push(
                                reasoning_item(item).map_err(|_| WorkModelError::BadRequest)?,
                            );
                        }
                        WorkModelPart::Image { .. } => return Err(WorkModelError::BadRequest),
                    }
                }
            }
            WorkModelMessage::ToolResults(results) => {
                if results.len() > MAX_TOOLS {
                    return Err(WorkModelError::BadRequest);
                }
                for result in results {
                    if !bounded_word(&result.call, 256) {
                        return Err(WorkModelError::BadRequest);
                    }
                    charge(&result.content)?;
                    let output = if result.is_error {
                        format!("Error: {}", result.content)
                    } else {
                        result.content.clone()
                    };
                    input.push(json!({"type":"function_call_output", "call_id":result.call, "output":output}));
                }
            }
        }
        if input.len() > 4096 {
            return Err(WorkModelError::BadRequest);
        }
    }
    let mut instructions = String::new();
    for block in &request.system {
        charge(&block.text)?;
        if !instructions.is_empty() {
            instructions.push_str("\n\n");
        }
        instructions.push_str(&block.text);
    }
    let mut names = HashSet::new();
    let mut tools = Vec::new();
    for tool in &request.tools {
        if !tool_name(&tool.name)
            || !names.insert(tool.name.as_str())
            || tool.description.len() > 4096
            || !tool.schema.is_object()
        {
            return Err(WorkModelError::BadRequest);
        }
        let schema = serde_json::to_string(&tool.schema).map_err(|_| WorkModelError::BadRequest)?;
        if schema.len() > MAX_TOOL_ARGUMENT_BYTES {
            return Err(WorkModelError::BadRequest);
        }
        charge(&schema)?;
        charge(&tool.description)?;
        tools.push(json!({"type":"function", "name":tool.name, "description":tool.description, "parameters":tool.schema, "strict":tool.strict}));
    }
    if let WorkModelToolChoice::Named(name) = &request.tool_choice {
        // Namespace-qualified named selectors are not documented for this
        // preview. Requiring the sole offered function has the same authority.
        tools.retain(|tool| tool.get("name").and_then(Value::as_str) == Some(name));
        if tools.is_empty() {
            return Err(chatgpt_error(ChatGptModelFault::Unsupported));
        }
    }
    let mut body = json!({"model":request.model.model,"instructions":instructions,"input":input,"store":false,"stream":true,"include":["reasoning.encrypted_content"]});
    if let Some(effort) = request.reasoning {
        body["reasoning"] = json!({"effort": effort});
    }
    if !tools.is_empty() {
        body["tools"] = json!([{"type":"namespace", "name":NAMESPACE, "description":"Tools explicitly offered by the local Zephium runtime.", "tools":tools}]);
        body["parallel_tool_calls"] = json!(request.parallel_tools);
        body["tool_choice"] = match &request.tool_choice {
            WorkModelToolChoice::Auto => json!("auto"),
            WorkModelToolChoice::Required | WorkModelToolChoice::Named(_) => json!("required"),
        };
    } else if request.tool_choice != WorkModelToolChoice::Auto {
        return Err(WorkModelError::BadRequest);
    }
    Ok(body)
}

fn reasoning_item(item: &Value) -> Result<Value, WorkModelError> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return Err(WorkModelError::Protocol);
    }
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| bounded_word(id, 256))
        .ok_or(WorkModelError::Protocol)?;
    let encrypted = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .filter(|text| bounded_word(text, MAX_EVENT_BYTES / 2))
        .ok_or(WorkModelError::Protocol)?;
    Ok(json!({"type":"reasoning","id":id,"summary":[],"encrypted_content":encrypted}))
}

fn chatgpt_error(fault: ChatGptModelFault) -> WorkModelError {
    WorkModelError::ChatGpt { fault }
}

fn provider_error(code: &str) -> Option<WorkModelError> {
    match code {
        "subscription_sharing_usage_limit_exceeded" | "insufficient_quota" => {
            Some(chatgpt_error(ChatGptModelFault::UsageLimit))
        }
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            Some(chatgpt_error(ChatGptModelFault::Unavailable))
        }
        "subscription_sharing_user_not_eligible"
        | "chatpass_v2_scope_not_authorized"
        | "chatpass_v2_invalid_authorization_context" => {
            Some(chatgpt_error(ChatGptModelFault::NotEligible))
        }
        "subscription_sharing_unsupported_capability"
        | "subscription_sharing_route_not_supported" => {
            Some(chatgpt_error(ChatGptModelFault::Unsupported))
        }
        "subscription_sharing_invalid_user"
        | "invalid_api_key"
        | "invalid_token"
        | "token_expired"
        | "authentication_error" => Some(chatgpt_error(ChatGptModelFault::SignedOut)),
        "context_length_exceeded" => Some(WorkModelError::ContextTooLong),
        _ => None,
    }
}

fn http_error(status: u16, body: &[u8]) -> WorkModelError {
    let value = serde_json::from_slice::<Value>(body).ok();
    if let Some(error) = value
        .as_ref()
        .and_then(|value| value.pointer("/error/code"))
        .and_then(Value::as_str)
        .and_then(provider_error)
    {
        return error;
    }
    match status {
        401 => chatgpt_error(ChatGptModelFault::SignedOut),
        403 => chatgpt_error(ChatGptModelFault::NotEligible),
        429 => chatgpt_error(ChatGptModelFault::UsageLimit),
        503 => chatgpt_error(ChatGptModelFault::Unavailable),
        400 | 404 | 413 | 422 => chatgpt_error(ChatGptModelFault::Unsupported),
        _ => WorkModelError::Protocol,
    }
}

struct StreamDecoder {
    line: Vec<u8>,
    data: String,
    event: String,
    skip_lf: bool,
    bytes: usize,
    event_count: usize,
    text_bytes: usize,
    allowed: HashSet<String>,
    requires_tool: bool,
}

impl StreamDecoder {
    fn new(request: &WorkModelRequest) -> Self {
        Self {
            line: Vec::new(),
            data: String::new(),
            event: String::new(),
            skip_lf: false,
            bytes: 0,
            event_count: 0,
            text_bytes: 0,
            requires_tool: request.tool_choice != WorkModelToolChoice::Auto,
            allowed: request
                .tools
                .iter()
                .filter(|tool| match &request.tool_choice {
                    WorkModelToolChoice::Named(name) => &tool.name == name,
                    _ => true,
                })
                .map(|tool| tool.name.clone())
                .collect(),
        }
    }

    fn push(
        &mut self,
        chunk: &[u8],
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<Option<WorkModelOutcome>, WorkModelError> {
        self.bytes = self.bytes.saturating_add(chunk.len());
        if self.bytes > MAX_STREAM_BYTES {
            return Err(WorkModelError::Protocol);
        }
        for &byte in chunk {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' || byte == b'\n' {
                if byte == b'\r' {
                    self.skip_lf = true;
                }
                if let Some(outcome) = self.line_end(sink)? {
                    return Ok(Some(outcome));
                }
            } else {
                if self.line.len() >= MAX_EVENT_BYTES {
                    return Err(WorkModelError::Protocol);
                }
                self.line.push(byte);
            }
        }
        Ok(None)
    }

    fn finish(
        &mut self,
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<WorkModelOutcome, WorkModelError> {
        if !self.line.is_empty() {
            if let Some(outcome) = self.line_end(sink)? {
                return Ok(outcome);
            }
        }
        self.dispatch(sink)?.ok_or(WorkModelError::Protocol)
    }

    fn line_end(
        &mut self,
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<Option<WorkModelOutcome>, WorkModelError> {
        if self.line.is_empty() {
            return self.dispatch(sink);
        }
        let bytes = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&bytes).map_err(|_| WorkModelError::Protocol)?;
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self
                    .data
                    .len()
                    .saturating_add(value.len())
                    .saturating_add(1)
                    > MAX_EVENT_BYTES
                {
                    return Err(WorkModelError::Protocol);
                }
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(
        &mut self,
        sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
    ) -> Result<Option<WorkModelOutcome>, WorkModelError> {
        if self.data.is_empty() {
            self.event.clear();
            return Ok(None);
        }
        self.event_count += 1;
        if self.event_count > MAX_EVENTS {
            return Err(WorkModelError::Protocol);
        }
        let data = std::mem::take(&mut self.data);
        let event = std::mem::take(&mut self.event);
        if data == "[DONE]" {
            return Err(WorkModelError::Protocol);
        }
        let value: Value = serde_json::from_str(&data).map_err(|_| WorkModelError::Protocol)?;
        let kind = value.get("type").and_then(Value::as_str).unwrap_or(&event);
        match kind {
            "response.output_text.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or(WorkModelError::Protocol)?;
                self.text_bytes = self.text_bytes.saturating_add(delta.len());
                if self.text_bytes > MAX_OUTPUT_BYTES {
                    return Err(WorkModelError::Protocol);
                }
                if !delta.is_empty() {
                    sink(WorkModelEvent::Text(delta.to_owned()));
                }
            }
            "response.completed" => {
                return completed(
                    value.get("response").ok_or(WorkModelError::Protocol)?,
                    &self.allowed,
                    self.requires_tool,
                    self.text_bytes == 0,
                    sink,
                )
                .map(Some)
            }
            "response.failed" | "error" => {
                let code = value
                    .pointer("/response/error/code")
                    .or_else(|| value.pointer("/error/code"))
                    .or_else(|| value.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                return Err(provider_error(code).unwrap_or(WorkModelError::Protocol));
            }
            "response.incomplete" => return Err(WorkModelError::Protocol),
            _ => {}
        }
        Ok(None)
    }
}

fn completed(
    response: &Value,
    allowed: &HashSet<String>,
    requires_tool: bool,
    emit_text: bool,
    sink: &(dyn Fn(WorkModelEvent) + Send + Sync),
) -> Result<WorkModelOutcome, WorkModelError> {
    if response.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(WorkModelError::Protocol);
    }
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or(WorkModelError::Protocol)?;
    if output.len() > MAX_TOOLS {
        return Err(WorkModelError::Protocol);
    }
    let usage = response.get("usage").ok_or(WorkModelError::Protocol)?;
    let input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .ok_or(WorkModelError::Protocol)?;
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .ok_or(WorkModelError::Protocol)?;
    let cached_input_tokens = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning_tokens = usage
        .pointer("/output_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if cached_input_tokens > input_tokens || reasoning_tokens > output_tokens {
        return Err(WorkModelError::Protocol);
    }
    let mut assistant = Vec::new();
    let mut emitted = Vec::new();
    let mut seen_calls = HashSet::new();
    let mut refused = false;
    let mut text_bytes = 0usize;
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if item.get("role").and_then(Value::as_str) != Some("assistant") {
                    return Err(WorkModelError::Protocol);
                }
                let content = item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or(WorkModelError::Protocol)?;
                let mut text = String::new();
                for part in content {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            let piece = part
                                .get("text")
                                .and_then(Value::as_str)
                                .ok_or(WorkModelError::Protocol)?;
                            text_bytes = text_bytes.saturating_add(piece.len());
                            if text_bytes > MAX_OUTPUT_BYTES {
                                return Err(WorkModelError::Protocol);
                            }
                            text.push_str(piece);
                        }
                        Some("refusal") => refused = true,
                        _ => return Err(WorkModelError::Protocol),
                    }
                }
                if !text.is_empty() {
                    if emit_text {
                        emitted.push(WorkModelEvent::Text(text.clone()));
                    }
                    assistant.push(WorkModelPart::Text(text));
                }
            }
            Some("function_call") => {
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|id| bounded_word(id, 256))
                    .ok_or(WorkModelError::Protocol)?;
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(WorkModelError::Protocol)?;
                if item.get("namespace").and_then(Value::as_str) != Some(NAMESPACE)
                    || !allowed.contains(name)
                    || !seen_calls.insert(id)
                {
                    return Err(WorkModelError::Protocol);
                }
                let raw = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .filter(|raw| raw.len() <= MAX_TOOL_ARGUMENT_BYTES)
                    .ok_or(WorkModelError::Protocol)?;
                let arguments: Value =
                    serde_json::from_str(raw).map_err(|_| WorkModelError::Protocol)?;
                if !arguments.is_object() {
                    return Err(WorkModelError::Protocol);
                }
                let call = WorkModelToolCall {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    arguments,
                };
                emitted.push(WorkModelEvent::ToolCall(call.clone()));
                assistant.push(WorkModelPart::ToolCall(call));
            }
            Some("reasoning") => assistant.push(WorkModelPart::Replay(
                json!({"wire":"chatgpt","value":reasoning_item(item)?}),
            )),
            _ => return Err(WorkModelError::Protocol),
        }
    }
    if requires_tool && seen_calls.is_empty() && !refused {
        return Err(WorkModelError::Protocol);
    }
    // Tool calls are observable only after the whole terminal response validates.
    for event in emitted {
        sink(event);
    }
    Ok(WorkModelOutcome {
        stop: if !seen_calls.is_empty() {
            WorkModelStop::ToolUse
        } else if refused {
            WorkModelStop::Refused
        } else {
            WorkModelStop::EndTurn
        },
        usage: WorkModelUsage {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            ..WorkModelUsage::default()
        },
        assistant,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn bearer_is_sensitive_and_requires_a_current_direct_grant() {
        let mut tokens = Tokens(crate::token::Record {
            version: 1,
            identity: crate::token::Identity {
                issuer: "https://auth.openai.com".into(),
                subject: "fixture-subject".into(),
                client_id: "oaiapp_fixture".into(),
                host_id: "urn:uuid:12345678-1234-4123-8123-123456789abc".into(),
                email: None,
            },
            access_token: Zeroizing::new("fixture-access-secret".into()),
            refresh_token: Zeroizing::new("fixture-refresh-secret".into()),
            id_token: Zeroizing::new("fixture-id-token".into()),
            nonce: Zeroizing::new("fixture-nonce".into()),
            scopes: vec!["chatgpt.tokens.use.direct".into()],
            expires_at: u64::MAX,
        });
        let header = bearer(&tokens).unwrap();
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains("fixture-access-secret"));
        tokens.0.expires_at = 0;
        assert_eq!(
            bearer(&tokens).err(),
            Some(chatgpt_error(ChatGptModelFault::SignedOut))
        );
        tokens.0.expires_at = u64::MAX;
        tokens.0.scopes = vec!["openid".into(), "email".into()];
        assert_eq!(
            bearer(&tokens).err(),
            Some(chatgpt_error(ChatGptModelFault::SignedOut))
        );
        tokens.0.scopes = vec!["chatgpt.tokens.use.direct".into()];
        tokens.0.access_token = Zeroizing::new("invalid\r\nheader".into());
        assert_eq!(
            bearer(&tokens).err(),
            Some(chatgpt_error(ChatGptModelFault::SignedOut))
        );
    }

    fn request() -> WorkModelRequest {
        WorkModelRequest {
            image_fence: None,
            model: WorkModelRef {
                provider: WorkModelProvider::OpenAi,
                wire: WorkModelWire::OpenAiResponses,
                model: "gpt-fixture".into(),
            },
            system: vec![WorkModelSystemBlock {
                text: "Follow the user's request.".into(),
                cache: false,
            }],
            tools: vec![WorkModelTool {
                name: "read_page".into(),
                description: "Read one approved page".into(),
                schema: json!({"type":"object","properties":{},"additionalProperties":false}),
                strict: true,
            }],
            messages: vec![WorkModelMessage::User(vec![WorkModelPart::Text(
                "Hello".into(),
            )])],
            max_output_tokens: 2048,
            reasoning: None,
            native_search: false,
            parallel_tools: true,
            tool_choice: WorkModelToolChoice::Auto,
            volatile_tail: 0,
            cache_key: Some("ignored".into()),
        }
    }

    fn text_response() -> Value {
        json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello"}]}],"usage":{"input_tokens":8,"output_tokens":2}}})
    }

    fn frame(value: Value) -> String {
        format!("data: {value}\n\n")
    }

    #[test]
    fn exact_known_account_models_offer_existing_effort_slider_and_wire_values() {
        use WorkModelReasoning::*;
        for slug in [
            "gpt-6.1-sol",
            "gpt-6-luna",
            "gpt-6-sol",
            "gpt-6-astra",
            "gpt-5.6-terra",
        ] {
            let entry = Model {
                slug: slug.into(),
                display_name: "Clean model name".into(),
            }
            .entry();
            assert!(entry.supports.reasoning);
            assert_eq!(entry.display_name, "Clean model name");
            assert_eq!(entry.id, format!("chatgpt/{slug}"));
            for (effort, wire) in [
                (Low, "low"),
                (Medium, "medium"),
                (High, "high"),
                (XHigh, "xhigh"),
                (Max, "max"),
            ] {
                let mut request = request();
                request.model = entry.model.clone();
                request.reasoning = Some(effort);
                assert_eq!(
                    request_body(&request).unwrap()["reasoning"],
                    json!({"effort":wire})
                );
            }
        }
    }

    #[test]
    fn unknown_aliases_and_similar_names_do_not_acquire_effort_capabilities() {
        for slug in [
            "gpt-6-unknown",
            "gpt-6.1-sol-custom",
            "gpt-5.6",
            "GPT-6-LUNA",
            "gpt-fixture",
        ] {
            let entry = Model {
                slug: slug.into(),
                display_name: "GPT-6 Luna".into(),
            }
            .entry();
            assert!(!entry.supports.reasoning);
            let mut request = request();
            request.model = entry.model;
            request.reasoning = Some(WorkModelReasoning::High);
            assert_eq!(
                request_body(&request).err(),
                Some(WorkModelError::BadRequest)
            );
        }
        assert!(request_body(&request()).unwrap().get("reasoning").is_none());
    }

    #[test]
    fn preview_body_is_explicit_and_omits_incompatible_api_fields() {
        let body = request_body(&request()).unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body["input"].is_array());
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["tools"][0]["type"], "namespace");
        assert_eq!(body["tools"][0]["name"], "zephium");
        assert_eq!(body["tools"][0]["tools"][0]["type"], "function");
        for field in [
            "background",
            "conversation",
            "max_output_tokens",
            "max_tool_calls",
            "metadata",
            "moderation",
            "multi_agent",
            "prompt",
            "prompt_cache_retention",
            "safety_identifier",
            "temperature",
            "top_logprobs",
            "top_p",
            "truncation",
            "user",
            "previous_response_id",
        ] {
            assert!(body.get(field).is_none(), "{field}");
        }
        assert!(!body.to_string().contains("backend-api"));
    }

    #[test]
    fn named_choice_offers_only_that_function_and_requires_its_use() {
        let mut request = request();
        let mut second = request.tools[0].clone();
        second.name = "other".into();
        request.tools.push(second);
        request.tool_choice = WorkModelToolChoice::Named("read_page".into());
        let body = request_body(&request).unwrap();
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["tools"][0]["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["tools"][0]["name"], "read_page");
        let mut decoder = StreamDecoder::new(&request);
        assert_eq!(
            decoder
                .push(frame(text_response()).as_bytes(), &|_| {})
                .err(),
            Some(WorkModelError::Protocol)
        );
        let mut decoder = StreamDecoder::new(&request);
        let mut other = tool_response();
        other["response"]["output"][0]["name"] = json!("other");
        assert_eq!(
            decoder.push(frame(other).as_bytes(), &|_| {}).err(),
            Some(WorkModelError::Protocol)
        );
        request.tool_choice = WorkModelToolChoice::Named("not_offered".into());
        assert_eq!(
            request_body(&request).err(),
            Some(chatgpt_error(ChatGptModelFault::Unsupported))
        );
    }

    #[test]
    fn model_catalog_is_account_specific_bounded_and_preserves_server_order() {
        let models = parse_models(br#"{"models":[{"slug":"model-z","display_name":"Z","visibility":"list"},{"slug":"hidden","display_name":"Secret","visibility":"hide"},{"slug":"model-a","display_name":"A","visibility":"list"}]}"#).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.slug.as_str())
                .collect::<Vec<_>>(),
            ["model-z", "model-a"]
        );
        let entry = models[0].entry();
        assert_eq!(entry.id, "chatgpt/model-z");
        assert_eq!(entry.display_name, "Z");
        assert_eq!(entry.model.provider, WorkModelProvider::OpenAi);
        assert!(!entry.supports.vision && !entry.supports.native_search);
        assert!(entry.price.is_none());
        for bytes in [br#"{"data":[{"id":"model"}]}"#.as_slice(), br#"{"models":[{"slug":"../bad","display_name":"B","visibility":"list"}]}"#.as_slice(), br#"{"models":[{"slug":"one","display_name":"A","visibility":"list"},{"slug":"one","display_name":"B","visibility":"list"}]}"#.as_slice()] {
            assert_eq!(parse_models(bytes), Err(WorkModelError::Protocol));
        }
        assert_eq!(
            parse_models(&vec![b' '; MAX_CATALOG_BYTES + 1]),
            Err(WorkModelError::Protocol)
        );
    }

    #[test]
    fn only_a_completed_terminal_event_is_success() {
        let mut decoder = StreamDecoder::new(&request());
        let output = decoder
            .push(frame(text_response()).as_bytes(), &|_| {})
            .unwrap()
            .unwrap();
        assert_eq!(output.stop, WorkModelStop::EndTurn);
        assert_eq!(output.usage.input_tokens, 8);
        assert_eq!(output.usage.output_tokens, 2);
        assert!(matches!(&output.assistant[0], WorkModelPart::Text(text) if text == "Hello"));
        for event in [
            json!({"type":"response.incomplete","response":{"status":"incomplete"}}),
            json!({"type":"response.failed","response":{"error":{"code":"unknown"}}}),
        ] {
            let mut decoder = StreamDecoder::new(&request());
            assert_eq!(
                decoder.push(frame(event).as_bytes(), &|_| {}).err(),
                Some(WorkModelError::Protocol)
            );
        }
        let mut decoder = StreamDecoder::new(&request());
        assert!(decoder
            .push(
                frame(json!({"type":"response.output_text.delta","delta":"Partial"})).as_bytes(),
                &|_| {}
            )
            .unwrap()
            .is_none());
        assert_eq!(
            decoder.finish(&|_| {}).err(),
            Some(WorkModelError::Protocol)
        );
        let mut decoder = StreamDecoder::new(&request());
        assert_eq!(
            decoder.push(b"data: [DONE]\n\n", &|_| {}).err(),
            Some(WorkModelError::Protocol)
        );
    }

    #[test]
    fn chunks_crlf_unicode_and_eof_terminal_are_framed_without_duplicate_text() {
        let events = Mutex::new(Vec::new());
        let sink = |event| events.lock().unwrap().push(event);
        let mut decoder = StreamDecoder::new(&request());
        let delta = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hé\"}\r\n\r\n";
        for byte in delta.as_bytes() {
            assert!(decoder.push(&[*byte], &sink).unwrap().is_none());
        }
        let mut completed = text_response();
        completed["response"]["output"][0]["content"][0]["text"] = json!("Hé");
        let line = format!("event: response.completed\ndata: {completed}");
        assert!(decoder.push(line.as_bytes(), &sink).unwrap().is_none());
        let output = decoder.finish(&sink).unwrap();
        assert!(matches!(&output.assistant[0], WorkModelPart::Text(text) if text == "Hé"));
        assert_eq!(events.lock().unwrap().len(), 1);
    }

    #[test]
    fn quota_failure_after_text_is_not_success_or_a_retryable_network_error() {
        for (code, expected) in [
            (
                "subscription_sharing_usage_limit_exceeded",
                chatgpt_error(ChatGptModelFault::UsageLimit),
            ),
            (
                "subscription_sharing_usage_unavailable",
                chatgpt_error(ChatGptModelFault::Unavailable),
            ),
        ] {
            let mut decoder = StreamDecoder::new(&request());
            decoder
                .push(
                    frame(json!({"type":"response.output_text.delta","delta":"Partial"}))
                        .as_bytes(),
                    &|_| {},
                )
                .unwrap();
            assert_eq!(decoder.push(frame(json!({"type":"response.failed","response":{"error":{"code":code,"message":"untrusted secret"}}})).as_bytes(), &|_| {}).err(), Some(expected));
        }
    }

    fn tool_response() -> Value {
        json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","call_id":"call_1","namespace":"zephium","name":"read_page","arguments":"{}"}],"usage":{"input_tokens":8,"output_tokens":3}}})
    }

    #[test]
    fn tool_calls_are_validated_and_delivered_only_after_successful_terminal() {
        let events = Mutex::new(Vec::new());
        let sink = |event| events.lock().unwrap().push(event);
        let mut decoder = StreamDecoder::new(&request());
        decoder.push(frame(json!({"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","namespace":"zephium","name":"read_page","arguments":"{}"}})).as_bytes(), &sink).unwrap();
        assert!(events.lock().unwrap().is_empty());
        let outcome = decoder
            .push(frame(tool_response()).as_bytes(), &sink)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.stop, WorkModelStop::ToolUse);
        assert_eq!(events.lock().unwrap().len(), 1);
        assert!(
            matches!(&outcome.assistant[0], WorkModelPart::ToolCall(call) if call.name == "read_page")
        );
    }

    #[test]
    fn malformed_or_unoffered_tool_never_escapes_as_an_event() {
        for (key, value) in [
            ("namespace", json!("other")),
            ("name", json!("run_shell")),
            ("arguments", json!("not-json")),
            ("arguments", json!("[]")),
        ] {
            let mut response = tool_response();
            response["response"]["output"][0][key] = value;
            let events = Mutex::new(Vec::new());
            let mut decoder = StreamDecoder::new(&request());
            assert_eq!(
                decoder
                    .push(frame(response).as_bytes(), &|event| events
                        .lock()
                        .unwrap()
                        .push(event))
                    .err(),
                Some(WorkModelError::Protocol)
            );
            assert!(events.lock().unwrap().is_empty());
        }
        let mut response = tool_response();
        let duplicate = response["response"]["output"][0].clone();
        response["response"]["output"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        let mut decoder = StreamDecoder::new(&request());
        assert_eq!(
            decoder.push(frame(response).as_bytes(), &|_| {}).err(),
            Some(WorkModelError::Protocol)
        );
    }

    #[test]
    fn a_late_invalid_item_cannot_release_an_earlier_tool_call() {
        let mut response = tool_response();
        response["response"]["output"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"hosted_tool_search","arguments":"{}"}));
        let events = Mutex::new(Vec::new());
        let mut decoder = StreamDecoder::new(&request());
        assert_eq!(
            decoder
                .push(frame(response).as_bytes(), &|event| events
                    .lock()
                    .unwrap()
                    .push(event))
                .err(),
            Some(WorkModelError::Protocol)
        );
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn incomplete_terminal_data_and_invalid_accounting_are_not_success() {
        for patch in [
            json!({"status":"failed"}),
            json!({"usage":null}),
            json!({"usage":{"input_tokens":1,"output_tokens":2,"input_tokens_details":{"cached_tokens":3}}}),
        ] {
            let mut response = text_response();
            for (key, value) in patch.as_object().unwrap() {
                response["response"][key] = value.clone();
            }
            let mut decoder = StreamDecoder::new(&request());
            assert_eq!(
                decoder.push(frame(response).as_bytes(), &|_| {}).err(),
                Some(WorkModelError::Protocol)
            );
        }
    }

    #[test]
    fn unsupported_images_search_and_cross_wire_replay_fail_before_dispatch() {
        let mut image = request();
        image.messages = vec![WorkModelMessage::User(vec![WorkModelPart::Image {
            media_type: "image/png".into(),
            bytes: vec![1, 2, 3],
        }])];
        assert_eq!(request_body(&image).err(), Some(WorkModelError::BadRequest));
        let mut search = request();
        search.native_search = true;
        assert_eq!(
            request_body(&search).err(),
            Some(WorkModelError::BadRequest)
        );
        let mut reasoning = request();
        reasoning.reasoning = Some(WorkModelReasoning::High);
        assert_eq!(
            request_body(&reasoning).err(),
            Some(WorkModelError::BadRequest)
        );
        let mut replay = request();
        replay.messages = vec![WorkModelMessage::Assistant(vec![WorkModelPart::Replay(
            json!({"wire":"openai","value":{"type":"reasoning"}}),
        )])];
        assert_eq!(
            request_body(&replay).err(),
            Some(WorkModelError::BadRequest)
        );
    }

    #[test]
    fn history_replays_only_this_routes_bounded_reasoning_and_tool_results() {
        let mut request = request();
        request.messages.push(WorkModelMessage::Assistant(vec![WorkModelPart::ToolCall(WorkModelToolCall { id:"call_1".into(),name:"read_page".into(),arguments:json!({}) }), WorkModelPart::Replay(json!({"wire":"chatgpt","value":{"type":"reasoning","id":"r1","encrypted_content":"opaque","unexpected":"discarded"}}))]));
        request
            .messages
            .push(WorkModelMessage::ToolResults(vec![WorkModelToolResult {
                call: "call_1".into(),
                content: "result".into(),
                is_error: false,
            }]));
        let body = request_body(&request).unwrap();
        assert_eq!(body["input"][1]["namespace"], "zephium");
        assert_eq!(body["input"][2]["encrypted_content"], "opaque");
        assert!(body["input"][2].get("unexpected").is_none());
        assert_eq!(body["input"][3]["type"], "function_call_output");
    }

    #[test]
    fn stream_bounds_apply_before_json_and_partial_utf8_is_rejected() {
        let mut decoder = StreamDecoder::new(&request());
        assert_eq!(
            decoder
                .push(&vec![b'x'; MAX_EVENT_BYTES + 1], &|_| {})
                .err(),
            Some(WorkModelError::Protocol)
        );
        let mut decoder = StreamDecoder::new(&request());
        assert_eq!(
            decoder.push(b"data: \xff\n\n", &|_| {}).err(),
            Some(WorkModelError::Protocol)
        );
        let mut decoder = StreamDecoder::new(&request());
        decoder.bytes = MAX_STREAM_BYTES;
        assert_eq!(
            decoder.push(b"x", &|_| {}).err(),
            Some(WorkModelError::Protocol)
        );
    }

    #[test]
    fn http_errors_are_closed_subscription_faults_without_automatic_retry() {
        for (status, expected) in [
            (401, ChatGptModelFault::SignedOut),
            (403, ChatGptModelFault::NotEligible),
            (429, ChatGptModelFault::UsageLimit),
            (503, ChatGptModelFault::Unavailable),
            (400, ChatGptModelFault::Unsupported),
        ] {
            assert_eq!(
                http_error(status, b"untrusted secret body"),
                chatgpt_error(expected)
            );
        }
        for (code, expected) in [
            (
                "subscription_sharing_usage_limit_exceeded",
                ChatGptModelFault::UsageLimit,
            ),
            (
                "subscription_sharing_usage_unavailable",
                ChatGptModelFault::Unavailable,
            ),
            (
                "subscription_sharing_user_unavailable",
                ChatGptModelFault::Unavailable,
            ),
            (
                "subscription_sharing_user_not_eligible",
                ChatGptModelFault::NotEligible,
            ),
            (
                "chatpass_v2_scope_not_authorized",
                ChatGptModelFault::NotEligible,
            ),
            (
                "chatpass_v2_invalid_authorization_context",
                ChatGptModelFault::NotEligible,
            ),
            (
                "subscription_sharing_unsupported_capability",
                ChatGptModelFault::Unsupported,
            ),
            (
                "subscription_sharing_route_not_supported",
                ChatGptModelFault::Unsupported,
            ),
            (
                "subscription_sharing_invalid_user",
                ChatGptModelFault::SignedOut,
            ),
        ] {
            let body =
                serde_json::to_vec(&json!({"error":{"code":code,"message":"secret"}})).unwrap();
            assert_eq!(http_error(400, &body), chatgpt_error(expected));
        }
        assert_eq!(http_error(500, b"unknown error"), WorkModelError::Protocol);
    }
}
