//! The code index on disk: one tantivy index for the instance, one document
//! per file of each project's default branch.
//!
//! tantivy is only a filter here. Its regex query matches single terms and its
//! n-gram tokenizer has no positions, so it cannot find text inside a line by
//! itself: we store the trigrams of each file and ask for the files that hold
//! the trigrams of the query (`Need`). The caller then reads those files from
//! git and checks them. File content is not stored in the index.
//!
//! The index can always be rebuilt from the repositories. It carries a
//! generation id (`GENERATION` file); the database records which generation
//! each project was indexed into, so a wiped, replaced or restored index is
//! filled again by the sweep.

use std::path::{Path, PathBuf};

use tantivy::collector::DocSetCollector;
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, BooleanQuery, Query, TermQuery, TermSetQuery};
use tantivy::schema::{Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value};
use tantivy::tokenizer::{PreTokenizedString, Token};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Term};

use super::query::{Need, fold, grams};

/// Bumped when the fields or the way trigrams are cut change: older indexes
/// are thrown away and rebuilt.
const FORMAT: &str = "1";
const GENERATION_FILE: &str = "GENERATION";
/// Memory the writer may use before it flushes a segment.
const WRITER_BYTES: usize = 64 << 20;
/// Files one query may ask to be checked.
pub const MAX_CANDIDATES: usize = 20_000;

#[derive(Clone, Copy)]
struct Fields {
    /// `"{project_id}:{path}"`, to replace or delete one file.
    key: Field,
    project_id: Field,
    path: Field,
    oid: Field,
    /// Lower-cased last path segment.
    filename: Field,
    /// Lower-cased language name (`languages::language_of`).
    lang: Field,
    path_grams: Field,
    content_grams: Field,
}

fn schema() -> (Schema, Fields) {
    let grams = || {
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default().set_tokenizer("raw").set_index_option(IndexRecordOption::Basic).set_fieldnorms(false),
        )
    };
    let mut b = Schema::builder();
    let fields = Fields {
        key: b.add_text_field("key", STRING),
        project_id: b.add_u64_field("project_id", INDEXED | STORED),
        path: b.add_text_field("path", STORED),
        oid: b.add_text_field("oid", STORED),
        filename: b.add_text_field("filename", STRING),
        lang: b.add_text_field("lang", STRING),
        path_grams: b.add_text_field("path_grams", grams()),
        content_grams: b.add_text_field("content_grams", grams()),
    };
    (b.build(), fields)
}

fn pre_tokenized<'a>(grams: impl IntoIterator<Item = &'a str>) -> PreTokenizedString {
    let tokens = grams.into_iter().enumerate().map(|(position, g)| Token { position, text: g.to_string(), ..Token::default() }).collect();
    PreTokenizedString { text: String::new(), tokens }
}

/// A file the index holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Candidate {
    pub project_id: i64,
    pub path: String,
    pub oid: String,
}

/// Which files to ask the index for.
#[derive(Debug, Clone)]
pub struct Filter {
    pub need: Need,
    /// Only these projects (`None`: every project).
    pub projects: Option<Vec<i64>>,
    /// Text the path contains, any case.
    pub path: Option<String>,
    /// Exact file name, any case.
    pub filename: Option<String>,
    /// Language name, any case.
    pub lang: Option<String>,
}

pub struct CodeIndex {
    dir: PathBuf,
    index: Index,
    reader: IndexReader,
    fields: Fields,
    generation: String,
}

impl CodeIndex {
    /// Open the index in `dir`, creating it when missing. An index that cannot
    /// be opened, or of another format, is thrown away: it is rebuilt from the
    /// repositories.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        match Self::try_open(dir) {
            Ok(Some(index)) => return Ok(index),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, dir = %dir.display(), "the code search index cannot be opened; rebuilding it"),
        }
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        std::fs::create_dir_all(dir)?;
        let (schema, _) = schema();
        Index::create_in_dir(dir, schema)?;
        std::fs::write(dir.join(GENERATION_FILE), format!("{FORMAT}:{}", crate::crypto::random_urlsafe(12)))?;
        Self::try_open(dir)?.ok_or_else(|| anyhow::anyhow!("the code search index was not created"))
    }

    /// `None`: nothing usable there (missing, or another format).
    fn try_open(dir: &Path) -> anyhow::Result<Option<Self>> {
        let Ok(generation) = std::fs::read_to_string(dir.join(GENERATION_FILE)) else { return Ok(None) };
        if generation.split_once(':').map(|(f, _)| f) != Some(FORMAT) {
            return Ok(None);
        }
        let index = Index::open(MmapDirectory::open(dir)?)?;
        let (schema, fields) = schema();
        if index.schema() != schema {
            return Ok(None);
        }
        // reloaded by hand before each search: a commit made by this process
        // or another one is seen by the next query, not some time later
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        Ok(Some(CodeIndex { dir: dir.to_path_buf(), index, reader, fields, generation }))
    }

    /// Identifies this index: it changes when the index is rebuilt from nothing.
    pub fn generation(&self) -> &str {
        &self.generation
    }

    /// The single writer. Only the index job opens one, and those run one at a
    /// time (queue `code-index`): a second writer fails on tantivy's lock.
    pub fn writer(&self) -> anyhow::Result<Writer> {
        Ok(Writer { inner: self.index.writer_with_num_threads(1, WRITER_BYTES)?, fields: self.fields })
    }

    /// Bytes the index takes on disk.
    pub fn disk_bytes(&self) -> u64 {
        crate::services::quotas::dir_bytes(&self.dir)
    }

    /// Files indexed.
    pub fn files(&self) -> anyhow::Result<u64> {
        self.reader.reload()?;
        Ok(self.reader.searcher().num_docs())
    }

    fn text_term(&self, field: Field, text: &str) -> Box<dyn Query> {
        Box::new(TermQuery::new(Term::from_field_text(field, text), IndexRecordOption::Basic))
    }

    fn all_grams<'a>(&self, field: Field, grams: impl IntoIterator<Item = &'a str>) -> Box<dyn Query> {
        Box::new(BooleanQuery::intersection(grams.into_iter().map(|g| self.text_term(field, g)).collect()))
    }

    fn need_query(&self, need: &Need) -> Box<dyn Query> {
        match need {
            Need::Any => Box::new(AllQuery),
            Need::Grams(grams) => self.all_grams(self.fields.content_grams, grams.iter().map(String::as_str)),
            Need::All(needs) => Box::new(BooleanQuery::intersection(needs.iter().map(|n| self.need_query(n)).collect())),
            Need::OneOf(needs) => Box::new(BooleanQuery::union(needs.iter().map(|n| self.need_query(n)).collect())),
        }
    }

    /// The files that may match, sorted by project and path, and whether there
    /// were more than `MAX_CANDIDATES` (the rest is left out). The filters on
    /// the path are only narrowed here: the caller checks the real path.
    pub fn candidates(&self, filter: &Filter) -> anyhow::Result<(Vec<Candidate>, bool)> {
        let f = self.fields;
        let mut must: Vec<Box<dyn Query>> = vec![self.need_query(&filter.need)];
        if let Some(ids) = &filter.projects {
            must.push(Box::new(TermSetQuery::new(ids.iter().map(|id| Term::from_field_u64(f.project_id, *id as u64)))));
        }
        if let Some(path) = &filter.path {
            let folded = fold(path);
            let grams = grams(&folded);
            if !grams.is_empty() {
                must.push(self.all_grams(f.path_grams, grams));
            }
        }
        if let Some(name) = &filter.filename {
            must.push(self.text_term(f.filename, &fold(name)));
        }
        if let Some(lang) = &filter.lang {
            must.push(self.text_term(f.lang, &fold(lang)));
        }
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let mut found: Vec<_> = searcher.search(&BooleanQuery::intersection(must), &DocSetCollector)?.into_iter().collect();
        // in the order the documents are stored: reading them goes through
        // each block of the store once
        found.sort_unstable();
        let capped = found.len() > MAX_CANDIDATES;
        found.truncate(MAX_CANDIDATES);
        let text =
            |doc: &TantivyDocument, field: Field| doc.get_first(field).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        let mut out = Vec::with_capacity(found.len());
        for address in found {
            let doc: TantivyDocument = searcher.doc(address)?;
            let Some(project_id) = doc.get_first(f.project_id).and_then(|v| v.as_u64()) else { continue };
            out.push(Candidate { project_id: project_id as i64, path: text(&doc, f.path), oid: text(&doc, f.oid) });
        }
        out.sort_unstable();
        Ok((out, capped))
    }
}

pub struct Writer {
    inner: IndexWriter,
    fields: Fields,
}

impl Writer {
    fn key(&self, project_id: i64, path: &str) -> Term {
        Term::from_field_text(self.fields.key, &format!("{project_id}:{path}"))
    }

    pub fn delete_project(&self, project_id: i64) {
        self.inner.delete_term(Term::from_field_u64(self.fields.project_id, project_id as u64));
    }

    pub fn delete_file(&self, project_id: i64, path: &str) {
        self.inner.delete_term(self.key(project_id, path));
    }

    /// Add a file, replacing what the index had at this path.
    pub fn put_file(&self, project_id: i64, path: &str, oid: &str, lang: Option<&str>, text: &str) -> anyhow::Result<()> {
        let f = self.fields;
        self.delete_file(project_id, path);
        let mut doc = TantivyDocument::default();
        doc.add_text(f.key, format!("{project_id}:{path}"));
        doc.add_u64(f.project_id, project_id as u64);
        doc.add_text(f.path, path);
        doc.add_text(f.oid, oid);
        doc.add_text(f.filename, fold(path.rsplit('/').next().unwrap_or(path)));
        if let Some(lang) = lang {
            doc.add_text(f.lang, fold(lang));
        }
        doc.add_pre_tokenized_text(f.path_grams, pre_tokenized(grams(&fold(path))));
        doc.add_pre_tokenized_text(f.content_grams, pre_tokenized(grams(&fold(text))));
        self.inner.add_document(doc)?;
        Ok(())
    }

    /// Make what was written visible to searches, and wait for the merges it started.
    pub fn commit(mut self) -> anyhow::Result<()> {
        self.inner.commit()?;
        self.inner.wait_merging_threads()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::query::plan;
    use super::*;

    fn filter(text: &str) -> Filter {
        Filter { need: plan(text, false, false).unwrap().0, projects: None, path: None, filename: None, lang: None }
    }

    fn paths(index: &CodeIndex, f: &Filter) -> Vec<(i64, String)> {
        index.candidates(f).unwrap().0.into_iter().map(|c| (c.project_id, c.path)).collect()
    }

    fn seeded(dir: &Path) -> CodeIndex {
        let index = CodeIndex::open(dir).unwrap();
        let w = index.writer().unwrap();
        w.put_file(1, "src/main.rs", "a1", Some("Rust"), "fn main() {\n    println!(\"Hello\");\n}\n").unwrap();
        w.put_file(1, "README.md", "a2", Some("Markdown"), "# Hello world\n").unwrap();
        w.put_file(2, "lib/Main.py", "a3", Some("Python"), "def main():\n    print('hello')\n").unwrap();
        w.commit().unwrap();
        index
    }

    #[test]
    fn finds_files_by_trigrams_any_case() {
        let dir = tempfile::tempdir().unwrap();
        let index = seeded(dir.path());
        assert_eq!(index.files().unwrap(), 3);
        assert_eq!(paths(&index, &filter("HELLO")), vec![(1, "README.md".into()), (1, "src/main.rs".into()), (2, "lib/Main.py".into())]);
        assert_eq!(paths(&index, &filter("println!")), vec![(1, "src/main.rs".into())]);
        // trigrams do not cross lines: "{\n    p" is not text of the file
        assert!(paths(&index, &filter("() {    println")).is_empty());
        assert!(paths(&index, &filter("nothing here")).is_empty());
        assert!(index.disk_bytes() > 0);
    }

    #[test]
    fn filters_by_project_path_name_and_language() {
        let dir = tempfile::tempdir().unwrap();
        let index = seeded(dir.path());
        let with = |change: fn(&mut Filter)| {
            let mut f = filter("main");
            change(&mut f);
            paths(&index, &f)
        };
        assert_eq!(with(|_| {}), vec![(1, "src/main.rs".into()), (2, "lib/Main.py".into())]);
        assert_eq!(with(|f| f.projects = Some(vec![2, 9])), vec![(2, "lib/Main.py".into())]);
        assert!(with(|f| f.projects = Some(vec![])).is_empty());
        assert_eq!(with(|f| f.path = Some("SRC/".into())), vec![(1, "src/main.rs".into())]);
        assert_eq!(with(|f| f.filename = Some("main.py".into())), vec![(2, "lib/Main.py".into())]);
        assert_eq!(with(|f| f.lang = Some("rust".into())), vec![(1, "src/main.rs".into())]);
    }

    #[test]
    fn files_are_replaced_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let index = seeded(dir.path());
        let w = index.writer().unwrap();
        w.put_file(1, "src/main.rs", "b1", Some("Rust"), "fn start() {}\n").unwrap();
        w.delete_file(1, "README.md");
        w.commit().unwrap();
        assert_eq!(paths(&index, &filter("hello")), vec![(2, "lib/Main.py".into())]);
        assert_eq!(index.candidates(&filter("start")).unwrap().0[0].oid, "b1");
        let w = index.writer().unwrap();
        w.delete_project(2);
        w.commit().unwrap();
        assert!(paths(&index, &filter("hello")).is_empty());
        assert_eq!(index.files().unwrap(), 1);
    }

    #[test]
    fn reopening_keeps_the_index_and_a_broken_one_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let generation = seeded(dir.path()).generation().to_string();
        let again = CodeIndex::open(dir.path()).unwrap();
        assert_eq!(again.generation(), generation);
        assert_eq!(again.files().unwrap(), 3);
        drop(again);
        std::fs::write(dir.path().join("meta.json"), "not json").unwrap();
        let rebuilt = CodeIndex::open(dir.path()).unwrap();
        assert_ne!(rebuilt.generation(), generation);
        assert_eq!(rebuilt.files().unwrap(), 0);
        drop(rebuilt);
        // another format: thrown away too
        std::fs::write(dir.path().join(GENERATION_FILE), "0:old").unwrap();
        assert_eq!(CodeIndex::open(dir.path()).unwrap().files().unwrap(), 0);
    }
}
