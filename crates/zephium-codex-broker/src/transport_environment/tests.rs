use super::*;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

#[test]
fn lookup_reads_only_the_seven_original_transport_names() {
    let mut requested = Vec::new();
    let captured = InheritedTransport::capture(|name| {
        requested.push(name);
        None
    })
    .unwrap();
    assert_eq!(requested, NAMES);
    assert_eq!(captured.entries().count(), 0);
}

#[test]
fn no_secret_origin_plugin_or_owned_path_variable_is_requested() {
    let forbidden = [
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "OPENAI_BASE_URL",
        "CODEX_REFRESH_TOKEN_URL",
        "CODEX_REVOKE_TOKEN_URL",
        "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
        "CODEX_SANDBOX",
        "CODEX_HOME",
        "HOME",
        "USERPROFILE",
        "PATH",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "NODE_OPTIONS",
        "NODE_EXTRA_CA_CERTS",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "SSLKEYLOGFILE",
        "SSL_VERIFY",
        "PLUGIN_PATH",
    ];
    let captured = InheritedTransport::capture(|name| {
        assert!(!forbidden.contains(&name));
        None
    })
    .unwrap();
    assert_eq!(captured.entries().count(), 0);
}

#[test]
fn windows_case_insensitive_lookup_can_preserve_original_values() {
    let originals = [
        ("https_proxy", wide("http://synthetic-proxy.invalid:1234")),
        ("sSl_CeRt_FiLe", wide(r"C:\Synthetic CA\roots.pem")),
    ];
    let captured = InheritedTransport::capture(|name| {
        originals
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    })
    .unwrap();
    assert_eq!(
        captured.entries().collect::<Vec<_>>(),
        vec![
            ("HTTPS_PROXY", originals[0].1.as_slice()),
            ("SSL_CERT_FILE", originals[1].1.as_slice()),
        ]
    );
}

#[test]
fn missing_and_present_empty_remain_different() {
    let captured = InheritedTransport::capture(|name| (name == "NO_PROXY").then(Vec::new)).unwrap();
    assert_eq!(
        captured.entries().collect::<Vec<_>>(),
        vec![("NO_PROXY", &[][..])]
    );
}

#[test]
fn source_precedence_inputs_are_preserved_without_selecting_or_normalizing() {
    let originals = [
        ("HTTP_PROXY", " http://proxy.invalid:80 "),
        (
            "HTTPS_PROXY",
            "https://user:synthetic-proxy-password@proxy.invalid:443",
        ),
        ("ALL_PROXY", "socks5://proxy.invalid:1080"),
        ("NO_PROXY", "localhost,.example.invalid,127.0.0.1"),
        ("CODEX_CA_CERTIFICATE", ""),
        ("SSL_CERT_FILE", r"C:\Synthetic CA\التحقق.pem"),
        ("SSL_CERT_DIR", r"C:\Synthetic Roots;D:\Other Roots"),
    ];
    let captured = InheritedTransport::capture(|name| {
        originals
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| wide(value))
    })
    .unwrap();
    assert_eq!(captured.entries().count(), 7);
    for (name, value) in captured.entries() {
        let original = originals.iter().find(|(key, _)| *key == name).unwrap().1;
        assert_eq!(value, wide(original));
    }
}

#[test]
fn non_unicode_windows_utf16_is_not_lossily_reencoded() {
    let original = vec![
        b'C' as u16,
        b':' as u16,
        0xd800,
        0xdcff,
        0xdfff,
        b'x' as u16,
    ];
    let captured =
        InheritedTransport::capture(|name| (name == "SSL_CERT_FILE").then(|| original.clone()))
            .unwrap();
    assert_eq!(captured.entries().next().unwrap().1, original);
}

#[test]
fn debug_never_discloses_proxy_credentials_or_trust_paths() {
    let captured = InheritedTransport::capture(|name| match name {
        "HTTPS_PROXY" => Some(wide(
            "https://private-user:private-password@private-host.invalid",
        )),
        "SSL_CERT_FILE" => Some(wide(r"C:\PrivateOwner\private-roots.pem")),
        _ => None,
    })
    .unwrap();
    let debug = format!("{captured:?}");
    assert!(debug.contains("HTTPS_PROXY"));
    assert!(debug.contains("<redacted>"));
    for forbidden in [
        "private-user",
        "private-password",
        "private-host",
        "PrivateOwner",
        "private-roots",
    ] {
        assert!(!debug.contains(forbidden));
    }
}

#[test]
fn embedded_nul_refuses_the_whole_capture_without_silent_drop() {
    for value in [vec![0], vec![1, 0], vec![0, 1], vec![1, 0, 2]] {
        let fault = InheritedTransport::capture(|name| match name {
            "ALL_PROXY" => Some(wide("http://valid-first.invalid")),
            "HTTPS_PROXY" => Some(value.clone()),
            _ => None,
        })
        .unwrap_err();
        assert_eq!(fault, EnvFault::EmbeddedNul);
        assert_eq!(format!("{fault:?}"), "EmbeddedNul");
    }
}

#[test]
fn exact_per_value_bound_is_kept_but_larger_value_refuses() {
    let result = InheritedTransport::capture(|name| {
        (name == "NO_PROXY").then(|| vec![b'x' as u16; MAX_VALUE_UNITS])
    })
    .unwrap();
    assert_eq!(result.entries().next().unwrap().1.len(), MAX_VALUE_UNITS);
    let fault = InheritedTransport::capture(|name| {
        (name == "NO_PROXY").then(|| vec![b'x' as u16; MAX_VALUE_UNITS + 1])
    })
    .unwrap_err();
    assert_eq!(fault, EnvFault::ValueTooLarge);
}

#[test]
fn aggregate_bound_includes_names_separators_and_terminators() {
    let overhead = 1 + NAMES.iter().map(|name| name.len() + 2).sum::<usize>();
    let remainder = MAX_BLOCK_UNITS - overhead - MAX_VALUE_UNITS;
    let captured = InheritedTransport::capture(|name| {
        Some(vec![
            b'x' as u16;
            match name {
                "ALL_PROXY" => MAX_VALUE_UNITS,
                "CODEX_CA_CERTIFICATE" => remainder,
                _ => 0,
            }
        ])
    })
    .unwrap();
    assert_eq!(captured.entries().count(), 7);
    let fault = InheritedTransport::capture(|name| {
        Some(vec![
            b'x' as u16;
            match name {
                "ALL_PROXY" => MAX_VALUE_UNITS,
                "CODEX_CA_CERTIFICATE" => remainder,
                "NO_PROXY" => 1,
                _ => 0,
            }
        ])
    })
    .unwrap_err();
    assert_eq!(fault, EnvFault::BlockTooLarge);
}

#[test]
fn invalid_capture_stops_before_later_settings_are_read() {
    let mut requested = Vec::new();
    let fault = InheritedTransport::capture(|name| {
        requested.push(name);
        Some(vec![0])
    })
    .unwrap_err();
    assert_eq!(fault, EnvFault::EmbeddedNul);
    assert_eq!(requested, vec!["ALL_PROXY"]);
}
