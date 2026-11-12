//! What CI keywords do when a job ends: the resource group passes to
//! the next job, `coverage` is read from the log, `release:` makes the
//! release, `workflow:auto_cancel:on_job_failure` cancels the rest.

use std::collections::HashMap;

use forgust_db::ci::Build;

use crate::auth::Principal;
use crate::ci::pipeline::StoredSpec;
use crate::state::AppState;

/// Called by `finish_job` once the job's final status is recorded.
pub async fn after_job(state: &AppState, b: &Build, status: &str) {
    if let Err(e) = after_job_inner(state, b, status).await {
        tracing::warn!(job = b.id, error = %e, "CI keywords after the job");
    }
}

/// A job of a resource group ended (or was canceled): if the group is free,
/// the job that waited longest goes.
pub async fn release_resource(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    if let Some((project_id, group)) = forgust_db::ci::resource_group_of(&state.db, build_id).await?
        && !forgust_db::ci::resource_busy(&state.db, project_id, &group, 0).await?
        && let Some((next, _)) = forgust_db::ci::next_waiting(&state.db, project_id, &group).await?
    {
        crate::ci::pipeline::start_waiting(state, next).await?;
    }
    Ok(())
}

async fn after_job_inner(state: &AppState, b: &Build, status: &str) -> anyhow::Result<()> {
    release_resource(state, b.id).await?;
    let Ok(spec) = StoredSpec::of(b) else { return Ok(()) };
    if let Some(re) = &spec.job.coverage
        && (status == "success" || status == "failed")
        && let Err(e) = coverage(state, b.id, re).await
    {
        tracing::warn!(job = b.id, error = %e, "coverage not read");
    }
    if status == "success"
        && let Some(pg) = &spec.job.pages
        && let Err(e) = crate::pages::deploy(state, b, &pg.publish).await
    {
        tracing::warn!(job = b.id, error = %e, "Pages not deployed");
    }
    if status == "success"
        && let Some(r) = &spec.job.release
        && let Err(e) = release(state, b, &spec, r).await
    {
        tracing::warn!(job = b.id, error = %e, "release not created");
    }
    if status == "failed" {
        let fresh = forgust_db::ci::build(&state.db, b.id).await?;
        let blocking = fresh.is_some_and(|f| !f.allow_failure);
        if blocking
            && let Some((_, mode)) = forgust_db::ci::pipeline_extras(&state.db, b.pipeline_id).await?
            && mode == "all"
        {
            crate::ci::pipeline::cancel(state, b.pipeline_id).await?;
        }
    }
    Ok(())
}

/// GitLab's `coverage: '/regex/'`: the last match in the log (its last
/// capture group when it has groups) gives the percentage, the first number in it.
pub fn coverage_of(log: &str, pattern: &str) -> Option<f64> {
    let p = pattern.trim();
    let p = p.strip_prefix('/').and_then(|x| x.strip_suffix('/')).unwrap_or(p);
    let re = regex::RegexBuilder::new(p).size_limit(1 << 20).build().ok()?;
    let number = regex::Regex::new(r"\d+(?:\.\d+)?").expect("valid");
    let caps = log.lines().rev().find_map(|l| re.captures(l))?;
    let text = (1..caps.len()).rev().find_map(|i| caps.get(i)).or_else(|| caps.get(0))?.as_str();
    number.find(text).and_then(|n| n.as_str().parse().ok())
}

async fn coverage(state: &AppState, build_id: i64, pattern: &str) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    // the end of the log is where tools print totals
    let from = (b.trace_size - (1 << 20)).max(0);
    let bytes = crate::ci::trace::read(state, &b, from, 1 << 20).await?;
    if let Some(c) = coverage_of(&String::from_utf8_lossy(&bytes), pattern) {
        forgust_db::ci::set_coverage(&state.db, b.id, c.clamp(0.0, 100.0)).await?;
    }
    Ok(())
}

/// The variables a job's `release:` fields may use: the pipeline's, then the
/// YAML ones; with the project and the job's user, loaded on the way.
async fn job_variables(
    state: &AppState,
    b: &Build,
    spec: &StoredSpec,
) -> anyhow::Result<(HashMap<String, String>, forgust_db::projects::Project, Option<forgust_db::users::User>)> {
    let pipeline = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await?.ok_or_else(|| anyhow::anyhow!("pipeline vanished"))?;
    let project = forgust_db::projects::find(&state.db, b.project_id).await?.ok_or_else(|| anyhow::anyhow!("project vanished"))?;
    let user = match b.user_id {
        Some(u) => forgust_db::users::find(&state.db, u).await?,
        None => None,
    };
    let settings = forgust_db::ci::settings(&state.db, b.project_id).await?;
    let info = crate::ci::variables::PipelineInfo {
        project: &project,
        git_ref: &pipeline.git_ref,
        tag: pipeline.tag,
        sha: &pipeline.sha,
        before_sha: &pipeline.before_sha,
        source: &pipeline.source,
        protected: pipeline.protected,
        pipeline: Some(&pipeline),
        config_path: &settings.ci_config_path,
        user: user.as_ref(),
        commit: None,
        mr: None,
    };
    let mut vars = crate::ci::rule_variables(state, &info, &[]).await?;
    for (k, v) in spec.global_variables.iter().chain(spec.job.variables.iter()) {
        vars.entry(k.clone()).or_insert_with(|| v.clone());
    }
    vars.insert("CI_JOB_ID".into(), b.id.to_string());
    vars.insert("CI_JOB_NAME".into(), b.name.clone());
    Ok((vars, project, user))
}

/// `release:`: made by Forgust when the job succeeds (no release-cli in the
/// image needed), as the job's user, from the pipeline's commit.
async fn release(state: &AppState, b: &Build, spec: &StoredSpec, r: &forgust_ci_config::job::Release) -> anyhow::Result<()> {
    let (vars, project, user) = job_variables(state, b, spec).await?;
    let x = |s: &str| crate::ci::variables::expand(s, &vars);
    let user = user.ok_or_else(|| anyhow::anyhow!("the job has no user"))?;
    let principal = Principal::User { user, scopes: None };
    let tag = x(&r.tag_name);
    let git_ref = r.git_ref.as_deref().map(x).unwrap_or_else(|| vars.get("CI_COMMIT_SHA").cloned().unwrap_or_default());
    let links: Vec<crate::services::releases::NewLink> = r
        .links
        .iter()
        .map(|l| crate::services::releases::NewLink {
            name: x(&l.name),
            url: x(&l.url),
            direct_asset_path: l.filepath.as_deref().map(x),
            link_type: l.link_type.clone(),
        })
        .collect();
    let released_at =
        r.released_at.as_deref().and_then(|t| chrono::DateTime::parse_from_rfc3339(&x(t)).ok()).map(|t| t.with_timezone(&chrono::Utc));
    let (name, description, message) = (r.name.as_deref().map(x), r.description.as_deref().map(x), r.tag_message.as_deref().map(x));
    crate::services::releases::create(
        state,
        &principal,
        &project,
        crate::services::releases::NewRelease {
            tag_name: &tag,
            name: name.as_deref(),
            description: description.as_deref(),
            git_ref: Some(&git_ref),
            tag_message: message.as_deref(),
            released_at,
            links: &links,
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_from_the_last_matching_line() {
        let log = "Coverage: 12%\nrunning\nTOTAL   120   18   85.00%\ndone\n";
        assert_eq!(coverage_of(log, r"/TOTAL.*\s(\d+(?:\.\d+)?)%/"), Some(85.0), "the last group, as GitLab");
        assert_eq!(coverage_of(log, r"/TOTAL.*%/"), Some(120.0), "no group: the first number of the match");
        assert_eq!(coverage_of(log, r"/\d+(?:\.\d+)?%/"), Some(85.0));
        assert_eq!(coverage_of(log, r"/Coverage: \d+%/"), Some(12.0));
        assert_eq!(coverage_of(log, "/nothing/"), None);
    }
}
