//! Browsing a job's artifacts archive (M7) without downloading it: the zip's
//! central directory is read with ranged reads, one entry is streamed (stored,
//! or inflated with a bound). Entry names are only shown and matched exactly,
//! never used as paths.

use futures::StreamExt;

use crate::blob::ByteStream;
use crate::state::AppState;

/// More entries than this and the listing stops (the download still works).
pub const MAX_ENTRIES: usize = 100_000;
/// Read whole on every listing: kept small (a 100 000-entry directory is ~8 MB).
const MAX_CENTRAL_DIR: u64 = 8 << 20;
const EOCD: u32 = 0x0605_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const ZIP64_EOCD: u32 = 0x0606_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    /// 0 stored, 8 deflate (others are listed, not opened).
    pub method: u16,
    pub compressed: u64,
    pub size: u64,
    pub offset: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    b.get(i..i + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()))
}

/// Where the central directory is: (offset, size, entries).
fn locate(tail: &[u8], tail_start: u64) -> anyhow::Result<(u64, u64, u64)> {
    let i = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(tail, i) == Some(EOCD))
        .ok_or_else(|| anyhow::anyhow!("not a zip archive"))?;
    let entries = u16_at(tail, i + 10).unwrap_or(0) as u64;
    let size = u32_at(tail, i + 12).unwrap_or(0) as u64;
    let offset = u32_at(tail, i + 16).unwrap_or(0) as u64;
    if entries != 0xFFFF && size != 0xFFFF_FFFF && offset != 0xFFFF_FFFF {
        return Ok((offset, size, entries));
    }
    // zip64: the locator sits just before the end record and points at the zip64 end record
    let l = i.checked_sub(20).ok_or_else(|| anyhow::anyhow!("zip64 locator missing"))?;
    anyhow::ensure!(u32_at(tail, l) == Some(ZIP64_LOCATOR), "zip64 locator missing");
    let rec = u64_at(tail, l + 8).unwrap_or(0);
    let r = rec.checked_sub(tail_start).map(|r| r as usize).ok_or_else(|| anyhow::anyhow!("zip64 record out of reach"))?;
    anyhow::ensure!(u32_at(tail, r) == Some(ZIP64_EOCD), "zip64 end record missing");
    Ok((u64_at(tail, r + 48).unwrap_or(0), u64_at(tail, r + 40).unwrap_or(0), u64_at(tail, r + 32).unwrap_or(0)))
}

fn parse_central(cd: &[u8], max: usize) -> anyhow::Result<(Vec<Entry>, bool)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 46 <= cd.len() {
        anyhow::ensure!(u32_at(cd, i) == Some(CENTRAL), "bad central directory");
        if out.len() >= max {
            return Ok((out, true));
        }
        let method = u16_at(cd, i + 10).unwrap_or(0);
        let mut compressed = u32_at(cd, i + 20).unwrap_or(0) as u64;
        let mut size = u32_at(cd, i + 24).unwrap_or(0) as u64;
        let name_len = u16_at(cd, i + 28).unwrap_or(0) as usize;
        let extra_len = u16_at(cd, i + 30).unwrap_or(0) as usize;
        let comment_len = u16_at(cd, i + 32).unwrap_or(0) as usize;
        let mut offset = u32_at(cd, i + 42).unwrap_or(0) as u64;
        let name_bytes = cd.get(i + 46..i + 46 + name_len).ok_or_else(|| anyhow::anyhow!("truncated entry"))?;
        let extra = cd.get(i + 46 + name_len..i + 46 + name_len + extra_len).unwrap_or(&[]);
        // zip64 extra field: the 0xFFFFFFFF values, in order
        let mut j = 0;
        while j + 4 <= extra.len() {
            let id = u16_at(extra, j).unwrap_or(0);
            let len = u16_at(extra, j + 2).unwrap_or(0) as usize;
            if id == 0x0001 {
                let mut k = j + 4;
                for v in [&mut size, &mut compressed, &mut offset] {
                    if *v == 0xFFFF_FFFF
                        && let Some(x) = u64_at(extra, k)
                    {
                        *v = x;
                        k += 8;
                    }
                }
            }
            j += 4 + len;
        }
        out.push(Entry { name: String::from_utf8_lossy(name_bytes).into_owned(), method, compressed, size, offset });
        i += 46 + name_len + extra_len + comment_len;
    }
    Ok((out, false))
}

/// The archive's entries (at most `MAX_ENTRIES`; `true` when there are more).
pub async fn entries(state: &AppState, key: &str) -> anyhow::Result<(Vec<Entry>, bool)> {
    let size = state.blobs.size(key).await?.ok_or_else(|| anyhow::anyhow!("the archive is gone"))?;
    // end record (22 bytes + a comment of up to 64 KB), plus room for zip64 records
    let tail_len = size.min(22 + 65_535 + 20 + 56);
    let tail_start = size - tail_len;
    let tail = state.blobs.get_range(key, tail_start, size).await?;
    let (offset, cd_size, _) = locate(&tail, tail_start)?;
    let end = offset.checked_add(cd_size).filter(|e| *e <= size);
    let Some(end) = end.filter(|_| cd_size <= MAX_CENTRAL_DIR) else { anyhow::bail!("the archive's directory is too large or damaged") };
    let cd = state.blobs.get_range(key, offset, end).await?;
    parse_central(&cd, MAX_ENTRIES)
}

#[derive(Debug, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct Listed {
    /// The entry's name within the directory (no slash).
    pub name: String,
    /// The full path inside the archive.
    pub path: String,
    pub dir: bool,
    pub size: u64,
}

/// The direct children of `dir` (`""` or `a/b/`), folders first; folders that
/// only appear in file paths are listed too.
pub fn children(all: &[Entry], dir: &str) -> Vec<Listed> {
    let mut seen = std::collections::BTreeMap::new();
    for e in all {
        let Some(rest) = e.name.strip_prefix(dir) else { continue };
        if rest.is_empty() {
            continue;
        }
        match rest.split_once('/') {
            Some((first, _)) => {
                seen.entry((0, first.to_string())).or_insert_with(|| Listed {
                    name: first.to_string(),
                    path: format!("{dir}{first}/"),
                    dir: true,
                    size: 0,
                });
            }
            None => {
                seen.entry((1, rest.to_string())).or_insert_with(|| Listed {
                    name: rest.to_string(),
                    path: e.name.clone(),
                    dir: false,
                    size: e.size,
                });
            }
        }
    }
    seen.into_values().collect()
}

/// One entry's content, streamed (inflated for deflate), never more than its size.
pub async fn open(state: &AppState, key: &str, e: &Entry) -> anyhow::Result<ByteStream> {
    anyhow::ensure!(matches!(e.method, 0 | 8), "this file is compressed in a way that cannot be opened here");
    // a stored entry is sent as is, with its announced size: they must agree
    anyhow::ensure!(e.method != 0 || e.compressed == e.size, "damaged archive");
    let damaged = || anyhow::anyhow!("damaged archive");
    let head = state.blobs.get_range(key, e.offset, e.offset.checked_add(30).ok_or_else(damaged)?).await?;
    anyhow::ensure!(u32_at(&head, 0) == Some(LOCAL), "damaged archive");
    let start =
        e.offset.checked_add(30 + u16_at(&head, 26).unwrap_or(0) as u64 + u16_at(&head, 28).unwrap_or(0) as u64).ok_or_else(damaged)?;
    let end = start.checked_add(e.compressed).ok_or_else(damaged)?;
    let raw = state.blobs.get_range_stream(key, start, end).await?;
    if e.method == 0 {
        return Ok(raw);
    }
    let reader = tokio_util::io::StreamReader::new(raw);
    let inflated = async_compression::tokio::bufread::DeflateDecoder::new(tokio::io::BufReader::new(reader));
    // a lying size cannot inflate past what the directory announced
    let limited = tokio::io::AsyncReadExt::take(inflated, e.size);
    Ok(tokio_util::io::ReaderStream::new(limited).boxed())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str) -> Entry {
        Entry { name: name.into(), method: 0, compressed: 1, size: 1, offset: 0 }
    }

    #[test]
    fn lists_one_level() {
        let all = [e("out/"), e("out/r.txt"), e("out/sub/x.bin"), e("top.log"), e("deep/a/b.txt")];
        let root: Vec<(String, bool)> = children(&all, "").into_iter().map(|l| (l.name, l.dir)).collect();
        assert_eq!(root, vec![("deep".into(), true), ("out".into(), true), ("top.log".into(), false)]);
        let out: Vec<String> = children(&all, "out/").into_iter().map(|l| l.path).collect();
        assert_eq!(out, vec!["out/sub/", "out/r.txt"]);
    }

    #[test]
    fn central_directory() {
        // a minimal archive: one stored entry "a.txt" = "hi"
        let mut z = Vec::new();
        let name = b"a.txt";
        z.extend_from_slice(&LOCAL.to_le_bytes());
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&0u32.to_le_bytes());
        z.extend_from_slice(&2u32.to_le_bytes());
        z.extend_from_slice(&2u32.to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(name);
        z.extend_from_slice(b"hi");
        let cd_start = z.len();
        z.extend_from_slice(&CENTRAL.to_le_bytes());
        z.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&0u32.to_le_bytes());
        z.extend_from_slice(&2u32.to_le_bytes());
        z.extend_from_slice(&2u32.to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&0u32.to_le_bytes());
        z.extend_from_slice(name);
        let cd_len = z.len() - cd_start;
        z.extend_from_slice(&EOCD.to_le_bytes());
        z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend_from_slice(&(cd_len as u32).to_le_bytes());
        z.extend_from_slice(&(cd_start as u32).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        let (off, size, n) = locate(&z, 0).unwrap();
        assert_eq!((off, size, n), (cd_start as u64, cd_len as u64, 1));
        let (list, more) = parse_central(&z[cd_start..cd_start + cd_len], 10).unwrap();
        assert!(!more);
        assert_eq!(list[0], Entry { name: "a.txt".into(), method: 0, compressed: 2, size: 2, offset: 0 });
        assert!(locate(b"not a zip at all, really not", 0).is_err());
    }
}
