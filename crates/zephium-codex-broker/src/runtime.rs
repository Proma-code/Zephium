//! Fresh credential-free runtime used only by the reviewed native qualifier.
//! A durable application profile constructor is deliberately not exposed yet.
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

pub(crate) struct RuntimePaths {
    // Drop pins before TempDir cleanup. Session retains the whole RuntimePaths.
    _pins: Vec<File>,
    home: PathBuf,
    cwd: PathBuf,
    temp: PathBuf,
    appdata: PathBuf,
    localappdata: PathBuf,
    userprofile: PathBuf,
    _root: TempDir,
}
impl RuntimePaths {
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    pub(crate) fn fresh() -> Result<Self, &'static str> {
        let root = tempfile::Builder::new()
            .prefix("Zephium.CodexFixture.")
            .tempdir()
            .map_err(|_| "runtime-create")?;
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
            ("config.toml", crate::config::CONFIG_TOML),
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
        let mut pins = Vec::new();
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
            _root: root,
        })
    }
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    pub(crate) fn close(self) -> Result<(), &'static str> {
        let Self { _pins, _root, .. } = self;
        drop(_pins);
        _root.close().map_err(|_| "runtime-cleanup")
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
