//! ESC/P and ESC/P2 (Epson and compatible dot-matrix printers).
//!
//! Dot-matrix output is always sent as RAW; it is never rasterised or converted to PDF.
//! Phase 3 adds a command builder (CPI/LPI, condensed, bold, form length) on top of this.

use kiln_core::protocol::{Inspection, LanguageFamily, LanguageInfo, PrinterProtocol};

use crate::bytes::looks_like_zpl;

#[derive(Debug, Clone, Copy)]
pub struct EscP;

static INFO: LanguageInfo = LanguageInfo {
    id: "ESC/P",
    name: "Epson ESC/P and ESC/P2",
    family: LanguageFamily::DotMatrix,
    aliases: &["ESCP", "ESC_P", "ESC/P2", "ESCP2"],
};

impl PrinterProtocol for EscP {
    fn info(&self) -> &LanguageInfo {
        &INFO
    }

    fn inspect(&self, data: &[u8]) -> Inspection {
        let mut out = Inspection::default();
        if looks_like_zpl(data) {
            out.warn("data looks like ZPL, not ESC/P");
        }
        out
    }
}
