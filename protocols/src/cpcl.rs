//! CPCL (Comtec/Zebra mobile printers).

use kiln_core::protocol::{Inspection, LanguageFamily, LanguageInfo, PrinterProtocol};

use crate::bytes::{has_line_starting_with, looks_like_zpl, trim_start};

#[derive(Debug, Clone, Copy)]
pub struct Cpcl;

static INFO: LanguageInfo = LanguageInfo {
    id: "CPCL",
    name: "Comtec Printer Control Language",
    family: LanguageFamily::Label,
    aliases: &[],
};

impl PrinterProtocol for Cpcl {
    fn info(&self) -> &LanguageInfo {
        &INFO
    }

    fn inspect(&self, data: &[u8]) -> Inspection {
        let mut out = Inspection::default();
        if looks_like_zpl(data) {
            out.warn("data looks like ZPL, not CPCL");
        }
        // A label session starts with `! <offset> <hres> <vres> <height> <qty>`; line-print
        // and utility sessions (`! U1 …`) also start with `!`.
        if trim_start(data).first() != Some(&b'!') {
            out.warn("CPCL data normally starts with a '!' session header");
        }
        let is_utility = has_line_starting_with(data, b"! U");
        if !is_utility && !has_line_starting_with(data, b"PRINT") {
            out.warn("no PRINT command found; the label will not print");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_label_is_clean() {
        let label = b"! 0 200 200 210 1\r\nTEXT 4 0 30 40 Hello\r\nFORM\r\nPRINT\r\n";
        assert!(Cpcl.inspect(label).warnings.is_empty());
    }

    #[test]
    fn utility_session_does_not_need_print() {
        assert!(
            Cpcl.inspect(b"! U1 getvar \"device.languages\"\r\n")
                .warnings
                .is_empty()
        );
    }
}
