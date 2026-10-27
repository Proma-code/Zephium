//! Test-only owned junction control, using the same fixed FSCTL as image tests.
use super::*;
pub(super) fn junction(root: &tempfile::TempDir, path: &Path, target: &Path) {
    assert!(path.starts_with(root.path()) && target.starts_with(root.path()));
    let target = std::fs::canonicalize(target).unwrap();
    let units: Vec<u16> = target.as_os_str().encode_wide().collect();
    let units = units.strip_prefix(&[92, 92, 63, 92]).unwrap_or(&units);
    let target = format!(r"\??\{}", String::from_utf16(units).unwrap());
    let substitute: Vec<u8> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let print: Vec<u8> = target
        .strip_prefix(r"\??\")
        .unwrap()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let length = 8 + substitute.len() + 2 + print.len() + 2;
    let mut bytes = Vec::new();
    bytes.extend(0xa000_0003u32.to_le_bytes());
    bytes.extend(u16::try_from(length).unwrap().to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(u16::try_from(substitute.len()).unwrap().to_le_bytes());
    bytes.extend(u16::try_from(substitute.len() + 2).unwrap().to_le_bytes());
    bytes.extend(u16::try_from(print.len()).unwrap().to_le_bytes());
    bytes.extend(substitute);
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(print);
    bytes.extend(0u16.to_le_bytes());
    let file = OpenOptions::new()
        .write(true)
        .custom_flags(0x0220_0000)
        .open(path)
        .unwrap();
    let mut returned = 0;
    unsafe {
        windows::Win32::System::IO::DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            0x0009_00a4,
            Some(bytes.as_ptr().cast()),
            u32::try_from(bytes.len()).unwrap(),
            None,
            0,
            Some(&mut returned),
            None,
        )
    }
    .unwrap();
}
