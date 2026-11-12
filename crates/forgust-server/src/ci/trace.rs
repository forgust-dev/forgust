//! Job logs: live chunks in Postgres while the job runs, one blob when it ends
//! (ADR-0014).

use bytes::Bytes;

use crate::state::AppState;

pub fn blob_key(project_id: i64, build_id: i64) -> String {
    format!("ci/traces/{project_id}/{build_id}.log")
}

/// Move a finished job's log to the blob store and drop its chunks.
pub async fn archive(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    if b.trace_archived {
        return Ok(());
    }
    let data = forgust_db::ci::read_trace(&state.db, build_id, 0, i64::MAX).await?;
    let key = blob_key(b.project_id, b.id);
    let size = data.len() as i64;
    state.blobs.put_bytes(&key, Bytes::from(data)).await?;
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
            file_format: "raw",
            filename: "job.log",
            size,
            blob_key: &key,
            expire_at: None,
        },
    )
    .await?;
    forgust_db::ci::mark_trace_archived(&mut tx, build_id).await?;
    tx.commit().await?;
    Ok(())
}

/// At most `max` log bytes from `offset`, wherever they live now (an archived
/// log is read by range, never whole).
pub async fn read(state: &AppState, build: &forgust_db::ci::Build, offset: i64, max: i64) -> anyhow::Result<Vec<u8>> {
    if !build.trace_archived {
        return Ok(forgust_db::ci::read_trace(&state.db, build.id, offset, max).await?);
    }
    let key = blob_key(build.project_id, build.id);
    let Some(size) = state.blobs.size(&key).await? else { return Ok(Vec::new()) };
    let start = (offset.max(0) as u64).min(size);
    let end = start.saturating_add(max.max(0) as u64).min(size);
    if start == end {
        return Ok(Vec::new());
    }
    Ok(state.blobs.get_range(&key, start, end).await?.to_vec())
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
