//! ZPL / ZPL II (Zebra and compatible label printers).

use kiln_core::protocol::{Inspection, LanguageFamily, LanguageInfo, PrinterProtocol};

use crate::bytes::{contains, count, trim_start};

#[derive(Debug, Clone, Copy)]
pub struct Zpl;

static INFO: LanguageInfo = LanguageInfo {
    id: "ZPL",
    name: "Zebra Programming Language (ZPL II)",
    family: LanguageFamily::Label,
    aliases: &["ZPL2", "ZPL II", "ZPLII"],
};

impl PrinterProtocol for Zpl {
    fn info(&self) -> &LanguageInfo {
        &INFO
    }

    fn encode_label(
        &self,
        label: &kiln_core::model::LabelDocument,
    ) -> Option<kiln_core::error::Result<Vec<u8>>> {
        Some(crate::label::zpl::encode(label))
    }

    fn inspect(&self, data: &[u8]) -> Inspection {
        let mut out = Inspection::default();
        let starts = count(data, b"^XA");
        let ends = count(data, b"^XZ");
        // Host commands (~HS, ~JA, …) are valid without a ^XA format.
        let host_only = starts == 0 && trim_start(data).first() == Some(&b'~');
        if starts == 0 && !host_only {
            out.warn("no ^XA format start found; is this really ZPL?");
        }
        if starts != ends {
            out.warn(format!(
                "{starts} ^XA format start(s) but {ends} ^XZ end(s); a label may be truncated"
            ));
        }
        if contains(data, b"^CC") || contains(data, b"~CC") {
            out.warn("data changes the caret prefix (^CC); format checks may be inaccurate");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_label_has_no_warnings() {
        let label = b"^XA^FO50,50^A0N,50,50^FDHello^FS^XZ";
        assert!(Zpl.inspect(label).warnings.is_empty());
    }

    #[test]
    fn host_status_query_is_accepted() {
        assert!(Zpl.inspect(b"~HS").warnings.is_empty());
    }

    #[test]
    fn truncated_label_warns() {
        let w = Zpl.inspect(b"^XA^FO50,50^FDHello^FS").warnings;
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("truncated"));
    }

    #[test]
    fn non_zpl_warns() {
        assert!(!Zpl.inspect(b"\x1b@Hello\n").warnings.is_empty());
    }
}
