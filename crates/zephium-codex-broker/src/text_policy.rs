//! Native text-only admission for original Codex 0.164.0.
//!
//! This is a request-data builder, not proof of an installed thread policy.
//! The native actor must install `fixed_config()` at thread/start or a genuinely
//! cold same-ID thread/resume, validate the effective no-executor boundary, and
//! only then consume the input. Unsubscribe ACK alone is not a cold barrier.
//! No renderer-selected config, paths, structured skills, or tool inputs enter.
//!
//! Prospective plain skill names are disabled, not removed from the text. This
//! preserves currency/code/escaped text even if a same-named host skill appears
//! later. Exact rules apply to all matching skill paths, not a stale catalog.
//! Explicit resource links are unsupported: their display name need not match
//! the selected skill's name or its canonical/discovery alias. Ordinary paths,
//! SKILL.md discussion and Markdown links without the mention syntax are text.
//!
//! Source: openai/codex@6aaac694803004a63c6f393d153c96d22521a6b9,
//! skills/src/{mentions,selection}.rs and config/src/skills_config.rs:99–219.
//! The adapted lexer and its Apache-2.0 attribution live in text_policy/.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::config::{CONFIG_REQUIRED_FALSE_PATHS, CONFIG_REQUIRED_STRING_PAIRS};

#[path = "text_policy/mentions.rs"]
mod mentions;

pub(crate) const MAX_TEXT_BYTES: usize = 32 * 1024;
pub(crate) const MAX_SKILL_NAMES: usize = 256;
pub(crate) const MAX_CONFIG_BYTES: usize = 64 * 1024;

/// Closed errors only: never include text, resource paths, or parsed names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextPolicyError {
    TextTooLarge,
    TooManySkillNames,
    UnsupportedResourceMention,
    ConfigTooLarge,
}

/// Deliberately non-Clone and without Debug/Serialize. The actor retains this
/// exact text/config pair across cold thread admission before consuming it.
pub(crate) struct PreparedText {
    text: String,
    config: Value,
}

pub(crate) fn prepare_text(text: String) -> Result<PreparedText, TextPolicyError> {
    prepare_with_limit(text, MAX_TEXT_BYTES)
}
fn prepare_with_limit(text: String, maximum: usize) -> Result<PreparedText, TextPolicyError> {
    if text.len() > maximum {
        return Err(TextPolicyError::TextTooLarge);
    }
    let parsed = mentions::extract_tool_mentions(&text);
    if !parsed.paths.is_empty() {
        return Err(TextPolicyError::UnsupportedResourceMention);
    }
    if parsed.plain_names.len() > MAX_SKILL_NAMES {
        return Err(TextPolicyError::TooManySkillNames);
    }
    // Sorting stabilizes the exact native override, not the user's text.
    let names = parsed.plain_names.into_iter().collect::<BTreeSet<_>>();
    let config = bounded_fixed_config(names)?;
    Ok(PreparedText { text, config })
}

impl PreparedText {
    /// Only native fixed settings and exact name=false rules. Callers must not
    /// merge a renderer map or add later enable=true rules. Request overrides
    /// become SessionFlags in the pinned config manager (547–576).
    pub(crate) fn fixed_config(&self) -> Value {
        self.config.clone()
    }

    /// A single text input, unchanged byte-for-byte, with no UI-defined spans.
    /// `text_elements` is the pinned UserInput::Text field spelling; this API
    /// cannot construct Skill, Mention, image, audio, tool output, or metadata.
    pub(crate) fn into_turn_input(self) -> Value {
        json!([{"type": "text", "text": self.text, "text_elements": []}])
    }
}

fn bounded_fixed_config<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Result<Value, TextPolicyError> {
    let mut config = Map::new();
    for &(path, value) in CONFIG_REQUIRED_STRING_PAIRS {
        config.insert(dotted_key(path), Value::String(value.to_owned()));
    }
    for path in CONFIG_REQUIRED_FALSE_PATHS {
        config.insert(dotted_key(path), Value::Bool(false));
    }
    config.insert("notify".into(), json!([]));
    config.insert("mcp_servers".into(), json!({}));
    // Supported zero-delay idle unload. It must be applied on every cold
    // resume: the previous runtime's override is not persisted by Codex.
    config.insert("thread_unload_delay_secs".into(), json!(0));
    config.insert(
        "skills.config".into(),
        Value::Array(
            names
                .into_iter()
                .map(|name| json!({"name": name, "enabled": false}))
                .collect(),
        ),
    );
    let config = Value::Object(config);
    let encoded = serde_json::to_vec(&config).map_err(|_| TextPolicyError::ConfigTooLarge)?;
    if encoded.len() > MAX_CONFIG_BYTES {
        return Err(TextPolicyError::ConfigTooLarge);
    }
    Ok(config)
}

fn dotted_key(path: &str) -> String {
    path.trim_start_matches('/').replace('/', ".")
}

#[cfg(test)]
#[path = "text_policy/tests.rs"]
mod tests;

/// Separate browser-composed-input type. Same original mention lexer and name
/// denial policy, over the COMPLETE admitted input, with no truncation. This
/// remains closed/test-only until the Page browser actor consumes this type.
pub(crate) struct PreparedBrowserText(PreparedText);
pub(crate) fn prepare_browser_text(text: String) -> Result<PreparedBrowserText, TextPolicyError> {
    prepare_with_limit(text, 128 * 1024).map(PreparedBrowserText)
}
impl PreparedBrowserText {
    pub(crate) fn into_validated_text(self) -> PreparedText {
        self.0
    }

    pub(crate) fn text(&self) -> &str {
        &self.0.text
    }
    #[cfg(test)]
    pub(crate) fn fixed_config(&self) -> Value {
        self.0.fixed_config()
    }
    #[cfg(test)]
    pub(crate) fn into_turn_input(self) -> Value {
        self.0.into_turn_input()
    }
}
#[cfg(test)]
mod browser_tests {
    use super::*;
    #[test]
    fn validated_browser_text_moves_into_fixed_command_without_reclassification() {
        let prepared = prepare_browser_text("complete $context_skill".into())
            .unwrap()
            .into_validated_text();
        assert_eq!(
            prepared.fixed_config()["skills.config"],
            serde_json::json!([{"name":"context_skill","enabled":false}])
        );
        assert_eq!(
            prepared.into_turn_input()[0]["text"],
            "complete $context_skill"
        );
    }
    #[test]
    fn composed_input_beyond_plain_limit_is_whole_and_plain_api_unchanged() {
        let text = format!(
            "{}\n<readout>$context_skill</readout>\nUser: hello",
            "x".repeat(40 * 1024)
        );
        assert!(matches!(
            prepare_text(text.clone()),
            Err(TextPolicyError::TextTooLarge)
        ));
        let prepared = prepare_browser_text(text.clone()).unwrap();
        assert_eq!(
            prepared.fixed_config()["skills.config"],
            json!([{"name":"context_skill","enabled":false}])
        );
        assert_eq!(
            prepared.into_turn_input(),
            json!([{"type":"text","text":text,"text_elements":[]}])
        );
    }
    #[test]
    fn resources_in_context_not_only_user_text_remain_refused() {
        let text = format!(
            "{}\n<readout>[$context](file:///owned/SKILL.md)</readout>\nUser: hello",
            "x".repeat(40 * 1024)
        );
        assert!(matches!(
            prepare_browser_text(text),
            Err(TextPolicyError::UnsupportedResourceMention)
        ));
    }
    #[test]
    fn composed_total_and_name_count_are_refused_not_truncated() {
        assert!(matches!(
            prepare_browser_text("x".repeat(128 * 1024 + 1)),
            Err(TextPolicyError::TextTooLarge)
        ));
        assert!(prepare_browser_text("x".repeat(128 * 1024)).is_ok());
        let text = (0..257).map(|n| format!("$name_{n} ")).collect::<String>();
        assert!(matches!(
            prepare_browser_text(text),
            Err(TextPolicyError::TooManySkillNames)
        ));
    }
}
