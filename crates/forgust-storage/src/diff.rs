//! Structured diffs: `git diff --numstat` for the file list, then a unified patch for
//! the requested page of files, parsed into hunks and lines.

use serde::Serialize;

#[derive(Debug, Clone)]
pub struct DiffOptions {
    /// Index of the first file in the page.
    pub offset: usize,
    /// Files per page.
    pub limit: usize,
    /// Lines kept per file before the file is collapsed.
    pub max_lines_per_file: usize,
    /// Only these paths (old or new).
    pub paths: Option<Vec<String>>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions { offset: 0, limit: 100, max_lines_per_file: 5000, paths: None }
    }
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Diff {
    pub from: String,
    pub to: String,
    pub total_files: usize,
    pub additions: u64,
    pub deletions: u64,
    pub offset: usize,
    pub files: Vec<DiffFile>,
    /// Lightweight list of every changed file (path + stats), for the file tree.
    pub summary: Vec<FileStat>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FileStat {
    pub old_path: String,
    pub new_path: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default, utoipa::ToSchema)]
pub struct DiffFile {
    pub old_path: String,
    pub new_path: String,
    pub status: String,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub similarity: Option<u32>,
    pub binary: bool,
    pub additions: u64,
    pub deletions: u64,
    /// Too many lines: hunks were dropped.
    pub collapsed: bool,
    /// Blob ids from the `index` line (None for mode-only changes).
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
    NoNewline,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
    /// Highlighted HTML of the line, when the server highlighted the diff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

/// Parse `git diff --numstat -z -M` output.
pub fn parse_numstat_z(out: &[u8]) -> Vec<FileStat> {
    let mut res = Vec::new();
    let mut fields = out.split(|&b| b == 0).peekable();
    while let Some(head) = fields.next() {
        if head.is_empty() {
            continue;
        }
        let head = String::from_utf8_lossy(head);
        let mut parts = head.splitn(3, '\t');
        let add = parts.next().unwrap_or("-");
        let del = parts.next().unwrap_or("-");
        let path = parts.next().unwrap_or("");
        let (old_path, new_path) = if path.is_empty() {
            // rename/copy: two more NUL-separated fields follow
            let old = fields.next().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            let new = fields.next().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            (old, new)
        } else {
            (path.to_string(), path.to_string())
        };
        res.push(FileStat { old_path, new_path, additions: add.parse().ok(), deletions: del.parse().ok() });
    }
    res
}

/// Undo git's C-style quoting of paths (`"a/caf\303\251"`).
fn unquote(s: &str) -> String {
    let s = s.trim_end_matches('\t');
    if !(s.starts_with('"') && s.ends_with('"') && s.len() >= 2) {
        return s.to_string();
    }
    let inner = &s.as_bytes()[1..s.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] == b'\\' && i + 1 < inner.len() {
            let c = inner[i + 1];
            match c {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'"' => out.push(b'"'),
                b'\\' => out.push(b'\\'),
                b'0'..=b'7' if i + 3 < inner.len() + 1 => {
                    let oct = std::str::from_utf8(&inner[i + 1..(i + 4).min(inner.len())]).unwrap_or("0");
                    out.push(u8::from_str_radix(oct, 8).unwrap_or(b'?'));
                    i += 4;
                    continue;
                }
                other => out.push(other),
            }
            i += 2;
        } else {
            out.push(inner[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_prefix(p: &str, prefix: &str) -> String {
    let p = unquote(p);
    if p == "/dev/null" {
        return String::new();
    }
    p.strip_prefix(prefix).map(str::to_string).unwrap_or(p)
}

fn parse_range(s: &str) -> (u32, u32) {
    let mut it = s.splitn(2, ',');
    let start = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let len = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
    (start, len)
}

/// Parse unified diff text (`git diff -p --src-prefix=a/ --dst-prefix=b/`).
pub fn parse_unified_diff(text: &str, max_lines_per_file: usize) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut cur: Option<DiffFile> = None;
    let mut line_count = 0usize;
    let mut in_hunks = false;
    let (mut old_no, mut new_no) = (0u32, 0u32);

    let finish = |files: &mut Vec<DiffFile>, f: Option<DiffFile>| {
        if let Some(mut f) = f {
            if f.status.is_empty() {
                f.status = if f.old_path != f.new_path { "renamed".into() } else { "modified".into() };
            }
            files.push(f);
        }
    };

    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish(&mut files, cur.take());
            line_count = 0;
            in_hunks = false;
            // best effort; exact paths come from ---/+++ or rename lines
            let (a, b) = match rest.find(" b/") {
                Some(i) if !rest.starts_with('"') => (rest[..i].to_string(), rest[i + 1..].to_string()),
                _ => {
                    let mut parts = rest.splitn(2, "\" \"");
                    (parts.next().unwrap_or("").to_string() + "\"", "\"".to_string() + parts.next().unwrap_or(""))
                }
            };
            cur = Some(DiffFile { old_path: strip_prefix(&a, "a/"), new_path: strip_prefix(&b, "b/"), ..Default::default() });
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if !in_hunks && !line.starts_with("@@") {
            if let Some(m) = line.strip_prefix("new file mode ") {
                f.status = "added".into();
                f.new_mode = Some(m.into());
            } else if let Some(m) = line.strip_prefix("deleted file mode ") {
                f.status = "deleted".into();
                f.old_mode = Some(m.into());
            } else if let Some(m) = line.strip_prefix("old mode ") {
                f.old_mode = Some(m.into());
            } else if let Some(m) = line.strip_prefix("new mode ") {
                f.new_mode = Some(m.into());
            } else if let Some(s) = line.strip_prefix("similarity index ") {
                f.similarity = s.trim_end_matches('%').parse().ok();
            } else if let Some(p) = line.strip_prefix("rename from ") {
                f.old_path = unquote(p);
                f.status = "renamed".into();
            } else if let Some(p) = line.strip_prefix("rename to ") {
                f.new_path = unquote(p);
            } else if let Some(p) = line.strip_prefix("copy from ") {
                f.old_path = unquote(p);
                f.status = "copied".into();
            } else if let Some(p) = line.strip_prefix("copy to ") {
                f.new_path = unquote(p);
            } else if let Some(p) = line.strip_prefix("--- ") {
                f.old_path = strip_prefix(p, "a/");
            } else if let Some(p) = line.strip_prefix("+++ ") {
                f.new_path = strip_prefix(p, "b/");
            } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
                f.binary = true;
            } else if let Some(idx) = line.strip_prefix("index ") {
                // "index abc..def 100644"
                if let Some((a, b)) = idx.split(' ').next().and_then(|r| r.split_once("..")) {
                    let zero = |o: &str| o.bytes().all(|c| c == b'0');
                    f.old_oid = (!zero(a)).then(|| a.to_string());
                    f.new_oid = (!zero(b)).then(|| b.to_string());
                }
                if let Some(mode) = idx.split(' ').nth(1) {
                    f.old_mode.get_or_insert_with(|| mode.to_string());
                    f.new_mode.get_or_insert_with(|| mode.to_string());
                }
            }
            if f.old_mode.is_some() && f.new_mode.is_some() && f.old_mode != f.new_mode && f.status.is_empty() {
                f.status = "mode_changed".into();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            in_hunks = true;
            // @@ -a,b +c,d @@ section
            let end = rest.find(" @@").unwrap_or(rest.len());
            let ranges = &rest[..end];
            let mut it = ranges.split(' ');
            let (os, ol) = parse_range(it.next().unwrap_or("-0").trim_start_matches('-'));
            let (ns, nl) = parse_range(it.next().unwrap_or("+0").trim_start_matches('+'));
            old_no = os;
            new_no = ns;
            if !f.collapsed {
                f.hunks.push(DiffHunk {
                    header: line.to_string(),
                    old_start: os,
                    old_lines: ol,
                    new_start: ns,
                    new_lines: nl,
                    lines: Vec::new(),
                });
            }
            continue;
        }
        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (DiffLineKind::Add, &line[1..]),
            Some(b'-') => (DiffLineKind::Del, &line[1..]),
            Some(b' ') => (DiffLineKind::Context, &line[1..]),
            Some(b'\\') => (DiffLineKind::NoNewline, line),
            _ => continue,
        };
        let (o, n) = match kind {
            DiffLineKind::Add => {
                f.additions += 1;
                new_no += 1;
                (None, Some(new_no - 1))
            }
            DiffLineKind::Del => {
                f.deletions += 1;
                old_no += 1;
                (Some(old_no - 1), None)
            }
            DiffLineKind::Context => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
            DiffLineKind::NoNewline => (None, None),
        };
        line_count += 1;
        if line_count > max_lines_per_file {
            if !f.collapsed {
                f.collapsed = true;
                f.hunks.clear();
            }
            continue;
        }
        if let Some(h) = f.hunks.last_mut() {
            h.lines.push(DiffLine { kind, old: o, new: n, text: text.to_string(), html: None });
        }
    }
    finish(&mut files, cur.take());
    files
}

/// Where a line on the old side of `file` ends up on its new side: `None` when the
/// line was removed or rewritten (the comment becomes outdated). Lines outside the
/// hunks move by the size changes of the hunks above them. Requires a file that was
/// not collapsed.
pub fn map_old_line_to_new(file: &DiffFile, line: u32) -> Option<u32> {
    if file.status == "deleted" || file.collapsed {
        return None;
    }
    let mut delta: i64 = 0;
    for h in &file.hunks {
        if h.old_lines == 0 {
            // pure insertion after old line `old_start`
            if line <= h.old_start {
                return Some((line as i64 + delta) as u32);
            }
            delta += h.new_lines as i64;
            continue;
        }
        if line < h.old_start {
            return Some((line as i64 + delta) as u32);
        }
        if line < h.old_start + h.old_lines {
            return h.lines.iter().find(|l| l.old == Some(line)).and_then(|l| if l.kind == DiffLineKind::Context { l.new } else { None });
        }
        delta += h.new_lines as i64 - h.old_lines as i64;
    }
    Some((line as i64 + delta) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_lines_across_hunks() {
        // 3 lines inserted at the top, line 10 rewritten
        let text = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,3 @@\n+a\n+b\n+c\n@@ -9,3 +12,3 @@\n 9\n-10\n+TEN\n 11\n";
        let f = &parse_unified_diff(text, 1000)[0];
        assert_eq!(map_old_line_to_new(f, 5), Some(8));
        assert_eq!(map_old_line_to_new(f, 9), Some(12));
        assert_eq!(map_old_line_to_new(f, 10), None);
        assert_eq!(map_old_line_to_new(f, 11), Some(14));
        assert_eq!(map_old_line_to_new(f, 40), Some(43));
    }

    #[test]
    fn deletions_shift_lines_up() {
        let text = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -2,2 +1,0 @@\n-x\n-y\n";
        let f = &parse_unified_diff(text, 1000)[0];
        assert_eq!(map_old_line_to_new(f, 1), Some(1));
        assert_eq!(map_old_line_to_new(f, 2), None);
        assert_eq!(map_old_line_to_new(f, 5), Some(3));
    }

    #[test]
    fn parses_modified_added_renamed() {
        let text = "diff --git a/src/a.rs b/src/a.rs\nindex 111..222 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,3 @@ fn x\n one\n-two\n+TWO\n three\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 000..333\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n\\ No newline at end of file\ndiff --git a/old b/renamed\nsimilarity index 100%\nrename from old\nrename to renamed\n";
        let files = parse_unified_diff(text, 100);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].status, "modified");
        assert_eq!((files[0].additions, files[0].deletions), (1, 1));
        assert_eq!(files[0].hunks[0].lines[2].new, Some(2));
        assert_eq!((files[0].old_oid.as_deref(), files[0].new_oid.as_deref()), (Some("111"), Some("222")));
        assert_eq!(files[1].old_oid, None);
        assert_eq!(files[1].status, "added");
        assert_eq!(files[1].new_path, "new.txt");
        assert_eq!(files[1].old_path, "");
        assert_eq!(files[2].status, "renamed");
        assert_eq!(files[2].old_path, "old");
        assert_eq!(files[2].new_path, "renamed");
    }

    #[test]
    fn collapses_big_files() {
        let mut text = String::from("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,0 +1,10 @@\n");
        for i in 0..10 {
            text.push_str(&format!("+{i}\n"));
        }
        let files = parse_unified_diff(&text, 5);
        assert!(files[0].collapsed);
        assert!(files[0].hunks.is_empty());
        assert_eq!(files[0].additions, 10);
    }

    #[test]
    fn numstat_with_rename() {
        let out = b"1\t2\tsrc/a.rs\x000\t0\t\x00old\x00new\x00-\t-\timg.png\x00";
        let s = parse_numstat_z(out);
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].old_path, "old");
        assert_eq!(s[1].new_path, "new");
        assert_eq!(s[2].additions, None);
    }

    #[test]
    fn unquotes_octal() {
        assert_eq!(unquote("\"a/caf\\303\\251\""), "a/café");
    }
}
