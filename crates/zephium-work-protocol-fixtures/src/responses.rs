//! Offline legacy Responses wire fixtures for retained Work protocol tests.
//! This module has no network client, credentials or production account route.

use std::collections::HashSet;

use serde_json::{json, Value};
use zephium_core::work::model::*;

const NAMESPACE: &str = "zephium";
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAM_BYTES: usize = 32 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_TOOLS: usize = 128;
const MAX_EVENTS: usize = 100_000;

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

/// Exercise the retained legacy wire contract with an in-memory response only.
/// This feature has no credentials, networking, endpoint override or retries.
pub fn fixture_call(
    request: &WorkModelRequest,
    response: &[u8],
    events: &(dyn Fn(WorkModelEvent) + Send + Sync),
) -> Result<(Vec<u8>, WorkModelOutcome), WorkModelError> {
    let encoded = encode_request(request)?;
    let mut decoder = StreamDecoder::new(request);
    let decoded = match decoder.push(response, events) {
        Ok(Some(outcome)) => Ok(outcome),
        Ok(None) => decoder.finish(events),
        Err(error) => Err(error),
    };
    decoded
        .map(|outcome| (encoded, outcome))
        .map_err(|error| diagnose(ChatGptFailureStage::Stream, Some(200), error))
}

fn encode_request(request: &WorkModelRequest) -> Result<Vec<u8>, WorkModelError> {
    let encode = || {
        let body = request_body(request)?;
        let encoded = serde_json::to_vec(&body).map_err(|_| WorkModelError::BadRequest)?;
        if encoded.len() > MAX_REQUEST_BYTES {
            return Err(WorkModelError::BadRequest);
        }
        Ok(encoded)
    };
    encode().map_err(|error| diagnose(ChatGptFailureStage::Request, None, error))
}

fn diagnostic(
    stage: ChatGptFailureStage,
    status: Option<u16>,
    issue: ChatGptFailureIssue,
) -> WorkModelError {
    chatgpt_error(ChatGptModelFault::Diagnostic {
        stage,
        status: status.filter(|status| (100..=599).contains(status)),
        issue,
        field: None,
    })
}

/// Attach native evidence without retaining any provider-authored payload.
pub(crate) fn diagnose(
    stage: ChatGptFailureStage,
    status: Option<u16>,
    error: WorkModelError,
) -> WorkModelError {
    let issue = match error {
        WorkModelError::ChatGpt {
            fault: ChatGptModelFault::Diagnostic { .. },
        } => return error,
        WorkModelError::ChatGpt { fault } => fault.issue(),
        WorkModelError::BadRequest | WorkModelError::ContextTooLong => {
            ChatGptFailureIssue::InvalidRequest
        }
        WorkModelError::Unauthorized | WorkModelError::MissingKey => ChatGptFailureIssue::SignedOut,
        WorkModelError::Network => ChatGptFailureIssue::Unavailable,
        WorkModelError::Cancelled => ChatGptFailureIssue::Cancelled,
        _ => ChatGptFailureIssue::Protocol,
    };
    diagnostic(stage, status, issue)
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
        self.dispatch(sink)?.ok_or_else(|| {
            diagnostic(
                ChatGptFailureStage::Stream,
                Some(200),
                ChatGptFailureIssue::Incomplete,
            )
        })
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
                    value.get("response").ok_or_else(|| {
                        diagnostic(
                            ChatGptFailureStage::Complete,
                            Some(200),
                            ChatGptFailureIssue::Protocol,
                        )
                    })?,
                    &self.allowed,
                    self.requires_tool,
                    self.text_bytes == 0,
                    sink,
                )
                .map(Some)
                .map_err(|error| diagnose(ChatGptFailureStage::Complete, Some(200), error))
            }
            "response.failed" | "error" => {
                let code = value
                    .pointer("/response/error/code")
                    .or_else(|| value.pointer("/error/code"))
                    .or_else(|| value.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                return Err(diagnose(
                    ChatGptFailureStage::Stream,
                    Some(200),
                    provider_error(code).unwrap_or(WorkModelError::Protocol),
                ));
            }
            "response.incomplete" => {
                return Err(diagnostic(
                    ChatGptFailureStage::Stream,
                    Some(200),
                    ChatGptFailureIssue::Incomplete,
                ))
            }
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
    let usage = response_usage(response.get("usage"))?;
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
    let usage = usage.unwrap_or_else(WorkModelUsage::unavailable);
    // Completion and measurement are independent: Page may consume a validated
    // tool outcome under its existing bounded loop even when usage is absent.
    // Work, however, may execute ToolCall callbacks before inspecting the
    // outcome. Withhold only those callbacks until measurement is known; its
    // source guard will then stop the run conservatively on unavailable usage.
    for event in emitted {
        if usage.is_reported() || !matches!(event, WorkModelEvent::ToolCall(_)) {
            sink(event);
        }
    }
    Ok(WorkModelOutcome {
        stop: if !seen_calls.is_empty() {
            WorkModelStop::ToolUse
        } else if refused {
            WorkModelStop::Refused
        } else {
            WorkModelStop::EndTurn
        },
        usage,
        assistant,
    })
}

// The official Response schema permits absent/null usage. Completion and
// measurement are separate facts; malformed *present* usage is not absence.
fn response_usage(value: Option<&Value>) -> Result<Option<WorkModelUsage>, WorkModelError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    if !value.is_object() {
        return Err(WorkModelError::Protocol);
    }
    let input_tokens = value
        .get("input_tokens")
        .and_then(Value::as_u64)
        .ok_or(WorkModelError::Protocol)?;
    let output_tokens = value
        .get("output_tokens")
        .and_then(Value::as_u64)
        .ok_or(WorkModelError::Protocol)?;
    let detail = |object: &str, key: &str| -> Result<u64, WorkModelError> {
        match value.get(object) {
            None | Some(Value::Null) => Ok(0),
            Some(details) if details.is_object() => match details.get(key) {
                None => Ok(0),
                Some(count) => count.as_u64().ok_or(WorkModelError::Protocol),
            },
            _ => Err(WorkModelError::Protocol),
        }
    };
    let cached_input_tokens = detail("input_tokens_details", "cached_tokens")?;
    let reasoning_tokens = detail("output_tokens_details", "reasoning_tokens")?;
    let total = input_tokens
        .checked_add(output_tokens)
        .ok_or(WorkModelError::Protocol)?;
    if cached_input_tokens > input_tokens
        || reasoning_tokens > output_tokens
        || value
            .get("total_tokens")
            .is_some_and(|value| value.as_u64() != Some(total))
    {
        return Err(WorkModelError::Protocol);
    }
    Ok(Some(WorkModelUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        reasoning_tokens,
        ..WorkModelUsage::default()
    }))
}

#[cfg(test)]
mod tests;
