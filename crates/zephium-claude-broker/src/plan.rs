//! Closed process vocabulary. Plans carry no execution authority or credential data.
use std::{fmt, time::Duration};

/// Maximum text forwarded to the official client's stdin for one turn.
pub const MAX_STDIN_BYTES: usize = 256 * 1024;
/// Maximum stdout accumulated by the transport, before decoding.
pub const MAX_STDOUT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum stderr retained transiently by the transport; never JSON or a log.
pub const MAX_STDERR_BYTES: usize = 64 * 1024;

/// Only supported vendor operations; never a shell command or arbitrary argv.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Fixed JSON status of this broker's own configuration directory.
    ReadStatus,
    /// Official unmodified login ceremony, without forced account type.
    StartLogin,
    /// Official logout of this broker's own account only.
    Logout,
    /// One no-tools conversation turn; bounded text goes on stdin, not argv.
    TextTurn,
}

/// A process-free immutable command plan, not authorization to launch it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandPlan(Operation);
impl CommandPlan {
    /// Build the sole reviewed candidate argv for an operation.
    pub const fn for_operation(operation: Operation) -> Self {
        Self(operation)
    }
    /// Fixed arguments. The empty tools argument is a real argv entry.
    pub const fn arguments(self) -> &'static [&'static str] {
        match self.0 {
            Operation::ReadStatus => &["auth", "status"],
            Operation::StartLogin => &["auth", "login"],
            Operation::Logout => &["auth", "logout"],
            Operation::TextTurn => &[
                "--restricted",
                "--safe-mode",
                "-p",
                "--input-format",
                "text",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--tools",
                "",
                "--disallowedTools",
                "mcp__*",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
                "--setting-sources=",
                "--permission-mode",
                "dontAsk",
                "--permission-prompts",
                "none",
                "--no-chrome",
                "--no-session-persistence",
                "--max-turns",
                "1",
            ],
        }
    }
    /// Maximum operation duration. The future native owner must start this clock
    /// at reservation, before profile/image preparation; this plan starts no timer.
    pub const fn deadline(self) -> Duration {
        Duration::from_secs(match self.0 {
            Operation::ReadStatus | Operation::Logout => 10,
            Operation::StartLogin => 300,
            Operation::TextTurn => 120,
        })
    }
}

/// Invalid bounded input. Never embeds user text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputError {
    /// The text exceeds the byte limit.
    TooLarge,
    /// Empty input, NUL or a leading CLI slash command is not admitted.
    InvalidText,
}

/// Transient explicit user text; Debug intentionally never prints its content.
pub struct TurnInput(Vec<u8>);
impl TurnInput {
    /// Validate before copying into owned transport storage.
    pub fn new(text: &str) -> Result<Self, InputError> {
        if text.len() > MAX_STDIN_BYTES {
            return Err(InputError::TooLarge);
        }
        if text.is_empty() || text.contains('\0') || text.trim_start().starts_with('/') {
            return Err(InputError::InvalidText);
        }
        Ok(Self(text.as_bytes().to_vec()))
    }
    /// Bytes for the owning native stdin writer only, never command arguments.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for TurnInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TurnInput([REDACTED])")
    }
}
impl Drop for TurnInput {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

/// Quote exact Windows argv, including empty entries; no shell interpretation.
/// The executable must separately be a held, pinned native image.
pub fn windows_command_line(program: &str, arguments: &[&str]) -> Result<Vec<u16>, InputError> {
    let mut line = Vec::new();
    for value in std::iter::once(program).chain(arguments.iter().copied()) {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        line.push(b'"' as u16);
        let mut slashes = 0;
        for unit in value.encode_utf16() {
            if unit == 0 {
                return Err(InputError::InvalidText);
            }
            if unit == b'\\' as u16 {
                slashes += 1;
                if slashes >= 32767 {
                    return Err(InputError::TooLarge);
                }
                continue;
            }
            let quoted_slashes = if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            };
            if line.len().saturating_add(quoted_slashes + 1) >= 32767 {
                return Err(InputError::TooLarge);
            }
            line.extend(std::iter::repeat_n(b'\\' as u16, quoted_slashes));
            slashes = 0;
            line.push(unit);
            if line.len() >= 32767 {
                return Err(InputError::TooLarge);
            }
        }
        // Bound before adding a potentially large trailing run of slashes.
        if line.len().saturating_add(slashes * 2 + 1) >= 32767 {
            return Err(InputError::TooLarge);
        }
        line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        line.push(b'"' as u16);
    }
    line.push(0);
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_vocabulary_retains_subscription_auth_and_no_tools() {
        let args = CommandPlan::for_operation(Operation::TextTurn).arguments();
        assert!(args.windows(2).any(|pair| pair == ["--tools", ""]));
        for forbidden in [
            "--bare",
            "--dangerously-skip-permissions",
            "--resume",
            "--plugin-dir",
            "--add-dir",
            "--ide",
        ] {
            assert!(!args.contains(&forbidden));
        }
        assert!(args.contains(&"--safe-mode") && args.contains(&"--restricted"));
        assert_eq!(
            CommandPlan::for_operation(Operation::StartLogin).arguments(),
            ["auth", "login"]
        );
        assert_eq!(
            CommandPlan::for_operation(Operation::ReadStatus).arguments(),
            ["auth", "status"]
        );
        assert_eq!(
            CommandPlan::for_operation(Operation::Logout).arguments(),
            ["auth", "logout"]
        );
    }
    #[test]
    fn stdin_is_bounded_and_redacted() {
        assert_eq!(
            TurnInput::new(&"x".repeat(MAX_STDIN_BYTES + 1)).unwrap_err(),
            InputError::TooLarge
        );
        assert_eq!(TurnInput::new("x\0y").unwrap_err(), InputError::InvalidText);
        assert_eq!(
            TurnInput::new("  /login").unwrap_err(),
            InputError::InvalidText
        );
        let input = TurnInput::new("private prompt --flag").unwrap();
        assert!(!format!("{input:?}").contains("private"));
        assert_eq!(input.as_bytes(), b"private prompt --flag");
    }
    #[test]
    fn windows_empty_quote_and_trailing_slashes_are_exact() {
        let line = windows_command_line(
            r"C:\private folder\claude.exe",
            &["--tools", "", "a\"b", "end\\"],
        )
        .unwrap();
        assert_eq!(
            String::from_utf16(&line[..line.len() - 1]).unwrap(),
            "\"C:\\private folder\\claude.exe\" \"--tools\" \"\" \"a\\\"b\" \"end\\\\\""
        );
        assert_eq!(
            windows_command_line("a\0b", &[]),
            Err(InputError::InvalidText)
        );
        assert_eq!(
            windows_command_line("claude.exe", &[&format!("{}a", "\\".repeat(1_000_000))]),
            Err(InputError::TooLarge)
        );
        assert_eq!(
            windows_command_line("claude.exe", &[&"\\".repeat(32768)]),
            Err(InputError::TooLarge)
        );
    }
}
