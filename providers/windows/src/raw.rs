//! Byte-for-byte RAW printing through the spooler (`RAW` datatype).
//!
//! With the `RAW` datatype the print processor forwards the spool file to the port
//! monitor without involving the driver's renderer, so the device receives exactly the
//! bytes written here. Copies are produced by writing the payload N times inside one spool
//! job (one "page" per copy): RAW data bypasses the driver, so `DEVMODE` copy counts do
//! not apply. Printer languages usually also have their own quantity commands (`^PQ`,
//! `P<n>`), which pass through untouched.

use std::path::Path;

use kiln_core::error::{ErrorCode, PrintError};
use windows::Win32::Graphics::Printing::{
    AbortPrinter, DOC_INFO_1W, EndDocPrinter, EndPagePrinter, StartDocPrinterW, StartPagePrinter,
    WritePrinter,
};
use windows::core::PWSTR;

use crate::ffi::{PrinterHandle, PrinterNameExt, map_error, wide, win32_code};

const CHUNK: usize = 64 * 1024;

/// Aborts the spool job unless explicitly finished, so an error at any point never leaves
/// a half-written job to print.
struct SpoolJob<'a> {
    handle: &'a PrinterHandle,
    finished: bool,
}

impl Drop for SpoolJob<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // SAFETY: the handle is open and has an active document.
            let _ = unsafe { AbortPrinter(self.handle.raw()) };
        }
    }
}

pub(crate) fn submit(
    printer_name: &str,
    document_name: &str,
    copies: u32,
    data: &[u8],
    output_file: Option<&Path>,
) -> Result<u32, PrintError> {
    let handle = PrinterHandle::open(printer_name)?;
    let mut doc_name = wide(document_name);
    let mut datatype = wide("RAW");
    let mut output = output_file.map(|p| wide(&p.to_string_lossy()));
    let info = DOC_INFO_1W {
        pDocName: PWSTR(doc_name.as_mut_ptr()),
        pOutputFile: output
            .as_mut()
            .map_or(PWSTR::null(), |o| PWSTR(o.as_mut_ptr())),
        pDatatype: PWSTR(datatype.as_mut_ptr()),
    };

    // SAFETY: `info` and the strings it points to outlive the call.
    let job_id = unsafe { StartDocPrinterW(handle.raw(), 1, &info) };
    if job_id == 0 {
        let err = windows::core::Error::from_thread();
        let mapped = if win32_code(&err) == Some(crate::ffi::win32::ERROR_INVALID_DATATYPE) {
            PrintError::new(
                ErrorCode::UnsupportedOperation,
                "the printer driver does not accept RAW data (v4/XPS drivers usually do not)",
            )
        } else {
            map_error("the spooler refused the job", &err)
        };
        return Err(mapped.with_printer_name(printer_name));
    }
    let mut job = SpoolJob {
        handle: &handle,
        finished: false,
    };

    for _ in 0..copies {
        // SAFETY: a document is active on the handle.
        check(
            unsafe { StartPagePrinter(handle.raw()) }.ok(),
            "could not start page",
            printer_name,
        )?;
        write_all(&handle, data).map_err(|e| partial(e, printer_name))?;
        // SAFETY: a page is active on the handle.
        check(
            unsafe { EndPagePrinter(handle.raw()) }.ok(),
            "could not end page",
            printer_name,
        )?;
    }
    // SAFETY: a document is active on the handle.
    check(
        unsafe { EndDocPrinter(handle.raw()) }.ok(),
        "could not finish the spool job",
        printer_name,
    )?;
    job.finished = true;
    Ok(job_id)
}

fn write_all(handle: &PrinterHandle, mut data: &[u8]) -> windows::core::Result<()> {
    while !data.is_empty() {
        let len = data.len().min(CHUNK);
        let mut written = 0u32;
        // SAFETY: `data[..len]` is valid for reads; `written` is a valid out-pointer.
        unsafe { WritePrinter(handle.raw(), data.as_ptr().cast(), len as u32, &mut written) }
            .ok()?;
        if written == 0 {
            // A zero-byte write would loop forever; the port stopped accepting data.
            return Err(windows::core::Error::from_hresult(
                windows::core::HRESULT::from_win32(crate::ffi::win32::ERROR_PRINT_CANCELLED),
            ));
        }
        data = &data[written as usize..];
    }
    Ok(())
}

fn check(
    result: windows::core::Result<()>,
    context: &str,
    printer: &str,
) -> Result<(), PrintError> {
    result.map_err(|e| partial(e, printer).with_message_context(context))
}

/// A failure after `StartDocPrinter`: the job is aborted, but a printer configured to
/// "print directly to the printer" (no spooling) may already have received some bytes.
fn partial(err: windows::core::Error, printer: &str) -> PrintError {
    map_error(
        "writing to the printer failed; the spool job was aborted",
        &err,
    )
    .recoverable(false)
    .with_printer_name(printer)
}

trait MessageContext {
    fn with_message_context(self, context: &str) -> Self;
}

impl MessageContext for PrintError {
    fn with_message_context(mut self, context: &str) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }
}
