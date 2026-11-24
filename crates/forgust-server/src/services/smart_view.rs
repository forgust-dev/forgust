//! The Smart View's maps. With it on for a project, Forgust runs a pipeline of
//! its own (source `smart_view`) on the instance's runners, as a bot that is a
//! Reporter of the project, on each new commit of the default branch and of an
//! open merge request. Its job runs the assay image (`crates/forgust-assay`,
//! released with the server under the same version) on a full clone, and
//! uploads the map as a report ([`report`]): the contract between the two.
//!
//! The server keeps what it read of the report, not the job's artifact, and
//! only the latest map of each: one that is ready takes the place of the older
//! ones of the same branch or merge request, a commit pushed over one still
//! being mapped has that run given up, and the maps of a merge request that is
//! merged or closed, or of a branch that is no longer the default one, go
//! ([`sweep`]); those of a project go when it turns the Smart View off.

pub use forgust_assay::report;

use forgust_db::merge_requests::MergeRequest;
use forgust_db::projects::Project;
use forgust_db::smart_view as db;
use forgust_db::users::User;
use report::Report;
use serde_json::json;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::config::SmartViewConfig;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const SOURCE: &str = "smart_view";
/// The report kind (`artifacts:reports:smart_view`).
pub const REPORT: &str = "smart_view";
const JOB: &str = "smart_view";
/// The user type of the projects' Smart View bots.
pub const BOT_TYPE: &str = "smart_view_bot";

/// What an administrator set over the config file's `[smart_view]` (Admin >
/// Smart View), saved whole: when there is none, the file's values hold.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Overrides {
    pub enabled: bool,
    /// Only one set on purpose: without it, the config file's, by default the
    /// assay image released with this server, which moves with each upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub tags: Vec<String>,
    pub timeout_secs: u64,
}

/// The settings in effect: the administrator's, over the config file's field
/// by field.
pub async fn config(state: &AppState) -> anyhow::Result<SmartViewConfig> {
    Ok(config_of(state, db::instance(&state.db).await?))
}

/// [`config`], with the saved settings already read.
pub fn config_of(state: &AppState, saved: serde_json::Value) -> SmartViewConfig {
    let file = &state.cfg.smart_view;
    over(file, saved).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the Smart View settings saved in the admin area do not read: the config file's hold");
        file.clone()
    })
}

fn over(file: &SmartViewConfig, saved: serde_json::Value) -> anyhow::Result<SmartViewConfig> {
    let serde_json::Value::Object(saved) = saved else { anyhow::bail!("not an object") };
    let serde_json::Value::Object(mut merged) = serde_json::to_value(file)? else { unreachable!("a struct") };
    merged.extend(saved);
    Ok(serde_json::from_value(merged.into())?)
}

/// `user` when it is `project`'s own Smart View bot and the Smart View is on
/// there: who a map job runs as.
pub fn own_bot<'a>(user: Option<&'a User>, settings: Option<&db::Settings>) -> Option<&'a User> {
    let s = settings.filter(|s| s.enabled)?;
    user.filter(|u| u.user_type == BOT_TYPE && s.bot_user_id == Some(u.id))
}

async fn ensure_bot(state: &AppState, project: &Project, known: Option<i64>) -> ApiResult<User> {
    let kind = super::renovate::ProjectBot {
        username: format!("smart-view-bot-{}", project.id),
        name: "Smart View",
        user_type: BOT_TYPE,
        level: forgust_db::access::REPORTER,
    };
    super::renovate::ensure_project_bot(state, project, &kind, known).await
}

/// Whether the Smart View is on for the project, and on the instance.
pub async fn is_on(state: &AppState, project_id: i64) -> ApiResult<bool> {
    if !db::get(&state.db, project_id).await?.is_some_and(|s| s.enabled) {
        return Ok(false);
    }
    Ok(config(state).await.map_err(ApiError::Internal)?.enabled)
}

/// Turn the Smart View on for a project (its bot joins as a Reporter, and the
/// default branch is mapped) or off (its maps go, those being made stop).
pub async fn configure(state: &AppState, principal: &Principal, project: &Project, enabled: bool) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Admin, project).await?;
    crate::auth::authz::writable(project)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let before = db::get(&state.db, project.id).await?;
    let was_on = before.as_ref().is_some_and(|s| s.enabled);
    if enabled {
        if !config(state).await.map_err(ApiError::Internal)?.enabled {
            return Err(ApiError::conflict("the Smart View is turned off on this instance"));
        }
        let bot = ensure_bot(state, project, before.as_ref().and_then(|s| s.bot_user_id)).await?;
        db::set(&state.db, project.id, true, Some(bot.id), Some(user.id)).await?;
        map_default_branch(state, project).await;
    } else {
        db::set(&state.db, project.id, false, None, None).await?;
        db::drop_history(&state.db, project.id).await?;
        for pl in db::drop_project(&state.db, project.id).await? {
            crate::ci::pipeline::cancel(state, pl).await.map_err(ApiError::Internal)?;
        }
    }
    if enabled != was_on {
        let action = if enabled { "smart_view_enabled" } else { "smart_view_disabled" };
        super::audit(state, principal, action, Some(("project", project.id)), json!({}), None).await;
        told(state, project.id);
    }
    Ok(())
}

/// The project's open pages hear that its map changed, and read it again: one shown before a
/// commit started being mapped would otherwise wait for a reload. While one is being mapped
/// they also ask every few seconds, so a run settled by the sweep reaches them too.
fn told(state: &AppState, project_id: i64) {
    state.events.publish(crate::events::Event::SmartView { project_id });
}

/// What a map job reads besides the map: for the default branch, what its
/// commits changed to it, after the last one read (`since`) when there is one.
#[derive(Debug, Clone, Copy)]
enum Reads<'a> {
    Map,
    History { since: Option<&'a str> },
}

/// The job: the assay image running `assay report` on a full clone (the
/// history tells who knows each block, and what each commit changed). The
/// image's entrypoint is overridden: one that is not a shell would not run the
/// script.
fn job_config(cfg: &SmartViewConfig, name: &str, reads: Reads) -> String {
    let mut variables = json!({
        "GIT_DEPTH": "0",
        // the block of the repository's top folder takes the project's name,
        // not the folder a runner clones into
        "FORGUST_SMART_VIEW_NAME": name,
    });
    let mut script = format!("assay report --name \"$FORGUST_SMART_VIEW_NAME\" --out {}", report::FILE);
    if let Reads::History { since } = reads {
        script.push_str(" --history");
        if let Some(since) = since {
            variables["FORGUST_SMART_VIEW_SINCE"] = json!(since);
            script.push_str(" --since \"$FORGUST_SMART_VIEW_SINCE\"");
        }
    }
    let job = json!({
        "image": { "name": cfg.image, "entrypoint": [""] },
        "tags": cfg.tags,
        "timeout": format!("{} minutes", cfg.timeout_secs.div_ceil(60).max(1)),
        "variables": variables,
        "rules": [{ "when": "always" }],
        "script": [script],
        // the server keeps what it reads of it: the file goes soon
        "artifacts": { "expire_in": "1 day", "reports": { REPORT: report::FILE } },
    });
    // JSON is YAML
    json!({ JOB: job }).to_string()
}

/// Map `sha`, the head of the default branch (`mr` None) or of a merge
/// request, unless it is mapped or being mapped already. The runs of the other
/// commits of the same that still go are given up.
async fn start(state: &AppState, project: &Project, mr: Option<&MergeRequest>, git_ref: &str, sha: &str) -> ApiResult<()> {
    let Some(settings) = db::get(&state.db, project.id).await?.filter(|s| s.enabled) else { return Ok(()) };
    if project.archived() || mr.is_some_and(|m| m.state != "opened") {
        return Ok(());
    }
    let cfg = config(state).await.map_err(ApiError::Internal)?;
    if !cfg.enabled {
        return Ok(());
    }
    let bot = ensure_bot(state, project, settings.bot_user_id).await?;
    if settings.bot_user_id != Some(bot.id) {
        // the bot was removed and made again: the jobs run as this one
        db::set_bot(&state.db, project.id, bot.id).await?;
    }
    let mr_id = mr.map(|m| m.id);
    // only the head is worth a map: a push handled after a newer one starts nothing, and gives
    // up no run (a branch not found here is taken at its word)
    let head = state.storage.resolve(project.id, &format!("refs/heads/{git_ref}")).await.ok().flatten();
    if head.is_some_and(|h| h != sha) {
        return Ok(());
    }
    // the push and the merge request's opening may both get here: one run
    let mut tx = state.db.begin().await?;
    let claimed = db::claim(&mut tx, project.id, mr_id, git_ref, sha).await?;
    tx.commit().await?;
    if let Some(id) = claimed {
        // the default branch's history goes on from the last commit read of it
        let kept = match mr {
            None => db::history_head(&state.db, project.id).await?.filter(|(branch, _)| branch == git_ref).map(|(_, head)| head),
            Some(_) => None,
        };
        let reads = if mr.is_none() { Reads::History { since: kept.as_deref() } } else { Reads::Map };
        let started: anyhow::Result<()> = async {
            let trigger = crate::ci::pipeline::Trigger {
                merge_request_id: mr_id,
                config: Some(job_config(&cfg, &project.path, reads)),
                ..crate::ci::pipeline::Trigger::new(project.id, git_ref, sha, SOURCE, Some(bot.id))
            };
            let pipeline_id = crate::ci::pipeline::create(state, trigger).await?.id().ok_or_else(|| anyhow::anyhow!("no pipeline"))?;
            Ok(db::started(&state.db, id, pipeline_id).await?)
        }
        .await;
        if let Err(e) = started {
            // not started, or unrecorded (its report could never be kept): the
            // commit is free for another run
            db::unclaim(&state.db, id).await?;
            return Err(ApiError::Internal(e));
        }
        told(state, project.id);
    }
    // the head is mapped or being mapped (now, or already: a branch pushed back to a commit it
    // was at): the runs of the commits it left would only put an older map over its own
    for pl in db::drop_pending_but(&state.db, project.id, mr_id, sha).await? {
        if let Err(e) = crate::ci::pipeline::cancel(state, pl).await {
            tracing::warn!(pipeline_id = pl, error = %format!("{e:#}"), "the map of a commit the branch left goes on though given up");
        }
    }
    Ok(())
}

/// The default branch moved: map its new head.
pub async fn on_default_branch_push(state: &AppState, project: &Project, sha: &str) {
    let Some(branch) = &project.default_branch else { return };
    if let Err(e) = start(state, project, None, branch, sha).await {
        tracing::info!(project_id = project.id, error = ?e, "the default branch's new head not mapped");
    }
}

/// A merge request's head moved (or it opened): map it.
pub async fn on_merge_request_head(state: &AppState, project: &Project, mr: &MergeRequest, sha: &str) {
    if let Err(e) = start(state, project, Some(mr), &mr.source_branch, sha).await {
        tracing::info!(project_id = project.id, mr = mr.iid, error = ?e, "the merge request's head not mapped");
    }
}

/// Map the default branch as it is now (the Smart View was just turned on).
async fn map_default_branch(state: &AppState, project: &Project) {
    let Some(branch) = &project.default_branch else { return };
    match state.storage.resolve(project.id, &format!("refs/heads/{branch}")).await {
        Ok(Some(sha)) => on_default_branch_push(state, project, &sha).await,
        Ok(None) => {}
        Err(e) => tracing::info!(project_id = project.id, error = %e, "the default branch not mapped: its head is not read"),
    }
}

/// A map's pipeline ended failed or canceled: no report follows, the page says so now.
pub async fn on_pipeline_finished(state: &AppState, pipeline: &forgust_db::ci::Pipeline, status: &str) {
    // called at each change of status: only one it will not leave, short of a success
    if pipeline.source != SOURCE || status == "success" || forgust_db::ci::ACTIVE.contains(&status) {
        return;
    }
    let settled: anyhow::Result<()> = async {
        let Some(row) = db::of_pipeline(&state.db, pipeline.id).await? else { return Ok(()) };
        settle_failed(state, row.id, &format!("the job ended ({status}) without a report")).await?;
        told(state, pipeline.project_id);
        Ok(())
    }
    .await;
    if let Err(e) = settled {
        tracing::warn!(pipeline_id = pipeline.id, error = %format!("{e:#}"), "a map's failed run not recorded (the sweep will)");
    }
}

/// Every minute: forget the maps nothing shows any more, give up those no
/// runner took, and mark failed those whose pipeline ended without a report read.
pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    for pl in db::drop_outdated(&state.db).await? {
        if let Err(e) = crate::ci::pipeline::cancel(state, pl).await {
            tracing::warn!(pipeline_id = pl, error = %format!("{e:#}"), "the map of a commit nothing shows goes on");
        }
    }
    let cfg = config(state).await?;
    for (id, pipeline) in db::stale(&state.db, cfg.timeout_secs as f64 + 3_600.0).await? {
        tracing::warn!(map = id, "a Smart View map never finished; given up (no runner with the configured tags?)");
        // said first: the pipeline's end, canceled, would say less
        settle_failed(state, id, "no runner took the job in time").await?;
        if let Some(pl) = pipeline {
            crate::ci::pipeline::cancel(state, pl).await?;
        }
    }
    for (id, status) in db::ended_pending(&state.db).await? {
        settle_failed(state, id, &format!("the job ended ({status}) without a report")).await?;
    }
    Ok(())
}

async fn settle_failed(state: &AppState, id: i64, why: &str) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    db::fail(&mut tx, id, &super::ai_review::clip(why, 500)).await?;
    tx.commit().await?;
    Ok(())
}

/// Read a finished map job's report and keep its map.
pub async fn ingest(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    let Some(pl) = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await? else { return Ok(()) };
    if pl.source != SOURCE {
        return Ok(());
    }
    // given up since (a newer commit, the Smart View turned off): nothing to keep
    let Some(row) = db::of_pipeline(&state.db, pl.id).await? else { return Ok(()) };
    let project_id = pl.project_id;
    let fail = |why: String| async move {
        settle_failed(state, row.id, &why).await?;
        told(state, project_id);
        Ok(())
    };
    let user = match pl.user_id {
        Some(id) => forgust_db::users::find(&state.db, id).await?,
        None => None,
    };
    let settings = db::get(&state.db, pl.project_id).await?;
    if own_bot(user.as_ref(), settings.as_ref()).is_none() {
        return fail("the Smart View was turned off for this project, or the job did not run as its bot".into()).await;
    }
    let Some(art) = forgust_db::ci::artifacts_of_build(&state.db, b.id).await?.into_iter().find(|a| a.file_type == REPORT) else {
        return fail("the job uploaded no report".into()).await;
    };
    let read = crate::ci::reports::read(state, &art.blob_key, &art.file_format, report::MAX_BYTES).await;
    let mut rep: Report = match read.and_then(|d| serde_json::from_slice(&d).map_err(Into::into)) {
        Ok(r) => r,
        Err(e) => return fail(format!("the report is unreadable: {e}")).await,
    };
    if rep.version != report::VERSION || rep.commit != row.sha {
        return fail("the report is not of this commit".into()).await;
    }
    if rep.map.is_none() && rep.error.is_none() {
        return fail("the report has no map".into()).await;
    }
    // the history is kept apart from the map, and only the default branch's
    let history = rep.history.take().filter(|_| pl.merge_request_id.is_none());
    let mut tx = state.db.begin().await?;
    db::ready(&mut tx, row.id, &serde_json::to_value(&rep)?).await?;
    if let Some(h) = history {
        let kept = keep_history(&mut tx, pl.project_id, &row.git_ref, &rep.commit, h).await?;
        if !kept {
            tracing::info!(project_id = pl.project_id, commit = %rep.commit, "a map's history not kept: another one was, read from a later commit");
        }
    }
    tx.commit().await?;
    told(state, project_id);
    Ok(())
}

/// What a job read of the default branch's history, kept: after the chapters kept
/// when it read on from the last commit kept, else in their place.
async fn keep_history(
    tx: &mut sqlx::PgConnection,
    project_id: i64,
    git_ref: &str,
    head: &str,
    h: forgust_assay::History,
) -> anyhow::Result<bool> {
    let chapters = h
        .chapters
        .into_iter()
        .map(|c| {
            Ok(db::NewChapter {
                changes: serde_json::to_string(&c.changes)?,
                sha: c.commit,
                merge_sha: c.merge,
                author_name: c.author,
                author_email: c.email,
                at: c.at,
                title: c.title,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let start = h.start.as_ref().map(|s| (s.commit.as_str(), s.at));
    Ok(db::keep_history(tx, project_id, git_ref, head, h.since.as_deref(), start, &chapters).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipeline_parses() {
        let cfg = SmartViewConfig { tags: vec!["docker".into()], ..Default::default() };
        for (reads, history, since) in [
            (Reads::Map, false, None),
            (Reads::History { since: None }, true, None),
            (Reads::History { since: Some("abc123") }, true, Some("abc123")),
        ] {
            let text = job_config(&cfg, "shop", reads);
            let parsed = forgust_ci_config::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
            assert_eq!(parsed.jobs.keys().collect::<Vec<_>>(), [JOB]);
            let job = &parsed.jobs[JOB];
            assert_eq!(job.variables.get("GIT_DEPTH").map(String::as_str), Some("0"));
            assert_eq!(job.variables.get("FORGUST_SMART_VIEW_NAME").map(String::as_str), Some("shop"));
            assert!(text.contains("assay report --name"), "{text}");
            assert!(text.contains(report::FILE), "{text}");
            assert_eq!(text.contains("--history"), history, "{text}");
            assert_eq!(job.variables.get("FORGUST_SMART_VIEW_SINCE").map(String::as_str), since);
            assert_eq!(text.contains("--since \\\"$FORGUST_SMART_VIEW_SINCE\\\""), since.is_some(), "{text}");
        }
    }

    #[test]
    fn the_admin_settings_go_over_the_file() {
        let file = SmartViewConfig::default();
        let saved = json!({ "enabled": false, "tags": ["maps"], "timeout_secs": 120 });
        let c = over(&file, saved).unwrap();
        assert!(!c.enabled);
        assert_eq!(c.tags, ["maps"]);
        assert_eq!(c.timeout_secs, 120);
        // no image set on purpose: the file's, which moves with each upgrade
        assert_eq!(c.image, file.image);
        assert_eq!(over(&file, json!({})).unwrap().image, file.image);
    }
}
