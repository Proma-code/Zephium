//! Owned Windows stdio custody for the fixed, native-admitted app-server image.
//! This is a trusted-runtime boundary, not a same-user or malicious-vendor sandbox.
use crate::{PinnedImage, RuntimePaths};
use ::windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            SetHandleInformation, ERROR_BROKEN_PIPE, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Globalization::{CompareStringOrdinal, CSTR_EQUAL},
        Security::SECURITY_ATTRIBUTES,
        System::{
            JobObjects::{
                CreateJobObjectW, IsProcessInJob, JobObjectBasicAccountingInformation,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject, TerminateJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Pipes::{CreatePipe, PeekNamedPipe},
            SystemInformation::GetSystemDirectoryW,
            Threading::*,
        },
    },
};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::Read,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[path = "windows/framing.rs"]
mod framing;
use framing::{Frames, MAX_FRAME_BYTES};
mod write_fence;
pub(crate) use write_fence::WriteFence;
mod dormant_quota;
mod write_receipt;
use dormant_quota::DormantQuotaDrain;
pub(crate) use write_receipt::{
    BrowserInputIntent, MatchedUnsent, TurnHistory, TurnIntent, TurnOperation, TurnWrite,
    UnsentTurn,
};
use write_receipt::{OwnedFrame, SessionStamp, TurnBinding, TurnTicket, WriteOutcome};

#[cfg(all(debug_assertions, feature = "native-fixtures"))]
pub(crate) mod fixtures;

// Fixture admission remains a separate, debug-only, feature-gated type. Production image
// constructors and digests cannot be substituted by test executable paths.
enum Image {
    Original(Arc<PinnedImage>),
    SchemaExport(Arc<PinnedImage>),
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    Fixture(Arc<fixtures::FixtureImage>, fixtures::Mode),
}
impl Image {
    fn path(&self) -> &Path {
        match self {
            Self::Original(image) | Self::SchemaExport(image) => image.path(),
            #[cfg(all(debug_assertions, feature = "native-fixtures"))]
            Self::Fixture(image, _) => image.path(),
        }
    }
    fn args(&self) -> &'static [&'static str] {
        match self {
            Self::Original(image) => image.kind().args(),
            Self::SchemaExport(_) => &[
                "app-server",
                "generate-json-schema",
                "--experimental",
                "--out",
                "schema-output",
            ],
            #[cfg(all(debug_assertions, feature = "native-fixtures"))]
            Self::Fixture(_, mode) => mode.args(),
        }
    }
}

const MAX_STDERR_BYTES: u64 = 64 * 1024;
const MAX_STDOUT_BYTES: u64 = 64 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    InvalidRuntime,
    Spawn,
    Custody,
    Pipe,
    Frame,
    OutputLimit,
    Timeout,
    Cancelled,
    UnexpectedExit,
    Writer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Exit {
    pub(crate) code: u32,
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
}

fn raw(value: &impl AsRawHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}
fn wide(value: &OsStr) -> Result<Vec<u16>, Fault> {
    let mut output: Vec<u16> = value.encode_wide().collect();
    if output.is_empty() || output.len() >= 32767 || output.contains(&0) {
        return Err(Fault::InvalidRuntime);
    }
    output.push(0);
    Ok(output)
}

struct Job(OwnedHandle);
impl Job {
    fn create() -> Result<Self, Fault> {
        let native =
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|_| Fault::Custody)?;
        let job = Self(unsafe { OwnedHandle::from_raw_handle(native.0) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                native,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            )
        }
        .map_err(|_| Fault::Custody)?;
        Ok(job)
    }
    fn empty(&self) -> Result<bool, Fault> {
        let mut counts = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(raw(&self.0)),
                JobObjectBasicAccountingInformation,
                (&mut counts as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of_val(&counts) as u32,
                None,
            )
        }
        .map_err(|_| Fault::Custody)?;
        Ok(counts.ActiveProcesses == 0)
    }
    fn stop(&self) {
        let _ = unsafe { TerminateJobObject(raw(&self.0), 1) };
    }
}

struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new() -> Result<Self, Fault> {
        let mut bytes = 0;
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, 2, None, &mut bytes);
        }
        if bytes == 0 || bytes > 65536 {
            return Err(Fault::Custody);
        }
        let mut storage = vec![0; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &mut bytes) }
            .map_err(|_| Fault::Custody)?;
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
fn pipe() -> Result<(OwnedHandle, OwnedHandle), Fault> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe { CreatePipe(&mut read, &mut write, Some(&attributes), 8192) }
        .map_err(|_| Fault::Pipe)?;
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read.0),
            OwnedHandle::from_raw_handle(write.0),
        )
    })
}
fn no_inherit(handle: &OwnedHandle) -> Result<(), Fault> {
    unsafe { SetHandleInformation(raw(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(|_| Fault::Custody)
}

mod environment_block;
fn environment(runtime: &RuntimePaths) -> Result<Vec<u16>, Fault> {
    let mut system = [0; 32768];
    let length = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if length == 0 || length >= system.len() {
        return Err(Fault::InvalidRuntime);
    }
    let system = PathBuf::from(OsString::from_wide(&system[..length]));
    let root = system.parent().ok_or(Fault::InvalidRuntime)?;
    let mut entries: Vec<(&str, OsString)> = vec![
        ("SystemRoot", root.as_os_str().to_owned()),
        ("WINDIR", root.as_os_str().to_owned()),
        ("PATH", system.as_os_str().to_owned()),
        ("CODEX_HOME", runtime.home().as_os_str().to_owned()),
        ("HOME", runtime.home().as_os_str().to_owned()),
        ("USERPROFILE", runtime.userprofile().as_os_str().to_owned()),
        ("APPDATA", runtime.appdata().as_os_str().to_owned()),
        (
            "LOCALAPPDATA",
            runtime.localappdata().as_os_str().to_owned(),
        ),
        ("TEMP", runtime.temp().as_os_str().to_owned()),
        ("TMP", runtime.temp().as_os_str().to_owned()),
    ];
    // Windows var_os uses the platform's case-insensitive name lookup. Only
    // these seven source-proven transport names are queried; never enumerate or
    // inherit provider/auth/config/environment overrides. Values stay unlogged.
    let transport = crate::transport_environment::InheritedTransport::capture(|name| {
        std::env::var_os(name).map(|value| value.encode_wide().collect())
    })
    .map_err(|_| Fault::InvalidRuntime)?;
    entries.extend(
        transport
            .entries()
            .map(|(name, value)| (name, OsString::from_wide(value))),
    );
    entries.sort_by_cached_key(|(key, _)| key.to_ascii_uppercase());
    // 65,536 UTF-16 units is our total resource bound, not a Windows OS limit.
    // A malformed/oversized entry refuses the whole spawn, never partial env.
    environment_block::encode(entries.into_iter().map(|(key, value)| {
        let mut entry = OsString::from(key);
        entry.push("=");
        entry.push(value);
        wide(&entry).map_err(|_| ())
    }))
    .map_err(|_| Fault::InvalidRuntime)
}

pub(crate) struct TurnSendFailure {
    pub(crate) fault: Fault,
    pub(crate) never_started: Option<UnsentTurn>,
    observation_rejected: Option<write_receipt::UnsentObservation>,
}

pub(crate) enum QuotaWrite {
    Written,
    Rejected,
}

struct WriteRequest {
    frame: OwnedFrame,
    completed: mpsc::SyncSender<WriteOutcome>,
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    started: Arc<AtomicBool>,
}

/// No native resource can outlive this custodian without the custodian remaining
/// alive. Drop intentionally blocks after termination until process/writer settle.
pub(crate) struct Session {
    write_stamp: SessionStamp,
    write_sequence: u64,
    _image: Image,
    _runtime: Arc<RuntimePaths>,
    job: Job,
    process: Option<OwnedHandle>,
    stdin: Option<mpsc::SyncSender<WriteRequest>>,
    writer: Option<JoinHandle<()>>,
    stdout: Option<File>,
    stderr: Option<File>,
    frames: Frames,
    stdout_bytes: u64,
    stderr_bytes: u64,
    settled: bool,
    runtime_admitted: bool,
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    observed_pending_write: bool,
}

#[cfg(all(debug_assertions, feature = "native-fixtures"))]
pub(crate) struct FixtureBrowserReceipt {
    stamp: SessionStamp,
    image: Arc<fixtures::FixtureImage>,
}
impl Session {
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    pub(crate) fn fixture_browser_receipt(&self) -> Option<FixtureBrowserReceipt> {
        match &self._image {
            Image::Fixture(image, fixtures::Mode::Browser(_) | fixtures::Mode::Dormant(_)) => {
                Some(FixtureBrowserReceipt {
                    stamp: self.write_stamp.clone(),
                    image: image.clone(),
                })
            }
            _ => None,
        }
    }
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    pub(crate) fn owns_fixture_browser(&self, receipt: &FixtureBrowserReceipt) -> bool {
        matches!(&self._image, Image::Fixture(image, fixtures::Mode::Browser(_) | fixtures::Mode::Dormant(_)) if Arc::ptr_eq(image, &receipt.image))
            && self.write_stamp.same(&receipt.stamp)
    }
    pub(crate) fn owns_original_image(&self, expected: &Arc<PinnedImage>) -> bool {
        match &self._image {
            Image::Original(image) => Arc::ptr_eq(image, expected),
            Image::SchemaExport(_) => false,
            #[cfg(all(debug_assertions, feature = "native-fixtures"))]
            Image::Fixture(..) => false,
        }
    }

    pub(crate) fn spawn(
        image: Arc<PinnedImage>,
        runtime: Arc<RuntimePaths>,
    ) -> Result<Self, Fault> {
        Self::spawn_image(Image::Original(image), runtime)
    }

    /// Fixed full-CLI data exporter only; cwd/output are owned disposable paths.
    pub(crate) fn spawn_schema_export(
        image: Arc<PinnedImage>,
        runtime: Arc<RuntimePaths>,
    ) -> Result<Self, Fault> {
        if image.kind() != crate::pin::ImageKind::Cli {
            return Err(Fault::InvalidRuntime);
        }
        Self::spawn_image(Image::SchemaExport(image), runtime)
    }

    fn spawn_image(image: Image, runtime: Arc<RuntimePaths>) -> Result<Self, Fault> {
        let application = wide(image.path().as_os_str())?;
        let mut command = OsString::from("\"");
        command.push(image.path());
        command.push("\"");
        for argument in image.args() {
            command.push(" ");
            command.push(argument);
        }
        if image
            .path()
            .as_os_str()
            .encode_wide()
            .any(|c| c == u16::from(b'"'))
        {
            return Err(Fault::InvalidRuntime);
        }
        let mut command = wide(&command)?;
        let cwd = wide(runtime.cwd().as_os_str())?;
        let environment = environment(&runtime)?;
        let job = Job::create()?;
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        let (stdin_read, stdin_write) = pipe()?;
        for parent in [&stdout_read, &stderr_read, &stdin_write] {
            no_inherit(parent)?;
        }
        let attributes = Attributes::new()?;
        let inherited = [raw(&stdin_read), raw(&stdout_write), raw(&stderr_write)];
        let jobs = [raw(&job.0)];
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
            .map_err(|_| Fault::Custody)?;
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some(jobs.as_ptr().cast()),
                size_of_val(&jobs),
                None,
                None,
            )
            .map_err(|_| Fault::Custody)?;
        }
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = raw(&stdin_read);
        startup.StartupInfo.hStdOutput = raw(&stdout_write);
        startup.StartupInfo.hStdError = raw(&stderr_write);
        startup.lpAttributeList = attributes.list;
        let mut info = PROCESS_INFORMATION::default();
        // Allocate the complete owner before CreateProcess: guard installation
        // after native success itself cannot allocate or invoke user callbacks.
        let mut session = Self {
            write_stamp: SessionStamp::new(),
            write_sequence: 0,
            _image: image,
            _runtime: runtime,
            job,
            process: None,
            stdin: None,
            writer: None,
            stdout: Some(File::from(stdout_read)),
            stderr: Some(File::from(stderr_read)),
            frames: Frames::default(),
            stdout_bytes: 0,
            stderr_bytes: 0,
            settled: false,
            runtime_admitted: false,
            #[cfg(all(debug_assertions, feature = "native-fixtures"))]
            observed_pending_write: false,
        };
        session
            ._runtime
            .begin_connection()
            .map_err(|_| Fault::Custody)?;
        session.runtime_admitted = true;
        unsafe {
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                true,
                CREATE_NO_WINDOW
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_SUSPENDED,
                Some(environment.as_ptr().cast()),
                PCWSTR(cwd.as_ptr()),
                &startup.StartupInfo,
                &mut info,
            )
        }
        .map_err(|_| Fault::Spawn)?;
        session.process = Some(unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) });
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread.0) };
        drop(attributes);
        // Parent copies of the child ends must close before any read/cleanup.
        drop(stdin_read);
        drop(stdout_write);
        drop(stderr_write);
        let mut in_job = false.into();
        unsafe {
            IsProcessInJob(
                session.process_handle()?,
                Some(raw(&session.job.0)),
                &mut in_job,
            )
        }
        .map_err(|_| Fault::Custody)?;
        if !in_job.as_bool() {
            return Err(Fault::Custody);
        }
        let mut actual = vec![0; 32768];
        let mut length = actual.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                session.process_handle()?,
                PROCESS_NAME_WIN32,
                PWSTR(actual.as_mut_ptr()),
                &mut length,
            )
        }
        .map_err(|_| Fault::Custody)?;
        if !same_windows_path(&actual[..length as usize], session._image.path()) {
            return Err(Fault::Custody);
        }
        let (sender, receiver) = mpsc::sync_channel::<WriteRequest>(1);
        session.writer = Some(
            std::thread::Builder::new()
                .name("codex-stdin".into())
                .spawn(move || {
                    let mut stdin = File::from(stdin_write);
                    while let Ok(request) = receiver.recv() {
                        #[cfg(all(debug_assertions, feature = "native-fixtures"))]
                        request.started.store(true, Ordering::Release);
                        let outcome = request.frame.write(&mut stdin);
                        let entered_failure = matches!(outcome, WriteOutcome::EnteredFailure);
                        let _ = request.completed.send(outcome);
                        // A pre-write gate rejection does not damage the pipe.
                        // Keep it alive for owned unsubscribe/close settlement.
                        if entered_failure {
                            break;
                        }
                    }
                })
                .map_err(|_| Fault::Writer)?,
        );
        session.stdin = Some(sender);
        if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
            return Err(Fault::Spawn);
        }
        Ok(session)
    }

    fn process_handle(&self) -> Result<HANDLE, Fault> {
        self.process.as_ref().map(raw).ok_or(Fault::Custody)
    }

    pub(crate) fn send_frame(
        &mut self,
        bytes: &[u8],
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<(), Fault> {
        self.send_frame_fenced(bytes, deadline, cancelled, None)
    }
    pub(crate) fn send_frame_fenced(
        &mut self,
        bytes: &[u8],
        deadline: Instant,
        cancelled: &AtomicBool,
        fence: Option<WriteFence>,
    ) -> Result<(), Fault> {
        self.send_owned(bytes.to_vec(), None, deadline, cancelled, fence)
            .map_err(|failure| {
                self.settle_physical();
                failure.fault
            })
    }
    /// Fixed observation only. Preserve an owner only from the same actual
    /// pre-write rejection receipt, never from a timeout/cancel error category.
    pub(crate) fn send_quota_read(
        &mut self,
        id: u64,
        deadline: Instant,
        service_stop: &AtomicBool,
        fence: Option<WriteFence>,
    ) -> Result<QuotaWrite, Fault> {
        let bytes=serde_json::to_vec(&serde_json::json!({"id":id,"method":"account/rateLimits/read","params":crate::quota::AccountQuota::read_params()}))
            .map_err(|_| Fault::Frame)?;
        match self.send_owned(bytes, None, deadline, service_stop, fence) {
            Ok(()) => Ok(QuotaWrite::Written),
            Err(failure)
                if failure.observation_rejected.is_some()
                    && !self.settled
                    && !service_stop.load(Ordering::Acquire) =>
            {
                Ok(QuotaWrite::Rejected)
            }
            Err(failure) => {
                self.settle_physical();
                Err(failure.fault)
            }
        }
    }

    /// Issued only after the owning Connection validates current scope/grant
    /// and exact history. Reserving an intent sends no bytes or provider RPC.
    pub(crate) fn reserve_turn(&mut self, operation: TurnOperation) -> Result<TurnIntent, Fault> {
        if self.settled {
            return Err(Fault::UnexpectedExit);
        }
        self.write_sequence = self.write_sequence.checked_add(1).ok_or(Fault::Writer)?;
        Ok(TurnIntent::new(
            self.write_stamp.clone(),
            self.write_sequence,
            operation,
        ))
    }
    pub(crate) fn reserve_browser_input(
        &mut self,
        operation: TurnOperation,
    ) -> Result<BrowserInputIntent, Fault> {
        if self.settled {
            return Err(Fault::UnexpectedExit);
        }
        self.write_sequence = self.write_sequence.checked_add(1).ok_or(Fault::Writer)?;
        BrowserInputIntent::new(self.write_stamp.clone(), self.write_sequence, operation)
            .ok_or(Fault::Writer)
    }
    pub(crate) fn reject_browser_input(&self, intent: BrowserInputIntent) -> Option<UnsentTurn> {
        intent.reject(&self.write_stamp, self.write_sequence)
    }
    /// This proves no turn-frame handoff, not that prior thread preparation or
    /// original process custody has settled. The caller must retain those gates.
    pub(crate) fn reject_intent(&self, intent: TurnIntent) -> Option<UnsentTurn> {
        intent.reject(&self.write_stamp, self.write_sequence)
    }

    /// Native fixed turn only. A returned unsent receipt is exact pre-write
    /// evidence; all other failures retain uncertain dispatch in the caller.
    pub(crate) fn send_turn_frame(
        &mut self,
        turn: TurnWrite,
        deadline: Instant,
        cancelled: &AtomicBool,
        fence: WriteFence,
    ) -> Result<(), TurnSendFailure> {
        if !fence.is_text() {
            return Err(TurnSendFailure {
                fault: Fault::Frame,
                never_started: None,
                observation_rejected: None,
            });
        }
        let (ticket, bytes) = turn.into_parts();
        self.send_owned(bytes, Some(ticket), deadline, cancelled, Some(fence))
    }

    fn send_owned(
        &mut self,
        mut bytes: Vec<u8>,
        turn: Option<TurnTicket>,
        deadline: Instant,
        cancelled: &AtomicBool,
        fence: Option<WriteFence>,
    ) -> Result<(), TurnSendFailure> {
        if bytes.is_empty()
            || bytes.len() > MAX_FRAME_BYTES
            || bytes.contains(&b'\n')
            || bytes.contains(&b'\r')
        {
            bytes.fill(0);
            return Err(TurnSendFailure {
                fault: Fault::Frame,
                never_started: None,
                observation_rejected: None,
            });
        }
        let (sequence, binding) = if let Some(ticket) = turn {
            if !ticket.belongs_to(&self.write_stamp)
                || ticket.sequence == 0
                || ticket.sequence > self.write_sequence
                || !ticket.matches_bytes(&bytes)
            {
                bytes.fill(0);
                return Err(TurnSendFailure {
                    fault: Fault::Frame,
                    never_started: None,
                    observation_rejected: None,
                });
            }
            (ticket.sequence, Some(ticket.binding))
        } else {
            let Some(sequence) = self.write_sequence.checked_add(1) else {
                bytes.fill(0);
                return Err(TurnSendFailure {
                    fault: Fault::Writer,
                    never_started: None,
                    observation_rejected: None,
                });
            };
            self.write_sequence = sequence;
            (sequence, None)
        };
        bytes.push(b'\n');
        let frame = OwnedFrame::new(
            self.write_stamp.clone(),
            sequence,
            binding.clone(),
            bytes,
            deadline,
            fence,
        );
        if let Err(fault) = self.check(deadline, cancelled) {
            let receipt = frame.reject_unqueued();
            return Err(self.unsent_failure(fault, sequence, binding.as_ref(), receipt));
        }
        let (completed, result) = mpsc::sync_channel(1);
        #[cfg(all(debug_assertions, feature = "native-fixtures"))]
        let started = Arc::new(AtomicBool::new(false));
        #[cfg(all(debug_assertions, feature = "native-fixtures"))]
        {
            self.observed_pending_write = false;
        }
        let Some(stdin) = self.stdin.as_ref() else {
            self.settle_physical();
            return Err(self.unsent_failure(
                Fault::Writer,
                sequence,
                binding.as_ref(),
                frame.reject_unqueued(),
            ));
        };
        if let Err(rejected) = stdin.try_send(WriteRequest {
            frame,
            completed,
            #[cfg(all(debug_assertions, feature = "native-fixtures"))]
            started: started.clone(),
        }) {
            // try_send returns ownership of the entire never-enqueued frame.
            let rejected = match rejected {
                mpsc::TrySendError::Full(request) | mpsc::TrySendError::Disconnected(request) => {
                    request
                }
            };
            let receipt = rejected.frame.reject_unqueued();
            self.settle_physical();
            return Err(self.unsent_failure(Fault::Writer, sequence, binding.as_ref(), receipt));
        }
        loop {
            // A scope stop/deadline may win over the ACK. Physical settlement
            // joins the writer; only an actual received Rejected result can be
            // recovered afterward. Join/disconnection cannot create a receipt.
            if let Err(fault) = self.check(deadline, cancelled).and_then(|()| self.drain()) {
                self.settle_physical();
                return Err(match result.try_recv() {
                    Ok(WriteOutcome::Rejected(receipt)) => {
                        self.unsent_failure(fault, sequence, binding.as_ref(), receipt)
                    }
                    _ => TurnSendFailure {
                        fault,
                        never_started: None,
                        observation_rejected: None,
                    },
                });
            }
            match result.try_recv() {
                Ok(WriteOutcome::Written) => return Ok(()),
                Ok(WriteOutcome::Rejected(receipt)) => {
                    let fault = if receipt.expired() {
                        Fault::Timeout
                    } else {
                        Fault::Cancelled
                    };
                    return Err(self.unsent_failure(fault, sequence, binding.as_ref(), receipt));
                }
                Ok(WriteOutcome::EnteredFailure) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.settle_physical();
                    return Err(TurnSendFailure {
                        fault: Fault::Writer,
                        never_started: None,
                        observation_rejected: None,
                    });
                }
                Err(mpsc::TryRecvError::Empty) =>
                {
                    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
                    if started.load(Ordering::Acquire) {
                        self.observed_pending_write = true;
                    }
                }
            }
            std::thread::sleep(POLL);
        }
    }
    fn unsent_failure(
        &self,
        fault: Fault,
        sequence: u64,
        binding: Option<&TurnBinding>,
        receipt: write_receipt::NeverStarted,
    ) -> TurnSendFailure {
        let (never_started, observation_rejected) = match binding {
            Some(binding) => (receipt.consume(&self.write_stamp, sequence, binding), None),
            None => (
                None,
                receipt.consume_observation(&self.write_stamp, sequence),
            ),
        };
        TurnSendFailure {
            fault,
            never_started,
            observation_rejected,
        }
    }

    pub(crate) fn recv_frame(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, Fault> {
        loop {
            self.check(deadline, cancelled)?;
            self.drain()?;
            if let Some(frame) = self.frames.pop() {
                return Ok(frame);
            }
            // A process may exit with several pipe chunks still unread. Do not
            // confuse root exit with stdout EOF or discard its final frame.
            match self.exited() {
                Ok(_) if self.stdout.is_none() => return self.fail(Fault::UnexpectedExit),
                Ok(_) => {}
                Err(fault) => return self.fail(fault),
            }
            std::thread::sleep(POLL);
        }
    }

    pub(crate) fn poll_frame(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Option<Vec<u8>>, Fault> {
        self.check(deadline, cancelled)?;
        self.drain()?;
        if let Some(frame) = self.frames.pop() {
            return Ok(Some(frame));
        }
        if self.stdout.is_none() {
            return self.fail(Fault::UnexpectedExit);
        }
        Ok(None)
    }

    fn check(&mut self, deadline: Instant, cancelled: &AtomicBool) -> Result<(), Fault> {
        if self.settled {
            return Err(Fault::UnexpectedExit);
        }
        if cancelled.load(Ordering::Acquire) {
            return self.fail(Fault::Cancelled);
        }
        if Instant::now() >= deadline {
            return self.fail(Fault::Timeout);
        }
        Ok(())
    }
    fn fail<T>(&mut self, fault: Fault) -> Result<T, Fault> {
        self.settle_physical();
        Err(fault)
    }

    fn drain(&mut self) -> Result<(), Fault> {
        let outcome = (|| {
            if let Some(file) = self.stdout.as_mut() {
                match available(file)? {
                    Some(bytes) if !bytes.is_empty() => {
                        self.stdout_bytes = self
                            .stdout_bytes
                            .checked_add(bytes.len() as u64)
                            .ok_or(Fault::OutputLimit)?;
                        if self.stdout_bytes > MAX_STDOUT_BYTES {
                            return Err(Fault::OutputLimit);
                        }
                        self.frames.push(&bytes).map_err(|_| Fault::Frame)?;
                    }
                    None => {
                        self.frames.eof().map_err(|_| Fault::Frame)?;
                        self.stdout = None;
                    }
                    _ => {}
                }
            }
            if let Some(file) = self.stderr.as_mut() {
                match available(file)? {
                    Some(bytes) => {
                        self.stderr_bytes = self
                            .stderr_bytes
                            .checked_add(bytes.len() as u64)
                            .ok_or(Fault::OutputLimit)?;
                        if self.stderr_bytes > MAX_STDERR_BYTES {
                            return Err(Fault::OutputLimit);
                        }
                    }
                    None => self.stderr = None,
                }
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            self.fail(error)
        } else {
            Ok(())
        }
    }
    fn exited(&self) -> Result<bool, Fault> {
        match unsafe { WaitForSingleObject(self.process_handle()?, 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(Fault::Custody),
        }
    }

    /// Close stdin, then require the process, descendant job and pipe writer to
    /// finish. A forced cleanup is an error, never graceful success.
    pub(crate) fn finish(self, deadline: Instant) -> Result<Exit, Fault> {
        self.finish_cancellable(deadline, &AtomicBool::new(false))
    }
    pub(crate) fn finish_cancellable(
        self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Exit, Fault> {
        self.finish_owned(deadline, cancel, None)
    }
    /// Only a Connection's exact expired quota tombstones may enter this mode.
    /// All writes already completed synchronously; no new frame can be queued
    /// after the consuming call. Ordinary finish retains its strict semantics.
    pub(crate) fn finish_dormant_quota(
        self,
        deadline: Instant,
        scope: &crate::conversation::Scope,
        expired: Vec<(u64, u64)>,
    ) -> Result<Exit, Fault> {
        let drain = DormantQuotaDrain::new(self.write_stamp.clone(), scope.clone(), expired)
            .ok_or(Fault::Frame)?;
        if !drain.joins(&self.write_stamp, scope) {
            return Err(Fault::Frame);
        }
        self.finish_owned(deadline, &AtomicBool::new(false), Some(drain))
    }
    fn finish_owned(
        mut self,
        deadline: Instant,
        cancel: &AtomicBool,
        mut quota: Option<DormantQuotaDrain>,
    ) -> Result<Exit, Fault> {
        if self.settled {
            // A prior transport fault already forced cleanup. Ignoring that
            // fault cannot turn a later finish into a graceful receipt.
            return Err(Fault::UnexpectedExit);
        }
        self.stdin.take();
        loop {
            if cancel.load(Ordering::Acquire) {
                return self.fail(Fault::Cancelled);
            }
            if Instant::now() >= deadline {
                return self.fail(Fault::Timeout);
            }
            self.drain()?;
            if let Some(quota) = &mut quota {
                while let Some(mut frame) = self.frames.pop() {
                    let result = quota.observe(&frame);
                    frame.fill(0);
                    if result.is_err() {
                        return self.fail(Fault::Frame);
                    }
                }
            }
            if self.exited()? && self.job.empty()? && self.stdout.is_none() && self.stderr.is_none()
            {
                break;
            }
            std::thread::sleep(POLL);
        }
        if self.frames.pending() != 0 {
            return self.fail(Fault::Frame);
        }
        if let Some(writer) = self.writer.take() {
            writer.join().map_err(|_| Fault::Writer)?;
        }
        let mut code = 0;
        unsafe { GetExitCodeProcess(self.process_handle()?, &mut code) }
            .map_err(|_| Fault::Custody)?;
        self.settled = true;
        self._runtime
            .physically_settled()
            .map_err(|_| Fault::Custody)?;
        self.runtime_admitted = false;
        Ok(Exit {
            code,
            stdout_bytes: self.stdout_bytes,
            stderr_bytes: self.stderr_bytes,
        })
    }

    fn settle_physical(&mut self) {
        if self.settled {
            return;
        }
        self.stdin.take();
        if let Some(process) = &self.process {
            self.job.stop();
            // Custody deliberately outlives a logical deadline. Never release
            // image/runtime state while an unconfirmed child/writer can use it.
            while unsafe { WaitForSingleObject(raw(process), 10) } != WAIT_OBJECT_0
                || !self.job.empty().unwrap_or(false)
            {
                self.job.stop();
            }
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        self.stdout.take();
        self.stderr.take();
        self.settled = true;
        if self.runtime_admitted {
            let _ = self._runtime.physically_settled();
            self.runtime_admitted = false;
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.settle_physical();
    }
}

fn same_windows_path(actual: &[u16], expected: &Path) -> bool {
    // This is an additional post-create correlation, not image admission.
    let expected: Vec<u16> = expected.as_os_str().encode_wide().collect();
    !actual.is_empty()
        && unsafe { CompareStringOrdinal(drive_path(actual), drive_path(&expected), true) }
            == CSTR_EQUAL
}

// QueryFullProcessImageNameW may return a DOS drive path while the held image
// was opened with its fixed verbatim-drive prefix. No UNC, dot, separator or
// trailing-name normalization is performed here.
fn drive_path(value: &[u16]) -> &[u16] {
    if value.len() >= 7
        && value[..4] == [92, 92, 63, 92]
        && ((65..=90).contains(&value[4]) || (97..=122).contains(&value[4]))
        && value[5..7] == [58, 92]
    {
        &value[4..]
    } else {
        value
    }
}

fn available(file: &mut File) -> Result<Option<Vec<u8>>, Fault> {
    let mut count = 0;
    if let Err(error) = unsafe { PeekNamedPipe(raw(file), None, 0, None, Some(&mut count), None) } {
        return if error.code() == windows::core::HRESULT::from_win32(ERROR_BROKEN_PIPE.0) {
            Ok(None)
        } else {
            Err(Fault::Pipe)
        };
    }
    if count == 0 {
        return Ok(Some(Vec::new()));
    }
    let mut bytes = vec![0; (count as usize).min(64 * 1024)];
    let read = file.read(&mut bytes).map_err(|_| Fault::Pipe)?;
    if read == 0 {
        return Ok(None);
    }
    bytes.truncate(read);
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_create_path_correlation_is_utf16_and_drive_prefix_only() {
        fn units(value: &str) -> Vec<u16> {
            value.encode_utf16().collect()
        }
        assert!(same_windows_path(
            &units(r"C:\Owned\Codex.exe"),
            Path::new(r"c:\owned\codex.exe")
        ));
        assert!(same_windows_path(
            &units(r"C:\Owned\Codex.exe"),
            Path::new(r"\\?\C:\Owned\Codex.exe")
        ));
        assert!(!same_windows_path(
            &units(r"C:\Other\Codex.exe"),
            Path::new(r"C:\Owned\Codex.exe")
        ));
        assert!(!same_windows_path(
            &units(r"C:\Owned\..\Codex.exe"),
            Path::new(r"C:\Codex.exe")
        ));
        assert_eq!(
            drive_path(&units(r"\\?\UNC\host\path")),
            units(r"\\?\UNC\host\path")
        );
        let invalid = [67, 58, 92, 0xd800];
        assert!(!same_windows_path(&invalid, Path::new("C:\\�")));
    }
}
