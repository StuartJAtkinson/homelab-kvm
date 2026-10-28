//! Stable monitor identity from EDID, in the same format Windows uses to key
//! its saved display arrangements
//! (`HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Configuration`),
//! e.g. `DELA10F8ZSZTY2_2F_07E4_B4`:
//!
//! ```text
//! <PNP id: 3-letter maker + 4 hex product><serial>_<week:02X>_<year:04X>_<checksum:02X>
//! ```
//! `serial` is the serial-number text descriptor if present, else the numeric
//! serial in decimal. Computed identically on every OS, so a monitor keeps its
//! id when it moves between machines.

#![cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]

const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edid {
    /// e.g. `DELA10F`.
    pub pnp_id: String,
    pub serial: String,
    /// Monitor name descriptor, e.g. `BenQ GL2780`, if present.
    pub model: Option<String>,
    pub week: u8,
    pub year: u16,
    pub checksum: u8,
}

impl Edid {
    pub fn parse(bytes: &[u8]) -> Option<Edid> {
        let b = bytes.get(..128)?;
        if b[..8] != HEADER {
            return None;
        }
        let mfg = u16::from_be_bytes([b[8], b[9]]);
        let letter = |shift: u16| char::from(b'A' - 1 + ((mfg >> shift) & 0x1F) as u8);
        let product = u16::from_le_bytes([b[10], b[11]]);
        let pnp_id = format!("{}{}{}{product:04X}", letter(10), letter(5), letter(0));
        let numeric_serial = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);

        let mut serial_text = None;
        let mut model = None;
        // Four 18-byte descriptors; display descriptors start with 00 00 00 <tag>.
        for d in b[54..126].chunks(18) {
            if d[..3] != [0, 0, 0] {
                continue;
            }
            let text = || {
                let raw = &d[5..18];
                let end = raw.iter().position(|&c| c == b'\n').unwrap_or(raw.len());
                Some(String::from_utf8_lossy(&raw[..end]).trim_end().to_string()).filter(|s| !s.is_empty())
            };
            match d[3] {
                0xFF => serial_text = text(),
                0xFC => model = text(),
                _ => {}
            }
        }

        Some(Edid {
            pnp_id,
            serial: serial_text.unwrap_or_else(|| numeric_serial.to_string()),
            model,
            week: b[16],
            year: 1990 + u16::from(b[17]),
            checksum: b[127],
        })
    }

    /// Windows' monitor id, e.g. `BNQ78ECETM5L0083404U_14_07E4_D4`.
    pub fn monitor_id(&self) -> String {
        format!("{}{}_{:02X}_{:04X}_{:02X}", self.pnp_id, self.serial, self.week, self.year, self.checksum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // First 128 bytes of real EDIDs, checked against the matching key names
    // in a Windows 11 GraphicsDrivers\Configuration database.
    const BENQ: &str = "00FFFFFFFFFFFF0009D1EC7801010000141E0103803C22782EBA25A35650A0280E5054A56B80D1C0B300A9C08180810081C001010101023A801871382D40582C450056502100001E000000FF0045544D354C3030383334303455000000FD00324C1E5315000A202020202020000000FC0042656E5120474C323738300A2001D4";
    const LAPTOP: &str = "00FFFFFFFFFFFF000DAED51400000000281A0104A51F117802EE95A3544C99260F505400000001010101010101010101010101010101B43B804A71383440503C680035AD10000018000000FE004E3134304843452D454E320A20000000FE00434D4E0A202020202020202020000000FE004E3134304843452D454E320A200010";
    const NO_SERIAL: &str = "00FFFFFFFFFFFF0049F7000000000000191A0103800000000AD7A5A2594A9624145054AFCE00D1C0B30081008180814081C001010101023A801871382D40582C45003F432100001A7F2156AA51001E30468F33003F432100001E000000FD00324C1E5010000A202020202020000000FC004141410A20202020202020200A017A";

    #[test]
    fn matches_windows_ids() {
        let benq = Edid::parse(&unhex(BENQ)).unwrap();
        assert_eq!(benq.monitor_id(), "BNQ78ECETM5L0083404U_14_07E4_D4");
        assert_eq!(benq.model.as_deref(), Some("BenQ GL2780"));
        assert_eq!(benq.year, 2020);

        let panel = Edid::parse(&unhex(LAPTOP)).unwrap();
        assert_eq!(panel.monitor_id(), "CMN14D50_28_07E0_10");
        assert_eq!(panel.model, None);

        assert_eq!(Edid::parse(&unhex(NO_SERIAL)).unwrap().monitor_id(), "ROW00000_19_07E0_7A");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Edid::parse(&[0; 128]), None);
        assert_eq!(Edid::parse(&unhex(BENQ)[..100]), None);
    }
}
