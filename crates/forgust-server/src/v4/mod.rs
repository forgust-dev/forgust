//! A GitLab-compatible REST subset under `/api/v4` (M5): what Renovate and `glab`
//! call, mapped onto Forgust's own services. Project ids are numeric or the
//! URL-encoded path (`group%2Fproject`); lists page with `page`/`per_page` and
//! answer with `Link`, `X-Page`, `X-Next-Page` (and `X-Total` when known).
//!
//! The runner API lives in the same `/api/v4` tree (`crate::ci::runner_api`).

mod awards;
mod ci;
mod issues;
mod json;
mod milestones;
mod mrs;
mod projects;
mod registry;
mod releases;

use axum::Router;
use axum::http::{HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// GitLab version reported to clients. Renovate gates features on it
/// (detailed_merge_status since 15.6, reviewers since 13.9).
pub const VERSION: &str = "17.11.0";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/version", get(projects::version))
        .route("/metadata", get(projects::version))
        .route("/user", get(projects::me))
        .route("/users", get(projects::users))
        .route("/users/{id}/status", get(projects::user_status))
        .route("/projects", get(projects::list))
        .route("/groups/{id}/projects", get(projects::group_projects))
        .route("/projects/{id}", get(projects::show))
        .route("/projects/{id}/members", get(projects::members))
        .route("/projects/{id}/members/all", get(projects::members_all))
        .route("/projects/{id}/repository/files/{file}", get(projects::file))
        .route("/projects/{id}/repository/files/{file}/raw", get(projects::file_raw))
        .route("/projects/{id}/repository/branches", get(projects::branches).post(projects::create_branch))
        .route("/projects/{id}/repository/branches/{branch}", get(projects::branch).delete(projects::delete_branch))
        .route("/projects/{id}/repository/commits", get(projects::commits).post(projects::create_commit))
        .route("/projects/{id}/repository/tree", get(projects::tree))
        .route("/projects/{id}/labels", get(issues::labels).post(crate::api::issues::create_label))
        .route("/projects/{id}/registry/repositories", get(registry::repositories))
        .route("/projects/{id}/registry/repositories/{repository_id}/tags", get(registry::tags))
        .route(
            "/projects/{id}/registry/repositories/{repository_id}/tags/{tag}",
            get(registry::tag).delete(crate::api::registry::delete_tag),
        )
        .route("/projects/{id}/repository/tags", get(releases::tags).post(releases::create_tag))
        .route("/projects/{id}/repository/tags/{tag}", get(releases::tag).delete(releases::delete_tag))
        .route("/projects/{id}/protected_tags", get(releases::protected_tags).post(releases::protect_tag))
        .route("/projects/{id}/protected_tags/{name}", get(releases::protected_tag).delete(releases::unprotect_tag))
        .route("/projects/{id}/{kind}/{iid}/award_emoji", get(awards::list).post(awards::create))
        .route("/projects/{id}/{kind}/{iid}/award_emoji/{award_id}", axum::routing::delete(awards::delete))
        // the same shapes as GitLab's
        .route("/notification_settings", get(crate::api::notifications::mine).put(crate::api::notifications::set_mine))
        .route("/projects/{id}/notification_settings", get(crate::api::notifications::project).put(crate::api::notifications::set_project))
        .route("/projects/{id}/milestones", get(milestones::list).post(milestones::create))
        .route("/projects/{id}/milestones/{milestone_id}", get(milestones::show).put(milestones::update).delete(milestones::delete))
        .route("/projects/{id}/releases", get(releases::list).post(releases::create))
        .route("/projects/{id}/releases/{tag}", get(releases::show).put(releases::update).delete(releases::delete))
        .route("/projects/{id}/releases/{tag}/assets/links", get(releases::links).post(releases::add_link))
        .route("/projects/{id}/releases/{tag}/assets/links/{link_id}", get(releases::link).delete(releases::delete_link))
        .route("/projects/{id}/releases/{tag}/downloads/{*path}", get(releases::download))
        // streamed and capped by limits.max_upload_bytes
        .route("/projects/{id}/uploads", post(crate::api::releases::upload).layer(axum::extract::DefaultBodyLimit::disable()))
        .route("/projects/{id}/repository/commits/{sha}", get(projects::commit))
        .route("/projects/{id}/repository/commits/{sha}/statuses", get(projects::statuses))
        .route("/projects/{id}/statuses/{sha}", post(projects::set_status))
        .route("/projects/{id}/merge_requests", get(mrs::list).post(mrs::create))
        .route("/projects/{id}/merge_requests/{iid}", get(mrs::show).put(mrs::update))
        .route("/projects/{id}/merge_requests/{iid}/merge", put(mrs::merge))
        .route("/projects/{id}/merge_requests/{iid}/changes", get(mrs::changes))
        .route("/projects/{id}/merge_requests/{iid}/diffs", get(mrs::diffs))
        .route("/projects/{id}/merge_requests/{iid}/commits", get(mrs::commits))
        .route("/projects/{id}/merge_trains", get(mrs::merge_trains))
        .route("/projects/{id}/merge_trains/merge_requests/{iid}", get(mrs::merge_train_car).post(mrs::add_to_merge_train))
        .route("/projects/{id}/merge_trains/{target_branch}", get(mrs::merge_train))
        .route("/projects/{id}/merge_requests/{iid}/cancel_merge_when_pipeline_succeeds", post(mrs::cancel_auto_merge))
        .route("/projects/{id}/merge_requests/{iid}/approve", post(mrs::approve))
        .route("/projects/{id}/merge_requests/{iid}/unapprove", post(mrs::unapprove))
        .route("/projects/{id}/merge_requests/{iid}/approval_rules", get(mrs::approval_rules))
        .route("/projects/{id}/merge_requests/{iid}/approval_state", get(mrs::approval_state))
        .route("/projects/{id}/merge_requests/{iid}/notes", get(mrs::notes).post(mrs::create_note))
        .route("/projects/{id}/merge_requests/{iid}/notes/{note_id}", put(mrs::edit_note).delete(mrs::delete_note))
        .route("/projects/{id}/merge_requests/{iid}/discussions", get(mrs::discussions).post(mrs::create_discussion))
        .route("/projects/{id}/merge_requests/{iid}/discussions/{discussion}/notes", post(mrs::reply))
        .route("/projects/{id}/issues", get(issues::list).post(issues::create))
        .route("/projects/{id}/issues/{iid}", get(issues::show).put(issues::update))
        .route("/projects/{id}/issues/{iid}/notes", get(issues::notes).post(issues::create_note))
        .route("/projects/{id}/pipelines", get(ci::list))
        .route("/projects/{id}/pipelines/latest", get(ci::latest))
        .route("/projects/{id}/pipelines/{pipeline}", get(ci::show))
        .route("/projects/{id}/pipelines/{pipeline}/jobs", get(ci::jobs))
        .route("/projects/{id}/jobs/{job_id}/artifacts/{*artifact_path}", get(ci::artifact_file))
}

/// A `*_username` filter as an id: an unknown name matches nobody (-1).
pub async fn user_filter(state: &AppState, name: &str) -> crate::error::ApiResult<i64> {
    Ok(forgust_db::users::find_by_username(&state.db, name).await?.map(|u| u.id).unwrap_or(-1))
}

/// A field that may be absent (`None`), `null` (`Some(None)`) or set: GitLab
/// clears `due_date` and `milestone_id` with `null`.
pub fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// `page` and `per_page` as GitLab takes them (20 per page, at most 100).
#[derive(Deserialize, Default, Clone)]
pub struct PageQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

impl PageQuery {
    pub fn page(&self) -> i64 {
        self.page.unwrap_or(1).max(1)
    }
    pub fn per_page(&self) -> i64 {
        crate::api::page_window(self.page(), self.per_page.unwrap_or(20)).0
    }
    pub fn offset(&self) -> i64 {
        crate::api::page_window(self.page(), self.per_page.unwrap_or(20)).1
    }
    /// One more than a page, to know whether another page follows.
    pub fn probe(&self) -> i64 {
        self.per_page() + 1
    }
}

/// A page of `items` (fetched with `probe()`), with GitLab's pagination headers.
/// `total` is sent when known.
pub fn paged<T: Serialize>(state: &AppState, uri: &Uri, q: &PageQuery, mut items: Vec<T>, total: Option<i64>) -> Response {
    let per = q.per_page();
    let page = q.page();
    let more = match total {
        Some(t) => page * per < t,
        None => items.len() as i64 > per,
    };
    items.truncate(per as usize);
    let mut resp = axum::Json(items).into_response();
    let h = resp.headers_mut();
    let num = |n: i64| HeaderValue::from(n);
    h.insert("x-page", num(page));
    h.insert("x-per-page", num(per));
    h.insert("x-next-page", if more { num(page + 1) } else { HeaderValue::from_static("") });
    h.insert("x-prev-page", if page > 1 { num(page - 1) } else { HeaderValue::from_static("") });
    if let Some(t) = total {
        h.insert("x-total", num(t));
        h.insert("x-total-pages", num(((t + per - 1) / per).max(1)));
    }
    let link_to = |p: i64, rel: &str| format!("<{}>; rel=\"{rel}\"", page_url(state, uri, p, per));
    let mut links = Vec::new();
    if page > 1 {
        links.push(link_to(page - 1, "prev"));
    }
    if more {
        links.push(link_to(page + 1, "next"));
    }
    links.push(link_to(1, "first"));
    if let Some(t) = total {
        links.push(link_to(((t + per - 1) / per).max(1), "last"));
    }
    if let Ok(v) = HeaderValue::from_str(&links.join(", ")) {
        h.insert("link", v);
    }
    resp
}

/// The same request on page `page`, as an absolute URL on the external address
/// (Renovate ignores next links on another host).
fn page_url(state: &AppState, uri: &Uri, page: i64, per: i64) -> String {
    let base = state.cfg.server.external_url.as_str();
    let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .filter(|(k, _)| k != "page" && k != "per_page")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.push(("page".into(), page.to_string()));
    pairs.push(("per_page".into(), per.to_string()));
    let query = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish();
    format!("{base}{}?{query}", uri.path())
}

/// GitLab sends labels as a comma-separated string (or an array in JSON).
#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum Labels {
    List(Vec<String>),
    Csv(String),
}

impl Labels {
    pub fn names(&self) -> Vec<String> {
        let raw: Vec<String> = match self {
            Labels::List(v) => v.clone(),
            Labels::Csv(s) => s.split(',').map(str::to_string).collect(),
        };
        raw.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    }
}

/// User ids as GitLab clients send them: a JSON array, one number, or `1,2`.
/// `0` (GitLab's "nobody") is dropped.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Ids {
    List(Vec<i64>),
    One(i64),
    Csv(String),
}

impl Ids {
    pub fn ids(&self) -> Vec<i64> {
        let raw: Vec<i64> = match self {
            Ids::List(v) => v.clone(),
            Ids::One(n) => vec![*n],
            Ids::Csv(s) => s.split(',').filter_map(|x| x.trim().parse().ok()).collect(),
        };
        raw.into_iter().filter(|n| *n > 0).collect()
    }
}

/// `name[]=1&name[]=2` or `name=1,2` in a query string (Renovate puts
/// `assignee_ids[]` there); None when the parameter is absent.
pub fn query_ids(uri: &axum::http::Uri, name: &str) -> Option<Vec<i64>> {
    let q = uri.query()?;
    let mut found = false;
    let mut out = Vec::new();
    for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
        if k == name || k == format!("{name}[]") {
            found = true;
            out.extend(v.split(',').filter_map(|x| x.trim().parse::<i64>().ok()).filter(|n| *n > 0));
        }
    }
    found.then_some(out)
}

/// Apply `labels` (replace) or `add_labels`/`remove_labels` to a current set.
pub fn relabel(current: &[String], labels: Option<&Labels>, add: Option<&Labels>, remove: Option<&Labels>) -> Option<Vec<String>> {
    if labels.is_none() && add.is_none() && remove.is_none() {
        return None;
    }
    let mut out: Vec<String> = labels.map(Labels::names).unwrap_or_else(|| current.to_vec());
    for n in add.map(Labels::names).unwrap_or_default() {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    let gone = remove.map(Labels::names).unwrap_or_default();
    out.retain(|n| !gone.contains(n));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_parse_every_shape() {
        assert_eq!(Ids::List(vec![3, 0, 4]).ids(), vec![3, 4]);
        assert_eq!(Ids::One(0).ids(), Vec::<i64>::new());
        assert_eq!(Ids::Csv("1, 2".into()).ids(), vec![1, 2]);
        let uri: axum::http::Uri = "/x?assignee_ids[]=5&assignee_ids[]=6&b=1".parse().unwrap();
        assert_eq!(query_ids(&uri, "assignee_ids"), Some(vec![5, 6]));
        assert_eq!(query_ids(&uri, "reviewer_ids"), None);
    }

    #[test]
    fn labels_parse_both_shapes() {
        assert_eq!(Labels::Csv("a, b,,c".into()).names(), vec!["a", "b", "c"]);
        assert_eq!(Labels::List(vec!["x".into()]).names(), vec!["x"]);
    }

    #[test]
    fn relabel_adds_and_removes() {
        let cur = vec!["a".to_string(), "b".to_string()];
        assert_eq!(relabel(&cur, None, None, None), None);
        assert_eq!(relabel(&cur, None, Some(&Labels::Csv("c".into())), Some(&Labels::Csv("a".into()))).unwrap(), vec!["b", "c"]);
        assert_eq!(relabel(&cur, Some(&Labels::Csv("z".into())), None, None).unwrap(), vec!["z"]);
    }
}
