//! CRC-32 (IEEE 802.3, reflected polynomial 0xEDB88320), slicing-by-8.

const POLY: u32 = 0xedb8_8320;

static TABLES: [[u32; 256]; 8] = build_tables();

const fn build_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut s = 1;
    while s < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            i += 1;
        }
        s += 1;
    }
    t
}

/// Streaming CRC-32 hasher.
#[derive(Debug, Clone, Copy)]
pub struct Crc32 {
    crc: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// Creates a hasher with the standard initial value.
    pub const fn new() -> Self {
        Self { crc: !0 }
    }

    /// Feeds more data.
    pub fn update(&mut self, data: &[u8]) {
        let t = &TABLES;
        let mut crc = self.crc;
        let (blocks, rest) = data.as_chunks::<8>();
        for b in blocks {
            let lo = crc ^ u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            crc = t[7][(lo & 0xff) as usize]
                ^ t[6][((lo >> 8) & 0xff) as usize]
                ^ t[5][((lo >> 16) & 0xff) as usize]
                ^ t[4][(lo >> 24) as usize]
                ^ t[3][b[4] as usize]
                ^ t[2][b[5] as usize]
                ^ t[1][b[6] as usize]
                ^ t[0][b[7] as usize];
        }
        for &b in rest {
            crc = (crc >> 8) ^ t[0][((crc ^ u32::from(b)) & 0xff) as usize];
        }
        self.crc = crc;
    }

    /// Returns the checksum of everything fed so far.
    pub const fn finalize(&self) -> u32 {
        !self.crc
    }
}

/// CRC-32 of `data` in one call.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414f_a339
        );
    }

    #[test]
    fn streaming_matches_bytewise() {
        let data: std::vec::Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let whole = crc32(&data);
        for split in [0, 1, 3, 7, 8, 9, 500, 999] {
            let mut c = Crc32::new();
            c.update(&data[..split]);
            c.update(&data[split..]);
            assert_eq!(c.finalize(), whole);
        }
    }
}
