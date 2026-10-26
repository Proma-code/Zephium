//! Finite synthetic transport controls. No caller-selected image or arguments,
//! original-client admission bypass, authentication or provider request.
use super::*;
use ::windows::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    process::{Command, Stdio},
};

const MAX_HELPER_BYTES: u64 = 64 * 1024 * 1024;
const OPEN_NOFOLLOW: u32 = 0x0020_0000;
const REPARSE: u32 = 0x400;
const CONTROL_BUDGET: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Nonzero,
    Truncated,
    Blocked,
    Tree,
}
impl Mode {
    pub(super) fn args(self) -> &'static [&'static str] {
        match self {
            Self::Nonzero => &["transport-child", "nonzero"],
            Self::Truncated => &["transport-child", "truncated"],
            Self::Blocked => &["transport-child", "blocked"],
            Self::Tree => &["transport-child", "tree"],
        }
    }
}

pub(super) struct FixtureImage {
    path: PathBuf,
    _source: File,
    _image: File,
}
impl FixtureImage {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn copy_current(runtime: &RuntimePaths) -> Result<Self, &'static str> {
        let source = std::env::current_exe().map_err(|_| "helper-source")?;
        crate::image::require_absolute_local(&source)?;
        let mut source = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(OPEN_NOFOLLOW)
            .open(source)
            .map_err(|_| "helper-source-open")?;
        let meta = source.metadata().map_err(|_| "helper-source-metadata")?;
        if !meta.is_file()
            || meta.file_attributes() & REPARSE != 0
            || meta.len() == 0
            || meta.len() > MAX_HELPER_BYTES
        {
            return Err("helper-source-shape");
        }
        let expected_bytes = meta.len();
        let path = runtime.temp().join("codex-transport-helper.exe");
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(1)
            .custom_flags(OPEN_NOFOLLOW)
            .open(&path)
            .map_err(|_| "helper-copy-create")?;
        let mut hash = Sha256::new();
        let mut copied = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let length = source.read(&mut buffer).map_err(|_| "helper-copy-read")?;
            if length == 0 {
                break;
            }
            copied = copied
                .checked_add(length as u64)
                .ok_or("helper-copy-length")?;
            if copied > expected_bytes {
                return Err("helper-copy-length");
            }
            hash.update(&buffer[..length]);
            target
                .write_all(&buffer[..length])
                .map_err(|_| "helper-copy-write")?;
        }
        if copied != expected_bytes {
            return Err("helper-copy-length");
        }
        target.sync_all().map_err(|_| "helper-copy-sync")?;
        drop(target); // A writer-open image must never be passed to CreateProcess.
        let expected_hash = hash.finalize();
        let mut image = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(OPEN_NOFOLLOW)
            .open(&path)
            .map_err(|_| "helper-image-open")?;
        let metadata = image.metadata().map_err(|_| "helper-image-metadata")?;
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(raw(&image), &mut information) }
            .map_err(|_| "helper-image-identity")?;
        if !metadata.is_file()
            || metadata.file_attributes() & REPARSE != 0
            || metadata.len() != expected_bytes
            || information.nNumberOfLinks != 1
        {
            return Err("helper-image-shape");
        }
        let mut hash = Sha256::new();
        let mut read = 0u64;
        loop {
            let length = image.read(&mut buffer).map_err(|_| "helper-image-read")?;
            if length == 0 {
                break;
            }
            read = read
                .checked_add(length as u64)
                .ok_or("helper-image-length")?;
            if read > expected_bytes {
                return Err("helper-image-length");
            }
            hash.update(&buffer[..length]);
        }
        if read != expected_bytes || hash.finalize() != expected_hash {
            return Err("helper-image-hash");
        }
        Ok(Self {
            path,
            _source: source,
            _image: image,
        })
    }
}

fn fresh(mode: Mode) -> Result<(Session, Arc<RuntimePaths>), &'static str> {
    let runtime = Arc::new(RuntimePaths::fresh()?);
    let image = Arc::new(FixtureImage::copy_current(&runtime)?);
    let session = Session::spawn_image(Image::Fixture(image, mode), runtime.clone())
        .map_err(|_| "helper-spawn")?;
    Ok((session, runtime))
}
fn close_runtime(runtime: Arc<RuntimePaths>) -> Result<(), &'static str> {
    Arc::try_unwrap(runtime)
        .map_err(|_| "helper-runtime-retained")?
        .close()
}
fn settled(session: &Session) -> Result<(), &'static str> {
    if !session.settled
        || !session.exited().map_err(|_| "helper-exit-query")?
        || !session.job.empty().map_err(|_| "helper-job-query")?
        || session.writer.is_some()
        || session.stdin.is_some()
        || session.stdout.is_some()
        || session.stderr.is_some()
    {
        return Err("helper-not-settled");
    }
    Ok(())
}

/// Only fixed own-helper operations; every failure stays a failed qualification.
pub(crate) fn qualify_transport() -> Result<(), &'static str> {
    let cancel = AtomicBool::new(false);
    let (session, runtime) = fresh(Mode::Nonzero)?;
    let exit = session
        .finish(Instant::now() + CONTROL_BUDGET)
        .map_err(|_| "helper-nonzero-finish")?;
    if exit.code != 7 || exit.stdout_bytes != 0 || exit.stderr_bytes != 0 {
        return Err("helper-nonzero-receipt");
    }
    close_runtime(runtime)?;
    println!("codex-transport-nonzero: native-code=0");

    let (mut session, runtime) = fresh(Mode::Truncated)?;
    if session.recv_frame(Instant::now() + CONTROL_BUDGET, &cancel) != Err(Fault::Frame) {
        return Err("helper-eof-receipt");
    }
    settled(&session)?;
    drop(session);
    close_runtime(runtime)?;
    println!("codex-transport-truncated-eof: native-code=0");

    let (mut session, runtime) = fresh(Mode::Blocked)?;
    if session
        .recv_frame(Instant::now() + CONTROL_BUDGET, &cancel)
        .map_err(|_| "helper-blocked-ready")?
        != br#"{"ready":1}"#
    {
        return Err("helper-blocked-ready");
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    if session.send_frame(&vec![b'x'; MAX_FRAME_BYTES], deadline, &cancel) != Err(Fault::Timeout) {
        return Err("helper-blocked-receipt");
    }
    settled(&session)?;
    if !session.observed_pending_write {
        return Err("helper-writer-not-observed");
    }
    // A caller cannot ignore the first fault and obtain a graceful receipt.
    if session.finish(Instant::now() + CONTROL_BUDGET) != Err(Fault::UnexpectedExit) {
        return Err("helper-forced-finish");
    }
    close_runtime(runtime)?;
    println!("codex-transport-blocked-writer: native-code=0");

    let (mut session, runtime) = fresh(Mode::Tree)?;
    let receipt = session
        .recv_frame(Instant::now() + CONTROL_BUDGET, &cancel)
        .map_err(|_| "helper-tree-ready")?;
    let pid = descendant_pid(&receipt).ok_or("helper-tree-receipt")?;
    let child = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    }
    .map_err(|_| "helper-tree-open")?;
    let child = unsafe { OwnedHandle::from_raw_handle(child.0) };
    let mut member = false.into();
    unsafe { IsProcessInJob(raw(&child), Some(raw(&session.job.0)), &mut member) }
        .map_err(|_| "helper-tree-job")?;
    if !member.as_bool() || unsafe { WaitForSingleObject(raw(&child), 0) } != WAIT_TIMEOUT {
        return Err("helper-tree-membership");
    }
    cancel.store(true, Ordering::Release);
    if session.recv_frame(Instant::now() + CONTROL_BUDGET, &cancel) != Err(Fault::Cancelled) {
        return Err("helper-tree-cancel");
    }
    settled(&session)?;
    if unsafe { WaitForSingleObject(raw(&child), 0) } != WAIT_OBJECT_0 {
        return Err("helper-tree-descendant-exit");
    }
    drop(child);
    drop(session);
    close_runtime(runtime)?;
    println!("codex-transport-owned-descendant: native-code=0");
    Ok(())
}

fn descendant_pid(bytes: &[u8]) -> Option<u32> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Receipt {
        descendant: u32,
    }
    if bytes.len() > 64 {
        return None;
    }
    let receipt: Receipt = serde_json::from_slice(bytes).ok()?;
    (receipt.descendant != 0).then_some(receipt.descendant)
}

/// Finite dispatcher for this program's synthetic helper child, never a shell.
pub(crate) fn child(mode: &str) -> Result<(), &'static str> {
    match mode {
        "nonzero" => std::process::exit(7),
        "truncated" => {
            let mut output = std::io::stdout().lock();
            output
                .write_all(b"{\"unterminated\"")
                .map_err(|_| "helper-write")?;
            output.flush().map_err(|_| "helper-flush")?;
        }
        "blocked" => {
            println!(r#"{{"ready":1}}"#);
            std::io::stdout().flush().map_err(|_| "helper-flush")?;
            std::thread::sleep(Duration::from_secs(60));
        }
        "tree" => {
            let mut child = Command::new(std::env::current_exe().map_err(|_| "helper-current")?)
                .args(["transport-child", "leaf"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| "helper-child-spawn")?;
            println!(r#"{{"descendant":{}}}"#, child.id());
            std::io::stdout().flush().map_err(|_| "helper-flush")?;
            let _ = child.wait().map_err(|_| "helper-child-wait")?;
        }
        "leaf" => std::thread::sleep(Duration::from_secs(60)),
        _ => return Err("helper-mode"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descendant_receipt_is_one_fixed_bounded_nonzero_pid() {
        assert_eq!(descendant_pid(br#"{"descendant":1}"#), Some(1));
        for invalid in [
            br#"{"descendant":0}"#.as_slice(),
            br#"{"descendant":4294967296}"#,
            br#"{"descendant":1,"extra":2}"#,
            br#"{"descendant":"1"}"#,
            br#"{"descendant":1,"descendant":2}"#,
            br#"[]"#,
            br#"{"pid":1}"#,
        ] {
            assert_eq!(descendant_pid(invalid), None);
        }
    }
}
