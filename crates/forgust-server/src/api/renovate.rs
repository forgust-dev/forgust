//! Dependency updates (built-in Renovate, ADR-0023): a project's settings,
//! its last run, and "Run now".

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::{GroupAction, ProjectAction};
use crate::auth::{Admin, SignedIn};
use crate::error::{ApiError, ApiResult};
use crate::services::renovate as svc;
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct RenovateRun {
    pub pipeline_iid: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct RenovateStatus {
    /// The instance offers dependency updates.
    pub available: bool,
    pub enabled: bool,
    /// Hours between runs (1 to 168).
    pub interval_hours: i32,
    /// Renovate opens an onboarding merge request first; false: it runs with
    /// its defaults, no config file needed.
    pub onboarding: bool,
    /// The project's Renovate bot, once turned on.
    pub bot_username: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run: Option<RenovateRun>,
    /// The image runs use.
    pub image: String,
}

#[derive(Deserialize, ToSchema)]
pub struct RenovateSettingsBody {
    pub enabled: bool,
    #[serde(default = "default_interval")]
    pub interval_hours: i32,
    #[serde(default = "default_true")]
    pub onboarding: bool,
}

fn default_interval() -> i32 {
    24
}
fn default_true() -> bool {
    true
}

#[derive(Serialize, ToSchema)]
pub struct RenovateRunStarted {
    /// None: nothing to run yet (an empty repository).
    pub pipeline_iid: Option<i64>,
}

async fn status_of(state: &AppState, project_id: i64) -> ApiResult<RenovateStatus> {
    let cfg = svc::config(state).await.map_err(ApiError::Internal)?;
    let s = forgust_db::renovate::get(&state.db, project_id).await?;
    let bot_username = match s.as_ref().and_then(|s| s.bot_user_id) {
        Some(id) => forgust_db::users::find(&state.db, id).await?.map(|u| u.username),
        None => None,
    };
    let last_run = match s.as_ref().and_then(|s| s.last_pipeline_id) {
        Some(id) => forgust_db::ci::pipeline(&state.db, id).await?.map(|p| RenovateRun {
            pipeline_iid: p.iid,
            status: p.status,
            created_at: p.created_at,
            finished_at: p.finished_at,
        }),
        None => None,
    };
    Ok(RenovateStatus {
        available: cfg.enabled,
        enabled: s.as_ref().is_some_and(|s| s.enabled),
        interval_hours: s.as_ref().map_or(24, |s| s.interval_hours),
        onboarding: s.as_ref().is_none_or(|s| s.onboarding),
        bot_username,
        next_run_at: s.as_ref().filter(|s| s.enabled).and_then(|s| s.next_run_at),
        last_run,
        image: cfg.image,
    })
}

/// Dependency updates of the project (Maintainers).
#[utoipa::path(get, path = "/api/v1/projects/{id}/renovate", responses((status = 200, body = RenovateStatus)), tag = "renovate")]
pub async fn get(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<RenovateStatus>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    Ok(Json(status_of(&state, p.id).await?))
}

/// Turn dependency updates on or off, and how often they run.
#[utoipa::path(put, path = "/api/v1/projects/{id}/renovate", request_body = RenovateSettingsBody, responses((status = 200, body = RenovateStatus)), tag = "renovate")]
pub async fn set(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(body): Json<RenovateSettingsBody>,
) -> ApiResult<Json<RenovateStatus>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    svc::configure(
        &state,
        &principal,
        &p,
        svc::Change { enabled: body.enabled, interval_hours: body.interval_hours, onboarding: body.onboarding },
    )
    .await?;
    Ok(Json(status_of(&state, p.id).await?))
}

/// Run Renovate now (one run at a time per project).
#[utoipa::path(post, path = "/api/v1/projects/{id}/renovate/run", responses((status = 200, body = RenovateRunStarted)), tag = "renovate")]
pub async fn run_now(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<RenovateRunStarted>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    let id = svc::run(&state, &p).await?;
    let pipeline_iid = match id {
        Some(id) => forgust_db::ci::pipeline(&state.db, id).await?.map(|pl| pl.iid),
        None => None,
    };
    crate::services::audit(
        &state,
        &principal,
        "renovate_run",
        Some(("project", p.id)),
        serde_json::json!({ "pipeline_iid": pipeline_iid }),
        None,
    )
    .await;
    Ok(Json(RenovateRunStarted { pipeline_iid }))
}

// ------------------------------------------------------------ host rules

#[derive(Deserialize, ToSchema)]
pub struct NewHostRule {
    /// A host (`registry.example.com`) or URL prefix Renovate matches.
    pub match_host: String,
    /// `docker`, `npm`, `maven`, `pypi`… (empty: any).
    #[serde(default)]
    pub host_type: Option<String>,
    #[serde(default)]
    pub username: String,
    /// The password or token; stored encrypted, never shown again.
    pub password: String,
}

/// Credentials Renovate uses for the project's private registries (passwords not shown).
#[utoipa::path(get, path = "/api/v1/projects/{id}/renovate/host_rules", responses((status = 200, body = Vec<forgust_db::renovate::HostRule>)), tag = "renovate")]
pub async fn host_rules(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<forgust_db::renovate::HostRule>>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    Ok(Json(forgust_db::renovate::host_rules(&state.db, Some(p.id)).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/renovate/host_rules", request_body = NewHostRule, responses((status = 200, body = Vec<forgust_db::renovate::HostRule>)), tag = "renovate")]
pub async fn add_host_rule(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(r): Json<NewHostRule>,
) -> ApiResult<Json<Vec<forgust_db::renovate::HostRule>>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    Ok(Json(add_rule(&state, &principal, Some(p.id), r).await?))
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/renovate/host_rules/{rule_id}", responses((status = 204)), tag = "renovate")]
pub async fn delete_host_rule(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path((id, rule_id)): Path<(String, i64)>,
) -> ApiResult<axum::http::StatusCode> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    delete_rule(&state, &principal, Some(p.id), rule_id).await
}

/// Credentials every project's runs use (projects' own rules come after, so they win for the same host).
#[utoipa::path(get, path = "/api/v1/admin/renovate/host_rules", responses((status = 200, body = Vec<forgust_db::renovate::HostRule>)), tag = "renovate")]
pub async fn admin_host_rules(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<Vec<forgust_db::renovate::HostRule>>> {
    Ok(Json(forgust_db::renovate::host_rules(&state.db, None).await?))
}

#[utoipa::path(post, path = "/api/v1/admin/renovate/host_rules", request_body = NewHostRule, responses((status = 200, body = Vec<forgust_db::renovate::HostRule>)), tag = "renovate")]
pub async fn admin_add_host_rule(
    State(state): State<AppState>,
    Admin(_, principal): Admin,
    Json(r): Json<NewHostRule>,
) -> ApiResult<Json<Vec<forgust_db::renovate::HostRule>>> {
    Ok(Json(add_rule(&state, &principal, None, r).await?))
}

#[utoipa::path(delete, path = "/api/v1/admin/renovate/host_rules/{rule_id}", responses((status = 204)), tag = "renovate")]
pub async fn admin_delete_host_rule(
    State(state): State<AppState>,
    Admin(_, principal): Admin,
    Path(rule_id): Path<i64>,
) -> ApiResult<axum::http::StatusCode> {
    delete_rule(&state, &principal, None, rule_id).await
}

/// Add a host rule to a project, or to the instance (`None`).
async fn add_rule(
    state: &AppState,
    principal: &crate::auth::Principal,
    project_id: Option<i64>,
    r: NewHostRule,
) -> ApiResult<Vec<forgust_db::renovate::HostRule>> {
    let host = r.match_host.trim();
    if host.is_empty() || host.len() > 255 || host.chars().any(char::is_whitespace) {
        return Err(ApiError::bad("the host is a name or URL without spaces"));
    }
    let host_type = r.host_type.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if host_type.is_some_and(|t| t.len() > 40 || !t.chars().all(|c| c.is_ascii_lowercase() || c == '-')) {
        return Err(ApiError::bad("the host type is a Renovate datasource or platform name, like docker or npm"));
    }
    if r.password.is_empty() || r.password.len() > 4096 {
        return Err(ApiError::bad("a password or token is needed"));
    }
    if forgust_db::renovate::host_rules(&state.db, project_id).await?.len() >= 50 {
        return Err(ApiError::bad("at most 50 host rules"));
    }
    let sealed = state.secrets.encrypt(r.password.as_bytes()).map_err(ApiError::Internal)?;
    forgust_db::renovate::add_host_rule(&state.db, project_id, host, host_type, r.username.trim(), &sealed).await?;
    let target = project_id.map(|id| ("project", id));
    crate::services::audit(state, principal, "renovate_host_rule_added", target, serde_json::json!({ "host": host }), None).await;
    Ok(forgust_db::renovate::host_rules(&state.db, project_id).await?)
}

async fn delete_rule(
    state: &AppState,
    principal: &crate::auth::Principal,
    project_id: Option<i64>,
    rule_id: i64,
) -> ApiResult<axum::http::StatusCode> {
    if !forgust_db::renovate::delete_host_rule(&state.db, project_id, rule_id).await? {
        return Err(ApiError::NotFound);
    }
    let target = project_id.map(|id| ("project", id));
    crate::services::audit(state, principal, "renovate_host_rule_removed", target, serde_json::json!({ "id": rule_id }), None).await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------ groups

#[derive(Serialize, Deserialize, ToSchema)]
pub struct GroupRenovate {
    /// Dependency updates are on for the group's projects that never chose, and new ones.
    pub enabled: bool,
}

#[derive(Serialize, ToSchema)]
pub struct GroupRenovateStatus {
    pub available: bool,
    pub enabled: bool,
    /// Projects turned on by the last change.
    pub turned_on: usize,
}

#[utoipa::path(get, path = "/api/v1/groups/{id}/renovate", responses((status = 200, body = GroupRenovateStatus)), tag = "renovate")]
pub async fn group_get(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<GroupRenovateStatus>> {
    let ns = super::group_for(&state, &principal, &id, GroupAction::Admin).await?;
    let available = svc::config(&state).await.map_err(ApiError::Internal)?.enabled;
    Ok(Json(GroupRenovateStatus { available, enabled: forgust_db::renovate::group_default(&state.db, ns.id).await?, turned_on: 0 }))
}

/// Turn dependency updates on for every project of the group (those that never chose, and new ones).
#[utoipa::path(put, path = "/api/v1/groups/{id}/renovate", request_body = GroupRenovate, responses((status = 200, body = GroupRenovateStatus)), tag = "renovate")]
pub async fn group_set(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(body): Json<GroupRenovate>,
) -> ApiResult<Json<GroupRenovateStatus>> {
    let ns = super::group_for(&state, &principal, &id, GroupAction::Admin).await?;
    if !ns.is_group() {
        return Err(ApiError::bad("only groups have a default"));
    }
    let turned_on = svc::set_group_default(&state, &principal, &ns, body.enabled).await?;
    Ok(Json(GroupRenovateStatus { available: true, enabled: body.enabled, turned_on }))
}

// ------------------------------------------------------------ admin

#[derive(Serialize, Deserialize, ToSchema)]
pub struct AdminRenovate {
    pub enabled: bool,
    pub image: String,
    pub tags: Vec<String>,
    pub max_concurrent: u32,
    pub timeout_secs: u64,
    /// `RENOVATE_ONBOARDING_CONFIG` (JSON).
    pub onboarding_config: Option<String>,
    /// More variables for every run (shown in jobs: no secrets).
    pub extra_env: std::collections::BTreeMap<String, String>,
    /// Read: whether a github.com token is set. Never returned.
    #[serde(default)]
    pub github_com_token_set: bool,
    /// Write: a new github.com token (no scopes); "" removes it; absent keeps it.
    #[serde(default, skip_serializing)]
    pub github_com_token: Option<String>,
    /// The runner pulls `image` with these (a private registry or a mirror).
    #[serde(default)]
    pub image_pull_username: String,
    /// Read: whether an image pull password is set. Never returned.
    #[serde(default)]
    pub image_pull_password_set: bool,
    /// Write: a new image pull password or token; "" removes the credentials; absent keeps them.
    #[serde(default, skip_serializing)]
    pub image_pull_password: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AdminRenovateStatus {
    #[serde(flatten)]
    pub settings: AdminRenovate,
    pub enabled_projects: i64,
    pub active_runs: i64,
}

async fn admin_status(state: &AppState) -> ApiResult<AdminRenovateStatus> {
    let c = svc::config(state).await.map_err(ApiError::Internal)?;
    let pull = forgust_db::renovate::image_pull(&state.db).await?;
    Ok(AdminRenovateStatus {
        settings: AdminRenovate {
            enabled: c.enabled,
            image: c.image,
            tags: c.tags,
            max_concurrent: c.max_concurrent,
            timeout_secs: c.timeout_secs,
            onboarding_config: c.onboarding_config,
            extra_env: c.extra_env,
            github_com_token_set: c.github_com_token.is_some(),
            github_com_token: None,
            image_pull_password_set: pull.is_some(),
            image_pull_username: pull.map(|(u, _)| u).unwrap_or_default(),
            image_pull_password: None,
        },
        enabled_projects: forgust_db::renovate::enabled_count(&state.db).await?,
        active_runs: forgust_db::renovate::active_runs(&state.db, None).await?,
    })
}

/// Built-in Renovate on this instance: the settings in effect (the config file, then changes made here).
#[utoipa::path(get, path = "/api/v1/admin/renovate", responses((status = 200, body = AdminRenovateStatus)), tag = "renovate")]
pub async fn admin_get(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<AdminRenovateStatus>> {
    Ok(Json(admin_status(&state).await?))
}

#[utoipa::path(put, path = "/api/v1/admin/renovate", request_body = AdminRenovate, responses((status = 200, body = AdminRenovateStatus)), tag = "renovate")]
pub async fn admin_set(
    State(state): State<AppState>,
    Admin(_, principal): Admin,
    Json(s): Json<AdminRenovate>,
) -> ApiResult<Json<AdminRenovateStatus>> {
    if s.image.trim().is_empty() || s.image.chars().any(char::is_whitespace) {
        return Err(ApiError::bad("the image is a name like renovate/renovate:44.226.3"));
    }
    if !(1..=100).contains(&s.max_concurrent) {
        return Err(ApiError::bad("1 to 100 runs at once"));
    }
    if !(300..=86_400).contains(&s.timeout_secs) {
        return Err(ApiError::bad("the timeout is 5 minutes to 24 hours"));
    }
    let onboarding_config = s.onboarding_config.as_deref().map(str::trim).filter(|c| !c.is_empty());
    if onboarding_config.is_some_and(|c| serde_json::from_str::<serde_json::Value>(c).is_err()) {
        return Err(ApiError::bad("the onboarding config is JSON"));
    }
    let key_ok = |k: &str| {
        !k.is_empty()
            && !k.starts_with(|c: char| c.is_ascii_digit())
            && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    };
    if let Some(k) = s.extra_env.keys().find(|k| !key_ok(k)) {
        return Err(ApiError::bad(format!("`{k}` is not a variable name (A-Z, 0-9, _)")));
    }
    if s.extra_env.keys().any(|k| k == "RENOVATE_TOKEN" || k.starts_with("CI_")) {
        return Err(ApiError::bad("RENOVATE_TOKEN and CI_ variables are set by Forgust"));
    }
    let tags: Vec<String> = s.tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    let overrides = svc::Overrides {
        enabled: Some(s.enabled),
        image: Some(s.image.trim().to_string()),
        tags: Some(tags),
        max_concurrent: Some(s.max_concurrent),
        timeout_secs: Some(s.timeout_secs),
        onboarding_config: Some(onboarding_config.map(str::to_string)),
        extra_env: Some(s.extra_env),
    };
    let sealed = match s.github_com_token.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(None),
        Some(t) => Some(Some(state.secrets.encrypt(t.as_bytes()).map_err(ApiError::Internal)?)),
    };
    let value = serde_json::to_value(&overrides).map_err(|e| ApiError::Internal(e.into()))?;
    forgust_db::renovate::set_instance(&state.db, &value, sealed.as_ref().map(|t| t.as_deref())).await?;
    let user = s.image_pull_username.trim();
    match s.image_pull_password.as_deref() {
        Some("") => forgust_db::renovate::set_image_pull(&state.db, None).await?,
        Some(pw) => {
            if user.is_empty() || user.len() > 255 || pw.len() > 4096 {
                return Err(ApiError::bad("image pull credentials need a username and a password of at most 4096 characters"));
            }
            let sealed = state.secrets.encrypt(pw.as_bytes()).map_err(ApiError::Internal)?;
            forgust_db::renovate::set_image_pull(&state.db, Some((user, &sealed))).await?;
        }
        // a new username with the stored password
        None => {
            if let Some((old, sealed)) = forgust_db::renovate::image_pull(&state.db).await?
                && old != user
            {
                if user.is_empty() {
                    return Err(ApiError::bad("image pull credentials need a username"));
                }
                forgust_db::renovate::set_image_pull(&state.db, Some((user, &sealed))).await?;
            }
        }
    }
    crate::services::audit(&state, &principal, "renovate_instance_updated", None, value, None).await;
    Ok(Json(admin_status(&state).await?))
}
