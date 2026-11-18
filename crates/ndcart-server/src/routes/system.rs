//! Static files, health and the setup checks.

use crate::*;

pub(crate) async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "app": "nd-cartographer",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

pub(crate) async fn index() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
        .into_response()
}

pub(crate) async fn app_js() -> Response {
    ([(header::CONTENT_TYPE, "application/javascript")], APP_JS).into_response()
}

pub(crate) async fn app_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css")], APP_CSS).into_response()
}

/// `GET /api/doctor` — the same checks as `ndcart doctor`, for the Settings panel:
/// `{checks: [{level: ok|warn|fail, what, detail}]}`. Read-only; nothing secret is shown.
pub(crate) async fn doctor_checks(State(app): State<App>) -> Json<serde_json::Value> {
    let data = app.data_dir.clone();
    #[allow(unused_mut)]
    let mut checks =
        tokio::task::spawn_blocking(move || doctor::local_checks_with(data.as_deref(), None))
            .await
            .unwrap_or_default();
    #[cfg(feature = "bridge")]
    {
        let stored = app.ai.lock().expect("ai settings").clone();
        checks.extend(doctor::ai_checks_with(&stored).await);
        checks.extend(doctor::local_ai_checks(app.data_dir.as_deref(), &stored, false).await);
    }
    let skipped = app.store.read().await.skipped.clone();
    if !skipped.is_empty() {
        checks.push(doctor::Check {
            level: doctor::Level::Warn,
            what: "projects",
            detail: format!(
                "{} project folder(s) could not be loaded and were skipped: {}",
                skipped.len(),
                skipped.join(", ")
            ),
        });
    }
    Json(serde_json::json!({ "checks": checks }))
}
