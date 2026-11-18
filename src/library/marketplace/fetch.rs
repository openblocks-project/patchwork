//! P4.3.1A: fetching the catalogue over HTTPS.
//!
//! [`HttpTransport`] only talks to one allowed origin (production:
//! `https://raw.githubusercontent.com`). It follows at most
//! [`MAX_REDIRECTS`] redirects, and only within that origin, so nothing
//! (credentials included; none are ever sent) can be forwarded to another
//! host. Responses are capped while streaming, requests time out, and the
//! blocking calls run on a worker thread (never on the UI thread).

use std::io::Read;
use std::time::Duration;

use super::RAW_HOST;

/// Most redirects followed (all within the allowed origin).
pub const MAX_REDIRECTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    Timeout,
    TooLarge { limit: usize },
    Status(u16),
    UnsafeRedirect(String),
    UnsafeUrl(String),
    Network(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Timeout => write!(f, "The catalogue server did not respond in time."),
            FetchError::TooLarge { limit } => {
                write!(f, "The catalogue response is larger than {limit} bytes.")
            }
            FetchError::Status(code) => write!(f, "The catalogue server answered HTTP {code}."),
            FetchError::UnsafeRedirect(detail) => {
                write!(
                    f,
                    "The catalogue server sent a redirect that is not allowed ({detail})."
                )
            }
            FetchError::UnsafeUrl(url) => write!(f, "{url} is not an allowed catalogue address."),
            FetchError::Network(detail) => write!(f, "Network error: {detail}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchResponse {
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
    /// HTTP 304 for the `etag` that was sent.
    NotModified,
}

/// How the catalogue is fetched (mocked in tests).
pub trait Transport: Send + Sync {
    fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        max_bytes: usize,
    ) -> Result<FetchResponse, FetchError>;
}

/// The only scheme, host and port a transport talks to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    /// Production: `https://raw.githubusercontent.com`.
    pub fn github_raw() -> Self {
        Self {
            scheme: "https".to_string(),
            host: RAW_HOST.to_string(),
            port: 443,
        }
    }

    /// A plain-HTTP local fixture (tests only).
    #[cfg(test)]
    pub fn local_http(port: u16) -> Self {
        Self {
            scheme: "http".to_string(),
            host: "127.0.0.1".to_string(),
            port,
        }
    }

    pub fn allows(&self, url: &reqwest::Url) -> bool {
        url.scheme() == self.scheme
            && url.host_str() == Some(self.host.as_str())
            && url.port_or_known_default() == Some(self.port)
            && url.username().is_empty()
            && url.password().is_none()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    /// Whole request, including reading the body.
    pub total: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            total: Duration::from_secs(30),
        }
    }
}

fn valid_etag(tag: &str) -> bool {
    !tag.is_empty() && tag.len() <= 256 && tag.chars().all(|c| (' '..='~').contains(&c))
}

/// Blocking HTTP transport restricted to one [`Origin`].
pub struct HttpTransport {
    client: reqwest::blocking::Client,
    origin: Origin,
}

impl HttpTransport {
    /// The production transport for `raw.githubusercontent.com`.
    pub fn github() -> Result<Self, FetchError> {
        Self::new(Origin::github_raw(), Timeouts::default())
    }

    pub fn new(origin: Origin, timeouts: Timeouts) -> Result<Self, FetchError> {
        let allowed = origin.clone();
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                let message = format!("more than {MAX_REDIRECTS} redirects");
                attempt.error(message)
            } else if allowed.allows(attempt.url()) {
                attempt.follow()
            } else {
                let message = format!("redirect to {}", attempt.url());
                attempt.error(message)
            }
        });
        let client = reqwest::blocking::Client::builder()
            .redirect(policy)
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.total)
            .https_only(origin.scheme == "https")
            .referer(false)
            .user_agent(concat!("Patchwork/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| FetchError::Network(e.to_string()))?;
        Ok(Self { client, origin })
    }
}

fn map_reqwest(error: reqwest::Error) -> FetchError {
    if error.is_timeout() {
        return FetchError::Timeout;
    }
    if error.is_redirect() {
        let detail = std::error::Error::source(&error)
            .map(|s| s.to_string())
            .unwrap_or_else(|| error.to_string());
        return FetchError::UnsafeRedirect(detail);
    }
    FetchError::Network(error.without_url().to_string())
}

fn map_io(error: std::io::Error) -> FetchError {
    let timed_out = error.kind() == std::io::ErrorKind::TimedOut
        || error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
            .is_some_and(|e| e.is_timeout());
    if timed_out {
        FetchError::Timeout
    } else {
        FetchError::Network(error.to_string())
    }
}

impl Transport for HttpTransport {
    fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        max_bytes: usize,
    ) -> Result<FetchResponse, FetchError> {
        let parsed =
            reqwest::Url::parse(url).map_err(|_| FetchError::UnsafeUrl(url.to_string()))?;
        if !self.origin.allows(&parsed) {
            return Err(FetchError::UnsafeUrl(url.to_string()));
        }
        let etag = etag.filter(|t| valid_etag(t));
        let mut request = self
            .client
            .get(parsed)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(tag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, tag);
        }
        let response = request.send().map_err(map_reqwest)?;
        if !self.origin.allows(response.url()) {
            return Err(FetchError::UnsafeRedirect(response.url().to_string()));
        }
        let status = response.status();
        if status == reqwest::StatusCode::NOT_MODIFIED && etag.is_some() {
            return Ok(FetchResponse::NotModified);
        }
        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > max_bytes as u64)
        {
            return Err(FetchError::TooLarge { limit: max_bytes });
        }
        let new_etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .filter(|t| valid_etag(t))
            .map(str::to_string);
        let mut bytes = Vec::new();
        response
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(map_io)?;
        if bytes.len() > max_bytes {
            return Err(FetchError::TooLarge { limit: max_bytes });
        }
        Ok(FetchResponse::Body {
            bytes,
            etag: new_etag,
        })
    }
}

/// Deterministic local HTTP server for transport tests.
#[cfg(test)]
pub(crate) mod fixture {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone)]
    pub enum Route {
        /// 200 with a body and optional ETag; 304 when the request's
        /// If-None-Match equals that ETag.
        Ok {
            body: Vec<u8>,
            etag: Option<String>,
        },
        Status(u16),
        Redirect(String),
        /// Waits before answering.
        Slow(Duration),
        /// `bytes` of body; with or without a Content-Length header.
        Huge {
            bytes: usize,
            content_length: bool,
        },
    }

    pub struct Server {
        pub port: u16,
        pub routes: Arc<Mutex<HashMap<String, Route>>>,
        /// Requests seen: (path, headers in lowercase).
        pub requests: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Server {
        pub fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
            let requests: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
            let (r, q) = (routes.clone(), requests.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (r, q) = (r.clone(), q.clone());
                    std::thread::spawn(move || serve(stream, &r, &q));
                }
            });
            Self {
                port,
                routes,
                requests,
            }
        }

        pub fn route(&self, path: &str, route: Route) {
            self.routes.lock().unwrap().insert(path.to_string(), route);
        }

        pub fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }
    }

    fn serve(
        mut stream: TcpStream,
        routes: &Mutex<HashMap<String, Route>>,
        requests: &Mutex<Vec<(String, String)>>,
    ) {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && head.len() < 16 * 1024 {
            match stream.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => return,
            }
        }
        let head = String::from_utf8_lossy(&head).to_lowercase();
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        requests.lock().unwrap().push((path.clone(), head.clone()));
        let route = routes.lock().unwrap().get(&path).cloned();
        let reply = |stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]| {
            let _ = stream.write_all(
                format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").as_bytes(),
            );
            let _ = stream.write_all(body);
            let _ = stream.flush();
        };
        match route {
            None => reply(&mut stream, "404 Not Found", "Content-Length: 0\r\n", b""),
            Some(Route::Ok { body, etag }) => {
                let matches = etag.as_ref().is_some_and(|t| {
                    head.contains(&format!("if-none-match: {}", t.to_lowercase()))
                });
                if matches {
                    reply(&mut stream, "304 Not Modified", "", b"");
                } else {
                    let etag_header = etag.map(|t| format!("ETag: {t}\r\n")).unwrap_or_default();
                    let headers = format!("Content-Length: {}\r\n{etag_header}", body.len());
                    reply(&mut stream, "200 OK", &headers, &body);
                }
            }
            Some(Route::Status(code)) => reply(
                &mut stream,
                &format!("{code} Status"),
                "Content-Length: 0\r\n",
                b"",
            ),
            Some(Route::Redirect(location)) => reply(
                &mut stream,
                "302 Found",
                &format!("Location: {location}\r\nContent-Length: 0\r\n"),
                b"",
            ),
            Some(Route::Slow(delay)) => {
                std::thread::sleep(delay);
                reply(&mut stream, "200 OK", "Content-Length: 2\r\n", b"{}");
            }
            Some(Route::Huge {
                bytes,
                content_length,
            }) => {
                let headers = if content_length {
                    format!("Content-Length: {bytes}\r\n")
                } else {
                    String::new()
                };
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{headers}\r\n").as_bytes(),
                );
                let chunk = vec![b' '; 64 * 1024];
                let mut sent = 0;
                while sent < bytes {
                    let n = chunk.len().min(bytes - sent);
                    if stream.write_all(&chunk[..n]).is_err() {
                        return;
                    }
                    sent += n;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{Route, Server};
    use super::*;

    fn transport(server: &Server) -> HttpTransport {
        HttpTransport::new(
            Origin::local_http(server.port),
            Timeouts {
                connect: Duration::from_secs(2),
                total: Duration::from_millis(800),
            },
        )
        .unwrap()
    }

    #[test]
    fn fetches_a_body_with_its_etag_and_honours_not_modified() {
        let server = Server::start();
        server.route(
            "/index.json",
            Route::Ok {
                body: b"{\"a\":1}".to_vec(),
                etag: Some("\"v1\"".into()),
            },
        );
        let t = transport(&server);
        let url = server.url("/index.json");
        assert_eq!(
            t.fetch(&url, None, 1024).unwrap(),
            FetchResponse::Body {
                bytes: b"{\"a\":1}".to_vec(),
                etag: Some("\"v1\"".into())
            }
        );
        assert_eq!(
            t.fetch(&url, Some("\"v1\""), 1024).unwrap(),
            FetchResponse::NotModified
        );
        // No credentials or cookies are ever sent.
        for (_, head) in server.requests.lock().unwrap().iter() {
            assert!(!head.contains("authorization:") && !head.contains("cookie:"));
            assert!(head.contains("user-agent: patchwork/"));
        }
    }

    #[test]
    fn http_errors_are_reported() {
        let server = Server::start();
        server.route("/gone", Route::Status(404));
        server.route("/broken", Route::Status(500));
        let t = transport(&server);
        assert_eq!(
            t.fetch(&server.url("/gone"), None, 1024),
            Err(FetchError::Status(404))
        );
        assert_eq!(
            t.fetch(&server.url("/broken"), None, 1024),
            Err(FetchError::Status(500))
        );
        // A connection that is refused.
        let closed = TcpListenerPort::unused();
        let t2 = HttpTransport::new(Origin::local_http(closed), Timeouts::default()).unwrap();
        assert!(matches!(
            t2.fetch(&format!("http://127.0.0.1:{closed}/x"), None, 1024),
            Err(FetchError::Network(_))
        ));
    }

    struct TcpListenerPort;
    impl TcpListenerPort {
        /// A port nothing listens on.
        fn unused() -> u16 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        }
    }

    #[test]
    fn slow_servers_time_out() {
        let server = Server::start();
        server.route("/slow", Route::Slow(Duration::from_secs(3)));
        let started = std::time::Instant::now();
        assert_eq!(
            transport(&server).fetch(&server.url("/slow"), None, 1024),
            Err(FetchError::Timeout)
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn oversized_responses_are_cut_off_while_streaming() {
        let server = Server::start();
        server.route(
            "/declared",
            Route::Huge {
                bytes: 4096,
                content_length: true,
            },
        );
        server.route(
            "/streamed",
            Route::Huge {
                bytes: 8 * 1024 * 1024,
                content_length: false,
            },
        );
        let t = transport(&server);
        assert_eq!(
            t.fetch(&server.url("/declared"), None, 1024),
            Err(FetchError::TooLarge { limit: 1024 })
        );
        assert_eq!(
            t.fetch(&server.url("/streamed"), None, 1024),
            Err(FetchError::TooLarge { limit: 1024 })
        );
        // Exactly at the limit is fine.
        server.route(
            "/exact",
            Route::Huge {
                bytes: 1024,
                content_length: false,
            },
        );
        assert!(matches!(
            t.fetch(&server.url("/exact"), None, 1024),
            Ok(FetchResponse::Body { bytes, .. }) if bytes.len() == 1024
        ));
    }

    #[test]
    fn redirects_stay_within_the_origin() {
        let server = Server::start();
        server.route(
            "/index.json",
            Route::Ok {
                body: b"{}".to_vec(),
                etag: None,
            },
        );
        // Same origin: followed.
        server.route("/moved", Route::Redirect(server.url("/index.json")));
        let t = transport(&server);
        assert!(matches!(
            t.fetch(&server.url("/moved"), None, 1024),
            Ok(FetchResponse::Body { .. })
        ));
        // Another host (even on this machine), scheme or port: refused.
        let elsewhere = [
            format!("http://localhost:{}/index.json", server.port),
            format!("https://127.0.0.1:{}/index.json", server.port),
            format!("http://127.0.0.1:{}/index.json", server.port + 1),
            format!("http://user:pw@127.0.0.1:{}/index.json", server.port),
            "https://evil.example.com/index.json".to_string(),
        ];
        for (i, target) in elsewhere.iter().enumerate() {
            let path = format!("/away{i}");
            server.route(&path, Route::Redirect(target.clone()));
            assert!(
                matches!(
                    t.fetch(&server.url(&path), None, 1024),
                    Err(FetchError::UnsafeRedirect(_))
                ),
                "{target}"
            );
        }
        // Redirect loops stop.
        server.route("/loop", Route::Redirect(server.url("/loop")));
        assert!(matches!(
            t.fetch(&server.url("/loop"), None, 1024),
            Err(FetchError::UnsafeRedirect(_))
        ));
        let requests = server.requests.lock().unwrap();
        assert!(
            requests.iter().filter(|(p, _)| p == "/loop").count() <= MAX_REDIRECTS + 1,
            "bounded redirects"
        );
    }

    #[test]
    fn urls_outside_the_origin_are_never_requested() {
        let server = Server::start();
        let t = transport(&server);
        for url in [
            "https://raw.githubusercontent.com/a/b/main/index.json".to_string(),
            format!("http://localhost:{}/x", server.port),
            "not a url".to_string(),
        ] {
            assert!(
                matches!(t.fetch(&url, None, 1024), Err(FetchError::UnsafeUrl(_))),
                "{url}"
            );
        }
        assert!(server.requests.lock().unwrap().is_empty());

        let github = Origin::github_raw();
        let ok =
            reqwest::Url::parse("https://raw.githubusercontent.com/a/b/main/index.json").unwrap();
        assert!(github.allows(&ok));
        for bad in [
            "http://raw.githubusercontent.com/a",
            "https://raw.githubusercontent.com:8443/a",
            "https://user@raw.githubusercontent.com/a",
            "https://github.com/a",
        ] {
            assert!(!github.allows(&reqwest::Url::parse(bad).unwrap()), "{bad}");
        }
    }
}
