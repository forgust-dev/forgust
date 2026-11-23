//! The Forgust bot (ADR-0026): commands in comments, carried out by the
//! server itself. A project turns it on; `forgust-bot`, one account of the
//! instance, joins as a Developer and from then on reads every comment on
//! the project's issues and merge requests for lines such as `/label ~bug`
//! or `/assign @alice` ([`parse`]).
//!
//! Who may run a command: whoever could do the same from the page, and
//! whoever `.forgust/bot.toml` adds ([`config`]), read from the target
//! branch of a merge request (the default branch for an issue), never from
//! the merge request's own changes. Triage commands are carried out as the
//! bot; `/merge`, `/rebase` and `/retest` as the commenter, with their own
//! rights: they make commits and pipelines in a person's name.
//!
//! A command that worked leaves its system note and nothing else; what was
//! refused or unknown gets one reply per comment, saying why. Every command
//! is audited (`bot_command`) as the bot, with who asked. The bot never
//! reads its own comments.
//!
//! It also labels merge requests by the same file's `[labels]` rules
//! ([`labels`]).

pub mod config;
pub mod labels;
pub mod parse;

use forgust_db::access::{DEVELOPER, REPORTER};
use forgust_db::issues::Issue;
use forgust_db::merge_requests::{MergeRequest, Position};
use forgust_db::projects::Project;
use forgust_db::users::{NewUser, User};
use serde_json::json;

use self::config::{Allow, BotConfig};
use self::parse::{Command, Kind};
use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, can_project, project_role};
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::state::AppState;

pub const USERNAME: &str = "forgust-bot";
pub const BOT_TYPE: &str = "forgust_bot";

/// What a comment was written on.
#[derive(Clone, Copy)]
pub enum Target<'a> {
    Issue(&'a Issue),
    MergeRequest(&'a MergeRequest),
}

impl Target<'_> {
    fn author_id(&self) -> Option<i64> {
        match self {
            Target::Issue(i) => i.author_id,
            Target::MergeRequest(m) => m.author_id,
        }
    }
}

// ------------------------------------------------------------------ settings

/// The instance's bot account, created on first use: no password, no token.
pub async fn account(state: &AppState) -> ApiResult<User> {
    for _ in 0..2 {
        match forgust_db::users::find_by_username(&state.db, USERNAME).await? {
            Some(u) if u.user_type == BOT_TYPE => {
                if u.avatar_at.is_none()
                    && let Err(e) = super::avatars::set(state, super::avatars::Owner::User(u.id), &super::renovate::bot_avatar()).await
                {
                    tracing::warn!(error = ?e, "bot avatar not set");
                }
                return Ok(u);
            }
            Some(_) => return Err(ApiError::conflict(format!("the username `{USERNAME}` is taken: rename that account first"))),
            None => {}
        }
        let mut conn = state.db.acquire().await?;
        let new = NewUser {
            username: USERNAME,
            email: &format!("{USERNAME}@noreply.forgust.local"),
            name: "Forgust bot",
            password_hash: None,
            user_type: BOT_TYPE,
            bot_source: None,
            is_admin: false,
            email_verified: true,
        };
        // made by another request meanwhile: found on the second pass
        if let Err(e) = forgust_db::users::insert(&mut conn, new).await {
            tracing::debug!(error = %e, "bot account not created (a concurrent request?)");
        }
    }
    Err(ApiError::Internal(anyhow::anyhow!("the bot account could not be created")))
}

/// The bot, a Developer of the project (unless a Maintainer gave it another role).
async fn member(state: &AppState, project: &Project) -> ApiResult<User> {
    let bot = account(state).await?;
    if forgust_db::members::level_of(&state.db, "project", project.id, bot.id).await?.is_none() {
        let mut tx = state.db.begin().await?;
        forgust_db::members::upsert(&mut *tx, "project", project.id, bot.id, DEVELOPER, None, None).await?;
        forgust_db::members::refresh_authorizations(&mut tx, &[bot.id]).await?;
        tx.commit().await?;
    }
    Ok(bot)
}

/// Whether the bot reads this project's comments.
pub async fn is_on(state: &AppState, project_id: i64) -> ApiResult<bool> {
    Ok(state.cfg.bot.enabled && forgust_db::bot::enabled(&state.db, project_id).await? == Some(true))
}

/// Turn the bot on or off for a project. On: it joins as a Developer. Off: it
/// leaves (what it did stays).
pub async fn configure(state: &AppState, principal: &Principal, project: &Project, enabled: bool) -> ApiResult<()> {
    if enabled && !state.cfg.bot.enabled {
        return Err(ApiError::conflict("the Forgust bot is turned off on this instance"));
    }
    let was_on = forgust_db::bot::enabled(&state.db, project.id).await? == Some(true);
    if enabled {
        member(state, project).await?;
    } else if let Some(bot) = forgust_db::users::find_by_username(&state.db, USERNAME).await?.filter(|u| u.user_type == BOT_TYPE) {
        let mut tx = state.db.begin().await?;
        forgust_db::members::remove(&mut *tx, "project", project.id, bot.id).await?;
        forgust_db::members::refresh_authorizations(&mut tx, &[bot.id]).await?;
        tx.commit().await?;
    }
    forgust_db::bot::set_enabled(&state.db, project.id, enabled, principal.direct_user().map(|u| u.id)).await?;
    if enabled != was_on {
        let action = if enabled { "bot_enabled" } else { "bot_disabled" };
        super::audit(state, principal, action, Some(("project", project.id)), json!({}), None).await;
    }
    Ok(())
}

/// A group's default for its projects (those that never chose, and new ones).
pub async fn set_group_default(
    state: &AppState,
    principal: &Principal,
    ns: &forgust_db::namespaces::Namespace,
    enabled: bool,
) -> ApiResult<usize> {
    super::builtin::set_group_default::<Bot>(state, principal, ns, enabled).await
}

/// A new project under a group that turned the bot on gets it.
pub async fn on_project_created(state: &AppState, principal: &Principal, project: &Project) {
    super::builtin::on_project_created::<Bot>(state, principal, project).await
}

/// The bot among the built-ins: the per-project switch and the groups'
/// defaults are shared; nothing is ever due (its scheduled rules come later,
/// ADR-0026).
struct Bot;

impl super::builtin::Builtin for Bot {
    const FEATURE: &'static str = "the Forgust bot";
    const RUN: &'static str = "bot sweep";
    const GROUP_AUDIT: &'static str = "bot_group_default";

    async fn limits(state: &AppState) -> anyhow::Result<Option<(i64, f64)>> {
        Ok(state.cfg.bot.enabled.then_some((0, 0.0)))
    }
    async fn stale(_: &AppState, _: f64) -> anyhow::Result<Vec<i64>> {
        Ok(Vec::new())
    }
    async fn active(_: &AppState) -> anyhow::Result<i64> {
        Ok(0)
    }
    async fn due(_: &AppState, _: i64) -> anyhow::Result<Vec<i64>> {
        Ok(Vec::new())
    }
    async fn run(_: &AppState, _: &Project) -> ApiResult<Option<i64>> {
        Ok(None)
    }
    async fn later(_: &AppState, _: i64, _: chrono::DateTime<chrono::Utc>) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_group(state: &AppState, namespace_id: i64, enabled: bool) -> anyhow::Result<()> {
        Ok(forgust_db::bot::set_group_default(&state.db, namespace_id, enabled).await?)
    }
    async fn undecided_under(state: &AppState, namespace_id: i64) -> anyhow::Result<Vec<i64>> {
        Ok(forgust_db::bot::undecided_projects_under(&state.db, namespace_id).await?)
    }
    async fn inherited(state: &AppState, traversal_ids: &[i64]) -> anyhow::Result<bool> {
        Ok(forgust_db::bot::inherited_default(&state.db, traversal_ids).await?)
    }
    async fn turn_on(state: &AppState, principal: &Principal, project: &Project) -> ApiResult<()> {
        configure(state, principal, project, true).await
    }
}

/// The rules at `branch` (None: the default branch): the file's, the
/// defaults when there is none, or what is wrong with it.
pub(crate) async fn rules(state: &AppState, p: &Project, branch: Option<&str>) -> ApiResult<Result<BotConfig, String>> {
    let Some(branch) = branch.or(p.default_branch.as_deref()) else { return Ok(Ok(BotConfig::default())) };
    let Some(commit) = state.storage.resolve(p.id, &format!("refs/heads/{branch}")).await? else { return Ok(Ok(BotConfig::default())) };
    match state.storage.blob(p.id, &commit, config::PATH, config::MAX_BYTES).await {
        Ok(b) if b.truncated => Ok(Err(format!("it is larger than {} KiB", config::MAX_BYTES / 1024))),
        Ok(b) => Ok(match std::str::from_utf8(&b.data) {
            Ok(text) => config::parse(text),
            Err(_) => Err("it is not UTF-8 text".into()),
        }),
        Err(forgust_storage::StorageError::PathNotFound(_)) => Ok(Ok(BotConfig::default())),
        Err(e) => Err(e.into()),
    }
}

// ------------------------------------------------------------------ comments

/// What the comment services await: boxed, since the bot's reply goes
/// through them again (and stops there: it is the bot's).
type Reading<'a> = std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// A comment was written on an issue: run its commands.
pub fn on_issue_note<'a>(
    state: &'a AppState,
    principal: &'a Principal,
    p: &'a Project,
    issue: &'a Issue,
    body: &'a str,
    internal: bool,
) -> Reading<'a> {
    Box::pin(on_note(state, principal, p, Target::Issue(issue), body, internal))
}

/// A comment was written on a merge request: run its commands.
pub fn on_mr_note<'a>(
    state: &'a AppState,
    principal: &'a Principal,
    p: &'a Project,
    mr: &'a MergeRequest,
    body: &'a str,
    internal: bool,
) -> Reading<'a> {
    Box::pin(on_note(state, principal, p, Target::MergeRequest(mr), body, internal))
}

/// The comment is saved whatever happens here: a failure is logged.
async fn on_note(state: &AppState, principal: &Principal, p: &Project, target: Target<'_>, body: &str, internal: bool) {
    // people's and tokens' comments; never the bot's own (no loops)
    let Some(user) = principal.direct_user().filter(|u| u.user_type != BOT_TYPE) else { return };
    let parsed = parse::parse(body);
    if parsed.is_empty() {
        return;
    }
    if let Err(e) = run(state, principal, user, p, target, parsed, internal).await {
        tracing::warn!(project_id = p.id, error = ?e, "bot: a comment's commands were not run");
    }
}

/// What the bot knows while it runs one comment's commands.
struct Ctx<'a> {
    state: &'a AppState,
    /// Who wrote the comment.
    principal: &'a Principal,
    user: &'a User,
    /// The bot, with its membership's rights.
    bot: Principal,
    bot_id: i64,
    p: &'a Project,
    rules: BotConfig,
}

/// A command that was not carried out, and why (shown to the commenter).
type Outcome = ApiResult<Result<(), String>>;

fn refused(why: impl Into<String>) -> Outcome {
    Ok(Err(why.into()))
}

async fn run(
    state: &AppState,
    principal: &Principal,
    user: &User,
    p: &Project,
    target: Target<'_>,
    parsed: parse::Parsed,
    internal: bool,
) -> ApiResult<()> {
    if !is_on(state, p.id).await? {
        return Ok(());
    }
    let bot = member(state, p).await?;
    let branch = match target {
        Target::MergeRequest(m) => Some(m.target_branch.as_str()),
        Target::Issue(_) => None,
    };
    let mut problems: Vec<String> = Vec::new();
    match rules(state, p, branch).await? {
        Err(e) => problems.push(format!("no command was run: `{}` cannot be used ({e})", config::PATH)),
        Ok(rules) => {
            let ctx = Ctx { state, principal, user, bot: Principal::member(bot.clone()), bot_id: bot.id, p, rules };
            for c in &parsed.commands {
                let outcome = match ctx.command(target, c).await {
                    Ok(o) => o,
                    // the services' own refusals are answers too
                    Err(e @ (ApiError::BadRequest(_) | ApiError::Forbidden(_) | ApiError::Conflict(_) | ApiError::TooLarge(_))) => {
                        Err(e.to_string())
                    }
                    Err(ApiError::NotFound) => Err("not found".to_string()),
                    Err(e) => {
                        tracing::warn!(project_id = p.id, command = c.kind.name(), error = ?e, "bot: command failed");
                        Err("something went wrong on the server".to_string())
                    }
                };
                ctx.audit(target, c, outcome.as_ref().err()).await;
                if let Err(why) = outcome {
                    problems.push(format!("`{}`: {why}", written(c)));
                }
            }
        }
    }
    for u in &parsed.unknown {
        problems.push(format!("`/{}`: there is no such command", u.0));
    }
    if parsed.dropped > 0 {
        problems.push(format!("{} more command(s) were left out: at most {} per comment", parsed.dropped, parse::MAX_COMMANDS));
    }
    if problems.is_empty() {
        return Ok(());
    }
    let list = problems.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n");
    let mut reply = format!("@{} I could not do everything you asked:\n\n{list}", user.username);
    if !parsed.unknown.is_empty() {
        let known = Kind::ALL.iter().map(|k| format!("`/{}`", k.name())).collect::<Vec<_>>().join(", ");
        reply.push_str(&format!("\n\nThe commands are {known}."));
    }
    let bot = Principal::member(bot);
    // an answer to an internal comment is internal: it quotes it
    match target {
        Target::Issue(i) => {
            super::issues::add_note(state, &bot, p, i, &reply, internal).await?;
        }
        Target::MergeRequest(m) => {
            super::mr::add_note(state, &bot, p, m, &reply, None, None, internal).await?;
        }
    }
    Ok(())
}

/// The command as it is quoted back: `/label ~bug`.
fn written(c: &Command) -> String {
    let sigil = match c.kind {
        Kind::Assign | Kind::Unassign | Kind::Cc => "@",
        Kind::Label | Kind::Unlabel => "~",
        Kind::Milestone => "%",
        _ => "",
    };
    let args: Vec<String> = c
        .args
        .iter()
        .map(|a| match a.contains(char::is_whitespace) {
            true => format!(" {sigil}\"{}\"", a.replace('`', "'")),
            false if a == "me" => " me".to_string(),
            false => format!(" {sigil}{}", a.replace('`', "'")),
        })
        .collect();
    format!("/{}{}", c.kind.name(), args.concat())
}

impl Ctx<'_> {
    async fn audit(&self, target: Target<'_>, c: &Command, refused: Option<&String>) {
        let mut details = json!({
            "command": c.kind.name(),
            "args": c.args,
            "by": self.user.id,
            "outcome": if refused.is_some() { "refused" } else { "done" },
        });
        match target {
            Target::Issue(i) => details["issue"] = json!(i.iid),
            Target::MergeRequest(m) => details["merge_request"] = json!(m.iid),
        }
        if let Some(why) = refused {
            details["reason"] = json!(why);
        }
        super::write_audit(self.state, "bot", Some(self.bot_id), "bot_command", Some(("project", self.p.id)), details, None).await;
    }

    /// One command, on the issue or merge request as it is now (an earlier
    /// command of the same comment may have changed it).
    async fn command(&self, target: Target<'_>, c: &Command) -> Outcome {
        let name = c.kind.name();
        if !self.rules.command(c.kind).enabled {
            return refused(format!("`/{name}` is turned off in this project (`{}`)", config::PATH));
        }
        match target {
            Target::Issue(_) if c.kind.merge_request_only() => refused(format!("`/{name}` works on merge requests only")),
            Target::Issue(i) => {
                let i = forgust_db::issues::find(&self.state.db, self.p.id, i.iid).await?.ok_or(ApiError::NotFound)?;
                if !self.allowed(Target::Issue(&i), c.kind).await? {
                    return refused(format!("you are not allowed to run `/{name}` here"));
                }
                self.on_issue(&i, c).await
            }
            Target::MergeRequest(m) => {
                let m = forgust_db::merge_requests::find(&self.state.db, self.p.id, m.iid).await?.ok_or(ApiError::NotFound)?;
                if c.kind.as_commenter() {
                    return self.as_commenter(&m, c.kind).await;
                }
                if !self.allowed(Target::MergeRequest(&m), c.kind).await? {
                    return refused(format!("you are not allowed to run `/{name}` here"));
                }
                self.on_mr(&m, c).await
            }
        }
    }

    /// Whether the commenter may run a triage command: they could do the same
    /// from the page, or `.forgust/bot.toml` lets them.
    async fn allowed(&self, target: Target<'_>, kind: Kind) -> ApiResult<bool> {
        let (state, principal, p) = (self.state, self.principal, self.p);
        let role = project_role(state, principal, p).await?.level;
        let own = match (kind, target) {
            (Kind::Label | Kind::Unlabel | Kind::Milestone | Kind::RemoveMilestone, _) => {
                can_project(state, principal, ProjectAction::Push, p).await?
            }
            (Kind::Assign | Kind::Unassign, Target::Issue(i)) => role >= REPORTER || i.author_id == Some(self.user.id),
            (Kind::Close | Kind::Reopen, Target::Issue(i)) => super::issues::may_edit(state, principal, p, i).await.is_ok(),
            (_, Target::MergeRequest(m)) => super::mr::may_edit(state, principal, p, m).await.is_ok(),
            (_, Target::Issue(_)) => false,
        };
        if own {
            return Ok(true);
        }
        for a in self.rules.command(kind).allow {
            let yes = match (a, target) {
                (Allow::Anyone, _) => true,
                (Allow::Author, t) => t.author_id() == Some(self.user.id),
                (Allow::Role(level), _) => role >= level,
                (Allow::CodeOwners, Target::MergeRequest(m)) => {
                    let owners = super::codeowners::for_merge_request(state, p, m).await?;
                    owners.file_owners.values().any(|names| names.contains(&self.user.username))
                }
                (Allow::CodeOwners, Target::Issue(_)) => false,
            };
            if yes {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `@name` and `me` → user ids, or the first name that is nobody's.
    async fn people(&self, args: &[String]) -> ApiResult<Result<Vec<i64>, String>> {
        let mut ids = Vec::new();
        for a in args {
            if a == "me" {
                ids.push(self.user.id);
                continue;
            }
            match forgust_db::users::find_by_username(&self.state.db, a).await? {
                Some(u) => ids.push(u.id),
                None => return Ok(Err(format!("there is no user `{}`", a.replace('`', "'")))),
            }
        }
        Ok(Ok(ids))
    }

    /// `current` with `ids` added or taken out (no `ids` to take out: all go).
    fn changed(current: Vec<i64>, ids: &[i64], add: bool) -> Vec<i64> {
        match (add, ids.is_empty()) {
            (true, _) => current.iter().chain(ids).copied().collect(),
            (false, true) => Vec::new(),
            (false, false) => current.into_iter().filter(|id| !ids.contains(id)).collect(),
        }
    }

    /// Label names, the same way.
    fn relabeled(current: Vec<String>, names: &[String], add: bool) -> Vec<String> {
        match (add, names.is_empty()) {
            (true, _) => current.iter().chain(names).cloned().collect(),
            (false, true) => Vec::new(),
            (false, false) => current.into_iter().filter(|n| !names.contains(n)).collect(),
        }
    }

    /// The milestone a `/milestone` names (its id).
    async fn milestone(&self, args: &[String]) -> ApiResult<Result<i64, String>> {
        if args.is_empty() {
            return Ok(Err("which milestone? `/milestone %\"1.0\"` (`/remove_milestone` takes it off)".into()));
        }
        // `/milestone 1.0 beta`, written without quotes
        let title = args.join(" ");
        let found = forgust_db::milestones::ids_by_title(&self.state.db, self.p.id).await?.into_iter().find(|(t, _)| *t == title);
        Ok(found.map(|(_, id)| id).ok_or_else(|| format!("there is no milestone `{}` in this project", title.replace('`', "'"))))
    }

    async fn on_issue(&self, i: &Issue, c: &Command) -> Outcome {
        let (state, bot, p) = (self.state, &self.bot, self.p);
        match c.kind {
            Kind::Assign | Kind::Unassign => {
                let add = c.kind == Kind::Assign;
                let me = ["me".to_string()];
                let ids = match self.people(if add && c.args.is_empty() { &me } else { &c.args }).await? {
                    Ok(ids) => ids,
                    Err(e) => return refused(e),
                };
                let current = forgust_db::people::issue_assignees(&state.db, &[i.id]).await?.into_iter().map(|(_, u)| u.id).collect();
                super::people::set_issue(state, bot, p, i, &Self::changed(current, &ids, add)).await?;
            }
            Kind::Label | Kind::Unlabel => {
                let add = c.kind == Kind::Label;
                if add && c.args.is_empty() {
                    return refused("which labels? `/label ~bug ~\"needs info\"`");
                }
                let current = forgust_db::issues::labels_of_issues(&state.db, &[i.id]).await?.into_iter().map(|(_, l)| l.name).collect();
                let names = Self::relabeled(current, &c.args, add);
                super::issues::update(state, bot, p, i, super::issues::UpdateIssue { labels: Some(&names), ..Default::default() }).await?;
            }
            Kind::Close | Kind::Reopen => {
                let open = Some(c.kind == Kind::Reopen);
                super::issues::update(state, bot, p, i, super::issues::UpdateIssue { open, ..Default::default() }).await?;
            }
            Kind::Milestone => match self.milestone(&c.args).await? {
                Ok(id) => super::milestones::set_issue(state, bot, p, i, Some(id)).await?,
                Err(e) => return refused(e),
            },
            Kind::RemoveMilestone => super::milestones::set_issue(state, bot, p, i, None).await?,
            Kind::Cc | Kind::Hold | Kind::Unhold | Kind::Retest | Kind::Merge | Kind::Rebase => {
                return refused(format!("`/{}` works on merge requests only", c.kind.name()));
            }
        }
        Ok(Ok(()))
    }

    async fn on_mr(&self, m: &MergeRequest, c: &Command) -> Outcome {
        let (state, bot, p) = (self.state, &self.bot, self.p);
        match c.kind {
            Kind::Assign | Kind::Unassign => {
                let add = c.kind == Kind::Assign;
                let me = ["me".to_string()];
                let ids = match self.people(if add && c.args.is_empty() { &me } else { &c.args }).await? {
                    Ok(ids) => ids,
                    Err(e) => return refused(e),
                };
                let current = forgust_db::people::mr_assignees(&state.db, &[m.id]).await?.into_iter().map(|(_, u)| u.id).collect();
                super::people::set_mr(state, bot, p, m, Some(&Self::changed(current, &ids, add)), None).await?;
            }
            Kind::Cc => {
                if c.args.is_empty() {
                    return refused("who should review? `/cc @alice`");
                }
                let ids = match self.people(&c.args).await? {
                    Ok(ids) => ids,
                    Err(e) => return refused(e),
                };
                let current = forgust_db::people::mr_reviewers(&state.db, &[m.id]).await?.into_iter().map(|(_, r)| r.user.id).collect();
                super::people::set_mr(state, bot, p, m, None, Some(&Self::changed(current, &ids, true))).await?;
            }
            Kind::Label | Kind::Unlabel => {
                let add = c.kind == Kind::Label;
                if add && c.args.is_empty() {
                    return refused("which labels? `/label ~bug ~\"needs info\"`");
                }
                let current = forgust_db::issues::labels_of_mrs(&state.db, &[m.id]).await?.into_iter().map(|(_, l)| l.name).collect();
                super::mr::set_labels(state, bot, p, m, &Self::relabeled(current, &c.args, add)).await?;
            }
            Kind::Close | Kind::Reopen => {
                super::mr::set_open(state, bot, p, m, c.kind == Kind::Reopen).await?;
            }
            Kind::Milestone => match self.milestone(&c.args).await? {
                Ok(id) => super::milestones::set_merge_request(state, bot, p, m, Some(id)).await?,
                Err(e) => return refused(e),
            },
            Kind::RemoveMilestone => super::milestones::set_merge_request(state, bot, p, m, None).await?,
            Kind::Hold | Kind::Unhold => return self.hold(m, c.kind == Kind::Hold).await,
            Kind::Retest | Kind::Merge | Kind::Rebase => return self.as_commenter(m, c.kind).await,
        }
        Ok(Ok(()))
    }

    /// `/hold`: the merge request does not merge (a merge check of its own)
    /// until `/unhold`. Its train lets it go; auto-merge waits.
    async fn hold(&self, m: &MergeRequest, hold: bool) -> Outcome {
        let (state, p) = (self.state, self.p);
        crate::auth::authz::writable(p)?;
        if m.state != "opened" {
            return refused(format!("the merge request is {}", m.state));
        }
        let mut tx = state.db.begin().await?;
        let changed = match hold {
            true => forgust_db::bot::hold(&mut *tx, m.id, self.user.id).await?,
            false => forgust_db::bot::unhold(&mut *tx, m.id).await?,
        };
        if !changed {
            return refused(if hold { "the merge request is already on hold" } else { "the merge request is not on hold" });
        }
        let text = if hold { "put this merge request on hold" } else { "took this merge request off hold" };
        forgust_db::merge_requests::add_note(&mut *tx, m.id, Some(self.bot_id), None, text, true, false, Position::default()).await?;
        forgust_db::merge_requests::touch(&mut *tx, m.id).await?;
        tx.commit().await?;
        state.events.publish(Event::MergeRequest { project_id: p.id, iid: m.iid });
        if hold {
            let why = "removed this merge request from the merge train: it is on hold";
            if let Err(e) = super::merge_train::drop_mr(state, p, m.id, why).await {
                tracing::warn!(mr = m.id, error = %e, "held merge request not taken off its merge train");
            }
        } else {
            super::auto_merge::poke(state, p.id, m.iid).await;
        }
        Ok(Ok(()))
    }

    /// `/merge`, `/rebase`, `/retest`: what the page's buttons do, as the
    /// commenter and with their rights.
    async fn as_commenter(&self, m: &MergeRequest, kind: Kind) -> Outcome {
        let (state, principal, p) = (self.state, self.principal, self.p);
        match kind {
            Kind::Rebase => {
                super::mr::rebase_source(state, principal, p, m).await?;
            }
            Kind::Merge => {
                if forgust_db::ci::settings(&state.db, p.id).await?.merge_trains_enabled {
                    let opts = super::merge_train::TrainOptions { squash: m.squash, rebase: false, remove_source_branch: None };
                    super::merge_train::add(state, principal, p, m, opts).await?;
                } else {
                    // no train here: merge when every check passes (now, when they do)
                    let method = if m.squash { super::mr::MergeMethod::Squash } else { super::mr::MergeMethod::Merge };
                    super::auto_merge::enable(state, principal, p, m, method, None, m.remove_source_branch).await?;
                }
            }
            Kind::Retest => {
                crate::auth::authz::writable(p)?;
                if m.state != "opened" {
                    return refused(format!("the merge request is {}", m.state));
                }
                let head = state.storage.resolve(p.id, &format!("refs/heads/{}", m.source_branch)).await?;
                let Some(head) = head else { return refused(format!("source branch `{}` no longer exists", m.source_branch)) };
                let Some(id) = forgust_db::ci::latest_id_for_sha(&state.db, p.id, &head, m.id).await? else {
                    return refused("no pipeline ran for the latest commit");
                };
                let pipeline = forgust_db::ci::pipeline(&state.db, id).await?.ok_or(ApiError::NotFound)?;
                if !crate::ci::may_run(state, principal, p, &pipeline.git_ref, pipeline.tag).await? {
                    return refused("you cannot run pipelines for this ref");
                }
                let mut retried = 0;
                for b in forgust_db::ci::builds_of_pipeline(&state.db, pipeline.id).await? {
                    if !b.retried && matches!(b.status.as_str(), "failed" | "canceled") {
                        crate::ci::pipeline::retry_job(state, &b, Some(self.user.id)).await.map_err(ApiError::Internal)?;
                        retried += 1;
                    }
                }
                if retried == 0 {
                    return refused(format!("pipeline #{} has no failed job to run again (it is {})", pipeline.iid, pipeline.status));
                }
            }
            _ => unreachable!("only the commands that run as the commenter"),
        }
        Ok(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_quoted_back_as_written() {
        let c = |kind, args: &[&str]| Command { kind, args: args.iter().map(|s| s.to_string()).collect() };
        assert_eq!(written(&c(Kind::Label, &["bug", "needs info"])), "/label ~bug ~\"needs info\"");
        assert_eq!(written(&c(Kind::Assign, &["alice", "me"])), "/assign @alice me");
        assert_eq!(written(&c(Kind::Milestone, &["1.0"])), "/milestone %1.0");
        assert_eq!(written(&c(Kind::Hold, &[])), "/hold");
        // nothing a comment can use to break out of the reply's code span
        assert_eq!(written(&c(Kind::Label, &["a`b"])), "/label ~a'b");
    }

    #[test]
    fn lists_grow_and_shrink() {
        assert_eq!(Ctx::changed(vec![1, 2], &[3], true), vec![1, 2, 3]);
        assert_eq!(Ctx::changed(vec![1, 2], &[2], false), vec![1]);
        assert_eq!(Ctx::changed(vec![1, 2], &[], false), Vec::<i64>::new());
        assert_eq!(Ctx::relabeled(vec!["a".into()], &["b".into()], true), vec!["a".to_string(), "b".to_string()]);
    }
}
