//! Archived job logs in zstd's seekable format: the log cut in frames of
//! [`FRAME`] bytes, each compressed on its own, then a table of their sizes in
//! a skippable frame at the end
//! (<https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md>).
//! Reading a few lines decompresses the frames that hold them, not the whole
//! log, and `zstd -d` still reads the file whole: it skips the table.
//!
//! Measured on real job logs in GitLab Runner's format, timestamps included
//! (a verbose `cargo build` of forgust-server, 1.3 MiB; a `vite build` of the
//! web UI, 63 KiB), as the log's size over the stored size:
//!
//! | | gzip -6 | zstd -3 | zstd -9 | zstd -9, 1 MiB frames | zstd -9, 256 KiB frames |
//! |---|---|---|---|---|---|
//! | cargo build | 10.2 | 12.3 | 14.9 | 13.8 | 11.9 |
//! | vite build | 7.7 | 7.7 | 8.4 | 8.3 | 8.3 |
//!
//! Level 9 compresses at 80 to 110 MB/s and decompresses at over 2 GB/s: done
//! once per job, after it ends, it costs little. Frames of 1 MiB keep most of
//! the gain while a read of 1 MiB decompresses 2 MiB at most.

use std::io;

/// Log bytes per frame.
pub const FRAME: usize = 1 << 20;

const LEVEL: i32 = 9;

/// The table is a skippable frame: this magic, then the size of what follows.
const SKIPPABLE_MAGIC: u32 = 0x184D_2A5E;
/// The table's footer ends with this.
const SEEKABLE_MAGIC: u32 = 0x8F92_EAB1;
const HEADER: usize = 8;
/// A frame's compressed and log sizes (4 bytes each; the table carries no
/// checksums, each frame has its own).
const ENTRY: usize = 8;
/// The number of frames (4 bytes), a descriptor (1) and the magic (4).
const FOOTER: usize = 9;

/// One frame: where its compressed bytes are in the file, and which log
/// bytes it holds.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub at: u64,
    pub compressed: u32,
    pub start: u64,
    pub len: u32,
}

impl Frame {
    fn end(&self) -> u64 {
        self.start + u64::from(self.len)
    }
}

/// The frames of a file, in order.
#[derive(Debug)]
pub struct Table {
    pub frames: Vec<Frame>,
    /// The log's size.
    pub size: u64,
}

/// `log` in the seekable format.
pub fn encode(log: &[u8]) -> io::Result<Vec<u8>> {
    let mut z = zstd::bulk::Compressor::new(LEVEL)?;
    // a damaged frame fails to read rather than reading wrong
    z.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))?;
    let mut out = Vec::with_capacity(log.len() / 8);
    let mut sizes = Vec::new();
    for frame in log.chunks(FRAME) {
        let compressed = z.compress(frame)?;
        sizes.push((compressed.len() as u32, frame.len() as u32));
        out.extend_from_slice(&compressed);
    }
    out.extend_from_slice(&SKIPPABLE_MAGIC.to_le_bytes());
    out.extend_from_slice(&((sizes.len() * ENTRY + FOOTER) as u32).to_le_bytes());
    for (compressed, len) in &sizes {
        out.extend_from_slice(&compressed.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
    }
    out.extend_from_slice(&(sizes.len() as u32).to_le_bytes());
    out.push(0);
    out.extend_from_slice(&SEEKABLE_MAGIC.to_le_bytes());
    Ok(out)
}

fn corrupt(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("the stored job log {why}"))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// How many bytes the table takes at the end of the file, from `tail`, the
/// file's last bytes (its footer at least).
pub fn table_len(tail: &[u8]) -> io::Result<usize> {
    if tail.len() < HEADER + FOOTER {
        return Err(corrupt("is too short"));
    }
    let footer = &tail[tail.len() - FOOTER..];
    if u32_at(footer, 5) != SEEKABLE_MAGIC {
        return Err(corrupt("has no frame table"));
    }
    if footer[4] != 0 {
        return Err(corrupt("has a frame table this version cannot read"));
    }
    Ok(HEADER + u32_at(footer, 0) as usize * ENTRY + FOOTER)
}

impl Table {
    /// The table of a file of `file_size` bytes, from its last bytes
    /// (at least [`table_len`] of them).
    pub fn parse(tail: &[u8], file_size: u64) -> io::Result<Table> {
        let len = table_len(tail)?;
        if tail.len() < len || file_size < len as u64 {
            return Err(corrupt("is cut short"));
        }
        let table = &tail[tail.len() - len..];
        if u32_at(table, 0) != SKIPPABLE_MAGIC || u32_at(table, 4) as usize != len - HEADER {
            return Err(corrupt("has a damaged frame table"));
        }
        let (mut at, mut start) = (0u64, 0u64);
        let frames: Vec<Frame> = table[HEADER..len - FOOTER]
            .as_chunks::<ENTRY>()
            .0
            .iter()
            .map(|e| {
                let f = Frame { at, compressed: u32_at(e, 0), start, len: u32_at(e, 4) };
                at += u64::from(f.compressed);
                start += u64::from(f.len);
                f
            })
            .collect();
        if at + len as u64 != file_size {
            return Err(corrupt("does not match its frame table"));
        }
        Ok(Table { frames, size: start })
    }

    /// The frames that hold log bytes `start..end`.
    pub fn frames_for(&self, start: u64, end: u64) -> &[Frame] {
        let first = self.frames.partition_point(|f| f.end() <= start);
        let last = self.frames.partition_point(|f| f.start < end).max(first);
        &self.frames[first..last]
    }

    /// Where the compressed bytes of `frames` (consecutive, as
    /// [`Table::frames_for`] gives them) are in the file; None for no frame.
    pub fn span(frames: &[Frame]) -> Option<std::ops::Range<u64>> {
        Some(frames.first()?.at..frames.last().map(|l| l.at + u64::from(l.compressed))?)
    }
}

/// The log bytes of frame `f`, from its compressed bytes.
pub fn decompress_frame(f: &Frame, compressed: &[u8]) -> io::Result<Vec<u8>> {
    let log = zstd::bulk::decompress(compressed, f.len as usize)?;
    if log.len() != f.len as usize {
        return Err(corrupt("has a frame shorter than its table says"));
    }
    Ok(log)
}

/// Log bytes `start..end` from `frames` (from [`Table::frames_for`]) and
/// their compressed bytes, read in one piece from the first one's `at`.
pub fn decompress(frames: &[Frame], compressed: &[u8], start: u64, end: u64) -> io::Result<Vec<u8>> {
    let Some(first) = frames.first() else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for f in frames {
        let from = (f.at - first.at) as usize;
        let bytes = compressed.get(from..from + f.compressed as usize).ok_or_else(|| corrupt("is cut short"))?;
        let log = decompress_frame(f, bytes)?;
        let lo = start.saturating_sub(f.start).min(u64::from(f.len)) as usize;
        let hi = end.saturating_sub(f.start).min(u64::from(f.len)) as usize;
        out.extend_from_slice(&log[lo..hi.max(lo)]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A log like a job's: repeated lines with a counter.
    fn log(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + 64);
        let mut i = 0u64;
        while out.len() < len {
            out.extend_from_slice(format!("\x1b[32;1m$ step {i}\x1b[0;m compiling crate-{} v1.{}\n", i % 97, i % 13).as_bytes());
            i += 1;
        }
        out.truncate(len);
        out
    }

    fn read(file: &[u8], start: u64, end: u64) -> Vec<u8> {
        let t = Table::parse(file, file.len() as u64).unwrap();
        let frames = t.frames_for(start, end);
        let Some(span) = Table::span(frames) else { return Vec::new() };
        decompress(frames, &file[span.start as usize..span.end as usize], start, end).unwrap()
    }

    #[test]
    fn reads_any_range_and_stays_a_zstd_file() {
        let data = log(2 * FRAME + 12_345);
        let file = encode(&data).unwrap();
        assert!(file.len() * 10 < data.len(), "{} bytes for {}", file.len(), data.len());
        // zstd -d reads it whole: the table is a skippable frame
        assert_eq!(zstd::stream::decode_all(&file[..]).unwrap(), data);
        let t = Table::parse(&file, file.len() as u64).unwrap();
        assert_eq!((t.frames.len(), t.size), (3, data.len() as u64));
        assert_eq!(Table::span(&t.frames).map_or(0, |s| s.end) as usize + table_len(&file).unwrap(), file.len());
        let n = data.len() as u64;
        let f = FRAME as u64;
        for (start, end) in [(0, n), (0, 10), (f - 5, f + 5), (f, 2 * f), (f + 1, n), (n - 3, n), (n, n + 10), (5, 5), (n + 7, n + 9)] {
            let want = &data[(start.min(n) as usize)..(end.min(n) as usize)];
            assert_eq!(read(&file, start, end), want, "{start}..{end}");
        }
        assert_eq!(t.frames_for(f - 1, f + 1).len(), 2);
        assert_eq!(t.frames_for(f, f + 1).len(), 1);
        assert!(t.frames_for(n, n + 1).is_empty());
    }

    #[test]
    fn short_and_empty_logs() {
        for data in [Vec::new(), b"hello\n".to_vec(), log(FRAME)] {
            let file = encode(&data).unwrap();
            assert_eq!(zstd::stream::decode_all(&file[..]).unwrap(), data);
            let t = Table::parse(&file, file.len() as u64).unwrap();
            assert_eq!(t.size, data.len() as u64);
            assert_eq!(read(&file, 0, u64::MAX), data);
        }
    }

    #[test]
    fn damage_is_an_error_not_a_wrong_log() {
        let data = log(FRAME + 100);
        let file = encode(&data).unwrap();
        // cut short: the sizes no longer add up
        assert!(Table::parse(&file[1..], file.len() as u64 - 1).is_err());
        assert!(table_len(b"not a seekable file at all").is_err());
        // a flipped byte inside a frame fails its checksum
        let mut bad = file.clone();
        bad[100] ^= 0xff;
        let t = Table::parse(&bad, bad.len() as u64).unwrap();
        let f = t.frames[0];
        assert!(decompress_frame(&f, &bad[..f.compressed as usize]).is_err());
    }
}
