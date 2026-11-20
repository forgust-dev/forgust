//! SBOMs of tags: with security scanning and `sbom_on_tags` on, each new tag
//! gets a built-in pipeline (source `sbom`: never a scan, never a status of
//! the tag) whose job writes a CycloneDX and an SPDX file with the scanners'
//! Trivy. Once it passed, the files become uploads of the project and links
//! of the tag's release, now or when the release is created.

use forgust_db::projects::Project;
use forgust_db::security as sdb;
use graphile_worker::{IntoTaskHandlerResult, TaskHandler, WorkerContext};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{ApiError, ApiResult};
use crate::jobs::JobRequest;
use crate::state::AppState;

pub const SOURCE: &str = "sbom";
const JOB: &str = "sbom";
/// New tags in one push that get an SBOM (`git push --tags` of a history).
const MAX_TAGS_PER_PUSH: usize = 5;
/// Per file: an SBOM is text, not a build output.
const MAX_FILE: u64 = 50 << 20;
/// `(file in the job, name of the release link, asset path)`.
const FILES: [(&str, &str, &str); 2] =
    [("sbom.cdx.json", "SBOM (CycloneDX)", "/sbom.cdx.json"), ("sbom.spdx.json", "SBOM (SPDX)", "/sbom.spdx.json")];

/// The pipeline: one job, both formats. `mirror`: Java's index from here.
pub fn job_config(cfg: &crate::config::SecurityScanningConfig, mirror: &super::vulnerability_dbs::TrivyFlags) -> String {
    let run = |format: &str, file: &str| {
        format!("trivy fs --config /dev/null --format {format} --output {file} --exit-code 0{} .", mirror.sbom())
    };
    json!({
        JOB: {
            "image": { "name": cfg.trivy_image, "entrypoint": [""] },
            "tags": cfg.tags,
            "timeout": format!("{} minutes", cfg.timeout_secs.div_ceil(60).max(1)),
            "rules": [{ "when": "always" }],
            "variables": { "TRIVY_CACHE_DIR": ".trivycache", "TRIVY_NO_PROGRESS": "true" },
            "cache": { "key": "trivy", "paths": [".trivycache/"], "when": "always" },
            "script": [run("cyclonedx", FILES[0].0), run("spdx-json", FILES[1].0)],
            // the server keeps its copies: these go soon
            "artifacts": { "paths": [FILES[0].0, FILES[1].0], "expire_in": "1 day" },
        }
    })
    .to_string()
}

/// Tags a push created (`(name, the ref's new value)`): an SBOM run for each,
/// the first few of a push, once per tag and commit.
pub async fn on_tags_created(state: &AppState, project: &Project, tags: &[(String, String)]) {
    if tags.is_empty() || project.archived() {
        return;
    }
    let r: anyhow::Result<()> = async {
        let Some(s) = sdb::scanning(&state.db, project.id).await?.filter(|s| s.enabled && s.sbom_on_tags) else { return Ok(()) };
        let cfg = super::scanning::config(state).await?;
        if !cfg.enabled {
            return Ok(());
        }
        if tags.len() > MAX_TAGS_PER_PUSH {
            tracing::info!(project_id = project.id, tags = tags.len(), "SBOMs for the first {MAX_TAGS_PER_PUSH} tags of the push only");
        }
        for (tag, sha) in tags.iter().take(MAX_TAGS_PER_PUSH) {
            let Some(id) = sdb::claim_sbom(&state.db, project.id, tag, sha).await? else { continue };
            match start(state, project, &s, &cfg, tag, sha).await {
                Ok(Some(pipeline_id)) => {
                    // unrecorded, the run's files could never be kept: free
                    // the tag for another run
                    if let Err(e) = sdb::sbom_started(&state.db, id, pipeline_id).await {
                        tracing::warn!(project_id = project.id, tag, pipeline_id, error = %e, "SBOM run not recorded");
                        sdb::unclaim_sbom(&state.db, id).await?;
                    }
                }
                Ok(None) => sdb::unclaim_sbom(&state.db, id).await?,
                Err(e) => {
                    sdb::unclaim_sbom(&state.db, id).await?;
                    tracing::info!(project_id = project.id, tag, error = ?e, "no SBOM for the tag");
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(project_id = project.id, error = %format!("{e:#}"), "SBOMs of new tags not started");
    }
}

async fn start(
    state: &AppState,
    project: &Project,
    s: &sdb::ScanningSettings,
    cfg: &crate::config::SecurityScanningConfig,
    tag: &str,
    sha: &str,
) -> ApiResult<Option<i64>> {
    // as the scans: run by the project's scanner bot, while who turned
    // scanning on may still read the code
    let Some(bot) = super::scanning::run_as(state, project, s).await? else {
        return Err(ApiError::conflict("who turned security scanning on can no longer read this project"));
    };
    let mirror = super::vulnerability_dbs::trivy_flags(state, cfg).await.map_err(ApiError::Internal)?;
    let created = crate::ci::pipeline::create(
        state,
        crate::ci::pipeline::Trigger {
            tag: true,
            config: Some(job_config(cfg, &mirror)),
            ..crate::ci::pipeline::Trigger::new(project.id, tag, sha, SOURCE, Some(bot.id))
        },
    )
    .await
    .map_err(ApiError::Internal)?;
    Ok(created.id())
}

/// A pipeline ended: an SBOM run that passed has its files kept.
pub async fn on_pipeline_finished(state: &AppState, pipeline: &forgust_db::ci::Pipeline, status: &str) {
    if pipeline.source != SOURCE || status != "success" {
        return;
    }
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => return tracing::warn!(pipeline_id = pipeline.id, error = %e, "SBOM not kept"),
    };
    let queued = state.jobs.enqueue(&mut tx, StoreSbom { pipeline_id: pipeline.id }.request()).await;
    if let Err(e) = match queued {
        Ok(()) => tx.commit().await.map_err(anyhow::Error::from),
        Err(e) => Err(e),
    } {
        tracing::warn!(pipeline_id = pipeline.id, error = %e, "SBOM not kept");
    }
}

/// Copy a passed SBOM run's files into the project's uploads, then attach.
#[derive(Debug, Serialize, Deserialize)]
pub struct StoreSbom {
    pub pipeline_id: i64,
}

impl StoreSbom {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            payload: serde_json::to_value(&self).unwrap_or_default(),
            job_key: Some(format!("sbom:{}", self.pipeline_id)),
            ..Default::default()
        }
    }
}

impl TaskHandler for StoreSbom {
    const IDENTIFIER: &'static str = "store_sbom";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        store(state, self.pipeline_id).await.map_err(|e| format!("{e:#}"))
    }
}

async fn store(state: &AppState, pipeline_id: i64) -> anyhow::Result<()> {
    let Some(row) = sdb::sbom_of_pipeline(&state.db, pipeline_id).await? else { return Ok(()) };
    let Some(p) = forgust_db::projects::find(&state.db, row.project_id).await? else { return Ok(()) };
    if row.cyclonedx_upload_id.is_none() || row.spdx_upload_id.is_none() {
        let builds = forgust_db::ci::builds_of_pipeline(&state.db, pipeline_id).await?;
        let Some(job) = builds.iter().rev().find(|b| b.name == JOB && !b.retried && b.status == "success") else { return Ok(()) };
        let Some(archive) = forgust_db::ci::artifacts_of_build(&state.db, job.id).await?.into_iter().find(|a| a.file_type == "archive")
        else {
            return Ok(());
        };
        let (entries, _) = crate::ci::archive::entries(state, &archive.blob_key).await?;
        let mut found = Vec::new();
        for (file, ..) in FILES {
            let Some(e) = entries.iter().find(|e| e.name == file) else {
                tracing::info!(pipeline_id, file, "the SBOM job left no such file");
                return Ok(());
            };
            if e.size > MAX_FILE {
                tracing::info!(pipeline_id, file, size = e.size, "SBOM too large to keep");
                return Ok(());
            }
            found.push(e.clone());
        }
        // what the server writes counts against the project's storage too
        let total: u64 = found.iter().map(|e| e.size).sum();
        if let Err(e) = super::quotas::check_write(state, p.id, total).await {
            tracing::info!(project_id = p.id, error = %e, "SBOM not kept: the storage quota is reached");
            return Ok(());
        }
        // both files stored, then both rows at once: a retry after a failure
        // leaves nothing behind
        let mut stored = Vec::new();
        for e in &found {
            let secret = hex::encode(rand::random::<[u8; 16]>());
            let key = format!("uploads/{}/{secret}", p.id);
            let put = match crate::ci::archive::open(state, &archive.blob_key, e).await {
                Ok(stream) => state.blobs.put_stream(&key, stream).await,
                Err(e) => Err(e),
            };
            match put {
                Ok(size) => stored.push((secret, key, size)),
                Err(e) => {
                    forget(state, &stored).await;
                    let _ = state.blobs.delete(&key).await;
                    return Err(e);
                }
            }
        }
        if let Err(e) = keep(state, &p, &row, &stored).await {
            forget(state, &stored).await;
            return Err(e);
        }
    }
    attach(state, &p, &row.tag).await
}

/// The uploads of a run's files, and the run's record of them, together.
async fn keep(state: &AppState, p: &Project, row: &sdb::TagSbom, stored: &[(String, String, u64)]) -> anyhow::Result<()> {
    let tag: String = row.tag.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    let mut tx = state.db.begin().await?;
    let mut ids = Vec::new();
    for ((secret, key, size), (file, ..)) in stored.iter().zip(FILES) {
        let filename = format!("{tag}-{file}");
        let u = forgust_db::releases::add_upload(
            &mut *tx,
            forgust_db::releases::NewUpload {
                project_id: p.id,
                secret,
                filename: &filename,
                size: *size as i64,
                content_type: "application/json",
                blob_key: key,
                uploaded_by: None,
            },
        )
        .await?;
        ids.push(u.id);
    }
    sdb::sbom_files(&mut *tx, row.id, ids[0], ids[1]).await?;
    tx.commit().await?;
    Ok(())
}

async fn forget(state: &AppState, stored: &[(String, String, u64)]) {
    for (_, key, _) in stored {
        let _ = state.blobs.delete(key).await;
    }
}

/// Link the SBOM of the commit the tag points to now from its release, in
/// place of an older one of the tag Forgust linked (a link of the same name
/// or path someone else made stays; a release at its limit gets none).
pub async fn attach(state: &AppState, p: &Project, tag: &str) -> anyhow::Result<()> {
    let Some(release) = forgust_db::releases::find(&state.db, p.id, tag).await? else { return Ok(()) };
    // the ref's own value (the tag object when annotated), as the push gave it
    let refname = format!("refs/tags/{tag}");
    let Some(sha) = state.storage.refs(p.id, &refname).await?.into_iter().find(|r| r.name == refname).map(|r| r.target) else {
        return Ok(());
    };
    let Some(row) = sdb::sbom_of_tag(&state.db, p.id, tag, &sha).await? else { return Ok(()) };
    let mut wanted = Vec::new();
    for (upload_id, (_, name, path)) in [row.cyclonedx_upload_id, row.spdx_upload_id].into_iter().zip(FILES) {
        let Some(u) = forgust_db::releases::upload_by_id(&state.db, p.id, upload_id.unwrap_or_default()).await? else { continue };
        wanted.push((name, path, super::uploads::full_path(&u)));
    }
    // links to an older SBOM of the tag (it moved since) go
    let ours: Vec<String> = {
        let mut urls = Vec::new();
        for id in sdb::sbom_uploads_of_tag(&state.db, p.id, tag).await? {
            if let Some(u) = forgust_db::releases::upload_by_id(&state.db, p.id, id).await? {
                urls.push(super::uploads::full_path(&u));
            }
        }
        urls
    };
    let mut links = forgust_db::releases::links(&state.db, &[release.id]).await?;
    for l in links.extract_if(.., |l| ours.contains(&l.url) && !wanted.iter().any(|(_, _, url)| *url == l.url)) {
        forgust_db::releases::delete_link(&state.db, release.id, l.id).await?;
    }
    for (name, path, url) in wanted {
        if links.iter().any(|l| l.url == url) {
            continue;
        }
        if links.len() >= super::releases::MAX_LINKS {
            tracing::info!(project_id = p.id, tag, "the release has as many links as it may: no SBOM link");
            break;
        }
        if let Some(l) = forgust_db::releases::add_link(&state.db, release.id, name, &url, Some(path), "other").await? {
            links.push(l);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipeline_parses() {
        let cfg = crate::config::SecurityScanningConfig::default();
        let text = job_config(&cfg, &Default::default());
        let parsed = forgust_ci_config::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(parsed.jobs.keys().collect::<Vec<_>>(), ["sbom"]);
        assert!(text.contains("--format cyclonedx --output sbom.cdx.json"), "{text}");
        assert!(text.contains("--format spdx-json --output sbom.spdx.json"), "{text}");
    }
}
