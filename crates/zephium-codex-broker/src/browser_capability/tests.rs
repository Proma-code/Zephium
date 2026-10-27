//! These are Windows filesystem tests, not portable custody simulations. Every
//! fixture is disposable and contains public schemas or synthetic bytes only.
//! None launches an executable, creates a BrowserCapability or proves a process
//! has exited/reaped; that separate boundary belongs to native actor fixtures.
use super::*;
use ::windows::Win32::System::IO::DeviceIoControl;
use std::{os::windows::ffi::OsStrExt, time::Duration};

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("Zephium.BrowserSchemaOutput.")
        .tempdir()
        .unwrap()
}

fn tree<'a>(root: &Path, cancel: &'a AtomicBool) -> OutputTree<'a> {
    OutputTree {
        root: root.to_owned(),
        pins: vec![crate::image::pin_directory(root).unwrap()],
        selected: std::array::from_fn(|_| None),
        count: 0,
        bytes: 0,
        deadline: Instant::now() + Duration::from_secs(30),
        cancel,
    }
}

fn schemas(root: &Path) {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../browser_schema/fixtures/browser-v1.json")).unwrap();
    for (name, key) in browser_schema::FILE_NAMES.into_iter().zip([
        "server_requests",
        "tool_response",
        "rpc_response",
        "client_requests",
        "server_notifications",
    ]) {
        std::fs::write(root.join(name), serde_json::to_vec(&fixture[key]).unwrap()).unwrap();
    }
}

fn sized(root: &Path, name: &str, length: u64) {
    File::create(root.join(name))
        .unwrap()
        .set_len(length)
        .unwrap();
}

#[test]
fn cancellation_and_deadline_refuse_before_output_inspection() {
    let cancel = AtomicBool::new(false);
    assert_eq!(current(Instant::now(), &cancel), Err(Fault::Expired));
    cancel.store(true, Ordering::Release);
    assert_eq!(
        current(Instant::now() + Duration::from_secs(30), &cancel),
        Err(Fault::Cancelled)
    );
    assert_eq!(current(Instant::now(), &cancel), Err(Fault::Cancelled));
}

#[test]
#[ignore = "owned Windows output files and held handles; no executable launch"]
fn native_complete_output_returns_only_shape_and_pins_selected_files() {
    let root = root();
    schemas(root.path());
    let cancel = AtomicBool::new(false);
    let mut tree = tree(root.path(), &cancel);
    tree.collect(root.path(), 0).unwrap();
    let shape = tree.shape().unwrap().unwrap();
    assert_ne!(shape.digest(), [0; 32]);
    assert_eq!(tree.count, 5);
    assert_eq!(tree.pins.len(), 6);
    let selected = root.path().join(browser_schema::FILE_NAMES[0]);
    assert!(OpenOptions::new().write(true).open(&selected).is_err());
    assert!(std::fs::remove_file(&selected).is_err());
    drop(tree);
    std::fs::write(&selected, b"after-owned-pin-drop").unwrap();
    root.close().unwrap();
}

#[test]
#[ignore = "owned Windows held output-root direct mkdir/write qualification; no executable launch"]
fn native_held_output_root_allows_direct_generation_without_allowing_root_replacement() {
    let root = root();
    let output = root.path().join("schema-output");
    std::fs::create_dir(&output).unwrap();
    let held_root = crate::image::pin_directory(&output).unwrap();
    let identity = |file: &File| {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: file owns a live directory handle and info is writable for
        // the entire synchronous query. This never closes or transfers it.
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.unwrap();
        (
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        )
    };
    let original = identity(&held_root);
    // Test the empty directory so deletion cannot fail merely because it has
    // children. Both failures must be the Windows sharing violation.
    assert_eq!(
        std::fs::remove_dir(&output).unwrap_err().raw_os_error(),
        Some(32)
    );
    assert_eq!(
        std::fs::rename(&output, root.path().join("replaced"))
            .unwrap_err()
            .raw_os_error(),
        Some(32)
    );
    // Match the fixed upstream generator's direct create_dir_all/fs::write
    // behavior, not an unrelated atomic rename/replacement strategy.
    let nested = output.join("v2").join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("extra.json"), b"{\"public\":true}").unwrap();
    std::fs::write(
        output.join(browser_schema::FILE_NAMES[0]),
        b"to-be-truncated",
    )
    .unwrap();
    schemas(&output);
    let current_root = crate::image::pin_directory(&output).unwrap();
    assert_eq!(identity(&held_root), original);
    assert_eq!(identity(&current_root), original);
    let cancel = AtomicBool::new(false);
    let mut tree = tree(&output, &cancel);
    tree.collect(&output, 0).unwrap();
    assert!(tree.shape().unwrap().is_some());
    drop(tree);
    drop(current_root);
    drop(held_root);
    root.close().unwrap();
}

#[test]
#[ignore = "owned Windows missing-output qualification; no executable launch"]
fn native_each_missing_selected_file_is_output_failure() {
    for missing in browser_schema::FILE_NAMES {
        let root = root();
        schemas(root.path());
        std::fs::remove_file(root.path().join(missing)).unwrap();
        let cancel = AtomicBool::new(false);
        let mut tree = tree(root.path(), &cancel);
        tree.collect(root.path(), 0).unwrap();
        assert_eq!(tree.shape(), Err(Fault::Output), "{missing}");
        drop(tree);
        root.close().unwrap();
    }
}

#[test]
#[ignore = "owned Windows output-shape qualification; no executable launch"]
fn native_directory_or_nested_file_never_substitutes_for_selected_root_file() {
    for nested in [false, true] {
        let root = root();
        schemas(root.path());
        let selected = root.path().join(browser_schema::FILE_NAMES[0]);
        let bytes = std::fs::read(&selected).unwrap();
        std::fs::remove_file(&selected).unwrap();
        if nested {
            let other = root.path().join("nested");
            std::fs::create_dir(&other).unwrap();
            std::fs::write(other.join(browser_schema::FILE_NAMES[0]), &bytes).unwrap();
        } else {
            std::fs::create_dir(&selected).unwrap();
            std::fs::write(selected.join("schema.json"), &bytes).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let mut tree = tree(root.path(), &cancel);
        tree.collect(root.path(), 0).unwrap();
        assert_eq!(tree.shape(), Err(Fault::Output));
        drop(tree);
        root.close().unwrap();
    }
}

#[test]
#[ignore = "owned Windows exact-name qualification; no executable launch"]
fn native_case_alias_is_not_selected_and_duplicate_collection_is_refused() {
    let root = root();
    schemas(root.path());
    let exact = root.path().join(browser_schema::FILE_NAMES[0]);
    let alias = root
        .path()
        .join(browser_schema::FILE_NAMES[0].to_ascii_lowercase());
    // Use an intermediate name so this is a real directory-entry case change
    // even on a case-insensitive Windows filesystem.
    let intermediate = root.path().join("renaming.json");
    std::fs::rename(&exact, &intermediate).unwrap();
    std::fs::rename(&intermediate, &alias).unwrap();
    let cancel = AtomicBool::new(false);
    let mut first = tree(root.path(), &cancel);
    first.collect(root.path(), 0).unwrap();
    assert_eq!(first.shape(), Err(Fault::Output));
    drop(first);
    std::fs::rename(&alias, &intermediate).unwrap();
    std::fs::rename(&intermediate, &exact).unwrap();
    let mut second = tree(root.path(), &cancel);
    second.collect(root.path(), 0).unwrap();
    assert!(second.shape().unwrap().is_some());
    // This is the collector's duplicate-selection defense, not a claim that
    // ordinary case-insensitive Windows can contain duplicate exact names.
    assert_eq!(second.collect(root.path(), 0), Err(Fault::Output));
    drop(second);
    root.close().unwrap();
}

#[test]
#[ignore = "owned Windows malformed schema qualification; no executable launch"]
fn native_malformed_complete_export_is_unsupported_not_an_output_capability() {
    let root = root();
    schemas(root.path());
    std::fs::write(root.path().join(browser_schema::FILE_NAMES[0]), b"{").unwrap();
    let cancel = AtomicBool::new(false);
    let mut tree = tree(root.path(), &cancel);
    tree.collect(root.path(), 0).unwrap();
    assert_eq!(tree.shape(), Ok(None));
    drop(tree);
    root.close().unwrap();
}

#[test]
#[ignore = "owned Windows output byte bounds; no executable launch"]
fn native_zero_selected_overbound_and_unselected_overbound_files_are_refused() {
    for (name, length) in [
        ("unselected.json", 0),
        (
            browser_schema::FILE_NAMES[0],
            browser_schema::MAX_FILE_BYTES as u64 + 1,
        ),
        ("unselected.json", MAX_ONE + 1),
    ] {
        let root = root();
        sized(root.path(), name, length);
        let cancel = AtomicBool::new(false);
        let mut tree = tree(root.path(), &cancel);
        assert_eq!(tree.collect(root.path(), 0), Err(Fault::Output));
        drop(tree);
        root.close().unwrap();
    }
}

#[test]
#[ignore = "owned Windows output aggregate-byte bound; no executable launch"]
fn native_total_output_budget_counts_unselected_files() {
    let root = root();
    for index in 0..MAX_BYTES / MAX_ONE {
        sized(root.path(), &format!("unselected-{index}.json"), MAX_ONE);
    }
    sized(root.path(), "one-more-byte.json", 1);
    let cancel = AtomicBool::new(false);
    let mut tree = tree(root.path(), &cancel);
    assert_eq!(tree.collect(root.path(), 0), Err(Fault::Output));
    assert_eq!(tree.bytes, MAX_BYTES + 1);
    drop(tree);
    root.close().unwrap();
}

#[test]
#[ignore = "owned Windows output entry bound; no executable launch"]
fn native_output_entry_count_has_exact_boundary() {
    let root = root();
    for index in 0..MAX_ENTRIES {
        sized(root.path(), &format!("unselected-{index}.json"), 1);
    }
    let cancel = AtomicBool::new(false);
    let mut at_limit = tree(root.path(), &cancel);
    at_limit.collect(root.path(), 0).unwrap();
    assert_eq!(at_limit.count, MAX_ENTRIES);
    drop(at_limit);
    sized(root.path(), "over-limit.json", 1);
    let mut over_limit = tree(root.path(), &cancel);
    assert_eq!(over_limit.collect(root.path(), 0), Err(Fault::Output));
    assert_eq!(over_limit.count, MAX_ENTRIES + 1);
    drop(over_limit);
    root.close().unwrap();
}

#[test]
#[ignore = "owned Windows output directory-depth bound; no executable launch"]
fn native_output_depth_has_exact_boundary() {
    let root = root();
    let mut directory = root.path().to_owned();
    for _ in 0..MAX_DEPTH {
        directory = directory.join("nested");
        std::fs::create_dir(&directory).unwrap();
    }
    let cancel = AtomicBool::new(false);
    let mut at_limit = tree(root.path(), &cancel);
    at_limit.collect(root.path(), 0).unwrap();
    drop(at_limit);
    std::fs::create_dir(directory.join("over-limit")).unwrap();
    let mut over_limit = tree(root.path(), &cancel);
    assert_eq!(over_limit.collect(root.path(), 0), Err(Fault::Output));
    drop(over_limit);
    root.close().unwrap();
}

#[test]
#[ignore = "owned Windows output hard-link qualification; no executable launch"]
fn native_multilink_file_is_refused_even_when_only_one_name_is_in_output() {
    let root = root();
    let output = root.path().join("output");
    std::fs::create_dir(&output).unwrap();
    let outside = root.path().join("synthetic-outside-output.json");
    std::fs::write(&outside, b"public-synthetic-bytes").unwrap();
    std::fs::hard_link(&outside, output.join(browser_schema::FILE_NAMES[0])).unwrap();
    let cancel = AtomicBool::new(false);
    let mut tree = tree(&output, &cancel);
    assert_eq!(tree.collect(&output, 0), Err(Fault::Output));
    assert_eq!(tree.bytes, 0);
    assert!(tree.selected.iter().all(Option::is_none));
    drop(tree);
    assert_eq!(std::fs::read(&outside).unwrap(), b"public-synthetic-bytes");
    root.close().unwrap();
}

fn junction(path: &Path, target: &Path) {
    let target = std::fs::canonicalize(target).unwrap();
    let units: Vec<u16> = target.as_os_str().encode_wide().collect();
    let units = units.strip_prefix(&[92, 92, 63, 92]).unwrap_or(&units);
    let print = String::from_utf16(units).unwrap();
    let substitute: Vec<u8> = format!(r"\??\{print}")
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let print: Vec<u8> = print.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut bytes = Vec::new();
    bytes.extend(0xa000_0003u32.to_le_bytes());
    bytes.extend(((8 + substitute.len() + 2 + print.len() + 2) as u16).to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend((substitute.len() as u16).to_le_bytes());
    bytes.extend(((substitute.len() + 2) as u16).to_le_bytes());
    bytes.extend((print.len() as u16).to_le_bytes());
    bytes.extend(substitute);
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(print);
    bytes.extend(0u16.to_le_bytes());
    let folder = OpenOptions::new()
        .write(true)
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path)
        .unwrap();
    let mut returned = 0;
    // SAFETY: folder is a live handle to this test's newly created empty
    // directory; bytes is the complete bounded mount-point buffer and remains
    // live for this synchronous call. The target is another owned fixture path.
    unsafe {
        DeviceIoControl(
            HANDLE(folder.as_raw_handle()),
            0x0009_00a4,
            Some(bytes.as_ptr().cast()),
            bytes.len() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
    }
    .unwrap();
}

#[test]
#[ignore = "owned Windows junction-output qualification; no executable launch"]
fn native_reparse_directory_is_refused_before_target_collection() {
    let root = root();
    let output = root.path().join("output");
    let outside = root.path().join("synthetic-outside-output");
    let alias = output.join("alias");
    std::fs::create_dir(&output).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::create_dir(&alias).unwrap();
    schemas(&outside);
    junction(&alias, &outside);
    let cancel = AtomicBool::new(false);
    let mut tree = tree(&output, &cancel);
    assert_eq!(tree.collect(&output, 0), Err(Fault::Output));
    assert_eq!(tree.count, 1);
    assert_eq!(tree.bytes, 0);
    assert_eq!(tree.pins.len(), 1);
    assert!(tree.selected.iter().all(Option::is_none));
    drop(tree);
    std::fs::remove_dir(&alias).unwrap();
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 5);
    root.close().unwrap();
}

#[test]
#[ignore = "owned Windows cancellation during held output inspection; no executable launch"]
fn native_cancelled_or_expired_tree_never_collects_entries() {
    let root = root();
    schemas(root.path());
    let cancel = AtomicBool::new(false);
    let mut tree = tree(root.path(), &cancel);
    cancel.store(true, Ordering::Release);
    assert_eq!(tree.collect(root.path(), 0), Err(Fault::Cancelled));
    assert_eq!(tree.count, 0);
    cancel.store(false, Ordering::Release);
    tree.deadline = Instant::now();
    assert_eq!(tree.collect(root.path(), 0), Err(Fault::Expired));
    assert_eq!(tree.count, 0);
    assert!(tree.selected.iter().all(Option::is_none));
    drop(tree);
    root.close().unwrap();
}

#[cfg(all(debug_assertions, feature = "native-fixtures"))]
#[test]
#[ignore = "owned Windows disposable-runtime cleanup failure; no executable launch"]
fn native_disposable_cleanup_failure_is_observable_and_owned_remainder_is_removable() {
    let runtime = RuntimePaths::fresh().unwrap();
    let root = runtime.cwd().parent().unwrap().to_owned();
    let blocker = runtime.cwd().join("synthetic-cleanup-blocker");
    std::fs::write(&blocker, b"public-fixture").unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&blocker)
        .unwrap();
    assert_eq!(runtime.close(), Err("runtime-cleanup"));
    assert!(root.exists());
    assert_eq!(held.metadata().unwrap().len(), 14);
    drop(held);
    // The consumed TempDir reported failure; remove only the captured fresh
    // fixture root after releasing its intentionally held blocker.
    std::fs::remove_dir_all(&root).unwrap();
    assert!(!root.exists());
}
