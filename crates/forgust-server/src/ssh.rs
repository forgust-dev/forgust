//! Git over SSH with our own server (russh): key lookup by fingerprint, connection
//! limits, then hand off to git (ADR-0012). Commands: git-upload-pack,
//! git-receive-pack, git-upload-archive, git-lfs-authenticate.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;
use russh::keys::PrivateKey;
use russh::keys::ssh_key::{HashAlg, LineEnding, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Handle, Msg, PublicKeyOffer, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, can_project};
use crate::crypto;
use crate::state::AppState;
use forgust_storage::{GitService, ServiceOptions};

pub fn load_or_create_host_key(path: &Path) -> anyhow::Result<PrivateKey> {
    if path.exists() {
        return Ok(PrivateKey::read_openssh_file(path)?);
    }
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
    let key = PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    key.write_openssh_file(path, LineEnding::LF)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(key)
}

pub async fn serve(state: AppState, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let key = load_or_create_host_key(&state.cfg.server.data_dir.join("ssh_host_ed25519_key"))?;
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::from_millis(500),
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: Some(Duration::from_secs(600)),
        max_auth_attempts: 10,
        methods: russh::MethodSet::from(&[russh::MethodKind::PublicKey][..]),
        ..Default::default()
    });
    tracing::info!(addr = %listener.local_addr()?, "SSH listening");
    let mut server = SshServer { state, active: Arc::new(AtomicUsize::new(0)) };
    server.run_on_socket(config, &listener).await?;
    Ok(())
}

#[derive(Clone)]
struct SshServer {
    state: AppState,
    active: Arc<AtomicUsize>,
}

impl russh::server::Server for SshServer {
    type Handler = Conn;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Conn {
        let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        Conn {
            state: self.state.clone(),
            active: self.active.clone(),
            over_limit: n > self.state.cfg.limits.ssh_max_connections,
            peer,
            principal: None,
            channels: HashMap::new(),
        }
    }
}

struct ChannelState {
    env: Vec<(String, String)>,
    stdin: Option<mpsc::Sender<Vec<u8>>>,
}

pub struct Conn {
    state: AppState,
    active: Arc<AtomicUsize>,
    over_limit: bool,
    peer: Option<std::net::SocketAddr>,
    principal: Option<Principal>,
    channels: HashMap<ChannelId, ChannelState>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn reject() -> Auth {
    Auth::Reject { proceed_with_methods: None, partial_success: false }
}

/// Parse `git-upload-pack 'group/project.git'` into (command, path).
pub fn parse_command(cmd: &str) -> Option<(String, String)> {
    let cmd = cmd.trim();
    let (name, rest) = cmd.split_once(' ')?;
    let mut path = rest.trim();
    // git-lfs-authenticate <path> <operation>
    let mut extra = "";
    if name == "git-lfs-authenticate" {
        let mut parts = path.rsplitn(2, ' ');
        extra = parts.next()?;
        path = parts.next()?.trim();
    }
    let path = path.trim_matches(|c| c == '\'' || c == '"').trim_start_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    if path.is_empty() || path.contains("..") || path.contains('\'') {
        return None;
    }
    let name = if extra.is_empty() { name.to_string() } else { format!("{name} {extra}") };
    Some((name, path.to_string()))
}

impl Conn {
    async fn send_error(&self, handle: &Handle, channel: ChannelId, msg: &str) {
        let _ = handle.extended_data(channel, 1, Bytes::from(format!("Forgust: {msg}\n"))).await;
        let _ = handle.exit_status_request(channel, 1).await;
        let _ = handle.eof(channel).await;
        let _ = handle.close(channel).await;
    }

    async fn exec(&mut self, channel: ChannelId, command: String, session: &mut Session) -> anyhow::Result<()> {
        let handle = session.handle();
        let Some(principal) = self.principal.clone() else {
            self.send_error(&handle, channel, "not authenticated").await;
            return Ok(());
        };
        let Some((cmd, path)) = parse_command(&command) else {
            self.send_error(&handle, channel, "only git commands are supported").await;
            return Ok(());
        };
        let Some((project, moved)) = crate::git_http::find_project(&self.state, &path).await? else {
            self.send_error(
                &handle,
                channel,
                "The project you were looking for could not be found or you don't have permission to view it.",
            )
            .await;
            return Ok(());
        };

        if let Some(op) = cmd.strip_prefix("git-lfs-authenticate ") {
            return self.lfs_authenticate(&handle, channel, &principal, &project, op).await;
        }
        let service = match GitService::from_name(&cmd) {
            Some(s) => s,
            None => {
                self.send_error(&handle, channel, "unknown command").await;
                return Ok(());
            }
        };
        let action = if service == GitService::ReceivePack { ProjectAction::Push } else { ProjectAction::ReadCode };
        let allowed = match can_project(&self.state, &principal, action, &project).await {
            Ok(a) => a,
            Err(crate::error::ApiError::TwoFactorRequired) => {
                self.send_error(&handle, channel, "two-factor authentication is required by this organization").await;
                return Ok(());
            }
            Err(e) => return Err(anyhow::anyhow!("{e}")),
        };
        if !allowed {
            let visible = can_project(&self.state, &principal, ProjectAction::ReadCode, &project).await.unwrap_or(false);
            let msg = if visible && project.archived() {
                crate::auth::authz::ARCHIVED
            } else if visible {
                "You are not allowed to push code to this project."
            } else {
                "The project you were looking for could not be found or you don't have permission to view it."
            };
            self.send_error(&handle, channel, msg).await;
            return Ok(());
        }
        if let Some(new) = &moved {
            let _ =
                handle.extended_data(channel, 1, Bytes::from(format!("Forgust: the project moved to {new}; update your remote.\n"))).await;
        }

        let env = self.channels.get(&channel).map(|c| c.env.clone()).unwrap_or_default();
        let git_protocol = env.iter().find(|(k, _)| k == "GIT_PROTOCOL").map(|(_, v)| v.clone());
        let mut opts = ServiceOptions { git_protocol, ..Default::default() };
        if service == GitService::ReceivePack {
            opts.hooks_path = Some(self.state.hooks.hooks_dir.clone());
            opts.env = crate::hooks::hook_env(&self.state, project.id, &principal, "ssh");
        }
        metrics::counter!("forgust_git_requests_total", "service" => service.name(), "transport" => "ssh").increment(1);
        let lock = if service == GitService::ReceivePack { Some(self.state.repo_locks.get(project.id).read_owned().await) } else { None };
        let mut proc = self.state.storage.spawn_service(project.id, service, opts).await?;

        // client → git stdin
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(32);
        if let Some(c) = self.channels.get_mut(&channel) {
            c.stdin = Some(tx);
        }
        let mut stdin = proc.stdin.take().expect("stdin");
        tokio::spawn(async move {
            while let Some(buf) = rx.recv().await {
                if stdin.write_all(&buf).await.is_err() {
                    break;
                }
            }
            let _ = stdin.shutdown().await;
        });

        // git stderr → SSH extended data
        if let Some(mut err) = proc.stderr.take() {
            let h = handle.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                while let Ok(n) = err.read(&mut buf).await {
                    if n == 0 || h.extended_data(channel, 1, Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                        break;
                    }
                }
            });
        }

        // git stdout → SSH data, then exit status and close
        let mut stdout = std::mem::replace(&mut proc.stdout, Box::pin(tokio::io::empty()));
        tokio::spawn(async move {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if handle.data(channel, Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                            break;
                        }
                    }
                }
            }
            let code = proc.wait().await.ok().and_then(|s| s.code()).unwrap_or(1) as u32;
            drop(lock);
            let _ = handle.exit_status_request(channel, code).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }

    /// Hand git-lfs an HTTP endpoint and a short-lived token.
    async fn lfs_authenticate(
        &self,
        handle: &Handle,
        channel: ChannelId,
        principal: &Principal,
        project: &forgust_db::projects::Project,
        op: &str,
    ) -> anyhow::Result<()> {
        let write = op == "upload";
        let ok = can_project(&self.state, principal, if write { ProjectAction::Push } else { ProjectAction::ReadCode }, project)
            .await
            .unwrap_or(false);
        if !ok {
            self.send_error(handle, channel, "access denied").await;
            return Ok(());
        }
        let token = crypto::random_token(crypto::prefix::LFS_SSH);
        forgust_db::tokens::create_lfs_ssh_token(
            &self.state.db,
            &crypto::token_hash(&token),
            project.id,
            principal.kind(),
            principal.id().unwrap_or(0),
            write,
            Utc::now() + chrono::Duration::minutes(30),
        )
        .await?;
        let basic = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("lfs:{token}"));
        let body = json!({
            "href": format!("{}/info/lfs", self.state.cfg.http_clone_url(&project.full_path)),
            "header": { "Authorization": format!("Basic {basic}") },
            "expires_in": 1800
        });
        let _ = handle.data(channel, Bytes::from(format!("{body}\n"))).await;
        let _ = handle.exit_status_request(channel, 0).await;
        let _ = handle.eof(channel).await;
        let _ = handle.close(channel).await;
        Ok(())
    }
}

impl russh::server::Handler for Conn {
    type Error = anyhow::Error;

    async fn auth_publickey_offered(&mut self, user: &str, _offer: PublicKeyOffer<'_>) -> Result<Auth, Self::Error> {
        Ok(if user == "git" && !self.over_limit { Auth::Accept } else { reject() })
    }

    async fn auth_publickey(&mut self, user: &str, verified: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        let key: &PublicKey = verified.public_key;
        if user != "git" || self.over_limit {
            return Ok(reject());
        }
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        let Some(k) = forgust_db::ssh_keys::find_by_fingerprint(&self.state.db, &fp).await? else {
            tracing::debug!(peer = ?self.peer, %fp, "unknown SSH key");
            return Ok(reject());
        };
        forgust_db::ssh_keys::touch(&self.state.db, k.id).await?;
        self.principal = match (k.kind.as_str(), k.user_id) {
            ("user", Some(uid)) => match forgust_db::users::find(&self.state.db, uid).await? {
                Some(u) if u.is_active() => Some(Principal::User { user: u, scopes: None }),
                _ => None,
            },
            ("deploy", _) => Some(Principal::DeployKey { key_id: k.id }),
            _ => None,
        };
        Ok(if self.principal.is_some() { Auth::Accept } else { reject() })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), ChannelState { env: Vec::new(), stdin: None });
        reply.accept().await;
        Ok(())
    }

    async fn env_request(&mut self, channel: ChannelId, name: &str, value: &str, _session: &mut Session) -> Result<(), Self::Error> {
        if name == "GIT_PROTOCOL"
            && value.len() < 256
            && let Some(c) = self.channels.get_mut(&channel)
        {
            c.env.push((name.to_string(), value.to_string()));
        }
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        let name = match &self.principal {
            Some(p) => p.user().map(|u| format!("@{}", u.username)).unwrap_or_else(|| "deploy key".into()),
            None => "anonymous".into(),
        };
        session.data(channel, Bytes::from(format!("Welcome to Forgust, {name}!\r\n")))?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).to_string();
        session.channel_success(channel)?;
        if let Err(e) = self.exec(channel, command, session).await {
            tracing::warn!(error = %e, "SSH exec failed");
            self.send_error(&session.handle(), channel, "internal error").await;
        }
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], _session: &mut Session) -> Result<(), Self::Error> {
        if let Some(tx) = self.channels.get(&channel).and_then(|c| c.stdin.clone()) {
            let _ = tx.send(data.to_vec()).await;
        }
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, _session: &mut Session) -> Result<(), Self::Error> {
        if let Some(c) = self.channels.get_mut(&channel) {
            c.stdin = None; // closes git's stdin
        }
        Ok(())
    }

    async fn channel_close(&mut self, channel: ChannelId, _session: &mut Session) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_commands() {
        assert_eq!(parse_command("git-upload-pack 'acme/api.git'"), Some(("git-upload-pack".into(), "acme/api".into())));
        assert_eq!(parse_command("git-receive-pack '/acme/sub/api.git'"), Some(("git-receive-pack".into(), "acme/sub/api".into())));
        assert_eq!(
            parse_command("git-lfs-authenticate acme/api.git upload"),
            Some(("git-lfs-authenticate upload".into(), "acme/api".into()))
        );
        assert_eq!(parse_command("git-upload-pack '../etc'"), None);
    }
}
