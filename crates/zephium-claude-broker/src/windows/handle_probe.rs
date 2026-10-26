//! Fixed suspended-child object-identity probe, never a generic handle API.
use super::handle;
use std::{
    mem::size_of,
    os::windows::io::{FromRawHandle, OwnedHandle},
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{
            CompareObjectHandles, DuplicateHandle, GetHandleInformation, GetLastError,
            SetLastError, DUPLICATE_SAME_ACCESS, ERROR_INVALID_HANDLE, ERROR_NOT_SAME_OBJECT,
            HANDLE, HANDLE_FLAG_INHERIT, WIN32_ERROR,
        },
        Security::SECURITY_ATTRIBUTES,
        System::Threading::{CreateEventW, GetCurrentProcess},
    },
};

pub(super) struct Probe {
    // Remains held across creation, inspection, resume and actual settlement.
    event: OwnedHandle,
}
fn event(inherit: bool) -> Result<OwnedHandle, &'static str> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: inherit.into(),
        ..Default::default()
    };
    // SAFETY: live initialized attributes, unnamed fresh event, owned return.
    let raw = unsafe { CreateEventW(Some(&attributes), true, false, PCWSTR::null()) }
        .map_err(|_| "fresh handle sentinel unavailable")?;
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
}
fn same(first: HANDLE, second: HANDLE) -> Result<bool, &'static str> {
    // SAFETY: both handles are live owned handles. Clear only this thread's last
    // error so an unknown comparison failure cannot inherit a stale status.
    unsafe { SetLastError(WIN32_ERROR(0)) };
    let equal = unsafe { CompareObjectHandles(first, second) }.as_bool();
    let error = unsafe { GetLastError() };
    if equal {
        Ok(true)
    } else if error == ERROR_NOT_SAME_OBJECT {
        Ok(false)
    } else {
        super::super::diagnostic("handle-object-comparison", error.0 as i32);
        Err("handle object comparison unconfirmed")
    }
}
fn duplicate(process: HANDLE, source: HANDLE) -> windows::core::Result<OwnedHandle> {
    let mut raw = HANDLE::default();
    // SAFETY: process is either our live owned suspended child or the current
    // process. No CLOSE_SOURCE, inheritance or target-child duplication occurs.
    unsafe {
        DuplicateHandle(
            process,
            source,
            GetCurrentProcess(),
            &mut raw,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }?;
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
}
impl Probe {
    pub(super) fn new() -> Result<Self, &'static str> {
        let event = event(true)?;
        let mut flags = 0;
        unsafe { GetHandleInformation(handle(&event), &mut flags) }
            .map_err(|_| "parent event handle control failed")?;
        if flags & HANDLE_FLAG_INHERIT.0 == 0 {
            return Err("parent event was not inheritable");
        }
        let copy = duplicate(unsafe { GetCurrentProcess() }, handle(&event))
            .map_err(|_| "parent handle duplicate control failed")?;
        let other = self::event(false)?;
        if !same(handle(&event), handle(&copy))? || same(handle(&event), handle(&other))? {
            return Err("parent object identity controls failed");
        }
        super::super::diagnostic("handle-parent-object-controls", 0);
        Ok(Self { event })
    }
    /// Must be called on the actual still-suspended child before pipe closure or
    /// ResumeThread. The listed stdout is a positive for both numeric inheritance
    /// and the kernel-object comparison, not merely a valid numeric slot.
    pub(super) fn verify(&self, child: HANDLE, listed_stdout: HANDLE) -> Result<(), &'static str> {
        let listed = duplicate(child, listed_stdout)
            .map_err(|_| "listed child handle could not be inspected")?;
        if !same(listed_stdout, handle(&listed))? {
            return Err("listed child handle identity mismatch");
        }
        super::super::diagnostic("handle-listed-object-control", 0);
        match duplicate(child, handle(&self.event)) {
            Ok(candidate) => {
                if same(handle(&self.event), handle(&candidate))? {
                    return Err("unlisted event object was inherited");
                }
                super::super::diagnostic("handle-unlisted-other-object", 0);
            }
            Err(error)
                if error.code() == windows::core::HRESULT::from_win32(ERROR_INVALID_HANDLE.0) =>
            {
                super::super::diagnostic("handle-unlisted-slot-absent", 0);
            }
            Err(error) => {
                super::super::diagnostic("handle-unlisted-duplicate", error.code().0);
                return Err("unlisted handle inspection unconfirmed");
            }
        }
        Ok(())
    }
}
