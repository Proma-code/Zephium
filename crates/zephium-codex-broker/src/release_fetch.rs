//! Credential-free fixed-origin HTTPS metadata. No artifact download/installation.
//! Handles remain owned through synchronous WinHTTP completion. Logical timeout
//! fences admission; it is not a claim that a blocking OS call was hard-killed.
use crate::release_admission::{
    OfficialReleaseResponse, OriginalCliArtifact, ReleaseQuery, FETCH_TIMEOUT,
};
use std::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use windows::{
    core::{w, HSTRING, PCWSTR},
    Win32::Networking::WinHttp::*,
};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    Cancelled,
    Timeout,
    Transport,
    Status,
    Limit,
    Metadata,
}
struct Internet(*mut c_void);
impl Internet {
    fn new(raw: *mut c_void) -> Result<Self, Fault> {
        if raw.is_null() {
            Err(Fault::Transport)
        } else {
            Ok(Self(raw))
        }
    }
}
impl Drop for Internet {
    fn drop(&mut self) {
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}
fn current(deadline: Instant, cancel: &AtomicBool) -> Result<(), Fault> {
    if cancel.load(Ordering::Acquire) {
        Err(Fault::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Fault::Timeout)
    } else {
        Ok(())
    }
}
/// Only ReleaseQuery can choose the exact public path on api.github.com.
/// TLS hostname/certificate validation is unchanged. Never sends auth/cookies,
/// accepts a redirect, uses ambient proxy credentials, or interprets asset URLs.
pub(crate) fn fetch(
    query: ReleaseQuery,
    outer_deadline: Instant,
    cancel: &AtomicBool,
) -> Result<OriginalCliArtifact, Fault> {
    fetch_response(query, outer_deadline, cancel)?
        .decode(Instant::now())
        .map_err(|_| Fault::Metadata)
}
pub(crate) fn fetch_index(
    query: ReleaseQuery,
    outer_deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Vec<OriginalCliArtifact>, Fault> {
    fetch_response(query, outer_deadline, cancel)?
        .decode_index(Instant::now())
        .map_err(|_| Fault::Metadata)
}
fn fetch_response(
    query: ReleaseQuery,
    outer_deadline: Instant,
    cancel: &AtomicBool,
) -> Result<OfficialReleaseResponse, Fault> {
    let started = Instant::now();
    let deadline = (started + FETCH_TIMEOUT).min(outer_deadline);
    current(deadline, cancel)?;
    let path = query
        .url()
        .strip_prefix("https://api.github.com")
        .ok_or(Fault::Metadata)?;
    let object = HSTRING::from(path);
    let session = Internet::new(unsafe {
        WinHttpOpen(
            w!("Zephium-Codex-Compatibility/1"),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    })?;
    let time = i32::try_from(
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis(),
    )
    .unwrap_or(10_000)
    .clamp(1, 10_000);
    unsafe { WinHttpSetTimeouts(session.0, time, time, time, time) }
        .map_err(|_| Fault::Transport)?;
    let connection =
        Internet::new(unsafe { WinHttpConnect(session.0, w!("api.github.com"), 443, 0) })?;
    current(deadline, cancel)?;
    let request = Internet::new(unsafe {
        WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            &object,
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    })?;
    let disabled =
        (WINHTTP_DISABLE_AUTHENTICATION | WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS)
            .to_ne_bytes();
    unsafe {
        WinHttpSetOption(
            Some(request.0),
            WINHTTP_OPTION_DISABLE_FEATURE,
            Some(&disabled),
        )
    }
    .map_err(|_| Fault::Transport)?;
    current(deadline, cancel)?;
    let headers = "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n"
        .encode_utf16()
        .collect::<Vec<_>>();
    unsafe { WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0) }
        .map_err(|_| Fault::Transport)?;
    current(deadline, cancel)?;
    unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) }.map_err(|_| Fault::Transport)?;
    current(deadline, cancel)?;
    let mut status = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            ptr::null_mut(),
        )
    }
    .map_err(|_| Fault::Transport)?;
    if size != 4 || status != 200 {
        return Err(Fault::Status);
    }
    let mut body = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        current(deadline, cancel)?;
        let mut read = 0u32;
        unsafe {
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
        }
        .map_err(|_| Fault::Transport)?;
        let read = usize::try_from(read).map_err(|_| Fault::Limit)?;
        if read > buffer.len() || body.len().saturating_add(read) > query.max_metadata_bytes() {
            return Err(Fault::Limit);
        }
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
    }
    current(deadline, cancel)?;
    let completed = Instant::now();
    let url = query.url().to_owned();
    OfficialReleaseResponse::from_native_fetch(query, &url, 200, started, completed, body)
        .map_err(|_| Fault::Metadata)
}
