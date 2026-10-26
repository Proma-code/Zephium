//! Fixed fresh normal-config write qualification, not a production store factory.
const PAYLOAD: &str = "synthetic-settings.json";
const TEMPORARY: &str = ".synthetic-settings.tmp";
#[cfg(windows)]
const CONTROL_DIRECTORY: &str = "synthetic-control-directory";
const INITIAL: &[u8] = b"synthetic initial settings";
pub(super) const REWRITTEN: &[u8] = b"synthetic rewritten settings";
pub(super) const REPLACED: &[u8] = b"synthetic replaced settings";
const OK: &[u8] = b"store-config-ok\n";
const FAILED_STDERR: &[u8] = b"claude-broker-fixture: synthetic normal config I/O failed\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Operation {
    Write,
    Replace,
}
impl Operation {
    pub(super) fn expected(self) -> &'static [u8] {
        match self {
            Self::Write => REWRITTEN,
            Self::Replace => REPLACED,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Failure {
    pub(super) stage: i32,
    pub(super) code: i32,
}
pub(super) fn validate(
    code: i32,
    stopped: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<(), Failure> {
    if !stopped && code == 0 && stdout == OK && stderr.is_empty() {
        return Ok(());
    }
    if !stopped && code == 1 && stderr == FAILED_STDERR && stdout.len() <= 64 {
        if let Some(value) = std::str::from_utf8(stdout)
            .ok()
            .and_then(|s| s.strip_prefix("store-config-failed="))
            .and_then(|s| s.strip_suffix('\n'))
        {
            if let Some((stage, code)) = value.split_once(',') {
                if let (Ok(stage_number), Ok(code_number)) =
                    (stage.parse::<i32>(), code.parse::<i32>())
                {
                    if (1..=10).contains(&stage_number)
                        && stage_number.to_string() == stage
                        && code_number.to_string() == code
                    {
                        return Err(Failure {
                            stage: stage_number,
                            code: code_number,
                        });
                    }
                }
            }
        }
    }
    Err(Failure { stage: 0, code: -1 })
}

#[cfg(windows)]
pub(super) fn child(operation: Operation) -> Result<(), &'static str> {
    use std::{
        fs::{File, OpenOptions},
        io::{Read, Write},
        path::Path,
    };
    let mut input = Vec::new();
    std::io::stdin()
        .take(4097)
        .read_to_end(&mut input)
        .map_err(|_| "synthetic config input invalid")?;
    if input.len() > 4096 {
        return Err("synthetic config input invalid");
    }
    let value = crate::strict_json::parse(&input).map_err(|_| "synthetic config input invalid")?;
    let object = value.as_object().ok_or("synthetic config input invalid")?;
    if object.len() != 2 {
        return Err("synthetic config input invalid");
    }
    let config = object
        .get("config")
        .and_then(serde_json::Value::as_str)
        .ok_or("synthetic config input invalid")?;
    if config.is_empty()
        || std::env::var_os("CLAUDE_CONFIG_DIR").as_deref() != Some(std::ffi::OsStr::new(config))
    {
        return Err("synthetic config binding failed");
    }
    let foreign = object
        .get("foreign")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or("synthetic foreign binding invalid")?;
    let target = Path::new(config).join(PAYLOAD);
    let temporary = Path::new(config).join(TEMPORARY);
    let step = |stage, result: std::io::Result<()>| {
        result.map_err(|error| Failure {
            stage,
            code: error.raw_os_error().unwrap_or(-1),
        })
    };
    let check = |path: &Path, expected: &[u8]| -> std::io::Result<()> {
        let mut bytes = Vec::new();
        File::open(path)?.take(128).read_to_end(&mut bytes)?;
        if bytes != expected {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        Ok(())
    };
    let result = (|| -> Result<(), Failure> {
        if operation == Operation::Write {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| Failure {
                    stage: 1,
                    code: e.raw_os_error().unwrap_or(-1),
                })?;
            step(2, file.write_all(INITIAL).and_then(|_| file.sync_all()))?;
            drop(file);
            step(3, check(&target, INITIAL))?;
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&target)
                .map_err(|e| Failure {
                    stage: 4,
                    code: e.raw_os_error().unwrap_or(-1),
                })?;
            step(4, file.write_all(REWRITTEN).and_then(|_| file.sync_all()))?;
            drop(file);
            step(5, check(&target, REWRITTEN))?;
            return child_rights(Path::new(config), Path::new(foreign));
        }
        step(5, check(&target, REWRITTEN))?;
        child_rights(Path::new(config), Path::new(foreign))?;
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| Failure {
                stage: 6,
                code: e.raw_os_error().unwrap_or(-1),
            })?;
        step(6, temp.write_all(REPLACED).and_then(|_| temp.sync_all()))?;
        drop(temp);
        // Deliberately close temp first: this measures this ordinary path-based
        // replace contract, not every possible original-client save algorithm.
        step(7, std::fs::rename(&temporary, &target))?;
        step(8, check(&target, REPLACED))?;
        child_rights(Path::new(config), Path::new(foreign))
    })();
    match result {
        Ok(()) => std::io::stdout()
            .write_all(OK)
            .map_err(|_| "synthetic config receipt failed"),
        Err(failure) => {
            std::io::stdout()
                .write_all(
                    format!("store-config-failed={},{}\n", failure.stage, failure.code).as_bytes(),
                )
                .map_err(|_| "synthetic config receipt failed")?;
            Err("synthetic normal config I/O failed")
        }
    }
}

// The finite tested rights are shared by parent positives and child negatives.
// Parent opens additionally request READ_ATTRIBUTES for same-handle identity;
// a positive for that superset still proves the single tested right. Child
// negatives request only the tested right, never the metadata superset.
// None of these opens mutates a descriptor, deletes an object or reads foreign
// data. Actual root DELETE is not tested through its no-delete-share held handle;
// an independently openable inherited directory provides that differential.
#[cfg(windows)]
const DIRECTORY_RIGHTS: [u32; 3] = [0x0004_0000, 0x0008_0000, 0x40];
#[cfg(windows)]
const PAYLOAD_ADMIN_RIGHTS: [u32; 2] = [0x0004_0000, 0x0008_0000];
#[cfg(windows)]
const FOREIGN_RIGHTS: [u32; 5] = [1, 2, 0x0001_0000, 0x0004_0000, 0x0008_0000];
#[cfg(windows)]
fn rights_open(path: &std::path::Path, right: u32) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .access_mode(right)
        .share_mode(7)
        .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        .open(path)
}
#[cfg(windows)]
fn child_rights(config: &std::path::Path, foreign: &std::path::Path) -> Result<(), Failure> {
    fn deny(path: &std::path::Path, right: u32, stage: i32) -> Result<(), Failure> {
        match rights_open(path, right) {
            Err(error) if error.raw_os_error() == Some(5) => Ok(()),
            Err(error) => Err(Failure {
                stage,
                code: error.raw_os_error().unwrap_or(-1),
            }),
            Ok(_) => Err(Failure { stage, code: 0 }),
        }
    }
    let root = config.parent().ok_or(Failure { stage: 9, code: -1 })?;
    for path in [root, config] {
        for right in DIRECTORY_RIGHTS {
            deny(path, right, 9)?;
        }
    }
    for right in PAYLOAD_ADMIN_RIGHTS {
        deny(&config.join(PAYLOAD), right, 9)?;
    }
    deny(&config.join(CONTROL_DIRECTORY), 0x0001_0000, 9)?;
    for right in FOREIGN_RIGHTS {
        deny(foreign, right, 10)?;
    }
    Ok(())
}

#[cfg(windows)]
fn file_identity(file: &std::fs::File) -> Result<(u32, u32, u32, bool), &'static str> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: a live owned File and initialized output buffer are held throughout.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| "synthetic rights identity unavailable")?;
    let directory = info.dwFileAttributes & 0x10 != 0;
    if info.dwFileAttributes & 0x400 != 0 || (!directory && info.nNumberOfLinks != 1) {
        return Err("synthetic rights target aliased");
    }
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
        directory,
    ))
}
#[cfg(windows)]
fn parent_rights(path: &std::path::Path, rights: &[u32]) -> Result<(), &'static str> {
    let reference =
        rights_open(path, 0x0002_0080).map_err(|_| "synthetic rights reference unavailable")?;
    let expected = file_identity(&reference)?;
    for &right in rights {
        let file = rights_open(path, right | 0x80).map_err(|error| {
            super::diagnostic(
                "store-rights-parent-open",
                error.raw_os_error().unwrap_or(-1),
            );
            "synthetic rights parent positive failed"
        })?;
        if file_identity(&file)? != expected {
            return Err("synthetic rights parent target changed");
        }
    }
    Ok(())
}
#[cfg(windows)]
fn parent_config_rights(
    config: &std::path::Path,
    payload_exists: bool,
) -> Result<(), &'static str> {
    let root = config.parent().ok_or("synthetic root unavailable")?;
    for path in [root, config] {
        parent_rights(path, &DIRECTORY_RIGHTS)?;
    }
    parent_rights(&config.join(CONTROL_DIRECTORY), &[0x0001_0000])?;
    if payload_exists {
        parent_rights(&config.join(PAYLOAD), &PAYLOAD_ADMIN_RIGHTS)?;
    }
    Ok(())
}

/// Exclusively fresh synthetic data under a separate package identity. Callers
/// cannot import a path or choose the contents. Field order closes the held file
/// before owned-root cleanup, then releases its Profile.
#[cfg(windows)]
pub(super) struct ForeignSentinel {
    file: std::fs::File,
    root: super::fixtures::Root,
    _profile: super::security::Profile,
}
#[cfg(windows)]
impl ForeignSentinel {
    const CONTENT: &'static [u8] = b"synthetic foreign profile data";
    fn create(suffix: &str) -> Result<Self, &'static str> {
        use std::{io::Write, os::windows::fs::OpenOptionsExt};
        let profile =
            super::security::Profile::create(&format!("Zephium.ClaudeFixture.{suffix}.Foreign"))?;
        let root = super::fixtures::Root::create(
            std::env::temp_dir().join(format!("Zephium.ClaudeFixture.{suffix}.Foreign")),
            &profile,
        )?;
        // Inherits only this fresh foreign Profile's private root descriptor.
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(7)
            .custom_flags(0x0020_0000)
            .open(root.0.join("foreign-data.bin"))
            .map_err(|_| "fresh foreign sentinel unavailable")?;
        file.write_all(Self::CONTENT)
            .and_then(|_| file.sync_all())
            .map_err(|_| "fresh foreign sentinel write failed")?;
        let result = Self {
            file,
            root,
            _profile: profile,
        };
        result.verify()?;
        Ok(result)
    }
    pub(super) fn path(&self) -> std::path::PathBuf {
        self.root.0.join("foreign-data.bin")
    }
    pub(super) fn verify(&self) -> Result<(), &'static str> {
        use std::io::{Read, Seek, SeekFrom};
        let expected = file_identity(&self.file)?;
        if expected.3 {
            return Err("foreign sentinel was not a file");
        }
        parent_rights(&self.path(), &FOREIGN_RIGHTS)?;
        let current =
            rights_open(&self.path(), 1 | 0x80).map_err(|_| "foreign sentinel unavailable")?;
        if file_identity(&current)? != expected {
            return Err("foreign sentinel identity changed");
        }
        let mut file = &self.file;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "foreign sentinel seek failed")?;
        let mut bytes = Vec::new();
        file.take(128)
            .read_to_end(&mut bytes)
            .map_err(|_| "foreign sentinel read failed")?;
        if bytes != Self::CONTENT {
            return Err("foreign sentinel content changed");
        }
        Ok(())
    }
    fn cleanup(self) -> Result<(), &'static str> {
        let Self {
            file,
            root,
            _profile,
        } = self;
        drop(file);
        root.cleanup()?;
        drop(_profile);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Comparison {
    Baseline,
    FileDelete,
}
fn expected_baseline_failure(
    mode: Comparison,
    operation: Operation,
    result: Result<(), Failure>,
) -> bool {
    mode == Comparison::Baseline
        && operation == Operation::Replace
        && result == Err(Failure { stage: 7, code: 5 })
}

#[cfg(windows)]
pub(super) fn controls() -> Result<(), &'static str> {
    comparison(Comparison::Baseline)?;
    comparison(Comparison::FileDelete)
}
#[cfg(windows)]
fn comparison(mode: Comparison) -> Result<(), &'static str> {
    use super::{
        account_store::{ActiveStore, FixtureDomain, FixtureKey, FixtureStoreOwner, StoreLease},
        fixtures::Root,
        process::{self, Layout},
        security::{self, HeldImage, Profile},
    };
    use std::{
        fs::OpenOptions,
        os::windows::fs::{MetadataExt, OpenOptionsExt},
        time::{SystemTime, UNIX_EPOCH},
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock unavailable")?
        .as_nanos();
    let suffix = format!("{}.{}.StoreConfig.{mode:?}", std::process::id(), stamp);
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}"))?;
    let key = FixtureKey {
        domain: FixtureDomain::TrialModel,
        profile: stamp,
        incarnation: 1,
    };
    let (owner, lease) = match mode {
        Comparison::Baseline => FixtureStoreOwner::create(profile, key),
        Comparison::FileDelete => FixtureStoreOwner::create_replace_fixture(profile, key),
    }
    .map_err(|_| "fresh config store unavailable")?;
    let receipt = owner.receipt();
    let active = lease
        .begin_operation()
        .map_err(|_| "fresh config store admission failed")?;
    if active.fixture_key() != key {
        return Err("fresh config key mismatch");
    }
    let profile = active
        .fixture_profile()
        .map_err(|_| "fresh config profile unavailable")?;
    let root = Root::create(
        std::env::temp_dir().join(format!("Zephium.ClaudeFixture.{suffix}")),
        &profile,
    )?;
    let layout = Layout::create(&root.0, &profile)?;
    let config = active
        .config_path()
        .map_err(|_| "fresh config path unavailable")?;
    if std::fs::read_dir(&config)
        .map_err(|_| "fresh config empty proof failed")?
        .next()
        .is_some()
    {
        return Err("fresh config was not empty");
    }
    std::fs::create_dir(config.join(CONTROL_DIRECTORY))
        .map_err(|_| "fresh control directory failed")?;
    parent_config_rights(&config, false)?;
    let foreign = ForeignSentinel::create(&suffix)?;
    let source_path = std::env::current_exe().map_err(|_| "synthetic image unavailable")?;
    let mut source = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .custom_flags(0x0020_0000)
        .open(&source_path)
        .map_err(|_| "synthetic image cannot be held")?;
    let metadata = source
        .metadata()
        .map_err(|_| "synthetic image metadata unavailable")?;
    if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
        return Err("synthetic image was aliased");
    }
    let hash = security::hash(&mut source)?;
    let target = root.0.join("fixture.exe");
    profile.copy_image(&source_path, &target)?;
    let image = HeldImage::open(&target, hash)?;
    let mut first = Some(active);
    for operation in [Operation::Write, Operation::Replace] {
        let active = match first.take() {
            Some(active) => active,
            None => lease
                .begin_operation()
                .map_err(|_| "normal config next admission failed")?,
        };
        let call = process::run_store_config(active, &image, &layout, operation, &foreign)?;
        let observed = call.fixture_result();
        if mode == Comparison::Baseline && operation == Operation::Replace && observed.is_ok() {
            // An unexpected baseline success is not an authority to clear it.
            drop(call);
            lease
                .fixture_failed_dirty()
                .map_err(|_| "unexpected baseline lost dirty custody")?;
            return Err("baseline unexpectedly allowed replacement");
        }
        // Still owns the active store: any parent negative-control failure drops
        // it DIRTY rather than granting CLEAN on a child success receipt alone.
        parent_config_rights(&config, true)?;
        foreign.verify()?;
        if let Err(error) = ActiveStore::finish_config_fixture(call) {
            lease
                .fixture_failed_dirty()
                .map_err(|_| "failed config lost dirty custody")?;
            drop(lease);
            if !matches!(
                StoreLease::reopen(&receipt, key),
                Err(super::account_store::Error::Busy)
            ) {
                return Err("failed config released its retained lease");
            }
            super::diagnostic("store-normal-config-error-dirty", 0);
            if !expected_baseline_failure(mode, operation, observed) {
                return Err(error);
            }
            drop(image);
            drop(source);
            root.cleanup()?;
            foreign.cleanup()?;
            super::diagnostic("store-replace-baseline-denied", 0);
            return Ok(());
        }
        lease
            .verify()
            .map_err(|_| "settled config lease rejected")?;
        super::diagnostic(
            match (mode, operation) {
                (Comparison::Baseline, Operation::Write) => "store-baseline-write-settled",
                (Comparison::Baseline, Operation::Replace) => unreachable!(),
                (Comparison::FileDelete, Operation::Write) => "store-normal-write-settled",
                (Comparison::FileDelete, Operation::Replace) => "store-normal-replace-settled",
            },
            0,
        );
    }
    drop(lease);
    let reopened =
        StoreLease::reopen(&receipt, key).map_err(|_| "normal config clean reopen failed")?;
    reopened
        .verify()
        .map_err(|_| "normal config reopened proof failed")?;
    drop(reopened);
    drop(image);
    drop(source);
    root.cleanup()?;
    foreign.cleanup()?;
    drop(profile);
    drop(owner);
    drop(receipt);
    super::diagnostic("store-normal-config-settled", 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_keeps_its_exact_expected_payload() {
        assert_eq!(Operation::Write.expected(), REWRITTEN);
        assert_eq!(Operation::Replace.expected(), REPLACED);
        assert_ne!(Operation::Write.expected(), Operation::Replace.expected());
    }
    #[test]
    fn only_exact_zero_exit_success_can_clean() {
        assert_eq!(validate(0, false, OK, b""), Ok(()));
        for (code, stopped, out, err) in [
            (1, false, OK, b"".as_slice()),
            (0, true, OK, b""),
            (0, false, OK, b"noise"),
            (0, false, b"store-config-ok\nextra", b""),
        ] {
            assert!(validate(code, stopped, out, err).is_err());
        }
    }
    #[test]
    fn compatibility_failure_is_never_success_and_has_finite_diagnostics() {
        assert_eq!(
            validate(1, false, b"store-config-failed=7,5\n", FAILED_STDERR),
            Err(Failure { stage: 7, code: 5 })
        );
        for out in [
            b"store-config-failed=11,5\n".as_slice(),
            b"store-config-failed=07,5\n",
            b"store-config-failed=7,+5\n",
            b"private path\n",
        ] {
            assert_eq!(
                validate(1, false, out, FAILED_STDERR),
                Err(Failure { stage: 0, code: -1 })
            );
        }
    }
    #[test]
    fn baseline_requires_exact_typed_replace_denial() {
        let denied = Err(Failure { stage: 7, code: 5 });
        assert!(expected_baseline_failure(
            Comparison::Baseline,
            Operation::Replace,
            denied
        ));
        assert!(!expected_baseline_failure(
            Comparison::FileDelete,
            Operation::Replace,
            denied
        ));
        assert!(!expected_baseline_failure(
            Comparison::Baseline,
            Operation::Write,
            denied
        ));
        for result in [
            Ok(()),
            Err(Failure { stage: 7, code: 32 }),
            Err(Failure { stage: 9, code: 5 }),
            Err(Failure { stage: 0, code: -1 }),
        ] {
            assert!(!expected_baseline_failure(
                Comparison::Baseline,
                Operation::Replace,
                result
            ));
        }
    }
}
