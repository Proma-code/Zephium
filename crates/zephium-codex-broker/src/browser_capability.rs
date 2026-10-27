//! Browser authority joins the SAME held full CLI to a fixed experimental export,
//! successful physical custody and checked disposable cleanup. Shape alone is
//! structural data, never execution authority or a version-string permission.
use crate::{
    browser_schema::{self, BrowserSchemas, BrowserShape},
    windows::Session,
    PinnedImage, RuntimePaths,
};
use ::windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};
const MAX_ENTRIES: usize = 1024;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ONE: u64 = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 8;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    Cancelled,
    Expired,
    Setup,
    Custody,
    Output,
}
pub(crate) struct BrowserCapability {
    image: CapabilityImage,
    contract: BrowserShape,
}
enum CapabilityImage {
    Original(Arc<PinnedImage>),
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    Fixture(crate::windows::FixtureBrowserReceipt),
}
impl BrowserCapability {
    /// Synthetic actor admission only. Neither this mode-bound receipt nor the
    /// compiled fixture shape represents an original export-custody success.
    #[cfg(all(debug_assertions, feature = "native-fixtures"))]
    pub(crate) fn fixture(session: &Session) -> Result<Self, Fault> {
        let receipt = session.fixture_browser_receipt().ok_or(Fault::Setup)?;
        let value: serde_json::Value =
            serde_json::from_str(include_str!("browser_schema/fixtures/browser-v1.json"))
                .map_err(|_| Fault::Output)?;
        let data = [
            "server_requests",
            "tool_response",
            "rpc_response",
            "client_requests",
            "server_notifications",
        ]
        .map(|key| serde_json::to_vec(&value[key]).map_err(|_| Fault::Output));
        let [a, b, c, d, e] = data;
        let [a, b, c, d, e] = [a?, b?, c?, d?, e?];
        let contract = browser_schema::project(BrowserSchemas {
            server_requests: &a,
            tool_response: &b,
            rpc_response: &c,
            client_requests: &d,
            server_notifications: &e,
        })
        .map_err(|_| Fault::Output)?;
        Ok(Self {
            image: CapabilityImage::Fixture(receipt),
            contract,
        })
    }
    pub(crate) fn qualifies(&self, session: &Session) -> bool {
        (match &self.image {
            CapabilityImage::Original(image) => session.owns_original_image(image),
            #[cfg(all(debug_assertions, feature = "native-fixtures"))]
            CapabilityImage::Fixture(receipt) => session.owns_fixture_browser(receipt),
        }) && self.contract.digest() != [0; 32]
    }
    pub(crate) fn probe(
        image: Arc<PinnedImage>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Option<Self>, Fault> {
        current(deadline, cancel)?;
        if image.kind() != crate::pin::ImageKind::Cli {
            return Err(Fault::Setup);
        }
        let runtime = Arc::new(RuntimePaths::fresh_managed().map_err(|_| Fault::Setup)?);
        let result = export(&image, &runtime, deadline, cancel);
        // No capability on a cleanup failure, even after a valid structural
        // shape. Failed export still physically drops its Session before this.
        let cleanup = Arc::try_unwrap(runtime)
            .map_err(|_| Fault::Custody)
            .and_then(|runtime| runtime.close().map_err(|_| Fault::Custody));
        cleanup?;
        let shape = result?;
        current(deadline, cancel)?;
        Ok(shape.map(|contract| Self {
            image: CapabilityImage::Original(image),
            contract,
        }))
    }
}
fn current(deadline: Instant, cancel: &AtomicBool) -> Result<(), Fault> {
    if cancel.load(Ordering::Acquire) {
        Err(Fault::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Fault::Expired)
    } else {
        Ok(())
    }
}
fn export(
    image: &Arc<PinnedImage>,
    runtime: &Arc<RuntimePaths>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Option<BrowserShape>, Fault> {
    current(deadline, cancel)?;
    let output = runtime.cwd().join("schema-output");
    std::fs::create_dir(&output).map_err(|_| Fault::Output)?;
    let root = crate::image::pin_directory(&output).map_err(|_| Fault::Output)?;
    current(deadline, cancel)?;
    let session =
        Session::spawn_schema_export(image.clone(), runtime.clone()).map_err(|_| Fault::Custody)?;
    let exit = session
        .finish_cancellable(deadline, cancel)
        .map_err(|fault| match fault {
            crate::windows::Fault::Cancelled => Fault::Cancelled,
            crate::windows::Fault::Timeout => Fault::Expired,
            _ => Fault::Custody,
        })?;
    if exit.code != 0 || exit.stdout_bytes != 0 {
        return Err(Fault::Output);
    }
    current(deadline, cancel)?;
    let mut tree = OutputTree {
        root: output.clone(),
        pins: vec![root],
        selected: std::array::from_fn(|_| None),
        count: 0,
        bytes: 0,
        deadline,
        cancel,
    };
    tree.collect(&output, 0)?;
    let shape = tree.shape()?;
    current(deadline, cancel)?;
    // All file/directory pins remain live through complete validation. Their
    // drop precedes checked removal of this uniquely owned disposable root.
    drop(tree);
    Ok(shape)
}
struct OutputTree<'a> {
    root: PathBuf,
    pins: Vec<File>,
    selected: [Option<Vec<u8>>; 5],
    count: usize,
    bytes: u64,
    deadline: Instant,
    cancel: &'a AtomicBool,
}
impl OutputTree<'_> {
    fn shape(&self) -> Result<Option<BrowserShape>, Fault> {
        let [server_requests, tool_response, rpc_response, client_requests, server_notifications] =
            self.selected.each_ref();
        let shape = browser_schema::project(BrowserSchemas {
            server_requests: server_requests.as_deref().ok_or(Fault::Output)?,
            tool_response: tool_response.as_deref().ok_or(Fault::Output)?,
            rpc_response: rpc_response.as_deref().ok_or(Fault::Output)?,
            client_requests: client_requests.as_deref().ok_or(Fault::Output)?,
            server_notifications: server_notifications.as_deref().ok_or(Fault::Output)?,
        })
        .ok();
        Ok(shape)
    }

    fn collect(&mut self, directory: &Path, depth: usize) -> Result<(), Fault> {
        current(self.deadline, self.cancel)?;
        if depth > MAX_DEPTH {
            return Err(Fault::Output);
        }
        for entry in std::fs::read_dir(directory).map_err(|_| Fault::Output)? {
            current(self.deadline, self.cancel)?;
            self.count = self.count.checked_add(1).ok_or(Fault::Output)?;
            if self.count > MAX_ENTRIES {
                return Err(Fault::Output);
            }
            let entry = entry.map_err(|_| Fault::Output)?;
            let name = entry.file_name();
            if Path::new(&name).components().count() != 1
                || !matches!(
                    Path::new(&name).components().next(),
                    Some(Component::Normal(_))
                )
            {
                return Err(Fault::Output);
            }
            let path = entry.path();
            let mut held = OpenOptions::new()
                .read(true)
                .share_mode(1)
                .custom_flags(0x0020_0000 | 0x0200_0000)
                .open(&path)
                .map_err(|_| Fault::Output)?;
            let meta = held.metadata().map_err(|_| Fault::Output)?;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(Fault::Output);
            }
            if meta.is_dir() {
                self.pins.push(held);
                self.collect(&path, depth + 1)?;
                continue;
            }
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            unsafe { GetFileInformationByHandle(HANDLE(held.as_raw_handle()), &mut info) }
                .map_err(|_| Fault::Output)?;
            if !meta.is_file()
                || info.nNumberOfLinks != 1
                || meta.len() == 0
                || meta.len() > MAX_ONE
            {
                return Err(Fault::Output);
            }
            self.bytes = self.bytes.checked_add(meta.len()).ok_or(Fault::Output)?;
            if self.bytes > MAX_BYTES {
                return Err(Fault::Output);
            }
            if directory == self.root {
                if let Some(index) = browser_schema::FILE_NAMES
                    .iter()
                    .position(|expected| name == *expected)
                {
                    if self.selected[index].is_some()
                        || meta.len() > browser_schema::MAX_FILE_BYTES as u64
                    {
                        return Err(Fault::Output);
                    }
                    let mut bytes = Vec::with_capacity(meta.len() as usize);
                    (&mut held)
                        .take(browser_schema::MAX_FILE_BYTES as u64 + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|_| Fault::Output)?;
                    if bytes.len() as u64 != meta.len() {
                        return Err(Fault::Output);
                    }
                    self.selected[index] = Some(bytes);
                }
            }
            self.pins.push(held);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
