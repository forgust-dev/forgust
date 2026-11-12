//! Security reports (ADR-0024): GitLab's security report JSON
//! (`artifacts:reports:sast`, `secret_detection`, `dependency_scanning`,
//! `container_scanning`) and SARIF 2.1, read into findings. Pure parsing here;
//! storing and states are in `services/security.rs`.

use serde::Serialize;
use serde_json::Value;

/// What a report may hold at most.
pub const MAX_FINDINGS: usize = 5_000;
pub const MAX_BYTES: usize = 16 << 20;

pub const REPORT_TYPES: &[&str] = &["sast", "secret_detection", "dependency_scanning", "container_scanning"];
pub const SEVERITIES: &[&str] = &["critical", "high", "medium", "low", "info", "unknown"];

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct Identifier {
    /// `cve`, `cwe`, `ghsa`, `owasp`, or the scanner's rule type.
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub value: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Finding {
    pub report_type: String,
    pub scanner: String,
    pub severity: String,
    pub title: String,
    pub description: String,
    pub solution: String,
    pub rule_id: String,
    pub identifiers: Vec<Identifier>,
    pub file: Option<String>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub package_name: Option<String>,
    pub package_version: Option<String>,
    pub fixed_version: Option<String>,
    pub image: Option<String>,
    /// A fingerprint the scanner gives (SARIF `partialFingerprints`): steadier
    /// than lines.
    pub stable: Option<String>,
}

impl Finding {
    /// Where it is, for its fingerprint: the package for dependencies, the
    /// image (without its tag) and package for containers; code and secrets
    /// add the text of their lines (`lines`, read from the commit).
    pub fn location_key(&self, lines: Option<&str>) -> String {
        match self.report_type.as_str() {
            "dependency_scanning" => format!("{}:{}", self.file.as_deref().unwrap_or(""), self.package_name.as_deref().unwrap_or("")),
            "container_scanning" => {
                let image = self.image.as_deref().unwrap_or("");
                // `host:port/name:tag` and `name@sha256:...`: drop the tag or digest only
                let name = image.split('@').next().unwrap_or(image);
                let name = match name.rsplit_once(':') {
                    Some((n, tag)) if !tag.contains('/') => n,
                    _ => name,
                };
                format!("{name}:{}", self.package_name.as_deref().unwrap_or(""))
            }
            _ => match (&self.stable, lines) {
                (Some(s), _) => format!("{}:{s}", self.file.as_deref().unwrap_or("")),
                (None, Some(text)) => format!("{}:{}", self.file.as_deref().unwrap_or(""), hash(normalize(text).as_bytes())),
                (None, None) => format!("{}:{}", self.file.as_deref().unwrap_or(""), self.start_line.unwrap_or(0)),
            },
        }
    }

    pub fn fingerprint(&self, lines: Option<&str>) -> String {
        let key = format!("{}|{}|{}", self.report_type, self.rule_id, self.location_key(lines));
        hash(key.as_bytes())[..32].to_string()
    }
}

/// Whitespace runs as one space: re-indenting does not make a new finding.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn hash(b: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(b))
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

fn severity(v: &str) -> String {
    let l = v.trim().to_ascii_lowercase();
    if SEVERITIES.contains(&l.as_str()) { l } else { "unknown".into() }
}

/// A path as the repository has it: no `file://`, no leading `./` or `/`.
fn repo_path(p: &str) -> String {
    let p = p.strip_prefix("file://").unwrap_or(p);
    let p = p.trim_start_matches("./").trim_start_matches('/');
    percent_encoding::percent_decode_str(p).decode_utf8_lossy().into_owned()
}

/// The version a solution names (`Upgrade to version 4.17.21 or above.`,
/// `Upgrade libssl3 to 3.1.4-r0`).
fn fixed_version(solution: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?i)\bupgrade\b.*?\bto(?: version)? v?([0-9][0-9A-Za-z.+\-_:~]*)").ok()?;
    re.captures(solution).map(|c| c[1].trim_end_matches('.').to_string())
}

/// Whether the bytes are SARIF (whatever report type the job declared).
pub fn is_sarif(v: &Value) -> bool {
    v.get("runs").is_some_and(Value::is_array) && v.get("version").and_then(Value::as_str).is_some_and(|x| x.starts_with("2."))
}

/// Read a report: SARIF or GitLab's format for `report_type`. A job that
/// lists several files under one report type uploads them one after the
/// other: each document is read.
pub fn parse(data: &[u8], report_type: &str) -> Result<(String, Vec<Finding>), String> {
    let mut scanners: Vec<String> = Vec::new();
    let mut all = Vec::new();
    for doc in serde_json::Deserializer::from_slice(data).into_iter::<Value>() {
        let v = doc.map_err(|e| format!("not JSON: {e}"))?;
        let (scanner, found) = if is_sarif(&v) { parse_sarif(&v, report_type)? } else { parse_gitlab(&v, report_type)? };
        if !scanners.contains(&scanner) {
            scanners.push(scanner);
        }
        all.extend(found);
        if all.len() > MAX_FINDINGS {
            return Err(format!("more than {MAX_FINDINGS} findings"));
        }
    }
    if scanners.is_empty() {
        return Err("an empty report".into());
    }
    Ok((scanners.join(", "), all))
}

/// GitLab's security report (schema 14 and 15): `(scanner, findings)`.
pub fn parse_gitlab(v: &Value, report_type: &str) -> Result<(String, Vec<Finding>), String> {
    let list = v.get("vulnerabilities").and_then(Value::as_array).ok_or("no `vulnerabilities` list: not a security report")?;
    if list.len() > MAX_FINDINGS {
        return Err(format!("more than {MAX_FINDINGS} findings"));
    }
    let report_type = v["scan"]["type"].as_str().filter(|t| REPORT_TYPES.contains(t)).unwrap_or(report_type).to_string();
    let scan_scanner = s(&v["scan"]["scanner"]["id"]).or_else(|| s(&v["scan"]["scanner"]["name"]));
    let mut out = Vec::new();
    for x in list {
        let scanner = s(&x["scanner"]["id"]).or_else(|| scan_scanner.clone()).unwrap_or_else(|| "unknown".into());
        let identifiers: Vec<Identifier> = x["identifiers"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|i| {
                        Some(Identifier {
                            kind: s(&i["type"])?.to_ascii_lowercase(),
                            name: s(&i["name"]).or_else(|| s(&i["value"]))?,
                            value: s(&i["value"]).or_else(|| s(&i["name"]))?,
                            url: s(&i["url"]),
                        })
                    })
                    .take(20)
                    .collect()
            })
            .unwrap_or_default();
        // the first identifier is the primary one (GitLab's rule)
        let rule_id = identifiers.first().map(|i| i.value.clone()).or_else(|| s(&x["cve"])).unwrap_or_else(|| "unknown".into());
        let loc = &x["location"];
        let title = s(&x["name"])
            .or_else(|| s(&x["message"]))
            .or_else(|| identifiers.first().map(|i| i.name.clone()))
            .unwrap_or_else(|| rule_id.clone());
        let solution = s(&x["solution"]).unwrap_or_default();
        out.push(Finding {
            report_type: report_type.clone(),
            scanner,
            severity: severity(x["severity"].as_str().unwrap_or("unknown")),
            description: s(&x["description"]).unwrap_or_default(),
            fixed_version: fixed_version(&solution),
            solution,
            title,
            rule_id,
            identifiers,
            file: s(&loc["file"]).map(|f| repo_path(&f)),
            start_line: loc["start_line"].as_i64().map(|n| n as i32),
            end_line: loc["end_line"].as_i64().or(loc["start_line"].as_i64()).map(|n| n as i32),
            package_name: s(&loc["dependency"]["package"]["name"]),
            package_version: s(&loc["dependency"]["version"]),
            image: s(&loc["image"]),
            stable: None,
        });
    }
    Ok((scan_scanner.unwrap_or_else(|| "unknown".into()), out))
}

/// `Key: value` lines of a message (Trivy writes `Package:`, `Installed
/// Version:`, `Fixed Version:` there).
fn message_field(message: &str, key: &str) -> Option<String> {
    message
        .lines()
        .find_map(|l| l.trim().strip_prefix(key).and_then(|r| r.strip_prefix(':')).map(|v| v.trim().to_string()))
        .filter(|v| !v.is_empty())
}

/// SARIF 2.1 (CodeQL, Semgrep, Trivy, gitleaks...): every result of every run.
/// Vulnerabilities (CVE, GHSA) are dependencies, or containers when the
/// report was uploaded as `container_scanning`.
pub fn parse_sarif(v: &Value, declared: &str) -> Result<(String, Vec<Finding>), String> {
    let runs = v["runs"].as_array().ok_or("no `runs`")?;
    let mut out = Vec::new();
    let mut tools = Vec::new();
    for run in runs {
        let driver = &run["tool"]["driver"];
        let tool = s(&driver["name"]).unwrap_or_else(|| "unknown".into());
        tools.push(tool.clone());
        let rules = driver["rules"].as_array().cloned().unwrap_or_default();
        for r in run["results"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            if out.len() >= MAX_FINDINGS {
                return Err(format!("more than {MAX_FINDINGS} findings"));
            }
            // suppressed in the source (`// nosemgrep`, `codeql[...]`): not a finding
            if r["suppressions"].as_array().is_some_and(|x| !x.is_empty()) {
                continue;
            }
            let rule_id = s(&r["ruleId"]).or_else(|| s(&r["rule"]["id"])).unwrap_or_else(|| "unknown".into());
            let rule = r["ruleIndex"]
                .as_u64()
                .and_then(|i| rules.get(i as usize))
                .filter(|x| x["id"].as_str() == Some(rule_id.as_str()))
                .or_else(|| rules.iter().find(|x| x["id"].as_str() == Some(rule_id.as_str())))
                .cloned()
                .unwrap_or(Value::Null);
            let props = &rule["properties"];
            let level = s(&r["level"]).or_else(|| s(&rule["defaultConfiguration"]["level"])).unwrap_or_else(|| "warning".into());
            let sev = match props["security-severity"].as_str().and_then(|x| x.parse::<f64>().ok()) {
                Some(n) if n >= 9.0 => "critical",
                Some(n) if n >= 7.0 => "high",
                Some(n) if n >= 4.0 => "medium",
                Some(n) if n > 0.0 => "low",
                Some(_) => "info",
                None => match level.as_str() {
                    "error" => "high",
                    "warning" => "medium",
                    "note" => "low",
                    _ => "info",
                },
            };
            let tags: Vec<String> = props["tags"].as_array().map(|t| t.iter().filter_map(s).collect()).unwrap_or_default();
            let mut identifiers = vec![Identifier {
                kind: format!("{}_rule", tool.to_ascii_lowercase().replace(' ', "_")),
                name: s(&rule["name"]).unwrap_or_else(|| rule_id.clone()),
                value: rule_id.clone(),
                url: s(&rule["helpUri"]),
            }];
            for t in &tags {
                if let Some(n) = t.strip_prefix("external/cwe/cwe-") {
                    let n = n.trim_start_matches('0');
                    identifiers.push(Identifier {
                        kind: "cwe".into(),
                        name: format!("CWE-{n}"),
                        value: n.into(),
                        url: Some(format!("https://cwe.mitre.org/data/definitions/{n}.html")),
                    });
                }
            }
            let upper = rule_id.to_ascii_uppercase();
            if upper.starts_with("CVE-") || upper.starts_with("GHSA-") {
                identifiers.push(Identifier {
                    kind: upper[..upper.find('-').unwrap()].to_ascii_lowercase(),
                    name: rule_id.clone(),
                    value: rule_id.clone(),
                    url: None,
                });
            }
            let tool_l = tool.to_ascii_lowercase();
            let report_type = if tool_l.contains("gitleaks") || tool_l.contains("secret") || tags.iter().any(|t| t == "secret") {
                "secret_detection"
            } else if upper.starts_with("CVE-") || upper.starts_with("GHSA-") || tags.iter().any(|t| t == "vulnerability") {
                if declared == "container_scanning" { "container_scanning" } else { "dependency_scanning" }
            } else {
                "sast"
            };
            let loc = &r["locations"][0]["physicalLocation"];
            let file = s(&loc["artifactLocation"]["uri"]).map(|u| repo_path(&u));
            let start = loc["region"]["startLine"].as_i64().map(|n| n as i32);
            let message = s(&r["message"]["text"]).unwrap_or_default();
            let title = s(&rule["shortDescription"]["text"]).or_else(|| s(&rule["name"])).unwrap_or_else(|| message.clone());
            let description = [message.as_str(), rule["fullDescription"]["text"].as_str().unwrap_or("")]
                .iter()
                .filter(|x| !x.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n\n");
            let stable = r["partialFingerprints"]
                .as_object()
                .and_then(|m| m.get("primaryLocationLineHash").or_else(|| m.values().next()))
                .and_then(s)
                .or_else(|| r["fingerprints"].as_object().and_then(|m| m.values().next()).and_then(s));
            let package_name = message_field(&message, "Package");
            let image =
                if report_type == "container_scanning" { s(&run["properties"]["imageName"]).or_else(|| file.clone()) } else { None };
            out.push(Finding {
                package_version: message_field(&message, "Installed Version"),
                fixed_version: message_field(&message, "Fixed Version"),
                package_name,
                image,
                report_type: report_type.into(),
                scanner: tool_l.replace(' ', "_"),
                severity: sev.into(),
                title,
                description,
                solution: s(&rule["help"]["text"]).unwrap_or_default(),
                rule_id: rule_id.clone(),
                identifiers,
                file,
                start_line: start,
                end_line: loc["region"]["endLine"].as_i64().map(|n| n as i32).or(start),
                stable,
            });
        }
    }
    Ok((tools.join(", "), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/security/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn gitlab_sast() {
        let (scanner, f) = parse(&fixture("semgrep-sast.json"), "sast").unwrap();
        assert_eq!(scanner, "semgrep");
        assert_eq!(f.len(), 2);
        let sqli = &f[0];
        assert_eq!((sqli.severity.as_str(), sqli.rule_id.as_str(), sqli.scanner.as_str()), ("high", "bandit.B608", "semgrep"));
        assert_eq!((sqli.file.as_deref(), sqli.start_line, sqli.end_line), (Some("app/db.py"), Some(12), Some(12)));
        assert_eq!(sqli.identifiers[1].name, "CWE-89");
        assert_eq!(f[1].file.as_deref(), Some("app/hash.py"), "no ./");
        assert_eq!(f[1].end_line, Some(3), "a single line");
        // the same finding, re-indented or moved, has the same fingerprint
        assert_eq!(sqli.fingerprint(Some("  q = f\"SELECT * {x}\"")), sqli.fingerprint(Some("q = f\"SELECT * {x}\"")));
        assert_ne!(sqli.fingerprint(Some("a")), f[1].fingerprint(Some("a")));
    }

    #[test]
    fn gitlab_secrets_dependencies_containers() {
        let (_, s) = parse(&fixture("secrets.json"), "secret_detection").unwrap();
        assert_eq!((s[0].report_type.as_str(), s[0].severity.as_str(), s[0].rule_id.as_str()), ("secret_detection", "critical", "AWS"));
        assert!(!format!("{:?}", s[0]).contains("AKIA"), "the secret itself is never kept");

        let (scanner, d) = parse(&fixture("gemnasium-dependency.json"), "dependency_scanning").unwrap();
        assert_eq!(scanner, "gemnasium");
        let lodash = &d[0];
        assert_eq!((lodash.package_name.as_deref(), lodash.package_version.as_deref()), (Some("lodash"), Some("4.17.15")));
        assert_eq!(lodash.fixed_version.as_deref(), Some("4.17.21"));
        assert!(lodash.identifiers.iter().any(|i| i.kind == "cve" && i.value == "CVE-2020-8203"));
        // a version bump that does not fix it is the same finding
        let mut bumped = lodash.clone();
        bumped.package_version = Some("4.17.16".into());
        assert_eq!(bumped.fingerprint(None), lodash.fingerprint(None));

        let (_, c) = parse(&fixture("trivy-container.json"), "container_scanning").unwrap();
        assert_eq!(
            (c[0].image.as_deref(), c[0].fixed_version.as_deref()),
            (Some("registry.example.com/acme/shop:1.4.2"), Some("3.1.4-r0"))
        );
        let mut retagged = c[0].clone();
        retagged.image = Some("registry.example.com/acme/shop:1.5.0".into());
        assert_eq!(retagged.fingerprint(None), c[0].fingerprint(None), "a new tag of the same image");
        assert!(c[0].location_key(None).starts_with("registry.example.com/acme/shop:libssl3"), "{}", c[0].location_key(None));
    }

    #[test]
    fn sarif() {
        let (tool, f) = parse(&fixture("codeql.sarif"), "sast").unwrap();
        assert_eq!(tool, "CodeQL");
        assert_eq!(f.len(), 2);
        let sqli = &f[0];
        assert_eq!((sqli.severity.as_str(), sqli.report_type.as_str(), sqli.scanner.as_str()), ("high", "sast", "codeql"));
        assert_eq!(sqli.title, "Database query built from user-controlled sources");
        assert_eq!((sqli.file.as_deref(), sqli.start_line, sqli.end_line), (Some("src/routes/users.js"), Some(21), Some(22)));
        assert_eq!(
            sqli.identifiers.iter().filter(|i| i.kind == "cwe").map(|i| i.name.as_str()).collect::<Vec<_>>(),
            vec!["CWE-89", "CWE-90"]
        );
        assert!(sqli.stable.is_some());
        assert_eq!(sqli.solution, "Use parameterized queries.");
        assert_eq!((f[1].severity.as_str(), f[1].file.as_deref()), ("low", Some("src/app.js")), "level, and file:// dropped");
        // the scanner's own fingerprint wins over the lines
        assert_eq!(sqli.fingerprint(Some("x")), sqli.fingerprint(Some("y")));
    }

    #[test]
    fn trivy_sarif_as_dependencies_and_containers() {
        let (tool, d) = parse(&fixture("trivy-fs.sarif"), "dependency_scanning").unwrap();
        assert_eq!(tool, "Trivy");
        let lodash = &d[0];
        assert_eq!((lodash.report_type.as_str(), lodash.severity.as_str()), ("dependency_scanning", "high"));
        assert_eq!(
            (lodash.package_name.as_deref(), lodash.package_version.as_deref(), lodash.fixed_version.as_deref()),
            (Some("lodash"), Some("4.17.15"), Some("4.17.19"))
        );
        assert_eq!(lodash.file.as_deref(), Some("package-lock.json"));
        assert!(lodash.identifiers.iter().any(|i| i.kind == "cve" && i.value == "CVE-2020-8203"));
        assert!(lodash.image.is_none());
        // the same report uploaded as container scanning: the image is the run's
        let (_, c) = parse(&fixture("trivy-fs.sarif"), "container_scanning").unwrap();
        assert_eq!(
            (c[0].report_type.as_str(), c[0].image.as_deref()),
            ("container_scanning", Some("registry.example.com/acme/shop:1.4.2"))
        );
    }

    #[test]
    fn several_files_in_one_report() {
        let mut both = fixture("semgrep-sast.json");
        both.extend_from_slice(b"\n");
        both.extend(fixture("codeql.sarif"));
        let (scanners, f) = parse(&both, "sast").unwrap();
        assert_eq!((scanners.as_str(), f.len()), ("semgrep, CodeQL", 4));
    }

    #[test]
    fn bad_reports() {
        assert!(parse(b"not json", "sast").is_err());
        assert!(parse(b"{\"version\": \"15.0.0\"}", "sast").is_err());
        let many = serde_json::json!({ "vulnerabilities": vec![serde_json::json!({}); MAX_FINDINGS + 1] });
        assert!(parse(&serde_json::to_vec(&many).unwrap(), "sast").is_err());
    }
}
