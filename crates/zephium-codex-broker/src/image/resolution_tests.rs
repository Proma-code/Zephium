use super::*;
use std::io::Write;

fn mount_bytes(target: &str) -> Vec<u8> {
    let substitute: Vec<u8> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let print: Vec<u8> = target
        .strip_prefix(r"\??\")
        .unwrap_or(target)
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut bytes = Vec::new();
    bytes.extend(MOUNT_POINT.to_le_bytes());
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
    bytes
}
#[test]
fn reparse_target_requires_bounded_absolute_local_native_substitute() {
    assert_eq!(
        parse_target(&mount_bytes(r"\??\C:\Owned\release")).unwrap(),
        Path::new(r"C:\Owned\release")
    );
    for target in [
        r"\??\UNC\host\share",
        r"\??\C:relative",
        r"relative",
        r"\Device\HarddiskVolume1\path",
        r"\??\Volume{123}\path",
        r"\??\C:\owned\..\other",
    ] {
        assert_eq!(parse_target(&mount_bytes(target)), Err(ResolveFault::Shape));
    }
    let mut unknown = mount_bytes(r"\??\C:\owned");
    unknown[..4].copy_from_slice(&123u32.to_le_bytes());
    assert_eq!(parse_target(&unknown), Err(ResolveFault::Shape));
    let mut odd = mount_bytes(r"\??\C:\owned");
    odd[8..10].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(parse_target(&odd), Err(ResolveFault::Shape));
    let mut extra = mount_bytes(r"\??\C:\owned");
    extra.push(0);
    assert_eq!(parse_target(&extra), Err(ResolveFault::Shape));
}
fn junction(path: &Path, target: &Path) {
    let target = std::fs::canonicalize(target).unwrap();
    let units: Vec<u16> = target.as_os_str().encode_wide().collect();
    let units = units.strip_prefix(&[92, 92, 63, 92]).unwrap_or(&units);
    let target = String::from_utf16(units).unwrap();
    let bytes = mount_bytes(&format!(r"\??\{target}"));
    let folder = OpenOptions::new()
        .write(true)
        .custom_flags(OPEN_NOFOLLOW | OPEN_DIRECTORY)
        .open(path)
        .unwrap();
    let mut returned = 0;
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
#[ignore = "owned Windows junction/held-file qualification; no executable launch"]
fn native_visible_junction_resolves_to_held_identical_physical_file() {
    let root = tempfile::Builder::new()
        .prefix("Zephium.CodexDiscovery.")
        .tempdir()
        .unwrap();
    let target = root.path().join("release");
    let alias = root.path().join("visible");
    std::fs::create_dir(&target).unwrap();
    std::fs::create_dir(&alias).unwrap();
    let image = target.join("codex.exe");
    std::fs::write(&image, b"synthetic-not-executable").unwrap();
    junction(&alias, &target);
    let resolved = local_file(&alias.join("codex.exe")).unwrap();
    assert_eq!(
        identity(&resolved.file).unwrap(),
        identity(&File::open(&image).unwrap()).unwrap()
    );
    assert_eq!(
        native_path(&resolved.file).unwrap(),
        std::fs::canonicalize(&image).unwrap()
    );
    assert_eq!(
        std::fs::write(&image, b"changed")
            .unwrap_err()
            .raw_os_error(),
        Some(32)
    );
    assert_eq!(
        std::fs::rename(&image, target.join("moved.exe"))
            .unwrap_err()
            .raw_os_error(),
        Some(32)
    );
    // The independently joined target is held, not the mutable visible selector.
    std::fs::remove_dir(&alias).unwrap();
    assert!(resolved.file.metadata().unwrap().is_file());
    drop(resolved);
    std::fs::write(&image, b"after-close").unwrap();
    assert!(matches!(
        CliCandidate::open(&image),
        Err(ExistingImageFault::Unsupported)
    ));
    root.close().unwrap();
}
#[test]
#[ignore = "owned Windows identity mismatch qualification; no executable launch"]
fn native_same_bytes_different_file_never_satisfy_identity_join() {
    let root = tempfile::Builder::new()
        .prefix("Zephium.CodexIdentity.")
        .tempdir()
        .unwrap();
    let first = root.path().join("first.exe");
    let second = root.path().join("second.exe");
    for path in [&first, &second] {
        let mut file = File::create(path).unwrap();
        file.write_all(b"same-synthetic-bytes").unwrap();
        file.sync_all().unwrap();
    }
    let initial = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&first)
        .unwrap();
    assert!(matches!(
        join_physical(&initial, &std::fs::canonicalize(&second).unwrap()),
        Err(ResolveFault::Changed)
    ));
    drop(initial);
    root.close().unwrap();
}

#[test]
fn final_file_symlink_keeps_exact_target_without_trailing_separator() {
    let target = PathBuf::from(r"C:\owned\codex.exe");
    assert_eq!(
        append_remaining(target.clone(), Path::new("")).as_os_str(),
        target.as_os_str()
    );
    let mut bytes = mount_bytes(r"\??\C:\owned\codex.exe");
    bytes[..4].copy_from_slice(&SYMLINK.to_le_bytes());
    bytes.splice(16..16, [0u8; 4]);
    let declared = (bytes.len() - 8) as u16;
    bytes[4..6].copy_from_slice(&declared.to_le_bytes());
    assert_eq!(parse_target(&bytes).unwrap(), target);
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(parse_target(&bytes), Err(ResolveFault::Shape));
}
