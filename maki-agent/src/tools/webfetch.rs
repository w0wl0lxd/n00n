use std::io::Read;
use std::time::Duration;

use maki_tool_macro::Tool;
use ureq::Agent;

use super::truncate_output;

const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const CF_MITIGATED: &str = "cf-mitigated";
const CF_CHALLENGE: &str = "challenge";
const FALLBACK_USER_AGENT: &str = "maki";

#[derive(Tool, Debug, Clone)]
pub struct WebFetch {
    #[param(description = "URL to fetch (http:// or https://)")]
    url: String,
    #[param(description = "Output format: markdown (default), text, or html")]
    format: Option<String>,
    #[param(description = "Timeout in seconds (default 30, max 120)")]
    timeout: Option<u64>,
}

impl WebFetch {
    pub const NAME: &str = "webfetch";
    pub const DESCRIPTION: &str = include_str!("webfetch.md");

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<String, String> {
        let url = validate_and_upgrade_url(&self.url)?;
        let format = self.validated_format()?;
        let timeout = Duration::from_secs(
            self.timeout
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .min(MAX_TIMEOUT_SECS),
        );

        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build()
            .into();

        let accept = accept_header(format);

        let response = agent
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", accept)
            .call()
            .map_err(|e| format!("request failed: {e}"))?;

        let is_cf_challenge = response.status().as_u16() == 403
            && response
                .headers()
                .get(CF_MITIGATED)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains(CF_CHALLENGE));
        let response = if is_cf_challenge {
            agent
                .get(&url)
                .header("User-Agent", FALLBACK_USER_AGENT)
                .header("Accept", accept)
                .call()
                .map_err(|e| format!("request failed: {e}"))?
        } else {
            response
        };

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}"));
        }

        if let Some(len) = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
            && len > MAX_RESPONSE_BYTES
        {
            return Err(format!("response too large: {len} bytes"));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = read_body(response.into_body().into_reader())?;

        if is_image_content(&content_type) {
            return Err("image content cannot be displayed as text".into());
        }

        let text = String::from_utf8_lossy(&body).into_owned();
        let is_html = content_type.contains("text/html");

        let output = match format {
            "markdown" if is_html => {
                htmd::convert(&text).map_err(|e| format!("html conversion: {e}"))?
            }
            "text" if is_html => strip_html_tags(&text),
            _ => text,
        };

        Ok(truncate_output(output))
    }

    fn validated_format(&self) -> Result<&'static str, String> {
        match self.format.as_deref() {
            None | Some("markdown") => Ok("markdown"),
            Some("text") => Ok("text"),
            Some("html") => Ok("html"),
            Some(other) => Err(format!("unknown format: {other}")),
        }
    }

    pub fn start_summary(&self) -> String {
        self.url.clone()
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}

fn validate_and_upgrade_url(url: &str) -> Result<String, String> {
    if let Some(rest) = url.strip_prefix("http://") {
        return Ok(format!("https://{rest}"));
    }
    if url.starts_with("https://") {
        return Ok(url.to_string());
    }
    Err(format!(
        "URL must start with http:// or https://, got: {url}"
    ))
}

fn accept_header(format: &str) -> &'static str {
    match format {
        "html" => "text/html,*/*;q=0.5",
        "text" => "text/plain,text/html;q=0.9,*/*;q=0.5",
        _ => "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.5",
    }
}

fn read_body(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let limit = MAX_RESPONSE_BYTES + 1;
    reader
        .take(limit as u64)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read error: {e}"))?;
    if buf.len() > MAX_RESPONSE_BYTES {
        return Err(format!("response too large: {} bytes", buf.len()));
    }
    Ok(buf)
}

fn is_image_content(content_type: &str) -> bool {
    content_type.starts_with("image/") && !content_type.contains("svg")
}

fn strip_html_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut skip_tag: Option<&'static str> = None;
    let mut tag_buf = String::new();

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
            continue;
        }
        if ch == '>' {
            in_tag = false;
            let tag_lower = tag_buf.to_ascii_lowercase();
            let tag_name = tag_lower.split_whitespace().next().unwrap_or("");

            match skip_tag {
                None => {
                    skip_tag = match tag_name {
                        "script" => Some("script"),
                        "style" => Some("style"),
                        "noscript" => Some("noscript"),
                        _ => None,
                    };
                }
                Some(expected) => {
                    if tag_name.strip_prefix('/') == Some(expected) {
                        skip_tag = None;
                    }
                }
            }

            if skip_tag.is_none() && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        if in_tag {
            tag_buf.push(ch);
            continue;
        }
        if skip_tag.is_some() {
            continue;
        }
        if ch.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }

    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("https://example.com", "https://example.com" ; "https_passthrough")]
    #[test_case("http://example.com", "https://example.com" ; "http_upgraded_to_https")]
    fn validate_and_upgrade_url_valid(input: &str, expected: &str) {
        assert_eq!(validate_and_upgrade_url(input).unwrap(), expected);
    }

    #[test_case("ftp://example.com" ; "unsupported_scheme")]
    #[test_case("example.com" ; "bare_domain")]
    fn validate_and_upgrade_url_invalid(input: &str) {
        assert!(validate_and_upgrade_url(input).is_err());
    }

    #[test]
    fn validated_format_defaults_to_markdown() {
        let wf = WebFetch {
            url: "https://x.com".into(),
            format: None,
            timeout: None,
        };
        assert_eq!(wf.validated_format().unwrap(), "markdown");
    }

    #[test]
    fn validated_format_rejects_unknown() {
        let wf = WebFetch {
            url: "https://x.com".into(),
            format: Some("xml".into()),
            timeout: None,
        };
        assert!(wf.validated_format().is_err());
    }

    #[test_case("<p>Hello <b>world</b></p>", "Hello world" ; "strips_tags")]
    #[test_case("<p>  hello   \n\t  world  </p>", "hello world" ; "normalizes_whitespace")]
    #[test_case("<script><script>inner</script></script><p>visible</p>", "visible" ; "nested_skip_blocks")]
    fn strip_html_tags_cases(input: &str, expected: &str) {
        assert_eq!(strip_html_tags(input), expected);
    }

    #[test]
    fn strip_html_tags_skips_script_style_noscript() {
        let html = "<div>before</div><script>var x=1;</script><style>.a{}</style><noscript>no</noscript><p>after</p>";
        assert_eq!(strip_html_tags(html), "before after");
    }

    #[test_case("image/png", true ; "image_detected")]
    #[test_case("image/svg+xml", false ; "svg_allowed")]
    fn is_image_content_cases(ct: &str, expected: bool) {
        assert_eq!(is_image_content(ct), expected);
    }

    #[test]
    fn read_body_rejects_oversized() {
        let data = vec![0u8; MAX_RESPONSE_BYTES + 1];
        assert!(read_body(data.as_slice()).is_err());
    }
}
