//! Data access, grouped per domain (ADR-0005). Every query is compile-time checked
//! with `sqlx::query!`; no SQL lives in HTTP handlers.

pub mod admin;
pub mod audit;
pub mod authz;
pub mod awards;
pub mod ci;
pub mod ci_variables;
pub mod contributions;
pub mod environments;
pub mod gpg_keys;
pub mod imports;
pub mod issues;
pub mod lfs;
pub mod lfs_locks;
pub mod links;
pub mod maintenance;
pub mod members;
pub mod merge_requests;
pub mod merge_trains;
pub mod milestones;
pub mod namespaces;
pub mod notifications;
pub mod oauth;
pub mod pages;
pub mod people;
pub mod projects;
pub mod protected_branches;
pub mod push_rules;
pub mod quotas;
pub mod registry;
pub mod releases;
pub mod renovate;
pub mod routes;
pub mod search;
pub mod security;
pub mod sessions;
pub mod settings;
pub mod ssh_keys;
pub mod todos;
pub mod tokens;
pub mod users;
pub mod webhooks;

pub use sqlx::{PgConnection, PgPool, Postgres, Transaction};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub type Result<T> = std::result::Result<T, sqlx::Error>;

/// `(id, value)` rows grouped by id, in order (labels or people per issue / MR).
pub fn group_by_id<T>(rows: Vec<(i64, T)>) -> std::collections::HashMap<i64, Vec<T>> {
    let mut out: std::collections::HashMap<i64, Vec<T>> = std::collections::HashMap::new();
    for (id, v) in rows {
        out.entry(id).or_default().push(v);
    }
    out
}

/// `s` for `LIKE ... ESCAPE '\'`: its `%`, `_` and `\` match themselves.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub mod access {
    pub const NONE: i16 = 0;
    pub const GUEST: i16 = 10;
    pub const REPORTER: i16 = 20;
    pub const DEVELOPER: i16 = 30;
    pub const MAINTAINER: i16 = 40;
    pub const OWNER: i16 = 50;

    pub fn name(level: i16) -> &'static str {
        match level {
            10 => "guest",
            20 => "reporter",
            30 => "developer",
            40 => "maintainer",
            50 => "owner",
            _ => "none",
        }
    }

    pub fn parse(s: &str) -> Option<i16> {
        Some(match s {
            "guest" => GUEST,
            "reporter" => REPORTER,
            "developer" => DEVELOPER,
            "maintainer" => MAINTAINER,
            "owner" => OWNER,
            _ => return None,
        })
    }
}

pub mod visibility {
    pub const PRIVATE: i16 = 0;
    pub const INTERNAL: i16 = 10;
    pub const PUBLIC: i16 = 20;

    pub fn name(v: i16) -> &'static str {
        match v {
            20 => "public",
            10 => "internal",
            _ => "private",
        }
    }

    pub fn parse(s: &str) -> Option<i16> {
        Some(match s {
            "private" => PRIVATE,
            "internal" => INTERNAL,
            "public" => PUBLIC,
            _ => return None,
        })
    }
}

/// True when the error is a unique-constraint violation.
pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}
