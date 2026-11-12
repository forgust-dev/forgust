//! Endpoints for push rules, GPG keys, commit signatures, code owners of a
//! merge request, and the project's Git LFS locks.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use forgust_db::gpg_keys::GpgKey;
use forgust_db::lfs_locks::LfsLock;
use forgust_db::push_rules::PushRules;
use serde::Deserialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::codeowners::CodeOwners;
use crate::services::signatures::Verification;
use crate::services::{self, signatures};
use crate::state::AppState;

// ------------------------------------------------------------------ push rules

#[utoipa::path(get, path = "/api/v1/projects/{id}/push_rule", responses((status = 200, body = PushRules)), tag = "projects")]
pub async fn push_rule(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<PushRules>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    Ok(Json(forgust_db::push_rules::get(&state.db, p.id).await?))
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/push_rule", request_body = PushRules, responses((status = 200, body = PushRules)), tag = "projects")]
pub async fn set_push_rule(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(rules): Json<PushRules>,
) -> ApiResult<Json<PushRules>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    services::push_rules::validate(&rules)?;
    forgust_db::push_rules::set(&state.db, p.id, &rules).await?;
    services::audit(
        &state,
        &principal,
        "push_rules_changed",
        Some(("project", p.id)),
        serde_json::to_value(&rules).unwrap_or_default(),
        None,
    )
    .await;
    Ok(Json(forgust_db::push_rules::get(&state.db, p.id).await?))
}

// ------------------------------------------------------------------ GPG keys

#[utoipa::path(get, path = "/api/v1/user/gpg_keys", responses((status = 200, body = [GpgKey])), tag = "user")]
pub async fn list_gpg_keys(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Vec<GpgKey>>> {
    Ok(Json(forgust_db::gpg_keys::list_for_user(&state.db, u.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct AddGpgKey {
    /// The armored public key (`gpg --armor --export <id>`).
    pub key: String,
}

#[utoipa::path(post, path = "/api/v1/user/gpg_keys", request_body = AddGpgKey, responses((status = 200, body = GpgKey)), tag = "user")]
pub async fn add_gpg_key(State(state): State<AppState>, SignedIn(u, p): SignedIn, Json(req): Json<AddGpgKey>) -> ApiResult<Json<GpgKey>> {
    // a key makes commits show as this user's: as for SSH keys, not with a token
    super::user::require_interactive(&u, &p)?;
    super::user::require_confirmed(&state, &u)?;
    if req.key.len() > 64 << 10 {
        return Err(ApiError::TooLarge("that key is too large".into()));
    }
    let (fingerprint, ids, emails) = signatures::parse_gpg_key(&req.key).map_err(ApiError::BadRequest)?;
    let id = forgust_db::gpg_keys::insert(&state.db, u.id, req.key.trim(), &fingerprint, &ids, &emails)
        .await
        .map_err(|e| if forgust_db::is_unique_violation(&e) { ApiError::conflict("this key is already in use") } else { e.into() })?;
    services::audit(&state, &p, "gpg_key_added", Some(("user", u.id)), json!({ "fingerprint": fingerprint }), None).await;
    let keys = forgust_db::gpg_keys::list_for_user(&state.db, u.id).await?;
    Ok(Json(keys.into_iter().find(|k| k.id == id).ok_or(ApiError::NotFound)?))
}

pub async fn delete_gpg_key(State(state): State<AppState>, SignedIn(u, p): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    if !forgust_db::gpg_keys::delete_for_user(&state.db, u.id, id).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(&state, &p, "gpg_key_removed", Some(("user", u.id)), json!({ "key_id": id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ signatures

/// A commit's signature, checked now (keys can be added or removed later).
#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/commits/{sha}/signature", responses((status = 200, body = Verification)), tag = "repository")]
pub async fn commit_signature(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, sha)): Path<(String, String)>,
) -> ApiResult<Json<Verification>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let full = state.storage.resolve(p.id, &sha).await?.ok_or(ApiError::NotFound)?;
    let raw = state.storage.raw_commits(p.id, None, &[full]).await?.into_iter().next().ok_or(ApiError::NotFound)?;
    signatures::verify(&state, &raw.1).await.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Deserialize, ToSchema)]
pub struct SignaturesQuery {
    /// Full commit ids (at most 100).
    pub shas: Vec<String>,
}

/// Signatures of several commits (a commit list): sha → verification; unsigned
/// commits are left out.
#[utoipa::path(post, path = "/api/v1/projects/{id}/repository/signatures", request_body = SignaturesQuery, responses((status = 200, body = BTreeMap<String, Verification>)), tag = "repository")]
pub async fn signatures(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Json(q): Json<SignaturesQuery>,
) -> ApiResult<Json<BTreeMap<String, Verification>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    if q.shas.len() > 100 {
        return Err(ApiError::bad("at most 100 commits at a time"));
    }
    let shas: Vec<String> = q.shas.into_iter().filter(|s| s.len() >= 40 && s.bytes().all(|b| b.is_ascii_hexdigit())).collect();
    let mut out = BTreeMap::new();
    for (sha, raw) in state.storage.raw_commits(p.id, None, &shas).await? {
        if let Some(v) = signatures::verify(&state, &raw).await {
            out.insert(sha, v);
        }
    }
    Ok(Json(out))
}

// ------------------------------------------------------------------ code owners

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/code_owners", responses((status = 200, body = CodeOwners)), tag = "merge_requests")]
pub async fn code_owners(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<CodeOwners>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let mr = forgust_db::merge_requests::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(services::codeowners::for_merge_request(&state, &p, &mr).await?))
}

// ------------------------------------------------------------------ LFS locks

#[utoipa::path(get, path = "/api/v1/projects/{id}/lfs_locks", responses((status = 200, body = [LfsLock])), tag = "repository")]
pub async fn lfs_locks(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<Vec<LfsLock>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    Ok(Json(forgust_db::lfs_locks::list(&state.db, p.id, None, None, 0, 1000).await?))
}

/// Remove a lock: its owner, or a Maintainer for anyone's.
pub async fn unlock(
    State(state): State<AppState>,
    SignedIn(u, principal): SignedIn,
    Path((id, lock_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Push).await?;
    let lock = forgust_db::lfs_locks::find(&state.db, p.id, lock_id).await?.ok_or(ApiError::NotFound)?;
    if lock.owner_id != u.id {
        project_for(&state, &principal, &id, ProjectAction::Admin).await.map_err(|_| {
            ApiError::forbidden(format!("{} is locked by @{}; only they or a Maintainer can unlock it", lock.path, lock.owner_username))
        })?;
    }
    forgust_db::lfs_locks::delete(&state.db, p.id, lock_id).await?;
    services::audit(
        &state,
        &principal,
        "lfs_lock_removed",
        Some(("project", p.id)),
        json!({ "path": lock.path, "owner": lock.owner_username }),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}
