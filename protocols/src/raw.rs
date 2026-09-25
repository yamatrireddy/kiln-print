//! Plain RAW: opaque bytes with no language assumptions.

use kiln_core::protocol::{LanguageFamily, LanguageInfo, PrinterProtocol};

#[derive(Debug, Clone, Copy)]
pub struct Raw;

static INFO: LanguageInfo = LanguageInfo {
    id: "RAW",
    name: "Plain RAW bytes",
    family: LanguageFamily::Generic,
    aliases: &["binary", "passthrough", "none"],
};

impl PrinterProtocol for Raw {
    fn info(&self) -> &LanguageInfo {
        &INFO
    }
}
