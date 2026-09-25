//! Printer enumeration via `EnumPrintersW` level 2.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use kiln_core::error::PrintError;
use kiln_core::model::{Printer, PrinterId};
use tracing::debug;
use windows::Win32::Graphics::Printing::{
    DATATYPES_INFO_1W, DRIVER_INFO_8W, EnumPrintProcessorDatatypesW, EnumPrintersW,
    GetDefaultPrinterW, GetPrinterDriverW, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL,
    PRINTER_INFO_2W,
};
use windows::core::{PCWSTR, PWSTR};

use crate::ffi::{PrinterHandle, map_error, query, read_pwstr, wide};
use crate::provider::PROVIDER_ID;
use crate::status;

/// Per-printer facts that are expensive to query and only change with the driver.
#[derive(Debug, Clone)]
pub(crate) struct PrinterMeta {
    pub driver: String,
    pub port: Option<String>,
    pub datatypes: Option<Vec<String>>,
    /// Driver model version: 3 = classic GDI/v3 (including XPSDrv), 4 = v4 class driver.
    pub driver_version: Option<u32>,
}

impl PrinterMeta {
    /// RAW is usable when the print processor lists it and the driver is not a v4 driver
    /// (the v4 pipeline expects XPS and does not pass RAW data through reliably).
    pub fn raw_supported(&self) -> Option<bool> {
        let listed = self
            .datatypes
            .as_ref()
            .map(|d| d.iter().any(|t| t.eq_ignore_ascii_case("RAW")));
        match (listed, self.driver_version) {
            (Some(false), _) => Some(false),
            (_, Some(v)) if v >= 4 => Some(false),
            (listed, _) => listed,
        }
    }
}

pub(crate) type MetaCache = Mutex<HashMap<String, PrinterMeta>>;

pub(crate) fn enumerate(cache: &MetaCache) -> Result<Vec<Printer>, PrintError> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut count = 0u32;
    let buffer = query(|buf, needed| {
        // SAFETY: buffer/needed/count are valid for the duration of the call.
        unsafe { EnumPrintersW(flags, PCWSTR::null(), 2, buf, needed, &mut count) }
    })
    .map_err(|e| map_error("printer enumeration failed", &e))?;
    if count == 0 || buffer.len() == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: on success the spooler wrote `count` PRINTER_INFO_2W records at the start
    // of the (8-byte aligned) buffer; their string pointers point into the same buffer.
    let infos =
        unsafe { std::slice::from_raw_parts(buffer.as_ptr::<PRINTER_INFO_2W>(), count as usize) };
    let default_name = default_printer_name();

    let mut printers = Vec::with_capacity(infos.len());
    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    for info in infos {
        // SAFETY: pointers come from the spooler buffer above.
        let (name, port, driver, location, processor) = unsafe {
            (
                read_pwstr(info.pPrinterName),
                read_pwstr(info.pPortName),
                read_pwstr(info.pDriverName).unwrap_or_default(),
                read_pwstr(info.pLocation),
                read_pwstr(info.pPrintProcessor),
            )
        };
        let Some(name) = name else { continue };
        let (state, conditions, online) = status::printer_state(info.Status, info.Attributes);
        let connection = status::classify_connection(
            &name,
            port.as_deref().unwrap_or(""),
            &driver,
            info.Attributes,
        );

        let stale = cache
            .get(&name)
            .is_none_or(|m| m.driver != driver || m.port != port);
        if stale {
            let meta = PrinterMeta {
                datatypes: processor.as_deref().and_then(datatypes),
                driver_version: driver_version(&name),
                driver: driver.clone(),
                port: port.clone(),
            };
            debug!(target: "kiln::windows", printer = %name, ?meta, "printer metadata refreshed");
            cache.insert(name.clone(), meta);
        }

        let language = status::guess_language(&name, &driver).map(str::to_owned);
        printers.push(Printer {
            id: PrinterId::derive(PROVIDER_ID, &name),
            display_name: name.clone(),
            default: default_name.as_deref() == Some(name.as_str()),
            name,
            provider: PROVIDER_ID.into(),
            connection,
            driver: (!driver.is_empty()).then_some(driver),
            port,
            location,
            online,
            status: state,
            conditions,
            queued_jobs: Some(info.cJobs),
            language,
            capabilities: None,
        });
    }
    let live: std::collections::HashSet<&str> = printers.iter().map(|p| p.name.as_str()).collect();
    cache.retain(|name, _| live.contains(name.as_str()));
    Ok(printers)
}

fn default_printer_name() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: querying the required length with a null buffer is documented behaviour.
    let _ = unsafe { GetDefaultPrinterW(None, &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` holds `len` UTF-16 units.
    if !unsafe { GetDefaultPrinterW(Some(PWSTR(buf.as_mut_ptr())), &mut len) }.as_bool() {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

fn datatypes(print_processor: &str) -> Option<Vec<String>> {
    let processor = wide(print_processor);
    let mut count = 0u32;
    let buffer = query(|buf, needed| {
        // SAFETY: arguments are valid for the duration of the call.
        unsafe {
            EnumPrintProcessorDatatypesW(
                PCWSTR::null(),
                PCWSTR(processor.as_ptr()),
                1,
                buf,
                needed,
                &mut count,
            )
        }
        .ok()
    })
    .ok()?;
    if count == 0 || buffer.len() == 0 {
        return Some(Vec::new());
    }
    // SAFETY: the spooler wrote `count` DATATYPES_INFO_1W records.
    let infos =
        unsafe { std::slice::from_raw_parts(buffer.as_ptr::<DATATYPES_INFO_1W>(), count as usize) };
    // SAFETY: names point into `buffer`.
    Some(
        infos
            .iter()
            .filter_map(|i| unsafe { read_pwstr(i.pName) })
            .collect(),
    )
}

fn driver_version(printer_name: &str) -> Option<u32> {
    let handle = PrinterHandle::open(printer_name).ok()?;
    let buffer = query(|buf, needed| {
        // SAFETY: arguments are valid for the duration of the call.
        unsafe { GetPrinterDriverW(handle.raw(), PCWSTR::null(), 8, buf, needed) }.ok()
    })
    .ok()?;
    if buffer.len() < std::mem::size_of::<DRIVER_INFO_8W>() {
        return None;
    }
    // SAFETY: the buffer holds one DRIVER_INFO_8W record.
    Some(unsafe { (*buffer.as_ptr::<DRIVER_INFO_8W>()).cVersion })
}
