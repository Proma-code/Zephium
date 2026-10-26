// Derived from OpenAI Codex, Copyright 2025 OpenAI, Apache-2.0.
// Source: https://github.com/openai/codex/blob/6aaac694803004a63c6f393d153c96d22521a6b9/codex-rs/skills/src/mentions.rs
// Upstream Git blob: 1f391235d1ede003f6edfc8c93fddc2925650dbe.
// See LICENSE-APACHE-2.0 and NOTICE in this directory.
// Modifications: retain only plain-name/path extraction used by this host;
// remove unused tool-kind/name aggregation and helpers; narrow visibility;
// express the same let-chain conditions using Rust 2021-compatible syntax.
// The lexical scanning and common-environment-name behavior are unchanged.

use std::collections::HashSet;

pub(super) struct ToolMentions<'a> {
    pub(super) paths: HashSet<&'a str>,
    pub(super) plain_names: HashSet<&'a str>,
}

const TOOL_MENTION_SIGIL: char = '$';

pub(super) fn extract_tool_mentions(text: &str) -> ToolMentions<'_> {
    extract_tool_mentions_with_sigil(text, TOOL_MENTION_SIGIL)
}

fn extract_tool_mentions_with_sigil(text: &str, sigil: char) -> ToolMentions<'_> {
    let text_bytes = text.as_bytes();
    let mut mentioned_paths: HashSet<&str> = HashSet::new();
    let mut plain_names: HashSet<&str> = HashSet::new();

    let mut index = 0;
    while index < text_bytes.len() {
        let byte = text_bytes[index];
        if byte == b'[' {
            if let Some((name, path, end_index)) =
                parse_linked_tool_mention(text, text_bytes, index, sigil)
            {
                if !is_common_env_var(name) {
                    mentioned_paths.insert(path);
                }
                index = end_index;
                continue;
            }
        }

        if byte != sigil as u8 {
            index += 1;
            continue;
        }

        let name_start = index + 1;
        let Some(first_name_byte) = text_bytes.get(name_start) else {
            index += 1;
            continue;
        };
        if !is_mention_name_char(*first_name_byte) {
            index += 1;
            continue;
        }

        let mut name_end = name_start + 1;
        while text_bytes
            .get(name_end)
            .is_some_and(|next_byte| is_mention_name_char(*next_byte))
        {
            name_end += 1;
        }

        let name = &text[name_start..name_end];
        if !is_common_env_var(name) {
            plain_names.insert(name);
        }
        index = name_end;
    }

    ToolMentions {
        paths: mentioned_paths,
        plain_names,
    }
}

fn parse_linked_tool_mention<'a>(
    text: &'a str,
    text_bytes: &[u8],
    start: usize,
    sigil: char,
) -> Option<(&'a str, &'a str, usize)> {
    let sigil_index = start + 1;
    if text_bytes.get(sigil_index) != Some(&(sigil as u8)) {
        return None;
    }

    let name_start = sigil_index + 1;
    let first_name_byte = text_bytes.get(name_start)?;
    if !is_mention_name_char(*first_name_byte) {
        return None;
    }

    let mut name_end = name_start + 1;
    while text_bytes
        .get(name_end)
        .is_some_and(|next_byte| is_mention_name_char(*next_byte))
    {
        name_end += 1;
    }

    if text_bytes.get(name_end) != Some(&b']') {
        return None;
    }

    let mut path_start = name_end + 1;
    while text_bytes
        .get(path_start)
        .is_some_and(u8::is_ascii_whitespace)
    {
        path_start += 1;
    }
    if text_bytes.get(path_start) != Some(&b'(') {
        return None;
    }

    let mut path_end = path_start + 1;
    while text_bytes
        .get(path_end)
        .is_some_and(|next_byte| *next_byte != b')')
    {
        path_end += 1;
    }
    if text_bytes.get(path_end) != Some(&b')') {
        return None;
    }

    let path = text[path_start + 1..path_end].trim();
    if path.is_empty() {
        return None;
    }

    let name = &text[name_start..name_end];
    Some((name, path, path_end + 1))
}

fn is_common_env_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "SHELL"
            | "PWD"
            | "TMPDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "TERM"
            | "XDG_CONFIG_HOME"
    )
}

fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b':')
}
