//! GitLab's JSON shapes for Forgust's objects (only the fields clients read,
//! plus their usual neighbours).

use chrono::{DateTime, Utc};
use forgust_db::ci::{Build, Pipeline};
use forgust_db::issues::{Issue, IssueNote};
use forgust_db::merge_requests::{MergeRequest, Note};
use forgust_db::people::UserBrief;
use forgust_db::projects::Project;
use forgust_db::users::User;
use serde_json::{Value, json};

use crate::api::dto::ProjectDto;
use crate::config::Config;

pub fn base(cfg: &Config) -> &str {
    &cfg.server.external_url
}

pub fn ts(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn user(cfg: &Config, u: &User) -> Value {
    json!({
        "id": u.id, "username": u.username, "name": if u.name.is_empty() { &u.username } else { &u.name },
        "state": u.state, "locked": false, "avatar_url": null, "web_url": format!("{}/{}", base(cfg), u.username),
        "bot": u.is_bot(),
    })
}

/// The signed-in user (GET /user): with emails and admin flag.
pub fn me(cfg: &Config, u: &User) -> Value {
    let mut v = user(cfg, u);
    let o = v.as_object_mut().unwrap();
    o.insert("email".into(), json!(u.email));
    o.insert("commit_email".into(), json!(u.email));
    o.insert("public_email".into(), json!(""));
    o.insert("is_admin".into(), json!(u.is_admin));
    o.insert("created_at".into(), json!(ts(&u.created_at)));
    o.insert("two_factor_enabled".into(), json!(u.totp_enabled));
    o.insert("external".into(), json!(u.is_external));
    v
}

/// A user known only by id and username (authors of MRs, issues, notes).
pub fn person(cfg: &Config, id: Option<i64>, username: Option<&str>, name: Option<&str>, imported: Option<&str>) -> Value {
    match (id, username) {
        (Some(id), Some(u)) => json!({
            "id": id, "username": u, "name": name.filter(|n| !n.is_empty()).unwrap_or(u), "state": "active",
            "avatar_url": null, "web_url": format!("{}/{u}", base(cfg)),
        }),
        // imported authors with no account: GitLab's "Ghost User", named
        _ => {
            json!({ "id": 0, "username": "ghost", "name": imported.unwrap_or("Ghost User"), "state": "active", "avatar_url": null, "web_url": format!("{}/ghost", base(cfg)) })
        }
    }
}

pub fn project(cfg: &Config, p: &Project, empty: bool) -> Value {
    let dto = ProjectDto::from(p, cfg);
    let (ns_path, _) = p.full_path.rsplit_once('/').unwrap_or(("", &p.path));
    let ns_name = ns_path.rsplit('/').next().unwrap_or(ns_path);
    let mut out = json!({
        "id": p.id,
        "name": p.name,
        "name_with_namespace": format!("{} / {}", ns_path.replace('/', " / "), p.name),
        "path": p.path,
        "path_with_namespace": p.full_path,
        "description": p.description,
        "default_branch": p.default_branch,
        "visibility": forgust_db::visibility::name(p.visibility),
        "web_url": dto.web_url,
        "http_url_to_repo": dto.http_url,
        "ssh_url_to_repo": dto.ssh_url,
        "readme_url": null,
        "avatar_url": null,
        "topics": [],
        "tag_list": [],
        "created_at": ts(&p.created_at),
        "last_activity_at": ts(&p.last_activity_at),
        "namespace": { "id": p.namespace_id, "name": ns_name, "path": ns_name, "kind": "group", "full_path": ns_path,
                       "web_url": format!("{}/{ns_path}", base(cfg)) },
    });
    let features = json!({
        "archived": p.archived(),
        "empty_repo": empty,
        "mirror": false,
        "marked_for_deletion_at": null,
        "marked_for_deletion_on": null,
        "forked_from_project": null,
        "issues_enabled": true,
        "merge_requests_enabled": true,
        "jobs_enabled": true,
        "wiki_enabled": false,
        "snippets_enabled": false,
        "container_registry_enabled": false,
        "repository_access_level": "enabled",
        "merge_requests_access_level": "enabled",
        "issues_access_level": "enabled",
        "builds_access_level": "enabled",
        "wiki_access_level": "disabled",
        "merge_method": "merge",
        "squash_option": "default_off",
        "merge_trains_enabled": false,
        "only_allow_merge_if_pipeline_succeeds": false,
        "remove_source_branch_after_merge": true,
        "lfs_enabled": true,
        "open_issues_count": null,
        "_links": {
            "self": format!("{}/api/v4/projects/{}", base(cfg), p.id),
            "issues": format!("{}/api/v4/projects/{}/issues", base(cfg), p.id),
            "merge_requests": format!("{}/api/v4/projects/{}/merge_requests", base(cfg), p.id),
            "repo_branches": format!("{}/api/v4/projects/{}/repository/branches", base(cfg), p.id),
        },
    });
    if let (Some(o), Value::Object(more)) = (out.as_object_mut(), features) {
        o.extend(more);
    }
    out
}

/// A user from a people list (assignees, reviewers), GitLab's short form.
pub fn person_of(cfg: &Config, u: &UserBrief) -> Value {
    json!({
        "id": u.id, "username": u.username, "name": if u.name.is_empty() { &u.username } else { &u.name },
        "state": "active", "locked": false, "avatar_url": null, "web_url": format!("{}/{}", base(cfg), u.username),
    })
}

/// Facts about an MR that need a lookup beyond its row.
#[derive(Default)]
pub struct MrExtras {
    pub labels: Vec<String>,
    pub sha: Option<String>,
    pub diff_refs: Option<(String, String, String)>,
    pub changes_count: Option<i32>,
    pub user_notes_count: i64,
    pub pipeline: Option<Value>,
    /// (merge_status, detailed_merge_status, has_conflicts, user can merge), computed for one MR
    pub status: Option<(&'static str, &'static str, bool, bool)>,
    pub assignees: Vec<UserBrief>,
    pub reviewers: Vec<UserBrief>,
}

/// The short milestone object issues and merge requests carry.
fn milestone(p: &Project, id: Option<i64>, title: Option<&str>) -> Value {
    match (id, title) {
        (Some(id), Some(title)) => json!({ "id": id, "title": title, "project_id": p.id }),
        _ => Value::Null,
    }
}

pub fn merge_request(cfg: &Config, p: &Project, m: &MergeRequest, x: MrExtras) -> Value {
    let web = format!("{}/{}/-/merge_requests/{}", base(cfg), p.full_path, m.iid);
    let author = person(cfg, m.author_id, m.author_username.as_deref(), None, m.imported_author.as_deref());
    let (merge_status, detailed, conflicts, can_merge) = x.status.unwrap_or(("unchecked", "unchecked", false, false));
    let title = m.title.clone();
    json!({
        "id": m.id,
        "iid": m.iid,
        "project_id": p.id,
        "title": title,
        "description": m.description,
        "state": m.state,
        "created_at": ts(&m.created_at),
        "updated_at": ts(&m.updated_at),
        "merged_at": m.merged_at.as_ref().map(ts),
        "closed_at": m.closed_at.as_ref().map(ts),
        "merged_by": null,
        "merge_user": null,
        "target_branch": m.target_branch,
        "source_branch": m.source_branch,
        "source_project_id": p.id,
        "target_project_id": p.id,
        "author": author,
        "assignee": x.assignees.first().map(|u| person_of(cfg, u)),
        "assignees": x.assignees.iter().map(|u| person_of(cfg, u)).collect::<Vec<_>>(),
        "reviewers": x.reviewers.iter().map(|u| person_of(cfg, u)).collect::<Vec<_>>(),
        "labels": x.labels,
        "draft": m.draft,
        "work_in_progress": m.draft,
        "milestone": milestone(p, m.milestone_id, m.milestone_title.as_deref()),
        "merge_when_pipeline_succeeds": m.auto_merge_by.is_some(),
        "auto_merge_strategy": m.auto_merge_by.map(|_| "merge_when_pipeline_succeeds"),
        "merge_status": merge_status,
        "detailed_merge_status": detailed,
        "has_conflicts": conflicts,
        "blocking_discussions_resolved": detailed != "discussions_not_resolved",
        "sha": x.sha,
        "merge_commit_sha": m.merge_commit_sha,
        "squash_commit_sha": null,
        "squash": m.squash,
        "should_remove_source_branch": null,
        "force_remove_source_branch": m.remove_source_branch,
        "user_notes_count": x.user_notes_count,
        "changes_count": x.changes_count.map(|n| n.to_string()),
        "diff_refs": x.diff_refs.map(|(base_sha, start_sha, head_sha)| json!({ "base_sha": base_sha, "start_sha": start_sha, "head_sha": head_sha })),
        "head_pipeline": x.pipeline.clone(),
        "pipeline": x.pipeline,
        "references": { "short": format!("!{}", m.iid), "relative": format!("!{}", m.iid), "full": format!("{}!{}", p.full_path, m.iid) },
        "web_url": web,
        "user": { "can_merge": can_merge },
        "upvotes": 0,
        "downvotes": 0,
        "discussion_locked": null,
        "time_stats": { "time_estimate": 0, "total_time_spent": 0, "human_time_estimate": null, "human_total_time_spent": null },
    })
}

pub fn issue(cfg: &Config, p: &Project, i: &Issue, labels: Vec<String>, assignees: &[UserBrief]) -> Value {
    json!({
        "id": i.id,
        "iid": i.iid,
        "project_id": p.id,
        "title": i.title,
        "description": i.description,
        "state": i.state,
        "created_at": ts(&i.created_at),
        "updated_at": ts(&i.updated_at),
        "closed_at": i.closed_at.as_ref().map(ts),
        "closed_by": null,
        "labels": labels,
        "milestone": milestone(p, i.milestone_id, i.milestone_title.as_deref()),
        "due_date": i.due_date,
        "assignees": assignees.iter().map(|u| person_of(cfg, u)).collect::<Vec<_>>(),
        "assignee": assignees.first().map(|u| person_of(cfg, u)),
        "author": person(cfg, i.author_id, i.author_username.as_deref(), i.author_name.as_deref(), i.imported_author.as_deref()),
        "type": "ISSUE",
        "issue_type": "issue",
        "user_notes_count": i.notes_count,
        "merge_requests_count": 0,
        "upvotes": 0,
        "downvotes": 0,
        "confidential": false,
        "discussion_locked": null,
        "web_url": format!("{}/{}/-/issues/{}", base(cfg), p.full_path, i.iid),
        "references": { "short": format!("#{}", i.iid), "relative": format!("#{}", i.iid), "full": format!("{}#{}", p.full_path, i.iid) },
        "time_stats": { "time_estimate": 0, "total_time_spent": 0, "human_time_estimate": null, "human_total_time_spent": null },
    })
}

pub fn note(cfg: &Config, n: &Note, mr_iid: i64) -> Value {
    let position = n.head_sha.as_ref().map(|head| {
        json!({
            "base_sha": n.base_sha, "start_sha": n.start_sha, "head_sha": head, "position_type": "text",
            "old_path": n.old_path, "new_path": n.new_path, "old_line": n.old_line, "new_line": n.new_line,
        })
    });
    json!({
        "id": n.id,
        "type": if position.is_some() { json!("DiffNote") } else if n.parent_id.is_some() { json!("DiscussionNote") } else { Value::Null },
        "body": n.body,
        "author": person(cfg, n.author_id, n.author_username.as_deref(), n.author_name.as_deref(), n.imported_author.as_deref()),
        "created_at": ts(&n.created_at),
        "updated_at": ts(&n.updated_at),
        "system": n.system,
        "noteable_type": "MergeRequest",
        "noteable_iid": mr_iid,
        "resolvable": n.parent_id.is_none() && position.is_some(),
        "resolved": n.resolved_at.is_some(),
        "position": position,
        "internal": false,
        "confidential": false,
    })
}

pub fn issue_note(cfg: &Config, n: &IssueNote, issue_iid: i64) -> Value {
    json!({
        "id": n.id,
        "type": null,
        "body": n.body,
        "author": person(cfg, n.author_id, n.author_username.as_deref(), n.author_name.as_deref(), n.imported_author.as_deref()),
        "created_at": ts(&n.created_at),
        "updated_at": ts(&n.updated_at),
        "system": n.system,
        "noteable_type": "Issue",
        "noteable_iid": issue_iid,
        "resolvable": false,
        "internal": false,
        "confidential": false,
    })
}

pub fn pipeline(cfg: &Config, p: &Project, pl: &Pipeline) -> Value {
    json!({
        "id": pl.id,
        "iid": pl.iid,
        "project_id": p.id,
        "sha": pl.sha,
        "ref": pl.git_ref,
        "tag": pl.tag,
        "status": pl.status,
        "source": pl.source,
        "created_at": ts(&pl.created_at),
        "updated_at": ts(&pl.updated_at),
        "started_at": pl.started_at.as_ref().map(ts),
        "finished_at": pl.finished_at.as_ref().map(ts),
        "duration": match (pl.started_at, pl.finished_at) { (Some(a), Some(b)) => Some((b - a).num_seconds()), _ => None },
        "yaml_errors": pl.yaml_errors,
        "web_url": format!("{}/{}/-/pipelines/{}", base(cfg), p.full_path, pl.iid),
        "user": pl.username.as_ref().map(|u| json!({ "username": u })),
        "detailed_status": { "group": pl.status, "text": pl.status, "label": pl.status },
    })
}

pub fn job(cfg: &Config, p: &Project, b: &Build, pl: &Pipeline) -> Value {
    json!({
        "id": b.id,
        "name": b.name,
        "stage": b.stage,
        "status": b.status,
        "ref": pl.git_ref,
        "tag": pl.tag,
        "allow_failure": b.allow_failure,
        "created_at": ts(&b.created_at),
        "started_at": b.started_at.as_ref().map(ts),
        "finished_at": b.finished_at.as_ref().map(ts),
        "duration": match (b.started_at, b.finished_at) { (Some(a), Some(c)) => Some((c - a).num_milliseconds() as f64 / 1000.0), _ => None },
        "failure_reason": b.failure_reason,
        "web_url": format!("{}/{}/-/jobs/{}", base(cfg), p.full_path, b.id),
        "pipeline": { "id": pl.id, "iid": pl.iid, "project_id": p.id, "ref": pl.git_ref, "sha": pl.sha, "status": pl.status },
        "commit": { "id": pl.sha, "short_id": &pl.sha[..pl.sha.len().min(8)] },
    })
}

pub fn commit(cfg: &Config, p: &Project, c: &forgust_storage::CommitInfo) -> Value {
    let time = |t: i64| DateTime::<Utc>::from_timestamp(t, 0).map(|d| ts(&d));
    let message = if c.body.is_empty() { c.subject.clone() } else { format!("{}\n\n{}", c.subject, c.body) };
    json!({
        "id": c.id,
        "short_id": &c.id[..c.id.len().min(8)],
        "title": c.subject,
        "message": message,
        "parent_ids": c.parents,
        "author_name": c.author.name,
        "author_email": c.author.email,
        "authored_date": time(c.author.time),
        "committer_name": c.committer.name,
        "committer_email": c.committer.email,
        "committed_date": time(c.committer.time),
        "created_at": time(c.committer.time),
        "web_url": format!("{}/{}/-/commit/{}", base(cfg), p.full_path, c.id),
    })
}
