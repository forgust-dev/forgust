//! Job logs: live chunks in Postgres while the job runs, one blob when it ends
//! (ADR-0014), zstd-compressed in the seekable format (`ci::seekable`). Logs
//! archived before that, and logs compression would not shrink, are raw.

use bytes::{Bytes, BytesMut};
use futures::StreamExt;

use crate::blob::ByteStream;
use crate::ci::seekable;
use crate::state::AppState;

/// Where a raw log is.
pub fn blob_key(project_id: i64, build_id: i64) -> String {
    format!("ci/traces/{project_id}/{build_id}.log")
}

/// Where a compressed log is.
fn zstd_key(project_id: i64, build_id: i64) -> String {
    format!("ci/traces/{project_id}/{build_id}.log.zst")
}

/// How much of a compressed log's end is read for its frame table: all of it
/// up to 500 frames (a 500 MiB log), else a second read gets the rest.
const TABLE_READ: u64 = 4096;

/// Move a finished job's log to the blob store and drop its chunks.
pub async fn archive(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    if b.trace_archived {
        return Ok(());
    }
    let data = Bytes::from(forgust_db::ci::read_trace(&state.db, build_id, 0, i64::MAX).await?);
    let size = data.len() as i64;
    let log = data.clone();
    let encoded = tokio::task::spawn_blocking(move || seekable::encode(&log)).await??;
    // a log of a few bytes comes out larger: it stays as it is
    let (key, file_format, stored) = if encoded.len() < data.len() {
        (zstd_key(b.project_id, b.id), "zstd", Bytes::from(encoded))
    } else {
        (blob_key(b.project_id, b.id), "raw", data)
    };
    let stored_size = stored.len() as i64;
    state.blobs.put_bytes(&key, stored).await?;
    let mut tx = state.db.begin().await?;
    // the row is locked so a late PATCH cannot slip in between copy and delete
    let Some(locked) = forgust_db::ci::build_for_update(&mut tx, build_id).await? else { return Ok(()) };
    if locked.trace_size != size {
        // bytes arrived while we copied: try again on the next sweep
        tx.rollback().await?;
        anyhow::bail!("trace of job {build_id} grew during archiving");
    }
    forgust_db::ci::upsert_artifact(
        &mut tx,
        forgust_db::ci::NewArtifact {
            build_id,
            project_id: b.project_id,
            file_type: "trace",
            file_format,
            filename: "job.log",
            // what the blob store holds: storage pages and quotas count it
            size: stored_size,
            blob_key: &key,
            expire_at: None,
        },
    )
    .await?;
    forgust_db::ci::mark_trace_archived(&mut tx, build_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Where an archived log is, and whether it is compressed. Its artifact row
/// says; without one the log is where every log used to be, raw.
async fn archived(state: &AppState, build: &forgust_db::ci::Build) -> anyhow::Result<(String, bool)> {
    Ok(match forgust_db::ci::artifact(&state.db, build.id, "trace").await? {
        Some(a) => (a.blob_key, a.file_format == "zstd"),
        None => (blob_key(build.project_id, build.id), false),
    })
}

/// The frame table of the compressed log at `key`, `size` bytes long.
async fn table(state: &AppState, key: &str, size: u64) -> anyhow::Result<seekable::Table> {
    let tail = state.blobs.get_range(key, size.saturating_sub(TABLE_READ), size).await?;
    let need = seekable::table_len(&tail)? as u64;
    let tail = if need > tail.len() as u64 && need <= size { state.blobs.get_range(key, size - need, size).await? } else { tail };
    Ok(seekable::Table::parse(&tail, size)?)
}

/// At most `max` log bytes from `offset`, wherever they live now (an archived
/// log is read by range, never whole: a compressed one, the frames that hold
/// the range).
pub async fn read(state: &AppState, build: &forgust_db::ci::Build, offset: i64, max: i64) -> anyhow::Result<Vec<u8>> {
    if !build.trace_archived {
        return Ok(forgust_db::ci::read_trace(&state.db, build.id, offset, max).await?);
    }
    let (key, compressed) = archived(state, build).await?;
    let Some(size) = state.blobs.size(&key).await? else { return Ok(Vec::new()) };
    if !compressed {
        let start = (offset.max(0) as u64).min(size);
        let end = start.saturating_add(max.max(0) as u64).min(size);
        if start == end {
            return Ok(Vec::new());
        }
        return Ok(state.blobs.get_range(&key, start, end).await?.to_vec());
    }
    let table = table(state, &key, size).await?;
    let start = offset.max(0) as u64;
    let end = start.saturating_add(max.max(0) as u64);
    let frames = table.frames_for(start, end).to_vec();
    let (Some(first), Some(last)) = (frames.first(), frames.last()) else { return Ok(Vec::new()) };
    let bytes = state.blobs.get_range(&key, first.at, last.at + u64::from(last.compressed)).await?;
    Ok(tokio::task::spawn_blocking(move || seekable::decompress(&frames, &bytes, start, end)).await??)
}

/// A finished job's whole log, streamed (None when it is not archived or its
/// blob is gone): a compressed one a frame at a time.
pub async fn download(state: &AppState, build: &forgust_db::ci::Build) -> anyhow::Result<Option<axum::response::Response>> {
    use axum::http::{HeaderValue, header};
    if !build.trace_archived {
        return Ok(None);
    }
    let text = "text/plain; charset=utf-8";
    let (key, compressed) = archived(state, build).await?;
    if !compressed {
        return blob_download(state, &key, text, None).await;
    }
    let Some(size) = state.blobs.size(&key).await? else { return Ok(None) };
    let table = table(state, &key, size).await?;
    let body = match table.frames_end() {
        0 => futures::stream::empty().boxed(),
        end => state.blobs.get_range_stream(&key, 0, end).await?,
    };
    let mut r = axum::response::Response::new(axum::body::Body::from_stream(frames_of(table.frames, body)));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(text));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(table.size));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(Some(r))
}

/// The log bytes of `frames`, decompressed one at a time as `body` (the
/// file from its first frame) streams in.
fn frames_of(frames: Vec<seekable::Frame>, body: ByteStream) -> ByteStream {
    futures::stream::try_unfold((body, BytesMut::new(), frames.into_iter()), |(mut body, mut buf, mut frames)| async move {
        let Some(f) = frames.next() else { return Ok(None) };
        while buf.len() < f.compressed as usize {
            match body.next().await {
                Some(chunk) => buf.extend_from_slice(&chunk?),
                None => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the stored job log is cut short")),
            }
        }
        let compressed = buf.split_to(f.compressed as usize).freeze();
        let log =
            tokio::task::spawn_blocking(move || seekable::decompress_frame(&f, &compressed)).await.map_err(std::io::Error::other)??;
        Ok(Some((Bytes::from(log), (body, buf, frames))))
    })
    .boxed()
}

/// Stream a stored blob (None when it is gone): an attachment named
/// `filename`, or shown inline without one.
pub async fn blob_download(
    state: &AppState,
    key: &str,
    content_type: &'static str,
    filename: Option<&str>,
) -> anyhow::Result<Option<axum::response::Response>> {
    use axum::http::{HeaderValue, header};
    let Some((size, stream)) = state.blobs.get(key).await? else { return Ok(None) };
    let mut r = axum::response::Response::new(axum::body::Body::from_stream(stream));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if let Some(name) = filename
        && let Ok(v) = HeaderValue::from_str(&crate::blob::disposition(name, false))
    {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(Some(r))
}

/// Archive in the background after a job finishes.
pub fn archive_later(state: &AppState, build_id: i64) {
    let state = state.clone();
    tokio::spawn(async move {
        // let the runner's last PATCH land first
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Err(e) = archive(&state, build_id).await {
            tracing::warn!(build_id, error = %e, "trace not archived yet (the sweep retries)");
        }
    });
}
