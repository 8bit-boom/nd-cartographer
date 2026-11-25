//! The embeddable entry point nd-world compiles in: the studio router nested under
//! a prefix, with no standalone token gate / Host guard (the host supplies auth).
//! Lives in its own integration-test binary because `embed_router` sets the
//! process-global base path, which must not leak into the lib's unit tests.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(router: &axum::Router, uri: &str) -> (StatusCode, String) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn embed_router_serves_under_a_prefix_without_a_token() {
    let dir = std::env::temp_dir().join(format!("ndcart-embed-it-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let router = ndcart_server::embed_router(dir.clone(), "/cartographer/studio", None);

    // Health and the UI answer under the prefix with no token / Host guard.
    let (status, _) = get(&router, "/cartographer/studio/health").await;
    assert_eq!(status, StatusCode::OK);
    let (status, html) = get(&router, "/cartographer/studio").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("<base href=\"/cartographer/studio/\">"),
        "base tag missing"
    );
    let (status, js) = get(&router, "/cartographer/studio/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        js.contains("ND_BASE"),
        "the base-path shim missing from app.js"
    );

    // The root is not served: everything is nested under the prefix.
    let (status, _) = get(&router, "/health").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&dir);
}
