//! Retained runtime paths for disposable qualification or a private trial profile.
//! The exact lease/config/directory pins outlive every native process and writer.
use crate::profile::native::ProfileLease;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
};
use tempfile::TempDir;

enum Custody {
    Temporary(TempDir),
    Persistent(Box<ProfileLease>),
}

pub(crate) struct RuntimePaths {
    // Drop pins before TempDir cleanup. Session retains the whole RuntimePaths.
    _pins: Vec<File>,
    home: PathBuf,
    cwd: PathBuf,
    temp: PathBuf,
    appdata: PathBuf,
    localappdata: PathBuf,
    userprofile: PathBuf,
    _custody: Custody,
    active: AtomicBool,
}
impl RuntimePaths {
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    pub(crate) fn fresh() -> Result<Self, &'static str> {
        let root = tempfile::Builder::new()
            .prefix("Zephium.CodexFixture.")
            .tempdir()
            .map_err(|_| "runtime-create")?;
        Self::fresh_root(root, crate::config::CONFIG_TOML, Vec::new())
    }
    /// Disposable credential-free compatibility home under one validated local
    /// native KnownFolder, never ambient TEMP/PATH or the retained account home.
    pub(crate) fn fresh_managed() -> Result<Self, &'static str> {
        let parent =
            crate::discovery::known_folder(&windows::Win32::UI::Shell::FOLDERID_LocalAppData)
                .map_err(|_| "probe-known-folder")?;
        crate::image::resolution::local_drive(&parent).map_err(|_| "probe-local-drive")?;
        let mut pins = Vec::new();
        for path in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
            pins.push(crate::image::pin_directory(path)?);
        }
        let root = tempfile::Builder::new()
            .prefix("Zephium.CodexProbe.")
            .tempdir_in(&parent)
            .map_err(|_| "probe-create")?;
        Self::fresh_root(root, &crate::compatibility::managed_config(), pins)
    }
    fn fresh_root(root: TempDir, config: &str, mut pins: Vec<File>) -> Result<Self, &'static str> {
        crate::image::require_absolute_local(root.path())?;
        let canonical_root = canonical_wire_path(root.path())?;
        let home = canonical_root.join("codex-home");
        let cwd = canonical_root.join("work");
        let temp = canonical_root.join("temp");
        let appdata = canonical_root.join("appdata");
        let localappdata = canonical_root.join("localappdata");
        let userprofile = canonical_root.join("userprofile");
        for path in [&home, &cwd, &temp, &appdata, &localappdata, &userprofile] {
            std::fs::create_dir(path).map_err(|_| "runtime-directory")?;
        }
        for (name, bytes) in [
            ("config.toml", config),
            ("environments.toml", crate::config::ENVIRONMENTS_TOML),
        ] {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(home.join(name))
                .map_err(|_| "runtime-config-create")?;
            file.write_all(bytes.as_bytes())
                .map_err(|_| "runtime-config-write")?;
            file.sync_all().map_err(|_| "runtime-config-sync")?;
        }
        for path in [
            canonical_root.as_path(),
            &home,
            &cwd,
            &temp,
            &appdata,
            &localappdata,
            &userprofile,
        ] {
            pins.push(crate::image::pin_directory(path)?);
        }
        for name in ["config.toml", "environments.toml"] {
            pins.push(
                OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .custom_flags(0x0020_0000)
                    .open(home.join(name))
                    .map_err(|_| "runtime-config-pin")?,
            );
        }
        Ok(Self {
            _pins: pins,
            home,
            cwd,
            temp,
            appdata,
            localappdata,
            userprofile,
            _custody: Custody::Temporary(root),
            active: AtomicBool::new(false),
        })
    }
    pub(crate) fn close(self) -> Result<(), &'static str> {
        let Self {
            _pins, _custody, ..
        } = self;
        drop(_pins);
        match _custody {
            Custody::Temporary(root) => root.close().map_err(|_| "runtime-cleanup"),
            Custody::Persistent(_) => Err("runtime-not-disposable"),
        }
    }
    pub(crate) fn persistent(lease: ProfileLease) -> Result<Self, &'static str> {
        let root = canonical_wire_path(&lease.root)?;
        Ok(Self {
            _pins: Vec::new(),
            home: root.join("codex-home"),
            cwd: root.join("work"),
            temp: root.join("temp"),
            appdata: root.join("appdata"),
            localappdata: root.join("localappdata"),
            userprofile: root.join("userprofile"),
            _custody: Custody::Persistent(Box::new(lease)),
            active: AtomicBool::new(false),
        })
    }
    pub(crate) fn reserve_removal(
        &self,
        attempt: crate::profile::removal::RemovalAttemptId,
        grant_epoch: u64,
    ) -> Result<crate::profile::native::removal::RemovalReservation, &'static str> {
        match &self._custody {
            Custody::Persistent(lease) => lease
                .reserve_removal(attempt, grant_epoch)
                .map_err(|_| "removal-reservation"),
            Custody::Temporary(_) => Err("removal-not-persistent"),
        }
    }
    pub(crate) fn into_removal(self) -> Result<ProfileLease, Box<Self>> {
        if self.active.load(Ordering::Acquire) || !matches!(self._custody, Custody::Persistent(_)) {
            return Err(Box::new(self));
        }
        let Self {
            _pins, _custody, ..
        } = self;
        drop(_pins);
        match _custody {
            Custody::Persistent(lease) => Ok(*lease),
            Custody::Temporary(_) => unreachable!("persistent custody checked"),
        }
    }
    pub(crate) fn persistent_identity(&self) -> Option<(crate::profile::ProfileId, [u8; 16])> {
        self.persistent_record()
            .map(|record| (record.profile, record.incarnation))
    }
    pub(crate) fn persistent_record(&self) -> Option<&crate::profile::Record> {
        match &self._custody {
            Custody::Persistent(lease) => Some(&lease.record),
            Custody::Temporary(_) => None,
        }
    }
    pub(crate) fn verify_dormant_clean(&self) -> Result<(), &'static str> {
        if self.active.load(Ordering::Acquire) {
            return Err("runtime-active");
        }
        match &self._custody {
            Custody::Persistent(lease) => lease
                .verify_clean_for_dormant()
                .map_err(|_| "runtime-not-clean"),
            Custody::Temporary(_) => Err("runtime-not-persistent"),
        }
    }
    pub(crate) fn begin_connection(&self) -> Result<(), &'static str> {
        // A failed DIRTY write deliberately leaves active=true: this exact runtime
        // cannot be retried after uncertain I/O, even if no child was created.
        self.active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "runtime-busy")?;
        if let Custody::Persistent(lease) = &self._custody {
            lease.dirty().map_err(|_| "runtime-dirty")?;
        }
        Ok(())
    }
    pub(crate) fn physically_settled(&self) -> Result<(), &'static str> {
        if !self.active.load(Ordering::Acquire) {
            return Err("runtime-not-active");
        }
        if let Custody::Persistent(lease) = &self._custody {
            lease.physically_settled().map_err(|_| "runtime-clean")?;
        }
        self.active.store(false, Ordering::Release);
        Ok(())
    }
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }
    pub(crate) fn temp(&self) -> &Path {
        &self.temp
    }
    pub(crate) fn appdata(&self) -> &Path {
        &self.appdata
    }
    pub(crate) fn localappdata(&self) -> &Path {
        &self.localappdata
    }
    pub(crate) fn userprofile(&self) -> &Path {
        &self.userprofile
    }
}

fn canonical_wire_path(path: &Path) -> Result<PathBuf, &'static str> {
    let canonical = std::fs::canonicalize(path).map_err(|_| "runtime-canonical")?;
    crate::image::require_absolute_local(&canonical)?;
    wire_path(&canonical)
}
fn wire_path(canonical: &Path) -> Result<PathBuf, &'static str> {
    crate::image::require_absolute_local(canonical)?;
    let units: Vec<u16> = canonical.as_os_str().encode_wide().collect();
    let stripped = if units.starts_with(&[92, 92, 63, 92]) {
        &units[4..]
    } else {
        &units
    };
    let output = PathBuf::from(std::ffi::OsString::from_wide(stripped));
    crate::image::require_absolute_local(&output)?;
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_canonical_disk_verbatim_spelling_is_removed() {
        assert_eq!(
            wire_path(Path::new(r"C:\owned\home")).unwrap(),
            Path::new(r"C:\owned\home")
        );
        assert_eq!(
            wire_path(Path::new(r"\\?\C:\owned\home")).unwrap(),
            Path::new(r"C:\owned\home")
        );
        for bad in [
            r"\\server\share",
            r"\\?\UNC\server\share",
            r"C:relative",
            r"C:\owned\..\other",
        ] {
            assert!(wire_path(Path::new(bad)).is_err());
        }
    }
}
