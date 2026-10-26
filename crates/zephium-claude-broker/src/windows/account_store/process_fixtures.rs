//! Fixed same-user cooperating broker helper, not a vendor/LPAC runner.
//! The fresh parent transfers a designated stdin directory capability. Unkeyed
//! bindings prove its relationship, not authentication against a malicious user.
//! No path, persisted receipt, arbitrary executable or general command is imported.
use super::super::security::{self, HeldImage};
use super::*;
use std::{
    ffi::OsString,
    mem::{size_of, size_of_val},
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
    time::{Duration, Instant},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            DuplicateHandle, SetHandleInformation, DUPLICATE_HANDLE_OPTIONS, HANDLE, HANDLE_FLAGS,
            HANDLE_FLAG_INHERIT, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            JobObjects::{
                CreateJobObjectW, JobObjectBasicAccountingInformation,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject, TerminateJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Pipes::{CreatePipe, PeekNamedPipe},
            Threading::*,
        },
    },
};

const KEY: FixtureKey = FixtureKey {
    domain: FixtureDomain::TrialModel,
    profile: 17,
    incarnation: 23,
};
const OUTPUT_BOUND: usize = 4096;
const RUN_LIMIT: Duration = Duration::from_secs(10);
const STOP_LIMIT: Duration = Duration::from_secs(10);
const TIMEOUT_READY: &[u8] = b"store-child-waiting\n";
const READY_LIMIT: Duration = Duration::from_millis(250);

fn deadline_error(elapsed: Duration, limit: Duration) -> Option<&'static str> {
    (elapsed >= limit).then_some("store child deadline exceeded")
}

fn admit_prepared_child(elapsed: Duration) -> Result<(), &'static str> {
    if deadline_error(elapsed, RUN_LIMIT).is_some() {
        return Err("store child preparation deadline exceeded");
    }
    Ok(())
}

fn api_error(
    stage: &'static str,
    message: &'static str,
    error: windows::core::Error,
) -> &'static str {
    super::super::diagnostic(stage, error.code().0);
    message
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Busy,
    Clean,
    Abandon,
    Timeout,
    Overflow,
}
impl Mode {
    fn argument(self) -> &'static str {
        match self {
            Self::Busy => "store-custody-busy",
            Self::Clean => "store-custody-clean",
            Self::Abandon => "store-custody-abandon",
            Self::Timeout => "store-custody-timeout",
            Self::Overflow => "store-custody-overflow",
        }
    }
    fn output(self) -> &'static [u8] {
        match self {
            Self::Busy => b"store-child-busy\n",
            Self::Clean => b"store-child-clean\n",
            Self::Abandon => b"store-child-dirty\n",
            Self::Timeout => b"store-child-timeout\n",
            Self::Overflow => b"store-child-overflow\n",
        }
    }
}
fn fixture_deadline(
    mode: Mode,
    elapsed: Duration,
    ready: Option<Duration>,
) -> Option<&'static str> {
    if elapsed >= RUN_LIMIT && mode == Mode::Timeout && ready.is_none() {
        return Some("store child readiness deadline exceeded");
    }
    deadline_error(elapsed, RUN_LIMIT).or_else(|| {
        if mode == Mode::Timeout {
            ready.and_then(|elapsed| deadline_error(elapsed, READY_LIMIT))
        } else {
            None
        }
    })
}
struct Bindings([[u8; 32]; 3]);
fn encode(value: &[u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn decode(value: &std::ffi::OsStr) -> Result<[u8; 32], &'static str> {
    let value = value.to_str().ok_or("store child binding invalid")?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("store child binding invalid");
    }
    let mut result = [0; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "store child binding invalid")?;
    }
    Ok(result)
}
fn request(arguments: &[OsString]) -> Result<(Mode, Bindings), &'static str> {
    let [flag, mode, root, lock, state] = arguments else {
        return Err("store child invocation invalid");
    };
    if flag != "--child" {
        return Err("store child invocation invalid");
    }
    let mode = match mode.to_str() {
        Some("store-custody-busy") => Mode::Busy,
        Some("store-custody-clean") => Mode::Clean,
        Some("store-custody-abandon") => Mode::Abandon,
        Some("store-custody-timeout") => Mode::Timeout,
        Some("store-custody-overflow") => Mode::Overflow,
        _ => return Err("store child invocation invalid"),
    };
    Ok((
        mode,
        Bindings([decode(root)?, decode(lock)?, decode(state)?]),
    ))
}

fn fresh() -> Result<(FixtureStoreOwner, StoreLease, HeldImage), &'static str> {
    let path = std::env::current_exe().map_err(|_| "store fixture image unavailable")?;
    if path.file_name() != Some(std::ffi::OsStr::new("claude-broker-fixture.exe")) {
        return Err("store fixture image identity invalid");
    }
    let expected =
        security::hash(&mut File::open(&path).map_err(|_| "store fixture image unavailable")?)?;
    let image = HeldImage::open(&path, expected)?;
    let suffix = native::fresh_name().map_err(|_| "store fixture nonce unavailable")?;
    let suffix = suffix
        .strip_prefix("Zephium.ClaudeStoreFixture.")
        .ok_or("store fixture nonce invalid")?;
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}"))?;
    let (owner, lease) =
        FixtureStoreOwner::create(profile, KEY).map_err(|_| "store fixture creation failed")?;
    Ok((owner, lease, image))
}

/// Narrow later fixture-binary wrapper; accepts no caller root or executable.
pub(crate) fn controls() -> Result<(), &'static str> {
    let (owner, lease, image) = fresh()?;
    let active = lease
        .begin_operation()
        .map_err(|_| "store fixture activation failed")?;
    let context = Arc::new(Context::new(&lease, image)?);
    let proof = run_child(
        Mode::Busy,
        Arc::clone(&context),
        Some(Arc::clone(&lease.inner)),
    )?;
    // A native owning return boundary, not a caller boolean or vendor authority.
    active
        .settle(proof)
        .map_err(|_| "store fixture settlement failed")?;
    lease
        .verify()
        .map_err(|_| "store fixture settled proof failed")?;
    super::super::diagnostic("store-process-parent-custody", 0);
    drop(lease);

    let _clean_child_settled = run_child(Mode::Clean, Arc::clone(&context), None)?;
    let reopened = StoreLease::reopen(&owner.receipt(), KEY)
        .map_err(|_| "store fixture clean reopen failed")?;
    reopened
        .verify()
        .map_err(|_| "store fixture reopened proof failed")?;
    drop(reopened);
    super::super::diagnostic("store-process-clean-release", 0);

    let _dirty_child_settled = run_child(Mode::Abandon, context, None)?;
    if !matches!(StoreLease::reopen(&owner.receipt(), KEY), Err(Error::Dirty)) {
        return Err("store fixture abandoned state was admitted");
    }
    super::super::diagnostic("store-process-dirty-refusal", 0);
    for (mode, expected_error, stage) in [
        (
            Mode::Timeout,
            "store child deadline exceeded",
            "store-process-timeout-dirty",
        ),
        (
            Mode::Overflow,
            "store child output bound exceeded",
            "store-process-overflow-dirty",
        ),
    ] {
        failure_keeps_dirty(mode, expected_error)?;
        super::super::diagnostic(stage, 0);
    }
    // All roots remain deliberately retained. No retirement/crash recovery claim.
    Ok(())
}

fn failure_keeps_dirty(mode: Mode, expected_error: &'static str) -> Result<(), &'static str> {
    let (owner, lease, image) = fresh()?;
    let receipt = owner.receipt();
    let active = lease
        .begin_operation()
        .map_err(|_| "store negative activation failed")?;
    let context = Arc::new(Context::new(&lease, image)?);
    match run_child(mode, context, Some(Arc::clone(&lease.inner))) {
        Err(error) if error == expected_error => {}
        _ => return Err("store negative outcome mismatch"),
    }
    // Only a physically settled closed error is accepted above. In particular,
    // an unresolved stop/pipe is not a passing timeout or overflow experiment.
    lease
        .inner
        .require_state(DIRTY)
        .map_err(|_| "store negative dirty proof failed")?;
    active
        .config_path()
        .map_err(|_| "store negative active proof failed")?;
    drop(active); // No proof was returned: deliberately quarantine, never clear.
    if !matches!(lease.verify(), Err(Error::Quarantined)) {
        return Err("store negative quarantine missing");
    }
    drop(lease);
    drop(owner);
    if !matches!(StoreLease::reopen(&receipt, KEY), Err(Error::Busy)) {
        return Err("store negative custody was released");
    }
    Ok(())
}

/// Fixed child entrypoint. stdin is a directory HANDLE, never a textual request.
pub(crate) fn child() -> Result<(), &'static str> {
    let arguments: Vec<_> = std::env::args_os().skip(1).take(6).collect();
    let (mode, bindings) = request(&arguments)?;
    let root = File::from(
        std::io::stdin()
            .as_handle()
            .try_clone_to_owned()
            .map_err(|_| "store child capability unavailable")?,
    );
    let (root_binding, owner) =
        native::fixture_binding(&root, true).map_err(|_| "store child root proof failed")?;
    if root_binding != bindings.0[0] {
        return Err("store child root binding mismatch");
    }
    let policy = Policy::broker_file(&owner);
    let lock =
        native::open_file(&root, LOCK, &policy).map_err(|_| "store child lock proof failed")?;
    if native::fixture_binding(&lock, false)
        .map_err(|_| "store child lock binding failed")?
        .0
        != bindings.0[1]
        || lock
            .metadata()
            .map_err(|_| "store child lock metadata failed")?
            .len()
            != 0
    {
        return Err("store child lock binding mismatch");
    }
    match (mode, native::lock_exclusive(&lock)) {
        (Mode::Busy | Mode::Timeout | Mode::Overflow, Err(Error::Busy)) => {}
        (Mode::Busy | Mode::Timeout | Mode::Overflow, _) => {
            return Err("store child parent lease was not exclusive")
        }
        (_, Ok(())) => {
            let state = native::open_file(&root, STATE, &policy)
                .map_err(|_| "store child state proof failed")?;
            if native::fixture_binding(&state, false)
                .map_err(|_| "store child state binding failed")?
                .0
                != bindings.0[2]
            {
                return Err("store child state binding mismatch");
            }
            require_file_state(&state, CLEAN).map_err(|_| "store child clean marker refused")?;
            if mode == Mode::Abandon {
                write_file_state(&state, DIRTY)
                    .map_err(|_| "store child dirty persistence failed")?;
                require_file_state(&state, DIRTY).map_err(|_| "store child dirty proof failed")?;
                // Deliberately keep the independently opened OS lock until this
                // exact process exits. No Drop path restores CLEAN.
                std::mem::forget(lock);
            }
        }
        (_, Err(_)) => return Err("store child released lease unavailable"),
    }
    if mode == Mode::Timeout {
        std::io::stdout()
            .write_all(TIMEOUT_READY)
            .map_err(|_| "store child readiness receipt failed")?;
        std::io::stdout()
            .flush()
            .map_err(|_| "store child readiness flush failed")?;
        std::thread::sleep(Duration::from_secs(2));
    } else if mode == Mode::Overflow {
        return std::io::stdout()
            .write_all(&[b'x'; OUTPUT_BOUND + 1])
            .map_err(|_| "store child overflow receipt failed");
    }
    std::io::stdout()
        .write_all(mode.output())
        .map_err(|_| "store child receipt failed")
}

struct Context {
    image: HeldImage,
    root: File,
    bindings: Bindings,
    _receipt: InProcessFixtureReceipt,
    _ancestors: Vec<File>,
    _config: File,
}
impl Context {
    fn new(lease: &StoreLease, image: HeldImage) -> Result<Self, &'static str> {
        lease
            .verify()
            .map_err(|_| "store handoff source proof failed")?;
        let root = &lease
            .inner
            .chain
            .last()
            .ok_or("store handoff root unavailable")?
            .file;
        let state = lease
            .inner
            .state
            .lock()
            .map_err(|_| "store handoff state unavailable")?;
        let bindings = Bindings([
            native::fixture_binding(root, true)
                .map_err(|_| "store handoff root proof failed")?
                .0,
            native::fixture_binding(&lease.inner.lock, false)
                .map_err(|_| "store handoff lock proof failed")?
                .0,
            native::fixture_binding(&state, false)
                .map_err(|_| "store handoff state proof failed")?
                .0,
        ]);
        Ok(Self {
            image,
            root: root
                .try_clone()
                .map_err(|_| "store handoff root hold failed")?,
            bindings,
            _receipt: lease.inner.receipt.clone(),
            _ancestors: lease
                .inner
                .chain
                .iter()
                .map(|entry| entry.file.try_clone())
                .collect::<Result<_, _>>()
                .map_err(|_| "store handoff ancestor hold failed")?,
            _config: lease
                .inner
                .config
                .file
                .try_clone()
                .map_err(|_| "store handoff config hold failed")?,
        })
    }
}
fn handle(value: &impl AsRawHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}
fn pipe() -> Result<(File, OwnedHandle), &'static str> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe { CreatePipe(&mut read, &mut write, Some(&attributes), 8192) }
        .map_err(|_| "store child pipe creation failed")?;
    let read = unsafe { File::from_raw_handle(read.0) };
    let write = unsafe { OwnedHandle::from_raw_handle(write.0) };
    unsafe { SetHandleInformation(handle(&read), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(|_| "store child pipe isolation failed")?;
    Ok((read, write))
}
struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new() -> Result<Self, &'static str> {
        let mut bytes = 0;
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, 2, None, &mut bytes);
        }
        if bytes == 0 || bytes > 64 * 1024 {
            return Err("store child attributes unavailable");
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &mut bytes) }
            .map_err(|_| "store child attributes unavailable")?;
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
struct Capture {
    file: File,
    bytes: Vec<u8>,
    eof: bool,
}
impl Capture {
    fn read_available(&mut self) -> Result<(), &'static str> {
        if self.eof {
            return Ok(());
        }
        let mut available = 0;
        match unsafe {
            PeekNamedPipe(
                handle(&self.file),
                None,
                0,
                None,
                Some(&mut available),
                None,
            )
        } {
            Ok(()) => {}
            Err(error) if error.code().0 as u32 == 0x8007_006d => {
                self.eof = true;
                return Ok(());
            }
            Err(_) => return Err("store child pipe read unconfirmed"),
        }
        if available == 0 {
            return Ok(());
        }
        let mut bytes = [0; 1024];
        let count = self
            .file
            .read(&mut bytes[..(available as usize).min(1024)])
            .map_err(|_| "store child pipe read unconfirmed")?;
        if count == 0 {
            self.eof = true;
            return Ok(());
        }
        if count > OUTPUT_BOUND.saturating_sub(self.bytes.len()) {
            return Err("store child output bound exceeded");
        }
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(())
    }
}
struct RunningHandles {
    job: OwnedHandle,
    process: OwnedHandle,
    stdout: Capture,
    stderr: Capture,
    _context: Arc<Context>,
    _parent_lease: Option<Arc<Inner>>,
}
struct Running {
    handles: Option<RunningHandles>,
    settled: bool,
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.settled {
            if let Some(handles) = self.handles.take() {
                let _ = unsafe { TerminateJobObject(handle(&handles.job), 1) };
                // Termination request is NOT settlement. Preserve every hold on
                // uncertainty/panic until the outer synthetic process exits.
                std::mem::forget(handles);
            }
        }
    }
}

fn run_child(
    mode: Mode,
    context: Arc<Context>,
    parent_lease: Option<Arc<Inner>>,
) -> Result<WorkerSettled, &'static str> {
    // Includes fixed launch preparation; expiry never becomes a success merely
    // because process/pipe settlement was first observed after the budget.
    let started = Instant::now();
    let proofs = context.bindings.0.each_ref().map(encode);
    let image = context
        .image
        .path
        .to_str()
        .ok_or("store fixture image spelling invalid")?;
    let mut line = crate::plan::windows_command_line(
        image,
        &[
            "--child",
            mode.argument(),
            &proofs[0],
            &proofs[1],
            &proofs[2],
        ],
    )
    .map_err(|_| "store child command invalid")?;
    let application = super::super::wide(context.image.path.as_os_str())?;
    let cwd = native::canonical_parent(&context.root)
        .map_err(|_| "store child directory proof failed")?;
    let cwd = super::super::wide(cwd.as_os_str())?;
    let mut stdin = HANDLE::default();
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle(&context.root),
            GetCurrentProcess(),
            &mut stdin,
            0x0012_00a9,
            true,
            DUPLICATE_HANDLE_OPTIONS(0),
        )
    }
    .map_err(|_| "store child capability transfer failed")?;
    let stdin = unsafe { OwnedHandle::from_raw_handle(stdin.0) };
    let (stdout, stdout_write) = pipe()?;
    let (stderr, stderr_write) = pipe()?;
    let raw_job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
        .map_err(|_| "store child job unavailable")?;
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job.0) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    unsafe {
        SetInformationJobObject(
            raw_job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )
    }
    .map_err(|_| "store child job limit unavailable")?;
    let inherited = [handle(&stdin), handle(&stdout_write), handle(&stderr_write)];
    let assigned = [raw_job];
    let attributes = Attributes::new()?;
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
        .map_err(|_| "store child handle list unavailable")?;
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            Some(assigned.as_ptr().cast()),
            size_of_val(&assigned),
            None,
            None,
        )
        .map_err(|_| "store child job assignment unavailable")?;
    }
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handle(&stdin);
    startup.StartupInfo.hStdOutput = handle(&stdout_write);
    startup.StartupInfo.hStdError = handle(&stderr_write);
    startup.lpAttributeList = attributes.list;
    let mut info = PROCESS_INFORMATION::default();
    let environment = [0u16; 2]; // No inherited credential/provider/environment text.
    admit_prepared_child(started.elapsed())?;
    let created = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            Some(environment.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            &startup.StartupInfo,
            &mut info,
        )
    };
    drop(attributes); // Attribute values above remain live through list destruction.
    created.map_err(|error| {
        api_error(
            "store-process-create",
            "store child process creation failed",
            error,
        )
    })?;
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) };
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread.0) };
    let mut running = Running {
        handles: Some(RunningHandles {
            job,
            process,
            stdout: Capture {
                file: stdout,
                bytes: Vec::new(),
                eof: false,
            },
            stderr: Capture {
                file: stderr,
                bytes: Vec::new(),
                eof: false,
            },
            _context: context,
            _parent_lease: parent_lease,
        }),
        settled: false,
    };
    drop(thread);
    drop(stdin);
    drop(stdout_write);
    drop(stderr_write);
    let mut stopped = None;
    let mut ready_at: Option<Instant> = None;
    let mut failure = None;
    loop {
        failure = failure
            .or_else(|| fixture_deadline(mode, started.elapsed(), ready_at.map(|at| at.elapsed())));
        let handles = running
            .handles
            .as_mut()
            .ok_or("store child custody unavailable")?;
        for stream in [&mut handles.stdout, &mut handles.stderr] {
            if let Err(error) = stream.read_available() {
                failure.get_or_insert(error);
            }
        }
        if mode == Mode::Timeout && ready_at.is_none() && handles.stdout.bytes == TIMEOUT_READY {
            ready_at = Some(Instant::now());
        }
        let exited = match unsafe { WaitForSingleObject(handle(&handles.process), 0) } {
            WAIT_OBJECT_0 => true,
            WAIT_TIMEOUT => false,
            _ => {
                failure.get_or_insert("store child exit unconfirmed");
                false
            }
        };
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let job_empty = match unsafe {
            QueryInformationJobObject(
                Some(handle(&handles.job)),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of_val(&accounting) as u32,
                None,
            )
        } {
            Ok(()) => accounting.ActiveProcesses == 0,
            Err(_) => {
                failure.get_or_insert("store child job settlement unconfirmed");
                false
            }
        };
        if exited && job_empty && handles.stdout.eof && handles.stderr.eof {
            running.settled = true;
            if let Some(error) = failure {
                return Err(error);
            }
            let mut code = 1;
            unsafe { GetExitCodeProcess(handle(&handles.process), &mut code) }
                .map_err(|_| "store child exit code unavailable")?;
            if code != 0
                || handles.stdout.bytes != mode.output()
                || !handles.stderr.bytes.is_empty()
            {
                return Err("store child fixed receipt mismatch");
            }
            if let Some(error) =
                fixture_deadline(mode, started.elapsed(), ready_at.map(|at| at.elapsed()))
            {
                return Err(error);
            }
            // No reusable/public constructor: only this exact synthetic owner,
            // after process exit, Job0 and both synchronously drained pipe EOFs.
            return Ok(WorkerSettled { _private: () });
        }
        if failure.is_some() && stopped.is_none() {
            let _ = unsafe { TerminateJobObject(handle(&handles.job), 1) };
            stopped = Some(Instant::now());
        }
        if stopped.is_some_and(|at| at.elapsed() >= STOP_LIMIT) {
            return Err("store child custody remains unresolved");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_boundary_never_becomes_a_success_after_settlement() {
        let limit = Duration::from_millis(250);
        assert!(admit_prepared_child(RUN_LIMIT - Duration::from_nanos(1)).is_ok());
        assert_eq!(
            admit_prepared_child(RUN_LIMIT),
            Err("store child preparation deadline exceeded")
        );
        assert!(admit_prepared_child(RUN_LIMIT + Duration::from_nanos(1)).is_err());
        assert!(deadline_error(limit - Duration::from_nanos(1), limit).is_none());
        assert!(deadline_error(limit, limit).is_some());
        assert!(deadline_error(limit + Duration::from_nanos(1), limit).is_some());
        let failure = Some("store child output bound exceeded");
        assert!(failure
            .or_else(|| deadline_error(Duration::ZERO, limit))
            .is_some());
        assert!(fixture_deadline(Mode::Timeout, Duration::from_secs(1), None).is_none());
        assert_eq!(
            fixture_deadline(Mode::Timeout, RUN_LIMIT, None),
            Some("store child readiness deadline exceeded")
        );
        assert_eq!(
            fixture_deadline(Mode::Timeout, Duration::from_secs(1), Some(READY_LIMIT)),
            Some("store child deadline exceeded")
        );
    }
    #[test]
    fn binding_encoding_is_exact_and_bounded() {
        let bytes = [0xab; 32];
        assert!(decode(encode(&bytes).as_ref()).unwrap() == bytes);
        for value in [
            "",
            &"a".repeat(63),
            &"a".repeat(65),
            &"A".repeat(64),
            &"g".repeat(64),
        ] {
            assert!(decode(value.as_ref()).is_err());
        }
    }
    #[test]
    fn child_request_is_only_fixed_modes_and_three_bindings() {
        for mode in [
            Mode::Busy,
            Mode::Clean,
            Mode::Abandon,
            Mode::Timeout,
            Mode::Overflow,
        ] {
            let args: Vec<OsString> = [
                "--child",
                mode.argument(),
                &"0".repeat(64),
                &"1".repeat(64),
                &"2".repeat(64),
            ]
            .into_iter()
            .map(Into::into)
            .collect();
            assert!(request(&args).unwrap().0 == mode);
            assert!(request(&args[..4]).is_err());
            let mut extra = args.clone();
            extra.push("extra".into());
            assert!(request(&extra).is_err());
            let mut unknown = args;
            unknown[1] = "arbitrary".into();
            assert!(request(&unknown).is_err());
        }
    }
}
