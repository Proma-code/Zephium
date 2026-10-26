// The upstream_lexical_parity_corpus cases are adapted from OpenAI Codex,
// Copyright 2025 OpenAI, Apache-2.0; see this directory's LICENSE and NOTICE.
// Source: openai/codex@6aaac694803004a63c6f393d153c96d22521a6b9,
// codex-rs/skills/src/mentions_tests.rs. Other tests exercise this host policy.
use super::*;

fn names(text: &str) -> Vec<String> {
    let prepared = prepare_text(text.to_owned()).unwrap();
    prepared.fixed_config()["skills.config"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule["name"].as_str().unwrap().to_owned())
        .collect()
}

fn assert_refused(text: &str, expected: TextPolicyError) {
    match prepare_text(text.to_owned()) {
        Err(error) => assert_eq!(error, expected),
        Ok(_) => panic!("expected a closed text policy refusal"),
    }
}

#[test]
fn ordinary_text_currency_paths_and_markdown_are_not_rewritten_or_rejected() {
    for original in [
        "مرحبا، هل يمكنك المساعدة؟",
        "Budget $50000 and $5.25, $ 7, $$ and \\$20. Keep all punctuation.",
        "Discuss SKILL.md at C:\\docs\\SKILL.md, /tmp/SKILL.md and skill://example.",
        "[ordinary link](https://example.test/SKILL.md) and [guide](/tmp/skill)",
        "Unicode 🦀\n\t`$code` and \\$escaped stay exactly as entered.\r\n",
        "",
    ] {
        let prepared = prepare_text(original.to_owned()).unwrap();
        assert_eq!(
            prepared.into_turn_input(),
            json!([{"type":"text", "text":original, "text_elements":[]}])
        );
    }
    assert_eq!(names("$50000 $5.25 \\$20"), ["20", "5", "50000"]);
}

#[test]
fn names_are_exact_sorted_unique_and_disabled_as_data() {
    let prepared = prepare_text("$Beta $alpha $Beta $slack:search $beta_extra".into()).unwrap();
    assert_eq!(
        prepared.fixed_config()["skills.config"],
        json!([
            {"name":"Beta","enabled":false},
            {"name":"alpha","enabled":false},
            {"name":"beta_extra","enabled":false},
            {"name":"slack:search","enabled":false}
        ])
    );
    assert_eq!(names("$Same $same"), ["Same", "same"]);
    assert_eq!(names("$a\"; shell_tool=true"), ["a"]);
}

#[test]
fn no_escape_code_or_word_boundary_exemptions_are_invented() {
    assert_eq!(
        names("\\$escape `$inline` ```$fenced``` prefix$attached $5 $_x $-x $:x"),
        ["-x", "5", ":x", "_x", "attached", "escape", "fenced", "inline"]
    );
    assert_eq!(names("$café $中 $alpha.skill"), ["alpha", "caf"]);
}

#[test]
fn exact_common_environment_names_are_excluded_case_insensitively() {
    for name in [
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "PWD",
        "TMPDIR",
        "TEMP",
        "TMP",
        "LANG",
        "TERM",
        "XDG_CONFIG_HOME",
    ] {
        assert!(names(&format!("${name} ${}", name.to_ascii_lowercase())).is_empty());
        assert!(names(&format!("[${name}](skill://not-imported)")).is_empty());
    }
    assert_eq!(names("$PATH_more $HOMES"), ["HOMES", "PATH_more"]);
}

#[test]
fn explicit_resource_mentions_refuse_without_claiming_catalog_resolution() {
    for text in [
        "[$display](/different/name/SKILL.md)",
        "[$display](C:\\alias\\not-the-filename)",
        "[$display](skill://logical/discovery/alias)",
        "[$display](relative-resource)",
        "[$display](file:///C:/canonical/SKILL.md)",
        "[$display](app://browser)",
        "[$display](mcp://server/tool)",
        "[$display](plugin://id?app=sample)",
        "[$display](https://example.test/path)",
        "\\[$display](/path)",
        "`[$display](/path)`",
        "[$display] \r\n\t ( /path )",
        "[$5](/a-valid-numeric-skill)",
        "[$x](nested(path))",
    ] {
        assert_refused(text, TextPolicyError::UnsupportedResourceMention);
    }
}

#[test]
fn malformed_link_syntax_falls_back_to_plain_names_exactly() {
    for text in [
        "[$beta] /tmp/beta",
        "[$beta]()",
        "[$beta](  )",
        "[$beta](/unterminated",
    ] {
        assert_eq!(names(text), ["beta"]);
    }
    assert_eq!(names("[beta](/tmp/beta)"), Vec::<String>::new());
    // Non-ASCII whitespace is not the allowed bracket-to-parenthesis spacing.
    assert_eq!(names("[$beta]\u{a0}(/path)"), ["beta"]);
}

#[test]
fn upstream_lexical_parity_corpus() {
    // Ported relevant cases from pinned skills/src/mentions_tests.rs. The
    // policy deliberately rejects parsed resource paths after lexical parity.
    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            "use $alpha and [$beta](/tmp/beta)",
            &["alpha"],
            &["/tmp/beta"],
        ),
        ("use $PATH and $alpha", &["alpha"], &[]),
        ("use [$HOME](/tmp/skill)", &[], &[]),
        ("use $XDG_CONFIG_HOME and $beta", &["beta"], &[]),
        ("[beta](/tmp/beta)", &[], &[]),
        ("[$beta] /tmp/beta", &["beta"], &[]),
        ("[$beta]()", &["beta"], &[]),
        ("use [$beta]   ( /tmp/beta )", &[], &["/tmp/beta"]),
        (
            "use $alpha.skill and $beta_extra",
            &["alpha", "beta_extra"],
            &[],
        ),
        (
            "use $slack:search and $alpha",
            &["alpha", "slack:search"],
            &[],
        ),
        (
            "use $notion-research-doc but not $notion-research-docs or $notion-research-doc_extra",
            &[
                "notion-research-doc",
                "notion-research-doc_extra",
                "notion-research-docs",
            ],
            &[],
        ),
    ];
    for &(text, expected_names, expected_paths) in cases {
        let parsed = mentions::extract_tool_mentions(text);
        assert_eq!(
            parsed.plain_names.into_iter().collect::<BTreeSet<_>>(),
            expected_names.iter().copied().collect()
        );
        assert_eq!(
            parsed.paths.into_iter().collect::<BTreeSet<_>>(),
            expected_paths.iter().copied().collect()
        );
    }
    assert!(names(&format!("{} not-a-mention", "$".repeat(256))).is_empty());
}

#[test]
fn text_and_unique_name_limits_refuse_instead_of_truncating() {
    let exact_text = "x".repeat(MAX_TEXT_BYTES);
    let prepared = prepare_text(exact_text.clone()).unwrap();
    assert_eq!(prepared.into_turn_input()[0]["text"], exact_text);
    assert_refused(
        &"x".repeat(MAX_TEXT_BYTES + 1),
        TextPolicyError::TextTooLarge,
    );
    // Count bytes, not Unicode scalar values.
    assert_refused(
        &"🦀".repeat(MAX_TEXT_BYTES / 4 + 1),
        TextPolicyError::TextTooLarge,
    );

    let at_limit = (0..MAX_SKILL_NAMES)
        .map(|n| format!("$name{n} "))
        .collect::<String>();
    assert_eq!(names(&at_limit).len(), MAX_SKILL_NAMES);
    assert_refused(
        &format!("{at_limit}$extra"),
        TextPolicyError::TooManySkillNames,
    );
    assert_eq!(names(&"$duplicate ".repeat(1000)), ["duplicate"]);
}

#[test]
fn long_names_are_not_silently_dropped_and_serialized_config_is_bounded() {
    let long_name = "a".repeat(1024);
    assert_eq!(names(&format!("${long_name}")), [long_name]);
    let oversized = "x".repeat(MAX_CONFIG_BYTES);
    assert_eq!(
        bounded_fixed_config([oversized.as_str()]),
        Err(TextPolicyError::ConfigTooLarge)
    );
    let maximum_input = format!("${}", "x".repeat(MAX_TEXT_BYTES - 1));
    let prepared = prepare_text(maximum_input).unwrap();
    assert!(serde_json::to_vec(&prepared.fixed_config()).unwrap().len() <= MAX_CONFIG_BYTES);
}

#[test]
fn fixed_config_preserves_all_owned_controls_and_only_adds_native_denies() {
    let prepared = prepare_text("$alpha".into()).unwrap();
    let config = prepared.fixed_config();
    let map = config.as_object().unwrap();
    assert_eq!(
        map.len(),
        CONFIG_REQUIRED_FALSE_PATHS.len() + CONFIG_REQUIRED_STRING_PAIRS.len() + 4
    );
    for &(path, value) in CONFIG_REQUIRED_STRING_PAIRS {
        assert_eq!(map[&dotted_key(path)], value);
    }
    for path in CONFIG_REQUIRED_FALSE_PATHS {
        assert_eq!(map[&dotted_key(path)], false);
    }
    assert_eq!(config["notify"], json!([]));
    assert_eq!(config["mcp_servers"], json!({}));
    assert_eq!(config["thread_unload_delay_secs"], 0);
    assert_eq!(
        config["skills.config"],
        json!([{"name":"alpha","enabled":false}])
    );
    assert!(map
        .keys()
        .all(|key| !key.contains('/') && !key.starts_with('.')));
    assert!(!map.contains_key("bypass_hook_trust"));

    // A caller mutation of its returned JSON cannot change the retained pair.
    let mut copy = prepared.fixed_config();
    copy["skills.config"][0]["enabled"] = json!(true);
    assert_eq!(
        prepared.fixed_config()["skills.config"][0]["enabled"],
        false
    );
}

#[test]
fn input_has_no_structured_capability_or_renderer_spans() {
    let input = prepare_text("literal $alpha".into())
        .unwrap()
        .into_turn_input();
    assert_eq!(input.as_array().unwrap().len(), 1);
    let item = input[0].as_object().unwrap();
    assert_eq!(
        item.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        ["text", "text_elements", "type"].into()
    );
    assert_eq!(item["type"], "text");
    assert_eq!(item["text_elements"], json!([]));
}
