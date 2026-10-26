//! Fixed synthetic tests only; never Claude executable/login/account activity.
use super::{
    process::{self, Command, Fixture, Layout},
    security::{self, HeldImage, Profile},
};
use std::{
    fs::File,
    io::Write,
    net::{TcpListener, TcpStream},
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) struct Root(pub(super) std::path::PathBuf, bool);
impl Root {
    pub(super) fn create(
        path: std::path::PathBuf,
        profile: &Profile,
    ) -> Result<Self, &'static str> {
        profile.create_directory(&path, false)?;
        Ok(Self(path, true))
    }
    pub(super) fn create_sentinel(path: std::path::PathBuf) -> Result<Self, &'static str> {
        std::fs::create_dir(&path).map_err(|_| "fresh sentinel creation failed")?;
        Ok(Self(path, true))
    }
    pub(super) fn cleanup(mut self) -> Result<(), &'static str> {
        std::fs::remove_dir_all(&self.0).map_err(|_| "private fixture cleanup failed")?;
        self.1 = false;
        Ok(())
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
pub(crate) fn qualify() -> Result<(), &'static str> {
    // This dedicated synthetic helper process owns its environment; no real
    // values are read, copied or restored, and the invoking shell is unchanged.
    for key in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "OPENAI_API_KEY",
        "NODE_OPTIONS",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "AWS_SECRET_ACCESS_KEY",
    ] {
        std::env::set_var(key, "synthetic-must-not-inherit");
    }
    let suffix = format!(
        "{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
    );
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}"))?;
    let root = Root::create(
        std::env::temp_dir().join(format!("Zephium.ClaudeFixture.{suffix}")),
        &profile,
    )?;
    let layout = Layout::create(&root.0, &profile)?;
    let image_path = root.0.join("fixture.exe");
    profile.copy_image(
        &std::env::current_exe().map_err(|_| "fixture identity unavailable")?,
        &image_path,
    )?;
    let hash =
        security::hash(&mut File::open(&image_path).map_err(|_| "fixture image unavailable")?)?;
    // An existing target cannot be adopted, overwritten, re-owned or cleaned up
    // by a failed fresh-create attempt.
    if Root::create(root.0.clone(), &profile).is_ok() || !root.0.is_dir() {
        return Err("existing root creation collision was not preserved");
    }
    if profile.copy_image(&image_path, &image_path).is_ok() || !image_path.is_file() {
        return Err("existing image creation collision was not preserved");
    }
    let after_collision = security::hash(
        &mut File::open(&image_path).map_err(|_| "collision removed existing image")?,
    )?;
    if after_collision != hash {
        return Err("collision changed existing image bytes");
    }
    let image = HeldImage::open(&image_path, hash)?;
    let cancelled = AtomicBool::new(false);
    let input = "synthetic stdin • split UTF-8\n".as_bytes().to_vec();
    let echo = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::Echo),
        input.clone(),
        &cancelled,
    )?;
    receipt("echo", &echo);
    if echo.code != 0
        || echo.stopped.is_some()
        || echo.stdout != input
        || echo.stderr != b"synthetic stderr only"
    {
        return Err("separate bounded pipe fixture failed");
    }

    let argv = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::Argv),
        Vec::new(),
        &cancelled,
    )?;
    receipt("argv", &argv);
    if argv.code != 0
        || argv.stopped.is_some()
        || argv.stdout != b"argv-ok"
        || !argv.stderr.is_empty()
    {
        return Err("native empty argv round trip failed");
    }
    // Sentinel belongs to the fixture, not the user. Ordinary parent can access it;
    // the LPAC receives no grant outside its own sealed root.
    let sentinel = Root::create_sentinel(
        std::env::temp_dir().join(format!("Zephium.ClaudeSentinel.{suffix}")),
    )?;
    let sentinel_file = sentinel.0.join("private.txt");
    std::fs::write(&sentinel_file, b"synthetic private sentinel")
        .map_err(|_| "sentinel write failed")?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| "loopback listener unavailable")?;
    let address = listener
        .local_addr()
        .map_err(|_| "loopback address unavailable")?;
    let _baseline = TcpStream::connect_timeout(&address, Duration::from_secs(1))
        .map_err(|_| "loopback baseline unavailable")?;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    let attributes = windows::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    let event = unsafe {
        windows::Win32::System::Threading::CreateEventW(
            Some(&attributes),
            true,
            false,
            windows::core::PCWSTR::null(),
        )
    }
    .map_err(|_| "inherited-handle fixture unavailable")?;
    let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
    let mut flags = 0u32;
    unsafe {
        windows::Win32::Foundation::GetHandleInformation(
            windows::Win32::Foundation::HANDLE(event.as_raw_handle()),
            &mut flags,
        )
    }
    .map_err(|_| "parent event handle control failed")?;
    if flags & windows::Win32::Foundation::HANDLE_FLAG_INHERIT.0 == 0 {
        return Err("parent event was not inheritable");
    }
    let handle_probe = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::HandleProbe),
        (event.as_raw_handle() as usize as u64)
            .to_le_bytes()
            .to_vec(),
        &cancelled,
    )?;
    receipt("handle-probe", &handle_probe);
    let strict_denial = crate::plan::fixture_handle_denial(
        handle_probe.code,
        handle_probe.stopped.is_some(),
        &handle_probe.stdout,
        &handle_probe.stderr,
    )
    .ok_or("unlisted handle denial was not established")?;
    super::diagnostic(
        "unlisted-handle-denial-established",
        i32::from(strict_denial),
    );
    let request = serde_json::json!({"sentinel":sentinel_file,"outside":sentinel.0,"loopback":address.to_string(),"image":image_path,"config":layout.config,"home":layout.home,"work":layout.work,"appdata":layout.appdata,"localappdata":layout.localappdata});
    let output = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::Isolation),
        serde_json::to_vec(&request).map_err(|_| "fixture serialization failed")?,
        &cancelled,
    )?;
    receipt("output", &output);
    // Recognize only this fixed helper grammar; never forward raw child output.
    for (stage, prefix) in [
        ("isolation-reached-files", b"isolation-files\n".as_slice()),
        ("isolation-reached-network", b"isolation-files\nisolation-network\n".as_slice()),
        ("isolation-reached-child", b"isolation-files\nisolation-network\nisolation-address-parsed\nwinsock-startup=0\nisolation-connect\nisolation-child\n".as_slice()),
        ("isolation-reached-environment", b"isolation-files\nisolation-network\nisolation-address-parsed\nwinsock-startup=0\nisolation-connect\nisolation-child\nisolation-environment\n".as_slice()),
        ("isolation-reached-config", b"isolation-files\nisolation-network\nisolation-address-parsed\nwinsock-startup=0\nisolation-connect\nisolation-child\nisolation-environment\nisolation-config\n".as_slice()),
    ] { if output.stdout.starts_with(prefix) { super::diagnostic(stage, 0); } }
    if let Some(rest) = output.stdout.strip_prefix(
        b"isolation-files\nisolation-network\nisolation-address-parsed\nwinsock-startup=",
    ) {
        if let Some(line) = rest
            .split(|byte| *byte == b'\n')
            .next()
            .filter(|line| line.len() <= 11)
        {
            if let Ok(text) = std::str::from_utf8(line) {
                if let Ok(code) = text.parse::<i32>() {
                    if code.to_string() == text {
                        super::diagnostic("synthetic-winsock-startup", code);
                    }
                }
            }
        }
    }
    match output.stderr.as_slice() {
        b"fixture-panic=network-runtime\n" => {
            super::diagnostic("synthetic-panic-network-runtime", 101)
        }
        b"fixture-panic=process-runtime\n" => {
            super::diagnostic("synthetic-panic-process-runtime", 101)
        }
        b"fixture-panic=unclassified\n" => super::diagnostic("synthetic-panic-unclassified", 101),
        b"claude-broker-fixture: private HOME path mismatch\n" => {
            super::diagnostic("synthetic-home-path-mismatch", 1)
        }
        b"claude-broker-fixture: private USERPROFILE path mismatch\n" => {
            super::diagnostic("synthetic-userprofile-path-mismatch", 1)
        }
        b"claude-broker-fixture: private APPDATA path mismatch\n" => {
            super::diagnostic("synthetic-appdata-path-mismatch", 1)
        }
        b"claude-broker-fixture: private LOCALAPPDATA path mismatch\n" => {
            super::diagnostic("synthetic-localappdata-path-mismatch", 1)
        }
        b"claude-broker-fixture: private work directory mismatch\n" => {
            super::diagnostic("synthetic-work-path-mismatch", 1)
        }
        b"claude-broker-fixture: private work directory unavailable\n" => {
            super::diagnostic("synthetic-work-path-unavailable", 1)
        }
        b"claude-broker-fixture: private config not writable\n" => {
            super::diagnostic("synthetic-config-write-failed", 1)
        }
        b"claude-broker-fixture: empty work directory was writable\n" => {
            super::diagnostic("synthetic-work-write-succeeded", 1)
        }
        _ => {}
    }
    if output.code != 0
        || output.stopped.is_some()
        || output.stdout != b"isolation-files\nisolation-network\nisolation-address-parsed\nwinsock-startup=0\nisolation-connect\nisolation-child\nisolation-environment\nisolation-config\nisolation-ok"
        || !output.stderr.is_empty()
    {
        return Err("ACL, child or loopback denial fixture failed");
    }
    let flood = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::Flood),
        Vec::new(),
        &cancelled,
    )?;
    receipt("flood", &flood);
    if flood.stopped != Some("output bound exceeded")
        || flood.stdout.len() > crate::plan::MAX_STDOUT_BYTES
        || flood.stderr.len() > crate::plan::MAX_STDERR_BYTES
    {
        return Err("pipe flood was not bounded");
    }
    let stderr_flood = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::StderrFlood),
        Vec::new(),
        &cancelled,
    )?;
    receipt("stderr_flood", &stderr_flood);
    if stderr_flood.stopped != Some("output bound exceeded")
        || stderr_flood.stderr.len() > crate::plan::MAX_STDERR_BYTES
        || !stderr_flood.stdout.is_empty()
    {
        return Err("stderr-only flood was not bounded separately");
    }
    let cancel = AtomicBool::new(false);
    let cancelled_run = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_secs(2));
            cancel.store(true, std::sync::atomic::Ordering::Release);
        });
        process::run(
            &profile,
            &image,
            &layout,
            Command::Fixture(Fixture::CancelWait),
            Vec::new(),
            &cancel,
        )
    })?;
    receipt("cancelled_run", &cancelled_run);
    if cancelled_run.stopped != Some("cancelled or deadline exceeded")
        || cancelled_run.stdout != b"ready"
        || cancelled_run.code == 0
    {
        return Err("mid-run cancel did not observe exit and settle");
    }
    let stall = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::Stall),
        vec![b'x'; crate::plan::MAX_STDIN_BYTES],
        &cancelled,
    )?;
    receipt("stall", &stall);
    if stall.stopped != Some("cancelled or deadline exceeded") || stall.code == 0 {
        return Err("deadline, blocked writer or reap fixture failed");
    }
    // A wrong hash and hard-link alias are rejected before CreateProcessW.
    if HeldImage::open(&image_path, [0; 32]).is_ok() {
        return Err("wrong image hash admitted");
    }
    // Held read-only image identity prevents write/rename replacement.
    if std::fs::OpenOptions::new()
        .write(true)
        .open(&image_path)
        .is_ok()
    {
        return Err("held executable allowed replacement");
    }
    drop(image); // Release held ancestors before removing the fresh fixture tree.
    let alias = root.0.join("alias.exe");
    std::fs::hard_link(&image_path, &alias).map_err(|_| "hard-link fixture unavailable")?;
    if HeldImage::open(&alias, hash).is_ok() {
        return Err("hard-linked executable admitted");
    }
    profile.close()?;
    sentinel.cleanup()?;
    root.cleanup()?;
    // Success is intentionally narrow: no vendor or LAN/public-egress qualification.
    std::io::stdout().write_all(b"synthetic LPAC/Job/pipes/ACL/loopback/hash fixtures passed; vendor and LAN/public-egress qualification pending\n").map_err(|_| "fixture receipt unavailable")?;
    Ok(())
}

/// Actual suspended-token discrimination only. This scenario never resumes a
/// child, reads an account, or admits a vendor image. It is separate from the
/// strict synthetic runtime qualification, which requires the LPAC matrix.
pub(crate) fn token_controls() -> Result<(), &'static str> {
    let suffix = format!(
        "{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
    );
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}"))?;
    let root = Root::create(
        std::env::temp_dir().join(format!("Zephium.ClaudeTokenControl.{suffix}")),
        &profile,
    )?;
    let layout = Layout::create(&root.0, &profile)?;
    let image_path = root.0.join("fixture.exe");
    profile.copy_image(
        &std::env::current_exe().map_err(|_| "fixture identity unavailable")?,
        &image_path,
    )?;
    let hash =
        security::hash(&mut File::open(&image_path).map_err(|_| "fixture image unavailable")?)?;
    let image = HeldImage::open(&image_path, hash)?;
    let cancelled = AtomicBool::new(false);
    for (stage, mode) in [
        (
            "ordinary-ac-token-control",
            Fixture::TokenAppContainerControl,
        ),
        ("lpac-token-control", Fixture::TokenLpacControl),
    ] {
        let output = process::run(
            &profile,
            &image,
            &layout,
            Command::Fixture(mode),
            Vec::new(),
            &cancelled,
        )?;
        receipt(stage, &output);
        if output.code != 1
            || output.stopped != Some("suspended token control completed")
            || !output.stdout.is_empty()
            || !output.stderr.is_empty()
        {
            return Err("suspended token control did not settle");
        }
    }
    drop(image);
    drop(layout);
    root.cleanup()?;
    profile.close()?;
    super::diagnostic("actual-token-controls-settled", 0);
    Ok(())
}

/// Read-only local startup of the exact signed vendor image, without an install,
/// account status/login or inference. Fixed argv; fresh empty private directories.
pub(crate) fn vendor_probe(source: &std::path::Path) -> Result<(), &'static str> {
    use super::signature;
    let original = HeldImage::open(source, signature::CANDIDATE_SHA256)?;
    signature::verify(&original)?;
    let suffix = format!(
        "{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
    );
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}"))?;
    let root = Root::create(
        std::env::temp_dir().join(format!("Zephium.ClaudeFixture.{suffix}")),
        &profile,
    )?;
    let layout = Layout::create(&root.0, &profile)?;
    let target = root.0.join("claude.exe");
    profile.copy_image(&original.path, &target)?;
    let image = HeldImage::open(&target, signature::CANDIDATE_SHA256)?;
    signature::verify(&image)?;
    let cancelled = AtomicBool::new(false);
    let version = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::VendorVersion),
        Vec::new(),
        &cancelled,
    )?;
    let expected = format!("{} (Claude Code)", signature::CANDIDATE_VERSION);
    receipt("version", &version);
    if version.code != 0
        || version.stopped.is_some()
        || std::str::from_utf8(&version.stdout).map(str::trim) != Ok(expected.as_str())
        || !version.stderr.is_empty()
    {
        return Err("pinned vendor version did not run under LPAC");
    }
    let help = process::run(
        &profile,
        &image,
        &layout,
        Command::Fixture(Fixture::VendorHelp),
        Vec::new(),
        &cancelled,
    )?;
    receipt("help", &help);
    if help.code != 0 || help.stopped.is_some() || help.stdout.is_empty() || !help.stderr.is_empty()
    {
        return Err("pinned vendor help did not run under LPAC");
    }
    drop(image);
    profile.close()?;
    root.cleanup()?;
    drop(original);
    std::io::stdout().write_all(b"pinned signed Claude 2.1.310 version/help ran under LPAC; login, account status and inference NOT qualified\n").map_err(|_| "vendor receipt failed")?;
    Ok(())
}

pub(super) fn receipt(stage: &'static str, output: &process::Output) {
    use std::io::Write;
    let _ = std::io::stdout().write_all(
        format!(
            "fixture-stage={stage}; exit={}; stopped={}; stdout-bytes={}; stderr-bytes={}\n",
            output.code,
            output.stopped.unwrap_or("none"),
            output.stdout.len(),
            output.stderr.len()
        )
        .as_bytes(),
    );
}

/// Synthetic prompt/manual-input scenarios only, under the unchanged strict
/// native launch checks. No real auth stdout grammar is guessed here.
pub(crate) fn auth_inputs() -> Result<(), &'static str> {
    use crate::{
        auth::{Controller, ManualCode},
        custody::Owner,
    };
    use std::sync::{atomic::Ordering, Arc};
    struct StopOnDrop(Arc<AtomicBool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let suffix = format!(
        "{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
    );
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}"))?;
    let root = Root::create(
        std::env::temp_dir().join(format!("Zephium.ClaudeAuthFixture.{suffix}")),
        &profile,
    )?;
    let layout = Layout::create(&root.0, &profile)?;
    let image_path = root.0.join("fixture.exe");
    profile.copy_image(
        &std::env::current_exe().map_err(|_| "fixture identity unavailable")?,
        &image_path,
    )?;
    let hash =
        security::hash(&mut File::open(&image_path).map_err(|_| "fixture image unavailable")?)?;
    let image = HeldImage::open(&image_path, hash)?;
    let owner = Owner {
        profile: 1,
        account_generation: 1,
        profile_generation: 1,
    };
    let mut controller =
        Controller::new(owner).map_err(|_| "synthetic controller identity exhausted")?;
    for scenario in ["manual-code", "cancel-waiting-code", "exit-waiting-code"] {
        let (attempt, input, cancelled) = controller
            .reserve(owner, std::time::Instant::now())
            .map_err(|_| "synthetic auth reservation failed")?;
        let (sender, events) = std::sync::mpsc::sync_channel(2);
        let output = std::thread::scope(|scope| -> Result<process::Output, &'static str> {
            let _stop = StopOnDrop(Arc::clone(&cancelled));
            let cancellation = &cancelled;
            let worker = scope.spawn(|| {
                process::run_login_fixture(
                    &profile,
                    &image,
                    &layout,
                    input,
                    cancellation,
                    sender,
                    scenario == "exit-waiting-code",
                )
            });
            if events
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "synthetic creation event missing")?
                != process::LoginFixtureEvent::Created
            {
                return Err("synthetic auth event order mismatch");
            }
            controller
                .spawned(attempt)
                .map_err(|_| "synthetic spawn accounting failed")?;
            if events
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "synthetic prompt event missing")?
                != process::LoginFixtureEvent::CodeRequested
            {
                return Err("synthetic auth prompt order mismatch");
            }
            controller
                .code_requested(owner, attempt, std::time::Instant::now())
                .map_err(|_| "synthetic prompt accounting failed")?;
            if scenario == "manual-code" {
                controller
                    .submit_code(
                        owner,
                        attempt,
                        ManualCode::new("synthetic-native-code#synthetic-state")
                            .map_err(|_| "synthetic code invalid")?,
                        std::time::Instant::now(),
                    )
                    .map_err(|_| "synthetic code delivery failed")?;
            } else if scenario == "cancel-waiting-code" {
                controller
                    .cancel(owner, attempt)
                    .map_err(|_| "synthetic cancel failed")?;
            }
            worker
                .join()
                .map_err(|_| "synthetic auth worker panicked")?
        })?;
        receipt(scenario, &output);
        match scenario {
            "manual-code"
                if output.code == 0
                    && output.stopped.is_none()
                    && output.stdout == process::LOGIN_FIXTURE_COMPLETE => {}
            "cancel-waiting-code"
                if output.code != 0
                    && output.stopped == Some("cancelled or deadline exceeded")
                    && output.stdout == process::LOGIN_FIXTURE_PROMPT => {}
            "exit-waiting-code"
                if output.code == 0
                    && output.stopped == Some("stdin interrupted")
                    && output.stdout == process::LOGIN_FIXTURE_PROMPT => {}
            _ => return Err("synthetic auth pipe lifecycle mismatch"),
        }
        if !output.stderr.is_empty() {
            return Err("synthetic auth stderr was not empty");
        }
        controller
            .observed_exit(attempt, output.code)
            .map_err(|_| "synthetic auth exit accounting failed")?;
        for pipe in 0..3 {
            controller
                .settled_pipe(attempt, pipe)
                .map_err(|_| "synthetic auth pipe accounting failed")?;
        }
        if controller
            .release(attempt)
            .map_err(|_| "synthetic auth custody not settled")?
            != output.code
        {
            return Err("synthetic auth release mismatch");
        }
    }
    drop(image);
    drop(layout);
    root.cleanup()?;
    profile.close()?;
    super::diagnostic("synthetic-auth-inputs-settled", 0);
    Ok(())
}
