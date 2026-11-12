//! Pipeline and job webhooks (M7), in GitLab's shapes: `object_kind` `pipeline`
//! ("Pipeline Hook") and `build` ("Job Hook"). Sent from the state transitions
//! themselves (after their transaction commits), never from runner requests,
//! which runners retry.

use chrono::{DateTime, Utc};
use forgust_db::ci::{Build, Pipeline};
use forgust_db::projects::Project;
use serde_json::{Value, json};

use crate::events::Event;
use crate::services::mr::project_json;
use crate::services::webhooks;
use crate::state::AppState;

fn ts(t: &Option<DateTime<Utc>>) -> Value {
    t.map(|t| json!(t.format("%Y-%m-%d %H:%M:%S UTC").to_string())).unwrap_or(Value::Null)
}

fn secs(a: &Option<DateTime<Utc>>, b: &Option<DateTime<Utc>>) -> Value {
    match (a, b) {
        (Some(a), Some(b)) => json!((*b - *a).num_milliseconds() as f64 / 1000.0),
        _ => Value::Null,
    }
}

async fn user_json(state: &AppState, id: Option<i64>) -> Value {
    match id {
        Some(id) => match forgust_db::users::find(&state.db, id).await {
            Ok(Some(u)) => json!({ "id": u.id, "name": u.name, "username": u.username, "email": "[REDACTED]" }),
            _ => Value::Null,
        },
        None => Value::Null,
    }
}

async fn commit_json(state: &AppState, p: &Project, sha: &str) -> Value {
    match state.storage.commit(p.id, sha).await {
        Ok(Some(c)) => json!({
            "id": c.id,
            "message": if c.body.is_empty() { c.subject.clone() } else { format!("{}\n\n{}", c.subject, c.body) },
            "title": c.subject,
            "timestamp": DateTime::<Utc>::from_timestamp(c.committer.time, 0).map(|t| t.to_rfc3339()),
            "url": state.cfg.project_page(&p.full_path, &format!("commit/{}", c.id)),
            "author": { "name": c.author.name, "email": c.author.email },
        }),
        _ => json!({ "id": sha }),
    }
}

fn build_json(b: &Build) -> Value {
    json!({
        "id": b.id,
        "stage": b.stage,
        "name": b.name,
        "status": b.status,
        "created_at": ts(&Some(b.created_at)),
        "started_at": ts(&b.started_at),
        "finished_at": ts(&b.finished_at),
        "duration": secs(&b.started_at, &b.finished_at),
        "queued_duration": secs(&b.queued_at, &b.started_at),
        "failure_reason": b.failure_reason,
        "when": b.when_,
        "manual": b.when_ == "manual",
        "allow_failure": b.allow_failure,
        "runner": b.runner_id.map(|id| json!({ "id": id })),
    })
}

/// The pipeline moved to a new status.
pub async fn pipeline(state: &AppState, project_id: i64, pipeline_id: i64) {
    let r: anyhow::Result<()> = async {
        if forgust_db::webhooks::for_event(&state.db, project_id, "pipeline").await?.is_empty() {
            return Ok(());
        }
        let Some(pl) = forgust_db::ci::pipeline(&state.db, pipeline_id).await? else { return Ok(()) };
        let Some(p) = forgust_db::projects::find(&state.db, pl.project_id).await? else { return Ok(()) };
        let builds: Vec<Value> =
            forgust_db::ci::builds_of_pipeline(&state.db, pl.id).await?.iter().filter(|b| !b.retried).map(build_json).collect();
        let mr = match pl.merge_request_id {
            Some(id) => forgust_db::merge_requests::find_by_id(&state.db, id).await?.map(|m| {
                json!({ "id": m.id, "iid": m.iid, "title": m.title, "source_branch": m.source_branch, "target_branch": m.target_branch,
                        "state": m.state, "url": state.cfg.mr_url(&p.full_path, m.iid) })
            }),
            None => None,
        };
        let payload = json!({
            "object_kind": "pipeline",
            "object_attributes": attributes(state, &p, &pl),
            "merge_request": mr,
            "user": user_json(state, pl.user_id).await,
            "project": project_json(state, &p),
            "commit": commit_json(state, &p, &pl.sha).await,
            "builds": builds,
        });
        webhooks::emit(state, &p, "pipeline", payload).await;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(pipeline_id, error = %e, "pipeline hook not sent");
    }
}

fn attributes(state: &AppState, p: &Project, pl: &Pipeline) -> Value {
    json!({
        "id": pl.id,
        "iid": pl.iid,
        "ref": pl.git_ref,
        "tag": pl.tag,
        "sha": pl.sha,
        "before_sha": pl.before_sha,
        "source": pl.source,
        "status": pl.status,
        "detailed_status": pl.status,
        "created_at": ts(&Some(pl.created_at)),
        "finished_at": ts(&pl.finished_at),
        "duration": secs(&pl.started_at, &pl.finished_at),
        "url": state.cfg.project_page(&p.full_path, &format!("pipelines/{}", pl.iid)),
    })
}

/// A job moved to `status` (from the transition: the row may have moved on).
pub async fn job(state: &AppState, project_id: i64, build_id: i64, status: &str) {
    let r: anyhow::Result<()> = async {
        // most projects have no job hooks: ask that first (this runs on every
        // job's every change)
        if forgust_db::webhooks::for_event(&state.db, project_id, "build").await?.is_empty() {
            return Ok(());
        }
        let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
        let Some(p) = forgust_db::projects::find(&state.db, b.project_id).await? else { return Ok(()) };
        let Some(pl) = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await? else { return Ok(()) };
        let commit = commit_json(state, &p, &pl.sha).await;
        let payload = json!({
            "object_kind": "build",
            "ref": pl.git_ref,
            "tag": pl.tag,
            "before_sha": pl.before_sha,
            "sha": pl.sha,
            "build_id": b.id,
            "build_name": b.name,
            "build_stage": b.stage,
            "build_status": status,
            "build_created_at": ts(&Some(b.created_at)),
            "build_started_at": ts(&b.started_at),
            "build_finished_at": ts(&b.finished_at),
            "build_duration": secs(&b.started_at, &b.finished_at),
            "build_queued_duration": secs(&b.queued_at, &b.started_at),
            "build_allow_failure": b.allow_failure,
            "build_failure_reason": b.failure_reason.clone().unwrap_or_else(|| "unknown_failure".into()),
            "retries_count": b.attempt - 1,
            "pipeline_id": pl.id,
            "project_id": p.id,
            "project_name": p.full_path,
            "user": user_json(state, b.user_id).await,
            "commit": {
                "id": pl.id,
                "sha": pl.sha,
                "message": commit["message"],
                "author_name": commit["author"]["name"],
                "author_email": commit["author"]["email"],
                "status": pl.status,
                "duration": secs(&pl.started_at, &pl.finished_at),
                "started_at": ts(&pl.started_at),
                "finished_at": ts(&pl.finished_at),
            },
            "repository": {
                "name": p.name,
                "url": state.cfg.http_clone_url(&p.full_path),
                "description": p.description,
                "homepage": state.cfg.web_url(&p.full_path),
            },
            "project": project_json(state, &p),
            "runner": b.runner_id.map(|id| json!({ "id": id })),
        });
        webhooks::emit(state, &p, "build", payload).await;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(build_id, error = %e, "job hook not sent");
    }
}

/// A job changed status: the live event and the job webhook.
pub async fn announce(state: &AppState, b: &Build, status: &str) {
    state.events.publish(Event::job(b, status));
    // a deployment follows its job
    if let Err(e) = crate::ci::environments::on_job_status(state, b, status).await {
        tracing::warn!(job = b.id, error = %e, "deployment not updated");
    }
    job(state, b.project_id, b.id, status).await;
}
