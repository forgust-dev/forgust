//! Answering a code search: ask the index for candidate files, read them from
//! git and keep the lines that really match.
//!
//! Permissions are applied twice. The projects the caller may read go into
//! the index query, so limits, "there is more" and timing say nothing about
//! hidden code. Each project that has a match is then checked with the same
//! `ReadCode` rule as the file browser, which also covers token scopes and
//! organisations that require two-factor.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use forgust_db::code_search::SearchedProject;

use super::index::{Candidate, Filter};
use super::query::{Matcher, Need, QueryError, plan};
use super::searchable_text;
use crate::api::{group_for, project_for};
use crate::auth::Principal;
use crate::auth::authz::{GroupAction, ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Projects one search covers; a caller who may read more is told the answer
/// is partial.
const MAX_PROJECTS: i64 = 50_000;
/// Blobs read from git at a time.
const BATCH: usize = 16;
/// Lines shown around a matching line.
const CONTEXT: usize = 2;
/// Matching lines shown per file (all are counted).
const MAX_SHOWN: usize = 5;
/// Characters of a line that are sent; a minified file has very long ones.
const MAX_LINE_CHARS: usize = 400;

#[derive(Debug, Clone, Default)]
pub struct Params {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// Text the path contains, any case.
    pub path: Option<String>,
    /// Exact file name, any case.
    pub filename: Option<String>,
    /// File extension without the dot, any case.
    pub extension: Option<String>,
    /// Language or file type (`languages::file_type_of`), any case.
    pub language: Option<String>,
    /// Only this project.
    pub project_id: Option<i64>,
    /// Only the projects under this group (its full path).
    pub group_path: Option<String>,
    /// Files to return.
    pub limit: usize,
    /// Start after this file (`next` of the previous answer).
    pub after: Option<(i64, String)>,
}

impl Params {
    /// A search limited to a project or a group (ids or full paths) the
    /// caller may see, going on after `after`; the rest is left to fill in.
    pub async fn scoped(
        state: &AppState,
        principal: &Principal,
        project: Option<&str>,
        group: Option<&str>,
        after: Option<&str>,
    ) -> ApiResult<Params> {
        let project_id = match project {
            Some(id) => Some(project_for(state, principal, id, ProjectAction::ReadCode).await?.id),
            None => None,
        };
        let group_path = match group {
            Some(id) => Some(group_for(state, principal, id, GroupAction::Read).await?.full_path),
            None => None,
        };
        Ok(Params { project_id, group_path, after: after.map(decode_cursor).transpose()?, ..Params::default() })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    /// Byte ranges of the matches in `text`.
    pub ranges: Vec<(usize, usize)>,
}

/// Consecutive lines of a file, some of which match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Number of the first line (from 1).
    pub start: usize,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone)]
pub struct FileHit {
    pub project_id: i64,
    pub project_path: String,
    pub path: String,
    /// The default branch when the file was indexed.
    pub branch: String,
    /// The commit that was indexed.
    pub commit: String,
    pub language: Option<&'static str>,
    /// Lines that match in the whole file.
    pub matches: usize,
    pub chunks: Vec<Chunk>,
}

#[derive(Debug, Default)]
pub struct Found {
    pub files: Vec<FileHit>,
    /// Where to go on from, when there may be more.
    pub next: Option<(i64, String)>,
    /// Not everything was looked at: too many candidate files, or out of time.
    pub partial: bool,
    /// Projects of this search that are not indexed yet (counted on the
    /// first page only).
    pub not_indexed: i64,
}

impl From<QueryError> for ApiError {
    fn from(e: QueryError) -> Self {
        ApiError::bad(e.to_string())
    }
}

/// `"{project_id}:{path}"`, the form `next` and `after` travel in.
pub fn encode_cursor(c: &(i64, String)) -> String {
    format!("{}:{}", c.0, c.1)
}

pub fn decode_cursor(s: &str) -> ApiResult<(i64, String)> {
    s.split_once(':')
        .and_then(|(id, path)| Some((id.parse().ok()?, path.to_string())))
        .ok_or_else(|| ApiError::bad("`after` must be the `next` of an earlier answer"))
}

fn cut(line: &str) -> &str {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((at, _)) => &line[..at],
        None => line,
    }
}

/// The matching lines of a file with their context, and how many lines match.
pub fn match_lines(text: &str, matcher: &Matcher) -> (usize, Vec<Chunk>) {
    let lines: Vec<&str> = text.lines().collect();
    let hits: Vec<(usize, Vec<(usize, usize)>)> =
        lines.iter().enumerate().filter_map(|(i, l)| Some((i, matcher.find(l))).filter(|(_, r)| !r.is_empty())).collect();
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut end = 0; // one past the last line of the last chunk
    for (i, _) in hits.iter().take(MAX_SHOWN) {
        let from = i.saturating_sub(CONTEXT);
        let to = (i + CONTEXT + 1).min(lines.len());
        let from = match chunks.last() {
            // touching or overlapping the previous chunk: continue it
            Some(_) if from <= end => end,
            _ => {
                chunks.push(Chunk { start: from + 1, lines: Vec::new() });
                from
            }
        };
        let chunk = chunks.last_mut().expect("just pushed");
        for (n, line) in lines.iter().enumerate().take(to).skip(from) {
            let text = cut(line);
            let ranges = hits
                .binary_search_by_key(&n, |(i, _)| *i)
                .map(|at| hits[at].1.iter().filter(|(s, _)| *s < text.len()).map(|(s, e)| (*s, (*e).min(text.len()))).collect())
                .unwrap_or_default();
            chunk.lines.push(Line { text: text.to_string(), ranges });
        }
        end = to;
    }
    (hits.len(), chunks)
}

/// Search the code the caller may read.
pub async fn search(state: &AppState, principal: &Principal, params: Params) -> ApiResult<Found> {
    let Some(index) = state.code_index.clone() else {
        return Err(ApiError::Unavailable("code search is turned off on this instance".into()));
    };
    let (need, matcher) = plan(params.text.trim(), params.regex, params.case_sensitive)?;
    // a pattern the index cannot narrow reads every file: only inside one project
    if need == Need::Any && params.project_id.is_none() {
        return Err(QueryError::NoLiteral.into());
    }
    let viewer = principal.search_viewer();
    let not_indexed = if params.after.is_none() {
        forgust_db::code_search::not_indexed(&state.db, viewer, index.generation(), params.project_id, params.group_path.as_deref()).await?
    } else {
        0
    };
    let projects = forgust_db::code_search::readable_projects(
        &state.db,
        viewer,
        index.generation(),
        params.project_id,
        params.group_path.as_deref(),
        MAX_PROJECTS,
    )
    .await?;
    let mut found = Found { partial: projects.len() as i64 == MAX_PROJECTS, not_indexed, ..Found::default() };
    if projects.is_empty() || params.limit == 0 {
        return Ok(found);
    }
    let given = |filter: &Option<String>| filter.as_deref().map(str::trim).filter(|f| !f.is_empty()).map(str::to_string);
    let path_filter = given(&params.path).map(|p| p.to_lowercase());
    let suffix = given(&params.extension).map(|e| format!(".{}", e.trim_start_matches('.').to_lowercase()));
    let filter = Filter {
        need,
        projects: Some(projects.iter().map(|p| p.id).collect()),
        path: path_filter.iter().chain(&suffix).cloned().collect(),
        filename: given(&params.filename),
        lang: given(&params.language),
    };
    let (candidates, capped) = tokio::task::spawn_blocking(move || index.candidates(&filter)).await.map_err(anyhow::Error::from)??;
    found.partial |= capped;
    let projects: HashMap<i64, SearchedProject> = projects.into_iter().map(|p| (p.id, p)).collect();
    let candidates: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| params.after.as_ref().is_none_or(|(id, path)| (c.project_id, c.path.as_str()) > (*id, path.as_str())))
        .filter(|c| path_filter.as_ref().is_none_or(|p| c.path.to_lowercase().contains(p)))
        .filter(|c| suffix.as_ref().is_none_or(|s| c.path.to_lowercase().ends_with(s)))
        .collect();

    let deadline = Instant::now() + Duration::from_millis(state.cfg.code_search.query_timeout_ms);
    let max_bytes = state.cfg.code_search.max_file_bytes;
    // whether the caller may read each project that had a match
    let mut allowed: HashMap<i64, bool> = HashMap::new();
    let mut at = 0;
    'all: while at < candidates.len() {
        if Instant::now() > deadline {
            // out of time: the next request goes on from the last file looked at
            found.partial = true;
            found.next = at.checked_sub(1).map(|last| (candidates[last].project_id, candidates[last].path.clone()));
            break;
        }
        let project_id = candidates[at].project_id;
        let run = candidates[at..].iter().take(BATCH).take_while(|c| c.project_id == project_id).count();
        let batch = &candidates[at..at + run];
        at += run;
        if allowed.get(&project_id) == Some(&false) {
            continue;
        }
        let Some(project) = projects.get(&project_id) else { continue };
        let oids: Vec<String> = batch.iter().map(|c| c.oid.clone()).collect();
        // a repository that went away since it was indexed has nothing to show
        let Ok(blobs) = state.storage.blobs_by_id(project_id, &oids, max_bytes as usize + 1).await else {
            allowed.insert(project_id, false);
            continue;
        };
        let texts: Vec<Option<String>> =
            blobs.into_iter().map(|b| b.and_then(|(size, data)| searchable_text(size, &data, max_bytes))).collect();
        let m = matcher.clone();
        let matched =
            tokio::task::spawn_blocking(move || texts.iter().map(|t| t.as_deref().map(|t| match_lines(t, &m))).collect::<Vec<_>>())
                .await
                .map_err(anyhow::Error::from)?;
        for (i, (candidate, lines)) in batch.iter().zip(matched).enumerate() {
            let Some((matches, chunks)) = lines.filter(|(n, _)| *n > 0) else { continue };
            if let std::collections::hash_map::Entry::Vacant(e) = allowed.entry(project_id) {
                let ok = match forgust_db::projects::find(&state.db, project_id).await? {
                    Some(p) => require_project(state, principal, ProjectAction::ReadCode, &p).await.is_ok(),
                    None => false,
                };
                e.insert(ok);
            }
            if !allowed[&project_id] {
                break;
            }
            found.files.push(FileHit {
                project_id,
                project_path: project.full_path.clone(),
                path: candidate.path.clone(),
                branch: project.default_branch.clone().unwrap_or_default(),
                commit: project.indexed_commit.clone().unwrap_or_default(),
                language: crate::languages::file_type_of(&candidate.path),
                matches,
                chunks,
            });
            if found.files.len() == params.limit {
                // more may follow: the rest of this batch, or of the candidates
                if at < candidates.len() || i + 1 < batch.len() {
                    found.next = Some((project_id, candidate.path.clone()));
                }
                break 'all;
            }
        }
    }
    Ok(found)
}

/// The lines of a chunk as HTML: syntax colours from the highlighter, the
/// matches inside `<mark>`.
pub fn chunk_html(state: &AppState, path: &str, chunk: &Chunk) -> Vec<String> {
    let text: String = chunk.lines.iter().flat_map(|l| [l.text.as_str(), "\n"]).collect();
    let html = state.highlighter.highlight_lines(path, &text);
    chunk
        .lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            html.get(i)
                .and_then(|h| mark_html(h, &line.text, &line.ranges))
                .unwrap_or_else(|| mark_html(&crate::render::escape_html(&line.text), &line.text, &line.ranges).unwrap_or_default())
        })
        .collect()
}

/// Wrap the byte `ranges` of `plain` in `<mark>` inside `html`, the same text
/// with tags and entities. A mark is closed before a tag and opened again at
/// the next character of the match, so the result nests properly and no mark
/// is empty. `None` when `html` is not `plain`.
fn mark_html(html: &str, plain: &str, ranges: &[(usize, usize)]) -> Option<String> {
    let mut out = String::with_capacity(html.len() + ranges.len() * 13);
    let mut plain_chars = plain.char_indices();
    let mut ranges = ranges.iter().peekable();
    // whether a `<mark>` is open in the output
    let mut open = false;
    let mut rest = html;
    while let Some(c) = rest.chars().next() {
        if c == '<' {
            let end = rest.find('>')? + 1;
            if open {
                out.push_str("</mark>");
                open = false;
            }
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        // one character of text: itself, or an entity
        let unit = if c == '&' { rest.find(';')? + 1 } else { c.len_utf8() };
        let (at, plain_char) = plain_chars.next()?;
        if c != '&' && c != plain_char {
            return None;
        }
        // the current range stays in front until its last character is out
        let inside = ranges.peek().is_some_and(|(s, _)| *s <= at);
        if inside && !open {
            out.push_str("<mark>");
            open = true;
        }
        out.push_str(&rest[..unit]);
        if inside && ranges.peek().is_some_and(|(_, e)| *e <= at + plain_char.len_utf8()) {
            out.push_str("</mark>");
            open = false;
            ranges.next();
        }
        rest = &rest[unit..];
    }
    if open {
        out.push_str("</mark>");
    }
    plain_chars.next().is_none().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(text: &str) -> Matcher {
        plan(text, false, false).unwrap().1
    }

    #[test]
    fn matching_lines_come_with_context_and_merge() {
        let text = (1..=30)
            .map(|n| if [5, 7, 20].contains(&n) { format!("line {n} needle") } else { format!("line {n}") })
            .collect::<Vec<_>>()
            .join("\n");
        let (count, chunks) = match_lines(&text, &matcher("needle"));
        assert_eq!(count, 3);
        assert_eq!(chunks.iter().map(|c| (c.start, c.lines.len())).collect::<Vec<_>>(), vec![(3, 7), (18, 5)]);
        assert_eq!(chunks[0].lines[2], Line { text: "line 5 needle".into(), ranges: vec![(7, 13)] });
        assert!(chunks[0].lines[3].ranges.is_empty());
        // the first and last lines of a file have less context
        let (_, edge) = match_lines("needle\nb\nc\nd\nneedle", &matcher("needle"));
        assert_eq!(edge.iter().map(|c| (c.start, c.lines.len())).collect::<Vec<_>>(), vec![(1, 5)]);
    }

    #[test]
    fn many_matches_are_counted_but_few_are_shown_and_long_lines_are_cut() {
        let text = "needle\n\n\n\n\n\n".repeat(20);
        let (count, chunks) = match_lines(&text, &matcher("needle"));
        assert_eq!((count, chunks.len()), (20, MAX_SHOWN));
        let long = format!("{}needle {}needle", "é".repeat(10), "x".repeat(MAX_LINE_CHARS));
        let (_, chunks) = match_lines(&long, &matcher("needle"));
        assert_eq!(chunks[0].lines[0].text.chars().count(), MAX_LINE_CHARS);
        assert_eq!(chunks[0].lines[0].ranges, vec![(20, 26)]);
    }

    #[test]
    fn marks_go_around_matches_and_never_cross_tags() {
        let plain = "if a < b { call(\"é&\") }";
        let html = "<span class=\"k\">if</span> a &lt; b { <span class=\"f\">call</span>(&quot;é&amp;&quot;) }";
        // "a < b", then "call(\"é&" which starts inside a span and ends outside
        let got = mark_html(html, plain, &[(3, 8), (11, 20)]).unwrap();
        assert_eq!(
            got,
            "<span class=\"k\">if</span> <mark>a &lt; b</mark> { <span class=\"f\"><mark>call</mark></span><mark>(&quot;é&amp;</mark>&quot;) }"
        );
        // a tag right where a match starts or ends leaves no empty mark
        assert_eq!(mark_html("<b></b>ab<i>c</i>", "abc", &[(0, 2)]).unwrap(), "<b></b><mark>ab</mark><i>c</i>");
        assert_eq!(mark_html("plain", "plain", &[]).unwrap(), "plain");
        assert_eq!(mark_html("ab", "ab", &[(0, 2)]).unwrap(), "<mark>ab</mark>");
        // html that is not this text is refused, so the caller falls back
        assert_eq!(mark_html("<b>other</b>", "plain", &[(0, 2)]), None);
        assert_eq!(mark_html("pla", "plain", &[]), None);
    }

    #[test]
    fn cursors_round_trip() {
        let c = (42, "src/a:b.rs".to_string());
        assert_eq!(decode_cursor(&encode_cursor(&c)).unwrap(), c);
        assert!(decode_cursor("nope").is_err());
    }
}
