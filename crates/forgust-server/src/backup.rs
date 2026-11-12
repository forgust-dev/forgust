//! Backup and restore of Postgres plus every repository (architecture:
//! Deployment and operations). The blob store (LFS, artifacts) is backed up with
//! the storage's own tools (S3 versioning, rsync of the local directory).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Context;
use forgust_storage::{CreateRepoOptions, RefFormat, Storage};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::config::Config;

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub forgust_version: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub projects: Vec<RepoEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct RepoEntry {
    pub id: i64,
    pub default_branch: Option<String>,
    pub ref_format: String,
    /// False for empty repos (nothing to bundle).
    pub bundle: bool,
}

fn command(spec: &str) -> anyhow::Result<tokio::process::Command> {
    let mut parts = spec.split_whitespace();
    let program = parts.next().context("empty command")?;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(parts);
    Ok(cmd)
}

pub async fn backup(cfg: &Config, storage: &dyn Storage, pool: &sqlx::PgPool, out: &Path) -> anyhow::Result<PathBuf> {
    let dir = out.join(chrono::Utc::now().format("forgust-%Y%m%dT%H%M%SZ").to_string());
    tokio::fs::create_dir_all(dir.join("repos")).await?;

    // 1. database: custom-format dump on stdout, so it works through `docker exec`
    let mut cmd = command(&cfg.backup.pg_dump)?;
    let (url, password) = split_password(&backup_db_url(cfg));
    cmd.arg("--format=custom").arg("--no-owner").arg(format!("--dbname={url}")).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(pw) = &password {
        cmd.env("PGPASSWORD", pw);
    }
    let child = cmd.spawn().with_context(|| format!("running {}", cfg.backup.pg_dump))?;
    let output = child.wait_with_output().await?;
    anyhow::ensure!(output.status.success(), "pg_dump failed: {}", String::from_utf8_lossy(&output.stderr));
    tokio::fs::write(dir.join("database.dump"), &output.stdout).await?;

    // 2. repositories (after the dump: a repo newer than its metadata is harmless)
    let mut projects = Vec::new();
    for (id, default_branch, ref_format) in forgust_db::projects::all_ids(pool).await? {
        let bundle = match storage.bundle_create(id, &dir.join("repos").join(format!("{id}.bundle"))).await {
            Ok(b) => b,
            Err(forgust_storage::StorageError::RepoNotFound(_)) => {
                tracing::warn!(project = id, "repository missing on disk; skipped");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        projects.push(RepoEntry { id, default_branch, ref_format, bundle });
    }
    let manifest = Manifest { version: 1, forgust_version: env!("CARGO_PKG_VERSION").into(), created_at: chrono::Utc::now(), projects };
    tokio::fs::write(dir.join("manifest.json"), serde_json::to_vec_pretty(&manifest)?).await?;
    Ok(dir)
}

/// The URL without its password, and the password: it goes to pg_dump and
/// pg_restore as `PGPASSWORD`, never on a command line other users of the
/// host can read (`ps`, `/proc/*/cmdline`). Through `docker exec`, add
/// `-e PGPASSWORD` to the command so the container gets it.
fn split_password(url: &str) -> (String, Option<String>) {
    match url::Url::parse(url) {
        Ok(mut u) if u.password().is_some() => {
            let pw = percent_encoding::percent_decode_str(u.password().unwrap_or_default()).decode_utf8_lossy().into_owned();
            let _ = u.set_password(None);
            (u.to_string(), Some(pw))
        }
        _ => (url.to_string(), None),
    }
}

fn backup_db_url(cfg: &Config) -> String {
    std::env::var("FORGUST_BACKUP_DATABASE_URL").unwrap_or_else(|_| cfg.database.url.clone())
}

/// Restore into an empty database and an empty repositories directory.
pub async fn restore(cfg: &Config, storage: &dyn Storage, input: &Path) -> anyhow::Result<usize> {
    let manifest: Manifest = serde_json::from_slice(&tokio::fs::read(input.join("manifest.json")).await?)?;
    anyhow::ensure!(manifest.version == 1, "unsupported backup version {}", manifest.version);

    let mut cmd = command(&cfg.backup.pg_restore)?;
    let (url, password) = split_password(&backup_db_url(cfg));
    cmd.arg("--no-owner").arg("--exit-on-error").arg(format!("--dbname={url}")).stdin(Stdio::piped()).stderr(Stdio::piped());
    if let Some(pw) = &password {
        cmd.env("PGPASSWORD", pw);
    }
    let mut child = cmd.spawn().with_context(|| format!("running {}", cfg.backup.pg_restore))?;
    let dump = tokio::fs::read(input.join("database.dump")).await?;
    let mut stdin = child.stdin.take().context("stdin")?;
    stdin.write_all(&dump).await?;
    drop(stdin);
    let output = child.wait_with_output().await?;
    anyhow::ensure!(output.status.success(), "pg_restore failed: {}", String::from_utf8_lossy(&output.stderr));

    for p in &manifest.projects {
        let bundle = input.join("repos").join(format!("{}.bundle", p.id));
        let opts = CreateRepoOptions {
            default_branch: p.default_branch.clone().unwrap_or_else(|| "main".into()),
            ref_format: RefFormat::parse(&p.ref_format).unwrap_or_default(),
        };
        storage
            .restore_from_bundle(p.id, p.bundle.then_some(bundle.as_path()), opts)
            .await
            .with_context(|| format!("restoring project {}", p.id))?;
    }
    Ok(manifest.projects.len())
}

#[cfg(test)]
mod password_tests {
    #[test]
    fn the_password_leaves_the_url() {
        let (url, pw) = super::split_password("postgres://forgust:s%40cret@db:5432/forgust");
        assert_eq!((url.as_str(), pw.as_deref()), ("postgres://forgust@db:5432/forgust", Some("s@cret")));
        assert_eq!(super::split_password("postgres:///forgust?host=/run/pg").1, None);
    }
}
