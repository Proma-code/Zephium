//! Disposable synthetic differential only; never a vendor capability fallback.
use super::{
    fixtures::{receipt, Root},
    process::{self, Command, Fixture, Layout},
    security::{self, HeldImage, Profile},
};
#[path = "listener_research.rs"]
mod listener_research;
#[path = "registry_research.rs"]
mod registry_research;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    core::w,
    Win32::{
        Foundation::ERROR_SUCCESS,
        Networking::WinSock::{WSACleanup, WSAStartup, WSADATA},
        System::{
            JobObjects::{
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            },
            Registry::{
                RegCloseKey, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE,
            },
        },
    },
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    outside: PathBuf,
    foreign: PathBuf,
    image: PathBuf,
    config: PathBuf,
    loopback: SocketAddr,
    public: SocketAddr,
    registry: registry_research::Request,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    catalog_read: u32,
    catalog_write_open: u32,
    winsock_startup: i32,
    loopback_error: Option<i32>,
    loopback_timeout: bool,
    child_error: i32,
    job_query: i32,
    job_limit: u32,
    job_flags: u32,
    public_connected: bool,
    listener: Option<listener_research::Observation>,
    registry: registry_research::Observation,
}

impl Observation {
    fn baseline_matches(&self) -> bool {
        self.catalog_read == 5
            && self.catalog_write_open == 5
            && self.winsock_startup == 10107
            && self.loopback_error.is_none()
            && !self.loopback_timeout
            && !self.public_connected
            && self.listener.is_none()
            && self.child_quota_denied()
            && self.registry.denied()
    }
    fn candidate_matches(&self) -> bool {
        self.catalog_read == 0
            && self.catalog_write_open == 5
            && self.winsock_startup == 0
            && self.public_connected
            && self
                .listener
                .as_ref()
                .is_some_and(listener_research::Observation::measured)
            && ((self.loopback_error == Some(10013) && !self.loopback_timeout)
                || (matches!(self.loopback_error, None | Some(10060)) && self.loopback_timeout))
            && self.child_quota_denied()
            && self.registry.denied()
    }
    fn child_quota_denied(&self) -> bool {
        self.child_error == 1816
            && self.job_query == 0
            && self.job_limit == 1
            && self.job_flags
                == (JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                    | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                    | JOB_OBJECT_LIMIT_PROCESS_MEMORY)
                    .0
    }
}

// Conservative public-v4 filter for the one fixed DNS name. Never probe a
// returned loopback/LAN/link-local/shared/special-use address or another port.
fn public_address_allowed(address: SocketAddr) -> bool {
    let IpAddr::V4(ip) = address.ip() else {
        return false;
    };
    let [a, b, c, _] = ip.octets();
    address.port() == 443
        && !(a == 0
            || a == 10
            || a == 127
            || a >= 224
            || (a == 100 && (64..=127).contains(&b))
            || (a == 169 && b == 254)
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && (b == 168 || (b == 0 && c <= 2) || (b == 88 && c == 99)))
            || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
            || (a == 203 && b == 0 && c == 113))
}

fn public_positive_control() -> Result<SocketAddr, &'static str> {
    // Resolution happens only in this credential-free parent, against the
    // original image's fixed official download host. No URI/host comes from UI.
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = ("downloads.claude.ai", 443)
            .to_socket_addrs()
            .map(|addresses| {
                addresses
                    .take(16)
                    .filter(|address| public_address_allowed(*address))
                    .take(4)
                    .collect::<Vec<_>>()
            });
        let _ = sender.send(result);
    });
    let addresses = receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "research public DNS deadline exceeded")?
        .map_err(|_| "research public DNS unavailable")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    for address in addresses {
        let budget = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(2));
        if budget.is_zero() {
            break;
        }
        if TcpStream::connect_timeout(&address, budget).is_ok() {
            return Ok(address);
        }
    }
    Err("research public parent TCP control unavailable")
}

/// This opens one fixed OS catalog key; no values, enumeration, ACL changes or
/// registry writes. Even the write-right control only opens/closes the handle.
fn catalog_open(write: bool) -> Result<u32, &'static str> {
    let mut key = HKEY::default();
    let code = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!("SYSTEM\\CurrentControlSet\\Services\\WinSock2\\Parameters"),
            None,
            if write { KEY_SET_VALUE } else { KEY_READ },
            &mut key,
        )
    };
    if code == ERROR_SUCCESS && unsafe { RegCloseKey(key) } != ERROR_SUCCESS {
        return Err("research catalog handle cleanup failed");
    }
    Ok(code.0)
}

pub(crate) fn controls() -> Result<(), &'static str> {
    // This short-lived fixture process owns its environment. Never inspect or
    // restore ambient values; ensure the child absence tests are not vacuous.
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
    if catalog_open(false)? != 0 {
        return Err("research parent catalog positive control failed");
    }
    let public = public_positive_control()?;
    let suffix = format!(
        "{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
    );
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.Network.{suffix}"))?;
    let root = Root::create(
        std::env::temp_dir().join(format!("Zephium.ClaudeNetwork.{suffix}")),
        &profile,
    )?;
    let layout = Layout::create(&root.0, &profile)?;
    let image_path = root.0.join("fixture.exe");
    profile.copy_image(
        &std::env::current_exe().map_err(|_| "research image unavailable")?,
        &image_path,
    )?;
    let hash =
        security::hash(&mut File::open(&image_path).map_err(|_| "research image unavailable")?)?;
    let image = HeldImage::open(&image_path, hash)?;
    let foreign_profile = Profile::create(&format!("Zephium.ClaudeFixture.Foreign.{suffix}"))?;
    let registry = registry_research::Sentinels::create(&foreign_profile, &suffix)?;
    let foreign = Root::create(
        std::env::temp_dir().join(format!("Zephium.ClaudeForeign.{suffix}")),
        &foreign_profile,
    )?;
    let outside = Root::create_sentinel(
        std::env::temp_dir().join(format!("Zephium.ClaudeNetworkSentinel.{suffix}")),
    )?;
    let foreign_path = foreign.0.join("synthetic-credentials.json");
    let outside_path = outside.0.join("private.txt");
    const SENTINEL: &[u8] = b"synthetic-only-must-not-read-or-change";
    for path in [&foreign_path, &outside_path] {
        std::fs::write(path, SENTINEL).map_err(|_| "research sentinel creation failed")?;
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| "research loopback listener failed")?;
    let address = listener
        .local_addr()
        .map_err(|_| "research loopback address failed")?;
    let parent_connection = TcpStream::connect_timeout(&address, Duration::from_secs(1))
        .map_err(|_| "research loopback positive connect failed")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "research listener mode failed")?;
    let (accepted, _) = listener
        .accept()
        .map_err(|_| "research loopback positive accept failed")?;
    drop((accepted, parent_connection));
    let local_listener = listener_research::measure();
    local_listener.report();
    if !local_listener.positive() {
        return Err("research parent local-listener control failed");
    }
    let request = serde_json::to_vec(&Request {
        outside: outside_path.clone(),
        foreign: foreign_path.clone(),
        image: image_path,
        config: layout.config.clone(),
        loopback: address,
        public,
        registry: registry.request(),
    })
    .map_err(|_| "research request encoding failed")?;
    if request.len() > 8192 {
        return Err("research request exceeds bound");
    }
    let cancelled = AtomicBool::new(false);
    let mut observations = Vec::new();
    for (stage, mode) in [
        ("network-baseline", Fixture::NetworkBaseline),
        ("network-registry-candidate", Fixture::NetworkRegistryRead),
    ] {
        let output = process::run(
            &profile,
            &image,
            &layout,
            Command::Fixture(mode),
            request.clone(),
            &cancelled,
        )?;
        receipt(stage, &output);
        if output.code != 0
            || output.stopped.is_some()
            || !output.stderr.is_empty()
            || output.stdout.len() > 1024
        {
            return Err("research child controls did not settle cleanly");
        }
        let result: Observation =
            serde_json::from_slice(&output.stdout).map_err(|_| "research observation invalid")?;
        // Only fixed numeric fields are projected, never child-controlled strings.
        super::diagnostic(stage, result.winsock_startup);
        super::diagnostic("network-research-catalog-read", result.catalog_read as i32);
        super::diagnostic(
            "network-research-catalog-write-open",
            result.catalog_write_open as i32,
        );
        super::diagnostic(
            "network-research-loopback-error",
            result.loopback_error.unwrap_or(-1),
        );
        super::diagnostic(
            "network-research-loopback-timeout",
            i32::from(result.loopback_timeout),
        );
        super::diagnostic("network-research-child-error", result.child_error);
        super::diagnostic("network-research-job-query", result.job_query);
        super::diagnostic("network-research-job-limit", result.job_limit as i32);
        super::diagnostic("network-research-job-flags", result.job_flags as i32);
        super::diagnostic(
            "network-research-public-tcp",
            i32::from(result.public_connected),
        );
        if let Some(listener) = result.listener.as_ref() {
            listener.report();
        }
        result.registry.report();
        registry.verify()?;
        for path in [&foreign_path, &outside_path] {
            if std::fs::read(path).map_err(|_| "research sentinel disappeared")? != SENTINEL {
                return Err("research sentinel changed");
            }
        }
        if !matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        {
            return Err("research child reached loopback listener");
        }
        observations.push(result);
    }
    let baseline = &observations[0];
    let candidate = &observations[1];
    // A timeout is only observed loopback unreachability, correlated with the
    // live parent listener and this child's public TCP positive control. It is
    // not called an explicit access-denied result or proof of WFP attribution.
    let matched = baseline.baseline_matches() && candidate.candidate_matches();
    drop(image);
    profile.close()?;
    foreign_profile.close()?;
    root.cleanup()?;
    foreign.cleanup()?;
    outside.cleanup()?;
    registry.cleanup()?;
    if !matched {
        return Err("research network differential remains unqualified");
    }
    super::diagnostic("network-research-differential-settled", 0);
    super::diagnostic("network-research-loopback-observed-unreachable", 0);
    Ok(())
}

pub(crate) fn child() -> Result<(), &'static str> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| "research stdin failed")?;
    if bytes.len() > 8192 {
        return Err("research stdin exceeds bound");
    }
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|_| "research request invalid")?;
    if !request.loopback.ip().is_loopback() || request.loopback.port() == 0 {
        return Err("research address not loopback");
    }
    if !public_address_allowed(request.public) {
        return Err("research public endpoint invalid");
    }
    for path in [&request.outside, &request.foreign] {
        for result in [
            std::fs::read(path).map(|_| ()),
            std::fs::write(path, b"changed"),
            std::fs::remove_file(path),
        ] {
            if !matches!(result, Err(error) if error.raw_os_error() == Some(5)) {
                return Err("research private sentinel denial not established");
            }
        }
        if !matches!(std::fs::read_dir(path.parent().ok_or("research sentinel parent missing")?), Err(error) if error.raw_os_error() == Some(5))
        {
            return Err("research private directory denial not established");
        }
    }
    if std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from) != Some(request.config.clone()) {
        return Err("research private config mismatch");
    }
    std::fs::write(
        request.config.join("synthetic-network-owned"),
        b"fixture-only",
    )
    .map_err(|_| "research private config unwritable")?;
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
            return Err("research ambient environment inherited");
        }
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    let job_query = unsafe {
        QueryInformationJobObject(
            None,
            JobObjectExtendedLimitInformation,
            (&raw mut limits).cast(),
            std::mem::size_of_val(&limits) as u32,
            None,
        )
    }
    .err()
    .map_or(0, |error| error.code().0);
    let child_error = match std::process::Command::new(&request.image)
        .args(["--child", "echo"])
        .spawn()
    {
        Ok(mut child) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("research second process admitted");
        }
        Err(error) => error
            .raw_os_error()
            .ok_or("research child denial code absent")?,
    };
    let mut result = Observation {
        catalog_read: catalog_open(false)?,
        catalog_write_open: catalog_open(true)?,
        winsock_startup: 0,
        loopback_error: None,
        loopback_timeout: false,
        child_error,
        job_query,
        job_limit: limits.BasicLimitInformation.ActiveProcessLimit,
        job_flags: limits.BasicLimitInformation.LimitFlags.0,
        public_connected: false,
        listener: None,
        registry: registry_research::child(&request.registry)?,
    };
    struct Winsock;
    impl Drop for Winsock {
        fn drop(&mut self) {
            unsafe {
                WSACleanup();
            }
        }
    }
    let mut data = WSADATA::default();
    result.winsock_startup = unsafe { WSAStartup(0x0202, &mut data) };
    if result.winsock_startup == 0 {
        let winsock = Winsock;
        if data.wVersion != 0x0202 {
            return Err("research Winsock version mismatch");
        }
        result.listener = Some(listener_research::measure());
        result.public_connected =
            TcpStream::connect_timeout(&request.public, Duration::from_secs(3)).is_ok();
        match TcpStream::connect_timeout(&request.loopback, Duration::from_millis(750)) {
            Ok(_) => result.loopback_error = Some(0),
            Err(error) => {
                result.loopback_error = error.raw_os_error();
                result.loopback_timeout = error.kind() == std::io::ErrorKind::TimedOut;
            }
        }
        std::mem::forget(winsock);
        if unsafe { WSACleanup() } != 0 {
            return Err("research Winsock cleanup failed");
        }
    }
    let output = serde_json::to_vec(&result).map_err(|_| "research result encoding failed")?;
    std::io::stdout()
        .write_all(&output)
        .map_err(|_| "research result output failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate() -> Observation {
        Observation {
            catalog_read: 0,
            catalog_write_open: 5,
            winsock_startup: 0,
            loopback_error: Some(10013),
            loopback_timeout: false,
            child_error: 1816,
            job_query: 0,
            job_limit: 1,
            job_flags: 0x2108,
            public_connected: true,
            listener: Some(listener_research::Observation::positive_fixture()),
            registry: registry_research::Observation::denied_fixture(),
        }
    }
    #[test]
    fn listener_measurement_is_required_but_not_a_callback_or_login_proof() {
        let value = candidate();
        assert!(value.candidate_matches());
        assert!(serde_json::to_vec(&value).unwrap().len() <= 1024);
        let mut missing = candidate();
        missing.listener = None;
        assert!(!missing.candidate_matches());
    }
    #[test]
    fn startup_failure_is_not_network_denial() {
        let baseline = Observation {
            catalog_read: 5,
            catalog_write_open: 5,
            winsock_startup: 10107,
            loopback_error: None,
            loopback_timeout: false,
            child_error: 1816,
            job_query: 0,
            job_limit: 1,
            job_flags: 0x2108,
            public_connected: false,
            listener: None,
            registry: registry_research::Observation::denied_fixture(),
        };
        assert!(baseline.baseline_matches());
        assert!(!baseline.candidate_matches());
        assert!(!candidate().baseline_matches());
    }
    #[test]
    fn candidate_distinguishes_measured_timeout_from_denial_and_refusal() {
        assert!(candidate().candidate_matches());
        for code in [None, Some(0), Some(10060), Some(10061), Some(10107)] {
            let mut value = candidate();
            value.loopback_error = code;
            assert!(!value.candidate_matches());
        }
        let mut value = candidate();
        value.loopback_timeout = true;
        assert!(!value.candidate_matches());
        let mut value = candidate();
        value.loopback_error = None;
        value.loopback_timeout = true;
        assert!(value.candidate_matches());
        value.public_connected = false;
        assert!(!value.candidate_matches());
        let mut value = candidate();
        value.loopback_error = Some(10060);
        value.loopback_timeout = true;
        assert!(value.candidate_matches());
        let mut value = candidate();
        value.winsock_startup = 10107;
        assert!(!value.candidate_matches());
        let mut value = candidate();
        value.catalog_write_open = 0;
        assert!(!value.candidate_matches());
        let mut value = candidate();
        value.child_error = 0;
        assert!(!value.candidate_matches());
    }
    #[test]
    fn quota_error_requires_the_actual_nonbreakaway_one_child_job() {
        for error in [0, 5, 8, 87] {
            let mut value = candidate();
            value.child_error = error;
            assert!(!value.candidate_matches());
        }
        let mut value = candidate();
        value.job_query = -1;
        assert!(!value.candidate_matches());
        let mut value = candidate();
        value.job_limit = 2;
        assert!(!value.candidate_matches());
        for flags in [0, 0x2100, 0x2908, 0x3108] {
            let mut value = candidate();
            value.job_flags = flags;
            assert!(!value.candidate_matches());
        }
    }
    #[test]
    fn public_tcp_destination_rejects_nonpublic_and_non443_addresses() {
        for address in [
            "0.0.0.0:443",
            "10.1.2.3:443",
            "127.0.0.1:443",
            "100.64.0.1:443",
            "169.254.1.1:443",
            "172.16.0.1:443",
            "192.168.1.1:443",
            "192.0.2.1:443",
            "198.18.1.1:443",
            "198.51.100.1:443",
            "203.0.113.1:443",
            "224.0.0.1:443",
            "255.255.255.255:443",
            "[::1]:443",
            "[2606:4700:4700::1111]:443",
            "1.1.1.1:80",
        ] {
            assert!(
                !public_address_allowed(address.parse().unwrap()),
                "{address}"
            );
        }
        assert!(public_address_allowed("1.1.1.1:443".parse().unwrap()));
    }
    #[test]
    fn observation_rejects_unknown_and_duplicate_fields() {
        let bytes = serde_json::to_vec(&candidate()).unwrap();
        let roundtrip: Observation = serde_json::from_slice(&bytes).unwrap();
        assert!(roundtrip.candidate_matches());
        for extra in [r#","catalog_read":5}"#, r#","unexpected":0}"#] {
            let mut bytes = bytes.clone();
            bytes.pop();
            bytes.extend_from_slice(extra.as_bytes());
            assert!(serde_json::from_slice::<Observation>(&bytes).is_err());
        }
    }
}
