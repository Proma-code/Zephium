//! Existing full CLI detection only. KnownFolder/PATH/package records are hints;
//! no wrapper is run and only fixed official CLI bytes become executable authority.
use crate::{
    image::{self, CliCandidate, ExistingImageFault},
    PinnedImage,
};
use ::windows::{
    core::GUID,
    Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{
            FOLDERID_LocalAppData, FOLDERID_RoamingAppData, SHGetKnownFolderPath,
            KF_FLAG_DONT_VERIFY,
        },
    },
};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

pub(crate) mod plan;
use plan::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    NativeInstaller,
    DefaultNpm,
    PathExecutable,
    PathNpm,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiscoveryFault {
    NotFound,
    Unsupported,
    Unavailable,
    Limit,
    Cancelled,
    Timeout,
}
pub(crate) struct DetectedCli {
    image: Arc<PinnedImage>,
    origin: Origin,
}
impl DetectedCli {
    pub(crate) fn into_image(self) -> Arc<PinnedImage> {
        self.image
    }
    pub(crate) fn origin(&self) -> Origin {
        self.origin
    }
}
struct HeldCandidate {
    identity: (u64, [u8; 16]),
    hashed: Option<image::HashedCli>,
}
struct Search<'a> {
    deadline: Instant,
    cancelled: &'a AtomicBool,
    seen: Vec<HeldCandidate>,
    hashes: usize,
    unsupported: bool,
    lookups: crate::release_admission::LookupPlan,
    artifacts: Vec<crate::release_admission::OriginalCliArtifact>,
    unavailable: bool,
}
impl Search<'_> {
    fn check(&self) -> Result<(), DiscoveryFault> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(DiscoveryFault::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(DiscoveryFault::Timeout)
        } else {
            Ok(())
        }
    }
    fn record(&mut self, fault: ExistingImageFault) -> Result<(), DiscoveryFault> {
        match fault {
            ExistingImageFault::Cancelled => return Err(DiscoveryFault::Cancelled),
            ExistingImageFault::Timeout => return Err(DiscoveryFault::Timeout),
            ExistingImageFault::Unsupported => self.unsupported = true,
            ExistingImageFault::Resolve(image::resolution::ResolveFault::Missing) => {}
            ExistingImageFault::Resolve(_) => self.unavailable = true,
        }
        Ok(())
    }
    fn candidate(
        &mut self,
        path: &Path,
        origin: Origin,
        version_hint: Option<&str>,
    ) -> Result<Option<DetectedCli>, DiscoveryFault> {
        self.check()?;
        let candidate = match CliCandidate::open(path) {
            Ok(value) => value,
            Err(fault) => {
                self.record(fault)?;
                self.check()?;
                return Ok(None);
            }
        };
        let identity = candidate.identity();
        // Keep the original physically pinned, hashed object. A later npm
        // locator for this same object must not be suppressed by path dedup.
        let (slot, hashed) =
            if let Some(slot) = self.seen.iter().position(|held| held.identity == identity) {
                let hashed = self.seen[slot]
                    .hashed
                    .take()
                    .ok_or(DiscoveryFault::Unavailable)?;
                drop(candidate);
                (slot, hashed)
            } else {
                if self.hashes >= MAX_FULL_HASHES {
                    return Err(DiscoveryFault::Limit);
                }
                self.hashes += 1;
                let hashed = match candidate.hash(self.deadline, self.cancelled) {
                    Ok(hashed) => hashed,
                    Err(fault) => {
                        self.record(fault)?;
                        return Ok(None);
                    }
                };
                let slot = self.seen.len();
                self.seen.push(HeldCandidate {
                    identity,
                    hashed: None,
                });
                (slot, hashed)
            };
        let admitted = if hashed.reference().is_some() {
            hashed.admit_reference()
        } else {
            let mut matched = self
                .artifacts
                .iter()
                .position(|a| hashed.matches_artifact(a));
            for query in self.lookups.candidates(version_hint) {
                if matched.is_some() {
                    break;
                }
                if !self.lookups.reserve(&query) {
                    continue;
                }
                self.check()?;
                let result = if query.is_index() {
                    crate::release_fetch::fetch_index(query, self.deadline, self.cancelled)
                } else {
                    crate::release_fetch::fetch(query, self.deadline, self.cancelled)
                        .map(|a| vec![a])
                };
                match result {
                    Ok(artifacts) => self.artifacts.extend(artifacts),
                    Err(crate::release_fetch::Fault::Cancelled) => {
                        return Err(DiscoveryFault::Cancelled)
                    }
                    Err(crate::release_fetch::Fault::Timeout) => {
                        return Err(DiscoveryFault::Timeout)
                    }
                    Err(_) => {
                        self.unavailable = true;
                        continue;
                    }
                }
                matched = self
                    .artifacts
                    .iter()
                    .position(|a| hashed.matches_artifact(a));
            }
            let Some(matched) = matched else {
                self.unsupported = true;
                self.seen[slot].hashed = Some(hashed);
                return Ok(None);
            };
            hashed.admit_official(self.artifacts.remove(matched))
        };
        match admitted {
            Ok(image) => {
                self.check()?;
                Ok(Some(DetectedCli {
                    image: Arc::new(image),
                    origin,
                }))
            }
            Err(fault) => {
                self.record(fault)?;
                Ok(None)
            }
        }
    }
    fn metadata(&mut self, path: &Path) -> Result<Option<Vec<u8>>, DiscoveryFault> {
        self.check()?;
        let held = match image::resolution::local_file(path) {
            Ok(value) => value,
            Err(image::resolution::ResolveFault::Missing) => return Ok(None),
            Err(_) => {
                self.unavailable = true;
                return Ok(None);
            }
        };
        let bytes = match bounded_metadata(&held.file) {
            Ok(value) => value,
            Err(()) => {
                self.unavailable = true;
                return Ok(None);
            }
        };
        self.check()?;
        Ok(Some(bytes))
    }
    fn npm(
        &mut self,
        prefix: &Path,
        origin: Origin,
    ) -> Result<Option<DetectedCli>, DiscoveryFault> {
        let root = append(prefix, NPM_ROOT);
        let Some(bytes) = self.metadata(&root.join("package.json"))? else {
            return Ok(None);
        };
        let version = match root_package(&bytes) {
            Ok(version) => version,
            Err(_) => {
                self.unsupported = true;
                return Ok(None);
            }
        };
        for suffix in [PLATFORM_NESTED, PLATFORM_HOISTED] {
            let platform = append(prefix, suffix);
            let Some(bytes) = self.metadata(&platform.join("package.json"))? else {
                continue;
            };
            if platform_package(&bytes).ok().as_ref() != Some(&version) {
                self.unsupported = true;
                continue;
            }
            if let Some(found) =
                self.candidate(&append(&platform, VENDOR_IMAGE), origin, Some(&version))?
            {
                return Ok(Some(found));
            }
        }
        // Fixed fallback supported by the pinned official JS launcher. The JS
        // itself is neither interpreted nor run, even if its bytes were changed.
        self.candidate(&append(&root, VENDOR_IMAGE), origin, Some(&version))
    }
}
fn append(root: &Path, components: &[&str]) -> PathBuf {
    let mut path = root.to_owned();
    for component in components {
        path.push(component);
    }
    path
}
fn bounded_metadata(file: &File) -> Result<Vec<u8>, ()> {
    if file.metadata().map_err(|_| ())?.len() > MAX_METADATA as u64 {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take((MAX_METADATA + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > MAX_METADATA {
        return Err(());
    }
    Ok(bytes)
}
pub(crate) fn known_folder(id: &GUID) -> Result<PathBuf, DiscoveryFault> {
    let value = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None) }
        .map_err(|_| DiscoveryFault::Unavailable)?;
    if value.is_null() {
        return Err(DiscoveryFault::Unavailable);
    }
    struct Owned(::windows::core::PWSTR);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { CoTaskMemFree(Some(self.0 .0.cast())) };
        }
    }
    let value = Owned(value);
    let mut length = 0;
    while length < 32768 && unsafe { *value.0 .0.add(length) } != 0 {
        length += 1;
    }
    if length == 0 || length == 32768 {
        return Err(DiscoveryFault::Unavailable);
    }
    use std::os::windows::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
        std::slice::from_raw_parts(value.0 .0, length)
    }));
    image::require_absolute_local(&path).map_err(|_| DiscoveryFault::Unavailable)?;
    Ok(path)
}

/// Physical filesystem APIs may finish after this logical budget. Cancellation
/// fences later admission, not an unsupported promise to kill an OS file query.
pub(crate) fn discover(
    deadline: Instant,
    cached_release: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<DetectedCli, DiscoveryFault> {
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    crate::fixture_activity::DISCOVERY.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let mut search = Search {
        deadline,
        cancelled,
        seen: Vec::new(),
        hashes: 0,
        unsupported: false,
        lookups: crate::release_admission::LookupPlan::new(cached_release),
        artifacts: Vec::new(),
        unavailable: false,
    };
    search.check()?;
    match known_folder(&FOLDERID_LocalAppData) {
        Ok(local) => {
            if let Some(found) = search.candidate(
                &append(&local, &["Programs", "OpenAI", "Codex", "bin", "codex.exe"]),
                Origin::NativeInstaller,
                None,
            )? {
                return Ok(found);
            }
        }
        Err(_) => search.unavailable = true,
    }
    match known_folder(&FOLDERID_RoamingAppData) {
        Ok(roaming) => {
            if let Some(found) = search.npm(&roaming.join("npm"), Origin::DefaultNpm)? {
                return Ok(found);
            }
        }
        Err(_) => search.unavailable = true,
    }
    if let Some(path) = std::env::var_os("PATH") {
        let path = path
            .into_string()
            .map_err(|_| DiscoveryFault::Unavailable)?;
        for prefix in path_hints(&path).map_err(|_| DiscoveryFault::Limit)? {
            let prefix = Path::new(prefix);
            if let Some(found) =
                search.candidate(&prefix.join("codex.exe"), Origin::PathExecutable, None)?
            {
                return Ok(found);
            }
            if let Some(found) = search.npm(prefix, Origin::PathNpm)? {
                return Ok(found);
            }
        }
    }
    search.check()?;
    if search.unsupported {
        Err(DiscoveryFault::Unsupported)
    } else if search.unavailable {
        Err(DiscoveryFault::Unavailable)
    } else {
        Err(DiscoveryFault::NotFound)
    }
}
