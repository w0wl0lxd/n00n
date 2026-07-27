#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

//! Provider-neutral search contracts and secure bounded extraction primitives.

mod chunk;
mod error;
mod extract;
mod index;
mod transport;
mod types;
mod url_policy;
mod walk;

pub use chunk::Chunk;
pub use error::Error;
pub use extract::Extractor;
pub use index::{
    IndexMetadata, Progress, Query, SearchConfig, SearchIndex, SearchMode, SearchResult,
};
pub use transport::{FetchLimits, Fetcher};
pub use types::{ExtractionResult, FetchedContent};
pub use url_policy::UrlPolicy;
