//! Fixed fresh normal-config write qualification, not a production store factory.
const PAYLOAD: &str = "synthetic-settings.json";
const TEMPORARY: &str = ".synthetic-settings.tmp";
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
                    if (1..=8).contains(&stage_number)
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
    if object.len() != 1 {
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
            return Ok(());
        }
        step(5, check(&target, REWRITTEN))?;
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
        Ok(())
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

#[cfg(windows)]
pub(super) fn controls() -> Result<(), &'static str> {
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
    let suffix = format!("{}.{}.StoreConfig", std::process::id(), stamp);
    let profile = Profile::create(&format!("Zephium.ClaudeFixture.{suffix}"))?;
    let key = FixtureKey {
        domain: FixtureDomain::TrialModel,
        profile: stamp,
        incarnation: 1,
    };
    let (owner, lease) =
        FixtureStoreOwner::create(profile, key).map_err(|_| "fresh config store unavailable")?;
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
    if std::fs::read_dir(
        active
            .config_path()
            .map_err(|_| "fresh config path unavailable")?,
    )
    .map_err(|_| "fresh config empty proof failed")?
    .next()
    .is_some()
    {
        return Err("fresh config was not empty");
    }
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
        let call = process::run_store_config(active, &image, &layout, operation)?;
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
            return Err(error);
        }
        lease
            .verify()
            .map_err(|_| "settled config lease rejected")?;
        super::diagnostic(
            match operation {
                Operation::Write => "store-normal-write-settled",
                Operation::Replace => "store-normal-replace-settled",
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
            b"store-config-failed=9,5\n".as_slice(),
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
}
