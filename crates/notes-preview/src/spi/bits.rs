//! Most-significant-bit-first reader used by every SPI field.
use super::SpiError;

pub(super) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    /// Reader over one data packet; `start` is a bit offset.
    pub(super) fn new(data: &'a [u8], start: usize) -> Self {
        Bits { data, pos: start }
    }

    pub(super) fn position(&self) -> usize {
        self.pos
    }

    pub(super) fn read(&mut self, n: u32) -> Result<u32, SpiError> {
        debug_assert!(n <= 32);
        if n == 0 {
            return Ok(0);
        }
        let end = self.pos.checked_add(n as usize).ok_or(SpiError::Truncated)?;
        if end > self.data.len() * 8 {
            return Err(SpiError::Truncated);
        }
        let mut value: u64 = 0;
        let mut pos = self.pos;
        let mut left = n;
        while left > 0 {
            let byte = self.data[pos / 8] as u64;
            let offset = (pos % 8) as u32;
            let take = (8 - offset).min(left);
            let chunk = (byte >> (8 - offset - take)) & ((1u64 << take) - 1);
            value = (value << take) | chunk;
            pos += take as usize;
            left -= take;
        }
        self.pos = end;
        Ok(value as u32)
    }

    pub(super) fn bit(&mut self) -> Result<bool, SpiError> {
        Ok(self.read(1)? == 1)
    }

    /// Number of zero bits before the next set bit (the set bit is consumed).
    /// More than `limit` zeros is rejected.
    pub(super) fn zeros(&mut self, limit: u32) -> Result<u32, SpiError> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > limit {
                return Err(SpiError::Invalid("variable-length code exceeds bound"));
            }
        }
        Ok(zeros)
    }

    /// `k` zeros, a one, then `k` suffix bits: `2^k + suffix`.
    pub(super) fn positive(&mut self) -> Result<u32, SpiError> {
        let k = self.zeros(31)?;
        let suffix = self.read(k)?;
        Ok((1u32 << k) | suffix)
    }

    /// Skip to the next byte boundary of the packet.
    pub(super) fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    /// `n` whole bytes; the reader must be byte aligned.
    pub(super) fn bytes(&mut self, n: usize) -> Result<&'a [u8], SpiError> {
        debug_assert!(self.pos % 8 == 0);
        let start = self.pos / 8;
        let end = start.checked_add(n).ok_or(SpiError::Truncated)?;
        if end > self.data.len() {
            return Err(SpiError::Truncated);
        }
        self.pos = end * 8;
        Ok(&self.data[start..end])
    }
}
