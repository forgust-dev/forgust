//! Code search: text inside the files of each project's default branch.
//!
//! `index` keeps the trigram index on disk, `indexer` fills it from the
//! repositories, `query` turns a search into what to ask it and how to check
//! a line, `search` answers one for a caller.

pub mod index;
pub mod indexer;
pub mod query;
pub mod search;

pub use index::CodeIndex;

/// The text of a blob as it is indexed and searched, or `None` for what is
/// not: binary files, LFS pointers and files over the size limit (`data`
/// holds less than `size` then).
pub fn searchable_text(size: u64, data: &[u8], max_bytes: u64) -> Option<String> {
    if size > max_bytes || (data.len() as u64) < size || crate::languages::is_binary(data) || crate::lfs::parse_pointer(data).is_some() {
        return None;
    }
    Some(String::from_utf8_lossy(data).into_owned())
}
