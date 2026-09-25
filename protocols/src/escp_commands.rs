//! ESC/P and ESC/P2 command builders (Epson LQ/LX/FX/DFX and compatible dot-matrix
//! printers).

const ESC: u8 = 0x1B;

pub const INITIALIZE: [u8; 2] = [ESC, b'@'];
pub const FORM_FEED: u8 = 0x0C;
pub const CR_LF: [u8; 2] = [0x0D, 0x0A];
/// `SI`: condensed on. `DC2`: condensed off.
pub const CONDENSED_ON: u8 = 0x0F;
pub const CONDENSED_OFF: u8 = 0x12;

/// Pitch commands: 10 cpi `ESC P`, 12 cpi `ESC M`, 15 cpi `ESC g`, 17 cpi (condensed
/// pica) and 20 cpi (condensed elite).
pub fn pitch(cpi: u8) -> Vec<u8> {
    match cpi {
        12 => vec![ESC, b'M', CONDENSED_OFF],
        15 => vec![ESC, b'g', CONDENSED_OFF],
        17 => vec![ESC, b'P', CONDENSED_ON],
        20 => vec![ESC, b'M', CONDENSED_ON],
        _ => vec![ESC, b'P', CONDENSED_OFF],
    }
}

/// Line spacing: `ESC 2` (1/6"), `ESC 0` (1/8"), otherwise `ESC 3 n` with n/180" (24-pin)
/// or n/216" (9-pin) units.
pub fn line_spacing(lpi: f32, pins: u8) -> Vec<u8> {
    if (lpi - 6.0).abs() < f32::EPSILON {
        return vec![ESC, b'2'];
    }
    if (lpi - 8.0).abs() < f32::EPSILON {
        return vec![ESC, b'0'];
    }
    let unit = if pins == 9 { 216.0 } else { 180.0 };
    vec![ESC, b'3', (unit / lpi).round().clamp(1.0, 255.0) as u8]
}

/// `ESC x`: draft (0) or near letter quality (1).
pub fn quality(nlq: bool) -> [u8; 3] {
    [ESC, b'x', u8::from(nlq)]
}

/// `ESC C n`: page length in lines (uses the current line spacing).
pub fn form_length_lines(lines: u8) -> [u8; 3] {
    [ESC, b'C', lines]
}

/// `ESC C 0 n`: page length in inches.
pub fn form_length_inches(inches: u8) -> [u8; 4] {
    [ESC, b'C', 0, inches]
}

/// `ESC N n`: skip `n` lines over the perforation on continuous paper.
pub fn skip_perforation(lines: u8) -> [u8; 3] {
    [ESC, b'N', lines]
}

/// `ESC l n` / `ESC Q n`: left and right margins in columns.
pub fn left_margin(columns: u8) -> [u8; 3] {
    [ESC, b'l', columns]
}

pub fn right_margin(columns: u8) -> [u8; 3] {
    [ESC, b'Q', columns]
}

/// `ESC t n`: character table (1 = the PC437 graphics table).
pub fn character_table(n: u8) -> [u8; 3] {
    [ESC, b't', n]
}

/// Default character table for an encoding, when the printer has one built in.
pub fn character_table_for(canonical_encoding: &str) -> Option<u8> {
    match canonical_encoding {
        "IBM437" => Some(1),
        _ => None,
    }
}

pub fn bold(on: bool) -> [u8; 2] {
    [ESC, if on { b'E' } else { b'F' }]
}

pub fn double_strike(on: bool) -> [u8; 2] {
    [ESC, if on { b'G' } else { b'H' }]
}

pub fn italic(on: bool) -> [u8; 2] {
    [ESC, if on { b'4' } else { b'5' }]
}

pub fn underline(on: bool) -> [u8; 3] {
    [ESC, b'-', u8::from(on)]
}

pub fn double_width(on: bool) -> [u8; 3] {
    [ESC, b'W', u8::from(on)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_commands() {
        assert_eq!(pitch(10), [ESC, b'P', 0x12]);
        assert_eq!(pitch(17), [ESC, b'P', 0x0F]);
        assert_eq!(line_spacing(6.0, 24), [ESC, b'2']);
        assert_eq!(line_spacing(8.0, 9), [ESC, b'0']);
        assert_eq!(line_spacing(4.0, 24), [ESC, b'3', 45]);
        assert_eq!(line_spacing(4.0, 9), [ESC, b'3', 54]);
        assert_eq!(form_length_inches(11), [ESC, b'C', 0, 11]);
        assert_eq!(bold(false), [ESC, b'F']);
    }
}
