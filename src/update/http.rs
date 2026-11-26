//! The updater's HTTP: one `releases/latest` request and asset downloads.
//!
//! Blocking `ureq` calls, run on a detached thread by the caller (never on
//! tokio's blocking pool: iced drops its runtime on quit and that waits for
//! every blocking task). Redirects are followed by hand so each hop can be
//! checked against the host allowlist; logs never carry query strings
//! (GitHub's asset URLs are signed).

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use url::Url;

use super::release::{Release, parse_release};
use super::{CRATE, REPO};

/// Hosts a request may reach (initial URL or redirect).
pub const ALLOWED_HOSTS: [&str; 4] = [
    "api.github.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
/// Largest download (bytes).
pub const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;
/// Largest `releases/latest` body.
const MAX_API_BODY: u64 = 2 * 1024 * 1024;
/// Redirect hops followed.
const MAX_REDIRECTS: u32 = 5;

/// Which URLs may be fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Plain-HTTP loopback origin allowed besides GitHub (tests, through
    /// `FERRO_RELEASE_API=http://127.0.0.1:<port>/...`).
    pub loopback: Option<(String, u16)>,
}

impl Policy {
    /// GitHub over HTTPS only.
    pub fn github() -> Self {
        Self { loopback: None }
    }

    /// Whether `url` may be requested.
    pub fn allows(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        if url.scheme() == "https" && ALLOWED_HOSTS.contains(&host) && url.username().is_empty() {
            return true;
        }
        match &self.loopback {
            Some((h, port)) => {
                url.scheme() == "http" && host == h && url.port_or_known_default() == Some(*port)
            }
            None => false,
        }
    }
}

/// The `releases/latest` URL (or `FERRO_RELEASE_API`, as in install.sh) and
/// the policy that goes with it. `None` if the override is unusable.
pub fn api_endpoint() -> Option<(String, Policy)> {
    match std::env::var("FERRO_RELEASE_API") {
        Ok(v) if !v.trim().is_empty() => endpoint_for(v.trim()),
        _ => Some((
            format!("https://api.github.com/repos/{REPO}/releases/latest"),
            Policy::github(),
        )),
    }
}

/// Policy for an explicit API URL: HTTPS GitHub, or HTTP on loopback.
pub fn endpoint_for(api: &str) -> Option<(String, Policy)> {
    let url = Url::parse(api).ok()?;
    let host = url.host_str()?.to_string();
    let policy = if url.scheme() == "http" && (host == "127.0.0.1" || host == "localhost") {
        Policy {
            loopback: Some((host, url.port_or_known_default()?)),
        }
    } else {
        Policy::github()
    };
    policy.allows(&url).then(|| (api.to_string(), policy))
}

/// `scheme://host/path` without query or fragment, for logs.
pub fn redact(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut u) => {
            u.set_query(None);
            u.set_fragment(None);
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => "<invalid url>".into(),
    }
}

/// Result of a successful check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// A fresh answer: the usable latest release (if any) and its ETag.
    Fresh {
        /// `None`: no release, or a draft / prerelease.
        release: Option<Release>,
        /// `ETag` header.
        etag: Option<String>,
    },
    /// 304: same as the stored answer.
    NotModified,
}

/// A failed check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// 403 / 429: wait (seconds from `Retry-After` / `X-RateLimit-Reset`).
    RateLimited {
        /// Server-suggested wait.
        retry_after: Option<Duration>,
    },
    /// Anything else.
    Failed(String),
}

fn agent(global: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .https_only(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_global(Some(global))
        .user_agent(format!("{CRATE}/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// GET with manual, policy-checked redirects. Returns the final response.
fn get(
    agent: &ureq::Agent,
    url: &str,
    policy: &Policy,
    headers: &[(&str, &str)],
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut current = Url::parse(url).map_err(|_| "invalid download address".to_string())?;
    for _ in 0..=MAX_REDIRECTS {
        if !policy.allows(&current) {
            return Err(format!(
                "refusing to download from {}",
                current.host_str().unwrap_or("?")
            ));
        }
        let mut req = agent.get(current.as_str());
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req.call().map_err(|e| {
            tracing::debug!(url = %redact(current.as_str()), error = %e, "update request failed");
            format!("network error: {e}")
        })?;
        let status = resp.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("redirect without a location")?;
            current = current
                .join(loc)
                .map_err(|_| "invalid redirect".to_string())?;
            continue;
        }
        return Ok(resp);
    }
    Err("too many redirects".into())
}

fn header(resp: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Asks for the latest release (one small request).
pub fn fetch_latest(
    api: &str,
    policy: &Policy,
    etag: Option<&str>,
) -> Result<CheckOutcome, CheckError> {
    let agent = agent(Duration::from_secs(45));
    let mut headers = vec![
        ("Accept", "application/vnd.github+json"),
        ("X-GitHub-Api-Version", "2022-11-28"),
    ];
    if let Some(tag) = etag {
        headers.push(("If-None-Match", tag));
    }
    let mut resp = get(&agent, api, policy, &headers).map_err(CheckError::Failed)?;
    let status = resp.status().as_u16();
    tracing::info!(status, url = %redact(api), "update check");
    match status {
        200 => {
            let etag = header(&resp, "etag");
            let mut body = Vec::new();
            resp.body_mut()
                .with_config()
                .limit(MAX_API_BODY)
                .reader()
                .read_to_end(&mut body)
                .map_err(|e| CheckError::Failed(format!("network error: {e}")))?;
            let release = parse_release(&body).map_err(CheckError::Failed)?;
            Ok(CheckOutcome::Fresh { release, etag })
        }
        304 => Ok(CheckOutcome::NotModified),
        // No release published yet.
        404 => Ok(CheckOutcome::Fresh {
            release: None,
            etag: None,
        }),
        403 | 429 => {
            let retry_after = header(&resp, "retry-after")
                .and_then(|s| s.trim().parse::<u64>().ok())
                .or_else(|| {
                    let reset = header(&resp, "x-ratelimit-reset")?
                        .trim()
                        .parse::<i64>()
                        .ok()?;
                    let now = chrono::Utc::now().timestamp();
                    u64::try_from(reset - now).ok()
                })
                .map(Duration::from_secs);
            Err(CheckError::RateLimited { retry_after })
        }
        s => Err(CheckError::Failed(format!("GitHub answered HTTP {s}"))),
    }
}

/// Why a download stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// The user cancelled.
    Cancelled,
    /// Network, size or write error.
    Failed(String),
}

/// Streams `url` into `out`, at most `cap` bytes, checking `cancel` between
/// reads and reporting `(received, total)`.
pub fn download(
    url: &str,
    policy: &Policy,
    out: &mut dyn Write,
    cap: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<u64, DownloadError> {
    let agent = agent(Duration::from_secs(30 * 60));
    let mut resp = get(
        &agent,
        url,
        policy,
        &[("Accept", "application/octet-stream")],
    )
    .map_err(DownloadError::Failed)?;
    let status = resp.status().as_u16();
    if status != 200 {
        tracing::warn!(status, url = %redact(url), "update download refused");
        return Err(DownloadError::Failed(format!(
            "download failed (HTTP {status})"
        )));
    }
    let total = header(&resp, "content-length").and_then(|s| s.trim().parse::<u64>().ok());
    if total.is_some_and(|t| t > cap) {
        return Err(DownloadError::Failed("the download is too large".into()));
    }
    let mut reader = resp.body_mut().with_config().limit(cap).reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut received = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(DownloadError::Failed(format!("download interrupted: {e}"))),
        };
        received += n as u64;
        if received > cap {
            return Err(DownloadError::Failed("the download is too large".into()));
        }
        out.write_all(&buf[..n])
            .map_err(|e| DownloadError::Failed(format!("cannot write the download: {e}")))?;
        progress(received, total);
    }
    if total.is_some_and(|t| t != received) {
        return Err(DownloadError::Failed("the download was cut short".into()));
    }
    Ok(received)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_hosts_over_https_only() {
        let p = Policy::github();
        let ok = |s: &str| p.allows(&Url::parse(s).unwrap());
        assert!(ok(
            "https://api.github.com/repos/Kartudev/Ferrochat-kartu/releases/latest"
        ));
        assert!(ok(
            "https://github.com/Kartudev/Ferrochat-kartu/releases/download/v1.0.0/x.tar.gz"
        ));
        assert!(ok("https://objects.githubusercontent.com/a/b?sig=1"));
        assert!(ok("https://release-assets.githubusercontent.com/a/b?sig=1"));
        assert!(!ok("http://github.com/x"));
        assert!(!ok("https://github.com.evil.example/x"));
        assert!(!ok("https://evil.example/x"));
        assert!(!ok("https://user@github.com/x"));
        assert!(!ok("http://127.0.0.1:8080/x"));
        assert!(!ok("file:///etc/passwd"));
    }

    #[test]
    fn loopback_override_allows_only_its_origin() {
        let (api, p) = endpoint_for("http://127.0.0.1:4567/latest").unwrap();
        assert_eq!(api, "http://127.0.0.1:4567/latest");
        assert!(p.allows(&Url::parse("http://127.0.0.1:4567/a.tar.gz").unwrap()));
        assert!(!p.allows(&Url::parse("http://127.0.0.1:4568/a").unwrap()));
        assert!(!p.allows(&Url::parse("http://10.0.0.1:4567/a").unwrap()));
        assert!(p.allows(&Url::parse("https://github.com/a").unwrap()));
        assert!(endpoint_for("http://example.com/latest").is_none());
        assert!(endpoint_for("file:///tmp/latest.json").is_none());
        assert!(endpoint_for("https://api.github.com/repos/x/y/releases/latest").is_some());
    }

    #[test]
    fn redaction_drops_signed_queries() {
        assert_eq!(
            redact("https://objects.githubusercontent.com/a/b?X-Amz-Signature=secret#f"),
            "https://objects.githubusercontent.com/a/b"
        );
        assert_eq!(redact("https://u:p@github.com/x"), "https://github.com/x");
        assert_eq!(redact("not a url"), "<invalid url>");
    }
}
