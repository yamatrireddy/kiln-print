//! EPL / EPL2 (Eltron and Zebra desktop label printers).

use kiln_core::protocol::{Inspection, LanguageFamily, LanguageInfo, PrinterProtocol};

use crate::bytes::{has_line_starting_with, looks_like_zpl};

#[derive(Debug, Clone, Copy)]
pub struct Epl;

static INFO: LanguageInfo = LanguageInfo {
    id: "EPL",
    name: "Eltron Programming Language (EPL2)",
    family: LanguageFamily::Label,
    aliases: &["EPL2"],
};

impl PrinterProtocol for Epl {
    fn info(&self) -> &LanguageInfo {
        &INFO
    }

    fn encode_label(
        &self,
        label: &kiln_core::model::LabelDocument,
    ) -> Option<kiln_core::error::Result<Vec<u8>>> {
        Some(crate::label::epl::encode(label))
    }

    fn inspect(&self, data: &[u8]) -> Inspection {
        let mut out = Inspection::default();
        if looks_like_zpl(data) {
            out.warn("data looks like ZPL, not EPL");
        }
        // EPL prints with `P<n>` (or `PA`); without it the label is only buffered.
        let prints = data.split(|b| *b == b'\n').any(|line| {
            let line = crate::bytes::trim_start(line);
            line.first() == Some(&b'P')
                && line
                    .get(1)
                    .is_some_and(|c| c.is_ascii_digit() || *c == b'A')
        });
        if !prints {
            out.warn("no P<n> print command found; the label will not print");
        }
        if !has_line_starting_with(data, b"N") {
            out.warn("no N (clear image buffer) command; stale buffer content may print");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_label_is_clean() {
        let label = b"\nN\nA50,0,0,1,1,1,N,\"Hello\"\nP1\n";
        assert!(Epl.inspect(label).warnings.is_empty());
    }

    #[test]
    fn missing_print_command_warns() {
        let w = Epl.inspect(b"N\nA50,0,0,1,1,1,N,\"Hello\"\n").warnings;
        assert!(w.iter().any(|w| w.contains("P<n>")));
    }
}
