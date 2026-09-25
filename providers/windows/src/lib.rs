//! Windows print-spooler provider.
//!
//! All Win32 printing calls in Kiln Print live in this crate, behind
//! [`WindowsPrintProvider`]. The core engine never sees a Win32 type.
//!
//! | Capability            | Win32 API                                                     |
//! |-----------------------|---------------------------------------------------------------|
//! | Discovery / status    | `EnumPrintersW` (level 2), `GetDefaultPrinterW`               |
//! | RAW printing          | `OpenPrinterW` → `StartDocPrinterW("RAW")` → `WritePrinter`   |
//! | Text printing         | GDI: `CreateDCW` → `StartDocW` → `TextOutW`                   |
//! | PDF printing          | `Windows.Data.Pdf` rasterisation → GDI `StretchDIBits` (banded) |
//! | Image printing        | decoded raster → GDI `StretchDIBits` (banded)                 |
//! | Page setup            | `DocumentPropertiesW` DEVMODE: paper, orientation, duplex, colour, tray |
//! | Completion tracking   | `FindFirstPrinterChangeNotification` job-status history      |
//! | Job status            | `GetJobW` (level 1)                                           |
//! | Queue inspection      | `EnumJobsW` (level 1)                                         |
//! | Cancellation          | `SetJobW(JOB_CONTROL_DELETE)`                                 |
//! | Capabilities          | `DeviceCapabilitiesW`, `EnumPrintProcessorDatatypesW`         |
//!
//! The agent must run **in the user's session**, not as a Windows service: GDI printing
//! from services is unsupported by Microsoft, and per-user printer connections are not
//! visible to service accounts. See `docs/windows-strategy.md`.
//!
//! [`layout`] and [`status`] are pure and compile on every platform so their logic is
//! covered by CI on any OS.

pub mod layout;
pub mod status;

#[cfg(windows)]
mod capabilities;
#[cfg(windows)]
mod devmode;
#[cfg(windows)]
mod discovery;
#[cfg(windows)]
mod ffi;
#[cfg(windows)]
mod gdi;
#[cfg(windows)]
mod jobs;
#[cfg(windows)]
mod pdf;
#[cfg(windows)]
mod provider;
#[cfg(windows)]
mod raster;
#[cfg(windows)]
mod raw;
#[cfg(windows)]
mod watch;

#[cfg(windows)]
pub use provider::WindowsPrintProvider;
