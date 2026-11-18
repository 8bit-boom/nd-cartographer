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

pub(crate) fn random_token() -> String {
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
        let _ = crate::store::write_atomic_private(&f, t.as_bytes());
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

/// Cross-site request guard. A web page the GM visits can fire "simple"
/// cross-site POSTs at the studio; browsers label those with `Sec-Fetch-Site`
/// and `Origin`, so refuse state-changing requests that come from elsewhere.
/// Requests carrying neither header (curl, the CLI, the MCP bridge) pass.
pub async fn same_origin_guard(req: Request, next: Next) -> Response {
    use axum::http::Method;
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let h = req.headers();
    let site_ok = h
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v == "same-origin" || v == "none");
    let origin_ok = match h.get(header::ORIGIN) {
        None => true,
        Some(o) => {
            let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
            let o_auth = o
                .to_str()
                .ok()
                .and_then(|o| o.split_once("://"))
                .map(|(_, a)| a.trim_end_matches('/'));
            matches!((o_auth, host), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b))
        }
    };
    if site_ok && origin_ok {
        return next.run(req).await;
    }
    (StatusCode::FORBIDDEN, "ndcart: cross-site request refused").into_response()
}

/// Hardening headers on every response: no MIME sniffing of user-uploaded
/// assets, no `Referer` (so `?token=` URLs do not leak), and no framing by
/// other sites.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    resp
}

/// Hostnames (any port) accepted in loopback mode, plus the comma-separated
/// extras in `ND_CARTO_ALLOWED_HOSTS`.
pub fn allowed_hosts() -> Vec<String> {
    let mut v: Vec<String> = ["localhost", "127.0.0.1", "[::1]"].map(String::from).into();
    if let Ok(extra) = std::env::var("ND_CARTO_ALLOWED_HOSTS") {
        v.extend(
            extra
                .split(',')
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty()),
        );
    }
    v
}

fn host_name(host: &str) -> &str {
    if host.starts_with('[') {
        return host.find(']').map_or(host, |i| &host[..=i]);
    }
    host.split(':').next().unwrap_or(host)
}

/// DNS-rebinding guard for loopback mode: a page on `evil.example` that
/// re-resolves to 127.0.0.1 still sends `Host: evil.example`, so refuse any
/// Host that is not on the allow-list. A request with no Host header passes.
pub async fn host_guard(
    allowed: std::sync::Arc<Vec<String>>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(h) = req.headers().get(header::HOST) {
        let ok = h.to_str().is_ok_and(|h| {
            let name = host_name(h).to_ascii_lowercase();
            allowed.contains(&name)
        });
        if !ok {
            return (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response();
        }
    }
    next.run(req).await
}

/// Table tokens: project id -> the scoped token the table screen uses. In
/// memory only, like the table state itself.
pub type TableTokens = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>;

/// The table token for a project, created on first use and kept while the
/// table stays on.
pub fn table_token_for(tokens: &TableTokens, doc_id: &str) -> String {
    tokens
        .lock()
        .expect("table tokens")
        .entry(doc_id.to_string())
        .or_insert_with(random_token)
        .clone()
}

/// The project a table token may be used on for this request, if the route is
/// one the table screen needs (and nothing else):
/// `GET /table/{id}`, `GET|POST /api/docs/{id}/table`, `GET /api/docs/{id}/preview`
/// and `GET /api/docs/{id}/events`.
fn table_route_doc(method: &axum::http::Method, path: &str) -> Option<String> {
    use axum::http::Method;
    let seg: Vec<&str> = path.trim_matches('/').split('/').collect();
    let get = *method == Method::GET;
    let id = match seg.as_slice() {
        ["table", id] if get => id,
        ["api", "docs", id, "table"] if get || *method == Method::POST => id,
        ["api", "docs", id, "preview" | "events"] if get => id,
        _ => return None,
    };
    Some((*id).to_string())
}

fn cookie_named(req: &Request, name: &str) -> Option<String> {
    let c = req.headers().get(header::COOKIE)?.to_str().ok()?;
    c.split(';')
        .filter_map(|p| p.trim().strip_prefix(name)?.strip_prefix('='))
        .map(str::to_string)
        .next()
}

pub async fn gate(token: String, tables: TableTokens, req: Request, next: Next) -> Response {
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
    let cookie = cookie_token(&req);
    let is_gm = |s: &Option<String>| s.as_deref().is_some_and(|s| ct_eq(s, &token));
    let gm = is_gm(&bearer) || is_gm(&cookie) || is_gm(&from_query);
    // A table token is good only for the table screen's own routes, on its own project.
    // `table` = (project, whether it came in the query string).
    let mut table: Option<(String, bool)> = None;
    if !gm {
        if let Some(doc) = table_route_doc(req.method(), req.uri().path()) {
            let want = tables.lock().expect("table tokens").get(&doc).cloned();
            if let Some(want) = want {
                let is_t = |s: &Option<String>| s.as_deref().is_some_and(|s| ct_eq(s, &want));
                let table_cookie = cookie_named(&req, &format!("ndcart_table_{doc}"));
                if is_t(&bearer) || is_t(&table_cookie) || is_t(&from_query) {
                    table = Some((doc, is_t(&from_query)));
                }
            }
        }
        if table.is_none() {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                "ndcart: access token required (open the page with ?token=…)",
            )
                .into_response();
        }
    }
    let set_cookie = match (&table, &from_query) {
        (None, Some(q)) if is_gm(&Some(q.clone())) => Some(format!(
            "ndcart_token={token}; Path=/; HttpOnly; SameSite=Strict"
        )),
        // Remember a table token in a cookie named for its project, so the
        // GM's own cookie in the same browser is never overwritten.
        (Some((doc, true)), Some(q)) => Some(format!(
            "ndcart_table_{doc}={q}; Path=/; HttpOnly; SameSite=Strict"
        )),
        _ => None,
    };
    let mut resp = next.run(req).await;
    if let Some(v) = set_cookie {
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
    fn host_names_are_split_from_ports() {
        assert_eq!(host_name("localhost:8300"), "localhost");
        assert_eq!(host_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(host_name("[::1]:8300"), "[::1]");
        assert_eq!(host_name("evil.example:1"), "evil.example");
    }

    #[test]
    fn tokens_are_48_hex_chars_and_differ() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 48);
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }
}
