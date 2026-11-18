//! PRELOGIN message: version and encryption negotiation.

use crate::Error;

const VERSION: u8 = 0;
const ENCRYPTION: u8 = 1;
const INSTOPT: u8 = 2;
const THREADID: u8 = 3;
const MARS: u8 = 4;
const TERMINATOR: u8 = 0xFF;

/// ENCRYPTION option values.
pub(crate) const ENCRYPT_OFF: u8 = 0;
pub(crate) const ENCRYPT_ON: u8 = 1;
pub(crate) const ENCRYPT_NOT_SUP: u8 = 2;
pub(crate) const ENCRYPT_STRICT: u8 = 4;

/// Builds the client PRELOGIN payload. Options are written in token order, as
/// go-mssqldb does: VERSION, ENCRYPTION, INSTOPT (NUL-terminated instance name),
/// THREADID (0) and MARS (0 = off).
pub(crate) fn encode(encrypt: u8, instance: &str, version: [u8; 6]) -> Vec<u8> {
    let mut inst = instance.as_bytes().to_vec();
    inst.push(0);
    let fields: [(u8, &[u8]); 5] = [
        (VERSION, &version),
        (ENCRYPTION, &[encrypt]),
        (INSTOPT, &inst),
        (THREADID, &[0, 0, 0, 0]),
        (MARS, &[0]),
    ];
    let mut out = Vec::new();
    let mut offset = (fields.len() * 5 + 1) as u16;
    for (token, value) in &fields {
        out.push(*token);
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        offset += value.len() as u16;
    }
    out.push(TERMINATOR);
    for (_, value) in &fields {
        out.extend_from_slice(value);
    }
    out
}

/// The parts of the server's PRELOGIN response dl-tds uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Response {
    /// The server's ENCRYPTION answer (`ENCRYPT_*`).
    pub(crate) encryption: u8,
    /// Server version bytes (major, minor, build hi, build lo, sub-build), if sent.
    pub(crate) version: Option<[u8; 6]>,
}

/// Parses a PRELOGIN response payload.
pub(crate) fn decode(buf: &[u8]) -> Result<Response, Error> {
    let bad = |offset: usize, what: &'static str| Error::Malformed {
        token: 0,
        offset,
        what,
    };
    let mut pos = 0;
    let mut encryption = None;
    let mut version = None;
    loop {
        let Some(&token) = buf.get(pos) else {
            return Err(bad(pos, "PRELOGIN option list is not terminated"));
        };
        if token == TERMINATOR {
            break;
        }
        let Some(h) = buf.get(pos..pos + 5) else {
            return Err(bad(pos, "truncated PRELOGIN option header"));
        };
        let off = u16::from_be_bytes([h[1], h[2]]) as usize;
        let len = u16::from_be_bytes([h[3], h[4]]) as usize;
        let Some(value) = buf.get(off..off + len) else {
            return Err(bad(pos, "PRELOGIN option points outside the message"));
        };
        match token {
            ENCRYPTION => {
                if value.len() != 1 {
                    return Err(bad(off, "PRELOGIN ENCRYPTION must be one byte"));
                }
                encryption = Some(value[0]);
            }
            VERSION if value.len() == 6 => {
                let mut v = [0u8; 6];
                v.copy_from_slice(value);
                version = Some(v);
            }
            _ => {}
        }
        pos += 5;
    }
    let encryption =
        encryption.ok_or_else(|| bad(0, "PRELOGIN response has no ENCRYPTION option"))?;
    Ok(Response {
        encryption,
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_layout() {
        let p = encode(ENCRYPT_NOT_SUP, "", [1, 10, 0, 0, 0, 0]);
        let expected = [
            0x00, 0x00, 0x1A, 0x00, 0x06, // VERSION at 26, len 6
            0x01, 0x00, 0x20, 0x00, 0x01, // ENCRYPTION at 32, len 1
            0x02, 0x00, 0x21, 0x00, 0x01, // INSTOPT at 33, len 1
            0x03, 0x00, 0x22, 0x00, 0x04, // THREADID at 34, len 4
            0x04, 0x00, 0x26, 0x00, 0x01, // MARS at 38, len 1
            0xFF, // terminator
            1, 10, 0, 0, 0, 0, // version
            2, // NOT_SUP
            0, // empty instance
            0, 0, 0, 0, // thread id
            0, // MARS off
        ];
        assert_eq!(p, expected);
        // go-mssqldb's packet (testdata/go_mssqldb_capture.txt) has the same option
        // table plus TRACEID; the instance option carries the instance name.
        let p = encode(ENCRYPT_ON, "SQLEXPRESS", [0; 6]);
        assert_eq!(&p[33..44], b"SQLEXPRESS\0");
    }

    #[test]
    fn decode_server_response() {
        // SQL Server 2022 (16.0.4135) answering ENCRYPT_OFF.
        let r = [
            0x00, 0x00, 0x24, 0x00, 0x06, 0x01, 0x00, 0x2A, 0x00, 0x01, 0x02, 0x00, 0x2B, 0x00,
            0x01, 0x03, 0x00, 0x2C, 0x00, 0x00, 0x04, 0x00, 0x2C, 0x00, 0x01, 0x06, 0x00, 0x2D,
            0x00, 0x01, 0x07, 0x00, 0x2E, 0x00, 0x00, 0xFF, 0x10, 0x00, 0x10, 0x27, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        let d = decode(&r).unwrap();
        assert_eq!(d.encryption, ENCRYPT_OFF);
        assert_eq!(d.version, Some([0x10, 0, 0x10, 0x27, 0, 0]));
        // The fake server in the go-mssqldb capture: VERSION + ENCRYPTION=NOT_SUP.
        let r = [
            0, 0, 11, 0, 6, 1, 0, 17, 0, 1, 0xFF, 16, 0, 0x10, 0, 0, 0, 2,
        ];
        assert_eq!(decode(&r).unwrap().encryption, ENCRYPT_NOT_SUP);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0xFF]).is_err()); // no ENCRYPTION
        assert!(decode(&[1, 0, 200, 0, 1, 0xFF]).is_err()); // out of range
        assert!(decode(&[1, 0, 6]).is_err()); // truncated header
        assert!(decode(&[1, 0, 6, 0, 2, 0xFF, 0, 0]).is_err()); // 2-byte encryption
        assert!(decode(&[0, 0, 5, 0, 0]).is_err()); // not terminated
    }
}
