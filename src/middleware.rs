use crate::state::AppState;
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------
// Request ID counter
// ---------------------------------------------------------------------------

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Rate limiter (fixed-window per IP)
// ---------------------------------------------------------------------------

/// Simple fixed-window per-IP rate limiter.
/// Each IP gets its own 1-second window; the counter resets when the window expires.
pub struct RateLimiter {
    windows: Mutex<HashMap<String, (u32, Instant)>>,
    pub limit_per_second: u32,
}

impl RateLimiter {
    pub fn new(limit_per_second: u32) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            limit_per_second,
        }
    }

    /// Returns `true` if this request should be allowed, `false` when the limit is exceeded.
    pub fn allow(&self, ip: &str) -> bool {
        let mut map = self.windows.lock().unwrap();
        let now = Instant::now();

        // Prune stale entries when the table grows large to prevent unbounded memory use.
        if map.len() > 10_000 {
            map.retain(|_, (_, ts)| now.duration_since(*ts) < Duration::from_secs(2));
        }

        let entry = map.entry(ip.to_string()).or_insert((0, now));

        if now.duration_since(entry.1) >= Duration::from_secs(1) {
            // New window
            *entry = (1, now);
            true
        } else if entry.0 < self.limit_per_second {
            entry.0 += 1;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Combined middleware: request ID → rate limiting → auth → timeout
// ---------------------------------------------------------------------------

pub async fn middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    // 1. Assign X-Request-Id (also injects it into the request for downstream use)
    let request_id = if state.args.request_id {
        let id = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed).to_string();
        HeaderValue::from_str(&id).ok().inspect(|v| {
            request.headers_mut().insert("x-request-id", v.clone());
        })
    } else {
        None
    };

    // Helper: attach X-Request-Id to any response before returning
    let with_id = |mut resp: Response| -> Response {
        if let Some(ref id) = request_id {
            resp.headers_mut().insert("x-request-id", id.clone());
        }
        resp
    };

    // 2. Rate limiting (per IP)
    if let Some(ref limiter) = state.rate_limiter {
        let ip = peer_ip(&request);
        if !limiter.allow(&ip) {
            return with_id(
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::RETRY_AFTER, HeaderValue::from_static("1"))],
                    "Too Many Requests",
                )
                    .into_response(),
            );
        }
    }

    // 3. Authentication
    if (state.args.api_key.is_some() || state.args.basic_auth.is_some())
        && !check_auth(&state.args, &request)
    {
        return with_id(auth_error_response(&state.args));
    }

    // 4. Timeout (wraps the remaining handler chain)
    let response = if let Some(secs) = state.args.timeout {
        match tokio::time::timeout(Duration::from_secs(secs), next.run(request)).await {
            Ok(resp) => resp,
            Err(_) => (StatusCode::REQUEST_TIMEOUT, "Request timed out").into_response(),
        }
    } else {
        next.run(request).await
    };

    with_id(response)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn peer_ip(request: &Request) -> String {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

pub(crate) fn check_auth(args: &crate::cli::Args, request: &Request) -> bool {
    // --- API key ---
    if let Some(ref expected) = args.api_key {
        // X-Api-Key header
        if request
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|k| k == expected)
        {
            return true;
        }

        // Authorization: Bearer <key>
        if bearer_token(request)
            .as_deref()
            .is_some_and(|k| k == expected)
        {
            return true;
        }

        // API key configured but not matched; only continue to basic-auth if it is also set.
        if args.basic_auth.is_none() {
            return false;
        }
    }

    // --- Basic auth ---
    if let Some(ref expected) = args.basic_auth {
        let provided = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Basic "))
            .and_then(|encoded| STANDARD.decode(encoded).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok());

        return provided.as_deref() == Some(expected.as_str());
    }

    true
}

pub(crate) fn bearer_token(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
}

fn auth_error_response(args: &crate::cli::Args) -> Response {
    if args.basic_auth.is_some() {
        (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static(r#"Basic realm="mq-http""#),
            )],
            "Unauthorized",
        )
            .into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use rstest::rstest;

    // ---- helpers -----------------------------------------------------------

    fn default_args() -> crate::cli::Args {
        crate::cli::Args {
            script: None,
            command: None,
            port: 3000,
            addr: "127.0.0.1".into(),
            format: "json".into(),
            module_directories: None,
            args: None,
            raw_file: None,
            reload: false,
            otel_endpoint: None,
            otel_service_name: "test".into(),
            tls_cert: None,
            tls_key: None,
            stdin: false,
            docs: false,
            docs_title: "Test API".into(),
            docs_version: "0.1.0".into(),
            cors_origins: None,
            timeout: None,
            rate_limit: None,
            api_key: None,
            basic_auth: None,
            request_id: false,
            #[cfg(unix)]
            socket: None,
        }
    }

    fn basic_header(user: &str, pass: &str) -> String {
        use base64::Engine;
        let encoded = STANDARD.encode(format!("{user}:{pass}"));
        format!("Basic {encoded}")
    }

    fn bearer_header(token: &str) -> String {
        format!("Bearer {token}")
    }

    // ---- bearer_token ------------------------------------------------------

    #[rstest]
    #[case("Bearer token123", Some("token123"))]
    #[case("Bearer ", Some(""))]
    #[case("Basic dXNlcjpwYXNz", None)]
    #[case("Token abc", None)]
    fn test_bearer_token(#[case] auth_header: &str, #[case] expected: Option<&str>) {
        let request = Request::builder()
            .header("authorization", auth_header)
            .body(Body::empty())
            .unwrap();
        assert_eq!(bearer_token(&request).as_deref(), expected);
    }

    // ---- check_auth: no auth configured ------------------------------------

    #[test]
    fn test_auth_disabled_always_passes() {
        let args = default_args();
        let request = Request::builder().body(Body::empty()).unwrap();
        assert!(check_auth(&args, &request));
    }

    // ---- check_auth: API key -----------------------------------------------

    #[rstest]
    #[case(Some("x-api-key"), "secret", true)]
    #[case(Some("x-api-key"), "wrong", false)]
    #[case(None, "", false)] // no header → rejected
    fn test_auth_api_key_via_header(
        #[case] header_name: Option<&str>,
        #[case] header_value: &str,
        #[case] expected: bool,
    ) {
        let mut args = default_args();
        args.api_key = Some("secret".into());

        let mut builder = Request::builder();
        if let Some(name) = header_name {
            builder = builder.header(name, header_value);
        }
        let request = builder.body(Body::empty()).unwrap();
        assert_eq!(check_auth(&args, &request), expected);
    }

    #[rstest]
    #[case("secret", true)]
    #[case("wrong", false)]
    fn test_auth_api_key_via_bearer(#[case] token: &str, #[case] expected: bool) {
        let mut args = default_args();
        args.api_key = Some("secret".into());
        let request = Request::builder()
            .header("authorization", bearer_header(token))
            .body(Body::empty())
            .unwrap();
        assert_eq!(check_auth(&args, &request), expected);
    }

    // ---- check_auth: Basic auth --------------------------------------------

    #[rstest]
    #[case("admin", "pass", true)]
    #[case("admin", "wrong", false)]
    #[case("other", "pass", false)]
    fn test_auth_basic(#[case] user: &str, #[case] pass: &str, #[case] expected: bool) {
        let mut args = default_args();
        args.basic_auth = Some("admin:pass".into());
        let request = Request::builder()
            .header("authorization", basic_header(user, pass))
            .body(Body::empty())
            .unwrap();
        assert_eq!(check_auth(&args, &request), expected);
    }

    #[test]
    fn test_auth_basic_no_header_rejected() {
        let mut args = default_args();
        args.basic_auth = Some("admin:pass".into());
        let request = Request::builder().body(Body::empty()).unwrap();
        assert!(!check_auth(&args, &request));
    }

    // ---- check_auth: API key + Basic auth coexist --------------------------

    /// Either valid API key or valid Basic credentials should grant access.
    #[rstest]
    #[case(Some("x-api-key"), "secret", None, "", true)]
    #[case(None, "", Some(("admin", "pass")), "admin:pass", true)]
    #[case(None, "", Some(("admin", "wrong")), "admin:pass", false)]
    fn test_auth_both_configured(
        #[case] api_key_header: Option<&str>,
        #[case] api_key_value: &str,
        #[case] basic_creds: Option<(&str, &str)>,
        #[case] configured_basic: &str,
        #[case] expected: bool,
    ) {
        let mut args = default_args();
        args.api_key = Some("secret".into());
        args.basic_auth = Some(configured_basic.into());

        let mut builder = Request::builder();
        if let Some(name) = api_key_header {
            builder = builder.header(name, api_key_value);
        }
        if let Some((u, p)) = basic_creds {
            builder = builder.header("authorization", basic_header(u, p));
        }
        let request = builder.body(Body::empty()).unwrap();
        assert_eq!(check_auth(&args, &request), expected);
    }

    // ---- RateLimiter -------------------------------------------------------

    #[rstest]
    #[case(5, 5, 0)] // exactly at the limit → all pass
    #[case(5, 10, 5)] // twice the limit → half rejected
    #[case(1, 3, 2)] // very tight limit
    #[case(10, 3, 0)] // well within limit → none rejected
    fn test_rate_limiter_same_window(
        #[case] limit: u32,
        #[case] requests: u32,
        #[case] expected_rejections: u32,
    ) {
        let limiter = RateLimiter::new(limit);
        let rejections = (0..requests)
            .filter(|_| !limiter.allow("127.0.0.1"))
            .count() as u32;
        assert_eq!(rejections, expected_rejections);
    }

    #[test]
    fn test_rate_limiter_different_ips_are_independent() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.allow("1.2.3.4"));
        assert!(limiter.allow("1.2.3.4"));
        assert!(!limiter.allow("1.2.3.4")); // 3rd request from same IP → blocked

        // Different IP has its own fresh window
        assert!(limiter.allow("5.6.7.8"));
        assert!(limiter.allow("5.6.7.8"));
        assert!(!limiter.allow("5.6.7.8"));
    }

    #[test]
    fn test_rate_limiter_window_resets_after_one_second() {
        let limiter = RateLimiter::new(1);
        assert!(limiter.allow("127.0.0.1"));
        assert!(!limiter.allow("127.0.0.1")); // blocked in same window

        // Simulate window expiry by directly manipulating the entry
        {
            let mut map = limiter.windows.lock().unwrap();
            if let Some(entry) = map.get_mut("127.0.0.1") {
                entry.1 = Instant::now() - Duration::from_secs(2); // backdate
            }
        }
        assert!(limiter.allow("127.0.0.1")); // new window → allowed
    }
}
