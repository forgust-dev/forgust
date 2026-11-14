//! REST API under `/api/v1` (our own API; `/api/v4` compatibility comes in M4/M5).

pub mod admin;
pub mod auth;
pub mod avatars;
pub mod awards;
pub mod ci;
pub mod dto;
pub mod environments;
pub mod groups;
pub mod imports;
pub mod issues;
pub mod milestones;
pub mod mr;
pub mod notifications;
pub mod openapi;
pub mod projects;
pub mod registry;
pub mod releases;
pub mod renovate;
pub mod repo;
pub mod review;
pub mod search;
pub mod security;
pub mod tests;
pub mod user;

use axum::extract::{DefaultBodyLimit, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use forgust_db::namespaces::Namespace;
use forgust_db::projects::Project;
use serde::Deserialize;

use crate::auth::authz::{GroupAction, ProjectAction, require_group, require_project};
use crate::auth::{Principal, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        // session and sign-in
        .route("/auth/login", post(auth::login))
        .route("/auth/login/totp", post(auth::login_totp))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/password/forgot", post(auth::forgot_password))
        .route("/auth/password/reset", post(auth::reset_password))
        .route("/auth/email/confirm", post(auth::confirm_email))
        .route("/user/email/resend", post(user::resend_confirmation))
        .route("/auth/register", post(auth::register))
        .route("/auth/providers", get(auth::providers))
        .route("/auth/oidc/{name}/start", get(auth::oidc_start))
        .route("/auth/oidc/{name}/callback", get(auth::oidc_callback))
        .route("/session", get(auth::session))
        .route("/setup", get(auth::setup_status).post(auth::setup))
        // the signed-in user
        .route("/user", get(user::me).put(user::update_me))
        .route("/user/password", put(user::change_password))
        .route("/user/keys", get(user::list_keys).post(user::add_key))
        .route("/user/keys/{id}", delete(user::delete_key))
        .route("/user/gpg_keys", get(review::list_gpg_keys).post(review::add_gpg_key))
        .route("/user/gpg_keys/{id}", delete(review::delete_gpg_key))
        .route("/user/tokens", get(user::list_tokens).post(user::create_token))
        .route("/user/tokens/{id}", delete(user::revoke_token))
        .route("/oauth/authorize", get(crate::oauth::authorize_info).post(crate::oauth::authorize))
        .route("/user/two_factor/setup", post(user::totp_setup))
        .route("/user/two_factor/enable", post(user::totp_enable))
        .route("/user/two_factor/disable", post(user::totp_disable))
        .route("/user/projects", get(user::my_projects))
        .route("/user/work", get(user::work))
        .route("/user/todos", get(user::todos))
        .route("/user/todos/done", post(user::todos_done))
        .route("/user/todos/{id}/done", post(user::todo_done))
        .route("/users", get(user::search_users))
        .route("/users/{username}/contributions", get(user::contributions))
        .route("/users/{username}", get(user::profile))
        .route("/users/{username}/avatar", get(avatars::user_avatar))
        .route(
            "/user/avatar",
            put(avatars::set_my_avatar)
                .delete(avatars::delete_my_avatar)
                .layer(DefaultBodyLimit::max(crate::services::avatars::MAX_UPLOAD + 1024)),
        )
        .route(
            "/projects/{id}/avatar",
            get(avatars::project_avatar)
                .put(avatars::set_project_avatar)
                .delete(avatars::delete_project_avatar)
                .layer(DefaultBodyLimit::max(crate::services::avatars::MAX_UPLOAD + 1024)),
        )
        .route(
            "/groups/{id}/avatar",
            get(avatars::group_avatar)
                .put(avatars::set_group_avatar)
                .delete(avatars::delete_group_avatar)
                .layer(DefaultBodyLimit::max(crate::services::avatars::MAX_UPLOAD + 1024)),
        )
        // imports from GitLab and GitHub
        .route("/imports", get(imports::list).post(imports::create))
        .route("/imports/connect", post(imports::connect))
        .route("/imports/lookup", post(imports::lookup))
        .route("/imports/{id}", get(imports::show))
        .route("/imports/{id}/start", post(imports::start))
        .route("/imports/{id}/cancel", post(imports::cancel))
        .route("/imports/{id}/retry", post(imports::retry))
        // orgs and groups
        .route("/groups", get(groups::list_top).post(groups::create))
        .route("/groups/{id}", get(groups::show).put(groups::update).delete(groups::delete))
        .route("/groups/{id}/policy", put(groups::set_policy))
        .route("/groups/{id}/storage", get(groups::storage))
        .route("/groups/{id}/path", post(groups::rename))
        .route("/groups/{id}/transfer", post(groups::transfer))
        .route("/groups/{id}/subgroups", get(groups::subgroups))
        .route("/groups/{id}/projects", get(groups::projects))
        .route("/groups/{id}/members", get(groups::members).post(groups::add_member))
        .route("/groups/{id}/members/{user_id}", delete(groups::remove_member))
        .route("/groups/{id}/shares", get(groups::shares).post(groups::share))
        .route("/groups/{id}/shares/{group_id}", delete(groups::unshare))
        .route("/groups/{id}/access_tokens", get(groups::list_access_tokens).post(groups::create_access_token))
        .route("/groups/{id}/access_tokens/{bot_id}", delete(groups::revoke_access_token))
        .route("/groups/{id}/deploy_tokens", get(groups::list_deploy_tokens).post(groups::create_deploy_token))
        .route("/groups/{id}/deploy_tokens/{token_id}", delete(groups::revoke_deploy_token))
        // projects
        .route("/projects", get(projects::explore).post(projects::create))
        .route("/projects/{id}", get(projects::show).put(projects::update).delete(projects::delete))
        .route("/projects/{id}/path", post(projects::rename))
        .route("/projects/{id}/transfer", post(projects::transfer))
        .route("/projects/{id}/archive", post(projects::archive))
        .route("/projects/{id}/unarchive", post(projects::unarchive))
        .route("/projects/{id}/members", get(projects::members).post(projects::add_member))
        .route("/projects/{id}/members/{user_id}", delete(projects::remove_member))
        .route("/projects/{id}/shares", get(projects::shares).post(projects::share))
        .route("/projects/{id}/shares/{group_id}", delete(projects::unshare))
        .route("/projects/{id}/protected_branches", get(projects::protected_branches).post(projects::protect_branch))
        .route("/projects/{id}/protected_branches/{pb_id}", delete(projects::unprotect_branch))
        .route("/projects/{id}/protected_tags", get(projects::protected_tags).post(projects::protect_tag))
        .route("/projects/{id}/storage", get(projects::storage))
        .route("/projects/{id}/pages", get(projects::pages).delete(projects::remove_pages))
        .route("/projects/{id}/security/findings", get(security::findings))
        .route("/projects/{id}/security/findings/{finding_id}", get(security::finding))
        .route("/projects/{id}/security/findings/{finding_id}/dismiss", post(security::dismiss))
        .route("/projects/{id}/security/findings/{finding_id}/reopen", post(security::reopen))
        .route("/projects/{id}/merge_requests/{iid}/security", get(security::merge_request))
        .route("/projects/{id}/security/scanning", get(security::scanning).put(security::set_scanning))
        .route("/projects/{id}/security/scanning/run", post(security::run_scan))
        .route("/groups/{id}/security/scanning", get(security::group_scanning).put(security::set_group_scanning))
        .route("/admin/security/scanning", get(security::admin_scanning).put(security::set_admin_scanning))
        .route("/projects/{id}/protected_tags/{pt_id}", delete(projects::unprotect_tag))
        .route("/projects/{id}/deploy_keys", get(projects::deploy_keys).post(projects::add_deploy_key))
        .route("/projects/{id}/deploy_keys/{key_id}", delete(projects::remove_deploy_key))
        .route("/projects/{id}/deploy_tokens", get(projects::list_deploy_tokens).post(projects::create_deploy_token))
        .route("/projects/{id}/deploy_tokens/{token_id}", delete(projects::revoke_deploy_token))
        .route("/projects/{id}/access_tokens", get(projects::list_access_tokens).post(projects::create_access_token))
        .route("/projects/{id}/access_tokens/{bot_id}", delete(projects::revoke_access_token))
        .route("/projects/{id}/audit", get(projects::audit_log))
        // repository (M2)
        .route("/projects/{id}/repository/resolve", get(repo::resolve))
        .route("/projects/{id}/repository/tree", get(repo::tree))
        .route("/projects/{id}/repository/blob", get(repo::blob))
        .route("/projects/{id}/repository/raw", get(repo::raw))
        .route("/projects/{id}/repository/readme", get(repo::readme))
        .route("/projects/{id}/repository/last_commits", get(repo::last_commits))
        .route("/projects/{id}/repository/stats", get(repo::stats))
        .route("/projects/{id}/repository/archive", get(repo::archive))
        .route("/projects/{id}/repository/commits", get(repo::commits).post(repo::create_commit))
        .route("/projects/{id}/repository/commits/{sha}", get(repo::commit))
        .route("/projects/{id}/repository/commits/{sha}/signature", get(review::commit_signature))
        .route("/projects/{id}/repository/signatures", post(review::signatures))
        .route("/projects/{id}/push_rule", get(review::push_rule).put(review::set_push_rule))
        .route("/projects/{id}/lfs_locks", get(review::lfs_locks))
        .route("/projects/{id}/renovate", get(renovate::get).put(renovate::set))
        .route("/projects/{id}/renovate/run", post(renovate::run_now))
        .route("/projects/{id}/renovate/host_rules", get(renovate::host_rules).post(renovate::add_host_rule))
        .route("/projects/{id}/renovate/host_rules/{rule_id}", delete(renovate::delete_host_rule))
        .route("/groups/{id}/renovate", get(renovate::group_get).put(renovate::group_set))
        .route("/admin/renovate", get(renovate::admin_get).put(renovate::admin_set))
        .route("/admin/renovate/host_rules", get(renovate::admin_host_rules).post(renovate::admin_add_host_rule))
        .route("/admin/renovate/host_rules/{rule_id}", delete(renovate::admin_delete_host_rule))
        .route("/projects/{id}/registry", get(registry::info))
        .route("/projects/{id}/environments", get(environments::list))
        .route("/projects/{id}/environments/{env_id}", get(environments::show).delete(environments::delete))
        .route("/projects/{id}/environments/{env_id}/stop", post(environments::stop))
        .route("/projects/{id}/protected_environments", get(environments::protected).post(environments::protect))
        .route("/projects/{id}/protected_environments/{pe_id}", delete(environments::unprotect))
        .route("/projects/{id}/registry/repositories/{repository_id}", delete(registry::delete_repository))
        .route("/projects/{id}/registry/repositories/{repository_id}/tags", get(registry::tags))
        .route("/projects/{id}/registry/repositories/{repository_id}/tags/{tag}", delete(registry::delete_tag))
        .route("/projects/{id}/registry/cleanup_policy", get(registry::cleanup_policy).put(registry::set_cleanup_policy))
        .route("/projects/{id}/registry/cleanup", post(registry::run_cleanup))
        .route("/projects/{id}/lfs_locks/{lock_id}", delete(review::unlock))
        .route("/projects/{id}/repository/commits/{sha}/diff", get(repo::commit_diff))
        .route("/projects/{id}/repository/compare", get(repo::compare))
        .route("/projects/{id}/repository/branches", get(repo::branches).post(repo::create_branch))
        .route("/projects/{id}/repository/branches/{*branch}", delete(repo::delete_branch))
        .route("/projects/{id}/repository/tags", get(repo::tags).post(repo::create_tag))
        .route("/projects/{id}/merge_requests/{iid}/reactions", get(awards::mr_reactions).post(awards::mr_react))
        .route("/projects/{id}/issues/{iid}/reactions", get(awards::issue_reactions).post(awards::issue_react))
        .route("/user/notification_settings", get(notifications::mine).put(notifications::set_mine))
        .route("/user/notification_channels", get(notifications::channels).put(notifications::set_channel))
        .route("/user/notifications", get(notifications::inbox))
        .route("/user/notifications/read", post(notifications::read_all))
        .route("/user/notifications/{id}/read", post(notifications::read_one))
        .route("/unsubscribe/{user_id}/{project_id}/{sig}", get(notifications::unsubscribe_info).post(notifications::unsubscribe))
        .route("/projects/{id}/notification_settings", get(notifications::project).put(notifications::set_project))
        .route("/search", get(search::search))
        .route("/projects/{id}/pipelines/{iid}/test_report", get(tests::pipeline_tests))
        .route("/projects/{id}/pipelines/{iid}/test_cases", get(tests::pipeline_test_cases))
        .route("/projects/{id}/merge_requests/{iid}/test_summary", get(tests::mr_tests))
        .route("/projects/{id}/milestones", get(milestones::list).post(milestones::create))
        .route("/projects/{id}/milestones/{milestone_id}", get(milestones::show).put(milestones::update).delete(milestones::delete))
        .route("/projects/{id}/releases", get(releases::list).post(releases::create))
        .route("/projects/{id}/releases/{tag}", get(releases::show).put(releases::update).delete(releases::delete))
        .route("/projects/{id}/releases/{tag}/links", post(releases::add_link))
        .route("/projects/{id}/releases/{tag}/links/{link_id}", delete(releases::delete_link))
        // streamed and capped by limits.max_upload_bytes
        .route("/projects/{id}/uploads", post(releases::upload).layer(DefaultBodyLimit::disable()))
        .route("/projects/{id}/repository/tags/{*tag}", delete(repo::delete_tag))
        .route("/projects/{id}/repository/blame", get(repo::blame))
        .route("/projects/{id}/repository/files", get(repo::files))
        .route("/projects/{id}/markdown", post(repo::markdown))
        .route("/projects/{id}/templates/{kind}", get(repo::templates))
        // merge requests (M3)
        .route("/projects/{id}/merge_requests", get(mr::list).post(mr::create))
        .route("/projects/{id}/merge_requests_count", get(mr::count))
        // issues and labels
        .route("/projects/{id}/issues", get(issues::list).post(issues::create))
        .route("/projects/{id}/issues/{iid}", get(issues::show).put(issues::update))
        .route("/projects/{id}/issues/{iid}/notes", get(issues::notes).post(issues::create_note))
        .route("/projects/{id}/issues/{iid}/notes/{note_id}", put(issues::edit_note).delete(issues::delete_note))
        .route("/projects/{id}/labels", get(issues::labels).post(issues::create_label))
        .route("/projects/{id}/labels/{label_id}", put(issues::update_label).delete(issues::delete_label))
        .route("/projects/{id}/merge_requests/{iid}", get(mr::show).put(mr::update))
        .route("/projects/{id}/merge_requests/{iid}/close", post(mr::close))
        .route("/projects/{id}/merge_requests/{iid}/labels", put(mr::set_labels))
        .route("/projects/{id}/merge_requests/{iid}/reopen", post(mr::reopen))
        .route("/projects/{id}/merge_requests/{iid}/merge_status", get(mr::merge_status))
        .route("/projects/{id}/merge_requests/{iid}/code_owners", get(review::code_owners))
        .route("/projects/{id}/merge_requests/{iid}/merge_train", post(mr::add_to_train).delete(mr::remove_from_train))
        .route("/projects/{id}/merge_trains", get(mr::merge_trains))
        .route("/projects/{id}/merge_requests/{iid}/merge", post(mr::merge))
        .route("/projects/{id}/merge_requests/{iid}/auto_merge", post(mr::auto_merge).delete(mr::cancel_auto_merge))
        .route("/projects/{id}/merge_requests/{iid}/rebase", post(mr::rebase))
        .route("/projects/{id}/merge_requests/{iid}/approve", post(mr::approve))
        .route("/projects/{id}/merge_requests/{iid}/unapprove", post(mr::unapprove))
        .route("/projects/{id}/merge_requests/{iid}/request_changes", post(mr::request_changes))
        .route("/projects/{id}/merge_requests/{iid}/stack", put(mr::stack_on).delete(mr::leave_stack))
        .route("/projects/{id}/merge_requests/{iid}/stack/rebase", post(mr::rebase_stack))
        .route("/projects/{id}/merge_requests/{iid}/reviewers/{user_id}/rerequest", post(mr::rerequest_review))
        .route("/projects/{id}/users", get(projects::users))
        .route("/projects/{id}/merge_requests/{iid}/commits", get(mr::commits))
        .route("/projects/{id}/merge_requests/{iid}/diff", get(mr::diff))
        .route("/projects/{id}/merge_requests/{iid}/notes", get(mr::notes).post(mr::create_note))
        .route("/projects/{id}/merge_requests/{iid}/notes/{note_id}", put(mr::edit_note).delete(mr::delete_note))
        .route("/projects/{id}/merge_requests/{iid}/notes/{note_id}/resolve", post(mr::resolve_note))
        .route("/projects/{id}/merge_requests/{iid}/notes/{note_id}/apply_suggestion", post(mr::apply_suggestion))
        .route("/projects/{id}/approval_rules", get(mr::rules).post(mr::upsert_rule))
        .route("/projects/{id}/approval_rules/{rule_id}", delete(mr::delete_rule))
        .route("/{kind}/{id}/hooks", get(mr::list_hooks).post(mr::create_hook))
        .route("/{kind}/{id}/hooks/{hook_id}", delete(mr::delete_hook))
        .route("/{kind}/{id}/hooks/{hook_id}/deliveries", get(mr::hook_deliveries))
        .route("/events", get(mr::events))
        // CI/CD (M4)
        .route("/admin/runners", get(ci::instance_runners).post(ci::create_instance_runner))
        .route("/groups/{id}/runners", get(ci::group_runners).post(ci::create_group_runner))
        .route("/projects/{id}/runners", get(ci::project_runners).post(ci::create_project_runner))
        .route("/runners/{id}", get(ci::show_runner).put(ci::update_runner).delete(ci::delete_runner))
        .route("/runners/{id}/reset_token", post(ci::reset_runner_token))
        .route("/projects/{id}/pipelines", get(ci::list_pipelines).post(ci::run_pipeline))
        .route("/projects/{id}/pipelines/{iid}", get(ci::show_pipeline))
        .route("/projects/{id}/pipelines/{iid}/cancel", post(ci::cancel_pipeline))
        .route("/projects/{id}/pipelines/{iid}/retry", post(ci::retry_pipeline))
        .route("/projects/{id}/commit_statuses", get(ci::commit_statuses))
        .route("/projects/{id}/merge_requests/{iid}/pipelines", get(ci::mr_pipelines))
        .route("/projects/{id}/jobs/{job_id}", get(ci::show_job))
        .route("/projects/{id}/jobs/{job_id}/trace", get(ci::job_trace))
        .route("/projects/{id}/jobs/{job_id}/raw", get(ci::job_raw))
        .route("/projects/{id}/jobs/{job_id}/cancel", post(ci::cancel_job))
        .route("/projects/{id}/jobs/{job_id}/retry", post(ci::retry_job))
        .route("/projects/{id}/jobs/{job_id}/play", post(ci::play_job))
        .route("/projects/{id}/jobs/{job_id}/unschedule", post(ci::unschedule_job))
        .route("/projects/{id}/jobs/{job_id}/artifacts", get(ci::job_artifacts))
        .route("/projects/{id}/jobs/{job_id}/artifacts/tree", get(ci::artifact_tree))
        .route("/projects/{id}/jobs/{job_id}/artifacts/file", get(ci::artifact_file))
        .route("/projects/{id}/ci/settings", get(ci::ci_settings).put(ci::update_ci_settings))
        .route("/projects/{id}/variables", get(ci::project_variables).post(ci::set_project_variable))
        .route("/projects/{id}/variables/{key}", delete(ci::delete_project_variable))
        .route("/groups/{id}/variables", get(ci::group_variables).post(ci::set_group_variable))
        .route("/groups/{id}/variables/{key}", delete(ci::delete_group_variable))
        .route("/projects/{id}/job_token_allowlist", get(ci::job_token_allowlist).post(ci::allow_job_token))
        .route("/projects/{id}/job_token_allowlist/{source_id}", delete(ci::disallow_job_token))
        .route("/projects/{id}/pipeline_schedules", get(ci::list_schedules).post(ci::create_schedule))
        .route("/projects/{id}/pipeline_schedules/{sid}", put(ci::update_schedule).delete(ci::delete_schedule))
        .route("/projects/{id}/pipeline_schedules/{sid}/play", post(ci::play_schedule))
        // instance administration
        .route("/admin/overview", get(admin::overview))
        .route("/admin/storage", get(admin::storage))
        .route("/admin/projects", get(admin::projects))
        .route("/admin/projects/{id}/maintenance", post(admin::project_maintenance))
        .route("/admin/groups", get(admin::groups))
        .route("/admin/backups", get(admin::backups).post(admin::start_backup))
        .route("/admin/backups/{name}", delete(admin::delete_backup))
        .route("/admin/storage/scan", post(admin::scan_storage))
        .route("/admin/storage/quota", put(admin::set_default_quota))
        .route("/admin/namespaces/{id}/quota", put(admin::set_namespace_quota))
        .route("/admin/projects/{id}/quota", put(admin::set_project_quota))
        .route("/admin/health", get(admin::health))
        .route("/admin/jobs", get(admin::jobs))
        .route("/admin/users", get(admin::users).post(admin::create_user))
        .route("/admin/users/{id}", put(admin::update_user).delete(admin::delete_user))
        .route("/admin/users/{id}/reset_two_factor", post(admin::reset_two_factor))
        .route("/admin/users/{id}/sign_out", post(admin::sign_out))
        .route("/admin/users/{id}/impersonate", post(auth::impersonate))
        .route("/auth/stop_impersonating", post(auth::stop_impersonating))
        .route("/admin/users/{id}/revoke_tokens", post(admin::revoke_tokens))
        .route("/admin/users/{id}/resend_confirmation", post(admin::resend_confirmation))
        .route("/admin/jobs/{job_id}/retry", post(admin::retry_job))
        .route("/admin/jobs/{job_id}", delete(admin::drop_job))
        .route("/admin/tasks", get(admin::runnable))
        .route("/admin/tasks/{task}", post(admin::run_task))
        .route("/admin/health/email", post(admin::test_email))
        .route("/admin/banner", get(admin::banner_settings).put(admin::set_banner))
        .route("/banner", get(admin::banner))
        .route("/admin/settings", get(admin::settings).put(admin::update_settings))
        .route("/admin/audit", get(admin::audit))
        // misc
        .route("/resolve", get(resolve_path))
        .route("/highlight.css", get(highlight_css))
        .route("/version", get(version))
        .route("/openapi.json", get(openapi::spec))
}

/// `{id}` path segments accept a numeric ID or a URL-encoded full path.
pub async fn load_project(state: &AppState, id: &str) -> ApiResult<Project> {
    if let Ok(n) = id.parse::<i64>() {
        return forgust_db::projects::find(&state.db, n).await?.ok_or(ApiError::NotFound);
    }
    // by its path in one query; an old path (renamed, moved) through its redirect
    if let Some(p) = forgust_db::projects::find_by_path(&state.db, id).await? {
        return Ok(p);
    }
    let mut conn = state.db.acquire().await?;
    match forgust_db::routes::lookup(&mut conn, id).await? {
        Some(h) if h.source_type == "project" => forgust_db::projects::find(&mut *conn, h.source_id).await?.ok_or(ApiError::NotFound),
        _ => Err(ApiError::NotFound),
    }
}

pub async fn load_namespace(state: &AppState, id: &str) -> ApiResult<Namespace> {
    if let Ok(n) = id.parse::<i64>() {
        return forgust_db::namespaces::find(&state.db, n).await?.ok_or(ApiError::NotFound);
    }
    let mut conn = state.db.acquire().await?;
    match forgust_db::routes::lookup(&mut conn, id).await? {
        Some(h) if h.source_type == "namespace" => forgust_db::namespaces::find(&mut *conn, h.source_id).await?.ok_or(ApiError::NotFound),
        _ => Err(ApiError::NotFound),
    }
}

/// Load a project the principal may act on (404 when it cannot see it).
pub async fn project_for(state: &AppState, principal: &Principal, id: &str, action: ProjectAction) -> ApiResult<Project> {
    let p = load_project(state, id).await?;
    require_project(state, principal, action, &p).await?;
    Ok(p)
}

pub async fn group_for(state: &AppState, principal: &Principal, id: &str, action: GroupAction) -> ApiResult<Namespace> {
    let ns = load_namespace(state, id).await?;
    require_group(state, principal, action, &ns).await?;
    Ok(ns)
}

#[derive(Deserialize)]
pub struct Paging {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
    #[serde(default)]
    pub search: String,
}

fn default_page() -> i64 {
    1
}
fn default_per_page() -> i64 {
    20
}

/// `(limit, offset)` of a page: at most 100 a page, pages from 1.
pub fn page_window(page: i64, per_page: i64) -> (i64, i64) {
    let per = per_page.clamp(1, 100);
    (per, (page.max(1) - 1) * per)
}

impl Paging {
    pub fn limit(&self) -> i64 {
        page_window(self.page, self.per_page).0
    }
    pub fn offset(&self) -> i64 {
        page_window(self.page, self.per_page).1
    }
}

#[derive(Deserialize)]
pub struct ResolveQuery {
    path: String,
}

/// Resolve a URL path to a project, group or user namespace (used by the web app
/// for GitLab-shaped URLs). Old paths report where they moved.
async fn resolve_path(State(state): State<AppState>, Viewer(p): Viewer, Query(q): Query<ResolveQuery>) -> ApiResult<Json<dto::Resolved>> {
    let path = q.path.trim_matches('/');
    let mut conn = state.db.acquire().await?;
    let hit = forgust_db::routes::lookup(&mut conn, path).await?.ok_or(ApiError::NotFound)?;
    drop(conn);
    let kind = if hit.source_type == "project" {
        let proj = forgust_db::projects::find(&state.db, hit.source_id).await?.ok_or(ApiError::NotFound)?;
        require_project(&state, &p, ProjectAction::Read, &proj).await?;
        "project"
    } else {
        let ns = forgust_db::namespaces::find(&state.db, hit.source_id).await?.ok_or(ApiError::NotFound)?;
        if ns.is_group() {
            require_group(&state, &p, GroupAction::Read, &ns).await?;
            "group"
        } else {
            // a person's page (their profile) is open to signed-in people, like
            // the user search; what it lists is filtered by what they may see
            if p.direct_user().is_none() {
                return Err(ApiError::Unauthorized);
            }
            let owner = match ns.owner_id {
                Some(id) => forgust_db::users::find(&state.db, id).await?,
                None => None,
            };
            if !owner.is_some_and(|u| u.is_active() && !u.is_bot()) {
                return Err(ApiError::NotFound);
            }
            "user"
        }
    };
    Ok(Json(dto::Resolved { kind: kind.into(), id: hit.source_id, full_path: hit.path, redirected: hit.redirected }))
}

async fn highlight_css(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css"), (axum::http::header::CACHE_CONTROL, "public, max-age=86400")],
        state.highlighter.css().to_string(),
    )
}

async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "name": "Forgust", "version": env!("CARGO_PKG_VERSION") }))
}
