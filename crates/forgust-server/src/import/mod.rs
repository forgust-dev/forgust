//! Importing projects from GitLab and GitHub (ADR-0018).
//!
//! An import plans every project first (a dry run: counts and warnings), then
//! imports them one at a time per import, each in its own background job. Every
//! imported object is recorded in `imported_objects`, so a job that stops (rate
//! limit, restart) runs again and skips what is already there.

pub mod client;
pub mod github;
pub mod gitlab;
pub mod model;

use std::collections::HashMap;

use futures::stream::{self, StreamExt};

use forgust_db::access::{DEVELOPER, MAINTAINER, OWNER};
use forgust_db::imports::{self as idb, Import, ImportProject};
use forgust_db::issues::{self as issuedb, NewIssue, NewIssueNote};
use forgust_db::merge_requests::{self as mrdb, ImportedMergeRequest, ImportedNote, NewDiff, Position};
use forgust_db::milestones as milestonedb;
use forgust_db::namespaces::Namespace;
use forgust_db::projects::Project;
use forgust_storage::DiffOptions;
use graphile_worker::{IntoTaskHandlerResult, TaskHandler, WorkerContext};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::auth::authz::{GroupAction, ProjectAction, can_group, can_project};
use crate::jobs::JobRequest;
use crate::services;
use crate::state::AppState;
use model::*;

/// What an import brings besides the repository (all on by default).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct Options {
    pub issues: bool,
    pub merge_requests: bool,
    pub labels: bool,
    /// CI/CD variables (Owners of the target group only).
    pub variables: bool,
    pub protected_branches: bool,
    /// Members matched by email (Owners of the target group only).
    pub members: bool,
    /// Git LFS objects of the repository.
    pub lfs: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { issues: true, merge_requests: true, labels: true, variables: true, protected_branches: true, members: true, lfs: true }
    }
}

/// A connected source, after the URL check (ADR-0018: SSRF).
pub async fn connect(state: &AppState, kind: &str, base_url: &str, token: &str) -> Result<Box<dyn Source>> {
    let allow_local = state.cfg.features.imports_allow_local;
    match kind {
        "gitlab" => {
            let (url, addrs) = client::checked_url(&with_slash(base_url), allow_local).await?;
            Ok(Box::new(gitlab::GitLab::new(url, addrs, token.trim())?))
        }
        "github" => {
            let web = url::Url::parse(&with_slash(base_url)).map_err(|_| SourceError::Refused("invalid URL".into()))?;
            let (api, addrs) = client::checked_url(github::api_url(&web)?.as_str(), allow_local).await?;
            Ok(Box::new(github::GitHub::new(api, addrs, token.trim())?))
        }
        other => Err(SourceError::Refused(format!("unknown source `{other}`"))),
    }
}

fn with_slash(u: &str) -> String {
    let u = u.trim();
    if u.ends_with('/') { u.to_string() } else { format!("{u}/") }
}

/// The project path in `input`: `group/project`, or a URL of the project on the
/// source (`https://gitlab.com/group/project/-/tree/main`, `…/owner/repo.git`).
pub fn project_path(source: &str, base_url: &str, input: &str) -> Option<String> {
    let input = input.trim();
    let path = match url::Url::parse(input) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => {
            // only URLs on the forge being imported from
            let base = url::Url::parse(base_url.trim()).ok()?;
            fn host(u: &url::Url) -> Option<&str> {
                u.host_str().map(|h| h.trim_start_matches("www."))
            }
            if host(&u) != host(&base) {
                return None;
            }
            u.path().to_string()
        }
        _ => input.to_string(),
    };
    let path = path.trim_matches('/');
    // GitLab pages hang under `/-/`; GitHub's after owner/repo
    let path = path.split("/-/").next().unwrap_or(path);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let segs: Vec<&str> = if source == "github" { segs.into_iter().take(2).collect() } else { segs };
    let mut out = segs.join("/");
    if let Some(s) = out.strip_suffix(".git") {
        out = s.to_string();
    }
    let valid = out.split('/').count() >= 2 && out.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) && !out.contains("..");
    valid.then_some(out)
}

/// CI features outside the supported subset, found in a `.gitlab-ci.yml`.
pub fn ci_warnings(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    // includes are resolved when a pipeline runs; the parser alone cannot load them
    let parsed = match forgust_ci_config::parse(yaml) {
        Err(forgust_ci_config::ConfigError::Include(_)) | Ok(_) => None,
        Err(e) => Some(e.to_string()),
    };
    let mut seen = std::collections::BTreeSet::new();
    for line in yaml.lines() {
        let t = line.trim_start();
        let indented = t.len() != line.len();
        let key = t.trim_start_matches("- ").split(':').next().unwrap_or("").trim();
        let msg = match key {
            "secrets" if indented => "CI jobs use `secrets` (Vault and other secret managers), which is not supported; `id_tokens` is.",
            "pages" if !indented => {
                "The `pages` job publishes a static site: Forgust serves it when the instance has a Pages domain (`pages.domain`) and the project is public."
            }
            "template" if indented => {
                "The CI configuration includes GitLab's templates, which are not bundled here: copy them into the project."
            }
            "component" if indented => {
                "The CI configuration includes CI/CD components: they are read from projects on this server, so import (or mirror) the component projects too."
            }
            "kubernetes" if indented => "CI jobs use `environment:kubernetes`, which is not supported.",
            _ => "",
        };
        if !msg.is_empty() {
            seen.insert(msg);
        }
        if t.contains("$CI_REGISTRY") || t.contains("${CI_REGISTRY") {
            seen.insert("CI jobs use the container registry ($CI_REGISTRY): it works here too, but images already on GitLab are not copied (push them again, or skopeo copy --all).");
        }
    }
    // the parser stops at its first complaint; keep it when the scan did not say the same
    if let Some(e) = parsed {
        let said = ["secrets", "pages", "template", "component", "kubernetes", "include"]
            .iter()
            .any(|k| e.contains(k) && seen.iter().any(|m| m.contains(k)));
        if !said {
            out.push(format!("CI configuration: {e}"));
        }
    }
    out.extend(seen.into_iter().map(String::from));
    out
}

// ------------------------------------------------------------------ the job

/// Plan (dry run) or import one project of an import.
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportJob {
    pub import_project_id: i64,
    pub plan: bool,
}

impl ImportJob {
    pub fn request(self, import_id: i64) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            // one project at a time per import: polite to the source's rate limits
            queue: Some(format!("import:{import_id}")),
            job_key: Some(format!("import:{}:{}", self.import_project_id, if self.plan { "plan" } else { "run" })),
            payload: serde_json::to_value(&self).unwrap_or_default(),
            max_attempts: Some(6),
        }
    }
}

impl TaskHandler for ImportJob {
    const IDENTIFIER: &'static str = "import_project";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let Some(state) = ctx.get_ext::<AppState>() else { return Err("no app state".to_string()) };
        let last = ctx.job().attempts() >= ctx.job().max_attempts();
        run_job(state, &self, last).await
    }
}

async fn run_job(state: &AppState, job: &ImportJob, last_attempt: bool) -> std::result::Result<(), String> {
    let db = |e: sqlx::Error| e.to_string();
    let Some(row) = idb::project(&state.db, job.import_project_id).await.map_err(db)? else { return Ok(()) };
    let Some(import) = idb::find(&state.db, row.import_id).await.map_err(db)? else { return Ok(()) };
    if import.status == "canceled" || matches!(row.status.as_str(), "finished" | "skipped" | "canceled") {
        return Ok(());
    }
    let outcome = async {
        let token = token(state, &import).await?;
        let src = connect(state, &import.source, &import.base_url, &token).await?;
        let _notice = waiting_notice(state, row.id, &import.source, src.anonymous(), src.waiting());
        if job.plan {
            idb::set_project_status(&state.db, row.id, "planning", None).await.map_err(other)?;
            plan_project(state, &import, &row, src.as_ref()).await
        } else {
            idb::set_project_status(&state.db, row.id, "running", None).await.map_err(other)?;
            Runner::new(state, &import, row.clone(), src.as_ref()).await?.run().await
        }
    }
    .await;
    match outcome {
        Ok(()) => {
            let status = if job.plan { "planned" } else { "finished" };
            idb::set_project_status(&state.db, row.id, status, None).await.map_err(db)?;
        }
        Err(e) if e.retryable() && !last_attempt => {
            // the queue retries with backoff; the page shows why it waits
            let _ = idb::set_project_status(
                &state.db,
                row.id,
                if job.plan { "planning" } else { "running" },
                Some(&format!("will retry: {e}")),
            )
            .await;
            return Err(e.to_string());
        }
        Err(e) => {
            let canceled = idb::find(&state.db, import.id).await.map_err(db)?.is_some_and(|i| i.status == "canceled");
            if canceled {
                idb::set_project_status(&state.db, row.id, "canceled", None).await.map_err(db)?;
            } else {
                tracing::warn!(import = import.id, project = %row.source_path, error = %e, "import failed");
                idb::set_project_status(&state.db, row.id, "failed", Some(&e.to_string())).await.map_err(db)?;
            }
        }
    }
    settle(state, import.id).await.map_err(db)
}

/// Says under the project when the source makes it wait for a rate limit to
/// reset (up to an hour), so a quiet import does not look stuck.
fn waiting_notice(
    state: &AppState,
    row_id: i64,
    source: &str,
    anonymous: bool,
    waiting: Option<tokio::sync::watch::Receiver<i64>>,
) -> Option<AbortOnDrop> {
    let mut waiting = waiting?;
    let db = state.db.clone();
    let name = if source == "github" { "GitHub" } else { "GitLab" };
    // wakes only when a wait starts or ends; ends with the source's client
    let task = tokio::spawn(async move {
        while waiting.changed().await.is_ok() {
            let until = *waiting.borrow_and_update();
            let note = (until > 0).then(|| {
                let mins = (until - chrono::Utc::now().timestamp() + 59) / 60;
                let at = chrono::DateTime::from_timestamp(until, 0).unwrap_or_default().format("%H:%M");
                let hint = if anonymous { "; a token raises the limit" } else { "" };
                format!("Waiting about {mins} min for {name}'s rate limit to reset (until {at} UTC){hint}.")
            });
            let _ = idb::set_project_note(&db, row_id, note.as_deref()).await;
        }
    });
    Some(AbortOnDrop(task))
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// When no project is left in flight, the import is planned, finished or failed;
/// ending erases the token.
pub async fn settle(state: &AppState, import_id: i64) -> sqlx::Result<()> {
    let Some(import) = idb::find(&state.db, import_id).await? else { return Ok(()) };
    let counts: HashMap<String, i64> = idb::status_counts(&state.db, import_id).await?.into_iter().collect();
    let n = |s: &str| counts.get(s).copied().unwrap_or(0);
    let total: i64 = counts.values().sum();
    match import.status.as_str() {
        "planning" if n("pending") + n("planning") == 0 => idb::set_status(&state.db, import_id, "planned").await?,
        "running" if n("pending") + n("planned") + n("running") == 0 => {
            let failed_all = total > 0 && n("failed") == total;
            idb::set_status(&state.db, import_id, if failed_all { "failed" } else { "finished" }).await?
        }
        _ => {}
    }
    Ok(())
}

async fn token(state: &AppState, import: &Import) -> Result<String> {
    let sealed = idb::token(&state.db, import.id)
        .await
        .map_err(other)?
        .ok_or_else(|| SourceError::Other("the import has ended: its token was erased".into()))?;
    let raw = state.secrets.decrypt(&sealed).map_err(|e| SourceError::Other(format!("token: {e}")))?;
    String::from_utf8(raw).map_err(|_| SourceError::Other("token: not UTF-8".into()))
}

fn other(e: impl std::fmt::Display) -> SourceError {
    SourceError::Other(e.to_string())
}

async fn principal(state: &AppState, import: &Import) -> Result<Principal> {
    let user = forgust_db::users::find(&state.db, import.user_id)
        .await
        .map_err(other)?
        .ok_or_else(|| SourceError::Other("the user who started the import is gone".into()))?;
    if user.state != "active" {
        return Err(SourceError::Refused("the user who started the import is blocked".into()));
    }
    Ok(Principal::User { user, scopes: None })
}

/// Dry run: what the source has, what will not come over, and whether the
/// target path is free.
async fn plan_project(state: &AppState, import: &Import, row: &ImportProject, src: &dyn Source) -> Result<()> {
    let sp = src.project(&row.source_id).await?;
    let mut report = src.plan(&sp).await?;
    let full = format!("{}/{}", import.target_path, row.target_path);
    if row.project_id.is_none() && forgust_db::routes::path_taken(&state.db, &full).await.map_err(other)? {
        report.warnings.insert(0, format!("`{full}` is already taken here: choose another path or remove it before importing."));
    }
    let options: Options = serde_json::from_value(import.options.clone()).unwrap_or_default();
    let principal = principal(state, import).await?;
    let target = forgust_db::namespaces::find(&state.db, import.target_namespace_id)
        .await
        .map_err(other)?
        .ok_or_else(|| other("the target group is gone"))?;
    if (options.variables || options.members) && !owner_of(state, &principal, &target).await? {
        report.warnings.push("Variables, protected branches and members are imported only by an Owner of the target group.".into());
    }
    idb::set_report(&state.db, row.id, &serde_json::to_value(&report).unwrap_or_default()).await.map_err(other)?;
    Ok(())
}

async fn owner_of(state: &AppState, principal: &Principal, ns: &Namespace) -> Result<bool> {
    can_group(state, principal, GroupAction::Own, ns).await.map_err(|e| other(format!("{e:?}")))
}

// ------------------------------------------------------------------ importing one project

struct Runner<'a> {
    state: &'a AppState,
    import: &'a Import,
    row: ImportProject,
    src: &'a dyn Source,
    principal: Principal,
    options: Options,
    owner: bool,
    /// Source user id → (local user, display name when unmatched).
    users: HashMap<String, (Option<i64>, Option<String>)>,
    warnings: Vec<String>,
    /// The source refused comments without a token: stop asking for them.
    no_comments: bool,
    /// MRs whose source branch was in a fork (one warning for all).
    forks: Vec<String>,
    /// Milestone title → local id, once loaded (`milestones`).
    milestones: Option<HashMap<String, i64>>,
    /// The project was made by an earlier run: its refs are not overwritten.
    existing: bool,
    /// Label name → local id, filled as issues and merge requests use them.
    label_cache: std::sync::Mutex<HashMap<String, i64>>,
}

impl<'a> Runner<'a> {
    async fn new(state: &'a AppState, import: &'a Import, row: ImportProject, src: &'a dyn Source) -> Result<Runner<'a>> {
        let principal = principal(state, import).await?;
        let target = forgust_db::namespaces::find(&state.db, import.target_namespace_id)
            .await
            .map_err(other)?
            .ok_or_else(|| other("the target group is gone"))?;
        let owner = owner_of(state, &principal, &target).await?;
        let options = serde_json::from_value(import.options.clone()).unwrap_or_default();
        Ok(Runner {
            state,
            import,
            row,
            src,
            principal,
            options,
            owner,
            users: HashMap::new(),
            warnings: Vec::new(),
            no_comments: false,
            forks: Vec::new(),
            milestones: None,
            existing: false,
            label_cache: Default::default(),
        })
    }

    async fn stage(&self, s: &str) -> Result<()> {
        // a canceled import stops between stages
        if idb::find(&self.state.db, self.import.id).await.map_err(other)?.is_some_and(|i| i.status == "canceled") {
            return Err(SourceError::Refused("the import was canceled".into()));
        }
        idb::set_stage(&self.state.db, self.row.id, s).await.map_err(other)
    }

    async fn progress(&self, kind: &str, done: usize, total: usize) {
        let _ = idb::set_progress(&self.state.db, self.row.id, kind, done as i64, total as i64).await;
    }

    async fn run(mut self) -> Result<()> {
        let sp = self.src.project(&self.row.source_id).await?;
        self.stage("project").await?;
        let p = self.local_project(&sp).await?;
        if !sp.empty {
            self.stage("repository").await?;
            self.repository(&p, &sp).await?;
            if self.options.lfs {
                self.stage("lfs").await?;
                self.lfs(&p, &sp).await?;
            }
        }
        if self.options.labels {
            self.stage("labels").await?;
            let r = self.labels(&p, &sp).await;
            self.optional("Labels", r)?;
        }
        if self.options.issues && sp.issues_enabled {
            self.stage("issues").await?;
            let r = self.issues(&p, &sp).await;
            self.optional("Issues", r)?;
        }
        if self.options.merge_requests && !sp.empty {
            self.stage("merge_requests").await?;
            let r = self.merge_requests(&p, &sp).await;
            self.optional("Merge requests", r)?;
        }
        if self.owner {
            if self.options.variables {
                self.stage("variables").await?;
                self.variables(&p, &sp).await?;
            }
            if self.options.protected_branches {
                self.stage("protected_branches").await?;
                self.protected_branches(&p, &sp).await?;
            }
            if self.options.members {
                self.stage("members").await?;
                self.members(&p, &sp).await?;
            }
        }
        self.stage("cleanup").await?;
        for r in self.state.storage.refs(p.id, "refs/import/").await.map_err(other)? {
            let _ = self.state.storage.delete_ref(p.id, &r.name, None).await;
        }
        // keep the dry-run report, add what this run left out
        let mut report: Report = serde_json::from_value(self.row.report.clone()).unwrap_or_default();
        for w in self.warnings.drain(..) {
            if !report.warnings.contains(&w) {
                report.warnings.push(w);
            }
        }
        idb::set_report(&self.state.db, self.row.id, &serde_json::to_value(&report).unwrap_or_default()).await.map_err(other)?;
        Ok(())
    }

    /// A part the source may refuse (feature off, token too narrow): note it and go on.
    fn optional(&mut self, what: &str, r: Result<()>) -> Result<()> {
        match r {
            Err(SourceError::Refused(e) | SourceError::NotFound(e)) => {
                self.warnings.push(format!("{what} could not be read from the source and were not imported ({e})."));
                Ok(())
            }
            r => r,
        }
    }

    // -------------------------------------------------------------- people

    /// The local account for a source user (by email), or the name to show.
    async fn who(&mut self, u: Option<&SourceUser>) -> Result<(Option<i64>, Option<String>)> {
        let Some(u) = u else { return Ok((None, Some("ghost".into()))) };
        if let Some(hit) = self.users.get(&u.id) {
            return Ok(hit.clone());
        }
        let known = idb::user(&self.state.db, self.import.id, &u.id).await.map_err(other)?;
        let email = match &known {
            Some(k) if k.email.is_some() => k.email.clone(),
            _ => self.src.user_email(u).await?,
        };
        let local = idb::upsert_user(&self.state.db, self.import.id, &u.id, &u.username, &u.name, email.as_deref()).await.map_err(other)?;
        let display =
            if u.name.is_empty() || u.name == u.username { format!("@{}", u.username) } else { format!("{} (@{})", u.name, u.username) };
        let hit = match local {
            Some(id) => (Some(id), None),
            None => (None, Some(display)),
        };
        self.users.insert(u.id.clone(), hit.clone());
        Ok(hit)
    }

    // -------------------------------------------------------------- project

    async fn local_project(&mut self, sp: &SourceProject) -> Result<Project> {
        if let Some(id) = self.row.project_id
            && let Some(p) = forgust_db::projects::find(&self.state.db, id).await.map_err(other)?
        {
            if p.archived() {
                return Err(other(anyhow::anyhow!("{} is archived: unarchive it to import into it", p.full_path)));
            }
            // a run again into a project made earlier: the importer must still
            // run it (a removed or demoted importer must not overwrite it), and
            // the Owner-only stages go by where the project is now
            if !can_project(self.state, &self.principal, ProjectAction::Admin, &p).await.map_err(|e| other(format!("{e:?}")))? {
                return Err(other(anyhow::anyhow!("you are no longer a Maintainer of {}: it is not imported into again", p.full_path)));
            }
            let ns = forgust_db::namespaces::find(&self.state.db, p.namespace_id)
                .await
                .map_err(other)?
                .ok_or_else(|| other("the project's group is gone"))?;
            self.owner = owner_of(self.state, &self.principal, &ns).await?;
            self.existing = true;
            return Ok(p);
        }
        let mut ns = forgust_db::namespaces::find(&self.state.db, self.import.target_namespace_id)
            .await
            .map_err(other)?
            .ok_or_else(|| other("the target group is gone"))?;
        let segments: Vec<String> = self.row.target_path.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect();
        let (dirs, name) = segments.split_at(segments.len().saturating_sub(1));
        let path = name.first().cloned().unwrap_or_else(|| sp.path.clone());
        for dir in dirs {
            ns = self.subgroup(&ns, dir).await?;
        }
        let source = forgust_db::visibility::parse(&sp.visibility).unwrap_or(forgust_db::visibility::PRIVATE);
        // the group or the organization's policy may allow less than the source had
        let max = services::groups::max_visibility(self.state, &ns).await.map_err(api)?;
        let wanted = source.min(max);
        if wanted < source && max < ns.visibility {
            self.warnings.push(format!("Imported as {}: the organization allows at most that.", forgust_db::visibility::name(wanted)));
        }
        let create = |visibility: i16| services::projects::CreateProject {
            name: &sp.name,
            path: &path,
            description: &sp.description,
            visibility,
            default_branch: sp.default_branch.as_deref(),
            ref_format: Default::default(),
        };
        let p = services::projects::create(self.state, &self.principal, &ns, create(wanted)).await.map_err(api)?;
        idb::set_local_project(&self.state.db, self.row.id, p.id).await.map_err(other)?;
        self.row.project_id = Some(p.id);
        Ok(p)
    }

    /// The subgroup `path` under `parent`, created when missing.
    async fn subgroup(&self, parent: &Namespace, path: &str) -> Result<Namespace> {
        let full = format!("{}/{}", parent.full_path, path);
        let mut conn = self.state.db.acquire().await.map_err(other)?;
        if let Some(hit) = forgust_db::routes::lookup(&mut conn, &full).await.map_err(other)? {
            if hit.source_type != "namespace" {
                return Err(SourceError::Refused(format!("`{full}` is a project here, not a group")));
            }
            return forgust_db::namespaces::find(&self.state.db, hit.source_id)
                .await
                .map_err(other)?
                .ok_or_else(|| other("group vanished"));
        }
        drop(conn);
        services::groups::create(
            self.state,
            &self.principal,
            services::groups::CreateGroup { name: path, path, parent_id: Some(parent.id), visibility: parent.visibility, description: "" },
        )
        .await
        .map_err(api)
    }

    // -------------------------------------------------------------- repository

    /// The source repository's URL (checked) and git's settings for it (auth, pinned address).
    async fn git_remote(&self, sp: &SourceProject) -> Result<(url::Url, Vec<(String, String)>)> {
        let (url, addrs) = client::checked_url(&sp.http_url, self.state.cfg.features.imports_allow_local).await?;
        let mut cfg: Vec<(String, String)> = self.src.git_auth().into_iter().filter(|(k, _)| k != "http.curloptResolve").collect();
        cfg.extend(gitlab::pin(&url, &addrs));
        Ok((url, cfg))
    }

    async fn repository(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        let (url, cfg) = self.git_remote(sp).await?;
        // into a project that already has content (a run again): the source's
        // branches and tags land aside, and only missing ones are created
        // (never a forced update of what is there: no push rules or protected
        // branches would see it)
        let (heads, tags) = if self.existing { ("refs/import/heads", "refs/import/tags") } else { ("refs/heads", "refs/tags") };
        let refspecs = vec![
            format!("+refs/heads/*:{heads}/*"),
            format!("+refs/tags/*:{tags}/*"),
            format!("+{}:refs/import/mr/*", self.src.mr_ref_glob()),
        ];
        self.state
            .storage
            .fetch(p.id, url.as_str(), &refspecs, &cfg)
            .await
            .map_err(|e| SourceError::Transient(format!("git fetch: {e}")))?;
        if self.existing {
            for (aside, real) in [("refs/import/heads/", "refs/heads/"), ("refs/import/tags/", "refs/tags/")] {
                for r in self.state.storage.refs(p.id, aside).await.map_err(other)? {
                    let name = format!("{real}{}", r.name.strip_prefix(aside).unwrap_or(&r.name));
                    // only where nothing is: an existing branch or tag stays as it is
                    match self.state.storage.update_ref(p.id, &name, &r.target, Some(forgust_storage::ZERO_OID)).await {
                        Ok(()) | Err(forgust_storage::StorageError::RefChanged(_)) => {}
                        Err(e) => return Err(other(e)),
                    }
                }
            }
        }
        if let Some(b) = &sp.default_branch
            && self.state.storage.resolve(p.id, &format!("refs/heads/{b}")).await.map_err(other)?.is_some()
        {
            self.state.storage.set_default_branch(p.id, b).await.map_err(other)?;
        }
        Ok(())
    }

    // -------------------------------------------------------------- LFS

    /// Copy the LFS objects the history points to, through the source's batch API.
    async fn lfs(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        let st = &self.state.storage;
        let Some(branch) = &sp.default_branch else { return Ok(()) };
        let Some(head) = st.resolve(p.id, &format!("refs/heads/{branch}")).await.map_err(other)? else { return Ok(()) };
        let uses_lfs =
            st.blob(p.id, &head, ".gitattributes", 64 * 1024).await.is_ok_and(|b| String::from_utf8_lossy(&b.data).contains("filter=lfs"));
        if !uses_lfs {
            return Ok(());
        }
        let mut wanted: Vec<(String, u64)> = Vec::new();
        for id in st.small_blobs(p.id, 1024).await.map_err(other)? {
            let data = st.blob_by_id(p.id, &id, 1024).await.map_err(other)?;
            if let Some(ptr) = crate::lfs::parse_pointer(&data)
                && !wanted.iter().any(|(o, _)| *o == ptr.0)
            {
                wanted.push(ptr);
            }
        }
        let mut missing = Vec::new();
        for (oid, size) in wanted {
            if forgust_db::lfs::linked_size(&self.state.db, p.id, &oid).await.map_err(other)?.is_none() {
                missing.push((oid, size));
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let allow_local = self.state.cfg.features.imports_allow_local;
        let (repo_url, addrs) = client::checked_url(&sp.http_url, allow_local).await?;
        let batch = format!("{}/info/lfs/objects/batch", repo_url.as_str().trim_end_matches('/'));
        let auth = self
            .src
            .git_auth()
            .into_iter()
            .find(|(k, _)| k == "http.extraHeader")
            .and_then(|(_, v)| v.strip_prefix("Authorization: ").map(str::to_string));
        let api = lfs_client(&repo_url, &addrs)?;
        let total = missing.len();
        let mut done = 0;
        let mut failed = Vec::new();
        self.progress("lfs", 0, total).await;
        for chunk in missing.chunks(100) {
            let objects: Vec<serde_json::Value> = chunk.iter().map(|(oid, size)| json!({ "oid": oid, "size": size })).collect();
            let mut req = api
                .post(&batch)
                .header("accept", "application/vnd.git-lfs+json")
                .header("content-type", "application/vnd.git-lfs+json")
                .body(json!({ "operation": "download", "transfers": ["basic"], "objects": objects }).to_string());
            if let Some(a) = &auth {
                req = req.header("authorization", a);
            }
            let resp = req.send().await.map_err(|e| SourceError::Transient(format!("LFS batch: {e}")))?;
            if !resp.status().is_success() {
                self.warnings.push(format!("Git LFS objects were not copied: the source's LFS API answered {}.", resp.status()));
                return Ok(());
            }
            let body: LfsBatch = resp.json().await.map_err(|e| SourceError::Other(format!("LFS batch: {e}")))?;
            for o in body.objects {
                match o.actions.and_then(|a| a.download) {
                    Some(d) => match self.lfs_download(p, &o.oid, &d).await {
                        Ok(()) => done += 1,
                        Err(e) => failed.push(format!("{} ({e})", &o.oid[..o.oid.len().min(12)])),
                    },
                    None => failed.push(format!(
                        "{} ({})",
                        &o.oid[..o.oid.len().min(12)],
                        o.error.map(|e| e.message).unwrap_or_else(|| "not on the source".into())
                    )),
                }
                self.progress("lfs", done + failed.len(), total).await;
            }
        }
        if !failed.is_empty() {
            self.warnings.push(format!(
                "{} Git LFS object(s) could not be copied: {}.",
                failed.len(),
                failed.into_iter().take(10).collect::<Vec<_>>().join(", ")
            ));
        }
        Ok(())
    }

    async fn lfs_download(&self, p: &Project, oid: &str, d: &LfsAction) -> std::result::Result<(), String> {
        use futures::{StreamExt, TryStreamExt};
        let (url, addrs) = client::checked_url(&d.href, self.state.cfg.features.imports_allow_local).await.map_err(|e| e.to_string())?;
        let mut req = lfs_client(&url, &addrs).map_err(|e| e.to_string())?.get(url.as_str());
        for (k, v) in &d.header {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(resp.status().to_string());
        }
        let body = resp.bytes_stream().map_err(std::io::Error::other).boxed();
        crate::lfs::store_verified(self.state, p.id, oid, body).await.map(|_| ())
    }

    // -------------------------------------------------------------- labels

    async fn labels(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        let labels = self.src.labels(sp).await?;
        let existing = issuedb::labels(&self.state.db, p.id).await.map_err(other)?;
        for l in &labels {
            if existing.iter().any(|e| e.name == l.name) {
                continue;
            }
            let name: String = l.name.chars().take(255).collect::<String>().replace(',', " ");
            issuedb::upsert_label(&self.state.db, p.id, &name, &l.color, &l.description).await.map_err(other)?;
        }
        Ok(())
    }

    /// Label ids for names, creating the ones the project lacks.
    async fn label_ids(&self, p: &Project, names: &[String]) -> Result<Vec<i64>> {
        let names: Vec<String> =
            names.iter().map(|n| n.chars().take(255).collect::<String>().replace(',', " ")).filter(|n| !n.trim().is_empty()).collect();
        // known ones from earlier items; the others in one query, then made
        let missing: Vec<String> = {
            let cache = self.label_cache.lock().expect("not poisoned");
            names.iter().filter(|n| !cache.contains_key(*n)).cloned().collect()
        };
        if !missing.is_empty() {
            let found = issuedb::labels_by_name(&self.state.db, p.id, &missing).await.map_err(other)?;
            let mut known: HashMap<String, i64> = found.into_iter().map(|l| (l.name, l.id)).collect();
            for n in &missing {
                if !known.contains_key(n) {
                    known.insert(n.clone(), issuedb::upsert_label(&self.state.db, p.id, n, "#64748b", "").await.map_err(other)?.id);
                }
            }
            self.label_cache.lock().expect("not poisoned").extend(known);
        }
        let cache = self.label_cache.lock().expect("not poisoned");
        Ok(names.iter().filter_map(|n| cache.get(n).copied()).collect())
    }

    // -------------------------------------------------------------- milestones

    /// The project's milestones from the source, created here once (by title, so
    /// a rerun adds nothing); before the issues and merge requests that use them.
    async fn milestones(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        if self.milestones.is_some() {
            return Ok(());
        }
        let list = match self.src.milestones(sp).await {
            Ok(l) => l,
            Err(e) => {
                self.warnings.push(format!("Milestones could not be read ({e}); the ones issues use are still created."));
                Vec::new()
            }
        };
        self.milestones = Some(milestonedb::ids_by_title(&self.state.db, p.id).await.map_err(other)?.into_iter().collect());
        for m in &list {
            self.milestone_id(p, Some(m)).await?;
        }
        Ok(())
    }

    /// The local milestone for a source one, created when missing (a GitLab
    /// group milestone becomes the project's).
    async fn milestone_id(&mut self, p: &Project, m: Option<&SourceMilestone>) -> Result<Option<i64>> {
        let Some(m) = m else { return Ok(None) };
        let title = truncate(m.title.trim(), 255);
        if title.is_empty() {
            return Ok(None);
        }
        let map = self.milestones.get_or_insert_with(HashMap::new);
        if let Some(id) = map.get(&title) {
            return Ok(Some(*id));
        }
        let mut conn = self.state.db.acquire().await.map_err(other)?;
        let fields =
            milestonedb::MilestoneFields { title: &title, description: &m.description, start_date: m.start_date, due_date: m.due_date };
        let id = match milestonedb::create(&mut conn, p.id, fields, if m.open { "active" } else { "closed" }).await.map_err(other)? {
            Some(id) => id,
            // made meanwhile (a rerun): look it up
            None => milestonedb::ids_by_title(&mut *conn, p.id)
                .await
                .map_err(other)?
                .into_iter()
                .find(|(t, _)| *t == title)
                .map(|(_, id)| id)
                .ok_or_else(|| other("milestone vanished"))?,
        };
        map.insert(title, id);
        Ok(Some(id))
    }

    // -------------------------------------------------------------- issues

    async fn issues(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        self.milestones(p, sp).await?;
        let issues = self.src.issues(sp).await?;
        let total = issues.len();
        let max_iid = issues.iter().map(|i| i.iid).max().unwrap_or(0);
        let done_ids = idb::imported_ids(&self.state.db, p.id, "issue").await.map_err(other)?;
        let todo: Vec<&SourceIssue> = issues.iter().filter(|i| !done_ids.contains(&i.id)).collect();
        let mut done = total - todo.len();
        let src = self.src;
        for window in todo.chunks(WINDOW) {
            // comments of a window at once, then the issues in order
            let skip = self.no_comments;
            let futs: Vec<_> =
                window.iter().map(|i| async move { if skip { Ok(Vec::new()) } else { src.issue_comments(sp, i).await } }).collect();
            let comments: Vec<Result<Vec<SourceComment>>> = stream::iter(futs).buffered(src.concurrency()).collect().await;
            for (issue, comments) in window.iter().zip(comments) {
                let comments = self.comments_or_none(comments)?;
                self.insert_issue(p, issue, &comments).await?;
                done += 1;
            }
            self.progress("issues", done, total).await;
        }
        mrdb::bump_iid(&self.state.db, p.id, "issues", max_iid).await.map_err(other)?;
        self.progress("issues", total, total).await;
        Ok(())
    }

    async fn insert_issue(&mut self, p: &Project, issue: &SourceIssue, comments: &[SourceComment]) -> Result<()> {
        let (author_id, imported_author) = self.who(issue.author.as_ref()).await?;
        let mut people = Vec::with_capacity(comments.len());
        for c in comments {
            people.push(self.who(c.author.as_ref()).await?);
        }
        let labels = self.label_ids(p, &issue.labels).await?;
        let milestone_id = self.milestone_id(p, issue.milestone.as_ref()).await?;
        let mut tx = self.state.db.begin().await.map_err(other)?;
        let taken = issuedb::find(&mut *tx, p.id, issue.iid).await.map_err(other)?.is_some();
        let iid = if taken { mrdb::next_iid(&mut *tx, p.id, "issues").await.map_err(other)? } else { issue.iid };
        let id = issuedb::insert(
            &mut *tx,
            NewIssue {
                project_id: p.id,
                iid,
                title: &truncate(&issue.title, 255),
                description: &issue.description,
                author_id,
                imported_author: imported_author.as_deref(),
                state: if issue.open { "opened" } else { "closed" },
                created_at: Some(issue.created_at),
                updated_at: Some(issue.updated_at),
                closed_at: if issue.open { None } else { issue.closed_at.or(Some(issue.updated_at)) },
                milestone_id,
                due_date: issue.due_date,
            },
        )
        .await
        .map_err(other)?;
        issuedb::set_issue_labels(&mut tx, id, &labels).await.map_err(other)?;
        for (c, (uid, name)) in comments.iter().zip(&people) {
            issuedb::add_note(
                &mut *tx,
                NewIssueNote {
                    issue_id: id,
                    author_id: *uid,
                    imported_author: name.as_deref(),
                    body: &c.body,
                    system: false,
                    created_at: Some(c.created_at),
                },
            )
            .await
            .map_err(other)?;
        }
        idb::record(&mut *tx, p.id, "issue", &issue.id, id).await.map_err(other)?;
        tx.commit().await.map_err(other)?;
        Ok(())
    }

    // -------------------------------------------------------------- merge requests

    /// MR heads the repository fetch did not bring: the source no longer
    /// advertises their refs (gitlab.com drops `refs/merge-requests/*` of merged
    /// and closed merge requests) but still serves the commits by id. Fetched
    /// by id into the MR's import ref; one the source refuses stays missing.
    async fn fetch_missing_heads(&self, p: &Project, sp: &SourceProject, mrs: &[&SourceMergeRequest]) -> Result<()> {
        let st = &self.state.storage;
        let mut specs = Vec::new();
        for m in mrs {
            let sha = &m.head_sha;
            // a commit id only: never a ref name from the source
            if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            if st.commit(p.id, sha).await.ok().flatten().is_some()
                || st.resolve(p.id, &format!("refs/import/mr/{}", m.iid)).await.map_err(other)?.is_some()
            {
                continue;
            }
            specs.push(format!("+{sha}:refs/import/mr/{}", m.iid));
        }
        if specs.is_empty() {
            return Ok(());
        }
        let (url, cfg) = self.git_remote(sp).await?;
        for batch in specs.chunks(100) {
            if let Err(e) = st.fetch(p.id, url.as_str(), batch, &cfg).await {
                // one commit the source refuses fails the batch: then one by one
                tracing::info!(project_id = p.id, error = %e, "import: merge request heads by id, one at a time");
                for spec in batch {
                    let _ = st.fetch(p.id, url.as_str(), std::slice::from_ref(spec), &cfg).await;
                }
            }
        }
        Ok(())
    }

    async fn merge_requests(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        self.milestones(p, sp).await?;
        let mrs = self.src.merge_requests(sp).await?;
        let total = mrs.len();
        let max_iid = mrs.iter().map(|m| m.iid).max().unwrap_or(0);
        let done_ids = idb::imported_ids(&self.state.db, p.id, "merge_request").await.map_err(other)?;
        let todo: Vec<&SourceMergeRequest> = mrs.iter().filter(|m| !done_ids.contains(&m.id)).collect();
        self.fetch_missing_heads(p, sp, &todo).await?;
        let mut done = total - todo.len();
        let mut skipped = Vec::new();
        let (state, src) = (self.state, self.src);
        for window in todo.chunks(WINDOW) {
            // threads and git work of a window at once; rows in order; refs in one go
            let skip = self.no_comments;
            let futs: Vec<_> = window.iter().map(|m| prepare_mr(state, src, p, sp, m, skip)).collect();
            let prepared: Vec<Result<Option<PreparedMr>>> = stream::iter(futs).buffered(src.concurrency()).collect().await;
            let mut refs = Vec::new();
            for (mr, prep) in window.iter().zip(prepared) {
                match prep? {
                    Some(mut prep) => {
                        if let Some(threads) = prep.threads.take() {
                            refs.extend(self.insert_mr(p, mr, prep, threads).await?);
                        }
                    }
                    None => skipped.push(format!("!{}", mr.iid)),
                }
                done += 1;
            }
            // after the rows: a rerun finds these MRs recorded, so write their refs now
            state.storage.update_refs(p.id, &refs).await.map_err(other)?;
            self.progress("merge_requests", done, total).await;
        }
        if !skipped.is_empty() {
            self.warnings.push(format!(
                "{} merge request{} had no commits left on the source and {} not imported: {}.",
                skipped.len(),
                if skipped.len() == 1 { "" } else { "s" },
                if skipped.len() == 1 { "was" } else { "were" },
                skipped.join(", ")
            ));
        }
        if !self.forks.is_empty() {
            let forks = std::mem::take(&mut self.forks);
            self.warnings.push(if forks.len() == 1 {
                format!("{} came from a fork; it is imported with its commits, but the fork is not.", forks[0])
            } else {
                format!(
                    "{} merge requests came from forks; they are imported with their commits, but the forks are not: {}.",
                    forks.len(),
                    forks.join(", ")
                )
            });
        }
        mrdb::bump_iid(&self.state.db, p.id, "merge_requests", max_iid).await.map_err(other)?;
        self.progress("merge_requests", total, total).await;
        Ok(())
    }

    /// Comments the source refuses to show (gitlab.com without a token): none,
    /// said once in the report.
    fn comments_or_none<T>(&mut self, r: Result<Vec<T>>) -> Result<Vec<T>> {
        match r {
            Err(SourceError::Refused(_)) => {
                // without a token it is the same for every item; with one, a
                // refusal may be about this item only
                self.no_comments |= self.src.anonymous();
                let w =
                    "Comments could not be read (the source shows them only with a token): issues and merge requests came without them."
                        .to_string();
                if !self.warnings.contains(&w) {
                    self.warnings.push(w);
                }
                Ok(Vec::new())
            }
            r => r,
        }
    }

    /// Write one prepared MR; returns the refs to set (its head, kept-around commits).
    async fn insert_mr(
        &mut self,
        p: &Project,
        mr: &SourceMergeRequest,
        prep: PreparedMr,
        threads: Result<Vec<SourceThread>>,
    ) -> Result<Vec<(String, String)>> {
        let threads = self.comments_or_none(threads)?;
        let PreparedMr { head, base, start, commits, files, additions, deletions, bases, keep, .. } = prep;
        let (author_id, imported_author) = self.who(mr.author.as_ref()).await?;
        let merged_by = self.who(mr.merged_by.as_ref()).await?.0;
        let mut people = HashMap::new();
        for t in &threads {
            for n in &t.notes {
                let w = self.who(n.author.as_ref()).await?;
                people.insert(n.id.clone(), w);
            }
        }
        let labels = self.label_ids(p, &mr.labels).await?;
        let milestone_id = self.milestone_id(p, mr.milestone.as_ref()).await?;
        if mr.from_fork {
            self.forks.push(format!("!{}", mr.iid));
        }

        let mut tx = self.state.db.begin().await.map_err(other)?;
        let taken = mrdb::find(&mut *tx, p.id, mr.iid).await.map_err(other)?.is_some();
        let iid = if taken { mrdb::next_iid(&mut *tx, p.id, "merge_requests").await.map_err(other)? } else { mr.iid };
        let id = mrdb::insert_imported(
            &mut *tx,
            ImportedMergeRequest {
                project_id: p.id,
                iid,
                title: &truncate(&mr.title, 255),
                description: &mr.description,
                author_id,
                imported_author: imported_author.as_deref(),
                source_branch: &mr.source_branch,
                target_branch: &mr.target_branch,
                state: mr.state.as_str(),
                draft: mr.draft,
                squash: mr.squash,
                merge_commit_sha: mr.merge_commit_sha.as_deref(),
                merged_by,
                merged_at: mr.merged_at,
                closed_at: mr.closed_at,
                created_at: mr.created_at,
                updated_at: mr.updated_at,
                milestone_id,
            },
        )
        .await
        .map_err(other)?;
        let diff = mrdb::add_diff(
            &mut tx,
            id,
            NewDiff {
                base_sha: &base,
                start_sha: &start,
                head_sha: &head,
                commits_count: commits,
                files_count: files,
                additions,
                deletions,
            },
            Some(mr.updated_at),
        )
        .await
        .map_err(other)?;
        issuedb::set_mr_labels(&mut tx, id, &labels).await.map_err(other)?;
        for t in &threads {
            let mut parent = None;
            for (i, n) in t.notes.iter().enumerate() {
                let (uid, name) = people.get(&n.id).cloned().unwrap_or((None, None));
                let pos = match &n.position {
                    Some(pp) if i == 0 && (pp.old_line.is_some() || pp.new_line.is_some()) => Position {
                        diff_id: (pp.head_sha == head).then_some(diff.id),
                        base_sha: pp.base_sha.as_deref().or(bases.get(&pp.head_sha).map(|b| b.0.as_str())),
                        start_sha: pp.start_sha.as_deref().or(bases.get(&pp.head_sha).map(|b| b.1.as_str())),
                        head_sha: Some(&pp.head_sha),
                        old_path: pp.old_path.as_deref(),
                        new_path: pp.new_path.as_deref(),
                        old_line: pp.old_line,
                        new_line: pp.new_line,
                        ..Position::default()
                    },
                    _ => Position::default(),
                };
                let note = mrdb::add_note_imported(
                    &mut *tx,
                    ImportedNote {
                        merge_request_id: id,
                        author_id: uid,
                        imported_author: name.as_deref(),
                        parent_id: parent,
                        body: &n.body,
                        system: false,
                        pos,
                        resolved: i == 0 && t.resolved,
                        created_at: n.created_at,
                    },
                )
                .await
                .map_err(other)?;
                if i == 0 {
                    parent = Some(note);
                }
            }
        }
        idb::record(&mut *tx, p.id, "merge_request", &mr.id, id).await.map_err(other)?;
        tx.commit().await.map_err(other)?;

        let mut refs = vec![(services::mr::head_ref(iid), head)];
        refs.extend(keep.into_iter().map(|sha| (services::mr::keep_around(&sha), sha)));
        Ok(refs)
    }

    // -------------------------------------------------------------- owner-only

    async fn variables(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        let vars = match self.src.variables(sp).await {
            Ok(v) => v,
            Err(SourceError::Refused(_) | SourceError::NotFound(_)) => {
                self.warnings.push("CI/CD variables were not readable with this token and were not imported.".into());
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut scoped = 0;
        for v in &vars {
            // one value per key here: environment-scoped duplicates keep the `*` one
            if v.environment_scope != "*" && vars.iter().any(|o| o.key == v.key && o.environment_scope == "*") {
                scoped += 1;
                continue;
            }
            if v.environment_scope != "*" {
                scoped += 1;
            }
            let sealed = self.state.secrets.encrypt(v.value.as_bytes()).map_err(other)?;
            forgust_db::ci_variables::upsert(
                &self.state.db,
                Some(p.id),
                None,
                forgust_db::ci_variables::VariableInput {
                    key: &v.key,
                    value_enc: &sealed,
                    variable_type: if v.variable_type == "file" { "file" } else { "env_var" },
                    protected: v.protected,
                    masked: v.masked,
                    raw: v.raw,
                    description: &v.description,
                },
            )
            .await
            .map_err(other)?;
        }
        if scoped > 0 {
            self.warnings.push(format!("{scoped} variable(s) had an environment scope; environment-scoped variables are not supported yet, so scopes were dropped (the `*` value wins)."));
        }
        Ok(())
    }

    async fn protected_branches(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        let list = match self.src.protected_branches(sp).await {
            Ok(l) => l,
            Err(SourceError::Refused(_) | SourceError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        // GitLab levels: 0 no one, 30 developers, 40 maintainers (others round up)
        let level = |l: i16| {
            if l == 0 {
                0
            } else if l <= DEVELOPER {
                DEVELOPER
            } else {
                MAINTAINER
            }
        };
        for b in list {
            forgust_db::protected_branches::upsert(
                &self.state.db,
                p.id,
                &b.name,
                level(b.push_level),
                level(b.merge_level),
                b.allow_force_push,
                false,
            )
            .await
            .map_err(other)?;
        }
        Ok(())
    }

    /// The source groups above a project paired with the target groups above
    /// its copy, from the deepest up (`team/sub/app` → `imported/apps/app`
    /// pairs `team/sub` with `imported/apps`, then `team` with `imported`).
    async fn mapped_groups(&self, sp: &SourceProject) -> Result<Vec<(String, Namespace)>> {
        let root = forgust_db::namespaces::find(&self.state.db, self.import.target_namespace_id)
            .await
            .map_err(other)?
            .ok_or_else(|| other("the target group is gone"))?;
        let src: Vec<&str> = sp.full_path.split('/').collect();
        let mut src_groups: Vec<String> = (1..src.len()).map(|i| src[..i].join("/")).collect();
        let dirs: Vec<&str> = self.row.target_path.split('/').filter(|s| !s.is_empty()).collect();
        let mut tgt_groups = vec![root.full_path.clone()];
        for i in 1..dirs.len() {
            tgt_groups.push(format!("{}/{}", root.full_path, dirs[..i].join("/")));
        }
        let mut out = Vec::new();
        let mut conn = self.state.db.acquire().await.map_err(other)?;
        while let (Some(s), Some(t)) = (src_groups.pop(), tgt_groups.pop()) {
            if let Some(hit) = forgust_db::routes::lookup(&mut conn, &t).await.map_err(other)?
                && hit.source_type == "namespace"
                && let Some(ns) = forgust_db::namespaces::find(&mut *conn, hit.source_id).await.map_err(other)?
            {
                out.push((s, ns));
            }
        }
        Ok(out)
    }

    /// A source group's place here, when it is one of the imported ones (or below one).
    async fn map_group(&self, mapped: &[(String, Namespace)], group: &str) -> Result<Option<Namespace>> {
        for (src, ns) in mapped {
            let rest = if group == src { Some("") } else { group.strip_prefix(&format!("{src}/")) };
            if let Some(rest) = rest {
                let path = if rest.is_empty() { ns.full_path.clone() } else { format!("{}/{rest}", ns.full_path) };
                let mut conn = self.state.db.acquire().await.map_err(other)?;
                if let Some(hit) = forgust_db::routes::lookup(&mut conn, &path).await.map_err(other)?
                    && hit.source_type == "namespace"
                {
                    return forgust_db::namespaces::find(&mut *conn, hit.source_id).await.map_err(other);
                }
                return Ok(None);
            }
        }
        Ok(None)
    }

    async fn members(&mut self, p: &Project, sp: &SourceProject) -> Result<()> {
        let me = self.principal.direct_user().map(|u| u.id);
        let mut unmatched = Vec::new();
        // group members on the groups their source groups became (once each)
        let mapped = self.mapped_groups(sp).await?;
        for (src, ns) in &mapped {
            let list = match self.src.group_members(src).await {
                Ok(l) => l,
                Err(SourceError::Refused(_) | SourceError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            for m in list {
                let (local, _) = self.who(Some(&m.user)).await?;
                match local {
                    Some(uid) if Some(uid) != me => {
                        let level = m.level.clamp(forgust_db::access::GUEST, OWNER);
                        if forgust_db::members::level_of(&self.state.db, "namespace", ns.id, uid)
                            .await
                            .map_err(other)?
                            .is_some_and(|l| l >= level)
                        {
                            continue;
                        }
                        if let Err(e) =
                            services::members::add(self.state, &self.principal, services::members::Source::Group(ns), uid, level, None)
                                .await
                        {
                            self.warnings.push(format!("@{} was not added to {}: {e:?}", m.user.username, ns.full_path));
                        }
                    }
                    Some(_) => {}
                    None => unmatched.push(format!("@{}", m.user.username)),
                }
            }
        }
        // then whoever the groups do not already give that access
        let list = match self.src.members(sp).await {
            Ok(l) => l,
            Err(SourceError::Refused(_) | SourceError::NotFound(_)) => Vec::new(),
            Err(e) => return Err(e),
        };
        for m in list {
            let (local, _) = self.who(Some(&m.user)).await?;
            match local {
                Some(uid) if Some(uid) != me => {
                    let level = m.level.clamp(forgust_db::access::GUEST, OWNER);
                    if forgust_db::authz::project_level(&self.state.db, uid, p.id).await.map_err(other)?.is_some_and(|l| l >= level) {
                        continue;
                    }
                    if let Err(e) =
                        services::members::add(self.state, &self.principal, services::members::Source::Project(p), uid, level, None).await
                    {
                        self.warnings.push(format!("@{} was not added as a member: {e:?}", m.user.username));
                    }
                }
                Some(_) => {}
                None if !unmatched.contains(&format!("@{}", m.user.username)) => unmatched.push(format!("@{}", m.user.username)),
                None => {}
            }
        }
        // sharing with groups that are part of the import
        let mut outside = Vec::new();
        let mut shares: Vec<(services::members::Source<'_>, String, i16)> = Vec::new();
        for (g, level) in self.src.project_shares(sp).await.unwrap_or_default() {
            shares.push((services::members::Source::Project(p), g, level));
        }
        for (src, ns) in &mapped {
            for (g, level) in self.src.group_shares(src).await.unwrap_or_default() {
                shares.push((services::members::Source::Group(ns), g, level));
            }
        }
        for (source, g, level) in shares {
            match self.map_group(&mapped, &g).await? {
                Some(with) => {
                    let level = level.clamp(forgust_db::access::GUEST, OWNER);
                    if let Err(e) = services::members::share(self.state, &self.principal, source, &with, level, None).await {
                        self.warnings.push(format!("Not shared with {}: {e:?}", with.full_path));
                    }
                }
                None => outside.push(g),
            }
        }
        if !outside.is_empty() {
            outside.sort();
            outside.dedup();
            self.warnings.push(format!(
                "Shared with group(s) outside this import, not shared here: {}.",
                outside.into_iter().take(20).collect::<Vec<_>>().join(", ")
            ));
        }
        if !unmatched.is_empty() {
            self.warnings.push(format!(
                "{} member(s) have no account here with the same email and were not added: {}.",
                unmatched.len(),
                unmatched.into_iter().take(20).collect::<Vec<_>>().join(", ")
            ));
        }
        Ok(())
    }
}

/// Items fetched and computed at once before they are written in order.
const WINDOW: usize = 32;

/// What writing an MR needs, gathered without touching the database.
struct PreparedMr {
    head: String,
    base: String,
    start: String,
    commits: i32,
    files: i32,
    additions: i32,
    deletions: i32,
    /// Always Some when prepared (taken by the writer); an error when the source
    /// refused to show the comments.
    threads: Option<Result<Vec<SourceThread>>>,
    /// Base and start for positions the source gave without them (GitHub).
    bases: HashMap<String, (String, String)>,
    /// Commits comments point to, kept reachable.
    keep: Vec<String>,
}

/// An MR's threads from the source and its version from the repository; None
/// when its commits are not on the source anymore.
async fn prepare_mr(
    state: &AppState,
    src: &dyn Source,
    p: &Project,
    sp: &SourceProject,
    mr: &SourceMergeRequest,
    skip_threads: bool,
) -> Result<Option<PreparedMr>> {
    let st = &state.storage;
    let exists = |sha: String| async move { !sha.is_empty() && st.commit(p.id, &sha).await.ok().flatten().is_some() };
    // the head: as the source says, else what its MR ref points to
    let head = if exists(mr.head_sha.clone()).await {
        mr.head_sha.clone()
    } else {
        match st.resolve(p.id, &format!("refs/import/mr/{}", mr.iid)).await.map_err(other)? {
            Some(h) => h,
            None => return Ok(None),
        }
    };
    let threads = async { if skip_threads { Ok(Vec::new()) } else { src.mr_threads(sp, mr).await } };
    let version = async {
        let (base, start) = diff_base(state, src, p, sp, mr, &head).await?;
        let commits = st.rev_list(p.id, &base, &head, false).await.map_err(other)?.len() as i32;
        let stats = st
            .diff(p.id, Some(&base), &head, DiffOptions { offset: 0, limit: 1, max_lines_per_file: 1, paths: None })
            .await
            .map_err(other)?;
        Ok::<_, SourceError>((base, start, commits, stats))
    };
    let (threads, version) = tokio::join!(threads, version);
    let (base, start, commits, stats) = version?;
    let threads = match threads {
        Err(SourceError::Refused(e)) => Err(SourceError::Refused(e)),
        r => Ok(r?),
    };
    // positions without a base (GitHub): the version's when written on its head,
    // else the merge base of the commit with the target
    let mut bases: HashMap<String, (String, String)> = HashMap::new();
    let mut keep = vec![head.clone(), start.clone(), base.clone()];
    for t in threads.as_ref().map(|t| t.as_slice()).unwrap_or_default() {
        let Some(pp) = t.notes.first().and_then(|n| n.position.as_ref()) else { continue };
        for sha in [Some(&pp.head_sha), pp.base_sha.as_ref(), pp.start_sha.as_ref()].into_iter().flatten() {
            if !keep.contains(sha) && exists(sha.clone()).await {
                keep.push(sha.clone());
            }
        }
        if pp.base_sha.is_some() || bases.contains_key(&pp.head_sha) {
            continue;
        }
        let pair = if pp.head_sha == head {
            (base.clone(), start.clone())
        } else if keep.contains(&pp.head_sha) {
            let b = st.merge_base(p.id, &start, &pp.head_sha).await.map_err(other)?.unwrap_or_else(|| base.clone());
            (b, start.clone())
        } else {
            continue;
        };
        bases.insert(pp.head_sha.clone(), pair);
    }
    Ok(Some(PreparedMr {
        head,
        base,
        start,
        commits,
        files: stats.total_files as i32,
        additions: stats.additions as i32,
        deletions: stats.deletions as i32,
        threads: Some(threads),
        bases,
        keep,
    }))
}

/// Base and start of an MR's diff: as the source gave them, else the target as it
/// is now (right for open and closed MRs, only `start` differs), else for a
/// merged MR the target before its merge or squash commit, else what the source
/// says when asked (fast-forward merges).
async fn diff_base(
    state: &AppState,
    src: &dyn Source,
    p: &Project,
    sp: &SourceProject,
    mr: &SourceMergeRequest,
    head: &str,
) -> Result<(String, String)> {
    let st = &state.storage;
    let usable = |d: DiffRefs| async move {
        let ok = d.head_sha == head
            && st.commit(p.id, &d.base_sha).await.ok().flatten().is_some()
            && st.commit(p.id, &d.start_sha).await.ok().flatten().is_some();
        ok.then_some((d.base_sha, d.start_sha))
    };
    if let Some(d) = mr.diff_refs.clone()
        && let Some(pair) = usable(d).await
    {
        return Ok(pair);
    }
    let start = st.resolve(p.id, &format!("refs/heads/{}", mr.target_branch)).await.map_err(other)?.unwrap_or_else(|| head.to_string());
    let base = st.merge_base(p.id, &start, head).await.map_err(other)?.unwrap_or_else(|| start.clone());
    if base != head {
        return Ok((base, start));
    }
    // the target already has the head (merged)
    if let Some(merge) = &mr.merge_commit_sha
        && let Some(parent) = st.commit(p.id, merge).await.ok().flatten().and_then(|c| c.parents.into_iter().next())
    {
        let b = st.merge_base(p.id, &parent, head).await.map_err(other)?.unwrap_or_else(|| parent.clone());
        if b != head {
            return Ok((b, parent));
        }
    }
    if let Some(d) = src.mr_diff_refs(sp, mr).await?
        && let Some(pair) = usable(d).await
    {
        return Ok(pair);
    }
    Ok((base, start))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn api(e: crate::error::ApiError) -> SourceError {
    SourceError::Refused(e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn ci_warnings_find_unsupported_features() {
        let yaml = "include:\n  - template: Auto-DevOps.gitlab-ci.yml\n  - component: gitlab.com/org/c/build@1\ndeploy:\n  script: [echo]\n  environment: production\n  secrets:\n    X: {vault: a/b}\n  image: $CI_REGISTRY_IMAGE:latest\npages:\n  script: [echo]\n";
        let w = super::ci_warnings(yaml);
        assert!(!w.iter().any(|m| m.contains("`environment`")), "environments are supported: {w:?}");
        assert!(w.iter().any(|m| m.contains("secrets")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("Pages")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("registry")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("templates")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("components")), "{w:?}");
        assert!(super::ci_warnings("build:\n  script: [make]\n").is_empty());
    }

    #[test]
    fn project_paths() {
        let p = |s: &str, b: &str, i: &str| super::project_path(s, b, i);
        assert_eq!(p("gitlab", "https://gitlab.com", "gitlab-org/cli").as_deref(), Some("gitlab-org/cli"));
        assert_eq!(
            p("gitlab", "https://gitlab.com", "https://gitlab.com/gitlab-org/sub/cli/-/tree/main").as_deref(),
            Some("gitlab-org/sub/cli")
        );
        assert_eq!(p("github", "https://github.com", "https://github.com/rust-lang/rust/pull/1").as_deref(), Some("rust-lang/rust"));
        assert_eq!(p("github", "https://github.com", "https://www.github.com/a/b.git").as_deref(), Some("a/b"));
        assert_eq!(p("gitlab", "https://gitlab.com", "https://evil.example/a/b"), None, "another host");
        assert_eq!(p("gitlab", "https://gitlab.com", "justone"), None);
        assert_eq!(p("gitlab", "https://gitlab.com", "a/../b"), None);
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(super::truncate("héllo", 2), "h");
        assert_eq!(super::truncate("abc", 5), "abc");
    }
}

// ------------------------------------------------------------------ LFS batch API

#[derive(Deserialize)]
struct LfsBatch {
    #[serde(default)]
    objects: Vec<LfsObject>,
}

#[derive(Deserialize)]
struct LfsObject {
    oid: String,
    actions: Option<LfsActions>,
    error: Option<LfsError>,
}

#[derive(Deserialize)]
struct LfsActions {
    download: Option<LfsAction>,
}

#[derive(Deserialize)]
struct LfsAction {
    href: String,
    #[serde(default)]
    header: HashMap<String, String>,
}

#[derive(Deserialize)]
struct LfsError {
    message: String,
}

/// A client pinned to the checked addresses of one host, without redirects.
fn lfs_client(url: &url::Url, addrs: &[std::net::SocketAddr]) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .resolve_to_addrs(url.host_str().unwrap_or_default(), addrs)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(3600))
        .user_agent("Forgust-importer")
        .build()
        .map_err(other)
}
