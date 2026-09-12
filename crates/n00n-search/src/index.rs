use std::fs::{self, File};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{Index, IndexWriter, ReloadPolicy, TantivyDocument, doc};

use crate::Error;
use crate::chunk::Chunk;
use crate::walk::collect_chunks;

const INDEX_VERSION: u32 = 1;
const WRITER_HEAP_BYTES: usize = 50_000_000;
const DEFAULT_TOP_K: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Bm25,
    Hybrid,
    Semantic,
}

#[derive(Debug, Clone, Default)]
pub struct SearchConfig {
    pub top_k: usize,
}

#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub mode: SearchMode,
    pub top_k: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f32,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexMetadata {
    pub version: u32,
    pub chunk_count: usize,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub phase: String,
    pub processed: usize,
    pub total: usize,
    pub message: String,
}

pub struct SearchIndex {
    path: PathBuf,
    index: Index,
    content_field: Field,
    path_field: Field,
    start_line_field: Field,
    end_line_field: Field,
    language_field: Field,
}

impl SearchIndex {
    pub fn index_dir(project: &Path) -> PathBuf {
        project.join(".n00n/search")
    }

    pub fn open_or_create(path: &Path, _config: &SearchConfig) -> Result<Self, Error> {
        fs::create_dir_all(path)?;
        let schema = build_schema();
        let index_path = path.join("tantivy_index");
        if !index_path.is_dir() {
            fs::create_dir_all(&index_path)?;
        }
        let directory = MmapDirectory::open(&index_path).map_err(|source| Error::Index {
            message: source.to_string(),
        })?;
        let index = Index::open_or_create(directory, schema.clone()).map_err(Error::from)?;

        Self::from_parts(path.to_path_buf(), index, &schema)
    }

    fn from_parts(path: PathBuf, index: Index, schema: &Schema) -> Result<Self, Error> {
        let content_field = schema
            .get_field("content")
            .map_err(|source| Error::Tantivy { source })?;
        let path_field = schema
            .get_field("path")
            .map_err(|source| Error::Tantivy { source })?;
        let start_line_field = schema
            .get_field("start_line")
            .map_err(|source| Error::Tantivy { source })?;
        let end_line_field = schema
            .get_field("end_line")
            .map_err(|source| Error::Tantivy { source })?;
        let language_field = schema
            .get_field("language")
            .map_err(|source| Error::Tantivy { source })?;
        Ok(Self {
            path,
            index,
            content_field,
            path_field,
            start_line_field,
            end_line_field,
            language_field,
        })
    }

    pub fn metadata(&self) -> Result<IndexMetadata, Error> {
        let metadata_path = self.path.join("metadata.json");
        if !metadata_path.is_file() {
            return Ok(IndexMetadata {
                version: INDEX_VERSION,
                chunk_count: 0,
            });
        }
        let raw = fs::read_to_string(metadata_path)?;
        let metadata: IndexMetadata = serde_json::from_str(&raw).map_err(|err| Error::Config {
            message: err.to_string(),
        })?;
        Ok(metadata)
    }

    pub fn write_metadata(&self, chunk_count: usize) -> Result<(), Error> {
        let metadata = IndexMetadata {
            version: INDEX_VERSION,
            chunk_count,
        };
        let raw = serde_json::to_string_pretty(&metadata).map_err(|err| Error::Config {
            message: err.to_string(),
        })?;
        fs::write(self.path.join("metadata.json"), raw)?;
        Ok(())
    }

    pub fn update(&mut self, repo: &Path, mut progress: impl FnMut(Progress)) -> Result<(), Error> {
        let _lock = acquire_lock(&self.path)?;
        progress(Progress {
            phase: String::from("walking"),
            processed: 0,
            total: 0,
            message: String::from("walking repository"),
        });
        let chunks = collect_chunks(repo)?;
        progress(Progress {
            phase: String::from("indexing"),
            processed: 0,
            total: chunks.len(),
            message: format!("indexing {} chunks", chunks.len()),
        });

        let index_path = self.path.join("tantivy_index");
        let schema = build_schema();
        if index_path.exists() {
            fs::remove_dir_all(&index_path)?;
        }
        fs::create_dir_all(&index_path)?;
        self.index = Index::create_in_dir(&index_path, schema.clone()).map_err(Error::from)?;
        *self = Self::from_parts(self.path.clone(), self.index.clone(), &schema)?;

        let mut writer = self.writer()?;
        for (processed, chunk) in chunks.iter().enumerate() {
            writer.add_document(document_for_chunk(
                self.content_field,
                self.path_field,
                self.start_line_field,
                self.end_line_field,
                self.language_field,
                chunk,
            )?)?;
            if processed % 100 == 0 {
                progress(Progress {
                    phase: String::from("indexing"),
                    processed,
                    total: chunks.len(),
                    message: format!("indexed {processed}/{} chunks", chunks.len()),
                });
            }
        }
        writer.commit()?;
        self.write_metadata(chunks.len())?;
        progress(Progress {
            phase: String::from("complete"),
            processed: chunks.len(),
            total: chunks.len(),
            message: String::from("index complete"),
        });
        Ok(())
    }

    pub fn search(&self, query: &Query) -> Result<Vec<SearchResult>, Error> {
        if query.text.trim().is_empty() {
            return Err(Error::Config {
                message: String::from("query text is required"),
            });
        }
        if matches!(query.mode, SearchMode::Hybrid | SearchMode::Semantic) {
            return Err(Error::NotSupported);
        }

        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(Error::from)?;
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![self.content_field]);
        let parsed = parser
            .parse_query(&query.text)
            .map_err(|err| Error::Index {
                message: err.to_string(),
            })?;
        let top_k = if query.top_k == 0 {
            DEFAULT_TOP_K
        } else {
            query.top_k
        };
        let top_docs = searcher
            .search(&parsed, &TopDocs::with_limit(top_k).order_by_score())
            .map_err(Error::from)?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let retrieved: TantivyDocument = searcher.doc(doc_address).map_err(Error::from)?;
            results.push(document_to_result(&retrieved, self, score)?);
        }
        Ok(results)
    }

    pub fn find_related(&self, file_path: &str, line: usize) -> Result<Vec<SearchResult>, Error> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(Error::from)?;
        let searcher = reader.searcher();
        let query = tantivy::query::AllQuery;
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(10_000).order_by_score())
            .map_err(Error::from)?;

        let mut anchor: Option<SearchResult> = None;
        for (score, doc_address) in top_docs {
            let retrieved: TantivyDocument = searcher.doc(doc_address).map_err(Error::from)?;
            let result = document_to_result(&retrieved, self, score)?;
            if result.file_path == file_path && line >= result.start_line && line <= result.end_line
            {
                anchor = Some(result);
                break;
            }
        }

        let anchor = anchor.ok_or_else(|| Error::Index {
            message: format!("no indexed chunk for {file_path}:{line}"),
        })?;

        let query_text = anchor
            .snippet
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|token| !token.is_empty())
            .take(8)
            .collect::<Vec<_>>()
            .join(" ");
        if query_text.is_empty() {
            return Err(Error::Index {
                message: format!("no searchable tokens in anchor chunk for {file_path}:{line}"),
            });
        }
        self.search(&Query {
            text: query_text,
            mode: SearchMode::Bm25,
            top_k: DEFAULT_TOP_K,
        })
    }

    fn writer(&self) -> Result<IndexWriter, Error> {
        self.index.writer(WRITER_HEAP_BYTES).map_err(Error::from)
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
    builder.add_text_field("content", text_options);
    builder.add_text_field("path", STRING | STORED);
    builder.add_u64_field("start_line", STORED);
    builder.add_u64_field("end_line", STORED);
    builder.add_text_field("language", STRING | STORED);
    builder.build()
}

fn document_for_chunk(
    content_field: Field,
    path_field: Field,
    start_line_field: Field,
    end_line_field: Field,
    language_field: Field,
    chunk: &Chunk,
) -> Result<TantivyDocument, Error> {
    let start = u64::try_from(chunk.start_line).map_err(|_| Error::Config {
        message: String::from("start_line out of range"),
    })?;
    let end = u64::try_from(chunk.end_line).map_err(|_| Error::Config {
        message: String::from("end_line out of range"),
    })?;
    Ok(doc!(
        content_field => chunk.content.clone(),
        path_field => chunk.file_path.clone(),
        start_line_field => start,
        end_line_field => end,
        language_field => chunk.language.clone(),
    ))
}

fn document_to_result(
    document: &TantivyDocument,
    index: &SearchIndex,
    score: f32,
) -> Result<SearchResult, Error> {
    let path = document
        .get_first(index.path_field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| Error::Index {
            message: String::from("missing path field"),
        })?;
    let start_line = document
        .get_first(index.start_line_field)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| Error::Index {
            message: String::from("missing start_line field"),
        })?;
    let end_line = document
        .get_first(index.end_line_field)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| Error::Index {
            message: String::from("missing end_line field"),
        })?;
    let snippet = document
        .get_first(index.content_field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| Error::Index {
            message: String::from("missing content field"),
        })?;
    Ok(SearchResult {
        file_path: path,
        start_line,
        end_line,
        score,
        snippet,
    })
}

fn acquire_lock(path: &Path) -> Result<File, Error> {
    let lock_path = path.join(".lock");
    fs::create_dir_all(path)?;
    let file = File::create(lock_path)?;
    file.try_lock_exclusive().map_err(|_| Error::Locked)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::{Query, SearchConfig, SearchIndex, SearchMode};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn search_returns_ranked_chunks() {
        let repo = tempdir().expect("tempdir");
        let root = repo.path();
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(
            root.join("src/auth.rs"),
            "pub fn login() {}\n\npub fn logout() {}",
        )
        .expect("write");

        let index_dir = root.join(".n00n/search");
        let mut index =
            SearchIndex::open_or_create(&index_dir, &SearchConfig::default()).expect("open");
        index.update(root, |_| {}).expect("update");

        let results = index
            .search(&Query {
                text: String::from("login"),
                mode: SearchMode::Bm25,
                top_k: 3,
            })
            .expect("search");
        assert!(!results.is_empty());
        assert!(results[0].file_path.contains("auth.rs"));
        assert!(results[0].snippet.contains("login"));
    }
}
