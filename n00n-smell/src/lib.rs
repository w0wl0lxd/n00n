#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, QueryParser};
use tantivy::schema::{Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value};
use tantivy::{Index, IndexWriter, ReloadPolicy, TantivyDocument, doc};

use n00n_git::conflicts::{self, ConflictsOptions, GitConflicts};

mod error;
pub use error::SmellError;

const INDEX_VERSION: u32 = 1;
const WRITER_HEAP_BYTES: usize = 50_000_000;
const DEFAULT_TOP_K: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmellFinding {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub kind: String,
    pub message: String,
    pub content: String,
    pub language: String,
}

#[derive(Debug, Clone, Default)]
pub struct SearchConfig {
    pub top_k: usize,
}

#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub kind: Option<String>,
    pub top_k: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub kind: String,
    pub message: String,
    pub score: f32,
    pub content: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexMetadata {
    pub version: u32,
    pub document_count: usize,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub phase: String,
    pub processed: usize,
    pub total: usize,
    pub message: String,
}

pub struct SmellIndex {
    path: PathBuf,
    index: Index,
    path_field: Field,
    start_line_field: Field,
    end_line_field: Field,
    kind_field: Field,
    message_field: Field,
    content_field: Field,
    language_field: Field,
}

impl SmellIndex {
    pub fn index_dir(project: &Path) -> PathBuf {
        project.join(".n00n/smells")
    }

    pub fn has_index(project: &Path) -> bool {
        Self::index_dir(project).join("metadata.json").is_file()
    }

    pub fn open_or_create(path: &Path, _config: &SearchConfig) -> Result<Self, SmellError> {
        fs::create_dir_all(path)?;
        let schema = build_schema();
        let index_path = path.join("tantivy_index");
        if !index_path.is_dir() {
            fs::create_dir_all(&index_path)?;
        }
        let directory = MmapDirectory::open(&index_path).map_err(|source| SmellError::Search {
            message: source.to_string(),
        })?;
        let index = Index::open_or_create(directory, schema.clone())?;

        Self::from_parts(path.to_path_buf(), index, &schema)
    }

    fn from_parts(path: PathBuf, index: Index, schema: &Schema) -> Result<Self, SmellError> {
        let path_field = schema.get_field("path").map_err(SmellError::Tantivy)?;
        let start_line_field = schema
            .get_field("start_line")
            .map_err(SmellError::Tantivy)?;
        let end_line_field = schema.get_field("end_line").map_err(SmellError::Tantivy)?;
        let kind_field = schema.get_field("kind").map_err(SmellError::Tantivy)?;
        let message_field = schema.get_field("message").map_err(SmellError::Tantivy)?;
        let content_field = schema.get_field("content").map_err(SmellError::Tantivy)?;
        let language_field = schema.get_field("language").map_err(SmellError::Tantivy)?;

        Ok(Self {
            path,
            index,
            path_field,
            start_line_field,
            end_line_field,
            kind_field,
            message_field,
            content_field,
            language_field,
        })
    }

    pub fn metadata(&self) -> Result<IndexMetadata, SmellError> {
        let metadata_path = self.path.join("metadata.json");
        if !metadata_path.is_file() {
            return Ok(IndexMetadata {
                version: INDEX_VERSION,
                document_count: 0,
            });
        }
        let raw = fs::read_to_string(metadata_path)?;
        let metadata: IndexMetadata =
            serde_json::from_str(&raw).map_err(|err| SmellError::Config {
                message: err.to_string(),
            })?;
        Ok(metadata)
    }

    pub fn write_metadata(&self, document_count: usize) -> Result<(), SmellError> {
        let metadata = IndexMetadata {
            version: INDEX_VERSION,
            document_count,
        };
        let raw = serde_json::to_string_pretty(&metadata).map_err(|err| SmellError::Config {
            message: err.to_string(),
        })?;
        fs::write(self.path.join("metadata.json"), raw)?;
        Ok(())
    }

    pub fn update(
        &mut self,
        repo: &Path,
        mut progress: impl FnMut(Progress),
    ) -> Result<(), SmellError> {
        let _lock = acquire_lock(&self.path)?;

        progress(Progress {
            phase: String::from("scanning"),
            processed: 0,
            total: 0,
            message: String::from("scanning repository for smells"),
        });

        let smells = collect_smells(repo)?;

        progress(Progress {
            phase: String::from("indexing"),
            processed: 0,
            total: smells.len(),
            message: format!("indexing {} smells", smells.len()),
        });

        let index_path = self.path.join("tantivy_index");
        let schema = build_schema();
        if index_path.exists() {
            fs::remove_dir_all(&index_path)?;
        }
        fs::create_dir_all(&index_path)?;
        let index = Index::create_in_dir(&index_path, schema.clone())?;
        *self = Self::from_parts(self.path.clone(), index, &schema)?;

        let mut writer = self.writer()?;
        for (processed, smell) in smells.iter().enumerate() {
            writer.add_document(document_for_smell(
                self.path_field,
                self.start_line_field,
                self.end_line_field,
                self.kind_field,
                self.message_field,
                self.content_field,
                self.language_field,
                smell,
            )?)?;
            if processed % 100 == 0 {
                progress(Progress {
                    phase: String::from("indexing"),
                    processed,
                    total: smells.len(),
                    message: format!("indexed {processed}/{} smells", smells.len()),
                });
            }
        }
        writer.commit()?;
        self.write_metadata(smells.len())?;

        progress(Progress {
            phase: String::from("complete"),
            processed: smells.len(),
            total: smells.len(),
            message: String::from("smell index complete"),
        });

        Ok(())
    }

    pub fn search(&self, query: &Query) -> Result<Vec<SearchResult>, SmellError> {
        if query.text.trim().is_empty() {
            return Err(SmellError::Config {
                message: String::from("query text is required"),
            });
        }

        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let searcher = reader.searcher();

        let fields = vec![
            self.content_field,
            self.message_field,
            self.kind_field,
            self.path_field,
            self.language_field,
        ];

        let parsed = if let Some(ref kind) = query.kind {
            let kind_query = format!("kind:{kind}");
            let text_query = if query.text == "*" {
                String::new()
            } else {
                query.text.clone()
            };
            if text_query.is_empty() {
                let parser = QueryParser::for_index(&self.index, fields);
                parser
                    .parse_query(&kind_query)
                    .map_err(|err| SmellError::Search {
                        message: err.to_string(),
                    })?
            } else {
                let parser = QueryParser::for_index(&self.index, fields);
                let combined = format!("{text_query} AND {kind_query}");
                parser
                    .parse_query(&combined)
                    .map_err(|err| SmellError::Search {
                        message: err.to_string(),
                    })?
            }
        } else {
            let parser = QueryParser::for_index(&self.index, fields);
            parser
                .parse_query(&query.text)
                .map_err(|err| SmellError::Search {
                    message: err.to_string(),
                })?
        };

        let top_k = if query.top_k == 0 {
            DEFAULT_TOP_K
        } else {
            query.top_k
        };
        let top_docs = searcher.search(&parsed, &TopDocs::with_limit(top_k).order_by_score())?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let retrieved: TantivyDocument = searcher.doc(doc_address)?;
            results.push(document_to_result(&retrieved, self, score)?);
        }
        Ok(results)
    }

    pub fn list_all(&self, top_k: usize) -> Result<Vec<SearchResult>, SmellError> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let searcher = reader.searcher();
        let top_docs = searcher.search(&AllQuery, &TopDocs::with_limit(top_k).order_by_score())?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let retrieved: TantivyDocument = searcher.doc(doc_address)?;
            results.push(document_to_result(&retrieved, self, score)?);
        }
        Ok(results)
    }

    fn writer(&self) -> Result<IndexWriter, SmellError> {
        self.index
            .writer(WRITER_HEAP_BYTES)
            .map_err(SmellError::from)
    }
}

fn build_schema() -> Schema {
    let mut builder = Schema::builder();
    let text_indexing = TextFieldIndexing::default()
        .set_tokenizer("default")
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let text_options = TextOptions::default()
        .set_indexing_options(text_indexing)
        .set_stored();

    builder.add_text_field("path", text_options.clone());
    builder.add_u64_field("start_line", tantivy::schema::STORED);
    builder.add_u64_field("end_line", tantivy::schema::STORED);
    builder.add_text_field("kind", text_options.clone());
    builder.add_text_field("message", text_options.clone());
    builder.add_text_field("content", text_options.clone());
    builder.add_text_field("language", text_options);
    builder.build()
}

fn document_for_smell(
    path_field: Field,
    start_line_field: Field,
    end_line_field: Field,
    kind_field: Field,
    message_field: Field,
    content_field: Field,
    language_field: Field,
    smell: &SmellFinding,
) -> Result<TantivyDocument, SmellError> {
    let start = u64::try_from(smell.start_line).map_err(|_| SmellError::Config {
        message: String::from("start_line out of range"),
    })?;
    let end = u64::try_from(smell.end_line).map_err(|_| SmellError::Config {
        message: String::from("end_line out of range"),
    })?;
    Ok(doc!(
        path_field => smell.path.clone(),
        start_line_field => start,
        end_line_field => end,
        kind_field => smell.kind.clone(),
        message_field => smell.message.clone(),
        content_field => smell.content.clone(),
        language_field => smell.language.clone(),
    ))
}

fn document_to_result(
    document: &TantivyDocument,
    index: &SmellIndex,
    score: f32,
) -> Result<SearchResult, SmellError> {
    let path = document
        .get_first(index.path_field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| SmellError::Search {
            message: String::from("missing path field"),
        })?;
    let start_line = document
        .get_first(index.start_line_field)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| SmellError::Search {
            message: String::from("missing start_line field"),
        })?;
    let end_line = document
        .get_first(index.end_line_field)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| SmellError::Search {
            message: String::from("missing end_line field"),
        })?;
    let kind = document
        .get_first(index.kind_field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| SmellError::Search {
            message: String::from("missing kind field"),
        })?;
    let message = document
        .get_first(index.message_field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| SmellError::Search {
            message: String::from("missing message field"),
        })?;
    let content = document
        .get_first(index.content_field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| SmellError::Search {
            message: String::from("missing content field"),
        })?;
    let language = document
        .get_first(index.language_field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| SmellError::Search {
            message: String::from("missing language field"),
        })?;

    Ok(SearchResult {
        path,
        start_line,
        end_line,
        kind,
        message,
        score,
        content,
        language,
    })
}

fn collect_smells(repo: &Path) -> Result<Vec<SmellFinding>, SmellError> {
    let conflicts = conflicts::find(repo, &ConflictsOptions::default())?;
    Ok(convert_conflicts(&conflicts))
}

fn convert_conflicts(conflicts: &GitConflicts) -> Vec<SmellFinding> {
    let mut smells = Vec::new();
    for file in &conflicts.files {
        let language = language_for_path(&file.path);
        for finding in &file.findings {
            let (start_line, end_line, content) = if let Some(ref hunk) = finding.hunk {
                let content = match (hunk.ours.as_ref(), hunk.base.as_ref(), hunk.theirs.as_ref()) {
                    (Some(ours), _, _) if !ours.is_empty() => ours.join("\n"),
                    (_, Some(base), _) if !base.is_empty() => base.join("\n"),
                    (_, _, Some(theirs)) if !theirs.is_empty() => theirs.join("\n"),
                    _ => String::new(),
                };
                (hunk.start_line, hunk.end_line, content)
            } else {
                (
                    finding.line,
                    finding.line,
                    finding.content.clone().unwrap_or_else(String::new),
                )
            };

            smells.push(SmellFinding {
                path: file.path.clone(),
                start_line: start_line as usize,
                end_line: end_line as usize,
                kind: finding.kind.clone(),
                message: finding.message.clone(),
                content,
                language: language.clone(),
            });
        }
    }
    smells
}

fn language_for_path(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map_or_else(|| String::from("text"), str::to_ascii_lowercase)
}

fn acquire_lock(path: &Path) -> Result<File, SmellError> {
    let lock_path = path.join(".lock");
    fs::create_dir_all(path)?;
    let file = File::create(lock_path)?;
    file.try_lock_exclusive().map_err(|_| SmellError::Locked)?;
    Ok(file)
}

pub fn format_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return String::from("No matches.");
    }
    results
        .iter()
        .map(|result| {
            format!(
                "{}:{}-{} [{}] score={:.3}\n{}",
                result.path,
                result.start_line,
                result.end_line,
                result.kind,
                result.score,
                result
                    .content
                    .lines()
                    .next()
                    .map_or(result.content.as_str(), std::convert::identity)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_repo(path: &Path) {
        fs::write(
            path.join(".gitconfig"),
            "[user]\nname = Test\nemail = test@example.com\n",
        )
        .expect("write gitconfig");
        let run = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(path)
                .env("HOME", path)
                .env("XDG_CONFIG_HOME", path)
                .args(args)
                .output()
                .expect("git command");
            assert!(output.status.success(), "git {args:?} failed");
        };
        run(&["init"]);
        fs::write(path.join(".gitkeep"), "").expect("write .gitkeep");
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);
    }

    #[test]
    fn index_finds_todo_comment() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);

        let src = root.join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("a.rs"), "// TODO: fix this\nfn main() {}").expect("write");

        let index_dir = SmellIndex::index_dir(root);
        let mut index =
            SmellIndex::open_or_create(&index_dir, &SearchConfig::default()).expect("open");
        index.update(root, |_| {}).expect("update");

        let results = index
            .search(&Query {
                text: String::from("todo"),
                kind: None,
                top_k: 3,
            })
            .expect("search");
        assert!(!results.is_empty());
        assert_eq!(results[0].kind, "todo");
        assert!(results[0].path.ends_with("src/a.rs"));
    }

    #[test]
    fn kind_filter_works() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);
        fs::write(root.join("a.rs"), "// TODO: a\n// FIXME: b\n").expect("write");

        let index_dir = SmellIndex::index_dir(root);
        let mut index =
            SmellIndex::open_or_create(&index_dir, &SearchConfig::default()).expect("open");
        index.update(root, |_| {}).expect("update");

        let results = index
            .search(&Query {
                text: String::from("fixme"),
                kind: Some(String::from("fixme")),
                top_k: 3,
            })
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "fixme");
    }
}
