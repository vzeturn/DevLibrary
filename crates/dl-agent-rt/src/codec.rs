//! Byte layout of dl-rt/1 frames (big endian, network order):
//!
//! ```text
//! off  size  field
//!   0     1  version      = 1
//!   1     1  chan         0 control, 1 capture, 2 commands, 3 results, 4 bulk
//!   2     1  kind         1 data .. 8 close
//!   3     1  flags        bit 0 = extension present; bits 1-7 reserved, must be 0
//!   4     8  seq          u64
//!  12     8  sent_us      u64
//!  20     4  payload_len  u32
//!  24     1  ext_len      only when flag bit 0: bytes of entries, a non-zero multiple of 9
//!  25   n*9  entries      tag u8 + value u64; tag 1 t_detected_us, 2 t_read_us
//!   ..  len  payload
//! ```
//!
//! The frame is exactly that long: a shorter buffer is `truncated`, a longer one `trailing`.

use alloc::vec::Vec;

use crate::frame::{Channel, EXT_ENTRY_LEN, Ext, Frame, HEADER_LEN, Kind, VERSION};
use crate::{DEFAULT_MAX_FRAME, Error};

/// Flag bit: a header extension follows the fixed header.
pub const FLAG_EXT: u8 = 0x01;
/// Extension tag of [`Ext::t_detected_us`].
pub const TAG_T_DETECTED: u8 = 1;
/// Extension tag of [`Ext::t_read_us`].
pub const TAG_T_READ: u8 = 2;

/// Size limits of the codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    max_frame: usize,
}

impl Limits {
    /// Limits with the given largest frame in bytes (header + extension + payload).
    pub const fn new(max_frame: usize) -> Limits {
        Limits { max_frame }
    }

    /// Largest frame in bytes.
    pub const fn max_frame(&self) -> usize {
        self.max_frame
    }

    /// Largest payload of a data frame without extension (`max_frame - 24`).
    pub const fn max_payload(&self) -> usize {
        self.max_frame.saturating_sub(HEADER_LEN)
    }
}

impl Default for Limits {
    /// [`DEFAULT_MAX_FRAME`] (1 MiB).
    fn default() -> Limits {
        Limits::new(DEFAULT_MAX_FRAME)
    }
}

fn be_u64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_be_bytes(a)
}

impl Frame {
    /// Encoded size in bytes.
    pub fn encoded_len(&self) -> usize {
        HEADER_LEN + self.ext.encoded_len() + self.payload.len()
    }

    /// Appends the encoding to `out`. Fails on an invalid frame or one over the limit;
    /// `out` is unchanged then.
    pub fn encode_into(&self, out: &mut Vec<u8>, limits: &Limits) -> Result<(), Error> {
        self.validate()?;
        let len = self.encoded_len();
        if len > limits.max_frame || u32::try_from(self.payload.len()).is_err() {
            return Err(Error::Oversize {
                len: len as u64,
                max: limits.max_frame,
            });
        }
        let ext_len = self.ext.encoded_len();
        out.reserve(len);
        out.push(VERSION);
        out.push(self.chan.as_u8());
        out.push(self.kind.as_u8());
        out.push(if ext_len == 0 { 0 } else { FLAG_EXT });
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.sent_us.to_be_bytes());
        // Fits: checked with u32::try_from above.
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        if ext_len != 0 {
            // At most 2 entries, so ext_len - 1 <= 18 fits a byte.
            out.push((ext_len - 1) as u8);
            for (tag, value) in [
                (TAG_T_DETECTED, self.ext.t_detected_us),
                (TAG_T_READ, self.ext.t_read_us),
            ] {
                if let Some(v) = value {
                    out.push(tag);
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
        out.extend_from_slice(&self.payload);
        Ok(())
    }

    /// The encoding of this frame (one WebSocket binary message).
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.encode_into(&mut out, limits)?;
        Ok(out)
    }

    /// Decodes one whole frame. Never panics; every malformed input is an [`Error`].
    ///
    /// Checks run in the order of the protocol document, section "Decoding": size limit,
    /// version, header length, channel, kind, flags, extension, exact length, kind rules.
    /// Unknown extension tags (3-255) are skipped so later versions can add timestamps.
    pub fn decode(buf: &[u8], limits: &Limits) -> Result<Frame, Error> {
        let have = buf.len();
        if have > limits.max_frame {
            return Err(Error::Oversize {
                len: have as u64,
                max: limits.max_frame,
            });
        }
        let truncated = |need: usize| Error::Truncated {
            need: need as u64,
            have,
        };
        match buf.first() {
            None => return Err(truncated(HEADER_LEN)),
            Some(&v) if v != VERSION => return Err(Error::Version(v)),
            Some(_) => {}
        }
        if have < HEADER_LEN {
            return Err(truncated(HEADER_LEN));
        }
        let chan = Channel::from_u8(buf[1]).ok_or(Error::Channel(buf[1]))?;
        let kind = Kind::from_u8(buf[2]).ok_or(Error::Kind(buf[2]))?;
        let flags = buf[3];
        if flags & !FLAG_EXT != 0 {
            return Err(Error::Flags(flags));
        }
        let seq = be_u64(buf, 4);
        let sent_us = be_u64(buf, 12);
        let payload_len = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
        let mut pos = HEADER_LEN;
        let mut ext = Ext::NONE;
        if flags & FLAG_EXT != 0 {
            if have <= HEADER_LEN {
                return Err(truncated(HEADER_LEN + 1));
            }
            let ext_len = usize::from(buf[HEADER_LEN]);
            if ext_len == 0 || !ext_len.is_multiple_of(EXT_ENTRY_LEN) {
                return Err(Error::Ext { offset: HEADER_LEN });
            }
            pos = HEADER_LEN + 1;
            if have < pos + ext_len {
                return Err(truncated(pos + ext_len));
            }
            let end = pos + ext_len;
            while pos < end {
                let slot = match buf[pos] {
                    TAG_T_DETECTED => Some(&mut ext.t_detected_us),
                    TAG_T_READ => Some(&mut ext.t_read_us),
                    0 => return Err(Error::Ext { offset: pos }),
                    _ => None,
                };
                if let Some(slot) = slot {
                    if slot.is_some() {
                        return Err(Error::Ext { offset: pos });
                    }
                    *slot = Some(be_u64(buf, pos + 1));
                }
                pos += EXT_ENTRY_LEN;
            }
        }
        let need = pos as u64 + u64::from(payload_len);
        if (have as u64) < need {
            return Err(Error::Truncated { need, have });
        }
        if (have as u64) > need {
            return Err(Error::Trailing {
                extra: (have as u64 - need) as usize,
            });
        }
        let frame = Frame {
            chan,
            kind,
            seq,
            sent_us,
            ext,
            payload: buf[pos..].to_vec(),
        };
        frame.validate()?;
        Ok(frame)
    }
}
