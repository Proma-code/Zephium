//! Credential-free synthetic Windows helper, never shipped in release builds.
#![cfg_attr(not(debug_assertions), allow(dead_code))]
#[cfg(not(debug_assertions))]
compile_error!("Synthetic Claude broker helper is debug-only");

#[cfg(windows)]
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [mode] if mode == "token-controls" => zephium_claude_broker::qualify_token_controls(),
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
            let inherited = input
                .get("inheritable_handle")
                .and_then(serde_json::Value::as_u64)
                .ok_or("fixture handle missing")?;
            let mut flags = 0u32;
            // SAFETY: probing an opaque numeric handle cannot dereference memory;
            // this known fixture event must not exist in the child's handle table.
            if unsafe {
                windows::Win32::Foundation::GetHandleInformation(
                    windows::Win32::Foundation::HANDLE(inherited as usize as *mut _),
                    &mut flags,
                )
            }
            .is_ok()
            {
                return Err("unlisted inheritable handle escaped");
            }
            if std::fs::read(path("sentinel")?).is_ok()
                || std::fs::write(path("sentinel")?, b"modified").is_ok()
                || std::fs::read_dir(path("outside")?).is_ok()
            {
                return Err("outside sentinel access succeeded");
            }
            let address = path("loopback")?
                .parse()
                .map_err(|_| "fixture address invalid")?;
            if std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(500))
                .is_ok()
            {
                return Err("loopback access succeeded");
            }
            if let Ok(mut child) = std::process::Command::new(path("image")?)
                .args(["--child", "echo"])
                .spawn()
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err("second child was admitted");
            }
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
            let config = std::env::var_os("CLAUDE_CONFIG_DIR").ok_or("private config missing")?;
            if std::path::Path::new(&config) != std::path::Path::new(path("config")?) {
                return Err("config isolation path mismatch");
            }
            for (variable, field) in [
                ("HOME", "home"),
                ("USERPROFILE", "home"),
                ("APPDATA", "appdata"),
                ("LOCALAPPDATA", "localappdata"),
            ] {
                let value = std::env::var_os(variable).ok_or("private environment path missing")?;
                if std::path::Path::new(&value) != std::path::Path::new(path(field)?) {
                    return Err("private environment path mismatch");
                }
            }
            if std::env::current_dir().map_err(|_| "private work directory unavailable")?
                != std::path::Path::new(path("work")?)
            {
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
