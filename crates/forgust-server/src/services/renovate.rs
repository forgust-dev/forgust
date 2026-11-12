//! Built-in Renovate (ADR-0023). A project turns on dependency
//! updates; Forgust runs Renovate as a CI job it writes itself (pipeline
//! source `renovate`), as a bot that is a Developer of that one project, with
//! a token that lives while the job runs.

use chrono::{DateTime, Duration, Utc};
use forgust_db::projects::Project;
use forgust_db::users::{NewUser, User};
use serde_json::json;

use crate::auth::Principal;
use crate::ci::variables::Var;
use crate::config::RenovateConfig;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

const DEVELOPER: i16 = 30;
pub const SOURCE: &str = "renovate";

/// What an administrator may change over the config file's `[renovate]`
/// (Admin > Dependency updates); absent fields keep the file's value.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Overrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onboarding_config: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
}

/// The settings in effect: the config file, then the administrator's changes.
pub async fn config(state: &AppState) -> anyhow::Result<RenovateConfig> {
    let mut c = state.cfg.renovate.clone();
    let (raw, token) = forgust_db::renovate::instance(&state.db).await?;
    let o: Overrides = serde_json::from_value(raw).unwrap_or_default();
    if let Some(v) = o.enabled {
        c.enabled = v;
    }
    if let Some(v) = o.image {
        c.image = v;
    }
    if let Some(v) = o.tags {
        c.tags = v;
    }
    if let Some(v) = o.max_concurrent {
        c.max_concurrent = v;
    }
    if let Some(v) = o.timeout_secs {
        c.timeout_secs = v;
    }
    if let Some(v) = o.onboarding_config {
        c.onboarding_config = v;
    }
    if let Some(v) = o.extra_env {
        c.extra_env = v;
    }
    if let Some(sealed) = token {
        c.github_com_token = Some(state.secrets.decrypt_str(&sealed)?);
    }
    Ok(c)
}

pub fn bot_username(project_id: i64) -> String {
    format!("renovate-bot-{project_id}")
}

/// When the run after one starting at `from` is due: every `interval_hours`,
/// shifted by up to a tenth of it so projects spread out (stable per project).
pub fn next_after(from: DateTime<Utc>, project_id: i64, interval_hours: i32) -> DateTime<Utc> {
    let interval = interval_hours.max(1) as i64 * 3600;
    let jitter = (project_id.unsigned_abs().wrapping_mul(2_654_435_761) % (interval as u64 / 10).max(1)) as i64;
    from + Duration::seconds(interval + jitter)
}

/// The project's bot, created on first use, and a Developer of the project
/// (unless a Maintainer gave it another role).
async fn ensure_bot(state: &AppState, project: &Project, bot_user_id: Option<i64>) -> ApiResult<User> {
    let kind = ProjectBot { username: bot_username(project.id), name: "Renovate", user_type: "renovate_bot", level: DEVELOPER };
    ensure_project_bot(state, project, &kind, bot_user_id).await
}

/// A bot Forgust runs one of its own features as, in one project.
pub(crate) struct ProjectBot {
    pub username: String,
    pub name: &'static str,
    pub user_type: &'static str,
    /// Its role when it joins (a Maintainer may change it later).
    pub level: i16,
}

/// `kind`'s bot of `project`, created on first use, and a member of it.
pub(crate) async fn ensure_project_bot(state: &AppState, project: &Project, kind: &ProjectBot, known: Option<i64>) -> ApiResult<User> {
    let existing = match known {
        Some(id) => forgust_db::users::find(&state.db, id).await?,
        None => None,
    };
    let bot = match existing {
        Some(b) => b,
        None => match forgust_db::users::find_by_username(&state.db, &kind.username).await? {
            Some(b) if b.user_type == kind.user_type => b,
            Some(_) => return Err(ApiError::conflict(format!("the username `{}` is taken", kind.username))),
            None => {
                let mut conn = state.db.acquire().await?;
                forgust_db::users::insert(
                    &mut conn,
                    NewUser {
                        username: &kind.username,
                        email: &format!("{}@noreply.forgust.local", kind.username),
                        name: kind.name,
                        password_hash: None,
                        user_type: kind.user_type,
                        bot_source: Some(("project", project.id)),
                        is_admin: false,
                        email_verified: true,
                    },
                )
                .await?
            }
        },
    };
    if bot.avatar_at.is_none()
        && let Err(e) = super::avatars::set(state, super::avatars::Owner::User(bot.id), &bot_avatar()).await
    {
        tracing::warn!(error = ?e, bot = %kind.username, "bot avatar not set");
    }
    if forgust_db::members::level_of(&state.db, "project", project.id, bot.id).await?.is_none() {
        let mut tx = state.db.begin().await?;
        forgust_db::members::upsert(&mut *tx, "project", project.id, bot.id, kind.level, None, None).await?;
        forgust_db::members::refresh_authorizations(&mut tx, &[bot.id]).await?;
        tx.commit().await?;
    }
    Ok(bot)
}

/// The bots' picture: a ring around a dot on a teal disc (a generic "update"
/// mark, drawn here so no file ships with the binary).
pub fn bot_avatar() -> Vec<u8> {
    const N: u32 = 256;
    let img = image::RgbaImage::from_fn(N, N, |x, y| {
        let (dx, dy) = (x as f32 + 0.5 - N as f32 / 2.0, y as f32 + 0.5 - N as f32 / 2.0);
        let r = (dx * dx + dy * dy).sqrt();
        match r {
            r if r <= 34.0 => image::Rgba([255, 255, 255, 255]),
            r if (62.0..=84.0).contains(&r) => image::Rgba([255, 255, 255, 255]),
            r if r <= 128.0 => image::Rgba([13, 128, 120, 255]),
            _ => image::Rgba([0, 0, 0, 0]),
        }
    });
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).expect("PNG in memory");
    out
}

/// What a Maintainer may change.
pub struct Change {
    pub enabled: bool,
    pub interval_hours: i32,
    pub onboarding: bool,
}

/// Turn dependency updates on or off. On: the bot joins and a first run is
/// due now. Off: the bot leaves (its merge requests stay) and runs stop.
pub async fn configure(state: &AppState, principal: &Principal, project: &Project, c: Change) -> ApiResult<()> {
    if c.enabled && !config(state).await.map_err(ApiError::Internal)?.enabled {
        return Err(ApiError::conflict("dependency updates are turned off on this instance"));
    }
    if !(1..=168).contains(&c.interval_hours) {
        return Err(ApiError::bad("runs are 1 to 168 hours apart"));
    }
    let before = forgust_db::renovate::get(&state.db, project.id).await?;
    let mut bot_id = before.as_ref().and_then(|s| s.bot_user_id);
    let was_on = before.as_ref().is_some_and(|s| s.enabled);
    let next = if c.enabled {
        bot_id = Some(ensure_bot(state, project, bot_id).await?.id);
        match &before {
            // the first run right away; later changes keep the schedule
            Some(s) if was_on => s.next_run_at.or(Some(Utc::now())),
            _ => Some(Utc::now()),
        }
    } else {
        // a run going on stops: its bot leaves the project
        for pl in forgust_db::renovate::active_pipelines(&state.db, project.id).await? {
            crate::ci::pipeline::cancel(state, pl).await.map_err(ApiError::Internal)?;
        }
        if let Some(id) = bot_id {
            let mut tx = state.db.begin().await?;
            forgust_db::members::remove(&mut *tx, "project", project.id, id).await?;
            forgust_db::members::refresh_authorizations(&mut tx, &[id]).await?;
            forgust_db::renovate::revoke_tokens(&mut *tx, id).await?;
            tx.commit().await?;
        }
        None
    };
    forgust_db::renovate::save(&state.db, project.id, c.enabled, c.interval_hours, c.onboarding, bot_id, next).await?;
    if c.enabled != was_on {
        let action = if c.enabled { "renovate_enabled" } else { "renovate_disabled" };
        super::audit(state, principal, action, Some(("project", project.id)), json!({}), None).await;
    }
    Ok(())
}

/// A group's default for its projects (those that never chose, and new ones).
pub async fn set_group_default(
    state: &AppState,
    principal: &Principal,
    ns: &forgust_db::namespaces::Namespace,
    enabled: bool,
) -> ApiResult<usize> {
    super::builtin::set_group_default::<Renovate>(state, principal, ns, enabled).await
}

/// A new project under a group that turned dependency updates on gets them.
pub async fn on_project_created(state: &AppState, principal: &Principal, project: &Project) {
    super::builtin::on_project_created::<Renovate>(state, principal, project).await
}

/// Built-in Renovate for the shared scheduler.
struct Renovate;

impl super::builtin::Builtin for Renovate {
    const FEATURE: &'static str = "dependency updates";
    const RUN: &'static str = "renovate run";
    const GROUP_AUDIT: &'static str = "renovate_group_default";

    async fn limits(state: &AppState) -> anyhow::Result<Option<(i64, f64)>> {
        let cfg = config(state).await?;
        Ok(cfg.enabled.then_some((cfg.max_concurrent as i64, cfg.timeout_secs as f64)))
    }
    async fn stale(state: &AppState, secs: f64) -> anyhow::Result<Vec<i64>> {
        Ok(forgust_db::renovate::stale_runs(&state.db, secs).await?)
    }
    async fn active(state: &AppState) -> anyhow::Result<i64> {
        Ok(forgust_db::renovate::active_runs(&state.db, None).await?)
    }
    async fn due(state: &AppState, n: i64) -> anyhow::Result<Vec<i64>> {
        Ok(forgust_db::renovate::due(&state.db, n).await?)
    }
    async fn run(state: &AppState, project: &Project) -> ApiResult<Option<i64>> {
        run(state, project).await
    }
    async fn later(state: &AppState, project_id: i64, at: DateTime<Utc>) -> anyhow::Result<()> {
        Ok(forgust_db::renovate::started(&state.db, project_id, None, at).await?)
    }
    async fn set_group(state: &AppState, namespace_id: i64, enabled: bool) -> anyhow::Result<()> {
        Ok(forgust_db::renovate::set_group_default(&state.db, namespace_id, enabled).await?)
    }
    async fn undecided_under(state: &AppState, namespace_id: i64) -> anyhow::Result<Vec<i64>> {
        Ok(forgust_db::renovate::undecided_projects_under(&state.db, namespace_id).await?)
    }
    async fn inherited(state: &AppState, traversal_ids: &[i64]) -> anyhow::Result<bool> {
        Ok(forgust_db::renovate::inherited_default(&state.db, traversal_ids).await?)
    }
    async fn turn_on(state: &AppState, principal: &Principal, project: &Project) -> ApiResult<()> {
        configure(state, principal, project, Change { enabled: true, interval_hours: 24, onboarding: true }).await
    }
}

/// The job Forgust runs (ADR-0023): the configured image, no clone (Renovate
/// clones by itself), always.
fn job_config(cfg: &RenovateConfig) -> String {
    let job = json!({
        "image": cfg.image,
        "tags": cfg.tags,
        "timeout": format!("{} minutes", cfg.timeout_secs.div_ceil(60).max(1)),
        // Renovate's cache (package metadata, cloned repositories) kept between runs
        "variables": { "GIT_STRATEGY": "none", "RENOVATE_CACHE_DIR": "$CI_PROJECT_DIR/.renovate-cache" },
        "cache": { "key": "renovate", "paths": [".renovate-cache/"], "when": "always" },
        "rules": [{ "when": "always" }],
        "script": ["renovate"],
    });
    // JSON is YAML
    json!({ "renovate": job }).to_string()
}

/// Start a run now. `Ok(None)`: nothing to run (an empty repository).
pub async fn run(state: &AppState, project: &Project) -> ApiResult<Option<i64>> {
    crate::auth::authz::writable(project)?;
    let cfg = config(state).await.map_err(ApiError::Internal)?;
    if !cfg.enabled {
        return Err(ApiError::conflict("dependency updates are turned off on this instance"));
    }
    let Some(s) = forgust_db::renovate::get(&state.db, project.id).await?.filter(|s| s.enabled) else {
        return Err(ApiError::conflict("dependency updates are off for this project"));
    };
    if forgust_db::renovate::active_runs(&state.db, Some(project.id)).await? > 0 {
        return Err(ApiError::conflict("a run is already going"));
    }
    let next = next_after(Utc::now(), project.id, s.interval_hours);
    let branch = project.default_branch.clone().unwrap_or_else(|| "main".into());
    let Some(sha) = state.storage.resolve(project.id, &format!("refs/heads/{branch}")).await? else {
        forgust_db::renovate::started(&state.db, project.id, None, next).await?;
        return Ok(None);
    };
    let bot = ensure_bot(state, project, s.bot_user_id).await?;
    let created = crate::ci::pipeline::create(
        state,
        crate::ci::pipeline::Trigger {
            config: Some(job_config(&cfg)),
            ..crate::ci::pipeline::Trigger::new(project.id, &branch, &sha, SOURCE, Some(bot.id))
        },
    )
    .await
    .map_err(ApiError::Internal)?;
    let id = created.id();
    forgust_db::renovate::started(&state.db, project.id, id, next).await?;
    Ok(id)
}

/// Every minute: start due runs (shared with the scanners: [`super::builtin`]).
pub async fn run_due(state: &AppState) -> anyhow::Result<()> {
    super::builtin::run_due::<Renovate>(state).await
}

/// A push to the default branch (not by the bot): run soon, so Renovate
/// rebases its branches on it.
pub async fn on_default_branch_push(state: &AppState, project_id: i64, pusher: Option<&Principal>) {
    if pusher.and_then(|p| p.user()).is_some_and(|u| u.user_type == "renovate_bot") {
        return;
    }
    let _ = forgust_db::renovate::bring_forward(&state.db, project_id, Utc::now() + Duration::minutes(10), 3600.0).await;
}

/// An edit of the bot's Dependency Dashboard issue or of one of its merge
/// requests that ticks a box (by a person): run within a minute.
pub async fn on_description_edit(state: &AppState, project_id: i64, author_id: Option<i64>, editor: &User, old: &str, new: &str) {
    if editor.user_type == "renovate_bot" {
        return;
    }
    let ticked = |s: &str| s.matches("[x]").count() + s.matches("[X]").count();
    if ticked(new) <= ticked(old) {
        return;
    }
    let Ok(Some(s)) = forgust_db::renovate::get(&state.db, project_id).await else { return };
    if s.enabled && author_id.is_some() && author_id == s.bot_user_id {
        let _ = forgust_db::renovate::bring_forward(&state.db, project_id, Utc::now(), 300.0).await;
    }
}

/// A Renovate pipeline ended: its run tokens go.
pub async fn on_pipeline_finished(state: &AppState, pipeline: &forgust_db::ci::Pipeline, status: &str) {
    if pipeline.source != SOURCE || !crate::ci::pipeline::is_complete(status) {
        return;
    }
    if let Some(bot) = pipeline.user_id
        && let Err(e) = forgust_db::renovate::revoke_tokens(&state.db, bot).await
    {
        tracing::warn!(pipeline = pipeline.id, error = %e, "renovate run tokens not revoked");
    }
}

/// The job's variables, given when a runner takes it: a token for the bot
/// that expires with the job, and Renovate's settings. Nothing from the
/// project's, groups' or instance's CI variables.
pub async fn job_variables(
    state: &AppState,
    project: &Project,
    settings: Option<&forgust_db::renovate::Settings>,
    bot: &User,
    timeout_secs: i64,
) -> anyhow::Result<Vec<Var>> {
    let cfg = config(state).await?;
    let onboarding = settings.is_none_or(|s| s.onboarding);
    let token = crypto::random_token(crypto::prefix::PERSONAL);
    let expires = Utc::now() + Duration::seconds(timeout_secs + 300);
    let scopes = ["api".to_string(), "write_repository".to_string()];
    forgust_db::tokens::create_pat(&state.db, bot.id, "Renovate run", &crypto::token_hash(&token), &scopes, Some(expires)).await?;
    let secret = |k: &str, v: String| Var { key: k.into(), value: v, public: false, masked: true, file: false, raw: true };
    let public = |k: &str, v: String| Var { key: k.into(), value: v, public: true, masked: false, file: false, raw: true };
    let mut v = vec![
        public("RENOVATE_PLATFORM", "gitlab".into()),
        public("RENOVATE_ENDPOINT", format!("{}/api/v4/", state.cfg.server.external_url.trim_end_matches('/'))),
        secret("RENOVATE_TOKEN", token.clone()),
        public("RENOVATE_REPOSITORIES", project.full_path.clone()),
        public("RENOVATE_GIT_AUTHOR", format!("Renovate Bot <{}>", bot.email)),
        public("RENOVATE_ONBOARDING", onboarding.to_string()),
        public("RENOVATE_REQUIRE_CONFIG", if onboarding { "required" } else { "optional" }.into()),
        // Merge requests for vulnerable dependencies first, from OSV
        // (labelled `security`, outside the schedule)
        public("RENOVATE_OSV_VULNERABILITY_ALERTS", "true".into()),
        public("RENOVATE_DEPENDENCY_DASHBOARD", "true".into()),
    ];
    if let Some(c) = &cfg.onboarding_config {
        v.push(public("RENOVATE_ONBOARDING_CONFIG", c.clone()));
    }
    if let Some(t) = &cfg.github_com_token {
        v.push(secret("RENOVATE_GITHUB_COM_TOKEN", t.clone()));
    }
    // credentials for registries: this instance's (the same token), the ones
    // set for every project, then the project's own (later rules win for a host)
    let mut rules = Vec::new();
    if state.cfg.registry.enabled {
        rules.push(json!({ "matchHost": crate::registry::host(state), "hostType": "docker", "username": bot.username, "password": token }));
    }
    let mut host_rules = forgust_db::renovate::host_rules(&state.db, None).await?;
    host_rules.extend(forgust_db::renovate::host_rules(&state.db, Some(project.id)).await?);
    for r in host_rules {
        let mut rule = json!({ "matchHost": r.match_host, "password": state.secrets.decrypt_str(&r.password_enc)? });
        if !r.username.is_empty() {
            rule["username"] = json!(r.username);
        }
        if let Some(t) = r.host_type {
            rule["hostType"] = json!(t);
        }
        rules.push(rule);
    }
    if !rules.is_empty() {
        v.push(secret("RENOVATE_HOST_RULES", serde_json::Value::Array(rules).to_string()));
    }
    // the runner reads DOCKER_AUTH_CONFIG to pull the job's image
    if let Some((user, sealed)) = forgust_db::renovate::image_pull(&state.db).await? {
        let password = state.secrets.decrypt_str(&sealed)?;
        v.push(secret("DOCKER_AUTH_CONFIG", docker_auth_config(&cfg.image, &user, &password)));
    }
    for (k, val) in &cfg.extra_env {
        v.push(public(k, val.clone()));
    }
    Ok(v)
}

/// The registry an image is pulled from, as Docker names it in `auths`.
pub fn image_registry(image: &str) -> String {
    match image.split_once('/') {
        Some((first, _)) if first.contains(['.', ':']) || first == "localhost" => first.to_string(),
        _ => "https://index.docker.io/v1/".to_string(),
    }
}

/// `DOCKER_AUTH_CONFIG` for pulling `image` with these credentials.
fn docker_auth_config(image: &str, user: &str, password: &str) -> String {
    use base64::Engine as _;
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    json!({ "auths": { image_registry(image): { "auth": auth } } }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_registries() {
        assert_eq!(image_registry("renovate/renovate:44.226.3"), "https://index.docker.io/v1/");
        assert_eq!(image_registry("renovate:44"), "https://index.docker.io/v1/");
        assert_eq!(image_registry("ghcr.io/renovatebot/renovate:44"), "ghcr.io");
        assert_eq!(image_registry("mirror.example.com:5000/renovate/renovate"), "mirror.example.com:5000");
        assert_eq!(image_registry("localhost/renovate"), "localhost");
    }
}
