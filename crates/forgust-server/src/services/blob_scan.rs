//! Admin > Storage: what the blob store holds, by top-level prefix, and
//! what nothing references. A dry run: nothing is deleted here.
//!
//! An object counts as unreferenced only when it is older than a day: uploads,
//! LFS pushes and log archiving write the object before its row.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use forgust_db::admin::{OrphanGroup, PrefixUsage};
use futures::StreamExt;

use crate::state::AppState;

pub struct Limits {
    pub max_objects: u64,
    pub budget: Duration,
    /// Younger objects may still be waiting for their row.
    pub min_age: chrono::Duration,
}

pub struct Report {
    pub prefixes: Vec<PrefixUsage>,
    pub orphans: Vec<OrphanGroup>,
    /// False when it stopped at its limits (both lists are then partial).
    pub complete: bool,
}

/// Keys looked up per query.
const BATCH: usize = 1000;
/// Keys shown per family.
const SAMPLE: usize = 10;

/// The family of a key, by how Forgust names what it writes.
pub fn family(key: &str) -> &'static str {
    let mut parts = key.split('/');
    match (parts.next(), parts.next()) {
        (Some("lfs"), _) => "lfs",
        (Some("registry"), Some("blobs")) => "registry",
        (Some("registry"), Some("uploads")) => "registry_uploads",
        (Some("ci"), Some("artifacts" | "traces")) => "ci",
        (Some("uploads"), _) => "uploads",
        (Some("avatars"), _) => "avatars",
        (Some("tmp" | ".health"), _) => "tmp",
        _ => "unrecognized",
    }
}

pub async fn scan(state: &AppState, limits: &Limits) -> anyhow::Result<Report> {
    let deadline = Instant::now() + limits.budget;
    let cutoff = chrono::Utc::now() - limits.min_age;
    let mut prefixes: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut orphans = Orphans::default();
    let mut batch: Vec<(String, u64)> = Vec::with_capacity(BATCH);
    let (mut seen, mut complete) = (0u64, true);
    let mut list = state.blobs.list();
    while let Some(m) = list.next().await {
        let m = m?;
        let top = prefixes.entry(m.key.split('/').next().unwrap_or_default().to_string()).or_default();
        top.0 += 1;
        top.1 += m.size as i64;
        match family(&m.key) {
            "unrecognized" => orphans.add("unrecognized", &m.key, m.size),
            _ if m.modified > cutoff => {}
            "tmp" => orphans.add("tmp", &m.key, m.size),
            _ => {
                batch.push((m.key, m.size));
                if batch.len() >= BATCH {
                    orphans.resolve(state, &mut batch).await?;
                }
            }
        }
        seen += 1;
        if seen >= limits.max_objects || Instant::now() > deadline {
            complete = false;
            break;
        }
    }
    orphans.resolve(state, &mut batch).await?;
    Ok(Report {
        prefixes: prefixes.into_iter().map(|(prefix, (objects, bytes))| PrefixUsage { prefix, objects, bytes }).collect(),
        orphans: orphans.0.into_values().collect(),
        complete,
    })
}

#[derive(Default)]
struct Orphans(BTreeMap<&'static str, OrphanGroup>);

impl Orphans {
    fn add(&mut self, family: &'static str, key: &str, size: u64) {
        let g = self.0.entry(family).or_insert_with(|| OrphanGroup { family: family.into(), ..Default::default() });
        g.objects += 1;
        g.bytes += size as i64;
        if g.sample.len() < SAMPLE {
            g.sample.push(key.to_string());
        }
    }

    /// Look the batch up; what nothing accounts for is an orphan of its family.
    async fn resolve(&mut self, state: &AppState, batch: &mut Vec<(String, u64)>) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let keys: Vec<String> = batch.iter().map(|(k, _)| k.clone()).collect();
        let status: HashMap<String, String> = forgust_db::admin::blob_key_status(&state.db, &keys).await?.into_iter().collect();
        for (key, size) in batch.drain(..) {
            match status.get(&key).map(String::as_str) {
                Some("unused") => self.add("lfs_unused", &key, size),
                Some(_) => {}
                None => self.add(family(&key), &key, size),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::family;

    #[test]
    fn families_follow_the_keys_forgust_writes() {
        for (key, f) in [
            ("lfs/ab/cd/abcd", "lfs"),
            ("registry/blobs/sha256/0f", "registry"),
            ("registry/uploads/5b0c/0", "registry_uploads"),
            ("ci/artifacts/1/2/archive-x", "ci"),
            ("ci/traces/1/2.log", "ci"),
            ("uploads/3/secret", "uploads"),
            ("avatars/user/4.png", "avatars"),
            ("tmp/lfs-ab-x", "tmp"),
            (".health/probe", "tmp"),
            ("ci/other", "unrecognized"),
            ("backup.tar", "unrecognized"),
        ] {
            assert_eq!(family(key), f, "{key}");
        }
    }
}
