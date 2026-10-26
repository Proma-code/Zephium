//! One-child LPAC/Job transport. All parent pipe ends are non-inheritable.
use super::{
    security::{HeldImage, Profile},
    wide,
};
use crate::{
    plan::{self, CommandPlan, Operation},
    transport_control::Fence,
};
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    mem::{size_of, size_of_val},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        Security::{
            EqualSid, GetTokenInformation, TokenAppContainerSid, TokenCapabilities,
            TokenIsAppContainer, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
            TOKEN_APPCONTAINER_INFORMATION, TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_QUERY,
        },
        System::{
            JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            },
            Pipes::{CreatePipe, PeekNamedPipe},
            SystemInformation::GetSystemDirectoryW,
            Threading::*,
        },
    },
};

fn handle(value: &impl AsRawHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}
struct Job(OwnedHandle);
impl Job {
    fn stop(&self) -> Result<(), &'static str> {
        unsafe { TerminateJobObject(handle(&self.0), 1) }.map_err(|_| "job termination unconfirmed")
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
// This guard is installed before any post-create inspection can unwind. Its
// Drop is intentionally blocking: cancellation/panic never releases actual
// profile/image resources while the process or stdin writer is still alive.
struct ChildGuard<'a> {
    process: Option<OwnedHandle>,
    #[cfg(feature = "native-fixtures")]
    store: Option<super::account_store::ActiveStore>,
    job: &'a Job,
    writer: Option<std::thread::JoinHandle<bool>>,
    finished: Arc<AtomicBool>,
    panic_probe: Option<Arc<PanicProbe>>,
}
impl AsRawHandle for ChildGuard<'_> {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.process
            .as_ref()
            .expect("created child handle")
            .as_raw_handle()
    }
}
impl ChildGuard<'_> {
    fn join_writer(&mut self) -> bool {
        let Some(writer) = self.writer.take() else {
            return false;
        };
        let joined = writer.join();
        if let Some(probe) = &self.panic_probe {
            probe
                .writer_panicked
                .store(joined.is_err(), Ordering::Release);
            probe.writer_joined.store(true, Ordering::Release);
        }
        joined.unwrap_or(false)
    }
}
impl ChildGuard<'_> {
    fn settle_physical(&mut self) {
        self.finished.store(true, Ordering::Release);
        if let Some(process) = &self.process {
            terminate_and_reap(self.job, process);
            if let Some(probe) = &self.panic_probe {
                probe.reaped.store(true, Ordering::Release);
            }
        }
        if self.writer.is_some() {
            let _ = self.join_writer();
        }
    }
}
impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        self.settle_physical();
    }
}
// Only debug synthetic qualification constructs a fault probe. No command,
// environment or production caller can request injection.
#[derive(Clone, Copy, Eq, PartialEq)]
enum PanicPoint {
    CreatedSuspended,
    LiveBlockedWriter,
}
struct PanicProbe {
    point: PanicPoint,
    created: AtomicBool,
    injected: AtomicBool,
    reaped: AtomicBool,
    writer_entered: AtomicBool,
    writer_joined: AtomicBool,
    writer_panicked: AtomicBool,
}
#[derive(Debug)]
struct FixturePanic;
fn inject_panic(probe: &Option<Arc<PanicProbe>>, point: PanicPoint) {
    if let Some(probe) = probe.as_ref().filter(|probe| probe.point == point) {
        probe.injected.store(true, Ordering::Release);
        std::panic::panic_any(FixturePanic);
    }
}
struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new() -> Result<Self, &'static str> {
        let mut bytes = 0;
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, 4, None, &mut bytes);
        }
        if bytes == 0 || bytes > 64 * 1024 {
            return Err("process attributes unavailable");
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 4, None, &mut bytes) }
            .map_err(|_| "process attributes unavailable")?;
        Ok(Self {
            _storage: storage,
            list,
        })
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}
fn pipe() -> Result<(OwnedHandle, OwnedHandle), &'static str> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe { CreatePipe(&mut read, &mut write, Some(&attributes), 8192) }
        .map_err(|_| "private pipe creation failed")?;
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read.0),
            OwnedHandle::from_raw_handle(write.0),
        )
    })
}
fn no_inherit(value: &OwnedHandle) -> Result<(), &'static str> {
    unsafe { SetHandleInformation(handle(value), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(|_| "pipe inheritance isolation failed")
}

#[derive(Clone)]
pub(super) struct Layout {
    pub(super) root: PathBuf,
    pub(super) work: PathBuf,
    pub(super) config: PathBuf,
    pub(super) home: PathBuf,
    pub(super) temporary: PathBuf,
    pub(super) appdata: PathBuf,
    pub(super) localappdata: PathBuf,
}
impl Layout {
    pub(super) fn create(root: &Path, profile: &Profile) -> Result<Self, &'static str> {
        // The root is fresh and empty. Seal before any payload/credential can exist.
        profile.seal(root, false)?;
        let result = Self {
            root: root.to_owned(),
            work: root.join("empty-work"),
            config: root.join("official-config"),
            home: root.join("home"),
            temporary: root.join("scratch"),
            appdata: root.join("appdata"),
            localappdata: root.join("localappdata"),
        };
        for path in [
            &result.work,
            &result.config,
            &result.home,
            &result.temporary,
            &result.appdata,
            &result.localappdata,
        ] {
            profile.create_directory(path, path != &result.work)?;
            profile.seal(path, path != &result.work)?;
        }
        Ok(result)
    }
}
#[derive(Clone, Copy)]
pub(super) enum Command {
    Vendor(Operation),
    #[cfg(feature = "native-fixtures")]
    Fixture(Fixture),
}
#[cfg(feature = "native-fixtures")]
#[derive(Clone, Copy)]
pub(super) enum Fixture {
    StoreWrite,
    StoreReplace,
    NetworkBaseline,
    NetworkRegistryRead,
    HandleProbe,
    LoginInput,
    LoginExit,
    TokenAppContainerControl,
    TokenLpacControl,
    VendorVersion,
    VendorHelp,
    VendorLoginHelp,
    VendorStatusHelp,
    VendorLogoutHelp,
    VendorFreshStatus,
    Echo,
    Argv,
    Isolation,
    Flood,
    StderrFlood,
    CancelWait,
    Stall,
}
impl Command {
    #[cfg(feature = "native-fixtures")]
    fn fixture_registry_runtime(self) -> bool {
        // Fixed debug-only candidate list, never a fallback or caller-supplied
        // capability. Real operations and differential baselines remain separate.
        match self {
            Self::Vendor(_) => false,
            Self::Fixture(
                Fixture::NetworkBaseline
                | Fixture::TokenAppContainerControl
                | Fixture::TokenLpacControl,
            ) => false,
            Self::Fixture(
                Fixture::StoreWrite
                | Fixture::StoreReplace
                | Fixture::NetworkRegistryRead
                | Fixture::HandleProbe
                | Fixture::LoginInput
                | Fixture::LoginExit
                | Fixture::VendorVersion
                | Fixture::VendorHelp
                | Fixture::VendorLoginHelp
                | Fixture::VendorStatusHelp
                | Fixture::VendorLogoutHelp
                | Fixture::VendorFreshStatus
                | Fixture::Echo
                | Fixture::Argv
                | Fixture::Isolation
                | Fixture::Flood
                | Fixture::StderrFlood
                | Fixture::CancelWait
                | Fixture::Stall,
            ) => true,
        }
    }
    #[cfg(feature = "native-fixtures")]
    fn fixture_profile_environment(self) -> bool {
        // Repair only ordinary fixed fixtures/startup. Both sides of the network
        // capability differential retain the SAME original environment.
        match self {
            Self::Vendor(_) => false,
            Self::Fixture(
                Fixture::NetworkBaseline
                | Fixture::NetworkRegistryRead
                | Fixture::TokenAppContainerControl
                | Fixture::TokenLpacControl,
            ) => false,
            Self::Fixture(
                Fixture::StoreWrite
                | Fixture::StoreReplace
                | Fixture::HandleProbe
                | Fixture::LoginInput
                | Fixture::LoginExit
                | Fixture::VendorVersion
                | Fixture::VendorHelp
                | Fixture::VendorLoginHelp
                | Fixture::VendorStatusHelp
                | Fixture::VendorLogoutHelp
                | Fixture::VendorFreshStatus
                | Fixture::Echo
                | Fixture::Argv
                | Fixture::Isolation
                | Fixture::Flood
                | Fixture::StderrFlood
                | Fixture::CancelWait
                | Fixture::Stall,
            ) => true,
        }
    }
    #[cfg(feature = "native-fixtures")]
    fn token_probe(self) -> Option<bool> {
        match self {
            Self::Fixture(Fixture::TokenAppContainerControl) => Some(false),
            Self::Fixture(Fixture::TokenLpacControl) => Some(true),
            _ => None,
        }
    }
    fn arguments(self) -> &'static [&'static str] {
        match self {
            Self::Vendor(operation) => CommandPlan::for_operation(operation).arguments(),
            #[cfg(feature = "native-fixtures")]
            Self::Fixture(mode) => match mode {
                Fixture::StoreWrite => &["--child", "store-config-write"],
                Fixture::StoreReplace => &["--child", "store-config-replace"],
                Fixture::NetworkBaseline | Fixture::NetworkRegistryRead => {
                    &["--child", "network-control"]
                }
                Fixture::HandleProbe => &["--child", "handle-probe"],
                Fixture::LoginInput => &["--child", "login-input"],
                Fixture::LoginExit => &["--child", "login-exit"],
                Fixture::TokenAppContainerControl | Fixture::TokenLpacControl => {
                    &["--child", "stall"]
                }
                Fixture::VendorVersion => &["--version"],
                Fixture::VendorHelp => &["--restricted", "--safe-mode", "--help"],
                Fixture::VendorLoginHelp => &["auth", "login", "--help"],
                Fixture::VendorStatusHelp => &["auth", "status", "--help"],
                Fixture::VendorLogoutHelp => &["auth", "logout", "--help"],
                Fixture::VendorFreshStatus => &["auth", "status"],
                Fixture::Echo => &["--child", "echo"],
                Fixture::Argv => &["--child", "argv", "", "end\\", "a\"b"],
                Fixture::Isolation => &["--child", "isolation"],
                Fixture::Flood => &["--child", "flood"],
                Fixture::StderrFlood => &["--child", "stderr-flood"],
                Fixture::CancelWait => &["--child", "cancel-wait"],
                Fixture::Stall => &["--child", "stall"],
            },
        }
    }
    fn deadline(self) -> Duration {
        match self {
            Self::Vendor(operation) => CommandPlan::for_operation(operation).deadline(),
            #[cfg(feature = "native-fixtures")]
            Self::Fixture(Fixture::Stall) => Duration::from_secs(5),
            #[cfg(feature = "native-fixtures")]
            Self::Fixture(Fixture::VendorFreshStatus) => Duration::from_secs(10),
            #[cfg(feature = "native-fixtures")]
            Self::Fixture(_) => Duration::from_secs(15),
        }
    }
}
fn environment(layout: &Layout, initial_localappdata: &Path) -> Result<Vec<u16>, &'static str> {
    let mut system = [0u16; 32768];
    let len = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if len == 0 || len >= system.len() {
        return Err("Windows runtime directory unavailable");
    }
    use std::os::windows::ffi::OsStringExt;
    let system = PathBuf::from(OsString::from_wide(&system[..len]));
    let root = system.parent().ok_or("Windows runtime root unavailable")?;
    let mut entries: Vec<(OsString, OsString)> = vec![
        ("SystemRoot".into(), root.as_os_str().to_owned()),
        ("WINDIR".into(), root.as_os_str().to_owned()),
        ("PATH".into(), system.into_os_string()),
        ("HOME".into(), layout.home.as_os_str().to_owned()),
        ("USERPROFILE".into(), layout.home.as_os_str().to_owned()),
        ("APPDATA".into(), layout.appdata.as_os_str().to_owned()),
        (
            "LOCALAPPDATA".into(),
            initial_localappdata.as_os_str().to_owned(),
        ),
        ("TEMP".into(), layout.temporary.as_os_str().to_owned()),
        ("TMP".into(), layout.temporary.as_os_str().to_owned()),
        (
            "CLAUDE_CONFIG_DIR".into(),
            layout.config.as_os_str().to_owned(),
        ),
    ];
    for (key, value) in [
        ("DISABLE_UPDATES", "1"),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
        ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
        ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
        ("CLAUDE_CODE_AUTO_CONNECT_IDE", "false"),
        ("CLAUDE_CODE_SAFE_MODE", "1"),
        ("NO_COLOR", "1"),
    ] {
        entries.push((key.into(), value.into()));
    }
    entries.sort_by_key(|(key, _)| key.to_string_lossy().to_ascii_uppercase());
    let mut output = Vec::new();
    for (mut key, value) in entries {
        key.push("=");
        key.push(value);
        output.extend(wide(&key)?);
    }
    output.push(0);
    Ok(output)
}

pub(super) struct Output {
    pub(super) turn: Option<crate::protocol::TurnOutcome>,
    pub(super) code: i32,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) stopped: Option<&'static str>,
}
struct RunResult {
    output: Output,
    #[cfg(feature = "native-fixtures")]
    store: Option<super::account_store::ActiveStore>,
}
#[cfg(feature = "native-fixtures")]
pub(super) struct SettledStoreCall {
    operation: super::store_session::Operation,
    active: super::account_store::ActiveStore,
    output: Output,
}
#[cfg(feature = "native-fixtures")]
impl SettledStoreCall {
    pub(super) fn into_parts(
        self,
    ) -> (
        super::account_store::ActiveStore,
        Output,
        super::store_session::Operation,
    ) {
        (self.active, self.output, self.operation)
    }
}
/// Fixed synthetic normal-config operation, never a vendor or arbitrary command.
#[cfg(feature = "native-fixtures")]
pub(super) fn run_store_config(
    active: super::account_store::ActiveStore,
    image: &HeldImage,
    layout: &Layout,
    operation: super::store_session::Operation,
) -> Result<SettledStoreCall, &'static str> {
    let profile = active
        .fixture_profile()
        .map_err(|_| "active profile proof unavailable")?;
    let mut layout = layout.clone();
    layout.config = active
        .config_path()
        .map_err(|_| "active config proof unavailable")?;
    let request = serde_json::to_vec(&serde_json::json!({"config": layout.config}))
        .map_err(|_| "synthetic config request unavailable")?;
    let mut result = run_owned(
        &profile,
        image,
        &layout,
        Command::Fixture(match operation {
            super::store_session::Operation::Write => Fixture::StoreWrite,
            super::store_session::Operation::Replace => Fixture::StoreReplace,
        }),
        Input::Once(zeroize::Zeroizing::new(request)),
        Fence::Atomic(&AtomicBool::new(false)),
        FixtureHooks {
            store: Some(active),
            ..FixtureHooks::default()
        },
    )?;
    let active = result
        .store
        .take()
        .ok_or("settled store custody unavailable")?;
    Ok(SettledStoreCall {
        operation,
        active,
        output: result.output,
    })
}
/// Exclusive native resources for a single fresh-status qualification. This is
/// not a persistent-store constructor and cannot run any other vendor operation.
#[cfg(feature = "native-fixtures")]
pub(super) struct FreshSession {
    owner: crate::custody::Owner,
    image: HeldImage,
    layout: Layout,
    profile: Profile,
}
#[cfg(feature = "native-fixtures")]
impl FreshSession {
    pub(super) fn new(
        owner: crate::custody::Owner,
        image: HeldImage,
        layout: Layout,
        profile: Profile,
    ) -> Self {
        Self {
            owner,
            image,
            layout,
            profile,
        }
    }
    pub(super) fn config(&self) -> &Path {
        &self.layout.config
    }
    pub(super) fn run(&mut self, control: &crate::account::Control) -> SettledCall<'_> {
        let result = if control.owner() != self.owner {
            Err("native process owner mismatch")
        } else {
            run_owned(
                &self.profile,
                &self.image,
                &self.layout,
                Command::Fixture(Fixture::VendorFreshStatus),
                Input::Once(zeroize::Zeroizing::new(Vec::new())),
                Fence::Admitted(control),
                FixtureHooks::default(),
            )
            .map(|result| result.output)
        };
        // run_owned returns only after never-created or physical settlement.
        // A panic/uncertain wait cannot mint this receipt. Its exclusive borrow
        // prevents another call/close on these resources until it is consumed.
        SettledCall {
            result,
            _session: std::marker::PhantomData,
        }
    }
    pub(super) fn close(self) -> Result<(), &'static str> {
        drop(self.image);
        self.profile.close()
    }
}
/// Non-Clone, call-local evidence, not a transferable store-clean authority.
/// There is intentionally no constructor or generic settlement-token export.
#[cfg(feature = "native-fixtures")]
pub(super) struct SettledCall<'a> {
    result: Result<Output, &'static str>,
    _session: std::marker::PhantomData<&'a mut FreshSession>,
}
#[cfg(feature = "native-fixtures")]
impl SettledCall<'_> {
    pub(super) fn into_output(self) -> Result<Output, &'static str> {
        self.result
    }
}
enum Input {
    Once(zeroize::Zeroizing<Vec<u8>>),
    Manual(crate::auth::InputReceiver),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LoginFixtureEvent {
    Created,
    CodeRequested,
}
pub(super) const LOGIN_FIXTURE_PROMPT: &[u8] = b"synthetic-code-request\n";
pub(super) const LOGIN_FIXTURE_COMPLETE: &[u8] =
    b"synthetic-code-request\nsynthetic-code-received\n";

#[cfg(feature = "native-fixtures")]
pub(super) fn run_login_fixture(
    profile: &Profile,
    image: &HeldImage,
    layout: &Layout,
    input: crate::auth::InputReceiver,
    cancelled: &AtomicBool,
    events: SyncSender<LoginFixtureEvent>,
    exit_without_code: bool,
) -> Result<Output, &'static str> {
    let fixture = if exit_without_code {
        Fixture::LoginExit
    } else {
        Fixture::LoginInput
    };
    run_owned(
        profile,
        image,
        layout,
        Command::Fixture(fixture),
        Input::Manual(input),
        Fence::Atomic(cancelled),
        FixtureHooks {
            events: Some(events),
            ..FixtureHooks::default()
        },
    )
    .map(|result| result.output)
}
/// Deliberately unwind only fixed synthetic child stages, and verify actual
/// guard reap/join observations. No arbitrary panic/crash counts as success.
#[cfg(feature = "native-fixtures")]
pub(super) fn panic_controls(
    profile: &Profile,
    image: &HeldImage,
    layout: &Layout,
) -> Result<(), &'static str> {
    for point in [PanicPoint::CreatedSuspended, PanicPoint::LiveBlockedWriter] {
        let probe = Arc::new(PanicProbe {
            point,
            created: AtomicBool::new(false),
            injected: AtomicBool::new(false),
            reaped: AtomicBool::new(false),
            writer_entered: AtomicBool::new(false),
            writer_joined: AtomicBool::new(false),
            writer_panicked: AtomicBool::new(false),
        });
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_owned(
                profile,
                image,
                layout,
                Command::Fixture(Fixture::CancelWait),
                Input::Once(zeroize::Zeroizing::new(vec![b'x'; plan::MAX_STDIN_BYTES])),
                Fence::Atomic(&AtomicBool::new(false)),
                FixtureHooks {
                    panic: Some(Arc::clone(&probe)),
                    ..FixtureHooks::default()
                },
            )
        }));
        if !outcome
            .err()
            .is_some_and(|payload| panic_receipt(payload.as_ref(), &probe))
        {
            return Err("synthetic panic custody was not established");
        }
        super::diagnostic(
            match point {
                PanicPoint::CreatedSuspended => "panic-created-reaped",
                PanicPoint::LiveBlockedWriter => "panic-writer-reaped-joined",
            },
            0,
        );
    }
    Ok(())
}
#[cfg(feature = "native-fixtures")]
fn panic_receipt(payload: &(dyn std::any::Any + Send), probe: &PanicProbe) -> bool {
    payload.is::<FixturePanic>()
        && probe.created.load(Ordering::Acquire)
        && probe.injected.load(Ordering::Acquire)
        && probe.reaped.load(Ordering::Acquire)
        && !probe.writer_panicked.load(Ordering::Acquire)
        && probe.writer_joined.load(Ordering::Acquire)
            == (probe.point == PanicPoint::LiveBlockedWriter)
        && probe.writer_entered.load(Ordering::Acquire)
            == (probe.point == PanicPoint::LiveBlockedWriter)
}
#[cfg(all(test, feature = "native-fixtures"))]
mod panic_receipt_tests {
    use super::*;
    fn observed(point: PanicPoint) -> PanicProbe {
        let writer = point == PanicPoint::LiveBlockedWriter;
        PanicProbe {
            point,
            created: AtomicBool::new(true),
            injected: AtomicBool::new(true),
            reaped: AtomicBool::new(true),
            writer_entered: AtomicBool::new(writer),
            writer_joined: AtomicBool::new(writer),
            writer_panicked: AtomicBool::new(false),
        }
    }
    #[test]
    fn arbitrary_crash_or_missing_physical_observation_never_passes() {
        for point in [PanicPoint::CreatedSuspended, PanicPoint::LiveBlockedWriter] {
            let probe = observed(point);
            assert!(panic_receipt(&FixturePanic, &probe));
            assert!(!panic_receipt(&"other panic", &probe));
            for bit in [
                &probe.created,
                &probe.injected,
                &probe.reaped,
                &probe.writer_entered,
                &probe.writer_joined,
                &probe.writer_panicked,
            ] {
                let original = bit.fetch_xor(true, Ordering::AcqRel);
                assert!(!panic_receipt(&FixturePanic, &probe));
                bit.store(original, Ordering::Release);
            }
        }
    }
}
/// Blocking owning worker. It never returns while a native process/pipe writer is
/// live, even if its caller cancels or its deadline expires. A disconnected UI
/// must set `cancelled`; the worker and physical custody are independent of it.
pub(super) fn run(
    profile: &Profile,
    image: &HeldImage,
    layout: &Layout,
    command: Command,
    input: Vec<u8>,
    cancelled: &AtomicBool,
) -> Result<Output, &'static str> {
    run_owned(
        profile,
        image,
        layout,
        command,
        Input::Once(zeroize::Zeroizing::new(input)),
        Fence::Atomic(cancelled),
        FixtureHooks::default(),
    )
    .map(|result| result.output)
}
#[derive(Default)]
struct FixtureHooks {
    #[cfg(feature = "native-fixtures")]
    store: Option<super::account_store::ActiveStore>,
    events: Option<SyncSender<LoginFixtureEvent>>,
    panic: Option<Arc<PanicProbe>>,
}
fn run_owned(
    profile: &Profile,
    image: &HeldImage,
    layout: &Layout,
    command: Command,
    input: Input,
    control: Fence<'_>,
    hooks: FixtureHooks,
) -> Result<RunResult, &'static str> {
    let FixtureHooks {
        events: fixture_events,
        panic: panic_probe,
        #[cfg(feature = "native-fixtures")]
        store,
    } = hooks;
    // Before creation/preparation, never after spawn.
    let started = Instant::now();
    // Retain independently of controller and stdin-writer lifetime. This lease
    // cannot disappear until run_owned has reaped the child and joined pipes.
    let _admission = match &input {
        Input::Manual(receiver) => Some(receiver.admission()),
        Input::Once(_) => None,
    };
    let mut deadline = control.bound(
        started
            .checked_add(command.deadline())
            .ok_or("deadline unavailable")?,
    );
    if let Input::Manual(receiver) = &input {
        deadline = deadline.min(receiver.deadline());
    }
    if matches!(&input, Input::Once(bytes) if bytes.len() > plan::MAX_STDIN_BYTES) {
        return Err("stdin exceeds bound");
    }
    if matches!(command, Command::Vendor(Operation::StartLogin)) {
        return Err("original login handoff is not qualified");
    }
    if matches!(
        command,
        Command::Vendor(Operation::ReadStatus | Operation::Logout)
    ) && !matches!(&input, Input::Once(bytes) if bytes.is_empty())
    {
        return Err("account inspection and logout require empty stdin");
    }
    let mut decoder = if matches!(command, Command::Vendor(Operation::TextTurn)) {
        let Input::Once(bytes) = &input else {
            return Err("interactive stdin is not model input");
        };
        let text = std::str::from_utf8(bytes).map_err(|_| "invalid turn input")?;
        // The actual spawn boundary validates again; internal callers cannot turn
        // raw stdin into a local slash command or bypass the typed input limit.
        let _validated = plan::TurnInput::new(text).map_err(|_| "invalid turn input")?;
        Some(crate::protocol::TurnDecoder::new())
    } else {
        None
    };
    if control.cancelled(Instant::now()) || Instant::now() >= deadline {
        return Err("cancelled before creation");
    }
    let application = wide(image.path.as_os_str())?;
    let mut line = plan::windows_command_line(
        image
            .path
            .to_str()
            .ok_or("non-Unicode image path rejected")?,
        command.arguments(),
    )
    .map_err(|_| "invalid fixed argv")?;
    let cwd = wide(layout.work.as_os_str())?;
    // This candidate is fixed to the reviewed debug-only fixture/startup list.
    // Keep the own-SID folder/ancestor holds until physical process settlement.
    #[cfg(feature = "native-fixtures")]
    let fixture_environment = if command.fixture_profile_environment() {
        let folder = profile.fixture_folder()?;
        let base = profile.fixture_environment_base(&folder)?;
        Some((folder, base))
    } else {
        None
    };
    #[cfg(feature = "native-fixtures")]
    let initial_localappdata = fixture_environment
        .as_ref()
        .map(|(_, base)| base.as_path())
        .unwrap_or(&layout.localappdata);
    #[cfg(not(feature = "native-fixtures"))]
    let initial_localappdata = &layout.localappdata;
    let env = environment(layout, initial_localappdata)?;
    let raw_job =
        unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|_| "job creation failed")?;
    let job = Job(unsafe { OwnedHandle::from_raw_handle(raw_job.0) });
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    // Candidate bound, not a vendor memory-compatibility claim.
    limits.ProcessMemoryLimit = 768 * 1024 * 1024;
    unsafe {
        SetInformationJobObject(
            raw_job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )
    }
    .map_err(|_| "job limits unavailable")?;
    let (stdout_read, stdout_write) = pipe()?;
    let (stderr_read, stderr_write) = pipe()?;
    let (stdin_read, stdin_write) = pipe()?;
    for parent in [&stdout_read, &stderr_read, &stdin_write] {
        no_inherit(parent)?;
    }
    let attributes = Attributes::new()?;
    let inherited = [
        handle(&stdin_read),
        handle(&stdout_write),
        handle(&stderr_write),
    ];
    let assigned = [raw_job];
    let opt_out: u32 = 1;
    // Research-only ordinary AC control omits the LPAC policy attribute. It is
    // never resumed; all vendor commands retain opt-out=1.
    #[cfg(feature = "native-fixtures")]
    let ordinary_ac_control = command.token_probe() == Some(false);
    #[cfg(not(feature = "native-fixtures"))]
    let ordinary_ac_control = false;
    unsafe {
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inherited.as_ptr().cast()),
            size_of_val(&inherited),
            None,
            None,
        )
        .map_err(|_| "handle whitelist failed")?;
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            Some(assigned.as_ptr().cast()),
            size_of_val(&assigned),
            None,
            None,
        )
        .map_err(|_| "atomic job assignment failed")?;
        if !ordinary_ac_control {
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY as usize,
                Some((&raw const opt_out).cast()),
                size_of::<u32>(),
                None,
                None,
            )
            .map_err(|error| {
                super::diagnostic("lpac-policy-attribute", error.code().0);
                "LPAC policy unavailable"
            })?;
        }
    }
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handle(&stdin_read);
    startup.StartupInfo.hStdOutput = handle(&stdout_write);
    startup.StartupInfo.hStdError = handle(&stderr_write);
    startup.lpAttributeList = attributes.list;
    let mut info = PROCESS_INFORMATION::default();
    // Allocate before creation, so installing the physical guard cannot allocate.
    let writer_finished = Arc::new(AtomicBool::new(false));
    let mut pending = ChildGuard {
        process: None,
        job: &job,
        writer: None,
        finished: Arc::clone(&writer_finished),
        panic_probe: panic_probe.clone(),
        #[cfg(feature = "native-fixtures")]
        store,
    };
    // Security capability storage is live until process creation and token check.
    let create =
        |capabilities: &SECURITY_CAPABILITIES| -> Result<(ChildGuard<'_>, OwnedHandle), &'static str> {
            unsafe {
                UpdateProcThreadAttribute(
                    attributes.list,
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    Some((capabilities as *const SECURITY_CAPABILITIES).cast()),
                    size_of::<SECURITY_CAPABILITIES>(),
                    None,
                    None,
                )
            }
            .map_err(|_| "LPAC capabilities unavailable")?;
            if control.cancelled(Instant::now()) || Instant::now() >= deadline {
                return Err("cancelled before creation");
            }
            let created = unsafe {
                CreateProcessW(
                    PCWSTR(application.as_ptr()),
                    Some(PWSTR(line.as_mut_ptr())),
                    None,
                    None,
                    true,
                    CREATE_NO_WINDOW
                        | CREATE_UNICODE_ENVIRONMENT
                        | EXTENDED_STARTUPINFO_PRESENT
                        | CREATE_SUSPENDED,
                    Some(env.as_ptr().cast()),
                    PCWSTR(cwd.as_ptr()),
                    &startup.StartupInfo,
                    &mut info,
                )
            };
            created.map_err(|error| {
                super::diagnostic("create-process", error.code().0);
                "confined process creation failed"
            })?;
            let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) };
            let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread.0) };
            pending.process = Some(process);
            let process = pending;
            // Attribute values must outlive the list, not only CreateProcessW.
            // This FnOnce owns the list; the physical guard is already installed
            // before its destruction or any subsequent inspection can unwind.
            drop(attributes);
            if let Some(probe) = &panic_probe { probe.created.store(true, Ordering::Release); }
            inject_panic(&panic_probe, PanicPoint::CreatedSuspended);
            #[cfg(feature = "native-fixtures")]
            if let Some(expect_lpac) = command.token_probe() {
                let result = inspect_identity(handle(&process), capabilities).and_then(|token| {
                    super::token_access::verify_control(handle(&token), capabilities, expect_lpac)
                });
                if let Err(error) = result {
                    terminate_and_reap(&job, &process);
                    return Err(error);
                }
                return Ok((process, thread));
            }
            if let Err(error) = verify_token(handle(&process), capabilities) {
                terminate_and_reap(&job, &process);
                return Err(error);
            }
            Ok((process, thread))
        };
    #[cfg(feature = "native-fixtures")]
    let created = if command.fixture_registry_runtime() {
        profile.with_fixture_runtime_capabilities(create)?
    } else {
        profile.with_capabilities(create)
    }?;
    #[cfg(not(feature = "native-fixtures"))]
    let created = profile.with_capabilities(create)?;
    let (mut process, thread) = created;
    #[cfg(feature = "native-fixtures")]
    if command.token_probe().is_some() {
        // Both actual-token controls die suspended. No vendor or synthetic child
        // instruction runs with the ordinary-AC control's weaker authority.
        terminate_and_reap(&job, &process);
        let mut code = 0;
        unsafe { GetExitCodeProcess(handle(&process), &mut code) }
            .map_err(|_| "token control exit unavailable")?;
        return Ok(RunResult {
            output: Output {
                turn: None,
                code: code as i32,
                stdout: Vec::new(),
                stderr: Vec::new(),
                stopped: Some("suspended token control completed"),
            },
            store: None,
        });
    }
    // Parent must close every child-side pipe before any join or EOF observation.
    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);
    if let Some(events) = fixture_events.as_ref() {
        if events.try_send(LoginFixtureEvent::Created).is_err() {
            terminate_and_reap(&job, &process);
            return Err("synthetic auth controller unavailable");
        }
    }
    if control.cancelled(Instant::now())
        || Instant::now() >= deadline
        || unsafe { ResumeThread(handle(&thread)) } == u32::MAX
    {
        terminate_and_reap(&job, &process);
        return Err("cancelled or resume failed");
    }
    drop(thread);
    let writer_signal = Arc::clone(&writer_finished);
    let writer_probe = panic_probe.clone();
    process.writer = Some(
        match std::thread::Builder::new()
            .name("claude-private-stdin".into())
            .spawn(move || {
                let mut file = File::from(stdin_write);
                if let Some(probe) = writer_probe {
                    probe.writer_entered.store(true, Ordering::Release);
                }
                let result = match input {
                    Input::Once(mut input) => {
                        let result = file.write_all(&input).is_ok();
                        input.fill(0);
                        result
                    }
                    Input::Manual(receiver) => {
                        receiver.write_one(&mut file, &writer_signal).is_ok()
                    }
                };
                drop(file);
                result
            }) {
            Ok(writer) => writer,
            Err(_) => {
                terminate_and_reap(&job, &process);
                return Err("stdin writer unavailable");
            }
        },
    );
    let mut stdout = File::from(stdout_read);
    let mut stderr = File::from(stderr_read);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_closed = false;
    let mut err_closed = false;
    let mut exit = None;
    let mut stopped = None;
    let mut prompt_sent = false;
    loop {
        if stopped.is_none() && (control.cancelled(Instant::now()) || Instant::now() >= deadline) {
            stopped = Some("cancelled or deadline exceeded");
        }
        let previous_stdout = out.len();
        for (file, bytes, closed, max) in [
            (
                &mut stdout,
                &mut out,
                &mut out_closed,
                plan::MAX_STDOUT_BYTES,
            ),
            (
                &mut stderr,
                &mut err,
                &mut err_closed,
                plan::MAX_STDERR_BYTES,
            ),
        ] {
            if !*closed {
                if let Err(failure) = read_available(file, bytes, closed, max, stopped.is_some()) {
                    stopped.get_or_insert(failure);
                }
            }
        }
        if panic_probe.as_ref().is_some_and(|probe| {
            probe.point == PanicPoint::LiveBlockedWriter
                && out == b"ready"
                && !out_closed
                && !err_closed
                && probe.writer_entered.load(Ordering::Acquire)
                && process
                    .writer
                    .as_ref()
                    .is_some_and(|writer| !writer.is_finished())
        }) {
            inject_panic(&panic_probe, PanicPoint::LiveBlockedWriter);
        }
        if stopped.is_none() {
            if let Some(decoder) = decoder.as_mut() {
                if decoder.push(&out[previous_stdout..]).is_err() {
                    stopped = Some("vendor protocol mismatch");
                }
            }
            if let Some(events) = fixture_events.as_ref() {
                // Only the fixed synthetic helper emits this grammar. This must
                // never be used as an original vendor authentication parser.
                if !LOGIN_FIXTURE_COMPLETE.starts_with(&out) {
                    stopped = Some("synthetic auth prompt mismatch");
                } else if !prompt_sent && out.len() >= LOGIN_FIXTURE_PROMPT.len() {
                    if events.try_send(LoginFixtureEvent::CodeRequested).is_err() {
                        stopped = Some("synthetic auth controller unavailable");
                    } else {
                        prompt_sent = true;
                    }
                }
            }
        }
        if stopped.is_some() {
            writer_finished.store(true, Ordering::Release);
            let _ = job.stop();
        }
        if exit.is_none() {
            match unsafe { WaitForSingleObject(handle(&process), 0) } {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    if unsafe { GetExitCodeProcess(handle(&process), &mut code) }.is_err() {
                        stopped.get_or_insert("exit status unavailable");
                    }
                    exit = Some(code as i32);
                    writer_finished.store(true, Ordering::Release);
                }
                WAIT_TIMEOUT => {}
                _ => {
                    stopped.get_or_insert("process wait unconfirmed");
                    let _ = job.stop();
                }
            }
        }
        if exit.is_some() && out_closed && err_closed {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // No slot release until writer settlement; child-side handles are already gone.
    if !process.join_writer() {
        stopped.get_or_insert("stdin interrupted");
    }
    let code = exit.ok_or("process exit unconfirmed")?;
    let turn = if stopped.is_none() {
        match decoder.map(|decoder| decoder.finish(code)) {
            Some(Ok(outcome)) => Some(outcome),
            Some(Err(_)) => {
                stopped = Some("vendor result incomplete or incompatible");
                None
            }
            None => None,
        }
    } else {
        None
    };
    // No retained store can leave actual guard custody while even parent output
    // handles remain open. Reap/join is confirmed again before moving its owner.
    drop(stdout);
    drop(stderr);
    process.settle_physical();
    Ok(RunResult {
        output: Output {
            turn,
            code,
            stdout: out,
            stderr: err,
            stopped,
        },
        #[cfg(feature = "native-fixtures")]
        store: process.store.take(),
    })
}
// Even failed setup retains the held image/profile/job and process custody until
// the kernel confirms exit. WAIT_FAILED is not a successful reap; no timeout
// releases an uncertain process into a newly admitted account operation.
fn terminate_and_reap(job: &Job, process: &impl AsRawHandle) {
    loop {
        let _ = job.stop();
        if unsafe { WaitForSingleObject(handle(process), 50) } == WAIT_OBJECT_0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn read_available(
    file: &mut File,
    output: &mut Vec<u8>,
    closed: &mut bool,
    maximum: usize,
    discard: bool,
) -> Result<(), &'static str> {
    let mut available = 0;
    match unsafe { PeekNamedPipe(handle(file), None, 0, None, Some(&mut available), None) } {
        Ok(()) => {}
        Err(error) if error.code().0 as u32 == 0x8007_006d => {
            *closed = true;
            return Ok(());
        }
        Err(_) => {
            *closed = true;
            return Err("pipe read failed");
        }
    }
    if available == 0 {
        return Ok(());
    }
    let mut buffer = [0u8; 8192];
    let length = (available as usize).min(buffer.len());
    let count = file
        .read(&mut buffer[..length])
        .map_err(|_| "pipe read failed")?;
    if count == 0 {
        *closed = true;
        return Ok(());
    }
    if discard {
        return Ok(());
    }
    if count > maximum.saturating_sub(output.len()) {
        return Err("output bound exceeded");
    }
    output.extend_from_slice(&buffer[..count]);
    Ok(())
}
fn verify_token(process: HANDLE, capabilities: &SECURITY_CAPABILITIES) -> Result<(), &'static str> {
    let token = inspect_identity(process, capabilities)?;
    super::token_access::verify_lpac(handle(&token), capabilities)
}
fn inspect_boolean(
    token: &OwnedHandle,
    class: windows::Win32::Security::TOKEN_INFORMATION_CLASS,
) -> Result<(), &'static str> {
    let mut value = 0u32;
    let mut length = 0;
    unsafe {
        GetTokenInformation(
            handle(token),
            class,
            Some((&raw mut value).cast()),
            size_of::<u32>() as u32,
            &mut length,
        )
    }
    .map_err(|error| {
        super::diagnostic("token-information-class", class.0);
        super::diagnostic("token-information-query", error.code().0);
        "LPAC token inspection failed"
    })?;
    if length != size_of::<u32>() as u32 || value != 1 {
        super::diagnostic("token-information-class", class.0);
        super::diagnostic("token-information-length", length as i32);
        super::diagnostic("token-information-value", value as i32);
        return Err("LPAC token was not applied");
    }
    Ok(())
}
fn inspect_identity(
    process: HANDLE,
    capabilities: &SECURITY_CAPABILITIES,
) -> Result<OwnedHandle, &'static str> {
    let mut raw = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY | TOKEN_DUPLICATE, &mut raw) }.map_err(
        |error| {
            super::diagnostic("open-process-token", error.code().0);
            "token inspection failed"
        },
    )?;
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    inspect_boolean(&token, TokenIsAppContainer)?;
    let mut buffer = [0usize; 128];
    let mut length = 0;
    unsafe {
        GetTokenInformation(
            handle(&token),
            TokenAppContainerSid,
            Some(buffer.as_mut_ptr().cast()),
            size_of_val(&buffer) as u32,
            &mut length,
        )
    }
    .map_err(|error| {
        super::diagnostic("token-appcontainer-sid-query", error.code().0);
        "LPAC identity unavailable"
    })?;
    if length < size_of::<TOKEN_APPCONTAINER_INFORMATION>() as u32
        || length as usize > size_of_val(&buffer)
    {
        return Err("LPAC identity invalid");
    }
    let info = unsafe { &*buffer.as_ptr().cast::<TOKEN_APPCONTAINER_INFORMATION>() };
    unsafe { EqualSid(info.TokenAppContainer, capabilities.AppContainerSid) }
        .map_err(|_| "LPAC identity mismatch")?;
    buffer.fill(0);
    length = 0;
    unsafe {
        GetTokenInformation(
            handle(&token),
            TokenCapabilities,
            Some(buffer.as_mut_ptr().cast()),
            size_of_val(&buffer) as u32,
            &mut length,
        )
    }
    .map_err(|error| {
        super::diagnostic("token-capabilities-query", error.code().0);
        "LPAC capabilities unavailable"
    })?;
    if length < size_of::<TOKEN_GROUPS>() as u32 || length as usize > size_of_val(&buffer) {
        return Err("LPAC capabilities invalid");
    }
    let groups = unsafe { &*buffer.as_ptr().cast::<TOKEN_GROUPS>() };
    let count = capabilities.CapabilityCount as usize;
    if (count != 1 && !(cfg!(feature = "native-fixtures") && count == 2))
        || groups.GroupCount as usize != count
        || std::mem::offset_of!(TOKEN_GROUPS, Groups)
            + count * size_of::<windows::Win32::Security::SID_AND_ATTRIBUTES>()
            > length as usize
    {
        super::diagnostic("capability-count", groups.GroupCount as i32);
        return Err("unexpected LPAC capability count");
    }
    // Match the exact finite expected set, including attributes and uniqueness;
    // Windows may reorder capabilities. No unknown or duplicate SID is admitted.
    let actual = unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), count) };
    let expected = unsafe { std::slice::from_raw_parts(capabilities.Capabilities, count) };
    if count == 2 && unsafe { EqualSid(expected[0].Sid, expected[1].Sid) }.is_ok() {
        return Err("duplicate expected LPAC capability");
    }
    for wanted in expected {
        if wanted.Attributes != 4
            || actual
                .iter()
                .filter(|found| unsafe { EqualSid(found.Sid, wanted.Sid) }.is_ok())
                .count()
                != 1
        {
            return Err("unexpected LPAC capability");
        }
    }
    if actual.iter().any(|entry| entry.Attributes != 4) {
        return Err("unexpected LPAC capability attributes");
    }
    Ok(token)
}

#[cfg(all(test, feature = "native-fixtures"))]
mod research_admission_tests {
    use super::*;
    #[test]
    fn fresh_status_has_only_fixed_read_command_and_bound() {
        let command = Command::Fixture(Fixture::VendorFreshStatus);
        assert_eq!(command.arguments(), ["auth", "status"]);
        assert_eq!(command.deadline(), Duration::from_secs(10));
    }
    #[test]
    fn auth_metadata_has_no_variant_without_help() {
        for (mode, command) in [
            (Fixture::VendorLoginHelp, "login"),
            (Fixture::VendorStatusHelp, "status"),
            (Fixture::VendorLogoutHelp, "logout"),
        ] {
            assert_eq!(
                Command::Fixture(mode).arguments(),
                ["auth", command, "--help"]
            );
        }
    }
    #[test]
    fn environment_repair_preserves_both_sides_of_capability_differential() {
        for operation in [
            Operation::ReadStatus,
            Operation::StartLogin,
            Operation::Logout,
            Operation::TextTurn,
        ] {
            assert!(!Command::Vendor(operation).fixture_profile_environment());
        }
        for fixture in [
            Fixture::NetworkBaseline,
            Fixture::NetworkRegistryRead,
            Fixture::TokenAppContainerControl,
            Fixture::TokenLpacControl,
        ] {
            assert!(!Command::Fixture(fixture).fixture_profile_environment());
        }
        for fixture in [
            Fixture::StoreWrite,
            Fixture::StoreReplace,
            Fixture::VendorVersion,
            Fixture::VendorHelp,
            Fixture::VendorLoginHelp,
            Fixture::VendorStatusHelp,
            Fixture::VendorLogoutHelp,
            Fixture::VendorFreshStatus,
            Fixture::HandleProbe,
            Fixture::LoginInput,
            Fixture::LoginExit,
            Fixture::Echo,
            Fixture::Argv,
            Fixture::Isolation,
            Fixture::Flood,
            Fixture::StderrFlood,
            Fixture::CancelWait,
            Fixture::Stall,
        ] {
            assert!(Command::Fixture(fixture).fixture_profile_environment());
        }
    }
    #[test]
    fn fixture_runtime_capability_is_never_a_real_operation_or_baseline() {
        for operation in [
            Operation::ReadStatus,
            Operation::StartLogin,
            Operation::Logout,
            Operation::TextTurn,
        ] {
            assert!(!Command::Vendor(operation).fixture_registry_runtime());
        }
        for fixture in [
            Fixture::NetworkBaseline,
            Fixture::TokenLpacControl,
            Fixture::TokenAppContainerControl,
        ] {
            assert!(!Command::Fixture(fixture).fixture_registry_runtime());
        }
        for fixture in [
            Fixture::StoreWrite,
            Fixture::StoreReplace,
            Fixture::NetworkRegistryRead,
            Fixture::VendorVersion,
            Fixture::VendorHelp,
            Fixture::VendorLoginHelp,
            Fixture::VendorStatusHelp,
            Fixture::VendorLogoutHelp,
            Fixture::VendorFreshStatus,
            Fixture::Isolation,
            Fixture::LoginInput,
            Fixture::LoginExit,
            Fixture::HandleProbe,
            Fixture::Echo,
            Fixture::Argv,
            Fixture::Flood,
            Fixture::StderrFlood,
            Fixture::CancelWait,
            Fixture::Stall,
        ] {
            assert!(Command::Fixture(fixture).fixture_registry_runtime());
        }
        assert_eq!(
            Command::Fixture(Fixture::NetworkRegistryRead).arguments(),
            &["--child", "network-control"]
        );
    }
}
