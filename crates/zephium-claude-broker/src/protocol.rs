//! Narrow one-turn Claude Code NDJSON and account-status validation.
//!
//! These pure parsers grant no process or account authority. Input must come from
//! the separately qualified original CLI's stdout, never stderr. No credential,
//! account email, provider diagnostic or terminal command is projected. Sources:
//! <https://code.claude.com/docs/en/headless>,
//! <https://code.claude.com/docs/en/cli-reference>, and the official Agent SDK's
//! `message_parser.py` / `types.py`. The full pinned CLI sequence still requires
//! native qualification; an unknown protocol is rejected rather than guessed.

use crate::strict_json;
use serde_json::{Map, Value};
use std::fmt;

/// Maximum status output before parsing.
pub const MAX_STATUS_BYTES: usize = 16 * 1024;
/// Maximum bytes in an NDJSON line, excluding its final LF (including CR).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Maximum aggregate stdout bytes, including whitespace and delimiters.
pub const MAX_STDOUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_EVENTS: usize = 16_384;
const MAX_RESET_SECONDS: u64 = 8_640_000_000_000;

/// Closed protocol failures; no raw subprocess output is retained in errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    /// A fixed byte, event or structural limit was exceeded.
    LimitExceeded,
    /// Invalid UTF-8/JSON, duplicate keys, excessive nesting or multiple values.
    InvalidJson,
    /// A required field had an unexpected shape.
    InvalidShape,
    /// The event's type is outside the reviewed one-turn vocabulary.
    UnsupportedEvent,
    /// Tools, MCP, hooks, plugins, agent children or injected turns were observed.
    UnsafeCapability,
    /// A session, message, or content-block sequence did not match.
    SequenceMismatch,
    /// A prior failure poisoned this stream; it cannot be resumed.
    Poisoned,
    /// EOF arrived without one complete terminal result.
    Incomplete,
    /// A result was followed by more events, or the decoder was already finished.
    AfterTerminal,
    /// A terminal result was incompatible with its fields or operation contract.
    InvalidTerminal,
    /// The child did not exit successfully; displayed deltas are not completion.
    ProcessFailed,
    /// The CLI did not report the exact native-owned configuration directory.
    ConfigDirectoryMismatch,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LimitExceeded => "Claude output exceeded its fixed limit",
            Self::InvalidJson => "Claude output was not valid bounded JSON",
            Self::InvalidShape => "Claude output did not match the reviewed schema",
            Self::UnsupportedEvent => "Claude emitted an unsupported event",
            Self::UnsafeCapability => "Claude reported a disabled capability",
            Self::SequenceMismatch => "Claude output did not match this turn",
            Self::Poisoned => "Claude output was already rejected",
            Self::Incomplete => "Claude did not complete a terminal result",
            Self::AfterTerminal => "Claude emitted output after its terminal result",
            Self::InvalidTerminal => "Claude terminal fields were incompatible",
            Self::ProcessFailed => "Claude did not exit successfully",
            Self::ConfigDirectoryMismatch => "Claude configuration isolation did not match",
        })
    }
}
impl std::error::Error for ProtocolError {}

/// Authentication mode evidence from the original CLI, not entitlement evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthStatus {
    /// Official CLI explicitly reports signed out with exit 1.
    SignedOut,
    /// Official CLI reports its own Claude.ai authentication with exit 0.
    ClaudeAccount,
    /// API key/helper billing; not a subscription session.
    ApiCredential,
    /// Third-party provider; not a subscription session.
    ExternalProvider,
    /// A mode not admitted as subscription proof, including `oauth_token`.
    Unknown,
}

/// Validate bounded `auth status` stdout, process exit and native-owned directory.
///
/// The expected directory must come from native provisioning, not renderer or
/// provider text. Exact equality intentionally fails closed on aliases. A native
/// qualified path canonicalization contract may be added separately, not here.
pub fn parse_status(
    stdout: &[u8],
    exit_code: i32,
    expected_config_dir: &str,
) -> Result<AuthStatus, ProtocolError> {
    if stdout.len() > MAX_STATUS_BYTES {
        return Err(ProtocolError::LimitExceeded);
    }
    let value = strict_json::parse(stdout).map_err(|_| ProtocolError::InvalidJson)?;
    let object = object(&value)?;
    if expected_config_dir.is_empty() || text(object, "configDirectory")? != expected_config_dir {
        return Err(ProtocolError::ConfigDirectoryMismatch);
    }
    let logged_in = object
        .get("loggedIn")
        .and_then(Value::as_bool)
        .ok_or(ProtocolError::InvalidShape)?;
    let method = text(object, "authMethod")?;
    match (exit_code, logged_in, method) {
        (1, false, "none") => Ok(AuthStatus::SignedOut),
        (0, true, "claude.ai") => {
            if object
                .get("apiProvider")
                .is_some_and(|provider| provider != "firstParty")
            {
                Ok(AuthStatus::Unknown)
            } else {
                Ok(AuthStatus::ClaudeAccount)
            }
        }
        (0, true, "api_key" | "api_key_helper") => Ok(AuthStatus::ApiCredential),
        (0, true, "third_party") => Ok(AuthStatus::ExternalProvider),
        (0, true, _) => Ok(AuthStatus::Unknown),
        _ => Err(ProtocolError::InvalidTerminal),
    }
}

/// A provider's known rate-limit status; never inferred from local token counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RateStatus {
    /// Provider reports usage is within its limit.
    Allowed,
    /// Provider reports usage is approaching its limit.
    Warning,
    /// Provider reports this allowance rejected the request.
    Rejected,
}

/// Exact provider-reported window; absent windows are not fabricated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RateWindow {
    /// Provider's five-hour window.
    FiveHour,
    /// Provider's seven-day window.
    SevenDay,
    /// Provider's seven-day Opus window.
    SevenDayOpus,
    /// Provider's seven-day Sonnet window.
    SevenDaySonnet,
    /// Separate pay-as-you-go/overage allowance, not included subscription quota.
    Overage,
}

/// Passive observation from an already-authorized turn; not a quota-read command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateLimit {
    /// Provider-reported status.
    pub status: RateStatus,
    /// Exact provider window when present.
    pub window: Option<RateWindow>,
    /// Validated usage fraction in 0..=1; missing is unknown, never zero.
    pub utilization: Option<f64>,
    /// Unix seconds; expiry makes the observation stale, never automatically full.
    pub resets_at_seconds: Option<u64>,
    /// Independently reported overage status.
    pub overage_status: Option<RateStatus>,
    /// Independently reported overage reset in Unix seconds.
    pub overage_resets_at_seconds: Option<u64>,
}

/// Reviewed terminal semantics. Useful text does not imply successful completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    /// A consistent successful result and successful process exit.
    Succeeded,
    /// The model refused, independently of quota or billing.
    Refused,
    /// The provider reported an error; raw diagnostics are discarded.
    ProviderError,
    /// A token/turn bound stopped the response before normal completion.
    LimitReached,
    /// The CLI reported an interrupted query; no automatic replay is permitted.
    Interrupted,
}

/// Final result, only returned after stdout EOF and observed process exit.
#[derive(Clone, Eq, PartialEq)]
pub struct TurnOutcome {
    /// Closed completion classification.
    pub completion: Completion,
    /// Final plain text; insert as text, never executable HTML or terminal input.
    pub text: String,
    /// Optional validated HTTP error status; never assumed to mean quota.
    pub api_error_status: Option<u16>,
}
impl fmt::Debug for TurnOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnOutcome")
            .field("completion", &self.completion)
            .field("text", &"[redacted]")
            .field("api_error_status", &self.api_error_status)
            .finish()
    }
}

/// Safe native presentation events. The consumer still owns bounded rendering.
#[derive(Clone, PartialEq)]
pub enum TurnEvent {
    /// Validated text delta; do not render as HTML, terminal control or auto-open URLs.
    Text(String),
    /// Passive rate-limit observation, bound by the native operation/account scope.
    RateLimit(RateLimit),
}
impl fmt::Debug for TurnEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(_) => formatter.write_str("Text([redacted])"),
            Self::RateLimit(rate) => formatter.debug_tuple("RateLimit").field(rate).finish(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Block {
    Text,
    Thinking,
}

/// Incremental bounded one-turn stdout decoder, permanently poisoned on failure.
///
/// This object intentionally does not implement `Debug`, serialization, or Clone:
/// buffers contain conversation text. Drop it after cancellation; call `finish`
/// only when the owning process has exited and stdout has reached EOF. Stderr
/// belongs to an independent bounded drain and must never be fed into this parser.
#[derive(Default)]
pub struct TurnDecoder {
    line: Vec<u8>,
    total: usize,
    events: usize,
    failed: bool,
    finished: bool,
    session: Option<String>,
    message: Option<String>,
    stopped: bool,
    block: Option<Block>,
    next_block: u64,
    summary_block: Option<u64>,
    unstreamed_summary_seen: bool,
    prompt: Option<String>,
    terminal: Option<TurnOutcome>,
}

impl TurnDecoder {
    /// Construct a fresh decoder with fixed protocol limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes from stdout. UTF-8 can split across chunks; NDJSON lines cannot
    /// exceed the fixed bound. Any failure permanently poisons this decoder.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<TurnEvent>, ProtocolError> {
        if self.failed {
            return Err(ProtocolError::Poisoned);
        }
        if self.finished {
            return Err(ProtocolError::AfterTerminal);
        }
        let result = self.push_inner(bytes);
        if result.is_err() {
            self.failed = true;
            self.line.clear();
            self.terminal = None;
        }
        result
    }

    fn push_inner(&mut self, bytes: &[u8]) -> Result<Vec<TurnEvent>, ProtocolError> {
        self.total = self
            .total
            .checked_add(bytes.len())
            .filter(|count| *count <= MAX_STDOUT_BYTES)
            .ok_or(ProtocolError::LimitExceeded)?;
        let mut emitted = Vec::new();
        for segment in bytes.split_inclusive(|byte| *byte == b'\n') {
            let newline = segment.last() == Some(&b'\n');
            let fragment = if newline {
                &segment[..segment.len() - 1]
            } else {
                segment
            };
            let length = self
                .line
                .len()
                .checked_add(fragment.len())
                .ok_or(ProtocolError::LimitExceeded)?;
            if length > MAX_LINE_BYTES {
                return Err(ProtocolError::LimitExceeded);
            }
            self.line.extend_from_slice(fragment);
            if newline {
                let line = std::mem::take(&mut self.line);
                let line = line.strip_suffix(b"\r").unwrap_or(&line);
                if line.is_empty() {
                    return Err(ProtocolError::InvalidJson);
                }
                self.events += 1;
                if self.events > MAX_EVENTS {
                    return Err(ProtocolError::LimitExceeded);
                }
                if self.terminal.is_some() {
                    return Err(ProtocolError::AfterTerminal);
                }
                let value = strict_json::parse(line).map_err(|_| ProtocolError::InvalidJson)?;
                if let Some(event) = self.event(&value)? {
                    emitted.push(event);
                }
            }
        }
        Ok(emitted)
    }

    /// Finish after stdout EOF and process exit. A missing final LF is truncated,
    /// and an apparently successful result followed by nonzero exit is not success.
    /// This consumes the decoder so an incomplete/uncertain turn cannot be replayed.
    pub fn finish(mut self, exit_code: i32) -> Result<TurnOutcome, ProtocolError> {
        self.finished = true;
        if self.failed {
            return Err(ProtocolError::Poisoned);
        }
        if !self.line.is_empty() || self.terminal.is_none() {
            return Err(ProtocolError::Incomplete);
        }
        if exit_code != 0 {
            return Err(ProtocolError::ProcessFailed);
        }
        self.terminal.take().ok_or(ProtocolError::Incomplete)
    }

    fn event(&mut self, value: &Value) -> Result<Option<TurnEvent>, ProtocolError> {
        let value = object(value)?;
        let kind = text(value, "type")?;
        if kind == "system" && value.get("subtype").and_then(Value::as_str) == Some("init") {
            self.init(value)?;
            return Ok(None);
        }
        let session = self
            .session
            .as_deref()
            .ok_or(ProtocolError::SequenceMismatch)?;
        if text(value, "session_id")? != session {
            return Err(ProtocolError::SequenceMismatch);
        }
        absent_or_null(value, "parent_tool_use_id")?;
        if let Some(origin) = value.get("origin").filter(|value| !value.is_null()) {
            if object(origin)?.get("kind").and_then(Value::as_str) != Some("human") {
                return Err(ProtocolError::UnsafeCapability);
            }
        }
        if let Some(prompt) = optional_text(value, "user_message_uuid")? {
            valid_id(prompt)?;
            if self
                .prompt
                .as_deref()
                .is_some_and(|current| current != prompt)
            {
                return Err(ProtocolError::SequenceMismatch);
            }
            self.prompt.get_or_insert_with(|| prompt.to_owned());
        }
        match kind {
            "stream_event" => self.stream(object(
                value.get("event").ok_or(ProtocolError::InvalidShape)?,
            )?),
            "assistant" => {
                self.assistant(value)?;
                Ok(None)
            }
            "result" => {
                self.terminal = Some(self.result(value)?);
                Ok(None)
            }
            "rate_limit_event" => Ok(Some(TurnEvent::RateLimit(rate_limit(value)?))),
            // API retry is observational only, but permitting arbitrary system
            // events would admit hooks/tasks/plugins. Its exact fields are checked.
            "system" if value.get("subtype").and_then(Value::as_str) == Some("api_retry") => {
                api_retry(value)?;
                Ok(None)
            }
            "control_request" | "control_response" | "user" | "conversation_reset" => {
                Err(ProtocolError::UnsafeCapability)
            }
            "system" => Err(ProtocolError::UnsafeCapability),
            _ => Err(ProtocolError::UnsupportedEvent),
        }
    }

    fn init(&mut self, value: &Map<String, Value>) -> Result<(), ProtocolError> {
        if self.session.is_some() {
            return Err(ProtocolError::SequenceMismatch);
        }
        let session = text(value, "session_id")?;
        valid_id(session)?;
        for key in ["tools", "mcp_servers", "plugins"] {
            empty_array(value, key, true)?;
        }
        for key in ["plugin_errors", "mcp_server_errors"] {
            empty_array(value, key, false)?;
        }
        if text(value, "apiKeySource")? != "none" || text(value, "permissionMode")? != "dontAsk" {
            return Err(ProtocolError::UnsafeCapability);
        }
        absent_or_null(value, "parent_tool_use_id")?;
        self.session = Some(session.to_owned());
        Ok(())
    }

    fn stream(&mut self, event: &Map<String, Value>) -> Result<Option<TurnEvent>, ProtocolError> {
        match text(event, "type")? {
            "message_start" => {
                if self.message.is_some() {
                    return Err(ProtocolError::SequenceMismatch);
                }
                let message = object(event.get("message").ok_or(ProtocolError::InvalidShape)?)?;
                if text(message, "role")? != "assistant" {
                    return Err(ProtocolError::InvalidShape);
                }
                empty_array(message, "content", true)?;
                let id = text(message, "id")?;
                valid_id(id)?;
                self.message = Some(id.to_owned());
                Ok(None)
            }
            "content_block_start" => {
                self.in_message()?;
                if self.block.is_some() || integer(event, "index")? != self.next_block {
                    return Err(ProtocolError::SequenceMismatch);
                }
                let block = object(
                    event
                        .get("content_block")
                        .ok_or(ProtocolError::InvalidShape)?,
                )?;
                match text(block, "type")? {
                    "text" => {
                        self.block = Some(Block::Text);
                        let text = safe_text(text(block, "text")?)?;
                        Ok((!text.is_empty()).then_some(TurnEvent::Text(text)))
                    }
                    "thinking" => {
                        self.block = Some(Block::Thinking);
                        Ok(None)
                    }
                    _ => Err(ProtocolError::UnsafeCapability),
                }
            }
            "content_block_delta" => {
                self.in_message()?;
                if integer(event, "index")? != self.next_block {
                    return Err(ProtocolError::SequenceMismatch);
                }
                let delta = object(event.get("delta").ok_or(ProtocolError::InvalidShape)?)?;
                match (self.block, text(delta, "type")?) {
                    (Some(Block::Text), "text_delta") => {
                        Ok(Some(TurnEvent::Text(safe_text(text(delta, "text")?)?)))
                    }
                    (Some(Block::Thinking), "thinking_delta") => {
                        text(delta, "thinking")?;
                        Ok(None)
                    }
                    (Some(Block::Thinking), "signature_delta") => {
                        text(delta, "signature")?;
                        Ok(None)
                    }
                    _ => Err(ProtocolError::UnsafeCapability),
                }
            }
            "content_block_stop" => {
                self.in_message()?;
                if self.block.take().is_none() || integer(event, "index")? != self.next_block {
                    return Err(ProtocolError::SequenceMismatch);
                }
                self.next_block += 1;
                Ok(None)
            }
            "message_delta" => {
                self.in_message()?;
                if self.block.is_some() {
                    return Err(ProtocolError::SequenceMismatch);
                }
                let delta = object(event.get("delta").ok_or(ProtocolError::InvalidShape)?)?;
                validate_stop_reason(delta)?;
                Ok(None)
            }
            "message_stop" => {
                self.in_message()?;
                if self.block.is_some() {
                    return Err(ProtocolError::SequenceMismatch);
                }
                self.stopped = true;
                Ok(None)
            }
            "ping" => Ok(None),
            _ => Err(ProtocolError::UnsupportedEvent),
        }
    }

    fn in_message(&self) -> Result<(), ProtocolError> {
        if self.message.is_none() || self.stopped {
            Err(ProtocolError::SequenceMismatch)
        } else {
            Ok(())
        }
    }

    fn assistant(&mut self, value: &Map<String, Value>) -> Result<(), ProtocolError> {
        let message = object(value.get("message").ok_or(ProtocolError::InvalidShape)?)?;
        if text(message, "role")? != "assistant" {
            return Err(ProtocolError::InvalidShape);
        }
        // The original CLI emits one summary per nonempty content block, BEFORE
        // content_block_stop. Thinking and text therefore have separate summaries
        // with the same message ID. Summaries are validated but never re-emitted.
        if let Some(id) = self.message.as_deref() {
            if text(message, "id")? != id
                || self.block.is_none()
                || self.stopped
                || self.summary_block == Some(self.next_block)
            {
                return Err(ProtocolError::SequenceMismatch);
            }
            self.summary_block = Some(self.next_block);
        } else {
            // Early API failure may supply only an assistant diagnostic and a
            // terminal error. No raw diagnostic is exposed by this method.
            if self.unstreamed_summary_seen {
                return Err(ProtocolError::SequenceMismatch);
            }
            self.unstreamed_summary_seen = true;
        }
        let blocks = message
            .get("content")
            .and_then(Value::as_array)
            .ok_or(ProtocolError::InvalidShape)?;
        if self.block.is_some() && blocks.len() != 1 {
            return Err(ProtocolError::SequenceMismatch);
        }
        for block in blocks {
            let block = object(block)?;
            match text(block, "type")? {
                "text" if self.block.is_none_or(|block| block == Block::Text) => {
                    safe_text(text(block, "text")?)?;
                }
                "thinking" | "redacted_thinking"
                    if self.block.is_none_or(|block| block == Block::Thinking) => {}
                _ => return Err(ProtocolError::UnsafeCapability),
            }
        }
        validate_stop_reason(message)?;
        Ok(())
    }

    fn result(&self, value: &Map<String, Value>) -> Result<TurnOutcome, ProtocolError> {
        if self.block.is_some() || (self.message.is_some() && !self.stopped) {
            return Err(ProtocolError::Incomplete);
        }
        absent_or_null(value, "deferred_tool_use")?;
        absent_or_null(value, "structured_output")?;
        empty_array(value, "permission_denials", false)?;
        let is_error = value
            .get("is_error")
            .and_then(Value::as_bool)
            .ok_or(ProtocolError::InvalidShape)?;
        let turns = integer(value, "num_turns")?;
        if turns > 1 {
            return Err(ProtocolError::InvalidTerminal);
        }
        let subtype = text(value, "subtype")?;
        match subtype {
            "success" => {}
            "error_during_execution"
            | "error_max_turns"
            | "error_max_budget_usd"
            | "error_max_structured_output_retries"
                if is_error => {}
            _ => return Err(ProtocolError::InvalidTerminal),
        }
        let stop = validate_stop_reason(value)?;
        let terminal = optional_text(value, "terminal_reason")?;
        let api_status = optional_u64(value, "api_error_status")?
            .map(|code| {
                if (400..=599).contains(&code) {
                    Ok(code as u16)
                } else {
                    Err(ProtocolError::InvalidTerminal)
                }
            })
            .transpose()?;
        if api_status.is_some() && !is_error {
            return Err(ProtocolError::InvalidTerminal);
        }
        let completion = match (subtype, is_error, stop, terminal) {
            (_, _, _, Some("aborted_streaming" | "aborted_tools")) => Completion::Interrupted,
            (_, _, Some("refusal"), _) if api_status.is_none() => Completion::Refused,
            ("success", true, _, _) => Completion::ProviderError,
            ("error_during_execution", true, _, _) => Completion::ProviderError,
            (
                "error_max_turns" | "error_max_budget_usd" | "error_max_structured_output_retries",
                true,
                _,
                _,
            ) => Completion::LimitReached,
            ("success", false, Some("max_tokens"), _)
            | ("success", false, _, Some("max_turns")) => Completion::LimitReached,
            (
                "success",
                false,
                None | Some("end_turn" | "stop_sequence"),
                None | Some("completed"),
            ) if turns == 1 => Completion::Succeeded,
            _ => return Err(ProtocolError::InvalidTerminal),
        };
        // Unknown terminal reasons are not silently converted to provider errors.
        if !matches!(
            terminal,
            None | Some("completed" | "max_turns" | "aborted_streaming" | "aborted_tools")
        ) {
            return Err(ProtocolError::InvalidTerminal);
        }
        let result_text = optional_text(value, "result")?;
        let text = match (completion, result_text) {
            // Provider diagnostics can echo account details. They are not model
            // output and never cross the native presentation boundary.
            (Completion::ProviderError | Completion::Interrupted, _) => String::new(),
            (_, Some(text)) => safe_text(text)?,
            (Completion::Succeeded, None) => return Err(ProtocolError::InvalidShape),
            (_, None) => String::new(),
        };
        Ok(TurnOutcome {
            completion,
            text,
            api_error_status: api_status,
        })
    }
}

fn object(value: &Value) -> Result<&Map<String, Value>, ProtocolError> {
    value.as_object().ok_or(ProtocolError::InvalidShape)
}
fn text<'a>(value: &'a Map<String, Value>, key: &str) -> Result<&'a str, ProtocolError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ProtocolError::InvalidShape)
}
fn integer(value: &Map<String, Value>, key: &str) -> Result<u64, ProtocolError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(ProtocolError::InvalidShape)
}
fn optional_text<'a>(
    value: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, ProtocolError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(ProtocolError::InvalidShape),
    }
}
fn optional_u64(value: &Map<String, Value>, key: &str) -> Result<Option<u64>, ProtocolError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(ProtocolError::InvalidShape),
    }
}
fn absent_or_null(value: &Map<String, Value>, key: &str) -> Result<(), ProtocolError> {
    if value.get(key).is_none_or(Value::is_null) {
        Ok(())
    } else {
        Err(ProtocolError::UnsafeCapability)
    }
}
fn empty_array(value: &Map<String, Value>, key: &str, required: bool) -> Result<(), ProtocolError> {
    match value.get(key) {
        None if !required => Ok(()),
        Some(Value::Array(values)) if values.is_empty() => Ok(()),
        Some(Value::Array(_)) => Err(ProtocolError::UnsafeCapability),
        _ => Err(ProtocolError::InvalidShape),
    }
}
fn valid_id(id: &str) -> Result<(), ProtocolError> {
    if id.is_empty()
        || id.len() > MAX_ID_BYTES
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        Err(ProtocolError::InvalidShape)
    } else {
        Ok(())
    }
}
fn safe_text(text: &str) -> Result<String, ProtocolError> {
    // No terminal escape/OSC, NUL, bidi override or hidden directional isolate.
    // HTML-looking text is allowed only as inert text; the UI must not interpret it.
    if text.chars().any(|ch| {
        (ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }) {
        return Err(ProtocolError::InvalidShape);
    }
    Ok(text.to_owned())
}
fn validate_stop_reason(value: &Map<String, Value>) -> Result<Option<&str>, ProtocolError> {
    let stop = optional_text(value, "stop_reason")?;
    match stop {
        None | Some("end_turn" | "stop_sequence" | "max_tokens" | "refusal") => Ok(stop),
        Some("tool_use" | "pause_turn") => Err(ProtocolError::UnsafeCapability),
        _ => Err(ProtocolError::InvalidTerminal),
    }
}
fn rate_status(value: &str) -> Result<RateStatus, ProtocolError> {
    match value {
        "allowed" => Ok(RateStatus::Allowed),
        "allowed_warning" => Ok(RateStatus::Warning),
        "rejected" => Ok(RateStatus::Rejected),
        _ => Err(ProtocolError::InvalidShape),
    }
}
fn reset(value: &Map<String, Value>, key: &str) -> Result<Option<u64>, ProtocolError> {
    let reset = optional_u64(value, key)?;
    if reset.is_some_and(|value| value > MAX_RESET_SECONDS) {
        Err(ProtocolError::InvalidShape)
    } else {
        Ok(reset)
    }
}
fn rate_limit(value: &Map<String, Value>) -> Result<RateLimit, ProtocolError> {
    let value = object(
        value
            .get("rate_limit_info")
            .ok_or(ProtocolError::InvalidShape)?,
    )?;
    let window = match optional_text(value, "rateLimitType")? {
        None => None,
        Some("five_hour") => Some(RateWindow::FiveHour),
        Some("seven_day") => Some(RateWindow::SevenDay),
        Some("seven_day_opus") => Some(RateWindow::SevenDayOpus),
        Some("seven_day_sonnet") => Some(RateWindow::SevenDaySonnet),
        Some("overage") => Some(RateWindow::Overage),
        _ => return Err(ProtocolError::InvalidShape),
    };
    let utilization = match value.get("utilization") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_f64()
                .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
                .ok_or(ProtocolError::InvalidShape)?,
        ),
    };
    Ok(RateLimit {
        status: rate_status(text(value, "status")?)?,
        window,
        utilization,
        resets_at_seconds: reset(value, "resetsAt")?,
        overage_status: optional_text(value, "overageStatus")?
            .map(rate_status)
            .transpose()?,
        overage_resets_at_seconds: reset(value, "overageResetsAt")?,
    })
}
fn api_retry(value: &Map<String, Value>) -> Result<(), ProtocolError> {
    let attempt = integer(value, "attempt")?;
    let maximum = integer(value, "max_retries")?;
    if attempt == 0
        || attempt > maximum
        || maximum > 32
        || integer(value, "retry_delay_ms")? > 120_000
    {
        return Err(ProtocolError::InvalidShape);
    }
    if optional_u64(value, "error_status")?.is_some_and(|code| !(400..=599).contains(&code)) {
        return Err(ProtocolError::InvalidShape);
    }
    match text(value, "error")? {
        "authentication_failed"
        | "oauth_org_not_allowed"
        | "account_on_hold"
        | "billing_error"
        | "rate_limit"
        | "overloaded"
        | "invalid_request"
        | "model_not_found"
        | "server_error"
        | "max_output_tokens"
        | "unknown" => Ok(()),
        "cloud_credential_error" => Err(ProtocolError::UnsafeCapability),
        _ => Err(ProtocolError::InvalidShape),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn init() -> Value {
        json!({"type":"system","subtype":"init","session_id":"session-1","tools":[],"mcp_servers":[],"plugins":[],"apiKeySource":"none","permissionMode":"dontAsk"})
    }
    fn stream(event: Value) -> Value {
        json!({"type":"stream_event","session_id":"session-1","parent_tool_use_id":null,"event":event})
    }
    fn result() -> Value {
        json!({"type":"result","subtype":"success","session_id":"session-1","is_error":false,"num_turns":1,"result":"Hi 世界 🦀","stop_reason":"end_turn","terminal_reason":"completed"})
    }
    fn lines(values: &[Value]) -> Vec<u8> {
        values
            .iter()
            .map(|value| format!("{value}\n"))
            .collect::<String>()
            .into_bytes()
    }
    fn successful_stream() -> Vec<Value> {
        vec![
            init(),
            stream(
                json!({"type":"message_start","message":{"role":"assistant","id":"message-1","content":[]}}),
            ),
            stream(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            stream(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi 世界 🦀"}}),
            ),
            json!({"type":"assistant","session_id":"session-1","parent_tool_use_id":null,"message":{"role":"assistant","id":"message-1","content":[{"type":"text","text":"Hi 世界 🦀"}],"stop_reason":null}}),
            stream(json!({"type":"content_block_stop","index":0})),
            stream(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}})),
            stream(json!({"type":"message_stop"})),
            result(),
        ]
    }
    fn reject(values: &[Value], expected: ProtocolError) {
        let mut decoder = TurnDecoder::new();
        assert_eq!(decoder.push(&lines(values)), Err(expected));
        assert_eq!(
            decoder.push(&lines(&[init()])),
            Err(ProtocolError::Poisoned)
        );
        assert_eq!(decoder.finish(0), Err(ProtocolError::Poisoned));
    }

    #[test]
    fn all_byte_boundaries_preserve_utf8_and_only_text_deltas_are_emitted() {
        let input = lines(&successful_stream());
        for split in 0..=input.len() {
            let mut decoder = TurnDecoder::new();
            let mut output = decoder.push(&input[..split]).unwrap();
            output.extend(decoder.push(&input[split..]).unwrap());
            assert_eq!(output, vec![TurnEvent::Text("Hi 世界 🦀".into())]);
            let result = decoder.finish(0).unwrap();
            assert_eq!(result.completion, Completion::Succeeded);
            assert_eq!(result.text, "Hi 世界 🦀");
        }
        let mut decoder = TurnDecoder::new();
        for byte in input {
            decoder.push(&[byte]).unwrap();
        }
        assert_eq!(decoder.finish(0).unwrap().completion, Completion::Succeeded);
    }

    #[test]
    fn documented_per_block_summaries_precede_stops_without_repeating_text() {
        let base = successful_stream();
        let mut events = vec![
            base[0].clone(),
            base[1].clone(),
            stream(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            stream(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"synthetic-hidden-reasoning"}}),
            ),
            stream(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"synthetic-signature"}}),
            ),
            json!({"type":"assistant","session_id":"session-1","message":{"role":"assistant","id":"message-1","content":[{"type":"thinking","thinking":"synthetic-hidden-reasoning","signature":"synthetic-signature"}]}}),
            stream(json!({"type":"content_block_stop","index":0})),
        ];
        for event in &base[2..] {
            let mut event = event.clone();
            if event
                .get("event")
                .and_then(|event| event.get("index"))
                .is_some()
            {
                event["event"]["index"] = json!(1);
            }
            events.push(event);
        }
        let mut decoder = TurnDecoder::new();
        assert_eq!(
            decoder.push(&lines(&events)).unwrap(),
            vec![TurnEvent::Text("Hi 世界 🦀".into())]
        );
        assert_eq!(decoder.finish(0).unwrap().completion, Completion::Succeeded);
        let mut duplicate = base.clone();
        duplicate.insert(5, base[4].clone());
        reject(&duplicate, ProtocolError::SequenceMismatch);
        let mut late_summary = base.clone();
        let summary = late_summary.remove(4);
        late_summary.insert(7, summary);
        reject(&late_summary, ProtocolError::SequenceMismatch);
    }

    #[test]
    fn a_changed_user_message_uuid_cannot_replace_the_admitted_turn() {
        let mut events = successful_stream();
        events[1]["user_message_uuid"] = json!("user-turn-1");
        events[3]["user_message_uuid"] = json!("user-turn-2");
        reject(&events, ProtocolError::SequenceMismatch);
    }

    #[test]
    fn provider_error_diagnostics_and_unknown_fields_are_not_projected() {
        let sentinel = "synthetic-auth-secret";
        let mut result = result();
        result["is_error"] = json!(true);
        result["result"] = json!(sentinel);
        result["errors"] = json!([sentinel]);
        result["api_error_status"] = json!(401);
        let mut decoder = TurnDecoder::new();
        decoder.push(&lines(&[init(), result])).unwrap();
        let outcome = decoder.finish(0).unwrap();
        assert_eq!(outcome.completion, Completion::ProviderError);
        assert!(outcome.text.is_empty());
        assert!(!format!("{outcome:?}").contains(sentinel));
    }

    #[test]
    fn documented_api_retry_is_observational_but_cloud_and_unknown_modes_fail() {
        let retry = json!({"type":"system","subtype":"api_retry","session_id":"session-1","attempt":1,"max_retries":3,"retry_delay_ms":1000,"error_status":529,"error":"overloaded"});
        let mut decoder = TurnDecoder::new();
        assert!(decoder
            .push(&lines(&[init(), retry.clone()]))
            .unwrap()
            .is_empty());
        for (key, value, expected) in [
            ("attempt", json!(0), ProtocolError::InvalidShape),
            ("max_retries", json!(33), ProtocolError::InvalidShape),
            (
                "error",
                json!("cloud_credential_error"),
                ProtocolError::UnsafeCapability,
            ),
            ("error", json!("future_error"), ProtocolError::InvalidShape),
        ] {
            let mut bad = retry.clone();
            bad[key] = value;
            reject(&[init(), bad], expected);
        }
    }

    #[test]
    fn crlf_chunks_are_allowed_but_truncation_is_never_completed() {
        let input = String::from_utf8(lines(&successful_stream()))
            .unwrap()
            .replace('\n', "\r\n");
        let mut decoder = TurnDecoder::new();
        for byte in input.as_bytes() {
            decoder.push(&[*byte]).unwrap();
        }
        assert_eq!(decoder.finish(0).unwrap().completion, Completion::Succeeded);
        let input = lines(&successful_stream());
        for end in [0, input.len() / 2, input.len() - 1] {
            let mut decoder = TurnDecoder::new();
            decoder.push(&input[..end]).unwrap();
            assert_eq!(decoder.finish(0), Err(ProtocolError::Incomplete));
        }
    }

    #[test]
    fn aggregate_line_event_utf8_and_json_bounds_poison_the_decoder() {
        let mut decoder = TurnDecoder::new();
        assert_eq!(
            decoder.push(&vec![b'x'; MAX_LINE_BYTES + 1]),
            Err(ProtocolError::LimitExceeded)
        );
        assert!(decoder.line.is_empty());
        let mut decoder = TurnDecoder::new();
        assert_eq!(
            decoder.push(&vec![b' '; MAX_STDOUT_BYTES + 1]),
            Err(ProtocolError::LimitExceeded)
        );
        for input in [
            b"\xff\n".as_slice(),
            b"{} {}\n",
            b"\n",
            b"[]\n",
            b"null\n",
            b"{\"type\":\"system\",\"type\":\"result\"}\n",
        ] {
            let mut decoder = TurnDecoder::new();
            assert!(decoder.push(input).is_err());
            assert_eq!(decoder.push(b""), Err(ProtocolError::Poisoned));
        }
        let mut decoder = TurnDecoder::new();
        decoder.push(&lines(&[init()])).unwrap();
        let ping = lines(&[stream(json!({"type":"ping"}))]);
        for _ in 1..MAX_EVENTS {
            decoder.push(&ping).unwrap();
        }
        assert_eq!(decoder.push(&ping), Err(ProtocolError::LimitExceeded));
    }

    #[test]
    fn capability_startup_and_protocol_mismatches_are_closed() {
        for field in [
            "tools",
            "mcp_servers",
            "plugins",
            "plugin_errors",
            "mcp_server_errors",
        ] {
            let mut unsafe_init = init();
            unsafe_init[field] = json!(["not-allowed"]);
            reject(&[unsafe_init], ProtocolError::UnsafeCapability);
        }
        for field in ["apiKeySource", "permissionMode"] {
            let mut unsafe_init = init();
            unsafe_init[field] = json!("unreviewed");
            reject(&[unsafe_init], ProtocolError::UnsafeCapability);
        }
        for subtype in [
            "hook_started",
            "hook_progress",
            "hook_response",
            "plugin_install",
            "task_started",
            "task_notification",
        ] {
            reject(
                &[
                    init(),
                    json!({"type":"system","subtype":subtype,"session_id":"session-1"}),
                ],
                ProtocolError::UnsafeCapability,
            );
        }
        for kind in [
            "control_request",
            "control_response",
            "user",
            "conversation_reset",
        ] {
            reject(
                &[init(), json!({"type":kind,"session_id":"session-1"})],
                ProtocolError::UnsafeCapability,
            );
        }
        reject(
            &[
                init(),
                json!({"type":"future_event","session_id":"session-1"}),
            ],
            ProtocolError::UnsupportedEvent,
        );
        reject(&[init(), init()], ProtocolError::SequenceMismatch);
    }

    #[test]
    fn tool_blocks_in_partial_and_full_messages_are_rejected() {
        for kind in [
            "tool_use",
            "server_tool_use",
            "tool_result",
            "advisor_tool_result",
            "future_block",
        ] {
            let mut partial = successful_stream();
            partial[2]["event"]["content_block"]["type"] = json!(kind);
            reject(&partial, ProtocolError::UnsafeCapability);
            let mut full = successful_stream();
            full[4]["message"]["content"][0]["type"] = json!(kind);
            reject(&full, ProtocolError::UnsafeCapability);
        }
        let mut stream = successful_stream();
        stream[3]["event"]["delta"]["type"] = json!("input_json_delta");
        reject(&stream, ProtocolError::UnsafeCapability);
    }

    #[test]
    fn text_requires_valid_content_block_and_one_session() {
        let stream = successful_stream();
        reject(
            &[init(), stream[3].clone()],
            ProtocolError::SequenceMismatch,
        );
        let mut wrong_session = stream.clone();
        wrong_session[3]["session_id"] = json!("other");
        reject(&wrong_session, ProtocolError::SequenceMismatch);
        let mut wrong_block = stream.clone();
        wrong_block[3]["event"]["index"] = json!(1);
        reject(&wrong_block, ProtocolError::SequenceMismatch);
        let mut subagent = stream.clone();
        subagent[3]["parent_tool_use_id"] = json!("agent-tool");
        reject(&subagent, ProtocolError::UnsafeCapability);
        let mut injected = stream.clone();
        injected[8]["origin"] = json!({"kind":"task-notification"});
        reject(&injected, ProtocolError::UnsafeCapability);
        let mut reset = stream.clone();
        reset.insert(4, stream[1].clone());
        reject(&reset, ProtocolError::SequenceMismatch);
        let mut late_text = stream.clone();
        late_text.insert(8, stream[3].clone());
        reject(&late_text, ProtocolError::SequenceMismatch);
        let mut duplicate_result = stream.clone();
        duplicate_result.push(result());
        reject(&duplicate_result, ProtocolError::AfterTerminal);
        let mut after_terminal = stream.clone();
        after_terminal.push(stream[3].clone());
        reject(&after_terminal, ProtocolError::AfterTerminal);
    }

    #[test]
    fn terminal_success_requires_compatible_fields_and_process_exit() {
        let mut decoder = TurnDecoder::new();
        decoder.push(&lines(&successful_stream())).unwrap();
        assert_eq!(decoder.finish(1), Err(ProtocolError::ProcessFailed));
        let mut decoder = TurnDecoder::new();
        decoder.push(&lines(&successful_stream()[..8])).unwrap();
        assert_eq!(decoder.finish(0), Err(ProtocolError::Incomplete));
        for (field, value) in [
            ("is_error", json!("false")),
            ("num_turns", json!(2)),
            ("num_turns", json!(-1)),
            ("api_error_status", json!(429)),
            ("terminal_reason", json!("new_reason")),
            ("subtype", json!("new_subtype")),
        ] {
            let mut invalid = result();
            invalid[field] = value;
            let mut decoder = TurnDecoder::new();
            assert!(decoder.push(&lines(&[init(), invalid])).is_err());
        }
        for subtype in ["future_subtype", "error_during_execution"] {
            let mut bad = result();
            bad["subtype"] = json!(subtype);
            bad["stop_reason"] = json!("refusal");
            reject(&[init(), bad], ProtocolError::InvalidTerminal);
        }
        let mut structured = result();
        structured["structured_output"] = json!({"unexpected":"tool-shaped output"});
        reject(&[init(), structured], ProtocolError::UnsafeCapability);
        let mut deferred = result();
        deferred["deferred_tool_use"] = json!({"name":"Read"});
        reject(&[init(), deferred], ProtocolError::UnsafeCapability);
        let mut denied = result();
        denied["permission_denials"] = json!([{"tool_name":"Read"}]);
        reject(&[init(), denied], ProtocolError::UnsafeCapability);
    }

    #[test]
    fn errors_refusals_truncation_and_cancellation_are_not_quota() {
        for (is_error, stop, terminal, expected) in [
            (true, "end_turn", "completed", Completion::ProviderError),
            (true, "refusal", "completed", Completion::Refused),
            (false, "max_tokens", "completed", Completion::LimitReached),
            (false, "end_turn", "max_turns", Completion::LimitReached),
            (
                false,
                "end_turn",
                "aborted_streaming",
                Completion::Interrupted,
            ),
        ] {
            let mut result = result();
            result["is_error"] = json!(is_error);
            result["stop_reason"] = json!(stop);
            result["terminal_reason"] = json!(terminal);
            if expected == Completion::ProviderError {
                result["api_error_status"] = json!(429);
            }
            let mut decoder = TurnDecoder::new();
            decoder.push(&lines(&[init(), result])).unwrap();
            assert_eq!(decoder.finish(0).unwrap().completion, expected);
        }
    }

    #[test]
    fn terminal_controls_and_debug_output_do_not_expose_content() {
        for text in [
            "\u{1b}]8;;https://example.test\u{7}click",
            "\0",
            "\u{009b}",
            "\u{202e}",
            "\u{2066}",
        ] {
            let mut result = result();
            result["result"] = json!(text);
            reject(&[init(), result], ProtocolError::InvalidShape);
        }
        let sentinel = "synthetic-conversation-secret";
        let event = TurnEvent::Text(sentinel.into());
        assert!(!format!("{event:?}").contains(sentinel));
        let outcome = TurnOutcome {
            completion: Completion::Succeeded,
            text: sentinel.into(),
            api_error_status: None,
        };
        assert!(!format!("{outcome:?}").contains(sentinel));
        assert_eq!(
            safe_text("<script>inert text</script>\n\t").unwrap(),
            "<script>inert text</script>\n\t"
        );
    }

    #[test]
    fn status_binds_directory_exit_and_mode_without_retaining_identity() {
        let directory = r"C:\ZephiumTrial\account-1\claude";
        for (method, expected) in [
            ("claude.ai", AuthStatus::ClaudeAccount),
            ("api_key", AuthStatus::ApiCredential),
            ("api_key_helper", AuthStatus::ApiCredential),
            ("third_party", AuthStatus::ExternalProvider),
            ("oauth_token", AuthStatus::Unknown),
            ("future_mode", AuthStatus::Unknown),
        ] {
            let status = json!({"loggedIn":true,"authMethod":method,"configDirectory":directory,"email":"not-retained@example.test","token":"synthetic-not-retained"}).to_string();
            assert_eq!(parse_status(status.as_bytes(), 0, directory), Ok(expected));
            assert_eq!(
                parse_status(status.as_bytes(), 1, directory),
                Err(ProtocolError::InvalidTerminal)
            );
            assert_eq!(
                parse_status(status.as_bytes(), 0, "other-directory"),
                Err(ProtocolError::ConfigDirectoryMismatch)
            );
        }
        let status =
            json!({"loggedIn":false,"authMethod":"none","configDirectory":directory}).to_string();
        assert_eq!(
            parse_status(status.as_bytes(), 1, directory),
            Ok(AuthStatus::SignedOut)
        );
        assert_eq!(
            parse_status(status.as_bytes(), 0, directory),
            Err(ProtocolError::InvalidTerminal)
        );
        assert_eq!(
            parse_status(status.as_bytes(), 7, directory),
            Err(ProtocolError::InvalidTerminal)
        );
    }

    #[test]
    fn status_duplicate_keys_malformed_and_oversize_are_not_evidence() {
        for input in [
            br#"{"loggedIn":false,"loggedIn":true}"#.as_slice(),
            br#"{"ignored":{"token":"a","token":"b"}}"#,
            b"not-json",
        ] {
            assert_eq!(
                parse_status(input, 0, "private"),
                Err(ProtocolError::InvalidJson)
            );
        }
        assert_eq!(
            parse_status(&vec![b' '; MAX_STATUS_BYTES + 1], 0, "private"),
            Err(ProtocolError::LimitExceeded)
        );
        let status = json!({"loggedIn":true,"authMethod":"claude.ai","configDirectory":"private","apiProvider":"bedrock"}).to_string();
        assert_eq!(
            parse_status(status.as_bytes(), 0, "private"),
            Ok(AuthStatus::Unknown)
        );
        assert!(parse_status(
            br#"{"loggedIn":true,"authMethod":"claude.ai"}"#,
            0,
            "private"
        )
        .is_err());
    }

    #[test]
    fn quota_fraction_unknowns_and_overage_stay_distinct() {
        let rate = json!({"type":"rate_limit_event","session_id":"session-1","rate_limit_info":{"status":"allowed_warning","rateLimitType":"five_hour","utilization":0.75,"resetsAt":2000000000,"overageStatus":"rejected","overageResetsAt":2000000100}});
        let mut decoder = TurnDecoder::new();
        let events = decoder.push(&lines(&[init(), rate.clone()])).unwrap();
        assert_eq!(
            events,
            vec![TurnEvent::RateLimit(RateLimit {
                status: RateStatus::Warning,
                window: Some(RateWindow::FiveHour),
                utilization: Some(0.75),
                resets_at_seconds: Some(2000000000),
                overage_status: Some(RateStatus::Rejected),
                overage_resets_at_seconds: Some(2000000100)
            })]
        );
        let absent = json!({"type":"rate_limit_event","session_id":"session-1","rate_limit_info":{"status":"allowed"}});
        let events = decoder.push(&lines(&[absent])).unwrap();
        assert!(matches!(
            events.as_slice(),
            [TurnEvent::RateLimit(RateLimit {
                utilization: None,
                window: None,
                resets_at_seconds: None,
                ..
            })]
        ));
        for (key, value) in [
            ("utilization", json!(-0.1)),
            ("utilization", json!(1.001)),
            ("utilization", json!("0.5")),
            ("resetsAt", json!(-1)),
            ("resetsAt", json!(0.5)),
            ("resetsAt", json!(MAX_RESET_SECONDS + 1)),
            ("status", json!("future")),
            ("rateLimitType", json!("future")),
        ] {
            let mut bad = rate.clone();
            bad["rate_limit_info"][key] = value;
            reject(&[init(), bad], ProtocolError::InvalidShape);
        }
    }
}
