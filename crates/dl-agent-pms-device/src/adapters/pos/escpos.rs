//! `EscPos.cs`: the Epson ESC/POS command bytes the POS adapters send, the pole display
//! encoding and the DLE EOT status decoding.

use dl_json::{ToJson, Value, json};

use super::failures;
use super::unicode::is_non_spacing_mark;
use crate::contracts::{AdapterError, codes};

const ESC: u8 = 0x1B;
const GS: u8 = 0x1D;
const DLE: u8 = 0x10;
const EOT: u8 = 0x04;
const FF: u8 = 0x0C;
const US: u8 = 0x1F;

/// Real-time printer status decoded from DLE EOT 1 (printer) and DLE EOT 4 (paper roll).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PosPrinterStatus {
    /// The printer is online.
    pub online: bool,
    /// The drawer kick-out connector pin 3 is high (drawer sensor).
    pub drawer_pin3_high: bool,
    /// The roll paper is out.
    pub paper_out: bool,
    /// The roll paper is near its end.
    pub paper_near_end: bool,
}

impl ToJson for PosPrinterStatus {
    fn to_json(&self) -> Value {
        json!({
            "online": self.online,
            "drawerPin3High": self.drawer_pin3_high,
            "paperOut": self.paper_out,
            "paperNearEnd": self.paper_near_end,
        })
    }
}

/// The ESC/POS commands (POSWin printed and kicked the drawer with raw ESC/POS over COM,
/// TCP 9100 or the spooler; pole displays take the ESC/POS customer-display set).
pub struct EscPos;

/// The Vietnamese letters folded to their base letter (the C# agent runs with
/// InvariantGlobalization, where `string.Normalize` does not decompose).
const VIETNAMESE_FOLD: [(&str, char); 14] = [
    ("àáảãạăằắẳẵặâầấẩẫậ", 'a'),
    ("ÀÁẢÃẠĂẰẮẲẴẶÂẦẤẨẪẬ", 'A'),
    ("èéẻẽẹêềếểễệ", 'e'),
    ("ÈÉẺẼẸÊỀẾỂỄỆ", 'E'),
    ("ìíỉĩị", 'i'),
    ("ÌÍỈĨỊ", 'I'),
    ("òóỏõọôồốổỗộơờớởỡợ", 'o'),
    ("ÒÓỎÕỌÔỒỐỔỖỘƠỜỚỞỠỢ", 'O'),
    ("ùúủũụưừứửữự", 'u'),
    ("ÙÚỦŨỤƯỪỨỬỮỰ", 'U'),
    ("ỳýỷỹỵ", 'y'),
    ("ỲÝỶỸỴ", 'Y'),
    ("đ", 'd'),
    ("Đ", 'D'),
];

impl EscPos {
    /// GS V 66 0: feed to the cutter and partial-cut.
    pub const CUT: [u8; 4] = [GS, 0x56, 0x42, 0x00];

    /// DLE EOT 1: printer status (online, drawer kick-out connector pin 3).
    pub const PRINTER_STATUS_QUERY: [u8; 3] = [DLE, EOT, 0x01];

    /// DLE EOT 4: roll paper sensor status.
    pub const PAPER_STATUS_QUERY: [u8; 3] = [DLE, EOT, 0x04];

    /// ESC p m t1 t2: pulse the drawer kick-out connector. m = 0 for pin 2, 1 for pin 5;
    /// t1/t2 are the on/off times in 2 ms units (2..510 ms).
    pub fn kick(pin: i32, on_ms: i32, off_ms: i32) -> Result<[u8; 5], AdapterError> {
        let m = match pin {
            2 => 0,
            5 => 1,
            _ => {
                return Err(failures::bad_payload(format!(
                    "drawer pin must be 2 or 5, got {pin}"
                )));
            }
        };
        Ok([
            ESC,
            b'p',
            m,
            pulse_units(on_ms, "onMs")?,
            pulse_units(off_ms, "offMs")?,
        ])
    }

    /// ESC @ (initialise), FF (clear), then each line at row i via US $ 1 i, ASCII-folded and
    /// padded or cut to `columns`. A `None` list or line is the C# NullReferenceException.
    pub fn display(
        lines: Option<&[Option<String>]>,
        columns: i32,
    ) -> Result<Vec<u8>, AdapterError> {
        if !(1..=40).contains(&columns) {
            return Err(failures::bad_payload(format!(
                "display columns must be 1-40, got {columns}"
            )));
        }
        let lines = lines.ok_or_else(failures::null_reference)?;
        if !(1..=4).contains(&lines.len()) {
            return Err(failures::bad_payload(format!(
                "a display takes 1-4 lines, got {}",
                lines.len()
            )));
        }
        let mut bytes = vec![ESC, b'@', FF];
        for (i, line) in lines.iter().enumerate() {
            let line = line.as_deref().ok_or_else(failures::null_reference)?;
            // At most 4 lines: the row always fits a byte.
            bytes.extend_from_slice(&[US, b'$', 1, (i + 1) as u8]);
            bytes.extend_from_slice(Self::fit_line(line, columns).as_bytes());
        }
        Ok(bytes)
    }

    /// The text a pole display shows for one line: ASCII-folded, cut or padded to the width.
    pub fn fit_line(line: &str, columns: i32) -> String {
        let width = usize::try_from(columns).unwrap_or(0);
        let mut ascii = Self::to_ascii(line);
        if ascii.len() >= width {
            ascii.truncate(width);
        } else {
            let pad = width - ascii.len();
            ascii.extend(std::iter::repeat_n(' ', pad));
        }
        ascii
    }

    /// Folds Vietnamese letters to their base letter and drops loose combining marks; any
    /// other character outside printable ASCII becomes `'?'` (one per UTF-16 unit, as C#).
    pub fn to_ascii(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut units = [0u16; 2];
        for ch in text.chars() {
            let encoded = ch.encode_utf16(&mut units);
            if encoded.len() == 1 && is_non_spacing_mark(encoded[0]) {
                continue;
            }
            if let Some(folded) = fold(ch) {
                out.push(folded);
            } else if (' '..='~').contains(&ch) {
                out.push(ch);
            } else {
                // A supplementary character is two UTF-16 units, each a '?' in C#.
                for _ in 0..encoded.len() {
                    out.push('?');
                }
            }
        }
        out
    }

    /// Decodes the two DLE EOT replies; a reply that is not a status byte is DEVICE_ERROR.
    pub fn parse_status(printer: u8, paper: u8) -> Result<PosPrinterStatus, AdapterError> {
        // Fixed bits per ESC/POS: bit 0 = 0, bit 1 = 1, bit 4 = 1, bit 7 = 0.
        if (printer & 0x93) != 0x12 || (paper & 0x93) != 0x12 {
            return Err(AdapterError::new(
                codes::ERROR,
                format!(
                    "printer answered DLE EOT with 0x{printer:02X} 0x{paper:02X}, not a status byte"
                ),
            ));
        }
        Ok(PosPrinterStatus {
            online: (printer & 0x08) == 0,
            drawer_pin3_high: (printer & 0x04) != 0,
            paper_out: (paper & 0x60) != 0,
            paper_near_end: (paper & 0x0C) != 0,
        })
    }
}

fn fold(ch: char) -> Option<char> {
    VIETNAMESE_FOLD
        .iter()
        .find(|(letters, _)| letters.contains(ch))
        .map(|&(_, base)| base)
}

fn pulse_units(ms: i32, name: &str) -> Result<u8, AdapterError> {
    if (2..=510).contains(&ms) {
        // 2..=510 / 2 is 1..=255.
        Ok((ms / 2) as u8)
    } else {
        Err(failures::bad_payload(format!(
            "{name} must be 2-510 ms, got {ms}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_ascii_units() {
        assert_eq!(EscPos::to_ascii("a\u{0301}b"), "ab");
        assert_eq!(EscPos::to_ascii("x😀y"), "x??y");
        assert_eq!(EscPos::to_ascii("tab\there"), "tab?here");
        assert_eq!(EscPos::fit_line("abc", 5), "abc  ");
        assert_eq!(EscPos::fit_line("abcdef", 3), "abc");
    }
}
