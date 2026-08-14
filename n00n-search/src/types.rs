use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const MAX_EXTRACT_URLS: usize = 20;
pub const MAX_CHUNKS_PER_SOURCE: usize = 32;
pub const MAX_SOURCE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractFormat {
    Markdown,
    Text,
    Html,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExtractRequest {
    pub urls: Vec<String>,
    pub format: ExtractFormat,
    pub query: Option<String>,
    pub chunks_per_source: usize,
    pub max_bytes_per_source: usize,
}

impl ExtractRequest {
    /// Validates extraction cardinality and byte budgets.
    ///
    /// # Errors
    /// Returns a validation error when a count or budget is outside its allowed range.
    pub fn validate(&self) -> Result<(), Error> {
        if self.urls.is_empty() || self.urls.len() > MAX_EXTRACT_URLS {
            return Err(Error::validation(
                "urls",
                format!("must contain 1 to {MAX_EXTRACT_URLS} URLs"),
            ));
        }
        if self.max_bytes_per_source == 0 || self.max_bytes_per_source > MAX_SOURCE_BYTES {
            return Err(Error::validation(
                "max_bytes_per_source",
                "is outside the allowed range",
            ));
        }
        if self.chunks_per_source > MAX_CHUNKS_PER_SOURCE {
            return Err(Error::validation(
                "chunks_per_source",
                "exceeds the allowed maximum",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentTrust {
    ExternalUntrusted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExtractedContent {
    pub requested_url: String,
    pub final_url: String,
    pub content_type: String,
    pub content: String,
    pub trust: ContentTrust,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PartialFailure {
    pub engine: Option<String>,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExtractResponse {
    pub results: Vec<ExtractedContent>,
    pub failures: Vec<PartialFailure>,
}

// Type alias for public API
pub type ExtractionResult = Result<ExtractResponse, Error>;

#[cfg(test)]
mod tests {
    use super::{ExtractFormat, ExtractRequest};

    #[test]
    fn extraction_requires_a_bounded_nonempty_url_set() {
        let request = ExtractRequest {
            urls: Vec::new(),
            format: ExtractFormat::Markdown,
            query: None,
            chunks_per_source: 0,
            max_bytes_per_source: 1_024,
        };
        assert!(request.validate().is_err());
    }
}
