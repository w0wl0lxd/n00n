use std::sync::OnceLock;
use std::time::Duration;

static LATEST: OnceLock<String> = OnceLock::new();

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const RELEASES_URL: &str = "https://api.github.com/repos/tontinton/maki/releases/latest";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub fn latest_version() -> Option<&'static str> {
    LATEST.get().map(|s| s.as_str())
}

pub fn spawn_check() {
    smol::spawn(async {
        match fetch().await {
            Ok(v) if is_newer(&v, CURRENT) => {
                let _ = LATEST.set(v);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "update check failed");
            }
        }
    })
    .detach();
}

async fn fetch() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    use isahc::config::Configurable;
    use isahc::{AsyncReadResponseExt, Request};

    let client = isahc::HttpClient::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    let req = Request::get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "maki")
        .body(())?;
    let mut resp = client.send_async(req).await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()).into());
    }
    let bytes = resp.bytes().await?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or("missing tag_name")?;
    Ok(tag.strip_prefix('v').unwrap_or(tag).to_owned())
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut it = s.split('.');
        Some((
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
        ))
    };
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("0.2.0", "0.1.0", true  ; "minor_bump")]
    #[test_case("1.0.0", "0.9.9", true  ; "major_bump")]
    #[test_case("0.1.1", "0.1.0", true  ; "patch_bump")]
    #[test_case("0.1.0", "0.1.0", false ; "equal")]
    #[test_case("0.0.9", "0.1.0", false ; "older")]
    #[test_case("abc",   "0.1.0", false ; "garbage_latest")]
    #[test_case("1.0.0-rc1", "0.9.0", false ; "prerelease_ignored")]
    fn is_newer_cases(latest: &str, current: &str, expected: bool) {
        assert_eq!(is_newer(latest, current), expected);
    }
}
