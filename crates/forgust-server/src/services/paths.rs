//! Path and name rules for users, groups and projects (ADR-0013).

use crate::error::{ApiError, ApiResult};

pub const MAX_DEPTH: usize = 20;

/// Top-level paths the web app and APIs use; no user or org may take them.
pub const RESERVED_TOP_LEVEL: &[&str] = &[
    "-",
    ".well-known",
    "admin",
    "api",
    "assets",
    "dashboard",
    "explore",
    "favicon.ico",
    "forgot-password",
    "groups",
    "health",
    "help",
    "import",
    "login",
    "logout",
    "metrics",
    "new",
    "oauth",
    "profile",
    "projects",
    "reset-password",
    "robots.txt",
    "s",
    "search",
    "settings",
    "setup",
    "static",
    "unsubscribe",
    "uploads",
    "users",
    "verify-email",
];

/// Reserved at any depth.
pub const RESERVED_ANY: &[&str] = &["-"];

pub fn validate_segment(path: &str) -> ApiResult<()> {
    let ok = !path.is_empty()
        && path.len() <= 255
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        && (path.as_bytes()[0].is_ascii_alphanumeric() || path.as_bytes()[0] == b'_')
        && !path.ends_with('.')
        && !path.contains("..")
        && !path.to_ascii_lowercase().ends_with(".git")
        && !path.to_ascii_lowercase().ends_with(".atom");
    if !ok {
        return Err(ApiError::bad(format!(
            "invalid path `{path}`: use letters, digits, `_`, `-` and `.`; start with a letter, digit or `_`; do not end with `.git`"
        )));
    }
    if RESERVED_ANY.contains(&path) {
        return Err(ApiError::bad(format!("`{path}` is reserved")));
    }
    Ok(())
}

pub fn validate_top_level(path: &str) -> ApiResult<()> {
    validate_segment(path)?;
    if RESERVED_TOP_LEVEL.iter().any(|r| r.eq_ignore_ascii_case(path)) {
        return Err(ApiError::bad(format!("`{path}` is reserved")));
    }
    Ok(())
}

pub fn validate_name(name: &str) -> ApiResult<()> {
    if name.trim().is_empty() || name.len() > 255 || name.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad("name must be 1-255 printable characters"));
    }
    Ok(())
}

pub fn join(parent: Option<&str>, path: &str) -> String {
    match parent {
        Some(p) => format!("{p}/{path}"),
        None => path.to_string(),
    }
}

/// A reasonable path from a display name ("My Project" → "my-project").
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches(|c| c == '-' || c == '.').to_string()
}

pub fn validate_email(email: &str) -> ApiResult<()> {
    let ok = email.len() <= 254 && email.split_once('@').is_some_and(|(a, b)| !a.is_empty() && b.contains('.') && !b.starts_with('.'));
    if !ok {
        return Err(ApiError::bad("invalid email address"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments() {
        for ok in ["acme", "my-project", "a.b", "_x", "A1"] {
            assert!(validate_segment(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", ".x", "a..b", "x.git", "x.", "a/b", "a b", "-", "é"] {
            assert!(validate_segment(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reserved_top_level() {
        assert!(validate_top_level("api").is_err());
        assert!(validate_top_level("API").is_err());
        assert!(validate_top_level("acme").is_ok());
        // reserved names are fine below the top level
        assert!(validate_segment("api").is_ok());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("My Project!"), "my-project");
        assert_eq!(slug("  Hello  World "), "hello-world");
    }
}
