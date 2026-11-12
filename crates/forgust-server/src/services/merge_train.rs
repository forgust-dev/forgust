//! Merge trains: with the project's setting on, merge requests queue
//! per target branch. Each car is tested on the merge of the target, every car
//! ahead of it and itself (`refs/merge-requests/<iid>/train`); the first car
//! whose pipeline passed merges by moving the target to exactly that commit. A
//! car that fails leaves the train, and the cars behind it are rebuilt; so do
//! they all when the target moves by other means.

use forgust_db::merge_requests::{self as mrdb, MergeRequest, Position};
use forgust_db::merge_trains::{self as tdb, Car};
use forgust_db::projects::Project;
use serde_json::json;

use super::mr::{self, MergeMethod, MergeOptions};
use crate::auth::Principal;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn train_ref(iid: i64) -> String {
    format!("refs/merge-requests/{iid}/train")
}

/// Merge-status blockers that a train takes care of (its own pipeline).
fn train_handles(blocker: &str) -> bool {
    blocker.starts_with("the pipeline must succeed")
}

/// How a car merges, chosen when it is added.
#[derive(Debug, Default, Clone, Copy, serde::Deserialize, utoipa::ToSchema)]
pub struct TrainOptions {
    /// One squashed commit on the target instead of a merge commit.
    #[serde(default)]
    pub squash: bool,
    /// The merge request's own commits, rebased onto what is ahead (the
    /// rebase and fast-forward methods), instead of a merge commit.
    #[serde(default)]
    pub rebase: bool,
    /// Delete the source branch once merged.
    #[serde(default)]
    pub remove_source_branch: bool,
}

/// Put a merge request on its target's train.
pub async fn add(state: &AppState, principal: &Principal, p: &Project, m: &MergeRequest, opts: TrainOptions) -> ApiResult<Car> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    let settings = forgust_db::ci::settings(&state.db, p.id).await?;
    if !settings.merge_trains_enabled {
        return Err(ApiError::conflict("merge trains are off for this project (CI/CD settings)"));
    }
    let status = mr::merge_status(state, principal, p, m).await?;
    if !status.user_can_merge {
        return Err(ApiError::forbidden(format!("you are not allowed to merge into `{}`", m.target_branch)));
    }
    let blockers: Vec<&String> = status.blockers.iter().filter(|b| !train_handles(b)).collect();
    if !blockers.is_empty() {
        return Err(ApiError::conflict(blockers.iter().map(|b| b.as_str()).collect::<Vec<_>>().join("; ")));
    }
    // the train runs its own pipeline, but not for something already broken
    if let Some(s @ ("failed" | "canceled")) = status.pipeline_status.as_deref() {
        return Err(ApiError::conflict(format!("the merge request's pipeline {s}: fix it before adding it to the merge train")));
    }
    if opts.squash && opts.rebase {
        return Err(ApiError::bad("a car is squashed or rebased, not both"));
    }
    let Some(head) = status.head_sha.as_deref() else { return Err(ApiError::conflict("the source branch is gone")) };
    if !tdb::add(
        &state.db,
        p.id,
        &m.target_branch,
        m.id,
        user.id,
        head,
        tdb::CarOptions { squash: opts.squash, rebase: opts.rebase, remove_source_branch: opts.remove_source_branch },
    )
    .await?
    {
        return Err(ApiError::conflict("this merge request is already on the merge train"));
    }
    let how = if opts.squash {
        " (squashed)"
    } else if opts.rebase {
        " (rebased)"
    } else {
        ""
    };
    note(state, m.id, Some(user.id), &format!("added this merge request to the merge train{how}")).await;
    super::audit(state, principal, "merge_train_added", Some(("project", p.id)), json!({ "iid": m.iid }), None).await;
    refresh(state, p, &m.target_branch).await.map_err(ApiError::Internal)?;
    tdb::of_mr(&state.db, m.id).await?.ok_or_else(|| ApiError::conflict("the merge request left the train at once (see its notes)"))
}

/// Take a merge request off its train (the cars behind are rebuilt).
pub async fn remove(state: &AppState, principal: &Principal, p: &Project, m: &MergeRequest) -> ApiResult<()> {
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let car = tdb::of_mr(&state.db, m.id).await?.ok_or(ApiError::NotFound)?;
    if car.user_id != Some(user.id) && !mr::merge_status(state, principal, p, m).await?.user_can_merge {
        return Err(ApiError::forbidden("only who added it, or someone who may merge, removes it from the train"));
    }
    leave(state, p, &car, Some(user.id), "removed this merge request from the merge train").await.map_err(ApiError::Internal)
}

async fn note(state: &AppState, mr_id: i64, user_id: Option<i64>, body: &str) {
    if let Err(e) = mrdb::add_note(&state.db, mr_id, user_id, None, body, true, Position::default()).await {
        tracing::warn!(mr_id, error = %e, "merge train note not written");
    }
}

/// A car leaves: its pipeline stops, and the train behind it is rebuilt.
async fn leave(state: &AppState, p: &Project, car: &Car, user_id: Option<i64>, why: &str) -> anyhow::Result<()> {
    tdb::remove(&state.db, car.merge_request_id).await?;
    if let Some(pid) = car.pipeline_id
        && car.pipeline_status.as_deref().is_some_and(|s| !crate::ci::pipeline::is_complete(s))
    {
        crate::ci::pipeline::cancel(state, pid).await?;
    }
    let _ = state.storage.delete_ref(p.id, &train_ref(car.iid), None).await;
    note(state, car.merge_request_id, user_id, why).await;
    state.events.publish(crate::events::Event::MergeRequest { project_id: p.id, iid: car.iid });
    Box::pin(refresh(state, p, &car.target_branch)).await
}

/// Make every car of a train match the target and the cars ahead of it:
/// those still built on the right base and head keep their pipeline, the
/// others get a new train commit and pipeline. Then merge what is ready.
pub async fn refresh(state: &AppState, p: &Project, target: &str) -> anyhow::Result<()> {
    // an archived project's trains stand still (nothing merges)
    if p.archived() {
        return Ok(());
    }
    let mut to_leave: Vec<(Car, String)> = Vec::new();
    // pipelines of rebuilt cars, canceled once the cars point at their new ones
    // (a cancel reaches `pipeline_changed`, which must not find the car)
    let mut stale_pipelines: Vec<i64> = Vec::new();
    {
        // one refresh per train at a time (any process)
        let mut tx = state.db.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))").bind(p.id as i32).bind(target).execute(&mut *tx).await?;
        let Some(mut base) = state.storage.resolve(p.id, &format!("refs/heads/{target}")).await? else { return Ok(()) };
        for car in tdb::cars(&mut *tx, p.id, Some(target)).await? {
            let Some(m) = mrdb::find_by_id(&mut *tx, car.merge_request_id).await? else { continue };
            if m.state != "opened" {
                tdb::remove(&mut *tx, m.id).await?;
                continue;
            }
            let Some(head) = state.storage.resolve(p.id, &format!("refs/heads/{}", m.source_branch)).await? else {
                to_leave.push((car, "removed this merge request from the merge train: its source branch is gone".into()));
                continue;
            };
            // new commits were not vetted by who added it (approvals, review)
            if car.head_sha.as_deref().is_some_and(|h| h != head) {
                to_leave.push((car, "removed this merge request from the merge train: its source branch was updated; add it again".into()));
                continue;
            }
            if car.base_sha.as_deref() == Some(base.as_str())
                && car.head_sha.as_deref() == Some(head.as_str())
                && let Some(t) = &car.train_sha
            {
                base = t.clone();
                continue;
            }
            // rebased: its own commits on top of what is ahead (none needed when
            // it already sits on it); a conflict sends it off the train
            if car.rebase {
                let commit = if state.storage.is_ancestor(p.id, &base, &head).await? {
                    head.clone()
                } else {
                    let upstream = state.storage.merge_base(p.id, &base, &head).await?;
                    match upstream {
                        Some(u) => match state.storage.rebase(p.id, &base, &u, &head, &mr::committer(state)).await {
                            Ok(c) => c,
                            Err(e) => {
                                to_leave.push((car, format!("removed this merge request from the merge train: it does not rebase onto the merge requests ahead of it ({e})")));
                                continue;
                            }
                        },
                        None => {
                            to_leave
                                .push((car, "removed this merge request from the merge train: no common history with its target".into()));
                            continue;
                        }
                    }
                };
                let pipeline_id = start_car_pipeline(state, &mut stale_pipelines, p, &m, &car, &commit).await?;
                tdb::rebuilt(&mut *tx, car.id, &base, &head, &commit, pipeline_id).await?;
                state.events.publish(crate::events::Event::MergeRequest { project_id: p.id, iid: m.iid });
                base = commit;
                continue;
            }
            let merged = state.storage.merge_tree(p.id, &base, &head, None).await?;
            if !merged.clean() {
                to_leave.push((
                    car,
                    format!(
                        "removed this merge request from the merge train: it conflicts with the merge requests ahead of it ({})",
                        merged.conflicts.join(", ")
                    ),
                ));
                continue;
            }
            let user = match car.user_id {
                Some(u) => forgust_db::users::find(&mut *tx, u).await?,
                None => None,
            };
            let author = user.as_ref().map(mr::identity).unwrap_or_else(|| mr::committer(state));
            // the commit the pipeline tests is the one the target moves to:
            // squashed onto what is ahead, or a merge commit
            let commit = if car.squash {
                let (msg, mr_author) = mr::squash_parts(state, &m, &author).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                state.storage.commit_tree(p.id, &merged.tree, std::slice::from_ref(&base), &msg, &mr_author, &mr::committer(state)).await?
            } else {
                let msg = format!(
                    "Merge branch '{}' into '{target}'\n\n{}\n\nSee merge request !{} (merge train)",
                    m.source_branch, m.title, m.iid
                );
                state.storage.commit_tree(p.id, &merged.tree, &[base.clone(), head.clone()], &msg, &author, &mr::committer(state)).await?
            };
            let pipeline_id = start_car_pipeline(state, &mut stale_pipelines, p, &m, &car, &commit).await?;
            tdb::rebuilt(&mut *tx, car.id, &base, &head, &commit, pipeline_id).await?;
            state.events.publish(crate::events::Event::MergeRequest { project_id: p.id, iid: m.iid });
            base = commit;
        }
        tx.commit().await?;
    }
    for old in stale_pipelines {
        let _ = crate::ci::pipeline::cancel(state, old).await;
    }
    // one at a time: leaving rebuilds the train, which finds any other
    if let Some((car, why)) = to_leave.into_iter().next() {
        return leave(state, p, &car, None, &why).await;
    }
    try_merge(state, p, target).await
}

/// Point the car's ref at the commit it would land, and start its pipeline
/// (the old one, testing something that will not merge, is canceled later).
async fn start_car_pipeline(
    state: &AppState,
    stale_pipelines: &mut Vec<i64>,
    p: &Project,
    m: &MergeRequest,
    car: &Car,
    commit: &str,
) -> anyhow::Result<Option<i64>> {
    state.storage.update_ref(p.id, &train_ref(m.iid), commit, None).await?;
    if let Some(old) = car.pipeline_id
        && car.pipeline_status.as_deref().is_some_and(|s| !crate::ci::pipeline::is_complete(s))
    {
        stale_pipelines.push(old);
    }
    let created = crate::ci::pipeline::create(
        state,
        crate::ci::pipeline::Trigger {
            merge_request_id: Some(m.id),
            variables: vec![("CI_MERGE_REQUEST_EVENT_TYPE".into(), "merge_train".into())],
            ..crate::ci::pipeline::Trigger::new(p.id, &m.source_branch, commit, "merge_request_event", car.user_id)
        },
    )
    .await?;
    Ok(match created {
        crate::ci::pipeline::Created::Pipeline(id) => Some(id),
        _ => None,
    })
}

/// Merge the first cars whose pipelines passed (or that have none to run).
pub async fn try_merge(state: &AppState, p: &Project, target: &str) -> anyhow::Result<()> {
    let leaving = {
        // the train's lock: two paths (a pipeline ending, a push) never merge the same car
        let mut tx = state.db.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))").bind(p.id as i32).bind(target).execute(&mut *tx).await?;
        let l = merge_ready(state, p, target).await?;
        tx.commit().await?;
        l
    };
    match leaving {
        Some((car, why)) => leave(state, p, &car, None, &why).await,
        None => Ok(()),
    }
}

/// The merging loop of [`try_merge`], under the train's lock; a car that
/// must leave is returned (leaving takes the lock again).
async fn merge_ready(state: &AppState, p: &Project, target: &str) -> anyhow::Result<Option<(Car, String)>> {
    loop {
        let cars = tdb::cars(&state.db, p.id, Some(target)).await?;
        let Some(car) = cars.into_iter().next() else { return Ok(None) };
        let ready = match (&car.pipeline_id, car.pipeline_status.as_deref()) {
            (Some(_), Some("success")) => true,
            (Some(_), _) => false,
            // no pipeline ran for it (no config, or rules skipped it)
            (None, _) => car.train_sha.is_some(),
        };
        if !ready {
            return Ok(None);
        }
        let (Some(train), Some(base), Some(head)) = (car.train_sha.clone(), car.base_sha.clone(), car.head_sha.clone()) else {
            return Ok(None);
        };
        let Some(m) = mrdb::find_by_id(&state.db, car.merge_request_id).await? else { return Ok(None) };
        if m.state != "opened" {
            tdb::remove(&state.db, m.id).await?;
            continue;
        }
        let user = match car.user_id {
            Some(u) => forgust_db::users::find(&state.db, u).await?,
            None => None,
        };
        let Some(user) = user else {
            return Ok(Some((car, "removed this merge request from the merge train: who added it is gone".into())));
        };
        let principal = Principal::User { user, scopes: None };
        // no train pipeline (the config has no merge request jobs): the merge
        // request's own pipeline must pass first, as without a train
        if car.pipeline_id.is_none() && mr::merge_status(state, &principal, p, &m).await?.blockers.iter().any(|b| train_handles(b)) {
            return Ok(None);
        }
        let opts = MergeOptions {
            method: if car.squash {
                MergeMethod::Squash
            } else if car.rebase {
                MergeMethod::Rebase
            } else {
                MergeMethod::Merge
            },
            expected_head: Some(&head),
            remove_source_branch: car.remove_source_branch || m.remove_source_branch,
            message: None,
            train: Some((&train, &base)),
        };
        match Box::pin(mr::merge(state, &principal, p, &m, opts)).await {
            Ok(_) => {
                tdb::remove(&state.db, m.id).await?;
                let _ = state.storage.delete_ref(p.id, &train_ref(m.iid), None).await;
            }
            Err(e) => {
                return Ok(Some((car, format!("removed this merge request from the merge train: it could not merge ({e})"))));
            }
        }
    }
}

/// A pipeline's status changed: a train car that passed may merge; one that
/// failed leaves.
pub async fn pipeline_changed(state: &AppState, pipeline_id: i64, status: &str) -> anyhow::Result<()> {
    if !crate::ci::pipeline::is_complete(status) {
        return Ok(());
    }
    let Some((project_id, target)) = tdb::of_pipeline(&state.db, pipeline_id).await? else {
        // a merge request's own pipeline: cars without a train pipeline wait for
        // it, and leave when it does not pass (else the train behind them stalls)
        let Some(pl) = forgust_db::ci::pipeline(&state.db, pipeline_id).await? else { return Ok(()) };
        if !tdb::any_in(&state.db, pl.project_id).await? {
            return Ok(());
        }
        let Some(p) = forgust_db::projects::find(&state.db, pl.project_id).await?.filter(|p| !p.archived()) else { return Ok(()) };
        if status == "success" {
            let mut targets: Vec<String> = tdb::cars(&state.db, p.id, None).await?.into_iter().map(|c| c.target_branch).collect();
            targets.dedup();
            for target in targets {
                try_merge(state, &p, &target).await?;
            }
            return Ok(());
        }
        for car in tdb::cars(&state.db, p.id, None).await? {
            if car.pipeline_id.is_some() || car.head_sha.as_deref() != Some(pl.sha.as_str()) {
                continue;
            }
            let Some(m) = mrdb::find_by_id(&state.db, car.merge_request_id).await? else { continue };
            if pl.merge_request_id == Some(m.id) || (pl.merge_request_id.is_none() && !pl.tag && pl.git_ref == m.source_branch) {
                let why = format!("removed this merge request from the merge train: its pipeline {status}");
                leave(state, &p, &car, None, &why).await?;
                // leaving rebuilt the train: other cars are read again next time
                return Ok(());
            }
        }
        return Ok(());
    };
    // an archived project's trains stand still: no merge, no car leaves
    let Some(p) = forgust_db::projects::find(&state.db, project_id).await?.filter(|p| !p.archived()) else { return Ok(()) };
    if status == "success" {
        return try_merge(state, &p, &target).await;
    }
    let cars = tdb::cars(&state.db, project_id, Some(&target)).await?;
    if let Some(car) = cars.into_iter().find(|c| c.pipeline_id == Some(pipeline_id)) {
        leave(state, &p, &car, None, &format!("removed this merge request from the merge train: its pipeline {status}")).await?;
    }
    Ok(())
}

/// After a push: trains of the project follow their targets and sources.
pub async fn on_push(state: &AppState, project_id: i64) -> anyhow::Result<()> {
    if !tdb::any_in(&state.db, project_id).await? {
        return Ok(());
    }
    let Some(p) = forgust_db::projects::find(&state.db, project_id).await? else { return Ok(()) };
    let mut targets: Vec<String> = tdb::cars(&state.db, project_id, None).await?.into_iter().map(|c| c.target_branch).collect();
    targets.dedup();
    for t in targets {
        refresh(state, &p, &t).await?;
    }
    Ok(())
}
