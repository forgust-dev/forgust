//! SBOMs of tags: with security scanning and `sbom_on_tags` on, each tag
//! pushed (new, or moved to another commit) gets a built-in pipeline (source `sbom`: never a scan, never a status of
//! the tag) whose job writes a CycloneDX and an SPDX file with the scanners'
//! Trivy. Once it passed, the files become uploads of the project (made from
//! the code: for those who read it) and links of the tag's release, now or
//! when the release is created. They last as long as the tag points to their
//! commit: moved or deleted, its older SBOMs go, files and links.

use anyhow::Context;
use forgust_db::projects::Project;
use forgust_db::security as sdb;
use graphile_worker::{IntoTaskHandlerResult, TaskHandler, WorkerContext};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::jobs::JobRequest;
use crate::state::AppState;

pub const SOURCE: &str = "sbom";
const JOB: &str = "sbom";
/// New tags in one push that get an SBOM (`git push --tags` of a history).
const MAX_TAGS_PER_PUSH: usize = 5;
/// Per file: an SBOM is text, not a build output.
const MAX_FILE: u64 = 50 << 20;
/// `(file in the job, name of the release link, asset path)`, CycloneDX first.
const FILES: [(&str, &str, &str); 2] =
    [("sbom.cdx.json", "SBOM (CycloneDX)", "/sbom.cdx.json"), ("sbom.spdx.json", "SBOM (SPDX)", "/sbom.spdx.json")];

/// The pipeline: one job, both formats (the mirror gives Java's index).
pub fn job_config(cfg: &crate::config::SecurityScanningConfig, mirror: &super::vulnerability_dbs::TrivyMirror) -> String {
    // neither run reads what the job itself wrote (Trivy 0.57 does not; a
    // configured image may)
    let skip = FILES.map(|(file, ..)| format!("--skip-files {file}")).join(" ");
    let run = |format: &str, file: &str| format!("trivy fs --config /dev/null {skip} --format {format} --output {file} --exit-code 0 .");
    let extra = json!({
        "script": [run("cyclonedx", FILES[0].0), run("spdx-json", FILES[1].0)],
        // the server keeps its copies: these go soon
        "artifacts": { "paths": [FILES[0].0, FILES[1].0], "expire_in": "1 day" },
    });
    json!({ JOB: super::scanning::trivy_job(cfg, mirror, json!({}), extra) }).to_string()
}

/// Tags a push created or moved (`(name, the ref's new value)`): the SBOMs
/// of the commits they left go, and each gets an SBOM run, the first few of a
/// push, once per tag and commit.
pub async fn on_tags_pushed(state: &AppState, project: &Project, tags: &[(String, String)]) {
    for (tag, sha) in tags {
        if let Err(e) = forget(state, project, tag, Some(sha)).await {
            tracing::warn!(project_id = project.id, tag, error = %format!("{e:#}"), "older SBOMs of the tag not removed");
        }
    }
    if tags.is_empty() || project.archived() {
        return;
    }
    if let Err(e) = start_all(state, project, tags).await {
        tracing::warn!(project_id = project.id, error = %format!("{e:#}"), "SBOMs of pushed tags not started");
    }
}

/// Tags deleted: their SBOMs go with them (their releases went already).
pub async fn on_tags_deleted(state: &AppState, project: &Project, tags: &[String]) {
    for tag in tags {
        if let Err(e) = forget(state, project, tag, None).await {
            tracing::warn!(project_id = project.id, tag, error = %format!("{e:#}"), "SBOMs of the deleted tag not removed");
        }
    }
}

/// Drop the SBOMs of `tag` (all, or those of another commit than `keep`):
/// the release's links to them first (found by their files), then the runs
/// and files; runs still going stop.
async fn forget(state: &AppState, p: &Project, tag: &str, keep: Option<&str>) -> anyhow::Result<()> {
    let files = sdb::sbom_files_of_tag(&state.db, p.id, tag).await?;
    let stale: Vec<String> =
        files.iter().filter(|f| keep != Some(f.sha.as_str())).map(|f| super::uploads::path_of(p.id, &f.secret, &f.filename)).collect();
    if !stale.is_empty()
        && let Some(release) = forgust_db::releases::find(&state.db, p.id, tag).await?
    {
        for l in forgust_db::releases::links(&state.db, &[release.id]).await? {
            if stale.contains(&l.url) {
                forgust_db::releases::delete_link(&state.db, release.id, l.id).await?;
            }
        }
    }
    let mut tx = state.db.begin().await?;
    let pipelines = sdb::drop_sboms(&mut tx, p.id, tag, keep).await?;
    tx.commit().await?;
    let active = sdb::active_sbom_runs(&state.db, p.id).await?;
    for id in pipelines.into_iter().filter(|id| active.contains(id)) {
        if let Err(e) = crate::ci::pipeline::cancel(state, id).await {
            tracing::warn!(pipeline_id = id, error = %format!("{e:#}"), "SBOM run of a tag that moved not canceled");
        }
    }
    Ok(())
}

/// Stop a project's SBOM runs (scanning, or SBOMs on tags, turned off).
pub async fn cancel_runs(state: &AppState, project_id: i64) -> anyhow::Result<()> {
    for id in sdb::active_sbom_runs(&state.db, project_id).await? {
        crate::ci::pipeline::cancel(state, id).await?;
    }
    Ok(())
}

async fn start_all(state: &AppState, project: &Project, tags: &[(String, String)]) -> anyhow::Result<()> {
    let Some(s) = sdb::scanning(&state.db, project.id).await?.filter(|s| s.enabled && s.sbom_on_tags) else { return Ok(()) };
    let cfg = super::scanning::config(state).await?;
    if !cfg.enabled {
        return Ok(());
    }
    // as the scans: by the project's scanner bot, while who turned scanning
    // on may still read the code
    let Some(bot) = super::scanning::run_as(state, project, &s).await? else {
        tracing::info!(project_id = project.id, "no SBOMs: who turned security scanning on can no longer read the project");
        return Ok(());
    };
    let config = job_config(&cfg, &super::vulnerability_dbs::trivy_mirror(state, &cfg).await?);
    if tags.len() > MAX_TAGS_PER_PUSH {
        tracing::info!(project_id = project.id, tags = tags.len(), "SBOMs for the first {MAX_TAGS_PER_PUSH} tags of the push only");
    }
    for (tag, sha) in tags.iter().take(MAX_TAGS_PER_PUSH) {
        let Some(id) = sdb::claim_sbom(&state.db, project.id, tag, sha).await? else { continue };
        let started: anyhow::Result<()> = async {
            let trigger = crate::ci::pipeline::Trigger {
                tag: true,
                config: Some(config.clone()),
                ..crate::ci::pipeline::Trigger::new(project.id, tag, sha, SOURCE, Some(bot.id))
            };
            let pipeline_id = crate::ci::pipeline::create(state, trigger).await?.id().context("no pipeline")?;
            Ok(sdb::sbom_started(&state.db, id, pipeline_id).await?)
        }
        .await;
        // not started, or unrecorded (its files could never be kept): the
        // tag is free for another run
        if let Err(e) = started {
            tracing::info!(project_id = project.id, tag, error = %format!("{e:#}"), "no SBOM for the tag");
            sdb::unclaim_sbom(&state.db, id).await?;
        }
    }
    Ok(())
}

/// A pipeline ended: an SBOM run that passed has its files kept.
pub async fn on_pipeline_finished(state: &AppState, pipeline: &forgust_db::ci::Pipeline, status: &str) {
    if pipeline.source != SOURCE || status != "success" {
        return;
    }
    let queued: anyhow::Result<()> = async {
        let mut conn = state.db.acquire().await?;
        state.jobs.enqueue(&mut conn, StoreSbom { pipeline_id: pipeline.id }.request()).await
    }
    .await;
    if let Err(e) = queued {
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
        copy_files(state, &p, &row, pipeline_id).await?;
    }
    attach(state, &p, &row.tag).await
}

/// The job's two files into uploads: both blobs stored, then both rows and
/// the run's record of them at once (a retry after a failure leaves nothing).
async fn copy_files(state: &AppState, p: &Project, row: &sdb::TagSbom, pipeline_id: i64) -> anyhow::Result<()> {
    let builds = forgust_db::ci::builds_of_pipeline(&state.db, pipeline_id).await?;
    let Some(job) = builds.iter().rev().find(|b| b.name == JOB && !b.retried && b.status == "success") else { return Ok(()) };
    let Some(archive) = forgust_db::ci::artifact(&state.db, job.id, "archive").await? else { return Ok(()) };
    let (entries, _) = crate::ci::archive::entries(state, &archive.blob_key).await?;
    let mut found = Vec::new();
    for (file, ..) in FILES {
        let Some(e) = entries.iter().find(|e| e.name == file && e.size <= MAX_FILE) else {
            tracing::info!(pipeline_id, file, "the SBOM job left no such file, or too large a one");
            return Ok(());
        };
        found.push(e);
    }
    // what the server writes counts against the project's storage too
    if let Err(e) = super::quotas::check_write(state, p.id, found.iter().map(|e| e.size).sum()).await {
        tracing::info!(project_id = p.id, error = %e, "SBOM not kept: the storage quota is reached");
        return Ok(());
    }
    let mut stored: Vec<(String, String, u64)> = Vec::new();
    for e in found {
        let (secret, key) = super::uploads::new_key(p.id);
        let put = async { state.blobs.put_stream(&key, crate::ci::archive::open(state, &archive.blob_key, e).await?).await }.await;
        match put {
            Ok(size) => stored.push((secret, key, size)),
            Err(err) => {
                let _ = state.blobs.delete(&key).await;
                discard(state, &stored).await;
                return Err(err);
            }
        }
    }
    let kept = async {
        let tag = crate::blob::ascii_name(&row.tag);
        let mut tx = state.db.begin().await?;
        let mut ids = Vec::new();
        for ((secret, key, size), (file, ..)) in stored.iter().zip(FILES) {
            let u = forgust_db::releases::add_upload(
                &mut *tx,
                forgust_db::releases::NewUpload {
                    project_id: p.id,
                    secret,
                    filename: &format!("{tag}-{file}"),
                    size: *size as i64,
                    content_type: "application/json",
                    blob_key: key,
                    uploaded_by: None,
                    requires_code: true,
                },
            )
            .await?;
            ids.push(u.id);
        }
        // the tag moved or went while the files were copied: nothing is kept
        if !sdb::sbom_files(&mut *tx, row.id, ids[0], ids[1]).await? {
            return Ok(false);
        }
        tx.commit().await?;
        anyhow::Ok(true)
    }
    .await;
    if !matches!(kept, Ok(true)) {
        discard(state, &stored).await;
    }
    kept.map(|_| ())
}

async fn discard(state: &AppState, stored: &[(String, String, u64)]) {
    for (_, key, _) in stored {
        let _ = state.blobs.delete(key).await;
    }
}

/// Link the SBOM of the commit the tag points to now from its release, in
/// place of an older one of the tag Forgust linked (a link of the same name
/// or path someone else made stays; a release at its limit gets none).
pub async fn attach(state: &AppState, p: &Project, tag: &str) -> anyhow::Result<()> {
    let files = sdb::sbom_files_of_tag(&state.db, p.id, tag).await?;
    if files.is_empty() {
        return Ok(());
    }
    let Some(release) = forgust_db::releases::find(&state.db, p.id, tag).await? else { return Ok(()) };
    // the ref's own value (the tag object when annotated), as the push gave it
    let refname = format!("refs/tags/{tag}");
    let Some(sha) = state.storage.refs(p.id, &refname).await?.into_iter().find(|r| r.name == refname).map(|r| r.target) else {
        return Ok(());
    };
    let url = |f: &sdb::SbomFile| super::uploads::path_of(p.id, &f.secret, &f.filename);
    let mut current: Vec<_> = files.iter().filter(|f| f.sha == sha).collect();
    current.sort_by_key(|f| !f.cyclonedx);
    let wanted: Vec<_> = current.into_iter().zip(FILES).map(|(f, (_, name, path))| (name, path, url(f))).collect();
    // links to an older SBOM of the tag (it moved since) go
    let ours: Vec<String> = files.iter().map(url).collect();
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
        let mirror =
            crate::services::vulnerability_dbs::TrivyMirror(vec![("TRIVY_JAVA_DB_REPOSITORY", "h/admin/mirrors/trivy-java-db:1".into())]);
        let text = job_config(&cfg, &mirror);
        let parsed = forgust_ci_config::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(parsed.jobs.keys().collect::<Vec<_>>(), ["sbom"]);
        assert!(text.contains("--format cyclonedx --output sbom.cdx.json"), "{text}");
        assert!(text.contains("--format spdx-json --output sbom.spdx.json"), "{text}");
        let vars = &parsed.jobs["sbom"].variables;
        assert_eq!(vars.get("TRIVY_JAVA_DB_REPOSITORY").map(String::as_str), Some("h/admin/mirrors/trivy-java-db:1"));
    }
}
