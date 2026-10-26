//! The command enters its kill-on-close job before its first instruction.
use super::*;
use ::windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        Security::{SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES},
        System::{
            JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            },
            Pipes::CreatePipe,
            SystemInformation::GetSystemDirectoryW,
            Threading::*,
        },
    },
};
use base64::Engine;
use std::{
    ffi::OsStr,
    fs::File,
    io::Read,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};

fn handle(value: &OwnedHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}

struct Job {
    handle: OwnedHandle,
    quitting: AtomicBool,
}
impl Job {
    fn stop(&self) {
        // SAFETY: this is the exclusively owned, non-inheritable job handle.
        unsafe {
            let _ = TerminateJobObject(handle(&self.handle), 1);
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.stop();
    }
}

static CLOSING: AtomicBool = AtomicBool::new(false);

fn jobs() -> &'static Mutex<Vec<Weak<Job>>> {
    static JOBS: OnceLock<Mutex<Vec<Weak<Job>>>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}
pub(super) fn shutdown() {
    let mut jobs = jobs().lock().unwrap_or_else(|e| e.into_inner());
    CLOSING.store(true, Ordering::Release);
    jobs.retain(|entry| {
        if let Some(job) = entry.upgrade() {
            job.quitting.store(true, Ordering::Release);
            job.stop();
            true
        } else {
            false
        }
    });
}

fn wide(value: &OsStr) -> Result<Vec<u16>, &'static str> {
    let mut result: Vec<_> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err("The command contains an invalid path");
    }
    result.push(0);
    Ok(result)
}

fn pipe() -> Result<(OwnedHandle, OwnedHandle), &'static str> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: pointers refer to live output handles and attributes.
    unsafe {
        CreatePipe(&mut read, &mut write, Some(&attributes), 8192)
            .map_err(|_| "The command pipe could not be created")?;
        Ok((
            OwnedHandle::from_raw_handle(read.0),
            OwnedHandle::from_raw_handle(write.0),
        ))
    }
}

struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new(count: u32) -> Result<Self, &'static str> {
        let mut bytes = 0;
        // SAFETY: the first call asks only for the required allocation size.
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, count, None, &mut bytes);
        }
        if bytes == 0 || bytes > 64 * 1024 {
            return Err("Process attributes are unavailable");
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: stable, aligned backing allocation is at least `bytes` long.
        unsafe { InitializeProcThreadAttributeList(Some(list), count, None, &mut bytes) }
            .map_err(|_| "Process attributes are unavailable")?;
        Ok(Self {
            _storage: storage,
            list,
        })
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: initialized list remains backed by `_storage` until after drop.
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

pub(super) struct Process {
    process: OwnedHandle,
    job: Arc<Job>,
    pub(super) output: tokio::sync::mpsc::Receiver<Result<Vec<u8>, ()>>,
}
impl Drop for Process {
    fn drop(&mut self) {
        self.job.stop();
    }
}
impl Process {
    pub(super) fn stop(&self) {
        self.job.stop();
    }
    pub(super) fn quitting(&self) -> bool {
        self.job.quitting.load(Ordering::Acquire)
    }
    pub(super) fn status(&self) -> Result<Option<i32>, &'static str> {
        // SAFETY: this process handle is live and never supplied by model input.
        match unsafe { WaitForSingleObject(handle(&self.process), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                unsafe { GetExitCodeProcess(handle(&self.process), &mut code) }
                    .map_err(|_| "The command could not be waited for")?;
                Ok(Some(code as i32))
            }
            _ => Err("The command could not be waited for"),
        }
    }
}

fn spawn(cwd: &Path, command: &str) -> Result<Process, &'static str> {
    let mut system = [0u16; 32768];
    // SAFETY: API writes within the supplied buffer and returns its used length.
    let length = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if length == 0 || length >= system.len() {
        return Err("The command shell is unavailable");
    }
    use std::os::windows::ffi::OsStringExt;
    let system_directory = std::path::PathBuf::from(OsString::from_wide(&system[..length]));
    let shell = system_directory.join("WindowsPowerShell/v1.0/powershell.exe");
    let script = format!("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $OutputEncoding=[Console]::OutputEncoding; & {{\n{command}\n}}; if ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }}");
    let script: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    let arguments: Vec<OsString> = [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-InputFormat",
        "Text",
        "-OutputFormat",
        "Text",
        "-EncodedCommand",
        &encoded,
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let mut env = environment();
    env.push(("PATH".into(), system_directory.as_os_str().to_owned()));
    if let Some(root) = system_directory.parent() {
        env.push(("SystemRoot".into(), root.as_os_str().to_owned()));
    }
    env.retain(|(key, _)| {
        !["NO_COLOR", "CI", "GIT_TERMINAL_PROMPT", "GIT_PAGER"]
            .iter()
            .any(|fixed| key.eq_ignore_ascii_case(OsStr::new(fixed)))
    });
    env.extend([
        ("NO_COLOR".into(), "1".into()),
        ("CI".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_PAGER".into(), "cat".into()),
    ]);
    spawn_program(cwd, &shell, &arguments, env, None)
}

/// Shared native lifecycle; only the typed runner supplies a confinement token.
pub(super) fn spawn_program(
    cwd: &Path,
    program: &Path,
    arguments: &[OsString],
    mut env: Vec<(OsString, OsString)>,
    confinement: Option<&SECURITY_CAPABILITIES>,
) -> Result<Process, &'static str> {
    let application = wide(program.as_os_str())?;
    let mut line = command_line(program.as_os_str(), arguments)?;
    let cwd = wide(cwd.as_os_str())?;
    env.sort_by_key(|(key, _)| key.to_string_lossy().to_ascii_uppercase());
    env.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
    let mut environment = Vec::<u16>::new();
    for (key, value) in env {
        let mut entry = key;
        entry.push("=");
        entry.push(value);
        environment.extend(wide(&entry)?);
    }
    environment.push(0);

    // SAFETY: unnamed job receives no inherited handle and returns owned identity.
    let raw_job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
        .map_err(|_| "The command job could not be created")?;
    let job = Arc::new(Job {
        handle: unsafe { OwnedHandle::from_raw_handle(raw_job.0) },
        quitting: AtomicBool::new(false),
    });
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if confinement.is_some() {
        limits.BasicLimitInformation.LimitFlags |=
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.ProcessMemoryLimit = 256 * 1024 * 1024;
    }
    unsafe {
        SetInformationJobObject(
            raw_job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )
    }
    .map_err(|_| "The command job could not be configured")?;
    let (read, write) = pipe()?;
    let (stdin, stdin_write) = pipe()?;
    drop(stdin_write);
    unsafe { SetHandleInformation(handle(&read), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(|_| "The command pipe could not be protected")?;
    let attributes = Attributes::new(if confinement.is_some() { 4 } else { 2 })?;
    let inherited = [handle(&write), handle(&stdin)];
    let assigned = [raw_job];
    // SAFETY: both arrays and the attribute allocation live through CreateProcessW.
    // The job assignment occurs before execution; only our stdio handles inherit.
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
        .map_err(|_| "The command handles could not be isolated")?;
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            Some(assigned.as_ptr().cast()),
            size_of_val(&assigned),
            None,
            None,
        )
        .map_err(|_| "The command job could not be assigned")?;
    }
    let opt_out: u32 = 1; // PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT
    if let Some(capabilities) = confinement {
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
            .map_err(|_| "The confined token could not be configured")?;
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY as usize,
                Some((&raw const opt_out).cast()),
                size_of::<u32>(),
                None,
                None,
            )
            .map_err(|_| "The less-privileged token could not be configured")?;
        }
    }
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handle(&stdin);
    startup.StartupInfo.hStdOutput = handle(&write);
    startup.StartupInfo.hStdError = handle(&write);
    startup.lpAttributeList = attributes.list;
    let mut process = PROCESS_INFORMATION::default();
    // Admission and registration linearize with shutdown, which must see
    // every process admitted before it acquired this same lock.
    let mut registry = jobs().lock().unwrap_or_else(|e| e.into_inner());
    if confinement.is_some() && CLOSING.load(Ordering::Acquire) {
        return Err("The application is shutting down");
    }
    unsafe {
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
            Some(environment.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            &startup.StartupInfo,
            &mut process,
        )
        .map_err(|_| "The command could not be started")?;
    }
    let process_handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess.0) };
    let thread = unsafe { OwnedHandle::from_raw_handle(process.hThread.0) };
    if let Some(capabilities) = confinement {
        verify_confined_token(handle(&process_handle), capabilities)?;
    }
    if unsafe { ResumeThread(handle(&thread)) } == u32::MAX {
        return Err("The command could not be resumed");
    }
    registry.retain(|entry| entry.strong_count() != 0);
    registry.push(Arc::downgrade(&job));
    drop(registry);
    drop(thread);
    drop(write);
    drop(stdin);
    let (sender, output) = tokio::sync::mpsc::channel(8);
    std::thread::Builder::new()
        .name("work-command-output".into())
        .spawn(move || {
            let mut pipe = File::from(read);
            let mut bytes = [0u8; 8192];
            loop {
                match pipe.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        if sender.blocking_send(Ok(bytes[..count].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = sender.blocking_send(Err(()));
                        break;
                    }
                }
            }
        })
        .map_err(|_| "The command output could not be read")?;
    Ok(Process {
        process: process_handle,
        job,
        output,
    })
}

fn command_line(program: &OsStr, arguments: &[OsString]) -> Result<Vec<u16>, &'static str> {
    let mut line = Vec::new();
    for value in std::iter::once(program).chain(arguments.iter().map(OsString::as_os_str)) {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        line.push(b'"' as u16);
        let mut slashes = 0;
        for unit in value.encode_wide() {
            if unit == 0 {
                return Err("The command contains an invalid argument");
            }
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            line.extend(std::iter::repeat_n(
                b'\\' as u16,
                if unit == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            slashes = 0;
            line.push(unit);
        }
        line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        line.push(b'"' as u16);
    }
    if line.len() >= 32767 {
        return Err("The command is too long");
    }
    line.push(0);
    Ok(line)
}

fn verify_confined_token(
    process: HANDLE,
    capabilities: &SECURITY_CAPABILITIES,
) -> Result<(), &'static str> {
    use ::windows::Win32::Security::{
        EqualSid, GetTokenInformation, TokenAppContainerSid, TokenIsAppContainer,
        TokenIsLessPrivilegedAppContainer, TOKEN_APPCONTAINER_INFORMATION, TOKEN_QUERY,
    };
    let mut raw = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw) }
        .map_err(|_| "The confined token could not be verified")?;
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    for class in [TokenIsAppContainer, TokenIsLessPrivilegedAppContainer] {
        let mut value = 0u32;
        let mut length = 0;
        unsafe {
            GetTokenInformation(
                handle(&token),
                class,
                Some((&raw mut value).cast()),
                size_of::<u32>() as u32,
                &mut length,
            )
        }
        .map_err(|_| "The confined token could not be verified")?;
        if length != size_of::<u32>() as u32 || value != 1 {
            return Err("The confined token was not applied");
        }
    }
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
    .map_err(|_| "The confined identity could not be verified")?;
    if length < size_of::<TOKEN_APPCONTAINER_INFORMATION>() as u32
        || length as usize > size_of_val(&buffer)
    {
        return Err("The confined identity was invalid");
    }
    let info = unsafe { &*buffer.as_ptr().cast::<TOKEN_APPCONTAINER_INFORMATION>() };
    unsafe { EqualSid(info.TokenAppContainer, capabilities.AppContainerSid) }
        .map_err(|_| "The confined identity did not match")
}

pub async fn run<C, CF, P, PF>(
    cwd: &Path,
    command: &str,
    timeout_secs: u32,
    cancelled: C,
    progress: P,
) -> Result<CommandResult, &'static str>
where
    C: Fn() -> CF,
    CF: Future<Output = bool>,
    P: Fn(WorkCommandOutputV1) -> PF,
    PF: Future<Output = ()>,
{
    super::ensure_available()?;
    run_unconfined(cwd, command, timeout_secs, cancelled, progress).await
}

async fn run_unconfined<C, CF, P, PF>(
    cwd: &Path,
    command: &str,
    timeout_secs: u32,
    cancelled: C,
    progress: P,
) -> Result<CommandResult, &'static str>
where
    C: Fn() -> CF,
    CF: Future<Output = bool>,
    P: Fn(WorkCommandOutputV1) -> PF,
    PF: Future<Output = ()>,
{
    let started = Instant::now();
    let mut process = spawn(cwd, command)?;
    let mut output = Output::new();
    progress(output.view()).await;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut published = Instant::now();
    let mut status = None;
    let mut closed = false;
    let mut stopping: Option<(Instant, String)> = None;
    loop {
        tokio::select! {
            read = process.output.recv(), if !closed => match read {
                Some(Ok(bytes)) => output.push(&bytes),
                Some(Err(())) => { stopping.get_or_insert((Instant::now(), "The output could not be read".into())); process.job.stop(); },
                None => closed = true,
            },
            _ = tick.tick() => {
                if stopping.is_none() {
                    let note = if process.job.quitting.load(Ordering::Acquire) { Some("Stopped when the app quit".into()) }
                        else if started.elapsed() >= Duration::from_secs(u64::from(timeout_secs)) { Some(format!("Stopped after {timeout_secs} s")) }
                        else if cancelled().await { Some("Stopped".into()) } else { None };
                    if let Some(note) = note { process.job.stop(); stopping = Some((Instant::now(), note)); }
                }
                if status.is_none() {
                    status = process.status()?;
                    if status.is_some() { process.job.stop(); }
                }
                if stopping.as_ref().is_some_and(|(at, _)| at.elapsed() >= Duration::from_secs(2)) { break; }
                if published.elapsed() >= Duration::from_millis(300) {
                    let mut view = output.view();
                    view.elapsed_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
                    progress(view).await;
                    published = Instant::now();
                }
            }
        }
        if status.is_some() && closed {
            break;
        }
    }
    let succeeded = stopping.is_none() && status == Some(0);
    let note = stopping.map(|(_, note)| note).unwrap_or_else(|| {
        if succeeded {
            "Completed".into()
        } else {
            "The command failed".into()
        }
    });
    let view = output.view();
    Ok(CommandResult {
        evidence: WorkCommandEvidenceV1 {
            cwd: cwd.to_string_lossy().into_owned(),
            command: command.into(),
            exit: status,
            signal: None,
            elapsed_ms: started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            bytes: view.bytes,
            digest: format!("{:x}", output.hash.finalize()),
            text: view.text,
            truncated: view.truncated,
        },
        note,
        succeeded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::Win32::System::JobObjects::{
        IsProcessInJob, JobObjectBasicAccountingInformation, QueryInformationJobObject,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    };
    use std::sync::atomic::AtomicU32;

    #[test]
    fn fixed_arguments_round_trip_through_windows_parser() {
        use ::windows::Win32::{
            Foundation::{LocalFree, HLOCAL},
            UI::Shell::CommandLineToArgvW,
        };
        let program = OsStr::new(r"C:\Program Files\tool.exe");
        let args: Vec<OsString> = ["", "plain", "space here", r"ends\", "quote\"and\\", "żółć"]
            .into_iter()
            .map(Into::into)
            .collect();
        let line = command_line(program, &args).unwrap();
        let mut count = 0;
        let parsed = unsafe { CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut count) };
        assert!(!parsed.is_null());
        let values: Vec<String> = unsafe { std::slice::from_raw_parts(parsed, count as usize) }
            .iter()
            .map(|value| unsafe { value.to_string().unwrap() })
            .collect();
        unsafe { LocalFree(Some(HLOCAL(parsed.cast()))) };
        assert_eq!(
            values,
            std::iter::once(program.to_string_lossy().into_owned())
                .chain(args.iter().map(|arg| arg.to_string_lossy().into_owned()))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn windows_command_preserves_unicode_output_and_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_unconfined(
            dir.path(),
            "[Console]::Write('hello żółć'); [Console]::Error.Write('error'); exit 7",
            10,
            || async { false },
            |_| async {},
        )
        .await
        .unwrap();
        if result.evidence.exit != Some(7)
            || !result.evidence.text.contains("hello żółć")
            || !result.evidence.text.contains("error")
        {
            // Fixture-only finite diagnostics: never print the environment,
            // paths, arbitrary command output, or another test's payload.
            let termination = match result.note.as_str() {
                "Stopped after 10 s" => 1,
                "Stopped" => 2,
                "Stopped when the app quit" => 3,
                "The output could not be read" => 4,
                "The command failed" => 5,
                "Completed" => 6,
                _ => 0,
            };
            let categories = [
                "#< CLIXML",
                "ParserError",
                "CommandNotFoundException",
                "FileNotFoundException",
                "UnauthorizedAccessException",
                "PSSecurityException",
                "OutOfMemoryException",
                "is not recognized",
            ]
            .iter()
            .enumerate()
            .fold(0u16, |bits, (index, marker)| {
                bits | (u16::from(result.evidence.text.contains(marker)) << index)
            });
            eprintln!("unicode-fixture: exit={:?} termination={} elapsed_ms={} bytes={} truncated={} hello={} stderr_marker={} category_bits={:#06x}",
                result.evidence.exit, termination, result.evidence.elapsed_ms,
                result.evidence.bytes, result.evidence.truncated,
                result.evidence.text.contains("hello żółć"),
                result.evidence.text.contains("error"), categories);
        }
        assert!(!result.succeeded);
        assert_eq!(result.evidence.exit, Some(7));
        assert!(result.evidence.text.contains("hello żółć"));
        assert!(result.evidence.text.contains("error"));
    }

    fn owned_tree_receipt(line: &str) -> Option<(u32, u32)> {
        let (root, child) = line.strip_prefix("owned-tree:")?.split_once(':')?;
        let root: u32 = root.parse().ok()?;
        let child: u32 = child.parse().ok()?;
        (root != 0 && child != 0 && root != child && line == format!("owned-tree:{root}:{child}"))
            .then_some((root, child))
    }

    #[test]
    fn owned_tree_receipt_is_fixed_and_unambiguous() {
        assert_eq!(owned_tree_receipt("owned-tree:123:456"), Some((123, 456)));
        for value in [
            "123",
            "owned-tree:0:1",
            "owned-tree:1:1",
            "owned-tree:01:2",
            "owned-tree:1:2:3",
            "owned-tree:1:2 ",
        ] {
            assert_eq!(owned_tree_receipt(value), None);
        }
    }

    #[test]
    fn owned_tree_lf_receipt_survives_the_real_output_projection() {
        let mut output = super::super::Output::new();
        output.push(b"owned-tree:123:456\n");
        assert_eq!(
            output.view().text.lines().find_map(owned_tree_receipt),
            Some((123, 456))
        );
        let mut crlf = super::super::Output::new();
        crlf.push(b"owned-tree:123:456\r\n");
        assert_eq!(crlf.view().text, "owned-tree:123:456 \n");
        assert_eq!(crlf.view().text.lines().find_map(owned_tree_receipt), None);
    }

    #[tokio::test]
    async fn windows_command_timeout_and_stop_end_the_owned_tree() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_unconfined(
            dir.path(),
            "Write-Output started; Start-Sleep -Seconds 30",
            1,
            || async { false },
            |_| async {},
        )
        .await
        .unwrap();
        assert_eq!(result.note, "Stopped after 1 s");
        assert!(result.evidence.elapsed_ms < 5000);
        let pid = AtomicU32::new(0);
        let owned_tree = Mutex::new(None::<(OwnedHandle, OwnedHandle, Arc<Job>)>);
        // A fixed no-shell child writes and flushes readiness before its parent
        // advertises the PID. Start-Process can use shell/window behavior which
        // does not establish this redirected, owned-child fixture contract.
        let command = r#"
$ErrorActionPreference='Stop'
[Console]::WriteLine('owned-root-ready'); [Console]::Out.Flush()
$info=[System.Diagnostics.ProcessStartInfo]::new()
$info.FileName=$PSHOME+'\powershell.exe'
$info.UseShellExecute=$false
$info.CreateNoWindow=$true
$info.RedirectStandardOutput=$true
$info.Arguments='-NoProfile -NonInteractive -Command "[Console]::WriteLine(''owned-ready''); [Console]::Out.Flush(); [Threading.Thread]::Sleep(30000)"'
$child=[System.Diagnostics.Process]::Start($info)
if ($null -eq $child -or $child.StandardOutput.ReadLine() -ne 'owned-ready') { throw 'owned fixture child did not become ready' }
[Console]::Write('owned-tree:'+$PID+':'+$child.Id+"`n")
[Console]::Out.Flush()
[Threading.Thread]::Sleep(30000)
"#;
        let result = run_unconfined(
            dir.path(),
            command,
            10,
            || async { pid.load(Ordering::Acquire) != 0 },
            |view| {
                if owned_tree.lock().unwrap().is_some() {
                    return std::future::ready(());
                }
                let Some((root_pid, child_pid)) = view.text.lines().find_map(owned_tree_receipt)
                else {
                    return std::future::ready(());
                };
                // Both PIDs originate in the fixed no-shell factory. Bind
                // their held handles to the same unique owned Job before
                // this fixture requests cancellation.
                let open = |value| {
                    let raw = unsafe {
                        OpenProcess(
                            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                            false,
                            value,
                        )
                    }
                    .unwrap();
                    unsafe { OwnedHandle::from_raw_handle(raw.0) }
                };
                let root = open(root_pid);
                let child = open(child_pid);
                let matching: Vec<_> = jobs()
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(Weak::upgrade)
                    .filter(|job| {
                        [&root, &child].into_iter().all(|process| {
                            let mut member = ::windows::core::BOOL::default();
                            unsafe {
                                IsProcessInJob(
                                    handle(process),
                                    Some(handle(&job.handle)),
                                    &mut member,
                                )
                            }
                            .is_ok()
                                && member.as_bool()
                        })
                    })
                    .collect();
                assert_eq!(
                    matching.len(),
                    1,
                    "fixed parent and child must share exactly one owned Job"
                );
                *owned_tree.lock().unwrap() =
                    Some((root, child, matching.into_iter().next().unwrap()));
                pid.store(child_pid, Ordering::Release);
                std::future::ready(())
            },
        )
        .await
        .unwrap();
        assert_eq!(
            result.note, "Stopped",
            "fixture output: {}",
            result.evidence.text
        );
        assert_ne!(pid.load(Ordering::Acquire), 0);
        let (root, child, job) = owned_tree
            .into_inner()
            .unwrap()
            .expect("owned tree handles");
        // Inspect only retained handles; never signal or kill by PID. A timeout
        // or a best-effort stop return alone does not prove physical settlement.
        assert!(result.evidence.exit.is_some(), "root exit must be observed");
        for process in [&root, &child] {
            assert_eq!(
                unsafe { WaitForSingleObject(handle(process), 2000) },
                WAIT_OBJECT_0
            );
        }
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(handle(&job.handle)),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of_val(&accounting) as u32,
                None,
            )
        }
        .unwrap();
        assert_eq!(accounting.ActiveProcesses, 0, "owned Job must be empty");
    }

    #[tokio::test]
    async fn windows_command_drop_kills_an_inflight_process() {
        let dir = tempfile::tempdir().unwrap();
        let mut process = spawn(dir.path(), "Start-Sleep -Seconds 30").unwrap();
        let owned = process.process.try_clone().unwrap();
        process.output.close();
        drop(process);
        assert_eq!(
            unsafe { WaitForSingleObject(handle(&owned), 2000) },
            WAIT_OBJECT_0
        );
    }
}
