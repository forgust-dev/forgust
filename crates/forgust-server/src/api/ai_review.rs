//! AI review: the instance's endpoint (administrators), a project's switch
//! (Maintainers), and a merge request's reviews ("Request AI review").

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::{ProjectAction, can_project};
use crate::auth::{Admin, SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::ai_review as svc;
use crate::state::AppState;
use forgust_db::merge_requests::MergeRequest;
use forgust_db::projects::Project;

// ------------------------------------------------------------ project

#[derive(Serialize, ToSchema)]
pub struct AiReviewSettings {
    /// The instance has AI review on and set up.
    pub available: bool,
    pub enabled: bool,
    /// Where the code of reviewed merge requests is sent (host of the endpoint).
    pub endpoint: Option<String>,
    pub model: String,
}

#[derive(Deserialize, ToSchema)]
pub struct AiReviewSettingsBody {
    pub enabled: bool,
}

async fn settings_of(state: &AppState, project_id: i64) -> ApiResult<AiReviewSettings> {
    let c = svc::config(state).await.map_err(ApiError::Internal)?;
    let enabled = forgust_db::ai_review::get(&state.db, project_id).await?.is_some_and(|s| s.enabled);
    Ok(AiReviewSettings { available: svc::usable(&c), enabled, endpoint: svc::endpoint_host(&c), model: c.model })
}

/// AI review of the project's merge requests (Maintainers).
#[utoipa::path(get, path = "/api/v1/projects/{id}/ai_review", responses((status = 200, body = AiReviewSettings)), tag = "ai_review")]
pub async fn get(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<AiReviewSettings>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    Ok(Json(settings_of(&state, p.id).await?))
}

/// Turn AI review on or off for the project.
#[utoipa::path(put, path = "/api/v1/projects/{id}/ai_review", request_body = AiReviewSettingsBody, responses((status = 200, body = AiReviewSettings)), tag = "ai_review")]
pub async fn set(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(body): Json<AiReviewSettingsBody>,
) -> ApiResult<Json<AiReviewSettings>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    svc::configure(&state, &principal, &p, body.enabled).await?;
    Ok(Json(settings_of(&state, p.id).await?))
}

// ------------------------------------------------------------ merge request

#[derive(Serialize, ToSchema)]
pub struct AiReviewRun {
    pub pipeline_iid: i64,
    pub pipeline_status: String,
    /// pending, posted, outdated or failed
    pub state: String,
    pub base_sha: String,
    pub head_sha: String,
    /// Reviewed the merge request's latest version.
    pub current: bool,
    pub comments_posted: i32,
    pub comments_dropped: i32,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct AiReviewStatus {
    /// AI review is on for the project (and the instance).
    pub available: bool,
    /// The viewer may ask for a review.
    pub can_request: bool,
    /// Where a review sends the code (for who can push).
    pub endpoint: Option<String>,
    pub latest: Option<AiReviewRun>,
}

async fn status_of(state: &AppState, principal: &crate::auth::Principal, p: &Project, mr: &MergeRequest) -> ApiResult<AiReviewStatus> {
    // a project that never turned it on has no reviews: the usual answer, cheap
    let Some(settings) = forgust_db::ai_review::get(&state.db, p.id).await? else {
        return Ok(AiReviewStatus { available: false, can_request: false, endpoint: None, latest: None });
    };
    let c = svc::config(state).await.map_err(ApiError::Internal)?;
    let on = svc::usable(&c) && settings.enabled;
    // where the code goes and why a review failed are for who can push
    let pusher = can_project(state, principal, ProjectAction::Push, p).await?;
    let can_request = on && mr.state == "opened" && !p.archived() && pusher;
    let latest = match forgust_db::ai_review::latest(&state.db, mr.id).await? {
        Some(r) => {
            let head = forgust_db::merge_requests::latest_diff(&state.db, mr.id).await?.map(|d| d.head_sha);
            let pl = forgust_db::ci::pipeline(&state.db, r.pipeline_id).await?;
            // a retried job goes on: reviewing again
            let going = pl.as_ref().is_some_and(|pl| forgust_db::ci::ACTIVE.contains(&pl.status.as_str()));
            Some(AiReviewRun {
                pipeline_iid: pl.as_ref().map_or(0, |pl| pl.iid),
                pipeline_status: pl.map_or_else(String::new, |pl| pl.status),
                current: head.as_deref() == Some(r.head_sha.as_str()),
                state: if going { "pending".into() } else { r.state },
                base_sha: r.base_sha,
                head_sha: r.head_sha,
                comments_posted: r.comments_posted,
                comments_dropped: r.comments_dropped,
                error: r.error.filter(|_| pusher),
                created_at: r.created_at,
                finished_at: r.finished_at,
            })
        }
        None => None,
    };
    Ok(AiReviewStatus { available: on, can_request, endpoint: (on && pusher).then(|| svc::endpoint_host(&c)).flatten(), latest })
}

/// The merge request's latest AI review.
#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/ai_review", responses((status = 200, body = AiReviewStatus)), tag = "ai_review")]
pub async fn mr_status(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<AiReviewStatus>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let mr = forgust_db::merge_requests::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(status_of(&state, &v, &p, &mr).await?))
}

/// Ask for an AI review of the merge request's latest version (who can push).
#[utoipa::path(post, path = "/api/v1/projects/{id}/merge_requests/{iid}/ai_review", responses((status = 200, body = AiReviewStatus)), tag = "ai_review")]
pub async fn mr_request(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<AiReviewStatus>> {
    let p = project_for(&state, &principal, &id, ProjectAction::ReadCode).await?;
    let mr = forgust_db::merge_requests::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    svc::request(&state, &principal, &p, &mr).await?;
    Ok(Json(status_of(&state, &principal, &p, &mr).await?))
}

// ------------------------------------------------------------ admin

#[derive(Serialize, Deserialize, ToSchema)]
pub struct AdminAiReview {
    pub enabled: bool,
    /// Which API (`openai`: any OpenAI-compatible endpoint); the reviewer
    /// image says which it knows.
    pub provider: String,
    /// The API's base, reached from the runners (e.g. `http://ollama:11434/v1`).
    pub base_url: String,
    pub model: String,
    pub image: String,
    pub tags: Vec<String>,
    pub max_concurrent: u32,
    pub timeout_secs: u64,
    pub max_comments: u32,
    /// The reviewer's options (`MAX_DIFF_BYTES`, `TEMPERATURE`...), given to
    /// the job as `AI_REVIEW_<NAME>`. Not masked: no secrets.
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, String>,
    /// Read: the names of the secret options set. Their values are never returned.
    #[serde(default)]
    pub secret_option_names: Vec<String>,
    /// Write: the secret options, all of them ({} removes them); absent keeps them.
    #[serde(default, skip_serializing)]
    pub secret_options: Option<std::collections::BTreeMap<String, String>>,
    /// PEM of an in-house CA the endpoint's certificate is signed by.
    pub ca_pem: Option<String>,
    /// Read: whether a key is set. Never returned.
    #[serde(default)]
    pub api_key_set: bool,
    /// Write: a new key; "" removes it; absent keeps it.
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AdminAiReviewStatus {
    #[serde(flatten)]
    pub settings: AdminAiReview,
    pub enabled_projects: i64,
    pub active_runs: i64,
}

async fn admin_status(state: &AppState) -> ApiResult<AdminAiReviewStatus> {
    let c = svc::config(state).await.map_err(ApiError::Internal)?;
    let (key, secret_options) = svc::secrets(state).await.map_err(ApiError::Internal)?;
    Ok(AdminAiReviewStatus {
        settings: AdminAiReview {
            enabled: c.enabled,
            provider: c.provider,
            base_url: c.base_url,
            model: c.model,
            image: c.image,
            tags: c.tags,
            max_concurrent: c.max_concurrent,
            timeout_secs: c.timeout_secs,
            max_comments: c.max_comments,
            options: c.options,
            secret_option_names: secret_options.into_keys().collect(),
            secret_options: None,
            ca_pem: c.ca_pem,
            api_key_set: key.is_some(),
            api_key: None,
        },
        enabled_projects: forgust_db::ai_review::enabled_count(&state.db).await?,
        active_runs: forgust_db::ai_review::active(&state.db).await?,
    })
}

/// AI review on this instance: the settings in effect (the config file, then changes made here).
#[utoipa::path(get, path = "/api/v1/admin/ai_review", responses((status = 200, body = AdminAiReviewStatus)), tag = "ai_review")]
pub async fn admin_get(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<AdminAiReviewStatus>> {
    Ok(Json(admin_status(&state).await?))
}

#[utoipa::path(put, path = "/api/v1/admin/ai_review", request_body = AdminAiReview, responses((status = 200, body = AdminAiReviewStatus)), tag = "ai_review")]
pub async fn admin_set(
    State(state): State<AppState>,
    Admin(_, principal): Admin,
    Json(s): Json<AdminAiReview>,
) -> ApiResult<Json<AdminAiReviewStatus>> {
    let base_url = s.base_url.trim();
    if !base_url.is_empty() {
        let ok = crate::services::webhooks::validate_url(base_url).is_ok_and(|u| u.password().is_none());
        if !ok {
            return Err(ApiError::bad(
                "the endpoint is an http(s) URL like https://api.openai.com/v1, without credentials (the key has its own field)",
            ));
        }
    }
    if s.enabled && (base_url.is_empty() || s.model.trim().is_empty()) {
        return Err(ApiError::bad("turning AI review on needs an endpoint and a model"));
    }
    if s.model.len() > 200 || s.model.chars().any(char::is_control) {
        return Err(ApiError::bad("the model is a name like gpt-4.1 or qwen2.5-coder:32b"));
    }
    if s.image.trim().is_empty() || s.image.chars().any(char::is_whitespace) {
        return Err(ApiError::bad("the image is a name like ghcr.io/forgust-dev/forgust-reviewer:0.3.0"));
    }
    if !(1..=50).contains(&s.max_concurrent) {
        return Err(ApiError::bad("1 to 50 reviews at once"));
    }
    if !(60..=7_200).contains(&s.timeout_secs) {
        return Err(ApiError::bad("the timeout is 1 minute to 2 hours"));
    }
    if !(1..=50).contains(&s.max_comments) {
        return Err(ApiError::bad("1 to 50 comments per review"));
    }
    let provider = s.provider.trim();
    if provider.is_empty() || provider.len() > 64 || !provider.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(ApiError::bad("the provider is a name like openai"));
    }
    let names_ok = |m: &std::collections::BTreeMap<String, String>| -> ApiResult<()> {
        if m.len() > 50 {
            return Err(ApiError::bad("at most 50 options"));
        }
        if let Some(k) = m.keys().find(|k| !svc::option_name_ok(k)) {
            return Err(ApiError::bad(format!(
                "`{k}` is not an option name (A-Z, 0-9, _; not {}, which Forgust sets)",
                svc::RESERVED_OPTIONS.join(", ")
            )));
        }
        if m.values().any(|v| v.len() > 4096) {
            return Err(ApiError::bad("an option's value is at most 4096 characters"));
        }
        Ok(())
    };
    names_ok(&s.options)?;
    if let Some(m) = &s.secret_options {
        names_ok(m)?;
    }
    let ca_pem = s.ca_pem.as_deref().map(str::trim).filter(|p| !p.is_empty());
    if ca_pem.is_some_and(|p| !p.contains("-----BEGIN CERTIFICATE-----") || p.len() > 100_000) {
        return Err(ApiError::bad("the CA is PEM (-----BEGIN CERTIFICATE-----)"));
    }
    let tags: Vec<String> = s.tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    let overrides = svc::Overrides {
        enabled: s.enabled,
        provider: provider.to_string(),
        base_url: base_url.to_string(),
        model: s.model.trim().to_string(),
        image: s.image.trim().to_string(),
        tags,
        max_concurrent: s.max_concurrent,
        timeout_secs: s.timeout_secs,
        max_comments: s.max_comments,
        options: s.options,
        ca_pem: ca_pem.map(str::to_string),
    };
    let sealed = match s.api_key.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(None),
        Some(k) if k.len() > 4096 => return Err(ApiError::bad("the key is at most 4096 characters")),
        Some(k) => Some(Some(state.secrets.encrypt(k.as_bytes()).map_err(ApiError::Internal)?)),
    };
    let sealed_options = match &s.secret_options {
        None => None,
        Some(m) if m.is_empty() => Some(None),
        Some(m) => Some(Some(state.secrets.encrypt_json(m).map_err(ApiError::Internal)?)),
    };
    let value = serde_json::to_value(&overrides).map_err(|e| ApiError::Internal(e.into()))?;
    forgust_db::ai_review::set_instance(
        &state.db,
        &value,
        sealed.as_ref().map(|t| t.as_deref()),
        sealed_options.as_ref().map(|t| t.as_deref()),
    )
    .await?;
    let mut audit = value.clone();
    let what = |x: &Option<Option<String>>| match x {
        None => "kept",
        Some(None) => "removed",
        Some(Some(_)) => "changed",
    };
    audit["api_key"] = serde_json::json!(what(&sealed));
    audit["secret_options"] = match &s.secret_options {
        Some(m) if !m.is_empty() => serde_json::json!({ "set": m.keys().collect::<Vec<_>>() }),
        _ => serde_json::json!(what(&sealed_options)),
    };
    crate::services::audit(&state, &principal, "ai_review_instance_updated", None, audit, None).await;
    Ok(Json(admin_status(&state).await?))
}
