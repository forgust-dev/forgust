//! Notification levels (M7, ADR-0021).

use sqlx::PgExecutor;

use crate::Result;

pub const LEVELS: &[&str] = &["watch", "participating", "mention", "disabled"];

pub async fn global_level(e: impl PgExecutor<'_>, user_id: i64) -> Result<String> {
    sqlx::query_scalar!("SELECT notification_level FROM users WHERE id = $1", user_id).fetch_one(e).await
}

pub async fn set_global_level(e: impl PgExecutor<'_>, user_id: i64, level: &str) -> Result<()> {
    sqlx::query!("UPDATE users SET notification_level = $2 WHERE id = $1", user_id, level).execute(e).await?;
    Ok(())
}

/// The person's choice for the project (`None`: their default applies).
pub async fn project_level(e: impl PgExecutor<'_>, user_id: i64, project_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar!("SELECT level FROM notification_settings WHERE user_id = $1 AND project_id = $2", user_id, project_id)
        .fetch_optional(e)
        .await
}

/// `None` goes back to the default.
pub async fn set_project_level(e: impl PgExecutor<'_>, user_id: i64, project_id: i64, level: Option<&str>) -> Result<()> {
    match level {
        Some(l) => {
            sqlx::query!(
                "INSERT INTO notification_settings (user_id, project_id, level) VALUES ($1, $2, $3)
                 ON CONFLICT (user_id, project_id) DO UPDATE SET level = $3, updated_at = now()",
                user_id,
                project_id,
                l
            )
            .execute(e)
            .await?;
        }
        None => {
            sqlx::query!("DELETE FROM notification_settings WHERE user_id = $1 AND project_id = $2", user_id, project_id)
                .execute(e)
                .await?;
        }
    }
    Ok(())
}

/// Effective level of each of these people for the project.
pub async fn levels(e: impl PgExecutor<'_>, project_id: i64, user_ids: &[i64]) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query!(
        r#"SELECT u.id, coalesce(ns.level, u.notification_level) AS "level!"
             FROM users u LEFT JOIN notification_settings ns ON ns.user_id = u.id AND ns.project_id = $1
            WHERE u.id = ANY($2)"#,
        project_id,
        user_ids
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.id, r.level))
    .collect())
}

/// Who watches the project: chose `watch` for it, or watch by default and are
/// members (a default does not reach every project of the instance). Access is
/// checked again when sending.
pub async fn watchers(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        r#"SELECT u.id AS "id!" FROM users u
             LEFT JOIN notification_settings ns ON ns.user_id = u.id AND ns.project_id = $1
            WHERE ns.level = 'watch'
               OR (ns.level IS NULL AND u.notification_level = 'watch'
                   AND EXISTS (SELECT 1 FROM project_authorizations a WHERE a.user_id = u.id AND a.project_id = $1))"#,
        project_id
    )
    .fetch_all(e)
    .await
}

/// What a notification is about: each kind has its own switch per channel.
/// (The `notification_kind` domain in the database lists the same names.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Someone @mentioned them.
    Mention,
    /// They were assigned or asked to review.
    Assignment,
    /// New issues and merge requests, comments, closing, reopening, merging.
    Activity,
    /// A review of their merge request: approved, changes requested.
    Review,
    PipelineFailed,
    /// A pipeline passed where the one before failed.
    PipelineFixed,
    /// What a security scan found.
    Security,
}

impl Kind {
    /// Every kind, in the order settings show them.
    pub const ALL: [Kind; 7] =
        [Kind::Mention, Kind::Assignment, Kind::Activity, Kind::Review, Kind::PipelineFailed, Kind::PipelineFixed, Kind::Security];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Mention => "mention",
            Kind::Assignment => "assignment",
            Kind::Activity => "activity",
            Kind::Review => "review",
            Kind::PipelineFailed => "pipeline_failed",
            Kind::PipelineFixed => "pipeline_fixed",
            Kind::Security => "security",
        }
    }
}

/// Where a notification goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The bell in the app.
    Web,
    Email,
    /// A message on screen as it arrives, while the app is open (for what
    /// reaches the bell).
    Screen,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Web => "web",
            Channel::Email => "email",
            Channel::Screen => "screen",
        }
    }
}

/// Whether a kind goes to a channel for someone who chose nothing: everything
/// but the email for a pipeline that passes again.
pub fn default_on(kind: Kind, channel: Channel) -> bool {
    !(kind == Kind::PipelineFixed && channel == Channel::Email)
}

/// Whether `kind` goes to `channel`, from a person's own choices for that kind
/// (`(channel, enabled)` rows) or the default.
pub fn enabled<'a>(own: impl IntoIterator<Item = (&'a str, bool)>, kind: Kind, channel: Channel) -> bool {
    own.into_iter().find(|(c, _)| *c == channel.as_str()).map(|(_, on)| on).unwrap_or_else(|| default_on(kind, channel))
}

/// A person's own choices: (kind, channel, enabled).
pub async fn channels(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<(String, String, bool)>> {
    Ok(sqlx::query!(r#"SELECT kind AS "kind!: String", channel, enabled FROM notification_channels WHERE user_id = $1"#, user_id)
        .fetch_all(e)
        .await?
        .into_iter()
        .map(|r| (r.kind, r.channel, r.enabled))
        .collect())
}

pub async fn set_channel(e: impl PgExecutor<'_>, user_id: i64, kind: Kind, channel: Channel, enabled: bool) -> Result<()> {
    sqlx::query!(
        "INSERT INTO notification_channels (user_id, kind, channel, enabled) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, kind, channel) DO UPDATE SET enabled = $4, updated_at = now()",
        user_id,
        kind.as_str(),
        channel.as_str(),
        enabled
    )
    .execute(e)
    .await?;
    Ok(())
}

/// The choices these people made for one kind: (user, channel, enabled).
pub async fn channel_choices(e: impl PgExecutor<'_>, user_ids: &[i64], kind: Kind) -> Result<Vec<(i64, String, bool)>> {
    Ok(sqlx::query!(
        "SELECT user_id, channel, enabled FROM notification_channels WHERE user_id = ANY($1) AND kind = $2",
        user_ids,
        kind.as_str()
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.user_id, r.channel, r.enabled))
    .collect())
}

/// A notification in someone's inbox.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct Notification {
    pub id: i64,
    /// mention, assignment, activity, review, pipeline_failed, pipeline_fixed or security
    pub kind: String,
    pub project_id: i64,
    pub project_path: String,
    /// merge_request, issue, pipeline or security (the project's findings)
    pub target_type: String,
    /// Its number in the project (none for security).
    pub target_iid: Option<i64>,
    pub title: String,
    pub summary: String,
    pub actor_username: Option<String>,
    pub actor_name: Option<String>,
    pub read: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct NewNotification<'a> {
    pub project_id: i64,
    pub kind: Kind,
    pub target_type: &'a str,
    pub target_iid: Option<i64>,
    pub actor_id: Option<i64>,
    pub title: &'a str,
    pub summary: &'a str,
}

/// The same notification in the inbox of each of these people: (user, its id
/// there, when).
pub async fn add(
    e: impl PgExecutor<'_>,
    user_ids: &[i64],
    n: NewNotification<'_>,
) -> Result<Vec<(i64, i64, chrono::DateTime<chrono::Utc>)>> {
    Ok(sqlx::query!(
        "INSERT INTO notifications (user_id, project_id, kind, target_type, target_iid, actor_id, title, summary)
         SELECT u, $2, $3, $4, $5, $6, $7, $8 FROM unnest($1::bigint[]) u
         RETURNING user_id, id, created_at",
        user_ids,
        n.project_id,
        n.kind.as_str(),
        n.target_type,
        n.target_iid,
        n.actor_id,
        n.title,
        n.summary
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.user_id, r.id, r.created_at))
    .collect())
}

/// Someone's inbox, newest first: what they can still see (`listed_notifications`).
pub async fn inbox(e: impl PgExecutor<'_>, user_id: i64, limit: i64) -> Result<Vec<Notification>> {
    sqlx::query_as!(
        Notification,
        r#"SELECT n.id AS "id!", n.kind AS "kind!: String", n.project_id AS "project_id!", r.path AS project_path,
                  n.target_type AS "target_type!", n.target_iid, n.title AS "title!", n.summary AS "summary!",
                  u.username AS "actor_username?", u.name AS "actor_name?", n.read_at IS NOT NULL AS "read!", n.created_at AS "created_at!"
           FROM listed_notifications n
           JOIN routes r ON r.source_type = 'project' AND r.source_id = n.project_id
           LEFT JOIN users u ON u.id = n.actor_id
           WHERE n.user_id = $1
           ORDER BY n.created_at DESC, n.id DESC LIMIT $2"#,
        user_id,
        limit
    )
    .fetch_all(e)
    .await
}

/// How many are unread, up to 100 (the bell shows "99+" beyond).
pub async fn unread_count(e: impl PgExecutor<'_>, user_id: i64) -> Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM (SELECT 1 FROM listed_notifications WHERE user_id = $1 AND read_at IS NULL LIMIT 100) unread"#,
        user_id
    )
    .fetch_one(e)
    .await
}

/// Mark one (`id`, read already or not) or all of someone's notifications read;
/// returns how many there were.
pub async fn mark_read(e: impl PgExecutor<'_>, user_id: i64, id: Option<i64>) -> Result<u64> {
    Ok(sqlx::query!(
        "UPDATE notifications SET read_at = coalesce(read_at, now())
          WHERE user_id = $1 AND (($2::bigint IS NULL AND read_at IS NULL) OR id = $2)",
        user_id,
        id
    )
    .execute(e)
    .await?
    .rows_affected())
}

/// Drop what was read more than `days` ago, and what nobody read for three
/// times as long (housekeeping).
pub async fn prune(e: impl PgExecutor<'_>, days: i32) -> Result<u64> {
    Ok(sqlx::query!(
        "DELETE FROM notifications WHERE read_at < now() - make_interval(days => $1) OR created_at < now() - make_interval(days => 3 * $1)",
        days
    )
    .execute(e)
    .await?
    .rows_affected())
}
