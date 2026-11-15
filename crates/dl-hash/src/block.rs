//! Merkle–Damgård block buffering and padding shared by MD5, SHA-1 and SHA-2.

#[derive(Clone)]
pub(crate) struct Buffer<const B: usize> {
    buf: [u8; B],
    len: usize,
    /// Total bytes absorbed so far.
    total: u64,
}

impl<const B: usize> Default for Buffer<B> {
    fn default() -> Self {
        Self {
            buf: [0; B],
            len: 0,
            total: 0,
        }
    }
}

/// Byte order of the message-length field appended during padding.
#[derive(Clone, Copy)]
pub(crate) enum LengthOrder {
    Big,
    Little,
}

impl<const B: usize> Buffer<B> {
    #[inline]
    pub(crate) fn update(&mut self, mut data: &[u8], mut compress: impl FnMut(&[u8; B])) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.len > 0 {
            let take = (B - self.len).min(data.len());
            self.buf[self.len..self.len + take].copy_from_slice(&data[..take]);
            self.len += take;
            data = &data[take..];
            if self.len < B {
                return;
            }
            compress(&self.buf);
            self.len = 0;
        }
        let (blocks, rest) = data.as_chunks::<B>();
        for block in blocks {
            compress(block);
        }
        self.buf[..rest.len()].copy_from_slice(rest);
        self.len = rest.len();
    }

    /// Appends `0x80`, zero padding and the bit length in a `len_bytes`-wide field, then
    /// compresses the final block(s).
    pub(crate) fn finish(
        &mut self,
        len_bytes: usize,
        order: LengthOrder,
        mut compress: impl FnMut(&[u8; B]),
    ) {
        let bit_len = u128::from(self.total) * 8;
        // `len < B` always holds between calls, so there is room for the 0x80 marker.
        self.buf[self.len] = 0x80;
        self.len += 1;
        if self.len > B - len_bytes {
            self.buf[self.len..].fill(0);
            compress(&self.buf);
            self.len = 0;
        }
        self.buf[self.len..B - len_bytes].fill(0);
        let field = &mut self.buf[B - len_bytes..];
        match order {
            LengthOrder::Big => field.copy_from_slice(&bit_len.to_be_bytes()[16 - len_bytes..]),
            LengthOrder::Little => field.copy_from_slice(&bit_len.to_le_bytes()[..len_bytes]),
        }
        compress(&self.buf);
        self.len = 0;
    }
}
