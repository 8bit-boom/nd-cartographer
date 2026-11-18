//! The shared table screen (what the players see).

use crate::*;

#[derive(Deserialize)]
pub(crate) struct TableBody {
    #[serde(default)]
    pub(crate) floor: Option<String>,
    pub(crate) x: f64,
    pub(crate) y: f64,
    #[serde(default = "d_radius")]
    pub(crate) radius: f64,
    #[serde(default = "yes_bool")]
    pub(crate) darkness: bool,
}

pub(crate) fn d_radius() -> f64 {
    12.0
}

/// `POST /api/docs/{id}/table` `{floor?, x, y, radius?, darkness?}` — show the
/// players' view from this spot on the table screen (`/table/{id}`). `DELETE` turns it off.
pub(crate) async fn table_set(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<TableBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    if !(b.x.is_finite() && b.y.is_finite() && b.radius.is_finite())
        || !(0.5..=200.0).contains(&b.radius)
    {
        return Err(ApiError(CoreError::Invalid(
            "x and y must be numbers and radius 0.5-200".into(),
        )));
    }
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor = match &b.floor {
        Some(f) => doc.floors[floors_by_id(&doc.floors, f)?].id.clone(),
        None => doc.floors[0].id.clone(),
    };
    let n = doc.table.as_ref().map_or(1, |t| t.n + 1);
    doc.table = Some(store::TableState {
        floor,
        x: b.x,
        y: b.y,
        radius: b.radius,
        darkness: b.darkness,
        n,
    });
    drop(store);
    // The scoped token the table screen uses instead of the GM token
    // (kept while the table stays on; revoked by `table_off`).
    let token = auth::table_token_for(&app.table_tokens, &id);
    Ok(Json(
        serde_json::json!({ "ok": true, "n": n, "table_token": token }),
    ))
}

pub(crate) async fn table_off(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.table = None;
    app.table_tokens.lock().expect("table tokens").remove(&id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /api/docs/{id}/table` — what the table screen polls: `{on, n, rev, …}`.
pub(crate) async fn table_get(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    Ok(Json(match &doc.table {
        Some(t) => {
            serde_json::json!({ "on": true, "rev": doc.rev, "name": doc.manifest.name, "state": t })
        }
        None => serde_json::json!({ "on": false, "rev": doc.rev, "name": doc.manifest.name }),
    }))
}

/// `GET /table/{id}` — the table screen: the players' view, full screen, kept in
/// step with the GM. Put it on a TV or a tablet; nothing on it can edit anything.
pub(crate) async fn table_page(Path(id): Path<String>) -> Response {
    // The id is only ever used by the page as a URL path segment; refuse anything odd.
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return (StatusCode::BAD_REQUEST, "bad project id").into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        TABLE_HTML.replace("__DOC__", &id),
    )
        .into_response()
}
