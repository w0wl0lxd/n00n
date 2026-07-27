use serde::{Deserialize, Serialize};

use crate::error::SearchError;

pub const MAX_QUERY_BYTES: usize = 8_192;
pub const MAX_SEARCH_RESULTS: usize = 100;
pub const MAX_EXTRACT_URLS: usize = 20;
pub const MAX_SOURCE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchDepth {
    Fast,
    Balanced,
    Advanced,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchTopic {
    General,
    News,
    Technical,
    Research,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeSearch {
    Off,
    Moderate,
    Strict,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterRequirement {
    Required,
    BestEffort,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DomainFilter {
    pub domains: Vec<String>,
    pub requirement: FilterRequirement,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: usize,
    pub depth: SearchDepth,
    pub topic: SearchTopic,
    pub locale: Option<String>,
    pub safe_search: SafeSearch,
    pub include_domains: Option<DomainFilter>,
    pub exclude_domains: Option<DomainFilter>,
    pub backend: Option<String>,
    pub content_budget_bytes: usize,
}

impl SearchRequest {
    /// Validates model-controlled search fields and budgets.
    ///
    /// # Errors
    /// Returns a validation error when a field is empty, excessive, or incompatible.
    pub fn validate(&self) -> Result<(), SearchError> {
        if self.query.trim().is_empty() || self.query.len() > MAX_QUERY_BYTES {
            return Err(SearchError::validation(
                "query",
                "must be non-empty and within the query byte limit",
            ));
        }
        if !(1..=MAX_SEARCH_RESULTS).contains(&self.limit) {
            return Err(SearchError::validation(
                "limit",
                "is outside the allowed range",
            ));
        }
        if self.content_budget_bytes > MAX_SOURCE_BYTES {
            return Err(SearchError::validation(
                "content_budget_bytes",
                "exceeds the allowed maximum",
            ));
        }
        if self.include_domains.is_some() && self.exclude_domains.is_some() {
            return Err(SearchError::validation(
                "domains",
                "include and exclude filters cannot be combined",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceClass {
    GeneralWeb,
    Technical,
    Research,
    News,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceChunk {
    pub text: String,
    pub heading_path: Vec<String>,
    pub source_url: String,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SearchResult {
    pub url: String,
    pub final_url: Option<String>,
    pub title: String,
    pub snippet: String,
    pub published_at: Option<String>,
    pub updated_at: Option<String>,
    pub rank: usize,
    pub score: Option<f64>,
    pub engines: Vec<String>,
    pub source_class: SourceClass,
    pub chunks: Vec<EvidenceChunk>,
    pub trust: ContentTrust,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PartialFailure {
    pub engine: Option<String>,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub queried_engines: Vec<String>,
    pub failed_engines: Vec<PartialFailure>,
    pub degraded_capabilities: Vec<String>,
    pub elapsed_ms: u64,
    pub cache_hit: bool,
}

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
    pub fn validate(&self) -> Result<(), SearchError> {
        if self.urls.is_empty() || self.urls.len() > MAX_EXTRACT_URLS {
            return Err(SearchError::validation("urls", "must contain 1 to 20 URLs"));
        }
        if self.max_bytes_per_source == 0 || self.max_bytes_per_source > MAX_SOURCE_BYTES {
            return Err(SearchError::validation(
                "max_bytes_per_source",
                "is outside the allowed range",
            ));
        }
        if self.chunks_per_source > MAX_SEARCH_RESULTS {
            return Err(SearchError::validation(
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
    pub truncated: bool,
    pub trust: ContentTrust,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExtractResponse {
    pub results: Vec<ExtractedContent>,
    pub failures: Vec<PartialFailure>,
}

#[cfg(test)]
mod tests {
    use super::{
        ExtractFormat, ExtractRequest, SafeSearch, SearchDepth, SearchRequest, SearchTopic,
    };

    #[test]
    fn incompatible_domain_filters_are_rejected() {
        let filter = super::DomainFilter {
            domains: vec!["example.com".into()],
            requirement: super::FilterRequirement::Required,
        };
        let request = SearchRequest {
            query: "rust".into(),
            limit: 8,
            depth: SearchDepth::Balanced,
            topic: SearchTopic::Technical,
            locale: None,
            safe_search: SafeSearch::Moderate,
            include_domains: Some(filter.clone()),
            exclude_domains: Some(filter),
            backend: None,
            content_budget_bytes: 1_024,
        };
        assert!(request.validate().is_err());
    }

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
