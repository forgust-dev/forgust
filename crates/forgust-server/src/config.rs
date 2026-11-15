//! Configuration: a TOML file plus `FORGUST_` environment variables
//! (`FORGUST_DATABASE__URL` sets `database.url`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub storage: StorageConfig,
    pub blobs: BlobConfig,
    pub security: SecurityConfig,
    pub telemetry: TelemetryConfig,
    pub features: Features,
    pub limits: Limits,
    pub backup: BackupConfig,
    pub email: EmailConfig,
    pub ci: CiConfig,
    pub registry: RegistryConfig,
    pub renovate: RenovateConfig,
    pub pages: PagesConfig,
    pub security_scanning: SecurityScanningConfig,
    pub oidc: BTreeMap<String, OidcProvider>,
}

/// Built-in security scanners (ADR-0024): projects turn them on and
/// Forgust runs them as CI jobs on the instance's runners. Administrators
/// change these in Admin > Security scanning (stored over this). Config
/// section `[security_scanning]`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct SecurityScanningConfig {
    pub enabled: bool,
    /// Code: GitLab's Semgrep-based analyzer (MIT, GitLab's rules).
    pub sast_image: String,
    /// Secrets: GitLab's secrets analyzer (MIT, gitleaks inside).
    pub secrets_image: String,
    /// Dependencies, containers, infrastructure as code: Trivy (Apache-2.0).
    pub trivy_image: String,
    /// Runner tags the jobs ask for (empty: any instance runner taking
    /// untagged jobs). Only instance runners take them.
    pub tags: Vec<String>,
    /// Scans running at once on the instance; due projects wait their turn.
    pub max_concurrent: u32,
    pub timeout_secs: u64,
}

impl Default for SecurityScanningConfig {
    fn default() -> Self {
        SecurityScanningConfig {
            enabled: true,
            sast_image: "registry.gitlab.com/security-products/semgrep:5".into(),
            secrets_image: "registry.gitlab.com/security-products/secrets:6".into(),
            trivy_image: "aquasec/trivy:0.57.1".into(),
            tags: Vec::new(),
            max_concurrent: 2,
            timeout_secs: 3600,
        }
    }
}

/// Static sites from CI (`pages` jobs), served on their own domain: a wildcard
/// DNS record `*.<domain>` pointing at Forgust, a site per top-level namespace
/// (`https://acme.<domain>/project/`). Off while `domain` is empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PagesConfig {
    /// `pages.example.com`; never the host Forgust itself answers on (sites run
    /// visitors' scripts: they must live on another origin).
    pub domain: String,
    /// Sites' links use http instead of https (a test setup).
    pub http: bool,
}

impl PagesConfig {
    pub fn enabled(&self) -> bool {
        !self.domain.is_empty()
    }

    /// `https://<namespace>.<domain>/<rest of the path>/`.
    pub fn url(&self, full_path: &str) -> Option<String> {
        if !self.enabled() {
            return None;
        }
        let (top, rest) = full_path.split_once('/').unwrap_or((full_path, ""));
        let scheme = if self.http { "http" } else { "https" };
        let host = format!("{}.{}", top.to_ascii_lowercase(), self.domain);
        // `<namespace>.<domain>` as a project name: the namespace's root site
        Some(if rest.eq_ignore_ascii_case(&host) || rest.is_empty() {
            format!("{scheme}://{host}/")
        } else {
            format!("{scheme}://{host}/{rest}/")
        })
    }
}

/// Built-in Renovate (ADR-0023): projects turn on dependency updates and
/// Forgust runs Renovate as a CI job on the instance's runners.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RenovateConfig {
    pub enabled: bool,
    /// The image the job runs (the version CI tests against).
    pub image: String,
    /// Runner tags the job asks for (empty: any runner taking untagged jobs).
    /// Only instance runners take Renovate jobs.
    pub tags: Vec<String>,
    /// Renovate runs at once on the instance; due projects wait their turn.
    pub max_concurrent: u32,
    /// The job's timeout, also the run token's lifetime.
    pub timeout_secs: u64,
    /// `RENOVATE_ONBOARDING_CONFIG`: the config the onboarding merge request
    /// proposes (JSON), for example `{"extends": ["config:recommended"]}`.
    pub onboarding_config: Option<String>,
    /// More `RENOVATE_*` (or other) variables for every run, e.g. a global
    /// preset. Not masked: never put secrets here.
    pub extra_env: BTreeMap<String, String>,
    /// A github.com token, so Renovate can fetch changelogs (masked in the job).
    /// Every run has it in its environment, next to what Renovate runs from the
    /// repositories: use a token with no scopes (public data only).
    pub github_com_token: Option<String>,
}

impl Default for RenovateConfig {
    fn default() -> Self {
        RenovateConfig {
            enabled: true,
            image: "renovate/renovate:44.226.3".into(),
            tags: Vec::new(),
            max_concurrent: 2,
            timeout_secs: 3600,
            onboarding_config: None,
            extra_env: BTreeMap::new(),
            github_com_token: None,
        }
    }
}

/// The container registry (ADR-0022), at `/v2/` on `external_url`'s host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RegistryConfig {
    pub enabled: bool,
    /// Largest blob (image layer) accepted.
    pub max_blob_bytes: u64,
    /// Lifetime of the tokens `/jwt/auth` hands out.
    pub token_ttl_secs: u64,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        RegistryConfig { enabled: true, max_blob_bytes: 10 << 30, token_ttl_secs: 900 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CiConfig {
    /// How long `jobs/request` waits for work before answering 204.
    pub long_poll_secs: u64,
    /// Seconds between runner log updates for a running job (sent as
    /// `X-GitLab-Trace-Update-Interval`; 1 s while a cancel is pending).
    pub trace_update_interval_secs: u64,
    pub max_artifact_bytes: u64,
    /// Artifact storage one project may hold (0: no limit). An upload past it
    /// gets 413 and the job fails, as with `max_artifact_bytes`.
    pub max_artifact_bytes_per_project: u64,
    /// When `artifacts:expire_in` is not set (GitLab's default is 30 days).
    pub default_artifact_expire_in: String,
    /// A running job whose runner stays silent this long is failed.
    pub stuck_timeout_secs: u64,
    /// A job whose log grows past this fails (GitLab's default limit is 100 MB too).
    pub max_trace_bytes: u64,
}

impl Default for CiConfig {
    fn default() -> Self {
        CiConfig {
            long_poll_secs: 20,
            trace_update_interval_secs: 3,
            max_artifact_bytes: 1 << 30,
            max_artifact_bytes_per_project: 20 << 30,
            default_artifact_expire_in: "30 days".into(),
            stuck_timeout_secs: 3600,
            max_trace_bytes: 100 << 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub listen: String,
    /// Base URL users see; used in clone URLs, LFS hrefs and OIDC redirects.
    pub external_url: String,
    /// SSH listener; empty disables SSH.
    pub ssh_listen: String,
    /// `host` or `host:port` shown in SSH clone URLs.
    pub ssh_external: String,
    pub data_dir: PathBuf,
    /// Built frontend (`web/dist`). Empty disables the web UI.
    pub static_dir: PathBuf,
    /// Roles this process runs: api, worker, storage (ADR-0012).
    pub roles: Vec<String>,
    /// Binary git hooks call (`forgust hook ...`); defaults to the running executable.
    pub hooks_exe: Option<PathBuf>,
    /// Committer of commits Forgust makes (merges, squashes, rebases).
    pub commit_name: String,
    pub commit_email: String,
    /// Reverse proxies whose `X-Forwarded-For` / `X-Real-IP` is believed. The
    /// client IP (rate limits, audit, sessions) is the last hop not added by
    /// one of these; from anyone else the headers are ignored.
    pub trusted_proxies: Vec<ipnet::IpNet>,
    /// Where the worker asks for the latest Forgust release (GitHub's API, the
    /// shape of `releases/latest`); empty: never ask. Admin > Settings turns it off too.
    pub release_check_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Defaults to `<data_dir>/repositories`.
    pub repos_dir: Option<PathBuf>,
    pub git_bin: PathBuf,
    pub catfile_pool_size: usize,
    /// git processes at once: clones, fetches, archives, blame, log... (0: four
    /// per core, at least 16). More wait for a slot, then get 503.
    pub max_git_processes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BlobConfig {
    /// `local` (default) or `s3` (ADR-0010).
    pub backend: String,
    /// Defaults to `<data_dir>/blobs`.
    pub local_dir: Option<PathBuf>,
    pub s3_bucket: String,
    pub s3_region: String,
    pub s3_endpoint: String,
    pub s3_access_key_id: String,
    pub s3_secret_access_key: String,
    pub s3_allow_http: bool,
    /// Serve downloads (release files, attachments, artifact archives) from S3
    /// through presigned links that set the file name and type
    /// (`response-content-disposition`). Turn off for S3 servers that ignore it.
    pub s3_presign_downloads: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Base64 of 32 random bytes; encrypts secrets at rest (TOTP seeds, later CI
    /// variables). Generated into `<data_dir>/secret.key` when empty.
    pub secret_key: String,
    pub session_ttl_hours: i64,
    /// Mark cookies `Secure` even when `external_url` is plain http (they
    /// always are when it is https).
    pub cookie_secure: bool,
    /// Serve Markdown images from other sites through this server (on), or let
    /// browsers load them directly (off; the page's CSP then allows any image).
    pub image_proxy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// `text` or `json`.
    pub log_format: String,
    pub log_filter: String,
    /// OTLP gRPC endpoint (`http://collector:4317`); empty disables tracing export.
    pub otlp_endpoint: String,
    pub metrics: bool,
    /// When set, `/metrics` answers only to `Authorization: Bearer <token>`
    /// (Prometheus `authorization.credentials`). Empty: open to anyone who can
    /// reach the server.
    pub metrics_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    /// Accept CI job tokens (`gitlab-ci-token`) on git over HTTP (on since M4).
    pub ci_job_token_git_http: bool,
    /// Let webhooks reach private and loopback addresses (tests, labs). Off: SSRF.
    pub webhooks_allow_local: bool,
    /// The Markdown image proxy may fetch private/loopback addresses (any
    /// comment's image would reach them: tests and closed setups only).
    #[serde(default)]
    pub image_proxy_allow_local: bool,
    /// Let imports reach forges on private and loopback addresses (an in-house
    /// GitLab). Off: SSRF, as for webhooks (ADR-0018).
    pub imports_allow_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_render_bytes: usize,
    pub max_raw_bytes: u64,
    pub max_diff_files_per_page: usize,
    pub max_diff_lines_per_file: usize,
    pub max_lfs_object_bytes: u64,
    /// One uploaded file (comment attachments, release files), by a member.
    pub max_upload_bytes: u64,
    /// One uploaded file by someone who is not a member (a public project's visitor).
    pub max_upload_bytes_non_member: u64,
    /// Files one person may upload per hour, across projects (0: no limit).
    pub uploads_per_hour: u32,
    pub request_timeout_secs: u64,
    pub ssh_max_connections: usize,
    /// In-process response cache size (keyed by object IDs).
    pub cache_bytes: u64,
    /// Blame, commit history and archive requests one anonymous client may make
    /// per minute (signed-in users are not limited).
    pub anonymous_heavy_reads_per_minute: u32,
    /// A live update stream re-checks the viewer's access at most this often.
    pub events_recheck_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupConfig {
    /// Command used to dump Postgres (`pg_dump`, or `docker exec -i pg pg_dump`).
    pub pg_dump: String,
    pub pg_restore: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EmailConfig {
    /// `smtps://user:pass@host:465` (TLS), `smtp://user:pass@host:587?tls=required`
    /// (STARTTLS), or `smtp://host:port` with no TLS at all (a local relay, or
    /// mailpit in development: never across a network with a password).
    /// Empty disables email.
    pub smtp_url: String,
    pub from: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcProvider {
    pub display_name: String,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// Create a local account on first login.
    #[serde(default)]
    pub auto_create: bool,
    /// Link a first OIDC login to an existing local account with the same email,
    /// when that account confirmed its address (or was made by an admin). Off by
    /// default: it trusts the provider's claim that the email is the user's.
    #[serde(default)]
    pub link_existing_by_email: bool,
}

fn default_scopes() -> Vec<String> {
    vec!["openid".into(), "email".into(), "profile".into()]
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: "0.0.0.0:8080".into(),
            external_url: "http://localhost:8080".into(),
            ssh_listen: "0.0.0.0:2222".into(),
            ssh_external: "localhost:2222".into(),
            data_dir: PathBuf::from("./data"),
            static_dir: PathBuf::from("./web/dist"),
            roles: vec!["api".into(), "worker".into(), "storage".into()],
            hooks_exe: None,
            commit_name: "Forgust".into(),
            commit_email: "noreply@forgust.local".into(),
            trusted_proxies: vec!["127.0.0.0/8".parse().unwrap(), "::1/128".parse().unwrap()],
            release_check_url: "https://api.github.com/repos/forgust-dev/forgust/releases/latest".into(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        DatabaseConfig { url: "postgres://forgust:forgust@localhost:5432/forgust".into(), max_connections: 20 }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig { repos_dir: None, git_bin: PathBuf::from("git"), catfile_pool_size: 64, max_git_processes: 0 }
    }
}

impl Default for BlobConfig {
    fn default() -> Self {
        BlobConfig {
            backend: "local".into(),
            local_dir: None,
            s3_bucket: String::new(),
            s3_region: "us-east-1".into(),
            s3_endpoint: String::new(),
            s3_access_key_id: String::new(),
            s3_secret_access_key: String::new(),
            s3_allow_http: false,
            s3_presign_downloads: true,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        SecurityConfig { secret_key: String::new(), session_ttl_hours: 24 * 14, cookie_secure: false, image_proxy: true }
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        TelemetryConfig {
            log_format: "text".into(),
            log_filter: "info,sqlx=warn,graphile_worker=warn,russh=warn,tower_http=info".into(),
            otlp_endpoint: String::new(),
            metrics: true,
            metrics_token: String::new(),
        }
    }
}

impl Default for Features {
    fn default() -> Self {
        Features { ci_job_token_git_http: true, webhooks_allow_local: false, image_proxy_allow_local: false, imports_allow_local: false }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_render_bytes: 2 << 20,
            max_raw_bytes: 512 << 20,
            max_diff_files_per_page: 100,
            max_diff_lines_per_file: 5000,
            max_lfs_object_bytes: 10 << 30,
            max_upload_bytes: 100 << 20,
            max_upload_bytes_non_member: 10 << 20,
            uploads_per_hour: 60,
            request_timeout_secs: 60,
            ssh_max_connections: 512,
            cache_bytes: 128 << 20,
            anonymous_heavy_reads_per_minute: 60,
            events_recheck_secs: 60,
        }
    }
}

impl Default for BackupConfig {
    fn default() -> Self {
        BackupConfig { pg_dump: "pg_dump".into(), pg_restore: "pg_restore".into() }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let mut fig = Figment::from(Serialized::defaults(Config::default()));
        let path = path.map(Path::to_path_buf).or_else(|| {
            let p = PathBuf::from("forgust.toml");
            p.exists().then_some(p)
        });
        if let Some(p) = path {
            fig = fig.merge(Toml::file(p));
        }
        fig = fig.merge(Env::prefixed("FORGUST_").split("__"));
        let mut cfg: Config = fig.extract()?;
        cfg.server.external_url = cfg.server.external_url.trim_end_matches('/').to_string();
        cfg.check_pages()?;
        Ok(cfg)
    }

    /// Sites run visitors' scripts: their domain must not hold Forgust's own
    /// host (they would share its origin, or sit above it).
    pub fn check_pages(&mut self) -> anyhow::Result<()> {
        self.pages.domain = self.pages.domain.trim().trim_matches('.').to_ascii_lowercase();
        if !self.pages.enabled() {
            return Ok(());
        }
        let app_host =
            url::Url::parse(&self.server.external_url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
        let d = &self.pages.domain;
        // above Forgust's host (a site would share its origin) or below it (a site
        // could set cookies for Forgust's host)
        if app_host == *d || app_host.ends_with(&format!(".{d}")) || d.ends_with(&format!(".{app_host}")) {
            anyhow::bail!(
                "pages.domain ({d}) must be neither Forgust's host ({app_host}), above it nor under it: sites run visitors' scripts and need their own origin"
            );
        }
        // the same registrable domain (a rough check: the last two labels) lets
        // sites set cookies Forgust's pages receive
        let last2 = |h: &str| h.rsplit('.').take(2).map(str::to_string).collect::<Vec<String>>();
        if last2(&app_host) == last2(d) {
            tracing::warn!(
                pages_domain = %d,
                host = %app_host,
                "pages.domain shares Forgust's registrable domain: sites can set cookies for it; prefer a domain of its own (as github.io for github.com)"
            );
        }
        Ok(())
    }

    pub fn repos_dir(&self) -> PathBuf {
        self.storage.repos_dir.clone().unwrap_or_else(|| self.server.data_dir.join("repositories"))
    }

    /// Where backups made from the admin area go.
    pub fn backups_dir(&self) -> PathBuf {
        self.server.data_dir.join("backups")
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.blobs.local_dir.clone().unwrap_or_else(|| self.server.data_dir.join("blobs"))
    }

    pub fn has_role(&self, role: &str) -> bool {
        self.server.roles.iter().any(|r| r == role)
    }

    /// Clone URL over SSH: `ssh://git@host:port/path.git` (or scp-like on port 22).
    pub fn ssh_clone_url(&self, full_path: &str) -> String {
        let ext = &self.server.ssh_external;
        match ext.rsplit_once(':') {
            Some((host, port)) if port != "22" => format!("ssh://git@{host}:{port}/{full_path}.git"),
            Some((host, _)) => format!("git@{host}:{full_path}.git"),
            None => format!("git@{ext}:{full_path}.git"),
        }
    }

    pub fn http_clone_url(&self, full_path: &str) -> String {
        format!("{}/{full_path}.git", self.server.external_url)
    }

    /// Whether cookies only travel over https.
    pub fn secure_cookies(&self) -> bool {
        self.security.cookie_secure || self.server.external_url.starts_with("https://")
    }

    /// `external_url` (no trailing slash: `load` trims it) followed by `path`.
    pub fn web_url(&self, path: &str) -> String {
        format!("{}/{}", self.server.external_url, path.trim_start_matches('/'))
    }

    /// A page of a project: `section` is `merge_requests/3`, `commit/<sha>`…
    pub fn project_page(&self, full_path: &str, section: &str) -> String {
        format!("{}/{full_path}/-/{section}", self.server.external_url)
    }

    pub fn mr_url(&self, full_path: &str, iid: i64) -> String {
        self.project_page(full_path, &format!("merge_requests/{iid}"))
    }

    pub fn issue_url(&self, full_path: &str, iid: i64) -> String {
        self.project_page(full_path, &format!("issues/{iid}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_live_on_their_own_origin() {
        let with = |app: &str, pages: &str| {
            let mut c = Config::default();
            c.server.external_url = app.into();
            c.pages.domain = pages.into();
            c.check_pages().map(|_| (c.pages.domain.clone(), c.pages.url("acme/platform/shop")))
        };
        assert!(with("https://git.example.com", "example.com").is_err(), "a parent of Forgust's host");
        assert!(with("https://pages.example.com", "pages.example.com").is_err(), "Forgust's own host");
        assert!(with("https://git.example.com", "pages.git.example.com").is_err(), "under Forgust's host: cookie tossing");
        let (d, url) = with("https://git.example.com", ".Pages.Example.com.").unwrap();
        assert_eq!((d.as_str(), url.as_deref()), ("pages.example.com", Some("https://acme.pages.example.com/platform/shop/")));
        assert_eq!(with("https://git.example.com", "").unwrap().1, None, "off");
    }
}
