//! Server-side syntax highlighting and Markdown (architecture: Frontend). Results
//! are cached by blob ID, so the browser never ships grammars.

use std::sync::Arc;

use ammonia::UrlRelative;
use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, ClassedHTMLGenerator, css_for_theme_with_class_style};
use syntect::parsing::{SyntaxReference, SyntaxSet};

const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };
/// Files longer than this are shown without highlighting.
const MAX_HIGHLIGHT_LINES: usize = 20_000;

#[derive(Clone)]
pub struct Highlighter {
    syntaxes: Arc<SyntaxSet>,
    css: Arc<String>,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// Prefix every selector of a stylesheet (comma lists included) with `prefix`.
/// Enough for syntect's output: flat rules, comments, no at-rules.
fn scope_css(css: &str, prefix: &str) -> String {
    // drop comments: they can sit before a selector
    let mut plain = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        plain.push_str(&rest[..start]);
        rest = rest[start..].find("*/").map_or("", |end| &rest[start + end + 2..]);
    }
    plain.push_str(rest);
    let mut out = String::with_capacity(plain.len() * 2);
    for rule in plain.split_inclusive('}') {
        let Some((selectors, body)) = rule.split_once('{') else {
            out.push_str(rule);
            continue;
        };
        let scoped: Vec<String> = selectors.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("{prefix} {s}")).collect();
        if scoped.is_empty() {
            continue;
        }
        out.push_str(&scoped.join(",\n"));
        out.push_str(" {");
        out.push_str(body);
        out.push('\n');
    }
    out
}

impl Highlighter {
    pub fn new() -> Self {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        let light = css_for_theme_with_class_style(&themes.themes["InspiredGitHub"], CLASS_STYLE).unwrap_or_default();
        let dark = css_for_theme_with_class_style(&themes.themes["base16-ocean.dark"], CLASS_STYLE).unwrap_or_default();
        // Each theme applies only in its color scheme: the one the user
        // picked, or the system's. Scoping every selector (not only the first
        // of a list) keeps a more specific rule of one theme from winning in
        // the other, e.g. near-black JSON keys on a dark background.
        let css = format!(
            "/* light (chosen) */\n{}\n@media (prefers-color-scheme: light) {{\n{}\n}}\n/* dark (chosen) */\n{}\n@media (prefers-color-scheme: dark) {{\n{}\n}}\n",
            scope_css(&light, ":root[data-theme=light]"),
            scope_css(&light, ":root:not([data-theme=dark])"),
            scope_css(&dark, ":root[data-theme=dark]"),
            scope_css(&dark, ":root:not([data-theme=light])"),
        );
        Highlighter { syntaxes: Arc::new(syntaxes), css: Arc::new(css) }
    }

    pub fn css(&self) -> &str {
        &self.css
    }

    fn syntax_for(&self, path: &str, first_line: &str) -> &SyntaxReference {
        let name = path.rsplit('/').next().unwrap_or(path);
        let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or(name);
        let special = match name {
            "Dockerfile" | "Containerfile" => self.syntaxes.find_syntax_by_name("Dockerfile"),
            "Makefile" | "GNUmakefile" => self.syntaxes.find_syntax_by_name("Makefile"),
            "Cargo.lock" => self.syntaxes.find_syntax_by_extension("toml"),
            _ => None,
        };
        special
            .or_else(|| self.syntaxes.find_syntax_by_extension(ext))
            .or_else(|| match ext {
                "toml" | "lock" => self.syntaxes.find_syntax_by_extension("ini"),
                "tsx" | "ts" | "mjs" | "cjs" => self.syntaxes.find_syntax_by_extension("js"),
                "yml" => self.syntaxes.find_syntax_by_extension("yaml"),
                _ => None,
            })
            .or_else(|| self.syntaxes.find_syntax_by_first_line(first_line))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    pub fn language(&self, path: &str, first_line: &str) -> String {
        self.syntax_for(path, first_line).name.clone()
    }

    /// One HTML string per line; spans never cross line boundaries.
    pub fn highlight_lines(&self, path: &str, text: &str) -> Vec<String> {
        let first = text.lines().next().unwrap_or("");
        let syntax = self.syntax_for(path, first);
        if text.lines().count() > MAX_HIGHLIGHT_LINES || syntax.name == "Plain Text" {
            return text.lines().map(escape_html).collect();
        }
        let mut generator = ClassedHTMLGenerator::new_with_class_style(syntax, &self.syntaxes, CLASS_STYLE);
        for line in syntect::util::LinesWithEndings::from(text) {
            if generator.parse_html_for_line_which_includes_newline(line).is_err() {
                return text.lines().map(escape_html).collect();
            }
        }
        split_html_lines(&generator.finalize())
    }
}

/// Split highlighted HTML into self-contained lines, closing spans at the end of
/// each line and reopening them on the next.
fn split_html_lines(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open: Vec<String> = Vec::new();
    for raw in html.split('\n') {
        let mut line = open.concat();
        let mut i = 0;
        while i < raw.len() {
            let rest = &raw[i..];
            if rest.starts_with("<span") {
                let end = rest.find('>').map(|e| e + 1).unwrap_or(rest.len());
                open.push(rest[..end].to_string());
                line.push_str(&rest[..end]);
                i += end;
            } else if rest.starts_with("</span>") {
                open.pop();
                line.push_str("</span>");
                i += 7;
            } else {
                // text up to the next tag (always take the first char: a stray `<`)
                let first = rest.chars().next().map_or(1, char::len_utf8);
                let next = rest[first..].find('<').map(|n| n + first).unwrap_or(rest.len());
                line.push_str(&rest[..next]);
                i += next;
            }
        }
        for _ in 0..open.len() {
            line.push_str("</span>");
        }
        out.push(line);
    }
    // the generator ends with a newline: drop the empty trailing line
    if out.last().is_some_and(|l| strip_tags(l).is_empty()) {
        out.pop();
    }
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "avif"];

pub fn is_image(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, e)| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn is_markdown(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".md") || p.ends_with(".markdown") || p.ends_with(".mdown")
}

/// Images from other sites go through this server (`/-/image/<sig>/<url>`), so
/// viewers' addresses and cookies-free requests never reach the image's host,
/// and the page can forbid third-party images (CSP). The signature keeps the
/// proxy from serving URLs nobody wrote.
#[derive(Clone)]
pub struct ImageProxy {
    secrets: crate::crypto::SecretBox,
    /// `external_url`: images on this instance are not proxied.
    own: String,
}

impl ImageProxy {
    pub fn new(secrets: crate::crypto::SecretBox, external_url: &str) -> Self {
        ImageProxy { secrets, own: external_url.trim_end_matches('/').to_string() }
    }

    fn msg(url: &str) -> String {
        format!("image-proxy:{url}")
    }

    pub fn path(&self, url: &str) -> String {
        use base64::Engine;
        format!("/-/image/{}/{}", self.secrets.sign(&Self::msg(url)), base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url))
    }

    /// The URL a proxy path carries, when its signature is ours.
    pub fn verify(&self, sig: &str, encoded: &str) -> Option<String> {
        use base64::Engine;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).ok()?;
        let url = String::from_utf8(raw).ok()?;
        self.secrets.verify(&Self::msg(&url), sig).then_some(url)
    }

    fn wants(&self, src: &str) -> bool {
        let lower = src.to_ascii_lowercase();
        (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//"))
            && !(src.starts_with(&self.own) && src[self.own.len()..].starts_with('/'))
    }
}

/// The classes comrak gives alerts (`markdown-alert markdown-alert-note`, its title).
const ALERT_CLASSES: &[&str] = &[
    "markdown-alert markdown-alert-note",
    "markdown-alert markdown-alert-tip",
    "markdown-alert markdown-alert-important",
    "markdown-alert markdown-alert-warning",
    "markdown-alert markdown-alert-caution",
    "markdown-alert-title",
];

/// GitHub-flavored Markdown → sanitized HTML. Relative links resolve against the
/// file's directory: images to raw files, other links to the blob view.
pub fn markdown(source: &str, project_url: &str, git_ref: &str, dir: &str) -> String {
    markdown_with(source, project_url, git_ref, dir, None)
}

/// [`markdown`], with images from other sites sent through `images`.
pub fn markdown_with(source: &str, project_url: &str, git_ref: &str, dir: &str, images: Option<&ImageProxy>) -> String {
    let mut opts = comrak::Options::default();
    opts.extension.table = true;
    opts.extension.strikethrough = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    opts.extension.footnotes = true;
    opts.extension.header_ids = Some("user-content-".into());
    // `> [!NOTE]` and friends (GitHub's and GitLab's alerts)
    opts.extension.alerts = true;
    // `$x^2$`, `$$...$$`, `` $`x`$ `` and ```math: marked here, typeset in the browser
    opts.extension.math_dollars = true;
    opts.extension.math_code = true;
    opts.render.unsafe_ = false;
    let html = comrak::markdown_to_html(source, &opts);

    let rewrite = LinkRewriter {
        base_dir: if dir.is_empty() { String::new() } else { format!("{dir}/") },
        project_url: project_url.trim_end_matches('/').to_string(),
        git_ref: git_ref.to_string(),
    };
    let proxy = images.cloned();
    ammonia::Builder::default()
        .url_relative(UrlRelative::Custom(Box::new(rewrite)))
        .add_generic_attributes(&["id"])
        .add_tag_attributes("input", &["type", "checked", "disabled"])
        .add_tags(&["input"])
        // a fenced block's language (`language-rust`, `language-suggestion:-1+2`):
        // nothing else, and only plain characters
        .add_tag_attributes("code", &["class", "data-math-style"])
        .add_tag_attributes("span", &["data-math-style"])
        .add_tag_attributes("div", &["class"])
        .add_tag_attributes("p", &["class"])
        .attribute_filter(move |el, attr, value| {
            if el == "img"
                && attr == "src"
                && let Some(px) = &proxy
                && px.wants(value)
            {
                let abs = if value.starts_with("//") { format!("https:{value}") } else { value.to_string() };
                return Some(px.path(&abs).into());
            }
            // alerts: their own classes only
            if (el == "div" || el == "p") && attr == "class" {
                return ALERT_CLASSES.contains(&value).then(|| value.into());
            }
            if attr == "data-math-style" {
                return matches!(value, "inline" | "display").then(|| value.into());
            }
            if el == "code" && attr == "class" {
                let ok = value.strip_prefix("language-").is_some_and(|l| {
                    !l.is_empty() && l.len() <= 64 && l.chars().all(|c| c.is_ascii_alphanumeric() || "_+-:.#".contains(c))
                });
                return ok.then(|| value.into());
            }
            Some(value.into())
        })
        .clean(&html)
        .to_string()
}

// ------------------------------------------------------------------ references (M6)

/// `@user`, `#12` and `!45` written in a text (code included: a lookup of a few
/// extra names is harmless; linking skips code).
#[derive(Debug, Default, PartialEq)]
pub struct Refs {
    pub users: Vec<String>,
    pub issues: Vec<i64>,
    pub mrs: Vec<i64>,
}

fn ref_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // a reference starts the text or follows something that is not part of a
    // word, a path, an entity (`&#39;`) or another reference
    RE.get_or_init(|| {
        regex::Regex::new(r"(?:^|[^\w&/#!@.\-])(?:@([A-Za-z0-9_][A-Za-z0-9_.\-]*[A-Za-z0-9_]|[A-Za-z0-9_])|([#!])(\d{1,9}))\b").unwrap()
    })
}

pub fn find_refs(text: &str) -> Refs {
    let mut r = Refs::default();
    for c in ref_re().captures_iter(text) {
        if let Some(u) = c.get(1) {
            let u = u.as_str().to_ascii_lowercase();
            if !r.users.contains(&u) {
                r.users.push(u);
            }
        } else if let (Some(kind), Some(n)) = (c.get(2), c.get(3).and_then(|n| n.as_str().parse::<i64>().ok())) {
            let list = if kind.as_str() == "#" { &mut r.issues } else { &mut r.mrs };
            if !list.contains(&n) {
                list.push(n);
            }
        }
    }
    r
}

/// What references point at, looked up for one text.
#[derive(Debug, Default)]
pub struct RefTargets {
    /// `/group/project`
    pub project_url: String,
    /// lowercase username → display name
    pub users: std::collections::HashMap<String, String>,
    /// iid → (title, state)
    pub issues: std::collections::HashMap<i64, (String, String)>,
    pub mrs: std::collections::HashMap<i64, (String, String)>,
}

/// References a Markdown text makes in prose: not in code, not in links.
pub fn refs_in_prose(source: &str) -> Refs {
    let html = markdown(source, "", "HEAD", "");
    let mut text = String::new();
    each_prose_text(&html, |t| {
        text.push_str(t);
        text.push('\n');
    });
    find_refs(&text)
}

/// Calls `f` with each run of text outside tags and outside `<a>`, `<code>`, `<pre>`.
fn each_prose_text(html: &str, mut f: impl FnMut(&str)) {
    let mut skip = 0usize;
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('<') {
            let end = stripped.find('>').map(|i| i + 2).unwrap_or(rest.len());
            let tag = &rest[..end];
            let name: String = tag
                .trim_start_matches(['<', '/'])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if matches!(name.as_str(), "a" | "code" | "pre") {
                if tag.starts_with("</") {
                    skip = skip.saturating_sub(1);
                } else if !tag.ends_with("/>") {
                    skip += 1;
                }
            }
            rest = &rest[end..];
            continue;
        }
        let end = rest.find('<').unwrap_or(rest.len());
        if skip == 0 {
            f(&rest[..end]);
        }
        rest = &rest[end..];
    }
}

/// Turn references into links in sanitized HTML: text outside `<a>`, `<code>`
/// and `<pre>` only, and only references that exist.
pub fn link_refs(html: &str, t: &RefTargets) -> String {
    if t.users.is_empty() && t.issues.is_empty() && t.mrs.is_empty() {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len() + 64);
    let mut skip = 0usize; // inside a, code or pre
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('<') {
            let end = stripped.find('>').map(|i| i + 2).unwrap_or(rest.len());
            let tag = &rest[..end];
            let name: String = tag
                .trim_start_matches(['<', '/'])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if matches!(name.as_str(), "a" | "code" | "pre") {
                if tag.starts_with("</") {
                    skip = skip.saturating_sub(1);
                } else if !tag.ends_with("/>") {
                    skip += 1;
                }
            }
            out.push_str(tag);
            rest = &rest[end..];
            continue;
        }
        let end = rest.find('<').unwrap_or(rest.len());
        let text = &rest[..end];
        if skip > 0 {
            out.push_str(text);
        } else {
            out.push_str(&link_text(text, t));
        }
        rest = &rest[end..];
    }
    out
}

fn link_text(text: &str, t: &RefTargets) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for c in ref_re().captures_iter(text) {
        let (start, end, link) = if let Some(u) = c.get(1) {
            let Some(name) = t.users.get(&u.as_str().to_ascii_lowercase()) else { continue };
            let link = format!(r#"<a href="/{0}" class="gfm-ref user-mention" title="{1}">@{0}</a>"#, u.as_str(), escape_html(name));
            (u.start() - 1, u.end(), link)
        } else {
            let (Some(kind), Some(n)) = (c.get(2), c.get(3)) else { continue };
            let iid: i64 = n.as_str().parse().unwrap_or(0);
            let (map, path, class) =
                if kind.as_str() == "#" { (&t.issues, "issues", "ref-issue") } else { (&t.mrs, "merge_requests", "ref-mr") };
            let Some((title, state)) = map.get(&iid) else { continue };
            let link = format!(
                r#"<a href="{}/-/{path}/{iid}" class="gfm-ref {class} {state}" title="{}">{}{iid}</a>"#,
                t.project_url,
                escape_html(title),
                kind.as_str()
            );
            (kind.start(), n.end(), link)
        };
        out.push_str(&text[last..start]);
        out.push_str(&link);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

struct LinkRewriter {
    base_dir: String,
    project_url: String,
    git_ref: String,
}

impl<'a> ammonia::UrlRelativeEvaluate<'a> for LinkRewriter {
    fn evaluate<'u>(&self, url: &'u str) -> Option<std::borrow::Cow<'u, str>> {
        // instance URLs (`/-/project/<id>/uploads/...`) stay as they are
        if url.starts_with('#') || url.starts_with("//") || url.starts_with("/-/") {
            return Some(url.into());
        }
        // GitLab's project uploads (imported text, glab): served under the project
        if let Some(rest) = url.strip_prefix("/uploads/") {
            return Some(format!("{}/-/uploads/{rest}", self.project_url).into());
        }
        let (path, anchor) = match url.split_once('#') {
            Some((p, a)) => (p, format!("#{a}")),
            None => (url, String::new()),
        };
        let resolved = normalize(&format!("{}{}", self.base_dir, path.trim_start_matches("./")), url.starts_with('/'));
        let kind = if is_image(path) { "raw" } else { "blob" };
        Some(format!("{}/-/{kind}/{}/{resolved}{anchor}", self.project_url, self.git_ref).into())
    }
}

fn normalize(path: &str, absolute: bool) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let path = if absolute { path.rsplit_once("//").map(|(_, p)| p).unwrap_or(path).trim_start_matches('/') } else { path };
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_per_line() {
        let h = Highlighter::new();
        let lines = h.highlight_lines("src/main.rs", "fn main() {\n    let s = \"a\nb\";\n}\n");
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("hl-"));
        // a string spanning lines is closed and reopened
        assert_eq!(lines[1].matches("<span").count(), lines[1].matches("</span>").count());
        assert_eq!(lines[2].matches("<span").count(), lines[2].matches("</span>").count());
        assert_eq!(h.language("x.rs", ""), "Rust");
    }

    #[test]
    fn upload_links() {
        let h = markdown("![a](/-/project/3/uploads/abc/a.png) [b](/uploads/def/b.zip)", "/g/p", "main", "");
        assert!(h.contains(r#"src="/-/project/3/uploads/abc/a.png""#), "{h}");
        assert!(h.contains(r#"href="/g/p/-/uploads/def/b.zip""#), "{h}");
    }

    #[test]
    fn non_ascii_after_a_tag_does_not_panic() {
        // a text run starting with a multi-byte char right after a tag
        assert_eq!(
            split_html_lines("<span class=\"c\">é\nÉlément</span>\n"),
            vec!["<span class=\"c\">é</span>".to_string(), "<span class=\"c\">Élément</span>".to_string()]
        );
        let h = Highlighter::new();
        let lines = h.highlight_lines("src/lib.rs", "/*\né\n*/\n");
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains('é'));
    }

    #[test]
    fn plain_text_is_escaped() {
        let h = Highlighter::new();
        assert_eq!(h.highlight_lines("notes.unknownext", "<b>\n"), vec!["&lt;b&gt;".to_string()]);
    }

    #[test]
    fn finds_references_but_not_emails_entities_or_paths() {
        let r = find_refs("Hi @Alice and @bob.smith, see #12, !3 and #12 again. a@b.com &#39; x#4 path/#9 @a");
        assert_eq!(r.users, vec!["alice", "bob.smith", "a"]);
        assert_eq!(r.issues, vec![12]);
        assert_eq!(r.mrs, vec![3]);
    }

    #[test]
    fn code_keeps_its_language_class_only() {
        let out = markdown("```suggestion:-0+1\nx\n```\n\n<code class=\"evil\">y</code>", "/p", "main", "");
        assert!(out.contains(r#"<code class="language-suggestion:-0+1">"#), "{out}");
        assert!(!out.contains("evil"), "{out}");
    }

    #[test]
    fn prose_references_skip_code() {
        let r = refs_in_prose("cc @rita, `@olivia` and\n\n```\n@dave #4\n```\n\nfixes #1");
        assert_eq!(r.users, vec!["rita"]);
        assert_eq!(r.issues, vec![1]);
    }

    #[test]
    fn links_known_references_outside_code_and_links() {
        let mut t = RefTargets { project_url: "/acme/api".into(), ..Default::default() };
        t.users.insert("alice".into(), "Alice <A>".into());
        t.issues.insert(12, ("Crash".into(), "closed".into()));
        t.mrs.insert(3, ("Fix".into(), "opened".into()));
        let html = markdown("@alice fixed #12 in !3; `#12` and [#12](x) stay; #99 and @nobody too. It&#39;s fine", "/acme/api", "main", "");
        let out = link_refs(&html, &t);
        assert!(out.contains(r#"<a href="/alice" class="gfm-ref user-mention" title="Alice &lt;A&gt;">@alice</a>"#), "{out}");
        assert!(out.contains(r#"<a href="/acme/api/-/issues/12" class="gfm-ref ref-issue closed" title="Crash">#12</a> in"#), "{out}");
        assert!(out.contains(r#"<a href="/acme/api/-/merge_requests/3" class="gfm-ref ref-mr opened" title="Fix">!3</a>"#), "{out}");
        assert!(out.contains("<code>#12</code>"), "{out}");
        assert_eq!(out.matches("/-/issues/12").count(), 1, "{out}");
        assert!(out.contains("#99 and @nobody"), "{out}");
    }

    #[test]
    fn markdown_is_sanitized_and_links_rewritten() {
        let html = markdown(
            "# Title\n<script>alert(1)</script>\n[doc](docs/a.md) ![img](./img/x.png) [ext](https://example.com)",
            "/acme/api",
            "main",
            "",
        );
        assert!(!html.contains("<script"));
        assert!(html.contains("/acme/api/-/blob/main/docs/a.md"));
        assert!(html.contains("/acme/api/-/raw/main/img/x.png"));
        assert!(html.contains("https://example.com"));
    }

    #[test]
    fn alerts_and_math_are_marked_for_the_browser() {
        let html = markdown(
            "> [!WARNING]\n> Mind the gap.\n\nInline $x^2$ and prices $5 and $10.\n\n```math\na+b\n```\n\n<div class=\"markdown-alert evil\" data-math-style=\"x\">no</div>",
            "/acme/api",
            "main",
            "",
        );
        assert!(html.contains(r#"<div class="markdown-alert markdown-alert-warning">"#), "{html}");
        assert!(html.contains(r#"<p class="markdown-alert-title">Warning</p>"#), "{html}");
        assert!(html.contains(r#"<span data-math-style="inline">x^2</span>"#), "{html}");
        assert!(html.contains("prices $5 and $10"), "dollars in prose stay: {html}");
        assert!(html.contains(r#"<code class="language-math" data-math-style="display">a+b"#), "{html}");
        assert!(!html.contains("evil") && !html.contains(r#"data-math-style="x""#), "{html}");
    }
}

#[cfg(test)]
mod css_tests {
    #[test]
    fn every_selector_is_scoped() {
        let css = super::Highlighter::new().css().to_string();
        assert!(css.contains(":root[data-theme=dark] .hl-"));
        assert!(css.contains(":root:not([data-theme=light]) .hl-"));
        assert!(css.contains(":root[data-theme=light] .hl-"));
        // no rule of one theme may apply in the other: every selector carries its scope
        let body: String =
            css.lines().filter(|l| !l.starts_with("/*") && !l.starts_with("@media") && l.trim() != "}").collect::<Vec<_>>().join("\n");
        for rule in body.split('}') {
            let Some((selectors, _)) = rule.split_once('{') else { continue };
            for sel in selectors.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                assert!(sel.starts_with(":root"), "unscoped selector: {sel}");
            }
        }
    }

    #[test]
    fn scope_css_handles_lists_and_comments() {
        let css = "/* c */\n.a, .b .c {\n color: red;\n}\n.d { color: blue; }";
        let out = super::scope_css(css, ":root[x]");
        assert!(out.contains(":root[x] .a,\n:root[x] .b .c {"), "{out}");
        assert!(out.contains(":root[x] .d {"), "{out}");
        assert!(!out.contains("/*"));
    }
}
