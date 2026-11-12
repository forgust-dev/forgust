//! Storage quotas. Every path that adds bytes asks [`room`] first and
//! writes at most what is left: git push (pre-receive), LFS uploads, CI
//! artifacts, registry blobs, uploads. Reading and deleting stay open when
//! full, and so do pushes that bring no new objects.

use forgust_db::quotas::{Crossed, Limits};

use crate::state::AppState;

/// What a project may still add before a quota is reached.
#[derive(Debug, Clone)]
pub struct Room {
    /// Bytes left (0: full).
    pub left: u64,
    pub used: i64,
    pub quota: i64,
    /// What is full: `this project` or `the group acme` / `alice's namespace`.
    pub scope: String,
}

impl Room {
    pub fn full(&self) -> bool {
        self.left == 0
    }

    /// The error every refused write gives.
    pub fn message(&self) -> String {
        format!(
            "Storage quota reached: {} uses {} of {}. Delete what is no longer needed (artifacts, images, LFS files, uploads) or ask an administrator for more.",
            self.scope,
            bytes(self.used),
            bytes(self.quota)
        )
    }
}

/// The tighter of the project's and its namespace's quotas; None when neither
/// applies.
pub async fn room(state: &AppState, project_id: i64) -> anyhow::Result<Option<Room>> {
    let Some(l) = forgust_db::quotas::limits(&state.db, project_id).await? else { return Ok(None) };
    let mut best: Option<Room> = None;
    if let Some(q) = l.project_quota {
        let used = forgust_db::quotas::project_used(&state.db, project_id).await?;
        best = Some(Room { left: (q - used).max(0) as u64, used, quota: q, scope: "this project".into() });
    }
    if let Some(q) = l.namespace_quota {
        let used = forgust_db::quotas::namespace_used(&state.db, l.root_id).await?;
        let r = Room { left: (q - used).max(0) as u64, used, quota: q, scope: namespace_name(&l) };
        if best.as_ref().is_none_or(|b| r.left < b.left) {
            best = Some(r);
        }
    }
    Ok(best)
}

fn namespace_name(l: &Limits) -> String {
    if l.root_kind == "group" { format!("the group {}", l.root_path) } else { format!("{}'s personal projects", l.root_path) }
}

/// At most `limit`, and no more than the room left; Err with the message when
/// full.
pub async fn cap(state: &AppState, project_id: i64, limit: u64) -> anyhow::Result<Result<u64, String>> {
    Ok(match room(state, project_id).await? {
        Some(r) if r.full() => Err(r.message()),
        Some(r) => Ok(limit.min(r.left)),
        None => Ok(limit),
    })
}

/// Bytes under a directory (a push's quarantined objects).
pub fn dir_bytes(dir: &std::path::Path) -> u64 {
    let mut n = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => n += m.len(),
                Err(_) => {}
            }
        }
    }
    n
}

/// Hourly: store how full each namespace and project is, and email the Owners
/// of those that just reached 80 % or 100 %.
pub async fn measure(state: &AppState) -> anyhow::Result<()> {
    let mut conn = state.db.acquire().await?;
    let crossed = forgust_db::quotas::measure_levels(&mut conn).await?;
    drop(conn);
    for c in crossed {
        tell_owners(state, &c).await;
    }
    Ok(())
}

async fn tell_owners(state: &AppState, c: &Crossed) {
    let (owners, what, url) = match c.scope {
        "namespace" => {
            let path = forgust_db::namespaces::find(&state.db, c.id).await.ok().flatten().map(|n| n.path).unwrap_or_default();
            (forgust_db::quotas::namespace_owner_ids(&state.db, c.id).await, format!("Everything in {path}"), state.cfg.web_url(&path))
        }
        _ => {
            let p = forgust_db::projects::find(&state.db, c.id).await.ok().flatten();
            let path = p.map(|p| p.full_path).unwrap_or_default();
            (forgust_db::quotas::project_owner_ids(&state.db, c.id).await, format!("The project {path}"), state.cfg.web_url(&path))
        }
    };
    let Ok(owners) = owners else { return };
    let subject = if c.level >= 100 { "Storage quota reached" } else { "Storage quota almost reached" };
    let then = if c.level >= 100 {
        "New pushes, LFS files, CI artifacts, images and uploads are refused until something is deleted or the quota is raised."
    } else {
        "Once it is full, new pushes, LFS files, CI artifacts, images and uploads are refused."
    };
    let body = format!("{what} uses {} of its {} storage quota ({} %).\n\n{then}\n\n{url}\n", bytes(c.used), bytes(c.quota), c.level);
    for id in owners {
        if let Ok(Some(u)) = forgust_db::users::find(&state.db, id).await {
            crate::services::notify::send_to(state, &u.email, subject, &body).await;
        }
    }
}

/// As the web UI writes sizes (`bytes` in web/src/api/client.ts): 1024-based.
pub fn bytes(n: i64) -> String {
    let n = n.max(0) as f64;
    if n < 1024.0 {
        return format!("{n} B");
    }
    let mut v = n / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < 3 {
        v /= 1024.0;
        unit += 1;
    }
    let u = ["KB", "MB", "GB", "TB"][unit];
    if v < 10.0 { format!("{v:.1} {u}") } else { format!("{v:.0} {u}") }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bytes_read_like_the_ui() {
        assert_eq!(super::bytes(512), "512 B");
        assert_eq!(super::bytes(3 << 29), "1.5 GB");
        assert_eq!(super::bytes(300 << 20), "300 MB");
    }
}
