//! Keeping the index in step with each project's default branch.
//!
//! One job per project (`jobs::IndexCode`), all in one queue: tantivy allows a
//! single writer. The job compares the head of the default branch with the
//! commit the index holds and writes the difference, or the whole tree when
//! there is nothing to compare with. It is queued after a push to the default
//! branch, a change of default branch, an import and a deletion; the hourly
//! sweep queues what those missed (`queue_due`).

use std::sync::Arc;

use chrono::Utc;
use forgust_storage::BlobChange;

use super::index::CodeIndex;
use super::searchable_text;
use crate::state::AppState;

/// Files of one project the index takes; the rest of a larger tree is left out.
const MAX_FILES: usize = 200_000;
/// Blobs read from git before they are handed to the writer.
const BATCH: usize = 64;
/// Projects the sweep queues in one pass.
const SWEEP_BATCH: i64 = 2000;

fn is_regular(mode: u32) -> bool {
    mode == 0o100644 || mode == 0o100755
}

/// Bring the index up to date for one project.
pub async fn index_project(state: &AppState, project_id: i64) -> anyhow::Result<()> {
    let Some(index) = &state.code_index else { return Ok(()) };
    let started = Utc::now();
    let Some(project) = forgust_db::projects::find(&state.db, project_id).await? else {
        // deleted: its files leave the index, then its row
        remove_project(index, project_id).await?;
        forgust_db::code_search::delete_state(&state.db, project_id).await?;
        return Ok(());
    };
    let prior = forgust_db::code_search::state(&state.db, project_id).await?.filter(|s| s.generation == index.generation());
    let branch = project.default_branch.as_deref();
    let head = match branch {
        Some(b) => match state.storage.resolve(project_id, &format!("refs/heads/{b}")).await {
            Ok(head) => head,
            Err(forgust_storage::StorageError::RepoNotFound(_)) => None,
            Err(e) => return Err(e.into()),
        },
        None => None,
    };
    let held = prior.as_ref().and_then(|s| s.indexed_commit.as_deref());
    if prior.is_some() && held == head.as_deref() {
        // nothing moved: only note that we looked
        forgust_db::code_search::set_state(&state.db, project_id, index.generation(), held, branch, started).await?;
        return Ok(());
    }
    let Some(head) = head else {
        remove_project(index, project_id).await?;
        forgust_db::code_search::set_state(&state.db, project_id, index.generation(), None, branch, started).await?;
        return Ok(());
    };
    let max_bytes = state.cfg.code_search.max_file_bytes;
    // the paths to write: what changed since the commit we hold, or everything
    let changes = match held {
        Some(old) => state.storage.changed_blobs(project_id, old, &head).await.ok(),
        None => None,
    };
    let (changes, rebuild) = match changes {
        Some(changes) => (changes, false),
        None => {
            let (blobs, cut) = state.storage.tree_blobs(project_id, &head, MAX_FILES).await?;
            if cut {
                tracing::warn!(project_id, "more than {MAX_FILES} files: the rest is not in the code index");
            }
            // what cannot be indexed is left out here: nothing of it is read
            let wanted = blobs.into_iter().filter(|b| is_regular(b.mode) && b.size <= max_bytes);
            (wanted.map(|b| BlobChange { path: b.path, mode: b.mode, oid: Some(b.oid) }).collect(), true)
        }
    };
    let writer = Arc::new(index.writer()?);
    if rebuild {
        writer.delete_project(project_id);
    }
    let (mut indexed, mut bytes) = (0u64, 0u64);
    for batch in changes.chunks(BATCH) {
        let (read, gone): (Vec<&BlobChange>, Vec<&BlobChange>) = batch.iter().partition(|c| c.oid.is_some() && is_regular(c.mode));
        let oids: Vec<String> = read.iter().filter_map(|c| c.oid.clone()).collect();
        // one byte more than the limit is enough to see a file is over it
        let blobs = state.storage.blobs_by_id(project_id, &oids, max_bytes as usize + 1).await?;
        let mut put = Vec::new();
        let mut drop: Vec<String> = gone.into_iter().map(|c| c.path.clone()).collect();
        for (change, blob) in read.into_iter().zip(blobs) {
            match blob.and_then(|(size, data)| searchable_text(size, &data, max_bytes)) {
                Some(text) => {
                    indexed += 1;
                    bytes += text.len() as u64;
                    put.push((change.path.clone(), change.oid.clone().unwrap_or_default(), text));
                }
                None => drop.push(change.path.clone()),
            }
        }
        let w = writer.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if !rebuild {
                for path in &drop {
                    w.delete_file(project_id, path);
                }
            }
            for (path, oid, text) in &put {
                w.put_file(project_id, path, oid, crate::languages::file_type_of(path), text)?;
            }
            Ok(())
        })
        .await??;
    }
    let writer = Arc::try_unwrap(writer).map_err(|_| anyhow::anyhow!("the index writer is still in use"))?;
    tokio::task::spawn_blocking(move || writer.commit()).await??;
    forgust_db::code_search::set_state(&state.db, project_id, index.generation(), Some(&head), branch, started).await?;
    metrics::counter!("forgust_code_index_files_total").increment(indexed);
    tracing::debug!(project_id, files = indexed, bytes, rebuild, "code indexed");
    Ok(())
}

/// Take a project's files out of the index, off the async threads.
async fn remove_project(index: &Arc<CodeIndex>, project_id: i64) -> anyhow::Result<()> {
    let index = index.clone();
    tokio::task::spawn_blocking(move || {
        let writer = index.writer()?;
        writer.delete_project(project_id);
        writer.commit()
    })
    .await?
}

/// Queue the index job of one project. Jobs already waiting for it are replaced.
pub async fn queue(state: &AppState, project_id: i64) {
    if state.code_index.is_none() {
        return;
    }
    let queued = async {
        let mut conn = state.db.acquire().await?;
        state.jobs.enqueue(&mut conn, crate::jobs::IndexCode { project_id }.request()).await
    };
    if let Err(e) = queued.await {
        // the hourly sweep will find it
        tracing::warn!(project_id, error = %e, "code index job not queued");
    }
}

/// Queue every project the index is behind on: never indexed, indexed into
/// another index, active since, or deleted. Returns how many.
pub async fn queue_due(state: &AppState) -> anyhow::Result<usize> {
    let Some(index) = &state.code_index else { return Ok(0) };
    let due = forgust_db::code_search::due(&state.db, index.generation(), SWEEP_BATCH).await?;
    let mut tx = state.db.begin().await?;
    for project_id in &due {
        state.jobs.enqueue(&mut tx, crate::jobs::IndexCode { project_id: *project_id }.request()).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

/// Forget what was indexed and queue everything again (Admin > Background jobs).
pub async fn reindex_all(state: &AppState) -> anyhow::Result<usize> {
    forgust_db::code_search::clear(&state.db).await?;
    queue_due(state).await
}
