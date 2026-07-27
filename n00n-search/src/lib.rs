#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

mod chunk;
mod error;
mod index;
mod walk;

pub use chunk::Chunk;
pub use error::Error;
pub use index::{
    IndexMetadata, Progress, Query, SearchConfig, SearchIndex, SearchMode, SearchResult,
};
