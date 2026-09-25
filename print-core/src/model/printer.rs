//! Printer, status and capability model.
//!
//! Every capability field is optional: `None` means "the driver/OS did not tell us", which
//! is different from `Some(false)`. Clients must not assume a printer exposes everything.

use serde::{Deserialize, Serialize};
use std::fmt;

use super::DocumentType;

/// Stable printer identifier.
///
/// Derived deterministically from the owning provider and the native printer name so it
/// survives agent restarts without a registry, and so it is safe to embed in URLs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrinterId(pub String);

impl PrinterId {
    pub fn derive(provider: &str, native_name: &str) -> Self {
        // FNV-1a 64: stable across releases and platforms (unlike `DefaultHasher`).
        // It is an identifier, not a security boundary.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in provider.bytes().chain([0u8]).chain(native_name.bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self(format!("{provider}-{hash:016x}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PrinterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for PrinterId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// How the printer is attached. `Virtual` covers software printers (PDF/XPS writers,
/// OneNote, …) that never produce paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConnectionType {
    Local,
    Network,
    Usb,
    Serial,
    Virtual,
}

/// Coarse printer state for dashboards. Detailed reasons live in [`PrinterCondition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrinterState {
    Ready,
    Printing,
    Paused,
    Offline,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrinterCondition {
    Paused,
    Error,
    Offline,
    NotAvailable,
    PendingDeletion,
    PaperOut,
    PaperJam,
    PaperProblem,
    ManualFeed,
    DoorOpen,
    OutputBinFull,
    TonerLow,
    NoToner,
    OutOfMemory,
    UserIntervention,
    Busy,
    WarmingUp,
    PowerSave,
    /// Paper roll nearly empty (receipt printers).
    PaperLow,
    /// Print head or cover open (label/receipt printers).
    HeadOpen,
    RibbonOut,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Printer {
    pub id: PrinterId,
    /// Native name used to address the device through its provider.
    pub name: String,
    pub display_name: String,
    /// Id of the [`crate::provider::PrintProvider`] that owns this printer.
    pub provider: String,
    #[serde(rename = "type")]
    pub connection: ConnectionType,
    pub driver: Option<String>,
    pub port: Option<String>,
    pub location: Option<String>,
    pub default: bool,
    /// Best-effort reachability. Many drivers (notably USB on Windows) report "online"
    /// even when unplugged; see `docs/windows-strategy.md`.
    pub online: bool,
    pub status: PrinterState,
    pub conditions: Vec<PrinterCondition>,
    /// Number of jobs in the OS queue, if the provider reports it.
    pub queued_jobs: Option<u32>,
    /// Command language the printer is known or configured to understand (`ZPL`,
    /// `ESC/POS`, …). A hint used when a document does not name its language.
    #[serde(default)]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<PrinterCapabilities>,
}

impl Printer {
    /// True when anything a client would care about changed between two snapshots.
    pub fn status_differs(&self, other: &Self) -> bool {
        self.online != other.online
            || self.status != other.status
            || self.conditions != other.conditions
            || self.default != other.default
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperSize {
    /// Provider-native identifier (e.g. the Windows `DMPAPER_*` value).
    pub id: String,
    pub name: String,
    pub width_mm: f64,
    pub height_mm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub x_dpi: u32,
    pub y_dpi: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tray {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrinterCapabilities {
    pub paper_sizes: Option<Vec<PaperSize>>,
    pub color: Option<bool>,
    pub duplex: Option<bool>,
    pub resolutions: Option<Vec<Resolution>>,
    pub max_copies: Option<u32>,
    pub collate: Option<bool>,
    pub orientations: Option<Vec<Orientation>>,
    pub trays: Option<Vec<Tray>>,
    /// Whether the print path accepts byte-for-byte RAW data.
    pub raw: Option<bool>,
    /// Spooler datatypes accepted by the print processor (Windows: `RAW`, `TEXT`, `NT EMF 1.008`, …).
    pub datatypes: Option<Vec<String>>,
    /// Document types the agent can currently deliver to this printer.
    pub document_types: Vec<DocumentType>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_ids_are_stable_and_url_safe() {
        let a = PrinterId::derive("windows", "Zebra ZD421 (ZPL)");
        let b = PrinterId::derive("windows", "Zebra ZD421 (ZPL)");
        assert_eq!(a, b);
        assert!(
            a.as_str()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        );
        // Golden value: changing the hash would orphan every stored job's printer id.
        assert_eq!(
            PrinterId::derive("mock", "A").as_str(),
            "mock-fcf78c5cd7d5e290"
        );
    }

    #[test]
    fn provider_is_part_of_identity() {
        assert_ne!(
            PrinterId::derive("windows", "P"),
            PrinterId::derive("tcp", "P")
        );
    }
}
