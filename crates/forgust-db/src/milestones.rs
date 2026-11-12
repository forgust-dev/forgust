//! Milestones (M7): a project's named goals with dates; issues and merge requests
//! point at one.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Milestone {
    pub id: i64,
    pub project_id: i64,
    pub iid: i64,
    pub title: String,
    pub description: String,
    /// active or closed
    pub state: String,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub open_issues: i64,
    pub closed_issues: i64,
    pub open_merge_requests: i64,
    pub merged_merge_requests: i64,
}

/// `state`: active, closed or all. Active first by due date, then the rest.
pub async fn list(e: impl PgExecutor<'_>, project_id: i64, state: &str, search: &str) -> Result<Vec<Milestone>> {
    sqlx::query_as!(
        Milestone,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.state, m.start_date, m.due_date, m.created_at, m.updated_at,
                  (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id AND i.state = 'opened') AS "open_issues!",
                  (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id AND i.state = 'closed') AS "closed_issues!",
                  (SELECT count(*) FROM merge_requests r WHERE r.milestone_id = m.id AND r.state = 'opened') AS "open_merge_requests!",
                  (SELECT count(*) FROM merge_requests r WHERE r.milestone_id = m.id AND r.state = 'merged') AS "merged_merge_requests!"
           FROM milestones m
           WHERE m.project_id = $1 AND ($2 = 'all' OR m.state = $2)
             AND ($3 = '' OR m.title ILIKE '%' || $3 || '%' ESCAPE '\')
           ORDER BY m.state = 'closed', m.due_date ASC NULLS LAST, m.id DESC"#,
        project_id,
        state,
        search
    )
    .fetch_all(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<Option<Milestone>> {
    sqlx::query_as!(
        Milestone,
        r#"SELECT m.id, m.project_id, m.iid, m.title, m.description, m.state, m.start_date, m.due_date, m.created_at, m.updated_at,
                  (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id AND i.state = 'opened') AS "open_issues!",
                  (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id AND i.state = 'closed') AS "closed_issues!",
                  (SELECT count(*) FROM merge_requests r WHERE r.milestone_id = m.id AND r.state = 'opened') AS "open_merge_requests!",
                  (SELECT count(*) FROM merge_requests r WHERE r.milestone_id = m.id AND r.state = 'merged') AS "merged_merge_requests!"
           FROM milestones m WHERE m.project_id = $1 AND m.id = $2"#,
        project_id,
        id
    )
    .fetch_optional(e)
    .await
}

pub struct MilestoneFields<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
}

/// `None` when the title is taken in the project.
pub async fn create(conn: &mut sqlx::PgConnection, project_id: i64, f: MilestoneFields<'_>, state: &str) -> Result<Option<i64>> {
    if sqlx::query_scalar!("SELECT 1 FROM milestones WHERE project_id = $1 AND title = $2", project_id, f.title)
        .fetch_optional(&mut *conn)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    let iid = crate::merge_requests::next_iid(&mut *conn, project_id, "milestones").await?;
    sqlx::query_scalar!(
        "INSERT INTO milestones (project_id, iid, title, description, start_date, due_date, state) VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT DO NOTHING RETURNING id",
        project_id,
        iid,
        f.title,
        f.description,
        f.start_date,
        f.due_date,
        state
    )
    .fetch_optional(&mut *conn)
    .await
}

/// `false` when the new title is taken by another milestone.
pub async fn update(e: impl PgExecutor<'_>, id: i64, f: MilestoneFields<'_>, state: &str) -> Result<bool> {
    let r = sqlx::query!(
        "UPDATE milestones SET title = $2, description = $3, start_date = $4, due_date = $5, state = $6, updated_at = now()
         WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM milestones o WHERE o.project_id = milestones.project_id AND o.title = $2 AND o.id <> $1)",
        id,
        f.title,
        f.description,
        f.start_date,
        f.due_date,
        state
    )
    .execute(e)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Issues and merge requests on it lose their milestone.
pub async fn delete(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM milestones WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// Title → id of the project's milestones (imports, `milestone=` filters).
pub async fn ids_by_title(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<(String, i64)>> {
    Ok(sqlx::query!("SELECT title, id FROM milestones WHERE project_id = $1", project_id)
        .fetch_all(e)
        .await?
        .into_iter()
        .map(|r| (r.title, r.id))
        .collect())
}

pub async fn set_issue(e: impl PgExecutor<'_>, issue_id: i64, milestone_id: Option<i64>) -> Result<()> {
    sqlx::query!("UPDATE issues SET milestone_id = $2, updated_at = now() WHERE id = $1", issue_id, milestone_id).execute(e).await?;
    Ok(())
}

pub async fn set_issue_due_date(e: impl PgExecutor<'_>, issue_id: i64, due: Option<NaiveDate>) -> Result<()> {
    sqlx::query!("UPDATE issues SET due_date = $2, updated_at = now() WHERE id = $1", issue_id, due).execute(e).await?;
    Ok(())
}

pub async fn set_merge_request(e: impl PgExecutor<'_>, mr_id: i64, milestone_id: Option<i64>) -> Result<()> {
    sqlx::query!("UPDATE merge_requests SET milestone_id = $2, updated_at = now() WHERE id = $1", mr_id, milestone_id).execute(e).await?;
    Ok(())
}
