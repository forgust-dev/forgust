//! Pipeline schedules: standard 5-field cron in a time zone. A schedule that
//! missed slots (server down) runs once, then continues from now.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use croner::Cron;

use crate::state::AppState;

/// Parse and check a cron expression and time zone.
pub fn validate(cron: &str, tz: &str) -> Result<(), String> {
    if cron.split_whitespace().count() != 5 {
        return Err("use a 5-field cron expression (minute hour day month weekday)".into());
    }
    Cron::from_str(cron).map_err(|e| format!("invalid cron expression: {e}"))?;
    chrono_tz::Tz::from_str(tz).map_err(|_| format!("unknown time zone `{tz}`"))?;
    Ok(())
}

/// The first slot strictly after `after`.
pub fn next_run(cron: &str, tz: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let c = Cron::from_str(cron).ok()?;
    let zone = chrono_tz::Tz::from_str(tz).ok()?;
    let local = after.with_timezone(&zone);
    c.find_next_occurrence(&local, false).ok().map(|t| t.with_timezone(&Utc))
}

/// Run every due schedule once (called each minute by the CI sweep).
pub async fn run_due(state: &AppState) -> anyhow::Result<()> {
    loop {
        let mut tx = state.db.begin().await?;
        let due = forgust_db::ci::due_schedules(&mut tx, 20).await?;
        if due.is_empty() {
            return Ok(());
        }
        let now = Utc::now();
        // move every claimed schedule forward first: a slow pipeline creation
        // never makes another worker pick the same slot
        for s in &due {
            forgust_db::ci::schedule_ran(&mut *tx, s.id, next_run(&s.cron, &s.cron_timezone, now), None).await?;
        }
        tx.commit().await?;
        for s in due {
            match run(state, &s).await {
                Ok(Some(pid)) => forgust_db::ci::schedule_ran(&state.db, s.id, next_run(&s.cron, &s.cron_timezone, now), Some(pid)).await?,
                Ok(None) => {}
                Err(e) => tracing::warn!(schedule = s.id, project_id = s.project_id, error = %e, "scheduled pipeline not created"),
            }
        }
    }
}

/// Create the schedule's pipeline now, as its owner. None when nothing ran.
/// A schedule whose owner is gone, blocked or no longer allowed to run
/// pipelines for its ref is turned off (as in GitLab): a Maintainer takes it
/// over to turn it on again.
pub async fn run(state: &AppState, s: &forgust_db::ci::Schedule) -> anyhow::Result<Option<i64>> {
    let Some(project) = forgust_db::projects::find(&state.db, s.project_id).await? else { return Ok(None) };
    let user = match s.owner_id {
        Some(owner) => forgust_db::users::find(&state.db, owner).await?,
        None => None,
    };
    let Some(user) = user.filter(|u| u.is_active()) else {
        forgust_db::ci::deactivate_schedule(&state.db, s.id).await?;
        anyhow::bail!("the schedule has no active owner: turned off");
    };
    let Some((sha, tag)) = crate::ci::resolve_ref(state, project.id, &s.git_ref).await? else {
        anyhow::bail!("ref {} does not exist", s.git_ref);
    };
    let principal = crate::auth::Principal::User { user: user.clone(), scopes: None };
    if !crate::ci::may_run(state, &principal, &project, &s.git_ref, tag).await? {
        forgust_db::ci::deactivate_schedule(&state.db, s.id).await?;
        anyhow::bail!("the owner can no longer run pipelines for {}: turned off", s.git_ref);
    }
    let variables: Vec<(String, String)> = match &s.variables_enc {
        Some(enc) => state.secrets.decrypt_json(enc)?,
        None => Vec::new(),
    };
    let created = crate::ci::pipeline::create(
        state,
        crate::ci::pipeline::Trigger {
            tag,
            schedule_id: Some(s.id),
            variables,
            ..crate::ci::pipeline::Trigger::new(project.id, &s.git_ref, &sha, "schedule", Some(user.id))
        },
    )
    .await?;
    Ok(created.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_in_a_time_zone() {
        assert!(validate("0 3 * * 1-5", "Europe/Paris").is_ok());
        assert!(validate("0 0 3 * * *", "UTC").is_err(), "no seconds field");
        assert!(validate("61 * * * *", "UTC").is_err());
        assert!(validate("0 3 * * *", "Mars/Olympus").is_err());
        // 03:00 in Paris (UTC+1 in November) is 02:00 UTC
        let after = DateTime::parse_from_rfc3339("2026-11-09T12:00:00Z").unwrap().with_timezone(&Utc);
        let next = next_run("0 3 * * *", "Europe/Paris", after).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-11-10T02:00:00+00:00");
    }
}
