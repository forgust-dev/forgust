//! Report artifacts the server reads (M7): `dotenv` (variables for later jobs)
//! and `junit` (test results). The runner uploads them gzipped (one gzip member
//! per file; several files concatenate) or raw. Everything is bounded: the
//! compressed read, the inflated size, the counts.

use tokio::io::AsyncReadExt;

use crate::state::AppState;

/// GitLab's dotenv limits.
pub const DOTENV_MAX_BYTES: usize = 5 * 1024;
pub const DOTENV_MAX_VARS: usize = 20;

/// The report's bytes, inflated, at most `cap` (an error beyond).
pub async fn read(state: &AppState, blob_key: &str, format: &str, cap: usize) -> anyhow::Result<Vec<u8>> {
    use futures::StreamExt;
    let Some((_, mut stream)) = state.blobs.get(blob_key).await? else { anyhow::bail!("the report is gone") };
    // compressed input is bounded too, by the same cap (what compresses grows)
    let in_cap = cap.max(64 * 1024);
    let mut raw = Vec::new();
    while let Some(chunk) = stream.next().await {
        raw.extend_from_slice(&chunk?);
        if raw.len() > in_cap {
            anyhow::bail!("the report is larger than {} KB", cap / 1024);
        }
    }
    inflate(&raw, format, cap).await
}

pub async fn inflate(raw: &[u8], format: &str, cap: usize) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    if format == "gzip" {
        let mut d = async_compression::tokio::bufread::GzipDecoder::new(raw);
        d.multiple_members(true);
        let n = d.take(cap as u64 + 1).read_to_end(&mut out).await?;
        if n > cap {
            anyhow::bail!("the report is larger than {} KB", cap / 1024);
        }
    } else {
        if raw.len() > cap {
            anyhow::bail!("the report is larger than {} KB", cap / 1024);
        }
        out.extend_from_slice(raw);
    }
    Ok(out)
}

/// GitLab's dotenv rules: `KEY=value` lines; keys of ASCII letters, digits and
/// `_`; no empty lines, comments, quoting or multiline values; leading and
/// trailing spaces stripped; the last of a repeated key wins; at most 20.
pub fn parse_dotenv(data: &[u8]) -> Result<Vec<(String, String)>, String> {
    if data.len() > DOTENV_MAX_BYTES {
        return Err(format!("dotenv report is larger than {} KB", DOTENV_MAX_BYTES / 1024));
    }
    let text = std::str::from_utf8(data).map_err(|_| "dotenv report is not UTF-8".to_string())?;
    let mut out: Vec<(String, String)> = Vec::new();
    if text.trim().is_empty() {
        return Ok(out);
    }
    for (n, line) in text.trim_end_matches(['\n', '\r']).split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else { return Err(format!("dotenv line {}: not KEY=value", n + 1)) };
        let k = k.trim();
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("dotenv line {}: `{k}` is not a valid variable name", n + 1));
        }
        let v = v.trim().to_string();
        out.retain(|(o, _)| o != k);
        out.push((k.to_string(), v));
        if out.len() > DOTENV_MAX_VARS {
            return Err(format!("dotenv report has more than {DOTENV_MAX_VARS} variables"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv() {
        assert_eq!(
            parse_dotenv(b"VERSION=1.2.3\nURL = https://x/?a=b\nVERSION=1.2.4\n").unwrap(),
            vec![("URL".into(), "https://x/?a=b".into()), ("VERSION".into(), "1.2.4".into())]
        );
        assert_eq!(parse_dotenv(b"Q=\"kept\"\r\n").unwrap(), vec![("Q".into(), "\"kept\"".into())]);
        assert_eq!(parse_dotenv(b"").unwrap(), vec![]);
        assert!(parse_dotenv(b"A=1\n\nB=2\n").is_err(), "empty lines");
        assert!(parse_dotenv(b"# c\nA=1\n").is_err(), "comments");
        assert!(parse_dotenv(b"export A=1\n").is_err(), "names");
        assert!(parse_dotenv(b"A-B=1\n").is_err());
        let many: String = (0..21).map(|i| format!("V{i}=x\n")).collect();
        assert!(parse_dotenv(many.as_bytes()).is_err());
        assert!(parse_dotenv(&vec![b'a'; 6000]).is_err());
    }

    #[tokio::test]
    async fn gzip_members_concatenate_and_are_bounded() {
        use async_compression::tokio::write::GzipEncoder;
        use tokio::io::AsyncWriteExt;
        async fn gz(s: &[u8]) -> Vec<u8> {
            let mut e = GzipEncoder::new(Vec::new());
            e.write_all(s).await.unwrap();
            e.shutdown().await.unwrap();
            e.into_inner()
        }
        let mut both = gz(b"<a/>").await;
        both.extend(gz(b"<b/>").await);
        assert_eq!(inflate(&both, "gzip", 100).await.unwrap(), b"<a/><b/>");
        let bomb = gz(&vec![0u8; 1 << 20]).await;
        assert!(inflate(&bomb, "gzip", 1024).await.is_err());
        assert_eq!(inflate(b"raw", "raw", 10).await.unwrap(), b"raw");
    }
}

// ---------------------------------------------------------------- JUnit

/// GitLab's per-file limit; several files concatenate into one report.
pub const JUNIT_MAX_BYTES: usize = 30 << 20;
/// Failures kept with their message and output.
const MAX_FAILURES: usize = 200;
/// Cases kept (name and status) to compare with the target branch.
const MAX_CASES: usize = 20_000;
const MAX_TEXT: usize = 4096;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct TestCase {
    pub suite: String,
    pub classname: String,
    pub name: String,
    /// success, failed, error or skipped
    pub status: String,
    pub time: f64,
    pub message: String,
    pub output: String,
}

impl TestCase {
    /// What identifies a test across pipelines.
    pub fn key(&self) -> String {
        format!("{}::{}", self.classname, self.name)
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct TestSummary {
    pub total: i32,
    pub failed: i32,
    pub errored: i32,
    pub skipped: i32,
    pub duration: f64,
    /// Failing and erroring cases, the first `MAX_FAILURES`.
    pub failures: Vec<TestCase>,
    /// Each case, the first `MAX_CASES`.
    pub cases: Vec<CaseRow>,
}

/// One test case as stored for listing and comparing: `[key, status, time,
/// suite, name]` (reports stored before had only `[key, status]`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct CaseRow(pub String, pub String, pub f64, pub String, pub String);

impl CaseRow {
    /// Rows of a stored `cases` value, whichever form it has.
    pub fn read(v: &serde_json::Value) -> Vec<CaseRow> {
        let Some(list) = v.as_array() else { return Vec::new() };
        list.iter()
            .filter_map(|r| {
                let r = r.as_array()?;
                let s = |i: usize| r.get(i).and_then(|x| x.as_str()).unwrap_or_default().to_string();
                Some(CaseRow(s(0), s(1), r.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0), s(3), s(4)))
            })
            .filter(|c| !c.0.is_empty())
            .collect()
    }
}

fn clip(s: &str) -> String {
    if s.len() <= MAX_TEXT {
        return s.to_string();
    }
    let mut end = MAX_TEXT;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Test cases of one or more concatenated JUnit XML documents. A duplicate
/// name counts once, the first one (as GitLab).
pub fn parse_junit(data: &[u8]) -> Result<TestSummary, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(data);
    let mut buf = Vec::new();
    let mut suites: Vec<String> = Vec::new();
    let mut case: Option<TestCase> = None;
    // inside <failure>/<error>/<skipped>/<system-out> of the current case
    let mut text_target: Option<&'static str> = None;
    let mut seen = std::collections::HashSet::new();
    let mut out = TestSummary::default();
    let attr = |e: &quick_xml::events::BytesStart, name: &str| -> String {
        e.try_get_attribute(name)
            .ok()
            .flatten()
            .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned()))
            .unwrap_or_default()
    };
    let mut finish = |c: TestCase, out: &mut TestSummary| {
        if !seen.insert(c.key()) {
            return;
        }
        out.total += 1;
        out.duration += c.time;
        match c.status.as_str() {
            "failed" => out.failed += 1,
            "error" => out.errored += 1,
            "skipped" => out.skipped += 1,
            _ => {}
        }
        if out.cases.len() < MAX_CASES {
            out.cases.push(CaseRow(c.key(), c.status.clone(), c.time, c.suite.clone(), c.name.clone()));
        }
        if matches!(c.status.as_str(), "failed" | "error") && out.failures.len() < MAX_FAILURES {
            out.failures.push(c);
        }
    };
    loop {
        let ev = reader.read_event_into(&mut buf).map_err(|e| format!("JUnit XML: {e}"))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().into_inner() {
                    "testsuite" if !empty => suites.push(attr(e, "name")),
                    "testcase" => {
                        let c = TestCase {
                            suite: suites.last().cloned().unwrap_or_default(),
                            classname: attr(e, "classname"),
                            name: attr(e, "name"),
                            status: "success".into(),
                            time: attr(e, "time").parse().unwrap_or(0.0),
                            ..TestCase::default()
                        };
                        if empty {
                            finish(c, &mut out);
                        } else {
                            case = Some(c);
                        }
                    }
                    tag @ ("failure" | "error" | "skipped") => {
                        if let Some(c) = case.as_mut() {
                            c.status = match tag {
                                "failure" => "failed",
                                "error" => "error",
                                _ => "skipped",
                            }
                            .into();
                            let m = attr(e, "message");
                            if !m.is_empty() {
                                c.message = clip(&m);
                            }
                            if !empty {
                                text_target = Some("output");
                            }
                        }
                    }
                    "system-out" | "system-err" if !empty && case.is_some() => text_target = Some("output"),
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let (Some(_), Some(c)) = (text_target, case.as_mut())
                    && c.output.len() < MAX_TEXT
                {
                    c.output = clip(&format!("{}{}", c.output, t.into_inner()));
                }
            }
            Event::CData(t) => {
                if let (Some(_), Some(c)) = (text_target, case.as_mut())
                    && c.output.len() < MAX_TEXT
                {
                    c.output = clip(&format!("{}{}", c.output, t.into_inner()));
                }
            }
            // `&amp;` and `&#38;` arrive on their own
            Event::GeneralRef(r) => {
                if let (Some(_), Some(c)) = (text_target, case.as_mut()) {
                    let ch = match r.resolve_char_ref() {
                        Ok(Some(ch)) => Some(ch.to_string()),
                        _ => quick_xml::escape::resolve_predefined_entity(&r.into_inner()).map(str::to_string),
                    };
                    if let Some(ch) = ch {
                        c.output = clip(&format!("{}{ch}", c.output));
                    }
                }
            }
            Event::End(ref e) => match e.local_name().into_inner() {
                "testsuite" => {
                    suites.pop();
                }
                "testcase" => {
                    if let Some(c) = case.take() {
                        finish(c, &mut out);
                    }
                    text_target = None;
                }
                "failure" | "error" | "skipped" | "system-out" | "system-err" => text_target = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[cfg(test)]
mod junit_tests {
    use super::*;

    #[test]
    fn junit_counts_and_failures() {
        let a = br#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites><testsuite name="unit">
  <testcase classname="cart" name="adds" time="0.5"/>
  <testcase classname="cart" name="removes" time="0.25"><failure message="expected 1">assert 1 == 2 &amp; more</failure></testcase>
  <testcase classname="cart" name="adds" time="9"/>
  <testcase classname="cart" name="later"><skipped/></testcase>
</testsuite></testsuites>"#;
        let b = br#"<?xml version="1.0"?><testsuite name="api"><testcase classname="http" name="500"><error message="boom"><![CDATA[trace]]></error></testcase></testsuite>"#;
        let mut both = a.to_vec();
        both.extend_from_slice(b);
        let s = parse_junit(&both).unwrap();
        assert_eq!((s.total, s.failed, s.errored, s.skipped), (4, 1, 1, 1));
        assert!((s.duration - 0.75).abs() < 1e-9, "the duplicate is ignored");
        assert_eq!(s.failures[0].name, "removes");
        assert_eq!(s.failures[0].message, "expected 1");
        assert_eq!(s.failures[0].output, "assert 1 == 2 & more");
        assert_eq!(s.failures[0].suite, "unit");
        assert_eq!(s.failures[1].output, "trace");
        assert!(parse_junit(b"<testsuite><testcase").is_err() || parse_junit(b"<testsuite><testcase").unwrap().total == 0);
    }
}

/// Read and store a finished job's JUnit report (a background job, once).
pub async fn store_test_report(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    let Some(a) = forgust_db::ci::artifact(&state.db, build_id, "junit").await? else { return Ok(()) };
    let parsed = match read(state, &a.blob_key, &a.file_format, JUNIT_MAX_BYTES).await {
        Ok(data) => parse_junit(&data),
        Err(e) => Err(e.to_string()),
    };
    let (s, error) = match parsed {
        Ok(s) => (s, None),
        Err(e) => (TestSummary::default(), Some(e)),
    };
    let failures = serde_json::to_value(&s.failures)?;
    let cases = serde_json::to_value(&s.cases)?;
    forgust_db::ci::set_test_report(
        &state.db,
        forgust_db::ci::NewTestReport {
            build_id,
            pipeline_id: b.pipeline_id,
            total: s.total,
            failed: s.failed,
            errored: s.errored,
            skipped: s.skipped,
            duration: s.duration,
            failures: &failures,
            cases: &cases,
            error: error.as_deref(),
        },
    )
    .await?;
    Ok(())
}

/// After a job ends: queue reading its test report, when it uploaded one.
/// A finished job's reports to read later: test results, security findings
/// (its artifacts listed once, both queued in one transaction).
pub async fn after_job(state: &AppState, build_id: i64, project_id: i64) {
    let r: anyhow::Result<()> = async {
        let arts = forgust_db::ci::artifacts_of_build(&state.db, build_id).await?;
        let tests = arts.iter().any(|a| a.file_type == "junit");
        let security = arts.iter().any(|a| crate::ci::security::REPORT_TYPES.contains(&a.file_type.as_str()));
        if tests || security {
            let mut tx = state.db.begin().await?;
            if tests {
                state.jobs.enqueue(&mut tx, crate::jobs::ParseTestReport { build_id }.request()).await?;
            }
            if security {
                state.jobs.enqueue(&mut tx, crate::jobs::ReadSecurityReports { build_id, project_id }.request()).await?;
            }
            tx.commit().await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(build_id, error = %e, "job reports not queued");
    }
}
