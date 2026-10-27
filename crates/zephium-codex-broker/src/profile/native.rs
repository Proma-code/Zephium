//! Fixed Windows trial home authority. No external path/config/credential adoption.
#[cfg(any(feature = "native-fixtures", feature = "trial-integration"))]
pub(crate) mod removal;
use super::{Migration, NodeId, ProfileId, Record, CLEAN, DIRTY, LENGTH, MIGRATION_LENGTH};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    mem::{offset_of, size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::Mutex,
};
use windows::{
    core::{BOOL, PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation,
            IsValidAcl, IsValidSid, TokenUser, ACL, DACL_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
            TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, FileDispositionInfo, FileIdInfo, FileRenameInfo,
            GetFileInformationByHandle, GetFileInformationByHandleEx, SetFileInformationByHandle,
            BY_HANDLE_FILE_INFORMATION, CREATE_NEW, FILE_DISPOSITION_INFO,
            FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_INFO, FILE_RENAME_INFO, FILE_SHARE_MODE,
        },
        System::{
            Com::{CoCreateGuid, CoTaskMemFree},
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
        UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT},
    },
};

const LEAF: &str = "Zephium.SubscriptionTrial.CodexProfiles";
const NAMES: [&str; 6] = [
    "codex-home",
    "work",
    "temp",
    "appdata",
    "localappdata",
    "userprofile",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Invalid,
    Io,
    Busy,
    RecoveryRequired,
    Configuration,
    Scope,
}

// Closed numeric diagnostics for the explicit synthetic native tests only.
// Production returns the same Error::Io without printing paths, contents, or errors.
#[derive(Clone, Copy)]
#[repr(u8)]
enum IoStage {
    BaseMetadata = 1,
    RootMetadata = 2,
    LeaseSeek = 3,
    LeaseWrite = 4,
    LeaseLength = 5,
    LeaseSync = 6,
    FileLength = 7,
    NewWrite = 8,
    NewSync = 9,
    StageMetadata = 10,
    StageDiscardOpen = 11,
    StageDiscardDelete = 12,
    StageWrite = 13,
    StageSync = 14,
    StageReopen = 15,
    ReadSeek = 17,
    ReadExact = 18,
    DirectoryCreate = 19,
    FileCreate = 20,
    ReadOpen = 21,
    VerifyMetadata = 22,
    RenameByHandle = 23,
}
fn io_failure(stage: IoStage, error: std::io::Error) -> Error {
    #[cfg(test)]
    eprintln!(
        "owned-profile-io: stage={} win32={:?}",
        stage as u8,
        error.raw_os_error()
    );
    let _ = (stage, error);
    Error::Io
}
#[cfg(test)]
thread_local! {
    static LAST_NATIVE_FAILURE: std::cell::Cell<Option<(u8, u32)>> = const { std::cell::Cell::new(None) };
}
fn win_failure(stage: IoStage, error: windows::core::Error) -> Error {
    #[cfg(test)]
    LAST_NATIVE_FAILURE.set(Some((stage as u8, error.code().0 as u32)));
    #[cfg(test)]
    eprintln!(
        "owned-profile-io: stage={} hresult={:#010x}",
        stage as u8,
        error.code().0 as u32
    );
    let _ = (stage, error);
    Error::Io
}

/// Never Clone. Every live runtime keeps this exact exclusive lease and pins.
pub(crate) struct ProfileLease {
    pub(crate) root: PathBuf,
    pub(crate) record: Record,
    _pins: Vec<File>,
    lock: Mutex<File>,
}
impl ProfileLease {
    pub(crate) fn open(profile: ProfileId) -> Result<Self, Error> {
        let local = known_local()?;
        let mut ancestors = Vec::new();
        for path in local.ancestors().collect::<Vec<_>>().into_iter().rev() {
            ancestors.push(crate::image::pin_directory(path).map_err(|_| Error::Invalid)?);
        }
        let owner = owner_sid()?;
        let base = local.join(LEAF);
        match std::fs::symlink_metadata(&base) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_dir(&base, &owner)?
            }
            Err(error) => return Err(io_failure(IoStage::BaseMetadata, error)),
        }
        ancestors.push(hold_directory(&base, &owner)?);
        Self::under(&base, profile, &owner, ancestors)
    }
    /// Removal of a canonically configured owner is observational here: never
    /// provision a replacement home or migrate metadata to make logout work.
    #[cfg(any(feature = "native-fixtures", feature = "trial-integration"))]
    pub(crate) fn open_existing_for_removal(
        profile: ProfileId,
        incarnation: [u8; 16],
    ) -> Result<Self, Error> {
        let local = known_local()?;
        let mut ancestors = Vec::new();
        for path in local.ancestors().collect::<Vec<_>>().into_iter().rev() {
            ancestors.push(crate::image::pin_directory(path).map_err(|_| Error::Invalid)?);
        }
        let owner = owner_sid()?;
        Self::existing_for_removal_under(
            &local.join(LEAF),
            profile,
            incarnation,
            &owner,
            &ancestors,
        )
    }
    #[cfg(any(feature = "native-fixtures", feature = "trial-integration"))]
    fn existing_for_removal_under(
        base: &Path,
        profile: ProfileId,
        incarnation: [u8; 16],
        owner: &str,
        ancestors: &[File],
    ) -> Result<Self, Error> {
        if incarnation == [0; 16] {
            return Err(Error::Scope);
        }
        let exists = |path: &Path| match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::RecoveryRequired)
            }
            Err(error) => Err(io_failure(IoStage::RootMetadata, error)),
        };
        exists(base)?;
        let parent = hold_directory(base, owner)?;
        removal::require_no_intent(base, profile)?;
        let root = base.join(profile.leaf());
        exists(&root)?;
        let root_hold = hold_metadata_directory(&root, owner)?;
        exists(&root.join("lease"))?;
        exists(&root.join("registration"))?;
        let mut registration = open_read(&root.join("registration"))?;
        verify_file(&registration, owner)?;
        let owner_hash = Sha256::digest(owner.as_bytes()).into();
        let record = Record::decode(
            &read_bounded(&mut registration, LENGTH)?,
            profile,
            owner_hash,
        )
        .ok_or(Error::Scope)?;
        if record.incarnation != incarnation || identity(&root_hold)? != record.nodes[0] {
            return Err(Error::Scope);
        }
        if record.version != super::VERSION {
            // A known historical registration is not permission to migrate in
            // this existing-only removal admission. Ordinary Connect owns that.
            return Err(Error::RecoveryRequired);
        }
        // Keep the original root/registration guarded while the exact exclusive
        // CLEAN lease and every fixed identity/byte receipt are reconstructed.
        let lease =
            removal::recovery::recover_prepared_lease(base, owner, &record, &parent, ancestors)?;
        removal::require_no_intent(base, profile)?;
        Ok(lease)
    }
    // Only the explicit native fixture may supply a disposable parent. Product
    // admission has exactly the KnownFolder route above, never a renderer path.
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    pub(crate) fn fixture(parent: &Path, profile: ProfileId) -> Result<Self, Error> {
        let owner = owner_sid()?;
        let parent_hold = crate::image::pin_directory(parent).map_err(|_| Error::Invalid)?;
        Self::under(parent, profile, &owner, vec![parent_hold])
    }
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    pub(crate) fn fixture_protected(parent: &Path, profile: ProfileId) -> Result<Self, Error> {
        let base = parent.join("removal-owned-parent");
        create_dir(&base, &owner_sid()?)?;
        Self::fixture(&base, profile)
    }
    fn under(
        base: &Path,
        profile: ProfileId,
        owner: &str,
        mut pins: Vec<File>,
    ) -> Result<Self, Error> {
        #[cfg(any(feature = "native-fixtures", feature = "trial-integration"))]
        removal::require_no_intent(base, profile)?;
        let root = base.join(profile.leaf());
        let fresh = match std::fs::symlink_metadata(&root) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_dir(&root, owner)?;
                true
            }
            Err(error) => return Err(io_failure(IoStage::RootMetadata, error)),
        };
        let root_hold = hold_metadata_directory(&root, owner)?;
        let mut lock = if fresh {
            create_file(&root.join("lease"), owner, 0)?
        } else {
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .custom_flags(0x0020_0000)
                .open(root.join("lease"))
                .map_err(|e| {
                    if e.raw_os_error() == Some(32) {
                        Error::Busy
                    } else {
                        Error::Invalid
                    }
                })?
        };
        verify_file(&lock, owner)?;
        let marker = if fresh {
            write_marker(&mut lock, DIRTY)?;
            DIRTY.to_vec()
        } else {
            let marker = read_variable(&mut lock, MIGRATION_LENGTH)?;
            if marker.as_slice() != CLEAN && marker.len() != MIGRATION_LENGTH {
                return Err(Error::RecoveryRequired);
            }
            marker
        };
        let mut nodes = [NodeId {
            volume: 0,
            file: [0; 16],
        }; 8];
        nodes[0] = identity(&root_hold)?;
        nodes[7] = identity(&lock)?;
        pins.push(root_hold);
        for (index, name) in NAMES.iter().enumerate() {
            let path = root.join(name);
            if fresh {
                create_dir(&path, owner)?
            }
            let held = if index == 0 {
                hold_metadata_directory(&path, owner)?
            } else {
                hold_directory(&path, owner)?
            };
            nodes[index + 1] = identity(&held)?;
            pins.push(held);
        }
        let home = root.join(NAMES[0]);
        let owner_hash: [u8; 32] = Sha256::digest(owner.as_bytes()).into();
        let environments: [u8; 32] =
            Sha256::digest(crate::config::ENVIRONMENTS_TOML.as_bytes()).into();
        if fresh {
            for (path, bytes) in [
                (
                    home.join("config.toml"),
                    super::managed_config().into_owned(),
                ),
                (
                    home.join("environments.toml"),
                    crate::config::ENVIRONMENTS_TOML.into(),
                ),
            ] {
                write_new(&path, owner, bytes.as_bytes())?;
            }
        }
        let mut environment = open_read(&home.join("environments.toml"))?;
        verify_file(&environment, owner)?;
        if read_bounded(&mut environment, crate::config::ENVIRONMENTS_TOML.len())?
            != crate::config::ENVIRONMENTS_TOML.as_bytes()
        {
            return Err(Error::Configuration);
        }
        pins.push(environment);
        let record = if fresh {
            let record = Record {
                version: super::VERSION,
                profile,
                incarnation: random_id()?,
                owner: owner_hash,
                nodes,
                config: super::managed_policy_hash(),
                environments,
            };
            write_new(
                &root.join("registration"),
                owner,
                &record.encode().ok_or(Error::Invalid)?,
            )?;
            record
        } else {
            let mut file = open_read(&root.join("registration"))?;
            verify_file(&file, owner)?;
            let record = Record::decode(&read_bounded(&mut file, LENGTH)?, profile, owner_hash)
                .ok_or(Error::Scope)?;
            if record.nodes != nodes
                || record.environments != environments
                || !record.policy_valid()
            {
                return Err(Error::Scope);
            }
            record
        };
        let mut config = open_read(&home.join("config.toml"))?;
        verify_file(&config, owner)?;
        let config_bytes = read_variable(&mut config, crate::config::CONFIG_TOML.len())?;
        let mut registration = open_read(&root.join("registration"))?;
        verify_file(&registration, owner)?;
        if read_bounded(&mut registration, LENGTH)? != record.encode().ok_or(Error::Invalid)? {
            return Err(Error::Scope);
        }
        let record = if fresh || marker.as_slice() == CLEAN {
            // A CLEAN lease never authorizes deleting an unexplained staging leaf.
            require_absent(&home.join(CONFIG_STAGE))?;
            require_absent(&root.join(RECORD_STAGE))?;
            if record.version == super::VERSION {
                if config_bytes != super::managed_config().as_bytes() {
                    return Err(Error::Configuration);
                }
                pins.push(config);
                pins.push(registration);
                record
            } else {
                if config_bytes != crate::config::CONFIG_TOML.as_bytes() {
                    return Err(Error::Configuration);
                }
                let journal = Migration::for_record(&record).ok_or(Error::Scope)?;
                // Complete transition intent is durable before either immutable
                // metadata pin is released. Child DIRTY is never this permission.
                write_lease(&mut lock, &journal.encode())?;
                migration_checkpoint(MigrationPoint::Journal)?;
                drop(config);
                drop(registration);
                finish_migration(&root, &home, owner, &record, &mut pins, &mut lock)?
            }
        } else {
            let journal = Migration::admit(&marker, &record).ok_or(Error::RecoveryRequired)?;
            if !journal.accepts_config(&config_bytes) {
                return Err(Error::Configuration);
            }
            drop(config);
            drop(registration);
            finish_migration(&root, &home, owner, &record, &mut pins, &mut lock)?
        };
        if fresh {
            write_marker(&mut lock, CLEAN)?;
        }
        Ok(Self {
            root,
            record,
            _pins: pins,
            lock: Mutex::new(lock),
        })
    }
    pub(crate) fn dirty(&self) -> Result<(), Error> {
        let mut lock = self.lock.lock().map_err(|_| Error::Invalid)?;
        if read_bounded(&mut lock, 8)?.as_slice() != CLEAN {
            return Err(Error::RecoveryRequired);
        }
        write_marker(&mut lock, DIRTY)
    }
    // Only RuntimePaths' contained Session settlement invokes this. CLEAN means
    // no unsettled owned child, NOT successful login/turn or credential fsync.
    pub(crate) fn physically_settled(&self) -> Result<(), Error> {
        let mut lock = self.lock.lock().map_err(|_| Error::Invalid)?;
        write_marker(&mut lock, CLEAN)
    }
}
fn write_marker(file: &mut File, bytes: &[u8; 8]) -> Result<(), Error> {
    write_lease(file, bytes)
}
fn write_lease(file: &mut File, bytes: &[u8]) -> Result<(), Error> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_failure(IoStage::LeaseSeek, error))?;
    file.write_all(bytes)
        .map_err(|error| io_failure(IoStage::LeaseWrite, error))?;
    file.set_len(bytes.len() as u64)
        .map_err(|error| io_failure(IoStage::LeaseLength, error))?;
    file.sync_all()
        .map_err(|error| io_failure(IoStage::LeaseSync, error))
}
fn read_variable(file: &mut File, max: usize) -> Result<Vec<u8>, Error> {
    let len = usize::try_from(
        file.metadata()
            .map_err(|error| io_failure(IoStage::FileLength, error))?
            .len(),
    )
    .map_err(|_| Error::Invalid)?;
    if len > max {
        return Err(Error::Invalid);
    }
    read_bounded(file, len)
}
const CONFIG_STAGE: &str = "config.toml.zephium-next";
const RECORD_STAGE: &str = "registration.zephium-next";
fn require_absent(path: &Path) -> Result<(), Error> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(Error::Configuration),
    }
}
fn write_new(path: &Path, owner: &str, bytes: &[u8]) -> Result<(), Error> {
    let mut file = create_file(path, owner, 1)?;
    file.write_all(bytes)
        .map_err(|error| io_failure(IoStage::NewWrite, error))?;
    file.sync_all()
        .map_err(|error| io_failure(IoStage::NewSync, error))
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationPoint {
    Journal,
    StageCreated,
    RegistrationStageCreated,
    Config,
    Registration,
    BeforeClean,
}
#[cfg(test)]
thread_local! {static MIGRATION_STOP:std::cell::Cell<Option<MigrationPoint>>=const {std::cell::Cell::new(None)};}
fn migration_checkpoint(point: MigrationPoint) -> Result<(), Error> {
    #[cfg(test)]
    if MIGRATION_STOP.with(|stop| stop.get() == Some(point)) {
        return Err(Error::Io);
    }
    let _ = point;
    Ok(())
}
/// Under a valid synced journal only, delete the exact held staging object.
/// Its bytes are never adopted, parsed, logged, or used as policy/credentials.
fn discard_reserved_stage(path: &Path, owner: &str) -> Result<(), Error> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_failure(IoStage::StageMetadata, error)),
        Ok(_) => {}
    }
    let file = OpenOptions::new()
        .access_mode(0x8001_0000)
        .share_mode(1)
        .custom_flags(0x0020_0000)
        .open(path)
        .map_err(|error| io_failure(IoStage::StageDiscardOpen, error))?;
    verify_file(&file, owner)?;
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(|error| win_failure(IoStage::StageDiscardDelete, error))?;
    drop(file);
    require_absent(path)
}
fn replace_metadata(
    path: &Path,
    stage: &Path,
    owner: &str,
    old: &[u8],
    new: &[u8],
) -> Result<File, Error> {
    // Test output is finite phase/role numbers only; no path or metadata bytes.
    let phase = |number: u8| {
        #[cfg(test)]
        eprintln!(
            "owned-profile-replace: role={} phase={number}",
            u8::from(stage.file_name() == Some(OsStr::new(RECORD_STAGE)))
        );
        let _ = number;
    };
    phase(1); // Validate/discard the reserved staging leaf under the journal.
    discard_reserved_stage(stage, owner)?;
    phase(2); // Open and verify the current target.
    let mut current = open_read(path)?;
    verify_file(&current, owner)?;
    let bytes = read_variable(&mut current, old.len().max(new.len()))?;
    if bytes == new {
        return Ok(current);
    }
    if bytes != old {
        return Err(Error::Configuration);
    }
    phase(3); // Create and sync the fixed new metadata.
    let mut staged = create_file(stage, owner, 1)?;
    migration_checkpoint(if stage.file_name() == Some(OsStr::new(RECORD_STAGE)) {
        MigrationPoint::RegistrationStageCreated
    } else {
        MigrationPoint::StageCreated
    })?;
    staged
        .write_all(new)
        .map_err(|error| io_failure(IoStage::StageWrite, error))?;
    staged
        .sync_all()
        .map_err(|error| io_failure(IoStage::StageSync, error))?;
    let expected = identity(&staged)?;
    drop(staged);
    phase(4); // Reopen and verify the staged object before rename.
    let staged = OpenOptions::new()
        // Retain the verified object and use its DELETE authority for rename.
        .access_mode(0x8001_0000)
        .share_mode(5)
        .custom_flags(0x0020_0000)
        .open(stage)
        .map_err(|error| io_failure(IoStage::StageReopen, error))?;
    verify_file(&staged, owner)?;
    if identity(&staged)? != expected {
        return Err(Error::Scope);
    }
    // The exclusive lease and directory pins remain held. Only these two fixed
    // owned metadata leaves may be replaced; never auth or rollout files.
    drop(current);
    phase(5); // Rename the same verified staged object, without reopening its path.
    rename_metadata(&staged, path)?;
    drop(staged);
    phase(6); // Reopen, verify exact identity, and verify exact new bytes.
    let mut held = open_read(path)?;
    verify_file(&held, owner)?;
    if identity(&held)? != expected || read_bounded(&mut held, new.len())? != new {
        return Err(Error::Scope);
    }
    Ok(held)
}
/// Same-volume rename of the verified held source; no path-source reopening,
/// copy fallback, POSIX replacement, or sharing relaxation. The staged content
/// and journal were synced before this call. FileRenameInfo does not promise
/// MoveFileEx WRITE_THROUGH semantics: recovery is qualified for interrupted
/// processes, not arbitrary power-loss atomicity.
fn rename_metadata(file: &File, target: &Path) -> Result<(), Error> {
    if !target.is_absolute() {
        return Err(Error::Invalid);
    }
    let mut name = wide(target.as_os_str())?;
    name.pop(); // FileNameLength excludes the optional terminating NUL.
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or(Error::Invalid)?;
    let length = offset_of!(FILE_RENAME_INFO, FileName)
        .checked_add(name_bytes)
        .ok_or(Error::Invalid)?
        .max(size_of::<FILE_RENAME_INFO>());
    let length_u32 = u32::try_from(length).map_err(|_| Error::Invalid)?;
    let name_bytes_u32 = u32::try_from(name_bytes).map_err(|_| Error::Invalid)?;
    // usize storage provides the HANDLE alignment required by FILE_RENAME_INFO.
    let mut storage = vec![0usize; length.div_ceil(size_of::<usize>())];
    let rename = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*rename).Anonymous.ReplaceIfExists = true;
        (*rename).RootDirectory = HANDLE::default();
        (*rename).FileNameLength = name_bytes_u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*rename).FileName.as_mut_ptr(), name.len());
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileRenameInfo,
            storage.as_ptr().cast(),
            length_u32,
        )
    }
    .map_err(|error| win_failure(IoStage::RenameByHandle, error))
}
fn finish_migration(
    root: &Path,
    home: &Path,
    owner: &str,
    record: &Record,
    pins: &mut Vec<File>,
    lock: &mut File,
) -> Result<Record, Error> {
    let new = record.managed();
    let config = replace_metadata(
        &home.join("config.toml"),
        &home.join(CONFIG_STAGE),
        owner,
        crate::config::CONFIG_TOML.as_bytes(),
        super::managed_config().as_bytes(),
    )?;
    pins.push(config);
    migration_checkpoint(MigrationPoint::Config)?;
    let old_bytes = record.legacy().encode().ok_or(Error::Invalid)?;
    let new_bytes = new.encode().ok_or(Error::Invalid)?;
    let registration = replace_metadata(
        &root.join("registration"),
        &root.join(RECORD_STAGE),
        owner,
        &old_bytes,
        &new_bytes,
    )?;
    pins.push(registration);
    migration_checkpoint(MigrationPoint::Registration)?;
    migration_checkpoint(MigrationPoint::BeforeClean)?;
    write_marker(lock, CLEAN)?;
    Ok(new)
}
fn read_bounded(file: &mut File, max: usize) -> Result<Vec<u8>, Error> {
    if file
        .metadata()
        .map_err(|error| io_failure(IoStage::FileLength, error))?
        .len()
        != max as u64
    {
        return Err(Error::Invalid);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_failure(IoStage::ReadSeek, error))?;
    let mut bytes = vec![0; max];
    file.read_exact(&mut bytes)
        .map_err(|error| io_failure(IoStage::ReadExact, error))?;
    Ok(bytes)
}
fn wide(value: &OsStr) -> Result<Vec<u16>, Error> {
    let mut units: Vec<_> = value.encode_wide().collect();
    if units.contains(&0) || units.len() > 32766 {
        return Err(Error::Invalid);
    }
    units.push(0);
    Ok(units)
}
fn sid(sid: PSID) -> Result<String, Error> {
    if !unsafe { IsValidSid(sid) }.as_bool() {
        return Err(Error::Invalid);
    }
    let mut value = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut value) }.map_err(|_| Error::Invalid)?;
    let result = unsafe { value.to_string() }.map_err(|_| Error::Invalid);
    unsafe { LocalFree(Some(HLOCAL(value.0.cast()))) };
    result
}
fn owner_sid() -> Result<String, Error> {
    let mut raw = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) }
        .map_err(|_| Error::Invalid)?;
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut buffer = [0usize; 128];
    let mut length = 0;
    unsafe {
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size_of_val(&buffer) as u32,
            &mut length,
        )
    }
    .map_err(|_| Error::Invalid)?;
    if length < size_of::<TOKEN_USER>() as u32 || length as usize > size_of_val(&buffer) {
        return Err(Error::Invalid);
    }
    sid(unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Descriptor {
    fn new(owner: &str, directory: bool) -> Result<Self, Error> {
        let inheritance = if directory { "OICI" } else { "" };
        let sddl = wide(OsStr::new(&format!(
            "O:{owner}D:P(A;{inheritance};FA;;;{owner})(A;{inheritance};FA;;;SY)"
        )))?;
        let mut out = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut out,
                None,
            )
        }
        .map_err(|_| Error::Invalid)?;
        Ok(Self(out))
    }
    fn attrs(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0 .0,
            bInheritHandle: BOOL::from(false),
        }
    }
}
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0 .0))) };
    }
}
fn create_dir(path: &Path, owner: &str) -> Result<(), Error> {
    let sd = Descriptor::new(owner, true)?;
    let path = wide(path.as_os_str())?;
    unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&sd.attrs())) }
        .map_err(|error| win_failure(IoStage::DirectoryCreate, error))
}
fn create_file(path: &Path, owner: &str, share: u32) -> Result<File, Error> {
    let sd = Descriptor::new(owner, false)?;
    let path = wide(path.as_os_str())?;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            0xc000_0000,
            FILE_SHARE_MODE(share),
            Some(&sd.attrs()),
            CREATE_NEW,
            FILE_FLAGS_AND_ATTRIBUTES(0x0020_0000),
            None,
        )
    }
    .map_err(|error| win_failure(IoStage::FileCreate, error))?;
    Ok(unsafe { File::from_raw_handle(handle.0) })
}
fn open_read(path: &Path) -> Result<File, Error> {
    OpenOptions::new()
        .read(true)
        .share_mode(1)
        .custom_flags(0x0020_0000)
        .open(path)
        .map_err(|error| io_failure(IoStage::ReadOpen, error))
}
fn hold_directory(path: &Path, owner: &str) -> Result<File, Error> {
    let file = crate::image::pin_directory(path).map_err(|_| Error::Invalid)?;
    verify_security(&file, owner, true)?;
    Ok(file)
}
// Only the two native-owned metadata parents need child-entry mutation: the
// profile root (registration) and codex-home (config). READ desired access and
// protected owner/DACL/no-reparse admission are unchanged; sharing WRITE lets
// Windows open the rename target directory for FILE_ADD_FILE. DELETE is still
// denied, retaining directory identity. Ancestor/image and file pins stay strict.
fn hold_metadata_directory(path: &Path, owner: &str) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(0x0220_0000)
        .open(path)
        .map_err(|_| Error::Invalid)?;
    let meta = file.metadata().map_err(|_| Error::Invalid)?;
    if !meta.is_dir() || meta.file_attributes() & 0x400 != 0 {
        return Err(Error::Invalid);
    }
    verify_security(&file, owner, true)?;
    Ok(file)
}
fn verify_file(file: &File, owner: &str) -> Result<(), Error> {
    let meta = file
        .metadata()
        .map_err(|error| io_failure(IoStage::VerifyMetadata, error))?;
    if !meta.is_file() || meta.file_attributes() & 0x400 != 0 {
        return Err(Error::Invalid);
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| Error::Invalid)?;
    if info.nNumberOfLinks != 1 {
        return Err(Error::Invalid);
    }
    verify_security(file, owner, false)
}
fn identity(file: &File) -> Result<NodeId, Error> {
    let mut id = FILE_ID_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(|_| Error::Invalid)?;
    Ok(NodeId {
        volume: id.VolumeSerialNumber,
        file: id.FileId.Identifier,
    })
}
fn acl_bytes(sd: PSECURITY_DESCRIPTOR) -> Result<Vec<u8>, Error> {
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    let mut acl: *mut ACL = std::ptr::null_mut();
    unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted) }
        .map_err(|_| Error::Invalid)?;
    if !present.as_bool()
        || defaulted.as_bool()
        || acl.is_null()
        || !unsafe { IsValidAcl(acl) }.as_bool()
    {
        return Err(Error::Invalid);
    }
    let size = unsafe { (*acl).AclSize } as usize;
    if !(size_of::<ACL>()..=4096).contains(&size) {
        return Err(Error::Invalid);
    }
    Ok(unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), size) }.to_vec())
}
fn verify_security(file: &File, owner: &str, directory: bool) -> Result<(), Error> {
    let mut actual_owner = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut actual_owner),
            None,
            None,
            None,
            Some(&mut sd),
        )
    }
    .ok()
    .map_err(|_| Error::Invalid)?;
    let actual = Descriptor(sd);
    let mut control = 0;
    let mut revision = 0;
    unsafe { GetSecurityDescriptorControl(actual.0, &mut control, &mut revision) }
        .map_err(|_| Error::Invalid)?;
    if control & 0x1000 == 0 || sid(actual_owner)? != owner {
        return Err(Error::Invalid);
    }
    let expected = Descriptor::new(owner, directory)?;
    if acl_bytes(actual.0)? != acl_bytes(expected.0)? {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn known_local() -> Result<PathBuf, Error> {
    let value = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }
        .map_err(|_| Error::Invalid)?;
    let path = unsafe { value.to_string() }
        .map(PathBuf::from)
        .map_err(|_| Error::Invalid);
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    let path = path?;
    crate::image::require_absolute_local(&path).map_err(|_| Error::Invalid)?;
    Ok(path)
}
pub(crate) fn random_id() -> Result<[u8; 16], Error> {
    let id = unsafe { CoCreateGuid() }.map_err(|_| Error::Invalid)?;
    Ok(id.to_u128().to_le_bytes())
}

#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod tests {
    use super::*;
    fn id() -> ProfileId {
        ProfileId::from_native(random_id().unwrap()).unwrap()
    }
    #[test]
    #[ignore = "owned Windows profile fixture; no account or provider calls"]
    fn fresh_profile_reopens_with_same_identity_and_exclusive_lease() {
        let parent = tempfile::tempdir().unwrap();
        let profile = id();
        let held = ProfileLease::fixture(parent.path(), profile).unwrap();
        let record = held.record.clone();
        assert!(matches!(
            ProfileLease::fixture(parent.path(), profile),
            Err(Error::Busy)
        ));
        drop(held);
        let reopened = ProfileLease::fixture(parent.path(), profile).unwrap();
        assert_eq!(record, reopened.record);
        drop(reopened);
        parent.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows profile fixture; no account or provider calls"]
    fn dirty_profile_refuses_reopen_without_implicit_recovery() {
        let parent = tempfile::tempdir().unwrap();
        let profile = id();
        let held = ProfileLease::fixture(parent.path(), profile).unwrap();
        held.dirty().unwrap();
        drop(held);
        assert!(matches!(
            ProfileLease::fixture(parent.path(), profile),
            Err(Error::RecoveryRequired)
        ));
        parent.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows profile fixture; no account or provider calls"]
    fn config_is_pinned_and_changed_config_is_never_rewritten() {
        let parent = tempfile::tempdir().unwrap();
        let profile = id();
        let held = ProfileLease::fixture(parent.path(), profile).unwrap();
        let config = held.root.join("codex-home/config.toml");
        assert_eq!(
            std::fs::write(&config, b"changed")
                .unwrap_err()
                .raw_os_error(),
            Some(32)
        );
        drop(held);
        std::fs::write(&config, b"changed").unwrap();
        assert!(ProfileLease::fixture(parent.path(), profile).is_err());
        assert_eq!(std::fs::read(config).unwrap(), b"changed");
        parent.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows profile fixture; no account or provider calls"]
    fn replaced_lease_identity_and_unregistered_collision_are_rejected() {
        let parent = tempfile::tempdir().unwrap();
        let profile = id();
        let held = ProfileLease::fixture(parent.path(), profile).unwrap();
        let root = held.root.clone();
        drop(held);
        std::fs::rename(root.join("lease"), root.join("old-lease")).unwrap();
        let mut replacement = create_file(&root.join("lease"), &owner_sid().unwrap(), 0).unwrap();
        write_marker(&mut replacement, CLEAN).unwrap();
        drop(replacement);
        assert!(matches!(
            ProfileLease::fixture(parent.path(), profile),
            Err(Error::Scope)
        ));
        let other = id();
        create_dir(&parent.path().join(other.leaf()), &owner_sid().unwrap()).unwrap();
        assert!(ProfileLease::fixture(parent.path(), other).is_err());
        parent.close().unwrap();
    }
    fn legacy_fixture(parent: &Path, profile: ProfileId) -> (PathBuf, Record) {
        let held = ProfileLease::fixture(parent, profile).unwrap();
        let root = held.root.clone();
        let record = held.record.legacy();
        drop(held);
        std::fs::write(
            root.join("codex-home/config.toml"),
            crate::config::CONFIG_TOML,
        )
        .unwrap();
        std::fs::write(root.join("registration"), record.encode().unwrap()).unwrap();
        (root, record)
    }
    #[test]
    #[ignore = "owned Windows metadata migration fixture; no account/provider calls"]
    fn managed_policy_migration_recovers_every_interrupted_boundary_without_auth_or_identity_changes(
    ) {
        for point in [
            MigrationPoint::Journal,
            MigrationPoint::StageCreated,
            MigrationPoint::RegistrationStageCreated,
            MigrationPoint::Config,
            MigrationPoint::Registration,
            MigrationPoint::BeforeClean,
        ] {
            let parent = tempfile::tempdir().unwrap();
            let profile = id();
            let (root, old) = legacy_fixture(parent.path(), profile);
            let auth = root.join("codex-home/synthetic-auth-sentinel");
            let rollout = root.join("codex-home/synthetic-rollout-sentinel");
            std::fs::write(&auth, b"nonsecret-owned-auth-fixture").unwrap();
            std::fs::write(&rollout, b"nonsecret-owned-history-fixture").unwrap();
            MIGRATION_STOP.with(|stop| stop.set(Some(point)));
            assert!(
                matches!(
                    ProfileLease::fixture(parent.path(), profile),
                    Err(Error::Io)
                ),
                "{point:?}"
            );
            MIGRATION_STOP.with(|stop| stop.set(None));
            assert_eq!(
                std::fs::read(root.join("lease")).unwrap().len(),
                MIGRATION_LENGTH
            );
            let reopened = ProfileLease::fixture(parent.path(), profile)
                .unwrap_or_else(|error| panic!("migration recovery after {point:?}: {error:?}"));
            assert_eq!(reopened.record, old.managed());
            assert_eq!(
                std::fs::read(root.join("codex-home/config.toml")).unwrap(),
                super::super::managed_config().as_bytes()
            );
            assert_eq!(
                std::fs::read(&auth).unwrap(),
                b"nonsecret-owned-auth-fixture"
            );
            assert_eq!(
                std::fs::read(&rollout).unwrap(),
                b"nonsecret-owned-history-fixture"
            );
            drop(reopened);
            assert_eq!(std::fs::read(root.join("lease")).unwrap(), CLEAN);
            let again = ProfileLease::fixture(parent.path(), profile).unwrap();
            assert_eq!(again.record, old.managed());
            drop(again);
            parent.close().unwrap();
        }
    }
    #[test]
    #[ignore = "owned Windows metadata migration fixture; no account/provider calls"]
    fn unknown_clean_staging_and_unknown_journal_config_are_never_deleted_or_rewritten() {
        let parent = tempfile::tempdir().unwrap();
        let profile = id();
        let (root, old) = legacy_fixture(parent.path(), profile);
        let stage = root.join("codex-home").join(CONFIG_STAGE);
        std::fs::write(&stage, b"unclaimed staging fixture").unwrap();
        assert!(matches!(
            ProfileLease::fixture(parent.path(), profile),
            Err(Error::Configuration)
        ));
        assert_eq!(std::fs::read(&stage).unwrap(), b"unclaimed staging fixture");
        assert_eq!(
            std::fs::read(root.join("registration")).unwrap(),
            old.encode().unwrap()
        );
        std::fs::remove_file(&stage).unwrap();
        MIGRATION_STOP.with(|stop| stop.set(Some(MigrationPoint::Journal)));
        assert!(ProfileLease::fixture(parent.path(), profile).is_err());
        MIGRATION_STOP.with(|stop| stop.set(None));
        let journal = std::fs::read(root.join("lease")).unwrap();
        std::fs::write(root.join("codex-home/config.toml"), b"unknown config bytes").unwrap();
        assert!(ProfileLease::fixture(parent.path(), profile).is_err());
        assert_eq!(
            std::fs::read(root.join("codex-home/config.toml")).unwrap(),
            b"unknown config bytes"
        );
        assert_eq!(std::fs::read(root.join("lease")).unwrap(), journal);
        parent.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows metadata migration fixture; no account/provider calls"]
    fn migration_journal_corruption_and_linked_staging_are_not_cleanup_authority() {
        let parent = tempfile::tempdir().unwrap();
        let profile = id();
        let (root, _) = legacy_fixture(parent.path(), profile);
        MIGRATION_STOP.with(|stop| stop.set(Some(MigrationPoint::Journal)));
        assert!(ProfileLease::fixture(parent.path(), profile).is_err());
        MIGRATION_STOP.with(|stop| stop.set(None));
        let journal = std::fs::read(root.join("lease")).unwrap();
        let mut corrupt = journal.clone();
        corrupt[80] ^= 1;
        std::fs::write(root.join("lease"), &corrupt).unwrap();
        assert!(matches!(
            ProfileLease::fixture(parent.path(), profile),
            Err(Error::RecoveryRequired)
        ));
        assert_eq!(std::fs::read(root.join("lease")).unwrap(), corrupt);
        std::fs::write(root.join("lease"), &journal).unwrap();
        let other = root.join("synthetic-owned-source");
        write_new(&other, &owner_sid().unwrap(), b"nonsecret source").unwrap();
        let stage = root.join("codex-home").join(CONFIG_STAGE);
        std::fs::hard_link(&other, &stage).unwrap();
        assert!(ProfileLease::fixture(parent.path(), profile).is_err());
        assert_eq!(std::fs::read(&other).unwrap(), b"nonsecret source");
        assert!(stage.exists());
        parent.close().unwrap();
    }
    #[test]
    #[ignore = "owned Windows metadata migration fixture; no account/provider calls"]
    fn every_exact_old_new_metadata_pair_recovers_under_its_bound_journal() {
        for config_new in [false, true] {
            for registration_new in [false, true] {
                let parent = tempfile::tempdir().unwrap();
                let profile = id();
                let (root, old) = legacy_fixture(parent.path(), profile);
                let journal = Migration::for_record(&old).unwrap().encode();
                std::fs::write(root.join("lease"), journal).unwrap();
                if config_new {
                    std::fs::write(
                        root.join("codex-home/config.toml"),
                        super::super::managed_config().as_bytes(),
                    )
                    .unwrap();
                }
                if registration_new {
                    std::fs::write(root.join("registration"), old.managed().encode().unwrap())
                        .unwrap();
                }
                let reopened = ProfileLease::fixture(parent.path(), profile).unwrap_or_else(|error| {
                    panic!("migration pair config_new={config_new} registration_new={registration_new}: {error:?}")
                });
                assert_eq!(reopened.record, old.managed());
                drop(reopened);
                assert_eq!(std::fs::read(root.join("lease")).unwrap(), CLEAN);
                parent.close().unwrap();
            }
        }
    }
    #[test]
    #[ignore = "owned Windows retained-handle rights differential; no account/provider calls"]
    fn retained_metadata_rename_requires_mutable_owned_parent_and_preserves_identity() {
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        // Exercise both exact production roles, not only the previously observed
        // codex-home case. Every iteration starts with fresh owned metadata.
        for role in 0u8..2 {
            let parent = tempfile::tempdir().unwrap();
            let mut profile = ProfileLease::fixture(parent.path(), id()).unwrap();
            let owner = owner_sid().unwrap();
            let (container, container_id) = if role == 0 {
                (profile.root.join("codex-home"), profile.record.nodes[1])
            } else {
                (profile.root.clone(), profile.record.nodes[0])
            };
            let matches: Vec<_> = profile
                ._pins
                .iter()
                .enumerate()
                .filter_map(|(index, pin)| {
                    (identity(pin).unwrap() == container_id).then_some(index)
                })
                .collect();
            assert_eq!(matches.len(), 1);
            let pin_index = matches[0];
            let old_guard = hold_directory(&container, &owner).unwrap();
            assert_eq!(identity(&old_guard).unwrap(), container_id);
            drop(std::mem::replace(&mut profile._pins[pin_index], old_guard));
            let delete_denied = || {
                assert_eq!(
                    OpenOptions::new()
                        .access_mode(0x0001_0080)
                        .share_mode(7)
                        .custom_flags(0x0220_0000)
                        .open(&container)
                        .unwrap_err()
                        .raw_os_error(),
                    Some(32)
                )
            };
            delete_denied();
            let source = container.join("guard-source");
            let target = container.join("guard-target");
            let old = b"owned-old-metadata";
            let new = b"owned-new-metadata";
            write_new(&source, &owner, new).unwrap();
            write_new(&target, &owner, old).unwrap();
            let old_target_id = identity(&open_read(&target).unwrap()).unwrap();
            let mut candidate = OpenOptions::new()
                .access_mode(0x8001_0000)
                .share_mode(5)
                .custom_flags(0x0020_0000)
                .open(&source)
                .unwrap();
            verify_file(&candidate, &owner).unwrap();
            let expected = identity(&candidate).unwrap();
            assert_eq!(read_bounded(&mut candidate, new.len()).unwrap(), new);
            let source_wide = wide(source.as_os_str()).unwrap();
            let target_wide = wide(target.as_os_str()).unwrap();
            let error = unsafe {
                MoveFileExW(
                    PCWSTR(source_wide.as_ptr()),
                    PCWSTR(target_wide.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .expect_err("old parent guard must refuse pathname rename");
            assert_eq!(error.code().0 as u32, 0x8007_0020);
            LAST_NATIVE_FAILURE.set(None);
            assert_eq!(rename_metadata(&candidate, &target), Err(Error::Io));
            assert_eq!(LAST_NATIVE_FAILURE.get(), Some((23, 0x8007_0020)));
            let mut unchanged = open_read(&target).unwrap();
            assert_eq!(identity(&unchanged).unwrap(), old_target_id);
            assert_eq!(read_bounded(&mut unchanged, old.len()).unwrap(), old);
            drop(unchanged);
            assert_eq!(identity(&candidate).unwrap(), expected);
            assert_eq!(read_bounded(&mut candidate, new.len()).unwrap(), new);
            let writer_denied = |path: &Path| {
                assert_eq!(
                    OpenOptions::new()
                        .write(true)
                        .share_mode(7)
                        .open(path)
                        .unwrap_err()
                        .raw_os_error(),
                    Some(32)
                )
            };
            writer_denied(&source);
            // The verified replacement is held BEFORE the old pin is released.
            // Only this exact containing directory changes sharing; desired
            // access, all other pins, source object, ACL and lease stay held.
            let replacement = hold_metadata_directory(&container, &owner).unwrap();
            assert_eq!(identity(&replacement).unwrap(), container_id);
            let retained: Vec<_> = profile
                ._pins
                .iter()
                .map(|pin| identity(pin).unwrap())
                .collect();
            let lease_id = identity(&profile.lock.lock().unwrap()).unwrap();
            drop(std::mem::replace(
                &mut profile._pins[pin_index],
                replacement,
            ));
            delete_denied();
            rename_metadata(&candidate, &target)
                .expect("mutable owned parent must allow held-object rename");
            assert!(
                matches!(std::fs::symlink_metadata(&source),Err(error) if error.kind()==std::io::ErrorKind::NotFound)
            );
            assert_eq!(identity(&candidate).unwrap(), expected);
            assert_eq!(read_bounded(&mut candidate, new.len()).unwrap(), new);
            let mut final_target = OpenOptions::new()
                .read(true)
                .share_mode(5)
                .custom_flags(0x0020_0000)
                .open(&target)
                .unwrap();
            verify_file(&final_target, &owner).unwrap();
            assert_eq!(identity(&final_target).unwrap(), expected);
            assert_ne!(identity(&final_target).unwrap(), old_target_id);
            assert_eq!(read_bounded(&mut final_target, new.len()).unwrap(), new);
            writer_denied(&target);
            delete_denied();
            assert_eq!(
                profile
                    ._pins
                    .iter()
                    .map(|pin| identity(pin).unwrap())
                    .collect::<Vec<_>>(),
                retained
            );
            assert_eq!(identity(&profile.lock.lock().unwrap()).unwrap(), lease_id);
            eprintln!("owned-metadata-parent-differential: role={role} baseline=32 candidate=0 identity=1 writer-denied=32 delete-denied=32");
            drop(final_target);
            drop(candidate);
            drop(profile);
            parent.close().unwrap();
        }
    }
}

// Run only by the explicit isolated/serial diagnostic command. This does not
// change the nine strict production recovery/differential expectations above.
#[cfg(all(test, debug_assertions, feature = "native-fixtures"))]
mod directory_pin_diagnostic {
    use super::*;

    fn write_denied(path: &Path) {
        assert_eq!(
            OpenOptions::new()
                .write(true)
                .share_mode(7)
                .open(path)
                .unwrap_err()
                .raw_os_error(),
            Some(32)
        );
    }
    fn delete_denied(path: &Path) {
        assert_eq!(
            OpenOptions::new()
                .access_mode(0x0001_0080)
                .share_mode(7)
                .custom_flags(0x0220_0000)
                .open(path)
                .unwrap_err()
                .raw_os_error(),
            Some(32)
        );
    }
    // Relative target is explicitly bound to the already-held home directory.
    fn rename_relative(file: &File, directory: &File, leaf: &OsStr) -> Result<(), Error> {
        let mut name = wide(leaf)?;
        name.pop();
        let bytes = name
            .len()
            .checked_mul(size_of::<u16>())
            .ok_or(Error::Invalid)?;
        let length = offset_of!(FILE_RENAME_INFO, FileName)
            .checked_add(bytes)
            .ok_or(Error::Invalid)?
            .max(size_of::<FILE_RENAME_INFO>());
        let mut storage = vec![0usize; length.div_ceil(size_of::<usize>())];
        let rename = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*rename).Anonymous.ReplaceIfExists = true;
            (*rename).RootDirectory = HANDLE(directory.as_raw_handle());
            (*rename).FileNameLength = u32::try_from(bytes).map_err(|_| Error::Invalid)?;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                (*rename).FileName.as_mut_ptr(),
                name.len(),
            );
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileRenameInfo,
                storage.as_ptr().cast(),
                u32::try_from(length).map_err(|_| Error::Invalid)?,
            )
        }
        .map_err(|error| win_failure(IoStage::RenameByHandle, error))
    }
    fn observer(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .share_mode(5)
            .custom_flags(0x0020_0000)
            .open(path)
            .unwrap()
    }
    #[test]
    #[ignore = "isolated serial owned directory-pin causal matrix; no account/provider calls"]
    fn isolates_containing_directory_pin_without_changing_production_policy() {
        // 0=A absolute/all pins, 1=B relative leaf/held home root/all pins,
        // 2=C absolute/only containing pin released, 3=D absolute/containing
        // guard READ|WRITE sharing while still denying DELETE. Each is fresh.
        let mut observations = Vec::new();
        for case in 0u8..4 {
            let parent = tempfile::tempdir().unwrap();
            let id = ProfileId::from_native(random_id().unwrap()).unwrap();
            let mut profile = ProfileLease::fixture(parent.path(), id).unwrap();
            let owner = owner_sid().unwrap();
            let home = profile.root.join("codex-home");
            let source = home.join("matrix-source");
            let target = home.join("matrix-target");
            let old = b"owned-old-metadata";
            let new = b"owned-new-metadata";
            write_new(&source, &owner, new).unwrap();
            write_new(&target, &owner, old).unwrap();
            let mut staged = OpenOptions::new()
                .access_mode(0x8001_0000)
                .share_mode(5)
                .custom_flags(0x0020_0000)
                .open(&source)
                .unwrap();
            verify_file(&staged, &owner).unwrap();
            let source_id = identity(&staged).unwrap();
            let target_id = identity(&open_read(&target).unwrap()).unwrap();
            let home_id = profile.record.nodes[1];
            let matching: Vec<_> = profile
                ._pins
                .iter()
                .enumerate()
                .filter_map(|(index, pin)| (identity(pin).unwrap() == home_id).then_some(index))
                .collect();
            assert_eq!(matching.len(), 1);
            let home_index = matching[0];
            // Reconstruct the exact original READ/shareREAD baseline independently
            // of the current production policy, without dropping identity custody.
            let baseline = hold_directory(&home, &owner).unwrap();
            assert_eq!(identity(&baseline).unwrap(), home_id);
            drop(std::mem::replace(&mut profile._pins[home_index], baseline));
            let other_ids: Vec<_> = profile
                ._pins
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != home_index)
                .map(|(_, pin)| identity(pin).unwrap())
                .collect();
            let lease_id = identity(&profile.lock.lock().unwrap()).unwrap();
            delete_denied(&home);
            write_denied(&source);
            let mut delete_probe = 32u32;
            if case == 2 {
                drop(profile._pins.remove(home_index));
                // Positive control: the exact previously pinned directory is
                // may be DELETE-openable. Record refusal too, without aborting
                // the remaining rename controls; no deletion is issued here.
                delete_probe = match OpenOptions::new()
                    .access_mode(0x0001_0080)
                    .share_mode(7)
                    .custom_flags(0x0220_0000)
                    .open(&home)
                {
                    Ok(control) => {
                        assert_eq!(identity(&control).unwrap(), home_id);
                        drop(control);
                        0
                    }
                    Err(error) => error.raw_os_error().map_or(u32::MAX, |code| code as u32),
                };
            } else if case == 3 {
                // Open and verify the new guard BEFORE releasing the old one.
                // At no point can another handle acquire DELETE on this directory.
                let replacement = hold_metadata_directory(&home, &owner).unwrap();
                verify_security(&replacement, &owner, true).unwrap();
                assert_eq!(identity(&replacement).unwrap(), home_id);
                drop(std::mem::replace(
                    &mut profile._pins[home_index],
                    replacement,
                ));
                delete_denied(&home);
            }
            let sentinel_root = parent.path().join("unrelated-owned-directory");
            create_dir(&sentinel_root, &owner).unwrap();
            let sentinel = sentinel_root.join("matrix-target");
            let sentinel_bytes = b"unrelated-owned-sentinel";
            write_new(&sentinel, &owner, sentinel_bytes).unwrap();
            let sentinel_id = identity(&open_read(&sentinel).unwrap()).unwrap();
            LAST_NATIVE_FAILURE.set(None);
            let result = if case == 1 {
                rename_relative(
                    &staged,
                    &profile._pins[home_index],
                    OsStr::new("matrix-target"),
                )
            } else {
                rename_metadata(&staged, &target)
            };
            // Capture before observer opens/cleanup can change OS last-error state.
            let failure = LAST_NATIVE_FAILURE.get();
            let code = match result {
                Ok(()) => {
                    assert!(failure.is_none());
                    0
                }
                Err(Error::Io) => {
                    let (stage, code) = failure.expect("captured native failure");
                    assert_eq!(stage, 23);
                    code
                }
                Err(_) => panic!("diagnostic input must reach native rename"),
            };
            let source_absent = matches!(std::fs::symlink_metadata(&source), Err(error)
                if error.kind() == std::io::ErrorKind::NotFound);
            let mut final_target = observer(&target);
            let final_target_id = identity(&final_target).unwrap();
            let target_exact = if code == 0 {
                final_target_id == source_id
                    && read_bounded(&mut final_target, new.len()).unwrap() == new
            } else {
                final_target_id == target_id
                    && read_bounded(&mut final_target, old.len()).unwrap() == old
            };
            let mut sentinel_file = observer(&sentinel);
            let sentinel_untouched = identity(&sentinel_file).unwrap() == sentinel_id
                && read_variable(&mut sentinel_file, sentinel_bytes.len().max(new.len())).unwrap()
                    == sentinel_bytes;
            let source_exact = identity(&staged).unwrap() == source_id
                && read_bounded(&mut staged, new.len()).unwrap() == new;
            let location_exact = if code == 0 {
                source_absent
            } else {
                !source_absent && identity(&observer(&source)).unwrap() == source_id
            };
            let held_ids: Vec<_> = profile
                ._pins
                .iter()
                .map(|pin| identity(pin).unwrap())
                .collect();
            assert!(other_ids.iter().all(|id| held_ids.contains(id)));
            assert_eq!(identity(&profile.lock.lock().unwrap()).unwrap(), lease_id);
            if case != 2 {
                delete_denied(&home);
            }
            // The unchanged retained source guard must still deny writers at its
            // actual final location. The unrelated owned sentinel stays intact.
            let guarded = if code != 0 {
                &source
            } else if final_target_id == source_id {
                &target
            } else {
                &sentinel
            };
            write_denied(guarded);
            let integrity = target_exact && sentinel_untouched && source_exact && location_exact;
            eprintln!("directory-pin-matrix: case={case} code={code:#010x} delete_probe={delete_probe} target={} sentinel={} source={} location={} integrity={}",
                u8::from(target_exact),u8::from(sentinel_untouched),u8::from(source_exact),u8::from(location_exact),u8::from(integrity));
            observations.push((case, code, integrity));
            drop(sentinel_file);
            drop(final_target);
            drop(staged);
            drop(profile);
            parent.close().unwrap();
        }
        // A is already observed in the published native failure. C/D remain
        // diagnostic outcomes, not speculative success requirements. The old
        // nine strict production expectations still run independently afterward.
        assert_eq!(observations[0].1, 0x8007_0020);
        assert!(observations.iter().all(|(_, _, integrity)| *integrity));
    }
}
