//! Fixed credential-free help receipts, never authentication/status output.
#[derive(Clone, Copy)]
pub(crate) enum Topic {
    General,
    Login,
    Status,
    Logout,
}

pub(crate) fn valid(topic: Topic, bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > 32 * 1024 || bytes.contains(&0) || bytes.contains(&27) {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let usage = match topic {
        Topic::General => first.starts_with("Usage: claude [options]"),
        Topic::Login => first == "Usage: claude auth login [options]",
        Topic::Status => first == "Usage: claude auth status [options]",
        Topic::Logout => first == "Usage: claude auth logout [options]",
    };
    if !usage || !text.lines().any(|line| line.trim() == "Options:") || !has_option(bytes, "--help")
    {
        return false;
    }
    if !matches!(topic, Topic::General) {
        let lower = text.to_ascii_lowercase();
        if lower.contains("://")
            || lower.contains("code_verifier")
            || lower.contains("state=")
            || lower.lines().map(str::trim).any(|line| {
                line.starts_with(['{', '['])
                    || line.starts_with("paste ")
                    || line.starts_with("enter your ")
                    || line.starts_with("opening browser")
                    || line.starts_with("waiting for authentication")
            })
        {
            return false;
        }
    }
    true
}

// Only callers' fixed known option names are reported, as booleans. Absence is
// not proof of unsupported behavior: the vendor documents some flags as hidden.
pub(crate) fn has_option(bytes: &[u8], flag: &str) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    for line in text.lines().map(str::trim_start) {
        // A whitespace-separated continuation is another declaration only when
        // the preceding alias ended in a comma (for example `-h, --help`).
        // Stop before a parameter or description, even if prose mentions flags.
        for token in line.split_whitespace().take(4) {
            if !token.starts_with('-') {
                break;
            }
            if token.split(',').any(|alias| alias == flag) {
                return true;
            }
            if !token.ends_with(',') {
                break;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_auth_help_requires_its_exact_usage_and_options() {
        for (topic, command) in [
            (Topic::Login, "login"),
            (Topic::Status, "status"),
            (Topic::Logout, "logout"),
        ] {
            let text = format!("Usage: claude auth {command} [options]\n\nOptions:\n  -h, --help  display help for command\n");
            assert!(valid(topic, text.as_bytes()));
            assert!(!valid(
                topic,
                b"Usage: claude [options]\nOptions:\n --help help"
            ));
            assert!(!valid(topic, b"Successfully logged out"));
            assert!(!valid(topic, b"{\"loggedIn\":true}"));
        }
    }
    #[test]
    fn unexpected_flow_or_unbounded_content_is_not_help() {
        let base = "Usage: claude auth login [options]\nOptions:\n --help help\n";
        for extra in [
            "https://claude.ai/oauth/authorize?state=synthetic",
            "Paste your authorization code:",
            "Enter your code:",
            "Opening browser...",
            "{\"loggedIn\":true}",
            "\0",
            "\u{1b}[31m",
        ] {
            assert!(!valid(Topic::Login, format!("{base}{extra}").as_bytes()));
        }
        assert!(!valid(Topic::Login, &vec![b'x'; 32769]));
        assert!(!valid(Topic::Login, &[255]));
        assert!(!valid(Topic::Login, b""));
    }
    #[test]
    fn known_option_metadata_does_not_match_substrings_or_prose() {
        let bytes = b"Usage: claude auth login [options]\nOptions:\n --console  API billing\n --code-verifier <text> example\n -h, --help help\n prose --no-browser description\n";
        assert!(has_option(bytes, "--console"));
        assert!(!has_option(bytes, "--code"));
        assert!(!has_option(bytes, "--no-browser"));
        assert!(has_option(bytes, "--help"));
        for description in [
            b"  --other See --console".as_slice(),
            b"  --other --console example",
            b"  --other <arg> --console",
            b"  --other, prose --console",
        ] {
            assert!(!has_option(description, "--console"));
        }
        assert!(has_option(b" -h,--help display help", "--help"));
        assert!(!valid(
            Topic::Login,
            b"Usage: claude auth login [options]\nOptions:\n --other See --help",
        ));
    }
    #[test]
    fn global_help_shape_is_not_any_nonempty_output() {
        assert!(valid(
            Topic::General,
            b"Usage: claude [options] [command] [prompt]\nOptions:\n -h, --help help\n"
        ));
        assert!(!valid(Topic::General, b"unexpected output"));
        assert!(!valid(Topic::General, b"Usage: claude [options]\n"));
    }
}
