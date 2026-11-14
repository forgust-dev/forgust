//! Merge requests, their versions, notes and approvals (M3).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

/// Next per-project number for `usage` (`merge_requests`, later `issues`).
pub async fn next_iid(e: impl PgExecutor<'_>, project_id: i64, usage: &str) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO internal_ids (project_id, usage, last_value) VALUES ($1, $2, 1)
         ON CONFLICT (project_id, usage) DO UPDATE SET last_value = internal_ids.last_value + 1
         RETURNING last_value",
        project_id,
        usage
    )
    .fetch_one(e)
    .await
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MergeRequest {
    pub id: i64,
    pub project_id: i64,
    pub iid: i64,
    pub title: String,
    pub description: String,
    pub author_id: Option<i64>,
    pub author_username: Option<String>,
    pub source_branch: String,
    pub target_branch: String,
    pub state: String,
    pub draft: bool,
    pub squash: bool,
    pub merge_commit_sha: Option<String>,
    pub merged_by: Option<i64>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Who opened it on the forge it was imported from, when they have no account here.
    pub imported_author: Option<String>,
    /// The stack it belongs to (ADR-0019).
    pub stack_id: Option<i64>,
    #[serde(skip)]
    pub stack_base_sha: Option<String>,
    /// Who set auto-merge (it merges as them once every check passes).
    pub auto_merge_by: Option<i64>,
    /// merge, squash, fast_forward or rebase
    pub auto_merge_method: Option<String>,
    #[serde(skip)]
    pub auto_merge_sha: Option<String>,
    pub auto_merge_remove_source: bool,
    /// The target it was set for: a different target cancels it.
    #[serde(skip)]
    pub auto_merge_target: Option<String>,
    /// Delete the source branch when merging (the merge box's default).
    pub remove_source_branch: bool,
    pub milestone_id: Option<i64>,
    pub milestone_title: Option<String>,
}

pub struct NewMergeRequest<'a> {
    pub project_id: i64,
    pub iid: i64,
    pub title: &'a str,
    pub description: &'a str,
    pub author_id: i64,
    pub source_branch: &'a str,
    pub target_branch: &'a str,
    pub draft: bool,
    pub squash: bool,
    pub remove_source_branch: bool,
}

pub async fn insert(e: impl PgExecutor<'_>, m: NewMergeRequest<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO merge_requests (project_id, iid, title, description, author_id, source_branch, target_branch, draft, squash,
                                     remove_source_branch)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        m.project_id,
        m.iid,
        m.title,
        m.description,
        m.author_id,
        m.source_branch,
        m.target_branch,
        m.draft,
        m.squash,
        m.remove_source_branch
    )
    .fetch_one(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, project_id: i64, iid: i64) -> Result<Option<MergeRequest>> {
    sqlx::query_as!(
        MergeRequest,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.author_id, u.username AS "author_username?",
                  m.source_branch, m.target_branch, m.state, m.draft, m.squash, m.merge_commit_sha, m.merged_by,
                  m.merged_at, m.closed_at, m.created_at, m.updated_at, m.imported_author, m.stack_id, m.stack_base_sha,
                  m.auto_merge_by, m.auto_merge_method, m.auto_merge_sha, m.auto_merge_remove_source, m.auto_merge_target, m.remove_source_branch,
                  m.milestone_id, ms.title AS "milestone_title?"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id LEFT JOIN milestones ms ON ms.id = m.milestone_id
           WHERE m.project_id = $1 AND m.iid = $2"#,
        project_id,
        iid
    )
    .fetch_optional(e)
    .await
}

pub async fn find_by_id(e: impl PgExecutor<'_>, id: i64) -> Result<Option<MergeRequest>> {
    sqlx::query_as!(
        MergeRequest,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.author_id, u.username AS "author_username?",
                  m.source_branch, m.target_branch, m.state, m.draft, m.squash, m.merge_commit_sha, m.merged_by,
                  m.merged_at, m.closed_at, m.created_at, m.updated_at, m.imported_author, m.stack_id, m.stack_base_sha,
                  m.auto_merge_by, m.auto_merge_method, m.auto_merge_sha, m.auto_merge_remove_source, m.auto_merge_target, m.remove_source_branch,
                  m.milestone_id, ms.title AS "milestone_title?"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id LEFT JOIN milestones ms ON ms.id = m.milestone_id
           WHERE m.id = $1"#,
        id
    )
    .fetch_optional(e)
    .await
}

/// `state` = opened / closed / merged / all.
pub async fn list(
    e: impl PgExecutor<'_>,
    project_id: i64,
    state: &str,
    search: &str,
    people: PeopleFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<MergeRequest>> {
    sqlx::query_as!(
        MergeRequest,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.author_id, u.username AS "author_username?",
                  m.source_branch, m.target_branch, m.state, m.draft, m.squash, m.merge_commit_sha, m.merged_by,
                  m.merged_at, m.closed_at, m.created_at, m.updated_at, m.imported_author, m.stack_id, m.stack_base_sha,
                  m.auto_merge_by, m.auto_merge_method, m.auto_merge_sha, m.auto_merge_remove_source, m.auto_merge_target, m.remove_source_branch,
                  m.milestone_id, ms.title AS "milestone_title?"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id LEFT JOIN milestones ms ON ms.id = m.milestone_id
           WHERE m.project_id = $1 AND ($2 = 'all' OR m.state = $2)
             AND ($3 = '' OR m.title ILIKE '%' || $3 || '%' OR m.source_branch ILIKE '%' || $3 || '%'
                  OR m.iid::text = $3 OR ('!' || m.iid::text) = $3)
             AND ($6::bigint IS NULL OR m.author_id = $6)
             AND ($7::bigint IS NULL OR EXISTS (SELECT 1 FROM merge_request_assignees a WHERE a.merge_request_id = m.id AND a.user_id = $7))
             AND ($8::bigint IS NULL OR EXISTS (SELECT 1 FROM merge_request_reviewers r WHERE r.merge_request_id = m.id AND r.user_id = $8))
             AND ($9::bigint IS NULL OR ($9 = 0 AND m.milestone_id IS NULL) OR m.milestone_id = $9)
           ORDER BY m.updated_at DESC LIMIT $4 OFFSET $5"#,
        project_id,
        state,
        search,
        limit,
        offset,
        people.author_id,
        people.assignee_id,
        people.reviewer_id,
        people.milestone_id
    )
    .fetch_all(e)
    .await
}

/// Filters on the people of an MR; `None` matches anyone.
#[derive(Debug, Default, Clone, Copy)]
pub struct PeopleFilter {
    pub author_id: Option<i64>,
    pub assignee_id: Option<i64>,
    pub reviewer_id: Option<i64>,
    /// On this milestone (`Some(0)`: on none).
    pub milestone_id: Option<i64>,
}

/// Open MRs whose source branch is `branch` (to add a version after a push).
pub async fn open_with_source(e: impl PgExecutor<'_>, project_id: i64, branch: &str) -> Result<Vec<MergeRequest>> {
    sqlx::query_as!(
        MergeRequest,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.author_id, u.username AS "author_username?",
                  m.source_branch, m.target_branch, m.state, m.draft, m.squash, m.merge_commit_sha, m.merged_by,
                  m.merged_at, m.closed_at, m.created_at, m.updated_at, m.imported_author, m.stack_id, m.stack_base_sha,
                  m.auto_merge_by, m.auto_merge_method, m.auto_merge_sha, m.auto_merge_remove_source, m.auto_merge_target, m.remove_source_branch,
                  m.milestone_id, ms.title AS "milestone_title?"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id LEFT JOIN milestones ms ON ms.id = m.milestone_id
           WHERE m.project_id = $1 AND m.source_branch = $2 AND m.state = 'opened'"#,
        project_id,
        branch
    )
    .fetch_all(e)
    .await
}

pub async fn open_exists(e: impl PgExecutor<'_>, project_id: i64, source: &str, target: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        "SELECT iid FROM merge_requests WHERE project_id = $1 AND source_branch = $2 AND target_branch = $3 AND state = 'opened'",
        project_id,
        source,
        target
    )
    .fetch_optional(e)
    .await
}

pub async fn update_details(
    e: impl PgExecutor<'_>,
    id: i64,
    title: &str,
    description: &str,
    draft: bool,
    target_branch: &str,
) -> Result<()> {
    sqlx::query!(
        "UPDATE merge_requests SET title = $2, description = $3, draft = $4, target_branch = $5, updated_at = now() WHERE id = $1",
        id,
        title,
        description,
        draft,
        target_branch
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_state(e: impl PgExecutor<'_>, id: i64, state: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE merge_requests SET state = $2, closed_at = CASE WHEN $2 = 'closed' THEN now() ELSE NULL END, updated_at = now(),
                auto_merge_by = CASE WHEN $2 = 'opened' THEN auto_merge_by END WHERE id = $1",
        id,
        state
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_merged(e: impl PgExecutor<'_>, id: i64, sha: &str, by: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE merge_requests SET state = 'merged', merge_commit_sha = $2, merged_by = $3, merged_at = now(), updated_at = now(),
                auto_merge_by = NULL, auto_merge_method = NULL, auto_merge_sha = NULL WHERE id = $1",
        id,
        sha,
        by
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn touch(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE merge_requests SET updated_at = now() WHERE id = $1", id).execute(e).await?;
    Ok(())
}

// ------------------------------------------------------------------ versions

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MrDiff {
    pub id: i64,
    pub version: i32,
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
    pub commits_count: i32,
    pub files_count: i32,
    pub additions: i32,
    pub deletions: i32,
    pub created_at: DateTime<Utc>,
}

pub struct NewDiff<'a> {
    pub base_sha: &'a str,
    pub start_sha: &'a str,
    pub head_sha: &'a str,
    pub commits_count: i32,
    pub files_count: i32,
    pub additions: i32,
    pub deletions: i32,
}

/// Add the next version (numbering is per MR; the caller holds the MR row lock).
/// The MR's next version. `at`: when it was made (imports keep the source's
/// time); none is now.
pub async fn add_diff(conn: &mut PgConnection, mr_id: i64, d: NewDiff<'_>, at: Option<DateTime<Utc>>) -> Result<MrDiff> {
    sqlx::query_as!(
        MrDiff,
        "INSERT INTO merge_request_diffs (merge_request_id, version, base_sha, start_sha, head_sha, commits_count, files_count, additions,
                                          deletions, created_at)
         VALUES ($1, coalesce((SELECT max(version) FROM merge_request_diffs WHERE merge_request_id = $1), 0) + 1, $2, $3, $4, $5, $6, $7, $8,
                 coalesce($9, now()))
         RETURNING id, version, base_sha, start_sha, head_sha, commits_count, files_count, additions, deletions, created_at",
        mr_id,
        d.base_sha,
        d.start_sha,
        d.head_sha,
        d.commits_count,
        d.files_count,
        d.additions,
        d.deletions,
        at
    )
    .fetch_one(&mut *conn)
    .await
}

pub async fn diffs(e: impl PgExecutor<'_>, mr_id: i64) -> Result<Vec<MrDiff>> {
    sqlx::query_as!(
        MrDiff,
        "SELECT id, version, base_sha, start_sha, head_sha, commits_count, files_count, additions, deletions, created_at
         FROM merge_request_diffs WHERE merge_request_id = $1 ORDER BY version DESC",
        mr_id
    )
    .fetch_all(e)
    .await
}

pub async fn latest_diff(e: impl PgExecutor<'_>, mr_id: i64) -> Result<Option<MrDiff>> {
    sqlx::query_as!(
        MrDiff,
        "SELECT id, version, base_sha, start_sha, head_sha, commits_count, files_count, additions, deletions, created_at
         FROM merge_request_diffs WHERE merge_request_id = $1 ORDER BY version DESC LIMIT 1",
        mr_id
    )
    .fetch_optional(e)
    .await
}

/// The latest diff of each of several MRs: `(mr_id, diff)` (lists).
pub async fn latest_diffs(e: impl PgExecutor<'_>, mr_ids: &[i64]) -> Result<Vec<(i64, MrDiff)>> {
    let rows = sqlx::query!(
        "SELECT DISTINCT ON (merge_request_id) merge_request_id, id, version, base_sha, start_sha, head_sha, commits_count, files_count, additions, deletions, created_at
         FROM merge_request_diffs WHERE merge_request_id = ANY($1) ORDER BY merge_request_id, version DESC",
        mr_ids
    )
    .fetch_all(e)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.merge_request_id,
                MrDiff {
                    id: r.id,
                    version: r.version,
                    base_sha: r.base_sha,
                    start_sha: r.start_sha,
                    head_sha: r.head_sha,
                    commits_count: r.commits_count,
                    files_count: r.files_count,
                    additions: r.additions,
                    deletions: r.deletions,
                    created_at: r.created_at,
                },
            )
        })
        .collect())
}

/// Lock the MR row for the rest of the transaction (versions, merges).
pub async fn lock(conn: &mut PgConnection, mr_id: i64) -> Result<()> {
    sqlx::query!("SELECT id FROM merge_requests WHERE id = $1 FOR UPDATE", mr_id).fetch_one(&mut *conn).await?;
    Ok(())
}

// ------------------------------------------------------------------ notes

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Note {
    pub id: i64,
    pub merge_request_id: i64,
    pub author_id: Option<i64>,
    pub author_username: Option<String>,
    pub author_name: Option<String>,
    pub parent_id: Option<i64>,
    pub body: String,
    pub system: bool,
    pub diff_id: Option<i64>,
    pub base_sha: Option<String>,
    pub start_sha: Option<String>,
    pub head_sha: Option<String>,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub old_line: Option<i32>,
    pub new_line: Option<i32>,
    pub resolved_by: Option<i64>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub imported_author: Option<String>,
    /// The commit that applied this note's suggested change.
    pub suggestion_applied_sha: Option<String>,
    /// A comment on several lines: where they start (old_line / new_line end them).
    pub start_old_line: Option<i32>,
    pub start_new_line: Option<i32>,
    /// A review verdict that came with this comment: `requested_changes`.
    pub review: Option<String>,
}

/// Make a comment the one a review verdict came with.
pub async fn set_note_review(e: impl PgExecutor<'_>, note_id: i64, review: &str) -> Result<()> {
    sqlx::query!("UPDATE notes SET review = $2 WHERE id = $1", note_id, review).execute(e).await?;
    Ok(())
}

#[derive(Default)]
pub struct Position<'a> {
    pub diff_id: Option<i64>,
    pub base_sha: Option<&'a str>,
    pub start_sha: Option<&'a str>,
    pub head_sha: Option<&'a str>,
    pub old_path: Option<&'a str>,
    pub new_path: Option<&'a str>,
    pub old_line: Option<i32>,
    pub new_line: Option<i32>,
    pub start_old_line: Option<i32>,
    pub start_new_line: Option<i32>,
}

pub async fn add_note(
    e: impl PgExecutor<'_>,
    mr_id: i64,
    author: Option<i64>,
    parent: Option<i64>,
    body: &str,
    system: bool,
    pos: Position<'_>,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO notes (merge_request_id, author_id, parent_id, body, system, diff_id, base_sha, start_sha, head_sha,
                            old_path, new_path, old_line, new_line, start_old_line, start_new_line)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING id",
        mr_id,
        author,
        parent,
        body,
        system,
        pos.diff_id,
        pos.base_sha,
        pos.start_sha,
        pos.head_sha,
        pos.old_path,
        pos.new_path,
        pos.old_line,
        pos.new_line,
        pos.start_old_line,
        pos.start_new_line
    )
    .fetch_one(e)
    .await
}

pub async fn notes(e: impl PgExecutor<'_>, mr_id: i64) -> Result<Vec<Note>> {
    sqlx::query_as!(
        Note,
        r#"SELECT n.id, n.merge_request_id, n.author_id, u.username AS "author_username?", u.name AS "author_name?",
                  n.parent_id, n.body, n.system, n.diff_id, n.base_sha, n.start_sha, n.head_sha, n.old_path, n.new_path,
                  n.old_line, n.new_line, n.resolved_by, n.resolved_at, n.created_at, n.updated_at, n.imported_author,
                  n.suggestion_applied_sha, n.start_old_line, n.start_new_line, n.review
           FROM notes n LEFT JOIN users u ON u.id = n.author_id
           WHERE n.merge_request_id = $1 ORDER BY n.id"#,
        mr_id
    )
    .fetch_all(e)
    .await
}

pub async fn note(e: impl PgExecutor<'_>, mr_id: i64, id: i64) -> Result<Option<Note>> {
    sqlx::query_as!(
        Note,
        r#"SELECT n.id, n.merge_request_id, n.author_id, u.username AS "author_username?", u.name AS "author_name?",
                  n.parent_id, n.body, n.system, n.diff_id, n.base_sha, n.start_sha, n.head_sha, n.old_path, n.new_path,
                  n.old_line, n.new_line, n.resolved_by, n.resolved_at, n.created_at, n.updated_at, n.imported_author,
                  n.suggestion_applied_sha, n.start_old_line, n.start_new_line, n.review
           FROM notes n LEFT JOIN users u ON u.id = n.author_id
           WHERE n.merge_request_id = $1 AND n.id = $2"#,
        mr_id,
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn edit_note(e: impl PgExecutor<'_>, id: i64, body: &str) -> Result<()> {
    sqlx::query!("UPDATE notes SET body = $2, updated_at = now() WHERE id = $1", id, body).execute(e).await?;
    Ok(())
}

pub async fn delete_note(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM notes WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// Resolve or reopen a thread (its first note).
pub async fn set_resolved(e: impl PgExecutor<'_>, id: i64, by: Option<i64>) -> Result<()> {
    sqlx::query!(
        "UPDATE notes SET resolved_by = $2, resolved_at = CASE WHEN $2::bigint IS NULL THEN NULL ELSE now() END WHERE id = $1",
        id,
        by
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Unresolved threads that started on a diff line or as discussions (not system notes).
pub async fn unresolved_threads(e: impl PgExecutor<'_>, mr_id: i64) -> Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM notes
           WHERE merge_request_id = $1 AND parent_id IS NULL AND NOT system AND head_sha IS NOT NULL AND resolved_at IS NULL"#,
        mr_id
    )
    .fetch_one(e)
    .await
}

/// Users who wrote in the MR (for notifications).
pub async fn participants(e: impl PgExecutor<'_>, mr_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        r#"SELECT DISTINCT u AS "u!" FROM (
               SELECT author_id AS u FROM merge_requests WHERE id = $1
               UNION SELECT author_id FROM notes WHERE merge_request_id = $1 AND NOT system
               UNION SELECT user_id FROM approvals WHERE merge_request_id = $1
               UNION SELECT user_id FROM merge_request_assignees WHERE merge_request_id = $1
               UNION SELECT user_id FROM merge_request_reviewers WHERE merge_request_id = $1
           ) x WHERE u IS NOT NULL"#,
        mr_id
    )
    .fetch_all(e)
    .await
}

// ------------------------------------------------------------------ approvals

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ApprovalRule {
    pub id: i64,
    pub name: String,
    pub approvals_required: i16,
    pub min_access_level: i16,
}

pub async fn rules(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<ApprovalRule>> {
    sqlx::query_as!(
        ApprovalRule,
        "SELECT id, name, approvals_required, min_access_level FROM approval_rules WHERE project_id = $1 ORDER BY name",
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn upsert_rule(e: impl PgExecutor<'_>, project_id: i64, name: &str, required: i16, min_level: i16) -> Result<()> {
    sqlx::query!(
        "INSERT INTO approval_rules (project_id, name, approvals_required, min_access_level) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id, name) DO UPDATE SET approvals_required = EXCLUDED.approvals_required,
             min_access_level = EXCLUDED.min_access_level",
        project_id,
        name,
        required,
        min_level
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete_rule(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM approval_rules WHERE id = $1 AND project_id = $2", id, project_id).execute(e).await?.rows_affected() > 0)
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Approval {
    pub user_id: i64,
    pub username: String,
    pub name: String,
    /// The approver's current role on the project.
    pub access_level: i16,
    pub created_at: DateTime<Utc>,
}

pub async fn approvals(e: impl PgExecutor<'_>, mr_id: i64, project_id: i64) -> Result<Vec<Approval>> {
    sqlx::query_as!(
        Approval,
        r#"SELECT a.user_id, u.username, u.name, coalesce(pa.access_level, 0)::smallint AS "access_level!", a.created_at
           FROM approvals a JOIN users u ON u.id = a.user_id
           LEFT JOIN project_authorizations pa ON pa.user_id = a.user_id AND pa.project_id = $2
           WHERE a.merge_request_id = $1 ORDER BY a.created_at"#,
        mr_id,
        project_id
    )
    .fetch_all(e)
    .await
}

/// Whether the approval is new (not one the person had already given).
pub async fn approve(e: impl PgExecutor<'_>, mr_id: i64, user_id: i64) -> Result<bool> {
    Ok(sqlx::query!("INSERT INTO approvals (merge_request_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", mr_id, user_id)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

pub async fn unapprove(e: impl PgExecutor<'_>, mr_id: i64, user_id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM approvals WHERE merge_request_id = $1 AND user_id = $2", mr_id, user_id).execute(e).await?.rows_affected()
        > 0)
}

/// Approvals are reset when new commits arrive (documented in the API).
pub async fn reset_approvals(e: impl PgExecutor<'_>, mr_id: i64) -> Result<u64> {
    Ok(sqlx::query!("DELETE FROM approvals WHERE merge_request_id = $1", mr_id).execute(e).await?.rows_affected())
}

// ------------------------------------------------------------------ imports

/// A merge request as it was on the source forge (number, state and dates kept).
pub struct ImportedMergeRequest<'a> {
    pub project_id: i64,
    pub iid: i64,
    pub title: &'a str,
    pub description: &'a str,
    pub author_id: Option<i64>,
    pub imported_author: Option<&'a str>,
    pub source_branch: &'a str,
    pub target_branch: &'a str,
    pub state: &'a str,
    pub draft: bool,
    pub squash: bool,
    pub merge_commit_sha: Option<&'a str>,
    pub merged_by: Option<i64>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub milestone_id: Option<i64>,
}

pub async fn insert_imported(e: impl PgExecutor<'_>, m: ImportedMergeRequest<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO merge_requests (project_id, iid, title, description, author_id, imported_author, source_branch, target_branch,
                                     state, draft, squash, merge_commit_sha, merged_by, merged_at, closed_at, created_at, updated_at,
                                     milestone_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) RETURNING id",
        m.project_id,
        m.iid,
        m.title,
        m.description,
        m.author_id,
        m.imported_author,
        m.source_branch,
        m.target_branch,
        m.state,
        m.draft,
        m.squash,
        m.merge_commit_sha,
        m.merged_by,
        m.merged_at,
        m.closed_at,
        m.created_at,
        m.updated_at,
        m.milestone_id
    )
    .fetch_one(e)
    .await
}

pub struct ImportedNote<'a> {
    pub merge_request_id: i64,
    pub author_id: Option<i64>,
    pub imported_author: Option<&'a str>,
    pub parent_id: Option<i64>,
    pub body: &'a str,
    pub system: bool,
    pub pos: Position<'a>,
    pub resolved: bool,
    pub created_at: DateTime<Utc>,
}

pub async fn add_note_imported(e: impl PgExecutor<'_>, n: ImportedNote<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO notes (merge_request_id, author_id, imported_author, parent_id, body, system, diff_id, base_sha, start_sha, head_sha,
                            old_path, new_path, old_line, new_line, resolved_at, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, CASE WHEN $15::boolean THEN $16::timestamptz END, $16, $16) RETURNING id",
        n.merge_request_id,
        n.author_id,
        n.imported_author,
        n.parent_id,
        n.body,
        n.system,
        n.pos.diff_id,
        n.pos.base_sha,
        n.pos.start_sha,
        n.pos.head_sha,
        n.pos.old_path,
        n.pos.new_path,
        n.pos.old_line,
        n.pos.new_line,
        n.resolved,
        n.created_at
    )
    .fetch_one(e)
    .await
}

/// Move a project's counter past imported numbers so new ones follow them.
pub async fn bump_iid(e: impl PgExecutor<'_>, project_id: i64, usage: &str, at_least: i64) -> Result<()> {
    sqlx::query!(
        "INSERT INTO internal_ids (project_id, usage, last_value) VALUES ($1, $2, $3)
         ON CONFLICT (project_id, usage) DO UPDATE SET last_value = greatest(internal_ids.last_value, EXCLUDED.last_value)",
        project_id,
        usage,
        at_least
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Filters for the GitLab-compatible list (`/api/v4`).
pub struct ListFilter<'a> {
    /// opened / closed / merged / all
    pub state: &'a str,
    pub source_branch: Option<&'a str>,
    pub target_branch: Option<&'a str>,
    pub author_id: Option<i64>,
    pub assignee_id: Option<i64>,
    pub reviewer_id: Option<i64>,
    pub updated_after: Option<DateTime<Utc>>,
    pub search: &'a str,
    /// created_at or updated_at
    pub order_by: &'a str,
    pub ascending: bool,
    pub limit: i64,
    pub offset: i64,
}

pub async fn list_filtered(e: impl PgExecutor<'_>, project_id: i64, f: ListFilter<'_>) -> Result<Vec<MergeRequest>> {
    sqlx::query_as!(
        MergeRequest,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.author_id, u.username AS "author_username?",
                  m.source_branch, m.target_branch, m.state, m.draft, m.squash, m.merge_commit_sha, m.merged_by,
                  m.merged_at, m.closed_at, m.created_at, m.updated_at, m.imported_author, m.stack_id, m.stack_base_sha,
                  m.auto_merge_by, m.auto_merge_method, m.auto_merge_sha, m.auto_merge_remove_source, m.auto_merge_target, m.remove_source_branch,
                  m.milestone_id, ms.title AS "milestone_title?"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id LEFT JOIN milestones ms ON ms.id = m.milestone_id
           WHERE m.project_id = $1 AND ($2 = 'all' OR m.state = $2)
             AND ($3::text IS NULL OR m.source_branch = $3)
             AND ($4::text IS NULL OR m.target_branch = $4)
             AND ($5::bigint IS NULL OR m.author_id = $5)
             AND ($6::timestamptz IS NULL OR m.updated_at > $6)
             AND ($7 = '' OR m.title ILIKE '%' || $7 || '%' OR m.description ILIKE '%' || $7 || '%')
             AND ($12::bigint IS NULL OR EXISTS (SELECT 1 FROM merge_request_assignees a WHERE a.merge_request_id = m.id AND a.user_id = $12))
             AND ($13::bigint IS NULL OR EXISTS (SELECT 1 FROM merge_request_reviewers r WHERE r.merge_request_id = m.id AND r.user_id = $13))
           ORDER BY
             CASE WHEN $8 = 'created_at' AND $9 THEN m.created_at END ASC,
             CASE WHEN $8 = 'created_at' AND NOT $9 THEN m.created_at END DESC,
             CASE WHEN $8 <> 'created_at' AND $9 THEN m.updated_at END ASC,
             CASE WHEN $8 <> 'created_at' AND NOT $9 THEN m.updated_at END DESC,
             m.id DESC
           LIMIT $10 OFFSET $11"#,
        project_id,
        f.state,
        f.source_branch,
        f.target_branch,
        f.author_id,
        f.updated_after,
        f.search,
        f.order_by,
        f.ascending,
        f.limit,
        f.offset,
        f.assignee_id,
        f.reviewer_id
    )
    .fetch_all(e)
    .await
}

// ------------------------------------------------------------------ stacks (ADR-0019)

#[derive(Debug, Clone)]
pub struct Stack {
    pub id: i64,
    pub project_id: i64,
    pub trunk: String,
}

pub async fn create_stack(e: impl PgExecutor<'_>, project_id: i64, trunk: &str) -> Result<i64> {
    sqlx::query_scalar!("INSERT INTO merge_request_stacks (project_id, trunk) VALUES ($1, $2) RETURNING id", project_id, trunk)
        .fetch_one(e)
        .await
}

pub async fn find_stack(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Stack>> {
    sqlx::query_as!(Stack, "SELECT id, project_id, trunk FROM merge_request_stacks WHERE id = $1", id).fetch_optional(e).await
}

pub async fn delete_stack(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM merge_request_stacks WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// Put an MR in a stack (None takes it out), with the base its own commits start after.
pub async fn set_stack(e: impl PgExecutor<'_>, mr_id: i64, stack_id: Option<i64>, base_sha: Option<&str>) -> Result<()> {
    sqlx::query!("UPDATE merge_requests SET stack_id = $2, stack_base_sha = $3 WHERE id = $1", mr_id, stack_id, base_sha)
        .execute(e)
        .await?;
    Ok(())
}

pub async fn set_stack_base(e: impl PgExecutor<'_>, mr_id: i64, base_sha: &str) -> Result<()> {
    sqlx::query!("UPDATE merge_requests SET stack_base_sha = $2 WHERE id = $1", mr_id, base_sha).execute(e).await?;
    Ok(())
}

pub async fn set_target(e: impl PgExecutor<'_>, mr_id: i64, target: &str) -> Result<()> {
    sqlx::query!("UPDATE merge_requests SET target_branch = $2, updated_at = now() WHERE id = $1", mr_id, target).execute(e).await?;
    Ok(())
}

/// Every MR of the given stacks (any state), oldest first.
pub async fn stack_members(e: impl PgExecutor<'_>, stack_ids: &[i64]) -> Result<Vec<MergeRequest>> {
    sqlx::query_as!(
        MergeRequest,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.author_id, u.username AS "author_username?",
                  m.source_branch, m.target_branch, m.state, m.draft, m.squash, m.merge_commit_sha, m.merged_by,
                  m.merged_at, m.closed_at, m.created_at, m.updated_at, m.imported_author, m.stack_id, m.stack_base_sha,
                  m.auto_merge_by, m.auto_merge_method, m.auto_merge_sha, m.auto_merge_remove_source, m.auto_merge_target, m.remove_source_branch,
                  m.milestone_id, ms.title AS "milestone_title?"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id LEFT JOIN milestones ms ON ms.id = m.milestone_id
           WHERE m.stack_id = ANY($1) ORDER BY m.id"#,
        stack_ids
    )
    .fetch_all(e)
    .await
}

/// Open MRs targeting `branch` (MRs stacked on a branch, or merging into it).
pub async fn open_with_target(e: impl PgExecutor<'_>, project_id: i64, branch: &str) -> Result<Vec<MergeRequest>> {
    sqlx::query_as!(
        MergeRequest,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.author_id, u.username AS "author_username?",
                  m.source_branch, m.target_branch, m.state, m.draft, m.squash, m.merge_commit_sha, m.merged_by,
                  m.merged_at, m.closed_at, m.created_at, m.updated_at, m.imported_author, m.stack_id, m.stack_base_sha,
                  m.auto_merge_by, m.auto_merge_method, m.auto_merge_sha, m.auto_merge_remove_source, m.auto_merge_target, m.remove_source_branch,
                  m.milestone_id, ms.title AS "milestone_title?"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id LEFT JOIN milestones ms ON ms.id = m.milestone_id
           WHERE m.project_id = $1 AND m.target_branch = $2 AND m.state = 'opened' ORDER BY m.id"#,
        project_id,
        branch
    )
    .fetch_all(e)
    .await
}

/// (iid, title, state) of a project's MRs among `iids`, for `!12` references.
pub async fn titles(e: impl PgExecutor<'_>, project_id: i64, iids: &[i64]) -> Result<Vec<(i64, String, String)>> {
    let rows = sqlx::query!("SELECT iid, title, state FROM merge_requests WHERE project_id = $1 AND iid = ANY($2)", project_id, iids)
        .fetch_all(e)
        .await?;
    Ok(rows.into_iter().map(|r| (r.iid, r.title, r.state)).collect())
}

/// Merge requests whose title or description may reference issue `#iid`
/// (the caller checks the Markdown), and those listed by iid (`also`).
pub async fn referencing_issue(
    e: impl PgExecutor<'_>,
    project_id: i64,
    iid: i64,
    also: &[i64],
) -> Result<Vec<(i64, String, String, String)>> {
    let rows = sqlx::query!(
        r#"SELECT iid, title, state, description FROM merge_requests
           WHERE project_id = $1
             AND (iid = ANY($3) OR (title || ' ' || description) ~ ('#' || $2::bigint::text || '([^[:digit:]]|$)'))
           ORDER BY iid"#,
        project_id,
        iid,
        also
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.iid, r.title, r.state, r.description)).collect())
}

/// Turn auto-merge on (with the reviewed head) or off (`by` None).
pub async fn set_auto_merge(
    e: impl PgExecutor<'_>,
    mr_id: i64,
    by: Option<i64>,
    method: Option<&str>,
    sha: Option<&str>,
    remove_source: bool,
    target: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE merge_requests SET auto_merge_by = $2, auto_merge_method = $3, auto_merge_sha = $4, auto_merge_remove_source = $5,
                auto_merge_target = $6
         WHERE id = $1",
        mr_id,
        by,
        method,
        sha,
        remove_source,
        target
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Open MRs waiting to auto-merge, (project_id, iid).
pub async fn auto_merge_pending(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<(i64, i64)>> {
    let rows = sqlx::query!(
        "SELECT project_id, iid FROM merge_requests WHERE auto_merge_by IS NOT NULL AND state = 'opened' ORDER BY updated_at LIMIT $1",
        limit
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.project_id, r.iid)).collect())
}

/// A stack retarget (not a person's edit) carries a pending auto-merge along.
pub async fn retarget_auto_merge(e: impl PgExecutor<'_>, mr_id: i64, target: &str) -> Result<()> {
    sqlx::query!("UPDATE merge_requests SET auto_merge_target = $2 WHERE id = $1 AND auto_merge_by IS NOT NULL", mr_id, target)
        .execute(e)
        .await?;
    Ok(())
}

pub async fn set_suggestion_applied(e: impl PgExecutor<'_>, note_id: i64, sha: &str) -> Result<()> {
    sqlx::query!("UPDATE notes SET suggestion_applied_sha = $2 WHERE id = $1", note_id, sha).execute(e).await?;
    Ok(())
}

pub async fn set_remove_source_branch(e: impl PgExecutor<'_>, mr_id: i64, remove: bool) -> Result<()> {
    sqlx::query!("UPDATE merge_requests SET remove_source_branch = $2 WHERE id = $1", mr_id, remove).execute(e).await?;
    Ok(())
}

/// Comments written by people (not system notes), per MR.
pub async fn user_notes_counts(e: impl PgExecutor<'_>, mr_ids: &[i64]) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query!(
        r#"SELECT merge_request_id, count(*) AS "n!" FROM notes WHERE merge_request_id = ANY($1) AND NOT system GROUP BY merge_request_id"#,
        mr_ids
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.merge_request_id, r.n))
    .collect())
}

/// Open merge requests of a project (the sidebar's count).
/// Merge requests per state (opened, merged, closed) with `list`'s filters.
pub async fn count_by_state(e: impl PgExecutor<'_>, project_id: i64, search: &str, people: PeopleFilter) -> Result<(i64, i64, i64)> {
    let r = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE m.state = 'opened') AS "opened!",
                  count(*) FILTER (WHERE m.state = 'merged') AS "merged!",
                  count(*) FILTER (WHERE m.state = 'closed') AS "closed!"
           FROM merge_requests m
           WHERE m.project_id = $1
             AND ($2 = '' OR m.title ILIKE '%' || $2 || '%' OR m.source_branch ILIKE '%' || $2 || '%'
                  OR m.iid::text = $2 OR ('!' || m.iid::text) = $2)
             AND ($3::bigint IS NULL OR m.author_id = $3)
             AND ($4::bigint IS NULL OR EXISTS (SELECT 1 FROM merge_request_assignees a WHERE a.merge_request_id = m.id AND a.user_id = $4))
             AND ($5::bigint IS NULL OR EXISTS (SELECT 1 FROM merge_request_reviewers r WHERE r.merge_request_id = m.id AND r.user_id = $5))
             AND ($6::bigint IS NULL OR ($6 = 0 AND m.milestone_id IS NULL) OR m.milestone_id = $6)"#,
        project_id,
        search,
        people.author_id,
        people.assignee_id,
        people.reviewer_id,
        people.milestone_id
    )
    .fetch_one(e)
    .await?;
    Ok((r.opened, r.merged, r.closed))
}
