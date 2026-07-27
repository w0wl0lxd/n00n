use crate::{
    error::SearchError,
    transport::{FetchLimits, FetchResponse, Fetcher, Transport},
    types::{
        ContentTrust, ExtractFormat, ExtractRequest, ExtractResponse, ExtractedContent,
        PartialFailure,
    },
    url_policy::UrlPolicy,
};

pub struct Extractor<T> {
    fetcher: Fetcher<T>,
}

impl<T: Transport> Extractor<T> {
    /// Builds an extractor with a validated fetch budget.
    ///
    /// # Errors
    /// Returns a validation error when the fetch limits are invalid.
    pub fn new(transport: T, policy: UrlPolicy, limits: FetchLimits) -> Result<Self, SearchError> {
        Ok(Self {
            fetcher: Fetcher::new(transport, policy, limits)?,
        })
    }

    /// Extracts all usable URLs and records individual partial failures.
    ///
    /// # Errors
    /// Returns validation and policy errors, or the first acquisition error when no URL succeeds.
    pub async fn extract(&self, request: &ExtractRequest) -> Result<ExtractResponse, SearchError> {
        request.validate()?;
        let mut results = Vec::with_capacity(request.urls.len());
        let mut failures = Vec::new();
        let mut first_error = None;
        for url in &request.urls {
            match self
                .extract_one_bounded(url, &request.format, request.max_bytes_per_source)
                .await
            {
                Ok(content) => results.push(content),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error.clone());
                    }
                    failures.push(PartialFailure {
                        engine: None,
                        kind: error_kind(&error).into(),
                    });
                }
            }
        }
        if results.is_empty() {
            return Err(first_error.ok_or_else(|| {
                SearchError::validation("urls", "operation produced no usable output")
            })?);
        }
        Ok(ExtractResponse { results, failures })
    }

    /// Fetches and converts one URL according to its declared content type.
    ///
    /// # Errors
    /// Returns a policy, transport, size, HTTP, MIME, decoding, or conversion error.
    pub async fn extract_one(
        &self,
        requested_url: &str,
        format: &ExtractFormat,
    ) -> Result<ExtractedContent, SearchError> {
        let fetched = self.fetcher.fetch(requested_url).await?;
        render_content(requested_url, format, fetched)
    }

    async fn extract_one_bounded(
        &self,
        requested_url: &str,
        format: &ExtractFormat,
        max_response_bytes: usize,
    ) -> Result<ExtractedContent, SearchError> {
        let fetched = self
            .fetcher
            .fetch_bounded(requested_url, max_response_bytes)
            .await?;
        render_content(requested_url, format, fetched)
    }
}

fn render_content(
    requested_url: &str,
    format: &ExtractFormat,
    fetched: FetchResponse,
) -> Result<ExtractedContent, SearchError> {
    let content_type = match fetched.content_type.as_deref() {
        Some(value) => base_content_type(value),
        None => "text/plain",
    };
    if !is_text_content(content_type) {
        return Err(SearchError::UnsupportedContentType {
            content_type: content_type.into(),
        });
    }
    let decoded = String::from_utf8_lossy(&fetched.body);
    let content = match format {
        ExtractFormat::Markdown if is_html(content_type) => htmd::convert(decoded.as_ref())
            .map_err(|error| SearchError::Parse {
                message: error.to_string(),
            })?,
        ExtractFormat::Text if is_html(content_type) => {
            let markdown = htmd::convert(decoded.as_ref()).map_err(|error| SearchError::Parse {
                message: error.to_string(),
            })?;
            markdown_to_text(&markdown)
        }
        ExtractFormat::Html | ExtractFormat::Markdown | ExtractFormat::Text => decoded.into_owned(),
    };
    Ok(ExtractedContent {
        requested_url: requested_url.into(),
        final_url: fetched.final_url,
        content_type: content_type.into(),
        content,
        truncated: false,
        trust: ContentTrust::ExternalUntrusted,
    })
}

fn base_content_type(content_type: &str) -> &str {
    content_type
        .split_once(';')
        .map_or(content_type, |(mime, _)| mime)
        .trim()
}

fn is_html(content_type: &str) -> bool {
    matches!(content_type, "text/html" | "application/xhtml+xml")
}

fn is_text_content(content_type: &str) -> bool {
    content_type.starts_with("text/") || is_html(content_type)
}

fn markdown_to_text(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            line.trim_start_matches(['#', '>', '-', '*', ' '])
                .trim_end()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn error_kind(error: &SearchError) -> &'static str {
    match error {
        SearchError::Validation { .. } => "validation",
        SearchError::PolicyDenied { .. } => "policy_denial",
        SearchError::Auth { .. } => "auth",
        SearchError::Transport { .. } => "transport",
        SearchError::Timeout => "timeout",
        SearchError::RateLimit { .. } => "rate_limit",
        SearchError::Http { .. } => "http",
        SearchError::Challenge => "challenge",
        SearchError::Parse { .. } => "parse",
        SearchError::UnsupportedCapability { .. } => "unsupported_capability",
        SearchError::Quota => "quota",
        SearchError::ResponseTooLarge { .. } => "response_too_large",
        SearchError::UnsupportedContentType { .. } => "unsupported_content_type",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures_lite::io::Cursor;

    use super::Extractor;
    use crate::{
        error::SearchError,
        transport::{FetchLimits, Transport, TransportFuture, TransportRequest, TransportResponse},
        types::{ContentTrust, ExtractFormat},
        url_policy::UrlPolicy,
    };

    struct OneResponse(Mutex<Option<TransportResponse>>);

    impl Transport for OneResponse {
        fn send(&self, _request: TransportRequest) -> TransportFuture<'_> {
            Box::pin(async move {
                self.0
                    .lock()
                    .map_err(|error| SearchError::Transport {
                        message: error.to_string(),
                    })?
                    .take()
                    .ok_or_else(|| SearchError::Transport {
                        message: "response was already consumed".into(),
                    })
            })
        }
    }

    fn extractor(content_type: &str, body: &[u8]) -> Extractor<OneResponse> {
        let response = TransportResponse {
            status: 200,
            headers: vec![("content-type".into(), content_type.into())],
            body: Box::pin(Cursor::new(body.to_vec())),
        };
        Extractor::new(
            OneResponse(Mutex::new(Some(response))),
            UrlPolicy::untrusted_page(),
            FetchLimits {
                max_response_bytes: 1_024,
                max_redirects: 1,
            },
        )
        .unwrap()
    }

    #[test]
    fn html_is_converted_to_untrusted_markdown() {
        smol::block_on(async {
            let extractor = extractor("text/html; charset=utf-8", b"<h1>Title</h1><p>Body</p>");
            let content = extractor
                .extract_one("https://example.com", &ExtractFormat::Markdown)
                .await
                .unwrap();
            assert!(content.content.contains("# Title"));
            assert_eq!(content.trust, ContentTrust::ExternalUntrusted);
        });
    }

    #[test]
    fn image_content_is_rejected() {
        smol::block_on(async {
            let extractor = extractor("image/png", b"not really a png");
            assert!(matches!(
                extractor
                    .extract_one("https://example.com/image", &ExtractFormat::Markdown)
                    .await,
                Err(SearchError::UnsupportedContentType { .. })
            ));
        });
    }
}
