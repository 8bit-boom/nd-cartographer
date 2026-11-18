//! The AI instruction pack and the spec check, served without any AI configured.

use crate::*;

#[derive(Deserialize)]
pub(crate) struct InstructionsQuery {
    pub(crate) kind: Option<String>,
}

/// `GET /api/ai/instructions?kind=map|world|refine` (default `map`; `tags` and `theme_pack`
/// also work) — the system prompt, reference, repair template, schema and worked examples an
/// agent needs to write a spec. Read-only; works with no AI configured.
pub(crate) async fn ai_instructions(
    axum::extract::Query(q): axum::extract::Query<InstructionsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    aipack::instructions(q.kind.as_deref().unwrap_or("map"))
        .map(Json)
        .map_err(|m| ApiError(CoreError::Invalid(m)))
}

/// `GET /api/ai/schema/{mapspec|worldspec}` — the JSON Schema of a spec.
pub(crate) async fn ai_schema(Path(which): Path<String>) -> Result<Response, ApiError> {
    let text = match which.as_str() {
        "mapspec" => aipack::MAPSPEC_SCHEMA,
        "worldspec" => aipack::WORLDSPEC_SCHEMA,
        _ => {
            return Err(ApiError(CoreError::NotFound(format!(
                "no schema '{which}'; use mapspec or worldspec"
            ))))
        }
    };
    Ok(([(header::CONTENT_TYPE, "application/json")], text).into_response())
}

#[derive(Deserialize)]
pub(crate) struct CheckBody {
    pub(crate) spec: serde_json::Value,
    #[serde(default)]
    pub(crate) kind: Option<String>,
}

/// `POST /api/spec/check` `{spec, kind?}` — run a spec through the same checks the AI loop
/// uses (read, unknown fields, validate, lay out, lint) and report `{ok, kind, errors,
/// unknown_fields, problems}`. Changes nothing; works with no AI configured.
pub(crate) async fn spec_check(
    body: Result<Json<CheckBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be {\"spec\": {...}}".into())))?;
    // Laying a spec out is real work: keep it off the async threads.
    tokio::task::spawn_blocking(move || speccheck::check_value(&b.spec, b.kind.as_deref()))
        .await
        .map_err(|e| ApiError(CoreError::Invalid(e.to_string())))?
        .map(Json)
        .map_err(|m| ApiError(CoreError::Invalid(m)))
}
