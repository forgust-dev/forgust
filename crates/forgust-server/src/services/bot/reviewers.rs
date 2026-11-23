//! The bot asks for reviews (ADR-0026), by the `[reviewers]` rules of
//! `.forgust/bot.toml` on the target branch: the code owners of the changed
//! files first, then the people and groups of `from`, never the author, a
//! bot, or somebody listed as `away`.
//!
//! Once per merge request, when it is opened ready or leaves draft: reviewers
//! its author already asked count, and one taken off later is not replaced.
//! Among several people the bot takes whoever it asked longest ago
//! (`round_robin`), or whoever has the fewest open merge requests to review
//! in the project (`least_loaded`, then by turn).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use forgust_db::access::REPORTER;
use forgust_db::merge_requests::MergeRequest;
use forgust_db::projects::Project;
use serde_json::json;

use super::config::Pick;
use crate::auth::Principal;
use crate::error::ApiResult;
use crate::state::AppState;

/// Somebody the bot could ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: i64,
    pub username: String,
    /// Owns a file the merge request changes.
    pub owner: bool,
    /// Open merge requests of the project they are asked to review.
    pub load: i64,
    /// When the bot last chose them here.
    pub last: Option<DateTime<Utc>>,
}

/// The `need` to ask: code owners first, then by `pick`.
pub fn choose(mut candidates: Vec<Candidate>, pick: Pick, need: usize) -> Vec<Candidate> {
    candidates.sort_by(|a, b| {
        let load = |c: &Candidate| if pick == Pick::LeastLoaded { c.load } else { 0 };
        // never chosen (None) comes before any date
        (!a.owner, load(a), a.last, &a.username).cmp(&(!b.owner, load(b), b.last, &b.username))
    });
    candidates.truncate(need);
    candidates
}

/// A merge request was opened, or is no longer a draft: ask for its review.
/// A failure is logged and nothing more.
pub async fn on_merge_request(state: &AppState, p: &Project, mr: &MergeRequest) {
    if let Err(e) = sync(state, p, mr.iid).await {
        tracing::warn!(project_id = p.id, iid = mr.iid, error = ?e, "bot: no reviewer asked");
    }
}

async fn sync(state: &AppState, p: &Project, iid: i64) -> ApiResult<()> {
    if !super::is_on(state, p.id).await? {
        return Ok(());
    }
    // as it is now; a draft waits until it is ready
    let found = forgust_db::merge_requests::find(&state.db, p.id, iid).await?;
    let Some(m) = found.filter(|m| m.state == "opened" && !m.draft) else { return Ok(()) };
    let Ok(Some(rules)) = super::rules(state, p, Some(&m.target_branch)).await?.map(|r| r.reviewers) else { return Ok(()) };
    if !forgust_db::bot::claim_review(&state.db, m.id).await? {
        return Ok(());
    }
    let asked: Vec<i64> = forgust_db::people::mr_reviewers(&state.db, &[m.id]).await?.into_iter().map(|(_, r)| r.user.id).collect();
    let need = rules.count.saturating_sub(asked.len());
    if need == 0 {
        return Ok(());
    }

    // id → owns a changed file
    let mut found: BTreeMap<i64, bool> = BTreeMap::new();
    for r in &rules.from {
        for u in crate::services::codeowners::people(state, p, r).await? {
            found.insert(u.id, false);
        }
    }
    if rules.code_owners {
        for u in crate::services::codeowners::for_merge_request(state, p, &m).await?.suggested_reviewers {
            found.insert(u.id, true);
        }
    }
    let ids: Vec<i64> = found.keys().copied().collect();
    let loads: BTreeMap<i64, i64> = forgust_db::bot::review_loads(&state.db, p.id, &ids).await?.into_iter().collect();
    let turns: BTreeMap<i64, DateTime<Utc>> = forgust_db::bot::review_turns(&state.db, p.id, &ids).await?.into_iter().collect();
    let mut candidates = Vec::new();
    for u in forgust_db::users::find_many(&state.db, &ids).await? {
        let away = rules.away.iter().any(|a| a.eq_ignore_ascii_case(&u.username));
        if u.is_bot() || !u.is_active() || away || Some(u.id) == m.author_id || asked.contains(&u.id) {
            continue;
        }
        // who can be asked from the page: a member (an admin who is none cannot)
        if forgust_db::authz::project_level(&state.db, u.id, p.id).await?.unwrap_or(0) < REPORTER {
            continue;
        }
        candidates.push(Candidate {
            id: u.id,
            owner: found[&u.id],
            load: loads.get(&u.id).copied().unwrap_or(0),
            last: turns.get(&u.id).copied(),
            username: u.username,
        });
    }
    let chosen = choose(candidates, rules.pick, need);
    if chosen.is_empty() {
        return Ok(());
    }
    let bot = super::member(state, p).await?;
    let bot_id = bot.id;
    let ids: Vec<i64> = chosen.iter().map(|c| c.id).collect();
    let all: Vec<i64> = asked.iter().chain(&ids).copied().collect();
    crate::services::people::set_mr(state, &Principal::member(bot), p, &m, None, Some(&all)).await?;
    forgust_db::bot::took_review_turn(&state.db, p.id, &ids).await?;
    let details = json!({
        "merge_request": m.iid,
        "reviewers": chosen.iter().map(|c| &c.username).collect::<Vec<_>>(),
        "code_owners": chosen.iter().filter(|c| c.owner).map(|c| &c.username).collect::<Vec<_>>(),
    });
    crate::services::write_audit(state, "bot", Some(bot_id), "bot_reviewers", Some(("project", p.id)), details, None).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(username: &str, owner: bool, load: i64, last: Option<i64>) -> Candidate {
        let last = last.map(|s| DateTime::from_timestamp(s, 0).unwrap());
        Candidate { id: 0, username: username.into(), owner, load, last }
    }

    fn names(pick: Pick, need: usize, list: &[Candidate]) -> Vec<String> {
        choose(list.to_vec(), pick, need).into_iter().map(|c| c.username).collect()
    }

    #[test]
    fn owners_first_then_by_turn_or_load() {
        let list = [c("amy", false, 0, Some(30)), c("bo", false, 3, None), c("cy", true, 5, Some(20)), c("di", false, 1, Some(10))];
        // the owner, then whoever was never asked, then the oldest turn
        assert_eq!(names(Pick::RoundRobin, 4, &list), ["cy", "bo", "di", "amy"]);
        assert_eq!(names(Pick::RoundRobin, 1, &list), ["cy"]);
        // the owner still comes first; then the fewest open reviews
        assert_eq!(names(Pick::LeastLoaded, 3, &list), ["cy", "amy", "di"]);
        // equal load: by turn; equal turn: by name
        let even = [c("zed", false, 2, None), c("amy", false, 2, None), c("bo", false, 2, Some(5))];
        assert_eq!(names(Pick::LeastLoaded, 3, &even), ["amy", "zed", "bo"]);
        assert!(names(Pick::RoundRobin, 2, &[]).is_empty());
    }
}
