//! Environments and deployments: a job with `environment:` deploys to
//! it (`action: start`, the default) or stops it (`action: stop`); the
//! deployment follows the job's status. Protected environments say who may
//! deploy; `auto_stop_in` stops an environment later through its `on_stop` job.

use anyhow::Context as _;
use forgust_ci_config::job::Environment as EnvSpec;
use forgust_db::ci::Build;
use forgust_db::environments as edb;
use forgust_db::projects::Project;
use sha2::Digest;
use sqlx::PgConnection;

use crate::ci::pipeline::StoredSpec;
use crate::state::AppState;

/// GitLab's `CI_ENVIRONMENT_SLUG`: lower case, DNS-safe, at most 24
/// characters, starting with a letter; a short hash keeps altered names apart.
pub fn slug(name: &str) -> String {
    let mut s: String = name.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let mut s = s.trim_matches('-').to_string();
    if !s.starts_with(|c: char| c.is_ascii_lowercase()) {
        s.insert_str(0, "env-");
    }
    if s == name && s.len() <= 24 {
        return s;
    }
    let hash = hex::encode(sha2::Sha256::digest(name.as_bytes()));
    s.truncate(17);
    format!("{}-{}", s.trim_end_matches('-'), &hash[..6])
}

/// GitLab's guess when `deployment_tier` is not set.
pub fn tier_of(env: &EnvSpec) -> String {
    if let Some(t) = &env.deployment_tier {
        return t.clone();
    }
    let n = env.name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["prod", "live"]) {
        "production"
    } else if has(&["stag", "model", "demo"]) {
        "staging"
    } else if has(&["test", "tst", "int", "qa", "acc"]) {
        "testing"
    } else if has(&["dev", "review", "trunk"]) {
        "development"
    } else {
        "other"
    }
    .into()
}

/// A job with `environment:` was created: the environment exists from now
/// on, and a deploying job gets its deployment.
#[allow(clippy::too_many_arguments)]
pub async fn on_job_created(
    tx: &mut PgConnection,
    project: &Project,
    build_id: i64,
    env: &EnvSpec,
    git_ref: &str,
    tag: bool,
    sha: &str,
    user_id: Option<i64>,
) -> anyhow::Result<()> {
    if env.action == "stop" || env.action == "access" || env.action == "verify" {
        return Ok(());
    }
    let e = edb::ensure(&mut *tx, project.id, &env.name, &slug(&env.name), &tier_of(env)).await?;
    if env.action == "start" {
        let iid = forgust_db::merge_requests::next_iid(&mut *tx, project.id, "deployments").await?;
        edb::create_deployment(
            &mut *tx,
            edb::NewDeployment { project_id: project.id, iid, environment_id: e.id, build_id, git_ref, tag, sha, user_id },
        )
        .await?;
    }
    Ok(())
}

/// A job changed status: its deployment follows; a stop job that succeeded
/// stops its environment.
pub async fn on_job_status(state: &AppState, b: &Build, status: &str) -> anyhow::Result<()> {
    let Ok(spec) = StoredSpec::of(b) else { return Ok(()) };
    let Some(env) = spec.job.environment else { return Ok(()) };
    match env.action.as_str() {
        "start" => {
            let Some(d) = edb::of_build(&state.db, b.id).await? else { return Ok(()) };
            let dstatus = match status {
                "running" => "running",
                "success" => "success",
                "failed" => "failed",
                "canceled" => "canceled",
                "skipped" => "skipped",
                "manual" | "scheduled" | "waiting_for_resource" => "blocked",
                _ => return Ok(()),
            };
            edb::set_deployment_status(&state.db, d.id, dstatus).await?;
            if status == "success" {
                let stop_build = match &env.on_stop {
                    Some(name) => forgust_db::ci::builds_of_pipeline(&state.db, b.pipeline_id)
                        .await?
                        .into_iter()
                        .filter(|x| &x.name == name && !x.retried)
                        .map(|x| x.id)
                        .next_back(),
                    None => None,
                };
                let auto_stop_at = env
                    .auto_stop_in
                    .as_deref()
                    .and_then(crate::ci::pipeline::parse_duration)
                    .map(|s| chrono::Utc::now() + chrono::Duration::seconds(s));
                // a link people click: http(s) only (not `javascript:` or `data:`)
                let url = env.url.as_deref().filter(|u| {
                    let l = u.trim().to_ascii_lowercase();
                    l.starts_with("https://") || l.starts_with("http://")
                });
                edb::deployed(&state.db, d.environment_id, url, stop_build, auto_stop_at).await?;
            }
        }
        "stop" if status == "success" => {
            if let Some(e) = edb::by_name(&state.db, b.project_id, &env.name).await? {
                edb::set_state(&state.db, e.id, "stopped").await?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether the job's user may deploy to its environment (protected
/// environments; admins always may).
pub async fn may_deploy(state: &AppState, b: &Build) -> anyhow::Result<bool> {
    let Some(env) = StoredSpec::of(b).ok().and_then(|s| s.job.environment) else { return Ok(true) };
    let rules = edb::protected(&state.db, b.project_id).await?;
    let matching: Vec<_> = rules.iter().filter(|r| r.matches(&env.name)).collect();
    if matching.is_empty() {
        return Ok(true);
    }
    let Some(user_id) = b.user_id else { return Ok(false) };
    let Some(user) = forgust_db::users::find(&state.db, user_id).await? else { return Ok(false) };
    if user.is_admin {
        return Ok(true);
    }
    let level = forgust_db::authz::project_level(&state.db, user.id, b.project_id).await?.unwrap_or(0);
    Ok(matching.iter().all(|r| level >= r.deploy_access_level))
}

/// Stop an environment: run its `on_stop` job (as `user_id`), or mark it
/// stopped when it has none.
pub async fn stop(state: &AppState, project_id: i64, env_id: i64, user_id: Option<i64>) -> anyhow::Result<()> {
    let env = edb::find(&state.db, project_id, env_id).await?.context("environment vanished")?;
    if env.state == "stopped" {
        return Ok(());
    }
    if let Some(stop_id) = env.stop_build_id
        && let Some(b) = forgust_db::ci::build(&state.db, stop_id).await?
    {
        match b.status.as_str() {
            "manual" => {
                edb::set_state(&state.db, env.id, "stopping").await?;
                return crate::ci::pipeline::play_job(state, &b, user_id).await;
            }
            s if crate::ci::pipeline::is_complete(s) => {
                edb::set_state(&state.db, env.id, "stopping").await?;
                crate::ci::pipeline::retry_job(state, &b, user_id).await?;
                return Ok(());
            }
            _ => {}
        }
    }
    edb::set_state(&state.db, env.id, "stopped").await?;
    Ok(())
}

/// Environments whose `auto_stop_in` has passed.
pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    for (project_id, id) in edb::due_auto_stop(&state.db).await? {
        if let Err(e) = stop(state, project_id, id, None).await {
            tracing::warn!(project_id, environment = id, error = %e, "environment not stopped");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_tiers() {
        assert_eq!(slug("production"), "production");
        assert_eq!(slug("review/feature-x"), format!("review-feature-x-{}", &hex::encode(sha2::Sha256::digest(b"review/feature-x"))[..6]));
        assert!(slug("Review/A Very Long Branch Name Indeed").len() <= 24);
        assert!(slug("123").starts_with("env-"));
        let env = |n: &str| EnvSpec { name: n.into(), action: "start".into(), ..Default::default() };
        assert_eq!(tier_of(&env("production")), "production");
        assert_eq!(tier_of(&env("review/x")), "development");
        assert_eq!(tier_of(&env("staging")), "staging");
        assert_eq!(tier_of(&env("box")), "other");
    }
}
