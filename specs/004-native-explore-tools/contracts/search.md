# Contract: `n00n-search` Library

**Crate**: `n00n-search`  
**Purpose**: Shared indexing and search core for code intelligence.  
**Consumers**: `n00n-semble`, future search features.

## Public API

```rust
// Index management
pub struct SearchIndex { ... }
impl SearchIndex {
    pub fn open_or_create(path: &Path, config: &SearchConfig) -> Result<Self, Error>;
    pub fn update(&mut self, repo: &Path, progress: impl Fn(Progress)) -> Result<(), Error>;
    pub fn search(&self, query: &Query) -> Result<Vec<SearchResult>, Error>;
    pub fn find_related(&self, file_path: &str, line: usize) -> Result<Vec<SearchResult>, Error>;
}

// Embedder trait
pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Error>;
}

pub enum EmbedderConfig {
    None,
    Static { model_id: String },
    Vllm { url: String, model: String },
    Remote { url: String, api_key: String, model: String },
}

// vLLM presets
pub struct VllmPreset { ... }
impl VllmPreset {
    pub fn list() -> &'static [VllmPreset];
    pub fn podman_command(&self, host_port: u16) -> String;
}
```

## Responsibilities

- `SearchIndex` owns the `.n00n/search/` directory and manages `tantivy` BM25 + optional vector storage.
- `EmbedderConfig` is user-supplied; `n00n-search` never defaults to a cloud provider or bundled API key.
- `VllmPreset` provides runnable `podman run` commands for three local embedding models.

## Error Contract

- All fallible operations return `Result<T, n00n_search::Error>`.
- `Error` is a `thiserror` enum covering `Index`, `Embedder`, `IO`, `Config`, `NotSupported`, and `UserCancelled`.
- No panics, no `.ok()` discards, and no silent fallbacks in library code.
