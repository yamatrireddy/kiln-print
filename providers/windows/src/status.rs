//! Pure mapping of Windows spooler flags to the Kiln model.
//!
//! Flag values are copied from `winspool.h` so this module has no Win32 dependency and is
//! unit-tested on every platform.

use kiln_core::error::ErrorCode;
use kiln_core::model::{ConnectionType, PrinterCondition, PrinterState};
use kiln_core::provider::ProviderJobState;

pub mod job {
    pub const PAUSED: u32 = 0x1;
    pub const ERROR: u32 = 0x2;
    pub const DELETING: u32 = 0x4;
    pub const SPOOLING: u32 = 0x8;
    pub const PRINTING: u32 = 0x10;
    pub const OFFLINE: u32 = 0x20;
    pub const PAPEROUT: u32 = 0x40;
    pub const PRINTED: u32 = 0x80;
    pub const DELETED: u32 = 0x100;
    pub const BLOCKED_DEVQ: u32 = 0x200;
    pub const USER_INTERVENTION: u32 = 0x400;
    pub const RESTART: u32 = 0x800;
    pub const COMPLETE: u32 = 0x1000;
    pub const RETAINED: u32 = 0x2000;
}

pub mod printer {
    pub const PAUSED: u32 = 0x1;
    pub const ERROR: u32 = 0x2;
    pub const PENDING_DELETION: u32 = 0x4;
    pub const PAPER_JAM: u32 = 0x8;
    pub const PAPER_OUT: u32 = 0x10;
    pub const MANUAL_FEED: u32 = 0x20;
    pub const PAPER_PROBLEM: u32 = 0x40;
    pub const OFFLINE: u32 = 0x80;
    pub const IO_ACTIVE: u32 = 0x100;
    pub const BUSY: u32 = 0x200;
    pub const PRINTING: u32 = 0x400;
    pub const OUTPUT_BIN_FULL: u32 = 0x800;
    pub const NOT_AVAILABLE: u32 = 0x1000;
    pub const PROCESSING: u32 = 0x4000;
    pub const WARMING_UP: u32 = 0x10000;
    pub const TONER_LOW: u32 = 0x20000;
    pub const NO_TONER: u32 = 0x40000;
    pub const USER_INTERVENTION: u32 = 0x100000;
    pub const OUT_OF_MEMORY: u32 = 0x200000;
    pub const DOOR_OPEN: u32 = 0x400000;
    pub const SERVER_UNKNOWN: u32 = 0x800000;
    pub const POWER_SAVE: u32 = 0x1000000;
    pub const SERVER_OFFLINE: u32 = 0x2000000;

    pub const ATTRIBUTE_SHARED: u32 = 0x8;
    pub const ATTRIBUTE_NETWORK: u32 = 0x10;
    pub const ATTRIBUTE_WORK_OFFLINE: u32 = 0x400;
}

/// Maps `JOB_INFO_1W.Status` to a provider job state.
///
/// Note that Windows removes a job from the queue shortly after it prints (unless "keep
/// printed documents" is on), so `PRINTED`/`COMPLETE` may never be observed; the engine
/// then sees the job vanish and records `SPOOLER_JOB_RETIRED` evidence instead.
pub fn job_state(flags: u32, status_text: Option<String>) -> ProviderJobState {
    use job::*;
    let printed = flags & (PRINTED | COMPLETE) != 0;
    if flags & (DELETING | DELETED) != 0 {
        return if printed {
            ProviderJobState::Printed
        } else {
            ProviderJobState::Cancelled
        };
    }
    if printed {
        return ProviderJobState::Printed;
    }
    let blocked = |condition| ProviderJobState::Blocked {
        condition,
        message: status_text.clone(),
    };
    if flags & PAPEROUT != 0 {
        return blocked(ErrorCode::PaperOut);
    }
    if flags & OFFLINE != 0 {
        return blocked(ErrorCode::PrinterOffline);
    }
    if flags & (ERROR | USER_INTERVENTION | BLOCKED_DEVQ) != 0 {
        return blocked(ErrorCode::SpoolerError);
    }
    if flags & PAUSED != 0 {
        return blocked(ErrorCode::PrinterBusy);
    }
    if flags & PRINTING != 0 {
        return ProviderJobState::Printing;
    }
    ProviderJobState::Pending
}

/// Combines the job's current status (if it is still queued) with every status bit the
/// change-notification watcher observed during its life.
pub fn resolve_job_state(
    current: Option<(u32, Option<String>)>,
    observed: u32,
) -> ProviderJobState {
    use job::*;
    let printed_bits = observed & (PRINTED | COMPLETE);
    match current {
        Some((flags, text)) => job_state(flags | printed_bits, text),
        None if printed_bits != 0 => ProviderJobState::Printed,
        None if observed & (DELETING | DELETED) != 0 => ProviderJobState::Cancelled,
        None => ProviderJobState::Gone,
    }
}

/// Human-readable flag names for queue listings.
pub fn job_flag_names(flags: u32) -> Vec<String> {
    use job::*;
    [
        (PAUSED, "PAUSED"),
        (ERROR, "ERROR"),
        (DELETING, "DELETING"),
        (SPOOLING, "SPOOLING"),
        (PRINTING, "PRINTING"),
        (OFFLINE, "OFFLINE"),
        (PAPEROUT, "PAPER_OUT"),
        (PRINTED, "PRINTED"),
        (DELETED, "DELETED"),
        (BLOCKED_DEVQ, "BLOCKED"),
        (USER_INTERVENTION, "USER_INTERVENTION"),
        (RESTART, "RESTART"),
        (COMPLETE, "SENT_TO_PRINTER"),
        (RETAINED, "RETAINED"),
    ]
    .into_iter()
    .filter(|(bit, _)| flags & bit != 0)
    .map(|(_, name)| name.to_owned())
    .collect()
}

/// Maps `PRINTER_INFO_2W.Status`/`Attributes` to state, conditions and reachability.
pub fn printer_state(status: u32, attributes: u32) -> (PrinterState, Vec<PrinterCondition>, bool) {
    use PrinterCondition as C;
    use printer::*;
    let mut conditions: Vec<C> = [
        (PAUSED, C::Paused),
        (ERROR, C::Error),
        (PENDING_DELETION, C::PendingDeletion),
        (PAPER_JAM, C::PaperJam),
        (PAPER_OUT, C::PaperOut),
        (MANUAL_FEED, C::ManualFeed),
        (PAPER_PROBLEM, C::PaperProblem),
        (OFFLINE | SERVER_OFFLINE, C::Offline),
        (BUSY, C::Busy),
        (OUTPUT_BIN_FULL, C::OutputBinFull),
        (NOT_AVAILABLE, C::NotAvailable),
        (WARMING_UP, C::WarmingUp),
        (TONER_LOW, C::TonerLow),
        (NO_TONER, C::NoToner),
        (USER_INTERVENTION, C::UserIntervention),
        (OUT_OF_MEMORY, C::OutOfMemory),
        (DOOR_OPEN, C::DoorOpen),
        (POWER_SAVE, C::PowerSave),
    ]
    .into_iter()
    .filter(|(bits, _)| status & bits != 0)
    .map(|(_, c)| c)
    .collect();
    if attributes & ATTRIBUTE_WORK_OFFLINE != 0 && !conditions.contains(&C::Offline) {
        conditions.push(C::Offline);
    }
    conditions.sort_unstable();

    let offline = conditions
        .iter()
        .any(|c| matches!(c, C::Offline | C::NotAvailable | C::PendingDeletion));
    let error = conditions.iter().any(|c| {
        matches!(
            c,
            C::Error
                | C::PaperJam
                | C::PaperOut
                | C::PaperProblem
                | C::DoorOpen
                | C::NoToner
                | C::UserIntervention
                | C::OutOfMemory
                | C::OutputBinFull
        )
    });
    let state = if offline {
        PrinterState::Offline
    } else if status & SERVER_UNKNOWN != 0 {
        PrinterState::Unknown
    } else if status & PAUSED != 0 {
        PrinterState::Paused
    } else if error {
        PrinterState::Error
    } else if status & (PRINTING | PROCESSING | BUSY | IO_ACTIVE) != 0 {
        PrinterState::Printing
    } else {
        PrinterState::Ready
    };
    (state, conditions, !offline)
}

/// Best-effort attachment type from the port name, driver and attributes.
pub fn classify_connection(
    name: &str,
    port: &str,
    driver: &str,
    attributes: u32,
) -> ConnectionType {
    let port_upper = port.trim().to_ascii_uppercase();
    let driver_upper = driver.to_ascii_uppercase();
    const VIRTUAL_PORTS: [&str; 5] = ["PORTPROMPT:", "NUL:", "FILE:", "XPSPORT:", "SHRFAX:"];
    const VIRTUAL_DRIVERS: [&str; 4] = ["PRINT TO PDF", "XPS DOCUMENT WRITER", "ONENOTE", "FAX"];
    if VIRTUAL_PORTS.contains(&port_upper.as_str())
        || VIRTUAL_DRIVERS.iter().any(|d| driver_upper.contains(d))
    {
        return ConnectionType::Virtual;
    }
    if port_upper.starts_with("USB") || port_upper.starts_with("DOT4") {
        return ConnectionType::Usb;
    }
    if port_upper.starts_with("COM") {
        return ConnectionType::Serial;
    }
    if port_upper.starts_with("LPT") {
        return ConnectionType::Local;
    }
    let looks_like_ip = port_upper
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .any(|part| {
            part.split('.').count() == 4
                && part
                    .split('.')
                    .all(|o| !o.is_empty() && o.parse::<u8>().is_ok())
        });
    if attributes & printer::ATTRIBUTE_NETWORK != 0
        || name.starts_with("\\\\")
        || port_upper.starts_with("IP_")
        || port_upper.starts_with("WSD")
        || port_upper.starts_with("HTTP")
        || looks_like_ip
    {
        return ConnectionType::Network;
    }
    ConnectionType::Local
}

/// Printer command language suggested by the driver or queue name, if recognisable.
///
/// Only a hint: clients can always name the language explicitly. Order matters: Zebra's
/// "ZDesigner … (EPL)" drivers speak EPL, the others ZPL.
pub fn guess_language(name: &str, driver: &str) -> Option<&'static str> {
    let text = format!("{name} {driver}").to_ascii_uppercase();
    let has = |needle: &str| text.contains(needle);
    if has("EPL") {
        Some("EPL")
    } else if has("ZPL") || has("ZDESIGNER") || has("ZEBRA") {
        Some("ZPL")
    } else if has("CPCL") {
        Some("CPCL")
    } else if has("TSPL") || text.starts_with("TSC ") || has(" TSC ") {
        Some("TSPL")
    } else if has("ESC/POS")
        || has("ESCPOS")
        || has("EPSON TM-")
        || has(" TM-T")
        || has("POS-58")
        || has("POS-80")
        || has("RECEIPT")
    {
        Some("ESC/POS")
    } else if has("ESC/P")
        || has("EPSON LQ")
        || has("EPSON LX")
        || has("EPSON FX")
        || has("EPSON DFX")
    {
        Some("ESC/P")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_flags() {
        assert_eq!(job_state(0, None), ProviderJobState::Pending);
        assert_eq!(job_state(job::SPOOLING, None), ProviderJobState::Pending);
        assert_eq!(job_state(job::PRINTING, None), ProviderJobState::Printing);
        assert_eq!(
            job_state(job::PRINTED | job::DELETING, None),
            ProviderJobState::Printed
        );
        assert_eq!(job_state(job::COMPLETE, None), ProviderJobState::Printed);
        assert_eq!(job_state(job::DELETING, None), ProviderJobState::Cancelled);
        assert!(matches!(
            job_state(job::PRINTING | job::PAPEROUT, None),
            ProviderJobState::Blocked {
                condition: ErrorCode::PaperOut,
                ..
            }
        ));
        assert!(matches!(
            job_state(job::ERROR | job::PRINTING, Some("Jammed".into())),
            ProviderJobState::Blocked {
                condition: ErrorCode::SpoolerError,
                message: Some(_)
            }
        ));
    }

    #[test]
    fn observed_history_disambiguates_vanished_jobs() {
        assert_eq!(resolve_job_state(None, 0), ProviderJobState::Gone);
        assert_eq!(
            resolve_job_state(None, job::PRINTING | job::PRINTED),
            ProviderJobState::Printed
        );
        assert_eq!(
            resolve_job_state(None, job::PRINTING | job::DELETING),
            ProviderJobState::Cancelled
        );
        assert_eq!(
            resolve_job_state(None, job::PRINTED | job::DELETING),
            ProviderJobState::Printed,
            "a printed job being cleaned up is not a cancellation"
        );
        assert_eq!(
            resolve_job_state(Some((job::DELETING, None)), job::COMPLETE),
            ProviderJobState::Printed
        );
        assert_eq!(
            resolve_job_state(Some((job::PRINTING, None)), 0),
            ProviderJobState::Printing
        );
    }

    #[test]
    fn printer_flags() {
        let (state, conditions, online) = printer_state(0, 0);
        assert_eq!(
            (state, conditions.len(), online),
            (PrinterState::Ready, 0, true)
        );

        let (state, conditions, online) = printer_state(printer::PAPER_OUT, 0);
        assert_eq!(state, PrinterState::Error);
        assert_eq!(conditions, vec![PrinterCondition::PaperOut]);
        assert!(online);

        let (state, _, online) = printer_state(0, printer::ATTRIBUTE_WORK_OFFLINE);
        assert_eq!((state, online), (PrinterState::Offline, false));

        let (state, ..) = printer_state(printer::PAUSED, 0);
        assert_eq!(state, PrinterState::Paused);
    }

    #[test]
    fn language_hints_from_drivers() {
        assert_eq!(
            guess_language("Dock", "ZDesigner ZD421-203dpi ZPL"),
            Some("ZPL")
        );
        assert_eq!(
            guess_language("Old Zebra", "ZDesigner LP 2844 (EPL)"),
            Some("EPL")
        );
        assert_eq!(guess_language("TSC TE210", "TSC TE210"), Some("TSPL"));
        assert_eq!(
            guess_language("Front counter", "EPSON TM-T88V Receipt"),
            Some("ESC/POS")
        );
        assert_eq!(guess_language("Invoices", "EPSON LQ-590"), Some("ESC/P"));
        assert_eq!(
            guess_language("Office", "HP Universal Printing PCL 6"),
            None
        );
        assert_eq!(guess_language("Text", "Generic / Text Only"), None);
    }

    #[test]
    fn connection_classification() {
        use ConnectionType::*;
        let c = |name, port, driver| classify_connection(name, port, driver, 0);
        assert_eq!(c("Zebra", "USB001", "ZDesigner ZD421-203dpi ZPL"), Usb);
        assert_eq!(c("Epson", "LPT1:", "Epson LQ-590"), Local);
        assert_eq!(c("Legacy", "COM3:", "Generic / Text Only"), Serial);
        assert_eq!(c("HP", "IP_192.168.1.20", "HP Universal"), Network);
        assert_eq!(c("HP", "192.168.1.20", "HP Universal"), Network);
        assert_eq!(c("HP", "WSD-1234", "HP"), Network);
        assert_eq!(c("\\\\srv\\hp", "", "HP"), Network);
        assert_eq!(
            c(
                "Microsoft Print to PDF",
                "PORTPROMPT:",
                "Microsoft Print To PDF"
            ),
            Virtual
        );
        assert_eq!(
            c(
                "OneNote (Desktop)",
                "nul:",
                "Send to Microsoft OneNote 16 Driver"
            ),
            Virtual
        );
    }
}
