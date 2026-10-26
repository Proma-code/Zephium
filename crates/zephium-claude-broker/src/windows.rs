//! Native candidate runtime. No production constructor bypasses qualification.
#[cfg(feature = "native-fixtures")]
pub(super) mod fixtures;
mod process;
mod security;
mod signature;

use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
fn wide(value: &OsStr) -> Result<Vec<u16>, &'static str> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err("invalid native path");
    }
    value.push(0);
    Ok(value)
}

// Fixed, content-free fixture diagnostics. No path, argument, provider message,
// account identifier or credential is ever accepted by this reporter.
fn diagnostic(stage: &'static str, code: i32) {
    #[cfg(feature = "native-fixtures")]
    {
        use std::io::Write;
        let _ = std::io::stdout()
            .write_all(format!("fixture-stage={stage}; native-code={code}\n").as_bytes());
    }
    #[cfg(not(feature = "native-fixtures"))]
    let _ = (stage, code);
}
