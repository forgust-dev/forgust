//! OpenAPI document: the single source of truth for frontend types (ADR-0001).

use axum::Json;
use utoipa::OpenApi;

use super::{
    auth, awards, ci, dto, environments, groups, imports, issues, milestones, mr, notifications, projects, registry, releases, renovate,
    repo, review, search, security, tests, user,
};

#[derive(OpenApi)]
#[openapi(
    info(title = "Forgust API", description = "Forgust REST API (v1)"),
    paths(
        auth::login, auth::login_totp, auth::session, auth::forgot_password, auth::reset_password, auth::confirm_email,
        user::me, user::list_keys, user::list_tokens, crate::oauth::authorize_info, crate::oauth::authorize, user::my_projects, user::work, user::todos, user::contributions, user::profile,
        groups::list_top, groups::create, groups::show, groups::subgroups, groups::projects, groups::members, groups::storage,
        projects::explore, projects::protected_tags, projects::storage, projects::pages, projects::protect_tag, projects::unprotect_tag, projects::create, projects::show, projects::members, projects::users, projects::protected_branches,
        repo::tree, repo::last_commits, repo::stats, repo::blob, repo::commits, repo::commit, repo::commit_diff, repo::branches, repo::blame, repo::templates, repo::create_branch, repo::create_commit, repo::create_tag, repo::delete_tag,
        mr::list, mr::count, mr::create, mr::show, mr::stack_on, mr::auto_merge, mr::merge_status, mr::merge, mr::diff, mr::notes, mr::set_labels,
        awards::mr_reactions, awards::mr_react, awards::issue_reactions, awards::issue_react,
        notifications::mine, notifications::unsubscribe_info, notifications::unsubscribe, notifications::set_mine, notifications::project, notifications::set_project,
        search::search, security::findings, security::finding, security::dismiss, security::reopen, security::merge_request, security::scanning, security::set_scanning, security::run_scan,
        security::group_scanning, security::set_group_scanning, security::admin_scanning, security::set_admin_scanning, tests::pipeline_tests, tests::pipeline_test_cases, tests::mr_tests,
        milestones::list, milestones::show, milestones::create, milestones::update, milestones::delete,
        releases::list, releases::show, releases::create, releases::update, releases::delete, releases::add_link, releases::delete_link, releases::upload,
        issues::list, issues::create, issues::show, issues::update, issues::notes, issues::create_note, issues::edit_note,
        issues::delete_note, issues::labels, issues::create_label, issues::update_label, issues::delete_label,
        imports::connect, imports::lookup, imports::create, imports::list, imports::show, imports::start, imports::cancel, imports::retry,
        ci::instance_runners, ci::create_instance_runner, ci::group_runners, ci::create_group_runner, ci::project_runners,
        ci::create_project_runner, ci::show_runner, ci::update_runner, ci::delete_runner, ci::reset_runner_token,
        ci::list_pipelines, ci::run_pipeline, ci::show_pipeline, ci::cancel_pipeline, ci::retry_pipeline, ci::commit_statuses,
        ci::mr_pipelines, ci::show_job, ci::job_trace, ci::job_raw, ci::cancel_job, ci::retry_job, ci::play_job, ci::job_artifacts,
        ci::ci_settings, ci::update_ci_settings, ci::project_variables, ci::set_project_variable, ci::delete_project_variable,
        ci::group_variables, ci::set_group_variable, ci::delete_group_variable, ci::artifact_tree, ci::artifact_file, ci::job_token_allowlist, ci::allow_job_token,
        ci::disallow_job_token, ci::list_schedules, ci::create_schedule, ci::update_schedule, ci::delete_schedule, ci::play_schedule,
        review::push_rule, review::set_push_rule, review::list_gpg_keys, review::add_gpg_key, review::commit_signature, review::signatures,
        review::code_owners, review::lfs_locks,
        registry::info, registry::tags, registry::cleanup_policy, registry::set_cleanup_policy,
        renovate::get, renovate::set, renovate::run_now, renovate::host_rules, renovate::add_host_rule, renovate::delete_host_rule, renovate::admin_host_rules, renovate::admin_add_host_rule, renovate::admin_delete_host_rule,
        renovate::group_get, renovate::group_set, renovate::admin_get, renovate::admin_set,
        environments::list, environments::show, environments::protected, environments::protect,
        mr::add_to_train, mr::merge_trains,
    ),
    components(schemas(
        ci::RunnerDto, ci::NewRunnerToken, ci::RunnerBody, ci::JobDto, ci::PipelineDetail, ci::PipelineSummary, ci::KeyValue,
        ci::RunPipeline, ci::CommitStatus, ci::JobDetail, ci::TraceChunk, ci::CiSettingsBody, ci::VariableDto, ci::VariableBody,
        forgust_db::ci::Runner, forgust_db::ci::RunnerManager, forgust_db::ci::Pipeline, forgust_db::ci::Artifact,
        forgust_db::ci::ProjectCiSettings, forgust_db::ci::AllowedProject, ci::AllowProject,
        forgust_db::ci::Schedule, ci::ScheduleDto, ci::ScheduleBody, ci::KeyValueOut,
        auth::LoginRequest, auth::LoginResponse, auth::SessionInfo, auth::TotpLogin, auth::RegisterRequest, auth::Provider,
        auth::SetupRequest, auth::SetupStatus, auth::ForgotPassword, auth::ResetPassword, auth::ConfirmEmail,
        user::Work, user::Profile, user::Contributions, forgust_db::contributions::Day, forgust_db::contributions::Activity, forgust_db::people::WorkMr, forgust_db::people::WorkIssue, forgust_db::todos::Todo,
        crate::oauth::AuthorizeInfo, crate::oauth::AuthorizeDecision, crate::oauth::AuthorizeRedirect, crate::oauth::AuthorizeRequest, user::UpdateMe, user::ChangePassword, user::AddKey, user::CreateToken, user::TotpSetup, user::TotpCode, user::TotpDisable, user::MyProject,
        groups::CreateGroup, groups::UpdateGroup, groups::PolicyRequest, groups::NewPath, groups::TransferGroup, groups::AddMember,
        groups::ShareRequest, groups::CreateAccessToken, groups::CreateDeployToken,
        projects::CreateProject, projects::UpdateProject, projects::NewPath, projects::TransferProject, projects::ProtectBranch, projects::AddDeployKey,
        repo::RefResolved, repo::TreeResponse, repo::PathCommit, repo::LastCommits, repo::RepoStats, crate::languages::LanguageShare, repo::BlobResponse, repo::LfsInfo, repo::ReadmeResponse, repo::CompareResponse, repo::BranchDto, repo::TagDto, repo::BlameResponse, repo::FileList, repo::MarkdownBody, repo::MarkdownHtml, repo::DescriptionTemplate, repo::NewBranch, repo::CommitBody, crate::services::edits::FileAction, crate::services::edits::ActionKind, crate::services::edits::Committed,
        super::admin::Overview, super::admin::CreateUser, super::admin::UpdateUser,
        super::admin::StorageOverview, super::admin::SetQuota, super::admin::Backups, super::admin::BackupEntry, forgust_db::admin::AdminProject, forgust_db::admin::AdminGroup, super::admin::Runnable, super::admin::TestEmail, forgust_db::admin::AdminUser, forgust_db::admin::Banner, super::admin::Health, super::admin::HealthCheck, super::admin::JobsOverview, super::admin::UpdateStatus,
        forgust_db::admin::InstanceCounts, forgust_db::admin::CiActivity, forgust_db::admin::PipelineDay, forgust_db::admin::BusyProject,
        forgust_db::admin::StorageKinds, forgust_db::admin::ProjectStorage, forgust_db::admin::GroupStorage,
        forgust_db::admin::BlobScan, forgust_db::admin::PrefixUsage, forgust_db::admin::TaskQueue, forgust_db::admin::FailedJob,
        dto::Resolved, dto::UserDto, dto::GroupDto, dto::GroupDetail, dto::OrgPolicyDto, dto::Breadcrumb, dto::ProjectDto, dto::ProjectDetail,
        dto::MemberDto, dto::ShareDto, dto::ProtectedBranchDto, dto::NewToken, dto::AccessTokenDto,
        forgust_db::ssh_keys::SshKey, forgust_db::ssh_keys::DeployKey, forgust_db::tokens::AccessToken, forgust_db::tokens::DeployToken,
        forgust_db::audit::AuditEvent, forgust_db::settings::InstanceSettings, super::admin::UpdateInstanceSettings,
        forgust_storage::TreeEntry, forgust_storage::EntryKind, forgust_storage::CommitInfo, forgust_storage::CommitSummary, forgust_storage::Person,
        forgust_storage::BlameHunk, forgust_storage::Diff, forgust_storage::DiffFile, forgust_storage::DiffHunk, forgust_storage::DiffLine,
        forgust_storage::DiffLineKind, forgust_storage::FileStat,
        forgust_db::merge_requests::MergeRequest, forgust_db::merge_requests::MrDiff, forgust_db::merge_requests::Note,
        forgust_db::merge_requests::Approval, forgust_db::merge_requests::ApprovalRule, forgust_db::webhooks::Webhook, forgust_db::webhooks::Delivery,
        mr::CreateMr, mr::MrDetail, mr::UpdateMr, mr::MergeRequestBody, mr::NoteDto, mr::CreateNote, mr::EditNote, mr::ResolveBody,
        mr::RuleBody, mr::HookBody, mr::MrListItem, mr::LabelsBody, mr::StackOnBody, mr::AutoMergeBody, mr::AutoMergeResult, crate::services::auto_merge::Outcome,
        crate::services::stack::StackView, crate::services::stack::StackEntry, crate::services::stack::StackBadge,
        issues::IssueDto, issues::IssueList, issues::CreateIssueBody, issues::IssueDetail, issues::RelatedMr, issues::UpdateIssueBody, issues::IssueNoteDto,
        issues::NoteBody, issues::LabelBody, forgust_db::issues::Issue, forgust_db::issues::IssueNote, forgust_db::issues::Label,
        forgust_db::issues::LabelWithCounts, forgust_db::people::UserBrief, forgust_db::people::Reviewer,
        imports::ConnectBody, imports::LookupBody, imports::Connected, imports::ImportProjectBody, imports::CreateImportBody, imports::ImportDetail,
        imports::StartBody, imports::RetryBody, forgust_db::imports::Import, forgust_db::imports::ImportProject, forgust_db::imports::ImportUser,
        crate::import::model::SourceProject, crate::import::model::Report, crate::import::Options,
        crate::services::mr::MergeStatus, crate::services::mr::RuleStatus, crate::services::mr::MappedPosition,
        crate::services::mr::MergeMethod, crate::services::mr::NewPosition,
        forgust_db::push_rules::PushRules, forgust_db::gpg_keys::GpgKey, forgust_db::lfs_locks::LfsLock, review::AddGpgKey, review::SignaturesQuery,
        crate::services::signatures::Verification, crate::services::codeowners::CodeOwners, crate::services::codeowners::SectionReport,
        registry::RegistryInfo, registry::ImageRepository, forgust_db::registry::RepositorySummary, forgust_db::registry::Tag,
        forgust_db::registry::CleanupPolicy,
        renovate::RenovateStatus, renovate::RenovateRun, renovate::RenovateSettingsBody, renovate::RenovateRunStarted,
        renovate::NewHostRule, forgust_db::renovate::HostRule, renovate::GroupRenovate, renovate::GroupRenovateStatus, renovate::AdminRenovate,
        renovate::AdminRenovateStatus,
        environments::EnvironmentDetail, environments::ProtectEnvironment, forgust_db::environments::Environment,
        forgust_db::environments::EnvironmentSummary, forgust_db::environments::Deployment, forgust_db::environments::ProtectedEnvironment,
        ci::JobEnvironment, forgust_db::ci::PipelineLink, forgust_db::merge_trains::Car,
    )),
    tags((name = "auth"), (name = "user"), (name = "groups"), (name = "projects"), (name = "repository"), (name = "merge_requests"), (name = "issues"), (name = "releases"), (name = "imports"), (name = "registry"))
)]
pub struct ApiDoc;

pub fn document() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

pub async fn spec() -> Json<utoipa::openapi::OpenApi> {
    Json(document())
}

#[cfg(test)]
mod committed {
    /// `web/openapi.json` is what `forgust openapi` prints. Checked here rather than
    /// by running the binary in CI: `cargo run` builds the server a second time,
    /// with other features.
    #[test]
    fn committed_spec_is_up_to_date() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/openapi.json");
        let committed = std::fs::read_to_string(path).unwrap();
        let generated = serde_json::to_string_pretty(&super::document()).unwrap() + "\n";
        assert!(
            committed == generated,
            "web/openapi.json is out of date: run `cargo run -p forgust-server -- openapi > web/openapi.json`, then `(cd web && npm run gen:api)`"
        );
    }
}
