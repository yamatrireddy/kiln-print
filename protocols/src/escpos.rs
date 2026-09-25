//! ESC/POS (Epson and compatible receipt/POS printers).

use kiln_core::protocol::{Inspection, LanguageFamily, LanguageInfo, PrinterProtocol};

use crate::bytes::looks_like_zpl;

#[derive(Debug, Clone, Copy)]
pub struct EscPos;

static INFO: LanguageInfo = LanguageInfo {
    id: "ESC/POS",
    name: "Epson ESC/POS",
    family: LanguageFamily::Receipt,
    aliases: &["ESCPOS", "ESC_POS", "POS"],
};

impl PrinterProtocol for EscPos {
    fn info(&self) -> &LanguageInfo {
        &INFO
    }

    fn inspect(&self, data: &[u8]) -> Inspection {
        let mut out = Inspection::default();
        if looks_like_zpl(data) {
            out.warn("data looks like ZPL, not ESC/POS");
        }
        out
    }
}
