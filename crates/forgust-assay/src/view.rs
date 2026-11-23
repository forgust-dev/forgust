//! The map as a page to look at: one HTML file holding its data, with nothing to fetch, so
//! it opens offline and can be passed around.

use crate::model::RepoMap;

const TEMPLATE: &str = include_str!("view.html");

/// A page showing `map`, titled `title` (the repository's name).
pub fn page(map: &RepoMap, title: &str) -> String {
    // inside a <script>, a `</script>` or `<!--` in some value would end or open something:
    // `<` only occurs in strings, where `<` stands for it
    let json = serde_json::to_string(map).expect("a map serializes").replace('<', "\\u003c");
    TEMPLATE.replace("__ASSAY_TITLE__", &escape(title)).replace("__ASSAY_MAP__", &json)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SCHEMA, Unassigned};

    #[test]
    fn holds_its_data_safely() {
        let map = RepoMap {
            schema: SCHEMA,
            commit: None,
            blocks: Vec::new(),
            links: Vec::new(),
            unassigned: Unassigned { files: 1, sample: vec!["</script><b>x".to_string()] },
            warnings: Vec::new(),
            violations: Vec::new(),
        };
        let html = page(&map, "a <b> & c");
        assert!(html.contains("<title>a &lt;b&gt; &amp; c · assay</title>"));
        assert_eq!(html.matches("</script>").count(), 2, "a value cannot end the data's script");
        // `<` as JSON's unicode escape: a backslash, then u003c
        let bs = char::from(92u8);
        assert!(html.contains(&format!(r#""sample":["{bs}u003c/script>{bs}u003cb>x"]"#)));
        assert!(!html.contains("__ASSAY_"));
    }
}
