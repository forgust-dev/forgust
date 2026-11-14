//! Notifications: who hears about what (levels, ADR-0021), then where it goes,
//! a switch per kind and channel: the bell in the app, and email (SMTP, sent
//! from a background job; none when no SMTP URL is configured).

use std::sync::Mutex;

use async_trait::async_trait;
use forgust_db::merge_requests::{MergeRequest, Note};
use forgust_db::projects::Project;
use forgust_db::users::User;
use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};

use crate::auth::authz::{ProjectAction, can_project};
use crate::jobs::SendEmail;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Email {
    /// One address: the message is sent to it. Several (legacy): Bcc.
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Extra headers (List-Unsubscribe, the notification reason…).
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub message_id: Option<String>,
    /// The thread it belongs to (In-Reply-To and References).
    #[serde(default)]
    pub references: Option<String>,
}

#[async_trait]
pub trait Mailer: Send + Sync + 'static {
    async fn send(&self, email: &Email) -> anyhow::Result<()>;
    /// Emails kept in memory (only the in-memory mailer keeps any).
    fn sent(&self) -> Vec<Email> {
        vec![]
    }
}

/// SMTP via lettre: `smtp://user:pass@host:587` (STARTTLS) or `smtps://…` (TLS).
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpMailer {
    pub fn new(url: &str, from: &str) -> anyhow::Result<Self> {
        Ok(SmtpMailer { transport: AsyncSmtpTransport::<Tokio1Executor>::from_url(url)?.build(), from: from.parse()? })
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, email: &Email) -> anyhow::Result<()> {
        use lettre::message::header::{HeaderName, HeaderValue};
        let mut b = Message::builder().from(self.from.clone()).subject(&email.subject);
        if let [one] = &email.to[..] {
            b = b.to(one.parse()?);
        } else {
            for to in &email.to {
                b = b.bcc(to.parse()?);
            }
            b = b.to(self.from.clone());
        }
        if let Some(id) = &email.message_id {
            b = b.message_id(Some(id.clone()));
        }
        if let Some(r) = &email.references {
            b = b.in_reply_to(r.clone()).references(r.clone());
        }
        for (k, v) in &email.headers {
            b = b.raw_header(HeaderValue::new(HeaderName::new_from_ascii(k.clone())?, v.clone()));
        }
        let msg = b.body(email.body.clone())?;
        self.transport.send(msg).await?;
        Ok(())
    }
}

/// Keeps emails in memory (tests) or drops them (email disabled).
#[derive(Default)]
pub struct MemoryMailer {
    pub sent: Mutex<Vec<Email>>,
}

#[async_trait]
impl Mailer for MemoryMailer {
    async fn send(&self, email: &Email) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push(email.clone());
        Ok(())
    }

    fn sent(&self) -> Vec<Email> {
        self.sent.lock().unwrap().clone()
    }
}

async fn queue(state: &AppState, email: Email) {
    queue_all(state, vec![email]).await;
}

/// Each email is its own job (retried on its own), queued in one transaction.
async fn queue_all(state: &AppState, emails: Vec<Email>) {
    let emails: Vec<Email> = emails.into_iter().filter(|e| !e.to.is_empty()).collect();
    if emails.is_empty() {
        return;
    }
    let r: anyhow::Result<()> = async {
        let mut tx = state.db.begin().await?;
        for email in emails {
            state.jobs.enqueue(&mut tx, SendEmail { email }.request()).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(error = %e, "email not queued");
    }
}

/// Why someone hears about something, weakest first. Their level (ADR-0021)
/// decides which reasons reach them: `watch` all, `participating` all but
/// `Watching`, `mention` only `Mention`, `disabled` none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// They watch the project.
    Watching,
    /// They took part (wrote it, commented, approved, are assigned or reviewing).
    Participating,
    /// It was addressed to them (assigned, asked to review, changes requested).
    Direct,
    /// They were @mentioned.
    Mention,
}

pub fn allows(level: &str, r: Reason) -> bool {
    match level {
        "watch" => true,
        "mention" => r == Reason::Mention,
        "disabled" => false,
        _ => r >= Reason::Participating,
    }
}

use forgust_db::notifications::Channel;
pub use forgust_db::notifications::Kind;

/// What a notification points at, by its number in the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    MergeRequest(i64),
    Issue(i64),
    Pipeline(i64),
    /// The project's security findings.
    Security,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::MergeRequest(_) => "merge_request",
            Target::Issue(_) => "issue",
            Target::Pipeline(_) => "pipeline",
            Target::Security => "security",
        }
    }

    pub fn iid(self) -> Option<i64> {
        match self {
            Target::MergeRequest(iid) | Target::Issue(iid) | Target::Pipeline(iid) => Some(iid),
            Target::Security => None,
        }
    }

    /// What a recipient must still be allowed to do in the project to hear
    /// about it: issues are for anyone who sees the project, the rest is code.
    pub fn need(self) -> ProjectAction {
        match self {
            Target::Issue(_) => ProjectAction::Read,
            _ => ProjectAction::ReadCode,
        }
    }
}

/// What the bell shows: the thing, its title, and what happened in a few words
/// ("approved", "commented", "failed").
pub struct About {
    pub kind: Kind,
    pub target: Target,
    pub title: String,
    pub summary: String,
}

impl About {
    /// What a scan of `git_ref` found: it leads to the project's findings.
    pub fn security(git_ref: &str, summary: String) -> About {
        About { kind: Kind::Security, target: Target::Security, title: format!("Security scan of {git_ref}"), summary }
    }

    pub fn mr(kind: Kind, mr: &MergeRequest, summary: &str) -> About {
        About { kind, target: Target::MergeRequest(mr.iid), title: mr.title.clone(), summary: summary.into() }
    }

    pub fn issue(kind: Kind, issue: &forgust_db::issues::Issue, summary: &str) -> About {
        About { kind, target: Target::Issue(issue.iid), title: issue.title.clone(), summary: summary.into() }
    }
}

/// One notification about something in a project.
pub struct Notice<'a> {
    pub project: &'a Project,
    pub about: About,
    /// Who did it: never told about their own action.
    pub actor: Option<i64>,
    pub subject: String,
    pub body: String,
    /// The conversation it belongs to (`issue-<id>`, `merge_request-<id>`), and
    /// whether this message opens it.
    pub thread: Option<String>,
    pub starts_thread: bool,
}

pub fn issue_thread(issue_id: i64) -> Option<String> {
    Some(format!("issue-{issue_id}"))
}

pub fn mr_thread(mr_id: i64) -> Option<String> {
    Some(format!("merge_request-{mr_id}"))
}

impl Reason {
    fn header(self) -> &'static str {
        match self {
            Reason::Watching => "watching",
            Reason::Participating => "participating",
            Reason::Direct => "directly_addressed",
            Reason::Mention => "mentioned",
        }
    }

    fn because(self, project: &str) -> String {
        match self {
            Reason::Watching => format!("you watch {project}"),
            Reason::Participating => "you take part in it".into(),
            Reason::Direct => "it was addressed to you".into(),
            Reason::Mention => "you were mentioned".into(),
        }
    }
}

/// The signed one-click unsubscribe URL for a person and a project (RFC 8058):
/// it turns that project's notifications off for them.
pub fn unsubscribe_path(state: &AppState, user_id: i64, project_id: i64) -> String {
    let sig = state.secrets.sign(&format!("unsubscribe:{user_id}:{project_id}"));
    format!("/api/v1/unsubscribe/{user_id}/{project_id}/{sig}")
}

/// The single way project notifications go out: each person once (with their
/// strongest reason), if their level allows it, if they are an active human and
/// can still see the thing. Then where they chose to get that kind: the bell,
/// and an email of their own when their address is confirmed (ADR-0021).
pub async fn deliver(state: &AppState, n: Notice<'_>, people: Vec<(i64, Reason)>) {
    let mut strongest: std::collections::BTreeMap<i64, Reason> = std::collections::BTreeMap::new();
    for (id, r) in people {
        if Some(id) == n.actor {
            continue;
        }
        let e = strongest.entry(id).or_insert(r);
        *e = (*e).max(r);
    }
    if strongest.is_empty() {
        return;
    }
    let ids: Vec<i64> = strongest.keys().copied().collect();
    let levels = forgust_db::notifications::levels(&state.db, n.project.id, &ids).await.unwrap_or_default();
    let base = state.cfg.server.external_url.clone();
    let host = url::Url::parse(&base).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| "forgust".into());
    let thread_id = n.thread.as_ref().map(|t| format!("<{t}.{}@{host}>", n.project.id));
    let allowed: Vec<i64> = levels.iter().filter(|(id, level)| allows(level, strongest[id])).map(|(id, _)| *id).collect();
    let users = forgust_db::users::find_many(&state.db, &allowed).await.unwrap_or_default();
    let kind = n.about.kind;
    // (not known: nobody is told, rather than people who turned it off)
    let Ok(choices) = forgust_db::notifications::channel_choices(&state.db, &allowed, kind).await else {
        tracing::warn!(kind = kind.as_str(), "notification choices not read: not sent");
        return;
    };
    let on = |user: i64, channel: Channel| {
        forgust_db::notifications::enabled(choices.iter().filter(|(id, ..)| *id == user).map(|(_, c, on)| (c.as_str(), *on)), kind, channel)
    };
    let mut bell = Vec::new();
    let mut emails = Vec::new();
    for u in users {
        let reason = strongest[&u.id];
        if !u.is_active() || u.is_bot() {
            continue;
        }
        let who = crate::auth::Principal::User { user: u.clone(), scopes: None };
        if !can_project(state, &who, n.about.target.need(), n.project).await.unwrap_or(false) {
            continue;
        }
        if on(u.id, Channel::Web) {
            bell.push(u.id);
        }
        if !on(u.id, Channel::Email) || !u.email_verified() {
            continue;
        }
        // one message each: their own reason, their own unsubscribe link (ADR-0021)
        let unsub = format!("{base}{}", unsubscribe_path(state, u.id, n.project.id));
        let page = format!("{base}/unsubscribe?u={}&p={}&s={}", u.id, n.project.id, unsub.rsplit('/').next().unwrap_or(""));
        let footer = format!(
            "\n-- \nYou get this because {}.\nStop emails from {}: {page}\nChoose what you get: {base}/profile/notifications\n",
            reason.because(&n.project.full_path),
            n.project.full_path
        );
        let message_id = match (&thread_id, n.starts_thread) {
            (Some(t), true) => Some(t.clone()),
            _ => Some(format!("<{}@{host}>", crate::crypto::random_urlsafe(12))),
        };
        let email = Email {
            to: vec![u.email.clone()],
            subject: n.subject.clone(),
            body: format!("{}{footer}", n.body),
            headers: vec![
                ("List-Id".into(), format!("{} <{}.{host}>", n.project.full_path, n.project.id)),
                ("List-Unsubscribe".into(), format!("<{unsub}>")),
                ("List-Unsubscribe-Post".into(), "List-Unsubscribe=One-Click".into()),
                ("X-Forgust-Notification-Reason".into(), reason.header().into()),
                ("X-Forgust-Project".into(), n.project.full_path.clone()),
            ],
            message_id,
            references: if n.starts_thread { None } else { thread_id.clone() },
        };
        emails.push(email);
    }
    if !bell.is_empty() {
        let target = n.about.target;
        let new = forgust_db::notifications::NewNotification {
            project_id: n.project.id,
            kind,
            target_type: target.as_str(),
            target_iid: target.iid(),
            actor_id: n.actor,
            title: &n.about.title,
            summary: &n.about.summary,
        };
        let actor = match n.actor {
            Some(id) => forgust_db::users::find(&state.db, id).await.ok().flatten(),
            None => None,
        };
        match forgust_db::notifications::add(&state.db, &bell, new).await {
            // each person's streams get theirs, to show at once
            Ok(rows) => {
                for (user_id, id, created_at) in rows {
                    let notification = forgust_db::notifications::Notification {
                        id,
                        kind: kind.as_str().into(),
                        project_id: n.project.id,
                        project_path: n.project.full_path.clone(),
                        target_type: target.as_str().into(),
                        target_iid: target.iid(),
                        title: n.about.title.clone(),
                        summary: n.about.summary.clone(),
                        actor_username: actor.as_ref().map(|u| u.username.clone()),
                        actor_name: actor.as_ref().map(|u| u.name.clone()),
                        read: false,
                        created_at,
                    };
                    state.events.publish(crate::events::Event::Notification {
                        user_id,
                        screen: on(user_id, Channel::Screen),
                        notification,
                    });
                }
            }
            Err(e) => tracing::warn!(error = %e, "notifications not recorded"),
        }
    }
    queue_all(state, emails).await;
}

/// [`deliver`] to these people, for one reason.
#[allow(clippy::too_many_arguments)]
pub async fn tell(
    state: &AppState,
    p: &Project,
    actor: i64,
    users: &[User],
    reason: Reason,
    thread: Option<String>,
    about: About,
    subject: &str,
    body: &str,
) {
    let n = Notice { project: p, about, actor: Some(actor), subject: subject.into(), body: body.into(), thread, starts_thread: false };
    deliver(state, n, users.iter().map(|u| (u.id, reason)).collect()).await;
}

/// Participants of a merge request, then the project's watchers.
async fn mr_audience(state: &AppState, p: &Project, mr: &MergeRequest) -> Vec<(i64, Reason)> {
    let mut out: Vec<(i64, Reason)> = forgust_db::merge_requests::participants(&state.db, mr.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|id| (id, Reason::Participating))
        .collect();
    out.extend(watchers(state, p).await);
    out
}

/// Participants of an issue, then the project's watchers.
async fn issue_audience(state: &AppState, p: &Project, issue_id: i64) -> Vec<(i64, Reason)> {
    let mut out: Vec<(i64, Reason)> = forgust_db::issues::participants(&state.db, issue_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|id| (id, Reason::Participating))
        .collect();
    out.extend(watchers(state, p).await);
    out
}

async fn watchers(state: &AppState, p: &Project) -> Vec<(i64, Reason)> {
    forgust_db::notifications::watchers(&state.db, p.id).await.unwrap_or_default().into_iter().map(|id| (id, Reason::Watching)).collect()
}

/// Participants and watchers hear about a comment, except the people it
/// mentions (they get a mention email of their own).
pub async fn merge_request_note(state: &AppState, p: &Project, mr: &MergeRequest, author: &User, note: &Note, skip: &[i64]) {
    let place = match (&note.new_path, note.new_line.or(note.old_line)) {
        (Some(path), Some(line)) => format!(" on {path}:{line}"),
        _ => String::new(),
    };
    let people = mr_audience(state, p, mr).await.into_iter().filter(|(id, _)| !skip.contains(id)).collect();
    let n = Notice {
        about: About::mr(Kind::Activity, mr, "commented"),
        thread: mr_thread(mr.id),
        starts_thread: false,
        project: p,
        actor: Some(author.id),
        subject: format!("Re: {} !{}: {}", p.full_path, mr.iid, mr.title),
        body: format!("{} commented{place}:\n\n{}\n\n{}\n", author.username, note.body, state.cfg.mr_url(&p.full_path, mr.iid)),
    };
    deliver(state, n, people).await;
}

pub async fn merge_request_state(state: &AppState, p: &Project, mr: &MergeRequest, action: &str, actor: Option<&User>) {
    let who = actor.map(|u| u.username.as_str()).unwrap_or("someone");
    // what the bell says, and the email in full
    let short = match action {
        "merge" => "merged",
        "close" => "closed",
        "reopen" => "reopened",
        other => other,
    };
    let verb = if action == "merge" { format!("merged into {}", mr.target_branch) } else { short.to_string() };
    let n = Notice {
        about: About::mr(Kind::Activity, mr, short),
        thread: mr_thread(mr.id),
        starts_thread: false,
        project: p,
        actor: actor.map(|u| u.id),
        subject: format!("{} !{}: {}", p.full_path, mr.iid, mr.title),
        body: format!("{who} {verb} merge request !{} \"{}\".\n\n{}\n", mr.iid, mr.title, state.cfg.mr_url(&p.full_path, mr.iid)),
    };
    deliver(state, n, mr_audience(state, p, mr).await).await;
}

/// A new merge request: its watchers hear about it.
pub async fn new_merge_request(state: &AppState, p: &Project, mr: &MergeRequest, author: &User) {
    let n = Notice {
        about: About::mr(Kind::Activity, mr, "opened"),
        thread: mr_thread(mr.id),
        starts_thread: true,
        project: p,
        actor: Some(author.id),
        subject: format!("{} !{}: {}", p.full_path, mr.iid, mr.title),
        body: format!(
            "{} opened merge request !{} \"{}\" ({} into {}).\n\n{}\n\n{}\n",
            author.username,
            mr.iid,
            mr.title,
            mr.source_branch,
            mr.target_branch,
            mr.description.trim(),
            state.cfg.mr_url(&p.full_path, mr.iid)
        ),
    };
    deliver(state, n, watchers(state, p).await).await;
}

/// A new issue: its watchers hear about it.
pub async fn new_issue(state: &AppState, p: &Project, issue: &forgust_db::issues::Issue, author: &User) {
    let n = Notice {
        about: About::issue(Kind::Activity, issue, "opened"),
        thread: issue_thread(issue.id),
        starts_thread: true,
        project: p,
        actor: Some(author.id),
        subject: format!("{} #{}: {}", p.full_path, issue.iid, issue.title),
        body: format!(
            "{} opened issue #{} \"{}\".\n\n{}\n\n{}\n",
            author.username,
            issue.iid,
            issue.title,
            issue.description.trim(),
            state.cfg.issue_url(&p.full_path, issue.iid)
        ),
    };
    deliver(state, n, watchers(state, p).await).await;
}

pub async fn issue_note(state: &AppState, p: &Project, issue: &forgust_db::issues::Issue, author: &User, body: &str, skip: &[i64]) {
    let people = issue_audience(state, p, issue.id).await.into_iter().filter(|(id, _)| !skip.contains(id)).collect();
    let n = Notice {
        about: About::issue(Kind::Activity, issue, "commented"),
        thread: issue_thread(issue.id),
        starts_thread: false,
        project: p,
        actor: Some(author.id),
        subject: format!("Re: {} #{}: {}", p.full_path, issue.iid, issue.title),
        body: format!("{} commented:\n\n{}\n\n{}\n", author.username, body, state.cfg.issue_url(&p.full_path, issue.iid)),
    };
    deliver(state, n, people).await;
}

pub async fn issue_state(state: &AppState, p: &Project, issue: &forgust_db::issues::Issue, open: bool, actor: &User) {
    let n = Notice {
        about: About::issue(Kind::Activity, issue, if open { "reopened" } else { "closed" }),
        thread: issue_thread(issue.id),
        starts_thread: false,
        project: p,
        actor: Some(actor.id),
        subject: format!("{} #{}: {}", p.full_path, issue.iid, issue.title),
        body: format!(
            "{} {} issue #{} \"{}\".\n\n{}\n",
            actor.username,
            if open { "reopened" } else { "closed" },
            issue.iid,
            issue.title,
            state.cfg.issue_url(&p.full_path, issue.iid)
        ),
    };
    deliver(state, n, issue_audience(state, p, issue.id).await).await;
}

/// The author hears that someone approved their merge request (asking for
/// changes tells them from `people::request_changes`, with the comment).
pub async fn approved(state: &AppState, p: &Project, mr: &MergeRequest, by: &User) {
    let Some(author) = mr.author_id else { return };
    let n = Notice {
        about: About::mr(Kind::Review, mr, "approved"),
        thread: mr_thread(mr.id),
        starts_thread: false,
        project: p,
        actor: Some(by.id),
        subject: format!("{} !{}: {}", p.full_path, mr.iid, mr.title),
        body: format!(
            "{} approved merge request !{} \"{}\".\n\n{}\n",
            by.username,
            mr.iid,
            mr.title,
            state.cfg.mr_url(&p.full_path, mr.iid)
        ),
    };
    deliver(state, n, vec![(author, Reason::Direct)]).await;
}

/// A pipeline failed, or passed where the one before on its ref failed (or
/// where it failed itself before a retry): who started it hears about it, and
/// the authors of the merge requests it runs for. Not a child pipeline (its
/// parent says it), the instance's own (dependency updates, security scans),
/// nor one canceled or skipped.
pub async fn pipeline_finished(state: &AppState, pipeline: &forgust_db::ci::Pipeline, status: &str) {
    if matches!(pipeline.source.as_str(), "parent_pipeline" | "renovate" | "security") || !matches!(status, "success" | "failed") {
        return;
    }
    // a job retried or played makes the pipeline end again: each outcome is said once
    let Ok(before) = forgust_db::ci::set_notified_status(&state.db, pipeline.id, status).await else { return };
    let (kind, outcome) = match (status, before.as_deref()) {
        (_, Some(same)) if same == status => return,
        ("failed", _) => (Kind::PipelineFailed, "failed"),
        // it failed before and was retried
        (_, Some(_)) => (Kind::PipelineFixed, "passed again"),
        _ => match forgust_db::ci::previous_status(&state.db, pipeline).await {
            Ok(Some(earlier)) if earlier == "failed" => (Kind::PipelineFixed, "passed again"),
            _ => return,
        },
    };
    let Ok(Some(p)) = forgust_db::projects::find(&state.db, pipeline.project_id).await else { return };
    let mrs = match pipeline.merge_request_id {
        Some(id) => forgust_db::merge_requests::find_by_id(&state.db, id).await.ok().flatten().into_iter().collect(),
        None if pipeline.tag => vec![],
        None => forgust_db::merge_requests::open_with_source(&state.db, p.id, &pipeline.git_ref).await.unwrap_or_default(),
    };
    let people: Vec<(i64, Reason)> =
        pipeline.user_id.into_iter().chain(mrs.iter().filter_map(|m| m.author_id)).map(|id| (id, Reason::Direct)).collect();
    let title = format!("Pipeline #{} on {}", pipeline.iid, pipeline.git_ref);
    let commit = pipeline.commit_title.as_deref().unwrap_or("");
    let url = state.cfg.project_page(&p.full_path, &format!("pipelines/{}", pipeline.iid));
    let n = Notice {
        about: About { kind, target: Target::Pipeline(pipeline.iid), title: title.clone(), summary: outcome.into() },
        thread: None,
        starts_thread: false,
        project: &p,
        // who started it is told too
        actor: None,
        subject: format!("{} pipeline #{} {outcome} on {}", p.full_path, pipeline.iid, pipeline.git_ref),
        body: format!("{title} {outcome}.\n\n{} {commit}\n\n{url}\n", &pipeline.sha[..pipeline.sha.len().min(8)]),
    };
    deliver(state, n, people).await;
}

/// One email to one address, whatever its state (password resets, address
/// confirmations).
pub async fn send_to(state: &AppState, to: &str, subject: &str, body: &str) {
    queue(state, Email { to: vec![to.to_string()], subject: subject.into(), body: body.into(), ..Email::default() }).await;
}
