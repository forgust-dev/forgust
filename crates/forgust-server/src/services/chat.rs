//! Webhooks for chat apps: the same events and payloads as other
//! webhooks, turned into one short message in the shape each app's incoming
//! webhook takes. What is only noise in a channel (a pipeline starting, a job
//! passing) is not sent.

use serde_json::{Value, json};

/// `generic` sends the JSON payload itself (GitLab's shapes).
pub const FORMATS: &[&str] = &["generic", "slack", "mattermost", "discord", "matrix", "teams"];

/// A message: plain text pieces and links, written out per app.
#[derive(Debug, Default)]
struct Msg(Vec<Part>);

#[derive(Debug)]
enum Part {
    Text(String),
    Link(String, String),
    Code(String),
    Break,
}

impl Msg {
    fn t(mut self, s: impl Into<String>) -> Self {
        self.0.push(Part::Text(s.into()));
        self
    }
    fn link(mut self, text: impl Into<String>, url: impl Into<String>) -> Self {
        let url = url.into();
        self.0.push(if url.is_empty() { Part::Text(text.into()) } else { Part::Link(text.into(), url) });
        self
    }
    fn code(mut self, s: impl Into<String>) -> Self {
        self.0.push(Part::Code(s.into()));
        self
    }
    fn br(mut self) -> Self {
        self.0.push(Part::Break);
        self
    }

    /// Slack's mrkdwn: `<url|text>`, with `&`, `<`, `>` escaped.
    fn slack(&self) -> String {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        self.0
            .iter()
            .map(|p| match p {
                Part::Text(s) => esc(s),
                Part::Link(t, u) => format!("<{}|{}>", esc(u), esc(t).replace('|', "¦")),
                Part::Code(s) => format!("`{}`", esc(s).replace('`', "'")),
                Part::Break => "\n".into(),
            })
            .collect()
    }

    /// Markdown (Mattermost, Discord, Teams): `[text](url)`.
    fn markdown(&self) -> String {
        let esc = |s: &str| {
            let mut o = String::with_capacity(s.len());
            for c in s.chars() {
                if "\\`*_[]<>|~#".contains(c) {
                    o.push('\\');
                }
                o.push(c);
            }
            o
        };
        self.0
            .iter()
            .map(|p| match p {
                Part::Text(s) => esc(s),
                Part::Link(t, u) => format!("[{}]({})", esc(t), u.replace(')', "%29").replace(' ', "%20")),
                Part::Code(s) => format!("`{}`", s.replace('`', "'")),
                Part::Break => "\n".into(),
            })
            .collect()
    }

    fn plain(&self) -> String {
        self.0
            .iter()
            .map(|p| match p {
                Part::Text(s) | Part::Code(s) => s.clone(),
                Part::Link(t, u) => format!("{t} ({u})"),
                Part::Break => "\n".into(),
            })
            .collect()
    }

    fn html(&self) -> String {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
        self.0
            .iter()
            .map(|p| match p {
                Part::Text(s) => esc(s),
                Part::Link(t, u) => format!("<a href=\"{}\">{}</a>", esc(u), esc(t)),
                Part::Code(s) => format!("<code>{}</code>", esc(s)),
                Part::Break => "<br>".into(),
            })
            .collect()
    }
}

/// The body to post for this event, or None when it is not worth a message.
pub fn render(format: &str, event: &str, payload: &Value) -> Option<Value> {
    let m = message(event, payload)?;
    Some(match format {
        "slack" => json!({ "text": m.slack(), "unfurl_links": false }),
        "mattermost" => json!({ "text": m.markdown() }),
        // Discord caps a message at 2000 characters
        "discord" => json!({ "content": truncate(&m.markdown(), 1990), "allowed_mentions": { "parse": [] } }),
        // matrix-hookshot's generic webhooks
        "matrix" => json!({ "text": m.plain(), "html": m.html() }),
        // a Teams workflow ("Post to a channel when a webhook request is received")
        "teams" => json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "type": "AdaptiveCard",
                    "version": "1.4",
                    "body": [{ "type": "TextBlock", "text": m.markdown(), "wrap": true }],
                },
            }],
        }),
        _ => return None,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

fn s<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    let mut cur = v;
    for k in path {
        cur = &cur[*k];
    }
    cur.as_str().unwrap_or("")
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// The first line, at most 100 characters.
fn first_line(text: &str) -> String {
    truncate(text.lines().next().unwrap_or("").trim(), 100)
}

fn message(event: &str, p: &Value) -> Option<Msg> {
    let project = s(p, &["project", "path_with_namespace"]);
    let project_url = s(p, &["project", "web_url"]);
    let m = Msg::default();
    match event {
        "push" => {
            let r = s(p, &["ref"]);
            let (kind, name) = match r.strip_prefix("refs/heads/") {
                Some(b) => ("branch", b),
                None => ("tag", r.strip_prefix("refs/tags/").unwrap_or(r)),
            };
            let who = match s(p, &["user_name"]) {
                "" => "Someone".to_string(),
                n => n.to_string(),
            };
            let ref_url = if kind == "branch" { format!("{project_url}/-/tree/{name}") } else { format!("{project_url}/-/tags") };
            let m = m.t(format!("{who} "));
            if s(p, &["after"]).trim_matches('0').is_empty() {
                return Some(m.t(format!("deleted the {kind} ")).code(name).t(" of ").link(project, project_url));
            }
            let commits = p["commits"].as_array().cloned().unwrap_or_default();
            let total = p["total_commits_count"].as_u64().unwrap_or(commits.len() as u64);
            let created = s(p, &["before"]).trim_matches('0').is_empty();
            let mut m = if kind == "tag" {
                m.t("pushed the tag ").link(name, ref_url)
            } else if created {
                m.t("pushed the new branch ").link(name, ref_url)
            } else {
                m.t(format!("pushed {total} commit{} to ", if total == 1 { "" } else { "s" })).link(name, ref_url)
            };
            m = m.t(" of ").link(project, project_url);
            for c in commits.iter().take(5) {
                m = m.br().link(short(s(c, &["id"])), s(c, &["url"])).t(format!(
                    " {} ({})",
                    first_line(s(c, &["message"])),
                    s(c, &["author", "name"])
                ));
            }
            if commits.len() > 5 {
                m = m.br().t(format!("and {} more", total.saturating_sub(5)));
            }
            Some(m)
        }
        "merge_request" => {
            let a = &p["object_attributes"];
            let verb = match s(a, &["action"]) {
                "open" => "opened",
                "reopen" => "reopened",
                "close" => "closed",
                "merge" => "merged",
                "approved" => "approved",
                "unapproved" => "removed their approval of",
                // updates (pushes, edits) are too frequent for a channel
                _ => return None,
            };
            let who = match s(p, &["user", "name"]) {
                "" => "Someone".to_string(),
                n => n.to_string(),
            };
            let iid = a["iid"].as_i64().unwrap_or(0);
            Some(
                m.t(format!("{who} {verb} "))
                    .link(format!("!{iid} {}", first_line(s(a, &["title"]))), s(a, &["url"]))
                    .t(" in ")
                    .link(project, project_url)
                    .br()
                    .code(s(a, &["source_branch"]))
                    .t(" → ")
                    .code(s(a, &["target_branch"])),
            )
        }
        "note" => {
            let mr = &p["merge_request"];
            let iid = mr["iid"].as_i64().unwrap_or(0);
            let author = s(p, &["note", "author"]);
            let url = format!("{project_url}/-/merge_requests/{iid}");
            let body = s(p, &["note", "body"]);
            let excerpt = truncate(&body.split_whitespace().collect::<Vec<_>>().join(" "), 200);
            Some(
                m.t(format!("{author} commented on "))
                    .link(format!("!{iid} {}", first_line(s(mr, &["title"]))), url)
                    .t(" in ")
                    .link(project, project_url)
                    .br()
                    .t(excerpt),
            )
        }
        "release" => {
            if s(p, &["action"]) == "delete" {
                return None;
            }
            let verb = if s(p, &["action"]) == "update" { "updated the release" } else { "New release" };
            Some(
                m.t(format!("{verb} "))
                    .link(s(p, &["name"]), s(p, &["url"]))
                    .t(" of ")
                    .link(project, project_url)
                    .t(" (")
                    .code(s(p, &["tag"]))
                    .t(")"),
            )
        }
        "pipeline" => {
            let a = &p["object_attributes"];
            let status = s(a, &["status"]);
            let word = match status {
                "success" => "passed",
                "failed" => "failed",
                "canceled" => "was canceled",
                // running, pending, created...: only how it ended
                _ => return None,
            };
            let iid = a["iid"].as_i64().or(a["id"].as_i64()).unwrap_or(0);
            let url = match s(a, &["url"]) {
                "" => format!("{project_url}/-/pipelines/{iid}"),
                u => u.to_string(),
            };
            let mut m = m
                .t(if status == "success" {
                    "✅ "
                } else if status == "failed" {
                    "❌ "
                } else {
                    "⏹ "
                })
                .link(format!("Pipeline #{iid}"), url)
                .t(format!(" {word} on "))
                .code(s(a, &["ref"]))
                .t(" of ")
                .link(project, project_url);
            if let Some(mr) = p["merge_request"].as_object() {
                let iid = mr.get("iid").and_then(Value::as_i64).unwrap_or(0);
                m = m.t(" for ").link(format!("!{iid}"), mr.get("url").and_then(Value::as_str).unwrap_or(""));
            }
            let title = first_line(s(p, &["commit", "message"]));
            if !title.is_empty() {
                m = m.br().t(title);
            }
            Some(m)
        }
        "build" => {
            // only failures that matter: a job allowed to fail is not news
            if s(p, &["build_status"]) != "failed" || p["build_allow_failure"].as_bool() == Some(true) {
                return None;
            }
            let url = format!("{project_url}/-/jobs/{}", p["build_id"].as_i64().unwrap_or(0));
            Some(
                m.t("❌ Job ")
                    .link(s(p, &["build_name"]), url)
                    .t(" failed on ")
                    .code(s(p, &["ref"]))
                    .t(" of ")
                    .link(project, project_url)
                    .t(format!(" ({})", s(p, &["build_failure_reason"]).replace('_', " "))),
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Value {
        json!({ "path_with_namespace": "acme/shop", "web_url": "https://forge.example/acme/shop" })
    }

    #[test]
    fn a_merge_request_in_each_app() {
        let p = json!({
            "user": { "name": "Alice <Admin>" },
            "project": project(),
            "object_attributes": { "iid": 12, "title": "Fix *cart* | totals", "action": "open", "url": "https://forge.example/acme/shop/-/merge_requests/12",
                                   "source_branch": "fix/cart", "target_branch": "main" },
        });
        let slack = render("slack", "merge_request", &p).unwrap();
        assert_eq!(
            slack["text"],
            "Alice &lt;Admin&gt; opened <https://forge.example/acme/shop/-/merge_requests/12|!12 Fix *cart* ¦ totals> in <https://forge.example/acme/shop|acme/shop>\n`fix/cart` → `main`"
        );
        let md = render("mattermost", "merge_request", &p).unwrap();
        assert!(
            md["text"]
                .as_str()
                .unwrap()
                .starts_with(r"Alice \<Admin\> opened [!12 Fix \*cart\* \| totals](https://forge.example/acme/shop/-/merge_requests/12)"),
            "{md}"
        );
        let discord = render("discord", "merge_request", &p).unwrap();
        assert_eq!(discord["allowed_mentions"]["parse"], json!([]), "no @everyone");
        let matrix = render("matrix", "merge_request", &p).unwrap();
        assert!(matrix["html"].as_str().unwrap().contains(r#"<a href="https://forge.example/acme/shop">acme/shop</a>"#), "{matrix}");
        assert!(matrix["text"].as_str().unwrap().contains("Alice <Admin> opened"), "{matrix}");
        let teams = render("teams", "merge_request", &p).unwrap();
        assert_eq!(teams["attachments"][0]["content"]["type"], "AdaptiveCard");
        // edits and pushes to it: not for a channel
        let mut upd = p.clone();
        upd["object_attributes"]["action"] = json!("update");
        assert!(render("slack", "merge_request", &upd).is_none());
    }

    #[test]
    fn pushes_pipelines_and_jobs() {
        let push = json!({
            "ref": "refs/heads/main", "before": "a".repeat(40), "after": "b".repeat(40), "user_name": "Bob",
            "project": project(), "total_commits_count": 2,
            "commits": [
                { "id": "1234567890", "message": "Fix totals\n\nlong body", "url": "https://forge.example/acme/shop/-/commit/1234567890", "author": { "name": "Bob" } },
                { "id": "abcdef0123", "message": "Tidy", "url": "", "author": { "name": "Bob" } },
            ],
        });
        let t = render("mattermost", "push", &push).unwrap()["text"].as_str().unwrap().to_string();
        assert!(t.starts_with("Bob pushed 2 commits to [main](https://forge.example/acme/shop/-/tree/main) of [acme/shop]"), "{t}");
        assert!(t.contains("\n[12345678](https://forge.example/acme/shop/-/commit/1234567890) Fix totals (Bob)"), "{t}");
        let mut deleted = push.clone();
        deleted["after"] = json!("0".repeat(40));
        assert!(render("slack", "push", &deleted).unwrap()["text"].as_str().unwrap().starts_with("Bob deleted the branch `main`"));

        let pipeline = |status: &str| json!({ "project": project(), "object_attributes": { "iid": 7, "status": status, "ref": "main" }, "commit": { "message": "Fix totals" } });
        assert!(render("slack", "pipeline", &pipeline("running")).is_none());
        let failed = render("slack", "pipeline", &pipeline("failed")).unwrap();
        assert_eq!(
            failed["text"],
            "❌ <https://forge.example/acme/shop/-/pipelines/7|Pipeline #7> failed on `main` of <https://forge.example/acme/shop|acme/shop>\nFix totals"
        );

        let job = |status: &str, allowed: bool| {
            json!({ "project": project(), "build_id": 3, "build_name": "test", "build_status": status, "build_allow_failure": allowed,
                    "ref": "main", "build_failure_reason": "script_failure" })
        };
        assert!(render("slack", "build", &job("success", false)).is_none());
        assert!(render("slack", "build", &job("failed", true)).is_none());
        assert!(render("slack", "build", &job("failed", false)).unwrap()["text"].as_str().unwrap().contains("(script failure)"));
        assert!(render("generic", "build", &job("failed", false)).is_none(), "generic sends the payload itself");
    }
}
