use std::path::PathBuf;

use clap::{Parser, Subcommand};
use forgust_server::{app, backup, config::Config, services, telemetry, tls};

#[derive(Parser)]
#[command(name = "forgust", version, about = "Forgust: a fast, self-hosted code forge")]
struct Cli {
    /// Config file (default: ./forgust.toml when present). `FORGUST_*` env vars override it.
    #[arg(long, short, global = true, env = "FORGUST_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (roles from `server.roles`).
    Serve,
    /// Apply database migrations and exit.
    Migrate,
    /// Administrative tasks.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Git hook entry point (called by git, not by people).
    Hook { name: String },
    /// Back up the database and all repositories into a new directory under OUTPUT.
    Backup {
        #[arg(long)]
        output: PathBuf,
    },
    /// Restore a backup directory into an empty database and repositories directory.
    Restore {
        #[arg(long)]
        input: PathBuf,
    },
    /// Print the OpenAPI document (used to generate the frontend's types).
    Openapi,
    /// Print the effective configuration.
    Config,
    /// Exit 0 when the local server answers /health (container health checks).
    Health,
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Create a user (the first administrator, typically).
    CreateUser {
        #[arg(long)]
        username: String,
        #[arg(long)]
        email: String,
        #[arg(long, env = "FORGUST_ADMIN_PASSWORD")]
        password: String,
        #[arg(long)]
        admin: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // hooks run inside git: keep them lean (no config, no telemetry)
    if let Command::Hook { name } = &cli.command {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let code = rt.block_on(forgust_server::hooks::run_hook(name));
        std::process::exit(code);
    }
    if let Command::Openapi = &cli.command {
        println!("{}", serde_json::to_string_pretty(&forgust_server::api::openapi::document())?);
        return Ok(());
    }
    let cfg = Config::load(cli.config.as_deref())?;
    tls::init(&cfg)?;
    let sentry = matches!(cli.command, Command::Serve).then(|| telemetry::init_sentry(&cfg.telemetry)).transpose()?.flatten();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = rt.block_on(run(cli.command, cfg));
    // what stops the server (no database, a taken port) is returned, never logged
    if let (Err(e), Some(_)) = (&result, &sentry) {
        let error: &(dyn std::error::Error + 'static) = e.as_ref();
        sentry::capture_error(error);
    }
    result
}

async fn run(command: Command, cfg: Config) -> anyhow::Result<()> {
    match command {
        Command::Serve => {
            let tel = telemetry::init(&cfg.telemetry)?;
            let pool = app::connect(&cfg).await?;
            app::migrate(&pool).await?;
            let state = app::build_state(cfg, pool, tel.metrics.clone()).await?;
            let running = app::start(state).await?;
            running.http.await?;
            tel.shutdown();
        }
        Command::Migrate => {
            telemetry::init_cli();
            let pool = app::connect(&cfg).await?;
            app::migrate(&pool).await?;
            println!("migrations applied");
        }
        Command::Admin { command: AdminCommand::CreateUser { username, email, password, admin } } => {
            telemetry::init_cli();
            let pool = app::connect(&cfg).await?;
            app::migrate(&pool).await?;
            let mut tx = pool.begin().await?;
            let u = services::users::create(
                &mut tx,
                services::users::CreateUser {
                    username: &username,
                    email: &email,
                    name: "",
                    password: Some(&password),
                    is_admin: admin,
                    email_verified: true,
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            tx.commit().await?;
            println!("created user {} (id {}){}", u.username, u.id, if admin { " as administrator" } else { "" });
        }
        Command::Backup { output } => {
            telemetry::init_cli();
            let pool = app::connect(&cfg).await?;
            let storage = app::storage_for(&cfg);
            let dir = backup::backup(&cfg, storage.as_ref(), &pool, &output).await?;
            println!("backup written to {}", dir.display());
        }
        Command::Restore { input } => {
            telemetry::init_cli();
            let storage = app::storage_for(&cfg);
            let n = backup::restore(&cfg, storage.as_ref(), &input).await?;
            println!("restored database and {n} repositories");
        }
        Command::Config => println!("{}", toml_like(&cfg)),
        Command::Health => {
            let port = cfg.server.listen.rsplit(':').next().unwrap_or("8080");
            let resp = reqwest::get(format!("http://127.0.0.1:{port}/health")).await?;
            anyhow::ensure!(resp.status().is_success(), "unhealthy: {}", resp.status());
        }
        Command::Hook { .. } | Command::Openapi => unreachable!(),
    }
    Ok(())
}

fn toml_like(cfg: &Config) -> String {
    let mut v = serde_json::to_value(cfg).unwrap_or_default();
    for path in ["/security/secret_key", "/blobs/s3_secret_access_key"] {
        if let Some(s) = v.pointer_mut(path) {
            *s = serde_json::Value::String("<redacted>".into());
        }
    }
    for path in ["/database/url", "/email/smtp_url", "/telemetry/sentry_dsn", "/telemetry/metrics_token", "/renovate/github_com_token", "/ai_review/api_key"] {
        if let Some(s) = v.pointer_mut(path).filter(|s| s.as_str().is_some_and(|d| !d.trim().is_empty())) {
            *s = serde_json::Value::String("<redacted>".into());
        }
    }
    if let Some(oidc) = v.get_mut("oidc").and_then(|o| o.as_object_mut()) {
        for p in oidc.values_mut() {
            if let Some(s) = p.get_mut("client_secret").filter(|s| s.as_str().is_some_and(|d| !d.trim().is_empty())) {
                *s = serde_json::Value::String("<redacted>".into());
            }
        }
    }
    if let Some(serde_json::Value::Object(m)) = v.pointer_mut("/ai_review/secret_options") {
        m.values_mut().for_each(|x| *x = serde_json::Value::String("<redacted>".into()));
    }
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_output_redacts_secrets() {
        let mut cfg = Config::default();
        cfg.database.url = "postgres://forgust:db-secret@localhost/forgust".into();
        cfg.email.smtp_url = "smtp://user:smtp-secret@mail.example.com".into();
        cfg.telemetry.metrics_token = "metrics-secret".into();
        cfg.renovate.github_com_token = Some("ghp_renovate_secret".into());
        cfg.oidc.insert(
            "corp".into(),
            serde_json::from_value(serde_json::json!({
                "display_name": "Corp",
                "issuer": "https://idp.example.com",
                "client_id": "forgust",
                "client_secret": "oidc-secret"
            }))
            .unwrap(),
        );
        let out = toml_like(&cfg);
        for s in ["db-secret", "smtp-secret", "metrics-secret", "ghp_renovate_secret", "oidc-secret"] {
            assert!(!out.contains(s), "leaked {s} in {out}");
        }
    }
}
