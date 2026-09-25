//! Spooler change notifications (`FindFirstPrinterChangeNotification`).
//!
//! Windows removes a finished job from the queue within moments, often before the next
//! status poll, and a job deleted by the user looks the same once it is gone. A watcher
//! thread per active printer subscribes to job status changes and remembers every status
//! bit each job has shown, so the provider can tell "printed, then removed" from "deleted".
//!
//! Watchers start before a job is submitted (so its whole life is observed) and exit after
//! two minutes without anything to track, keeping the thread count bounded by the number of
//! printers that are actually busy.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Printing::{
    FindClosePrinterChangeNotification, FindFirstPrinterChangeNotification,
    FindNextPrinterChangeNotification, FreePrinterNotifyInfo, JOB_NOTIFY_FIELD_STATUS,
    JOB_NOTIFY_TYPE, PRINTER_CHANGE_JOB, PRINTER_NOTIFY_INFO, PRINTER_NOTIFY_INFO_DISCARDED,
    PRINTER_NOTIFY_OPTIONS, PRINTER_NOTIFY_OPTIONS_REFRESH, PRINTER_NOTIFY_OPTIONS_TYPE,
};
use windows::Win32::System::Threading::WaitForSingleObject;

use crate::ffi::PrinterHandle;

const IDLE_EXIT: Duration = Duration::from_secs(120);
const MAX_TRACKED: usize = 10_000;

#[derive(Default)]
struct Shared {
    /// Accumulated `JOB_STATUS_*` bits per spooler job id.
    history: Mutex<HashMap<u32, u32>>,
    last_used: Mutex<Option<Instant>>,
    stopped: AtomicBool,
}

impl Shared {
    fn touch(&self) {
        *self
            .last_used
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }

    /// Idle once the engine has not asked about this printer for a while (other
    /// applications' jobs keep arriving in `history`, so emptiness is no signal).
    fn idle(&self) -> bool {
        let last = *self
            .last_used
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        last.is_none_or(|t| t.elapsed() > IDLE_EXIT)
    }
}

#[derive(Default)]
pub(crate) struct JobWatchers {
    printers: Mutex<HashMap<String, Arc<Shared>>>,
}

impl std::fmt::Debug for JobWatchers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobWatchers").finish_non_exhaustive()
    }
}

impl JobWatchers {
    /// Ensures a watcher is armed for `printer`. Waits briefly for the subscription so a
    /// job submitted right after is observed from its first status change.
    pub(crate) fn ensure(&self, printer: &str) {
        let shared = {
            let mut printers = self.printers.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = printers
                .get(printer)
                .filter(|s| !s.stopped.load(Ordering::Acquire))
            {
                existing.touch();
                return;
            }
            let shared = Arc::new(Shared::default());
            shared.touch();
            printers.insert(printer.to_owned(), shared.clone());
            shared
        };
        let (ready_tx, ready_rx) = mpsc::channel();
        let name = printer.to_owned();
        let spawned = std::thread::Builder::new()
            .name("kiln-spool-watch".into())
            .spawn(move || run(&name, &shared, ready_tx));
        if spawned.is_ok() {
            let _ = ready_rx.recv_timeout(Duration::from_secs(2));
        }
    }

    /// Every status bit seen for the job so far (0 if never observed).
    pub(crate) fn observed(&self, printer: &str, job_id: u32) -> u32 {
        let printers = self.printers.lock().unwrap_or_else(PoisonError::into_inner);
        printers.get(printer).map_or(0, |s| {
            s.touch();
            s.history
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&job_id)
                .copied()
                .unwrap_or(0)
        })
    }

    pub(crate) fn forget(&self, printer: &str, job_id: u32) {
        let printers = self.printers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = printers.get(printer) {
            s.history
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&job_id);
        }
    }
}

struct Change(HANDLE);

impl Drop for Change {
    fn drop(&mut self) {
        // SAFETY: handle from FindFirstPrinterChangeNotification, closed once.
        let _ = unsafe { FindClosePrinterChangeNotification(self.0) };
    }
}

fn run(printer: &str, shared: &Shared, ready: mpsc::Sender<()>) {
    let finish = || shared.stopped.store(true, Ordering::Release);
    let Ok(handle) = PrinterHandle::open(printer) else {
        return finish();
    };
    let mut fields = [JOB_NOTIFY_FIELD_STATUS as u16];
    let mut kind = PRINTER_NOTIFY_OPTIONS_TYPE {
        Type: JOB_NOTIFY_TYPE as u16,
        Count: 1,
        pFields: fields.as_mut_ptr(),
        ..Default::default()
    };
    let mut options = PRINTER_NOTIFY_OPTIONS {
        Version: 2,
        Flags: 0,
        Count: 1,
        pTypes: &mut kind,
    };
    // SAFETY: the options structure and field array outlive the subscription.
    let change = unsafe {
        FindFirstPrinterChangeNotification(
            handle.raw(),
            PRINTER_CHANGE_JOB,
            0,
            Some((&raw const options).cast()),
        )
    };
    if change.is_invalid() {
        tracing::debug!(target: "kiln::windows", printer, "change notifications unavailable; relying on polling");
        return finish();
    }
    let change = Change(change);
    let _ = ready.send(());
    tracing::debug!(target: "kiln::windows", printer, "spooler watcher started");

    let mut refresh = false;
    while !shared.idle() {
        // SAFETY: valid change handle.
        if unsafe { WaitForSingleObject(change.0, 500) } != WAIT_OBJECT_0 && !refresh {
            continue;
        }
        options.Flags = if refresh {
            PRINTER_NOTIFY_OPTIONS_REFRESH
        } else {
            0
        };
        let mut cause = 0u32;
        let mut info: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: out-pointers are valid; `options` requests the same fields as FindFirst.
        let ok = unsafe {
            FindNextPrinterChangeNotification(
                change.0,
                Some(&mut cause),
                Some((&raw const options).cast()),
                Some(&mut info),
            )
        };
        refresh = false;
        if !ok.as_bool() || info.is_null() {
            continue;
        }
        let info = info.cast::<PRINTER_NOTIFY_INFO>();
        // SAFETY: the spooler returned a PRINTER_NOTIFY_INFO with `Count` data entries,
        // freed below with FreePrinterNotifyInfo.
        unsafe {
            refresh = (*info).Flags & PRINTER_NOTIFY_INFO_DISCARDED != 0;
            let data = std::slice::from_raw_parts(
                (&raw const (*info).aData)
                    .cast::<windows::Win32::Graphics::Printing::PRINTER_NOTIFY_INFO_DATA>(),
                (*info).Count as usize,
            );
            let mut history = shared
                .history
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if history.len() > MAX_TRACKED {
                history.clear();
            }
            for entry in data {
                if u32::from(entry.Type) == JOB_NOTIFY_TYPE
                    && u32::from(entry.Field) == JOB_NOTIFY_FIELD_STATUS
                {
                    *history.entry(entry.Id).or_default() |= entry.NotifyData.adwData[0];
                }
            }
            let _ = FreePrinterNotifyInfo(info);
        }
    }
    tracing::debug!(target: "kiln::windows", printer, "spooler watcher stopped (idle)");
    finish();
}
