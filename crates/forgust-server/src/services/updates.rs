//! New Forgust releases: the worker asks GitHub for the latest release every
//! 6 hours, at start and when the check is turned on (Admin > Settings turns it
//! off, an empty `server.release_check_url` too), and administrators see it when
//! it is newer than this one.

use std::cmp::Ordering;
use std::time::Duration;

use semver::Version;
use serde::Deserialize;

use crate::state::AppState;

/// Whether this instance asks for new releases at all.
pub async fn enabled(state: &AppState) -> anyhow::Result<bool> {
    Ok(!state.cfg.server.release_check_url.is_empty() && forgust_db::settings::update_check_enabled(&state.db).await?)
}

/// Fetch and keep the latest release, if the check is on.
pub async fn check(state: &AppState) -> anyhow::Result<()> {
    if !enabled(state).await? {
        return Ok(());
    }
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        html_url: Option<String>,
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(15)).build()?;
    let release: Release = client
        .get(&state.cfg.server.release_check_url)
        .header("user-agent", concat!("Forgust/", env!("CARGO_PKG_VERSION")))
        .header("accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let version = parse(&release.tag_name).ok_or_else(|| anyhow::anyhow!("unexpected release tag {:?}", release.tag_name))?;
    // the page administrators open: a web address only
    let url = release.html_url.filter(|u| u.starts_with("https://"));
    forgust_db::settings::set_latest_release(&state.db, &version.to_string(), url.as_deref()).await?;
    Ok(())
}

/// Check soon, in the background (at start, when turned on): a failure is only logged.
pub async fn queue_check(state: &AppState) -> anyhow::Result<()> {
    if !enabled(state).await? {
        return Ok(());
    }
    let job = crate::jobs::JobRequest {
        task: "update_check",
        payload: serde_json::json!({ "report": false }),
        job_key: Some("update_check".into()),
        max_attempts: Some(1),
        ..Default::default()
    };
    let mut conn = state.db.acquire().await?;
    state.jobs.enqueue(&mut conn, job).await
}

fn parse(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// `latest`, if it is newer than `current` (build metadata, `+…`, does not count).
pub fn newer(current: &str, latest: &str) -> Option<Version> {
    let (current, latest) = (parse(current)?, parse(latest)?);
    (latest.cmp_precedence(&current) == Ordering::Greater).then_some(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_newer_releases_count() {
        assert_eq!(newer("0.1.0", "v0.2.0").map(|v| v.to_string()), Some("0.2.0".into()));
        assert_eq!(newer("0.1.0", "0.1.1").map(|v| v.to_string()), Some("0.1.1".into()));
        assert_eq!(newer("0.9.0", "v0.10.0").map(|v| v.to_string()), Some("0.10.0".into()));
        assert_eq!(newer("1.0.0-rc.1", "v1.0.0").map(|v| v.to_string()), Some("1.0.0".into()));
        assert!(newer("0.2.0", "v0.2.0").is_none());
        assert!(newer("0.3.0", "v0.2.0").is_none());
        assert!(newer("0.1.0", "nightly").is_none());
        // the same release, built differently
        assert!(newer("0.2.0", "v0.2.0+build.1").is_none());
        assert!(newer("0.2.0+build.1", "v0.2.0").is_none());
        assert_eq!(newer("0.2.0+build.1", "v0.2.1").map(|v| v.to_string()), Some("0.2.1".into()));
    }
}
