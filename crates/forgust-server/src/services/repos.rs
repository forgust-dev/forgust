use forgust_storage::ZERO_OID;

use crate::events::Event;
use crate::jobs::RefUpdate;
use crate::state::AppState;

/// Follow-up after a push (runs in the worker).
/// Make `branch` the project's default branch (HEAD and the project), and
/// protect it like the one it replaces unless it has a rule of its own.
pub async fn adopt_default_branch(state: &AppState, project_id: i64, branch: &str) -> anyhow::Result<()> {
    state.storage.set_default_branch(project_id, branch).await?;
    forgust_db::projects::set_default_branch(&state.db, project_id, branch).await?;
    if !forgust_db::protected_branches::list(&state.db, project_id).await?.iter().any(|r| r.name == branch) {
        let m = forgust_db::access::MAINTAINER;
        forgust_db::protected_branches::upsert(&state.db, project_id, branch, m, m, false, false).await?;
    }
    Ok(())
}

pub async fn after_push(state: &AppState, project_id: i64, updates: &[RefUpdate]) -> anyhow::Result<()> {
    let Some(project) = forgust_db::projects::find(&state.db, project_id).await? else { return Ok(()) };
    forgust_db::projects::touch_activity(&state.db, project_id).await?;

    // First push into an empty repo whose HEAD names a branch that was not
    // pushed: point HEAD (and the project) at the first pushed branch.
    let head = project.default_branch.clone().unwrap_or_default();
    let head_exists = !head.is_empty() && state.storage.resolve(project_id, &format!("refs/heads/{head}")).await?.is_some();
    if !head_exists && let Some(b) = updates.iter().filter(|u| u.new != ZERO_OID).find_map(|u| u.name.strip_prefix("refs/heads/")) {
        adopt_default_branch(state, project_id, b).await?;
    }

    state.events.publish(Event::Push { project_id, refs: updates.iter().map(|u| u.name.clone()).collect() });
    metrics::counter!("forgust_pushes_total").increment(1);
    Ok(())
}
