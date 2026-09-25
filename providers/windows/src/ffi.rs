//! Small safe wrappers around Win32 printing primitives.

use std::ptr;

use kiln_core::error::{ErrorCode, PrintError};
use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows::Win32::Graphics::Printing::{
    ClosePrinter, OpenPrinterW, PRINTER_ACCESS_USE, PRINTER_DEFAULTSW, PRINTER_HANDLE,
};
use windows::core::{PCWSTR, PWSTR};

/// Null-terminated UTF-16 string.
pub(crate) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a spooler-owned string; `None` for null or empty.
///
/// # Safety
/// `p` must be null or point to a valid null-terminated UTF-16 string.
pub(crate) unsafe fn read_pwstr(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    let s = unsafe { p.to_string() }.ok()?;
    (!s.is_empty()).then_some(s)
}

/// 8-byte aligned byte buffer. Spooler APIs return arrays of structs containing pointers,
/// which must not be read from a `Vec<u8>` (alignment 1).
pub(crate) struct Buffer {
    words: Vec<u64>,
    len: usize,
}

impl Buffer {
    pub(crate) fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(8)],
            len,
        }
    }

    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: `words` owns at least `len` initialised bytes.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast::<u8>(), self.len) }
    }

    pub(crate) fn as_ptr<T>(&self) -> *const T {
        self.words.as_ptr().cast()
    }

    pub(crate) fn as_mut_ptr<T>(&mut self) -> *mut T {
        self.words.as_mut_ptr().cast()
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

/// Runs the Win32 "ask for size, allocate, call again" dance. Retries if the required size
/// grows between calls (e.g. a job was added to the queue).
pub(crate) fn query(
    mut call: impl FnMut(Option<&mut [u8]>, &mut u32) -> windows::core::Result<()>,
) -> windows::core::Result<Buffer> {
    let insufficient = ERROR_INSUFFICIENT_BUFFER.to_hresult();
    let mut needed = 0u32;
    match call(None, &mut needed) {
        Ok(()) => return Ok(Buffer::new(0)),
        Err(e) if e.code() == insufficient => {}
        Err(e) => return Err(e),
    }
    for _ in 0..4 {
        let mut buffer = Buffer::new(needed as usize);
        let mut again = 0u32;
        match call(Some(buffer.bytes_mut()), &mut again) {
            Ok(()) => return Ok(buffer),
            Err(e) if e.code() == insufficient => needed = again.max(needed.saturating_mul(2)),
            Err(e) => return Err(e),
        }
    }
    Err(windows::core::Error::from_hresult(insufficient))
}

/// Printer handle closed on drop.
pub(crate) struct PrinterHandle(PRINTER_HANDLE);

impl PrinterHandle {
    pub(crate) fn open(name: &str) -> Result<Self, PrintError> {
        let name_w = wide(name);
        let mut handle = PRINTER_HANDLE {
            Value: ptr::null_mut(),
        };
        let defaults = PRINTER_DEFAULTSW {
            pDatatype: PWSTR::null(),
            pDevMode: ptr::null_mut(),
            DesiredAccess: PRINTER_ACCESS_USE,
        };
        // SAFETY: `name_w` is null-terminated and outlives the call; `handle` is a valid
        // out-pointer.
        unsafe { OpenPrinterW(PCWSTR(name_w.as_ptr()), &mut handle, Some(&defaults)) }
            .map_err(|e| map_error("could not open printer", &e).with_printer_name(name))?;
        Ok(Self(handle))
    }

    pub(crate) fn raw(&self) -> PRINTER_HANDLE {
        self.0
    }
}

impl Drop for PrinterHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful OpenPrinterW and is closed once.
        let _ = unsafe { ClosePrinter(self.0) };
    }
}

pub(crate) mod win32 {
    pub const ERROR_ACCESS_DENIED: u32 = 5;
    pub const ERROR_NOT_SUPPORTED: u32 = 50;
    pub const ERROR_PRINT_CANCELLED: u32 = 63;
    pub const ERROR_INVALID_PARAMETER: u32 = 87;
    pub const ERROR_INVALID_PRINTER_NAME: u32 = 1801;
    pub const ERROR_INVALID_DATATYPE: u32 = 1804;
    pub const ERROR_PRINTER_DELETED: u32 = 1905;
    pub const RPC_S_SERVER_UNAVAILABLE: u32 = 1722;
    pub const RPC_S_CALL_FAILED: u32 = 1726;
}

/// Win32 error code carried by an HRESULT built from a Win32 error, if any.
pub(crate) fn win32_code(error: &windows::core::Error) -> Option<u32> {
    let hr = error.code().0 as u32;
    (hr & 0xFFFF_0000 == 0x8007_0000).then_some(hr & 0xFFFF)
}

/// Maps a Win32 failure to a client-safe error. The OS message is a short localised
/// sentence (no paths or internals), so it is included for diagnosability.
pub(crate) fn map_error(context: &str, error: &windows::core::Error) -> PrintError {
    use win32::*;
    let code = win32_code(error);
    let error_code = match code {
        Some(ERROR_INVALID_PRINTER_NAME | ERROR_PRINTER_DELETED) => ErrorCode::PrinterNotFound,
        Some(ERROR_ACCESS_DENIED) => ErrorCode::AccessDenied,
        Some(ERROR_INVALID_DATATYPE | ERROR_NOT_SUPPORTED) => ErrorCode::UnsupportedOperation,
        Some(RPC_S_SERVER_UNAVAILABLE | RPC_S_CALL_FAILED) => ErrorCode::ConnectionError,
        _ => ErrorCode::SpoolerError,
    };
    let os_message = error.message();
    let message = if os_message.trim().is_empty() {
        context.to_owned()
    } else {
        format!("{context}: {}", os_message.trim())
    };
    PrintError::new(error_code, message).with_details(details(code, error.code().0))
}

pub(crate) trait PrinterNameExt {
    fn with_printer_name(self, name: &str) -> Self;
}

impl PrinterNameExt for PrintError {
    fn with_printer_name(self, name: &str) -> Self {
        self.with_printer(kiln_core::model::PrinterId::derive(crate::provider::PROVIDER_ID, name).0)
    }
}

fn details(win32: Option<u32>, hresult: i32) -> serde_json::Value {
    match win32 {
        Some(code) => serde_json::json!({ "win32Error": code }),
        None => serde_json::json!({ "hresult": format!("0x{:08X}", hresult as u32) }),
    }
}
