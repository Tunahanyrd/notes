//! `gzip.GzipFile(fileobj=io.BytesIO(raw)).read(n)` of CPython 3.14: the
//! `io.BufferedReader` read pattern over `_GzipReader`, multi-member
//! streams, zero padding, CRC/ISIZE checks and the exception classes
//! (`gzip.BadGzipFile` is an OSError, truncation is `EOFError`, a corrupt
//! deflate stream is `zlib.error` which escapes `send_once`). Inflation is
//! the system zlib (flate2 "zlib" backend), the same library CPython uses.
use crate::error::NetError;
use flate2::{Crc, Decompress, FlushDecompress, Status};

/// `gzip.READ_BUFFER_SIZE`
const READ_BUFFER_SIZE: usize = 128 * 1024;
/// `io.DEFAULT_BUFFER_SIZE` of the BufferedReader GzipFile wraps.
const BUFFER_SIZE: usize = 128 * 1024;

const FHCRC: u8 = 2;
const FEXTRA: u8 = 4;
const FNAME: u8 = 8;
const FCOMMENT: u8 = 16;

fn bad(message: impl Into<String>) -> NetError {
    NetError::os("BadGzipFile", message)
}

fn truncated() -> NetError {
    NetError::eof("Compressed file ended before the end-of-stream marker was reached")
}

/// `_PaddedFile` over a BytesIO: a cursor with `prepend`.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn read(&mut self, n: usize) -> &'a [u8] {
        let end = (self.pos + n).min(self.data.len());
        let out = &self.data[self.pos..end];
        self.pos = end;
        out
    }
    fn prepend(&mut self, n: usize) {
        self.pos -= n;
    }
    /// `_read_exact`
    fn read_exact(&mut self, n: usize) -> Result<&'a [u8], NetError> {
        let out = self.read(n);
        if out.len() < n {
            return Err(truncated());
        }
        Ok(out)
    }
}

/// `zlib._ZlibDecompressor(wbits=-15)`
struct Inflater {
    z: Decompress,
    /// Input handed in but not consumed yet (kept while `needs_input` is False).
    pending: Vec<u8>,
    eof: bool,
    needs_input: bool,
    /// Bytes of input after the end of the deflate stream.
    unused: usize,
}

impl Inflater {
    fn new() -> Inflater {
        Inflater { z: Decompress::new(false), pending: Vec::new(), eof: false, needs_input: true, unused: 0 }
    }

    /// `decompress(data, max_length)`; `Err` is `zlib.error`.
    fn decompress(&mut self, data: &[u8], max_length: usize) -> Result<Vec<u8>, NetError> {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(data);
        let mut out: Vec<u8> = Vec::with_capacity(max_length.min(1 << 20));
        let mut offset = 0;
        loop {
            if out.len() >= max_length {
                break;
            }
            let before_in = self.z.total_in();
            let before_out = self.z.total_out();
            let room = max_length - out.len();
            let start = out.len();
            out.resize(start + room.min(1 << 20), 0);
            let status = self.z.decompress(&input[offset..], &mut out[start..], FlushDecompress::Sync);
            let produced = (self.z.total_out() - before_out) as usize;
            let consumed = (self.z.total_in() - before_in) as usize;
            out.truncate(start + produced);
            offset += consumed;
            match status {
                Err(e) => {
                    return Err(NetError::escape("zlib.error", format!("Error -3 while decompressing data: {e}")));
                }
                Ok(Status::StreamEnd) => {
                    self.eof = true;
                    break;
                }
                Ok(_) => {
                    if produced == 0 && consumed == 0 {
                        break; // needs more input (or no progress possible)
                    }
                }
            }
        }
        let rest = &input[offset..];
        if self.eof {
            self.needs_input = false;
            self.unused = rest.len();
        } else if rest.is_empty() {
            self.needs_input = true;
        } else {
            self.needs_input = false;
            self.pending = rest.to_vec();
        }
        Ok(out)
    }
}

/// `gzip._GzipReader` (raw stream of the BufferedReader).
struct GzipReader<'a> {
    fp: Cursor<'a>,
    inflater: Inflater,
    new_member: bool,
    crc: Crc,
    stream_size: u64,
}

impl<'a> GzipReader<'a> {
    /// `_read_gzip_header`: false at a clean end of input.
    fn read_gzip_header(&mut self) -> Result<bool, NetError> {
        let magic = self.fp.read(2);
        if magic.is_empty() {
            return Ok(false);
        }
        if magic != b"\x1f\x8b" {
            return Err(bad(format!("Not a gzipped file ({})", bytes_repr(magic))));
        }
        let header = self.fp.read_exact(8)?;
        let (method, flag) = (header[0], header[1]);
        if method != 8 {
            return Err(bad("Unknown compression method"));
        }
        if flag & FEXTRA != 0 {
            let len = self.fp.read_exact(2)?;
            let extra = u16::from_le_bytes([len[0], len[1]]) as usize;
            self.fp.read_exact(extra)?;
        }
        for bit in [FNAME, FCOMMENT] {
            if flag & bit != 0 {
                loop {
                    let s = self.fp.read(1);
                    if s.is_empty() || s == b"\0" {
                        break;
                    }
                }
            }
        }
        if flag & FHCRC != 0 {
            self.fp.read_exact(2)?;
        }
        Ok(true)
    }

    /// `_read_eof`
    fn read_eof(&mut self) -> Result<(), NetError> {
        let trailer = self.fp.read_exact(8)?;
        let crc32 = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
        let isize = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
        if crc32 != self.crc.sum() {
            return Err(bad(format!("CRC check failed {:#x} != {:#x}", crc32, self.crc.sum())));
        }
        if isize != (self.stream_size & 0xffff_ffff) as u32 {
            return Err(bad("Incorrect length of data produced"));
        }
        loop {
            let c = self.fp.read(1);
            if c.is_empty() {
                break;
            }
            if c != b"\0" {
                self.fp.prepend(1);
                break;
            }
        }
        Ok(())
    }

    /// `_GzipReader.read(size)`
    fn read(&mut self, size: usize) -> Result<Vec<u8>, NetError> {
        if size == 0 {
            return Ok(Vec::new());
        }
        let out = loop {
            if self.inflater.eof {
                self.read_eof()?;
                self.new_member = true;
                self.inflater = Inflater::new();
            }
            if self.new_member {
                self.crc = Crc::new();
                self.stream_size = 0;
                if !self.read_gzip_header()? {
                    return Ok(Vec::new());
                }
                self.new_member = false;
            }
            let buf: &[u8] = if self.inflater.needs_input { self.fp.read(READ_BUFFER_SIZE) } else { &[] };
            let uncompress = self.inflater.decompress(buf, size)?;
            if self.inflater.unused > 0 {
                self.fp.prepend(self.inflater.unused);
                self.inflater.unused = 0;
            }
            if !uncompress.is_empty() {
                break uncompress;
            }
            if buf.is_empty() {
                return Err(truncated());
            }
        };
        self.crc.update(&out);
        self.stream_size += out.len() as u64;
        Ok(out)
    }
}

fn bytes_repr(b: &[u8]) -> String {
    let mut s = String::from("b'");
    for &c in b {
        match c {
            b'\\' => s.push_str("\\\\"),
            b'\'' => s.push_str("\\'"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s.push('\'');
    s
}

/// `GzipFile(fileobj=BytesIO(raw)).read(n)` (BufferedReader.read_generic).
pub(crate) fn read_bounded(raw: &[u8], n: usize) -> Result<Vec<u8>, NetError> {
    let mut reader = GzipReader { fp: Cursor { data: raw, pos: 0 }, inflater: Inflater::new(), new_member: true,
        crc: Crc::new(), stream_size: 0 };
    let mut out = Vec::new();
    let mut remaining = n;
    // Whole blocks straight from the raw stream.
    while remaining > 0 {
        let r = remaining - remaining % BUFFER_SIZE;
        if r == 0 {
            break;
        }
        let data = reader.read(r)?;
        if data.is_empty() {
            return Ok(out);
        }
        remaining -= data.len();
        out.extend_from_slice(&data);
    }
    // The tail through the buffer (fill_buffer may decode ahead of `n`).
    let mut read_end = 0;
    while remaining > 0 && read_end < BUFFER_SIZE {
        let data = reader.read(BUFFER_SIZE - read_end)?;
        if data.is_empty() {
            return Ok(out);
        }
        read_end += data.len();
        let take = remaining.min(data.len());
        out.extend_from_slice(&data[..take]);
        remaining -= take;
    }
    Ok(out)
}
