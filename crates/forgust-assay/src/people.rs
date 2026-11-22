//! Who knows a block: who changed it lately, from the git history (`.mailmap` applied, bots
//! left out). People to ask, by how much they changed it: never a score to show.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use crate::model::{Block, Person};

/// Shown per block; the bus factor counts everyone.
const SHOWN: usize = 5;

/// A commit of the history that tells who knows a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Its author, `.mailmap` applied.
    pub name: String,
    pub email: String,
    /// When it was authored: a Unix time.
    pub time: i64,
    /// The paths it changes, from the top of the repository.
    pub files: Vec<String>,
}

/// The commits of the last `days` days touching `root` (merges left out), their paths from
/// `root`; none out of git.
pub fn history(root: &Path, days: u32) -> Option<Vec<Commit>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-z", "--no-merges", "--no-renames", "--relative", "--name-only", "--format=%x1e%aN%x1f%aE%x1f%at"])
        .arg(format!("--since={days}.days"))
        .args(["--", "."])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_log(&String::from_utf8_lossy(&out.stdout)))
}

/// What `git log -z --name-only` prints with the format above: each commit's head (`\x1e`, then
/// its author's name, address and time between `\x1f`), then its paths, each ended by a NUL and
/// as they are: without `-z`, git quotes those with an accent.
fn parse_log(text: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    for chunk in text.split('\x1e') {
        let mut parts = chunk.split('\0');
        let Some(head) = parts.next() else { continue };
        let mut fields = head.split('\x1f');
        let (Some(name), Some(email), Some(time)) = (fields.next(), fields.next(), fields.next().and_then(|t| t.trim().parse().ok()))
        else {
            continue;
        };
        let files = parts.map(|p| p.trim_start_matches('\n')).filter(|p| !p.is_empty()).map(str::to_string).collect();
        commits.push(Commit { name: name.to_string(), email: email.to_string(), time, files });
    }
    commits
}

/// The commit checked out, in a git repository with one.
pub fn head(root: &Path) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(["rev-parse", "HEAD"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_bot(name: &str, email: &str) -> bool {
    let s = format!("{name} {email}").to_lowercase();
    ["[bot]", "dependabot", "renovate", "github-actions", "gitlab-bot", "forgust-bot"].iter().any(|b| s.contains(b))
}

/// The people behind the commits: one per name or address shared (two addresses under one
/// name are one person, as two names on one address), shown by their latest commit's.
struct People {
    /// "name:…" or "email:…" → a person, by union-find.
    keys: HashMap<String, usize>,
    parent: Vec<usize>,
}

impl People {
    fn new<'c>(authors: impl IntoIterator<Item = (&'c str, &'c str)>) -> Self {
        let mut p = Self { keys: HashMap::new(), parent: Vec::new() };
        for (name, email) in authors {
            let (a, b) = (p.key(format!("name:{}", name.to_lowercase())), p.key(format!("email:{}", email.to_lowercase())));
            let (a, b) = (p.find(a), p.find(b));
            p.parent[a] = b;
        }
        p
    }

    fn key(&mut self, k: String) -> usize {
        let next = self.parent.len();
        let id = *self.keys.entry(k).or_insert(next);
        if id == next {
            self.parent.push(next);
        }
        id
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn of(&mut self, c: &Commit) -> usize {
        let i = self.keys[&format!("email:{}", c.email.to_lowercase())];
        self.find(i)
    }
}

/// Each block's people, from the commits touching its files (`block_of` finds a path's
/// block), and its bus factor: how many of them made at least a fifth of those commits.
/// Someone `exclude` names, by one of their names or addresses, is left out under all of them.
pub fn attach(blocks: &mut [Block], commits: &[Commit], block_of: impl Fn(&str) -> Option<usize>, exclude: &[String]) {
    let commits: Vec<&Commit> = commits.iter().filter(|c| !is_bot(&c.name, &c.email)).collect();
    let mut people = People::new(commits.iter().map(|c| (c.name.as_str(), c.email.as_str())));
    let named = |c: &Commit| exclude.iter().any(|x| x.eq_ignore_ascii_case(&c.name) || x.eq_ignore_ascii_case(&c.email));
    let opted_out: HashSet<usize> = commits.iter().filter(|c| named(c)).map(|c| people.of(c)).collect();
    // per block: person → their commits there, and the latest (with the name and address it bore)
    let mut per: Vec<HashMap<usize, (usize, &Commit)>> = vec![HashMap::new(); blocks.len()];
    for c in &commits {
        let who = people.of(c);
        if opted_out.contains(&who) {
            continue;
        }
        let touched: BTreeSet<usize> = c.files.iter().filter_map(|f| block_of(f)).collect();
        for b in touched {
            let e = per[b].entry(who).or_insert((0, c));
            e.0 += 1;
            if c.time > e.1.time {
                e.1 = c;
            }
        }
    }
    for (b, per) in blocks.iter_mut().zip(per) {
        let mut list: Vec<Person> = per
            .into_values()
            .map(|(n, latest)| Person { name: latest.name.clone(), email: latest.email.clone(), commits: n, last: date(latest.time) })
            .collect();
        list.sort_by(|a, b| b.commits.cmp(&a.commits).then_with(|| b.last.cmp(&a.last)).then_with(|| a.name.cmp(&b.name)));
        let total: usize = list.iter().map(|p| p.commits).sum();
        b.bus_factor = (total > 0).then(|| list.iter().filter(|p| p.commits * 5 >= total).count().max(1));
        list.truncate(SHOWN);
        b.people = list;
    }
}

/// A Unix time's day, `YYYY-MM-DD` in UTC (Howard Hinnant's `civil_from_days`).
fn date(t: i64) -> String {
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_paths_as_they_are() {
        let out = "\x1eZoé\x1fzoe@example.com\x1f100\0\ncrates/c/src/été.rs\0docs/a b.md\0\x1eAda\x1fada@example.com\x1f50\0\nREADME.md\0";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].files, ["crates/c/src/été.rs", "docs/a b.md"]);
        assert_eq!(
            (commits[1].name.as_str(), commits[1].time, commits[1].files.as_slice()),
            ("Ada", 50, ["README.md".to_string()].as_slice())
        );
    }

    #[test]
    fn opting_out_takes_every_address() {
        let block = |id: &str| Block {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            kind: crate::model::Kind::Rust,
            role: None,
            roots: vec![id.to_string()],
            manifests: Vec::new(),
            parent: None,
            image: None,
            files: 1,
            languages: Vec::new(),
            jobs: Vec::new(),
            owners: Vec::new(),
            people: Vec::new(),
            bus_factor: None,
            source: crate::model::Source::Detected,
        };
        let change = |name: &str, email: &str| Commit { name: name.into(), email: email.into(), time: 0, files: vec!["a/x.rs".into()] };
        let commits = [change("Eve", "eve@work.example"), change("Eve", "eve@home.example"), change("Ada", "ada@example.com")];
        let mut blocks = [block("a")];
        attach(&mut blocks, &commits, |_| Some(0), &["eve@work.example".to_string()]);
        assert_eq!(blocks[0].people.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Ada"]);
        assert_eq!(blocks[0].bus_factor, Some(1));
    }

    #[test]
    fn days() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_763_726_400), "2025-11-21");
        assert_eq!(date(951_782_400), "2000-02-29");
    }
}
