use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::Client;
use std::sync::OnceLock;
use std::time::Duration;

use super::job::{download_error, DownloadError, FailureCategory};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const READ_TIMEOUT: Duration = Duration::from_secs(120);

pub const BROWSER_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/131.0.0.0 Safari/537.36"
);

static CLIENT: OnceLock<Client> = OnceLock::new();

pub fn download_client() -> Result<Client, DownloadError> {
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }

    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(BROWSER_USER_AGENT));
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));

    // Version::HTTP_3. Do NOT call http3_prior_knowledge() — that forces H3-only and breaks
    // hosts that only speak TCP HTTP/1.1 or HTTP/2.
    let client = Client::builder()
        .default_headers(headers)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .pool_idle_timeout(Some(Duration::from_secs(120)))
        .pool_max_idle_per_host(16)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .tcp_nodelay(true)
        .http2_adaptive_window(true)
        .http3_congestion_bbr()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .redirect(Policy::none())
        .user_agent(BROWSER_USER_AGENT)
        .build()
        .map_err(|error| {
            download_error(
                FailureCategory::Internal,
                format!("Could not create download client: {error}"),
                false,
            )
        })?;

    let _ = CLIENT.set(client);
    CLIENT.get().cloned().ok_or_else(|| {
        download_error(
            FailureCategory::Internal,
            "Could not initialize shared download client.".into(),
            false,
        )
    })
}

pub fn referer_for_url(raw_url: &str) -> Option<String> {
    let parsed = url::Url::parse(raw_url).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?;
    let port = match (parsed.scheme(), parsed.port()) {
        ("https", Some(443)) | ("http", Some(80)) | (_, None) => String::new(),
        (_, Some(port)) => format!(":{port}"),
    };
    Some(format!("{}://{host}{port}/", parsed.scheme()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referer_strips_path_and_query() {
        assert_eq!(
            referer_for_url("https://ts.bzzhr.to/d/abc?v=token").as_deref(),
            Some("https://ts.bzzhr.to/")
        );
        assert_eq!(
            referer_for_url("http://example.com:8080/file.bin").as_deref(),
            Some("http://example.com:8080/")
        );
    }
}
