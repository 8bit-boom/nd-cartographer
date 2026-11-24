//! Token gate for non-loopback listening (plan §11.2): the studio will hold
//! GM credentials for nd-world, so it must not be open on a LAN.
//! Accepted: `Authorization: Bearer T`, cookie `ndcart_token=T`, or `?token=T`
//! (which also sets the cookie so the browser UI keeps working). `/health`
//! stays public.

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::path::Path;

pub fn is_loopback(listen: &str) -> bool {
    let host = listen.rsplit_once(':').map(|(h, _)| h).unwrap_or(listen);
    let host = host.trim_matches(|c| c == '[' || c == ']');
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// DNS-rebinding guard for the token-less loopback mode: a malicious web page
/// can resolve its own domain to 127.0.0.1 and then read/write the studio
/// same-origin. The browser still sends the attacker's domain in `Host`, so
/// only loopback hostnames are accepted. `/health` stays open for probes.
pub async fn host_guard(req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| {
            let host = h.rsplit_once(':').map(|(x, _)| x).unwrap_or(h);
            let host = host.trim_matches(|c| c == '[' || c == ']');
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
        .unwrap_or(false);
    if !ok {
        return (
            StatusCode::FORBIDDEN,
            "ndcart: request Host is not this loopback server",
        )
            .into_response();
    }
    next.run(req).await
}

fn random_token() -> String {
    // OsRng is the OS CSPRNG on every platform (/dev/urandom on Unix,
    // BCryptGenRandom on Windows) — reading the device file directly used to
    // panic on Windows starts with a non-loopback listener.
    use rand::RngCore;
    let mut buf = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Resolve the token: `ND_CARTO_TOKEN`, then `ND_CARTO_TOKEN_FILE`, then
/// `<data-dir>/token` (created 0600 on first run), else a fresh one-off.
/// Returns `(token, freshly_generated)`.
pub fn resolve_token(data_dir: Option<&Path>) -> (String, bool) {
    if let Ok(t) = std::env::var("ND_CARTO_TOKEN") {
        if !t.trim().is_empty() {
            return (t.trim().to_string(), false);
        }
    }
    let file = std::env::var("ND_CARTO_TOKEN_FILE")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| data_dir.map(|d| d.join("token")));
    if let Some(f) = &file {
        if let Ok(t) = std::fs::read_to_string(f) {
            if !t.trim().is_empty() {
                return (t.trim().to_string(), false);
            }
        }
    }
    let t = random_token();
    if let Some(f) = file {
        if let Some(parent) = f.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&f, &t).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    (t, true)
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn cookie_token(req: &Request) -> Option<String> {
    let c = req.headers().get(header::COOKIE)?.to_str().ok()?;
    c.split(';')
        .filter_map(|p| p.trim().strip_prefix("ndcart_token="))
        .map(str::to_string)
        .next()
}

fn query_token(req: &Request) -> Option<String> {
    req.uri()
        .query()?
        .split('&')
        .find_map(|p| p.strip_prefix("token="))
        .map(str::to_string)
}

pub async fn gate(token: String, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let from_query = query_token(&req);
    let supplied = bearer
        .clone()
        .or_else(|| cookie_token(&req))
        .or_else(|| from_query.clone());
    if !supplied.as_deref().is_some_and(|s| ct_eq(s, &token)) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "ndcart: access token required (open the page with ?token=…)",
        )
            .into_response();
    }
    let set_cookie = from_query.as_deref().is_some_and(|q| ct_eq(q, &token));
    let mut resp = next.run(req).await;
    if set_cookie {
        let v = format!("ndcart_token={token}; Path=/; HttpOnly; SameSite=Strict");
        if let Ok(hv) = HeaderValue::from_str(&v) {
            resp.headers_mut().append(header::SET_COOKIE, hv);
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("127.0.0.1:8300"));
        assert!(is_loopback("localhost:8300"));
        assert!(is_loopback("[::1]:8300"));
        assert!(!is_loopback("0.0.0.0:8300"));
        assert!(!is_loopback("192.168.1.5:8300"));
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }

    #[test]
    fn random_token_is_hex_and_unique() {
        // OsRng works on every platform (this used to read /dev/urandom and
        // panic on Windows). Two draws must differ and look like 48 hex chars.
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 48);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn host_guard_rejects_rebinding() {
        use axum::body::Body;
        use axum::http::Request;
        use axum::routing::get;
        use axum::Router;
        use tower::ServiceExt;
        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .route("/health", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(host_guard));
        let req = |host: Option<&str>, path: &str| {
            let mut b = Request::get(path);
            if let Some(h) = host {
                b = b.header("host", h);
            }
            b.body(Body::empty()).unwrap()
        };
        // Loopback hosts (any port) pass.
        for h in [
            "localhost:8300",
            "127.0.0.1:8300",
            "[::1]:8300",
            "LOCALHOST:1234",
        ] {
            let r = app.clone().oneshot(req(Some(h), "/")).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK, "host {h}");
        }
        // A rebinding attacker's domain is refused.
        for h in ["evil.example", "evil.example:8300", "192.168.1.5:8300"] {
            let r = app.clone().oneshot(req(Some(h), "/")).await.unwrap();
            assert_eq!(r.status(), StatusCode::FORBIDDEN, "host {h}");
        }
        // Missing Host is refused; /health stays open regardless.
        let r = app.clone().oneshot(req(None, "/")).await.unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = app
            .clone()
            .oneshot(req(Some("evil.example"), "/health"))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
}
