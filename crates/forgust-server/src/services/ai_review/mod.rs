//! AI review of merge requests. Someone who can push asks for a review; Forgust
//! runs a pipeline of its own (source `ai_review`) on the instance's runners,
//! as a bot that is a Reporter of the project, whose job sends the changes to
//! the model endpoint the administrator set. The job runs the reviewer image
//! (forgust-dev/forgust-reviewer); the variables it gets and the report it
//! leaves ([`report`]) are the contract between the two.
//!
//! The job holds no credential of the forge beyond its job token (clone
//! only): it uploads a report, and the server checks every comment in it (the
//! merge request did not move, the line is in its diff, at most
//! `max_comments`, not posted already) before posting it as the bot. A
//! review is advice: the bot cannot approve or request changes.

pub mod report;

use std::collections::{BTreeMap, HashMap, HashSet};

use forgust_db::merge_requests::{self as mrdb, MergeRequest, MrDiff};
use forgust_db::projects::Project;
use forgust_db::users::User;
use forgust_storage::{DiffLineKind, DiffOptions};
use report::Report;
use serde_json::json;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::ci::variables::Var;
use crate::config::AiReviewConfig;
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::state::AppState;

pub const SOURCE: &str = "ai_review";
/// The report kind (`artifacts:reports:ai_review`).
pub const REPORT: &str = "ai_review";

/// What an administrator may change over the config file's `[ai_review]`
/// (Admin > AI review); absent fields keep the file's value. The key and the
/// secret options are stored apart, encrypted.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Overrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
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
    pub options: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<Option<String>>,
}

/// The settings in effect: the config file, then the administrator's changes.
/// `api_key` and `secret_options` are the config file's only: those set in
/// the admin area are decrypted where a job needs them ([`secrets`]).
pub async fn config(state: &AppState) -> anyhow::Result<AiReviewConfig> {
    let mut c = state.cfg.ai_review.clone();
    let o: Overrides = serde_json::from_value(forgust_db::ai_review::instance(&state.db).await?.overrides).unwrap_or_default();
    macro_rules! over {
        ($($f:ident),*) => { $( if let Some(v) = o.$f { c.$f = v; } )* };
    }
    over!(enabled, provider, base_url, model, image, tags, max_concurrent, timeout_secs, max_comments, options, ca_pem);
    Ok(c)
}

/// The secrets in effect: (the endpoint's key, the secret options), each the
/// admin area's when set there, else the config file's.
pub async fn secrets(state: &AppState) -> anyhow::Result<(Option<String>, BTreeMap<String, String>)> {
    let i = forgust_db::ai_review::instance(&state.db).await?;
    let key = match i.api_key_enc {
        Some(sealed) => Some(state.secrets.decrypt_str(&sealed)?),
        None => state.cfg.ai_review.api_key.clone(),
    };
    let options = match i.secret_options_enc {
        Some(sealed) => state.secrets.decrypt_json(&sealed)?,
        None => state.cfg.ai_review.secret_options.clone(),
    };
    Ok((key.filter(|k| !k.is_empty()), options))
}

/// What Forgust gives a review job itself: an option may not take its name.
pub const RESERVED_OPTIONS: &[&str] = &["PROVIDER", "BASE_URL", "MODEL", "API_KEY", "CA_FILE", "MAX_COMMENTS"];

/// An option's name: `A-Z`, `0-9` and `_`, not starting with a digit, not one Forgust sets.
pub fn option_name_ok(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && !k.starts_with(|c: char| c.is_ascii_digit())
        && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && !RESERVED_OPTIONS.contains(&k)
}

/// The user type of the projects' AI review bots.
pub const BOT_TYPE: &str = "ai_review_bot";

/// `user` when it is `project`'s own AI review bot and AI review is on there:
/// who a review job runs as, and posts as.
pub fn own_bot<'a>(user: Option<&'a User>, settings: Option<&forgust_db::ai_review::Settings>) -> Option<&'a User> {
    let s = settings.filter(|s| s.enabled)?;
    user.filter(|u| u.user_type == BOT_TYPE && s.bot_user_id == Some(u.id))
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
        user_type: BOT_TYPE,
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
/// The job: the reviewer image running `forgust-review` on a full clone
/// (the base of an incremental review can be any older commit). The image's
/// entrypoint is overridden: one that is not a shell would not run the script.
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
        "script": ["forgust-review"],
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
    // asked again on a reviewed version (another model, another try): all of
    // it again; what was posted already is not posted twice
    let base = match forgust_db::ai_review::last_posted_head(&state.db, mr.id).await? {
        Some(prev)
            if prev != latest.head_sha
                && prev != latest.base_sha
                && state.storage.is_ancestor(project.id, &prev, &latest.head_sha).await.unwrap_or(false)
                // a merge (of the target branch, typically) brings in changes
                // that are not the merge request's: review all of it then
                && state.storage.rev_list(project.id, &prev, &latest.head_sha, true).await.is_ok_and(|m| m.is_empty()) =>
        {
            prev
        }
        _ => latest.base_sha.clone(),
    };
    let bot = ensure_bot(state, project, settings.bot_user_id).await?;
    // requests at once (two clicks, two merge requests): one review per
    // merge request, and no more than max_concurrent on the instance
    let mut lock = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))").bind("ai-review-start").execute(&mut *lock).await?;
    let (this_mr, all) = forgust_db::ai_review::active_counts(&mut *lock, mr.id).await?;
    if this_mr > 0 {
        return Err(ApiError::conflict("a review of this merge request is already going"));
    }
    if all >= cfg.max_concurrent as i64 {
        return Err(ApiError::conflict("the instance is reviewing as many merge requests as it may: try again in a few minutes"));
    }
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
pub async fn job_variables(state: &AppState, project: &Project, user: Option<&User>) -> anyhow::Result<Vec<Var>> {
    let settings = forgust_db::ai_review::get(&state.db, project.id).await?;
    anyhow::ensure!(
        own_bot(user, settings.as_ref()).is_some(),
        "an AI review job not run by the project's AI review bot, or AI review is off"
    );
    let cfg = config(state).await?;
    anyhow::ensure!(usable(&cfg), "AI review is turned off on this instance");
    let public = |k: &str, v: String| Var { raw: true, ..Var::public(k, v) };
    let mut v = vec![
        public("AI_REVIEW_PROVIDER", cfg.provider.as_str().into()),
        public("AI_REVIEW_BASE_URL", cfg.base_url.trim().into()),
        public("AI_REVIEW_MODEL", cfg.model.trim().into()),
        public("AI_REVIEW_MAX_COMMENTS", cfg.max_comments.to_string()),
    ];
    // the reviewer's options: passed on, not read; never over what is set above
    let (key, secret_options) = secrets(state).await?;
    for (k, value) in cfg.options.iter().filter(|(k, _)| option_name_ok(k)) {
        v.push(public(&format!("AI_REVIEW_{k}"), value.clone()));
    }
    for (k, value) in secret_options.into_iter().filter(|(k, _)| option_name_ok(k)) {
        v.push(Var { masked: true, raw: true, ..Var::secret(&format!("AI_REVIEW_{k}"), value) });
    }
    if let Some(k) = key {
        v.push(Var { masked: true, raw: true, ..Var::secret("AI_REVIEW_API_KEY", k) });
    }
    if let Some(pem) = cfg.ca_pem.filter(|p| !p.trim().is_empty()) {
        v.push(Var { file: true, raw: true, ..Var::secret("AI_REVIEW_CA_FILE", pem) });
    }
    Ok(v)
}

/// Every minute: cancel reviews no runner took, and mark failed those whose
/// pipeline ended without a report read.
pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    let cfg = config(state).await?;
    // asked for by someone waiting, and holding a slot: an hour to be taken
    for id in forgust_db::ai_review::stale(&state.db, cfg.timeout_secs as f64 + 3_600.0).await? {
        tracing::warn!(pipeline = id, "AI review never finished; canceled (no runner with the configured tags?)");
        crate::ci::pipeline::cancel(state, id).await?;
    }
    for (id, mr_id, status) in forgust_db::ai_review::ended_pending(&state.db).await? {
        let why = format!("the review job ended ({status}) without a report");
        settle(state, id, mr_id, None, "failed", Some(&why)).await?;
    }
    Ok(())
}

/// Record how a run ended ([`forgust_db::ai_review::finish`]), and tell the
/// merge request's viewers when it did.
async fn settle(
    state: &AppState,
    pipeline_id: i64,
    mr_id: i64,
    build: Option<i64>,
    outcome: &str,
    why: Option<&str>,
) -> anyhow::Result<bool> {
    let done = forgust_db::ai_review::finish(&state.db, pipeline_id, build, outcome, why).await?;
    if done && let Some(mr) = mrdb::find_by_id(&state.db, mr_id).await? {
        state.events.publish(Event::MergeRequest { project_id: mr.project_id, iid: mr.iid });
    }
    Ok(done)
}

/// Read a finished review job's report and post what holds up.
pub async fn ingest(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    let Some(pl) = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await? else { return Ok(()) };
    if pl.source != SOURCE {
        return Ok(());
    }
    let Some(run) = forgust_db::ai_review::run(&state.db, pl.id).await? else { return Ok(()) };
    let fail = |why: String| async move { settle(state, pl.id, run.merge_request_id, Some(b.id), "failed", Some(&why)).await.map(|_| ()) };
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
    let current = mrdb::latest_diff(&state.db, mr.id).await?.filter(|d| d.head_sha == run.head_sha);
    let Some(latest) = current.filter(|_| mr.state == "opened" && !project.archived()) else {
        settle(state, pl.id, mr.id, Some(b.id), "outdated", None).await?;
        return Ok(());
    };
    let user = match pl.user_id {
        Some(id) => forgust_db::users::find(&state.db, id).await?,
        None => None,
    };
    let settings = forgust_db::ai_review::get(&state.db, project.id).await?;
    let Some(bot) = own_bot(user.as_ref(), settings.as_ref()) else {
        return fail("AI review was turned off for this project, or the job did not run as its bot".into()).await;
    };
    let cfg = config(state).await?;
    // claimed before posting: reading this report again posts nothing twice
    // (a retried job is another report: what it repeats is not posted again)
    if !forgust_db::ai_review::finish(&state.db, pl.id, Some(b.id), "posted", None).await? {
        return Ok(());
    }
    match post(state, &project, &mr, &latest, &run, &rep, bot, cfg.max_comments as usize).await {
        Ok((posted, dropped)) => {
            forgust_db::ai_review::set_counts(&state.db, pl.id, posted as i32, dropped as i32).await?;
            state.events.publish(Event::MergeRequest { project_id: project.id, iid: mr.iid });
            Ok(())
        }
        Err(e) => {
            // the task is tried again: let it claim the run again (what was
            // posted already is not posted twice)
            forgust_db::ai_review::unclaim(&state.db, pl.id, b.id).await?;
            Err(e)
        }
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(" …");
    out
}

/// The lines of `paths` (new paths) a comment may go on.
async fn commentable(state: &AppState, project_id: i64, latest: &MrDiff, paths: &HashSet<&str>) -> anyhow::Result<Commentable> {
    let mut c = Commentable::default();
    if paths.is_empty() {
        return Ok(c);
    }
    // the whole diff, not only these paths: git sees a rename only with both
    // of its paths
    let opts = DiffOptions { offset: 0, limit: 5_000, max_lines_per_file: 20_000, paths: None };
    let d = state.storage.diff(project_id, Some(&latest.base_sha), &latest.head_sha, opts).await?;
    for f in d.files.into_iter().filter(|f| paths.contains(f.new_path.as_str())) {
        // the job names a file by its new path, renamed or not
        if f.old_path != f.new_path {
            c.old_path.insert(f.new_path.clone(), f.old_path.clone());
        }
        for h in &f.hunks {
            for l in &h.lines {
                match (l.kind, l.new, l.old) {
                    (DiffLineKind::Add | DiffLineKind::Context, Some(n), _) => {
                        c.new.insert((f.new_path.clone(), n));
                    }
                    (DiffLineKind::Del, _, Some(o)) => {
                        c.old.insert((f.new_path.clone(), o));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(c)
}

/// The lines of a diff a comment may go on, by the file's new path: added
/// and kept lines by their new number, removed ones by their old number.
#[derive(Default)]
struct Commentable {
    new: HashSet<(String, u32)>,
    old: HashSet<(String, u32)>,
    /// Renamed files: new path → old path.
    old_path: HashMap<String, String>,
}

/// The model's text without the suggestion blocks it wrote anyway.
fn strip_suggestion_blocks(body: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for l in body.lines() {
        let t = l.trim_start();
        if inside {
            inside = !t.starts_with("```");
        } else if t.starts_with("```suggestion") {
            inside = true;
        } else {
            out.push(l);
        }
    }
    out.join("\n")
}

/// A suggestion that holds up, as (the line its comment goes on, the block):
/// every line it replaces is a line of the diff, they read what the model
/// says they read (it often gets numbers or indentation wrong), and the code
/// is small, changes something and cannot end the block early.
fn checked_suggestion(file: Option<&str>, in_diff: impl Fn(u32) -> bool, line: u32, s: &report::Suggestion) -> Option<(u32, String)> {
    let (from, to) = (s.from_line, s.to_line);
    if from == 0 || to < from || to - from >= report::MAX_SUGGESTION_LINES || s.code.chars().count() > report::MAX_SUGGESTION_CHARS {
        return None;
    }
    if !(from..=to).all(&in_diff) {
        return None;
    }
    let current = super::suggestions::lines_of(file?, from as usize, to as usize)?;
    let same = |a: &[&str], b: &[String]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.trim_end() == y.trim_end());
    let original: Vec<&str> = s.original.lines().collect();
    if !same(&original, &current) {
        return None;
    }
    let code: Vec<String> = s.code.lines().map(str::to_string).collect();
    if code.iter().any(|l| l.trim_start().starts_with("```")) || same(&code.iter().map(String::as_str).collect::<Vec<_>>(), &current) {
        return None;
    }
    // on the commented line when it is in the range, else on the last one
    let anchor = if (from..=to).contains(&line) { line } else { to };
    let mut block = format!("```suggestion:-{}+{}\n", anchor - from, to - anchor);
    for l in &code {
        block.push_str(l);
        block.push('\n');
    }
    block.push_str("```");
    Some((anchor, block))
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
    let paths: HashSet<&str> = rep.comments.iter().map(|c| c.path.as_str()).collect();
    let lines = commentable(state, project.id, latest, &paths).await?;
    let (new_lines, old_lines) = (&lines.new, &lines.old);
    // a removed line's number is of the old file: the review's base, which is
    // the merge request's only when the whole of it was reviewed
    let whole = run.base_sha == latest.base_sha;
    let existing = mrdb::notes(&state.db, mr.id).await?;
    let already: HashSet<LineNote> = existing
        .iter()
        .filter(|n| n.author_id == Some(bot.id))
        .filter_map(|n| Some(LineNote { path: n.new_path.clone()?, new_line: n.new_line, old_line: n.old_line, body: n.body.clone() }))
        .collect();
    let mut dropped = 0usize;
    let mut mismatched = 0usize; // suggestions left out
    let mut files: HashMap<String, Option<String>> = HashMap::new();
    // each with the first line of its range (a suggestion on several lines),
    // and whether it carries a change to apply
    let mut notes: Vec<(LineNote, Option<i32>, bool)> = Vec::new();
    for c in &rep.comments {
        // a suggestion block is the server's to write, from a checked suggestion
        let mut body = clip(strip_suggestion_blocks(&c.body).trim(), report::MAX_COMMENT_CHARS);
        let on_line = match (c.new_line, c.old_line) {
            (Some(n), _) => new_lines.contains(&(c.path.clone(), n)),
            (None, Some(o)) => whole && old_lines.contains(&(c.path.clone(), o)),
            (None, None) => false,
        };
        if !on_line || body.is_empty() || notes.len() == max_comments {
            dropped += 1;
            continue;
        }
        let mut new_line = c.new_line;
        let mut start = None;
        let mut ready = false;
        if let (Some(s), Some(line)) = (&c.suggestion, c.new_line) {
            if !files.contains_key(&c.path) {
                let text = match state.storage.blob(project.id, &latest.head_sha, &c.path, 4 << 20).await {
                    Ok(b) if !b.truncated => String::from_utf8(b.data).ok(),
                    _ => None,
                };
                files.insert(c.path.clone(), text);
            }
            let file = files.get(&c.path).and_then(Option::as_deref);
            match checked_suggestion(file, |n| new_lines.contains(&(c.path.clone(), n)), line, s) {
                Some((anchor, block)) => {
                    body = format!("{body}\n\n{block}");
                    new_line = Some(anchor);
                    start = (s.from_line < anchor).then_some(s.from_line as i32);
                    ready = true;
                }
                None => mismatched += 1,
            }
        }
        let note = LineNote {
            path: c.path.clone(),
            new_line: new_line.map(|n| n as i32),
            old_line: c.old_line.filter(|_| c.new_line.is_none()).map(|o| o as i32),
            body,
        };
        if already.contains(&note) || notes.iter().any(|(n, ..)| n == &note) {
            continue;
        }
        notes.push((note, start, ready));
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
        1 => "1 comment on the changed lines".to_string(),
        n => format!("{n} comments on the changed lines"),
    });
    let ready = notes.iter().filter(|(.., ready)| *ready).count();
    if !notes.is_empty() {
        summary.push_str(&match ready {
            0 => ".".to_string(),
            1 => ", 1 with a change to apply.".to_string(),
            n => format!(", {n} with a change to apply."),
        });
    }
    if dropped > 0 {
        summary.push_str(&format!(" {dropped} more were left out (not on a changed line, or past the limit)."));
    }
    if mismatched > 0 {
        let n = mismatched;
        summary.push_str(&format!(
            " {n} suggested change{} did not match the code and {} left out (the comment{} posted as text).",
            if n == 1 { "" } else { "s" },
            if n == 1 { "was" } else { "were" },
            if n == 1 { " is" } else { "s are" }
        ));
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
    for (LineNote { path, new_line, old_line, body }, start_new_line, _) in &notes {
        let pos = mrdb::Position {
            diff_id: Some(latest.id),
            base_sha: Some(&latest.base_sha),
            start_sha: Some(&latest.start_sha),
            head_sha: Some(&latest.head_sha),
            old_path: Some(lines.old_path.get(path).unwrap_or(path)),
            new_path: Some(path),
            old_line: *old_line,
            new_line: *new_line,
            start_old_line: None,
            start_new_line: *start_new_line,
        };
        add_bot_note(state, project, mr, bot, body, pos).await?;
    }
    mrdb::touch(&state.db, mr.id).await?;
    Ok((notes.len(), dropped))
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
    if let Some(note) = mrdb::note(&state.db, mr.id, id).await? {
        super::mr::note_webhook(state, p, mr, &note, &bot.username).await;
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
        assert_eq!(j.script, vec!["forgust-review"]);
        assert_eq!(j.artifacts.as_ref().unwrap().reports[REPORT], vec![report::FILE]);
    }

    #[test]
    fn suggestions_hold_up_or_are_left_out() {
        let file = "a\n    let x = 1 / 0;\n    y\nz\n";
        let s = |from, to, original: &str, code: &str| report::Suggestion {
            from_line: from,
            to_line: to,
            original: original.into(),
            code: code.into(),
        };
        let all = |_: u32| true;
        // on the commented line, the model's trailing spaces forgiven
        let (at, block) = checked_suggestion(Some(file), all, 2, &s(2, 2, "    let x = 1 / 0;  ", "    let x = 1;")).unwrap();
        assert_eq!((at, block.as_str()), (2, "```suggestion:-0+0\n    let x = 1;\n```"));
        // a range: the comment moves into it
        let (at, block) = checked_suggestion(Some(file), all, 4, &s(2, 3, "    let x = 1 / 0;\n    y", "")).unwrap();
        assert_eq!((at, block.as_str()), (3, "```suggestion:-1+0\n```"));
        let r = crate::services::suggestions::parse(&format!("text\n\n{block}")).unwrap();
        assert_eq!((r.above, r.below, r.lines.len()), (1, 0, 0), "what Apply reads");
        // the lines do not read that (wrong number, wrong indentation)
        assert!(checked_suggestion(Some(file), all, 2, &s(3, 3, "    let x = 1 / 0;", "x")).is_none());
        assert!(checked_suggestion(Some(file), all, 2, &s(2, 2, "let x = 1 / 0;", "x")).is_none());
        // not in the diff, no change, a fence, too long, no file
        assert!(checked_suggestion(Some(file), |n| n != 2, 2, &s(2, 2, "    let x = 1 / 0;", "x")).is_none());
        assert!(checked_suggestion(Some(file), all, 2, &s(2, 2, "    let x = 1 / 0;", "    let x = 1 / 0;")).is_none());
        assert!(checked_suggestion(Some(file), all, 2, &s(2, 2, "    let x = 1 / 0;", "```\nx")).is_none());
        assert!(checked_suggestion(Some(file), all, 2, &s(1, 30, "a", "x")).is_none());
        assert!(checked_suggestion(None, all, 2, &s(2, 2, "    let x = 1 / 0;", "x")).is_none());
        assert_eq!(strip_suggestion_blocks("Fix:\n```suggestion\nbad\n```\nThanks"), "Fix:\nThanks");
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
