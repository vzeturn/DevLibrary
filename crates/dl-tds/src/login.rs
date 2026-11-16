//! LOGIN7 message.

use crate::wire::{to_utf16, utf16_len};

/// TDS 7.4 (SQL Server 2012 and later).
pub(crate) const TDS_74: u32 = 0x7400_0004;
/// TDS 8.0 (strict encryption).
pub(crate) const TDS_80: u32 = 0x0800_0000;

// OptionFlags1
const F_USE_DB: u8 = 0x20;
const F_SET_LANG: u8 = 0x80;
// OptionFlags2
const F_ODBC: u8 = 0x02;
const F_INT_SECURITY: u8 = 0x80;
// OptionFlags3
const F_CHANGE_PASSWORD: u8 = 0x01;
const F_EXTENSION: u8 = 0x10;
// TypeFlags
const F_READ_ONLY_INTENT: u8 = 0x20;

/// Size of the fixed LOGIN7 header.
const HEADER_LEN: usize = 94;

/// Everything that goes into a LOGIN7 message. Defaults follow go-mssqldb: `fUseDB`,
/// `fSetLang` and `fODBC` (unlimited TEXTSIZE, ANSI defaults), no time zone, LCID 0.
#[derive(Debug, Clone, Default)]
pub(crate) struct Login {
    pub(crate) tds_version: u32,
    pub(crate) packet_size: u32,
    pub(crate) client_prog_ver: u32,
    pub(crate) client_pid: u32,
    pub(crate) read_only_intent: bool,
    pub(crate) host_name: String,
    pub(crate) user_name: String,
    pub(crate) password: String,
    pub(crate) app_name: String,
    pub(crate) server_name: String,
    pub(crate) library_name: String,
    pub(crate) language: String,
    pub(crate) database: String,
    pub(crate) client_id: [u8; 6],
    /// First SSPI token for Windows integrated authentication. When non-empty,
    /// `fIntSecurity` is set and user name / password are not sent. Tokens of 65535 bytes
    /// or more put `0xFFFF` in `cbSSPI` and the real length in `cbSSPILong`.
    pub(crate) sspi: Vec<u8>,
    pub(crate) change_password: String,
    /// Raw FEATUREEXT block (features + 0xFF terminator); empty = no extension.
    pub(crate) feature_ext: Vec<u8>,
}

/// LOGIN7 password obfuscation: UTF-16LE, then each byte has its nibbles swapped and is
/// XORed with 0xA5.
pub(crate) fn mangle_password(password: &str) -> Vec<u8> {
    let mut b = to_utf16(password);
    for x in &mut b {
        *x = x.rotate_left(4) ^ 0xA5;
    }
    b
}

impl Login {
    /// Encodes the LOGIN7 payload (without packet headers).
    pub(crate) fn encode(&self) -> Vec<u8> {
        let integrated = !self.sspi.is_empty();
        let (user, password) = if integrated {
            ("", Vec::new())
        } else {
            (self.user_name.as_str(), mangle_password(&self.password))
        };
        let strings: [(Vec<u8>, usize); 8] = [
            (to_utf16(&self.host_name), utf16_len(&self.host_name)),
            (to_utf16(user), utf16_len(user)),
            (
                password,
                if integrated {
                    0
                } else {
                    utf16_len(&self.password)
                },
            ),
            (to_utf16(&self.app_name), utf16_len(&self.app_name)),
            (to_utf16(&self.server_name), utf16_len(&self.server_name)),
            (to_utf16(&self.library_name), utf16_len(&self.library_name)),
            (to_utf16(&self.language), utf16_len(&self.language)),
            (to_utf16(&self.database), utf16_len(&self.database)),
        ];
        let change_pw = mangle_password(&self.change_password);

        let mut flags3 = 0u8;
        if !self.feature_ext.is_empty() {
            flags3 |= F_EXTENSION;
        }
        if !change_pw.is_empty() {
            flags3 |= F_CHANGE_PASSWORD;
        }
        let mut flags2 = F_ODBC;
        if integrated {
            flags2 |= F_INT_SECURITY;
        }

        // Offsets and lengths, in the order of the header fields.
        let mut offset = HEADER_LEN;
        let mut slots = Vec::with_capacity(8);
        for (bytes, chars) in &strings {
            slots.push((offset as u16, *chars as u16));
            offset += bytes.len();
        }
        let sspi_off = offset;
        offset += self.sspi.len();
        let atch_off = offset;
        let chpw_off = offset;
        offset += change_pw.len();
        let (ext_off, ext_len, feat_off) = if self.feature_ext.is_empty() {
            (0usize, 0u16, 0usize)
        } else {
            let o = offset;
            offset += 4;
            (o, 4u16, offset)
        };
        let total = offset + self.feature_ext.len();

        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&self.tds_version.to_le_bytes());
        out.extend_from_slice(&self.packet_size.to_le_bytes());
        out.extend_from_slice(&self.client_prog_ver.to_le_bytes());
        out.extend_from_slice(&self.client_pid.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // ConnectionID
        out.push(F_USE_DB | F_SET_LANG);
        out.push(flags2);
        out.push(if self.read_only_intent {
            F_READ_ONLY_INTENT
        } else {
            0
        });
        out.push(flags3);
        out.extend_from_slice(&0i32.to_le_bytes()); // ClientTimeZone
        out.extend_from_slice(&0u32.to_le_bytes()); // ClientLCID
        let put = |out: &mut Vec<u8>, off: usize, len: u16| {
            out.extend_from_slice(&(off as u16).to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
        };
        for (i, (off, len)) in slots.iter().enumerate() {
            if i == 5 {
                // ibExtension / cbExtension sit between ServerName and CltIntName.
                put(&mut out, ext_off, ext_len);
            }
            put(&mut out, *off as usize, *len);
        }
        out.extend_from_slice(&self.client_id);
        put(&mut out, sspi_off, self.sspi.len().min(0xFFFF) as u16);
        put(&mut out, atch_off, 0);
        put(&mut out, chpw_off, utf16_len(&self.change_password) as u16);
        let long = if self.sspi.len() >= 0xFFFF {
            self.sspi.len() as u32
        } else {
            0
        };
        out.extend_from_slice(&long.to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_LEN);

        for (bytes, _) in &strings {
            out.extend_from_slice(bytes);
        }
        out.extend_from_slice(&self.sspi);
        out.extend_from_slice(&change_pw);
        if !self.feature_ext.is_empty() {
            out.extend_from_slice(&(feat_off as u32).to_le_bytes());
            out.extend_from_slice(&self.feature_ext);
        }
        out
    }
}

/// FEATUREEXT block with only the terminator would be pointless; this builds one with the
/// given `(feature id, data)` entries. Not used by default (go-mssqldb sends none either).
#[cfg(test)]
pub(crate) fn feature_ext(features: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (id, data) in features {
        out.push(*id);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out.push(0xFF);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn password_obfuscation_vectors() {
        // "a" = 61 00 -> 16^A5, 00^A5.
        assert_eq!(mangle_password("a"), [0xB3, 0xA5]);
        // "x" = 78 00 -> 87^A5 = 22.
        assert_eq!(mangle_password("x"), [0x22, 0xA5]);
        // The password field go-mssqldb sent for "S3crét!" (non-ASCII é = E9 00).
        assert_eq!(
            mangle_password("S3cr\u{e9}t!"),
            [
                0x90, 0xA5, 0x96, 0xA5, 0x93, 0xA5, 0x82, 0xA5, 0x3B, 0xA5, 0xE2, 0xA5, 0xB7, 0xA5
            ]
        );
        assert!(mangle_password("").is_empty());
    }

    /// Byte-for-byte equal to the LOGIN7 packet go-mssqldb v1.10.0 sent to a fake server
    /// (testdata/go_mssqldb_capture.txt, second line), with the same inputs.
    #[test]
    fn matches_go_mssqldb_capture() {
        let capture = include_str!("../testdata/go_mssqldb_capture.txt");
        let line = capture.lines().find(|l| l.starts_with("16 ")).unwrap();
        let go = hex(&line[3..]);
        let pid = u32::from_le_bytes([go[8 + 16], go[8 + 17], go[8 + 18], go[8 + 19]]);
        let mut client_id = [0u8; 6];
        client_id.copy_from_slice(&go[8 + 72..8 + 78]);
        let login = Login {
            tds_version: TDS_74,
            packet_size: 4096,
            client_prog_ver: 0x010A_0000,
            client_pid: pid,
            host_name: "HOST1".into(),
            user_name: "smile".into(),
            password: "S3cr\u{e9}t!".into(),
            app_name: "agent".into(),
            server_name: "127.0.0.1".into(),
            library_name: "go-mssqldb".into(),
            database: "SMILE_FO".into(),
            client_id,
            ..Login::default()
        };
        let mut ours = Vec::new();
        crate::packet::frame(crate::packet::LOGIN7, &login.encode(), 4096, &mut ours);
        assert_eq!(ours, go);
    }

    #[test]
    fn feature_ext_and_change_password_layout() {
        let login = Login {
            tds_version: TDS_74,
            packet_size: 4096,
            user_name: "u".into(),
            password: "p".into(),
            change_password: "q".into(),
            feature_ext: feature_ext(&[(0x0A, &[0x01])]),
            ..Login::default()
        };
        let b = login.encode();
        assert_eq!(
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize,
            b.len()
        );
        assert_eq!(b[27], F_EXTENSION | F_CHANGE_PASSWORD);
        let ext_off = u16::from_le_bytes([b[56], b[57]]) as usize;
        assert_eq!(u16::from_le_bytes([b[58], b[59]]), 4);
        let feat = u32::from_le_bytes([b[ext_off], b[ext_off + 1], b[ext_off + 2], b[ext_off + 3]])
            as usize;
        assert_eq!(&b[feat..], &[0x0A, 1, 0, 0, 0, 1, 0xFF]);
        let chpw_off = u16::from_le_bytes([b[86], b[87]]) as usize;
        assert_eq!(u16::from_le_bytes([b[88], b[89]]), 1);
        assert_eq!(&b[chpw_off..chpw_off + 2], &mangle_password("q")[..]);
    }

    #[test]
    fn integrated_auth_slot() {
        let login = Login {
            tds_version: TDS_74,
            user_name: "ignored".into(),
            password: "ignored".into(),
            sspi: vec![1, 2, 3],
            ..Login::default()
        };
        let b = login.encode();
        assert_eq!(b[25] & F_INT_SECURITY, F_INT_SECURITY);
        assert_eq!(u16::from_le_bytes([b[42], b[43]]), 0); // user name length
        assert_eq!(u16::from_le_bytes([b[46], b[47]]), 0); // password length
        let off = u16::from_le_bytes([b[78], b[79]]) as usize;
        assert_eq!(u16::from_le_bytes([b[80], b[81]]), 3);
        assert_eq!(&b[off..off + 3], &[1, 2, 3]);
    }

    fn le16(b: &[u8], at: usize) -> usize {
        u16::from_le_bytes([b[at], b[at + 1]]) as usize
    }

    fn le32(b: &[u8], at: usize) -> usize {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
    }

    #[test]
    fn sspi_layout() {
        let token: Vec<u8> = b"NTLMSSP\0\x01\0\0\0"
            .iter()
            .copied()
            .cycle()
            .take(40)
            .collect();
        let login = Login {
            tds_version: TDS_74,
            packet_size: 4096,
            host_name: "PC1".into(),
            app_name: "agent".into(),
            server_name: "localhost".into(),
            library_name: "dl-tds".into(),
            database: "master".into(),
            sspi: token.clone(),
            ..Login::default()
        };
        let b = login.encode();
        assert_eq!(le32(&b, 0), b.len());
        assert_eq!(b[25], F_ODBC | F_INT_SECURITY);
        assert_eq!(b[27], 0); // no FEATUREEXT, no password change
        // No user name, no password: zero lengths.
        assert_eq!((le16(&b, 42), le16(&b, 46)), (0, 0));
        // SSPI data right after the last string (database), at the advertised offset.
        let db_end = le16(&b, 68) + 2 * le16(&b, 70);
        assert_eq!(le16(&b, 78), db_end);
        assert_eq!(le16(&b, 80), token.len());
        assert_eq!(le32(&b, 90), 0); // cbSSPILong unused
        assert_eq!(&b[db_end..db_end + token.len()], &token[..]);
        // AttachDB and ChangePassword are empty and start after the token.
        assert_eq!(le16(&b, 82), db_end + token.len());
        assert_eq!((le16(&b, 84), le16(&b, 88)), (0, 0));
        assert_eq!(b.len(), db_end + token.len());
        // The password is not sent even when set.
        let with_pw = Login {
            user_name: "sa".into(),
            password: "secret".into(),
            ..login.clone()
        };
        assert_eq!(with_pw.encode(), b);
    }

    #[test]
    fn sspi_long_length() {
        for (n, short, long) in [
            (65534usize, 65534usize, 0usize),
            (65535, 0xFFFF, 65535),
            (70_000, 0xFFFF, 70_000),
        ] {
            let token: Vec<u8> = (0..n).map(|i| i as u8).collect();
            let login = Login {
                tds_version: TDS_74,
                host_name: "h".into(),
                sspi: token.clone(),
                ..Login::default()
            };
            let b = login.encode();
            assert_eq!(le32(&b, 0), b.len(), "{n}");
            assert_eq!(b[25] & F_INT_SECURITY, F_INT_SECURITY);
            assert_eq!(le16(&b, 80), short, "{n}");
            assert_eq!(le32(&b, 90), long, "{n}");
            let off = le16(&b, 78);
            assert_eq!(off, HEADER_LEN + 2);
            assert_eq!(&b[off..off + n], &token[..], "{n}");
            // The framed message splits into packets but carries every byte.
            let mut framed = Vec::new();
            crate::packet::frame(crate::packet::LOGIN7, &b, 4096, &mut framed);
            let mut r = crate::packet::MessageReader::new();
            r.push_raw(&framed);
            let (t, m) = r.poll().unwrap().unwrap();
            assert_eq!((t, m.len()), (crate::packet::LOGIN7, b.len()));
        }
    }
}
