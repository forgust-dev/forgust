//! `trigger:` jobs: a bridge starts a downstream pipeline, a child of
//! the same project (`include:`) or a pipeline of another project
//! (`project:`), and with `strategy: depend` ends as that pipeline ends.

use anyhow::Context as _;
use forgust_ci_config::job::{ChildInclude, Trigger as TriggerSpec};
use forgust_db::ci::Build;
use futures::TryStreamExt;

use crate::auth::Principal;
use crate::ci::pipeline::{self, Created, StoredSpec, Upstream};
use crate::state::AppState;

/// Largest generated child config read from an artifact.
const MAX_ARTIFACT_CONFIG: u64 = 1 << 20;

/// Start the downstream pipeline of a pending bridge job (queued by `admit`).
pub async fn start(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    if b.status != "pending" || forgust_db::ci::downstream_of(&state.db, b.id).await?.is_some() {
        return Ok(());
    }
    let spec = StoredSpec::of(&b)?;
    let trigger = spec.job.trigger.clone().context("not a trigger job")?;
    match create_downstream(state, &b, &spec, &trigger).await {
        Ok(Some(downstream)) => {
            forgust_db::ci::link_downstream(&state.db, b.id, downstream).await?;
            let down = forgust_db::ci::pipeline(&state.db, downstream).await?.context("downstream pipeline vanished")?;
            // it may have ended at once (a config error): mirror that, as later changes
            set_status(state, &b, "running", None).await?;
            if !trigger.depend {
                pipeline::finish_job(state, b.id, "success", None, None).await?;
            } else if pipeline::is_complete(&down.status) {
                downstream_changed(state, downstream, &down.status).await?;
            }
        }
        Ok(None) => {
            set_status(state, &b, "running", None).await?;
            pipeline::finish_job(state, b.id, "failed", Some("downstream_pipeline_creation_failed"), None).await?;
        }
        Err(Refusal(reason)) => {
            set_status(state, &b, "running", None).await?;
            pipeline::finish_job(state, b.id, "failed", Some(reason), None).await?;
        }
    }
    Ok(())
}

/// Why a downstream pipeline cannot start (GitLab's failure reasons).
struct Refusal(&'static str);

impl<E: std::fmt::Display> From<E> for Refusal
where
    E: Into<anyhow::Error>,
{
    fn from(e: E) -> Self {
        tracing::warn!(error = %e, "downstream pipeline not created");
        Refusal("downstream_pipeline_creation_failed")
    }
}

async fn set_status(state: &AppState, b: &Build, status: &str, reason: Option<&str>) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    forgust_db::ci::set_build_status(&mut *tx, b.id, status, reason).await?;
    forgust_db::ci::mark_started(&mut *tx, b.id).await?;
    tx.commit().await?;
    crate::ci::hooks::announce(state, b, status).await;
    pipeline::process(state, b.pipeline_id).await
}

/// The variables a downstream pipeline gets: the bridge's YAML variables
/// (`forward:yaml_variables`, on by default) and the upstream pipeline's own
/// (`forward:pipeline_variables`).
async fn forwarded(state: &AppState, b: &Build, spec: &StoredSpec, t: &TriggerSpec) -> anyhow::Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    if t.forward_yaml_variables {
        // expanded here, as GitLab does: `$CI_PIPELINE_ID` is the upstream's
        // (`PARENT_PIPELINE_ID: $CI_PIPELINE_ID` for `needs:pipeline`)
        let mut known: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        if let Some(pl) = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await? {
            known.insert("CI_PIPELINE_ID".into(), pl.id.to_string());
            known.insert("CI_PIPELINE_IID".into(), pl.iid.to_string());
            known.insert("CI_COMMIT_SHA".into(), pl.sha.clone());
            known.insert("CI_COMMIT_REF_NAME".into(), pl.git_ref.clone());
        }
        if let Some(p) = forgust_db::projects::find(&state.db, b.project_id).await? {
            known.insert("CI_PROJECT_ID".into(), p.id.to_string());
            known.insert("CI_PROJECT_PATH".into(), p.full_path.clone());
        }
        known.insert("CI_JOB_ID".into(), b.id.to_string());
        for (k, v) in spec.global_variables.iter().chain(spec.job.variables.iter()) {
            let v = crate::ci::variables::expand_known(v, &known);
            known.insert(k.clone(), v.clone());
            out.retain(|(x, _)| x != k);
            out.push((k.clone(), v));
        }
    }
    if t.forward_pipeline_variables
        && let Some(enc) = forgust_db::ci::pipeline_variables(&state.db, b.pipeline_id).await?
    {
        let vars: Vec<(String, String)> = state.secrets.decrypt_json(&enc)?;
        for (k, v) in vars {
            out.retain(|(x, _)| *x != k);
            out.push((k, v));
        }
    }
    Ok(out)
}

/// A file another job of the pipeline left in its artifacts.
async fn artifact_file(state: &AppState, b: &Build, job: &str, path: &str) -> Result<String, Refusal> {
    let builds = forgust_db::ci::builds_of_pipeline(&state.db, b.pipeline_id).await?;
    let source = builds.iter().rev().find(|x| x.name == job && !x.retried).ok_or(Refusal("downstream_pipeline_creation_failed"))?;
    let archive = forgust_db::ci::artifacts_of_build(&state.db, source.id)
        .await?
        .into_iter()
        .find(|a| a.file_type == "archive")
        .ok_or(Refusal("downstream_pipeline_creation_failed"))?;
    let (entries, _) = crate::ci::archive::entries(state, &archive.blob_key).await?;
    let path = path.trim_start_matches("./").trim_start_matches('/');
    let e =
        entries.iter().find(|e| e.name == path && e.size <= MAX_ARTIFACT_CONFIG).ok_or(Refusal("downstream_pipeline_creation_failed"))?;
    let stream = crate::ci::archive::open(state, &archive.blob_key, e).await?;
    let bytes: Vec<u8> = stream.map_ok(|c| c.to_vec()).try_concat().await?;
    String::from_utf8(bytes).map_err(|_| Refusal("downstream_pipeline_creation_failed"))
}

/// A child pipeline's config: its includes, and a generated file from an
/// artifact as the document itself.
async fn child_config(state: &AppState, b: &Build, includes: &[ChildInclude]) -> Result<String, Refusal> {
    let mut main: Option<String> = None;
    let mut list = Vec::new();
    for inc in includes {
        match inc {
            ChildInclude::Local(p) => list.push(serde_json::json!({ "local": p })),
            ChildInclude::Project { project, file, git_ref } => {
                let mut v = serde_json::json!({ "project": project, "file": file });
                if let Some(r) = git_ref {
                    v["ref"] = serde_json::Value::String(r.clone());
                }
                list.push(v);
            }
            ChildInclude::Artifact { artifact, job } => {
                if main.is_some() {
                    return Err(Refusal("downstream_pipeline_creation_failed"));
                }
                main = Some(artifact_file(state, b, job, artifact).await?);
            }
        }
    }
    let doc = main.unwrap_or_default();
    if list.is_empty() {
        return Ok(doc);
    }
    // a generated file with its own `include:` would get two: refuse rather than guess
    if doc.lines().any(|l| l.starts_with("include:")) {
        return Err(Refusal("downstream_pipeline_creation_failed"));
    }
    // block style; each entry a JSON (flow) mapping, which is YAML too
    let mut out = String::from("include:\n");
    for i in &list {
        out.push_str(&format!("  - {}\n", serde_json::to_string(i).expect("serializable")));
    }
    out.push_str(&doc);
    let doc = out;
    Ok(doc)
}

async fn create_downstream(state: &AppState, b: &Build, spec: &StoredSpec, t: &TriggerSpec) -> Result<Option<i64>, Refusal> {
    let upstream = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await?.ok_or(Refusal("downstream_pipeline_creation_failed"))?;
    let variables = forwarded(state, b, spec, t).await?;
    let user = match b.user_id {
        Some(u) => forgust_db::users::find(&state.db, u).await?,
        None => None,
    }
    .ok_or(Refusal("insufficient_bridge_permissions"))?;
    let created = match &t.project {
        None => {
            let config = child_config(state, b, &t.include).await?;
            pipeline::create(
                state,
                pipeline::Trigger {
                    tag: upstream.tag,
                    before_sha: &upstream.before_sha,
                    merge_request_id: upstream.merge_request_id,
                    variables,
                    upstream: Some(Upstream { build_id: b.id, pipeline_id: upstream.id, config: Some(config) }),
                    ..pipeline::Trigger::new(b.project_id, &upstream.git_ref, &upstream.sha, "parent_pipeline", Some(user.id))
                },
            )
            .await?
        }
        Some(path) => {
            let target = crate::api::load_project(state, path).await.map_err(|_| Refusal("downstream_bridge_project_not_found"))?;
            let principal = Principal::User { user: user.clone(), scopes: None };
            if !crate::auth::authz::can_project(state, &principal, crate::auth::authz::ProjectAction::Read, &target).await? {
                return Err(Refusal("downstream_bridge_project_not_found"));
            }
            let branch = match &t.branch {
                Some(br) => crate::ci::variables::expand(br, &spec.job.variables.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
                None => target.default_branch.clone().ok_or(Refusal("invalid_bridge_trigger"))?,
            };
            let (sha, tag) = crate::ci::resolve_ref(state, target.id, &branch).await?.ok_or(Refusal("invalid_bridge_trigger"))?;
            if !crate::ci::may_run(state, &principal, &target, &branch, tag).await? {
                return Err(Refusal("insufficient_bridge_permissions"));
            }
            pipeline::create(
                state,
                pipeline::Trigger {
                    tag,
                    variables,
                    upstream: Some(Upstream { build_id: b.id, pipeline_id: upstream.id, config: None }),
                    ..pipeline::Trigger::new(target.id, &branch, &sha, "pipeline", Some(user.id))
                },
            )
            .await?
        }
    };
    Ok(match created {
        Created::Pipeline(id) => Some(id),
        _ => None,
    })
}

/// A pipeline's status changed: when a bridge with `strategy: depend` started
/// it, the bridge ends with it.
pub async fn downstream_changed(state: &AppState, pipeline_id: i64, status: &str) -> anyhow::Result<()> {
    if !pipeline::is_complete(status) {
        return Ok(());
    }
    let Some((Some(bridge_id), _)) = forgust_db::ci::pipeline_extras(&state.db, pipeline_id).await? else { return Ok(()) };
    let Some(bridge) = forgust_db::ci::build(&state.db, bridge_id).await? else { return Ok(()) };
    let depend = StoredSpec::of(&bridge).ok().and_then(|s| s.job.trigger).is_some_and(|t| t.depend);
    // canceling: the bridge was canceled and waits for its pipeline to stop
    if !depend || !matches!(bridge.status.as_str(), "running" | "canceling") {
        return Ok(());
    }
    let (end, reason) = match status {
        "success" | "skipped" => ("success", None),
        "canceled" => ("canceled", None),
        _ => ("failed", Some("downstream_pipeline_failed")),
    };
    pipeline::finish_job(state, bridge_id, end, reason, None).await?;
    Ok(())
}
