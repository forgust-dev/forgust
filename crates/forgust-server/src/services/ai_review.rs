//! AI review of merge requests. Someone who can push asks for a review; Forgust
//! runs a pipeline of its own (source `ai_review`) on the instance's runners,
//! as a bot that is a Reporter of the project, whose job sends the changes to
//! the model endpoint the administrator set (`forgust ai-review`, in
//! `forgust-review`).
//!
//! The job holds no credential of the forge beyond its job token (clone
//! only): it uploads a report, and the server checks every comment in it (the
//! merge request did not move, the line is in its diff, at most
//! `max_comments`, not posted already) before posting it as the bot. A
//! review is advice: the bot cannot approve or request changes.

use std::collections::HashSet;

use forgust_db::merge_requests::{self as mrdb, MergeRequest, MrDiff};
use forgust_db::projects::Project;
use forgust_db::users::User;
use forgust_review::report::{self, Report};
use forgust_storage::{DiffLineKind, DiffOptions};
use serde_json::json;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::ci::variables::Var;
use crate::config::{AiProvider, AiReviewConfig};
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::state::AppState;

pub const SOURCE: &str = "ai_review";
/// The report kind (`artifacts:reports:ai_review`).
pub const REPORT: &str = "ai_review";

/// What an administrator may change over the config file's `[ai_review]`
/// (Admin > AI review); absent fields keep the file's value. The key is
/// stored apart, encrypted.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Overrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AiProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_comments: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_diff_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<Option<String>>,
}

/// The settings in effect: the config file, then the administrator's changes.
pub async fn config(state: &AppState) -> anyhow::Result<AiReviewConfig> {
    let mut c = state.cfg.ai_review.clone();
    let (raw, key) = forgust_db::ai_review::instance(&state.db).await?;
    let o: Overrides = serde_json::from_value(raw).unwrap_or_default();
    macro_rules! over {
        ($($f:ident),*) => { $( if let Some(v) = o.$f { c.$f = v; } )* };
    }
    over!(enabled, provider, base_url, model, image, tags, max_concurrent, timeout_secs, max_comments, max_diff_bytes, ca_pem);
    if let Some(sealed) = key {
        c.api_key = Some(state.secrets.decrypt_str(&sealed)?);
    }
    Ok(c)
}

/// On, and set up enough to run.
pub fn usable(c: &AiReviewConfig) -> bool {
    c.enabled && !c.base_url.trim().is_empty() && !c.model.trim().is_empty()
}

/// Where the code goes: the endpoint's host, to show people.
pub fn endpoint_host(c: &AiReviewConfig) -> Option<String> {
    url::Url::parse(c.base_url.trim()).ok().and_then(|u| {
        u.host_str().map(|h| match u.port() {
            Some(p) => format!("{h}:{p}"),
            None => h.to_string(),
        })
    })
}

pub fn bot_username(project_id: i64) -> String {
    format!("ai-review-bot-{project_id}")
}

async fn ensure_bot(state: &AppState, project: &Project, known: Option<i64>) -> ApiResult<User> {
    let kind = super::renovate::ProjectBot {
        username: bot_username(project.id),
        name: "AI review",
        user_type: "ai_review_bot",
        level: forgust_db::access::REPORTER,
    };
    super::renovate::ensure_project_bot(state, project, &kind, known).await
}

/// Turn AI review on for a project (its bot joins as a Reporter) or off
/// (reviews going on stop).
pub async fn configure(state: &AppState, principal: &Principal, project: &Project, enabled: bool) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Admin, project).await?;
    crate::auth::authz::writable(project)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    let before = forgust_db::ai_review::get(&state.db, project.id).await?;
    let was_on = before.as_ref().is_some_and(|s| s.enabled);
    if enabled {
        if !usable(&config(state).await.map_err(ApiError::Internal)?) {
            return Err(ApiError::conflict("AI review is turned off on this instance"));
        }
        let bot = ensure_bot(state, project, before.as_ref().and_then(|s| s.bot_user_id)).await?;
        forgust_db::ai_review::set(&state.db, project.id, true, Some(bot.id), Some(user.id)).await?;
    } else {
        for pl in forgust_db::ai_review::active_pipelines(&state.db, project.id).await? {
            crate::ci::pipeline::cancel(state, pl).await.map_err(ApiError::Internal)?;
        }
        forgust_db::ai_review::set(&state.db, project.id, false, None, None).await?;
    }
    if enabled != was_on {
        let action = if enabled { "ai_review_enabled" } else { "ai_review_disabled" };
        super::audit(state, principal, action, Some(("project", project.id)), json!({}), None).await;
    }
    Ok(())
}

/// The job: Forgust's own image running `forgust ai-review` on a full clone
/// (the base of an incremental review can be any older commit).
fn job_config(cfg: &AiReviewConfig, base: &str, head: &str, target: &str) -> String {
    let job = json!({
        "image": { "name": cfg.image, "entrypoint": [""] },
        "tags": cfg.tags,
        "timeout": format!("{} minutes", cfg.timeout_secs.div_ceil(60).max(1)),
        "variables": {
            "GIT_DEPTH": "0",
            "FORGUST_REVIEW_BASE_SHA": base,
            "FORGUST_REVIEW_HEAD_SHA": head,
            "FORGUST_REVIEW_TARGET_SHA": target,
        },
        "rules": [{ "when": "always" }],
        "script": ["forgust ai-review"],
        "artifacts": { "when": "always", "expire_in": "7 days", "reports": { REPORT: report::FILE } },
    });
    // JSON is YAML
    json!({ "ai_review": job }).to_string()
}

/// Ask for a review of a merge request's latest version: of what changed
/// since the last posted review when that is still in its history, else of
/// all of it (a version reviewed already is reviewed whole again). Returns
/// the pipeline's id.
pub async fn request(state: &AppState, principal: &Principal, project: &Project, mr: &MergeRequest) -> ApiResult<i64> {
    require_project(state, principal, ProjectAction::Push, project).await?;
    crate::auth::authz::writable(project)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    let cfg = config(state).await.map_err(ApiError::Internal)?;
    if !usable(&cfg) {
        return Err(ApiError::conflict("AI review is turned off on this instance"));
    }
    let Some(settings) = forgust_db::ai_review::get(&state.db, project.id).await?.filter(|s| s.enabled) else {
        return Err(ApiError::conflict("AI review is off for this project"));
    };
    if mr.state != "opened" {
        return Err(ApiError::conflict("only an open merge request is reviewed"));
    }
    let latest = mrdb::latest_diff(&state.db, mr.id).await?.ok_or_else(|| ApiError::conflict("the merge request has no changes yet"))?;
    // two clicks at once start one review
    let mut lock = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))").bind(format!("ai-review:{}", mr.id)).execute(&mut *lock).await?;
    if forgust_db::ai_review::active(&state.db, Some(mr.id)).await? > 0 {
        return Err(ApiError::conflict("a review of this merge request is already going"));
    }
    if forgust_db::ai_review::active(&state.db, None).await? >= cfg.max_concurrent as i64 {
        return Err(ApiError::conflict("the instance is reviewing as many merge requests as it may: try again in a few minutes"));
    }
    // asked again on a reviewed version (another model, another try): all of
    // it again; what was posted already is not posted twice
    let base = match forgust_db::ai_review::last_posted_head(&state.db, mr.id).await? {
        Some(prev)
            if prev != latest.head_sha
                && prev != latest.base_sha
                && state.storage.is_ancestor(project.id, &prev, &latest.head_sha).await.unwrap_or(false) =>
        {
            prev
        }
        _ => latest.base_sha.clone(),
    };
    let bot = ensure_bot(state, project, settings.bot_user_id).await?;
    let created = crate::ci::pipeline::create(
        state,
        crate::ci::pipeline::Trigger {
            merge_request_id: Some(mr.id),
            config: Some(job_config(&cfg, &base, &latest.head_sha, &latest.base_sha)),
            ..crate::ci::pipeline::Trigger::new(project.id, &mr.source_branch, &latest.head_sha, SOURCE, Some(bot.id))
        },
    )
    .await
    .map_err(ApiError::Internal)?;
    let id = created.id().ok_or_else(|| ApiError::Internal(anyhow::anyhow!("the review pipeline was not created")))?;
    forgust_db::ai_review::insert_run(&state.db, id, mr.id, &base, &latest.head_sha, user.id).await?;
    lock.commit().await?;
    super::audit(
        state,
        principal,
        "ai_review_requested",
        Some(("project", project.id)),
        json!({ "merge_request": mr.iid, "pipeline_id": id }),
        None,
    )
    .await;
    state.events.publish(Event::MergeRequest { project_id: project.id, iid: mr.iid });
    Ok(id)
}

/// The job's variables, given when a runner takes it: where the model is,
/// and its key (masked). Nothing from the project's, groups' or instance's
/// CI variables, and no token of the forge.
pub async fn job_variables(state: &AppState, mr: Option<&MergeRequest>) -> anyhow::Result<Vec<Var>> {
    let cfg = config(state).await?;
    anyhow::ensure!(usable(&cfg), "AI review is turned off on this instance");
    let public = |k: &str, v: String| Var { key: k.into(), value: v, public: true, masked: false, file: false, raw: true };
    let mut v = vec![
        public("AI_REVIEW_PROVIDER", cfg.provider.as_str().into()),
        public("AI_REVIEW_BASE_URL", cfg.base_url.trim().into()),
        public("AI_REVIEW_MODEL", cfg.model.trim().into()),
        public("AI_REVIEW_MAX_COMMENTS", cfg.max_comments.to_string()),
        public("AI_REVIEW_MAX_DIFF_BYTES", cfg.max_diff_bytes.to_string()),
    ];
    if let Some(m) = mr.filter(|m| !m.description.trim().is_empty()) {
        v.push(public("CI_MERGE_REQUEST_DESCRIPTION", clip(&m.description, 8_000)));
    }
    if let Some(k) = cfg.api_key.filter(|k| !k.is_empty()) {
        v.push(Var { key: "AI_REVIEW_API_KEY".into(), value: k, public: false, masked: true, file: false, raw: true });
    }
    if let Some(pem) = cfg.ca_pem.filter(|p| !p.trim().is_empty()) {
        v.push(Var { key: "AI_REVIEW_CA_FILE".into(), value: pem, public: false, masked: false, file: true, raw: true });
    }
    Ok(v)
}

/// Every minute: cancel reviews no runner took, and mark failed those whose
/// pipeline ended without a report read.
pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    let cfg = config(state).await?;
    for id in forgust_db::ai_review::stale(&state.db, cfg.timeout_secs as f64 + 86_400.0).await? {
        tracing::warn!(pipeline = id, "AI review never finished; canceled (no runner with the configured tags?)");
        crate::ci::pipeline::cancel(state, id).await?;
    }
    for (id, status) in forgust_db::ai_review::ended_pending(&state.db).await? {
        let why = format!("the review job ended ({status}) without a report");
        if forgust_db::ai_review::finish(&state.db, id, None, "failed", Some(&why)).await? {
            publish_for(state, id).await;
        }
    }
    Ok(())
}

async fn publish_for(state: &AppState, pipeline_id: i64) {
    if let Ok(Some(run)) = forgust_db::ai_review::run(&state.db, pipeline_id).await
        && let Ok(Some(mr)) = mrdb::find_by_id(&state.db, run.merge_request_id).await
    {
        state.events.publish(Event::MergeRequest { project_id: mr.project_id, iid: mr.iid });
    }
}

/// Read a finished review job's report and post what holds up.
pub async fn ingest(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    let Some(pl) = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await? else { return Ok(()) };
    if pl.source != SOURCE {
        return Ok(());
    }
    let Some(run) = forgust_db::ai_review::run(&state.db, pl.id).await? else { return Ok(()) };
    let fail = |why: String| fail_run(state, pl.id, b.id, why);
    let Some(art) = forgust_db::ci::artifacts_of_build(&state.db, b.id).await?.into_iter().find(|a| a.file_type == REPORT) else {
        return fail("the review job uploaded no report".into()).await;
    };
    let rep: Report = match crate::ci::reports::read(state, &art.blob_key, &art.file_format, report::MAX_BYTES)
        .await
        .and_then(|d| serde_json::from_slice(&d).map_err(Into::into))
    {
        Ok(r) => r,
        Err(e) => return fail(format!("the review's report is unreadable: {e}")).await,
    };
    if let Some(e) = &rep.error {
        return fail(clip(e, 500)).await;
    }
    if rep.version != report::VERSION || rep.head_sha != run.head_sha || rep.base_sha != run.base_sha {
        return fail("the report is not of this review".into()).await;
    }
    let Some(mr) = mrdb::find_by_id(&state.db, run.merge_request_id).await? else { return Ok(()) };
    let Some(project) = forgust_db::projects::find(&state.db, mr.project_id).await? else { return Ok(()) };
    let latest = mrdb::latest_diff(&state.db, mr.id).await?;
    let current = latest.as_ref().is_some_and(|d| d.head_sha == run.head_sha);
    if mr.state != "opened" || !current || project.archived() {
        if forgust_db::ai_review::finish(&state.db, pl.id, Some(b.id), "outdated", None).await? {
            publish_for(state, pl.id).await;
        }
        return Ok(());
    }
    let latest = latest.expect("current");
    let bot = match pl.user_id {
        Some(id) => forgust_db::users::find(&state.db, id).await?,
        None => None,
    };
    let Some(bot) = bot.filter(|u| u.user_type == "ai_review_bot") else {
        return fail("the review did not run as the project's AI review bot".into()).await;
    };
    let cfg = config(state).await?;
    // claimed before posting: reading this report again posts nothing twice
    // (a retried job is another report: what it repeats is not posted again)
    if !forgust_db::ai_review::finish(&state.db, pl.id, Some(b.id), "posted", None).await? {
        return Ok(());
    }
    let r = post(state, &project, &mr, &latest, &run, &rep, &bot, cfg.max_comments as usize).await;
    let (posted, dropped) = match &r {
        Ok(counts) => *counts,
        Err(_) => (0, 0),
    };
    forgust_db::ai_review::set_counts(&state.db, pl.id, posted as i32, dropped as i32).await?;
    state.events.publish(Event::MergeRequest { project_id: project.id, iid: mr.iid });
    r.map(|_| ())
}

async fn fail_run(state: &AppState, pipeline_id: i64, build_id: i64, why: String) -> anyhow::Result<()> {
    if forgust_db::ai_review::finish(&state.db, pipeline_id, Some(build_id), "failed", Some(&why)).await? {
        publish_for(state, pipeline_id).await;
    }
    Ok(())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(" …");
    out
}

/// The lines of `latest`'s diff a comment may go on: (path, new line) for
/// added and kept lines, (path, old line) for removed ones.
async fn commentable(
    state: &AppState,
    project_id: i64,
    latest: &MrDiff,
    paths: Vec<String>,
) -> anyhow::Result<(HashSet<(String, u32)>, HashSet<(String, u32)>)> {
    let mut new = HashSet::new();
    let mut old = HashSet::new();
    if paths.is_empty() {
        return Ok((new, old));
    }
    let opts = DiffOptions { offset: 0, limit: paths.len().max(1), max_lines_per_file: 20_000, paths: Some(paths) };
    let d = state.storage.diff(project_id, Some(&latest.base_sha), &latest.head_sha, opts).await?;
    for f in d.files {
        for h in &f.hunks {
            for l in &h.lines {
                match (l.kind, l.new, l.old) {
                    (DiffLineKind::Add | DiffLineKind::Context, Some(n), _) => {
                        new.insert((f.new_path.clone(), n));
                    }
                    (DiffLineKind::Del, _, Some(o)) => {
                        old.insert((f.old_path.clone(), o));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok((new, old))
}

/// A comment on a line, as posted (what a review posting it again would repeat).
#[derive(PartialEq, Eq, Hash)]
struct LineNote {
    path: String,
    new_line: Option<i32>,
    old_line: Option<i32>,
    body: String,
}

/// Post the summary and the comments that hold up: (posted, dropped).
#[allow(clippy::too_many_arguments)]
async fn post(
    state: &AppState,
    project: &Project,
    mr: &MergeRequest,
    latest: &MrDiff,
    run: &forgust_db::ai_review::Run,
    rep: &Report,
    bot: &User,
    max_comments: usize,
) -> anyhow::Result<(usize, usize)> {
    let paths: Vec<String> = rep
        .comments
        .iter()
        .map(|c| c.path.clone())
        .filter(|p| forgust_storage::validate_path(p))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let (new_lines, old_lines) = commentable(state, project.id, latest, paths).await?;
    // a removed line's number is of the old file: the review's base, which is
    // the merge request's only when the whole of it was reviewed
    let whole = run.base_sha == latest.base_sha;
    let existing = mrdb::notes(&state.db, mr.id).await?;
    let already: HashSet<LineNote> = existing
        .iter()
        .filter(|n| n.author_id == Some(bot.id))
        .filter_map(|n| Some(LineNote { path: n.new_path.clone()?, new_line: n.new_line, old_line: n.old_line, body: n.body.clone() }))
        .collect();
    let mut posted = 0usize;
    let mut dropped = 0usize;
    let mut notes: Vec<LineNote> = Vec::new();
    for c in &rep.comments {
        let body = clip(c.body.trim(), report::MAX_COMMENT_CHARS);
        let on_line = match (c.new_line, c.old_line) {
            (Some(n), _) => new_lines.contains(&(c.path.clone(), n)),
            (None, Some(o)) => whole && old_lines.contains(&(c.path.clone(), o)),
            (None, None) => false,
        };
        if !on_line || body.is_empty() || notes.len() == max_comments {
            dropped += 1;
            continue;
        }
        let note = LineNote {
            path: c.path.clone(),
            new_line: c.new_line.map(|n| n as i32),
            old_line: c.old_line.filter(|_| c.new_line.is_none()).map(|o| o as i32),
            body,
        };
        if already.contains(&note) || notes.contains(&note) {
            continue;
        }
        notes.push(note);
    }
    let scope =
        if whole { "the changes".to_string() } else { format!("the changes since `{}`", &run.base_sha[..run.base_sha.len().min(8)]) };
    let mut summary = format!(
        "**AI review** of {scope} up to `{}`, by `{}`. Automated: it can be wrong.\n\n",
        &run.head_sha[..run.head_sha.len().min(8)],
        clip(&rep.model, 80).replace('`', "")
    );
    let text = clip(rep.summary.trim(), report::MAX_SUMMARY_CHARS);
    if !text.is_empty() {
        summary.push_str(&text);
        summary.push_str("\n\n");
    }
    summary.push_str(&match notes.len() {
        0 => "No comment on the changed lines.".to_string(),
        1 => "1 comment on the changed lines.".to_string(),
        n => format!("{n} comments on the changed lines."),
    });
    if dropped > 0 {
        summary.push_str(&format!(" {dropped} more were left out (not on a changed line, or past the limit)."));
    }
    if !rep.skipped.is_empty() {
        summary.push_str("\n\nNot reviewed:\n");
        for s in rep.skipped.iter().take(50) {
            summary.push_str(&format!("- `{}`: {}\n", clip(&s.path, 200).replace('`', ""), clip(&s.reason, 200)));
        }
        if rep.skipped.len() > 50 {
            summary.push_str(&format!("- and {} more\n", rep.skipped.len() - 50));
        }
    }
    add_bot_note(state, project, mr, bot, &summary, mrdb::Position::default()).await?;
    for LineNote { path, new_line, old_line, body } in &notes {
        let pos = mrdb::Position {
            diff_id: Some(latest.id),
            base_sha: Some(&latest.base_sha),
            start_sha: Some(&latest.start_sha),
            head_sha: Some(&latest.head_sha),
            old_path: Some(path),
            new_path: Some(path),
            old_line: *old_line,
            new_line: *new_line,
            start_old_line: None,
            start_new_line: None,
        };
        add_bot_note(state, project, mr, bot, body, pos).await?;
        posted += 1;
    }
    Ok((posted, dropped))
}

/// A note of the bot: shown and sent to webhooks like any, but nobody is
/// mentioned or notified by what a model wrote.
async fn add_bot_note(
    state: &AppState,
    p: &Project,
    mr: &MergeRequest,
    bot: &User,
    body: &str,
    pos: mrdb::Position<'_>,
) -> anyhow::Result<()> {
    let id = mrdb::add_note(&state.db, mr.id, Some(bot.id), None, body, false, pos).await?;
    mrdb::touch(&state.db, mr.id).await?;
    if let Some(note) = mrdb::note(&state.db, mr.id, id).await? {
        super::webhooks::emit(
            state,
            p,
            "note",
            json!({ "object_kind": "note", "project": super::mr::project_json(state, p), "merge_request": { "iid": mr.iid, "title": mr.title },
                    "note": { "id": note.id, "body": note.body, "author": bot.username, "new_path": note.new_path, "new_line": note.new_line } }),
        )
        .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipeline_parses() {
        let cfg = AiReviewConfig { base_url: "http://m:1/v1".into(), model: "m".into(), enabled: true, ..Default::default() };
        let text = job_config(&cfg, "aa", "bb", "cc");
        let p = forgust_ci_config::parse(&text).expect("parses");
        let j = &p.jobs["ai_review"];
        assert_eq!(j.script, vec!["forgust ai-review"]);
        assert_eq!(j.artifacts.as_ref().unwrap().reports[REPORT], vec![report::FILE]);
    }

    #[test]
    fn where_the_code_goes() {
        let c = |u: &str| AiReviewConfig { base_url: u.into(), ..Default::default() };
        assert_eq!(endpoint_host(&c("https://api.openai.com/v1")).as_deref(), Some("api.openai.com"));
        assert_eq!(endpoint_host(&c("http://ollama:11434/v1")).as_deref(), Some("ollama:11434"));
        assert_eq!(endpoint_host(&c("nope")), None);
        assert!(!usable(&AiReviewConfig { enabled: true, ..Default::default() }));
    }
}
