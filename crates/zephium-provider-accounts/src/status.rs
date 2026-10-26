//! Status-only parsing: no credentials, raw errors, account emails or plans escape.

use serde_json::Value;

/// Maximum JSON/text status bytes admitted from an already-qualified CLI.
pub const MAX_STATUS_BYTES: usize = 16 * 1024;

/// Authentication evidence; none of these states grants model execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountStatus {
    /// The official client explicitly reports no authenticated account.
    SignedOut,
    /// Codex reports ChatGPT-managed authentication, not API billing.
    ChatGpt,
    /// Original Claude Code reports Claude.ai-managed authentication.
    Claude,
    /// A separately billed API key or key helper, not a consumer subscription.
    ApiCredential,
    /// An external provider; do not claim a consumer subscription.
    ExternalProvider,
    /// No trustworthy status evidence, including a failed or changed protocol.
    Unknown,
}

/// Parse `codex login status` without retaining an echoed key or account details.
/// Unknown output is never interpreted as proof of being signed out.
pub fn codex_status(success: bool, output: &str) -> AccountStatus {
    if output.len() > MAX_STATUS_BYTES {
        return AccountStatus::Unknown;
    }
    match (success, output.trim()) {
        (true, "Logged in using ChatGPT") => AccountStatus::ChatGpt,
        (true, "Logged in using an API key") => AccountStatus::ApiCredential,
        (false, "Not logged in") => AccountStatus::SignedOut,
        _ => AccountStatus::Unknown,
    }
}

/// Parse the official `claude auth status` JSON and exit code together.
/// OAuth tokens supplied by another app are deliberately not subscription proof.
pub fn claude_status(success: bool, output: &str) -> AccountStatus {
    if output.len() > MAX_STATUS_BYTES {
        return AccountStatus::Unknown;
    }
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return AccountStatus::Unknown;
    };
    match (success, value.get("loggedIn").and_then(Value::as_bool)) {
        (_, Some(false)) => AccountStatus::SignedOut,
        (true, Some(true)) => match value.get("authMethod").and_then(Value::as_str) {
            Some("claude.ai")
                if value
                    .get("apiProvider")
                    .is_none_or(|provider| provider == "firstParty") =>
            {
                AccountStatus::Claude
            }
            Some("api_key" | "api_key_helper") => AccountStatus::ApiCredential,
            Some("third_party") => AccountStatus::ExternalProvider,
            _ => AccountStatus::Unknown,
        },
        _ => AccountStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_requires_exact_status_and_exit_evidence() {
        assert_eq!(
            codex_status(true, "Logged in using ChatGPT\n"),
            AccountStatus::ChatGpt
        );
        assert_eq!(
            codex_status(true, "Logged in using an API key"),
            AccountStatus::ApiCredential
        );
        assert_eq!(
            codex_status(false, "Not logged in"),
            AccountStatus::SignedOut
        );
        for output in [
            "warning: Logged in using ChatGPT",
            "Not logged in",
            "",
            "Logged in using ChatGPT\nextra",
        ] {
            assert_eq!(codex_status(true, output), AccountStatus::Unknown);
        }
        assert_eq!(
            codex_status(false, "Logged in using ChatGPT"),
            AccountStatus::Unknown
        );
    }

    #[test]
    fn claude_subscription_and_api_auth_are_not_interchangeable() {
        for (method, expected) in [
            ("claude.ai", AccountStatus::Claude),
            ("api_key", AccountStatus::ApiCredential),
            ("api_key_helper", AccountStatus::ApiCredential),
            ("third_party", AccountStatus::ExternalProvider),
            ("oauth_token", AccountStatus::Unknown),
            ("none", AccountStatus::Unknown),
            ("new_method", AccountStatus::Unknown),
        ] {
            let output = serde_json::json!({"loggedIn": true, "authMethod": method, "email": "not-retained@example.test", "token": "not-retained"}).to_string();
            assert_eq!(claude_status(true, &output), expected);
            assert_eq!(claude_status(false, &output), AccountStatus::Unknown);
        }
        assert_eq!(
            claude_status(false, r#"{"loggedIn":false}"#),
            AccountStatus::SignedOut
        );
        assert_eq!(
            claude_status(
                true,
                r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"thirdParty"}"#
            ),
            AccountStatus::Unknown
        );
    }

    #[test]
    fn malformed_or_oversized_output_is_not_authentication_evidence() {
        for output in [
            "{}",
            "null",
            "[]",
            r#"{"loggedIn":"true","authMethod":"claude.ai"}"#,
            "not-json",
        ] {
            assert_eq!(claude_status(true, output), AccountStatus::Unknown);
        }
        let oversized = " ".repeat(MAX_STATUS_BYTES + 1);
        assert_eq!(claude_status(true, &oversized), AccountStatus::Unknown);
        assert_eq!(codex_status(true, &oversized), AccountStatus::Unknown);
    }
}
