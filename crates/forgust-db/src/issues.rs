//! Issues, their comments, and project labels (shared with merge requests).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

// ------------------------------------------------------------------ labels

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Label {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub description: String,
}

/// Labels with how many open issues and MRs use them.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LabelWithCounts {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub description: String,
    pub open_issues: i64,
    pub open_merge_requests: i64,
}

pub async fn labels(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<LabelWithCounts>> {
    sqlx::query_as!(
        LabelWithCounts,
        r#"SELECT l.id, l.name, l.color, l.description,
                  (SELECT count(*) FROM issue_labels il JOIN issues i ON i.id = il.issue_id
                    WHERE il.label_id = l.id AND i.state = 'opened') AS "open_issues!",
                  (SELECT count(*) FROM merge_request_labels ml JOIN merge_requests m ON m.id = ml.merge_request_id
                    WHERE ml.label_id = l.id AND m.state = 'opened') AS "open_merge_requests!"
           FROM labels l WHERE l.project_id = $1 ORDER BY lower(l.name)"#,
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn label(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<Option<Label>> {
    sqlx::query_as!(Label, "SELECT id, name, color, description FROM labels WHERE project_id = $1 AND id = $2", project_id, id)
        .fetch_optional(e)
        .await
}

pub async fn labels_by_name(e: impl PgExecutor<'_>, project_id: i64, names: &[String]) -> Result<Vec<Label>> {
    sqlx::query_as!(
        Label,
        "SELECT id, name, color, description FROM labels WHERE project_id = $1 AND name = ANY($2) ORDER BY lower(name)",
        project_id,
        names
    )
    .fetch_all(e)
    .await
}

/// Create a label, or update the color and description of the one with that name.
pub async fn upsert_label(e: impl PgExecutor<'_>, project_id: i64, name: &str, color: &str, description: &str) -> Result<Label> {
    sqlx::query_as!(
        Label,
        "INSERT INTO labels (project_id, name, color, description) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id, name) DO UPDATE SET color = EXCLUDED.color, description = EXCLUDED.description
         RETURNING id, name, color, description",
        project_id,
        name,
        color,
        description
    )
    .fetch_one(e)
    .await
}

pub async fn update_label(e: impl PgExecutor<'_>, id: i64, name: &str, color: &str, description: &str) -> Result<Label> {
    sqlx::query_as!(
        Label,
        "UPDATE labels SET name = $2, color = $3, description = $4 WHERE id = $1 RETURNING id, name, color, description",
        id,
        name,
        color,
        description
    )
    .fetch_one(e)
    .await
}

pub async fn delete_label(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM labels WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// Labels of each issue in `ids`, as (issue id, label).
pub async fn labels_of_issues(e: impl PgExecutor<'_>, ids: &[i64]) -> Result<Vec<(i64, Label)>> {
    let rows = sqlx::query!(
        "SELECT il.issue_id, l.id, l.name, l.color, l.description
         FROM issue_labels il JOIN labels l ON l.id = il.label_id
         WHERE il.issue_id = ANY($1) ORDER BY lower(l.name)",
        ids
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.issue_id, Label { id: r.id, name: r.name, color: r.color, description: r.description })).collect())
}

/// Labels of each merge request in `ids`, as (merge request id, label).
pub async fn labels_of_mrs(e: impl PgExecutor<'_>, ids: &[i64]) -> Result<Vec<(i64, Label)>> {
    let rows = sqlx::query!(
        "SELECT ml.merge_request_id, l.id, l.name, l.color, l.description
         FROM merge_request_labels ml JOIN labels l ON l.id = ml.label_id
         WHERE ml.merge_request_id = ANY($1) ORDER BY lower(l.name)",
        ids
    )
    .fetch_all(e)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.merge_request_id, Label { id: r.id, name: r.name, color: r.color, description: r.description }))
        .collect())
}

/// One item's labels replaced (issues, merge requests); the SQL per table is
/// spelled out (compile-checked), the body written once.
macro_rules! set_labels {
    ($(#[$doc:meta])* $name:ident, $delete:literal, $insert:literal) => {
        $(#[$doc])*
        pub async fn $name(conn: &mut sqlx::PgConnection, id: i64, label_ids: &[i64]) -> Result<()> {
            sqlx::query!($delete, id, label_ids).execute(&mut *conn).await?;
            sqlx::query!($insert, id, label_ids).execute(&mut *conn).await?;
            Ok(())
        }
    };
}

set_labels!(
    /// Replace the labels of an issue.
    set_issue_labels,
    "DELETE FROM issue_labels WHERE issue_id = $1 AND NOT (label_id = ANY($2))",
    "INSERT INTO issue_labels (issue_id, label_id) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING"
);

set_labels!(
    /// Replace the labels of a merge request.
    set_mr_labels,
    "DELETE FROM merge_request_labels WHERE merge_request_id = $1 AND NOT (label_id = ANY($2))",
    "INSERT INTO merge_request_labels (merge_request_id, label_id) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING"
);

// ------------------------------------------------------------------ issues

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Issue {
    pub id: i64,
    pub project_id: i64,
    pub iid: i64,
    pub title: String,
    pub description: String,
    pub author_id: Option<i64>,
    pub author_username: Option<String>,
    pub author_name: Option<String>,
    /// Who wrote it on the forge it was imported from, when they have no account here.
    pub imported_author: Option<String>,
    pub state: String,
    pub closed_by: Option<i64>,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Comments written by people (not system notes).
    pub notes_count: i64,
    pub milestone_id: Option<i64>,
    pub milestone_title: Option<String>,
    pub due_date: Option<chrono::NaiveDate>,
}

pub struct NewIssue<'a> {
    pub project_id: i64,
    pub iid: i64,
    pub title: &'a str,
    pub description: &'a str,
    pub author_id: Option<i64>,
    pub imported_author: Option<&'a str>,
    pub state: &'a str,
    /// Imported issues keep their dates; new ones get now().
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub milestone_id: Option<i64>,
    pub due_date: Option<chrono::NaiveDate>,
}

pub async fn insert(e: impl PgExecutor<'_>, i: NewIssue<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO issues (project_id, iid, title, description, author_id, imported_author, state, created_at, updated_at, closed_at,
                             milestone_id, due_date)
         VALUES ($1, $2, $3, $4, $5, $6, $7, coalesce($8, now()), coalesce($9, $8, now()), $10, $11, $12) RETURNING id",
        i.project_id,
        i.iid,
        i.title,
        i.description,
        i.author_id,
        i.imported_author,
        i.state,
        i.created_at,
        i.updated_at,
        i.closed_at,
        i.milestone_id,
        i.due_date
    )
    .fetch_one(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, project_id: i64, iid: i64) -> Result<Option<Issue>> {
    sqlx::query_as!(
        Issue,
        r#"SELECT i.id, i.project_id, i.iid, i.title, i.description, i.author_id, u.username AS "author_username?",
                  u.name AS "author_name?", i.imported_author, i.state, i.closed_by, i.closed_at, i.created_at, i.updated_at,
                  (SELECT count(*) FROM issue_notes n WHERE n.issue_id = i.id AND NOT n.system) AS "notes_count!",
                  i.milestone_id, ms.title AS "milestone_title?", i.due_date
           FROM issues i LEFT JOIN users u ON u.id = i.author_id LEFT JOIN milestones ms ON ms.id = i.milestone_id
           WHERE i.project_id = $1 AND i.iid = $2"#,
        project_id,
        iid
    )
    .fetch_optional(e)
    .await
}

pub struct ListFilter<'a> {
    /// opened / closed / all
    pub state: &'a str,
    pub search: &'a str,
    /// Only issues with this label ('' for any).
    pub label: &'a str,
    /// Only issues assigned to this user.
    pub assignee_id: Option<i64>,
    pub author_id: Option<i64>,
    /// Only issues on this milestone (`Some(0)`: on none).
    pub milestone_id: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

pub async fn list(e: impl PgExecutor<'_>, project_id: i64, f: ListFilter<'_>) -> Result<Vec<Issue>> {
    sqlx::query_as!(
        Issue,
        r#"SELECT i.id, i.project_id, i.iid, i.title, i.description, i.author_id, u.username AS "author_username?",
                  u.name AS "author_name?", i.imported_author, i.state, i.closed_by, i.closed_at, i.created_at, i.updated_at,
                  (SELECT count(*) FROM issue_notes n WHERE n.issue_id = i.id AND NOT n.system) AS "notes_count!",
                  i.milestone_id, ms.title AS "milestone_title?", i.due_date
           FROM issues i LEFT JOIN users u ON u.id = i.author_id LEFT JOIN milestones ms ON ms.id = i.milestone_id
           WHERE i.project_id = $1 AND ($2 = 'all' OR i.state = $2)
             AND ($3 = '' OR i.title ILIKE '%' || $3 || '%' OR ('#' || i.iid::text) = $3 OR i.iid::text = $3)
             AND ($4 = '' OR EXISTS (SELECT 1 FROM issue_labels il JOIN labels l ON l.id = il.label_id
                                     WHERE il.issue_id = i.id AND l.name = $4))
             AND ($7::bigint IS NULL OR EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id AND a.user_id = $7))
             AND ($8::bigint IS NULL OR i.author_id = $8)
             AND ($9::bigint IS NULL OR ($9 = 0 AND i.milestone_id IS NULL) OR i.milestone_id = $9)
           ORDER BY i.updated_at DESC LIMIT $5 OFFSET $6"#,
        project_id,
        f.state,
        f.search,
        f.label,
        f.limit,
        f.offset,
        f.assignee_id,
        f.author_id,
        f.milestone_id
    )
    .fetch_all(e)
    .await
}

/// Open and closed issue counts, for the list tabs.
pub async fn counts(e: impl PgExecutor<'_>, project_id: i64) -> Result<(i64, i64)> {
    let r = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE state = 'opened') AS "opened!", count(*) FILTER (WHERE state = 'closed') AS "closed!"
           FROM issues WHERE project_id = $1"#,
        project_id
    )
    .fetch_one(e)
    .await?;
    Ok((r.opened, r.closed))
}

pub async fn update_details(e: impl PgExecutor<'_>, id: i64, title: &str, description: &str) -> Result<()> {
    sqlx::query!("UPDATE issues SET title = $2, description = $3, updated_at = now() WHERE id = $1", id, title, description)
        .execute(e)
        .await?;
    Ok(())
}

pub async fn set_state(e: impl PgExecutor<'_>, id: i64, open: bool, by: Option<i64>) -> Result<()> {
    sqlx::query!(
        "UPDATE issues SET state = CASE WHEN $2::boolean THEN 'opened' ELSE 'closed' END,
                closed_by = CASE WHEN $2 THEN NULL ELSE $3::bigint END,
                closed_at = CASE WHEN $2 THEN NULL ELSE now() END,
                updated_at = now()
         WHERE id = $1",
        id,
        open,
        by
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn touch(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE issues SET updated_at = now() WHERE id = $1", id).execute(e).await?;
    Ok(())
}

// ------------------------------------------------------------------ comments

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct IssueNote {
    pub id: i64,
    pub issue_id: i64,
    pub author_id: Option<i64>,
    pub author_username: Option<String>,
    pub author_name: Option<String>,
    pub imported_author: Option<String>,
    pub body: String,
    pub system: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct NewIssueNote<'a> {
    pub issue_id: i64,
    pub author_id: Option<i64>,
    pub imported_author: Option<&'a str>,
    pub body: &'a str,
    pub system: bool,
    pub created_at: Option<DateTime<Utc>>,
}

pub async fn add_note(e: impl PgExecutor<'_>, n: NewIssueNote<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO issue_notes (issue_id, author_id, imported_author, body, system, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, coalesce($6, now()), coalesce($6, now())) RETURNING id",
        n.issue_id,
        n.author_id,
        n.imported_author,
        n.body,
        n.system,
        n.created_at
    )
    .fetch_one(e)
    .await
}

pub async fn notes(e: impl PgExecutor<'_>, issue_id: i64) -> Result<Vec<IssueNote>> {
    sqlx::query_as!(
        IssueNote,
        r#"SELECT n.id, n.issue_id, n.author_id, u.username AS "author_username?", u.name AS "author_name?", n.imported_author,
                  n.body, n.system, n.created_at, n.updated_at
           FROM issue_notes n LEFT JOIN users u ON u.id = n.author_id
           WHERE n.issue_id = $1 ORDER BY n.created_at, n.id"#,
        issue_id
    )
    .fetch_all(e)
    .await
}

pub async fn note(e: impl PgExecutor<'_>, issue_id: i64, id: i64) -> Result<Option<IssueNote>> {
    sqlx::query_as!(
        IssueNote,
        r#"SELECT n.id, n.issue_id, n.author_id, u.username AS "author_username?", u.name AS "author_name?", n.imported_author,
                  n.body, n.system, n.created_at, n.updated_at
           FROM issue_notes n LEFT JOIN users u ON u.id = n.author_id
           WHERE n.issue_id = $1 AND n.id = $2"#,
        issue_id,
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn edit_note(e: impl PgExecutor<'_>, id: i64, body: &str) -> Result<()> {
    sqlx::query!("UPDATE issue_notes SET body = $2, updated_at = now() WHERE id = $1", id, body).execute(e).await?;
    Ok(())
}

pub async fn delete_note(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM issue_notes WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// (iid, title, state) of a project's issues among `iids`, for `#12` references.
pub async fn titles(e: impl PgExecutor<'_>, project_id: i64, iids: &[i64]) -> Result<Vec<(i64, String, String)>> {
    let rows =
        sqlx::query!("SELECT iid, title, state FROM issues WHERE project_id = $1 AND iid = ANY($2)", project_id, iids).fetch_all(e).await?;
    Ok(rows.into_iter().map(|r| (r.iid, r.title, r.state)).collect())
}

/// People taking part in an issue: its author, assignees and commenters.
pub async fn participants(e: impl PgExecutor<'_>, issue_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        r#"SELECT DISTINCT u AS "u!" FROM (
               SELECT author_id AS u FROM issues WHERE id = $1
               UNION SELECT author_id FROM issue_notes WHERE issue_id = $1 AND NOT system
               UNION SELECT user_id FROM issue_assignees WHERE issue_id = $1
           ) x WHERE u IS NOT NULL"#,
        issue_id
    )
    .fetch_all(e)
    .await
}
