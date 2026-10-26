//! Credential-free synthetic Windows helper, never shipped in release builds.
#![cfg_attr(not(debug_assertions), allow(dead_code))]
#[cfg(not(debug_assertions))]
compile_error!("Synthetic Claude broker helper is debug-only");

#[cfg(any(windows, test))]
fn exact_path(value: &std::ffi::OsStr, expected: &str) -> bool {
    value == std::ffi::OsStr::new(expected)
}

// Relation bits are diagnostic only. No bit changes path admission.
#[cfg(any(windows, test))]
fn path_relations(value: &std::ffi::OsStr, expected: &str, requested: &str, home: &str) -> u16 {
    let Some(value) = value.to_str() else {
        return 0;
    };
    let actual = std::path::Path::new(value);
    let expected_path = std::path::Path::new(expected);
    let tests = [
        value == expected,
        value.eq_ignore_ascii_case(expected),
        value.trim_end_matches('\\') == expected.trim_end_matches('\\'),
        actual == expected_path,
        actual.as_os_str() == expected_path.join("AC").as_os_str(),
        actual.file_name() == expected_path.file_name(),
        actual.starts_with(requested),
        actual.starts_with(home),
        value.contains('~'),
        expected.contains('~'),
        expected_path
            .parent()
            .is_some_and(|parent| actual.as_os_str() == parent.as_os_str()),
    ];
    tests
        .into_iter()
        .enumerate()
        .fold(0, |bits, (index, value)| bits | (u16::from(value) << index))
}

#[cfg(windows)]
fn main() {
    // Synthetic helper diagnostics only. Never copy panic payloads, paths, user
    // input or account-bearing text into public Actions logs.
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let text = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("");
        let file = info
            .location()
            .map(|location| location.file())
            .unwrap_or("");
        let marker: &[u8] = if text.contains("WSAStartup")
            || text.contains("WinSock")
            || text.contains("Winsock")
            || file.ends_with("/winsock.rs")
            || file.ends_with("\\winsock.rs")
            || file.contains("/sys/net/")
            || file.contains("\\sys\\net\\")
        {
            b"fixture-panic=network-runtime\n"
        } else if file.contains("/sys/pal/windows/process") || file.contains("/sys/process/") {
            b"fixture-panic=process-runtime\n"
        } else {
            b"fixture-panic=unclassified\n"
        };
        let _ = std::io::stderr().write_all(marker);
    }));
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [mode] if mode == "store-config" => zephium_claude_broker::qualify_store_config(),
        [mode] if mode == "store-custody" => zephium_claude_broker::qualify_store_custody(),
        [flag, mode, _root, _lock, _state]
            if flag == "--child"
                && matches!(
                    mode.as_str(),
                    "store-custody-busy"
                        | "store-custody-clean"
                        | "store-custody-abandon"
                        | "store-custody-timeout"
                        | "store-custody-overflow"
                ) =>
        {
            zephium_claude_broker::qualify_store_custody_child()
        }
        [mode] if mode == "token-controls" => zephium_claude_broker::qualify_token_controls(),
        [mode] if mode == "auth-inputs" => zephium_claude_broker::qualify_synthetic_auth_inputs(),
        [mode] if mode == "network-controls" => zephium_claude_broker::research_network_controls(),
        [mode] if mode == "qualify" => zephium_claude_broker::qualify_synthetic_fixture(),
        [flag, mode, values @ ..] if flag == "--child" && mode == "argv" => {
            if values == ["", "end\\", "a\"b"] {
                use std::io::Write;
                std::io::stdout()
                    .write_all(b"argv-ok")
                    .map_err(|_| "argv receipt failed")
            } else {
                Err("native argv round trip mismatch")
            }
        }
        [flag, mode] if flag == "--child" => child(mode),
        [mode, image] if mode == "vendor-probe" => {
            zephium_claude_broker::qualify_vendor_startup(std::path::Path::new(image))
        }
        _ => Err("only fixed fixture modes are accepted"),
    };
    if let Err(error) = result {
        eprintln!("claude-broker-fixture: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("claude-broker-fixture requires Windows; no qualification ran");
    std::process::exit(1);
}

#[cfg(windows)]
fn child(mode: &str) -> Result<(), &'static str> {
    use std::io::{Read, Write};
    match mode {
        "store-config-write" => zephium_claude_broker::qualify_store_config_child(false)?,
        "store-config-replace" => zephium_claude_broker::qualify_store_config_child(true)?,
        "network-control" => zephium_claude_broker::research_network_child()?,
        "login-input" | "login-exit" => {
            std::io::stdout()
                .write_all(b"synthetic-code-request\n")
                .map_err(|_| "synthetic prompt failed")?;
            std::io::stdout()
                .flush()
                .map_err(|_| "synthetic prompt flush failed")?;
            if mode == "login-input" {
                let mut input = Vec::new();
                std::io::stdin()
                    .take(4098)
                    .read_to_end(&mut input)
                    .map_err(|_| "synthetic code input failed")?;
                let matched = input == b"synthetic-native-code#synthetic-state\n";
                input.fill(0);
                if !matched {
                    return Err("synthetic code input mismatch");
                }
                std::io::stdout()
                    .write_all(b"synthetic-code-received\n")
                    .map_err(|_| "synthetic code receipt failed")?;
            }
        }
        "echo" => {
            let mut input = Vec::new();
            std::io::stdin()
                .take(256 * 1024 + 1)
                .read_to_end(&mut input)
                .map_err(|_| "fixture stdin failed")?;
            if input.len() > 256 * 1024 {
                return Err("fixture stdin overflow");
            }
            std::io::stdout()
                .write_all(&input)
                .map_err(|_| "fixture stdout failed")?;
            std::io::stderr()
                .write_all(b"synthetic stderr only")
                .map_err(|_| "fixture stderr failed")?;
        }
        "stall" => {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        "cancel-wait" => {
            std::io::stdout()
                .write_all(b"ready")
                .map_err(|_| "fixture ready failed")?;
            std::io::stdout()
                .flush()
                .map_err(|_| "fixture flush failed")?;
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        "stderr-flood" => {
            let block = [b'x'; 8192];
            loop {
                std::io::stderr()
                    .write_all(&block)
                    .map_err(|_| "fixture flood stopped")?;
            }
        }
        "flood" => {
            let block = [b'x'; 8192];
            loop {
                std::io::stdout()
                    .write_all(&block)
                    .map_err(|_| "fixture flood stopped")?;
            }
        }
        "handle-probe" => {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{
                Foundation::{GetHandleInformation, HANDLE},
                System::{
                    SystemServices::PROCESS_MITIGATION_STRICT_HANDLE_CHECK_POLICY,
                    Threading::{
                        GetCurrentProcess, GetProcessMitigationPolicy,
                        ProcessStrictHandleCheckPolicy,
                    },
                },
            };
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(9)
                .read_to_end(&mut bytes)
                .map_err(|_| "handle fixture input failed")?;
            let bytes: [u8; 8] = bytes
                .try_into()
                .map_err(|_| "handle fixture input invalid")?;
            let inherited = usize::try_from(u64::from_le_bytes(bytes))
                .map_err(|_| "handle fixture value invalid")?;
            if inherited == 0 || inherited == usize::MAX {
                return Err("handle fixture value invalid");
            }
            let mut flags = 0u32;
            unsafe { GetHandleInformation(HANDLE(std::io::stdout().as_raw_handle()), &mut flags) }
                .map_err(|_| "known pipe handle control failed")?;
            let mut policy = PROCESS_MITIGATION_STRICT_HANDLE_CHECK_POLICY::default();
            unsafe {
                GetProcessMitigationPolicy(
                    GetCurrentProcess(),
                    ProcessStrictHandleCheckPolicy,
                    (&raw mut policy).cast(),
                    std::mem::size_of_val(&policy),
                )
            }
            .map_err(|_| "handle mitigation query failed")?;
            let policy = unsafe { policy.Anonymous.Flags };
            if policy & !3 != 0 {
                return Err("handle mitigation flags unknown");
            }
            let marker: &[u8] = if policy & 1 != 0 {
                b"known-handle-ok\nstrict-handles=1\nprobe-ready\n"
            } else {
                b"known-handle-ok\nstrict-handles=0\nprobe-ready\n"
            };
            std::io::stdout()
                .write_all(marker)
                .map_err(|_| "handle probe marker failed")?;
            std::io::stdout()
                .flush()
                .map_err(|_| "handle probe marker flush failed")?;
            // This exact negative query is intentionally the only operation after
            // the marker. Strict-handle policy may raise STATUS_INVALID_HANDLE;
            // it is not disabled and no generic child crash is accepted.
            match unsafe { GetHandleInformation(HANDLE(inherited as *mut _), &mut flags) } {
                Ok(()) => return Err("unlisted inheritable handle was present"),
                Err(error) if error.code().0 as u32 == 0x8007_0006 => {}
                Err(_) => return Err("handle probe returned unexpected error"),
            }
            std::io::stdout()
                .write_all(b"not-inherited\n")
                .map_err(|_| "handle probe receipt failed")?;
        }
        "isolation" => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(16 * 1024)
                .read_to_end(&mut bytes)
                .map_err(|_| "fixture input failed")?;
            let input: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| "fixture JSON failed")?;
            let path = |key| {
                input
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .ok_or("fixture field missing")
            };
            std::io::stdout()
                .write_all(b"isolation-files\n")
                .map_err(|_| "isolation marker failed")?;
            if std::fs::read(path("sentinel")?).is_ok()
                || std::fs::write(path("sentinel")?, b"modified").is_ok()
                || std::fs::read_dir(path("outside")?).is_ok()
            {
                return Err("outside sentinel access succeeded");
            }
            std::io::stdout()
                .write_all(b"isolation-network\n")
                .map_err(|_| "isolation marker failed")?;
            let address = path("loopback")?
                .parse()
                .map_err(|_| "fixture address invalid")?;
            std::io::stdout()
                .write_all(b"isolation-address-parsed\n")
                .map_err(|_| "isolation marker failed")?;
            use windows::Win32::Networking::WinSock::{WSACleanup, WSAStartup, WSADATA};
            struct Winsock(bool);
            impl Winsock {
                fn finish(mut self) -> Result<(), &'static str> {
                    self.0 = false;
                    if unsafe { WSACleanup() } != 0 {
                        return Err("synthetic Winsock cleanup failed");
                    }
                    Ok(())
                }
            }
            impl Drop for Winsock {
                fn drop(&mut self) {
                    if self.0 {
                        unsafe {
                            WSACleanup();
                        }
                    }
                }
            }
            let mut data = WSADATA::default();
            let code = unsafe { WSAStartup(0x0202, &mut data) };
            let winsock = (code == 0).then(|| Winsock(true));
            std::io::stdout()
                .write_all(format!("winsock-startup={code}\n").as_bytes())
                .map_err(|_| "Winsock marker failed")?;
            std::io::stdout()
                .flush()
                .map_err(|_| "Winsock marker flush failed")?;
            if code != 0 {
                return Err("synthetic Winsock initialization failed");
            }
            let winsock = winsock.ok_or("synthetic Winsock initialization failed")?;
            if data.wVersion != 0x0202 {
                return Err("synthetic Winsock version mismatch");
            }
            std::io::stdout()
                .write_all(b"isolation-connect\n")
                .map_err(|_| "isolation marker failed")?;
            std::io::stdout()
                .flush()
                .map_err(|_| "isolation marker flush failed")?;
            let connected = std::net::TcpStream::connect_timeout(
                &address,
                std::time::Duration::from_millis(500),
            )
            .is_ok();
            winsock.finish()?;
            if connected {
                return Err("loopback access succeeded");
            }
            std::io::stdout()
                .write_all(b"isolation-child\n")
                .map_err(|_| "isolation marker failed")?;
            if let Ok(mut child) = std::process::Command::new(path("image")?)
                .args(["--child", "echo"])
                .spawn()
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err("second child was admitted");
            }
            std::io::stdout()
                .write_all(b"isolation-environment\n")
                .map_err(|_| "isolation marker failed")?;
            for key in [
                "ANTHROPIC_API_KEY",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "OPENAI_API_KEY",
                "NODE_OPTIONS",
                "HTTPS_PROXY",
                "HTTP_PROXY",
                "AWS_SECRET_ACCESS_KEY",
            ] {
                if std::env::var_os(key).is_some() {
                    return Err("ambient sensitive variable inherited");
                }
            }
            std::io::stdout()
                .write_all(b"isolation-config\n")
                .map_err(|_| "isolation marker failed")?;
            let config = std::env::var_os("CLAUDE_CONFIG_DIR").ok_or("private config missing")?;
            if !exact_path(&config, path("config")?) {
                return Err("config isolation path mismatch");
            }
            for (variable, field) in [
                ("HOME", "home"),
                ("USERPROFILE", "home"),
                ("APPDATA", "appdata"),
                ("LOCALAPPDATA", "localappdata"),
            ] {
                let value = std::env::var_os(variable).ok_or("private environment path missing")?;
                if !exact_path(&value, path(field)?) {
                    if variable == "LOCALAPPDATA" {
                        let relation = path_relations(
                            &value,
                            path(field)?,
                            path("requested_localappdata")?,
                            path("home")?,
                        );
                        std::io::stdout()
                            .write_all(format!("localappdata-relation={relation}\n").as_bytes())
                            .map_err(|_| "profile relation diagnostic failed")?;
                    }
                    return Err(match variable {
                        "HOME" => "private HOME path mismatch",
                        "USERPROFILE" => "private USERPROFILE path mismatch",
                        "APPDATA" => "private APPDATA path mismatch",
                        "LOCALAPPDATA" => "private LOCALAPPDATA path mismatch",
                        _ => "private environment path mismatch",
                    });
                }
            }
            for variable in ["TMP", "TEMP"] {
                let value = std::env::var_os(variable).ok_or("private temporary path missing")?;
                if !exact_path(&value, path("temporary")?) {
                    return Err("private temporary path mismatch");
                }
            }
            for (field, leaf) in [
                ("localappdata", "synthetic-localappdata-write"),
                ("temporary", "synthetic-temp-write"),
            ] {
                let target = std::path::Path::new(path(field)?).join(leaf);
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(target)
                    .and_then(|mut file| file.write_all(b"fixture only"))
                    .map_err(|_| "fresh profile data write failed")?;
            }
            for write in [false, true] {
                let result = std::fs::OpenOptions::new()
                    .read(!write)
                    .write(write)
                    .open(path("foreign_data")?);
                match result {
                    Err(error) if error.raw_os_error() == Some(5) => {}
                    _ => return Err("foreign profile data denial not established"),
                }
            }
            if !exact_path(
                std::env::current_dir()
                    .map_err(|_| "private work directory unavailable")?
                    .as_os_str(),
                path("work")?,
            ) {
                return Err("private work directory mismatch");
            }
            std::fs::write(
                std::path::PathBuf::from(config).join("synthetic-owned-config"),
                b"fixture only",
            )
            .map_err(|_| "private config not writable")?;
            let home = std::env::var_os("HOME").ok_or("private home missing")?;
            if Some(home) != std::env::var_os("USERPROFILE") {
                return Err("home isolation mismatch");
            }
            if std::fs::write("unexpected-work-file", b"fixture only").is_ok() {
                return Err("empty work directory was writable");
            }
            std::io::stdout()
                .write_all(b"isolation-ok")
                .map_err(|_| "fixture receipt failed")?;
        }
        _ => return Err("unknown child fixture"),
    }
    Ok(())
}

#[cfg(test)]
mod path_tests {
    #[test]
    fn relation_bits_never_replace_exact_acceptance() {
        use std::ffi::OsStr;
        let expected = r"C:\synthetic\profile\AC";
        let different = r"c:\synthetic\profile\ac";
        let relation =
            super::path_relations(OsStr::new(different), expected, "unrelated", "unrelated");
        assert_eq!(relation & 1, 0);
        assert_ne!(relation & 2, 0);
        assert!(!super::exact_path(OsStr::new(different), expected));
        assert!(relation <= 2047);
        assert_ne!(
            super::path_relations(OsStr::new("x~1"), "other", "unrelated", "unrelated") & 256,
            0
        );
        assert_ne!(
            super::path_relations(OsStr::new("other"), "x~1", "unrelated", "unrelated") & 512,
            0
        );
    }
    #[test]
    fn exact_expected_path_never_accepts_lexical_aliases() {
        use std::ffi::OsStr;
        let expected = r"C:\synthetic\profile\AC";
        assert!(super::exact_path(OsStr::new(expected), expected));
        for alias in [
            r"C:\synthetic\profile\.\AC",
            r"C:\synthetic\profile\\AC",
            r"c:\synthetic\profile\AC",
            r"C:\synthetic\profile\AC\",
            r"C:/synthetic/profile/AC",
            r"C:\synthetic\other\AC",
        ] {
            assert!(!super::exact_path(OsStr::new(alias), expected));
        }
    }
}
