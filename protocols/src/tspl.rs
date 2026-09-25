//! TSPL / TSPL2 (TSC and many compatible label printers).

use kiln_core::protocol::{Inspection, LanguageFamily, LanguageInfo, PrinterProtocol};

use crate::bytes::{has_line_starting_with, looks_like_zpl};

#[derive(Debug, Clone, Copy)]
pub struct Tspl;

static INFO: LanguageInfo = LanguageInfo {
    id: "TSPL",
    name: "TSC Printer Language (TSPL/TSPL2)",
    family: LanguageFamily::Label,
    aliases: &["TSPL2"],
};

impl PrinterProtocol for Tspl {
    fn info(&self) -> &LanguageInfo {
        &INFO
    }

    fn encode_label(
        &self,
        label: &kiln_core::model::LabelDocument,
    ) -> Option<kiln_core::error::Result<Vec<u8>>> {
        Some(crate::label::tspl::encode(label))
    }

    fn inspect(&self, data: &[u8]) -> Inspection {
        let mut out = Inspection::default();
        if looks_like_zpl(data) {
            out.warn("data looks like ZPL, not TSPL");
        }
        if !has_line_starting_with(data, b"PRINT") {
            out.warn("no PRINT command found; the label will not print");
        }
        if !has_line_starting_with(data, b"CLS") {
            out.warn("no CLS command; stale image buffer content may print");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_label_is_clean() {
        let label = b"SIZE 60 mm,40 mm\r\nGAP 2 mm,0\r\nCLS\r\nTEXT 10,10,\"3\",0,1,1,\"Hi\"\r\nPRINT 1\r\n";
        assert!(Tspl.inspect(label).warnings.is_empty());
    }
}
