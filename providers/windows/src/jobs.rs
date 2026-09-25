//! Spooler job status, queue inspection and cancellation.

use chrono::{DateTime, NaiveDate, Utc};
use kiln_core::error::{ErrorCode, PrintError};
use kiln_core::provider::{ProviderJobState, QueueEntry};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Graphics::Printing::{
    EnumJobsW, GetJobW, JOB_CONTROL_DELETE, JOB_INFO_1W, SetJobW,
};

use crate::ffi::{PrinterHandle, PrinterNameExt, map_error, query, read_pwstr, win32, win32_code};
use crate::status;

pub(crate) fn state(printer_name: &str, job_id: u32) -> Result<ProviderJobState, PrintError> {
    let handle = PrinterHandle::open(printer_name)?;
    let result = query(|buf, needed| {
        // SAFETY: arguments are valid for the duration of the call.
        unsafe { GetJobW(handle.raw(), job_id, 1, buf, needed) }.ok()
    });
    let buffer = match result {
        Ok(buffer) => buffer,
        // The spooler no longer knows the job: it finished and was removed, or was deleted.
        Err(e) if win32_code(&e) == Some(win32::ERROR_INVALID_PARAMETER) => {
            return Ok(ProviderJobState::Gone);
        }
        Err(e) => {
            return Err(map_error("could not query job status", &e).with_printer_name(printer_name));
        }
    };
    if buffer.len() < std::mem::size_of::<JOB_INFO_1W>() {
        return Ok(ProviderJobState::Gone);
    }
    // SAFETY: the buffer holds one JOB_INFO_1W whose strings point into the buffer.
    let (flags, text) = unsafe {
        let info = &*buffer.as_ptr::<JOB_INFO_1W>();
        (info.Status, read_pwstr(info.pStatus))
    };
    Ok(status::job_state(flags, text))
}

pub(crate) fn queue(printer_name: &str) -> Result<Vec<QueueEntry>, PrintError> {
    let handle = PrinterHandle::open(printer_name)?;
    let mut count = 0u32;
    let buffer = query(|buf, needed| {
        // SAFETY: arguments are valid for the duration of the call.
        unsafe { EnumJobsW(handle.raw(), 0, u32::MAX, 1, buf, needed, &mut count) }
    })
    .map_err(|e| map_error("could not read the print queue", &e).with_printer_name(printer_name))?;
    if count == 0 || buffer.len() == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: the spooler wrote `count` JOB_INFO_1W records into the aligned buffer.
    let jobs =
        unsafe { std::slice::from_raw_parts(buffer.as_ptr::<JOB_INFO_1W>(), count as usize) };
    Ok(jobs
        .iter()
        .map(|j| QueueEntry {
            spooler_job_id: u64::from(j.JobId),
            // Document names and status text are user-visible strings owned by the spooler.
            // SAFETY: pointers reference the buffer above.
            document_name: unsafe { read_pwstr(j.pDocument) },
            status_text: unsafe { read_pwstr(j.pStatus) },
            status: status::job_flag_names(j.Status),
            position: Some(j.Position),
            total_pages: (j.TotalPages > 0).then_some(j.TotalPages),
            pages_printed: Some(j.PagesPrinted),
            size_bytes: None,
            submitted_at: systemtime_to_utc(&j.Submitted),
            kiln_job: None,
        })
        .collect())
}

pub(crate) fn cancel(printer_name: &str, job_id: u32) -> Result<(), PrintError> {
    let handle = PrinterHandle::open(printer_name)?;
    // SAFETY: valid handle; no job info structure is passed for JOB_CONTROL_DELETE.
    unsafe { SetJobW(handle.raw(), job_id, 0, None, JOB_CONTROL_DELETE) }
        .ok()
        .map_err(|e| {
            if win32_code(&e) == Some(win32::ERROR_INVALID_PARAMETER) {
                PrintError::new(
                    ErrorCode::InvalidJobState,
                    "the job is no longer in the print queue",
                )
            } else {
                map_error("could not cancel the job", &e)
            }
            .with_printer_name(printer_name)
        })
}

/// `JOB_INFO_1W.Submitted` is in UTC.
fn systemtime_to_utc(t: &SYSTEMTIME) -> Option<DateTime<Utc>> {
    let date = NaiveDate::from_ymd_opt(i32::from(t.wYear), u32::from(t.wMonth), u32::from(t.wDay))?;
    let time = date.and_hms_milli_opt(
        u32::from(t.wHour),
        u32::from(t.wMinute),
        u32::from(t.wSecond),
        u32::from(t.wMilliseconds),
    )?;
    Some(DateTime::from_naive_utc_and_offset(time, Utc))
}
