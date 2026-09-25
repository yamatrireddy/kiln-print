//! Device status queries over the printer's own TCP port. Unlike spooler status, these
//! answers come from the printer itself (paper out, head open, paused, …).

use kiln_core::model::{PrinterCondition, PrinterState};
use serde::{Deserialize, Serialize};

/// Which status protocol, if any, a configured printer answers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StatusQuery {
    /// Do not query; reachability is learned only when printing.
    #[default]
    None,
    /// ZPL `~HS` host status.
    Zpl,
    /// ESC/POS `DLE EOT` real-time status.
    #[serde(rename = "ESC/POS", alias = "ESCPOS", alias = "ESC_POS")]
    EscPos,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStatus {
    pub state: PrinterState,
    pub conditions: Vec<PrinterCondition>,
}

impl DeviceStatus {
    pub fn from_conditions(mut conditions: Vec<PrinterCondition>) -> Self {
        conditions.sort_unstable();
        conditions.dedup();
        let state = if conditions.contains(&PrinterCondition::Offline) {
            PrinterState::Offline
        } else if conditions.iter().any(|c| {
            matches!(
                c,
                PrinterCondition::PaperOut
                    | PrinterCondition::HeadOpen
                    | PrinterCondition::RibbonOut
                    | PrinterCondition::Error
                    | PrinterCondition::DoorOpen
            )
        }) {
            PrinterState::Error
        } else if conditions.contains(&PrinterCondition::Paused) {
            PrinterState::Paused
        } else {
            PrinterState::Ready
        };
        Self { state, conditions }
    }
}

/// Parses the three STX…ETX strings a ZPL printer returns for `~HS`.
///
/// String 1: `aaa,b,c,dddd,eee,f,g,h,iii,j,k,l` — b paper out, c pause, j corrupt RAM,
/// k under-temperature, l over-temperature. String 2: `mmm,n,o,p,…` — o head up,
/// p ribbon out.
pub fn parse_zpl_host_status(response: &[u8]) -> Option<DeviceStatus> {
    let text = String::from_utf8_lossy(response);
    let blocks: Vec<Vec<&str>> = text
        .split('\u{2}')
        .filter_map(|block| block.split('\u{3}').next())
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(|b| b.split(',').map(str::trim).collect())
        .collect();
    let first = blocks.first().filter(|f| f.len() >= 12)?;
    let second = blocks.get(1).filter(|s| s.len() >= 4)?;
    let flag = |fields: &[&str], i: usize| fields.get(i).is_some_and(|v| *v == "1");
    let mut conditions = Vec::new();
    if flag(first, 1) {
        conditions.push(PrinterCondition::PaperOut);
    }
    if flag(first, 2) {
        conditions.push(PrinterCondition::Paused);
    }
    if flag(first, 9) || flag(first, 10) || flag(first, 11) {
        conditions.push(PrinterCondition::Error);
    }
    if flag(second, 2) {
        conditions.push(PrinterCondition::HeadOpen);
    }
    if flag(second, 3) {
        conditions.push(PrinterCondition::RibbonOut);
    }
    Some(DeviceStatus::from_conditions(conditions))
}

/// `DLE EOT n` requests: printer (1), offline cause (2), paper (4).
pub const ESCPOS_QUERIES: [[u8; 3]; 3] = [[0x10, 0x04, 1], [0x10, 0x04, 2], [0x10, 0x04, 4]];

/// Interprets the three one-byte answers to [`ESCPOS_QUERIES`].
pub fn parse_escpos_status(answers: [u8; 3]) -> Option<DeviceStatus> {
    // Every status byte has bit 1 and bit 4 set and bits 0 and 7 clear.
    if answers.iter().any(|b| b & 0x93 != 0x12) {
        return None;
    }
    let [printer, offline, paper] = answers;
    let mut conditions = Vec::new();
    if printer & 0x08 != 0 {
        conditions.push(PrinterCondition::Offline);
    }
    if offline & 0x04 != 0 {
        conditions.push(PrinterCondition::HeadOpen);
    }
    if offline & 0x20 != 0 || paper & 0x60 != 0 {
        conditions.push(PrinterCondition::PaperOut);
    }
    if offline & 0x40 != 0 {
        conditions.push(PrinterCondition::Error);
    }
    if paper & 0x0C != 0 {
        conditions.push(PrinterCondition::PaperLow);
    }
    Some(DeviceStatus::from_conditions(conditions))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEALTHY: &[u8] = b"\x02030,0,0,1245,000,0,0,0,000,0,0,0\x03\r\n\x02000,0,0,0,1,2,6,0,00000000,1,000\x03\r\n\x021234,0\x03\r\n";

    #[test]
    fn zpl_host_status() {
        assert_eq!(
            parse_zpl_host_status(HEALTHY).expect("parsed").state,
            PrinterState::Ready
        );
        let paper_out_paused =
            b"\x02030,1,1,1245,000,0,0,0,000,0,0,0\x03\r\n\x02000,0,1,0,1,2,6,0,00000000,1,000\x03\r\n\x021234,0\x03";
        let status = parse_zpl_host_status(paper_out_paused).expect("parsed");
        assert_eq!(status.state, PrinterState::Error);
        assert_eq!(
            status.conditions,
            vec![
                PrinterCondition::Paused,
                PrinterCondition::PaperOut,
                PrinterCondition::HeadOpen
            ]
        );
        assert!(parse_zpl_host_status(b"garbage").is_none());
    }

    #[test]
    fn escpos_status() {
        assert_eq!(
            parse_escpos_status([0x12, 0x12, 0x12]).expect("ok").state,
            PrinterState::Ready
        );
        let status = parse_escpos_status([0x1A, 0x16, 0x7E]).expect("parsed");
        assert_eq!(status.state, PrinterState::Offline);
        assert!(status.conditions.contains(&PrinterCondition::HeadOpen));
        assert!(status.conditions.contains(&PrinterCondition::PaperOut));
        assert!(status.conditions.contains(&PrinterCondition::PaperLow));
        assert!(
            parse_escpos_status([0xFF, 0x12, 0x12]).is_none(),
            "not a status byte"
        );
    }

    #[test]
    fn status_query_names() {
        let q: StatusQuery = serde_json::from_str("\"ESC/POS\"").expect("parse");
        assert_eq!(q, StatusQuery::EscPos);
        assert_eq!(
            serde_json::from_str::<StatusQuery>("\"ZPL\"").expect("parse"),
            StatusQuery::Zpl
        );
    }
}
